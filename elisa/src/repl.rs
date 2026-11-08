//! CLI, agent loop, slash commands, approvals. Contract in DESIGN.md. All display and input goes through `Frontend`.
use crate::api::{Client, Request};
use crate::config::Config;
use crate::context::{self, estimate_messages, estimate_tokens, Store};
use crate::checks;
use crate::frontend::{Approval, Args, Banner, CheckEvent, CheckStatus, Frontend, Outside, Progress, COMMANDS};
use crate::outside;
use crate::perms::Level;
use crate::commands::FileCommand;
use crate::plugins;
use crate::images;
use serde_json::json;
use crate::metrics;
use crate::tools::{self, Access, ToolCtx};
use crate::types::{Block, Message, Reply, Role, ToolDef};
use crate::ui;
use crate::watcher::Watcher;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use elisa_mem::{HitKind, Memory, Role as MRole};

pub static CANCEL: AtomicBool = AtomicBool::new(false);
pub static AT_PROMPT: AtomicBool = AtomicBool::new(false);
pub static ASKING: AtomicBool = AtomicBool::new(false);

const USAGE: &str = "usage: elisa [-p PROMPT] [--yes] [--continue] [--provider NAME] [--base-url U [--type T]] [--model M] [--max-ctx N] [--cwd DIR] [--no-character] [--companion] [--serve-jsonl] [--no-checks] [--theme dark|light] [--demo-states [STATE]] [--version] [-h]\n       elisa eval [--suite NAME|all] [--case ID] [--model M] [--json FILE] [--keep]\n       elisa plugin list | add PATH [--yes] | remove NAME | enable NAME | disable NAME | update NAME [--yes] | trust NAME [--yes]\n       elisa sandbox list [--json] | pull NAME|IMAGE|stable|project|all [--yes] [--json]";
const COMPACT_SYSTEM: &str = "You write short, concrete session notes for a coding agent.";
const COMPACT_OUT: usize = 800;
const TOOL_PREAMBLE: usize = 240;
/// expected length of a fold summary in chars (progress = chars so far / this, clamped to 0.95)
const FOLD_EXPECT_CHARS: f32 = 1200.0;
/// a step slower than this shows a progress line (next time, or live for elisa-mem's own progress)
const SLOW: Duration = Duration::from_millis(150);
/// before exit (plain display): wait this long for watcher notes and the last view still in flight
const WATCH_EXIT_WAIT: Duration = Duration::from_secs(1);
/// at exit, index pending memories only if that should take less than this; the rest stays on disk for next time
const EXIT_INDEX_BUDGET_S: f64 = 3.0;
/// at exit with no measured speed yet, index only this many memories
const EXIT_INDEX_UNKNOWN: usize = 8;
/// measured embedding speed (memories/s), kept in memory_dir across sessions
const RATE_FILE: &str = "elisa-index-rate";
/// compact the memory store after a fold once its hot log is bigger than this
const COMPACT_HOT_BYTES: u64 = 4 << 20;
/// token cap on the pinned facts carried in the `[state]` block of a first message (newest kept first)
const PINNED_CAP: usize = 150;
/// smallest max_tokens a request is sent with (a near-full window still gets room to answer)
const MIN_OUT: usize = 256;
/// messages the relevance gate looks back over (`recent_text`)
const RECENT_MSGS: usize = 6;
/// system + tools above this share (percent) of the usable context gets a startup warning
const FIXED_WARN_PCT: usize = 30;
/// share of the limit a big step's results leave free for the turn's later steps (small results still append whole
/// there, so a full window does not force a rewrite one step later)
const STEP_TAIL: usize = 8;
const WARM_UNKNOWN: u8 = 0;
const WARM_YES: u8 = 1;
const WARM_NO: u8 = 2;
/// the user message that carries a step's tool results (role + framing)
const MSG_OVERHEAD_TOKENS: usize = 8;
/// Where elisa-mem's progress callback goes while an elisa-mem call runs on this (the agent) thread.
struct Sink { fe: *mut (dyn Frontend + 'static), t0: Instant, last: Option<Instant>, shown: bool }
thread_local! { static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) }; }

/// elisa-mem's progress callback (registered once): forwards to the frontend of the call in progress, <= 10/s.
fn mem_progress(label: &str, frac: Option<f32>) {
    SINK.with(|c| {
        let mut c = c.borrow_mut();
        let Some(s) = c.as_mut() else { return };
        if s.last.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) && frac.is_none_or(|f| f < 1.0) { return; }
        s.last = Some(Instant::now());
        s.shown = true;
        let p = Progress { label: label.to_string(), frac, elapsed_s: s.t0.elapsed().as_secs_f64(), done: None };
        // SAFETY: the pointer is set only for the duration of `Session::mem_call`, which does not touch `fe` itself
        unsafe { (*s.fe).progress(Some(&p)) }
    });
}

/// Runs `f` with elisa-mem's progress callback routed to `fe`; returns (result, whether a progress line was shown).
/// elisa-mem never sends a "done", so a shown line is cleared here (without this the TUI kept `indexing N memories 100%`
/// with a live timer after indexing finished).
fn with_progress<R>(fe: &mut dyn Frontend, f: impl FnOnce() -> R) -> (R, bool) {
    let p: *mut (dyn Frontend + '_) = &mut *fe;
    // SAFETY: lifetime erased only while `f` runs (the sink is taken below); `fe` outlives this call
    let p: *mut (dyn Frontend + 'static) = unsafe { std::mem::transmute(p) };
    SINK.with(|c| *c.borrow_mut() = Some(Sink { fe: p, t0: Instant::now(), last: None, shown: false }));
    let r = f();
    let shown = SINK.with(|c| c.borrow_mut().take().is_some_and(|s| s.shown));
    if shown { fe.progress(None); }
    (r, shown)
}

fn help_text() -> String {
    let mut s: String = COMMANDS.iter().map(|(c, d)| format!("{c:<10}{d}\n")).collect();
    s.push_str("A line ending in \\ continues on the next line.");
    s
}

extern "C" fn on_sigint(_: libc::c_int) {
    let w = |s: &str| unsafe { libc::write(2, s.as_ptr().cast(), s.len()); };
    if ASKING.load(Ordering::SeqCst) {
        CANCEL.store(true, Ordering::SeqCst);
        w("^C (press Enter)");
    } else if AT_PROMPT.load(Ordering::SeqCst) {
        CANCEL.store(true, Ordering::SeqCst);
        w("\n› ");
    } else if CANCEL.swap(true, Ordering::SeqCst) {
        w("\n");
        unsafe { libc::_exit(130) };
    } else {
        w("\n[cancelling… Ctrl-C again to quit]\n");
    }
}

fn install_sigint() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

/// Err = exit code (help / version print and give Err(0); a bad argument prints usage and gives Err(2)).
pub fn parse_args() -> Result<Args, i32> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let r: Result<(), String> = match arg.as_str() {
            "-p" | "--prompt" => val("-p").map(|v| a.prompt = Some(v)),
            "--yes" | "-y" => { a.yes = true; Ok(()) }
            "--continue" | "-c" => { a.cont = true; Ok(()) }
            "--base-url" => val("--base-url").map(|v| a.base_url = Some(v)),
            "--model" => val("--model").map(|v| a.model = Some(v)),
            "--provider" => val("--provider").map(|v| a.provider = Some(v)),
            "--type" => val("--type").and_then(|v| if ["streamdense", "anthropic", "openai"].contains(&v.as_str()) { a.kind = Some(v); Ok(()) } else { Err(format!("bad --type: {v} (streamdense | anthropic | openai)")) }),
            "--max-ctx" => val("--max-ctx").and_then(|v| v.parse().map(|n| a.max_ctx = Some(n)).map_err(|_| format!("bad --max-ctx: {v}"))),
            "--cwd" => val("--cwd").map(|v| a.cwd = Some(PathBuf::from(v))),
            "--no-character" => { a.no_character = true; Ok(()) }
            "--companion" => { a.companion = true; Ok(()) }
            "--serve-jsonl" => { a.serve_jsonl = true; Ok(()) }
            "--no-checks" => { a.no_checks = true; Ok(()) }
            "--theme" => val("--theme").and_then(|v| if v == "dark" || v == "light" { a.theme = Some(v); Ok(()) } else { Err(format!("bad --theme: {v} (dark | light)")) }),
            "--demo-states" => {
                let st = it.next_if(|v| !v.starts_with('-')).unwrap_or_default();
                if !st.is_empty() && crate::shade::St::parse(&st).is_none() { Err(format!("bad --demo-states: {st} (one of the 20 states, e.g. thinking)")) }
                else { a.demo = Some(st); Ok(()) }
            }
            "--version" | "-V" => { println!("elisa {}", env!("CARGO_PKG_VERSION")); return Err(0); }
            "-h" | "--help" => { println!("{USAGE}\n\n{}", help_text()); return Err(0); }
            _ => Err(format!("unknown argument: {arg}")),
        };
        if let Err(e) = r { ui::error(&e); eprintln!("{USAGE}"); return Err(2); }
    }
    Ok(a)
}

/// `-p`, or stdin/stdout not a terminal: plain ANSI output. Otherwise the full-screen TUI.
pub fn main_entry() -> i32 {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = crate::provider::cli(&raw) { return code; }
    if raw.first().map(String::as_str) == Some("eval") { return crate::eval::cli(&raw[1..]); }
    if matches!(raw.first().map(String::as_str), Some("plugin" | "plugins")) { return crate::plugins::cli(&raw[1..]); }
    if raw.first().map(String::as_str) == Some("sandbox") { return crate::images::cli(&raw[1..]); }
    let args = match parse_args() { Ok(a) => a, Err(c) => return c };
    if args.serve_jsonl { return crate::jsonl::run(args); }
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    if args.prompt.is_some() || !tty {
        install_sigint();
        let mut fe = ui::Plain::new();
        run_session(args, &mut fe)
    } else {
        crate::tui::run(args)
    }
}

/// Startup (cwd, config, max_ctx, store, --continue), then the `-p` run or the interactive loop on `fe.read_input()`.
pub fn run_session(args: Args, fe: &mut dyn Frontend) -> i32 {
    let cwd = match &args.cwd {
        Some(d) => match std::env::set_current_dir(d) {
            Ok(()) => std::env::current_dir().unwrap_or_else(|_| d.clone()),
            Err(e) => { fe.error(&format!("--cwd {}: {e}", d.display())); return 2; }
        },
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    // --base-url: that server. With --type (also the demo gateway) that kind of server with config.toml's settings and
    // no saved connection; without, the active connection's kind, model and size stay (Config::point_at)
    let kind = args.base_url.as_ref().and(args.kind.as_deref());
    let forced = args.provider.clone().or_else(|| kind.map(|_| crate::provider::NONE.to_string()));
    let mut cfg = match Config::load_as(&cwd, forced.as_deref()) { Ok(c) => c, Err(e) => { fe.error(&e); return 2; } };
    if let Some(u) = args.base_url.clone() { cfg.point_at(&u, kind); }
    if let Some(m) = args.model.clone() { cfg.model = m; }
    if args.max_ctx.is_some() { cfg.max_ctx = args.max_ctx; }
    // no model configured: say so once and send nothing (Elisa never picks one)
    let no_model = cfg.model.trim().is_empty();
    if no_model && args.prompt.is_some() { fe.error(crate::api::NO_MODEL); return 2; }
    let mut client = Client::new(&cfg);
    client.set_project(&cwd);
    let mut max_ctx = cfg.max_ctx.or_else(|| client.server_max_ctx()).unwrap_or(200_000);
    if !cfg.provider.is_empty() {
        // a connection's size cannot grow the gateway's window: past it, every request would overflow
        if let Some(has) = client.server_max_ctx().filter(|&has| has < max_ctx) {
            fe.info(&format!("context: the gateway runs with {has}, not {max_ctx}; using {has} (restart it with a larger --max-ctx to go higher)"));
            max_ctx = has;
        }
        fe.info(&match &cfg.leaves { Some(h) => format!("connection {}: what this session reads goes to {h}", cfg.provider), None => format!("connection {}: stays on this machine", cfg.provider) });
    }

    let mut s = Session::new(fe, cfg, client, cwd, max_ctx, args.prompt.is_none(), args.yes);
    // an `openai` connection may be Ollama: then its native API, which (unlike its OpenAI endpoint) keeps the window
    let ollama = (s.cfg.transport == crate::provider::Transport::OpenAi).then(|| crate::provider::ollama_caps(&s.cfg.base_url, &s.cfg.model)).flatten();
    if let Some(caps) = &ollama {
        s.client.use_ollama(s.max_ctx, caps.iter().any(|c| c == "thinking"));
        s.fe.info(&format!("Ollama: native API, window {} tokens · {}", s.max_ctx, caps.join(", ")));
    }
    s.vision = crate::provider::vision(&s.cfg.transport, ollama.as_deref(), s.cfg.vision);
    s.checks_off = args.no_checks;
    s.open_memory();
    let mut resumed = None;
    if args.cont {
        if let Some(h) = s.store.load_last_session() { resumed = Some(h.len()); s.history = h; }
    }
    // only a continued session carries the last state card and works on its notes; a new one starts clean
    // (the card's goal / next step are that session's task) and gets the notes as background
    if resumed.is_some() { s.carry_card(); s.notes_bg = false; }
    match args.prompt {
        Some(p) => {
            match (args.cont, resumed) {
                (true, Some(n)) => s.fe.info(&format!("resumed last session ({n} messages)")),
                (true, None) => s.fe.info("no saved session to continue"),
                _ => {}
            }
            s.budget_warning();
            let code = s.run_print(p);
            s.end_memory();
            code
        }
        None => {
            s.banner(resumed);
            if no_model { s.fe.error(crate::api::NO_MODEL); }
            s.budget_warning();
            if let Some(id) = s.card_id { s.fe.info(&format!("continuing from state card #{id}")); }
            if args.cont && resumed.is_none() { s.fe.info("no saved session to continue"); }
            // a plugin server that could not start for a missing image: ask once whether to download it
            s.offer_plugin_images();
            s.run_repl();
            s.end_memory();
            0
        }
    }
}

/// Graph memory for one Elisa session (opening Elisa until Ctrl+C / exit; /clear starts a new one).
struct Mem {
    db: Memory,
    session: u64,
    /// cwd's absolute path
    project: String,
}

struct Session<'f> {
    fe: &'f mut dyn Frontend,
    cfg: Config,
    client: Client,
    store: Store,
    tctx: ToolCtx,
    cwd: PathBuf,
    max_ctx: usize,
    /// the model can see pictures (`provider::vision`): the app enables its image button on this
    vision: bool,
    /// pictures that came with the input line just read: they go into this turn's user message
    attach: Vec<Block>,
    /// MCP servers plugged into this session (config `[mcp.NAME]` + the built-in `web`): their tools sit next to Elisa's
    mcp: crate::mcp_host::Host,
    tools: Vec<ToolDef>,
    tools_tokens: usize,
    project: Option<String>,
    notes: Option<String>,
    /// the notes are background from an earlier session (a new session, or after /clear or /reset), not this
    /// session's task: the first message marks them so and leaves their old session goal out (`context::background_notes`)
    notes_bg: bool,
    system: String,
    system_tokens: usize,
    history: Vec<Message>,
    /// (history len, real prompt+output tokens) from the last reply; the estimate builds on it until history is rewritten.
    real: Option<(usize, usize)>,
    /// (real prompt tokens, text estimate of that same prompt) from the last reply: after a rewrite (`real` = None)
    /// the text estimate is scaled by this ratio when it ran low, so a fit decision never trusts the estimate alone
    calib: Option<(usize, usize)>,
    /// Raw text of the current turn's user input (re-sent with the new notes after an auto-compact).
    input: String,
    interactive: bool,
    /// sandbox images the user said no to this session: a run that has a present line to fall back to is not asked
    /// about them again
    declined_images: Vec<&'static str>,
    yolo: bool,
    always: HashSet<String>,
    mem: Option<Mem>,
    /// turns recorded in this memory session (a turn = one user text or one assistant reply)
    turn_no: usize,
    /// turns already folded into notes (turn_no - folded = unfolded turns), and the unfolded turns' memory ids
    folded: usize,
    fold_ids: Vec<u64>,
    /// after a failed fold: don't retry below this window size (tokens)
    fold_retry_at: usize,
    /// a summary request ran out on reasoning: later folds use `context::digest` without asking the model again
    digest_only: bool,
    /// inside `turn` (a fold then keeps the current request in the first message)
    in_turn: bool,
    /// a fold already ran in the current user turn: no second fold until the next one
    folded_this_turn: bool,
    /// `[tool args → result]` lines of the current turn, recorded with the assistant reply
    tool_lines: Vec<String>,
    /// the current turn's request failed before anything was answered (nothing to record)
    unsent: bool,
    /// the last recall / session save was slow: show a progress line for the next one
    recall_slow: bool,
    save_slow: bool,
    /// latest state card: text, memory id, and the exchange it came from (fresh = from the last <= 2 exchanges)
    card: Option<String>,
    card_id: Option<u64>,
    card_ex: Option<usize>,
    /// completed exchanges (user turn + final reply) in this run
    exchanges: usize,
    /// card parsed from the current turn's final reply (None + `card_broken` = none or broken)
    new_card: Option<String>,
    card_broken: bool,
    /// measured embedding speed (memories per second), for the exit budget
    index_rate: Option<f64>,
    /// memories added since the last index_pending (this run)
    unindexed: usize,
    /// the gateway's domain watcher: notes after cards and folds; view refreshed after turns and folds
    watch: Watcher,
    /// --no-checks: no check-before-done after edits
    checks_off: bool,
    /// config `outside` (src/outside.rs): what a tool call outside the project folder does
    outside_mode: outside::Mode,
    /// "allow this path for the session" answers: (resolved path, write); never saved
    outside_ok: Vec<(PathBuf, bool)>,
    /// outline / read_symbol tokens vs the whole files they stood in for (/context)
    ast_stats: AstStats,
    /// state cards made in this run (the card number when memory is off)
    cards: u64,
    /// per-request metrics (src/metrics.rs): the request in progress, and the session totals for `/stats`
    req: Option<ReqRun>,
    stats: metrics::Stats,
    /// state cards since the notes were last written: a card fold merges them into the notes (no model call)
    card_log: Vec<String>,
    /// the session goal (`context::next_goal`): set from the first real request, changed only by a stated new goal;
    /// `[state]` shows it apart from the current request and the notes keep it on top
    goal: Option<String>,
    /// the current request in the user's words (one line)
    user_req: String,
    /// the gateway's prefill-only warm-up after a fold (config `warm`): WARM_UNKNOWN until /health or the first try
    /// says; a background thread sets it from the answer
    warm: Arc<AtomicU8>,
    /// the reasoning tools (`think` / `steps` / `step_done` / `calc`, `mcp_reason.rs`) are in this turn's request:
    /// decided once at turn start (`reason_turn`), so the tool list never changes mid-turn
    reason_on: bool,
    /// the relevance gate's choice for this turn (`mcp_reason::gate`, config `gate_tools`): (`docs_*`, `sec_*`) offered
    gate: (bool, bool),
    /// what Elisa's own tools changed this session (`context::Ledger`): the notes' file list and the check of a
    /// summary's "done" claims
    ledger: context::Ledger,
    /// the audit log (`audit.rs`; config `audit`): off = records nothing
    audit: crate::audit::Log,
    /// how the user answered the approval of the call in progress (None: no one was asked), for the audit log
    answer: Option<&'static str>,
    /// the home folder (plugins, the user config a saved rule goes to); None without HOME
    home: Option<PathBuf>,
    /// the file slash commands in effect (`commands.rs`), merged from the user's, the plugins' and the project's
    file_cmds: Vec<FileCommand>,
    cmd_user: Vec<FileCommand>,
    cmd_plugins: Vec<FileCommand>,
    cmd_project: Vec<FileCommand>,
    /// the installed plugins (as /plugin list shows them) and the (plugin, server) pairs running in this session
    plugin_infos: Vec<plugins::Info>,
    plugin_servers: Vec<(String, String)>,
    /// the project's own commands and plugins, waiting for the one-time trust prompt (None: none, or loaded)
    proj_pending: Option<plugins::Project>,
    proj_infos: Vec<plugins::Info>,
    /// the user said no to the project's trust prompt in this session: not asked again until /plugin trust project
    proj_declined: bool,
}

struct ReqRun { ts: u64, t0: Instant, prompt: String }

enum TurnEnd { Done, Stopped, Failed(String) }

fn end_name(e: &TurnEnd) -> &'static str {
    match e { TurnEnd::Done => "done", TurnEnd::Stopped => "stopped", TurnEnd::Failed(_) => "failed" }
}

impl<'f> Session<'f> {
    /// `/model`: what this session talks to, and the saved connections
    fn model_command(&mut self) {
        use crate::provider::{path_in, summary, Providers};
        self.fe.info(&format!("model {} @ {} · connection {}", self.cfg.model, self.cfg.base_url, if self.cfg.provider.is_empty() { "none (config.toml)" } else { &self.cfg.provider }));
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        match Providers::load(&home) {
            Ok(f) => for l in summary(&self.cfg.provider, &f, &path_in(&home)) { self.fe.info(&l); },
            Err(e) => self.fe.error(&e),
        }
    }

    fn new(fe: &'f mut dyn Frontend, mut cfg: Config, client: Client, cwd: PathBuf, max_ctx: usize, interactive: bool, yes: bool) -> Session<'f> {
        let store = Store::new(&cwd);
        let tctx = ToolCtx { cwd: cwd.clone(), max_chars: cfg.tool_output_max_chars, bash_timeout_s: cfg.bash_timeout_s, ast_guard: cfg.ast_guard };
        crate::sandbox::set_mode(crate::sandbox::Mode::parse(&cfg.shell_sandbox).unwrap_or(crate::sandbox::Mode::platform_default()));
        // what config loading skipped or ignored (a bad [mcp.NAME], a bad `web` value): one line each, nothing when clean
        for n in &cfg.notes { fe.info(n); }
        // the MCP servers: the configured ones, and Elisa's own web server first unless it is off or replaced
        let mut servers = cfg.mcp.clone();
        let web_on = !cfg.web.is_empty() && cfg.web != "off" && !cfg.mcp_off.iter().any(|n| n == "web");
        let web_in = web_on && !servers.iter().any(|c| c.name == "web");
        if web_in { servers.insert(0, crate::mcp_host::builtin_web(&cfg.web, &cfg.searxng_url, &cfg.brave_api_key)); }
        // the documents server (`docs_pdf` / `docs_sheet`), right after the web server, unless `docs = "off"` or replaced
        let docs_on = cfg.docs == "on" && !cfg.mcp_off.iter().any(|n| n == "docs");
        let docs_in = docs_on && !servers.iter().any(|c| c.name == "docs");
        if docs_in { servers.insert(usize::from(web_in), crate::mcp_host::builtin_docs(&cwd, cfg.tool_output_max_chars)); }
        // the security server (`sec_*`), after the documents server, unless `security = "off"` or replaced
        let sec_on = cfg.security == "on" && !cfg.mcp_off.iter().any(|n| n == "sec");
        if sec_on && !servers.iter().any(|c| c.name == "sec") {
            servers.insert(usize::from(web_in) + usize::from(docs_in), crate::mcp_host::builtin_sec(&cwd, cfg.tool_output_max_chars));
        }
        // the code sandbox (`run_code`): only when the user switched it on (`sandbox = "colima"`)
        let run_on = cfg.sandbox == "colima" && !cfg.mcp_off.iter().any(|n| n == "run");
        if run_on && !servers.iter().any(|c| c.name == "run") { servers.push(crate::mcp_host::builtin_run(cfg.sandbox_timeout_s, cfg.sandbox_fetch_timeout_s, &cwd)); }
        // the reasoning tools for small models, last, unless `reason = "off"` or replaced (offered per turn: `reason_turn`)
        let reason_srv = cfg.reason != "off" && !cfg.mcp_off.iter().any(|n| n == crate::mcp_reason::NAME);
        if reason_srv && !servers.iter().any(|c| c.name == crate::mcp_reason::NAME) { servers.push(crate::mcp_host::builtin_reason()); }
        // each server's tool filter (`[mcp.NAME] tools` / `exclude`; the built-in ones by their name too)
        for c in &mut servers { if let Some(f) = cfg.mcp_filter.get(&c.name) { c.filter = f.clone(); } }
        // enabled plugins (`plugins.rs`): their servers next to the configured ones, their rules and commands
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let taken: Vec<String> = servers.iter().map(|c| c.name.clone()).chain(crate::config::BUILTIN_SERVERS.iter().map(|s| s.to_string())).collect();
        let pl = home.as_ref().map(|h| plugins::load_user(h, &cwd, &|n| taken.iter().any(|t| t == n))).unwrap_or_default();
        for n in &pl.notes { fe.info(n); }
        for (lv, p, src) in &pl.rules { if let Err(e) = cfg.perms.add(*lv, p, src) { fe.info(&format!("{src}: rule {p:?} skipped: {e}")); } }
        let plugin_servers: Vec<(String, String)> = pl.servers.iter().map(|(p, c)| (p.clone(), c.name.clone())).collect();
        servers.extend(pl.servers.into_iter().map(|(_, c)| c));
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        let (mut mcp, mcp_notes) = crate::mcp_host::Host::start(&servers, &|n| own.iter().any(|o| o == n));
        for n in &mcp_notes { fe.info(n); }
        // filter names a server does not have: known only after its tools/list
        for n in mcp.filter_notes() { fe.info(n); cfg.notes.push(n.clone()); }
        // the audit log (servers started, from here on every tool call) and the tool-list pins of the servers that
        // are not Elisa's own (`pins.rs`): a changed list warns, and that server asks on every call until /trust
        let audit = match &home { Some(h) if cfg.audit == "on" => crate::audit::Log::open(h, &cwd), _ => crate::audit::Log::default() };
        for n in &mcp_notes { audit.note("server_start", &[("outcome", "error"), ("detail", n)]); }
        mcp.audit_to(&audit);
        if let Some(h) = &home {
            for (warn, n) in mcp.check_pins(&crate::pins::path_in(h)) { if warn { fe.error(&n) } else { fe.info(&n) } }
        }
        let (project, notes, always) = (store.load_project(), store.load_notes(), store.load_allowed());
        let watch = Watcher::new(&cfg.base_url, &cwd.display().to_string());
        if let Some(p) = fe.watcher_push() { watch.set_push(p); }
        if cfg.transport != crate::provider::Transport::Gateway { watch.disable(); }
        let mut s = Session {
            fe, cfg, client, store, tctx, cwd, max_ctx, vision: false, attach: Vec::new(), mcp, tools: Vec::new(), tools_tokens: 0, project, notes, notes_bg: true,
            system: String::new(), system_tokens: 0, history: Vec::new(), real: None, calib: None, input: String::new(), interactive, declined_images: Vec::new(), yolo: yes, always,
            mem: None, turn_no: 0, folded: 0, fold_ids: Vec::new(), fold_retry_at: 0, digest_only: false, in_turn: false, folded_this_turn: false, tool_lines: Vec::new(), unsent: false,
            recall_slow: false, save_slow: false, index_rate: None, unindexed: 0, watch,
            card: None, card_id: None, card_ex: None, exchanges: 0, new_card: None, card_broken: false, checks_off: false, outside_mode: outside::Mode::Ask, outside_ok: Vec::new(), ast_stats: AstStats::default(), cards: 0,
            req: None, stats: metrics::Stats::default(),
            card_log: Vec::new(), goal: None, user_req: String::new(), warm: Arc::new(AtomicU8::new(WARM_UNKNOWN)),
            reason_on: false, gate: (false, false), ledger: context::Ledger::default(), audit, answer: None,
            home, file_cmds: Vec::new(), cmd_user: Vec::new(), cmd_plugins: pl.commands, cmd_project: Vec::new(),
            plugin_infos: pl.infos, plugin_servers, proj_pending: None, proj_infos: Vec::new(), proj_declined: false,
        };
        s.load_commands();
        // the app's plugin pane: what is installed, from the start (nothing installed: no event; `/plugin list` asks)
        if !s.plugin_infos.is_empty() || !s.proj_infos.is_empty() { s.plugins_event(); }
        s.outside_mode = outside::Mode::parse(&s.cfg.outside).unwrap_or(outside::Mode::Ask);
        if s.cfg.gate_tools == "off" { s.gate = (true, true); }
        s.rebuild_system();
        s
    }

    /// The only place the system prompt and tools change: startup, compaction, /clear, /compact, /forget. One mode:
    /// the code prompt and the full tool set on every request (no chat/code split since 2026-11-01).
    fn rebuild_system(&mut self) {
        (self.system, self.tools) = (context::system_prompt(&self.cwd, self.cfg.system_prompt_file.as_deref(), self.project.as_deref(), None), tools::defs_for(self.cfg.ast));
        // stable tools first (Elisa's own, then every MCP server that is always offered); the per-turn ones after them,
        // in a fixed order (`docs`, `sec`, then the reasoning tools), so a flip only changes the end of the list
        let defs = self.mcp.defs();
        let m = &self.mcp;
        let gated = |name: &str| m.reason_tool(name) || m.gated_server(name).is_some();
        self.tools.extend(defs.iter().filter(|t| !gated(&t.name)).cloned());
        for (i, srv) in crate::mcp_reason::GATED.iter().enumerate() {
            if [self.gate.0, self.gate.1][i] { self.tools.extend(defs.iter().filter(|t| m.gated_server(&t.name) == Some(*srv)).cloned()); }
        }
        // the reasoning tools only on a turn that wants them (`reason_turn`; the list stays fixed within a turn)
        if self.reason_on { self.tools.extend(defs.iter().filter(|t| m.reason_tool(&t.name)).cloned()); }
        // config `ast = false`: no syntax-tree tools, so no prompt line about them (fixed per session: byte-stable)
        if !self.cfg.ast { self.system = self.system.lines().filter(|l| !l.contains("`outline`")).map(|l| format!("{l}\n")).collect(); }
        // one line on `<untrusted>` fences, only when a tool can return outside text (`untrusted.rs`)
        if self.tools.iter().any(|t| self.untrusted_source(&t.name, &serde_json::Value::Null).is_some()) {
            if !self.system.ends_with('\n') { self.system.push('\n'); }
            self.system.push_str(crate::untrusted::SYSTEM_LINE);
            self.system.push('\n');
        }
        // + the chat template's tool preamble (Qwen: ~236 tokens, measured with count_tokens)
        self.tools_tokens = estimate_tokens(&serde_json::to_string(&self.tools).unwrap_or_default()) + TOOL_PREAMBLE;
        self.real = None;
        self.system_tokens = estimate_tokens(&self.system);
    }

    /// Whether this session's requests leave the machine: a cloud connection, or no connection and a non-loopback
    /// `base_url` (then `redact = "remote"` hides secrets from the model).
    fn model_leaves(&self) -> bool {
        self.cfg.leaves.is_some() || (self.cfg.provider.is_empty() && !crate::provider::is_local(&self.cfg.base_url))
    }

    /// The `<untrusted>` fence source for a result of tool `name`, None when it is trusted (`untrusted::source`).
    fn untrusted_source(&self, name: &str, input: &serde_json::Value) -> Option<String> {
        let server = self.mcp.server_of(name);
        // a plugin's server is third-party code like a configured one
        let user = server.is_some_and(|s| self.cfg.mcp.iter().any(|c| c.name == s) || self.plugin_servers.iter().any(|(_, n)| n == s));
        crate::untrusted::source(name, server, user, input)
    }

    /// Tool output on its way to the model: secrets hidden (config `redact`, `secrets.rs`) and outside text fenced
    /// (`untrusted.rs`). The user's own display (`tool_done`) shows the output as it came.
    fn guard_output(&mut self, name: &str, input: &serde_json::Value, content: String) -> String {
        // whatever the mode: a secret seen here is hidden wherever it shows up again on disk (`secrets::Scrubber`)
        crate::secrets::learn(&content);
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        let content = if mode.applies(self.model_leaves()) {
            let (r, n) = crate::secrets::redact(&content);
            if n > 0 { self.fe.info(&format!("[{} hidden from the model]", crate::secrets::count_words(n))); }
            r
        } else { content };
        let Some(src) = self.untrusted_source(name, input) else { return content };
        let (fenced, hit) = crate::untrusted::fence(&src, &content);
        if let Some(h) = hit {
            self.fe.info(&format!("[{name}: text that looks like a prompt injection (\"{h}\"); marked for the model as data]"));
        }
        fenced
    }

    fn fixed(&self) -> usize { self.system_tokens + self.tools_tokens }
    /// Tokens of the next request. An empty history still sends a first message (cwd/OS/date + notes + [state]), so
    /// that header counts: after /clear the shown size is what the next request really starts with.
    fn estimate(&self) -> usize {
        match self.real {
            Some((n, t)) if n <= self.history.len() => t + estimate_messages(&self.history[n..]),
            _ if self.history.is_empty() => self.fixed() + estimate_messages(&[Message::user_text(self.first(""))]),
            _ => self.fixed() + estimate_messages(&self.history),
        }
    }
    /// The next request's size where going over the context is fatal (a step's results, the send check): the largest
    /// of the text estimate, the server's last reported tokens + the estimate of what was appended since, and the text
    /// estimate scaled by the last measured real/estimate ratio (`context::safe_request_tokens`).
    fn est_safe(&self) -> usize {
        let text = self.fixed() + estimate_messages(&self.history);
        let reported = self.real.filter(|&(n, _)| n <= self.history.len()).map(|(n, t)| (t, estimate_messages(&self.history[n..])));
        context::safe_request_tokens(text, reported, self.calib)
    }
    /// Output reserve, capped so a small max_ctx (below max_output) still leaves room for the prompt.
    /// the reply's room: its output, plus some of a reasoning model's reasoning (`Client::reasoning_room`)
    fn reserve(&self) -> usize { self.cfg.max_output.min(self.max_ctx / 4) + self.client.reasoning_room().min(self.max_ctx / 8) }
    /// Prompt budget: max_ctx minus the output reserve and the safety margin (`context::ctx_margin`, 3%).
    fn limit(&self) -> usize { self.max_ctx.saturating_sub(self.reserve() + context::ctx_margin(self.max_ctx)) }

    /// Once at startup: a warning when system + tools take more than FIXED_WARN_PCT of the usable context on a plain
    /// turn, naming the biggest servers (soak 2026-11-08: 2.8K of 5.9K at 8K, so the conversation had ~3K and folded
    /// before nearly every turn).
    fn budget_warning(&mut self) {
        let (fixed, limit) = (self.fixed(), self.limit());
        if limit == 0 || fixed * 100 <= limit * FIXED_WARN_PCT { return; }
        let parts = self.tool_costs();
        let top: Vec<String> = parts.iter().take(3).map(|(n, t)| format!("{n} {}", ui::k(*t))).collect();
        self.fe.info(&format!("context: system + tools take {} of the {} usable tokens ({}%) before any conversation; biggest: {} · \
            a larger max_ctx, or fewer servers (web / sandbox = \"off\", [mcp.NAME] enabled = false), leaves more room",
            ui::k(fixed), ui::k(limit), fixed * 100 / limit, top.join(", ")));
    }

    /// Estimated tokens of this request's tool definitions by owner: Elisa's own, then each MCP server whose tools
    /// are offered this turn; largest first.
    fn tool_costs(&self) -> Vec<(String, usize)> {
        let own: Vec<&ToolDef> = self.tools.iter().filter(|t| !self.mcp.owns(&t.name)).collect();
        let mut parts: Vec<(String, usize)> = self.mcp.costs().into_iter()
            .filter(|(srv, _)| self.tools.iter().any(|t| self.mcp.server_of(&t.name) == Some(srv.as_str()))).collect();
        parts.push(("Elisa's own tools".into(), estimate_tokens(&serde_json::to_string(&own).unwrap_or_default())));
        parts.sort_by_key(|p| std::cmp::Reverse(p.1));
        parts
    }

    /// The MCP servers that do not run because config switched them off (for `/mcp`): user servers with
    /// `enabled = false`, and Elisa's own servers with the setting that keeps each out.
    fn mcp_off(&self) -> Vec<crate::mcp_host::Off> {
        use crate::mcp_host::Off;
        let c = &self.cfg;
        let off_by = |n: &str| c.mcp_off.iter().any(|o| o == n);
        let mut v: Vec<Off> = Vec::new();
        let builtins: [(&str, bool, String); 5] = [
            ("web", c.web.is_empty() || c.web == "off", "web = \"off\"".into()),
            ("docs", c.docs != "on", "docs = \"off\"".into()),
            ("sec", c.security != "on", "security = \"off\"".into()),
            ("run", c.sandbox != "colima", format!("sandbox = \"{}\"", c.sandbox)),
            (crate::mcp_reason::NAME, c.reason == "off", "reason = \"off\"".into()),
        ];
        for (name, setting_off, why) in builtins.iter().cloned() {
            if self.mcp.running(name) || c.mcp.iter().any(|s| s.name == name) { continue; }
            if off_by(name) { v.push(Off { name: name.into(), builtin: true, why: format!("[mcp.{name}] enabled = false") }); }
            else if setting_off { v.push(Off { name: name.into(), builtin: true, why }); }
        }
        for n in c.mcp_off.iter().filter(|n| !builtins.iter().any(|b| b.0 == n.as_str())) {
            v.push(Off { name: n.clone(), builtin: false, why: format!("[mcp.{n}] enabled = false") });
        }
        v
    }

    /// `/mcp [NAME]`: the MCP servers, or one of them with its tools' descriptions (`mcp_host::Host::report`)
    fn mcp_command(&mut self, name: &str) {
        let off = self.mcp_off();
        let (gate, reason_on) = (self.gate, self.reason_on);
        let off_turn = |srv: &str| match srv { "docs" => !gate.0, "sec" => !gate.1, n if n == crate::mcp_reason::NAME => !reason_on, _ => false };
        if name.is_empty() { return self.fe.info(&self.mcp.report(&off, &off_turn)); }
        match self.mcp.report_one(name, &off, &off_turn) {
            Ok(t) => self.fe.info(&t),
            Err(e) => self.fe.error(&e),
        }
    }

    fn banner(&mut self, resumed: Option<usize>) {
        let b = Banner {
            model: self.cfg.model.clone(), base_url: self.cfg.base_url.clone(), cwd: self.cwd.display().to_string(),
            max_ctx: self.max_ctx, notes: self.notes.clone(), resumed, vision: self.vision,
        };
        self.fe.banner(&b);
    }

    /// the pictures sent with the line just read; a model without vision gets none, and the user is told why
    fn take_attached(&mut self) -> Vec<Block> {
        let imgs = self.fe.take_images();
        if imgs.is_empty() || self.vision { return imgs; }
        self.fe.error(&format!("{} can't see images, so the {} attached {} not sent. Choose a model with vision in Model Connections, or set vision = true on this connection if it has it.",
            self.cfg.model, imgs.len(), if imgs.len() == 1 { "image was" } else { "images were" }));
        Vec::new()
    }

    fn save(&mut self) {
        if self.history.is_empty() { return; }
        let t0 = Instant::now();
        let show = self.save_slow;
        if show { self.fe.progress(Some(&Progress { label: "saving session".into(), ..Default::default() })); }
        if let Err(e) = self.store.save_session(&self.history) { self.fe.error(&format!("session not saved: {e}")); }
        self.save_slow = t0.elapsed() > SLOW;
        if show { self.fe.progress(None); }
    }

    fn run_repl(&mut self) {
        loop {
            self.watch_poll();
            let Some(line) = self.fe.read_input() else { break };
            self.attach = self.take_attached();
            self.watch_poll();
            let line = line.trim();
            if line.is_empty() { continue; }
            self.fe.user(line);
            let mut prompt = line.to_string();
            if line.starts_with('/') {
                match self.expand_command(line) {
                    Some(Some(p)) => prompt = p,
                    Some(None) => { self.fe.turn_done(); continue; }
                    None => {
                        let go_on = self.command(line);
                        self.fe.turn_done();
                        if !go_on { break; }
                        continue;
                    }
                }
            }
            self.req_begin(line);
            let end = self.turn(prompt);
            let outcome = end_name(&end);
            if let TurnEnd::Failed(e) = end { self.fe.error(&e); }
            self.save();
            self.fe.turn_done();
            self.watch.refresh();
            self.maybe_fold();
            self.req_end(outcome);
        }
        // piped input: the last notes and view before exit
        if !self.watch.pushes() { self.watch.flush(WATCH_EXIT_WAIT); }
        self.watch_poll();
    }

    fn run_print(&mut self, prompt: String) -> i32 {
        // `-p "/review src"`: a file command becomes its prompt (a project one needs its trust given before)
        let prompt = match prompt.starts_with('/').then(|| self.expand_command(prompt.trim())).flatten() {
            Some(Some(p)) => p,
            Some(None) => return 1,
            None => prompt,
        };
        self.req_begin(&prompt);
        let end = self.turn(prompt);
        self.req_end(end_name(&end));
        self.save();
        self.fe.turn_done();
        self.watch.refresh();
        if !self.watch.pushes() { self.watch.flush(WATCH_EXIT_WAIT); }
        self.watch_poll();
        if let TurnEnd::Failed(e) = end { self.fe.error(&e); return 1; }
        let last = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default();
        println!("{}", last.trim());
        0
    }

    // ----- plugins, file commands, pattern rules (plugins.rs, commands.rs, perms.rs) -----

    /// the user's config file, where "always allow this pattern" saves its rule
    fn user_config(&self) -> Option<PathBuf> { self.home.as_ref().map(|h| h.join(".config/elisa/config.toml")) }

    /// Startup: the user's command files, and the project's commands and plugins — loaded when the user trusted this
    /// exact content before, else kept back until the trust prompt (asked lazily: the first time one of its commands
    /// runs, or on `/plugin trust project`).
    fn load_commands(&mut self) {
        let mut notes = Vec::new();
        if let Some(h) = &self.home { self.cmd_user = crate::commands::load_dir(&h.join(".elisa/commands"), &crate::commands::Source::User, &mut notes); }
        if let Some(p) = plugins::load_project(&self.cwd) {
            let trusted = self.home.as_ref().is_some_and(|h| plugins::project_trusted(h, &self.cwd, &p.hash));
            if trusted { self.apply_project(p); } else {
                self.fe.info(&format!("this project has its own commands or rules ({}); they load once you trust them: run one, or /plugin trust project",
                    project_summary(&p)));
                self.proj_infos = p.infos.clone();
                self.proj_pending = Some(p);
            }
        }
        for n in notes { self.fe.info(&n); }
        let notes = self.merge_commands();
        for n in notes { self.fe.info(&n); }
    }

    /// the file commands merged again (Elisa's own commands win a clash); the notes say what was left out
    fn merge_commands(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let lists = vec![self.cmd_user.clone(), self.cmd_plugins.clone(), self.cmd_project.clone()];
        self.file_cmds = crate::commands::merge(lists, &builtin_command, &mut notes);
        notes
    }

    /// a trusted project's commands and deny / ask rules, in effect from now on
    fn apply_project(&mut self, p: plugins::Project) {
        for (lv, pat, src) in &p.rules {
            if let Err(e) = self.cfg.perms.add(*lv, pat, src) { self.fe.info(&format!("{src}: rule {pat:?} skipped: {e}")); }
        }
        for n in &p.notes { self.fe.info(n); }
        self.cmd_project = p.commands;
        self.proj_infos = p.infos.into_iter().map(|mut i| { i.trusted = true; i }).collect();
    }

    /// The one-time trust prompt for the project's commands and plugins: their full text, then yes / no. True: loaded.
    fn trust_project(&mut self) -> bool {
        let Some(p) = self.proj_pending.take() else { self.fe.info("this project has no commands or plugins waiting for trust"); return false };
        let Some(home) = self.home.clone() else { self.fe.error("no home folder: the trust cannot be kept"); self.proj_pending = Some(p); return false };
        if !self.interactive { self.fe.error("trusting a project's commands needs a person to say yes (interactive session)"); self.proj_pending = Some(p); return false; }
        self.fe.info(&p.text);
        self.fe.event("project_trust", &json!({"path": self.cwd.display().to_string(), "hash": p.hash, "text": p.text,
            "commands": p.commands.iter().map(|c| json!({"name": c.name, "description": c.description, "source": c.source.kind()})).collect::<Vec<_>>(),
            "rules": p.rules.iter().map(|(l, pat, src)| json!({"level": l.word(), "pattern": pat, "source": src})).collect::<Vec<_>>(),
            "plugins": p.infos.iter().map(|i| i.name.clone()).collect::<Vec<_>>()}));
        let yes = self.fe.ask_yes_no("project commands", "Trust this project's commands and rules? (kept in ~/.elisa; any change asks again)");
        let path = self.cwd.display().to_string();
        if !yes {
            self.audit.note("plugin", &[("summary", "trust project"), ("outcome", "declined"), ("detail", &path)]);
            self.fe.info("not trusted: the project's commands stay off (/plugin trust project to review again)");
            self.proj_declined = true;
            self.proj_pending = Some(p);
            return false;
        }
        if let Err(e) = plugins::trust_project(&home, &self.cwd, &p.hash) { self.fe.error(&format!("the trust was not saved: {e}")); }
        self.audit.note("plugin", &[("summary", "trust project"), ("outcome", "ok"), ("detail", &path)]);
        self.proj_declined = false;
        self.apply_project(p);
        for n in self.merge_commands() { self.fe.info(&n); }
        self.fe.info("project commands and rules loaded");
        self.plugins_event();
        true
    }

    /// A line that names a file command: the turn it becomes (Some(None): handled here, nothing to send — a declined
    /// trust prompt). None: not a file command.
    fn expand_command(&mut self, line: &str) -> Option<Option<String>> {
        let (cmd, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if builtin_command(cmd) { return None; }
        if let Some(c) = self.file_cmds.iter().find(|c| c.name == cmd) { return Some(Some(c.prompt(args))); }
        // a project command not trusted yet: the trust prompt first
        if self.proj_pending.as_ref().is_some_and(|p| p.commands.iter().any(|c| c.name == cmd)) {
            if self.proj_declined {
                self.fe.error(&format!("{cmd} is a project command you have not trusted: /plugin trust project to review it"));
                return Some(None);
            }
            if !self.trust_project() { return Some(None); }
            return Some(self.file_cmds.iter().find(|c| c.name == cmd).map(|c| c.prompt(args)));
        }
        None
    }

    /// /help: Elisa's commands, then the file commands with where each comes from
    fn help(&self) -> String {
        let mut s = help_text();
        if !self.file_cmds.is_empty() {
            s.push_str("\nFile commands:\n");
            for c in &self.file_cmds { s.push_str(&c.help_line()); s.push('\n'); }
            s.pop();
        }
        if let Some(p) = &self.proj_pending {
            s.push_str(&format!("\nProject commands waiting for trust: {}", p.commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" ")));
        }
        s
    }

    /// every plugin, the user's and the project's
    fn all_plugins(&mut self) -> Vec<plugins::Info> {
        if let Some(h) = self.home.clone() { self.plugin_infos = plugins::load_user(&h, &self.cwd, &|_| false).infos; }
        self.plugin_infos.iter().chain(&self.proj_infos).cloned().collect()
    }

    /// the `plugins` event (the app's plugin pane)
    fn plugins_event(&mut self) {
        let all = self.all_plugins();
        let list: Vec<serde_json::Value> = all.iter().map(|i| {
            let mut v = plugins::info_json(i);
            // a running server's tool count and cost as this session sees them
            if let Some(a) = v["servers"].as_array_mut() {
                for s in a.iter_mut() {
                    if let Some((n, t)) = s["name"].as_str().and_then(|n| self.mcp.server_tools(n).filter(|_| self.plugin_servers.iter().any(|(_, x)| x == n))) {
                        s["tools"] = json!(n);
                        s["tokens"] = json!(t);
                        s["running"] = json!(true);
                    }
                }
                let (tools, tokens) = a.iter().fold((0, 0), |(x, y), s| (x + s["tools"].as_u64().unwrap_or(0), y + s["tokens"].as_u64().unwrap_or(0)));
                v["tools"] = json!(tools);
                v["tokens"] = json!(tokens);
            }
            v
        }).collect();
        self.fe.event("plugins", &json!({"plugins": list}));
    }

    /// The enabled plugins' servers, rules and commands brought in line with what is installed: servers of plugins
    /// that are gone or off stop, new ones start (`touched`: that plugin's servers restart either way).
    fn reload_plugins(&mut self, touched: Option<&str>) {
        let Some(home) = self.home.clone() else { return };
        let config_names: Vec<String> = self.cfg.mcp.iter().map(|c| c.name.clone()).collect();
        let running_other: Vec<String> = self.mcp_names().into_iter().filter(|n| !self.plugin_servers.iter().any(|(_, s)| s == n)).collect();
        let taken = |n: &str| config_names.iter().any(|c| c == n) || running_other.iter().any(|c| c == n) || crate::config::BUILTIN_SERVERS.contains(&n);
        let l = plugins::load_user(&home, &self.cwd, &taken);
        let keep: Vec<(String, crate::mcp_host::ServerCfg)> = l.servers;
        // stop: plugin servers no longer wanted, and the touched plugin's
        let stop: Vec<String> = self.plugin_servers.iter()
            .filter(|(p, s)| Some(p.as_str()) == touched || !keep.iter().any(|(kp, c)| kp == p && &c.name == s))
            .map(|(_, s)| s.clone()).collect();
        if !stop.is_empty() { self.mcp.remove(&stop); }
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        let start: Vec<crate::mcp_host::ServerCfg> = keep.iter().map(|(_, c)| c.clone()).collect();
        for (warn, n) in self.mcp.extend(&start, &|n| own.iter().any(|o| o == n)) { if warn { self.fe.error(&n) } else { self.fe.info(&n) } }
        self.plugin_servers = keep.iter().map(|(p, c)| (p.clone(), c.name.clone())).collect();
        self.cfg.perms.rules.retain(|r| !r.source.starts_with("plugin "));
        for (lv, p, src) in &l.rules { if let Err(e) = self.cfg.perms.add(*lv, p, src) { self.fe.info(&format!("{src}: rule {p:?} skipped: {e}")); } }
        for n in &l.notes { self.fe.info(n); }
        self.cmd_plugins = l.commands;
        self.plugin_infos = l.infos;
        for n in self.merge_commands() { self.fe.info(&n); }
        self.rebuild_system();
        self.offer_plugin_images();
    }

    fn mcp_names(&self) -> Vec<String> { self.mcp.costs().into_iter().map(|(n, _)| n).collect() }

    /// one plugin operation's outcome: the app's `plugin_result` event, the audit line, and a line for the user
    fn plugin_result(&mut self, op: &str, name: &str, r: Result<String, String>) {
        let (ok, text) = match &r { Ok(t) => (true, t.clone()), Err(e) => (false, e.clone()) };
        plugins::audit(&self.audit, op, name, if ok { "ok" } else if text == DECLINED { "declined" } else { "error" }, if ok { "" } else { &text });
        if ok { self.fe.info(&text) } else { self.fe.error(&text) }
        self.fe.event("plugin_result", &json!({"op": op, "name": name, "ok": ok, "text": text}));
        if ok { self.plugins_event(); }
    }

    /// the setup screen: the preview text, the app's `plugin_preview` event, then yes / no. Err: why not.
    fn plugin_setup(&mut self, op: &str, m: &plugins::Manifest, p: &plugins::Preview) -> Result<(), String> {
        self.fe.info(&p.text);
        self.fe.event("plugin_preview", &json!({"op": op, "name": m.name, "preview": p.json, "text": p.text}));
        if !p.problems.is_empty() { return Err(format!("plugin {}: cannot {op} now — {}", m.name, p.problems.join("; "))); }
        if !self.interactive { return Err(format!("plugin {}: a person has to approve the setup (run /plugin {op} in an interactive session, or elisa plugin {op} … --yes)", m.name)); }
        let q = match op { "update" => format!("Update plugin {} to {}?", m.name, m.version), "trust" => format!("Trust plugin {}'s current files?", m.name),
            _ => format!("Install plugin {} {}?", m.name, m.version) };
        if self.fe.ask_yes_no(&format!("plugin {op} {}", m.name), &q) { Ok(()) } else { Err(DECLINED.into()) }
    }

    /// after an install or update: the servers started once to learn their tools and cost (written to the lock)
    fn plugin_probe(&mut self, name: &str) -> String {
        let Some(home) = self.home.clone() else { return String::new() };
        let lines: Vec<String> = plugins::probe(&home, &self.cwd, name).into_iter().map(|(s, r)| match r {
            Ok((n, t)) => format!("{s}: {n} tool{} · ~{} tokens", if n == 1 { "" } else { "s" }, ui::k(t)),
            Err(e) => format!("{s}: did not start: {e}"),
        }).collect();
        lines.join("\n")
    }

    // ---------- sandbox images (`images.rs`) ----------

    /// the `sandbox_images` event (the app's list); quiet when docker cannot be reached
    fn sandbox_event(&mut self) {
        let Some(home) = self.home.clone() else { return };
        match images::status(&home, Some(&self.cwd)) {
            Ok(rows) => self.fe.event("sandbox_images", &images::status_json(&rows, &home)),
            Err(e) => self.fe.event("sandbox_images", &json!({"images": [], "error": e})),
        }
    }

    /// Downloads listed images after the user said yes (never for the model, never without a person): the question
    /// (`images::question`, `why` = what needs it), then each `docker pull` with its progress, cancellable with Esc /
    /// Ctrl-C, every outcome in the audit log. Ok: every image is there now.
    fn pull_images(&mut self, imgs: &[&'static images::Img], why: Option<String>) -> Result<(), String> {
        let Some(home) = self.home.clone() else { return Err("no home folder".into()) };
        if imgs.is_empty() { return Ok(()); }
        // no Colima: say how to start it (Elisa never does) before anyone is asked anything
        if let Err(e) = plugins::Docker::find(&home) { self.fe.error(&e); return Err(e); }
        let names = imgs.iter().map(|i| i.image).collect::<Vec<_>>().join(" ");
        if !self.interactive {
            let e = format!("downloading {names} needs a person to say yes: /sandbox pull in an interactive session, or elisa sandbox pull {names} --yes");
            for i in imgs { images::audit(&self.audit, i.image, "denied", "non-interactive"); }
            return Err(e);
        }
        let q = images::question(imgs, why.as_deref());
        if imgs.len() > 1 { self.fe.info(&imgs.iter().map(|i| format!("  {} (~{} MB) — {}", i.image, i.mb, i.purpose())).collect::<Vec<_>>().join("\n")); }
        if !self.fe.ask_yes_no(&format!("sandbox pull {names}"), &q) {
            for i in imgs { images::audit(&self.audit, i.image, "declined", ""); }
            self.fe.info("nothing was downloaded");
            return Err("declined".into());
        }
        for i in imgs {
            let fe: *mut (dyn Frontend + '_) = &mut *self.fe;
            let mut last = String::new();
            let t0 = Instant::now();
            // SAFETY: `fe` is used only inside these callbacks while `pull` runs on this thread; nothing else touches
            // `self.fe` meanwhile
            let r = images::pull(&home, i, &|| CANCEL.load(Ordering::SeqCst), &mut |p| unsafe {
                if p.label() != last {
                    last = p.label();
                    (*fe).progress(Some(&Progress { label: last.clone(), frac: (p.layers > 0).then(|| p.done as f32 / p.layers as f32), elapsed_s: p.elapsed_s, done: None }));
                }
                (*fe).event("image_pull", &p.json("pulling"));
            }, &mut |q| unsafe { (*fe).ask_yes_no(&format!("sandbox pull {}", i.image), q) });
            let label = format!("downloading {}", i.image);
            let done = |t: &str| Progress { label: label.clone(), frac: Some(1.0), elapsed_s: t0.elapsed().as_secs_f64(), done: Some(t.to_string()) };
            let (state, text, ok) = match r {
                Ok(images::Pulled::Ok(t)) => { images::audit(&self.audit, i.image, "ok", &t); ("done", t, true) }
                Ok(images::Pulled::Kept(t)) => { images::audit(&self.audit, i.image, "kept", &t); ("kept", t, false) }
                Ok(images::Pulled::Cancelled) => { images::audit(&self.audit, i.image, "cancelled", ""); ("cancelled", format!("download of {} cancelled", i.image), false) }
                Err(e) => { images::audit(&self.audit, i.image, "error", &e); ("failed", e, false) }
            };
            self.fe.progress(Some(&done(state)));
            self.fe.event("image_pull", &json!({"image": i.image, "state": state, "text": text}));
            if ok { self.fe.info(&text) } else { self.fe.error(&text) }
            if !ok { self.sandbox_event(); return Err(text); }
        }
        self.sandbox_event();
        Ok(())
    }

    /// `/sandbox [list] | pull NAME|IMAGE|stable|project|all`
    fn sandbox_command(&mut self, arg: &str) {
        let (op, rest) = arg.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        let Some(home) = self.home.clone() else { self.fe.error("no home folder"); return };
        match op {
            "" | "list" => match images::status(&home, Some(&self.cwd)) {
                Ok(rows) => { self.fe.info(&images::status_text(&rows)); self.fe.event("sandbox_images", &images::status_json(&rows, &home)); }
                Err(e) => { self.fe.error(&e); self.fe.event("sandbox_images", &json!({"images": [], "error": e})); }
            },
            "pull" => {
                let d = match plugins::Docker::find(&home) { Ok(d) => d, Err(e) => { self.fe.error(&e); return } };
                let present = |i: &images::Img| d.image_info(i.image).ok().flatten().is_some();
                match images::targets(rest, Some(&self.cwd), &present) {
                    Ok((v, _)) if v.is_empty() => self.fe.info("nothing to download: those images are on this machine already"),
                    Ok((v, again)) => {
                        let why = (again && v.len() == 1 && present(v[0]))
                            .then(|| format!("{} is on this machine already; downloading it again updates it. The {} uses", v[0].image, v[0].purpose()));
                        let _ = self.pull_images(&v, why);
                    }
                    Err(e) => self.fe.error(&e),
                }
            }
            _ => self.fe.error("usage: /sandbox [list] | pull NAME|IMAGE|stable|project|all"),
        }
    }

    /// Plugin servers that did not start because a listed image is missing: one question for those images, then
    /// the servers start again (interactive sessions only; elsewhere the startup note says how to get them).
    fn offer_plugin_images(&mut self) {
        if !self.interactive { return; }
        let down = self.mcp.down_for_image();
        if down.is_empty() { return; }
        let mut imgs: Vec<&'static images::Img> = Vec::new();
        for (_, i) in &down { if !imgs.contains(i) { imgs.push(i); } }
        let who = down.iter().map(|(c, _)| c.name.clone()).collect::<Vec<_>>().join(", ");
        let why = (imgs.len() == 1).then(|| format!("Plugin server {who} needs"));
        if self.pull_images(&imgs, why).is_err() { return; }
        let cfgs: Vec<crate::mcp_host::ServerCfg> = down.into_iter().map(|(c, _)| c).collect();
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        for (warn, n) in self.mcp.extend(&cfgs, &|n| own.iter().any(|o| o == n)) { if warn { self.fe.error(&n) } else { self.fe.info(&n) } }
        self.rebuild_system();
        self.plugins_event();
    }

    /// A plugin preview that lists missing images: shown, then one question to download them. true: they are
    /// there now (preview again).
    fn offer_preview_images(&mut self, op: &str, name: &str, p: &plugins::Preview) -> bool {
        if p.missing_images.is_empty() || !self.interactive { return false; }
        let imgs: Vec<&'static images::Img> = p.missing_images.iter().filter_map(|i| images::listed(i)).collect();
        if imgs.len() != p.missing_images.len() { return false; }
        self.fe.info(&p.text);
        self.fe.event("plugin_preview", &json!({"op": op, "name": name, "preview": p.json, "text": p.text}));
        let why = (imgs.len() == 1).then(|| format!("Plugin {name} needs"));
        self.pull_images(&imgs, why).is_ok()
    }

    /// `/plugin list | add PATH | remove NAME | enable NAME | disable NAME | update NAME | trust NAME|project`
    fn plugin_command(&mut self, arg: &str) {
        let (op, rest) = arg.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        let Some(home) = self.home.clone() else { self.fe.error("no home folder: plugins live in ~/.elisa/plugins"); return };
        let need = |s: &mut Self, what: &str| { s.fe.error(&format!("usage: /plugin {op} {what}")); };
        match op {
            "" | "list" => {
                let all = self.all_plugins();
                self.fe.info(&plugins::list_text(&all));
                self.plugins_event();
            }
            "add" => {
                if rest.is_empty() { return need(self, "PATH"); }
                let (m, src, p) = match plugins::prepare_add(&home, &self.cwd, rest) { Ok(x) => x, Err(e) => return self.plugin_result("add", rest, Err(e)) };
                let (m, src, p) = if self.offer_preview_images("add", &m.name, &p) {
                    match plugins::prepare_add(&home, &self.cwd, rest) { Ok(x) => x, Err(e) => return self.plugin_result("add", rest, Err(e)) }
                } else { (m, src, p) };
                if let Err(e) = self.plugin_setup("add", &m, &p) { return self.plugin_result("add", &m.name, Err(e)); }
                let fe = &mut *self.fe;
                let mut r = plugins::install(&home, &src, &m, &mut |l| fe.info(l));
                // the image went missing between the preview and the install: offer it once, then try again
                if let Some(img) = r.as_ref().err().and_then(|e| images::marked(e)) {
                    if self.pull_images(&[img], Some(format!("Plugin {} needs", m.name))).is_ok() {
                        let fe = &mut *self.fe;
                        r = plugins::install(&home, &src, &m, &mut |l| fe.info(l));
                    }
                }
                if let Err(e) = r { return self.plugin_result("add", &m.name, Err(e)); }
                let probe = self.plugin_probe(&m.name);
                self.reload_plugins(Some(&m.name));
                let mut t = format!("plugin {} {} installed and on", m.name, m.version);
                if !probe.is_empty() { t = format!("{probe}\n{t}"); }
                self.plugin_result("add", &m.name, Ok(t));
            }
            "remove" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let mine: Vec<String> = self.plugin_servers.iter().filter(|(p, _)| p == rest).map(|(_, s)| s.clone()).collect();
                if !mine.is_empty() { self.mcp.remove(&mine); }
                let r = plugins::remove(&home, rest).map(|notes| {
                    let mut t = format!("plugin {rest} removed");
                    for n in notes { t.push_str(&format!("\n{n}")); }
                    t
                });
                self.reload_plugins(None);
                self.plugin_result("remove", rest, r);
            }
            "enable" | "disable" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let r = plugins::set_enabled(&home, rest, op == "enable");
                if r.is_ok() { self.reload_plugins(Some(rest)); }
                self.plugin_result(op, rest, r);
            }
            "update" => {
                if rest.is_empty() { return need(self, "NAME"); }
                let (m, src, p) = match plugins::prepare_update(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("update", rest, Err(e)) };
                let (m, src, p) = if self.offer_preview_images("update", rest, &p) {
                    match plugins::prepare_update(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("update", rest, Err(e)) }
                } else { (m, src, p) };
                if let Err(e) = self.plugin_setup("update", &m, &p) { return self.plugin_result("update", rest, Err(e)); }
                let mine: Vec<String> = self.plugin_servers.iter().filter(|(pl, _)| pl == rest).map(|(_, s)| s.clone()).collect();
                if !mine.is_empty() { self.mcp.remove(&mine); }
                let fe = &mut *self.fe;
                let r = plugins::update(&home, &src, &m, &mut |l| fe.info(l));
                let probe = if r.is_ok() { self.plugin_probe(rest) } else { String::new() };
                self.reload_plugins(Some(rest));
                self.plugin_result("update", rest, r.map(|()| {
                    let t = format!("plugin {rest} updated to {}", m.version);
                    if probe.is_empty() { t } else { format!("{probe}\n{t}") }
                }));
            }
            "trust" if rest == "project" => {
                if self.proj_pending.is_none() { self.fe.info("this project has no commands or plugins waiting for trust"); return; }
                self.proj_declined = false;
                self.trust_project();
            }
            "trust" => {
                if rest.is_empty() { return need(self, "NAME | project"); }
                let (m, p) = match plugins::prepare_trust(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("trust", rest, Err(e)) };
                let (m, p) = if self.offer_preview_images("trust", rest, &p) {
                    match plugins::prepare_trust(&home, rest) { Ok(x) => x, Err(e) => return self.plugin_result("trust", rest, Err(e)) }
                } else { (m, p) };
                if let Err(e) = self.plugin_setup("trust", &m, &p) { return self.plugin_result("trust", rest, Err(e)); }
                let r = plugins::trust(&home, rest);
                if r.is_ok() { self.reload_plugins(Some(rest)); }
                self.plugin_result("trust", rest, r);
            }
            _ => self.fe.error("usage: /plugin list | add PATH | remove NAME | enable NAME | disable NAME | update NAME | trust NAME|project"),
        }
    }

    /// Returns false to quit.
    fn command(&mut self, line: &str) -> bool {
        let cmd = line.split_whitespace().next().unwrap_or("");
        match cmd {
            "/help" => { let h = self.help(); self.fe.info(&h) }
            "/plugin" | "/plugins" => self.plugin_command(line[cmd.len()..].trim()),
            "/sandbox" => self.sandbox_command(line[cmd.len()..].trim()),
            "/exit" | "/quit" => return false,
            "/clear" | "/compact" => {
                if self.history.is_empty() { self.fe.info("history is empty; notes unchanged"); return true; }
                // the text estimate, like the size after (a server count against an estimate would compare apples
                // with pears)
                let before = self.fixed() + estimate_messages(&self.history);
                // same progress job as the automatic fold ("summarizing …"): the status line shows it and input pauses
                let turns = self.history.iter().filter(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }))).count();
                let earlier = if self.notes.is_some() { " + earlier notes" } else { "" };
                let (label, t0) = (format!("summarizing {turns} turn{} ({}){earlier}", if turns == 1 { "" } else { "s" }, ui::k(before)), Instant::now());
                let prog = |frac: f32, done: Option<String>| Progress { label: label.clone(), frac: Some(frac), elapsed_s: t0.elapsed().as_secs_f64(), done };
                self.fe.progress(Some(&prog(0.0, None)));
                let r = self.summarise("[summarising…]");
                let done = match &r { Ok(_) => "saved summary".to_string(), Err(_) => "summary failed".to_string() };
                self.fe.progress(Some(&prog(1.0, Some(done))));
                match r {
                    Ok(n) => {
                        let old = self.notes.clone();
                        // merged into the earlier notes (the folds' summaries and cards stay), its claims checked
                        // and the files changed this session taken from Elisa's own records (soak: /clear replaced
                        // the notes with one turn's summary that said "No files were touched" after 4 edits)
                        let n = self.finish_notes(context::merge_notes(old.as_deref(), &self.verified(&n)));
                        self.mem_note(&n, usize::MAX);
                        // /clear is a fresh start: the notes become background and the card goes (the notes carry it)
                        let clear = cmd == "/clear";
                        if clear {
                            self.new_memory_session();
                            // the pre-clear conversation stays in its own session file (resumable)
                            context::new_session();
                            self.goal = None;
                            (self.card, self.card_id, self.card_ex) = (None, None, None);
                            self.ledger = context::Ledger::default();
                        }
                        self.card_log.clear();
                        self.history.clear();
                        self.set_notes(Some(n));
                        self.notes_bg = clear;
                        // /clear comes out smaller when only low-value lines need to go; /compact keeps the whole
                        // summary (it is the task's memory)
                        let after = if clear { self.shrink_notes(before, old.clone()) } else { self.estimate() };
                        if let Some(notes) = self.notes.clone() {
                            self.fe.notes_saved(&notes);
                            self.watch_note("fold", &notes);
                        }
                        // `compacted` carries the new context size: the status bars show it now, not at the next reply
                        self.fe.compacted(before, after);
                        if clear {
                            let kept = match &self.notes {
                                None => "no notes kept: the conversation was shorter than any summary",
                                Some(_) if after >= before => "notes saved to .elisa/notes.md, whole: the conversation was shorter than its facts, decisions and file changes",
                                Some(_) => "notes saved to .elisa/notes.md",
                            };
                            self.fe.info(&format!("[cleared: {} → {} tokens; {kept}]", ui::k(before), ui::k(after)));
                        }
                    }
                    Err(e) => self.fe.error(&format!("summary failed: {e}. Old notes kept, history kept (use /reset to wipe without a summary).")),
                }
            }
            "/reset" => {
                // a fresh start also drops the carried state card, or the next request still continues the old task
                self.history.clear(); self.real = None; self.fold_reset();
                self.drop_card();
                self.goal = None;
                self.notes_bg = true;
                self.fe.info("history and state card emptied (notes kept)");
            }
            "/context" => {
                let h = estimate_messages(&self.history);
                let msg = format!(
                    "system {} · tools {} · history {} ({} msgs) · total {} / limit {} (ctx {} - output {}) · fold at {}",
                    ui::k(self.system_tokens), ui::k(self.tools_tokens), ui::k(h), self.history.len(), ui::k(self.estimate()),
                    ui::k(self.limit()), ui::k(self.max_ctx), ui::k(self.reserve()), ui::k(self.fold_point()));
                if let Some(l) = self.ast_stats.line() { self.fe.info(&l); }
                self.fe.info(&msg);
                let by: Vec<String> = self.tool_costs().iter().map(|(n, t)| format!("{n} {}", ui::k(*t))).collect();
                self.fe.info(&format!("tools by owner (this turn): {} · /mcp for each server", by.join(" · ")));
                if self.cfg.gate_tools == "auto" && self.mcp.has_gated() {
                    self.fe.info(&format!("documents tools {} · security tools {} this turn (gate_tools = \"auto\": only on turns about them)",
                        if self.gate.0 { "offered" } else { "not offered" }, if self.gate.1 { "offered" } else { "not offered" }));
                }
                if self.mcp.reason_tool("think") {
                    self.fe.info(&format!("reasoning tools (think, steps, step_done, calc): {} this turn (reason = \"{}\")",
                        if self.reason_on { "offered" } else { "not offered" }, self.cfg.reason));
                }
                let probe = [Message::user_text(".")];
                let count = |m: &[Message]| self.client.count_tokens(&Request { system: &self.system, tools: &self.tools, messages: m, max_tokens: 1 });
                let msg = match (count(&probe), if self.history.is_empty() { Ok(0) } else { count(&self.history) }) {
                    (Ok(f), Ok(a)) => format!("server count: system+tools+empty turn {f} · whole prompt {a}"),
                    (Err(e), _) | (_, Err(e)) => format!("server count unavailable: {e}"),
                };
                self.fe.info(&msg);
            }
            "/state" => match (self.card.clone(), self.card_id) {
                (None, _) => self.fe.info("no state card yet"),
                (Some(c), None) => self.fe.info(&format!("state card (not saved: memory off):\n{c}")),
                (Some(_), Some(id)) => {
                    let chain = self.mem.as_ref().map(|m| m.db.card_chain(id, 3)).unwrap_or_default();
                    let s: Vec<String> = chain.iter().enumerate()
                        .map(|(i, (k, t))| format!("{} card #{k}:\n{t}", if i == 0 { "current" } else { "previous" })).collect();
                    self.fe.info(&s.join("\n\n"));
                }
            },
            "/trust" => {
                let name = line[cmd.len()..].trim();
                let changed_plugins: Vec<String> = self.home.clone().map(|h| self.all_plugins().into_iter()
                    .filter(|i| i.source == "user" && !i.trusted && plugins::changed(&h, &i.name)).map(|i| i.name).collect()).unwrap_or_default();
                if name.is_empty() {
                    let u = self.mcp.untrusted_servers();
                    self.fe.info(&if u.is_empty() { "every MCP server's tool list matches its pin (~/.elisa/mcp-pins.json)".to_string() }
                        else { format!("tool list changed since it was pinned: {} · /trust NAME accepts the new list", u.join(", ")) });
                    if !changed_plugins.is_empty() { self.fe.info(&format!("plugin files changed since you approved them: {} · /trust NAME reviews them", changed_plugins.join(", "))); }
                } else if changed_plugins.iter().any(|p| p == name) {
                    self.plugin_command(&format!("trust {name}"));
                } else {
                    match self.mcp.trust(name) {
                        Ok(m) => { self.audit.note("trust", &[("server", name), ("outcome", "ok")]); self.fe.info(&m); }
                        Err(e) => self.fe.error(&e),
                    }
                }
            }
            "/audit" => self.audit_command(line[cmd.len()..].trim()),
            "/mcp" => self.mcp_command(line[cmd.len()..].trim()),
            "/allow" => {
                if line[cmd.len()..].trim() == "clear" {
                    self.always.clear();
                    match self.store.save_allowed(&self.always) {
                        Ok(()) => self.fe.info("always-allow list cleared: every tool asks again"),
                        Err(e) => self.fe.error(&format!("could not clear .elisa/allow: {e}")),
                    }
                } else {
                    // MCP keys (`mcp:web`) read as "MCP server web"
                    let show = |k: &str| match k.strip_prefix(tools::MCP_KEY) { Some(srv) => format!("MCP server {srv}"), None => k.to_string() };
                    let mut v: Vec<String> = self.always.iter().map(|k| show(k)).collect();
                    v.sort_unstable();
                    let auto = self.cfg.auto_approve.iter().map(|k| show(k)).collect::<Vec<_>>().join(", ");
                    self.fe.info(&if v.is_empty() { format!("no tools always allowed in this project (never ask: {auto})") }
                        else { format!("always allowed in this project: {} (never ask: {auto}) · /allow clear to reset", v.join(", ")) });
                    let granted: Vec<String> = self.outside_ok.iter().map(|(p, w)| format!("{}{}", p.display(), if *w { " (read + write)" } else { " (read)" })).collect();
                    self.fe.info(&format!("outside the project folder: {} (config outside){}", self.outside_mode.word(),
                        if granted.is_empty() { String::new() } else { format!(" · allowed for this session: {}", granted.join(", ")) }));
                }
            }
            "/notes" => match self.notes.clone() {
                Some(n) => self.fe.info(&n),
                None => self.fe.info("no notes"),
            },
            "/forget" => {
                // the notes' "Facts to remember" were pinned into memory when the notes were saved: drop those too,
                // or the next request still carries them in [state]
                self.set_notes(None);
                self.drop_card();
                let n = self.mem.as_mut().map(|m| m.db.unpin_all(m.session, &m.project));
                match n {
                    Some(Ok(n)) => self.fe.info(&format!("notes deleted · {n} pinned facts unpinned")),
                    Some(Err(e)) => self.fe.error(&format!("notes deleted; unpinning facts failed: {e}")),
                    None => self.fe.info("notes deleted"),
                }
            }
            "/resume" => {
                let list = self.store.list_sessions();
                if list.is_empty() { self.fe.info("no saved session"); return true; }
                let Some(i) = self.fe.pick_session(&list).filter(|&i| i < list.len()) else { self.fe.info("resume cancelled"); return true; };
                match self.store.load_session(&list[i].path) {
                    Some(h) => {
                        self.fe.info(&format!("resumed \"{}\": {} messages restored (~{} tokens)", list[i].title, h.len(), ui::k(estimate_messages(&h))));
                        self.history = h;
                        self.real = None;
                        // back to that work: its card and notes are the task again
                        self.carry_card();
                        self.notes_bg = false;
                    }
                    None => self.fe.error(&format!("could not load {}", list[i].path.display())),
                }
            }
            "/pin" => {
                let fact = line[cmd.len()..].trim();
                if fact.is_empty() { self.fe.info("usage: /pin <fact>   e.g. /pin the release branch is trunk"); return true; }
                let r = self.mem.as_mut().map(|m| m.db.pin_fact(m.session, &crate::secrets::redact(fact).0));
                match r {
                    None => self.fe.error("memory is off: nothing to pin into"),
                    Some(Ok(id)) => { self.unindexed += 1; self.fe.info(&format!("pinned fact #{id}: {fact} (carried in [state] from the next fresh context)")); }
                    Some(Err(e)) => self.fe.error(&format!("memory: {e}")),
                }
            }
            "/watcher" => {
                let w = self.watch.fetch_now();
                self.watch_poll();
                self.fe.watcher(w.as_ref());
            }
            "/full" => self.full(),
            "/copy" => self.copy_last(),
            "/model" => self.model_command(),
            "/chat" | "/code" | "/mode" => self.fe.info("modes were removed: every request has the code prompt and all tools"),
            "/water" | "/feed" | "/food" | "/play" | "/toy" | "/poke" => self.fe.info("Elisa's character lives in the full-screen view (not in plain mode)"),
            "/stats" => {
                let file = self.store.dir.join(metrics::FILE);
                if self.cfg.metrics { self.fe.info(self.stats.text(&file).trim_end()); }
                else { self.fe.info("metrics are off (config `metrics = false`)"); }
            }
            "/yolo" => { self.yolo = !self.yolo; self.fe.info(if self.yolo { "approve-all ON" } else { "approve-all OFF" }); }
            _ => self.fe.error(&format!("unknown command {cmd} (try /help)")),
        }
        true
    }

    // ----- per-request metrics (src/metrics.rs) -----

    /// A user request starts: drop calls made outside a request (/clear, /compact summaries) and start the clock.
    fn req_begin(&mut self, prompt: &str) {
        // the user's own words: the session goal and `current request:` come from it
        self.user_req = context::one_line(prompt, 240);
        let goal = context::next_goal(self.goal.as_deref(), prompt);
        if goal != self.goal && self.goal.is_some() { self.fe.info(&format!("[session goal: {}]", goal.as_deref().unwrap_or(""))); }
        self.goal = goal;
        self.client.take_calls();
        if !self.cfg.metrics { return; }
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.req = Some(ReqRun { ts, t0: Instant::now(), prompt: prompt.to_string() });
    }

    /// The request finished (outcome: done | stopped | failed): one JSON line, and the `/stats` totals.
    fn req_end(&mut self, outcome: &str) {
        let calls = self.client.take_calls();
        let Some(r) = self.req.take() else { return };
        let m = metrics::RequestMetrics {
            ts: r.ts, prompt: r.prompt, outcome: outcome.into(), calls, wall_s: r.t0.elapsed().as_secs_f64(), ..Default::default()
        }.finish();
        if let Err(e) = metrics::append(&self.store.dir, &m) { self.fe.error(&format!("metrics not saved: {e}")); }
        self.stats.add(m);
    }

    /// After /clear (history empty, new notes set): the context should come out smaller than `before`. Notes
    /// longer than the conversation they replace (uitest: 2.5K → 2.9K after three short turns) lose their low-value
    /// lines (`context::shrink_for_clear`: new aliases, the model's file list and request quote, the excess of an
    /// overlong section), never a fact, a line of the earlier notes or Elisa's file records. When that is not enough
    /// the notes stay as they are and the context does not shrink (eval `context.clear_merges_notes`: a fact was cut
    /// so a tiny session could shrink). Returns the new context estimate.
    fn shrink_notes(&mut self, before: usize, old: Option<String>) -> usize {
        let (bg, after) = (self.notes_bg, self.estimate());
        if after < before { return after; }
        let n = self.notes.clone().unwrap_or_default();
        let over = after - before + 1 + before / 50;
        let cut = context::shrink_for_clear(&n, old.as_deref(), estimate_tokens(&n).saturating_sub(over));
        if !cut.is_empty() && estimate_tokens(&cut) < estimate_tokens(&n) {
            self.set_notes(Some(cut));
            self.notes_bg = bg;
        }
        self.estimate()
    }

    fn set_notes(&mut self, n: Option<String>) { self.put_notes(n, true); }

    /// The notes in use from now on, the session goal on top; `save`: also to `.elisa/notes.md` (a fold tries them
    /// first and saves only the version it keeps).
    fn put_notes(&mut self, n: Option<String>, save: bool) {
        self.notes_bg = false;
        // the session goal rides on top of the notes (a fold summary's `## Goal` is the newest request)
        let n = match (n, self.goal.as_deref()) { (Some(t), Some(g)) => Some(context::with_session_goal(&t, g)), (n, _) => n };
        if save {
            let r = match &n { Some(t) => self.store.save_notes(t), None => self.store.clear_notes() };
            if let Err(e) = r { self.fe.error(&format!("notes: {e}")); }
        }
        self.notes = n;
        self.rebuild_system();
    }

    /// New notes text (a summary, a digest, state cards) with its "done / changed" claims checked against Elisa's own
    /// edit records (`context::verify_claims`): a claim naming code no edit wrote is dropped.
    fn verified(&self, new: &str) -> String {
        let cwd = self.cwd.clone();
        context::verify_claims(new, &self.ledger, &move |p: &str| std::fs::read_to_string(cwd.join(p)).ok())
    }

    /// Merged notes as they are kept: Elisa's own list of the files changed this session merged in (ground truth),
    /// duplicates gone, cut by priority to the notes' share of the window (`context::notes_budget`).
    fn finish_notes(&self, merged: String) -> String {
        let merged = match self.ledger.section() { Some(sec) => context::merge_notes(Some(&merged), &sec), None => merged };
        context::cap_notes_priority(&merged, self.notes_budget())
    }

    /// The prompt size where a between-turns fold starts (`context::fold_point`: the fixed cost plus fold_at of the
    /// room left after it).
    fn fold_point(&self) -> usize { context::fold_point(self.fixed(), self.limit(), self.cfg.fold_at) }
    fn notes_budget(&self) -> usize { context::notes_budget(self.fixed(), self.limit()) }
    fn min_gain(&self) -> usize { context::min_fold_gain(self.fixed(), self.limit()) }

    /// What a between-turns fold could free at most (tokens): the history before the last request and the shrinkable
    /// part of the kept exchange, minus what the notes would grow by (up to their share of the window).
    fn fold_gain(&self) -> usize {
        let mut h = self.history.clone();
        let last = h.iter().rposition(is_request).unwrap_or(0);
        let now = estimate_messages(&h);
        context::shrink_all_results(&mut h);
        let kept = estimate_messages(&h[last..]);
        if last == 0 { return now.saturating_sub(kept); }
        let notes_now = self.notes.as_deref().map_or(0, estimate_tokens);
        let notes_after = (notes_now + COMPACT_OUT / 2).min(self.notes_budget().max(notes_now));
        let header = estimate_tokens(&self.first(""));
        now.saturating_sub(kept + header + notes_after - notes_now)
    }

    /// Summary of the whole history (plus old notes), as new notes. First in context: same system + tools + history
    /// and one more user message, so the gateway reuses its KV (prefix hit, ~1 s instead of a full re-read). Falls
    /// back to a plain transcript (no tools, small system) when that does not fit or the model calls a tool.
    fn summarise(&mut self, label: &str) -> Result<String, String> {
        let out = COMPACT_OUT.min(self.cfg.max_output);
        self.client.set_kind("summary");
        self.fe.info(label);
        CANCEL.store(false, Ordering::SeqCst);
        if self.estimate() + estimate_tokens(context::compact_instructions()) + out + 64 <= self.max_ctx {
            let mut msgs = self.history.clone();
            msgs.push(Message::user_text(context::compact_instructions()));
            let req = Request { system: &self.system, tools: &self.tools, messages: &msgs, max_tokens: out };
            self.fe.waiting();
            let r = self.client.send(&req, &CANCEL, &mut |_| {})?;
            if r.stop_reason == "cancelled" { return Err("cancelled".into()); }
            let tool = r.content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
            let n = summary_text(r.content);
            if !tool && !n.is_empty() { return Ok(n); }
            self.fe.info("[in-context summary unusable; retrying from a transcript]");
        }
        let budget = self.max_ctx.saturating_sub(out + self.client.reasoning_room() + 64 + estimate_tokens(COMPACT_SYSTEM)) * 85 / 100;
        // the earlier notes are merged by Elisa afterwards (`context::merge_notes`), not left to the model
        let msgs = [context::compact_request(&self.history, None, budget)];
        let req = Request { system: COMPACT_SYSTEM, tools: &[], messages: &msgs, max_tokens: out };
        self.fe.waiting();
        let r = self.client.send(&req, &CANCEL, &mut |_| {})?;
        if r.stop_reason == "cancelled" { return Err("cancelled".into()); }
        let n = summary_text(r.content);
        if n.is_empty() { Err("empty summary".into()) } else { Ok(n) }
    }

    /// The turn-boundary fold (after a reply, the cheap place): when the window reaches the fold point (the fixed
    /// cost plus fold_at of the room left after it, `context::fold_point`; real usage when known), or after
    /// fold_max_turns turns. Never when it could not free a meaningful share of the room (`fold_gain` under
    /// `min_gain`: soak 2026-11-08 folded before almost every turn at 8K, some folds growing the prompt). After a
    /// failed or skipped fold, wait until the window grows by that share again.
    fn maybe_fold(&mut self) {
        if self.history.len() < 2 { return; } // only the request: nothing to fold
        let est = self.estimate();
        let by_tokens = est >= self.fold_point().max(self.fold_retry_at);
        let by_turns = self.cfg.fold_max_turns > 0 && self.turn_no - self.folded >= self.cfg.fold_max_turns && !self.in_turn;
        if !(by_tokens || by_turns) { return; }
        if !by_turns && self.fold_gain() < self.min_gain() { self.fold_retry_at = est + self.min_gain(); return; }
        self.fold(by_turns && !by_tokens);
    }

    /// Before each request of a turn. Within a turn the history is append-only (a sent message never changes: the
    /// gateway's DeltaNet + KV state cannot rewind, so any rewrite re-prefills everything after system + tools; run2
    /// turn 1 lost ~430 s to 8 such rewrites). Only when the output reserve is gone (the request is over the limit)
    /// does the one rewrite a turn may make run: a fold (earlier turns, or this turn's older results when it alone is
    /// over). Returns whether the request fits max_ctx with MIN_OUT of output after the safety margin; when it does
    /// not, the caller stops the turn instead of sending a request the server would refuse (run2 call 30).
    fn mid_turn_fold(&mut self) -> bool {
        if !self.folded_this_turn && self.history.len() >= 2 && self.est_safe() > self.limit() {
            self.folded_this_turn = true;
            self.fold(false);
        }
        context::request_fits(self.est_safe(), MIN_OUT + 64, self.max_ctx)
    }

    /// One step's tool results against what the window has left (the limit minus the request so far): over it, the
    /// NEW results are cut before they are appended (largest first, `[trimmed N chars]`), so the next request never
    /// goes over the limit (ui_run1: four big JSON reads made it 10.1K tokens against 8192).
    ///
    /// Append-only within a turn: older messages are never rewritten to make room (run2 calls 18–25: each step shrank
    /// one earlier result to `[trimmed: … N lines]`, often longer than the original, and each call re-prefilled
    /// 4.6–6.2K tokens, 52–71 s). Only when even the cut-to-minimum results do not fit does the turn's one fold run.
    fn fit_step_results(&mut self, results: &mut [Block]) {
        let need = estimate_messages(&[Message::new(Role::User, results.to_vec())]);
        let tail = self.limit() / STEP_TAIL;
        // what this step may take: all of the room for small results, else the room above the tail (at least a
        // quarter of the tail, so a big read inside the tail still gets its head)
        let allowed = |s: &Self| {
            let room = s.limit().saturating_sub(s.est_safe() + MSG_OVERHEAD_TOKENS);
            if need <= tail / 4 { room } else { room.saturating_sub(tail).max(room.min(tail / 4)) }
        };
        if need <= allowed(self) { return; }
        // every result keeps a useful slice (head + tail, RESULT_MIN_CHARS): older history makes room first, never
        // a result cut to nothing (soak 2026-11-08: 47 results "trimmed to fit (0 tokens left)" were re-requested or
        // answered with invented numbers)
        let min = context::min_step_tokens(results);
        if allowed(self) < min && self.history.len() >= 2 && !self.folded_this_turn {
            self.folded_this_turn = true;
            self.fold(false);
        }
        // when the room is under the minimum slice, the reply's output gives way first (down to MIN_OUT)
        let hard = |s: &Self| s.max_ctx.saturating_sub(s.est_safe() + context::ctx_margin(s.max_ctx) + MIN_OUT + 64 + MSG_OVERHEAD_TOKENS);
        if allowed(self) < min && hard(self) < min && self.history.len() >= 2 {
            // still short: this turn's earlier results become one line each (a rewrite, but the new result is what
            // the next step needs)
            let cap = self.turn_cap(&self.history).min(self.history.len());
            let before = self.estimate();
            let mut h = self.history.clone();
            context::shrink_all_results(&mut h[cap..]);
            if estimate_messages(&h) < estimate_messages(&self.history) { self.history = h; self.real = None; self.shrunk(before); }
        }
        let room = match allowed(self) { r if r >= min => r, _ => hard(self).min(min) };
        if room < min {
            // not even the minimum slice fits: said plainly instead of a stub the model would fill in
            let mut n = 0;
            for b in results.iter_mut() {
                if let Block::ToolResult { content, .. } = b {
                    if content.chars().count() > context::RESULT_MIN_CHARS / 4 { *content = context::not_shown(content); n += 1; }
                }
            }
            if n > 0 { self.fe.info(&format!("[{n} tool result{} not shown: too big for the context even after a fold; the model was told so]", if n == 1 { "" } else { "s" })); }
            return;
        }
        let n = context::cap_step_results(results, room);
        if n > 0 { self.fe.info(&format!("[{n} tool result{} trimmed to fit the context (head and tail kept, ~{} tokens)]", if n == 1 { "" } else { "s" }, ui::k(room))); }
    }

    /// After a fold: when the kept current turn is still over the limit, its newest tool results are cut to fit, so
    /// the request after a fold never goes over it either (ui_run1: the fold kept the 25 KB reads and overflowed again).
    fn fit_newest_results(&mut self) {
        if self.est_safe() <= self.limit() { return; }
        let Some(i) = self.history.iter().rposition(|m| m.content.iter().any(|b| matches!(b, Block::ToolResult { .. }))) else { return };
        let mine = estimate_messages(&self.history[i..=i]);
        let room = self.limit().saturating_sub(self.est_safe().saturating_sub(mine) + MSG_OVERHEAD_TOKENS);
        let before = self.est_safe();
        if context::cap_step_results(&mut self.history[i].content, room) > 0 { self.real = None; self.shrunk(before); }
    }

    /// Index of the current user request in `msgs` while a turn runs (the fold keeps it and all after it);
    /// `msgs.len()` between turns.
    fn turn_cap(&self, msgs: &[Message]) -> usize {
        if !self.in_turn { return msgs.len(); }
        // Elisa's own correction message is not the request
        msgs.iter().rposition(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.starts_with(context::NUDGE_PREFIX)))).unwrap_or(0)
    }

    /// Turn start: are the reasoning tools in this turn's requests? A cheap check of the user's words, no model call
    /// (`mcp_reason::decide`; a follow-up like "continue" keeps the last choice). The tool list changes only when the
    /// choice flips, since a different list re-reads the whole prompt (the tools sit before the history in the
    /// template, so the server's prefix cache is lost once). The server's per-turn caps start again.
    fn reason_turn(&mut self, input: &str) {
        let mode = crate::mcp_reason::Mode::parse(&self.cfg.reason).unwrap_or(crate::mcp_reason::Mode::Auto);
        let body = context::request_body(input);
        let on = crate::mcp_reason::decide(mode, body, self.reason_on);
        // the same check, whatever `reason` is: a simple turn ("hi", "2+2=?") asks the model not to reason and gets a
        // smaller output cap (config `think_simple`; a follow-up like "ok" keeps the last turn's choice)
        let simple = crate::mcp_reason::wanted(body).map_or(self.client.simple(), |multi| !multi);
        self.client.set_simple(simple && self.cfg.think_simple == "off");
        self.mcp.new_turn();
        // the documents and security servers: only on turns about them (config `gate_tools`), same once-per-turn rule
        let gate = crate::mcp_reason::gate(self.cfg.gate_tools == "off", body, &self.recent_text(), self.gate);
        if on != self.reason_on || gate != self.gate {
            (self.reason_on, self.gate) = (on, gate);
            self.rebuild_system();
        }
    }

    /// Text of the recent context (the last few messages: words, tool calls and the head of each result) for the
    /// relevance gate: a document listed or read there keeps the documents tools on.
    fn recent_text(&self) -> String {
        let mut s = String::new();
        for m in self.history.iter().rev().take(RECENT_MSGS) {
            for b in &m.content {
                match b {
                    Block::Text { text } => s.push_str(context::request_body(text)),
                    Block::ToolUse { input, .. } => s.push_str(&input.to_string()),
                    Block::ToolResult { content, .. } => s.extend(content.chars().take(2000)),
                    Block::Image { .. } => {}
                }
                s.push('\n');
            }
        }
        s
    }

    /// One user turn; records the user text and the final assistant reply in memory.
    fn turn(&mut self, input: String) -> TurnEnd {
        if self.cfg.model.trim().is_empty() { self.unsent = true; return TurnEnd::Failed(crate::api::NO_MODEL.into()); }
        // a secret the user typed is hidden wherever it shows up later on disk, the model's restatement included
        crate::secrets::learn(&input);
        self.reason_turn(&input);
        self.tool_lines.clear();
        self.unsent = false;
        (self.new_card, self.card_broken) = (None, false);
        self.in_turn = true;
        self.folded_this_turn = false;
        let end = self.turn_inner(input.clone());
        self.in_turn = false;
        if !self.unsent {
            let mut reply = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default().trim().to_string();
            for l in std::mem::take(&mut self.tool_lines) { reply += &format!("\n{l}"); }
            let ids: Vec<u64> = [self.record(MRole::User, &input), self.record(MRole::Assistant, reply.trim())].into_iter().flatten().collect();
            self.exchanges += 1;
            match self.new_card.take() {
                Some(c) => self.set_card(c, &ids),
                None if self.card_broken && matches!(end, TurnEnd::Done) => {
                    let keep = self.card_id.map_or("none yet".to_string(), |k| format!("keeping card #{k}"));
                    self.fe.info(&format!("[no state card in this reply; {keep}]"));
                }
                None => {}
            }
        }
        end
    }

    fn turn_inner(&mut self, input: String) -> TurnEnd {
        // recalled memories go in the USER message (the system prompt stays byte-stable); only `input` is recorded
        let lines = self.recall(&input);
        if !lines.is_empty() { self.fe.recalled(&lines); }

        let text = if self.history.is_empty() {
            context::first_message_full(&self.cwd, self.first_notes().as_deref(), self.state_block().as_deref(), &lines, &input)
        } else {
            context::with_memory(&lines, &input)
        };
        self.input = input;
        let mut content = vec![Block::Text { text }];
        content.append(&mut self.attach);
        self.history.push(Message::new(Role::User, content));
        self.agent_loop(CheckRun::default(), true)
    }

    /// The agent loop of a turn: the history ends with a user message. `first` = that message is the turn's first
    /// (a failed send drops it again).
    fn agent_loop(&mut self, mut checks: CheckRun, mut first: bool) -> TurnEnd {
        // one correction per turn when a reply writes a tool call as text
        let mut nudged = false;
        let mut repeat = Repeat::default();
        // repeated identical calls, short cycles, steps without progress (`loop_guard.rs`)
        let mut guard = crate::loop_guard::Guard::default();
        // a reply with no visible text (only its state card): asked once more, with this nudge after the history for
        // that one request (it never enters the history, so the next requests' prefix is unchanged)
        let mut empty_retry: Option<Message> = None;
        let mut retried = false;
        let mut stashed_card: Option<String> = None;
        for _ in 0..self.cfg.max_steps.max(1) {
            if !self.mid_turn_fold() {
                // run2 call 30: sent anyway, 8,199 tokens against 8,192, and the server returned nothing
                self.history.push(Message::assistant_text("[stopped: context full]"));
                self.fe.info(&format!("[stopped: the next request (~{} tokens) would not fit the {}-token context; say \"continue\" to go on after a fold]", ui::k(self.est_safe()), ui::k(self.max_ctx)));
                return TurnEnd::Stopped;
            }
            CANCEL.store(false, Ordering::SeqCst);
            let max_tokens = self.cfg.max_output.min(self.max_ctx.saturating_sub(self.est_safe() + context::ctx_margin(self.max_ctx) + 64)).max(MIN_OUT);
            // a simple turn where the model does not reason: a smaller cap (`Client::simple_cap`)
            let max_tokens = self.client.simple_cap().map_or(max_tokens, |c| max_tokens.min(c));
            let interactive = self.interactive;
            self.fe.waiting();
            let fe = &mut *self.fe;
            let mut on_text = |t: &str| if interactive { fe.text_delta(t); };
            let retry_msgs: Vec<Message>;
            let retrying = empty_retry.is_some();
            let messages: &[Message] = match empty_retry.take() {
                Some(n) => { retry_msgs = self.history.iter().cloned().chain([n]).collect(); &retry_msgs }
                None => &self.history,
            };
            let req = Request { system: &self.system, tools: &self.tools, messages, max_tokens };
            self.client.set_kind("agent");
            let res = self.client.send(&req, &CANCEL, &mut on_text);
            let mut reply = match res {
                Ok(r) => r,
                Err(e) => {
                    if first { self.history.pop(); self.unsent = true; } else { self.history.push(Message::assistant_text("[request failed]")); }
                    return TurnEnd::Failed(api_error_line(&e));
                }
            };
            first = false;
            let prompt_real = reply.usage.input_tokens + reply.usage.cache_read_input_tokens;
            // a cancelled (Esc) or unreported reply has no usage: the partial output's estimate stands in for it
            if prompt_real == 0 && reply.usage.output_tokens == 0 {
                reply.usage.output_tokens = estimate_messages(&[Message::new(Role::Assistant, reply.content.clone())]).saturating_sub(MSG_OVERHEAD_TOKENS);
            }
            // Anthropic usage: input_tokens excludes the cached (reused) part. Without it the context is the estimate
            // of the request just sent plus that output (uitest: ctx_used 0 after Esc reset the app's ctx meter)
            let ctx_used = if prompt_real > 0 {
                prompt_real + reply.usage.output_tokens.saturating_sub(reply.usage.reasoning_tokens)
            } else {
                self.estimate() + reply.usage.output_tokens
            };
            if prompt_real > 0 && !retrying { self.calib = Some((prompt_real, self.fixed() + estimate_messages(&self.history))); }
            self.fe.reply_done(&ui::status(&reply, ctx_used, self.max_ctx));
            let Reply { content, stop_reason, id, usage, .. } = reply;
            let reasoning = usage.reasoning_tokens;
            let id = Some(id).filter(|i| !i.is_empty());
            let has_tools = content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
            let run_tools = has_tools && stop_reason != "cancelled" && stop_reason != "max_tokens";
            // a tool call cut by the output limit does not run (mario.html: a cut write was saved as finished)
            let cut = (has_tools && stop_reason == "max_tokens").then(|| content.iter().filter_map(|b| match b {
                Block::ToolUse { name, input, .. } => Some(input["path"].as_str().map_or(name.clone(), |p| format!("{name} {p}"))),
                _ => None,
            }).collect::<Vec<_>>().join(", "));
            if !run_tools {
                // the state card never enters history (append-only, prefix stays valid): only the visible text is kept
                let (vis, card, broken) = context::split_state(&texts(&Message { role: Role::Assistant, content, ..Default::default() }));
                let card = card.or_else(|| stashed_card.take());
                (self.new_card, self.card_broken) = (card, broken);
                // the reply showed words (not only the "[max_tokens]" placeholder below)
                let wrote = !vis.trim().is_empty();
                // nothing for the user to read (QA N3: qwen3-vl:2b after the no-think prefill answered "thanks" with
                // only a state card): ask once more, any provider, then say so rather than show nothing
                let normal_end = cut.is_none() && stop_reason != "cancelled" && stop_reason != "max_tokens";
                if !wrote && normal_end && !retried {
                    retried = true;
                    stashed_card = self.new_card.take();
                    empty_retry = Some(Message::user_text(EMPTY_REPLY_NUDGE));
                    self.fe.info("[the reply had no text for you, only its state card; asking the model once more]");
                    continue;
                }
                if !wrote && normal_end {
                    self.fe.info("[no answer: the model wrote no text for you twice; ask again or rephrase]");
                }
                let mut content: Vec<Block> = if vis.is_empty() { Vec::new() } else { vec![Block::Text { text: vis }] };
                if let Some(c) = &cut { content.push(Block::Text { text: format!("[cut off at the {max_tokens}-token output limit: `{c}` did not run]") }); }
                if content.is_empty() {
                    content.push(Block::Text { text: format!("[{}]", if stop_reason.is_empty() { "no output" } else { &stop_reason }) });
                }
                // a call cut by the limit: tell the model why, once, so it can use smaller pieces
                if let (Some(c), false) = (&cut, nudged) {
                    nudged = true;
                    (self.new_card, self.card_broken) = (None, false);
                    self.history.push(Message { role: Role::Assistant, content, id });
                    self.history.push(Message::user_text(context::cut_call_nudge(c, max_tokens)));
                    self.real = Some((self.history.len() - 1, ctx_used));
                    self.fe.info(&format!("[cut off: `{c}` hit the {max_tokens}-token output limit, so it did not run and nothing was saved; asking the model once to use smaller pieces]"));
                    continue;
                }
                // a tool call written as text ran nothing: ask once for a real call (the reply stays in history,
                // append-only, followed by the correction)
                let text_call = !has_tools && !nudged && stop_reason != "cancelled" && context::looks_like_text_tool_call(&texts(&Message::new(Role::Assistant, content.clone())));
                if text_call {
                    nudged = true;
                    (self.new_card, self.card_broken) = (None, false);
                    self.history.push(Message { role: Role::Assistant, content, id });
                    let names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
                    self.history.push(Message::user_text(context::tool_text_nudge(&names)));
                    self.real = Some((self.history.len() - 1, ctx_used));
                    self.fe.info("[the reply wrote a tool call as text, so nothing ran; asking the model once to call it properly]");
                    continue;
                }
                self.history.push(Message { role: Role::Assistant, content, id: id.clone() });
                // the gateway audits every reply: its audit line shows once it finishes
                if let Some(i) = &id { self.watch.mine(i); }
                self.real = Some((self.history.len(), ctx_used));
                return match stop_reason.as_str() {
                    "cancelled" => { self.fe.info("[cancelled]"); TurnEnd::Stopped }
                    "max_tokens" => {
                        match &cut {
                            Some(c) => self.fe.info(&format!("[stopped: `{c}` was cut off at the {max_tokens}-token output limit, so it did not run and nothing was saved; say \"continue\" to go on]")),
                            None if !wrote && reasoning > 0 => self.fe.info(&format!("[stopped: the model spent its whole output budget ({reasoning} tokens) reasoning and wrote no answer; ask again, or more simply]")),
                            None => self.fe.info("[stopped: reply hit max_tokens; say \"continue\" to go on]"),
                        }
                        TurnEnd::Stopped
                    }
                    _ => match self.check_gate(&mut checks) {
                        Gate::Fix => continue,
                        Gate::End(e) => e,
                    },
                };
            }
            let calls: Vec<(String, String, serde_json::Value)> = content.iter().filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            }).collect();
            self.history.push(Message { role: Role::Assistant, content, id });
            self.real = Some((self.history.len(), ctx_used));
            let mut results = Vec::with_capacity(calls.len());
            for (id, name, input) in &calls {
                let (content, is_error) = if CANCEL.load(Ordering::SeqCst) {
                    ("cancelled by user".to_string(), true)
                } else {
                    let summary = tools::summary(name, input);
                    // only a tool this request offered may run (Elisa's own + the MCP servers', as the reasoning
                    // gate filtered them): anything else is refused before any approval box (security s1: an
                    // approval for `sec_malware`, which this build does not have)
                    let offered = self.tools.iter().any(|t| t.name == *name);
                    let verdict = guard.check(name, input, !self.mcp.owns(name));
                    self.fe.tool_start(&summary);
                    let (content, is_error) = if !offered && self.mcp.gated_server(name).is_some() {
                        // a documents / security tool the relevance gate left out (the model saw a PDF in a listing
                        // mid-turn): not run now, but offered from the next request on (one tool-list change)
                        let srv = self.mcp.gated_server(name).unwrap_or("");
                        if srv == "docs" { self.gate.0 = true } else { self.gate.1 = true }
                        self.rebuild_system();
                        let why = format!("`{name}` was not offered on this request (the {srv} tools come only when the request is about them); it is offered from the next request on: call it again if you need it.");
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "gated_tool")]);
                        self.fe.tool_done(&why, true);
                        (why, true)
                    } else if !offered {
                        let why = unknown_tool_text(name, &self.tools, self.mcp.reason_tool(name));
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "unknown_tool")]);
                        self.fe.tool_done(&why, true);
                        (why, true)
                    } else if let crate::loop_guard::Verdict::Block(why) = verdict {
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "repeated_call")]);
                        self.fe.tool_done(&why, true);
                        (why, true)
                    } else {
                        self.fe.tool_detail(name, input, &self.cwd);
                        self.answer = None;
                        match self.approve(name, input, &summary) {
                            Err(why) => {
                                let answer = self.answer.take().unwrap_or("policy");
                                self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", answer), ("detail", &why)]);
                                self.fe.tool_done(&why, true);
                                (why, true)
                            }
                            Ok(()) => {
                                let answer = self.answer.take().unwrap_or("auto");
                                self.audit_tool("approval", name, &summary, &[("answer", answer)]);
                                let write = tools::access(name) == Access::Write;
                                // the file before the change: which symbols it touched narrows the test check
                                let before = if write && self.cfg.ast_targeted_checks { checks::before(&self.cwd, input) } else { None };
                                if write && !self.mcp.owns(name) { self.ledger.before(input, &self.cwd); }
                                let mut o = if self.mcp.owns(name) {
                                    self.mcp.call(name, input, &CANCEL, self.cfg.tool_output_max_chars)
                                } else { tools::run(name, input, &self.tctx) };
                                // the sandbox's image is not on this machine (a structured marker, not the words):
                                // Elisa, not the model, asks the user; on yes it downloads it and runs the call once more
                                // without the download, a present line that still fits what the project asks for runs it
                                if let Some((img, fallback)) = self.mcp.take_missing_image().and_then(|(i, f)| images::listed(&i).map(|i| (i, f))) {
                                    let pulled = if fallback.is_some() && self.declined_images.contains(&img.image) { false } else {
                                        let r = self.pull_images(&[img], Some(images::needs(img)));
                                        if matches!(&r, Err(e) if e == "declined") && !self.declined_images.contains(&img.image) { self.declined_images.push(img.image); }
                                        r.is_ok()
                                    };
                                    if pulled {
                                        o = self.mcp.call(name, input, &CANCEL, self.cfg.tool_output_max_chars);
                                        let _ = self.mcp.take_missing_image();
                                    } else if let Some(v) = fallback {
                                        let mut again = input.clone();
                                        again["version"] = json!(v);
                                        o = self.mcp.call(name, &again, &CANCEL, self.cfg.tool_output_max_chars);
                                        let _ = self.mcp.take_missing_image();
                                        o.content.push_str(&format!("\nnote: {} is not on this Mac (not downloaded); ran on {} {v}, the newest line here that fits what was asked", img.image, images::display(img.lang)));
                                    }
                                }
                                if !o.is_error && write { checks.edited(&self.cwd, input, before); }
                                // Elisa's own record of what changed (the notes' file list; checks a summary's claims)
                                if !o.is_error && !self.mcp.owns(name) { self.ledger.record(name, input, &self.cwd); }
                                // an edit whose old and new text are the same changed nothing (chat s3b looped on one)
                                let changed = name != "edit" || input.get("old") != input.get("new");
                                guard.ran(name, input, write, !o.is_error, changed);
                                if !self.mcp.owns(name) { guard.result(name, input, &o.content, o.is_error); }
                                if !o.is_error { self.ast_stats.see(name, input, &o.content, &self.tctx); }
                                self.audit_tool("tool_call", name, &summary, &[("outcome", if o.is_error { "error" } else { "ok" })]);
                                // a restarted MCP server that listed other tools than its pin (`pins.rs`)
                                for n in self.mcp.take_notes() { self.fe.error(&n); }
                                self.fe.tool_done(&o.content, o.is_error);
                                (self.guard_output(name, input, o.content), o.is_error)
                            }
                        }
                    };
                    self.tool_lines.push(tool_line(&summary, &content, is_error));
                    (content, is_error)
                };
                let n = repeat.see(name, input, &content, is_error);
                let content = if n >= REPEAT_MAX { format!("{content}\n{}", repeat_note(n)) } else { content };
                results.push(Block::ToolResult { tool_use_id: id.clone(), content, is_error });
            }
            // the loop guard's word on this step: a note on its last result, or the turn's stop (below)
            let guard_stop = match guard.step_done() {
                crate::loop_guard::After::Fine => None,
                crate::loop_guard::After::Note(n) => {
                    if let Some(Block::ToolResult { content, .. }) = results.last_mut() { content.push('\n'); content.push_str(&n); }
                    self.fe.info(&format!("[loop guard: {}]", n.trim_start_matches("[elisa: ").trim_end_matches(']')));
                    None
                }
                crate::loop_guard::After::Stop(why) => {
                    if let Some(Block::ToolResult { content, .. }) = results.last_mut() {
                        content.push_str("\n[elisa: the turn was stopped because the same calls kept coming back. Do not repeat them; change the approach or report what blocks you.]");
                    }
                    Some(why)
                }
            };
            self.fit_step_results(&mut results);
            // after the fit: what the model really got is what makes a read small (the cut comes after `see`)
            for ((_, name, input), r) in calls.iter().zip(&results) {
                if let Block::ToolResult { content, is_error, .. } = r { repeat.page(name, input, content, *is_error); }
            }
            self.history.push(Message::new(Role::User, results));
            if CANCEL.load(Ordering::SeqCst) {
                self.history.push(Message::assistant_text("[cancelled]"));
                self.fe.info("[cancelled]");
                return TurnEnd::Stopped;
            }
            if repeat.max >= REPEAT_MAX {
                // the same call and the same result again: the loop is not getting anywhere (Test 2026-11-01 ran one
                // failing command ~15 times until the context was full). Stop; the note above tells the model why.
                let why = repeat.why();
                self.history.push(Message::assistant_text(format!("[stopped: {why}]")));
                self.fe.info(&format!("[stopped: {why}; say how to proceed]"));
                return TurnEnd::Stopped;
            }
            if let Some(why) = guard_stop {
                self.history.push(Message::assistant_text(format!("[stopped: {why}]")));
                self.fe.info(&format!("[stopped: {why}; say how to proceed]"));
                return TurnEnd::Stopped;
            }
        }
        // History ends with tool results; close it so the next user text keeps roles alternating.
        self.history.push(Message::assistant_text("[step limit reached]"));
        self.fe.info(&format!("[stopped after {} steps; say \"continue\" to go on]", self.cfg.max_steps));
        TurnEnd::Stopped
    }

    // ----- check before done (src/checks.rs) -----

    /// The model is about to finish a turn. If it edited files since the last check, run the project's checks (fast
    /// first, then tests); on a failure append the trimmed errors as a user-side note (history stays append-only) and
    /// let it fix, up to `rounds` times. Every step shows as a `check` event; it stops on the same errors twice.
    fn check_gate(&mut self, cr: &mut CheckRun) -> Gate {
        if !cr.dirty {
            // a fix round ended without editing: the failure stands, say so (never silent)
            if let Some((name, summary)) = cr.failing.take() {
                self.check_ev(&name, CheckStatus::Stopped, cr.round, cr.rounds, format!("{summary} · no edit in the fix round"));
            }
            return Gate::End(TurnEnd::Done);
        }
        cr.dirty = false;
        let set = match checks::select_with(&self.cfg, &self.cwd, &cr.files, &cr.touches, self.checks_off) {
            Ok(p) => p,
            Err(e) => { self.fe.error(&format!("checks: {e}")); return Gate::End(TurnEnd::Done); }
        };
        cr.rounds = set.rounds;
        for c in &set.checks {
            self.check_ev(&c.name, CheckStatus::Running, cr.round, set.rounds, String::new());
            let o = checks::run(c, &self.cwd, self.cfg.check_output_max_chars);
            if o.cancelled || CANCEL.load(Ordering::SeqCst) {
                self.check_ev(&c.name, CheckStatus::Stopped, cr.round, set.rounds, "cancelled".into());
                self.fe.info("[cancelled]");
                return Gate::End(TurnEnd::Stopped);
            }
            match o.status {
                // a pass whose writes the shell sandbox blocked carries a note (`checks::SANDBOX_NOTE`)
                CheckStatus::Pass => self.check_ev(&c.name, CheckStatus::Pass, cr.round, set.rounds, o.summary.clone()),
                CheckStatus::Timeout => {
                    let rest = if c.stage == checks::Stage::Test { "skipped (slow tests)" } else { "remaining checks skipped" };
                    self.check_ev(&c.name, CheckStatus::Timeout, cr.round, set.rounds, format!("{} · {rest}", o.summary));
                    cr.failing = None;
                    return Gate::End(TurnEnd::Done);
                }
                _ => {
                    if cr.sig.as_deref() == Some(o.signature.as_str()) {
                        self.check_ev(&c.name, CheckStatus::Stopped, cr.round, set.rounds, format!("{} · same errors again", o.summary));
                        cr.failing = None;
                        return Gate::End(TurnEnd::Done);
                    }
                    if cr.round >= set.rounds {
                        let used = if set.rounds == 0 { "no fix rounds".to_string() } else { format!("{} fix round{} used", set.rounds, if set.rounds == 1 { "" } else { "s" }) };
                        self.check_ev(&c.name, CheckStatus::Stopped, cr.round, set.rounds, format!("{} · {used}", o.summary));
                        cr.failing = None;
                        return Gate::End(TurnEnd::Done);
                    }
                    self.check_ev(&c.name, CheckStatus::Fail, cr.round, set.rounds, o.summary.clone());
                    self.history.push(Message::user_text(checks::fix_note(c, &o, cr.round, set.rounds)));
                    (self.new_card, self.card_broken) = (None, false);
                    cr.round += 1;
                    cr.sig = Some(o.signature);
                    cr.failing = Some((c.name.clone(), o.summary));
                    return Gate::Fix;
                }
            }
        }
        cr.failing = None;
        Gate::End(TurnEnd::Done)
    }

    fn check_ev(&mut self, name: &str, status: CheckStatus, round: usize, rounds: usize, summary: String) {
        let e = CheckEvent { cmd: name.to_string(), status, round, rounds, summary };
        if status != CheckStatus::Running { self.tool_lines.push(format!("[{}]", e.line())); }
        self.fe.check(&e);
    }

    // ----- graph memory -----

    fn open_memory(&mut self) {
        if !self.cfg.memory { return; }
        let project = self.cwd.display().to_string();
        match Memory::open(&self.cfg.memory_dir) {
            Ok(mut db) => {
                db.set_progress(Box::new(mem_progress));
                // Elisa decides when to embed / compact, so end_session never blocks the exit
                db.auto_index = false;
                db.compact_every = 0;
                self.index_rate = std::fs::read_to_string(self.cfg.memory_dir.join(RATE_FILE)).ok().and_then(|s| s.trim().parse().ok()).filter(|r: &f64| *r > 0.0);
                let session = db.begin_session(&project);
                // the last card is carried only into a continued session (`carry_card`)
                self.mem = Some(Mem { db, session, project });
            }
            Err(e) => self.fe.error(&format!("memory off: {}: {e}", self.cfg.memory_dir.display())),
        }
    }

    /// `--continue` / `/resume`: the project's latest state card becomes current again (cleaned of template text; a
    /// card /reset or /forget dropped does not come back). A new session never calls this.
    fn carry_card(&mut self) {
        if self.card.is_some() { return; }
        let reset = self.store.card_reset();
        let Some(m) = self.mem.as_ref() else { return };
        let Some((id, text)) = m.db.latest_card(&m.project).filter(|(id, _)| reset.is_none_or(|r| *id > r)) else { return };
        if let Some(c) = context::clean_card(&text) { (self.card, self.card_id) = (Some(c), Some(id)); }
    }

    /// Runs `f` on the store with elisa-mem's progress shown live (`compacting memory i/n`, `indexing N memories`).
    /// Returns (result, whether a progress line was shown); a shown line is cleared before returning.
    fn mem_call<R>(&mut self, f: impl FnOnce(&mut Mem) -> R) -> Option<(R, bool)> {
        let m = self.mem.as_mut()?;
        Some(with_progress(&mut *self.fe, || f(m)))
    }

    /// Embeds memories that have no vector yet. With Some(budget_s) (exit) it runs only when the measured speed says
    /// it fits; otherwise the rest stays on disk and the next fold indexes it. Unknown speed: only a small batch.
    fn index_pending(&mut self, budget_s: Option<f64>) -> bool {
        if self.mem.is_none() || (budget_s.is_some() && self.unindexed == 0) { return false; }
        if let Some(b) = budget_s {
            let n = self.unindexed as f64;
            let fits = match self.index_rate { Some(r) => n / r <= b, None => self.unindexed <= EXIT_INDEX_UNKNOWN };
            if !fits {
                let eta = self.index_rate.map_or(String::new(), |r| format!(" (~{})", crate::ui::dur(n / r, 0)));
                self.fe.info(&format!("[memory: {} memories left to index next session{eta}; kept on disk]", self.unindexed));
                return false;
            }
        }
        let t0 = Instant::now();
        let Some((r, shown)) = self.mem_call(|m| m.db.index_pending()) else { return false };
        let dt = t0.elapsed().as_secs_f64();
        match r {
            Ok(n) => {
                self.unindexed = 0;
                if n >= 4 && dt > 0.2 {
                    let rate = n as f64 / dt;
                    self.index_rate = Some(rate);
                    let _ = std::fs::write(self.cfg.memory_dir.join(RATE_FILE), format!("{rate:.3}"));
                }
            }
            Err(e) => self.fe.error(&format!("memory index: {e}")),
        }
        shown
    }

    /// Merges the hot log into the index (elisa-mem's own every-10-sessions compaction is off so exit stays quick).
    fn compact_if_big(&mut self) -> bool {
        let big = self.mem.as_ref().is_some_and(|m| m.db.stats().hot_bytes > COMPACT_HOT_BYTES);
        if !big { return false; }
        let Some((r, shown)) = self.mem_call(|m| m.db.compact()) else { return false };
        if let Err(e) = r { self.fe.error(&format!("memory compact: {e}")); }
        shown
    }

    /// /full: the full model's answer to the newest reply its audit found changed (of this session's replies, else any).
    fn full(&mut self) {
        let _ = self.watch.fetch_now();
        self.watch_poll();
        if self.watch.off() { return self.fe.info("watcher: not available on this gateway"); }
        match self.watch.last_full() {
            Some(a) => {
                let at = a.diverge_at.map_or(String::new(), |t| format!(" · differs at token {t}"));
                let dom = if a.domain.is_empty() { String::new() } else { format!(" · {}", a.domain) };
                self.fe.full_answer(&format!("full model{dom}{at}"), a.full_answer.as_deref().unwrap_or(""));
            }
            None => self.fe.info("no audit with a full-model answer yet"),
        }
    }

    /// /copy: the last reply's text to the clipboard (pbcopy).
    fn copy_last(&mut self) {
        let Some(t) = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).filter(|t| !t.trim().is_empty()) else {
            return self.fe.info("no reply to copy yet");
        };
        let r = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn().and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().map_or(Ok(()), |mut i| i.write_all(t.trim().as_bytes()))?;
            c.wait()
        });
        match r {
            Ok(s) if s.success() => self.fe.info(&format!("copied the last reply ({} chars)", t.trim().chars().count())),
            Ok(s) => self.fe.error(&format!("copy failed: pbcopy {s}")),
            Err(e) => self.fe.error(&format!("copy failed: pbcopy: {e}")),
        }
    }

    /// A note for the domain watcher (background; dropped when the gateway has none).
    fn watch_note(&self, kind: &str, text: &str) { self.watch.note(kind, text); }
    /// Delivers a watcher update that arrived without a push sink (the plain display).
    fn watch_poll(&mut self) { if let Some(u) = self.watch.take() { self.fe.watcher_update(&u); } }
    /// A shrink without a summary: the card (or the token counts) stands for the fold.
    fn shrunk(&mut self, before: usize) {
        let after = self.estimate();
        self.fe.compacted(before, after);
        let t = self.card.clone().unwrap_or_else(|| format!("[compacted: {before} → {after} tokens]"));
        self.watch_note("fold", &t);
    }

    /// No model call here, so quitting stays instant; indexing runs only within EXIT_INDEX_BUDGET_S.
    fn end_memory(&mut self) {
        let Some((r, mut shown)) = self.mem_call(|m| m.db.end_session(m.session)) else { return };
        if let Err(e) = r { self.fe.error(&format!("memory: {e}")); }
        shown |= self.index_pending(Some(EXIT_INDEX_BUDGET_S));
        if shown { self.fe.progress(None); }
    }

    fn new_memory_session(&mut self) {
        self.end_memory();
        if let Some(m) = self.mem.as_mut() { m.session = m.db.begin_session(&m.project); }
        self.turn_no = 0;
        self.fold_reset();
    }

    fn fold_reset(&mut self) {
        self.folded = self.turn_no;
        self.fold_ids.clear();
        self.fold_retry_at = 0;
    }

    /// Counts the turn for the fold (also without memory) and records it.
    fn record(&mut self, role: MRole, text: &str) -> Option<u64> {
        self.turn_no += 1;
        let m = self.mem.as_mut()?;
        match m.db.add_turn(m.session, role, &crate::secrets::redact(text).0) {
            Ok(id) => { self.fold_ids.push(id); self.unindexed += 1; Some(id) }
            Err(e) => { self.fe.error(&format!("memory: {e}")); None }
        }
    }

    /// First message of a fresh history: cwd/OS/date + notes + [state] (latest card + pinned facts) + `input`.
    fn first(&self, input: &str) -> String {
        context::first_message_with(&self.cwd, self.first_notes().as_deref(), self.state_block().as_deref(), input)
    }

    /// The notes as a first message carries them: as they are while they are this session's own, marked as background
    /// (without the earlier session goal) in a new session and after /clear or /reset.
    fn first_notes(&self) -> Option<String> {
        let n = self.notes.as_deref()?;
        Some(if self.notes_bg { context::background_notes(n) } else { n.to_string() })
    }

    /// The `[state]` body: the latest state card, then this project's pinned facts (newest first within
    /// PINNED_CAP tokens, listed oldest first). None when there is neither.
    fn state_block(&self) -> Option<String> {
        let facts = self.mem.as_ref().map(|m| m.db.pinned_facts(&m.project)).unwrap_or_default();
        // the first message carries the notes too: a fact they already say is not repeated here
        let facts = context::facts_not_in(self.notes.as_deref(), facts);
        let (mut keep, mut used) = (Vec::new(), 0);
        for f in facts.iter().rev() {
            let l = format!("- {}", context::one_line(f, 200));
            let t = estimate_tokens(&l);
            if used + t > PINNED_CAP { break; }
            used += t;
            keep.push(l);
        }
        keep.reverse();
        let card = self.card.as_deref().map(str::trim).filter(|c| !c.is_empty());
        // the session goal apart from the current request (the card's `goal:` follows the newest request)
        // (not while the current request is the goal itself: the request section says it)
        let req = self.user_req.clone();
        let asking = self.in_turn;
        let goal = self.goal.as_deref().filter(|g| !(asking && *g == req)).map(|g| format!("session goal: {g}"));
        let cur = goal.as_ref().filter(|_| asking && !req.is_empty()).map(|_| format!("current request: {}", context::one_line(&req, 160)));
        if card.is_none() && keep.is_empty() && goal.is_none() { return None; }
        let mut s = String::new();
        for l in [goal, cur].into_iter().flatten() {
            if !s.is_empty() { s.push('\n'); }
            s += &l;
        }
        if let Some(c) = card {
            if !s.is_empty() { s.push('\n'); }
            s += c;
        }
        if !keep.is_empty() {
            if !s.is_empty() { s.push('\n'); }
            s += "Pinned facts:\n";
            s += &keep.join("\n");
        }
        Some(s)
    }

    /// /reset, /forget: no carried card now, and none from elisa-mem in a later session (`.elisa/card-reset`).
    fn drop_card(&mut self) {
        let newest = self.card_id.or_else(|| self.mem.as_ref().and_then(|m| m.db.latest_card(&m.project).map(|c| c.0)));
        if let Some(id) = newest { if let Err(e) = self.store.save_card_reset(id) { self.fe.error(&format!("card reset: {e}")); } }
        (self.card, self.card_id, self.card_ex) = (None, None, None);
        self.card_log.clear();
    }

    /// A new card from this exchange: show it, chain it in memory (prev = the last card), make it current.
    fn set_card(&mut self, card: String, ids: &[u64]) {
        if self.card_log.len() >= 64 { self.card_log.remove(0); }
        self.card_log.push(card.clone());
        self.watch_note("state_card", &card);
        let prev = self.card_id;
        let mut saved = None;
        if let Some(m) = self.mem.as_mut() {
            match m.db.add_card(m.session, &crate::secrets::redact(&card).0, prev, ids) {
                Ok(id) => { self.card_id = Some(id); saved = Some(id); self.unindexed += 1; }
                Err(e) => self.fe.error(&format!("memory: {e}")),
            }
        }
        // the card number: its memory id (/state shows `card #n`), or with memory off its count in this run
        self.cards += 1;
        self.fe.state_card_n(&card, saved.unwrap_or(self.cards));
        self.card = Some(card);
        self.card_ex = Some(self.exchanges);
    }

    /// [first message] + `rest`; the kept first user text gets the header, otherwise a header message is added
    /// (mid-turn and the request folded away: it carries the current request).
    fn rebuild_history(&mut self, rest: Vec<Message>, generic: &str) {
        let has_request = rest.iter().any(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. })));
        self.history = match rest.first() {
            Some(m) if m.role == Role::User => {
                let mut rest = rest;
                for b in rest[0].content.iter_mut() {
                    if let Block::Text { text } = b { *text = self.first(context::request_body(text)); break; }
                }
                rest
            }
            _ => {
                let req = if has_request || !self.in_turn { generic.to_string() } else { self.input.clone() };
                std::iter::once(Message::user_text(self.first(&req))).chain(rest).collect()
            }
        };
        self.real = None;
    }

    /// `n` turns left the history without a note (the state card carries them).
    fn mark_folded(&mut self, n: usize) {
        let n = n.min(self.turn_no - self.folded);
        self.fold_ids.drain(..n.min(self.fold_ids.len()));
        self.folded += n;
        self.fold_retry_at = 0;
    }

    /// A summary covering the next `n` unfolded turns (all of them for /clear and /compact).
    fn mem_note(&mut self, notes: &str, n: usize) {
        let n = n.min(self.turn_no - self.folded);
        let k = n.min(self.fold_ids.len());
        if let Some(m) = self.mem.as_mut() {
            match m.db.add_note(m.session, &crate::secrets::redact(notes).0, &self.fold_ids[..k]) {
                Ok(_) => self.unindexed += 1,
                Err(e) => self.fe.error(&format!("memory: {e}")),
            }
        }
        self.fold_ids.drain(..k);
        self.folded += n;
        self.fold_retry_at = 0;
    }

    /// Up to recall_k hits not already in the history or notes, one line each (<= 200 chars), <= recall_max_tokens.
    /// Skipped: turns of this memory session still in the window, the current card and its prev, the lower-scoring
    /// of a Card/Turn pair saying the same thing (>= 60% word overlap), and other projects' hits below 2x
    /// recall_min_score. User-turn hits score x0.5 and are skipped when the answer or a card of the same exchange is a
    /// candidate. At most 2 lines unless every candidate is strong (>= 2x recall_min_score).
    /// Every hit must also pass `context::recall_passes` (keyword evidence without the vector part, and the share of
    /// the query it covers; stricter mid-session), and other projects need a strong keyword score.
    fn recall(&mut self, query: &str) -> Vec<String> {
        // a greeting or a one-word reply carries no topic: recalling on it only drags old replies back in
        if self.mem.is_none() || self.cfg.recall_k == 0 || query.split_whitespace().count() < 3 { return Vec::new(); }
        let (k, t0, show) = (self.cfg.recall_k, Instant::now(), self.recall_slow);
        if show { self.fe.progress(Some(&Progress { label: "searching memory…".into(), ..Default::default() })); }
        let Some((hits, shown)) = self.mem_call(|m| m.db.recall(query, &m.project, k, true)) else { return Vec::new() };
        self.recall_slow = t0.elapsed() > SLOW;
        if show || shown { self.fe.progress(None); }
        if hits.is_empty() { return Vec::new(); }
        let mut hay: String = self.history.iter().map(texts).collect::<Vec<_>>().join(" ");
        if let Some(n) = &self.notes { hay += " "; hay += n; }
        let hay = context::one_line(&hay, usize::MAX);
        let (min, strong) = (self.cfg.recall_min_score, 2.0 * self.cfg.recall_min_score);
        // mid-session the window already holds the conversation: only clearly matching memories are worth adding
        let mid = !self.history.is_empty();
        let min_support = self.cfg.recall_min_support;
        let Some(m) = self.mem.as_ref() else { return Vec::new() };
        // the current card and its prev are already in [state] (or were just superseded by it)
        let cards: Vec<u64> = self.card_id.map(|c| m.db.card_chain(c, 2).into_iter().map(|(k, _)| k).chain([c]).collect()).unwrap_or_default();
        // user questions are low value: x0.5, and dropped when the answer or a card of the same exchange is a candidate
        let mut hits = hits;
        let info: Vec<Option<(bool, Vec<u64>)>> = hits.iter().map(|h| if h.kind == HitKind::Turn { m.db.exchange(h.id) } else { None }).collect();
        for (h, i) in hits.iter_mut().zip(&info) { if matches!(i, Some((true, _))) { h.score *= 0.5; } }
        let better: Vec<u64> = hits.iter().zip(&info)
            .filter(|(h, i)| h.score >= min && (h.kind == HitKind::Card || matches!(i, Some((false, _)))))
            .map(|(h, _)| h.id).collect();
        let mut hits: Vec<elisa_mem::Hit> = hits.into_iter().zip(info)
            .filter(|(_, i)| !matches!(i, Some((true, rel)) if rel.iter().any(|r| better.contains(r))))
            .map(|(h, _)| h).collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut cand: Vec<elisa_mem::Hit> = Vec::new();
        for h in hits {
            if h.score < min || !context::recall_passes(h.lex, h.support, mid, min, min_support) { continue; }
            match h.kind {
                HitKind::Turn if h.session == m.session && self.fold_ids.contains(&h.id) => continue,
                HitKind::Card if cards.contains(&h.id) => continue,
                _ => {}
            }
            if h.lex < strong && !m.db.in_project(h.id, &m.project) { continue; }
            // hits come best first: a Card/Turn twin of a kept hit is the weaker copy
            let twin = |a: &elisa_mem::Hit| matches!((&a.kind, &h.kind), (HitKind::Card, HitKind::Turn) | (HitKind::Turn, HitKind::Card))
                && context::word_overlap(&a.text, &h.text) >= 0.6;
            if cand.iter().any(twin) { continue; }
            let flat = context::one_line(&h.text, usize::MAX);
            let key: String = flat.chars().take(80).collect();
            if key.is_empty() || hay.contains(&key) { continue; }
            cand.push(h);
        }
        if !cand.iter().all(|h| h.lex >= strong) { cand.truncate(2); }
        let (mut used, mut out) = (0, Vec::new());
        for h in cand {
            let l = context::one_line(&h.text, 200);
            let t = estimate_tokens(&l);
            if used + t > self.cfg.recall_max_tokens { break; }
            used += t;
            out.push(l);
        }
        out
    }

    /// Fold: shrink tool results older than the last 2 turns to one line (between turns: every tool result, and only
    /// the last exchange is kept), then summarise the oldest messages (no
    /// tools, streamed with progress) until the kept history is <= fold_to of the budget, save the summary to the
    /// notes and the graph, and rebuild the history as [first message with the notes] + the kept tail (tool pairs
    /// intact). Mid-turn, the current user turn (request + its tool pairs so far) is never dropped: only earlier turns
    /// fold; when the current turn alone is over the target, its older tool results shrink (never the newest).
    /// `by_turns`: the fold_max_turns cap; then everything before the latest user turn is summarised even if it fits.
    fn fold(&mut self, by_turns: bool) {
        let before = (self.history.len(), self.folded, estimate_messages(&self.history));
        self.fold_inner(by_turns);
        self.fit_newest_results();
        // a between-turns fold rewrote the history: the next call would re-prefill all of it after system + tools
        // (the engine state cannot rewind); warm it while the user reads
        if !self.in_turn && (self.history.len(), self.folded, estimate_messages(&self.history)) != before { self.warm_up(); }
    }

    /// The folded prompt as a prefill-only call on another thread (config `warm`; gateway `x-streamdense-warm`). The
    /// next request (this history + the next user text) then starts from that state. Silent: no gateway support
    /// (/health, an error, or a generated token) turns it off for the session.
    fn warm_up(&mut self) {
        if self.history.is_empty() { return; }
        self.warm_msgs(self.history.clone());
    }

    /// A warm-up of these messages after the system prompt and tools (the folded history).
    fn warm_msgs(&mut self, msgs: Vec<Message>) {
        if !self.warm_ok() { return; }
        let job = self.client.warm_job(&Request { system: &self.system, tools: &self.tools, messages: &msgs, max_tokens: 0 });
        let flag = self.warm.clone();
        std::thread::spawn(move || {
            if !matches!(job(), Ok(true)) { flag.store(WARM_NO, Ordering::SeqCst); }
        });
    }

    /// Warm-ups allowed: `warm = "off"` never, `"on"` always, else once the gateway's /health says it takes them
    /// (asked once; a failed warm-up turns them off for the session).
    fn warm_ok(&mut self) -> bool {
        let st = self.warm.load(Ordering::SeqCst);
        if st == WARM_NO { return false; }
        if st == WARM_UNKNOWN {
            let ok = match self.cfg.warm.as_str() { "off" => false, "on" => true, _ => self.client.server_warm() };
            self.warm.store(if ok { WARM_YES } else { WARM_NO }, Ordering::SeqCst);
            return ok;
        }
        true
    }

    fn fold_inner(&mut self, by_turns: bool) {
        self.client.set_kind("fold");
        let before = self.estimate();
        let mut hist = self.history.clone();
        // between turns: the card/notes carry the past, so every tool result shrinks and only the last exchange stays
        // the newest real request: Elisa's own notes (check fix, tool-call nudge) are not the user's
        let last_user = hist.iter().rposition(is_request).unwrap_or(0);
        if self.in_turn { context::shrink_old_results(&mut hist, 2); } else { context::shrink_all_results(&mut hist); }
        let out = COMPACT_OUT.min(self.cfg.max_output);
        // room for the kept tail: fold_to of the budget minus the new notes (<= `out` tokens) or the fresh card
        let fresh = self.card.is_some() && self.card_id.is_some() && self.card_ex.is_some_and(|ex| self.exchanges - ex < 2);
        let new_text = if fresh { self.card.as_deref().map_or(0, estimate_tokens) } else { out.min(self.notes_budget()) };
        let room = context::fold_room(self.fixed(), self.limit(), self.cfg.fold_to, new_text);
        let cap = self.turn_cap(&hist);
        let mut start = hist.len();
        while start > 0 && estimate_messages(&hist[start - 1..]) <= room { start -= 1; }
        start = start.min(cap);
        // never start on tool results: keep the assistant tool_use that goes with them, or drop the orphans
        while start < hist.len() && hist[start].content.iter().any(|b| matches!(b, Block::ToolResult { .. })) {
            if start > 0 && estimate_messages(&hist[start - 1..]) <= room + room / 4 { start -= 1; } else { start += 1; }
        }
        if (start == 0 && by_turns) || !self.in_turn { start = start.max(last_user); }
        // the current turn alone over the target: its older tool results become one line
        if self.in_turn && estimate_messages(&hist[start..]) > room { context::shrink_results_but_newest(&mut hist[cap..]); }
        if start == 0 {
            // shrinking alone fits (or nothing before the current turn): no summary needed
            if estimate_messages(&hist) < estimate_messages(&self.history) {
                self.history = hist;
                self.real = None;
                self.shrunk(before);
            }
            let est = self.estimate();
            if est >= self.fold_point() { self.fold_retry_at = est + self.min_gain(); }
            return;
        }
        // turns in hist[..start]: 2 per user text, minus the reply when it is not folded too
        let u = hist[..start].iter().filter(|m| is_request(m)).count();
        let complete = if start == hist.len() { !self.in_turn } else { is_request(&hist[start]) };
        let nf = (2 * u).saturating_sub((!complete && u > 0) as usize);
        let (a, b) = (self.folded + 1, self.folded + nf.max(1));
        let label = format!("summarizing turns {a}–{b}");
        let t0 = Instant::now();
        let prog = |frac: f32, done: Option<String>| Progress { label: label.clone(), frac: Some(frac), elapsed_s: t0.elapsed().as_secs_f64(), done };
        // a fresh state card already says what the next step needs: no summary request
        if let (true, Some(k)) = (fresh, self.card_id) {
            {
                let p = |done: Option<String>| Progress { label: "folding with state card".into(), frac: Some(if done.is_some() { 1.0 } else { 0.0 }), elapsed_s: t0.elapsed().as_secs_f64(), done };
                self.fe.progress(Some(&p(None)));
                let rest = hist.split_off(start);
                let kept = rest.len();
                // the folded turns' decisions / open / done items go to the notes from their cards (no model call),
                // each card's "done" claims checked against Elisa's edit records first
                let cards: Vec<String> = self.card_log.iter().map(|c| self.verified(c)).collect();
                let merged = self.finish_notes(context::merge_card_notes(self.notes.as_deref(), &cards));
                if !self.commit_fold(merged, rest, "[earlier turns are carried by the state card above]", before) {
                    self.fe.progress(Some(&p(Some("fold skipped: it would not make the context smaller".into()))));
                    return;
                }
                self.card_log.clear();
                self.mark_folded(nf);
                self.save();
                self.index_pending(None);
                self.compact_if_big();
                self.fe.compacted(before, self.estimate());
                if let Some(c) = self.card.clone() { self.watch_note("fold", &c); }
                self.watch.refresh();
                self.fe.progress(Some(&p(Some(format!("folded · kept {kept} msgs · card #{k}")))));
                return;
            }
        }
        self.fe.progress(Some(&prog(0.0, None)));
        let digest_chars = out * 3;
        // a reasoning model (Ollama "thinking") reasons past the summary's budget (3,800 tokens, 35 s on qwen3-vl:2b)
        // and writes nothing: such a model folds to a digest straight away
        if self.digest_only || self.client.reasoning_room() > 0 {
            let why = if self.digest_only { "the model's summaries ran out on reasoning" } else { "a reasoning model's summary is slow" };
            let n = context::digest(&hist[..start], None, digest_chars);
            return self.fold_finish(n, hist, start, nf, before, prog(1.0, Some(format!("folded with a short digest ({why})"))));
        }
        // 15% under the estimate: the summary request itself never goes over the context (transcripts of code and
        // JSON tokenize denser than the 3.3 chars/token estimate)
        let budget = self.max_ctx.saturating_sub(out + self.client.reasoning_room() + 64 + estimate_tokens(COMPACT_SYSTEM)) * 85 / 100;
        // the earlier notes are merged by Elisa afterwards (`context::merge_notes`), not left to the model
        let msgs = [context::compact_request(&hist[..start], None, budget)];
        let req = Request { system: COMPACT_SYSTEM, tools: &[], messages: &msgs, max_tokens: out };
        CANCEL.store(false, Ordering::SeqCst);
        let (client, fe) = (&self.client, &mut *self.fe);
        let res = std::thread::scope(|sc| {
            let (tx, rx) = mpsc::channel::<usize>();
            let h = sc.spawn(move || client.send(&req, &CANCEL, &mut |t: &str| { let _ = tx.send(t.chars().count()); }));
            let (mut chars, mut last) = (0usize, Instant::now());
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(n) => chars += n,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if last.elapsed() >= Duration::from_millis(250) {
                    fe.progress(Some(&prog((chars as f32 / FOLD_EXPECT_CHARS).min(0.95), None)));
                    last = Instant::now();
                }
            }
            h.join().unwrap_or_else(|_| Err("summary thread panicked".into()))
        });
        let notes = match res {
            Ok(r) if r.stop_reason == "cancelled" => Err("cancelled".to_string()),
            Ok(r) => {
                let why = if r.usage.reasoning_tokens > 0 && r.stop_reason == "max_tokens" { format!("the model reasoned for {} tokens and ran out before writing it", r.usage.reasoning_tokens) } else { "empty summary".into() };
                Some(summary_text(r.content)).filter(|n| !n.is_empty()).ok_or(why)
            }
            Err(e) => Err(e),
        };
        let n = match notes {
            Ok(n) => n,
            Err(e) if e == "cancelled" => {
                self.fold_retry_at = before + self.limit() / 4;
                self.fe.progress(Some(&prog(1.0, Some("summary cancelled · history kept".into()))));
                return;
            }
            Err(e) => {
                // the history is still folded, with a digest for notes: kept as it was, the window filled up, the
                // summary was tried again (76 s on a small local model) before every request, and replies were cut
                self.digest_only |= e.starts_with("the model reasoned");
                let n = context::digest(&hist[..start], None, digest_chars);
                return self.fold_finish(n, hist, start, nf, before, prog(1.0, Some(format!("summary failed: {e} · folded with a short digest"))));
            }
        };
        let p = prog(1.0, None);
        self.fold_finish(n, hist, start, nf, before, p);
    }

    /// The end of a summary fold: `n` (its claims checked, merged into the earlier notes) becomes the notes and
    /// `hist[start..]` the history, when that makes the prompt smaller (`commit_fold`).
    fn fold_finish(&mut self, n: String, mut hist: Vec<Message>, start: usize, nf: usize, before: usize, mut p: Progress) {
        let n = self.finish_notes(context::merge_notes(self.notes.as_deref(), &self.verified(&n)));
        let rest = hist.split_off(start);
        if !self.commit_fold(n.clone(), rest, "[earlier turns are summarised in the notes above]", before) {
            p.done = Some("fold skipped: it would not make the context smaller".into());
            self.fe.progress(Some(&p));
            return;
        }
        // a digest is only the turns' own lines, which memory already has: saved again, it came back as a recalled
        // "memory" of the same words
        if p.done.is_none() { self.mem_note(&n, nf); } else { self.mark_folded(nf); }
        self.card_log.clear();
        self.save();
        self.index_pending(None);
        self.compact_if_big();
        let total = self.mem.as_ref().map_or(0, |m| m.db.stats().turns);
        self.fe.compacted(before, self.estimate());
        self.watch_note("fold", &n);
        self.watch.refresh();
        if p.done.is_none() { p.done = Some(format!("saved summary · {} words · memory {} turns", n.split_whitespace().count(), total)); }
        self.fe.progress(Some(&p));
    }

    /// A fold's result put in place: `notes` and the history `[first message] + rest`, only when that makes the
    /// prompt smaller than `before` (soak 2026-11-08: 6 of 40 folds made it bigger, 3842 → 3907 …). Over it, the notes
    /// are cut by priority to fit first; when even that does not do it, everything stays as it was, the retry waits
    /// for the window to grow, and false comes back. The notes are saved only when the fold is kept.
    fn commit_fold(&mut self, notes: String, rest: Vec<Message>, generic: &str, before: usize) -> bool {
        let saved = (self.history.clone(), self.notes.clone(), self.notes_bg, self.real);
        // text estimates on both sides (`before` may be the server's count)
        let base = self.fixed() + estimate_messages(&self.history);
        let try_with = |s: &mut Self, n: &str| {
            s.put_notes(Some(n.to_string()), false);
            s.rebuild_history(rest.clone(), generic);
            s.estimate()
        };
        let mut after = try_with(self, &notes);
        if after >= base {
            let over = after + 1 - base + self.min_gain() / 4;
            let cut = context::cap_notes_priority(&notes, estimate_tokens(&notes).saturating_sub(over));
            if !cut.is_empty() { after = try_with(self, &cut); }
        }
        if after >= base {
            (self.history, self.notes, self.notes_bg, self.real) = saved;
            self.rebuild_system();
            self.real = None;
            self.fold_retry_at = before + self.min_gain();
            return false;
        }
        if let Some(n) = &self.notes { if let Err(e) = self.store.save_notes(n) { self.fe.error(&format!("notes: {e}")); } }
        true
    }

    /// May this tool call run? A call that reaches outside the project folder goes through `approve_outside` first
    /// (its own rules: never the per-project always-allow list, never `--yes` for writes).
    fn approve(&mut self, name: &str, input: &serde_json::Value, summary: &str) -> Result<(), String> {
        // a file-tool call that cannot change anything (URL path, old == new, old not found, binary file) fails
        // before anyone is asked (`tools::precheck`); MCP tools are their servers' business
        if !self.mcp.owns(name) {
            if let Err(e) = tools::precheck(name, input, &self.tctx) { self.answer = Some("invalid"); return Err(e); }
        }
        // pattern rules (`[permissions]`, plugins; `perms.rs`): a deny stops the call here, an ask always asks, an
        // allow skips the prompt (never for a server whose tool list changed)
        let server = self.mcp.server_of(name).map(String::from);
        let remote = self.mcp.remote_of(name).map(String::from);
        let rule = self.cfg.perms.decide(name, input, server.as_deref(), remote.as_deref(), &self.cwd)
            .map(|r| RuleHit { level: r.level, pattern: r.pattern.clone(), source: r.source.clone() });
        if let Some(r) = rule.as_ref().filter(|r| r.level == Level::Deny) {
            self.answer = Some("rule");
            return Err(format!("denied by the rule deny {} ({}): {NO_RETRY}", r.pattern, r.source));
        }
        if let Some(o) = outside::check(name, input, &self.cwd) { return self.approve_outside(name, summary, &o, input, rule); }
        self.approve_inside(name, summary, input, rule)
    }

    /// Outside the project (config `outside`): deny → denied; allow → the normal rules; read-ok → reads take the
    /// normal rules, writes ask; ask → ask (Yes once / Always = these paths for this session / No). Writes outside
    /// (and bash naming outside paths) under `--yes` / approve-all are denied unless `outside = "allow"`; with nobody
    /// to ask (-p without a terminal) the call is denied. Paths granted for the session pass without asking.
    fn approve_outside(&mut self, name: &str, summary: &str, o: &Outside, input: &serde_json::Value, rule: Option<RuleHit>) -> Result<(), String> {
        let list = o.paths.join(", ");
        let what = o.access_word();
        match self.outside_mode {
            outside::Mode::Deny => return Err(format!("denied: {what} outside the project folder ({list}); config outside = \"deny\"")),
            outside::Mode::Allow => return self.approve_inside(name, summary, input, rule),
            outside::Mode::ReadOk if !o.write() => return self.approve_inside(name, summary, input, rule),
            _ => {}
        }
        if outside::covered(&self.outside_ok, o) { return Ok(()); }
        if self.yolo && o.write() {
            return Err(format!("denied: {what} outside the project folder ({list}) is never auto-approved by --yes / approve-all; \
                set outside = \"allow\" in ~/.config/elisa/config.toml to allow it"));
        }
        if !self.interactive {
            return Err(format!("denied: {what} outside the project folder ({list}) needs a person to approve it (non-interactive){}",
                if o.write() { "" } else { "; outside = \"read-ok\" allows outside reads" }));
        }
        let a = self.fe.approve_outside(name, summary, o);
        self.answer = Some(answer_word(&a));
        if CANCEL.load(Ordering::SeqCst) { self.answer = Some("cancelled"); return Err("cancelled by user".into()); }
        match a {
            Approval::Yes => Ok(()),
            Approval::Always => {
                for p in &o.paths {
                    let p = std::path::Path::new(p);
                    if p.is_absolute() { self.outside_ok.push((p.to_path_buf(), o.write())); }
                }
                self.fe.info(&format!("allowed for this session ({}): {list}", if o.write() { "read + write" } else { "read" }));
                Ok(())
            }
            Approval::Rule => Ok(()),
            Approval::No => Err(format!("denied by user ({what} outside the project folder: {list}): {NO_RETRY}")),
        }
    }

    /// one audit line about a tool call: its tool, its MCP server (if any) and its summary, plus `fields`
    fn audit_tool(&self, event: &str, name: &str, summary: &str, fields: &[(&str, &str)]) {
        if !self.audit.on() { return; }
        let server = self.mcp.server_of(name).unwrap_or("").to_string();
        let mut f: Vec<(&str, &str)> = vec![("tool", name), ("server", &server), ("summary", summary)];
        f.extend_from_slice(fields);
        self.audit.note(event, &f);
    }

    /// `/audit [N]`: the last N (20) entries of this project's audit log, and whether its hash chain holds
    fn audit_command(&mut self, arg: &str) {
        let Some(path) = self.audit.path().map(std::path::Path::to_path_buf) else { self.fe.info("the audit log is off (config audit = \"off\")"); return };
        if !path.exists() { self.fe.info(&format!("audit log empty ({})", path.display())); return; }
        let n = arg.parse::<usize>().unwrap_or(20).max(1);
        let lines = crate::audit::last(&path, n);
        if !lines.is_empty() { self.fe.info(&lines.join("\n")); }
        match crate::audit::verify(&path) {
            Ok(k) => self.fe.info(&format!("chain ok ({k} entries) · {}", path.display())),
            Err((l, why)) => self.fe.error(&format!("chain broken at line {l}: {why} · {}", path.display())),
        }
    }

    fn approve_inside(&mut self, name: &str, summary: &str, input: &serde_json::Value, rule: Option<RuleHit>) -> Result<(), String> {
        // outline / read_symbol go with `read`, edit_symbol with `edit`; an MCP tool goes with its server (`web`)
        // MCP keys live in their own namespace (`mcp:web`), so a server named like one of Elisa's tools can never
        // share its always-allow / auto_approve entry; auto_approve matches MCP tools only by `mcp:SERVER`
        // the built-in reasoning tools (think, steps, step_done, calc) have no side effects: never asked
        if self.mcp.reason_tool(name) { return Ok(()); }
        let server = self.mcp.server_of(name).map(String::from);
        let key = match &server { Some(s) => tools::mcp_key(s), None => tools::approval_name(name).to_string() };
        let key = key.as_str();
        // a server whose tool list changed since it was pinned (`pins.rs`): no always-allow, no --yes, until /trust
        let untrusted = server.as_deref().is_some_and(|s| self.mcp.untrusted(s));
        if untrusted && !self.interactive {
            let s = server.as_deref().unwrap_or("");
            return Err(format!("denied: MCP server {s}'s tool list changed since it was pinned, so each call needs a person to approve it; \
                check the server, then /trust {s} in an interactive session"));
        }
        // an ask rule: a person decides each time (no --yes, no always-allow, no allow rule gets past it)
        let ask = rule.as_ref().filter(|r| r.level == Level::Ask);
        if let Some(r) = ask {
            if !self.interactive {
                return Err(format!("denied: the rule ask {} ({}) needs a person to approve each call (non-interactive)", r.pattern, r.source));
            }
        } else {
            if !untrusted && rule.as_ref().is_some_and(|r| r.level == Level::Allow) { self.answer = Some("rule"); return Ok(()); }
            if !untrusted && (self.yolo || self.always.contains(key) || self.cfg.auto_approve.iter().any(|a| a == key || (server.is_none() && a == name))) { return Ok(()); }
            if !self.interactive {
                // nobody to ask: local reads run; MCP tools (the web too) are denied like a write
                return if tools::access(name) == Access::ReadOnly && server.is_none() { Ok(()) } else { Err("denied (non-interactive; use --yes)".into()) };
            }
        }
        let summary = match ask { Some(r) => format!("{summary}\nasked by the rule ask {} ({})", r.pattern, r.source), None => summary.to_string() };
        // "always allow this pattern": offered when nothing forces a prompt and there is a narrow pattern for the call
        let offer = if ask.is_none() && !untrusted && self.user_config().is_some() {
            crate::perms::suggest(name, input, server.as_deref(), self.mcp.remote_of(name), &self.cwd)
        } else { None };
        let a = match &offer {
            Some(p) => self.fe.approve_rule(name, &summary, server.as_deref(), p),
            None => self.fe.approve(name, &summary, server.as_deref()),
        };
        self.answer = Some(answer_word(&a));
        if CANCEL.load(Ordering::SeqCst) { self.answer = Some("cancelled"); return Err("cancelled by user".into()); }
        match a {
            Approval::Yes => Ok(()),
            Approval::Always | Approval::Rule if ask.is_some() => {
                self.fe.info("allowed once: an ask rule asks every time");
                Ok(())
            }
            Approval::Rule => {
                let (Some(p), Some(path)) = (offer, self.user_config()) else { return Ok(()) };
                match crate::perms::save_allow(&path, &p) {
                    Ok(()) => {
                        let _ = self.cfg.perms.add(Level::Allow, &p, &path.display().to_string());
                        self.audit.note("rule", &[("summary", &format!("allow {p}")), ("outcome", "saved"), ("detail", &path.display().to_string())]);
                        self.fe.info(&format!("always allowed: {p} (saved to {})", path.display()));
                    }
                    Err(e) => self.fe.error(&format!("the rule {p} was not saved: {e}")),
                }
                Ok(())
            }
            Approval::Always if untrusted => {
                self.fe.info(&format!("allowed once: \"always\" stays off for MCP server {} until /trust {0}", server.as_deref().unwrap_or("")));
                Ok(())
            }
            Approval::Always => {
                // kept per project (.elisa/allow), so a restart does not ask again
                self.always.insert(key.to_string());
                if let Err(e) = self.store.save_allowed(&self.always) { self.fe.error(&format!("could not save always-allow for {name}: {e}")); }
                Ok(())
            }
            Approval::No => Err(format!("denied by user: {NO_RETRY}")),
        }
    }
}

/// an approval answer as the audit log writes it
fn answer_word(a: &Approval) -> &'static str {
    match a { Approval::Yes => "yes", Approval::Always => "always", Approval::Rule => "rule", Approval::No => "no" }
}

/// what `plugin_setup` returns when the user said no
const DECLINED: &str = "nothing was changed: the setup was declined";

/// a slash command Elisa has built in (a file command of the same name is not loaded)
fn builtin_command(name: &str) -> bool {
    COMMANDS.iter().any(|(c, _)| *c == name) || ["/quit", "/plugins", "/chat", "/code", "/mode", "/food", "/toy"].contains(&name)
}

/// `2 commands · 1 plugin · 3 rules` for the project's trust notice
fn project_summary(p: &plugins::Project) -> String {
    let n = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    let mut v = vec![n(p.commands.len(), "command")];
    if !p.infos.is_empty() { v.push(n(p.infos.len(), "plugin")); }
    if !p.rules.is_empty() { v.push(n(p.rules.len(), "rule")); }
    v.join(" · ")
}

/// the permission rule that decided a call (`perms::Rules::decide`)
#[derive(Clone)]
struct RuleHit { level: Level, pattern: String, source: String }

/// added to a denial so the model does not ask again for the same thing
const NO_RETRY: &str = "do not call this tool again for this request; answer without it or ask the user";

/// Tokens the model read through outline / read_symbol, against what reading those files whole would have cost
/// (each file counted once per session; a directory outline counts on the outline side only). Shown by /context.
#[derive(Default)]
struct AstStats { calls: usize, tokens: usize, whole: usize, files: HashSet<PathBuf> }
impl AstStats {
    fn see(&mut self, name: &str, input: &serde_json::Value, content: &str, ctx: &ToolCtx) {
        if name != "outline" && name != "read_symbol" { return; }
        self.calls += 1;
        self.tokens += estimate_tokens(content);
        let Some(p) = input.get("path").and_then(|v| v.as_str()) else { return };
        let full = if std::path::Path::new(p).is_absolute() { PathBuf::from(p) } else { ctx.cwd.join(p) };
        if full.is_file() && self.files.insert(full) {
            if let Some(t) = tools::whole_read_text(ctx, p) { self.whole += estimate_tokens(&t); }
        }
    }
    fn line(&self) -> Option<String> {
        if self.calls == 0 { return None; }
        Some(ast_line(self.calls, self.tokens, self.whole, self.files.len()))
    }
}

/// The /context line on the syntax-tree tools: their tokens against reading the same files whole, and which side is
/// cheaper (uitest: "181 tokens vs 30 … 0.2x fewer" said fewer for 6x more).
fn ast_line(calls: usize, tokens: usize, whole: usize, files: usize) -> String {
    let what = if files == 1 { "that file".to_string() } else { format!("those {files} files") };
    let ratio = match (tokens, whole) {
        (0, _) | (_, 0) => String::new(),
        (t, w) if w >= t => format!(" · {:.1}x fewer tokens", w as f64 / t as f64),
        (t, w) => format!(" · {:.1}x more tokens (small files: reading them whole is cheaper)", t as f64 / w as f64),
    };
    format!("ast: outline + read_symbol {calls} call{} · {} tokens vs {} for reading {what} whole{ratio}",
        if calls == 1 { "" } else { "s" }, ui::k(tokens), ui::k(whole))
}

/// Check-before-done state within one user turn.
#[derive(Default)]
struct CheckRun {
    /// files written / edited in this turn (absolute)
    files: Vec<PathBuf>,
    /// the symbols those edits touched (targeted checks)
    touches: checks::Touches,
    /// an edit since the last check
    dirty: bool,
    /// fix rounds used so far
    round: usize,
    rounds: usize,
    /// the last failure's signature (same again = stop)
    sig: Option<String>,
    /// the check that failed last and its summary, until it passes
    failing: Option<(String, String)>,
}
impl CheckRun {
    fn edited(&mut self, cwd: &std::path::Path, input: &serde_json::Value, before: Option<checks::Before>) {
        if let Some(b) = before { checks::add_touch(&mut self.touches, b); }
        let Some(p) = input.get("path").and_then(|v| v.as_str()).filter(|p| !p.is_empty()) else { return };
        let p = std::path::Path::new(p);
        let p = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
        if !self.files.contains(&p) { self.files.push(p); }
        self.dirty = true;
    }
}

enum Gate { Fix, End(TurnEnd) }

/// A turn stops when one tool call (same name + input) gives the same result this many times in a row, or when this
/// many reads in a row page one file with small results.
const REPEAT_MAX: usize = 3;
/// A `read` with an offset and no limit under this many chars is a small page: only the end of a file is that short
/// (a cut read counts at any size; a read with its own `limit` got what it asked for, so it does not count).
const PAGE_SMALL_CHARS: usize = 600;

/// Streak of identical (call, result) pairs within one turn, and of small `read` pages of one file (any other call
/// ends that one, so the file is unchanged while it runs).
#[derive(Default)]
struct Repeat { last: Option<String>, n: usize, max: usize, what: String, page: Option<String>, pages: usize, paged: bool }
impl Repeat {
    /// Records one executed call; returns how many times in a row it has now given this same result.
    fn see(&mut self, name: &str, input: &serde_json::Value, content: &str, is_error: bool) -> usize {
        let key = format!("{name}\u{0}{input}\u{0}{is_error}\u{0}{content}");
        if self.last.as_deref() == Some(key.as_str()) { self.n += 1; } else { (self.last, self.n) = (Some(key), 1); }
        if self.n > self.max { (self.max, self.what, self.paged) = (self.n, context::one_line(&tools::summary(name, input), 80), false); }
        self.n
    }

    /// Records one call's result as fitted to the context (run 2026-11-05: 2-4 lines per read, new offset each time,
    /// so `see` never fired until the context was full).
    fn page(&mut self, name: &str, input: &serde_json::Value, content: &str, is_error: bool) {
        let path = small_page(name, input, content, is_error);
        self.pages = if path.is_some() && path == self.page { self.pages + 1 } else { usize::from(path.is_some()) };
        self.page = path;
        if self.pages > self.max { (self.max, self.what, self.paged) = (self.pages, context::one_line(&tools::summary(name, input), 80), true); }
    }

    /// Why the turn stops, once `max` reached REPEAT_MAX.
    fn why(&self) -> String {
        let what = &self.what;
        if self.paged { format!("`{what}` paged the file with small results {REPEAT_MAX} times in a row") }
        else { format!("`{what}` returned the same result {REPEAT_MAX} times in a row") }
    }
}

/// The path of a `read` that gave a small page (`PAGE_SMALL_CHARS`, `context::READ_CUT`); None for any other call.
fn small_page(name: &str, input: &serde_json::Value, content: &str, is_error: bool) -> Option<String> {
    if name != "read" || is_error { return None; }
    let small = input.get("offset").is_some() && input.get("limit").is_none() && content.chars().count() < PAGE_SMALL_CHARS;
    (small || content.ends_with(context::READ_CUT)).then(|| input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string())
}

fn repeat_note(n: usize) -> String {
    format!("[elisa: this exact call has now returned this same result {n} times in a row, so the turn was stopped. \
        Do not run it again unchanged: change the approach, or report what blocks you.]")
}

/// A failed model call as one line for the user: what happened and what to do (the server's words, cut short).
fn api_error_line(e: &str) -> String {
    let short = context::one_line(e, 200);
    let code = e.strip_prefix("HTTP ").and_then(|r| r.get(..3)).and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(401 | 403) => format!("the model server refused the request ({short}): check the connection's API key"),
        _ if crate::api::transient(e) && (e.contains("invalid character") || e.contains("JSON input") || e.contains("tool call")) =>
            format!("the model wrote a malformed tool call and the server could not read it, twice ({short}); ask again, or more simply"),
        Some(500..=599) => format!("the model server failed, twice ({short}); try again in a moment"),
        _ => format!("API error: {short}"),
    }
}

/// The error a call to a tool this request did not offer gets: what to do instead and a few names that exist (those
/// that look like it first: `sec_malware` → `sec_secrets`, …). `reason` = a reasoning tool the gate left out this turn.
/// The one-time retry after a reply with no visible text (`agent_loop`); sent for that request only.
const EMPTY_REPLY_NUDGE: &str = "[elisa] Your last reply had no text for the user, only the <state> card. Answer the user's \
    message in one plain sentence first; the <state> card goes after it.";

fn unknown_tool_text(name: &str, offered: &[ToolDef], reason: bool) -> String {
    const SHOW: usize = 6;
    let stem = |s: &str| s.split('_').next().unwrap_or(s).to_lowercase();
    let lname = name.to_lowercase();
    let like = |t: &str| { let t = t.to_lowercase(); stem(&t) == stem(&lname) || t.contains(&lname) || lname.contains(&t) };
    let mut names: Vec<&str> = offered.iter().map(|t| t.name.as_str()).filter(|t| like(t)).collect();
    names.extend(offered.iter().map(|t| t.name.as_str()).filter(|t| !like(t)));
    let more = names.len().saturating_sub(SHOW);
    names.truncate(SHOW);
    let list = format!("{}{}", names.join(", "), if more > 0 { format!(" (+{more} more)") } else { String::new() });
    if reason {
        format!("`{name}` is not offered on this turn: answer directly or use one of: {list}")
    } else {
        format!("unknown tool `{name}`: no tool by that name in this session (not built in or not switched on); nothing ran. Available: {list}")
    }
}

/// `[edit src/a.rs]`, `[bash cargo test → exit 0]`, `[write x → denied by user]`
fn tool_line(summary: &str, output: &str, is_error: bool) -> String {
    let s = context::one_line(summary, 100);
    let tail = output.trim_end().lines().last().unwrap_or("").trim();
    if s.starts_with("bash ") && tail.starts_with('[') && tail.ends_with(']') {
        return format!("[{s} → {}]", &tail[1..tail.len() - 1]);
    }
    if is_error { format!("[{s} → {}]", context::one_line(output, 60)) } else { format!("[{s}]") }
}

/// A user message with the user's own text (not one of Elisa's `[elisa] …` notes).
fn is_request(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.starts_with(context::NUDGE_PREFIX)))
}

fn texts(m: &Message) -> String {
    m.content.iter().filter_map(|b| match b { Block::Text { text } => Some(text.as_str()), _ => None }).collect::<Vec<_>>().join("")
}

/// A summary reply as notes: without a `<state>` card the model added out of habit, the summarizer's own
/// instructions and template text (`context::clean_notes`). Empty = unusable.
fn summary_text(content: Vec<Block>) -> String {
    let raw = texts(&Message::new(Role::Assistant, content));
    context::clean_notes(&context::split_state(&raw).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{SessionInfo, Status};

    /// records progress calls: Some(label) / None (cleared)
    #[derive(Default)]
    struct Rec(Vec<Option<String>>);
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
        fn progress(&mut self, p: Option<&Progress>) { self.0.push(p.map(|p| p.label.clone())); }
        fn pick_session(&mut self, _: &[SessionInfo]) -> Option<usize> { None }
        fn read_input(&mut self) -> Option<String> { None }
        fn turn_done(&mut self) {}
    }

    #[test]
    fn repeat_stops_small_pages_of_one_file_not_real_progress() {
        let read = |offset: usize| serde_json::json!({"path": "mario.html", "offset": offset});
        let small = "305\tx\n306\tx\n[trimmed 5000 chars]";
        let cut = format!("{}\n[trimmed 5000 chars] {}", "1\tx\n".repeat(400), context::READ_CUT);
        let full = "1\tx\n".repeat(400);
        // the recording: a new offset each time, so `see` alone never fires
        let mut r = Repeat::default();
        for o in [297, 305, 309] {
            assert_eq!(r.see("read", &read(o), small, false), 1);
            r.page("read", &read(o), small, false);
        }
        assert_eq!((r.max, r.what.as_str()), (REPEAT_MAX, "read mario.html"));
        assert!(r.why().contains("small results 3 times in a row"), "{}", r.why());
        // a cut read counts at any size
        let mut r = Repeat::default();
        for o in [297, 305, 309] { r.page("read", &read(o), &cut, false); }
        assert_eq!(r.max, REPEAT_MAX);
        // normal-size pages of a large file keep going
        let mut r = Repeat::default();
        for o in [1, 400, 800, 1200, 1600] { r.page("read", &read(o), &full, false); }
        assert_eq!(r.max, 0);
        // a read with its own limit got what it asked for
        let mut r = Repeat::default();
        for o in [10, 200, 300] { r.page("read", &serde_json::json!({"path": "a", "offset": o, "limit": 5}), small, false); }
        assert_eq!(r.max, 0);
        // another file, another call or an error ends the streak
        let mut r = Repeat::default();
        r.page("read", &read(1), small, false);
        r.page("read", &read(2), small, false);
        r.page("edit", &serde_json::json!({"path": "mario.html"}), "edited", false);
        r.page("read", &read(3), small, false);
        r.page("read", &serde_json::json!({"path": "b", "offset": 1}), small, false);
        r.page("read", &read(4), small, true);
        r.page("read", &read(5), small, false);
        assert_eq!(r.max, 2);
        // the identical-result stop keeps its own words
        let a = serde_json::json!({"command": "make test"});
        let mut r = Repeat::default();
        for _ in 0..REPEAT_MAX { r.see("bash", &a, "fail", true); }
        assert_eq!(r.why(), "`bash make test` returned the same result 3 times in a row");
    }

    #[test]
    fn a_failed_call_reads_as_one_clear_line() {
        let l = api_error_line("HTTP 500: invalid character '}' after object key (tried twice)");
        assert!(l.starts_with("the model wrote a malformed tool call"), "{l}");
        assert!(api_error_line("HTTP 502: bad gateway (tried twice)").starts_with("the model server failed, twice"));
        assert!(api_error_line("HTTP 401: invalid x-api-key").contains("API key"));
        assert_eq!(api_error_line("request failed: connection refused"), "API error: request failed: connection refused");
        assert!(!api_error_line(&"x".repeat(1000)).contains('\n'));
    }

    #[test]
    fn an_unknown_tool_gets_a_short_error_with_names_like_it_first() {
        let d = |n: &str| ToolDef { name: n.into(), description: String::new(), input_schema: serde_json::json!({}) };
        let offered: Vec<ToolDef> = ["read", "write", "edit", "bash", "grep", "glob", "web_search", "sec_secrets", "sec_deps"].iter().map(|n| d(n)).collect();
        let t = unknown_tool_text("sec_malware", &offered, false);
        assert!(t.starts_with("unknown tool `sec_malware`"), "{t}");
        assert!(t.contains("Available: sec_secrets, sec_deps, read, write, edit, bash (+3 more)"), "{t}");
        let t = unknown_tool_text("think", &offered, true);
        assert!(t.contains("not offered on this turn"), "{t}");
    }

    #[test]
    fn repeat_streak_needs_same_call_and_same_result() {
        let mut r = Repeat::default();
        let a = serde_json::json!({"command": "make test"});
        assert_eq!(r.see("bash", &a, "fail", true), 1);
        assert_eq!(r.see("bash", &a, "fail", true), 2);
        // a different result resets; so does a different call
        assert_eq!(r.see("bash", &a, "fail 2", true), 1);
        assert_eq!(r.see("read", &serde_json::json!({"path": "x"}), "fail 2", true), 1);
        assert_eq!(r.see("bash", &a, "fail 2", true), 1);
        assert_eq!(r.see("bash", &a, "fail 2", true), 2);
        assert_eq!(r.see("bash", &a, "fail 2", true), REPEAT_MAX);
        assert_eq!((r.max, r.what.as_str()), (REPEAT_MAX, "bash make test"));
        assert!(repeat_note(3).contains("3 times in a row"));
    }

    #[test]
    fn ast_line_says_which_side_is_cheaper() {
        // uitest: "181 tokens vs 30 for reading those 1 files whole · 0.2x fewer"
        assert_eq!(ast_line(3, 181, 30, 1), "ast: outline + read_symbol 3 calls · 181 tokens vs 30 for reading that file whole · 6.0x more tokens (small files: reading them whole is cheaper)");
        assert_eq!(ast_line(1, 200, 2400, 2), "ast: outline + read_symbol 1 call · 200 tokens vs 2.4K for reading those 2 files whole · 12.0x fewer tokens");
        assert_eq!(ast_line(2, 50, 0, 0), "ast: outline + read_symbol 2 calls · 50 tokens vs 0 for reading those 0 files whole");
    }

    #[test]
    fn mem_progress_is_cleared_when_the_call_ends() {
        let mut fe = Rec::default();
        let (r, shown) = with_progress(&mut fe, || { mem_progress("indexing 16 memories", Some(0.5)); mem_progress("indexing 16 memories", Some(1.0)); 7 });
        assert_eq!((r, shown), (7, true));
        assert_eq!(fe.0.first().cloned().flatten().as_deref(), Some("indexing 16 memories"));
        assert_eq!(fe.0.last(), Some(&None), "the line is cleared once elisa-mem returns: {:?}", fe.0);
        // nothing shown: nothing to clear; outside a call the callback goes nowhere
        let mut fe = Rec::default();
        assert_eq!(with_progress(&mut fe, || 1), (1, false));
        mem_progress("indexing 1 memories", Some(1.0));
        assert!(fe.0.is_empty());
    }
}
