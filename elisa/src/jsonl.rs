//! `elisa --serve-jsonl`: a headless frontend that speaks JSON lines on stdin / stdout, for the native app
//! (`app/ElisaApp`). The agent loop is the same `repl::run_session` the TUI and plain mode run (same turn loop, same
//! speed rules: byte-stable system turn, append-only history, no side requests); only the display differs.
//!
//! Protocol v2: UTF-8, one JSON object per line, every object Elisa writes has `"v":2` and `"type"` (`hello.protocol`
//! says the same number). v2 = v1 plus `send.documents` and the `activity` event; every v1 line still means the same, and the app's lines may
//! carry any `v`.
//!
//! Elisa → app (stdout):
//! - `hello` {pid, version, commands: [[name, description], …]} — first line
//! - `banner` {model, base_url, cwd, max_ctx, notes?, resumed?, vision, theme} — `vision`: the model can see pictures
//! - `ready` {} — the agent waits for input (the app may send the next message); `queued` {n} — messages waiting
//! - `user` {text} — a user turn / slash command was accepted
//! - `waiting` {} — a model request started (thinking until the first delta)
//! - `thinking` {text} — the model's reasoning as it streams (not kept in the history; only models that reason)
//! - `delta` {text} — reply text (the model's `<state>…</state>` card is removed, see `card`)
//! - `reply_done` {tok_s, ttft_s, ctx_used, max_ctx, reused, output_tokens, ctx_estimated} — one model reply finished
//!   (`ctx_estimated`: `ctx_used` is Elisa's own count, the server reported none). `tok_s` is
//!   the decode rate, or `null` when the reply had no decode window (a tool-call-only reply, or under 0.2 s of decode:
//!   show "—", never 0.0); then `ttft_s` is the whole request time. `ctx_used` is the context now (prompt + reply); a
//!   cancelled reply (Esc) that the server reported no usage for carries Elisa's estimate instead of 0
//! - `tool_start` {name, summary}; `tool_detail` {name, target, detail?}; `tool_done` {is_error, note, red, preview, more, output}
//!   `detail` = {kind: "edit"|"rewrite"|"new file", path, add, del, lines: [{k: ctx|del|add|gap|note, o?, n?, t}]}
//!   | {kind: "read", path, a, b} | {kind: "cmd", text}
//!   Syntax-tree tools (src/ast.rs, config `ast`): `outline` (detail cmd, note `24 symbols`), `read_symbol` (detail
//!   read with the symbol's lines, note `lines 12–30`), `edit_symbol` (detail edit: the diff of the change; asks like
//!   edit). A write / edit / edit_symbol that would add syntax errors comes back as `tool_done` with is_error and a
//!   note starting `refused:` (the parse guard; nothing was written).
//! - `approve` {tool, summary, outside} — blocking: answer with `approve` below. `outside` = true for a call that
//!   reaches outside the project folder (src/outside.rs), with `access` ("read" | "write" | "bash"), `access_word`
//!   ("read" | "write" | "run (may write)"), `paths` (the resolved outside paths) and `line` (`outside the project ·
//!   write · /a/b`); its answers mean yes = allow once, always = allow these paths for this session (never saved),
//!   no = deny. A plain approval has `outside: false`. `server` (string, optional): present only when the tool
//!   belongs to an MCP server (`{"type":"approve","tool":"fake_greet","summary":"…","outside":false,"server":"fake"}`);
//!   then always = always allow every tool of that server in this project (saved as `mcp:fake` in `.elisa/allow`).
//!   Absent for Elisa's own tools.
//!   `rule` (string, optional): the core offers "always allow this pattern" for this call (`shell(cargo test *)`,
//!   `write(src/**)`, `mcp:browser:navigate(https://github.com/*)`); answer `rule` to allow once and save the pattern to
//!   the user's `~/.config/elisa/config.toml` `[permissions] allow`. A summary line `asked by the rule ask …` means an
//!   ask rule forced the prompt (always / rule then allow once only). `question: true`: not a tool call but a yes / no
//!   question (a plugin's setup, a project's trust prompt): `tool` names it, `summary` is the question; yes / no.
//! - `plugins` {plugins: [{name, version, description, source: "user"|"project", enabled, trusted, servers: [{name,
//!   runs: "container"|"host", installed, tools, tokens, running?, state: "ok"|"failed"|"not installed", error}], tools,
//!   tokens, rules, commands: ["/x"], notes}]} (`error`: why the server did not start — docker's message and a hint,
//!   from this session's start or else the last probe after install / update; null when it runs)
//!   — at startup (when a plugin is installed or the project has one), after every plugin change and on `/plugin list`. `tools` / `tokens`: the tool definitions' count and
//!   estimated prompt cost (a running server's as this session sees it, else as recorded at install; null if unknown)
//! - `plugin_preview` {op: "add"|"update"|"trust", name, text, preview: {name, version, old_version, description,
//!   source, host, servers: [{name, runs, installer, package, version, image, image_present, command, mounts:
//!   [{path, write}], env: [names], tools}], rules: [{level, pattern, broad}], commands: [{name, description, args,
//!   text}] (`text`: the command's full prompt text, shown before the yes), notes,
//!   problems, missing_images: ["node:26-alpine"]}} — the setup screen, sent before anything is installed; a yes / no
//!   `approve` (question) follows unless `problems` is non-empty (no Colima, a missing image: each says what to do).
//!   With `missing_images` (listed images not on this machine) an interactive session first asks whether to download
//!   them (an `approve` question, tool `sandbox pull IMAGE…`), then sends the preview again
//! - `sandbox_images` {images: [{image, lang, version, label, purpose, stable, project, default, present, size_mb,
//!   disk_mb, pinned}], error?} — Elisa's pinned image list (src/images.rs) and what is on this machine: on `/sandbox`, after
//!   every download, and on a `sandbox` list request. `stable`: the language's stable line; `project`: the line this
//!   project's version files pick; `default`: the line a run with no project version uses and plugin installers
//!   take (the newest present, else stable); `size_mb`: approximate download; `disk_mb`: size on disk (null when missing);
//!   `pinned`: its digest is in ~/.elisa/images.lock. `error` (no Colima: says `colima start`) with an empty list
//! - `image_pull` {image, state: "pulling"|"done"|"kept"|"cancelled"|"failed", text, done?, layers?, elapsed_s?} — a
//!   download's progress (layers done of layers seen, not per byte) and its end. A download happens only after a yes
//!   to an `approve` question (tool `sandbox pull IMAGE`, summary `The Go 1.27 sandbox needs golang:1.27-alpine (~72
//!   MB), downloaded into Colima's VM …`): asked by Elisa itself when a `run_code` call or a plugin server is blocked
//!   by a missing listed image (the blocked call then runs once more), or after `/sandbox pull`. A pull whose digest
//!   differs from the pinned one asks a second question before it is used. `interrupt` cancels a download
//! - `plugin_result` {op, name, ok, working, text} — the outcome of add / remove / enable / disable / update / trust;
//!   `working` false with `ok` true: installed / updated, but a server did not start (said in `text`)
//! - `project_trust` {path, hash, text, commands: [{name, description, source}], rules: [{level, pattern, source}],
//!   plugins} — the project's own commands and plugins in full, before its one-time trust question
//! - `pick` {sessions: [{title, n_msgs, ts}]} — blocking: answer with `pick` below
//! - `info` {text}, `error` {text}, `notes` {text}, `compacted` {from, to} (the context shrank — a fold, /compact or
//!   /clear: `to` is the context size now, so the ctx meter shows it right away instead of at the next reply), `progress` {label, frac, elapsed_s, done} | {clear: true},
//!   `recalled` {hits}, `card` {text, n?} (n = the card number: its memory id as /state shows `card #n`, or with
//!   memory off its count in this run; additive v1 field), `watch` {domain, confidence, line?, audit?}, `full` {title, text}
//! - `shade` {state, verb, target, tone, pop, theme, face} — what Shade shows, sent when it changes (the same controller
//!   as the TUI dock; `face` = the status-bar face's state, one of the kit's 12)
//! - `check` {cmd, status, round, rounds, summary, line} — check before done (src/checks.rs): after a turn that edited
//!   files Elisa runs the project's checks and lets the model fix failures. `cmd` = short name (`cargo clippy`),
//!   `status` = "running" | "pass" | "fail" (errors went back to the model: a fix round follows) | "timeout" (killed,
//!   no fix round, the rest skipped) | "stopped" (gave up: rounds used up, same errors again, no edit in the fix
//!   round, or cancelled); `round` = 0 for the first check after the edits, n after fix round n; `rounds` = most fix
//!   rounds; `summary` = `3 errors` / `exit 2` / `timed out after 60s · …` ("" while running); `line` = the
//!   transcript line, e.g. `check: cargo clippy ✗ 3 errors → fixing (1/2)`. Shade: running → building / testing,
//!   fail / timeout → error, stopped → error (no celebration), all pass → the turn ends "build ok" / "tests green".
//! - `activity` {phase, id, kind, text, …} — what Elisa is doing, in plain words for the person (src/activity.rs),
//!   never tool names: `phase` "start" (`text` = the live line, `Searching the web for “Apple news”…`), "update" (the
//!   same `id`, new words; `waiting_ok: true` while an approval for it is open: `Editing app.py (waiting for your OK)`),
//!   "done" (`text` = `Read apple.com` / `Reading apple.com didn't work` / `Skipped: editing app.py (you said no)`,
//!   `summary` = `3 results` | `12 pages` | `exit 0` | "", `ms`, `error`), "summary" (once per turn that had steps,
//!   just before `turn_done`: `text` = `Searched the web · read 2 pages · 3.1 s`, `ms`, `steps`; a pure chat turn has
//!   none). `kind`: model | web | docs | files | edit | run | code | think | time | security | plugin | other | summary.
//!   `background: true`: the check of a file just attached in the composer (`prefetch`), outside any turn: it belongs
//!   to no reply (the file's chip shows the result, `doc_info`).
//!   Model requests are `kind: "model"` starts (`Waiting for the model…`, later `Thinking about the next step…`)
//!   with no done; their updates are `Writing the answer…` (first reply text), `Thinking it over…` (reasoning
//!   streams) and, when the start carried `slow_after_ms` / `slow_text` (a local model that is likely cold: the
//!   session's first request, or idle for minutes) and no token came by then, `Loading the model (first message)…`.
//!   A step's first, lazy MCP server start is an update (`Starting the web reader…`). One line at a time: the newest
//!   start / update is the live line; a display may leave a step under 300 ms off the live line (it still counts in
//!   the summary). Sent next to the `tool_*` events, which stay as they were.
//! - `turn_done` {}, `exit` {code}
//!
//! app → Elisa (stdin):
//! - `send` {text, images?: [{media_type, data}], documents?: [{path}]} — a user message or slash command (queued while
//!   a turn runs; /water /feed /play /poke are care). `images`: base64 PNG / JPEG / GIF / WebP, at most 4, each at most
//!   5 MB of base64; only a model with vision (`banner.vision`) gets them. `documents` (protocol v2): absolute paths of
//!   attached files, at most 8 — the path, never the content. Any model takes them (no vision needed): Elisa copies
//!   each into `<project>/.elisa/uploads/<session>/` (at most 50 MB a file), extracts its text (PDF, docx,
//!   xlsx / xls / ods / csv, text and code files) and puts it in the turn whole when it fits the window's document
//!   budget, else an overview (a vague request) or the pages that answer the question (src/attach.rs). A file that
//!   cannot be attached comes back as an `error` line (`not attached: report.pdf is too large …`) and the message goes
//!   on without it; one that is read gets a `doc_info` (below) with the plan used. `images_refused` (integer, optional):
//!   pictures the app itself refused for this message because the model has no vision (it blocks them at attach time,
//!   so they never come as `images`); Elisa then knows a picture was meant and answers a question about it herself
//!   ("I can't see pictures with gemma3:1b …", `context::no_picture_line`) instead of letting the model make one up
//! - `prefetch` {documents: [{path}]} — files just attached in the composer, before Enter: Elisa runs each one's
//!   intake in the background (`attach::intake`, cached by content): an `activity` step "Checking report.pdf…", then
//!   `doc_info`. Nothing reaches the model or the project; a bad list is an `error` line
//!
//! `doc_info` {path, name, kind, bytes, pages, unit, sheets: [{name, rows, columns}], words, tokens, lang, text_layer,
//!   category, category_word, sections: [{title, pages: [from, to]}], tables, outline, parts: [{title, pages: [from,
//!   to]}], fit: {plan: "whole"|"overview"|"selection", doc_tokens, budget}, warnings: [..], line} — Elisa → app, after
//!   `prefetch` and again when the message with the file is read. `line` is the chip's one-line check result
//!   ("38 pages · research paper · 7 sections · fits as overview", "⚠ has macros (not run; text only) · …"). A file
//!   that cannot be attached: {path, name, refused: true, warnings: [why], line: "⚠ why"}. `parts` are readable parts
//!   for the person: a click sends e.g. "Summarise part 2 (pages 9–14) of report.pdf", answered from those pages
//! `reply_retracted` {} — Elisa → app: the reply streamed in this turn said it could not read the attached document;
//!   it is withdrawn (the app removes that text) and the turn is asked once more with the text, the new reply follows
//! - `approve` {answer: "yes"|"always"|"rule"|"no"} ("once" = yes, "session" = always), `pick` {index: n | null}
//! - `plugin` {op: "list"|"add"|"remove"|"enable"|"disable"|"update"|"trust", path?: "…" (add), name?: "…"} — the same
//!   as sending `/plugin OP ARG` (queued like a message); `trust` with name "project" trusts the project's commands
//! - `sandbox` {op: "list"|"pull", name?: "go" | "golang:1.27-alpine" | "stable" | "project" | "all"} — the same as
//!   sending `/sandbox OP NAME` (a pull asks first)
//! - `interrupt` {} — Esc: cancels the request / tool / approval, drops queued messages
//! - `care` {action: "poke"|"water"|"food"|"toy"}, `typing` {nonempty}, `theme` {theme: "dark"|"light"}, `quit` {}
//!
//! Lines that are not JSON or have an unknown `type` are ignored (an `error` line says so).
use crate::frontend::*;
use crate::repl::{self, CANCEL};
use crate::shade::link::Frame;
use crate::shade::{face_state, Companion, Event as ShEv, Tone};
use serde_json::{json, Value};
use crate::types::{Block, IMAGES_MAX, IMAGE_B64_MAX, IMAGE_TYPES};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const VERSION: u64 = 2;
/// Shade is re-evaluated this often (flash queue, sleep timeout); a `shade` line goes out only on a change
const TICK: Duration = Duration::from_millis(50);
/// tool output kept in a `tool_done` line
const OUT_MAX: usize = 4000;
/// care slash commands, handled here (never reach the agent): the TUI does the same
const CARE: &[(&str, &str)] = &[("/water", "water"), ("/feed", "food"), ("/food", "food"), ("/play", "toy"), ("/toy", "toy"), ("/poke", "poke")];

/// Shade's event for a care action (`CARE`, or the care tray's `{"type":"care"}`)
fn care_event(a: &str) -> ShEv<'static> {
    match a { "water" => ShEv::Water, "food" => ShEv::Food, "toy" => ShEv::Toy, _ => ShEv::Poke }
}

/// what the agent thread and the stdin reader send to the main loop
pub enum Ev {
    Banner(Banner), User(String), Waiting, Delta(String), Done(Status), ToolStart(String),
    Detail { name: String, target: Option<String>, detail: Option<Value> },
    ToolDone(String, bool),
    /// `rule`: the pattern offered for "always allow this pattern"; `question`: a yes / no question (plugin setup,
    /// project trust), not a tool call
    Approve { tool: String, summary: String, outside: Option<Outside>, server: Option<String>, rule: Option<String>, question: bool, tx: Sender<Approval> },
    /// a structured event (`Frontend::event`): its type and fields
    Event(String, Value),
    Info(String), Error(String), Notes(String),
    Compacted(usize, usize), Progress(Option<Progress>), Recalled(Vec<String>), Card(String, Option<u64>), Watch(WatchUpdate),
    WatchView(Option<WatcherView>), Full(String, String), Check(CheckEvent), Pick(Vec<SessionInfo>, Sender<Option<usize>>),
    Input(Sender<Option<String>>), TurnDone, Exit(i32),
    /// a step in plain words (`Frontend::activity`)
    Activity(crate::activity::Activity),
    /// a line from stdin (None = stdin closed)
    Cmd(Option<String>),
}

/// The pictures of the input line being handed to the agent: set just before the line is sent, taken by the agent
/// right after it reads the line (`Frontend::take_images`). One session per process, so one slot.
static ATTACHED: Mutex<Vec<Block>> = Mutex::new(Vec::new());
/// the same for the line's documents (paths; `attach.rs` copies and reads them on the agent's side)
static ATTACHED_DOCS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// the pictures the app refused for that line (`send.images_refused`: the model has no vision)
static REFUSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn attached() -> std::sync::MutexGuard<'static, Vec<Block>> { ATTACHED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
fn attached_docs() -> std::sync::MutexGuard<'static, Vec<String>> { ATTACHED_DOCS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }

/// hands `text` to the waiting agent, its pictures first so they are there when the agent reads the line
#[cfg(test)]
fn hand_over(tx: &Sender<Option<String>>, text: String, imgs: Vec<Block>) { hand_over_all(tx, text, imgs, Vec::new(), 0) }

/// hands `text` to the waiting agent with its pictures and documents
fn hand_over_all(tx: &Sender<Option<String>>, text: String, imgs: Vec<Block>, docs: Vec<String>, refused: usize) {
    *attached() = imgs;
    *attached_docs() = docs;
    REFUSED.store(refused, Ordering::SeqCst);
    let _ = tx.send(Some(text));
}

/// Events from background work (a file's intake after `prefetch`), written out by the main loop on its next tick.
static BG: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());
/// the window and the context used so far (from `banner` / `reply_done`): the intake's fit plan
static WIN: Mutex<(usize, usize)> = Mutex::new((0, 0));

fn bg_push(kind: &str, v: Value) { BG.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((kind.into(), v)); }

/// The intake of a file just attached in the composer (`attach::intake`, cached by content), on its own thread:
/// "Checking report.pdf…" in the activity line, then a `doc_info` event with what was found (or why it cannot be
/// attached). The text is extracted on the way, so the `send` that follows does not wait for it.
fn intake_in_background(path: String) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(900_000_000);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("elisa-intake".into()).spawn(move || {
        use crate::activity::{Activity, Phase};
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let t0 = Instant::now();
        let act = |phase: Phase, text: String, summary: String, error: bool| {
            let mut v = Activity { id, phase, kind: "docs".into(), text, summary, ms: t0.elapsed().as_millis() as u64, error, skipped: false, waiting_ok: false, slow: None, steps: 0 }.json();
            v["background"] = json!(true);
            v
        };
        bg_push("activity", act(Phase::Start, format!("Checking {name}…"), String::new(), false));
        let (max_ctx, used) = *WIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let max_ctx = if max_ctx == 0 { 8192 } else { max_ctx };
        match crate::attach::intake(std::path::Path::new(&path)) {
            Ok(it) => {
                let v = crate::attach::info_json(&path, &it, max_ctx, used);
                let line = v["line"].as_str().unwrap_or("").to_string();
                bg_push("activity", act(Phase::Done, format!("Checked {name}"), line, false));
                bg_push("doc_info", v);
            }
            Err(e) => {
                bg_push("activity", act(Phase::Done, format!("Checked {name}"), "can't be attached".into(), true));
                bg_push("doc_info", crate::attach::refused_json(&path, &e));
            }
        }
    });
}

/// the documents of a `send`: at most `attach::DOCS_MAX` absolute paths with no line breaks (size and type are
/// checked when the agent reads them, with a reason per file)
pub fn check_documents(docs: &[String]) -> Result<(), String> {
    if docs.len() > crate::attach::DOCS_MAX { return Err(format!("{} documents attached: at most {} per message", docs.len(), crate::attach::DOCS_MAX)); }
    for d in docs {
        if d.is_empty() || d.contains(['\n', '\r', '\0']) || !d.starts_with('/') { return Err(format!("document path {d:?}: an absolute file path is needed")); }
    }
    Ok(())
}

/// the pictures of a `send`: known types, valid base64, within the count and size limits
pub fn check_images(imgs: &[Block]) -> Result<(), String> {
    if imgs.len() > IMAGES_MAX { return Err(format!("{} images attached: at most {IMAGES_MAX} per message", imgs.len())); }
    for (i, b) in imgs.iter().enumerate() {
        let Block::Image { source } = b else { continue };
        let n = i + 1;
        if !IMAGE_TYPES.contains(&source.media_type.as_str()) {
            return Err(format!("image {n}: type {:?} is not supported (use PNG, JPEG, GIF or WebP)", source.media_type));
        }
        if source.data.is_empty() { return Err(format!("image {n} is empty")); }
        if source.data.len() > IMAGE_B64_MAX { return Err(format!("image {n} is too large ({:.1} MB): at most {:.0} MB", source.data.len() as f64 / 1e6, IMAGE_B64_MAX as f64 / 1e6)); }
        if !source.data.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=')) { return Err(format!("image {n} is not valid base64")); }
    }
    Ok(())
}

/// The agent side: every Frontend call becomes an `Ev` (blocking calls wait for the main loop's answer).
pub struct JsonlFrontend(pub Sender<Ev>);
impl JsonlFrontend {
    fn send(&self, e: Ev) { let _ = self.0.send(e); }
    fn ask<T>(&self, mk: impl FnOnce(Sender<T>) -> Ev, dflt: T) -> T {
        let (tx, rx) = mpsc::channel();
        self.send(mk(tx));
        rx.recv().unwrap_or(dflt)
    }
}
impl Frontend for JsonlFrontend {
    fn banner(&mut self, b: &Banner) { self.send(Ev::Banner(b.clone())) }
    fn user(&mut self, t: &str) { self.send(Ev::User(t.into())) }
    fn waiting(&mut self) { self.send(Ev::Waiting) }
    fn text_delta(&mut self, s: &str) { self.send(Ev::Delta(s.into())) }
    fn reply_done(&mut self, s: &Status) { self.send(Ev::Done(*s)) }
    fn tool_start(&mut self, s: &str) { self.send(Ev::ToolStart(s.into())) }
    fn tool_detail(&mut self, n: &str, i: &Value, cwd: &std::path::Path) {
        let detail = crate::tui::detail(n, i, cwd).map(|d| detail_json(&d));
        self.send(Ev::Detail { name: n.into(), target: crate::tui::act_target(n, i, cwd), detail });
    }
    fn tool_done(&mut self, o: &str, e: bool) { self.send(Ev::ToolDone(o.into(), e)) }
    fn approve(&mut self, tool: &str, summary: &str, server: Option<&str>) -> Approval {
        self.ask(|tx| Ev::Approve { tool: tool.into(), summary: summary.into(), outside: None, server: server.map(String::from), rule: None, question: false, tx }, Approval::No)
    }
    fn approve_outside(&mut self, tool: &str, summary: &str, o: &Outside) -> Approval {
        self.ask(|tx| Ev::Approve { tool: tool.into(), summary: summary.into(), outside: Some(o.clone()), server: None, rule: None, question: false, tx }, Approval::No)
    }
    fn approve_rule(&mut self, tool: &str, summary: &str, server: Option<&str>, rule: &str) -> Approval {
        self.ask(|tx| Ev::Approve { tool: tool.into(), summary: summary.into(), outside: None, server: server.map(String::from), rule: Some(rule.into()), question: false, tx }, Approval::No)
    }
    fn ask_yes_no(&mut self, what: &str, question: &str) -> bool {
        let a = self.ask(|tx| Ev::Approve { tool: what.into(), summary: question.into(), outside: None, server: None, rule: None, question: true, tx }, Approval::No);
        a != Approval::No
    }
    fn event(&mut self, kind: &str, fields: &Value) { self.send(Ev::Event(kind.into(), fields.clone())) }
    fn info(&mut self, s: &str) { self.send(Ev::Info(s.into())) }
    fn error(&mut self, s: &str) { self.send(Ev::Error(s.into())) }
    fn notes_saved(&mut self, n: &str) { self.send(Ev::Notes(n.into())) }
    fn compacted(&mut self, a: usize, b: usize) { self.send(Ev::Compacted(a, b)) }
    fn progress(&mut self, p: Option<&Progress>) { self.send(Ev::Progress(p.cloned())) }
    fn recalled(&mut self, hits: &[String]) { self.send(Ev::Recalled(hits.to_vec())) }
    fn state_card(&mut self, c: &str) { self.send(Ev::Card(c.into(), None)) }
    fn state_card_n(&mut self, c: &str, n: u64) { self.send(Ev::Card(c.into(), Some(n))) }
    fn watcher_update(&mut self, u: &WatchUpdate) { self.send(Ev::Watch(u.clone())) }
    fn watcher(&mut self, w: Option<&WatcherView>) { self.send(Ev::WatchView(w.cloned())) }
    fn watcher_push(&mut self) -> Option<WatchPush> {
        let tx = self.0.clone();
        Some(Box::new(move |u| { let _ = tx.send(Ev::Watch(u)); }))
    }
    fn full_answer(&mut self, t: &str, x: &str) { self.send(Ev::Full(t.into(), x.into())) }
    fn pick_session(&mut self, l: &[SessionInfo]) -> Option<usize> { self.ask(|t| Ev::Pick(l.to_vec(), t), None) }
    fn read_input(&mut self) -> Option<String> { attached().clear(); attached_docs().clear(); REFUSED.store(0, Ordering::SeqCst); self.ask(Ev::Input, None) }
    fn take_refused_images(&mut self) -> usize { REFUSED.swap(0, Ordering::SeqCst) }
    fn take_images(&mut self) -> Vec<Block> { std::mem::take(&mut *attached()) }
    fn take_documents(&mut self) -> Vec<String> { std::mem::take(&mut *attached_docs()) }
    fn doc_info(&mut self, fields: &Value) { self.send(Ev::Event("doc_info".into(), fields.clone())) }
    fn reply_retracted(&mut self) { self.send(Ev::Event("reply_retracted".into(), json!({}))) }
    fn turn_done(&mut self) { self.send(Ev::TurnDone) }
    fn check(&mut self, c: &CheckEvent) { self.send(Ev::Check(c.clone())) }
    fn activity(&mut self, a: &crate::activity::Activity) { self.send(Ev::Activity(a.clone())) }
}

/// an event line: `{"v":2,"type":…, …fields}`
pub fn event(kind: &str, fields: Value) -> String {
    let mut m = serde_json::Map::new();
    m.insert("v".into(), json!(VERSION));
    m.insert("type".into(), json!(kind));
    if let Value::Object(f) = fields { m.extend(f); }
    format!("{}\n", Value::Object(m))
}

/// the `approve` event's fields: {tool, summary, outside: false} or, for a call outside the project folder,
/// {tool, summary, outside: true, access: "read"|"write"|"bash", access_word, paths: [resolved path, …], line};
/// plus `server` (the MCP server's name) only when the tool belongs to an MCP server
pub fn approve_json(tool: &str, summary: &str, o: Option<&Outside>, server: Option<&str>) -> Value {
    let mut v = match o {
        None => json!({"tool": tool, "summary": summary, "outside": false}),
        Some(o) => json!({"tool": tool, "summary": summary, "outside": true, "access": o.access, "access_word": o.access_word(),
            "paths": o.paths, "line": o.line()}),
    };
    if let Some(s) = server { v["server"] = json!(s); }
    v
}

/// the `check` event's fields
pub fn check_json(c: &CheckEvent) -> Value {
    json!({"cmd": c.cmd, "status": c.status.as_str(), "round": c.round, "rounds": c.rounds, "summary": c.summary, "line": c.line()})
}

/// A command line from the app. None: not a v1 command.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd { Send(String), SendImages(String, Vec<Block>),
    /// a `send` with documents (`documents: [{path}]`), and any pictures with them
    SendDocs(String, Vec<Block>, Vec<String>),
    /// a `send` with `images_refused: n` (the app refused n pictures at attach time: no vision), and any documents
    SendRefused(String, Vec<String>, usize), Approve(Approval), Pick(Option<usize>), Interrupt, Care(&'static str), Typing(bool), Theme(bool), Quit,
    /// `{"type":"plugin","op":…,"name"|"path":…}`: the same as sending the line `/plugin OP ARG`
    Plugin(String, String),
    /// `{"type":"prefetch","documents":[{path}]}`: files just attached, before Enter: their text is extracted into
    /// Elisa's cache in the background (`cache::prefetch_docs`), so the `send` that follows does not wait for it
    Prefetch(Vec<String>),
    /// `{"type":"sandbox","op":"list"|"pull","name":…}`: the same as sending the line `/sandbox OP NAME`
    Sandbox(String, String) }

/// the `plugin` command's operations
const PLUGIN_OPS: [&str; 7] = ["list", "add", "remove", "enable", "disable", "update", "trust"];

pub fn parse_cmd(line: &str) -> Option<Cmd> {
    let j: Value = serde_json::from_str(line.trim()).ok()?;
    Some(match j["type"].as_str()? {
        "send" => {
            let text = j["text"].as_str()?.to_string();
            // checked (type, size, count) where it is used, so a bad picture gets a reason, not "unknown command"
            let imgs: Vec<Block> = j["images"].as_array().map(|a| a.iter().map(|i| Block::image(i["media_type"].as_str().unwrap_or(""), i["data"].as_str().unwrap_or(""))).collect()).unwrap_or_default();
            // documents: [{path}] (a bare string is taken as the path too)
            let docs: Vec<String> = j["documents"].as_array().map(|a| a.iter().map(|d| d["path"].as_str().or(d.as_str()).unwrap_or("").to_string()).collect()).unwrap_or_default();
            let refused = j["images_refused"].as_u64().unwrap_or(0) as usize;
            if refused > 0 && imgs.is_empty() { return Some(Cmd::SendRefused(text, docs, refused)); }
            match (imgs.is_empty(), docs.is_empty()) {
                (true, true) => Cmd::Send(text),
                (false, true) => Cmd::SendImages(text, imgs),
                _ => Cmd::SendDocs(text, imgs, docs),
            }
        }
        "approve" => Cmd::Approve(match j["answer"].as_str()? { "yes" | "once" => Approval::Yes, "always" | "session" => Approval::Always, "rule" => Approval::Rule, "no" => Approval::No, _ => return None }),
        "plugin" => {
            let op = j["op"].as_str()?;
            if !PLUGIN_OPS.contains(&op) { return None; }
            let arg = j["path"].as_str().or(j["name"].as_str()).unwrap_or("");
            if arg.contains(['\n', '\r']) { return None; }
            Cmd::Plugin(op.to_string(), arg.to_string())
        }
        "sandbox" => {
            let op = j["op"].as_str().unwrap_or("list");
            if op != "list" && op != "pull" { return None; }
            let arg = j["name"].as_str().unwrap_or("");
            if arg.contains(['\n', '\r']) { return None; }
            Cmd::Sandbox(op.to_string(), arg.to_string())
        }
        "prefetch" => Cmd::Prefetch(j["documents"].as_array()?.iter().map(|d| d["path"].as_str().or(d.as_str()).unwrap_or("").to_string()).collect()),
        "pick" => Cmd::Pick(j["index"].as_u64().map(|i| i as usize)),
        "interrupt" => Cmd::Interrupt,
        "care" => Cmd::Care(match j["action"].as_str()? { "poke" => "poke", "water" => "water", "food" => "food", "toy" => "toy", _ => return None }),
        "typing" => Cmd::Typing(j["nonempty"].as_bool().unwrap_or(false)),
        "theme" => Cmd::Theme(j["theme"] == "light"),
        "quit" => Cmd::Quit,
        _ => return None,
    })
}

/// the command line for `c` (the app's side of the protocol; used by tests and as documentation)
pub fn encode_cmd(c: &Cmd) -> String {
    let (kind, f) = match c {
        Cmd::Send(t) => ("send", json!({"text": t})),
        Cmd::SendImages(t, imgs) => ("send", json!({"text": t, "images": imgs.iter().filter_map(|b| match b {
            Block::Image { source } => Some(json!({"media_type": source.media_type, "data": source.data})), _ => None }).collect::<Vec<_>>()})),
        Cmd::SendDocs(t, imgs, docs) => ("send", {
            let mut f = json!({"text": t, "documents": docs.iter().map(|p| json!({"path": p})).collect::<Vec<_>>()});
            if !imgs.is_empty() { f["images"] = json!(imgs.iter().filter_map(|b| match b {
                Block::Image { source } => Some(json!({"media_type": source.media_type, "data": source.data})), _ => None }).collect::<Vec<_>>()); }
            f
        }),
        Cmd::SendRefused(t, docs, n) => ("send", {
            let mut f = json!({"text": t, "images_refused": n});
            if !docs.is_empty() { f["documents"] = json!(docs.iter().map(|p| json!({"path": p})).collect::<Vec<_>>()); }
            f
        }),
        Cmd::Approve(a) => ("approve", json!({"answer": match a { Approval::Yes => "yes", Approval::Always => "always", Approval::Rule => "rule", Approval::No => "no" }})),
        Cmd::Plugin(op, arg) => ("plugin", if op == "add" { json!({"op": op, "path": arg}) } else if arg.is_empty() { json!({"op": op}) } else { json!({"op": op, "name": arg}) }),
        Cmd::Sandbox(op, arg) => ("sandbox", if arg.is_empty() { json!({"op": op}) } else { json!({"op": op, "name": arg}) }),
        Cmd::Prefetch(docs) => ("prefetch", json!({"documents": docs.iter().map(|p| json!({"path": p})).collect::<Vec<_>>()})),
        Cmd::Pick(i) => ("pick", json!({"index": i})),
        Cmd::Interrupt => ("interrupt", json!({})),
        Cmd::Care(a) => ("care", json!({"action": a})),
        Cmd::Typing(b) => ("typing", json!({"nonempty": b})),
        Cmd::Theme(l) => ("theme", json!({"theme": if *l { "light" } else { "dark" }})),
        Cmd::Quit => ("quit", json!({})),
    };
    event(kind, f)
}

/// the edit-panel model (the TUI's own, built before the tool runs) as JSON; old/new line numbers both filled
pub(crate) fn detail_json(d: &crate::tui::Detail) -> Value {
    use crate::tui::{Detail, PL};
    match d {
        Detail::View(v) => {
            // within each run of changed lines, deletions first (the kit's diff reads old, then new)
            let mut body: Vec<&PL> = Vec::with_capacity(v.body.len());
            let mut run: Vec<&PL> = vec![];
            for p in v.body.iter().chain(std::iter::once(&PL::Gap)) {
                if matches!(p, PL::Add(..) | PL::Del(..)) { run.push(p); continue; }
                run.sort_by_key(|p| matches!(p, PL::Add(..)));
                body.append(&mut run);
                body.push(p);
            }
            body.pop();
            let (mut off, mut add, mut del) = (0i64, 0, 0);
            let lines: Vec<Value> = body.into_iter().map(|p| match p {
                PL::Ctx(n, t) => json!({"k": "ctx", "o": (*n as i64 - off).max(1), "n": n, "t": t}),
                PL::Del(n, t) => { off -= 1; del += 1; json!({"k": "del", "o": n, "t": t}) }
                PL::Add(n, t) => { off += 1; add += 1; json!({"k": "add", "n": n, "t": t}) }
                PL::Gap => json!({"k": "gap", "t": "…"}),
                PL::Note(t) => json!({"k": "note", "t": t}),
            }).collect();
            json!({"kind": v.kind, "path": v.path, "add": add, "del": del, "lines": lines})
        }
        Detail::Read(r) => json!({"kind": "read", "path": r.path, "a": r.a, "b": r.b}),
        Detail::Cmd(c) => json!({"kind": "cmd", "text": c}),
    }
}

/// the short note under a finished tool row (`118 lines`, `exit 0 · test result: ok`), red or not, and up to 3
/// preview lines of a command's output plus how many more there were
pub fn tool_note(name: &str, o: &str, err: bool) -> (String, bool, Vec<String>, usize) {
    let first = || first_note(o).unwrap_or_else(|| (if err { "failed" } else { "(no output)" }).to_string());
    if err && o.starts_with("denied") { return ("denied".into(), true, vec![], 0); }
    if err && o.starts_with("cancelled") { return ("interrupted".into(), true, vec![], 0); }
    let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else if s.ends_with('h') { "es" } else { "s" });
    if name == "bash" {
        let mut ls: Vec<&str> = o.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
        let code = ls.last().and_then(|l| l.strip_prefix("[exit ")).and_then(|r| r.strip_suffix(']')).map(str::to_string);
        if code.is_some() { ls.pop(); }
        let bad = err || code.as_deref().is_some_and(|c| c != "0");
        let mut parts: Vec<String> = if bad { ls.last() } else { ls.first() }.map(|l| l.trim().to_string()).into_iter().collect();
        if let Some(c) = code { parts.push(format!("exit {c}")); }
        let prev: Vec<String> = if ls.len() > 1 { ls.iter().rev().take(3).rev().map(|l| l.to_string()).collect() } else { vec![] };
        return (parts.join(" · "), bad, prev, ls.len().saturating_sub(3));
    }
    if err { return (first(), true, vec![], 0); }
    let last = o.lines().last().unwrap_or("");
    let note = match name {
        "read" => match last.strip_prefix("[lines ") {
            Some(r) => format!("lines {}", r.split(']').next().unwrap_or("").replacen('-', "–", 1)),
            None if o.starts_with('[') => "empty file".into(),
            None => plural(o.lines().count(), "line"),
        },
        "outline" => crate::tui::outline_note(o),
        "read_symbol" => crate::tui::symbol_note(o),
        "glob" if o == "no files found" => "0 files".into(),
        "glob" => plural(o.lines().filter(|l| !l.starts_with('[')).count(), "file"),
        "grep" if o == "no matches" => "0 matches".into(),
        // at the cap the count is a floor, not the total
        "grep" if o.lines().any(|l| l.starts_with("[stopped")) => format!("{}+ matches (capped)", o.lines().filter(|l| !l.starts_with("[stopped")).count()),
        "grep" => plural(o.lines().filter(|l| !l.starts_with("[stopped")).count(), "match"),
        _ => first(),
    };
    (note, false, vec![], 0)
}

/// The one-line note for a result with no tool-specific note (MCP tools and the rest): for JSON (an MCP server's
/// pretty-printed answer, whose first line is only "{") its first scalar fields (`timezone: Asia/Tokyo
/// · datetime: 2026-11-08T19:46:00+09:00`) or its item count; else the first line with a letter or digit.
pub fn first_note(o: &str) -> Option<String> {
    let t = o.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if let Some(n) = json_note(&v) { return Some(n); }
        }
    }
    let l = o.lines().map(str::trim).find(|l| l.chars().any(char::is_alphanumeric))?;
    Some(if l.chars().count() > 160 { format!("{}…", l.chars().take(160).collect::<String>()) } else { l.to_string() })
}

fn json_note(v: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let scalar = |v: &Value| -> Option<String> {
        let s = match v { Value::String(s) => s.trim().replace(['\n', '\r', '\t'], " "), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), _ => return None };
        if s.is_empty() { return None; }
        Some(if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s })
    };
    match v {
        Value::Object(m) => {
            let parts: Vec<String> = m.iter().filter_map(|(k, v)| scalar(v).map(|s| format!("{k}: {s}"))).take(2).collect();
            if !parts.is_empty() { return Some(parts.join(" · ")); }
            // only nested values: the first object or list inside
            m.values().find_map(json_note).or_else(|| Some(format!("{} field{}", m.len(), if m.len() == 1 { "" } else { "s" })))
        }
        Value::Array(a) if a.is_empty() => Some("0 items".into()),
        Value::Array(a) => Some(format!("{} item{}{}", a.len(), if a.len() == 1 { "" } else { "s" },
            json_note(&a[0]).or_else(|| scalar(&a[0])).map(|n| format!(" · {n}")).unwrap_or_default())),
        _ => scalar(v),
    }
}

fn cap(s: &str, n: usize) -> String {
    if s.len() <= n { return s.to_string(); }
    let mut i = n;
    while !s.is_char_boundary(i) { i -= 1; }
    // a paging hint near the end (docs: `[output cut at the size limit; next: pages="2-11"]`, then the untrusted
    // fence's closing tag) stays visible
    let hint = s[i..].trim_end().rsplit('\n').take(3).find(|l| l.contains("next:") && l.len() < 300)
        .map_or(String::new(), |l| format!("\n{}", l.trim()));
    format!("{}\n[… {} more chars]{hint}", &s[..i], s.len() - i)
}

/// The main loop: owns stdout, Shade and the pending blocking calls.
struct Server<W: Write> {
    out: W,
    t0: Instant,
    shade: Companion,
    light: bool,
    shown: Option<(Frame, &'static str)>,
    input: Option<Sender<Option<String>>>,
    /// messages sent while a turn runs, each with the pictures that came with it
    queue: VecDeque<(String, Vec<Block>, Vec<String>, usize)>,
    approve: Option<Sender<Approval>>,
    pick: Option<Sender<Option<usize>>>,
    /// state-card stripping across deltas (as the TUI, `context::CardFilter`)
    cardf: crate::context::CardFilter,
    quitting: bool,
    /// the running tool's name (from its tool_start)
    last_tool: String,
    /// the model request on the live line: its start, when it came, and whether it got a slow / reasoning update
    model_act: Option<(crate::activity::Activity, Instant, bool, bool)>,
    /// reply text after its last space, held until the word ends: a secret split across deltas is masked whole
    held: crate::frontend::ShownText,
}

impl<W: Write> Server<W> {
    fn now(&self) -> u64 { self.t0.elapsed().as_millis() as u64 }
    /// One event out. Every string in it is masked for the screen (`secrets::mask_shown`: a key read from a file or
    /// written in a reply shows as "sk-l…"); reply text goes through `ShownText`, its unfinished last word held
    /// until the word ends or another event goes out, so a key streamed in pieces is masked whole.
    fn emit(&mut self, kind: &str, mut f: Value) {
        if kind == "delta" {
            let Some(t) = f["text"].as_str() else { return self.write_ev(kind, f) };
            let shown = self.held.push(t);
            if shown.is_empty() { return; }
            f["text"] = Value::String(shown);
        } else if !matches!(kind, "activity" | "shade" | "progress" | "watch" | "thinking" | "queued") {
            // (the live line, the character and the meters are not in the reply's text: the held word stays held)
            let h = self.held.finish();
            if !h.is_empty() { self.write_ev("delta", json!({"text": h})); }
        }
        self.write_ev(kind, f);
    }
    fn write_ev(&mut self, kind: &str, mut f: Value) {
        crate::secrets::mask_shown_json(&mut f);
        let _ = self.out.write_all(event(kind, f).as_bytes());
        let _ = self.out.flush();
    }
    fn sh(&mut self, e: ShEv) { let now = self.now(); self.shade.on(now, e); }

    /// the model request on the live line, said again in other words: its slow text once no token came in time,
    /// "Thinking it over…" while the model reasons
    fn model_tick(&mut self, reasoning: bool) {
        use crate::activity::{Activity, Phase};
        let Some((a, t0, slowed, thought)) = &mut self.model_act else { return };
        let ms = t0.elapsed().as_millis() as u64;
        let text = if reasoning && !*thought { *thought = true; Some("Thinking it over…".to_string()) }
            else if !*slowed && !*thought && a.slow.as_ref().is_some_and(|(after, _)| ms >= *after) { *slowed = true; a.slow.as_ref().map(|(_, t)| t.clone()) }
            else { None };
        if let Some(text) = text {
            let u = Activity { phase: Phase::Update, text, slow: None, ..a.clone() };
            self.emit("activity", u.json());
        }
    }

    /// a `shade` line when what Shade shows changed
    fn shade_frame(&mut self) {
        let now = self.now();
        // the gateway's phase for the running model call (queue, prefill, reasoning, a tool call being written)
        self.shade.gateway(now, crate::api::phase(), crate::api::call_target);
        let v = self.shade.view(now);
        let f = Frame::of(&v, self.light);
        let face = face_state(f.state).name();
        if self.shown.as_ref().is_some_and(|(s, _)| *s == f) { return; }
        let tone = match f.tone { Tone::Accent => "accent", Tone::Warn => "warn", Tone::Error => "error" };
        self.emit("shade", json!({"state": f.state.name(), "verb": f.verb, "target": f.target, "tone": tone, "pop": f.pop,
            "theme": if f.light { "light" } else { "dark" }, "face": face, "fidget": f.fidget}));
        self.shown = Some((f, face));
    }

    /// streamed text minus any state-card text; a partial tag or card line at the end is held until it resolves
    fn unstate(&mut self, s: &str) -> String { self.cardf.push(s) }

    fn submit(&mut self, text: String, imgs: Vec<Block>) { self.submit_all(text, imgs, Vec::new(), 0) }

    fn submit_all(&mut self, text: String, imgs: Vec<Block>, docs: Vec<String>, refused: usize) {
        let t = text.trim();
        if imgs.is_empty() && docs.is_empty() && refused == 0 { if let Some((c, a)) = CARE.iter().find(|(c, _)| *c == t) { return self.care_typed(c, a); } }
        match self.input.take() {
            Some(tx) => { hand_over_all(&tx, text, imgs, docs, refused); }
            None => { self.queue.push_back((text, imgs, docs, refused)); let n = self.queue.len(); self.emit("queued", json!({"n": n})); }
        }
    }

    fn care(&mut self, a: &str) { self.sh(care_event(a)); }

    /// a care command typed in the chat: the same as the care button, then shown like any other command (its echo, a
    /// line with Shade's bubble) and ended with "ready" while the session waits for input, so the frontend is not left
    /// waiting for a turn that never starts. During a running turn the turn's own "ready" ends it.
    fn care_typed(&mut self, cmd: &str, a: &str) {
        self.care(a);
        if self.input.is_none() { return; }
        let st = crate::shade::state_for(care_event(a));
        let (what, _) = crate::shade::care_of(st);
        self.emit("user", json!({"text": cmd}));
        self.emit("info", json!({"text": format!("{} · {}", crate::shade::verb(st), what)}));
        self.emit("ready", json!({}));
    }

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Send(t) => self.submit(t, Vec::new()),
            Cmd::SendImages(t, imgs) => match check_images(&imgs) {
                Ok(()) => self.submit(t, imgs),
                Err(e) => self.emit("error", json!({"text": e})),
            },
            Cmd::SendDocs(t, imgs, docs) => match check_images(&imgs).and_then(|_| check_documents(&docs)) {
                Ok(()) => self.submit_all(t, imgs, docs, 0),
                Err(e) => self.emit("error", json!({"text": e})),
            },
            Cmd::SendRefused(t, docs, n) => match check_documents(&docs) {
                Ok(()) => self.submit_all(t, Vec::new(), docs, n),
                Err(e) => self.emit("error", json!({"text": e})),
            },
            Cmd::Approve(a) => if let Some(tx) = self.approve.take() {
                let now = self.now();
                self.shade.approved(now, a != Approval::No);
                let _ = tx.send(a);
            },
            Cmd::Prefetch(docs) => match check_documents(&docs) {
                Ok(()) => for d in docs { if d.starts_with('/') { intake_in_background(d); } },
                Err(e) => self.emit("error", json!({"text": e})),
            },
            Cmd::Pick(i) => if let Some(tx) = self.pick.take() { let _ = tx.send(i); },
            Cmd::Interrupt => {
                let busy = self.input.is_none();
                // a message still waiting (the agent was busy finishing the last turn) is not sent: said so, as a
                // cancelled reply, or the app waits for an answer that never comes
                if !self.queue.is_empty() {
                    let n = self.queue.len();
                    self.queue.clear();
                    self.emit("queued", json!({"n": 0}));
                    self.emit("info", json!({"text": format!("Stopped. {} waiting message{} not sent.", n, if n == 1 { " was" } else { "s were" })}));
                }
                if let Some(tx) = self.pick.take() { let _ = tx.send(None); }
                if busy {
                    CANCEL.store(true, Ordering::SeqCst);
                    if let Some(tx) = self.approve.take() { let _ = tx.send(Approval::No); }
                    self.sh(ShEv::Esc);
                }
            }
            Cmd::Care(a) => self.care(a),
            Cmd::Typing(b) => if self.input.is_some() { let now = self.now(); self.shade.typing(now, b); },
            Cmd::Theme(l) => self.light = l,
            Cmd::Quit => self.quit(),
            Cmd::Plugin(op, arg) => self.submit(format!("/plugin {op} {arg}").trim_end().to_string(), Vec::new()),
            Cmd::Sandbox(op, arg) => self.submit(format!("/sandbox {op} {arg}").trim_end().to_string(), Vec::new()),
        }
    }

    fn quit(&mut self) {
        self.quitting = true;
        self.queue.clear();
        CANCEL.store(true, Ordering::SeqCst);
        if let Some(tx) = self.input.take() { let _ = tx.send(None); }
        if let Some(tx) = self.approve.take() { let _ = tx.send(Approval::No); }
        if let Some(tx) = self.pick.take() { let _ = tx.send(None); }
    }

    /// handles one event; Some(code) when the agent exited
    fn on_ev(&mut self, e: Ev) -> Option<i32> {
        match e {
            Ev::Banner(b) => {
                WIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0 = b.max_ctx;
                self.emit("banner", json!({"model": b.model, "base_url": b.base_url, "cwd": b.cwd, "max_ctx": b.max_ctx,
                    "notes": b.notes, "resumed": b.resumed, "vision": b.vision, "theme": if self.light { "light" } else { "dark" }}));
                self.sh(ShEv::Launch);
            }
            Ev::User(t) => self.emit("user", json!({"text": t})),
            Ev::Waiting => {
                self.cardf = Default::default();
                self.sh(ShEv::Waiting);
                self.emit("waiting", json!({}));
            }
            Ev::Delta(s) => {
                let s = self.unstate(&s);
                if !s.is_empty() { self.sh(ShEv::Delta); self.emit("delta", json!({"text": s})); }
            }
            Ev::Done(s) => {
                // a held `<sta…` that never became a tag (or a lone `next:` line) is ordinary text
                let h = self.cardf.finish();
                if !h.is_empty() { self.emit("delta", json!({"text": h})); }
                *WIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (s.max_ctx, s.ctx_used);
                self.emit("reply_done", json!({"tok_s": s.tok_s, "ttft_s": s.ttft_s, "ctx_used": s.ctx_used, "max_ctx": s.max_ctx,
                    "reused": s.reused, "output_tokens": s.output_tokens, "ctx_estimated": s.estimated}));
            }
            Ev::ToolStart(s) => {
                let (n, t) = s.split_once(' ').unwrap_or((&s, ""));
                self.sh(ShEv::Tool(n, t));
                self.emit("tool_start", json!({"name": n, "summary": s}));
            }
            Ev::Detail { name, target, detail } => self.emit("tool_detail", json!({"name": name, "target": target, "detail": detail})),
            Ev::ToolDone(o, e) => {
                // the name is the last tool_start's; the note needs it only for bash / read / glob / grep
                let name = self.last_tool.clone();
                let (note, red, preview, more) = tool_note(&name, &o, e);
                let failed = red && note != "denied" && note != "interrupted";
                let now = self.now();
                self.shade.tool_done(now, failed, &o);
                self.emit("tool_done", json!({"is_error": e, "note": note, "red": red, "preview": preview, "more": more, "output": cap(&o, OUT_MAX)}));
            }
            Ev::Approve { tool: t, summary: s, outside: o, server, rule, question, tx } => {
                if self.quitting { let _ = tx.send(Approval::No); return None; }
                self.sh(ShEv::Permission);
                self.shade_frame();
                self.approve = Some(tx);
                let mut v = approve_json(&t, &s, o.as_ref(), server.as_deref());
                if let Some(r) = rule { v["rule"] = json!(r); }
                if question { v["question"] = json!(true); }
                self.emit("approve", v);
            }
            Ev::Event(kind, fields) => self.emit(&kind, fields),
            Ev::Info(s) => self.emit("info", json!({"text": s})),
            Ev::Error(s) => {
                self.sh(if s.starts_with("unknown command") { ShEv::UnknownCommand } else { ShEv::Failed });
                self.emit("error", json!({"text": s}));
            }
            Ev::Notes(n) => self.emit("notes", json!({"text": n})),
            Ev::Compacted(a, b) => self.emit("compacted", json!({"from": a, "to": b})),
            Ev::Progress(p) => match p {
                Some(p) => {
                    if p.label.starts_with("summarizing") && p.done.is_none() { self.sh(ShEv::Summarizing); } else { self.shade.summary_done(); }
                    self.emit("progress", json!({"label": p.label, "frac": p.frac, "elapsed_s": p.elapsed_s, "done": p.done}));
                }
                None => { self.shade.summary_done(); self.emit("progress", json!({"clear": true})); }
            },
            Ev::Recalled(h) => {
                if !h.is_empty() { let now = self.now(); self.shade.recalled(now, h.len()); }
                self.emit("recalled", json!({"hits": h}));
            }
            Ev::Card(c, n) => self.emit("card", match n { Some(n) => json!({"text": c, "n": n}), None => json!({"text": c}) }),
            Ev::Watch(u) => {
                let line = match (&u.view, u.changed && u.was.is_some()) { (Some(v), true) => Some(crate::watcher::change_line(v, u.was.as_deref())), _ => None };
                let audit = u.audits.iter().rev().find_map(crate::watcher::audit_line);
                match &u.view {
                    Some(v) => self.emit("watch", json!({"domain": v.domain, "confidence": v.confidence, "line": line, "audit": audit})),
                    None => self.emit("watch", json!({"domain": null, "audit": audit})),
                }
            }
            Ev::WatchView(w) => match w {
                Some(v) => self.emit("info", json!({"text": format!("watcher: {} {:.2} · since turn {} · would unload {:.1} GB",
                    if v.domain.is_empty() { "(no domain yet)" } else { &v.domain }, v.confidence, v.since_turn, v.plan.gb_unload)})),
                None => self.emit("info", json!({"text": "watcher: not available on this gateway"})),
            },
            Ev::Full(t, x) => self.emit("full", json!({"title": t, "text": x})),
            Ev::Check(c) => {
                match c.status {
                    CheckStatus::Running => self.sh(ShEv::Tool("check", &format!("check {}", c.cmd))),
                    CheckStatus::Pass => self.sh(ShEv::ToolDone(false)),
                    CheckStatus::Fail | CheckStatus::Timeout => self.sh(ShEv::ToolDone(true)),
                    CheckStatus::Stopped => self.sh(ShEv::Failed),
                }
                self.emit("check", check_json(&c));
            }
            Ev::Pick(l, tx) => {
                if self.quitting || l.is_empty() { let _ = tx.send(None); return None; }
                let s: Vec<Value> = l.iter().map(|s| json!({"title": s.title, "n_msgs": s.n_msgs, "ts": s.ts})).collect();
                self.pick = Some(tx);
                self.emit("pick", json!({"sessions": s}));
            }
            Ev::Input(tx) => {
                if self.quitting { let _ = tx.send(None); return None; }
                self.shade.idle();
                match self.queue.pop_front() {
                    Some((q, imgs, docs, refused)) => { let n = self.queue.len(); self.emit("queued", json!({"n": n})); hand_over_all(&tx, q, imgs, docs, refused); }
                    None => { self.input = Some(tx); self.emit("ready", json!({})); }
                }
            }
            Ev::TurnDone => { self.model_act = None; self.sh(ShEv::Finished(false)); self.emit("turn_done", json!({})); }
            Ev::Activity(a) => {
                use crate::activity::Phase;
                match a.phase {
                    Phase::Start if a.kind == "model" => self.model_act = Some((a.clone(), Instant::now(), false, false)),
                    // the answer streams, or a step took the line: the model request is no longer waiting
                    Phase::Update if a.kind == "model" => self.model_act = None,
                    Phase::Start => self.model_act = None,
                    _ => {}
                }
                self.emit("activity", a.json());
            }
            Ev::Exit(c) => return Some(c),
            Ev::Cmd(None) => self.quit(),
            Ev::Cmd(Some(l)) => {
                if l.trim().is_empty() { return None; }
                match parse_cmd(&l) {
                    Some(c) => self.on_cmd(c),
                    None => self.emit("error", json!({"text": format!("jsonl: ignored line: {}", crate::context::one_line(&l, 120))})),
                }
            }
        }
        None
    }
}

/// `elisa --serve-jsonl [--cwd DIR] …`.
pub fn run(args: Args) -> i32 {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // stdout is the protocol: nothing else may print there
    let _ = out.write_all(event("hello", json!({"pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"),
        "protocol": VERSION, "commands": COMMANDS.iter().map(|(c, d)| json!([c, d])).collect::<Vec<_>>()})).as_bytes());
    let _ = out.flush();
    // the theme and sleep timeout come from the same config the session loads
    let dir = args.cwd.clone().or_else(|| std::env::current_dir().ok()).unwrap_or_default();
    let cfg = crate::config::Config::load(&dir).unwrap_or_default();
    let light = args.theme.as_deref().unwrap_or(&cfg.theme) == "light";
    let (tx, rx) = mpsc::channel::<Ev>();
    let txin = tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line { Ok(l) => { if txin.send(Ev::Cmd(Some(l))).is_err() { return; } } Err(_) => break }
        }
        let _ = txin.send(Ev::Cmd(None));
    });
    let txa = tx.clone();
    std::thread::spawn(move || {
        let mut fe = JsonlFrontend(txa.clone());
        // a panic outside a turn (startup, exit) still ends the session with an error and `exit`, never a silent
        // hang (each line's own work is guarded in the repl, which keeps the session going)
        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| repl::run_session(args, &mut fe))).unwrap_or_else(|p| {
            let m = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            let _ = txa.send(Ev::Error(format!("internal error: {m}; the session ended")));
            101
        });
        let _ = txa.send(Ev::Exit(code));
    });
    drop(tx);
    let sleep = (cfg.sleep_after_s > 0).then_some(cfg.sleep_after_s * 1000);
    let mut s = Server { out, t0: Instant::now(), shade: Companion::new(0, sleep), light, shown: None, input: None, queue: VecDeque::new(),
        approve: None, pick: None, cardf: Default::default(), quitting: false, last_tool: String::new(), model_act: None, held: Default::default() };
    let mut quit_at: Option<Instant> = None;
    let code = loop {
        let r = rx.recv_timeout(TICK);
        // reasoning streamed so far goes out before the event (it comes before the answer's first delta)
        let t = crate::api::take_thinking();
        if !t.is_empty() { s.emit("thinking", json!({"text": t})); }
        let bg = std::mem::take(&mut *BG.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for (k, v) in bg { s.emit(&k, v); }
        s.model_tick(!t.is_empty());
        match r {
            Ok(e) => {
                if let Ev::ToolStart(t) = &e { s.last_tool = t.split(' ').next().unwrap_or("").to_string(); }
                if let Some(c) = s.on_ev(e) { break c; }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break 0,
        }
        if s.quitting && quit_at.is_none() { quit_at = Some(Instant::now()); }
        // a turn that does not stop within 3 s of quit (a tool that ignores the cancel) is left behind
        if quit_at.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) { break 0; }
        s.shade_frame();
    };
    s.emit("exit", json!({"code": code}));
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_carries_server_only_for_mcp_tools() {
        assert_eq!(approve_json("fake_greet", "fake_greet {}", None, Some("fake")),
            json!({"tool": "fake_greet", "summary": "fake_greet {}", "outside": false, "server": "fake"}));
        assert!(approve_json("bash", "bash ls", None, None).get("server").is_none());
    }

    #[test]
    fn a_send_with_images_round_trips_and_is_checked() {
        let c = Cmd::SendImages("what is this?".into(), vec![Block::image("image/png", "iVBORw0KGgo=")]);
        assert_eq!(parse_cmd(&encode_cmd(&c)), Some(c));
        // an empty list is a plain send
        assert_eq!(parse_cmd(r#"{"type":"send","text":"hi","images":[]}"#), Some(Cmd::Send("hi".into())));
        assert!(check_images(&[Block::image("image/jpeg", "AAAA")]).is_ok());
        let bad = |b: Vec<Block>| check_images(&b).unwrap_err();
        assert!(bad(vec![Block::image("image/bmp", "AAAA")]).contains("not supported"));
        assert!(bad(vec![Block::image("image/png", "")]).contains("empty"));
        assert!(bad(vec![Block::image("image/png", "not base64!")]).contains("base64"));
        assert!(bad(vec![Block::image("image/png", &"A".repeat(IMAGE_B64_MAX + 1))]).contains("too large"));
        assert!(bad(vec![Block::image("image/png", "AAAA"); IMAGES_MAX + 1]).contains("at most"));
    }

    /// The app refuses a picture at attach time (no vision) and says so in the `send`; the agent learns it
    #[test]
    fn a_send_says_how_many_pictures_the_app_refused() {
        let c = Cmd::SendRefused("what colour are its ears?".into(), Vec::new(), 1);
        assert_eq!(parse_cmd(&encode_cmd(&c)), Some(c));
        assert_eq!(parse_cmd(r#"{"type":"send","text":"x","images_refused":2,"documents":[{"path":"/a.pdf"}]}"#), Some(Cmd::SendRefused("x".into(), vec!["/a.pdf".into()], 2)));
        assert_eq!(parse_cmd(r#"{"type":"send","text":"hi","images_refused":0}"#), Some(Cmd::Send("hi".into())));
        let (tx, rx) = mpsc::channel();
        hand_over_all(&tx, "look".into(), Vec::new(), Vec::new(), 1);
        assert_eq!(rx.recv().unwrap().as_deref(), Some("look"));
        let (etx, _erx) = mpsc::channel();
        let mut fe = JsonlFrontend(etx);
        assert_eq!(fe.take_refused_images(), 1);
        assert_eq!(fe.take_refused_images(), 0, "taken once");
    }

    #[test]
    fn the_pictures_reach_the_agent_with_their_line_even_when_queued() {
        let (tx, rx) = mpsc::channel();
        hand_over(&tx, "look".into(), vec![Block::image("image/png", "AAAA")]);
        assert_eq!(rx.recv().unwrap().as_deref(), Some("look"));
        let (etx, _erx) = mpsc::channel();
        let mut fe = JsonlFrontend(etx);
        assert_eq!(fe.take_images().len(), 1);
        assert!(fe.take_images().is_empty(), "taken once");
    }

    #[test]
    fn commands_round_trip_and_bad_lines_are_ignored() {
        let all = [Cmd::Send("fix it \"now\"\nplease — ü".into()), Cmd::Approve(Approval::Yes), Cmd::Approve(Approval::Always),
            Cmd::Approve(Approval::No), Cmd::Pick(Some(2)), Cmd::Pick(None), Cmd::Interrupt, Cmd::Care("water"), Cmd::Care("poke"),
            Cmd::Typing(true), Cmd::Theme(true), Cmd::Theme(false), Cmd::Quit, Cmd::Approve(Approval::Rule),
            Cmd::Plugin("add".into(), "/Users/me/my plugin".into()), Cmd::Plugin("list".into(), String::new()), Cmd::Plugin("trust".into(), "project".into()),
            Cmd::Sandbox("list".into(), String::new()), Cmd::Sandbox("pull".into(), "golang:1.27-alpine".into())];
        for c in all {
            let l = encode_cmd(&c);
            assert!(l.ends_with('\n') && l.matches('\n').count() == 1, "one line: {l}");
            assert_eq!(parse_cmd(&l), Some(c), "{l}");
        }
        for bad in ["", "nope", "{}", r#"{"type":"send"}"#, r#"{"type":"approve","answer":"maybe"}"#, r#"{"type":"care","action":"pizza"}"#, r#"{"type":"plan_choice","answer":"yes"}"#, r#"{"type":"dance"}"#,
            r#"{"type":"sandbox","op":"rm","name":"x"}"#,
            r#"{"type":"plugin","op":"install","name":"x"}"#, r#"{"type":"plugin","op":"add","path":"a
b"}"#] {
            assert_eq!(parse_cmd(bad), None, "{bad}");
        }
    }

    #[test]
    fn events_are_one_versioned_line() {
        let l = event("delta", json!({"text": "a\nb \"c\""}));
        assert!(l.ends_with('\n') && l.matches('\n').count() == 1);
        let j: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(j, json!({"v": VERSION, "type": "delta", "text": "a\nb \"c\""}));
    }

    #[test]
    fn edit_detail_has_old_and_new_line_numbers() {
        let d = crate::testdir::TempDir::new("jsonl");
        std::fs::write(d.join("a.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let det = crate::tui::detail("edit", &json!({"path": "a.rs", "old": "two\nthree", "new": "2\n2b\n3"}), &d).unwrap();
        let j = detail_json(&det);
        assert_eq!((j["kind"].as_str(), j["path"].as_str(), j["add"].as_u64(), j["del"].as_u64()), (Some("edit"), Some("a.rs"), Some(3), Some(2)));
        let ls = j["lines"].as_array().unwrap();
        let k: Vec<&str> = ls.iter().map(|l| l["k"].as_str().unwrap()).collect();
        assert_eq!(k, ["ctx", "del", "del", "add", "add", "add", "ctx", "ctx"]);
        // the context after the change: old 4 / new 5
        assert_eq!((ls[6]["o"].as_u64(), ls[6]["n"].as_u64(), ls[6]["t"].as_str()), (Some(4), Some(5), Some("four")));
        assert_eq!((ls[1]["o"].as_u64(), ls[1]["n"].as_u64()), (Some(2), None));
    }

    #[test]
    fn tool_notes() {
        assert_eq!(tool_note("bash", "running 3 tests\ntest result: ok\n[exit 0]", false), ("running 3 tests · exit 0".into(), false, vec!["running 3 tests".into(), "test result: ok".into()], 0));
        let (n, red, _, _) = tool_note("bash", "a\nb\nerror: boom\n[exit 101]", false);
        assert_eq!((n.as_str(), red), ("error: boom · exit 101", true));
        assert_eq!(tool_note("edit", "denied by user", true).0, "denied");
        assert_eq!(tool_note("bash", "cancelled by user", true).0, "interrupted");
        assert_eq!(tool_note("read", "1\ta\n2\tb\n[lines 1-2 of 9]", false).0, "lines 1–2 of 9");
        assert_eq!(tool_note("grep", "a.rs:1: x\nb.rs:2: y", false).0, "2 matches");
        assert_eq!(tool_note("grep", "a.rs:1: x\nb.rs:2: y\n[stopped at 2 matches: there are more]", false).0, "2+ matches (capped)");
    }

    #[test]
    fn mcp_json_results_get_a_real_note() {
        // the time server's pretty JSON, whose first line is only "{"
        let time = "{\n  \"timezone\": \"Asia/Tokyo\",\n  \"datetime\": \"2026-11-08T19:46:00+09:00\",\n  \"is_dst\": false\n}";
        assert_eq!(tool_note("time_get_current_time", time, false).0, "timezone: Asia/Tokyo · datetime: 2026-11-08T19:46:00+09:00");
        assert_eq!(tool_note("x", "{\n  \"result\": {\n    \"count\": 3\n  }\n}", false).0, "count: 3");
        assert_eq!(tool_note("x", "[\n  {\"name\": \"a\"},\n  {\"name\": \"b\"}\n]", false).0, "2 items · name: a");
        assert_eq!(tool_note("x", "[]", false).0, "0 items");
        // not JSON (or cut): the first line with a letter or digit
        assert_eq!(tool_note("x", "{\n  \"cut\": \"abc", false).0, "\"cut\": \"abc");
        assert_eq!(tool_note("x", "---\n\n  Page Title: Example Domain\n", false).0, "Page Title: Example Domain");
        assert_eq!(tool_note("x", "{\n}", false).0, "0 fields");
        assert_eq!(tool_note("x", "", false).0, "(no output)");
        assert_eq!(tool_note("x", "{", true).0, "failed");
    }

    #[test]
    fn cap_keeps_a_paging_hint() {
        let long = format!("{}\n[output cut at the size limit; next: pages=\"2-11\"]\n</untrusted>\n", "x".repeat(50));
        assert_eq!(cap(&long, 10), format!("{}\n[… {} more chars]\n[output cut at the size limit; next: pages=\"2-11\"]", "x".repeat(10), long.len() - 10));
        assert_eq!(cap("abcdefgh", 4), "abcd\n[… 4 more chars]");
    }

    #[test]
    fn state_cards_are_stripped_across_deltas() {
        let mut s = Server { out: Vec::new(), t0: Instant::now(), shade: Companion::new(0, None), light: false, shown: None, input: None,
            queue: VecDeque::new(), approve: None, pick: None, cardf: Default::default(), quitting: false, last_tool: String::new(), model_act: None, held: Default::default() };
        let got: String = ["Done. <st", "ate>goal: x", "</sta", "te> bye"].iter().map(|d| s.unstate(d)).collect();
        assert_eq!(got, "Done.  bye");
    }

    #[test]
    fn a_typed_care_command_ends_with_ready() {
        let mut s = Server { out: Vec::new(), t0: Instant::now(), shade: Companion::new(0, None), light: false, shown: None, input: None,
            queue: VecDeque::new(), approve: None, pick: None, cardf: Default::default(), quitting: false, last_tool: String::new(), model_act: None, held: Default::default() };
        // during a turn: Shade only (the turn's own "ready" ends it), nothing queued for the agent
        s.submit("/water".into(), Vec::new());
        assert!(s.out.is_empty() && s.queue.is_empty());
        // waiting for input: echoed, one line, "ready", and the agent still waits
        let (tx, _rx) = std::sync::mpsc::channel();
        s.input = Some(tx);
        s.submit("/feed".into(), Vec::new());
        let out = String::from_utf8(s.out.clone()).unwrap();
        let kinds: Vec<String> = out.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["type"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["user", "info", "ready"]);
        assert!(out.contains("NOM NOM"));
        assert!(s.input.is_some() && s.queue.is_empty());
    }

    #[test]
    fn a_copied_card_instruction_is_dropped_across_deltas() {
        let mut s = Server { out: Vec::new(), t0: Instant::now(), shade: Companion::new(0, None), light: false, shown: None, input: None,
            queue: VecDeque::new(), approve: None, pick: None, cardf: Default::default(), quitting: false, last_tool: String::new(), model_act: None, held: Default::default() };
        let got: String = ["Blue. <=80-tok", "en card of what the next ", "step needs:\n<state>goal: x</state>", " a<b"].iter().map(|d| s.unstate(d)).collect();
        // "<b" starts neither a tag nor the line, so it is not held
        assert_eq!(got, "Blue. \n a<b");
        // a partial line at the end is held until it resolves
        assert_eq!(s.unstate("x <=80-tok"), "x ");
        assert_eq!(s.unstate("ens"), "<=80-tokens");
    }
}
