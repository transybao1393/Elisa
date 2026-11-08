//! Elisa's own security MCP server: `elisa --mcp-serve sec --root DIR [--max-chars N]`, a stdio MCP server with four
//! tools, all named `sec_*` (so `mcp_host` offers them unchanged; approval key `mcp:sec`):
//! - `sec_secrets {path?}`: walks the project (`.gitignore` respected, `.git` / `target` / `node_modules` / `.elisa`
//!   skipped) and reports `file:line rule masked` for each likely secret (`secrets::find`); never a full value.
//! - `sec_deps {}`: reads the lockfiles (Cargo.lock, package-lock.json / npm-shrinkwrap.json, requirements*.txt `==`
//!   pins, poetry.lock, uv.lock) and asks OSV (api.osv.dev) about every public-registry package. This is the one tool
//!   that uses the network: package names and versions (only) go to osv.dev; packages from git, paths or private
//!   registries are left out.
//! - `sec_binary {path}`: goblin's view of an executable / library (Mach-O incl. fat, ELF, PE).
//! - `sec_jwt {token}`: decodes a JWT's header and claims locally; the signature is never checked nor shown.
//! The session starts it like any other MCP server (`mcp_host.rs`) unless config `security = "off"`. MCP calls are
//! approved per server, not per path, so the server itself keeps to the project folder (`--root`, the session cwd):
//! every path is resolved (`..` and symlinks, `outside.rs`) and one outside the folder is refused; the walk never
//! follows a symlink. Output stays under `--max-chars` (the session's tool output cap).
use crate::outside;
use regex::Regex;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// the output cap when the session does not pass one (= the default `tool_output_max_chars`)
pub const SEC_CHARS: usize = 6000;
/// directories never walked
const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".elisa"];
/// the most files one walk visits
const MAX_FILES: usize = 20_000;
/// text files larger than this are not scanned for secrets (minified bundles, data dumps)
const SECRET_FILE_BYTES: u64 = 1024 * 1024;
/// a binary larger than this is not parsed
const BIN_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// OSV: the official API, and the tests' stand-in (accepted only on this machine: `osv_base`)
const OSV_URL: &str = "https://api.osv.dev";
pub const OSV_URL_ENV: &str = "ELISA_OSV_URL";
/// OSV: queries per batch request, advisories looked up in detail, seconds per request
const OSV_BATCH: usize = 1000;
const OSV_DETAILS: usize = 30;
const OSV_TIMEOUT_S: u64 = 20;
const OSV_BODY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SecCfg {
    /// the canonical project root: nothing outside it is read
    pub root: PathBuf,
    /// the most text one call returns
    pub max_chars: usize,
}

/// the tools this server lists (MCP `tools/list`); kept short for an 8K context
pub fn tools() -> Value {
    let path = json!({"type": "object", "properties": {"path": {"type": "string"}}});
    let t = vec![
        json!({"name": "sec_secrets", "description": "Find leaked secrets in project files (masked).", "inputSchema": path}),
        json!({"name": "sec_deps", "description": "Known vulnerabilities in lockfile deps. Sends names+versions to osv.dev.",
            "inputSchema": {"type": "object", "properties": {}}}),
        json!({"name": "sec_binary", "description": "Inspect an executable: format, arch, libs, imports, signing.",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}),
        json!({"name": "sec_jwt", "description": "Decode a JWT locally (signature not verified).",
            "inputSchema": {"type": "object", "properties": {"token": {"type": "string"}}, "required": ["token"]}}),
    ];
    Value::Array(t)
}

/// one `tools/call`: (text, is_error)
pub fn call(name: &str, args: &Value, cfg: &SecCfg) -> (String, bool) {
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let r = match name {
        "sec_secrets" => scan_secrets(arg("path").unwrap_or("."), cfg),
        "sec_deps" => audit_deps(cfg, &osv_base()),
        "sec_binary" => match arg("path") { Some(p) => inspect_binary(p, cfg), None => Err("missing path".into()) },
        "sec_jwt" => match arg("token") { Some(t) => decode_jwt(t, now_secs()), None => Err("missing token".into()) },
        n => Err(format!("unknown tool {n}")),
    };
    match r { Ok(t) => (cap(t, cfg.max_chars), false), Err(e) => (e, true) }
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, cfg: &SecCfg) -> Option<Value> {
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-sec", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], cfg);
            json!({"content": [{"type": "text", "text": text}], "isError": err})
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `--mcp-serve sec …`: serve stdin / stdout until stdin closes
pub fn serve(args: &[String]) -> i32 {
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let root = opt("--root").map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
    let max_chars = opt("--max-chars").and_then(|v| v.parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(SEC_CHARS);
    let cfg = SecCfg { root: outside::root(&root), max_chars };
    // a parser panic on a malformed binary is caught per call (`guard`); keep its message off stderr
    std::panic::set_hook(Box::new(|_| {}));
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(a) = answer(&m, &cfg) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
    0
}

// ---------- shared ----------

/// `t` cut to `n` characters at a line boundary, with a note
fn cap(t: String, n: usize) -> String {
    if t.chars().count() <= n { return t; }
    let room = n.saturating_sub(40);
    let cut: String = t.chars().take(room).collect();
    let cut = cut.rfind('\n').map_or(cut.as_str(), |i| &cut[..i]);
    format!("{cut}\n[… output cut at the size limit]")
}

/// `p` resolved inside the project; else the error for the model
fn project_path(p: &str, cfg: &SecCfg) -> Result<PathBuf, String> {
    let r = outside::resolve(&cfg.root, Path::new(p));
    if !outside::inside(&cfg.root, &r) {
        return Err(format!("{p} is outside the project folder; only paths inside {} can be checked", cfg.root.display()));
    }
    if !r.exists() { return Err(format!("no such file or folder: {p}")); }
    Ok(r)
}

fn rel(cfg: &SecCfg, p: &Path) -> String {
    let s = p.strip_prefix(&cfg.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
    if s.is_empty() { ".".into() } else { s }
}

fn is_binary(b: &[u8]) -> bool { b[..b.len().min(8192)].contains(&0) }

/// `f` with a parser panic turned into an error
fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(format!("{what}: the file looks damaged (the parser failed)")))
}

// ---------- the project walk, with .gitignore ----------

/// one `.gitignore` line
struct Pat { re: Regex, dir_only: bool, neg: bool }

/// the patterns of one `.gitignore`, for paths under `base` ("" = the root, else "dir/")
struct Layer { base: String, pats: Vec<Pat> }

/// a gitignore glob as a regex body: `**/` any directories, `*` / `?` within a name, `[…]` a class
fn glob_body(g: &str) -> String {
    let c: Vec<char> = g.chars().collect();
    let mut re = String::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '*' if c.get(i + 1) == Some(&'*') => {
                if c.get(i + 2) == Some(&'/') { re.push_str("(?:.*/)?"); i += 3; continue; }
                re.push_str(".*");
                i += 2;
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => match c[i + 1..].iter().position(|&x| x == ']') {
                Some(j) if j > 0 => {
                    let inner: String = c[i + 1..i + 1 + j].iter().collect();
                    let inner = inner.strip_prefix('!').map_or(inner.clone(), |r| format!("^{r}"));
                    re.push('[');
                    re.push_str(&inner.replace('\\', "\\\\"));
                    re.push(']');
                    i += j + 2;
                    continue;
                }
                _ => re.push_str("\\["),
            },
            '\\' if i + 1 < c.len() => { re.push_str(&regex::escape(&c[i + 1].to_string())); i += 2; continue; }
            ch => re.push_str(&regex::escape(&ch.to_string())),
        }
        i += 1;
    }
    re
}

fn parse_gitignore(text: &str) -> Vec<Pat> {
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim_end();
        if l.is_empty() || l.starts_with('#') { continue; }
        let (neg, l) = match l.strip_prefix('!') { Some(r) => (true, r), None => (false, l.strip_prefix('\\').unwrap_or(l)) };
        let (dir_only, l) = match l.strip_suffix('/') { Some(r) => (true, r), None => (false, l) };
        if l.is_empty() { continue; }
        let anchored = l.contains('/');
        let l = l.strip_prefix('/').unwrap_or(l);
        let re = format!("^{}{}$", if anchored { "" } else { "(?:.*/)?" }, glob_body(l));
        if let Ok(re) = Regex::new(&re) { out.push(Pat { re, dir_only, neg }); }
    }
    out
}

/// `rel` (slash path from the root) ignored by the layers: the last matching pattern wins
fn ignored(layers: &[Layer], rel: &str, is_dir: bool) -> bool {
    let mut ig = false;
    for l in layers {
        let Some(sub) = rel.strip_prefix(l.base.as_str()) else { continue };
        for p in &l.pats {
            if (!p.dir_only || is_dir) && p.re.is_match(sub) { ig = !p.neg; }
        }
    }
    ig
}

#[derive(Default)]
struct Walk { files: Vec<PathBuf>, capped: bool }

struct WalkOpt { gitignore: bool, skip: &'static [&'static str], max_files: usize }

const SECRETS_WALK: WalkOpt = WalkOpt { gitignore: true, skip: &SKIP_DIRS, max_files: MAX_FILES };

fn load_layer(dir: &Path, base: String) -> Option<Layer> {
    let t = std::fs::read_to_string(dir.join(".gitignore")).ok()?;
    let pats = parse_gitignore(&t);
    (!pats.is_empty()).then_some(Layer { base, pats })
}

/// the files under `start` (inside `root`), sorted by name, symlinks never followed
fn walk(root: &Path, start: &Path, opt: &WalkOpt) -> Walk {
    let mut w = Walk::default();
    if start.is_file() { w.files.push(start.to_path_buf()); return w; }
    // the .gitignore files from the root down to `start`
    let mut layers = Vec::new();
    let mut base = String::new();
    if opt.gitignore {
        if let Some(l) = load_layer(root, String::new()) { layers.push(l); }
        if let Ok(sub) = start.strip_prefix(root) {
            let mut dir = root.to_path_buf();
            for c in sub.components() {
                let name = c.as_os_str().to_string_lossy();
                dir.push(c);
                base.push_str(&name);
                base.push('/');
                if let Some(l) = load_layer(&dir, base.clone()) { layers.push(l); }
            }
        }
    } else if let Ok(sub) = start.strip_prefix(root) {
        for c in sub.components() { base.push_str(&c.as_os_str().to_string_lossy()); base.push('/'); }
    }
    visit(start, &base, &mut layers, opt, &mut w, 0);
    w
}

fn visit(dir: &Path, base: &str, layers: &mut Vec<Layer>, opt: &WalkOpt, w: &mut Walk, depth: usize) {
    if depth > 40 || w.capped { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() { continue; }
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("{base}{name}");
        if ft.is_dir() {
            if opt.skip.contains(&name.as_str()) || (opt.gitignore && ignored(layers, &rel, true)) { continue; }
            let sub = format!("{rel}/");
            let pushed = opt.gitignore && match load_layer(&e.path(), sub.clone()) { Some(l) => { layers.push(l); true } None => false };
            visit(&e.path(), &sub, layers, opt, w, depth + 1);
            if pushed { layers.pop(); }
            if w.capped { return; }
        } else if ft.is_file() {
            if opt.gitignore && ignored(layers, &rel, false) { continue; }
            if w.files.len() >= opt.max_files { w.capped = true; return; }
            w.files.push(e.path());
        }
    }
}

// ---------- D1: secrets ----------

fn scan_secrets(p: &str, cfg: &SecCfg) -> Result<String, String> {
    let start = project_path(p, cfg)?;
    let w = walk(&cfg.root, &start, &SECRETS_WALK);
    let (mut lines, mut scanned, mut used) = (Vec::new(), 0usize, 0usize);
    let room = cfg.max_chars.saturating_sub(200);
    let (mut hits, mut shown_files, mut files_with) = (0usize, 0usize, 0usize);
    for f in &w.files {
        if std::fs::metadata(f).map(|m| m.len() > SECRET_FILE_BYTES).unwrap_or(true) { continue; }
        let Ok(bytes) = std::fs::read(f) else { continue };
        if is_binary(&bytes) { continue; }
        scanned += 1;
        let text = String::from_utf8_lossy(&bytes);
        let found = crate::secrets::find(&text);
        if found.is_empty() { continue; }
        files_with += 1;
        let name = rel(cfg, f);
        let mut any = false;
        for h in found {
            hits += 1;
            let l = format!("{name}:{} {} {}", h.line, h.rule, h.masked);
            if used + l.len() + 1 > room { continue; }
            used += l.len() + 1;
            lines.push(l);
            any = true;
        }
        if any { shown_files += 1; }
    }
    let what = if start == cfg.root { "the project".to_string() } else { rel(cfg, &start) };
    let mut out = format!("[secrets in {what}: {scanned} text files scanned (.gitignore respected) · {hits} finding{} in {files_with} file{}]",
        if hits == 1 { "" } else { "s" }, if files_with == 1 { "" } else { "s" });
    if hits == 0 { out.push_str("\nno likely secrets found"); }
    for l in &lines { out.push('\n'); out.push_str(l); }
    if lines.len() < hits { out.push_str(&format!("\n[{} more not shown ({shown_files} files shown); narrow with path]", hits - lines.len())); }
    if w.capped { out.push_str(&format!("\n[stopped after {MAX_FILES} files; narrow with path]")); }
    Ok(out)
}

// ---------- D2: dependencies against OSV ----------

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pkg { pub eco: &'static str, pub name: String, pub version: String }

const CRATES: &str = "crates.io";
const NPM: &str = "npm";
const PYPI: &str = "PyPI";

fn pkg(eco: &'static str, name: &str, version: &str) -> Pkg { Pkg { eco, name: name.into(), version: version.into() } }

/// Cargo.lock: crates from crates.io only (git / path / other registries stay private)
pub fn parse_cargo_lock(t: &str) -> Vec<Pkg> {
    let Ok(v) = toml::from_str::<toml::Table>(t) else { return Vec::new() };
    v.get("package").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|p| {
        let src = p.get("source")?.as_str()?;
        let crates_io = src == "registry+https://github.com/rust-lang/crates.io-index" || src == "sparse+https://index.crates.io/";
        if !crates_io { return None; }
        Some(pkg(CRATES, p.get("name")?.as_str()?, p.get("version")?.as_str()?))
    }).collect()).unwrap_or_default()
}

/// an npm entry that comes from the public registry (or says nothing about where it came from)
fn npm_public(e: &Value) -> bool {
    if e["link"].as_bool() == Some(true) { return false; }
    match e["resolved"].as_str() { Some(r) => r.starts_with("https://registry.npmjs.org/"), None => true }
}

fn npm_version_ok(v: &str) -> bool { !v.is_empty() && v.chars().next().is_some_and(|c| c.is_ascii_digit()) }

/// package-lock.json / npm-shrinkwrap.json: v2 / v3 `packages`, else v1 `dependencies`
pub fn parse_package_lock(t: &str) -> Vec<Pkg> {
    let Ok(v) = serde_json::from_str::<Value>(t) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(m) = v["packages"].as_object() {
        for (k, e) in m {
            let Some(i) = k.rfind("node_modules/") else { continue };
            let name = e["name"].as_str().unwrap_or(&k[i + "node_modules/".len()..]);
            let ver = e["version"].as_str().unwrap_or("");
            if npm_public(e) && npm_version_ok(ver) && !name.is_empty() { out.push(pkg(NPM, name, ver)); }
        }
        return out;
    }
    fn v1(deps: &Value, out: &mut Vec<Pkg>, depth: usize) {
        let Some(m) = deps.as_object() else { return };
        if depth > 64 { return; }
        for (name, e) in m {
            let ver = e["version"].as_str().unwrap_or("");
            if npm_public(e) && npm_version_ok(ver) { out.push(pkg(NPM, name, ver)); }
            v1(&e["dependencies"], out, depth + 1);
        }
    }
    v1(&v["dependencies"], &mut out, 0);
    out
}

/// requirements*.txt: exact `name==version` pins only (no URLs, editables, options)
pub fn parse_requirements(t: &str) -> Vec<Pkg> {
    let name_ok = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    t.lines().filter_map(|l| {
        let l = l.split(" #").next()?.trim();
        if l.starts_with('-') || l.starts_with('#') || l.contains("://") || l.contains('@') || l.contains("===") { return None; }
        let l = l.split(';').next()?.trim();
        let (n, v) = l.split_once("==")?;
        let n = n.split('[').next()?.trim();
        let v = v.trim().split(|c: char| c == ',' || c.is_whitespace()).next()?;
        (name_ok(n) && !v.is_empty() && !v.contains('*')).then(|| pkg(PYPI, n, v))
    }).collect()
}

/// poetry.lock: packages without a `[package.source]` (those come from git, a path or a private index)
pub fn parse_poetry_lock(t: &str) -> Vec<Pkg> {
    let Ok(v) = toml::from_str::<toml::Table>(t) else { return Vec::new() };
    v.get("package").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|p| {
        if p.get("source").is_some() { return None; }
        Some(pkg(PYPI, p.get("name")?.as_str()?, p.get("version")?.as_str()?))
    }).collect()).unwrap_or_default()
}

/// uv.lock: packages whose source is the PyPI registry
pub fn parse_uv_lock(t: &str) -> Vec<Pkg> {
    let Ok(v) = toml::from_str::<toml::Table>(t) else { return Vec::new() };
    v.get("package").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|p| {
        let reg = p.get("source")?.get("registry")?.as_str()?;
        if !reg.starts_with("https://pypi.org/") { return None; }
        Some(pkg(PYPI, p.get("name")?.as_str()?, p.get("version")?.as_str()?))
    }).collect()).unwrap_or_default()
}

/// the lockfile parser for a file name
fn lock_parser(name: &str) -> Option<fn(&str) -> Vec<Pkg>> {
    Some(match name {
        "Cargo.lock" => parse_cargo_lock,
        "package-lock.json" | "npm-shrinkwrap.json" => parse_package_lock,
        "poetry.lock" => parse_poetry_lock,
        "uv.lock" => parse_uv_lock,
        n if n.starts_with("requirements") && n.ends_with(".txt") => parse_requirements,
        _ => return None,
    })
}

/// the OSV address: the real API, or a stand-in on this machine (tests: `ELISA_OSV_URL`). Any other override is
/// ignored, so package lists never go anywhere but osv.dev or localhost.
fn osv_base() -> String {
    match std::env::var(OSV_URL_ENV) { Ok(u) if local_url(&u) => u.trim_end_matches('/').to_string(), _ => OSV_URL.into() }
}

/// `http://127.0.0.1[:port][/…]` or `http://localhost[:port][/…]`
pub fn local_url(u: &str) -> bool {
    let Some(rest) = u.trim().strip_prefix("http://") else { return false };
    let hp = rest.split(['/', '?', '#']).next().unwrap_or("");
    if hp.contains('@') { return false; }
    let host = match hp.rsplit_once(':') { Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h, _ => hp };
    matches!(host, "127.0.0.1" | "localhost")
}

fn osv_agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0)
        .timeout_global(Some(Duration::from_secs(OSV_TIMEOUT_S))).build().into()
}

fn osv_json(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>, what: &str) -> Result<Value, String> {
    let mut r = r.map_err(|e| format!("cannot reach osv.dev ({what}): {e}"))?;
    let st = r.status().as_u16();
    let t = r.body_mut().with_config().limit(OSV_BODY_BYTES).read_to_string().map_err(|e| format!("osv.dev ({what}): {e}"))?;
    if !(200..300).contains(&st) { return Err(format!("osv.dev ({what}): HTTP {st}")); }
    serde_json::from_str(&t).map_err(|_| format!("osv.dev ({what}) did not answer with JSON"))
}

/// one advisory, as shown
struct Advisory { id: String, alias: String, severity: String, fixed: Vec<String>, summary: String }

fn advisory(id: &str, v: &Value, p: &Pkg) -> Advisory {
    let alias = v["aliases"].as_array().and_then(|a| a.iter().filter_map(Value::as_str).find(|s| s.starts_with("CVE-"))).unwrap_or("").to_string();
    let same = |a: &Value| a["package"]["ecosystem"].as_str() == Some(p.eco)
        && a["package"]["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(&p.name) || norm_py(n) == norm_py(&p.name));
    let affected: Vec<&Value> = v["affected"].as_array().map(|a| a.iter().filter(|a| same(a)).collect()).unwrap_or_default();
    let mut severity = v["database_specific"]["severity"].as_str().unwrap_or("").to_string();
    if severity.is_empty() {
        severity = affected.iter().find_map(|a| a["database_specific"]["severity"].as_str().or(a["database_specific"]["informational"].as_str()))
            .unwrap_or("").to_string();
    }
    if severity.is_empty() {
        severity = v["severity"].as_array().and_then(|s| s.first()).and_then(|s| s["type"].as_str()).map(|t| format!("{t} scored")).unwrap_or_default();
    }
    let mut fixed: Vec<String> = Vec::new();
    for a in &affected {
        for r in a["ranges"].as_array().into_iter().flatten() {
            for e in r["events"].as_array().into_iter().flatten() {
                if let Some(f) = e["fixed"].as_str() { if !fixed.iter().any(|x| x == f) { fixed.push(f.to_string()); } }
            }
        }
    }
    let summary = v["summary"].as_str().filter(|s| !s.trim().is_empty())
        .or_else(|| v["details"].as_str().and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty())))
        .unwrap_or("").to_string();
    Advisory { id: id.into(), alias, severity, fixed, summary: crate::context::one_line(&summary, 90) }
}

fn norm_py(n: &str) -> String { n.to_ascii_lowercase().replace(['_', '.'], "-") }

fn audit_deps(cfg: &SecCfg, base: &str) -> Result<String, String> {
    // the lockfiles: committed files at any depth (node_modules and the like skipped)
    let opt = WalkOpt { gitignore: true, skip: &SKIP_DIRS, max_files: MAX_FILES };
    let w = walk(&cfg.root, &cfg.root, &opt);
    let mut pkgs: Vec<Pkg> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for f in w.files.iter().filter(|f| f.components().count() - cfg.root.components().count() <= 5) {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(parse) = lock_parser(&name) else { continue };
        if sources.len() >= 20 { break; }
        let Ok(t) = std::fs::read_to_string(f) else { continue };
        let found = parse(&t);
        sources.push(format!("{} {}", rel(cfg, f), found.len()));
        pkgs.extend(found);
    }
    if sources.is_empty() {
        return Ok("no lockfiles found (Cargo.lock, package-lock.json, requirements*.txt with == pins, poetry.lock, uv.lock); nothing sent".into());
    }
    pkgs.sort();
    pkgs.dedup();
    if pkgs.is_empty() { return Ok(format!("[lockfiles: {}] no public-registry packages to check; nothing sent", sources.join(", "))); }
    let agent = osv_agent();
    // the batch: one id list per package, in order
    let mut ids: Vec<Vec<String>> = Vec::with_capacity(pkgs.len());
    for chunk in pkgs.chunks(OSV_BATCH) {
        let queries: Vec<Value> = chunk.iter().map(|p| json!({"package": {"name": p.name, "ecosystem": p.eco}, "version": p.version})).collect();
        let body = json!({"queries": queries}).to_string();
        let v = osv_json(agent.post(format!("{base}/v1/querybatch")).header("content-type", "application/json").send(body), "querybatch")?;
        let results = v["results"].as_array().ok_or("osv.dev: unexpected answer (no results)")?;
        for i in 0..chunk.len() {
            ids.push(results.get(i).and_then(|r| r["vulns"].as_array())
                .map(|a| a.iter().filter_map(|x| x["id"].as_str().map(String::from)).collect()).unwrap_or_default());
        }
    }
    let vulnerable = ids.iter().filter(|v| !v.is_empty()).count();
    let total_adv: usize = ids.iter().map(Vec::len).sum();
    let mut out = format!("[osv.dev: {} packages checked ({}) · {vulnerable} with known advisories ({total_adv})]", pkgs.len(), sources.join(", "));
    if vulnerable == 0 { out.push_str("\nno known vulnerabilities"); return Ok(out); }
    let mut details: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut skipped = 0usize;
    for (p, list) in pkgs.iter().zip(&ids) {
        for id in list {
            let v = match details.get(id) {
                Some(v) => Some(v.clone()),
                None if details.len() < OSV_DETAILS => {
                    let enc: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')).collect();
                    let v = osv_json(agent.get(format!("{base}/v1/vulns/{enc}")).call(), id).unwrap_or(Value::Null);
                    details.insert(id.clone(), v.clone());
                    Some(v)
                }
                None => { skipped += 1; None }
            };
            let line = match v.filter(|v| v.is_object()) {
                Some(v) => {
                    let a = advisory(id, &v, p);
                    let mut l = format!("{} {} → {}", p.name, p.version, a.id);
                    if !a.alias.is_empty() { l.push_str(&format!(" ({})", a.alias)); }
                    if !a.severity.is_empty() { l.push_str(&format!(" [{}]", a.severity)); }
                    l.push_str(&if a.fixed.is_empty() { " no fixed version".to_string() } else { format!(" fixed: {}", a.fixed.iter().take(3).cloned().collect::<Vec<_>>().join(", ")) });
                    if !a.summary.is_empty() { l.push_str(&format!(" — {}", a.summary)); }
                    l
                }
                None => format!("{} {} → {id}", p.name, p.version),
            };
            out.push('\n');
            out.push_str(&line);
        }
    }
    if skipped > 0 { out.push_str(&format!("\n[{skipped} advisories past the first {OSV_DETAILS} shown by id only]")); }
    Ok(out)
}

// ---------- D8: binaries ----------

/// imports worth pointing out (process injection, code loading, anti-debugging, networking, persistence)
const NOTABLE: [&str; 36] = [
    "VirtualAlloc", "VirtualAllocEx", "VirtualProtect", "WriteProcessMemory", "CreateRemoteThread", "NtUnmapViewOfSection",
    "LoadLibrary", "GetProcAddress", "URLDownloadToFile", "InternetOpen", "WinExec", "ShellExecute", "CreateProcess",
    "IsDebuggerPresent", "SetWindowsHookEx", "RegSetValueEx", "CryptEncrypt", "AdjustTokenPrivileges",
    "ptrace", "execve", "execv", "system", "popen", "fork", "dlopen", "dlsym", "mprotect", "socket", "connect",
    "setuid", "task_for_pid", "mach_vm_write", "posix_spawn", "NSCreateObjectFileImageFromMemory", "SecKeychainFindGenericPassword", "IOHIDManagerCreate",
];

/// `name` (leading underscores and a Windows A / W / Ex suffix aside) is one of [`NOTABLE`]
fn notable(name: &str) -> Option<&'static str> {
    let n = name.trim_start_matches('_');
    let n = n.split('@').next().unwrap_or(n);
    NOTABLE.iter().copied().find(|k| {
        n == *k || n.strip_prefix(k).is_some_and(|rest| matches!(rest, "A" | "W" | "ExA" | "ExW" | "Ex" | "64"))
    })
}

fn list(items: &[String], max: usize) -> String {
    let mut s = items.iter().take(max).cloned().collect::<Vec<_>>().join(", ");
    if items.len() > max { s.push_str(&format!(", … ({} more)", items.len() - max)); }
    s
}

fn notable_line(names: impl Iterator<Item = String>) -> String {
    let mut found: Vec<String> = Vec::new();
    for n in names { if let Some(k) = notable(&n) { if !found.iter().any(|f| f == k) { found.push(k.to_string()); } } }
    if found.is_empty() { String::new() } else { format!("\nnotable imports: {}", list(&found, 20)) }
}

fn macho_arch(cputype: u32, cpusubtype: u32) -> String {
    use goblin::mach::cputype::get_arch_name_from_types;
    get_arch_name_from_types(cputype, cpusubtype & 0x00ff_ffff).map(String::from).unwrap_or_else(|| match cputype {
        0x0100_000c => "arm64".into(), 0x0100_0007 => "x86_64".into(), 7 => "i386".into(), 12 => "arm".into(),
        t => format!("cputype {t:#x}"),
    })
}

fn describe_macho(m: &goblin::mach::MachO) -> String {
    use goblin::mach::{header, load_command, symbols};
    let h = &m.header;
    let signed = m.load_commands.iter().any(|lc| lc.command.cmd() == load_command::LC_CODE_SIGNATURE);
    let encrypted = m.load_commands.iter().any(|lc| matches!(lc.command.cmd(), load_command::LC_ENCRYPTION_INFO | load_command::LC_ENCRYPTION_INFO_64));
    let (mut local, mut total) = (0usize, 0usize);
    for (_, nl) in m.symbols().flatten() {
        total += 1;
        if nl.n_type & symbols::N_STAB != 0 || (nl.n_type & symbols::N_EXT == 0 && nl.n_type & symbols::N_TYPE != symbols::N_UNDF) { local += 1; }
    }
    let libs: Vec<String> = m.libs.iter().filter(|l| **l != "self").map(|l| l.to_string()).collect();
    let imports: Vec<String> = m.imports().map(|v| v.iter().map(|i| i.name.to_string()).collect()).unwrap_or_default();
    let mut s = format!("Mach-O {} {}{}{}", macho_arch(h.cputype, h.cpusubtype), header::filetype_to_str(h.filetype),
        if m.is_64 { ", 64-bit" } else { ", 32-bit" }, if h.flags & header::MH_PIE != 0 { ", PIE" } else { "" });
    s.push_str(&format!("\ncode signature: {}", if signed { "present (LC_CODE_SIGNATURE; not validated)" } else { "none" }));
    if encrypted { s.push_str("\nencryption info: present (App Store style)"); }
    if m.entry != 0 { s.push_str(&format!("\nentry: {:#x}{}", m.entry, if m.old_style_entry { " (LC_UNIXTHREAD)" } else { "" })); }
    s.push_str(&format!("\nsymbols: {total} ({})", if local == 0 { "stripped: no local symbols" } else { "not stripped" }));
    s.push_str(&format!("\nlinked libraries ({}): {}", libs.len(), list(&libs, 15)));
    if !m.rpaths.is_empty() { s.push_str(&format!("\nrpaths: {}", list(&m.rpaths.iter().map(|r| r.to_string()).collect::<Vec<_>>(), 5))); }
    s.push_str(&format!("\nimports: {}", imports.len()));
    s.push_str(&notable_line(imports.into_iter()));
    s
}

fn describe_elf(e: &goblin::elf::Elf) -> String {
    use goblin::elf::header;
    let imports: Vec<String> = e.dynsyms.iter().filter(|s| s.is_import()).filter_map(|s| e.dynstrtab.get_at(s.st_name).map(String::from)).filter(|n| !n.is_empty()).collect();
    let pie = e.header.e_type == header::ET_DYN && e.interpreter.is_some();
    let mut s = format!("ELF {} {}{}{}", header::machine_to_str(e.header.e_machine), header::et_to_str(e.header.e_type),
        if e.is_64 { ", 64-bit" } else { ", 32-bit" }, if pie { ", PIE" } else { "" });
    match e.interpreter { Some(i) => s.push_str(&format!("\ninterpreter: {i}")), None if e.dynamic.is_none() => s.push_str("\nstatically linked"), None => {} }
    if let Some(n) = e.soname { s.push_str(&format!("\nsoname: {n}")); }
    if e.entry != 0 { s.push_str(&format!("\nentry: {:#x}", e.entry)); }
    s.push_str(&format!("\nsymbols: {}", if e.syms.is_empty() { "stripped (no .symtab)".to_string() } else { format!("{} (not stripped)", e.syms.len()) }));
    s.push_str("\ncode signature: ELF has none built in");
    let libs: Vec<String> = e.libraries.iter().map(|l| l.to_string()).collect();
    s.push_str(&format!("\nlinked libraries ({}): {}", libs.len(), list(&libs, 15)));
    if !e.runpaths.is_empty() || !e.rpaths.is_empty() {
        s.push_str(&format!("\nrpath/runpath: {}", list(&e.rpaths.iter().chain(&e.runpaths).map(|r| r.to_string()).collect::<Vec<_>>(), 5)));
    }
    s.push_str(&format!("\nimports: {}", imports.len()));
    s.push_str(&notable_line(imports.into_iter()));
    s
}

fn describe_pe(p: &goblin::pe::PE) -> String {
    let machine = goblin::pe::header::machine_to_str(p.header.coff_header.machine);
    let mut s = format!("PE {machine} {}{}", if p.is_lib { "DLL" } else { "executable" }, if p.is_64 { ", 64-bit (PE32+)" } else { ", 32-bit" });
    s.push_str(&format!("\nAuthenticode signature: {}", if p.certificates.is_empty() { "none".to_string() } else { format!("present ({} certificate entr{}; not validated)", p.certificates.len(), if p.certificates.len() == 1 { "y" } else { "ies" }) }));
    if p.entry != 0 { s.push_str(&format!("\nentry (RVA): {:#x}", p.entry)); }
    s.push_str(&format!("\ndebug info: {}", if p.debug_data.is_some() { "present" } else { "none" }));
    let libs: Vec<String> = p.libraries.iter().map(|l| l.to_string()).collect();
    s.push_str(&format!("\nlinked libraries ({}): {}", libs.len(), list(&libs, 15)));
    s.push_str(&format!("\nimports: {}", p.imports.len()));
    s.push_str(&notable_line(p.imports.iter().map(|i| i.name.to_string())));
    s
}

fn inspect_binary(p: &str, cfg: &SecCfg) -> Result<String, String> {
    let path = project_path(p, cfg)?;
    let md = std::fs::metadata(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    if md.is_dir() { return Err(format!("{p} is a directory")); }
    if md.len() > BIN_FILE_BYTES { return Err(format!("{p} is too large ({} MB; the limit is {} MB)", md.len() >> 20, BIN_FILE_BYTES >> 20)); }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    let body = guard(p, || {
        use goblin::Object;
        Ok(match Object::parse(&bytes).map_err(|e| format!("{p}: not a binary goblin can read ({e})"))? {
            Object::Mach(goblin::mach::Mach::Binary(m)) => describe_macho(&m),
            Object::Mach(goblin::mach::Mach::Fat(fat)) => {
                let mut s = format!("Mach-O universal (fat) binary, {} architecture{}", fat.narches, if fat.narches == 1 { "" } else { "s" });
                for (i, a) in (&fat).into_iter().enumerate().take(8) {
                    match a {
                        Ok(goblin::mach::SingleArch::MachO(m)) => s.push_str(&format!("\n--- slice {} ---\n{}", i + 1, describe_macho(&m))),
                        Ok(goblin::mach::SingleArch::Archive(_)) => s.push_str(&format!("\n--- slice {} --- static archive", i + 1)),
                        Err(e) => s.push_str(&format!("\n--- slice {} --- unreadable: {e}", i + 1)),
                    }
                }
                s
            }
            Object::Elf(e) => describe_elf(&e),
            Object::PE(pe) => describe_pe(&pe),
            Object::Archive(a) => format!("static library archive (ar), {} members", a.len()),
            Object::COFF(_) => "COFF object file".to_string(),
            Object::TE(_) => "TE (terse executable) image".to_string(),
            Object::Unknown(magic) => return Err(format!("{p}: not a Mach-O, ELF or PE file (magic {magic:#x})")),
            _ => return Err(format!("{p}: unsupported binary format")),
        })
    })?;
    Ok(format!("[{p}: {} bytes]\n{body}", md.len()))
}

// ---------- D9: JWT ----------

/// base64url (padding optional; plain base64 `+` / `/` also accepted)
pub fn b64url(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b { b'A'..=b'Z' => b - b'A', b'a'..=b'z' => b - b'a' + 26, b'0'..=b'9' => b - b'0' + 52, b'-' | b'+' => 62, b'_' | b'/' => 63, _ => return None };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8); acc &= (1 << bits) - 1; }
    }
    (bits < 6).then_some(out)
}

fn now_secs() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64) }

/// days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days)
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// unix seconds as `2026-11-08 17:05:00 UTC`
pub fn utc(t: i64) -> String {
    let (y, m, d) = civil(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
}

/// a span of seconds in the biggest sensible unit
fn span(s: i64) -> String {
    let s = s.abs();
    match s {
        0..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        7200..=172_799 => format!("{} h", s / 3600),
        _ => format!("{} days", s / 86400),
    }
}

fn claim_str(v: &Value) -> String {
    match v {
        Value::String(s) => crate::context::one_line(s, 120),
        Value::Array(a) => crate::context::one_line(&a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), String::from)).collect::<Vec<_>>().join(", "), 160),
        v => crate::context::one_line(&v.to_string(), 120),
    }
}

/// `sec_jwt`: header and claims of a JWT, decoded here; `now` = unix seconds. The signature is not verified and its
/// text is never repeated.
pub fn decode_jwt(token: &str, now: i64) -> Result<String, String> {
    let t = token.trim();
    let t = t.strip_prefix("Bearer ").or_else(|| t.strip_prefix("bearer ")).unwrap_or(t).trim();
    let parts: Vec<&str> = t.split('.').collect();
    let obj = |s: &str, what: &str| -> Result<serde_json::Map<String, Value>, String> {
        let b = b64url(s).ok_or_else(|| format!("the {what} is not base64url"))?;
        match serde_json::from_slice::<Value>(&b) { Ok(Value::Object(m)) => Ok(m), _ => Err(format!("the {what} is not a JSON object")) }
    };
    if parts.len() == 5 {
        let h = obj(parts[0], "header")?;
        return Ok(format!("an encrypted JWT (JWE: alg {}, enc {}): the claims cannot be read without the key",
            claim_str(h.get("alg").unwrap_or(&Value::Null)), claim_str(h.get("enc").unwrap_or(&Value::Null))));
    }
    if parts.len() != 3 { return Err(format!("not a JWT: expected 3 dot-separated parts, got {}", parts.len())); }
    let header = obj(parts[0], "header")?;
    let claims = obj(parts[1], "claims part")?;
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    let mut out = String::from("JWT decoded locally. The signature is NOT verified: do not trust these claims.\n");
    out.push_str(&format!("alg: {}", if alg.is_empty() { "(missing)" } else { alg }));
    for k in ["typ", "kid", "cty"] { if let Some(v) = header.get(k) { out.push_str(&format!(" · {k}: {}", claim_str(v))); } }
    if alg.eq_ignore_ascii_case("none") || parts[2].is_empty() {
        out.push_str(&format!("\nWARNING: {} — anyone can forge this token; a server that accepts it is vulnerable",
            if alg.eq_ignore_ascii_case("none") { "alg \"none\" (unsigned)" } else { "no signature" }));
    } else if alg.starts_with("HS") {
        out.push_str("\nnote: HMAC (shared secret): anyone with the secret can mint tokens");
    }
    for k in ["iss", "sub", "aud"] { if let Some(v) = claims.get(k) { out.push_str(&format!("\n{k}: {}", claim_str(v))); } }
    for (k, label) in [("iat", "issued"), ("nbf", "not before"), ("exp", "expires")] {
        let Some(v) = claims.get(k) else { continue };
        let Some(t) = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)) else { out.push_str(&format!("\n{k}: {} (not a number)", claim_str(v))); continue };
        let mut l = format!("\n{k} ({label}): {}", utc(t));
        match k {
            "exp" if t <= now => l.push_str(&format!(" — EXPIRED {} ago", span(now - t))),
            "exp" => l.push_str(&format!(" — valid for {} more", span(t - now))),
            "nbf" if t > now => l.push_str(&format!(" — NOT YET VALID (in {})", span(t - now))),
            "iat" if t > now + 300 => l.push_str(" — in the future (clock skew or forged)"),
            _ => {}
        }
        out.push_str(&l);
    }
    if !claims.contains_key("exp") { out.push_str("\nexp: none — this token never expires"); }
    let other: Vec<String> = claims.iter().filter(|(k, _)| !matches!(k.as_str(), "iss" | "sub" | "aud" | "iat" | "nbf" | "exp"))
        .take(20).map(|(k, v)| format!("{k}={}", crate::context::one_line(&claim_str(v), 60))).collect();
    if !other.is_empty() { out.push_str(&format!("\nother claims: {}", other.join(" · "))); }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-sec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(root: &Path) -> SecCfg { SecCfg { root: outside::root(root), max_chars: SEC_CHARS } }

    #[test]
    fn speaks_mcp_and_lists_short_tools() {
        let d = tmp("mcp");
        let c = cfg(&d);
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), &c).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-sec");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &c).is_none());
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &c).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["sec_secrets", "sec_deps", "sec_binary", "sec_jwt"]);
        // an 8K context: the whole list stays small (~3.3 characters a token)
        let n = tools().to_string().len();
        assert!(n < 1000, "{n}");
        assert_eq!(answer(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), &c).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn gitignore_patterns() {
        let l = vec![Layer { base: String::new(), pats: parse_gitignore("# c\n*.log\n!keep.log\n/build/\nsecret?.txt\ndocs/**/*.tmp\n[ab].x\n") }];
        assert!(ignored(&l, "a.log", false) && ignored(&l, "x/y/a.log", false));
        assert!(!ignored(&l, "keep.log", false));
        assert!(ignored(&l, "build", true) && !ignored(&l, "build", false) && !ignored(&l, "src/build", true));
        assert!(ignored(&l, "secret1.txt", false) && !ignored(&l, "secret12.txt", false));
        assert!(ignored(&l, "docs/a/b/c.tmp", false) && ignored(&l, "docs/c.tmp", false) && !ignored(&l, "x/docs/c.tmp", false));
        assert!(ignored(&l, "a.x", false) && !ignored(&l, "c.x", false));
        // a nested layer applies under its own folder only
        let l2 = vec![Layer { base: "sub/".into(), pats: parse_gitignore("*.env\n") }];
        assert!(ignored(&l2, "sub/a.env", false) && !ignored(&l2, "a.env", false));
    }

    #[test]
    fn walk_respects_gitignore_skips_and_symlinks() {
        let d = tmp("walk");
        for (p, t) in [("a.txt", "x"), ("ignored.log", "x"), ("sub/b.txt", "x"), ("sub/c.env", "x"), ("target/t.txt", "x"), ("node_modules/n.js", "x")] {
            let f = d.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        std::fs::write(d.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(d.join("sub/.gitignore"), "*.env\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", d.join("etc")).unwrap();
        let root = outside::root(&d);
        let w = walk(&root, &root, &SECRETS_WALK);
        let names: Vec<String> = w.files.iter().map(|f| f.strip_prefix(&root).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, [".gitignore", "a.txt", "sub/.gitignore", "sub/b.txt"]);
        // started in a subfolder: the root's .gitignore still applies
        std::fs::write(d.join("sub/x.log"), "x").unwrap();
        let w = walk(&root, &root.join("sub"), &SECRETS_WALK);
        assert_eq!(w.files.len(), 2, "{:?}", w.files);
    }

    #[test]
    fn lockfile_parsers_keep_public_packages_only() {
        let cargo = r#"
version = 4
[[package]]
name = "time"
version = "0.1.45"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "mine"
version = "0.1.0"
[[package]]
name = "fork"
version = "1.0.0"
source = "git+https://example.com/fork#abc"
"#;
        assert_eq!(parse_cargo_lock(cargo), [pkg(CRATES, "time", "0.1.45")]);
        let npm3 = r#"{"lockfileVersion":3,"packages":{"":{"name":"app","version":"1.0.0"},
            "node_modules/lodash":{"version":"4.17.20","resolved":"https://registry.npmjs.org/lodash/-/lodash-4.17.20.tgz"},
            "node_modules/a/node_modules/@scope/b":{"version":"2.0.0"},
            "node_modules/local":{"resolved":"../local","link":true},
            "node_modules/priv":{"version":"1.0.0","resolved":"https://npm.corp.example/priv-1.0.0.tgz"}}}"#;
        assert_eq!(parse_package_lock(npm3), [pkg(NPM, "lodash", "4.17.20"), pkg(NPM, "@scope/b", "2.0.0")]);
        let npm1 = r#"{"lockfileVersion":1,"dependencies":{"minimist":{"version":"0.0.8","dependencies":{"x":{"version":"1.2.3"}}},"g":{"version":"github:u/g#1"}}}"#;
        assert_eq!(parse_package_lock(npm1), [pkg(NPM, "minimist", "0.0.8"), pkg(NPM, "x", "1.2.3")]);
        let req = "# deps\nDjango==3.2.0\nrequests[socks]==2.19.0 ; python_version>'3' # note\nflask>=2\n-e .\npkg @ https://x/y.whl\nweird===1\nnumpy == 1.21.0\n";
        assert_eq!(parse_requirements(req), [pkg(PYPI, "Django", "3.2.0"), pkg(PYPI, "requests", "2.19.0"), pkg(PYPI, "numpy", "1.21.0")]);
        let poetry = "[[package]]\nname = \"jinja2\"\nversion = \"2.10\"\n[[package]]\nname = \"inhouse\"\nversion = \"1.0\"\n[package.source]\ntype = \"legacy\"\nurl = \"https://pypi.corp\"\n";
        assert_eq!(parse_poetry_lock(poetry), [pkg(PYPI, "jinja2", "2.10")]);
        let uv = "version = 1\n[[package]]\nname = \"urllib3\"\nversion = \"1.26.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n";
        assert_eq!(parse_uv_lock(uv), [pkg(PYPI, "urllib3", "1.26.0")]);
        assert!(lock_parser("requirements-dev.txt").is_some() && lock_parser("README.txt").is_none());
    }

    #[test]
    fn osv_override_only_on_this_machine() {
        assert!(local_url("http://127.0.0.1:8080") && local_url("http://localhost/") && local_url("http://127.0.0.1"));
        assert!(!local_url("https://127.0.0.1:1") && !local_url("http://evil.com") && !local_url("http://127.0.0.1:80@evil.com"));
        assert!(!local_url("http://127.0.0.1.evil.com") && !local_url("http://localhost.evil.com:1/") && !local_url("http://10.0.0.1"));
    }

    #[test]
    fn advisory_fields() {
        let v = json!({"id": "GHSA-x", "aliases": ["CVE-2021-1"], "summary": "Prototype pollution",
            "database_specific": {"severity": "HIGH"},
            "affected": [{"package": {"ecosystem": "npm", "name": "lodash"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "4.17.21"}]}]},
                         {"package": {"ecosystem": "npm", "name": "other"}, "ranges": [{"events": [{"fixed": "9"}]}]}]});
        let a = advisory("GHSA-x", &v, &pkg(NPM, "lodash", "4.17.20"));
        assert_eq!((a.alias.as_str(), a.severity.as_str(), a.fixed.clone(), a.summary.as_str()), ("CVE-2021-1", "HIGH", vec!["4.17.21".to_string()], "Prototype pollution"));
    }

    #[test]
    fn jwt_decodes_and_warns() {
        let enc = |v: &Value| {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let b = v.to_string().into_bytes();
            let mut s = String::new();
            for ch in b.chunks(3) {
                let n = (u32::from(ch[0]) << 16) | (u32::from(*ch.get(1).unwrap_or(&0)) << 8) | u32::from(*ch.get(2).unwrap_or(&0));
                for i in 0..=ch.len() { s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char); }
            }
            s
        };
        let now = 1_800_000_000;
        let t = format!("{}.{}.SIGNATUREPART", enc(&json!({"alg": "RS256", "typ": "JWT"})),
            enc(&json!({"iss": "https://id.example", "sub": "u1", "aud": ["a", "b"], "iat": now - 3600, "exp": now - 60, "role": "admin"})));
        let out = decode_jwt(&format!("Bearer {t}"), now).unwrap();
        assert!(out.contains("NOT verified") && out.contains("alg: RS256") && out.contains("aud: a, b"), "{out}");
        assert!(out.contains("EXPIRED 60 s ago") && out.contains("role=admin"), "{out}");
        assert!(!out.contains("SIGNATUREPART"), "{out}");
        let none = format!("{}.{}.", enc(&json!({"alg": "none"})), enc(&json!({"sub": "x", "exp": now + 7200})));
        let out = decode_jwt(&none, now).unwrap();
        assert!(out.contains("WARNING") && out.contains("valid for 2 h more"), "{out}");
        assert!(decode_jwt("a.b", now).unwrap_err().contains("3 dot-separated"));
        assert!(decode_jwt("!!.e30.x", now).unwrap_err().contains("base64url"));
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(b64url("aGk").unwrap(), b"hi");
    }

    #[test]
    fn inspects_this_test_binary_and_refuses_outside() {
        // the running test executable is a real Mach-O / ELF; copied into a project folder
        let d = tmp("bin");
        std::fs::copy(std::env::current_exe().unwrap(), d.join("prog")).unwrap();
        let c = cfg(&d);
        let out = inspect_binary("prog", &c).unwrap();
        assert!(out.contains("Mach-O") || out.contains("ELF"), "{out}");
        assert!(out.contains("linked libraries") && out.contains("imports:"), "{out}");
        std::fs::write(d.join("text.txt"), "hello there, not a binary").unwrap();
        assert!(inspect_binary("text.txt", &c).is_err());
        assert!(inspect_binary("../x", &c).unwrap_err().contains("outside the project"));
        assert!(inspect_binary("/etc/hosts", &c).unwrap_err().contains("outside the project"));
        assert_eq!(notable("_dlopen"), Some("dlopen"));
        assert_eq!(notable("CreateProcessW"), Some("CreateProcess"));
        assert_eq!(notable("systemd_thing"), None);
    }
}
