//! The container images Elisa uses, in one pinned list, and the only ones it will ever download.
//!
//! - **The list** (`IMAGES`): every image the code sandbox (`mcp_run.rs`) and the plugin installer (`plugins.rs`) run,
//!   each pinned to a minor tag, with what it is for and its approximate download size. Per language a few
//!   security-supported release lines, one of them **stable** (the newest line that is out with at least one patch
//!   release and still has security support), plus older lines many machines already have. Plugin manifests cannot
//!   name an image: a plugin's installer takes the newest line of its language that is present, else stable.
//! - **The version a run uses** (`resolve`): an explicit `version` from the model (checked against the list), else
//!   what the project declares (`.python-version`, `pyproject.toml` requires-python, `runtime.txt`; `.nvmrc`,
//!   `.node-version`, `package.json` engines.node; `rust-toolchain(.toml)`, `Cargo.toml` rust-version; `go.mod` go /
//!   toolchain), mapped onto the listed lines (a minimum takes the newest line that satisfies it, and when that one is
//!   missing and the user declines the download, the newest present line that still does; a pin outside the list
//!   falls back with a one-line note), else the newest present line (named once per session when it is not stable),
//!   else stable. Only those small files are read (the session's
//!   folder, then upwards to the repository root); no project file goes into a container.
//! - **Downloading** (`pull`): only when the user said yes (a missing image in a session asks; `/sandbox pull`,
//!   `elisa sandbox pull`), never by the model (it has no tool for it). `docker pull` runs against the daemon the
//!   runs use (`DOCKER_HOST`, else Colima's socket), in its own process group so a cancel ends all of it; the image
//!   lands in Colima's VM, nothing is installed on the Mac. Elisa never starts Colima.
//! - **Trust on first pull** (`~/.elisa/images.lock`): the digest of each image Elisa pulled. A later pull of the same
//!   tag that brings another digest warns and asks before it is used (no: the old image is put back).
use crate::plugins::Docker;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// One listed image.
#[derive(Debug, PartialEq, Eq)]
pub struct Img {
    pub image: &'static str,
    /// the sandbox language: python, javascript, rust, go, bash
    pub lang: &'static str,
    /// the release line, as people write it: "3.15", "26", "1.27"
    pub version: &'static str,
    /// (major, minor) of the line; node lines are majors (minor 0)
    pub ver: (u32, u32),
    pub stable: bool,
    /// approximate download (compressed, linux/arm64), MB
    pub mb: u32,
}

/// The pinned list. Checked 2026-11-09 against the official sources: python.org (release cycle: 3.15.2 is out; 3.11 –
/// 3.15 have security support), nodejs.org (release schedule: 26 "Lithium" is Active LTS since 2026-10-28; 22 and 24
/// in maintenance), go.dev/dl (go1.27.3 and go1.26.10 are the supported releases), static.rust-lang.org
/// (channel-rust-stable: 1.99.1; only the newest stable is supported), bash 5.3 (5.3.20); every tag exists on Docker
/// Hub (sizes: its arm64 compressed size). `rust:1.91-alpine` and `bash:5.2` are older lines kept because they are
/// already on many machines (and in older plugin locks): Rust is backward compatible, so a project minimum they meet
/// still runs on them, and a run with no project version prefers an image that is present over a download.
pub const IMAGES: &[Img] = &[
    Img { image: "python:3.15-alpine", lang: "python", version: "3.15", ver: (3, 15), stable: true, mb: 20 },
    Img { image: "python:3.14-alpine", lang: "python", version: "3.14", ver: (3, 14), stable: false, mb: 18 },
    Img { image: "python:3.13-alpine", lang: "python", version: "3.13", ver: (3, 13), stable: false, mb: 17 },
    Img { image: "node:26-alpine", lang: "javascript", version: "26", ver: (26, 0), stable: true, mb: 68 },
    Img { image: "node:24-alpine", lang: "javascript", version: "24", ver: (24, 0), stable: false, mb: 62 },
    Img { image: "node:22-alpine", lang: "javascript", version: "22", ver: (22, 0), stable: false, mb: 61 },
    Img { image: "rust:1.99-alpine", lang: "rust", version: "1.99", ver: (1, 99), stable: true, mb: 351 },
    Img { image: "rust:1.91-alpine", lang: "rust", version: "1.91", ver: (1, 91), stable: false, mb: 330 },
    Img { image: "golang:1.27-alpine", lang: "go", version: "1.27", ver: (1, 27), stable: true, mb: 72 },
    Img { image: "golang:1.26-alpine", lang: "go", version: "1.26", ver: (1, 26), stable: false, mb: 69 },
    Img { image: "bash:5.3", lang: "bash", version: "5.3", ver: (5, 3), stable: true, mb: 7 },
    Img { image: "bash:5.2", lang: "bash", version: "5.2", ver: (5, 2), stable: false, mb: 7 },
];

/// the sandbox languages, in list order
pub const LANGS: [&str; 5] = ["python", "javascript", "rust", "go", "bash"];

/// the language a model or a user named, with the usual aliases
pub fn lang_name(l: &str) -> Option<&'static str> {
    let l = l.trim().to_ascii_lowercase();
    let l = match l.as_str() {
        "py" | "python3" => "python", "js" | "node" | "nodejs" => "javascript", "sh" | "shell" => "bash", "rs" => "rust",
        "golang" => "go", o => o,
    };
    LANGS.iter().copied().find(|x| *x == l)
}

/// how people call the language
pub fn display(lang: &str) -> &'static str {
    match lang { "python" => "Python", "javascript" => "Node", "rust" => "Rust", "go" => "Go", _ => "Bash" }
}

impl Img {
    /// what it is for: "Go sandbox"
    pub fn purpose(&self) -> String { format!("{} sandbox", display(self.lang)) }
    /// with what plugin installers use it for when it is the line they take: "Python sandbox · pip plugins"
    pub fn purpose_as(&self, default: bool) -> String {
        let mut p = self.purpose();
        if default {
            if let Some(i) = match self.lang { "python" => Some("pip"), "javascript" => Some("npm"), "rust" => Some("cargo"), _ => None } {
                p.push_str(&format!(" · {i} plugins"));
            }
        }
        p
    }
    /// "Go 1.27"
    pub fn label(&self) -> String { format!("{} {}", display(self.lang), self.version) }
}

/// the stable image of a language
pub fn stable(lang: &str) -> &'static Img {
    IMAGES.iter().find(|i| i.lang == lang && i.stable).unwrap_or(&IMAGES[0])
}

/// a language's lines, newest first
pub fn lines(lang: &str) -> Vec<&'static Img> {
    let mut v: Vec<&Img> = IMAGES.iter().filter(|i| i.lang == lang).collect();
    v.sort_by_key(|i| std::cmp::Reverse(i.ver));
    v
}

/// The line a run with no project version uses (and plugin installers): the newest listed one that is present, else
/// stable (which then is offered for download).
pub fn default_line(lang: &str, present: &dyn Fn(&Img) -> bool) -> &'static Img {
    lines(lang).into_iter().find(|i| present(i)).unwrap_or_else(|| stable(lang))
}

/// a listed image by its exact name
pub fn listed(image: &str) -> Option<&'static Img> { IMAGES.iter().find(|i| i.image == image.trim()) }

/// What `/sandbox pull X` names: an image (`golang:1.27-alpine`), a language (its stable line: `go`, `node`) or a
/// language and a line (`go@1.26`, `python 3.14`, `node:24`, `python3.13`).
pub fn find(name: &str) -> Option<&'static Img> {
    let n = name.trim();
    if let Some(i) = listed(n) { return Some(i); }
    if let Some(l) = lang_name(n) { return Some(stable(l)); }
    let (l, v) = n.split_once(['@', ' ', ':']).map(|(a, b)| (a.to_string(), b.to_string())).or_else(|| {
        let at = n.find(|c: char| c.is_ascii_digit())?;
        Some((n[..at].to_string(), n[at..].to_string()))
    })?;
    let lang = lang_name(&l)?;
    let (maj, min, _) = parse_ver(&v)?;
    lines(lang).into_iter().find(|i| i.ver.0 == maj && (lang == "javascript" || min.is_none_or(|m| m == i.ver.1)))
}

// ---------- versions ----------

/// "3.12.1" → (3, Some(12), Some(1)); leading words ("v", "go", "python-", "pypy") are skipped; a pre-release tail
/// on the last part ("3.13.0rc1") is dropped
fn parse_ver(s: &str) -> Option<(u32, Option<u32>, Option<u32>)> {
    let s = s.trim();
    let at = s.find(|c: char| c.is_ascii_digit())?;
    let mut parts = s[at..].split('.').map(|p| {
        let d: String = p.chars().take_while(char::is_ascii_digit).collect();
        if d.is_empty() { None } else { d.parse::<u32>().ok() }
    });
    let maj = parts.next()??;
    let min = parts.next().flatten();
    let patch = if min.is_some() { parts.next().flatten() } else { None };
    Some((maj, min, patch))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op { Eq, Ge, Gt, Le, Lt }

/// one comparison against a version, at the precision written (`>=3.10`, `20`, `<3.13`)
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cmp { op: Op, maj: u32, min: Option<u32> }

/// What a project declares: alternatives (`||`) of comparisons that must all hold, how it was written, where.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    alts: Vec<Vec<Cmp>>,
    /// as written: "3.9", ">=3.10,<3.13", "go 1.21"
    pub text: String,
    /// the file it came from, relative to the folder searched from
    pub file: String,
}

impl Cmp {
    fn holds(&self, lang: &str, line: (u32, u32)) -> bool {
        use std::cmp::Ordering as O;
        let o = if lang == "javascript" || self.min.is_none() { line.0.cmp(&self.maj) } else { line.cmp(&(self.maj, self.min.unwrap_or(0))) };
        match self.op { Op::Eq => o == O::Equal, Op::Ge => o != O::Less, Op::Gt => o == O::Greater, Op::Le => o != O::Greater, Op::Lt => o == O::Less }
    }
}

impl Want {
    fn holds(&self, lang: &str, line: (u32, u32)) -> bool { self.alts.iter().any(|a| a.iter().all(|c| c.holds(lang, line))) }
    /// the highest version it names (to tell "too new" from "too old")
    fn top(&self) -> (u32, u32) { self.alts.iter().flatten().filter(|c| c.op != Op::Lt && c.op != Op::Le).map(|c| (c.maj, c.min.unwrap_or(0))).max().unwrap_or((0, 0)) }
}

/// One comparator as package files write it (PEP 440 and npm semver, the common forms): `>=3.10`, `==3.11.*`,
/// `~=3.11`, `^20.1`, `~20.1`, `20.x`, `20`, `<3.13`. None: unreadable. Several Cmps (a caret or tilde is a range).
fn comparator(s: &str, lang: &str) -> Option<Vec<Cmp>> {
    let s = s.trim().trim_start_matches('v');
    if s.is_empty() || s == "*" || s == "x" { return Some(vec![]); }
    let (op, rest) = ["~=", "===", "==", ">=", "<=", "!=", "^", "~", ">", "<", "="].iter()
        .find_map(|p| s.strip_prefix(p).map(|r| (*p, r.trim()))).unwrap_or(("", s));
    if op == "!=" { return Some(vec![]); }
    let (maj, min, patch) = parse_ver(rest.trim_start_matches(['v', 'V']))?;
    let node = lang == "javascript";
    let c = |op, maj, min| Cmp { op, maj, min };
    Some(match op {
        // a range up to the next major (npm caret / python's compatible release on major.minor)
        "^" => vec![c(Op::Ge, maj, min), c(Op::Lt, maj + 1, None)],
        "~=" if patch.is_none() => vec![c(Op::Ge, maj, min), c(Op::Lt, maj + 1, None)],
        "~=" | "~" => vec![c(Op::Ge, maj, min), c(Op::Le, maj, if node { None } else { min })],
        ">=" => vec![c(Op::Ge, maj, min)],
        // `>3.10.2` still allows the 3.10 line; `>3.10` the next ones
        ">" if patch.is_some() || (node && min.is_some()) => vec![c(Op::Ge, maj, min)],
        ">" => vec![c(Op::Gt, maj, min)],
        "<=" => vec![c(Op::Le, maj, min)],
        "<" if patch.is_some_and(|p| p > 0) || (node && min.is_some_and(|m| m > 0)) => vec![c(Op::Le, maj, min)],
        "<" => vec![c(Op::Lt, maj, min)],
        // `==3.11.*`, `3.11`, `20.x`, `20`
        _ => vec![c(Op::Eq, maj, if node { None } else { min })],
    })
}

/// A whole constraint: alternatives split at `||`, each a list of comparators split at commas or spaces.
fn constraint(s: &str, lang: &str) -> Option<Vec<Vec<Cmp>>> {
    let mut alts = Vec::new();
    for alt in s.split("||") {
        let mut all = Vec::new();
        // npm writes `>= 18` too: glue an operator to the number after it
        let toks: Vec<&str> = alt.split([',', ' ']).filter(|t| !t.is_empty()).collect();
        let mut i = 0;
        while i < toks.len() {
            let mut t = toks[i].to_string();
            if t.chars().all(|c| "<>=~^!".contains(c)) && i + 1 < toks.len() { t.push_str(toks[i + 1]); i += 1; }
            // `1.2.3 - 2.3.4` (npm hyphen ranges) is rare: read it as the lower bound only
            if t != "-" { all.extend(comparator(&t, lang)?); }
            i += 1;
        }
        alts.push(all);
    }
    if alts.is_empty() { None } else { Some(alts) }
}

/// at most this many bytes of a project file are read
const MAX_FILE: u64 = 64 * 1024;

fn read_small(p: &Path) -> Option<String> {
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_FILE { return None; }
    std::fs::read_to_string(p).ok()
}

/// the project files of a language, in priority order
fn files(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" => &[".python-version", "pyproject.toml", "runtime.txt"],
        "javascript" => &[".nvmrc", ".node-version", "package.json"],
        "rust" => &["rust-toolchain.toml", "rust-toolchain", "Cargo.toml"],
        "go" => &["go.mod"],
        _ => &[],
    }
}

/// What one file says (None: nothing about the version, or a value that cannot be read).
fn want_in(lang: &str, file: &str, text: &str) -> Option<(String, Vec<Vec<Cmp>>)> {
    let first = || text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string);
    let exact = |v: &str| { let (maj, min, _) = parse_ver(v)?; Some(vec![vec![Cmp { op: Op::Eq, maj, min: if lang == "javascript" { None } else { min } }]]) };
    match file {
        ".python-version" => { let l = first()?; if l == "system" { return None; } exact(&l).map(|a| (l, a)) }
        "runtime.txt" => { let l = first()?; exact(l.trim_start_matches("python-")).map(|a| (l, a)) }
        "pyproject.toml" => {
            let v: toml::Value = toml::from_str(text).ok()?;
            let r = v.get("project")?.get("requires-python")?.as_str()?.to_string();
            constraint(&r, lang).map(|a| (r, a))
        }
        ".nvmrc" | ".node-version" => {
            let l = first()?;
            let low = l.to_ascii_lowercase();
            // lts/* is the Active LTS (stable here); lts/NAME by codename
            if low == "lts/*" || low == "node" || low == "stable" || low == "latest" { return None; }
            let major = match low.strip_prefix("lts/") { Some("iron") => Some(20), Some("jod") => Some(22), Some("krypton") => Some(24), Some("lithium") => Some(26), Some(_) => return None, None => None };
            match major { Some(m) => Some((l, vec![vec![Cmp { op: Op::Eq, maj: m, min: None }]])), None => exact(&l).map(|a| (l, a)) }
        }
        "package.json" => {
            let v: Value = serde_json::from_str(text).ok()?;
            let r = v["engines"]["node"].as_str()?.to_string();
            constraint(&r, lang).map(|a| (r, a))
        }
        "rust-toolchain.toml" | "rust-toolchain" => {
            // the TOML form, or the old one-line file
            let ch = toml::from_str::<toml::Value>(text).ok().and_then(|v| v.get("toolchain")?.get("channel")?.as_str().map(str::to_string)).or_else(first)?;
            if !ch.starts_with(|c: char| c.is_ascii_digit()) { return None; }
            exact(&ch).map(|a| (ch, a))
        }
        "Cargo.toml" => {
            let v: toml::Value = toml::from_str(text).ok()?;
            let r = v.get("package").and_then(|p| p.get("rust-version")).or_else(|| v.get("workspace")?.get("package")?.get("rust-version"))?.as_str()?.to_string();
            let (maj, min, _) = parse_ver(&r)?;
            Some((format!(">={r}"), vec![vec![Cmp { op: Op::Ge, maj, min }]]))
        }
        "go.mod" => {
            // the go line is the minimum language version; a toolchain line asks for at least that toolchain
            let mut best: Option<(u32, Option<u32>, String)> = None;
            for l in text.lines().map(str::trim) {
                let v = l.strip_prefix("go ").or_else(|| l.strip_prefix("toolchain go")).map(str::trim);
                if let Some((maj, min, _)) = v.and_then(parse_ver) {
                    if best.as_ref().is_none_or(|b| (maj, min.unwrap_or(0)) > (b.0, b.1.unwrap_or(0))) { best = Some((maj, min, l.to_string())); }
                }
            }
            let (maj, min, l) = best?;
            Some((l, vec![vec![Cmp { op: Op::Ge, maj, min }]]))
        }
        _ => None,
    }
}

/// the folders searched: `root`, then its parents up to the repository root (a folder with `.git`), at most 8
fn search_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![root.to_path_buf()];
    if root.join(".git").exists() { return v; }
    let mut d = root.parent();
    while let Some(p) = d {
        if v.len() >= 8 || p.parent().is_none() { break; }
        v.push(p.to_path_buf());
        if p.join(".git").exists() { return v; }
        d = p.parent();
    }
    // no repository above: only the folder itself
    v.truncate(1);
    v
}

/// What the project declares for `lang`: the nearest file that says something, in priority order per folder.
pub fn detect(lang: &str, root: &Path) -> Option<Want> {
    for (up, d) in search_dirs(root).into_iter().enumerate() {
        for f in files(lang) {
            let Some(text) = read_small(&d.join(f)) else { continue };
            if let Some((t, alts)) = want_in(lang, f, &text) {
                return Some(Want { alts, text: t, file: format!("{}{f}", "../".repeat(up)) });
            }
        }
    }
    None
}

/// the project has a file of this language at all (for `/sandbox pull project`)
pub fn uses(lang: &str, root: &Path) -> bool {
    search_dirs(root).iter().any(|d| files(lang).iter().any(|f| d.join(f).is_file()))
}

/// The image a run uses and why.
#[derive(Debug, PartialEq)]
pub struct Choice {
    pub img: &'static Img,
    /// "stable", "present", "version", or the project file it came from
    pub from: String,
    /// a one-line note for the result (the project's version is not in the list)
    pub note: Option<String>,
    /// `img` is missing: the newest present line that still fits what was asked, used when the user says no to the
    /// download
    pub fallback: Option<&'static Img>,
    /// a note worth saying once per session per language (an older present line is used; stable is not here)
    pub hint: Option<String>,
}

impl Choice {
    fn new(img: &'static Img, from: impl Into<String>) -> Choice { Choice { img, from: from.into(), note: None, fallback: None, hint: None } }
}

/// Picks the image for a run. `present`: whether a listed image is on this machine.
/// - `version` (checked against the list; Err names the lines there are), else what the project under `root`
///   declares: the newest listed line that fits; when it is missing, `fallback` is the newest present one that fits
///   (the run asks for the download first, and uses the fallback on a no).
/// - A declaration no listed line fits: the default line with a note.
/// - Nothing declared: the default line (`default_line`: the newest present, else stable), with a `hint` when it is
///   not stable.
pub fn resolve(lang: &str, version: Option<&str>, root: Option<&Path>, present: &dyn Fn(&Img) -> bool) -> Result<Choice, String> {
    let all = lines(lang);
    let st = stable(lang);
    let fit = |from: String, ok: &dyn Fn(&Img) -> bool| -> Option<Choice> {
        let fits: Vec<&'static Img> = all.iter().copied().filter(|i| ok(i)).collect();
        let best = *fits.first()?;
        let mut c = Choice::new(best, from);
        if !present(best) { c.fallback = fits.iter().copied().find(|i| present(i)); }
        Some(c)
    };
    if let Some(v) = version.map(str::trim).filter(|v| !v.is_empty()) {
        let list = all.iter().map(|i| i.version).collect::<Vec<_>>().join(", ");
        let alts = constraint(v, lang).filter(|a| !a.iter().all(Vec::is_empty));
        return alts.and_then(|a| fit("version".into(), &|i: &Img| a.iter().any(|x| x.iter().all(|c| c.holds(lang, i.ver))))).ok_or_else(|| format!(
            "version {v:?}: {} {list} can run here (stable {}); leave version out to use the project's version or stable (nothing was run)", display(lang), st.version));
    }
    let def = default_line(lang, present);
    let Some(w) = root.and_then(|r| detect(lang, r)) else {
        if !present(def) { return Ok(Choice::new(st, "stable")); }
        let mut c = Choice::new(def, if def.stable { "stable" } else { "present" });
        if !def.stable {
            let name = display(lang).to_ascii_lowercase();
            c.hint = Some(format!("using {name} {} (on this Mac); stable is {}, /sandbox pull {name} to get it", def.version, st.version));
        }
        return Ok(c);
    };
    if let Some(c) = fit(w.file.clone(), &|i: &Img| w.holds(lang, i.ver)) { return Ok(c); }
    let newest = all.first().map_or(st.version, |i| i.version);
    let oldest = all.last().map_or(st.version, |i| i.version);
    let edge = if w.top() > all.first().map_or((0, 0), |i| i.ver) { format!("the newest supported here is {newest}") } else { format!("the oldest supported here is {oldest}") };
    let using = if present(def) && !def.stable { format!("{} (on this Mac)", def.version) } else { def.version.to_string() };
    let note = format!("project asks for {} {} ({}); using {using}, {edge}", display(lang), w.text, w.file);
    Ok(Choice { note: Some(note), ..Choice::new(def, if def.stable { "stable" } else { "present" }) })
}

// ---------- what is on this machine ----------

/// a listed image as Colima has it
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Local { pub id: String, pub bytes: u64, pub digest: Option<String> }

/// One row of `/sandbox`.
pub struct Row {
    pub img: &'static Img,
    pub local: Option<Local>,
    /// the line this project's version files pick
    pub project: bool,
    /// the line a run with no project version uses, and plugin installers (`default_line`)
    pub default: bool,
}

/// The list with what is there: Err when docker cannot be reached (no Colima: says `colima start`).
pub fn status(home: &Path, project: Option<&Path>) -> Result<Vec<Row>, String> {
    let d = Docker::find(home)?;
    let sizes = d.image_sizes();
    let mut rows = IMAGES.iter().map(|img| {
        let mut local = d.image_info(img.image)?;
        if let (Some(l), Some(b)) = (local.as_mut(), sizes.get(img.image)) { l.bytes = *b; }
        Ok(Row { img, local, project: false, default: false })
    }).collect::<Result<Vec<_>, String>>()?;
    let have: Vec<&str> = rows.iter().filter(|r| r.local.is_some()).map(|r| r.img.image).collect();
    let present = |i: &Img| have.contains(&i.image);
    let need = project.map(|p| project_images(p, &present)).unwrap_or_default();
    let defaults: Vec<&Img> = LANGS.iter().map(|l| default_line(l, &present)).collect();
    for r in &mut rows { r.project = need.contains(&r.img); r.default = defaults.contains(&r.img); }
    Ok(rows)
}

/// the images the project under `root` needs: per language it has files of, the line `resolve` picks
pub fn project_images(root: &Path, present: &dyn Fn(&Img) -> bool) -> Vec<&'static Img> {
    LANGS.iter().filter(|l| uses(l, root)).filter_map(|l| resolve(l, None, Some(root), present).ok()).map(|c| c.img).collect()
}

fn mb(bytes: u64) -> String { format!("{:.0} MB", bytes as f64 / 1e6) }

/// `/sandbox` text
pub fn status_text(rows: &[Row]) -> String {
    let mut t = String::from("Sandbox images (downloaded into Colima's VM, never onto this Mac):\n");
    for r in rows {
        let mut tags = Vec::new();
        if r.img.stable { tags.push("stable"); }
        if r.project { tags.push("this project"); }
        if r.default && r.local.is_some() && !r.img.stable { tags.push("on this Mac"); }
        let tags = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
        let have = match &r.local { Some(l) => format!("present · {} on disk", mb(l.bytes)), None => format!("missing · ~{} MB download", r.img.mb) };
        t.push_str(&format!("  {:<20} {:<30} {have}{tags}\n", r.img.image, r.img.purpose_as(r.default)));
    }
    let missing: u32 = rows.iter().filter(|r| r.local.is_none()).map(|r| r.img.mb).sum();
    if missing > 0 { t.push_str(&format!("missing in all: ~{missing} MB · ")); }
    t.push_str("/sandbox pull NAME|IMAGE|stable|project|all");
    t
}

/// the `sandbox_images` event / `elisa sandbox list --json`
pub fn status_json(rows: &[Row], home: &Path) -> Value {
    let lock = Lock::load(home);
    json!({"images": rows.iter().map(|r| json!({
        "image": r.img.image, "lang": r.img.lang, "version": r.img.version, "label": r.img.label(), "purpose": r.img.purpose_as(r.default),
        "stable": r.img.stable, "project": r.project, "default": r.default, "present": r.local.is_some(), "size_mb": r.img.mb,
        "disk_mb": r.local.as_ref().map(|l| (l.bytes as f64 / 1e6).round() as u64),
        "pinned": lock.get(r.img.image).is_some(),
    })).collect::<Vec<_>>()})
}

/// What `/sandbox pull ARG` asks for: (images, re-pull present ones). `all` / `stable` / `project` take the missing
/// ones; a single name or image is pulled even when present (an update).
pub fn targets(arg: &str, project: Option<&Path>, present: &dyn Fn(&Img) -> bool) -> Result<(Vec<&'static Img>, bool), String> {
    let a = arg.trim();
    let pick = |v: Vec<&'static Img>| v.into_iter().filter(|i| !present(i)).collect::<Vec<_>>();
    match a {
        "" => Err("usage: /sandbox pull NAME|IMAGE|stable|project|all".into()),
        "all" => Ok((pick(IMAGES.iter().collect()), false)),
        "stable" => Ok((pick(IMAGES.iter().filter(|i| i.stable).collect()), false)),
        "project" => {
            let root = project.ok_or("no project folder")?;
            let v = project_images(root, present);
            if v.is_empty() { return Err("this project declares no sandbox language (no .python-version, pyproject.toml, package.json, Cargo.toml, go.mod, …): runs use stable — /sandbox pull stable".into()); }
            Ok((pick(v), false))
        }
        _ => {
            let mut v = Vec::new();
            for n in a.split([',', ' ']).filter(|n| !n.is_empty()) {
                let img = find(n).ok_or_else(|| format!("{n:?} is not on Elisa's image list (/sandbox lists it; only those images can be downloaded)"))?;
                if !v.contains(&img) { v.push(img); }
            }
            Ok((v, true))
        }
    }
}

/// the yes / no question before a download
pub fn question(imgs: &[&Img], why: Option<&str>) -> String {
    match imgs {
        [i] => format!("{} {} (~{} MB), downloaded into Colima's VM — nothing is installed on this Mac. Download it now?",
            why.map_or_else(|| format!("The {} uses", i.purpose()), str::to_string), i.image, i.mb),
        _ => format!("Download {} images (~{} MB in all) into Colima's VM — nothing is installed on this Mac? {}",
            imgs.len(), imgs.iter().map(|i| i.mb).sum::<u32>(), imgs.iter().map(|i| format!("{} (~{} MB)", i.image, i.mb)).collect::<Vec<_>>().join(", ")),
    }
}

/// "The Go sandbox needs" (a missing image blocked a run)
pub fn needs(i: &Img) -> String { format!("The {} sandbox needs", i.label()) }

// ---------- the structured "image missing" marker ----------

/// The run server's result carries the missing image in `_meta` under this key; a plugin server that could not start
/// writes `MARKER=IMAGE` first on its last stderr line. Only a listed image is ever offered for download.
pub const META_KEY: &str = "elisa/missing_image";
/// with META_KEY: the version of the newest present line that still fits what was asked (run on it after a no)
pub const FALLBACK_KEY: &str = "elisa/fallback_version";
pub const MARKER: &str = "elisa:missing-image";

/// `elisa:missing-image=IMAGE · …` → the listed image
pub fn marked(text: &str) -> Option<&'static Img> {
    let at = text.find(MARKER)? + MARKER.len();
    let rest = text[at..].strip_prefix('=')?;
    let name: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != ')' && *c != ',').collect();
    listed(&name)
}

// ---------- images.lock ----------

/// `~/.elisa/images.lock`: the digest of every image Elisa pulled, first pull trusted
#[derive(Default, Debug)]
pub struct Lock(serde_json::Map<String, Value>);

pub const LOCK_FILE: &str = ".elisa/images.lock";

impl Lock {
    pub fn load(home: &Path) -> Lock {
        let v: Value = std::fs::read_to_string(home.join(LOCK_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        Lock(v["images"].as_object().cloned().unwrap_or_default())
    }
    /// (digest or id) recorded for the image
    pub fn get(&self, image: &str) -> Option<String> {
        let e = self.0.get(image)?;
        e["digest"].as_str().filter(|d| !d.is_empty()).or(e["id"].as_str()).map(str::to_string)
    }
    pub fn set(&mut self, image: &str, l: &Local) {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.0.insert(image.into(), json!({"id": l.id, "digest": l.digest, "pulled": ts}));
    }
    pub fn save(&self, home: &Path) -> Result<(), String> {
        let p = home.join(LOCK_FILE);
        if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?; }
        let tmp = p.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&json!({"images": self.0})).unwrap_or_default() + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// what identifies a pulled image for the pin: its repository digest, else its local id
fn ident(l: &Local) -> String { l.digest.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| l.id.clone()) }

fn short(d: &str) -> String {
    let h = d.rsplit(':').next().unwrap_or(d);
    format!("sha256:{}", &h[..h.len().min(12)])
}

// ---------- docker pull ----------

/// Where a pull is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PullProgress { pub image: String, pub done: usize, pub layers: usize, pub elapsed_s: f64, pub line: String }

impl PullProgress {
    /// "downloading golang:1.27-alpine · 3/7 layers"
    pub fn label(&self) -> String {
        if self.layers == 0 { format!("downloading {}", self.image) } else { format!("downloading {} · {}/{} layers", self.image, self.done, self.layers) }
    }
    pub fn json(&self, state: &str) -> Value {
        json!({"image": self.image, "state": state, "done": self.done, "layers": self.layers, "elapsed_s": (self.elapsed_s * 10.0).round() / 10.0, "text": self.line})
    }
}

/// a pull runs at most this long
const PULL_LIMIT: Duration = Duration::from_secs(30 * 60);

/// Layer bookkeeping of `docker pull`'s plain output (`abc123def456: Pull complete`): (done, seen); a progress line
/// goes out when the count changes or every few seconds, never per byte.
#[derive(Default)]
struct Layers { seen: Vec<String>, done: Vec<String> }

impl Layers {
    /// true when this line changed the counts
    fn see(&mut self, line: &str) -> bool {
        let Some((id, st)) = line.split_once(": ") else { return false };
        if id.len() < 12 || !id.chars().all(|c| c.is_ascii_hexdigit()) { return false; }
        let mut changed = false;
        if !self.seen.iter().any(|s| s == id) { self.seen.push(id.to_string()); changed = true; }
        if (st.starts_with("Pull complete") || st.starts_with("Already exists")) && !self.done.iter().any(|s| s == id) { self.done.push(id.to_string()); changed = true; }
        changed
    }
}

/// signal a whole process group: SIGTERM, then SIGKILL after a moment
fn kill_group(child: &mut std::process::Child) {
    let pg = child.id() as i32;
    // SAFETY: signals to the process group this call created (process_group(0) below)
    unsafe { libc::kill(-pg, libc::SIGTERM); }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        if let Ok(Some(_)) = child.try_wait() { return; }
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::kill(-pg, libc::SIGKILL); }
    let _ = child.wait();
}

/// How a pull ended.
#[derive(Debug, PartialEq)]
pub enum Pulled {
    /// on this machine and pinned: a one-line summary
    Ok(String),
    /// the user pressed Esc / Ctrl-C (the pull's process group was ended)
    Cancelled,
    /// the tag brought another digest than the pinned one and the user kept the old image (or it was removed)
    Kept(String),
}

/// `docker pull IMAGE` (a listed image only) with progress and cancel, then the pin: a first pull is trusted and
/// recorded; a pull whose digest differs from the pinned one asks `ask_changed` (no: the old image is put back).
pub fn pull(home: &Path, img: &Img, cancel: &dyn Fn() -> bool, progress: &mut dyn FnMut(&PullProgress), ask_changed: &mut dyn FnMut(&str) -> bool) -> Result<Pulled, String> {
    use std::os::unix::process::CommandExt;
    if listed(img.image).is_none() { return Err(format!("{} is not on Elisa's image list", img.image)); }
    let d = Docker::find(home)?;
    let before = d.image_info(img.image)?;
    let mut c = d.command(&["pull", img.image]);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = c.spawn().map_err(|e| format!("cannot run docker: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<(bool, String)>();
    for (err, r) in [(false, child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>)), (true, child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>))] {
        let Some(r) = r else { continue };
        let tx = tx.clone();
        std::thread::spawn(move || for l in BufReader::new(r).lines().map_while(Result::ok) { if tx.send((err, l)).is_err() { return; } });
    }
    drop(tx);
    let t0 = Instant::now();
    let mut layers = Layers::default();
    let mut errs: Vec<String> = Vec::new();
    let mut p = PullProgress { image: img.image.into(), ..PullProgress::default() };
    let mut last = Instant::now();
    progress(&p);
    let status = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((err, l)) => {
                let l = l.trim().to_string();
                if err && !l.is_empty() { errs.push(l.clone()); }
                if layers.see(&l) || last.elapsed() > Duration::from_secs(3) {
                    p = PullProgress { image: img.image.into(), done: layers.done.len(), layers: layers.seen.len(), elapsed_s: t0.elapsed().as_secs_f64(), line: l };
                    progress(&p);
                    last = Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(Some(s)) = child.try_wait() { break s; }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if cancel() { kill_group(&mut child); return Ok(Pulled::Cancelled); }
        if t0.elapsed() > PULL_LIMIT { kill_group(&mut child); return Err(format!("docker pull {} did not finish in {} min", img.image, PULL_LIMIT.as_secs() / 60)); }
        if let Ok(Some(s)) = child.try_wait() {
            // drain what is left
            while let Ok((err, l)) = rx.recv_timeout(Duration::from_millis(50)) { if err && !l.trim().is_empty() { errs.push(l.trim().to_string()); } else { layers.see(l.trim()); } }
            break s;
        }
    };
    if !status.success() {
        let e = errs.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" · ");
        return Err(if e.contains("Cannot connect to the Docker daemon") { crate::mcp_run::NOT_RUNNING.into() } else { format!("docker pull {} failed: {e}", img.image) });
    }
    let after = d.image_info(img.image)?.ok_or_else(|| format!("docker pull {} ended but the image is not there", img.image))?;
    p = PullProgress { image: img.image.into(), done: layers.seen.len(), layers: layers.seen.len(), elapsed_s: t0.elapsed().as_secs_f64(), line: String::new() };
    progress(&p);
    let mut lock = Lock::load(home);
    match lock.get(img.image) {
        Some(pinned) if pinned != ident(&after) && pinned != after.id => {
            let q = format!("WARNING: {} now has another digest than when Elisa first downloaded it (pinned {}, now {}). A tag moves when a patch release comes out, but it could also be a replaced image. Use the new one?",
                img.image, short(&pinned), short(&ident(&after)));
            if ask_changed(&q) {
                lock.set(img.image, &after);
                lock.save(home)?;
                Ok(Pulled::Ok(format!("{} updated to {} (pin replaced in ~/{LOCK_FILE})", img.image, short(&ident(&after)))))
            } else {
                // put the old image back under the tag, or (gone) remove the new one so it is not used
                match before.filter(|b| b.id != after.id) {
                    Some(b) if d.tag(&b.id, img.image).is_ok() => Ok(Pulled::Kept(format!("kept the pinned {} ({})", img.image, short(&pinned)))),
                    _ => { let _ = d.rmi(img.image); Ok(Pulled::Kept(format!("the new {} was removed; the pinned one ({}) is no longer on this machine", img.image, short(&pinned)))) }
                }
            }
        }
        _ => {
            lock.set(img.image, &after);
            lock.save(home)?;
            let disk = d.image_sizes().get(img.image).copied().unwrap_or(after.bytes);
            Ok(Pulled::Ok(format!("{} is on this machine · {} on disk · {} pinned in ~/{LOCK_FILE}", img.image, mb(disk), short(&ident(&after)))))
        }
    }
}

// ---------- `elisa sandbox …` ----------

const CLI_USAGE: &str = "usage: elisa sandbox list [--json] | pull NAME|IMAGE|stable|project|all [--yes] [--json]";

static CLI_CANCEL: AtomicBool = AtomicBool::new(false);

extern "C" fn on_cli_signal(_: libc::c_int) { CLI_CANCEL.store(true, Ordering::SeqCst); }

/// one audit line for a download
pub fn audit(log: &crate::audit::Log, image: &str, outcome: &str, detail: &str) {
    log.note("image_pull", &[("summary", &format!("pull {image}")), ("outcome", outcome), ("detail", detail)]);
}

/// `elisa sandbox list [--json] | pull …` — the same as `/sandbox` in a session. With `--json` a pull prints JSON lines
/// (`image_pull` events, then `sandbox_images`) for the app; SIGINT / SIGTERM cancel it (the pull's process group
/// ends with it). Exit 0 = done, 1 = failed, declined or cancelled, 2 = usage.
pub fn cli(args: &[String]) -> i32 {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { eprintln!("no HOME"); return 2 };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let as_json = args.iter().any(|a| a == "--json");
    let rest: Vec<&str> = args.iter().map(String::as_str).filter(|a| !["--yes", "-y", "--json"].contains(a)).collect();
    let line = |kind: &str, v: Value| { let mut v = v; v["type"] = json!(kind); println!("{v}"); };
    match rest.first().copied() {
        Some("list") | None => match status(&home, Some(&cwd)) {
            Ok(rows) => { if as_json { println!("{}", status_json(&rows, &home)); } else { println!("{}", status_text(&rows)); } 0 }
            Err(e) => {
                if as_json { println!("{}", json!({"images": IMAGES.iter().map(|i| json!({"image": i.image, "lang": i.lang, "version": i.version, "label": i.label(), "purpose": i.purpose(), "stable": i.stable, "present": null, "size_mb": i.mb})).collect::<Vec<_>>(), "error": e})); }
                eprintln!("{e}");
                1
            }
        },
        Some("pull") => {
            let log = crate::audit::Log::open(&home, &cwd);
            let d = match Docker::find(&home) { Ok(d) => d, Err(e) => { if as_json { line("image_pull", json!({"state": "failed", "text": e})); } eprintln!("{e}"); return 1 } };
            let present = |i: &Img| d.image_info(i.image).ok().flatten().is_some();
            let (imgs, _) = match targets(&rest[1..].join(" "), Some(&cwd), &present) { Ok(x) => x, Err(e) => { eprintln!("{e}"); return if rest.len() < 2 { 2 } else { 1 } } };
            if imgs.is_empty() { println!("nothing to download: those images are on this machine already"); return 0; }
            let q = question(&imgs, None);
            if !crate::plugins::confirm(&q, yes) {
                for i in &imgs { audit(&log, i.image, "declined", ""); }
                println!("nothing was downloaded");
                return 1;
            }
            // SAFETY: the handlers only store to an atomic
            unsafe {
                libc::signal(libc::SIGINT, on_cli_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
                libc::signal(libc::SIGTERM, on_cli_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
            }
            let mut code = 0;
            for i in imgs {
                let mut shown = String::new();
                let r = pull(&home, i, &|| CLI_CANCEL.load(Ordering::SeqCst), &mut |p| {
                    if as_json { line("image_pull", p.json("pulling")); }
                    else if p.label() != shown { shown = p.label(); eprintln!("{shown} · {:.0} s", p.elapsed_s); }
                }, &mut |q| crate::plugins::confirm(q, false));
                let (state, text) = match r {
                    Ok(Pulled::Ok(t)) => { audit(&log, i.image, "ok", &t); ("done", t) }
                    Ok(Pulled::Kept(t)) => { audit(&log, i.image, "kept", &t); code = 1; ("kept", t) }
                    Ok(Pulled::Cancelled) => { audit(&log, i.image, "cancelled", ""); code = 1; ("cancelled", format!("download of {} cancelled", i.image)) }
                    Err(e) => { audit(&log, i.image, "error", &e); code = 1; ("failed", e) }
                };
                if as_json { line("image_pull", json!({"image": i.image, "state": state, "text": text})); } else if code == 0 { println!("{text}"); } else { eprintln!("{text}"); }
                if state == "cancelled" { break; }
            }
            if as_json { if let Ok(rows) = status(&home, Some(&cwd)) { line("sandbox_images", status_json(&rows, &home)); } }
            code
        }
        _ => { eprintln!("{CLI_USAGE}"); 2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> crate::testdir::TempDir {
        let d = crate::testdir::TempDir::new(&format!("images-{tag}"));
        std::fs::create_dir_all(d.join(".git")).unwrap();
        d
    }

    fn pick(lang: &str, files: &[(&str, &str)]) -> Choice {
        let d = dir(lang);
        for (f, t) in files { std::fs::write(d.join(f), t).unwrap(); }
        resolve(lang, None, Some(d.path()), &|_| false).unwrap()
    }

    #[test]
    fn one_stable_line_per_language_and_every_line_is_a_pinned_minor_tag() {
        for l in LANGS {
            assert_eq!(IMAGES.iter().filter(|i| i.lang == l && i.stable).count(), 1, "{l}");
            // stable is the newest listed line
            assert_eq!(lines(l)[0], stable(l), "{l}");
        }
        for i in IMAGES {
            assert!(i.image.contains(&format!(":{}", i.version)), "{}", i.image);
            assert!(!i.image.contains("latest") && i.mb > 0);
        }
        assert_eq!(stable("go").image, "golang:1.27-alpine");
        assert_eq!(find("go").unwrap().image, "golang:1.27-alpine");
        assert_eq!(find("go@1.26").unwrap().image, "golang:1.26-alpine");
        assert_eq!(find("python 3.14").unwrap().image, "python:3.14-alpine");
        assert_eq!(find("python3.13").unwrap().image, "python:3.13-alpine");
        assert_eq!(find("node:24").unwrap().image, "node:24-alpine");
        assert_eq!(find("node:24-alpine").unwrap().image, "node:24-alpine");
        assert!(find("ubuntu:latest").is_none() && find("python@3.9").is_none() && find("go 1.25").is_none());
        assert_eq!(stable("go").purpose(), "Go sandbox");
        assert_eq!(stable("javascript").purpose_as(true), "Node sandbox · npm plugins");
        assert_eq!(listed("rust:1.91-alpine").unwrap().purpose_as(false), "Rust sandbox");
    }

    #[test]
    fn no_project_file_means_stable() {
        let c = pick("python", &[]);
        assert_eq!((c.img.image, c.from.as_str(), c.note), ("python:3.15-alpine", "stable", None));
    }

    #[test]
    fn python_versions_from_each_file() {
        assert_eq!(pick("python", &[(".python-version", "3.13.2\n")]).img.image, "python:3.13-alpine");
        assert_eq!(pick("python", &[("runtime.txt", "python-3.14.1")]).img.image, "python:3.14-alpine");
        assert_eq!(pick("python", &[("pyproject.toml", "[project]\nname='x'\nrequires-python = \">=3.10\"\n")]).img.image, "python:3.15-alpine");
        assert_eq!(pick("python", &[("pyproject.toml", "[project]\nrequires-python = \">=3.10,<3.14\"\n")]).img.image, "python:3.13-alpine");
        assert_eq!(pick("python", &[("pyproject.toml", "[project]\nrequires-python = \"~=3.13.0\"\n")]).img.image, "python:3.13-alpine");
        assert_eq!(pick("python", &[("pyproject.toml", "[project]\nrequires-python = \"==3.14.*\"\n")]).img.image, "python:3.14-alpine");
        // .python-version wins over pyproject in the same folder
        assert_eq!(pick("python", &[(".python-version", "3.14"), ("pyproject.toml", "[project]\nrequires-python = \">=3.10\"\n")]).img.image, "python:3.14-alpine");
        // a pin outside the list: stable, with the note
        let c = pick("python", &[(".python-version", "3.9.18")]);
        assert_eq!(c.img.image, "python:3.15-alpine");
        assert_eq!(c.note.as_deref(), Some("project asks for Python 3.9.18 (.python-version); using 3.15, the oldest supported here is 3.13"));
        let c = pick("python", &[("pyproject.toml", "[project]\nrequires-python = \">=3.16\"\n")]);
        assert!(c.note.as_deref().unwrap().ends_with("using 3.15, the newest supported here is 3.15"), "{:?}", c.note);
        // "system" and unreadable values say nothing
        assert_eq!(pick("python", &[(".python-version", "system")]).note, None);
    }

    #[test]
    fn node_versions_from_each_file() {
        assert_eq!(pick("javascript", &[(".nvmrc", "v22.11.0\n")]).img.image, "node:22-alpine");
        assert_eq!(pick("javascript", &[(".node-version", "24")]).img.image, "node:24-alpine");
        assert_eq!(pick("javascript", &[(".nvmrc", "lts/jod")]).img.image, "node:22-alpine");
        assert_eq!(pick("javascript", &[(".nvmrc", "lts/*")]).img.image, "node:26-alpine");
        let pj = |e: &str| format!("{{\"name\":\"x\",\"engines\":{{\"node\":\"{e}\"}}}}");
        assert_eq!(pick("javascript", &[("package.json", &pj(">=18"))]).img.image, "node:26-alpine");
        assert_eq!(pick("javascript", &[("package.json", &pj("^22.1.0"))]).img.image, "node:22-alpine");
        assert_eq!(pick("javascript", &[("package.json", &pj("20.x || 24.x"))]).img.image, "node:24-alpine");
        assert_eq!(pick("javascript", &[("package.json", &pj(">= 18 < 25"))]).img.image, "node:24-alpine");
        assert_eq!(pick("javascript", &[("package.json", &pj("~24.3"))]).img.image, "node:24-alpine");
        let c = pick("javascript", &[(".nvmrc", "18")]);
        assert_eq!(c.note.as_deref(), Some("project asks for Node 18 (.nvmrc); using 26, the oldest supported here is 22"));
        assert_eq!(pick("javascript", &[("package.json", "{\"name\":\"x\"}")]).from, "stable");
    }

    #[test]
    fn rust_and_go_versions() {
        // a minimum: stable satisfies it
        assert_eq!(pick("rust", &[("Cargo.toml", "[package]\nname='x'\nrust-version = \"1.70\"\n")]).img.image, "rust:1.99-alpine");
        assert_eq!(pick("rust", &[("Cargo.toml", "[workspace.package]\nrust-version = \"1.80\"\n")]).note, None);
        let c = pick("rust", &[("rust-toolchain.toml", "[toolchain]\nchannel = \"1.75.0\"\n")]);
        assert_eq!(c.note.as_deref(), Some("project asks for Rust 1.75.0 (rust-toolchain.toml); using 1.99, the oldest supported here is 1.91"));
        assert_eq!(pick("rust", &[("rust-toolchain", "1.91\n")]).img.image, "rust:1.91-alpine");
        assert_eq!(pick("rust", &[("rust-toolchain", "1.99\n")]).img.image, "rust:1.99-alpine");
        assert_eq!(pick("rust", &[("rust-toolchain", "nightly\n")]).note, None);
        assert_eq!(pick("go", &[("go.mod", "module x\n\ngo 1.21\n")]).img.image, "golang:1.27-alpine");
        assert_eq!(pick("go", &[("go.mod", "module x\n\ngo 1.26.1\n")]).img.image, "golang:1.27-alpine");
        let c = pick("go", &[("go.mod", "module x\ngo 1.26\ntoolchain go1.28.0\n")]);
        assert_eq!(c.note.as_deref(), Some("project asks for Go toolchain go1.28.0 (go.mod); using 1.27, the newest supported here is 1.27"));
    }

    #[test]
    fn the_nearest_file_upwards_to_the_repository_root() {
        let d = dir("upward");
        std::fs::create_dir_all(d.join("svc/api")).unwrap();
        std::fs::write(d.join("go.mod"), "module x\ngo 1.26\n").unwrap();
        std::fs::write(d.join(".python-version"), "3.13\n").unwrap();
        std::fs::write(d.join("svc/.python-version"), "3.14\n").unwrap();
        let sub = d.join("svc/api");
        assert_eq!(resolve("python", None, Some(&sub), &|_| false).unwrap().img.image, "python:3.14-alpine");
        assert_eq!(resolve("go", None, Some(&sub), &|_| false).unwrap().from, "../../go.mod");
        assert!(uses("go", &sub) && !uses("rust", &sub));
        assert_eq!(project_images(&sub, &|_| false).iter().map(|i| i.image).collect::<Vec<_>>(), ["python:3.14-alpine", "golang:1.27-alpine"]);
    }

    #[test]
    fn an_explicit_version_is_checked_against_the_list() {
        assert_eq!(resolve("python", Some("3.14"), None, &|_| false).unwrap().img.image, "python:3.14-alpine");
        assert_eq!(resolve("go", Some("1.26.3"), None, &|_| false).unwrap().img.image, "golang:1.26-alpine");
        assert_eq!(resolve("javascript", Some("v24"), None, &|_| false).unwrap().img.image, "node:24-alpine");
        let e = resolve("python", Some("3.9"), None, &|_| false).unwrap_err();
        assert!(e.contains("Python 3.15, 3.14, 3.13 can run here (stable 3.15)") && e.contains("nothing was run"), "{e}");
        assert!(resolve("go", Some("latest"), None, &|_| false).is_err());
        // an explicit version beats the project's
        let d = dir("explicit");
        std::fs::write(d.join(".python-version"), "3.13\n").unwrap();
        assert_eq!(resolve("python", Some("3.14"), Some(d.path()), &|_| false).unwrap().img.image, "python:3.14-alpine");
    }

    #[test]
    fn what_is_present_is_preferred_unless_the_project_asks_otherwise() {
        let d = dir("present");
        let old = |i: &Img| ["python:3.13-alpine", "node:22-alpine", "rust:1.91-alpine", "bash:5.2"].contains(&i.image);
        // nothing declared: the newest present line, with a once-per-session hint when it is not stable
        let c = resolve("python", None, Some(d.path()), &old).unwrap();
        assert_eq!((c.img.image, c.from.as_str(), c.fallback, c.note.as_deref()), ("python:3.13-alpine", "present", None, None));
        assert_eq!(c.hint.as_deref(), Some("using python 3.13 (on this Mac); stable is 3.15, /sandbox pull python to get it"));
        assert_eq!(resolve("bash", None, None, &old).unwrap().img.image, "bash:5.2");
        // nothing present: stable (the download is offered), no hint
        let c = resolve("go", None, Some(d.path()), &old).unwrap();
        assert_eq!((c.img.image, c.hint, c.fallback), ("golang:1.27-alpine", None, None));
        // stable present: no hint
        assert_eq!(resolve("python", None, None, &|i: &Img| i.lang == "python").unwrap().hint, None);
        assert_eq!(default_line("rust", &old).image, "rust:1.91-alpine");
        assert_eq!(default_line("rust", &|_| false).image, "rust:1.99-alpine");
        // a project minimum: the newest line that fits (asked for), the newest present one that fits on a no
        std::fs::write(d.join("Cargo.toml"), "[package]\nname='x'\nrust-version = \"1.80\"\n").unwrap();
        let c = resolve("rust", None, Some(d.path()), &old).unwrap();
        assert_eq!((c.img.image, c.fallback.map(|i| i.image), c.from.as_str()), ("rust:1.99-alpine", Some("rust:1.91-alpine"), "Cargo.toml"));
        std::fs::write(d.join("Cargo.toml"), "[package]\nname='x'\nrust-version = \"1.95\"\n").unwrap();
        assert_eq!(resolve("rust", None, Some(d.path()), &old).unwrap().fallback, None, "1.91 does not meet 1.95");
        // the declared line present: used, nothing to ask
        std::fs::write(d.join(".python-version"), "3.13\n").unwrap();
        let c = resolve("python", None, Some(d.path()), &old).unwrap();
        assert_eq!((c.img.image, c.fallback, c.hint), ("python:3.13-alpine", None, None));
        std::fs::write(d.join(".python-version"), "3.14\n").unwrap();
        assert_eq!(resolve("python", None, Some(d.path()), &old).unwrap().fallback, None, "an exact pin has no other line");
        // a pin outside the list: the present line, with the note
        std::fs::write(d.join(".python-version"), "3.9\n").unwrap();
        let c = resolve("python", None, Some(d.path()), &old).unwrap();
        assert_eq!(c.img.image, "python:3.13-alpine");
        assert_eq!(c.note.as_deref(), Some("project asks for Python 3.9 (.python-version); using 3.13 (on this Mac), the oldest supported here is 3.13"));
        // an explicit range: the same rule
        let c = resolve("javascript", Some(">=22"), None, &old).unwrap();
        assert_eq!((c.img.image, c.fallback.map(|i| i.image)), ("node:26-alpine", Some("node:22-alpine")));
    }

    #[test]
    fn the_marker_names_only_listed_images() {
        assert_eq!(marked("elisa:missing-image=golang:1.27-alpine · the image is not here").unwrap().image, "golang:1.27-alpine");
        assert_eq!(marked("mcp x: not started (exit code 2): elisa:missing-image=node:26-alpine plugin p/s: …").unwrap().image, "node:26-alpine");
        assert!(marked("elisa:missing-image=evil/image:1").is_none());
        assert!(marked("the image golang:1.27-alpine is not on this machine").is_none());
    }

    #[test]
    fn layers_are_counted_not_echoed() {
        let mut l = Layers::default();
        assert!(!l.see("1.27-alpine: Pulling from library/golang"));
        assert!(l.see("6e771e15690e: Pulling fs layer"));
        assert!(l.see("aa2b3c4d5e6f: Pulling fs layer"));
        assert!(!l.see("6e771e15690e: Downloading"));
        assert!(l.see("6e771e15690e: Pull complete"));
        assert!(l.see("aa2b3c4d5e6f: Already exists"));
        assert!(!l.see("Digest: sha256:0123"));
        assert_eq!((l.done.len(), l.seen.len()), (2, 2));
        let p = PullProgress { image: "bash:5.3".into(), done: 1, layers: 3, ..PullProgress::default() };
        assert_eq!(p.label(), "downloading bash:5.3 · 1/3 layers");
    }

    #[test]
    fn targets_and_the_question() {
        let none = |_: &Img| false;
        let (v, again) = targets("stable", None, &none).unwrap();
        assert_eq!((v.len(), again), (5, false));
        let present_go = |i: &Img| i.lang == "go";
        assert!(targets("stable", None, &present_go).unwrap().0.iter().all(|i| i.lang != "go"));
        assert_eq!(targets("all", None, &none).unwrap().0.len(), IMAGES.len());
        let (v, again) = targets("go", None, &present_go).unwrap();
        assert_eq!((v[0].image, again), ("golang:1.27-alpine", true));
        assert!(targets("alpine:3", None, &none).unwrap_err().contains("not on Elisa's image list"));
        let g = stable("go");
        assert_eq!(question(&[g], Some(&needs(g))), "The Go 1.27 sandbox needs golang:1.27-alpine (~72 MB), downloaded into Colima's VM — nothing is installed on this Mac. Download it now?");
        assert!(question(&[g, stable("bash")], None).starts_with("Download 2 images (~79 MB in all)"));
    }

    #[test]
    fn the_lock_keeps_the_first_digest() {
        let h = dir("lock");
        let mut l = Lock::load(&h);
        assert!(l.get("bash:5.3").is_none());
        l.set("bash:5.3", &Local { id: "sha256:aaa".into(), bytes: 1, digest: Some("bash@sha256:bbb".into()) });
        l.save(&h).unwrap();
        assert_eq!(Lock::load(&h).get("bash:5.3").as_deref(), Some("bash@sha256:bbb"));
    }
}
