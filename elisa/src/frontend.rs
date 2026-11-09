//! The seam between the agent loop (repl.rs) and a display (ui::Plain for -p / pipes, tui::run for a terminal).
//! Owned by the coordinator; agents code against it and do not change it.
use std::path::PathBuf;
pub use crate::watcher::{Push as WatchPush, WatchUpdate, WatcherView};

/// Parsed command line. Built by `repl::parse_args`, consumed by `repl::run_session` / `tui::run`.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub prompt: Option<String>,
    pub yes: bool,
    pub cont: bool,
    /// --continue-open: `--continue` on the conversation that was open last, the newest saved session whatever was
    /// said in it (the app's restart on a model switch, BUGS #63); plain `--continue` skips a dead session
    pub cont_open: bool,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// --provider NAME: a connection of `~/.config/elisa/providers.toml` (`none` = ignore that file)
    pub provider: Option<String>,
    /// --type streamdense|anthropic|openai: with --base-url, the kind of server there (default: the active
    /// connection's kind, else config.toml's)
    pub kind: Option<String>,
    pub max_ctx: Option<usize>,
    pub cwd: Option<PathBuf>,
    /// --no-character: no companion in the TUI
    pub no_character: bool,
    /// --theme dark|light (overrides config `theme`)
    pub theme: Option<String>,
    /// --companion: mirror Shade to the companion window (app/ElisaCompanion) over ~/.elisa/companion.sock; the
    /// terminal dock hides while the window is connected (also config `companion = true`)
    pub companion: bool,
    /// --serve-jsonl: no terminal UI; JSON lines on stdin / stdout for the native app (src/jsonl.rs, app/ElisaApp).
    pub serve_jsonl: bool,
    /// --no-checks: no automatic check-before-done after edits (also config `checks = false`, `.elisa/checks.toml`)
    pub no_checks: bool,
    /// --attach FILE (repeatable, with -p): documents for the prompt, read like the app's attachments (src/attach.rs);
    /// absolute paths (made so against the directory Elisa started in)
    pub attach: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Banner {
    pub model: String,
    pub base_url: String,
    pub cwd: String,
    pub max_ctx: usize,
    /// this session's notes (`.elisa/notes/<id>.md`, full text; only a continued session has any), shown as a preview at startup
    pub notes: Option<String>,
    /// messages restored by --continue / /resume
    pub resumed: Option<usize>,
    /// the model can see pictures: the app enables its image button
    pub vision: bool,
}

/// One earlier turn of a resumed conversation, for the display (`context::earlier_turns`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Earlier {
    /// "user" or "assistant"
    pub role: String,
    pub text: String,
}

/// Shown after every model reply and kept in the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    /// decode rate; None when the reply had no decode window (a tool-call-only reply, or under 0.2 s of decode):
    /// shown as "—", never as a false 0.0
    pub tok_s: Option<f64>,
    pub ttft_s: f64,
    pub ctx_used: usize,
    pub max_ctx: usize,
    pub reused: usize,
    pub output_tokens: usize,
    /// `ctx_used` is Elisa's own estimate: the server reported no prompt count, or one that means something else
    /// (`backend::ctx_from`). Shown with a `~`.
    pub estimated: bool,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub path: PathBuf,
    /// unix seconds (from the file name)
    pub ts: u64,
    pub n_msgs: usize,
    /// first user text, one line, <= 80 chars
    pub title: String,
}

/// Progress of a memory job (summary + save to the graph store).
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// e.g. "summarizing turns 21–40"
    pub label: String,
    /// 0.0..=1.0 when known (e.g. output tokens / expected), None = indeterminate
    pub frac: Option<f32>,
    pub elapsed_s: f64,
    /// set when finished: e.g. "saved summary · 182 words · memory 1,240 turns"
    pub done: Option<String>,
}

/// Where one automatic check (src/checks.rs) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// the command started
    Running,
    Pass,
    /// it failed: the errors go back to the model for a fix round (`round < rounds`), else see `Stopped`
    Fail,
    /// it ran past its timeout and was killed: reported, no fix round, the remaining checks are skipped
    Timeout,
    /// the check loop gave up: rounds used up, the same errors again, or the model ended without editing
    Stopped,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self { CheckStatus::Running => "running", CheckStatus::Pass => "pass", CheckStatus::Fail => "fail",
            CheckStatus::Timeout => "timeout", CheckStatus::Stopped => "stopped" }
    }
}

/// One check-before-done step, e.g. `check: cargo clippy ✗ 3 errors → fixing (1/2)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckEvent {
    /// the command as run (or its configured name)
    pub cmd: String,
    pub status: CheckStatus,
    /// 0 = the first check after the edits, n = the check after fix round n
    pub round: usize,
    /// the most fix rounds (config `check_rounds`, default 2)
    pub rounds: usize,
    /// short: `3 errors`, `timed out after 60s`, `same errors again`, the first error line; empty while running
    pub summary: String,
}

impl CheckEvent {
    /// the transcript line every display shows
    pub fn line(&self) -> String {
        let s = if self.summary.is_empty() { String::new() } else { format!(" {}", self.summary) };
        match self.status {
            CheckStatus::Running => format!("check: {} …", self.cmd),
            CheckStatus::Pass => format!("check: {} ✓{s}", self.cmd),
            CheckStatus::Fail if self.round < self.rounds => format!("check: {} ✗{s} → fixing ({}/{})", self.cmd, self.round + 1, self.rounds),
            CheckStatus::Fail => format!("check: {} ✗{s}", self.cmd),
            CheckStatus::Timeout => format!("check: {} ⏱{s}", self.cmd),
            CheckStatus::Stopped => format!("check: {} ✗{s} → stopped", self.cmd),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// `Rule`: yes, and always allow the pattern offered with `approve_rule` (saved to the user's config)
pub enum Approval { Yes, No, Always, Rule }

/// A tool call that reaches outside the project folder (src/outside.rs). Asked with `approve_outside`, where
/// `Approval::Yes` = allow once, `Approval::Always` = allow these paths for this session (never saved), `No` = deny.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outside {
    /// "read" | "write" | "bash" (a command: treated as a write)
    pub access: String,
    /// the resolved outside paths (absolute; a bash word that cannot be resolved, e.g. `$TMPDIR/x`, as written)
    pub paths: Vec<String>,
}

impl Outside {
    pub fn write(&self) -> bool { self.access != "read" }
    /// what the prompt says the call does there
    pub fn access_word(&self) -> &'static str {
        match self.access.as_str() { "read" => "read", "write" => "write", _ => "run (may write)" }
    }
    /// one line for a prompt: `outside the project · write · /a/b.txt`
    pub fn line(&self) -> String { format!("outside the project · {} · {}", self.access_word(), self.paths.join(", ")) }
}

/// Slash commands, in menu order: (name, one-line description).
pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "list the commands"),
    ("/clear", "save a summary to notes, then start fresh"),
    ("/reset", "start fresh without a summary (notes kept, state card dropped)"),
    ("/compact", "summarise now and continue from the notes"),
    ("/context", "token use: system / tools / history / limit"),
    ("/notes", "show the saved notes"),
    ("/allow", "list always-allowed tools; /allow clear resets them"),
    ("/mcp", "MCP servers: state, tools, token cost; /mcp NAME for one server's tools"),
    ("/trust", "accept an MCP server's changed tool list or a changed plugin: /trust NAME (no name: list them)"),
    ("/plugin", "plugins: /plugin list · add PATH · remove · enable · disable · update NAME · trust NAME|project"),
    ("/sandbox", "sandbox images: /sandbox list · pull NAME|IMAGE|stable|project|all (asks first; into Colima's VM)"),
    ("/audit", "the last tool calls and approvals in the audit log, and its chain check: /audit [N]"),
    ("/forget", "forget what Elisa remembers in this project: delete the notes, unpin the pinned facts, stop recalling earlier chats (saved conversations are kept)"),
    ("/resume", "pick a past session to continue"),
    ("/state", "the state cards per topic (document) and which one this turn uses"),
    ("/pin", "pin a fact to memory (carried in [state]): /pin <fact>"),
    ("/watcher", "domain watcher: domain, candidates, unload plan, audit scorecard"),
    ("/full", "the full model's answer to the last reply its audit found changed"),
    ("/copy", "copy the last reply to the clipboard"),
    ("/stats", "metrics: the last request's model calls and the session totals"),
    ("/cache", "Elisa's result cache: entries, size, hit rate per kind · /cache clear [web_fetch|web_search|docs]"),
    ("/model", "the model connection in use, and the saved ones"),
    ("/yolo", "toggle approve-all for tools"),
    ("/water", "give Elisa water (F2)"),
    ("/feed", "give Elisa a snack (F3)"),
    ("/play", "give Elisa a toy (F4)"),
    ("/poke", "poke Elisa (or click her, ctrl-g)"),
    ("/exit", "quit"),
];

/// Everything the agent loop shows or asks. All methods are called from the agent-loop thread.
pub trait Frontend {
    fn banner(&mut self, b: &Banner);
    /// a user turn was accepted (echo it in the transcript)
    fn user(&mut self, text: &str);
    /// a model request started (show a spinner / "thinking…" until the first delta)
    fn waiting(&mut self);
    fn text_delta(&mut self, s: &str);
    fn reply_done(&mut self, st: &Status);
    fn tool_start(&mut self, summary: &str);
    /// the full tool call, sent right after tool_start (before approval) so a display can show the file being
    /// edited; `cwd` resolves relative paths
    fn tool_detail(&mut self, name: &str, input: &serde_json::Value, cwd: &std::path::Path) { let _ = (name, input, cwd); }
    fn tool_done(&mut self, output: &str, is_error: bool);
    /// blocking: ask to run a Write/Exec tool. `server`: the MCP server the tool belongs to (None for Elisa's own
    /// tools); "always" then covers that whole server (approval key `mcp:SERVER`)
    fn approve(&mut self, tool: &str, summary: &str, server: Option<&str>) -> Approval;
    /// blocking: ask to run a tool call that reaches outside the project (`o`: read / write and the exact paths).
    /// Yes = allow once, Always = allow these paths for this session, No = deny. Default: `approve` with the outside
    /// line added to the summary.
    fn approve_outside(&mut self, tool: &str, summary: &str, o: &Outside) -> Approval {
        self.approve(tool, &format!("{summary}\n{}", o.line()), None)
    }
    /// blocking: `approve`, with a fourth choice — yes, and always allow `rule` (a permission pattern the core
    /// picked, shown as is; `Approval::Rule` saves it to the user's config). Default: `approve` without that choice.
    fn approve_rule(&mut self, tool: &str, summary: &str, server: Option<&str>, rule: &str) -> Approval {
        let _ = rule;
        self.approve(tool, summary, server)
    }
    /// blocking: a yes / no question about `what` (a plugin's setup, a project's commands), after the full detail was
    /// shown with `info`. Default: through `approve` (Yes or Always = yes).
    fn ask_yes_no(&mut self, what: &str, question: &str) -> bool {
        matches!(self.approve(what, question, None), Approval::Yes | Approval::Always | Approval::Rule)
    }
    /// a structured event for frontends that carry them (`--serve-jsonl`: `{"type": kind, …fields}`); others ignore it
    fn event(&mut self, kind: &str, fields: &serde_json::Value) { let _ = (kind, fields); }
    /// A resumed conversation's earlier turns (`--continue`, `/resume`, "continue the last one"), right after the
    /// banner / the resume line: the app's `history` event shows them as normal blocks.
    fn history(&mut self, items: &[Earlier]) { if !items.is_empty() { self.event("history", &serde_json::json!({ "items": items })); } }
    fn info(&mut self, s: &str);
    fn error(&mut self, s: &str);
    /// a summary was written to notes (/clear, /compact, auto-compact): show it in full
    fn notes_saved(&mut self, notes: &str);
    fn compacted(&mut self, from_tokens: usize, to_tokens: usize);
    /// background memory work shown under the input (None = clear it), e.g. the every-N-turns summary
    fn progress(&mut self, p: Option<&Progress>) { let _ = p; }
    /// memories recalled from the graph store for the current user turn (one short line each)
    fn recalled(&mut self, hits: &[String]) { let _ = hits; }
    /// the model's state card for this reply (text between <state> and </state>, tags removed); displays must
    /// hide the raw <state>…</state> block from streamed text themselves
    fn state_card(&mut self, card: &str) { let _ = card; }
    /// `state_card` with the card's number (its memory id, or its count in this run with memory off); default: `state_card`
    fn state_card_n(&mut self, card: &str, n: u64) { let _ = n; self.state_card(card) }
    /// a new domain-watcher view (`u.view` None = this gateway has no watcher: hide it; `u.changed` = new domain)
    fn watcher_update(&mut self, u: &WatchUpdate) { let _ = u; }
    /// /watcher: the full view (None = not available on this gateway)
    fn watcher(&mut self, w: Option<&WatcherView>) { let _ = w; }
    /// a sink for watcher updates from a background thread (a display with its own event loop); None = updates come
    /// through `watcher_update` on the agent-loop thread at turn boundaries
    fn watcher_push(&mut self) -> Option<WatchPush> { None }
    /// /full: the full model's answer to a reply (markdown), shown like a reply in a box
    fn full_answer(&mut self, title: &str, text: &str) { let _ = (title, text); }
    /// blocking: choose one of the sessions (newest first); None = cancelled
    fn pick_session(&mut self, list: &[SessionInfo]) -> Option<usize>;
    /// blocking: next user line (text or slash command); None = quit
    fn read_input(&mut self) -> Option<String>;
    /// pictures that came with the line `read_input` just returned (only the app can attach them)
    fn take_images(&mut self) -> Vec<crate::types::Block> { Vec::new() }
    /// pictures the display itself refused for that line because the model has no vision (the app's
    /// `send.images_refused`): the agent answers a question about them honestly (`context::no_picture_line`)
    fn take_refused_images(&mut self) -> usize { 0 }
    /// paths of the documents that came with that line (the app's `send` {documents}; `attach.rs` reads them)
    fn take_documents(&mut self) -> Vec<String> { Vec::new() }
    /// an attached file's intake result (`attach::info_json`): the app shows it on the document's chip; a terminal
    /// prints its one-line check result
    fn doc_info(&mut self, fields: &serde_json::Value) {
        if let Some(l) = fields["line"].as_str() { self.info(&format!("{}: {l}", fields["name"].as_str().unwrap_or("document"))); }
    }
    /// the reply just streamed is withdrawn (it said it could not read an attached document; the turn is asked once
    /// more and the new reply follows): the app removes the streamed text; a terminal says so in one line
    fn reply_retracted(&mut self) { self.info("(that reply is replaced by the one below)") }
    /// a check-before-done step after the model edited files (src/checks.rs); default: one info line
    fn check(&mut self, c: &CheckEvent) { self.info(&c.line()) }
    /// what Elisa is doing, in plain words (src/activity.rs): a step's start / update / done, and the reply's one
    /// summary line. Default: ignored (plain mode prints its own tool lines)
    fn activity(&mut self, a: &crate::activity::Activity) { let _ = a; }
    /// the current user turn is finished (agent idle again)
    fn turn_done(&mut self);
}
