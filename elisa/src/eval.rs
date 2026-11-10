//! `elisa eval`: a repeatable evaluation suite. Every case runs Elisa itself (`elisa --serve-jsonl`, the protocol the
//! app speaks) in a fresh temp project with a temp HOME, sends user turns, answers approvals, and checks what happened:
//! the reply, the tool calls, the files, the audit log, what reached disk.
//!
//! Two kinds of case:
//! - `fake`: a scripted model (`eval_fake.rs`) answers, so the case tests Elisa's own behaviour whatever the model
//!   (loop guard, unknown tools, secrets on disk, sandbox, edit prechecks, /clear notes, state cards, empty replies).
//!   No model needed; `cargo test` runs them (tests/eval.rs).
//! - `model`: the user's configured connection (or `--model`) answers: model + Elisa measured together (tool use when
//!   asked, no reasoning tools on simple prompts, small tasks done right, no made-up facts, speed).
//!
//! Elisa never picks or recommends a model: with no model chosen, model cases do not run (`api::NO_MODEL`). The JSON
//! report (`--json`, schema `elisa-eval/1`, documented in docs/reference/eval.md) holds facts per case and per model, for the user
//! to compare.
//!
//! Isolation: each case gets `<tmp>/elisa-eval-…/{project,home,outside}`; only `~/.config/elisa/providers.toml` is
//! copied into the temp home (model cases), the real `~/.elisa` is never read or written by a case. The child runs in
//! its own process group, which is killed when the case ends. A run summary is appended to
//! `~/.elisa/evals/history.jsonl` (the eval command only; `--no-history` and the library default for tests skip it).
use crate::eval_fake::{FakeModel, Scripted};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "elisa-eval/1";
const USAGE: &str = "usage: elisa eval [--suite NAME|all|fake|model] [--case ID] [--model M] [--provider NAME] [--base-url URL [--type T]] [--max-ctx N] [--json FILE] [--save] [--keep] [--list] [--dir DIR] [--jobs N] [--no-history]";

/// The built-in cases (also what the installed binary runs: it has no repo next to it). `--dir` reads others.
pub const BUILTIN: &[(&str, &str)] = &[
    ("core.toml", include_str!("../evals/core.toml")),
    ("tools.toml", include_str!("../evals/tools.toml")),
    ("security.toml", include_str!("../evals/security.toml")),
    ("context.toml", include_str!("../evals/context.toml")),
    ("model.toml", include_str!("../evals/model.toml")),
    ("backends.toml", include_str!("../evals/backends.toml")),
];

// ---------- case files ----------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSpec {
    suite: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    case: Vec<Case>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    #[serde(default)]
    pub suite: String,
    /// "fake" (scripted model) or "model" (the user's model)
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    pub timeout_s: Option<u64>,
    /// "macos": skipped elsewhere (the shell sandbox is macOS only)
    pub platform: Option<String>,
    /// the temp HOME's `~/.config/elisa/config.toml`
    #[serde(default)]
    pub config: String,
    /// the project's `.elisa/config.toml`
    #[serde(default)]
    pub project_config: String,
    #[serde(default)]
    pub files: Vec<Fixture>,
    /// files in the temp HOME (user command files `.elisa/commands/x.md`, an installed plugin), paths relative to it
    #[serde(default)]
    pub home_files: Vec<Fixture>,
    /// the answer to approvals a turn does not list (default "no")
    pub approve: Option<String>,
    /// the window the fake model's /health reports (fake cases; default 32768)
    pub max_ctx: Option<usize>,
    /// fake cases: the scripted model speaks this backend's protocol instead of the gateway's (`eval_fake::BACKENDS`:
    /// ollama, llamacpp, lmstudio, vllm, mlx, localai, gpt4all, openai, anthropic), with its detection endpoints
    pub backend: Option<String>,
    #[serde(default)]
    pub script: Vec<ScriptItem>,
    #[serde(default)]
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub path: String,
    #[serde(default)]
    pub content: String,
    /// the content this many times (a big file)
    pub repeat: Option<usize>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    #[serde(default = "empty_table")]
    pub input: toml::Value,
}
fn empty_table() -> toml::Value { toml::Value::Table(Default::default()) }

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ScriptItem {
    pub text: Option<String>,
    /// the text this many times
    pub repeat_text: Option<usize>,
    pub tool: Option<ToolCall>,
    #[serde(default)]
    pub tools: Vec<ToolCall>,
    pub stop: Option<String>,
    pub out_tokens: Option<usize>,
    pub input_tokens: Option<usize>,
    /// this reply this many times in a row
    pub repeat: Option<usize>,
    #[serde(default)]
    pub slow: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    /// the user message or slash command
    pub send: Option<String>,
    /// answers to this turn's approvals, in order: yes | always | rule (yes + always allow the offered pattern) | no
    #[serde(default)]
    pub approvals: Vec<String>,
    /// quit and start a new session (same project and home) before this turn
    #[serde(default)]
    pub restart: bool,
    /// with `restart`: start it with --continue
    #[serde(default, rename = "continue")]
    pub cont: bool,
    /// press Esc (`interrupt`) at the first streamed text
    #[serde(default)]
    pub interrupt: bool,
    /// documents attached to this message (`send.documents`, src/attach.rs): paths relative to the case's project
    /// (`../home/x.csv` is a file outside it)
    #[serde(default)]
    pub attach: Vec<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Check {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    pub args: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub any: Vec<String>,
    pub regex: Option<String>,
    pub path: Option<String>,
    pub prefix: Option<String>,
    pub n: Option<f64>,
    #[serde(default)]
    pub values: Vec<String>,
    pub event: Option<String>,
    pub reason: Option<String>,
    pub tool: Option<String>,
    pub min: Option<usize>,
    pub max: Option<usize>,
    /// 1-based turn the check looks at (reply checks default to the last turn, the rest to all turns)
    pub turn: Option<usize>,
}

pub const CHECKS: &[&str] = &[
    "reply_contains", "reply_not_contains", "reply_matches", "reply_not_empty", "tool_called", "tool_not_called",
    "tool_result_contains", "no_tools_offered", "tools_offered", "file_contains", "file_not_contains", "file_unchanged",
    "file_absent", "max_steps", "max_seconds", "no_secret_on_disk", "audit_has", "approval_for", "no_approval_for",
    "approvals", "stopped_by_loop_guard", "info_contains", "info_not_contains", "request_contains",
    "request_not_contains", "event",
];

/// the approval answers a case may give
const ANSWERS: [&str; 4] = ["yes", "always", "rule", "no"];

/// Cases from TOML text (one file); `file` names it in errors.
pub fn parse_cases(file: &str, text: &str) -> Result<Vec<Case>, String> {
    let f: FileSpec = toml::from_str(text).map_err(|e| format!("{file}: {e}"))?;
    let mut out = Vec::new();
    for mut c in f.case {
        if c.suite.is_empty() { c.suite = f.suite.clone().unwrap_or_default(); }
        if c.kind.is_empty() { c.kind = f.kind.clone().unwrap_or_else(|| "fake".into()); }
        let at = format!("{file}: case {}", c.id);
        if c.id.trim().is_empty() { return Err(format!("{file}: a case has no id")); }
        if c.suite.is_empty() { return Err(format!("{at}: no suite")); }
        if c.kind != "fake" && c.kind != "model" { return Err(format!("{at}: kind must be \"fake\" or \"model\" (got {:?})", c.kind)); }
        if let Some(b) = c.backend.as_deref().filter(|b| !crate::eval_fake::BACKENDS.contains(b)) {
            return Err(format!("{at}: backend {b:?}: use one of {}", crate::eval_fake::BACKENDS.join(", ")));
        }
        if c.turns.is_empty() { return Err(format!("{at}: no turns")); }
        if c.kind == "model" && !c.script.is_empty() { return Err(format!("{at}: a model case has no script")); }
        for ch in &c.checks {
            if !CHECKS.contains(&ch.kind.as_str()) { return Err(format!("{at}: unknown check type {:?} (known: {})", ch.kind, CHECKS.join(", "))); }
        }
        for t in &c.turns {
            for a in &t.approvals { if !ANSWERS.contains(&a.as_str()) { return Err(format!("{at}: approval {a:?} is not yes | always | rule | no")); } }
        }
        if let Some(a) = &c.approve { if !ANSWERS.contains(&a.as_str()) { return Err(format!("{at}: approve {a:?} is not yes | always | rule | no")); } }
        out.push(c);
    }
    Ok(out)
}

/// Every case: the built-in ones, or those in `dir/*.toml`. Ids are unique.
pub fn load_cases(dir: Option<&Path>) -> Result<Vec<Case>, String> {
    let mut all = Vec::new();
    match dir {
        None => for (n, t) in BUILTIN { all.extend(parse_cases(n, t)?); },
        Some(d) => {
            let mut files: Vec<PathBuf> = std::fs::read_dir(d).map_err(|e| format!("{}: {e}", d.display()))?
                .flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
            files.sort();
            for p in files {
                let t = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                all.extend(parse_cases(&p.display().to_string(), &t)?);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for c in &all { if !seen.insert(c.id.clone()) { return Err(format!("case id {} is used twice", c.id)); } }
    Ok(all)
}

/// The cases a `--suite` / `--case` selection picks: a suite name, "all", "fake" (every scripted case) or "model".
pub fn select(all: &[Case], suite: &str, case: Option<&str>) -> Result<Vec<Case>, String> {
    let picked: Vec<Case> = all.iter().filter(|c| match suite {
        "all" => true,
        "fake" | "deterministic" => c.kind == "fake",
        s => c.suite == s || (s == "model" && c.kind == "model"),
    }).filter(|c| case.is_none_or(|id| c.id == id || c.id.starts_with(&format!("{id}.")))).cloned().collect();
    if picked.is_empty() {
        let mut suites: Vec<&str> = all.iter().map(|c| c.suite.as_str()).collect();
        suites.sort_unstable();
        suites.dedup();
        return Err(match case {
            Some(id) => format!("no case {id:?} in suite {suite:?} (elisa eval --list shows them)"),
            None => format!("no suite {suite:?} (suites: {}, or all | fake | model)", suites.join(", ")),
        });
    }
    Ok(picked)
}

// ---------- placeholders ----------

/// `{{secret1}}`…`{{secret9}}` (fresh random values per case: no secret literal sits in the repo), `{{project}}`,
/// `{{home}}`, `{{outside}}` (a folder next to the project).
pub struct Vars { pairs: Vec<(String, String)> }

impl Vars {
    fn new(root: &Path, seed: u64) -> Vars {
        let mut x = seed | 1;
        let mut next = move || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x };
        const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
        let mut pairs: Vec<(String, String)> = (1..=9).map(|i| {
            // upper + lower + digit up front, then 17 more: shaped like a real token, never a dictionary word
            let mut s: String = ["Q", "w", "7"].concat();
            s.extend((0..17).map(|_| A[(next() % A.len() as u64) as usize] as char));
            (format!("{{{{secret{i}}}}}"), s)
        }).collect();
        for (k, d) in [("project", "project"), ("home", "home"), ("outside", "outside")] {
            pairs.push((format!("{{{{{k}}}}}"), root.join(d).display().to_string()));
        }
        Vars { pairs }
    }
    pub fn s(&self, t: &str) -> String {
        if !t.contains("{{") { return t.to_string(); }
        let mut t = t.to_string();
        for (k, v) in &self.pairs { t = t.replace(k, v); }
        t
    }
    fn j(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.s(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.j(x)).collect()),
            Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), self.j(x))).collect()),
            x => x.clone(),
        }
    }
}

// ---------- results ----------

#[derive(Clone, Debug, Default)]
pub struct CheckResult { pub kind: String, pub pass: bool, pub detail: String }

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    /// seconds per user turn (send → turn_done), slash commands left out
    pub turn_s: Vec<f64>,
    pub model_calls: usize,
    pub max_calls_per_turn: usize,
    pub output_tokens: usize,
    pub max_ctx_used: usize,
    pub tool_calls: usize,
    pub tool_errors: usize,
    pub approvals: usize,
    /// turns whose visible reply was empty after Elisa's one retry
    pub empty_reply_turns: usize,
    /// Elisa asked again because a reply had no visible text
    pub empty_retries: usize,
    pub loop_guard_stops: usize,
    pub thinking_chars: usize,
    /// first-token latency of each model call, seconds
    pub ttft_s: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct CaseResult {
    pub id: String,
    pub suite: String,
    pub kind: String,
    pub description: String,
    /// "pass" | "fail" | "skip"
    pub status: String,
    pub seconds: f64,
    pub notes: Vec<String>,
    pub checks: Vec<CheckResult>,
    pub metrics: Metrics,
    /// the model the case talked to (from the session banner); "eval-fake" for scripted cases
    pub model: String,
    pub base_url: String,
    pub dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Options {
    pub suite: String,
    pub case: Option<String>,
    pub model: Option<String>,
    /// model cases: this saved connection (`--provider`, as for `elisa`; "none" = no saved connection)
    pub provider: Option<String>,
    /// model cases: this server (`--base-url`, with `--type` its kind: then no saved connection, as for `elisa`)
    pub base_url: Option<String>,
    pub kind: Option<String>,
    /// model cases: the context window (`--max-ctx`)
    pub max_ctx: Option<usize>,
    pub json: Option<PathBuf>,
    /// also keep the whole report in ~/.elisa/evals/reports/ (the history line names it)
    pub save: bool,
    pub keep: bool,
    /// the elisa binary under test
    pub exe: PathBuf,
    pub dir: Option<PathBuf>,
    /// append a summary to ~/.elisa/evals/history.jsonl
    pub history: bool,
    /// scripted cases run this many at a time (model cases always one at a time)
    pub jobs: usize,
    /// print each case as it finishes
    pub progress: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { suite: "all".into(), case: None, model: None, provider: None, base_url: None, kind: None, max_ctx: None, json: None, save: false, keep: false,
            exe: PathBuf::new(), dir: None, history: false, jobs: 4, progress: false }
    }
}

// ---------- stopping cleanly ----------

/// The running sessions (each the leader of its own process group) and the case folders to remove. A stop signal
/// (`stop_on_signal`) kills those groups and removes the folders, so a cancelled run leaves no Elisa (or model request)
/// running and no temp project behind. A session is spawned while this lock is held, so none starts unseen.
static LIVE_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static LIVE_ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn live<T>(m: &'static Mutex<Vec<T>>) -> std::sync::MutexGuard<'static, Vec<T>> { m.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }

/// SIGTERM / SIGINT / SIGHUP (the app's Cancel, Ctrl-C): kill every running session's process group, remove the
/// case folders, exit 128 + signal. The signals are blocked in every thread and taken by one waiting thread, so the
/// clean-up runs as ordinary code.
#[cfg(unix)]
pub fn stop_on_signal() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] { libc::sigaddset(&mut set, s); }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 { return; }
        std::thread::spawn(move || {
            let mut sig: libc::c_int = 0;
            if libc::sigwait(&set, &mut sig) != 0 { return; }
            let pids = live(&LIVE_PIDS);
            for &p in pids.iter() { libc::kill(-(p as i32), libc::SIGKILL); }
            for d in live(&LIVE_ROOTS).iter() { let _ = std::fs::remove_dir_all(d); }
            eprintln!("elisa eval: stopped");
            std::process::exit(128 + sig);
        });
    }
}
#[cfg(not(unix))]
pub fn stop_on_signal() {}

/// A file check's path with a `*` in its file name (`.elisa/notes/*.md`: notes are one file per session id): the
/// newest matching file, by numeric name (session ids are unix seconds), else by name. No `*`: the path itself.
fn newest_match(p: &Path) -> PathBuf {
    let Some(name) = p.file_name().and_then(|n| n.to_str()).filter(|n| n.contains('*')) else { return p.to_path_buf() };
    let (pre, suf) = name.split_once('*').unwrap_or((name, ""));
    let dir = p.parent().unwrap_or(Path::new("."));
    let key = |f: &str| (f[pre.len()..f.len() - suf.len()].parse::<u64>().ok(), f.to_string());
    std::fs::read_dir(dir).ok().and_then(|rd| rd.flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|f| f.len() >= pre.len() + suf.len() && f.starts_with(pre) && f.ends_with(suf))
        .max_by_key(|f| key(f)))
        .map_or_else(|| p.to_path_buf(), |f| dir.join(f))
}

// ---------- one session over JSON lines ----------

struct Session { child: Child, stdin: ChildStdin, rx: Receiver<Value> }

impl Session {
    /// `n`: the session's number in the case (its stderr and events go to `stderr-n.log` / `events-n.jsonl`)
    fn start(exe: &Path, root: &Path, extra: &[String], fake: bool, n: usize) -> Result<Session, String> {
        let (project, home) = (root.join("project"), root.join("home"));
        let mut cmd = Command::new(exe);
        cmd.arg("--serve-jsonl").arg("--cwd").arg(&project).args(extra).current_dir(&project).env("HOME", &home)
            .stdin(Stdio::piped()).stdout(Stdio::piped());
        let err = std::fs::File::create(root.join(format!("stderr-{n}.log"))).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
        cmd.stderr(err);
        // a scripted case must not reach a model the environment points at
        for (k, _) in std::env::vars() {
            if (fake && (k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") || k.starts_with("BRAVE_"))) || k == "ELISA_APP_LOG" { cmd.env_remove(&k); }
        }
        #[cfg(unix)]
        { use std::os::unix::process::CommandExt; cmd.process_group(0); }
        let mut pids = live(&LIVE_PIDS);
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", exe.display()))?;
        pids.push(child.id());
        drop(pids);
        let (stdin, out) = (child.stdin.take().ok_or("no stdin")?, child.stdout.take().ok_or("no stdout")?);
        let (tx, rx) = mpsc::channel();
        let mut file = std::fs::File::create(root.join(format!("events-{n}.jsonl"))).ok();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let Ok(j) = serde_json::from_str::<Value>(&l) else { continue };
                if j["type"] == "shade" { continue; }
                if let Some(f) = file.as_mut() { let _ = writeln!(f, "{l}"); }
                if tx.send(j).is_err() { return; }
            }
        });
        Ok(Session { child, stdin, rx })
    }
    fn cmd(&mut self, c: Value) {
        let _ = writeln!(self.stdin, "{c}");
        let _ = self.stdin.flush();
    }
    fn next(&self, deadline: Instant, waiting_for: &str) -> Result<Value, String> {
        let left = deadline.checked_duration_since(Instant::now()).ok_or_else(|| format!("timed out waiting for {waiting_for}"))?;
        match self.rx.recv_timeout(left) {
            Ok(v) => Ok(v),
            Err(RecvTimeoutError::Timeout) => Err(format!("timed out waiting for {waiting_for}")),
            Err(RecvTimeoutError::Disconnected) => Err(format!("elisa exited while waiting for {waiting_for}")),
        }
    }
    /// events up to the first of type `kind`
    fn until(&self, kind: &str, deadline: Instant) -> Result<Vec<Value>, String> {
        let mut got = Vec::new();
        loop {
            let e = self.next(deadline, kind)?;
            let done = e["type"] == kind;
            got.push(e);
            if done { return Ok(got); }
        }
    }
    /// quit, wait a little, then kill the whole process group (MCP servers end on stdin EOF)
    fn stop(mut self) {
        self.cmd(json!({"type": "quit"}));
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if let Ok(Some(_)) = self.child.try_wait() { break; }
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(unix)]
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL); }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let id = self.child.id();
        live(&LIVE_PIDS).retain(|&p| p != id);
    }
}

/// What one user turn did.
#[derive(Default, Debug, Clone)]
struct TurnLog {
    send: String,
    slash: bool,
    reply: String,
    events: Vec<Value>,
    seconds: f64,
    /// fake model requests made before this turn ended (index into the request log)
    req_end: usize,
    req_start: usize,
}

// ---------- running cases ----------

fn now_s() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

fn fresh_root(i: usize) -> Result<PathBuf, String> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let d = std::env::temp_dir().join(format!("elisa-eval-{}-{nanos}-{i}", std::process::id()));
    for sub in ["project", "home/.config/elisa", "outside"] {
        std::fs::create_dir_all(d.join(sub)).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    d.canonicalize().map_err(|e| format!("{}: {e}", d.display()))
}

fn script_of(c: &Case, v: &Vars) -> Vec<Scripted> {
    let mut out = Vec::new();
    for s in &c.script {
        let mut tools: Vec<(String, Value)> = s.tool.iter().chain(&s.tools)
            .map(|t| (t.name.clone(), v.j(&serde_json::to_value(&t.input).unwrap_or(Value::Null)))).collect();
        tools.retain(|(n, _)| !n.is_empty());
        let text = s.text.as_ref().map(|t| v.s(&t.repeat(s.repeat_text.unwrap_or(1).max(1))));
        let one = Scripted { text, tools, stop: s.stop.clone(), out_tokens: s.out_tokens.unwrap_or(0), input_tokens: s.input_tokens.unwrap_or(0), slow: s.slow };
        for _ in 0..s.repeat.unwrap_or(1).max(1) { out.push(one.clone()); }
    }
    out
}

/// Runs one case. `model` = the model name for model cases (`--model`), None = the connection's own.
pub fn run_case(c: &Case, opts: &Options, idx: usize) -> CaseResult {
    let mut r = CaseResult { id: c.id.clone(), suite: c.suite.clone(), kind: c.kind.clone(), description: c.description.clone(), ..Default::default() };
    if c.platform.as_deref() == Some("macos") && !cfg!(target_os = "macos") {
        r.status = "skip".into();
        r.notes.push("macOS only".into());
        return r;
    }
    let t0 = Instant::now();
    let root = match fresh_root(idx) {
        Ok(d) => d,
        Err(e) => { r.status = "fail".into(); r.notes.push(e); return r; }
    };
    if !opts.keep { live(&LIVE_ROOTS).push(root.clone()); }
    let seed = (SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(7) as u64) ^ ((idx as u64 + 1) << 32) ^ std::process::id() as u64;
    let vars = Vars::new(&root, seed);
    let out = drive(c, opts, &root, &vars, &mut r);
    r.seconds = t0.elapsed().as_secs_f64();
    match out {
        Ok((turns, fake_reqs, left)) => {
            let ctx = Ctx { case: c, root: &root, vars: &vars, turns: &turns, fake: fake_reqs.as_deref() };
            metrics(&turns, &mut r.metrics);
            if let Some(n) = left.filter(|&n| n > 0) { r.notes.push(format!("{n} scripted replies not used")); }
            for ch in &c.checks { r.checks.push(evaluate(ch, &ctx)); }
            let failed: Vec<&CheckResult> = r.checks.iter().filter(|x| !x.pass).collect();
            r.status = if failed.is_empty() { "pass" } else { "fail" }.into();
            for f in failed { r.notes.push(format!("{}: {}", f.kind, f.detail)); }
        }
        Err(e) => { r.status = "fail".into(); r.notes.push(e); }
    }
    if opts.keep { r.dir = Some(root); } else {
        let _ = std::fs::remove_dir_all(&root);
        live(&LIVE_ROOTS).retain(|d| d != &root);
    }
    r
}

type Driven = (Vec<TurnLog>, Option<Vec<Value>>, Option<usize>);

fn drive(c: &Case, opts: &Options, root: &Path, v: &Vars, r: &mut CaseResult) -> Result<Driven, String> {
    let (project, home) = (root.join("project"), root.join("home"));
    for f in &c.files {
        let p = project.join(&f.path);
        if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        std::fs::write(&p, v.s(&f.content).repeat(f.repeat.unwrap_or(1).max(1))).map_err(|e| format!("{}: {e}", f.path))?;
    }
    for f in &c.home_files {
        let p = home.join(&f.path);
        if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        std::fs::write(&p, v.s(&f.content).repeat(f.repeat.unwrap_or(1).max(1))).map_err(|e| format!("{}: {e}", f.path))?;
    }
    std::fs::write(home.join(".config/elisa/config.toml"), v.s(&c.config)).map_err(|e| e.to_string())?;
    if !c.project_config.is_empty() {
        std::fs::create_dir_all(project.join(".elisa")).map_err(|e| e.to_string())?;
        std::fs::write(project.join(".elisa/config.toml"), v.s(&c.project_config)).map_err(|e| e.to_string())?;
    }
    let fake = c.kind == "fake";
    let backend = c.backend.clone().unwrap_or_default();
    let fm = if fake { Some(FakeModel::start_as(script_of(c, v), c.max_ctx.unwrap_or(32768), &backend).map_err(|e| format!("fake model: {e}"))?) } else { None };
    let mut extra: Vec<String> = Vec::new();
    match &fm {
        Some(f) if backend == "anthropic" => extra.extend(["--base-url".into(), f.url(), "--type".into(), "anthropic".into(), "--model".into(), "eval-fake".into()]),
        Some(f) if !backend.is_empty() => extra.extend(["--base-url".into(), format!("{}/v1", f.url()), "--type".into(), "openai".into(), "--model".into(), "eval-fake".into()]),
        Some(f) => extra.extend(["--base-url".into(), f.url(), "--type".into(), "streamdense".into(), "--model".into(), "eval-fake".into()]),
        None => {
            // only the connection file: the user's config.toml, MCP servers and memory stay out of the case
            if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
                let p = crate::provider::path_in(&h);
                if p.is_file() { std::fs::copy(&p, crate::provider::path_in(&home)).map_err(|e| format!("providers.toml: {e}"))?; }
            }
            extra.extend(connection_args(opts));
        }
    }
    let timeout = Duration::from_secs(c.timeout_s.unwrap_or(if fake { 60 } else { 300 }));
    let deadline = Instant::now() + timeout;
    let mut n_sess = 0;
    let mut slot = Some(Session::start(&opts.exe, root, &extra, fake, n_sess)?);
    let res = (|| -> Result<Vec<TurnLog>, String> {
        let hello = slot.as_ref().ok_or("no session")?.until("ready", deadline)?;
        if let Some(b) = hello.iter().find(|e| e["type"] == "banner") {
            r.model = b["model"].as_str().unwrap_or("").into();
            r.base_url = b["base_url"].as_str().unwrap_or("").into();
        }
        let mut turns = Vec::new();
        let default = c.approve.clone().unwrap_or_else(|| "no".into());
        // the startup lines count as part of the next turn's events (infos, notes)
        let mut pending: Vec<Value> = hello;
        for t in &c.turns {
            if t.restart {
                if let Some(old) = slot.take() { old.stop(); }
                // session files are named by unix second: the next session must not share the last one's
                std::thread::sleep(Duration::from_millis(1100));
                n_sess += 1;
                let mut e = extra.clone();
                if t.cont { e.push("--continue".into()); }
                let ns = Session::start(&opts.exe, root, &e, fake, n_sess)?;
                pending.extend(ns.until("ready", deadline)?);
                slot = Some(ns);
            }
            let s = slot.as_mut().ok_or("no session")?;
            let Some(send) = &t.send else { continue };
            let send = v.s(send);
            let req_start = fm.as_ref().map_or(0, |f| f.requests().len());
            let t1 = Instant::now();
            let mut msg = json!({"type": "send", "text": send});
            if !t.attach.is_empty() {
                msg["documents"] = json!(t.attach.iter().map(|a| json!({"path": root.join("project").join(v.s(a)).display().to_string()})).collect::<Vec<_>>());
            }
            s.cmd(msg);
            let mut tl = TurnLog { send: send.clone(), slash: send.starts_with('/'), req_start, ..Default::default() };
            let mut answers = t.approvals.iter();
            let mut pressed = false;
            loop {
                let e = s.next(deadline, &format!("the end of turn {:?}", crate::context::one_line(&send, 60)))?;
                match e["type"].as_str().unwrap_or("") {
                    "delta" => {
                        tl.reply.push_str(e["text"].as_str().unwrap_or(""));
                        if t.interrupt && !pressed { pressed = true; s.cmd(json!({"type": "interrupt"})); }
                    }
                    "approve" => {
                        let a = answers.next().cloned().unwrap_or_else(|| default.clone());
                        s.cmd(json!({"type": "approve", "answer": a}));
                    }
                    "pick" => s.cmd(json!({"type": "pick", "index": null})),
                    "reply_retracted" => tl.reply.clear(),
                    _ => {}
                }
                let end = e["type"] == "turn_done";
                tl.events.push(e);
                if end { break; }
            }
            tl.seconds = t1.elapsed().as_secs_f64();
            s.until("ready", deadline)?;
            tl.req_end = fm.as_ref().map_or(0, |f| f.requests().len());
            let mut evs = std::mem::take(&mut pending);
            evs.append(&mut tl.events);
            tl.events = evs;
            turns.push(tl);
        }
        // a model case that checks the tools offered asks /context (the model's requests are not visible here)
        if !fake && c.checks.iter().any(|k| k.kind == "no_tools_offered" || k.kind == "tools_offered") {
            let s = slot.as_mut().ok_or("no session")?;
            s.cmd(json!({"type": "send", "text": "/context"}));
            let evs = s.until("turn_done", deadline)?;
            turns.push(TurnLog { send: "/context".into(), slash: true, events: evs, ..Default::default() });
        }
        Ok(turns)
    })();
    if let Some(s) = slot { s.stop(); }
    let turns = res?;
    Ok((turns, fm.as_ref().map(|f| f.requests()), fm.as_ref().map(|f| f.left())))
}

fn metrics(turns: &[TurnLog], m: &mut Metrics) {
    for t in turns {
        let calls = t.events.iter().filter(|e| e["type"] == "reply_done").count();
        if !t.slash {
            m.turn_s.push(t.seconds);
            if calls > 0 && t.reply.trim().is_empty() { m.empty_reply_turns += 1; }
        }
        m.model_calls += calls;
        m.max_calls_per_turn = m.max_calls_per_turn.max(calls);
        for e in &t.events {
            match e["type"].as_str().unwrap_or("") {
                "reply_done" => {
                    m.output_tokens += e["output_tokens"].as_u64().unwrap_or(0) as usize;
                    m.max_ctx_used = m.max_ctx_used.max(e["ctx_used"].as_u64().unwrap_or(0) as usize);
                    if let Some(x) = e["ttft_s"].as_f64() { m.ttft_s.push(x); }
                }
                "tool_start" => m.tool_calls += 1,
                "tool_done" if e["is_error"] == true => m.tool_errors += 1,
                "approve" => m.approvals += 1,
                "thinking" => m.thinking_chars += e["text"].as_str().map_or(0, |s| s.chars().count()),
                "info" => {
                    let t = e["text"].as_str().unwrap_or("");
                    if t.starts_with("[the reply had no text") { m.empty_retries += 1; }
                    if loop_stop(t) { m.loop_guard_stops += 1; }
                }
                _ => {}
            }
        }
    }
}

/// an info line saying the loop guard (or the same-result repeat check) stopped the turn's tool loop: the turn's stop,
/// or the stop that asks the model once for an answer with no tools (`repl::agent_loop`)
fn loop_stop(t: &str) -> bool {
    (t.starts_with("[stopped: ") || (t.starts_with("[loop guard: ") && t.contains("asking the model to answer")))
        && (t.contains("kept repeating") || t.contains("kept cycling") || t.contains("returned the same result") || t.contains("paged the file"))
}

// ---------- checks ----------

struct Ctx<'a> {
    case: &'a Case,
    root: &'a Path,
    vars: &'a Vars,
    turns: &'a [TurnLog],
    fake: Option<&'a [Value]>,
}

impl Ctx<'_> {
    /// the turns a check looks at: its `turn`, else all (or, for reply checks, the last user turn); the `/context`
    /// a model case adds after its turns is not one of them
    fn pick(&self, ch: &Check, last: bool) -> Vec<&TurnLog> {
        let n = self.case.turns.iter().filter(|t| t.send.is_some()).count();
        let case_turns: Vec<&TurnLog> = self.turns.iter().take(n).collect();
        match ch.turn {
            Some(n) => case_turns.get(n.saturating_sub(1)).copied().into_iter().collect(),
            None if last => case_turns.iter().rev().find(|t| !t.slash).copied().into_iter().collect(),
            None => case_turns,
        }
    }
    fn events(&self, ch: &Check) -> Vec<&Value> { self.pick(ch, false).into_iter().flat_map(|t| &t.events).collect() }
    fn reply(&self, ch: &Check) -> String { self.pick(ch, true).iter().map(|t| t.reply.as_str()).collect::<Vec<_>>().join("\n") }
    fn requests(&self, ch: &Check) -> Option<Vec<&Value>> {
        let f = self.fake?;
        Some(match ch.turn {
            Some(_) => self.pick(ch, false).iter().flat_map(|t| &f[t.req_start.min(f.len())..t.req_end.min(f.len())]).collect(),
            None => f.iter().collect(),
        })
    }
    fn path(&self, p: &str) -> PathBuf {
        let p = self.vars.s(p);
        if Path::new(&p).is_absolute() { PathBuf::from(p) } else { self.root.join("project").join(p) }
    }
    /// every file Elisa may have written for the case: the project's `.elisa/` and the temp home (its copied
    /// connection file and config left out)
    fn written(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for d in [self.root.join("project/.elisa"), self.root.join("home")] {
            for e in walkdir::WalkDir::new(&d).into_iter().flatten() {
                let p = e.path();
                if e.file_type().is_file() && !p.starts_with(self.root.join("home/.config")) { out.push(p.to_path_buf()); }
            }
        }
        out
    }
    /// (name, summary, input JSON if known) of every tool call the model made
    fn tool_calls(&self, ch: &Check) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.events(ch).iter().filter(|e| e["type"] == "tool_start")
            .map(|e| (e["name"].as_str().unwrap_or("").to_string(), e["summary"].as_str().unwrap_or("").to_string())).collect();
        // the full inputs, from the saved sessions (the summary is cut short)
        let dir = self.root.join("project/.elisa/sessions");
        let mut inputs: Vec<(String, String)> = Vec::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(h) = serde_json::from_str::<Vec<crate::types::Message>>(&t) else { continue };
            for m in h { for b in m.content { if let crate::types::Block::ToolUse { name, input, .. } = b { inputs.push((name, input.to_string())); } } }
        }
        for (n, s) in out.iter_mut() {
            let more: Vec<&str> = inputs.iter().filter(|(m, _)| m == n).map(|(_, i)| i.as_str()).collect();
            if !more.is_empty() { s.push('\n'); s.push_str(&more.join("\n")); }
        }
        out
    }
}

/// `name` matches a pattern: `*` = any, `a|b` = either, `x*` = prefix
fn name_is(name: &str, pat: &str) -> bool {
    pat.split('|').map(str::trim).any(|p| p == "*" || p == name || (p.ends_with('*') && name.starts_with(&p[..p.len() - 1])))
}

fn short(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 140 { format!("{}…", one.chars().take(140).collect::<String>()) } else { one }
}

fn ok(kind: &str, pass: bool, detail: impl Into<String>) -> CheckResult { CheckResult { kind: kind.into(), pass, detail: detail.into() } }

fn evaluate(ch: &Check, cx: &Ctx) -> CheckResult {
    let k = ch.kind.as_str();
    let text = ch.text.as_deref().map(|t| cx.vars.s(t)).unwrap_or_default();
    match k {
        "reply_contains" | "reply_not_contains" => {
            let reply = cx.reply(ch);
            let wants: Vec<String> = if ch.any.is_empty() { vec![text.clone()] } else { ch.any.iter().map(|a| cx.vars.s(a)).collect() };
            let hit = wants.iter().find(|w| reply.to_lowercase().contains(&w.to_lowercase()));
            let pass = (k == "reply_contains") == hit.is_some();
            ok(k, pass, if pass { format!("{:?}", hit.map_or(&wants[0], |h| h)) } else { format!("{:?} — reply: {:?}", wants.join(" | "), short(&reply)) })
        }
        "reply_matches" => {
            let reply = cx.reply(ch);
            match regex::Regex::new(&ch.regex.clone().unwrap_or_default()) {
                Ok(re) => { let p = re.is_match(&reply); ok(k, p, if p { "matched".to_string() } else { format!("/{}/ — reply: {:?}", re.as_str(), short(&reply)) }) }
                Err(e) => ok(k, false, format!("bad regex: {e}")),
            }
        }
        "reply_not_empty" => { let r = cx.reply(ch); ok(k, !r.trim().is_empty(), if r.trim().is_empty() { "empty reply".into() } else { format!("{} chars", r.len()) }) }
        "tool_called" | "tool_not_called" => {
            let pat = ch.name.clone().unwrap_or_else(|| "*".into());
            let args = ch.args.as_deref().map(|a| cx.vars.s(a));
            let calls = cx.tool_calls(ch);
            let hits: Vec<&(String, String)> = calls.iter().filter(|(n, s)| name_is(n, &pat) && args.as_deref().is_none_or(|a| s.contains(a))).collect();
            let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
            if k == "tool_called" {
                let min = ch.min.unwrap_or(1);
                let p = hits.len() >= min && ch.max.is_none_or(|m| hits.len() <= m);
                ok(k, p, format!("{pat}{}: {} call(s) — calls: [{}]", args.map(|a| format!(" ~{a:?}")).unwrap_or_default(), hits.len(), names.join(", ")))
            } else {
                ok(k, hits.is_empty(), format!("{pat}: {} call(s) — calls: [{}]", hits.len(), names.join(", ")))
            }
        }
        "tool_result_contains" => {
            let pat = ch.name.clone().unwrap_or_else(|| "*".into());
            let evs = cx.events(ch);
            let mut last = String::new();
            let mut outs = Vec::new();
            for e in evs {
                if e["type"] == "tool_start" { last = e["name"].as_str().unwrap_or("").to_string(); }
                if e["type"] == "tool_done" && name_is(&last, &pat) { outs.push(e["output"].as_str().unwrap_or("").to_string()); }
            }
            let p = outs.iter().any(|o| o.contains(&text));
            ok(k, p, if p { format!("{text:?}") } else { format!("{text:?} not in {} result(s) of {pat}: {:?}", outs.len(), outs.iter().map(|o| short(o)).collect::<Vec<_>>()) })
        }
        "no_tools_offered" | "tools_offered" => {
            let prefix = ch.prefix.clone().unwrap_or_default();
            let want_none = k == "no_tools_offered";
            match cx.requests(ch) {
                Some(reqs) => {
                    let offered: Vec<String> = reqs.iter().flat_map(|r| r["tools"].as_array().cloned().unwrap_or_default())
                        .filter_map(|t| t["name"].as_str().map(String::from)).filter(|n| n.starts_with(&prefix)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                    if reqs.is_empty() { return ok(k, false, "no model request to look at"); }
                    let p = offered.is_empty() == want_none;
                    ok(k, p, format!("{prefix}*: offered [{}] over {} request(s)", offered.join(", "), reqs.len()))
                }
                None => {
                    // a model case: `/context` after the turns says what the last turn offered
                    let ctx = cx.turns.iter().rev().find(|t| t.send == "/context");
                    let lines: Vec<String> = ctx.map(|t| t.events.iter().filter(|e| e["type"] == "info").filter_map(|e| e["text"].as_str().map(String::from)).collect()).unwrap_or_default();
                    let key = if ["think", "steps", "step_done", "calc"].iter().any(|r| r.starts_with(&prefix) || prefix.starts_with(r)) { "reasoning tools" }
                        else if prefix.starts_with("docs") { "documents tools" } else if prefix.starts_with("sec") { "security tools" } else { "" };
                    if key.is_empty() { return ok(k, false, format!("{prefix}: only reasoning / docs / sec tools can be checked in a model case")); }
                    let Some(l) = lines.iter().find(|l| l.contains(key)) else {
                        // no such line: the server is not running, so nothing of it is offered
                        return ok(k, want_none, format!("{key}: not running"));
                    };
                    let off = if key == "reasoning tools" { l.contains("not offered") }
                        else { let part = l.split(key).nth(1).unwrap_or(""); part.trim_start().starts_with("not offered") };
                    ok(k, off == want_none, format!("last turn: {}", short(l)))
                }
            }
        }
        "file_contains" | "file_not_contains" => {
            let p = newest_match(&cx.path(ch.path.as_deref().unwrap_or("")));
            match std::fs::read_to_string(&p) {
                Ok(s) => { let has = s.contains(&text); let pass = has == (k == "file_contains"); ok(k, pass, format!("{} {} {text:?}{}", ch.path.as_deref().unwrap_or(""), if has { "has" } else { "lacks" }, if pass { String::new() } else { format!(" — file: {:?}", short(&s)) })) }
                Err(e) => ok(k, k == "file_not_contains", format!("{}: {e}", ch.path.as_deref().unwrap_or(""))),
            }
        }
        "file_unchanged" => {
            let path = ch.path.clone().unwrap_or_default();
            let Some(f) = cx.case.files.iter().find(|f| f.path == path) else { return ok(k, false, format!("{path} is not a fixture file")) };
            let want = cx.vars.s(&f.content).repeat(f.repeat.unwrap_or(1).max(1));
            match std::fs::read_to_string(cx.path(&path)) {
                Ok(s) => ok(k, s == want, if s == want { format!("{path} unchanged") } else { format!("{path} changed: {:?}", short(&s)) }),
                Err(e) => ok(k, false, format!("{path}: {e}")),
            }
        }
        "file_absent" => { let p = cx.path(ch.path.as_deref().unwrap_or("")); ok(k, !p.exists(), format!("{} {}", ch.path.as_deref().unwrap_or(""), if p.exists() { "exists" } else { "absent" })) }
        "max_steps" => {
            let n = ch.n.unwrap_or(0.0) as usize;
            let worst = cx.pick(ch, false).iter().map(|t| t.events.iter().filter(|e| e["type"] == "reply_done").count()).max().unwrap_or(0);
            ok(k, worst <= n, format!("most model calls in a turn: {worst} (max {n})"))
        }
        "max_seconds" => {
            let n = ch.n.unwrap_or(0.0);
            let worst = cx.pick(ch, false).iter().filter(|t| !t.slash).map(|t| t.seconds).fold(0.0, f64::max);
            ok(k, worst <= n, format!("slowest turn {worst:.1}s (max {n}s)"))
        }
        "no_secret_on_disk" => {
            let vals: Vec<String> = ch.values.iter().map(|v| cx.vars.s(v)).filter(|v| !v.is_empty()).collect();
            let files = cx.written();
            let mut bad = Vec::new();
            for p in &files {
                let Ok(b) = std::fs::read(p) else { continue };
                let s = String::from_utf8_lossy(&b);
                for (i, v) in vals.iter().enumerate() { if s.contains(v.as_str()) { bad.push(format!("{} holds value {}", p.strip_prefix(cx.root).unwrap_or(p).display(), i + 1)); } }
            }
            ok(k, bad.is_empty() && !files.is_empty(), if bad.is_empty() { format!("{} values, {} files checked", vals.len(), files.len()) } else { bad.join("; ") })
        }
        "audit_has" => {
            let dir = cx.root.join("home/.elisa/audit");
            let lines: Vec<Value> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
                .flat_map(|e| std::fs::read_to_string(e.path()).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>()).collect();
            let want = |l: &&Value| ch.event.as_deref().is_none_or(|e| l["event"] == e) && ch.reason.as_deref().is_none_or(|r| l["reason"] == r)
                && ch.tool.as_deref().is_none_or(|t| l["tool"].as_str().is_some_and(|x| name_is(x, t)));
            let n = lines.iter().filter(want).count();
            let p = n >= ch.min.unwrap_or(1) && ch.max.is_none_or(|m| n <= m);
            ok(k, p, format!("{n} matching of {} audit entries (event {:?}, reason {:?}, tool {:?})", lines.len(), ch.event, ch.reason, ch.tool))
        }
        "approval_for" | "no_approval_for" => {
            let pat = ch.tool.clone().or(ch.name.clone()).unwrap_or_else(|| "*".into());
            let asked: Vec<String> = cx.events(ch).iter().filter(|e| e["type"] == "approve").map(|e| e["tool"].as_str().unwrap_or("").to_string()).collect();
            let n = asked.iter().filter(|t| name_is(t, &pat)).count();
            let p = (n > 0) == (k == "approval_for");
            ok(k, p, format!("{pat}: {n} approval(s) — asked for [{}]", asked.join(", ")))
        }
        "approvals" => {
            let n = cx.events(ch).iter().filter(|e| e["type"] == "approve").count();
            let p = n >= ch.min.unwrap_or(0) && ch.max.is_none_or(|m| n <= m);
            ok(k, p, format!("{n} approval(s) (min {:?}, max {:?})", ch.min, ch.max))
        }
        "stopped_by_loop_guard" => {
            let hit = cx.events(ch).iter().filter(|e| e["type"] == "info").filter_map(|e| e["text"].as_str()).find(|t| loop_stop(t)).map(String::from);
            ok(k, hit.is_some(), hit.map_or("no loop-guard stop".to_string(), |h| short(&h)))
        }
        "info_contains" | "info_not_contains" => {
            let lines: Vec<String> = cx.events(ch).iter().filter(|e| e["type"] == "info" || e["type"] == "error").filter_map(|e| e["text"].as_str().map(String::from)).collect();
            let hit = lines.iter().find(|l| l.contains(&text));
            let p = hit.is_some() == (k == "info_contains");
            ok(k, p, match hit { Some(h) => short(h), None => format!("{text:?} not in {} info lines", lines.len()) })
        }
        "request_contains" | "request_not_contains" => match cx.requests(ch) {
            None => ok(k, false, "only a scripted (fake) case can see the model requests"),
            Some(reqs) => {
                let n = reqs.iter().filter(|r| r.to_string().contains(&text) || r.to_string().contains(&serde_json::to_string(&text).unwrap_or_default().trim_matches('"').to_string())).count();
                let p = (n > 0) == (k == "request_contains");
                ok(k, p, format!("{text:?} in {n} of {} request(s)", reqs.len()))
            }
        },
        "event" => {
            let kind = ch.event.clone().unwrap_or_default();
            let evs: Vec<&Value> = cx.events(ch).into_iter().filter(|e| e["type"] == kind.as_str()).collect();
            let hit = evs.iter().find(|e| text.is_empty() || e.to_string().contains(&text));
            ok(k, hit.is_some(), match hit { Some(h) => short(&h.to_string()), None => format!("no {kind} event{} ({} of that type)", if text.is_empty() { String::new() } else { format!(" with {text:?}") }, evs.len()) })
        }
        _ => ok(k, false, "unknown check"),
    }
}

// ---------- the run ----------

/// Runs the selected cases; results in case order. Scripted cases run `opts.jobs` at a time, model cases one at a time
/// (one local model serves one request well).
pub fn run_cases(cases: &[Case], opts: &Options) -> Vec<CaseResult> {
    let results: Arc<Mutex<Vec<Option<CaseResult>>>> = Arc::new(Mutex::new(vec![None; cases.len()]));
    let print = |r: &CaseResult| if opts.progress { eprintln!("  {:<4} {:<44} {:>6.1}s{}", r.status.to_uppercase(), r.id, r.seconds, r.notes.first().map(|n| format!("  {}", short(n))).unwrap_or_default()); };
    let fake: Vec<usize> = (0..cases.len()).filter(|&i| cases[i].kind == "fake").collect();
    let queue = Arc::new(Mutex::new(fake.into_iter().collect::<std::collections::VecDeque<usize>>()));
    std::thread::scope(|sc| {
        for _ in 0..opts.jobs.max(1) {
            let (q, res) = (queue.clone(), results.clone());
            sc.spawn(move || loop {
                let Some(i) = q.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front() else { return };
                let r = run_case(&cases[i], opts, i);
                print(&r);
                res.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(r);
            });
        }
    });
    for (i, c) in cases.iter().enumerate().filter(|(_, c)| c.kind == "model") {
        let r = run_case(c, opts, i);
        print(&r);
        results.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(r);
    }
    let v = std::mem::take(&mut *results.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    v.into_iter().flatten().collect()
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() { return None; }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    Some(if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 })
}
fn rate(a: usize, b: usize) -> Value { if b == 0 { Value::Null } else { json!((a as f64 / b as f64 * 1000.0).round() / 1000.0) } }
fn r3(x: Option<f64>) -> Value { x.map_or(Value::Null, |x| json!((x * 1000.0).round() / 1000.0)) }

/// Aggregates per model: pass rate per suite, median turn time, tool-call accuracy, empty-reply rate, loop-guard
/// stops, tokens.
pub fn aggregates(results: &[CaseResult]) -> Vec<Value> {
    let mut by: BTreeMap<(String, String), Vec<&CaseResult>> = BTreeMap::new();
    for r in results { by.entry((r.kind.clone(), if r.model.is_empty() { "(unknown)".into() } else { r.model.clone() })).or_default().push(r); }
    by.into_iter().map(|((kind, model), rs)| {
        let ran: Vec<&&CaseResult> = rs.iter().filter(|r| r.status != "skip").collect();
        let passed = ran.iter().filter(|r| r.status == "pass").count();
        let mut suites = serde_json::Map::new();
        let mut names: Vec<&str> = rs.iter().map(|r| r.suite.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for s in names {
            let n = ran.iter().filter(|r| r.suite == s).count();
            let p = ran.iter().filter(|r| r.suite == s && r.status == "pass").count();
            suites.insert(s.into(), json!({"cases": n, "passed": p, "pass_rate": rate(p, n)}));
        }
        let turns: Vec<f64> = ran.iter().flat_map(|r| r.metrics.turn_s.clone()).collect();
        let ttft: Vec<f64> = ran.iter().flat_map(|r| r.metrics.ttft_s.clone()).collect();
        let tool_checks: Vec<&CheckResult> = ran.iter().flat_map(|r| &r.checks).filter(|c| c.kind == "tool_called" || c.kind == "tool_not_called").collect();
        let tool_ok = tool_checks.iter().filter(|c| c.pass).count();
        let sum = |f: fn(&Metrics) -> usize| ran.iter().map(|r| f(&r.metrics)).sum::<usize>();
        let empty = sum(|m| m.empty_reply_turns);
        json!({
            "model": model, "kind": kind, "base_url": rs.iter().map(|r| r.base_url.as_str()).find(|u| !u.is_empty()).unwrap_or(""),
            "cases": ran.len(), "passed": passed, "failed": ran.len() - passed, "skipped": rs.len() - ran.len(), "pass_rate": rate(passed, ran.len()),
            "suites": suites,
            "turns": turns.len(), "median_turn_s": r3(median(&turns)), "max_turn_s": r3(turns.iter().copied().reduce(f64::max)),
            "median_ttft_s": r3(median(&ttft)),
            "tool_call_accuracy": {"checks": tool_checks.len(), "passed": tool_ok, "rate": rate(tool_ok, tool_checks.len())},
            "empty_reply_turns": empty, "empty_reply_rate": rate(empty, turns.len()), "empty_retries": sum(|m| m.empty_retries),
            "loop_guard_stops": sum(|m| m.loop_guard_stops),
            "model_calls": sum(|m| m.model_calls), "tool_calls": sum(|m| m.tool_calls), "tool_errors": sum(|m| m.tool_errors),
            "output_tokens": sum(|m| m.output_tokens), "max_ctx_used": ran.iter().map(|r| r.metrics.max_ctx_used).max().unwrap_or(0),
            "thinking_chars": sum(|m| m.thinking_chars),
        })
    }).collect()
}

pub fn case_json(r: &CaseResult) -> Value {
    let m = &r.metrics;
    json!({
        "id": r.id, "suite": r.suite, "kind": r.kind, "model": r.model, "description": r.description, "status": r.status,
        "seconds": r3(Some(r.seconds)), "notes": r.notes,
        "checks": r.checks.iter().map(|c| json!({"type": c.kind, "pass": c.pass, "detail": c.detail})).collect::<Vec<_>>(),
        "metrics": {
            "turns": m.turn_s.len(), "turn_s": m.turn_s.iter().map(|x| r3(Some(*x))).collect::<Vec<_>>(),
            "model_calls": m.model_calls, "max_calls_per_turn": m.max_calls_per_turn, "output_tokens": m.output_tokens,
            "max_ctx_used": m.max_ctx_used, "tool_calls": m.tool_calls, "tool_errors": m.tool_errors, "approvals": m.approvals,
            "empty_reply_turns": m.empty_reply_turns, "empty_retries": m.empty_retries, "loop_guard_stops": m.loop_guard_stops,
            "thinking_chars": m.thinking_chars, "median_ttft_s": r3(median(&m.ttft_s)),
        },
        "dir": r.dir.as_ref().map(|d| d.display().to_string()),
    })
}

/// The whole report (schema `elisa-eval/1`, docs/reference/eval.md).
pub fn report(results: &[CaseResult], opts: &Options, started: u64, model: Option<&ModelInfo>) -> Value {
    json!({
        "schema": SCHEMA,
        "elisa_version": env!("CARGO_PKG_VERSION"),
        "started_at": started, "finished_at": now_s(),
        "suite": opts.suite, "case": opts.case,
        "model": model.map(|m| json!({"name": m.name, "connection": m.connection, "base_url": m.base_url})),
        "summary": {
            "cases": results.len(),
            "passed": results.iter().filter(|r| r.status == "pass").count(),
            "failed": results.iter().filter(|r| r.status == "fail").count(),
            "skipped": results.iter().filter(|r| r.status == "skip").count(),
        },
        "aggregates": aggregates(results),
        "cases": results.iter().map(case_json).collect::<Vec<_>>(),
    })
}

/// The readable table and summary.
pub fn table(results: &[CaseResult]) -> String {
    let w = results.iter().map(|r| r.id.len()).max().unwrap_or(4).max(4);
    let mut s = format!("{:<w$}  {:<6} {:>7}  notes\n", "case", "result", "time");
    for r in results {
        let note = if r.status == "pass" { r.notes.first().cloned().unwrap_or_default() } else { r.notes.join(" · ") };
        s.push_str(&format!("{:<w$}  {:<6} {:>6.1}s  {}\n", r.id, r.status.to_uppercase(), r.seconds, short(&note)));
    }
    let mut suites: Vec<&str> = results.iter().map(|r| r.suite.as_str()).collect();
    suites.sort_unstable();
    suites.dedup();
    s.push('\n');
    for su in suites {
        let rs: Vec<&CaseResult> = results.iter().filter(|r| r.suite == su && r.status != "skip").collect();
        let p = rs.iter().filter(|r| r.status == "pass").count();
        s.push_str(&format!("{su:<10} {p}/{} passed\n", rs.len()));
    }
    for a in aggregates(results) {
        if a["kind"] != "model" { continue; }
        let f = |k: &str| a[k].as_f64().map_or("—".to_string(), |x| format!("{x:.2}"));
        s.push_str(&format!("model {} · pass rate {} · median turn {}s · tool-call accuracy {} · empty replies {} · loop-guard stops {} · {} output tokens\n",
            a["model"].as_str().unwrap_or(""), f("pass_rate"), f("median_turn_s"),
            a["tool_call_accuracy"]["rate"].as_f64().map_or("—".to_string(), |x| format!("{x:.2}")), a["empty_reply_turns"], a["loop_guard_stops"], a["output_tokens"]));
    }
    let failed = results.iter().filter(|r| r.status == "fail").count();
    s.push_str(&format!("{} cases: {} passed, {failed} failed, {} skipped\n", results.len(),
        results.iter().filter(|r| r.status == "pass").count(), results.iter().filter(|r| r.status == "skip").count()));
    s
}

/// What a model case's `elisa --serve-jsonl` gets to reach the chosen model: `--provider`, `--base-url` / `--type`,
/// `--max-ctx` and `--model`, as given (the same flags `elisa` takes; nothing is filled in for the user).
pub fn connection_args(o: &Options) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if let Some(p) = &o.provider { a.extend(["--provider".into(), p.clone()]); }
    if let Some(u) = &o.base_url {
        a.extend(["--base-url".into(), u.clone()]);
        if let Some(k) = &o.kind { a.extend(["--type".into(), k.clone()]); }
    }
    if let Some(n) = o.max_ctx { a.extend(["--max-ctx".into(), n.to_string()]); }
    if let Some(m) = &o.model { a.extend(["--model".into(), m.clone()]); }
    a
}

/// The model the model cases talk to: `--model`, else the connection's: `--provider`, else (`--base-url` with `--type`:
/// none), else `ELISA_PROVIDER`, else the file's active one. Err: `--provider` names no saved connection.
#[derive(Debug)]
pub struct ModelInfo { pub name: String, pub connection: String, pub base_url: String }

pub fn chosen_model(home: Option<&Path>, o: &Options) -> Result<Option<ModelInfo>, String> {
    let f = match (home.map(crate::provider::Providers::load), &o.provider) {
        (Some(Err(e)), Some(_)) => return Err(e),
        (Some(r), _) => r.unwrap_or_default(),
        (None, _) => Default::default(),
    };
    let unsaved = o.base_url.is_some() && o.kind.is_some();
    let name = o.provider.clone().or_else(|| unsaved.then(|| crate::provider::NONE.to_string()))
        .or_else(|| std::env::var("ELISA_PROVIDER").ok().filter(|v| !v.is_empty())).unwrap_or_else(|| f.active.clone());
    let p = if name == crate::provider::NONE { None } else { f.providers.get(&name) };
    if let Some(forced) = o.provider.as_deref().filter(|n| *n != crate::provider::NONE) {
        if p.is_none() { return Err(format!("no connection named {forced:?} in ~/.config/elisa/providers.toml")); }
    }
    let Some(m) = o.model.clone().filter(|m| !m.trim().is_empty()).or_else(|| p.map(|p| p.model.clone()).filter(|m| !m.trim().is_empty())) else { return Ok(None) };
    let base_url = o.base_url.as_deref().map(|u| u.trim().trim_end_matches('/').to_string()).or_else(|| p.map(|p| p.address())).unwrap_or_default();
    Ok(Some(ModelInfo { name: m, connection: if p.is_some() { name } else { String::new() }, base_url }))
}

/// Where `--save` keeps a run's whole report: `~/.elisa/evals/reports/<started>-<model or suite>.json`.
pub fn report_path(home: &Path, started: u64, model: Option<&str>, suite: &str) -> PathBuf {
    let tag: String = model.unwrap_or(suite).chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).take(80).collect();
    home.join(".elisa/evals/reports").join(format!("{started}-{tag}.json"))
}

/// The command line: options, and whether `--list` was asked. Err(None) = `--help` was printed.
fn parse_cli(args: &[String]) -> Result<(Options, bool), Option<String>> {
    let mut o = Options { history: true, progress: true, ..Default::default() };
    let mut list = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or_else(|| format!("{n} needs a value"));
        let r: Result<(), String> = match a.as_str() {
            "--suite" => val("--suite").map(|v| o.suite = v),
            "--case" => val("--case").map(|v| o.case = Some(v)),
            "--model" => val("--model").map(|v| o.model = Some(v)),
            "--provider" => val("--provider").map(|v| o.provider = Some(v)),
            "--base-url" => val("--base-url").map(|v| o.base_url = Some(v)),
            "--type" => val("--type").and_then(|v| if ["streamdense", "anthropic", "openai"].contains(&v.as_str()) { o.kind = Some(v); Ok(()) } else { Err(format!("bad --type: {v} (streamdense | anthropic | openai)")) }),
            "--max-ctx" => val("--max-ctx").and_then(|v| v.parse().map(|n| o.max_ctx = Some(n)).map_err(|_| format!("bad --max-ctx: {v}"))),
            "--json" => val("--json").map(|v| o.json = Some(PathBuf::from(v))),
            "--save" => { o.save = true; Ok(()) }
            "--dir" => val("--dir").map(|v| o.dir = Some(PathBuf::from(v))),
            "--jobs" => val("--jobs").and_then(|v| v.parse().map(|n| o.jobs = n).map_err(|_| format!("bad --jobs: {v}"))),
            "--keep" => { o.keep = true; Ok(()) }
            "--list" => { list = true; Ok(()) }
            "--no-history" => { o.history = false; Ok(()) }
            "-h" | "--help" => { println!("{USAGE}"); return Err(None); }
            _ => Err(format!("unknown argument: {a}")),
        };
        r.map_err(Some)?;
    }
    if o.kind.is_some() && o.base_url.is_none() { return Err(Some("--type goes with --base-url".into())); }
    Ok((o, list))
}

/// `elisa eval …`: the exit code (0 all passed, 1 a case failed, 2 a usage error or no model).
pub fn cli(args: &[String]) -> i32 {
    let (mut o, list) = match parse_cli(args) {
        Ok(x) => x,
        Err(None) => return 0,
        Err(Some(e)) => { eprintln!("elisa eval: {e}\n{USAGE}"); return 2; }
    };
    let all = match load_cases(o.dir.as_deref()) { Ok(c) => c, Err(e) => { eprintln!("elisa eval: {e}"); return 2; } };
    if list {
        for c in &all { println!("{:<44} {:<9} {:<6} {}", c.id, c.suite, c.kind, c.description); }
        return 0;
    }
    let cases = match select(&all, &o.suite, o.case.as_deref()) { Ok(c) => c, Err(e) => { eprintln!("elisa eval: {e}"); return 2; } };
    o.exe = match std::env::current_exe() { Ok(p) => p, Err(e) => { eprintln!("elisa eval: cannot find the elisa binary: {e}"); return 2; } };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let model = match chosen_model(home.as_deref(), &o) { Ok(m) => m, Err(e) => { eprintln!("elisa eval: {e}"); return 2; } };
    if cases.iter().any(|c| c.kind == "model") && model.is_none() {
        eprintln!("{}", crate::api::NO_MODEL);
        return 2;
    }
    stop_on_signal();
    let started = now_s();
    let n_model = cases.iter().filter(|c| c.kind == "model").count();
    eprintln!("elisa eval: {} case(s){}", cases.len(), match (&model, n_model) {
        (Some(m), n) if n > 0 => format!(" · {n} with model {}{}", m.name, if m.connection.is_empty() { String::new() } else { format!(" (connection {})", m.connection) }),
        _ => String::new(),
    });
    let results = run_cases(&cases, &o);
    print!("\n{}", table(&results));
    let rep = report(&results, &o, started, model.as_ref().filter(|_| n_model > 0));
    if let Some(p) = &o.json {
        match std::fs::write(p, serde_json::to_string_pretty(&rep).unwrap_or_default() + "\n") {
            Ok(()) => eprintln!("report: {}", p.display()),
            Err(e) => eprintln!("elisa eval: {}: {e}", p.display()),
        }
    }
    if o.keep { for r in &results { if let Some(d) = &r.dir { eprintln!("kept {}: {}", r.id, d.display()); } } }
    let mut saved: Option<PathBuf> = None;
    if let (true, Some(h)) = (o.save, &home) {
        let p = report_path(h, started, rep["model"]["name"].as_str(), &o.suite);
        let w = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, serde_json::to_string_pretty(&rep).unwrap_or_default() + "\n"));
        match w { Ok(()) => { eprintln!("saved: {}", p.display()); saved = Some(p); } Err(e) => eprintln!("elisa eval: report not saved: {e}") }
    }
    if o.history {
        if let Some(h) = &home {
            let d = h.join(".elisa/evals");
            let line = json!({"schema": SCHEMA, "elisa_version": env!("CARGO_PKG_VERSION"), "started_at": started, "finished_at": rep["finished_at"],
                "suite": o.suite, "case": o.case, "model": rep["model"], "summary": rep["summary"], "aggregates": rep["aggregates"],
                "cases": results.iter().map(|r| json!({"id": r.id, "status": r.status, "seconds": r3(Some(r.seconds))})).collect::<Vec<_>>(),
                "report": saved.as_ref().map(|p| p.display().to_string())});
            let w = std::fs::create_dir_all(&d).and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(d.join("history.jsonl")))
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = w { eprintln!("elisa eval: run history not saved: {e}"); }
        }
    }
    i32::from(results.iter().any(|r| r.status == "fail"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_cases_parse_and_have_unique_ids() {
        let all = load_cases(None).unwrap();
        assert!((20..=100).contains(&all.len()), "{} cases", all.len());
        assert!(all.iter().any(|c| c.kind == "model") && all.iter().any(|c| c.kind == "fake"));
        assert_eq!(select(&all, "model", None).unwrap().iter().filter(|c| c.kind != "model").count(), 0);
        assert!(select(&all, "nope", None).is_err());
    }

    #[test]
    fn bad_case_files_say_where() {
        let e = parse_cases("x.toml", "suite = \"s\"\n[[case]]\nid = \"a\"\n[[case.turns]]\nsend = \"hi\"\n[[case.checks]]\ntype = \"reply_containz\"\n").unwrap_err();
        assert!(e.contains("x.toml: case a") && e.contains("reply_containz"), "{e}");
        let e = parse_cases("x.toml", "[[case]]\nid = \"a\"\nsuite = \"s\"\ntypo = 1\n").unwrap_err();
        assert!(e.contains("typo"), "{e}");
    }

    #[test]
    fn placeholders_and_name_patterns() {
        let v = Vars::new(Path::new("/r"), 42);
        let s = v.s("K={{secret1}} at {{project}}");
        assert!(s.starts_with("K=Qw7") && s.ends_with(" at /r/project") && !s.contains("{{"), "{s}");
        assert_ne!(v.s("{{secret1}}"), v.s("{{secret2}}"));
        assert!(name_is("read", "read|grep") && name_is("sec_secrets", "sec_*") && name_is("x", "*") && !name_is("bash", "read"));
        assert!(loop_stop("[stopped: the model kept repeating calls that already ran; say how to proceed]"));
        assert!(!loop_stop("[stopped after 30 steps; say \"continue\" to go on]"));
        assert!(loop_stop("[loop guard: the model kept repeating calls that already ran; asking the model to answer with what it has, no tools]"));
        assert!(!loop_stop("[loop guard: 10 tool steps without a change. If you have enough to answer, answer now]"));
    }

    #[test]
    fn aggregates_count_per_model_and_suite() {
        let mk = |id: &str, suite: &str, st: &str, t: f64| CaseResult { id: id.into(), suite: suite.into(), kind: "model".into(), status: st.into(), model: "m1".into(),
            metrics: Metrics { turn_s: vec![t], ..Default::default() },
            checks: vec![CheckResult { kind: "tool_called".into(), pass: st == "pass", detail: String::new() }], ..Default::default() };
        let a = aggregates(&[mk("a", "model", "pass", 1.0), mk("b", "model", "fail", 3.0), mk("c", "model", "skip", 9.0)]);
        assert_eq!(a.len(), 1);
        assert_eq!((a[0]["cases"].as_u64(), a[0]["passed"].as_u64(), a[0]["skipped"].as_u64()), (Some(2), Some(1), Some(1)));
        assert_eq!(a[0]["pass_rate"], json!(0.5));
        assert_eq!(a[0]["median_turn_s"], json!(2.0));
        assert_eq!(a[0]["tool_call_accuracy"]["rate"], json!(0.5));
        assert_eq!(a[0]["suites"]["model"]["passed"], json!(1));
    }

    fn args(s: &str) -> Vec<String> { s.split_whitespace().map(String::from).collect() }

    #[test]
    fn connection_flags_reach_the_model_sessions_as_given() {
        let (o, list) = parse_cli(&args("--suite model --model m1 --provider box --json r.json --save")).unwrap();
        assert!(!list && o.save && o.history);
        assert_eq!(connection_args(&o), args("--provider box --model m1"));
        let (o, _) = parse_cli(&args("--base-url http://127.0.0.1:11434/v1 --type openai --max-ctx 8192 --model q")).unwrap();
        assert_eq!(connection_args(&o), args("--base-url http://127.0.0.1:11434/v1 --type openai --max-ctx 8192 --model q"));
        // nothing chosen: nothing added (Elisa does not pick)
        assert!(connection_args(&parse_cli(&[]).unwrap().0).is_empty());
        assert!(parse_cli(&args("--type openai")).unwrap_err().unwrap().contains("--base-url"));
        assert!(parse_cli(&args("--type gpt --base-url x")).is_err());
        assert!(parse_cli(&args("--provider")).unwrap_err().unwrap().contains("needs a value"));
    }

    #[test]
    fn chosen_model_follows_provider_and_base_url() {
        let home = std::env::temp_dir().join(format!("elisa-eval-choose-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
        std::fs::write(crate::provider::path_in(&home), "active = \"a\"\n[providers.a]\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"ma\"\nmax_ctx = 8192\n[providers.b]\ntype = \"openai\"\nbase_url = \"https://b.example/v1\"\nmodel = \"mb\"\nmax_ctx = 8192\n").unwrap();
        let o = |s: &str| parse_cli(&args(s)).unwrap().0;
        let m = chosen_model(Some(&home), &o("--provider b")).unwrap().unwrap();
        assert_eq!((m.name.as_str(), m.connection.as_str(), m.base_url.as_str()), ("mb", "b", "https://b.example/v1"));
        let m = chosen_model(Some(&home), &o("--provider b --model x")).unwrap().unwrap();
        assert_eq!((m.name.as_str(), m.connection.as_str()), ("x", "b"));
        assert!(chosen_model(Some(&home), &o("--provider nope")).unwrap_err().contains("nope"));
        // an unsaved server: no connection, and no model unless one is given
        let m = chosen_model(Some(&home), &o("--base-url http://127.0.0.1:11434/v1/ --type openai --model q")).unwrap().unwrap();
        assert_eq!((m.name.as_str(), m.connection.as_str(), m.base_url.as_str()), ("q", "", "http://127.0.0.1:11434/v1"));
        assert!(chosen_model(Some(&home), &o("--base-url http://127.0.0.1:11434/v1 --type openai")).unwrap().is_none());
        assert!(chosen_model(Some(&home), &o("--provider none")).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn saved_reports_are_named_by_time_and_model() {
        let p = report_path(Path::new("/h"), 1794150000, Some("qwen3-vl:2b-instruct"), "model");
        assert_eq!(p, PathBuf::from("/h/.elisa/evals/reports/1794150000-qwen3-vl_2b-instruct.json"));
        assert_eq!(report_path(Path::new("/h"), 5, None, "fake").file_name().unwrap(), "5-fake.json");
        assert!(!report_path(Path::new("/h"), 5, Some("../x/y"), "s").to_string_lossy().contains("/x/"));
    }
}
