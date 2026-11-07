//! Check before done: after a turn in which the model edited files, run the project's own checks (build / type / lint
//! first, then cheap tests) and hand failures back to the model for a bounded number of fix rounds. From forheader
//! test G13 (compile + test the answer, regenerate on failure: 98/101 → 100/101 and 93/100 → 95/100 on the 12 GB model).
//!
//! Detection is per language of the edited files (nothing runs for a turn that only edited docs):
//! - Rust (`.rs`, `Cargo.toml`): `cargo check`, then `cargo clippy -- -D warnings` (if clippy is installed), then
//!   `cargo test -q` of the edited crate (test stage)
//! - Python (`.py`): `python3 -m py_compile <files>`, `ruff check <files>` (if ruff is installed), then
//!   `pytest -x -q <edited test files>` (test stage, only for edited `test_*.py` / `*_test.py`, if pytest is installed)
//! - Swift (`.swift` under a `Package.swift`): `swift build`
//! - JS/TS (`.ts` `.tsx` `.js` …, with `tsconfig.json` in the project root): `tsc --noEmit -p .`; with a `package.json`
//!   whose `scripts.test` is a real test script (not npm's "no test specified" stub): `npm test --silent` (test stage)
//! - Go (`.go` under a `go.mod`): `go vet ./...`, then `go test ./...` of that module (test stage), if go is installed
//!
//! The languages follow the edited files, so a session in any language (src/lang.rs) gets its own tools; a language
//! without built-in detection runs nothing unless `.elisa/checks.toml` names its commands.
//!
//! `.elisa/checks.toml` overrides detection and the config keys:
//! ```toml
//! enabled = true                     # false: no checks in this project
//! rounds = 2                         # most fix rounds
//! timeout_s = 120                    # per fast check
//! test_timeout_s = 60                # per test check
//! commands = ["make lint"]           # fast checks, in order (replace detection when commands or tests is given)
//! tests = ["make test-quick"]        # run after every fast check passed
//! ```
//! Every check runs in its own process group with a timeout and an output cap (`tools::shell`). A timed-out check is
//! reported and ends the checks of this turn without a fix round (a slow build is not something the model can fix).
use crate::ast::{self, Lang, Touched};
use crate::config::Config;
use crate::frontend::CheckStatus;
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// bytes of check output kept in RAM (the rest is dropped while the command runs on)
pub const OUTPUT_BUF_MAX: usize = 1 << 20;
/// error lines kept in a failure signature (the repeat test) and in the error block sent back
const SIG_LINES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage { Fast, Test }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// short display name, e.g. `cargo clippy`
    pub name: String,
    /// the zsh command line
    pub cmd: String,
    pub stage: Stage,
    pub timeout_s: u64,
}

/// What runs after this turn's edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckSet { pub enabled: bool, pub rounds: usize, pub checks: Vec<Check> }

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
struct ChecksFile {
    enabled: Option<bool>,
    rounds: Option<usize>,
    timeout_s: Option<u64>,
    test_timeout_s: Option<u64>,
    commands: Option<Vec<String>>,
    tests: Option<Vec<String>>,
}

/// What a supported code file looked like before a write / edit (for `ast::touched`): `old` None = a new file.
#[derive(Clone, Debug)]
pub struct Before { pub path: PathBuf, pub lang: Lang, pub old: Option<String> }

/// The file a Write tool is about to change, when it is a supported code file of parseable size.
pub fn before(cwd: &Path, input: &serde_json::Value) -> Option<Before> {
    let p = input.get("path").and_then(|v| v.as_str()).filter(|p| !p.is_empty())?;
    let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) };
    let lang = Lang::of(&path)?;
    let old = match std::fs::metadata(&path) {
        Ok(m) if m.len() as usize > ast::MAX_BYTES => return None,
        Ok(_) => Some(std::fs::read_to_string(&path).ok()?),
        Err(_) => None,
    };
    Some(Before { path, lang, old })
}

/// The symbols this turn's edits touched, per file (absolute path). A file edited in the turn but missing here (not a
/// supported language, too big, targeting off) makes its checks fall back to the untargeted ones.
pub type Touches = Vec<(PathBuf, Touched)>;

/// Adds one successful edit to `t` (`b` from `before`, taken before the tool ran).
pub fn add_touch(t: &mut Touches, b: Before) {
    let Ok(new) = std::fs::read_to_string(&b.path) else { return };
    // the first edit of a file in the turn decides "new file"; later edits add symbols
    let Some(x) = ast::touched(b.lang, b.old.as_deref(), &new) else { return };
    match t.iter_mut().find(|(p, _)| *p == b.path) {
        Some((_, have)) => {
            have.outside |= x.outside;
            for s in x.symbols { if !have.symbols.contains(&s) { have.symbols.push(s); } }
        }
        None => t.push((b.path, x)),
    }
}

/// The checks for files edited in this turn. `off` = `--no-checks`. Errors: a broken `.elisa/checks.toml`.
pub fn select(cfg: &Config, cwd: &Path, edited: &[PathBuf], off: bool) -> Result<CheckSet, String> { select_with(cfg, cwd, edited, &[], off) }

/// `select`, with the symbols the edits touched: with config `ast_targeted_checks` the detected test checks narrow to
/// them (`detect_with`). `.elisa/checks.toml` commands are never changed.
pub fn select_with(cfg: &Config, cwd: &Path, edited: &[PathBuf], touches: &[(PathBuf, Touched)], off: bool) -> Result<CheckSet, String> {
    let p = cwd.join(".elisa/checks.toml");
    let f: ChecksFile = if p.is_file() {
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?
    } else { ChecksFile::default() };
    let enabled = !off && f.enabled.unwrap_or(cfg.checks);
    let rounds = f.rounds.unwrap_or(cfg.check_rounds);
    let (fast_t, test_t) = (f.timeout_s.unwrap_or(cfg.check_timeout_s), f.test_timeout_s.unwrap_or(cfg.check_test_timeout_s));
    if !enabled || edited.is_empty() { return Ok(CheckSet { enabled, rounds, checks: vec![] }); }
    let mut checks = if f.commands.is_some() || f.tests.is_some() {
        let mk = |c: &String, stage| Check { name: crate::context::one_line(c, 60), cmd: c.clone(), stage, timeout_s: 0 };
        let mut v: Vec<Check> = f.commands.iter().flatten().filter(|c| !c.trim().is_empty()).map(|c| mk(c, Stage::Fast)).collect();
        v.extend(f.tests.iter().flatten().filter(|c| !c.trim().is_empty()).map(|c| mk(c, Stage::Test)));
        v
    } else {
        let t: &[(PathBuf, Touched)] = if cfg.ast_targeted_checks { touches } else { &[] };
        detect_with(cwd, edited, t, on_path)
    };
    for c in &mut checks { c.timeout_s = if c.stage == Stage::Fast { fast_t } else { test_t }; }
    // fast checks first, then tests (stable within each stage)
    checks.sort_by_key(|c| c.stage == Stage::Test);
    Ok(CheckSet { enabled, rounds, checks })
}

/// whether `exe` is on PATH
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

/// `'a b.py'` for zsh
fn q(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) { return s.to_string(); }
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn ext(p: &Path) -> &str { p.extension().and_then(|e| e.to_str()).unwrap_or("") }

/// the nearest directory from `p`'s parent up to `root` (inclusive) that holds `marker`
fn nearest(root: &Path, p: &Path, marker: &str) -> Option<PathBuf> {
    let mut d = p.parent()?;
    loop {
        if d.join(marker).is_file() { return Some(d.to_path_buf()); }
        if d == root { return None; }
        d = d.parent().filter(|x| x.starts_with(root))?;
    }
}

fn rel(root: &Path, p: &Path) -> String {
    let r = p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned();
    if r.is_empty() { ".".into() } else { r }
}

fn has_package(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest).is_ok_and(|s| s.lines().any(|l| l.trim() == "[package]"))
}

/// Built-in detection for the edited files (absolute paths under `cwd`). `has(exe)` = the tool is installed.
pub fn detect(cwd: &Path, edited: &[PathBuf], has: impl Fn(&str) -> bool) -> Vec<Check> { detect_with(cwd, edited, &[], has) }

/// `detect`, narrowed by the symbols the edits touched (`touches`; empty = untargeted):
/// - Rust: `cargo test` of each owning crate gets a test-name filter when every touched symbol there is a test or has
///   tests named after it (`cargo test -q -- split_words`); else (or for a new file, a change outside symbols, a
///   symbol without a named test) the whole crate's tests run as before
/// - Python: pytest also runs the test files that import a touched symbol's module and name the symbol
pub fn detect_with(cwd: &Path, edited: &[PathBuf], touches: &[(PathBuf, Touched)], has: impl Fn(&str) -> bool) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    let mut push = |name: &str, cmd: String, stage| {
        if !out.iter().any(|c| c.cmd == cmd) { out.push(Check { name: name.into(), cmd, stage, timeout_s: 0 }); }
    };
    let files: Vec<&PathBuf> = edited.iter().filter(|p| p.starts_with(cwd)).collect();

    // Rust: one check + clippy for the workspace at cwd, or per crate when cwd is not a cargo project
    let crates: Vec<PathBuf> = files.iter()
        .filter(|p| ext(p) == "rs" || p.file_name().is_some_and(|n| n == "Cargo.toml"))
        .filter_map(|p| nearest(cwd, p, "Cargo.toml")).fold(vec![], |mut v, d| { if !v.contains(&d) { v.push(d); } v });
    if !crates.is_empty() {
        let mp = |d: &Path| if d == cwd { String::new() } else { format!(" --manifest-path {}", q(&format!("{}/Cargo.toml", rel(cwd, d)))) };
        let roots: Vec<PathBuf> = if cwd.join("Cargo.toml").is_file() { vec![cwd.to_path_buf()] } else { crates.clone() };
        let clippy = has("cargo-clippy");
        for r in &roots { push("cargo check", format!("cargo check{} --message-format=short", mp(r)), Stage::Fast); }
        if clippy {
            for r in &roots { push("cargo clippy", format!("cargo clippy{} --message-format=short -- -D warnings", mp(r)), Stage::Fast); }
        }
        for c in crates.iter().filter(|d| has_package(&d.join("Cargo.toml"))) {
            let mine: Vec<&PathBuf> = files.iter().copied().filter(|f| nearest(cwd, f, "Cargo.toml").as_deref() == Some(c.as_path())).collect();
            let filt = if touches.is_empty() { None } else { rust_filters(c, &mine, touches) };
            match filt {
                Some(f) => push("cargo test", format!("cargo test -q{} -- {}", mp(c), f.iter().map(|x| q(x)).collect::<Vec<_>>().join(" ")), Stage::Test),
                None => push("cargo test", format!("cargo test -q{}", mp(c)), Stage::Test),
            }
        }
    }

    // Python
    let py: Vec<&&PathBuf> = files.iter().filter(|p| ext(p) == "py" && p.is_file()).collect();
    if !py.is_empty() {
        let args: Vec<String> = py.iter().map(|p| q(&rel(cwd, p))).collect();
        let exe = if has("python3") { "python3" } else { "python" };
        push("py_compile", format!("{exe} -m py_compile {}", args.join(" ")), Stage::Fast);
        if has("ruff") { push("ruff", format!("ruff check --output-format=concise {}", args.join(" ")), Stage::Fast); }
        let mut tests: Vec<String> = py.iter().filter(|p| is_py_test(p)).map(|p| q(&rel(cwd, p))).collect();
        if !touches.is_empty() {
            for t in py_targets(cwd, &py.iter().map(|p| p.as_path()).collect::<Vec<_>>(), touches) {
                let t = q(&rel(cwd, &t));
                if !tests.contains(&t) { tests.push(t); }
            }
        }
        // no test reaches the edit by name: the project's own pytest files still run (an import-only `make test` is not a test)
        if tests.is_empty() { tests = project_py_tests(cwd).iter().map(|t| q(t)).collect(); }
        if !tests.is_empty() && has("pytest") { push("pytest", format!("pytest -x -q {}", tests.join(" ")), Stage::Test); }
    }

    // Swift packages
    for d in files.iter().filter(|p| ext(p) == "swift").filter_map(|p| nearest(cwd, p, "Package.swift")) {
        let cmd = if d == cwd { "swift build".to_string() } else { format!("swift build --package-path {}", q(&rel(cwd, &d))) };
        push("swift build", cmd, Stage::Fast);
    }

    // JS / TS with a tsconfig at the project root
    if cwd.join("tsconfig.json").is_file() && files.iter().any(|p| ["ts", "tsx", "mts", "cts", "js", "jsx"].contains(&ext(p))) {
        let local = cwd.join("node_modules/.bin/tsc");
        if local.is_file() { push("tsc", "node_modules/.bin/tsc --noEmit -p .".into(), Stage::Fast); }
        else if has("tsc") { push("tsc", "tsc --noEmit -p .".into(), Stage::Fast); }
    }

    // JS / TS tests: the package's own test script (nearest package.json, stub scripts skipped)
    for d in files.iter().filter(|p| ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"].contains(&ext(p))).filter_map(|p| nearest(cwd, p, "package.json")) {
        if !has_test_script(&d.join("package.json")) || !has("npm") { continue; }
        let cmd = if d == cwd { "npm test --silent".to_string() } else { format!("npm test --silent --prefix {}", q(&rel(cwd, &d))) };
        push("npm test", cmd, Stage::Test);
    }

    // Go modules
    if has("go") {
        for d in files.iter().filter(|p| ext(p) == "go").filter_map(|p| nearest(cwd, p, "go.mod")) {
            let at = if d == cwd { String::new() } else { format!("cd {} && ", q(&rel(cwd, &d))) };
            push("go vet", format!("{at}go vet ./..."), Stage::Fast);
            push("go test", format!("{at}go test ./..."), Stage::Test);
        }
    }
    out
}

/// `package.json` has a `scripts.test` that is not npm's default "no test specified" stub
fn has_test_script(p: &Path) -> bool {
    let Ok(s) = std::fs::read_to_string(p) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { return false };
    v["scripts"]["test"].as_str().is_some_and(|t| !t.trim().is_empty() && !t.contains("no test specified"))
}

// ---------- targeted checks (config `ast_targeted_checks`) ----------

/// files searched for tests (per crate / per project), and the most test-name filters on one `cargo test`
const TARGET_FILES_MAX: usize = 2000;
const FILTERS_MAX: usize = 12;

/// The project's pytest files (`test_*.py` / `*_test.py`, no hidden / venv dirs), relative, sorted, at most 40.
pub fn project_py_tests(cwd: &Path) -> Vec<String> {
    files_under(cwd, "py").into_iter().filter(|p| is_py_test(p)).filter(|p| !p.components().any(|c| c.as_os_str().to_str().is_some_and(|n| n == "site-packages" || n == "env" || n == ".venv")))
        .take(40).map(|p| p.strip_prefix(cwd).map(Path::to_path_buf).unwrap_or(p).to_string_lossy().into_owned()).collect()
}

/// `make test` / `make check` whose recipe runs no pytest while the project has pytest files: the pytest command to
/// run after it (the import-only `make test` of the g40 session hid 3 failing tests). None otherwise.
pub fn make_test_gap(cwd: &Path, cmd: &str) -> Option<String> {
    let c = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    let target = match c.as_str() { "make test" => "test", "make check" => "check", _ => return None };
    let mk = std::fs::read_to_string(cwd.join("Makefile")).ok()?;
    // the target's recipe: the lines after `test:` that start with a tab
    let mut lines = mk.lines().skip_while(|l| !l.starts_with(&format!("{target}:")));
    lines.next()?;
    let recipe: String = lines.take_while(|l| l.starts_with('\t') || l.trim().is_empty()).collect::<Vec<_>>().join("\n");
    if recipe.contains("pytest") || recipe.contains("unittest") { return None; }
    let tests = project_py_tests(cwd);
    if tests.is_empty() { return None; }
    let exe = if on_path("pytest") { "pytest" } else { "python3 -m pytest" };
    Some(format!("{exe} -q {}", tests.iter().map(|t| q(t)).collect::<Vec<_>>().join(" ")))
}

fn is_py_test(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("test_") || n.ends_with("_test.py"))
}

/// files with extension `ext` under `root` (no .git / target / node_modules / .elisa / hidden dirs), sorted, bounded
fn files_under(root: &Path, ext: &str) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root).follow_links(false).sort_by_file_name().into_iter()
        .filter_entry(|e| e.depth() == 0 || !(e.file_type().is_dir()
            && e.file_name().to_str().is_some_and(|n| n.starts_with('.') || ["target", "node_modules", "__pycache__", "venv"].contains(&n))))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == ext))
        .take(TARGET_FILES_MAX).map(|e| e.into_path()).collect()
}

/// `SplitWords` → `split_words`; snake_case stays
fn snake(s: &str) -> String {
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() { if i > 0 && !o.ends_with('_') { o.push('_'); } o.push(c.to_ascii_lowercase()); } else { o.push(c); }
    }
    o
}

/// The test-name filters for one crate (`dir` holds its Cargo.toml) when every symbol its edited files (`mine`)
/// touched is a test or has tests named after it. None = unsure: run the crate's tests unfiltered.
pub fn rust_filters(dir: &Path, mine: &[&PathBuf], touches: &[(PathBuf, Touched)]) -> Option<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for f in mine {
        if ext(f) != "rs" { return None; }
        let (_, t) = touches.iter().find(|(p, _)| p == *f)?;
        if t.outside { return None; }
        for s in &t.symbols {
            let n = s.rsplit("::").next().unwrap_or(s).to_string();
            if !names.contains(&n) { names.push(n); }
        }
    }
    if names.is_empty() { return None; }
    let mut tests: Vec<String> = Vec::new();
    for f in files_under(dir, "rs") {
        if std::fs::metadata(&f).map_or(true, |m| m.len() as usize > ast::MAX_BYTES) { continue; }
        if let Ok(src) = std::fs::read_to_string(&f) {
            if src.contains("test") { tests.extend(ast::rust_tests(&src)); }
        }
    }
    let mut filters: Vec<String> = Vec::new();
    for n in names {
        let f = if tests.contains(&n) { n } else {
            let s = snake(&n);
            if s.len() < 4 || !tests.iter().any(|t| t.contains(&s)) { return None; }
            s
        };
        if !filters.contains(&f) { filters.push(f); }
    }
    (filters.len() <= FILTERS_MAX).then_some(filters)
}

/// Python test files (under `cwd`) that import the module of an edited file and name one of its touched symbols.
pub fn py_targets(cwd: &Path, edited: &[&Path], touches: &[(PathBuf, Touched)]) -> Vec<PathBuf> {
    let mut want: Vec<(String, Vec<String>)> = Vec::new();
    for f in edited.iter().filter(|f| !is_py_test(f)) {
        let Some((_, t)) = touches.iter().find(|(p, _)| p == f) else { continue };
        let Some(stem) = f.file_stem().and_then(|s| s.to_str()) else { continue };
        let syms: Vec<String> = t.symbols.iter().map(|s| s.split('.').next().unwrap_or(s).to_string()).collect();
        if !syms.is_empty() { want.push((stem.to_string(), syms)); }
    }
    if want.is_empty() { return vec![]; }
    let mut out = Vec::new();
    for f in files_under(cwd, "py").into_iter().filter(|f| is_py_test(f)) {
        if edited.contains(&f.as_path()) { continue; }
        let Ok(src) = std::fs::read_to_string(&f) else { continue };
        let hit = want.iter().any(|(stem, syms)| {
            let imp = Regex::new(&format!(r"(?m)^\s*(from\s+[\w.]*\b{0}\b[\w.]*\s+import\b|import\s+[\w.]*\b{0}\b)", regex::escape(stem)));
            imp.is_ok_and(|r| r.is_match(&src)) && syms.iter().any(|s| Regex::new(&format!(r"\b{}\b", regex::escape(s))).is_ok_and(|r| r.is_match(&src)))
        });
        if hit { out.push(f); }
    }
    out
}

/// One finished check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Pass, Fail or Timeout (Running / Stopped are the loop's)
    pub status: CheckStatus,
    /// `3 errors`, `exit 2`, `timed out after 60s`
    pub summary: String,
    pub errors: usize,
    /// exit code, None when killed
    pub code: Option<i32>,
    /// trimmed output for the model (error lines first, capped)
    pub output: String,
    /// the failure's identity for the repeat rule: error lines with line/column numbers removed
    pub signature: String,
    pub cancelled: bool,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).expect("check regex")) }

/// cargo / npm progress lines carry no diagnosis
fn noise(l: &str) -> bool {
    let t = l.trim_start();
    t.is_empty() || ["Compiling ", "Checking ", "Finished ", "Blocking ", "Running ", "Downloaded ", "Downloading ",
        "Updating ", "Locking ", "Fresh ", "Documenting "].iter().any(|p| t.starts_with(p))
}

/// a line that names one problem: rustc / clippy / swiftc / tsc / gcc diagnostics, pytest FAILED, Python exceptions,
/// ruff codes; build summaries (`could not compile`, `aborting due to`) are not counted
pub fn is_error_line(l: &str) -> bool {
    static SUMMARY: OnceLock<Regex> = OnceLock::new();
    static ERR: OnceLock<Regex> = OnceLock::new();
    let t = l.trim();
    if re(&SUMMARY, r"(?i)could not compile|aborting due to|^error: build failed|warnings? emitted|^error: process didn't exit|^error: test failed|^error: \d+ target").is_match(t) { return false; }
    re(&ERR, concat!(
        r"(?i:^error(\[\w+\])?:)|",                     // rustc / cargo long form
        r":\d+(:\d+)?:? (fatal )?error\b|",              // file:line:col: error (rust short, swift, gcc, clang)
        r"\(\d+,\d+\): error\b|",                        // tsc
        r"^FAILED |^ERROR |",                            // pytest -q summary
        r"^\w*(Error|Exception): |",                     // Python
        r"^\S+:\d+:\d+: [A-Z]{1,4}\d{2,4}\b",            // ruff / flake8 codes
    )).is_match(t)
}

fn plural(n: usize, w: &str) -> String { format!("{n} {w}{}", if n == 1 { "" } else { "s" }) }

/// Runs one check (blocking; Ctrl-C kills it). `max_chars` caps the output sent back to the model.
pub fn run(c: &Check, cwd: &Path, max_chars: usize) -> Outcome {
    let r = match crate::tools::shell(&c.cmd, cwd, c.timeout_s, OUTPUT_BUF_MAX) {
        Ok(r) => r,
        Err(e) => return Outcome { status: CheckStatus::Fail, summary: e.clone(), errors: 1, code: None, output: e.clone(), signature: e, cancelled: false },
    };
    if r.timed_out {
        return Outcome { status: CheckStatus::Timeout, summary: format!("timed out after {}", crate::ui::dur(c.timeout_s.max(1) as f64, 0)), errors: 0,
            code: None, output: String::new(), signature: String::new(), cancelled: false };
    }
    let kept: Vec<&str> = r.out.lines().filter(|l| !noise(l)).collect();
    let mut errs: Vec<&str> = Vec::new();
    for l in kept.iter().copied().filter(|l| is_error_line(l)) { if !errs.contains(&l) { errs.push(l); } }
    if r.code == Some(0) && !r.cancelled {
        return Outcome { status: CheckStatus::Pass, summary: String::new(), errors: 0, code: Some(0), output: String::new(), signature: String::new(), cancelled: false };
    }
    let summary = if errs.is_empty() {
        r.code.map_or("killed".into(), |c| format!("exit {c}"))
    } else { plural(errs.len(), "error") };
    let output = trim_output(&kept, &errs, max_chars);
    let sig_src: Vec<&str> = if errs.is_empty() { kept.iter().rev().take(SIG_LINES).rev().copied().collect() } else { errs.iter().take(SIG_LINES).copied().collect() };
    Outcome { status: CheckStatus::Fail, summary, errors: errs.len(), code: r.code, output, signature: signature(&sig_src), cancelled: r.cancelled }
}

/// error lines without line / column numbers, sorted: an edit that only moves the same errors still repeats
pub fn signature(lines: &[&str]) -> String {
    static NUM: OnceLock<Regex> = OnceLock::new();
    let mut v: Vec<String> = lines.iter().map(|l| re(&NUM, r"[:(,]\d+").replace_all(l.trim(), ":#").into_owned()).collect();
    v.sort();
    v.dedup();
    v.join("\n")
}

/// the whole (filtered) output when it fits; else the error lines first, then head + tail of the rest
fn trim_output(kept: &[&str], errs: &[&str], max: usize) -> String {
    let all = kept.join("\n");
    if all.chars().count() <= max || errs.is_empty() { return crate::tools::cap(&all, max); }
    let block = crate::tools::cap(&errs.iter().take(SIG_LINES).copied().collect::<Vec<_>>().join("\n"), max / 2);
    let rest = max.saturating_sub(block.chars().count() + 40).max(200);
    format!("errors:\n{block}\n--- output (cut) ---\n{}", crate::tools::cap(&all, rest))
}

/// The user-side note after a failed check (starts with `context::NUDGE_PREFIX`: never taken for the user's request).
/// Deterministic text only (no timings): it is appended to the history like any other message.
pub fn fix_note(c: &Check, o: &Outcome, round: usize, rounds: usize) -> String {
    let exit = o.code.map_or("was killed".to_string(), |c| format!("exited {c}"));
    let errs = if o.errors > 0 { format!(" with {}", plural(o.errors, "error")) } else { String::new() };
    format!("{}check failed after your edits (fix round {} of {rounds}): `{}` {exit}{errs}:\n{}\nFix these with the tools, \
        then finish with a short summary.", crate::context::NUDGE_PREFIX, round + 1, c.cmd, o.output.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::CheckEvent;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-chk-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }
    fn touch(d: &Path, rel: &str, body: &str) -> PathBuf {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }
    fn cmds(v: &[Check]) -> Vec<(&str, Stage)> { v.iter().map(|c| (c.cmd.as_str(), c.stage)).collect() }

    #[test]
    fn detects_go_modules_and_npm_tests() {
        let d = tmp("gojs");
        touch(&d, "go.mod", "module x\n");
        let g = touch(&d, "cmd/main.go", "package main\n");
        touch(&d, "svc/go.mod", "module y\n");
        let g2 = touch(&d, "svc/a.go", "package y\n");
        let cmds = |files: &[PathBuf], has: &dyn Fn(&str) -> bool| detect(&d, files, has).into_iter().map(|c| (c.cmd, c.stage)).collect::<Vec<_>>();
        assert_eq!(cmds(&[g.clone(), g2], &|_| true), vec![
            ("go vet ./...".to_string(), Stage::Fast), ("go test ./...".to_string(), Stage::Test),
            ("cd svc && go vet ./...".to_string(), Stage::Fast), ("cd svc && go test ./...".to_string(), Stage::Test)]);
        assert!(cmds(std::slice::from_ref(&g), &|e| e != "go").is_empty(), "no go: nothing");
        // npm: a real test script runs, the stub does not
        let j = touch(&d, "web/src/app.ts", "export const a = 1;\n");
        touch(&d, "web/package.json", r#"{"scripts": {"test": "vitest run"}}"#);
        assert_eq!(cmds(std::slice::from_ref(&j), &|_| true), vec![("npm test --silent --prefix web".to_string(), Stage::Test)]);
        touch(&d, "web/package.json", r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#);
        assert!(cmds(std::slice::from_ref(&j), &|_| true).is_empty());
    }

    #[test]
    fn detects_rust_workspace_and_member_tests() {
        let d = tmp("rust");
        touch(&d, "Cargo.toml", "[workspace]\nmembers = [\"a\"]\n");
        touch(&d, "a/Cargo.toml", "[package]\nname = \"a\"\n");
        let f = touch(&d, "a/src/lib.rs", "");
        let all = |_: &str| true;
        assert_eq!(cmds(&detect(&d, std::slice::from_ref(&f), all)), [
            ("cargo check --message-format=short", Stage::Fast),
            ("cargo clippy --message-format=short -- -D warnings", Stage::Fast),
            ("cargo test -q --manifest-path a/Cargo.toml", Stage::Test)]);
        // no clippy installed: check + test only; docs-only edits run nothing
        assert_eq!(detect(&d, &[f], |e| e != "cargo-clippy").len(), 2);
        assert!(detect(&d, &[touch(&d, "README.md", "")], all).is_empty());
    }

    #[test]
    fn detects_python_swift_ts() {
        let d = tmp("multi");
        let a = touch(&d, "pkg/a b.py", "x = 1\n");
        let t = touch(&d, "tests/test_a.py", "");
        assert_eq!(cmds(&detect(&d, &[a.clone(), t.clone()], |_| true)), [
            ("python3 -m py_compile 'pkg/a b.py' tests/test_a.py", Stage::Fast),
            ("ruff check --output-format=concise 'pkg/a b.py' tests/test_a.py", Stage::Fast),
            ("pytest -x -q tests/test_a.py", Stage::Test)]);
        // no ruff / pytest: compile only
        assert_eq!(detect(&d, &[a, t], |e| e == "python3").len(), 1);
        touch(&d, "app/Package.swift", "");
        let s = touch(&d, "app/Sources/main.swift", "");
        assert_eq!(cmds(&detect(&d, &[s], |_| true)), [("swift build --package-path app", Stage::Fast)]);
        touch(&d, "tsconfig.json", "{}");
        let ts = touch(&d, "src/x.ts", "");
        assert_eq!(cmds(&detect(&d, std::slice::from_ref(&ts), |_| true)), [("tsc --noEmit -p .", Stage::Fast)]);
        assert!(detect(&d, &[ts], |_| false).is_empty(), "no tsc anywhere: nothing");
    }

    fn touch_of(f: &Path, old: Option<&str>, new: &str) -> (PathBuf, Touched) {
        let mut t = Touches::new();
        std::fs::write(f, new).unwrap();
        add_touch(&mut t, Before { path: f.to_path_buf(), lang: Lang::Rust, old: old.map(String::from) });
        t.remove(0)
    }

    #[test]
    fn rust_tests_narrow_to_the_touched_symbols() {
        let d = tmp("target-rs");
        touch(&d, "Cargo.toml", "[workspace]\nmembers = [\"a\"]\n");
        touch(&d, "a/Cargo.toml", "[package]\nname = \"a\"\n");
        let src = "pub fn split_words(s: &str) -> usize { s.len() }\npub fn other() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn split_words_basic() {}\n}\n";
        let f = touch(&d, "a/src/lib.rs", src);
        touch(&d, "a/tests/more.rs", "#[test]\nfn split_words_unicode() {}\n");
        let full = |t: &[(PathBuf, Touched)]| -> Vec<String> {
            detect_with(&d, std::slice::from_ref(&f), t, |e| e != "cargo-clippy").into_iter().filter(|c| c.stage == Stage::Test).map(|c| c.cmd).collect()
        };
        // a symbol with tests named after it: those tests only
        let t = touch_of(&f, Some(src), &src.replace("s.len()", "s.len() + 0"));
        assert_eq!(t.1.symbols, ["split_words"]);
        assert_eq!(full(std::slice::from_ref(&t)), ["cargo test -q --manifest-path a/Cargo.toml -- split_words"]);
        // an edited test is its own filter
        let t = touch_of(&f, Some(src), &src.replace("fn split_words_basic() {}", "fn split_words_basic() { assert!(true) }"));
        assert_eq!(full(&[t]), ["cargo test -q --manifest-path a/Cargo.toml -- split_words_basic"]);
        // unsure → the whole crate: a symbol without a named test, a change outside symbols, a new file, no touch info
        let t = touch_of(&f, Some(src), &src.replace("pub fn other() {}", "pub fn other() { let _ = 1; }"));
        assert_eq!(full(&[t]), ["cargo test -q --manifest-path a/Cargo.toml"], "other() has no test named after it");
        let t = touch_of(&f, Some(src), &src.replace("#[cfg(test)]", "// x\n#[cfg(test)]"));
        assert_eq!(full(&[t]), ["cargo test -q --manifest-path a/Cargo.toml"]);
        let t = touch_of(&f, None, src);
        assert_eq!(full(&[t]), ["cargo test -q --manifest-path a/Cargo.toml"]);
        assert_eq!(full(&[]), ["cargo test -q --manifest-path a/Cargo.toml"]);
        // config ast_targeted_checks = false: select_with ignores the touches
        let t = touch_of(&f, Some(src), &src.replace("s.len()", "s.len() + 1"));
        let cfg = Config { ast_targeted_checks: false, ..Config::default() };
        let p = select_with(&cfg, &d, std::slice::from_ref(&f), std::slice::from_ref(&t), false).unwrap();
        assert!(p.checks.iter().any(|c| c.cmd == "cargo test -q --manifest-path a/Cargo.toml"), "{:?}", p.checks);
        let p = select_with(&Config::default(), &d, std::slice::from_ref(&f), &[t], false).unwrap();
        assert!(p.checks.iter().any(|c| c.cmd.ends_with("-- split_words")), "{:?}", p.checks);
    }

    #[test]
    fn python_tests_that_import_the_touched_symbol() {
        let d = tmp("target-py");
        let m = touch(&d, "pkg/shapes.py", "class Shape:\n    def area(self):\n        return 0\n\ndef unit():\n    return 1\n");
        touch(&d, "tests/test_shapes.py", "from pkg.shapes import Shape\n\ndef test_area():\n    assert Shape().area() == 0\n");
        touch(&d, "tests/test_unit.py", "from pkg.shapes import unit\n\ndef test_unit():\n    assert unit() == 1\n");
        touch(&d, "tests/test_other.py", "import os\n\ndef test_x():\n    assert os\n");
        let t = (m.clone(), Touched { symbols: vec!["Shape.area".into()], outside: false });
        let cmds: Vec<String> = detect_with(&d, std::slice::from_ref(&m), &[t], |_| true).into_iter().map(|c| c.cmd).collect();
        assert_eq!(cmds, ["python3 -m py_compile pkg/shapes.py", "ruff check --output-format=concise pkg/shapes.py", "pytest -x -q tests/test_shapes.py"]);
        // untargeted: no test names the edit, so the project's own pytest files run (not an import-only check)
        let un: Vec<String> = detect(&d, &[m], |_| true).into_iter().map(|c| c.cmd).collect();
        assert_eq!(un.last().map(String::as_str), Some("pytest -x -q tests/test_other.py tests/test_shapes.py tests/test_unit.py"), "{un:?}");
    }

    #[test]
    fn checks_toml_overrides_and_orders_stages() {
        let d = tmp("toml");
        touch(&d, "Cargo.toml", "[package]\nname = \"x\"\n");
        touch(&d, ".elisa/checks.toml", "rounds = 3\ntimeout_s = 7\ntests = [\"make t\"]\ncommands = [\"make lint\"]\n");
        let p = select(&Config::default(), &d, &[d.join("src/lib.rs")], false).unwrap();
        assert_eq!((p.enabled, p.rounds), (true, 3));
        assert_eq!(cmds(&p.checks), [("make lint", Stage::Fast), ("make t", Stage::Test)]);
        assert_eq!((p.checks[0].timeout_s, p.checks[1].timeout_s), (7, 60));
        assert!(select(&Config::default(), &d, &[d.join("src/lib.rs")], true).unwrap().checks.is_empty(), "--no-checks");
        touch(&d, ".elisa/checks.toml", "colour = 1\n");
        assert!(select(&Config::default(), &d, &[], false).is_err(), "unknown keys are an error");
    }

    #[test]
    fn error_lines_and_signatures() {
        for l in ["error[E0425]: cannot find value `x` in this scope", "src/a.rs:3:5: error[E0308]: mismatched types",
            "src/a.rs:1:5: error: unused import: `std::fs`", "Sources/m.swift:2:3: error: cannot find 'y' in scope",
            "src/x.ts(3,5): error TS2322: Type 'string' is not assignable", "FAILED tests/test_a.py::test_x - assert 1 == 2",
            "SyntaxError: '(' was never closed", "a.py:1:8: F401 [*] `os` imported but unused"] {
            assert!(is_error_line(l), "{l}");
        }
        for l in ["error: could not compile `x` (lib) due to 2 previous errors", "warning: unused variable", "test result: ok",
            "   Compiling x v0.1.0", "1 failed in 0.02s", "error: aborting due to 1 previous error"] {
            assert!(!is_error_line(l), "{l}");
        }
        // moved errors are the same errors
        assert_eq!(signature(&["src/a.rs:3:5: error: x", "src/b.rs:9:1: error: y"]), signature(&["src/b.rs:12:1: error: y", "src/a.rs:4:2: error: x"]));
        assert_ne!(signature(&["src/a.rs:3:5: error: x"]), signature(&["src/a.rs:3:5: error: z"]));
    }

    #[test]
    fn event_lines() {
        let e = |status, round, summary: &str| CheckEvent { cmd: "cargo clippy".into(), status, round, rounds: 2, summary: summary.into() }.line();
        assert_eq!(e(CheckStatus::Fail, 0, "3 errors"), "check: cargo clippy ✗ 3 errors → fixing (1/2)");
        assert_eq!(e(CheckStatus::Running, 1, ""), "check: cargo clippy …");
        assert_eq!(e(CheckStatus::Pass, 1, ""), "check: cargo clippy ✓");
        assert_eq!(e(CheckStatus::Stopped, 2, "3 errors · same errors again"), "check: cargo clippy ✗ 3 errors · same errors again → stopped");
    }
}
