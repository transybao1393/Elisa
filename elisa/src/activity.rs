//! What Elisa is doing in the background, in plain words for the person (not tool names): the one mapping from a
//! tool call to a friendly phrase ("Searching the web for “Apple news”…" → "Searched the web for “Apple news”"), a
//! short result summary ("3 results", "12 pages"), and one quiet line per reply ("Searched the web · read 2 pages ·
//! 3.1 s"). Displays get `Activity` events through `Frontend::activity` (`--serve-jsonl`: the `activity` event).
//!
//! Arguments are shortened and redacted (`secrets::redact`); a path shows only its file name, so nothing from
//! outside the project is spelled out.
//!
//! Other code that does slow work for the user (reading an attached document, say) reports it with a `Turn` step:
//! `let id = turn.begin(fe, &activity::reading_doc("report.pdf", Some(12)));` … `turn.end_with(fe, id, "12 pages", false);`
//! (or, with no `Turn` at hand, `activity::Step::start(fe, phrase)` … `step.finish(fe, "12 pages", false)`).
use crate::frontend::Frontend;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// a model request with no first token after this long, when the model is likely cold, says it is loading
pub const SLOW_MS: u64 = 1500;
/// a step shorter than this need not take the live line (displays may skip it; it still counts in the summary)
pub const QUICK_MS: u64 = 300;
/// Ollama unloads an idle model after 5 minutes by default: a request after this long may load it again
const COLD_AFTER: Duration = Duration::from_secs(270);
/// longest argument shown in a phrase
const ARG_MAX: usize = 40;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_id() -> u64 { NEXT_ID.fetch_add(1, Ordering::Relaxed) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// a step began: `text` is the live line
    Start,
    /// the same step, new words (waiting for the person's OK, a plugin server starting, the answer streaming)
    Update,
    /// the step finished: `text` is the past-tense line, `summary` the short result, `ms` its duration
    Done,
    /// the reply finished: `text` is its one quiet line; sent only when the reply had steps
    Summary,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self { Phase::Start => "start", Phase::Update => "update", Phase::Done => "done", Phase::Summary => "summary" }
    }
}

/// One activity event.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub id: u64,
    pub phase: Phase,
    /// model | web | docs | files | edit | run | code | think | time | security | plugin | voice | other
    pub kind: String,
    pub text: String,
    /// Done: the short result ("3 results", "12 pages", "exit 0"); may be empty
    pub summary: String,
    /// Done: the step's duration; Summary: the reply's
    pub ms: u64,
    /// Done: the step failed
    pub error: bool,
    /// Done: the step did not run: the person said no, it was stopped, or it repeated a call already made (no
    /// failure)
    pub skipped: bool,
    /// Update: an approval is open for this step
    pub waiting_ok: bool,
    /// Start of a model request: after this many ms with no first token, show this text instead
    pub slow: Option<(u64, String)>,
    /// Summary: how many steps the reply had
    pub steps: usize,
}

impl Activity {
    fn new(id: u64, phase: Phase, kind: &str, text: String) -> Self {
        Activity { id, phase, kind: kind.into(), text, summary: String::new(), ms: 0, error: false, skipped: false, waiting_ok: false, slow: None, steps: 0 }
    }
    /// the `activity` event's fields (`--serve-jsonl`)
    pub fn json(&self) -> Value {
        let mut v = json!({"phase": self.phase.as_str(), "id": self.id, "kind": self.kind, "text": self.text});
        match self.phase {
            Phase::Start => if let Some((ms, t)) = &self.slow { v["slow_after_ms"] = json!(ms); v["slow_text"] = json!(t); },
            Phase::Update => v["waiting_ok"] = json!(self.waiting_ok),
            Phase::Done => { v["summary"] = json!(self.summary); v["ms"] = json!(self.ms); v["error"] = json!(self.error); if self.skipped { v["skipped"] = json!(true); } }
            Phase::Summary => { v["ms"] = json!(self.ms); v["steps"] = json!(self.steps); }
        }
        v
    }
    /// the line a display shows `elapsed_ms` after this Start / Update (the slow text once it applies)
    pub fn live_text(&self, elapsed_ms: u64) -> &str {
        match &self.slow { Some((ms, t)) if elapsed_ms >= *ms => t, _ => &self.text }
    }
}

/// A step in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phrase {
    pub kind: &'static str,
    /// what the reply's summary line counts it under (`web_search`, `read`, `plugin:time`, …)
    pub group: String,
    /// what it acts on, for counting distinct files ("README.md"); may be empty
    pub target: String,
    /// present tense, no ellipsis: "Searching the web for “Apple news”"
    pub act: String,
    /// past tense: "Searched the web for “Apple news”"
    pub done: String,
}

impl Phrase {
    pub fn new(kind: &'static str, group: &str, target: &str, act: impl Into<String>, done: impl Into<String>) -> Self {
        Phrase { kind, group: group.into(), target: target.into(), act: act.into(), done: done.into() }
    }
    /// the live line: "Reading README.md…"
    pub fn live(&self) -> String { format!("{}…", self.act) }
}

fn sv<'a>(input: &'a Value, k: &str) -> &'a str { input.get(k).and_then(Value::as_str).unwrap_or("") }

/// a redacted, one-line, shortened argument
pub fn arg(s: &str) -> String { short(s, ARG_MAX) }

/// redacted, paths shortened, one line, at most `n` chars (cut at a word end when there is one)
fn short(s: &str, n: usize) -> String {
    let (r, _) = crate::secrets::redact(s);
    let flat = crate::context::one_line(&tidy_paths(&r), usize::MAX);
    if flat.chars().count() <= n { return flat; }
    let head: String = flat.chars().take(n).collect();
    let cut = match head.rfind(' ') { Some(i) if i > n / 2 => head[..i].trim_end_matches([',', ';', ':', '.', '-']).to_string(), _ => head };
    format!("{cut}…")
}

/// a call the loop guard refused as a repeat (`loop_guard.rs`): not a failure the person needs to see
fn repeat(output: &str) -> bool { output.starts_with("[elisa: not run") }

/// the output without the `<untrusted …>` fence lines around web / third-party results
fn unfenced(output: &str) -> String {
    output.lines().filter(|l| { let t = l.trim_start(); !t.starts_with("<untrusted") && !t.starts_with("</untrusted") }).collect::<Vec<_>>().join("\n")
}

/// a path's file name only ("/Users/me/a/report.pdf" → "report.pdf"); "" stays ""
pub fn file_name(p: &str) -> String {
    let p = p.trim().trim_end_matches('/');
    let name = Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string());
    let (r, _) = crate::secrets::redact(&name);
    crate::context::one_line(&r, ARG_MAX)
}

/// every absolute or home path word in `s` shortened to its file name (commands and patterns never show full paths)
fn tidy_paths(s: &str) -> String {
    s.split(' ').map(|w| {
        let bare = w.trim_matches(|c| c == '"' || c == '\'');
        if (bare.starts_with('/') || bare.starts_with("~/")) && bare.len() > 1 { w.replace(bare, &file_name(bare)) } else { w.to_string() }
    }).collect::<Vec<_>>().join(" ")
}

/// a URL's host without `www.` ("https://www.reuters.com/x" → "reuters.com")
pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let h = rest.split(['/', '?', '#']).next().unwrap_or("");
    let h = h.rsplit('@').next().unwrap_or(h);
    let h = h.split(':').next().unwrap_or(h);
    let h = h.strip_prefix("www.").unwrap_or(h);
    if h.is_empty() { arg(url) } else { arg(h) }
}

fn quoted(s: &str) -> String { format!("\u{201c}{}\u{201d}", arg(s)) }

/// a friendly name for an MCP server, as "Starting …" says it
pub fn server_name(server: &str) -> String {
    match server {
        "web" => "the web reader".into(),
        "docs" => "the document reader".into(),
        "run" => "the code sandbox".into(),
        "reason" => "the thinking helper".into(),
        "sec" => "the security checker".into(),
        s => format!("the {} plugin", arg(s)),
    }
}

/// "Starting the web reader…": the first, lazy start of an MCP server
pub fn starting(server: &str) -> String { format!("Starting {}…", server_name(server)) }

/// The phrase for a document the person attached (for the attachments code): "Reading report.pdf (12 pages)".
pub fn reading_doc(file: &str, pages: Option<usize>) -> Phrase {
    let f = file_name(file);
    let act = match pages {
        Some(n) => format!("Reading {f} ({n} page{})", if n == 1 { "" } else { "s" }),
        None => format!("Reading {f}"),
    };
    Phrase::new("docs", "doc", &f, act, format!("Read {f}"))
}

fn lang_name(l: &str) -> String {
    match l.to_ascii_lowercase().as_str() {
        "python" | "py" => "Python".into(), "javascript" | "js" | "node" => "JavaScript".into(), "typescript" | "ts" => "TypeScript".into(),
        "rust" | "rs" => "Rust".into(), "go" | "golang" => "Go".into(), "ruby" | "rb" => "Ruby".into(), "bash" | "sh" | "shell" => "shell".into(),
        "" => String::new(), o => arg(o),
    }
}

/// what a shell command is, in words: (group, act, done)
fn shell_phrase(cmd: &str) -> (&'static str, String, String) {
    let c = cmd.trim();
    let words: Vec<&str> = c.split_whitespace().collect();
    let has = |w: &str| words.contains(&w);
    let first = words.first().copied().unwrap_or("");
    let test = has("test") || words.iter().any(|w| { let f = w.rsplit('/').next().unwrap_or(w); f.starts_with("test") || f.contains("_test") || f.contains("tests") }) || has("tests") || first == "pytest" || first.ends_with("jest") || first == "vitest" || has("rspec") || c.contains("unittest");
    if test { return ("tests", "Running the tests".into(), "Ran the tests".into()); }
    let build = has("build") || first == "make" || first == "tsc" || first == "swiftc" || first == "gcc" || first == "clang";
    if build { return ("build", "Building the project".into(), "Built the project".into()); }
    if has("install") || (has("add") && matches!(first, "npm" | "pnpm" | "yarn" | "cargo" | "bun")) {
        return ("install", "Installing packages".into(), "Installed packages".into());
    }
    if matches!(first, "clippy" | "eslint" | "ruff" | "flake8") || has("clippy") || has("lint") { return ("lint", "Checking the code".into(), "Checked the code".into()); }
    if first == "git" { return ("git", format!("Checking git ({})", arg(words.get(1).copied().unwrap_or("status"))), "Checked git".into()); }
    let shown = arg(c);
    ("run", format!("Running {shown}"), format!("Ran {shown}"))
}

/// The one mapping from a tool call to its phrase. `server` / `remote`: the MCP server a tool belongs to and the
/// tool's own name there (None for Elisa's own tools); `cwd` tells a new file from an existing one.
pub fn describe(name: &str, input: &Value, cwd: &Path, server: Option<&str>, remote: Option<&str>) -> Phrase {
    let path = || file_name(sv(input, "path"));
    match name {
        "read" => { let f = path(); Phrase::new("files", "read", &f, format!("Reading {f}"), format!("Read {f}")) }
        "outline" => { let f = path(); Phrase::new("files", "read", &f, format!("Looking over {f}"), format!("Looked over {f}")) }
        "read_symbol" => {
            let f = path();
            let s = arg(sv(input, "name"));
            Phrase::new("files", "read", &f, format!("Reading {s} in {f}"), format!("Read {s} in {f}"))
        }
        "glob" => Phrase::new("files", "look", "", "Looking through your project files", "Looked through your project files"),
        "grep" => {
            let q = quoted(sv(input, "pattern"));
            Phrase::new("files", "search_project", "", format!("Searching your project for {q}"), format!("Searched your project for {q}"))
        }
        "write" => {
            let f = path();
            let p = sv(input, "path");
            let exists = !p.is_empty() && { let pp = Path::new(p); if pp.is_absolute() { pp.exists() } else { cwd.join(pp).exists() } };
            if exists { Phrase::new("edit", "edit", &f, format!("Rewriting {f}"), format!("Rewrote {f}")) }
            else { Phrase::new("edit", "create", &f, format!("Creating {f}"), format!("Created {f}")) }
        }
        "edit" | "edit_symbol" => { let f = path(); Phrase::new("edit", "edit", &f, format!("Editing {f}"), format!("Edited {f}")) }
        "bash" => { let (g, a, d) = shell_phrase(sv(input, "command")); Phrase::new("run", g, "", a, d) }
        "web_search" => {
            let q = quoted(sv(input, "query"));
            Phrase::new("web", "web_search", "", format!("Searching the web for {q}"), format!("Searched the web for {q}"))
        }
        "web_fetch" => { let h = host(sv(input, "url")); Phrase::new("web", "web_fetch", &h, format!("Reading {h}"), format!("Read {h}")) }
        "docs_pdf" => {
            let f = path();
            let pages = sv(input, "pages");
            let act = if pages.is_empty() { format!("Reading {f}") } else { format!("Reading {f} (pages {})", arg(pages)) };
            Phrase::new("docs", "doc", &f, act, format!("Read {f}"))
        }
        "docs_sheet" => { let f = path(); Phrase::new("docs", "doc", &f, format!("Reading the spreadsheet {f}"), format!("Read the spreadsheet {f}")) }
        "think" => Phrase::new("think", "think", "", "Thinking step by step", "Thought it through"),
        "steps" => Phrase::new("think", "plan", "", "Planning the steps", "Planned the steps"),
        "step_done" => {
            let n = input.get("n").map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default();
            Phrase::new("think", "plan", "", format!("Checking off step {n}"), format!("Checked off step {n}"))
        }
        "calc" => Phrase::new("think", "calc", "", "Calculating", "Calculated"),
        "run_code" => {
            let l = lang_name(sv(input, "language"));
            let what = if l.is_empty() { "code".to_string() } else { format!("{l} code") };
            Phrase::new("code", "code", "", format!("Running {what} in the sandbox"), format!("Ran {what}"))
        }
        "sec_secrets" => Phrase::new("security", "sec", "", "Checking for leaked secrets", "Checked for leaked secrets"),
        "sec_deps" => Phrase::new("security", "sec", "", "Checking your dependencies for known problems", "Checked your dependencies"),
        "sec_binary" => { let f = path(); Phrase::new("security", "sec", &f, format!("Inspecting {f}"), format!("Inspected {f}")) }
        "sec_jwt" => Phrase::new("security", "sec", "", "Decoding a token", "Decoded a token"),
        _ => {
            let srv = server.unwrap_or("");
            let tool = remote.unwrap_or_else(|| if srv.is_empty() { name } else { name.strip_prefix(&format!("{srv}_")).unwrap_or(name) });
            if srv == "time" || (srv.is_empty() && name.contains("time")) || tool.contains("current_time") {
                return Phrase::new("time", "time", "", "Checking the time", "Checked the time");
            }
            if srv.is_empty() {
                let t = arg(&tool.replace('_', " "));
                return Phrase::new("other", "other", "", format!("Using {t}"), format!("Used {t}"));
            }
            let (s, t) = (arg(srv), arg(&tool.replace('_', " ")));
            Phrase::new("plugin", &format!("plugin:{s}"), "", format!("Using {s} tool {t}"), format!("Used {s} tool {t}"))
        }
    }
}

/// The short result of a finished step: "3 results", "12 pages", "exit 0", "lines 1–40"; may be empty.
pub fn result_summary(name: &str, output: &str, is_error: bool) -> String {
    let output = &unfenced(output);
    let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
    if is_error {
        if repeat(output) { return "already done".into(); }
        if output.starts_with("denied") { return "you said no".into(); }
        if output.starts_with("cancelled") { return "stopped".into(); }
    }
    let s = match name {
        "read" | "glob" | "grep" | "bash" | "outline" | "read_symbol" => crate::jsonl::tool_note(name, output, is_error).0,
        _ if is_error => crate::jsonl::first_note(output).unwrap_or_else(|| "failed".into()),
        "web_search" => {
            let n = output.lines().filter(|l| { let t = l.trim_start(); t.split_once(". ").is_some_and(|(d, _)| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())) && !l.starts_with(' ') }).count();
            if output.contains("no results for") && n == 0 { "no results".into() } else { plural(n, "result") }
        }
        "web_fetch" => output.lines().find_map(|l| l.strip_prefix("title: ")).map(|t| crate::context::one_line(t, ARG_MAX)).unwrap_or_default(),
        "docs_pdf" => output.lines().find_map(|l| {
            let i = l.find(" page")?;
            let head = &l[..i];
            let n: String = head.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
            (!n.is_empty() && head.contains(": ")).then(|| format!("{n} page{}", if n == "1" { "" } else { "s" }))
        }).unwrap_or_default(),
        "docs_sheet" => plural(output.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('[') && !l.starts_with('<')).count(), "row"),
        "run_code" => output.lines().next().and_then(|l| l.split(" · ").next()).filter(|l| l.starts_with("exit")).unwrap_or("").to_string(),
        "calc" => crate::context::one_line(output.lines().find(|l| !l.trim().is_empty()).unwrap_or(""), ARG_MAX),
        "think" | "step_done" => String::new(),
        "steps" => { let n = output.lines().filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit())).count(); if n > 0 { plural(n, "step") } else { String::new() } }
        _ => crate::jsonl::first_note(output).unwrap_or_default(),
    };
    short(&s, 60)
}

/// the step's last line: "Read apple.com", "Reading apple.com didn't work", "Skipped: editing app.py (you said no)"
pub fn done_text(p: &Phrase, output: &str, is_error: bool) -> String {
    if !is_error { return p.done.clone(); }
    let low = lower_first(&p.act);
    if repeat(output) { format!("Skipped: {low} again (already done)") }
    else if output.starts_with("denied") { format!("Skipped: {low} (you said no)") }
    else if output.starts_with("cancelled") { format!("Stopped: {low}") }
    else { format!("{} didn't work", p.act) }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() { Some(f) => f.to_lowercase().chain(c).collect(), None => String::new() }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() { Some(f) => f.to_uppercase().chain(c).collect(), None => String::new() }
}

/// how one group of finished steps reads in the reply's summary line ("read 2 pages")
fn group_words(group: &str, n: usize, targets: &[String]) -> String {
    let distinct = targets.len().max(1);
    let one = targets.first().cloned().unwrap_or_default();
    let count = |verb: &str, noun: &str, single: &str| {
        if distinct == 1 && !one.is_empty() { format!("{verb} {one}") }
        else if distinct == 1 { single.to_string() }
        else { format!("{verb} {distinct} {noun}s") }
    };
    match group {
        "web_search" if n > 1 => format!("searched the web {n} times"),
        "web_search" => "searched the web".into(),
        "web_fetch" => format!("read {distinct} page{}", if distinct == 1 { "" } else { "s" }),
        "read" => count("read", "file", "read a file"),
        "look" => "looked through your files".into(),
        "search_project" => "searched your project".into(),
        "edit" => count("edited", "file", "edited a file"),
        "create" => count("created", "file", "created a file"),
        "tests" => "ran the tests".into(),
        "build" => "built the project".into(),
        "install" => "installed packages".into(),
        "lint" => "checked the code".into(),
        "git" => "checked git".into(),
        "run" if n > 1 => format!("ran {n} commands"),
        "run" => "ran a command".into(),
        "doc" => count("read", "document", "read a document"),
        "think" => "thought it through".into(),
        "plan" => "planned the steps".into(),
        "calc" => "calculated".into(),
        "code" if n > 1 => format!("ran code {n} times"),
        "code" => "ran code".into(),
        "time" => "checked the time".into(),
        "sec" => "ran a security check".into(),
        g => match g.strip_prefix("plugin:") { Some(s) => format!("used {s}"), None => "used a tool".into() },
    }
}

/// "3.1 s", "12 s", "1 m 5 s"
pub fn secs(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 10.0 { format!("{s:.1} s") } else if s < 60.0 { format!("{s:.0} s") } else { format!("{} m {} s", ms / 60_000, ms / 1000 % 60) }
}

/// The reply's one quiet line from its finished steps (group, target, failed), in order: "Searched the web · read 2
/// pages · 3.1 s". Denied / stopped steps are left out; None when nothing is left.
pub fn summary_line(steps: &[(String, String, bool)], total_ms: u64) -> Option<String> {
    let mut groups: Vec<(&str, usize, Vec<String>)> = Vec::new();
    for (g, t, skipped) in steps {
        if *skipped { continue; }
        match groups.iter_mut().find(|(x, ..)| x == g) {
            Some((_, n, ts)) => { *n += 1; if !t.is_empty() && !ts.contains(t) { ts.push(t.clone()); } }
            None => groups.push((g, 1, if t.is_empty() { vec![] } else { vec![t.clone()] })),
        }
    }
    if groups.is_empty() { return None; }
    let mut parts: Vec<String> = groups.iter().map(|(g, n, ts)| group_words(g, *n, ts)).collect();
    parts.push(secs(total_ms));
    Some(upper_first(&parts.join(" · ")))
}

/// The steps of one user turn: emits Start / Update / Done through a frontend and the reply's Summary at the end.
/// Kept by the session across turns (it remembers when the model last answered, to tell a cold model).
#[derive(Debug)]
pub struct Turn {
    t0: Instant,
    /// open steps: id, phrase, start
    open: Vec<(u64, Phrase, Instant)>,
    /// finished steps: group, target, skipped (denied / stopped)
    done: Vec<(String, String, bool)>,
    /// the current model request's activity id, and whether its answer started streaming
    model: Option<(u64, bool)>,
    /// model requests in this turn
    requests: usize,
    /// when the model last answered (None: not yet in this session)
    last_model: Option<Instant>,
}

impl Default for Turn { fn default() -> Self { Turn::new() } }

impl Turn {
    pub fn new() -> Self { Turn { t0: Instant::now(), open: vec![], done: vec![], model: None, requests: 0, last_model: None } }

    /// a new user turn (the cold-model clock carries over)
    pub fn reset(&mut self) {
        let last = self.last_model;
        *self = Turn::new();
        self.last_model = last;
    }

    /// a step begins: its Start event; returns its id
    pub fn begin(&mut self, fe: &mut dyn Frontend, p: &Phrase) -> u64 {
        let id = next_id();
        fe.activity(&Activity::new(id, Phase::Start, p.kind, p.live()));
        self.open.push((id, p.clone(), Instant::now()));
        id
    }

    /// the open step `id` says something else for now (None: its own live line again)
    pub fn say(&mut self, fe: &mut dyn Frontend, id: u64, text: Option<&str>, waiting_ok: bool) {
        let Some((_, p, _)) = self.open.iter().find(|(i, ..)| *i == id) else { return };
        let mut a = Activity::new(id, Phase::Update, p.kind, text.map_or_else(|| p.live(), String::from));
        a.waiting_ok = waiting_ok;
        fe.activity(&a);
    }

    /// an approval opens (true) or closes (false) for the newest open step: "Editing app.py (waiting for your OK)"
    pub fn waiting_ok(&mut self, fe: &mut dyn Frontend, on: bool) {
        let Some((id, p, _)) = self.open.last().cloned() else { return };
        let text = on.then(|| format!("{} (waiting for your OK)", p.act));
        self.say(fe, id, text.as_deref(), on);
    }

    /// a tool step ends: its Done event, with the result summary and the duration
    pub fn end(&mut self, fe: &mut dyn Frontend, id: u64, name: &str, output: &str, is_error: bool) {
        let summary = result_summary(name, output, is_error);
        self.finish(fe, id, output, &summary, is_error);
    }

    /// a step ends with a summary of the caller's own ("12 pages"); `error` = it failed
    pub fn end_with(&mut self, fe: &mut dyn Frontend, id: u64, summary: &str, error: bool) {
        self.finish(fe, id, if error { "failed" } else { "" }, summary, error);
    }

    fn finish(&mut self, fe: &mut dyn Frontend, id: u64, output: &str, summary: &str, is_error: bool) {
        let Some(i) = self.open.iter().position(|(x, ..)| *x == id) else { return };
        let (_, p, t0) = self.open.remove(i);
        let mut a = Activity::new(id, Phase::Done, p.kind, done_text(&p, output, is_error));
        a.summary = summary.to_string();
        a.ms = t0.elapsed().as_millis() as u64;
        // a refused repeat, the person's own no, or a stop is no failure for the person
        let skipped = is_error && (output.starts_with("denied") || output.starts_with("cancelled") || repeat(output));
        (a.error, a.skipped) = (is_error && !skipped, skipped);
        fe.activity(&a);
        self.done.push((p.group, p.target, skipped));
    }

    /// a model request starts: "Waiting for the model…" (the turn's first) or "Thinking about the next step…"; when the
    /// model is local and likely cold (its first request this session, or idle for minutes), it says it is loading
    /// once no token came for `SLOW_MS`
    pub fn model_wait(&mut self, fe: &mut dyn Frontend, local: bool) { self.model_wait_on(fe, local, false); }

    /// `model_wait` for a server that keeps its model loaded (`backend::Caps::always_loaded`: llama.cpp, LM Studio,
    /// vLLM, MLX): a slow first token there is the conversation being read (no prompt cache, or a long new part), not
    /// a model load, and the label says so.
    pub fn model_wait_on(&mut self, fe: &mut dyn Frontend, local: bool, loaded: bool) {
        let id = next_id();
        let text = if self.requests == 0 { "Waiting for the model…" } else { "Thinking about the next step…" };
        let mut a = Activity::new(id, Phase::Start, "model", text.into());
        if local && loaded {
            match self.last_model {
                None => a.slow = Some((SLOW_MS, "Reading the conversation…".into())),
                Some(t) if t.elapsed() > COLD_AFTER => a.slow = Some((SLOW_MS, "Reading the conversation…".into())),
                _ => {}
            }
        } else if local {
            match self.last_model {
                None => a.slow = Some((SLOW_MS, "Loading the model (first message)…".into())),
                Some(t) if t.elapsed() > COLD_AFTER => a.slow = Some((SLOW_MS, "Loading the model…".into())),
                _ => {}
            }
        }
        fe.activity(&a);
        self.requests += 1;
        self.model = Some((id, false));
    }

    /// reply text arrived: the first time, "Writing the answer…"
    pub fn model_text(&mut self, fe: &mut dyn Frontend) {
        if let Some((id, false)) = self.model {
            self.model = Some((id, true));
            fe.activity(&Activity::new(id, Phase::Update, "model", "Writing the answer…".into()));
        }
    }

    /// the model request finished (it answered: the cold clock restarts)
    pub fn model_done(&mut self) {
        self.model = None;
        self.last_model = Some(Instant::now());
    }

    /// the turn ended: its Summary (only when it had steps; a pure chat reply shows nothing extra)
    pub fn close(&mut self, fe: &mut dyn Frontend) {
        let total = self.t0.elapsed().as_millis() as u64;
        let steps = std::mem::take(&mut self.done);
        self.open.clear();
        self.model = None;
        if let Some(text) = summary_line(&steps, total) {
            let mut a = Activity::new(next_id(), Phase::Summary, "summary", text);
            a.ms = total;
            a.steps = steps.len();
            fe.activity(&a);
        }
    }
}

/// A step outside any `Turn` (it is not counted in a reply's summary line).
pub struct Step { id: u64, p: Phrase, t0: Instant }

impl Step {
    pub fn start(fe: &mut dyn Frontend, p: Phrase) -> Step {
        let id = next_id();
        fe.activity(&Activity::new(id, Phase::Start, p.kind, p.live()));
        Step { id, p, t0: Instant::now() }
    }
    pub fn id(&self) -> u64 { self.id }
    pub fn finish(self, fe: &mut dyn Frontend, summary: &str, error: bool) {
        let mut a = Activity::new(self.id, Phase::Done, self.p.kind, done_text(&self.p, if error { "failed" } else { "" }, error));
        a.summary = summary.into();
        a.ms = self.t0.elapsed().as_millis() as u64;
        a.error = error;
        fe.activity(&a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{Approval, Banner, SessionInfo, Status};

    fn d(name: &str, input: Value) -> Phrase { describe(name, &input, Path::new("/nonexistent-elisa-test"), None, None) }

    #[test]
    fn web_steps_read_as_words() {
        let p = d("web_search", json!({"query": "Apple news"}));
        assert_eq!(p.live(), "Searching the web for \u{201c}Apple news\u{201d}…");
        assert_eq!(p.done, "Searched the web for \u{201c}Apple news\u{201d}");
        let f = d("web_fetch", json!({"url": "https://www.reuters.com/tech/apple?x=1"}));
        assert_eq!(f.live(), "Reading reuters.com…");
        assert_eq!(result_summary("web_search", "results for \"x\":\n1. A\n   https://a\n   snip\n2. B\n   https://b\n3. C\n   https://c\n", false), "3 results");
        assert_eq!(result_summary("web_fetch", "title: Apple Newsroom\n\ntext", false), "Apple Newsroom");
    }

    #[test]
    fn docs_and_files_show_only_file_names() {
        let p = d("docs_pdf", json!({"path": "/Users/me/secret/dir/report.pdf"}));
        assert_eq!(p.live(), "Reading report.pdf…");
        assert!(!p.live().contains("/Users"));
        assert_eq!(d("docs_sheet", json!({"path": "data/sales.xlsx"})).live(), "Reading the spreadsheet sales.xlsx…");
        assert_eq!(result_summary("docs_pdf", "<untrusted source=\"docs\">\n[report.pdf: 12 pages · showing 1-12]\ntext", false), "12 pages");
        assert_eq!(d("read", json!({"path": "README.md"})).live(), "Reading README.md…");
        assert_eq!(d("glob", json!({"pattern": "**/*.rs"})).live(), "Looking through your project files…");
        assert_eq!(d("write", json!({"path": "app.py", "content": "x"})).act, "Creating app.py");
        assert_eq!(d("edit", json!({"path": "src/app.py"})).act, "Editing app.py");
        assert_eq!(reading_doc("/tmp/x/report.pdf", Some(12)).live(), "Reading report.pdf (12 pages)…");
    }

    #[test]
    fn commands_reasoning_code_and_plugins() {
        assert_eq!(d("bash", json!({"command": "cargo test --release"})).live(), "Running the tests…");
        assert_eq!(d("bash", json!({"command": "sh scripts/test.sh"})).live(), "Running the tests…");
        assert_eq!(d("bash", json!({"command": "ls /Users/me/private/dir"})).live(), "Running ls dir…");
        assert_eq!(d("think", json!({"thought": "x"})).live(), "Thinking step by step…");
        assert_eq!(d("calc", json!({"expr": "6*7"})).live(), "Calculating…");
        assert_eq!(d("run_code", json!({"language": "python", "code": "print(1)"})).live(), "Running Python code in the sandbox…");
        assert_eq!(result_summary("run_code", "exit 0 · 0.50 s\nstdout:\n1", false), "exit 0");
        let t = describe("time_get_current_time", &json!({}), Path::new("."), Some("time"), Some("get_current_time"));
        assert_eq!(t.live(), "Checking the time…");
        let g = describe("gh_list_issues", &json!({}), Path::new("."), Some("gh"), Some("list_issues"));
        assert_eq!(g.live(), "Using gh tool list issues…");
        assert_eq!(starting("web"), "Starting the web reader…");
    }

    #[test]
    fn arguments_are_redacted_and_short() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH";
        let p = d("bash", json!({"command": format!("curl -H 'x-api-key: {key}' https://api.example.com/v1/things")}));
        assert!(!p.live().contains("abcdefghijklmnop"), "{}", p.live());
        assert!(p.live().chars().count() <= ARG_MAX + 12, "{}", p.live());
        let q = d("web_search", json!({"query": "a ".repeat(60)}));
        assert!(q.live().contains('…'));
    }

    #[test]
    fn errors_and_denials_read_plainly() {
        let p = d("edit", json!({"path": "app.py"}));
        assert_eq!(done_text(&p, "denied by user", true), "Skipped: editing app.py (you said no)");
        assert_eq!(done_text(&p, "boom", true), "Editing app.py didn't work");
        assert_eq!(result_summary("edit", "denied by user", true), "you said no");
        let w = d("web_fetch", json!({"url": "https://bloomberg.com/x"}));
        assert_eq!(done_text(&w, "[elisa: not run: this exact call already ran 2 times", true), "Skipped: reading bloomberg.com again (already done)");
        assert_eq!(result_summary("web_fetch", "<untrusted source=\"web_fetch https://x\">\nhttps://x: HTTP 403\n</untrusted>", true), "https://x: HTTP 403");
        // a result naming a full path shows only the file name
        assert_eq!(result_summary("edit", "edited /private/tmp/some/where/app.py (1 replacement)", false), "edited app.py (1 replacement)");
    }

    #[test]
    fn summary_line_groups_steps() {
        let s = |g: &str, t: &str| (g.to_string(), t.to_string(), false);
        let steps = vec![s("web_search", ""), s("web_fetch", "apple.com"), s("web_fetch", "reuters.com")];
        assert_eq!(summary_line(&steps, 3100).unwrap(), "Searched the web · read 2 pages · 3.1 s");
        assert_eq!(summary_line(&[s("read", "README.md"), s("read", "README.md")], 800).unwrap(), "Read README.md · 0.8 s");
        assert_eq!(summary_line(&[], 500), None);
        assert_eq!(summary_line(&[("edit".into(), "a.py".into(), true)], 500), None);
    }

    #[derive(Default)]
    struct Rec(Vec<Activity>);
    impl Frontend for Rec {
        fn banner(&mut self, _: &Banner) {}
        fn user(&mut self, _: &str) {}
        fn waiting(&mut self) {}
        fn text_delta(&mut self, _: &str) {}
        fn reply_done(&mut self, _: &Status) {}
        fn tool_start(&mut self, _: &str) {}
        fn tool_done(&mut self, _: &str, _: bool) {}
        fn approve(&mut self, _: &str, _: &str, _: Option<&str>) -> Approval { Approval::No }
        fn info(&mut self, _: &str) {}
        fn error(&mut self, _: &str) {}
        fn notes_saved(&mut self, _: &str) {}
        fn compacted(&mut self, _: usize, _: usize) {}
        fn pick_session(&mut self, _: &[SessionInfo]) -> Option<usize> { None }
        fn read_input(&mut self) -> Option<String> { None }
        fn turn_done(&mut self) {}
        fn activity(&mut self, a: &Activity) { self.0.push(a.clone()) }
    }

    #[test]
    fn a_turn_emits_start_update_done_and_summary() {
        let mut fe = Rec::default();
        let mut t = Turn::new();
        t.model_wait(&mut fe, true);
        assert_eq!(fe.0[0].slow.as_ref().unwrap().1, "Loading the model (first message)…");
        assert_eq!(fe.0[0].live_text(0), "Waiting for the model…");
        assert_eq!(fe.0[0].live_text(SLOW_MS), "Loading the model (first message)…");
        t.model_done();
        let id = t.begin(&mut fe, &d("edit", json!({"path": "app.py"})));
        t.waiting_ok(&mut fe, true);
        assert_eq!(fe.0.last().unwrap().text, "Editing app.py (waiting for your OK)");
        assert!(fe.0.last().unwrap().waiting_ok);
        t.waiting_ok(&mut fe, false);
        t.end(&mut fe, id, "edit", "edited app.py (1 replacement)", false);
        assert_eq!(fe.0.last().unwrap().phase, Phase::Done);
        assert_eq!(fe.0.last().unwrap().text, "Edited app.py");
        t.model_wait(&mut fe, true);
        assert_eq!(fe.0.last().unwrap().text, "Thinking about the next step…");
        assert!(fe.0.last().unwrap().slow.is_none(), "warm after the first answer");
        t.model_text(&mut fe);
        t.model_text(&mut fe);
        assert_eq!(fe.0.iter().filter(|a| a.text == "Writing the answer…").count(), 1);
        t.close(&mut fe);
        let s = fe.0.last().unwrap();
        assert_eq!(s.phase, Phase::Summary);
        assert!(s.text.starts_with("Edited app.py · "), "{}", s.text);
        assert_eq!(s.json()["phase"], "summary");
        // a pure chat turn: no summary
        let mut fe2 = Rec::default();
        t.reset();
        t.model_wait(&mut fe2, true);
        t.close(&mut fe2);
        assert!(fe2.0.iter().all(|a| a.phase != Phase::Summary));
    }
}
