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
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use elisa_mem::{HitKind, Memory, Role as MRole};

pub static CANCEL: AtomicBool = AtomicBool::new(false);
pub static AT_PROMPT: AtomicBool = AtomicBool::new(false);
pub static ASKING: AtomicBool = AtomicBool::new(false);

const USAGE: &str = "usage: elisa [-p PROMPT] [--yes] [--continue | --continue-open] [--provider NAME] [--base-url U [--type T]] [--model M] [--max-ctx N] [--cwd DIR] [--no-character] [--companion] [--serve-jsonl] [--no-checks] [--attach FILE]… [--theme dark|light] [--version] [-h]\n       elisa eval [--suite NAME|all] [--case ID] [--model M] [--json FILE] [--keep]\n       elisa plugin list | add PATH [--yes] | remove NAME | enable NAME | disable NAME | update NAME [--yes] | trust NAME [--yes]\n       elisa sandbox list [--json] | pull NAME|IMAGE|stable|project|all [--yes] [--json]";
const COMPACT_SYSTEM: &str = "You write short, concrete session notes for an assistant.";
const COMPACT_OUT: usize = 800;
/// the output cap of the isolated picture request (`look`): a description, or the answer to one question about it
const PICTURE_OUT: usize = 320;
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

thread_local! {
    /// inside `guarded` on this thread: the panic hook keeps the message for the user instead of printing it
    static CATCHING: Cell<bool> = const { Cell::new(false) };
    static PANIC_MSG: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The panic hook (installed once, around whatever hook was there, e.g. the TUI's terminal restore): a panic inside
/// `guarded` is kept as one line (message + where) for the error event; any other panic goes to the previous hook.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |i| {
            if !CATCHING.with(Cell::get) { return prev(i); }
            let msg = i.payload().downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| i.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
            let at = i.location().map_or(String::new(), |l| format!(" at {}:{}", l.file(), l.line()));
            PANIC_MSG.with(|m| *m.borrow_mut() = Some(format!("{}{at}", context::one_line(&msg, 300))));
        }));
    });
}

/// Runs `f`, turning a panic into Err(its message and location).
fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    install_panic_hook();
    let was = CATCHING.with(|c| c.replace(true));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    CATCHING.with(|c| c.set(was));
    r.map_err(|_| PANIC_MSG.with(|m| m.borrow_mut().take()).unwrap_or_else(|| "panic".into()))
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
            "--continue-open" => { (a.cont, a.cont_open) = (true, true); Ok(()) }
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
            "--attach" => val("--attach").map(|v| a.attach.push(std::env::current_dir().map(|d| d.join(&v)).unwrap_or_else(|_| PathBuf::from(&v)))),
            "--theme" => val("--theme").and_then(|v| if v == "dark" || v == "light" { a.theme = Some(v); Ok(()) } else { Err(format!("bad --theme: {v} (dark | light)")) }),
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
    // --base-url: that server. With --type that kind of server with config.toml's settings and
    // no saved connection; without, the active connection's kind, model and size stay (Config::point_at)
    let kind = args.base_url.as_ref().and(args.kind.as_deref());
    let forced = args.provider.clone().or_else(|| kind.map(|_| crate::provider::NONE.to_string()));
    let mut cfg = match Config::load_as(&cwd, forced.as_deref()) { Ok(c) => c, Err(e) => { fe.error(&e); return 2; } };
    // Elisa's own result cache (~/.elisa/cache), before any MCP server starts (they get it in their environment)
    crate::cache::configure(cfg.cache, cfg.cache_max_mb);
    if let Some(u) = args.base_url.clone() {
        cfg.point_at(&u, kind);
        // an address with no --type and no saved connection: Elisa finds out what the server is itself. A recognised
        // model server (Ollama, llama.cpp, LM Studio, vLLM, …) or cloud host is spoken to as what it is; anything else
        // stays the gateway, as before
        if kind.is_none() && cfg.provider.is_empty() && cfg.transport == crate::provider::Transport::Gateway {
            use crate::backend::Kind;
            let found = crate::backend::detect(&crate::provider::Transport::OpenAi, &u, &cfg.model, "").kind;
            let t = match found {
                Kind::Unknown | Kind::Gateway | Kind::OpenAiCompat | Kind::Mcp => None,
                Kind::Anthropic | Kind::AnthropicCompat => Some(crate::provider::ANTHROPIC),
                _ => Some(crate::provider::OPENAI),
            };
            if let Some(t) = t {
                cfg.point_at(&u, Some(t));
                if cfg.api_key == "dummy" { cfg.api_key.clear(); }
            }
        }
    }
    if let Some(m) = args.model.clone() { cfg.model = m; }
    if args.max_ctx.is_some() { cfg.max_ctx = args.max_ctx; }
    // no model configured: say so once and send nothing (Elisa never picks one)
    let no_model = cfg.model.trim().is_empty();
    if no_model && args.prompt.is_some() { fe.error(crate::api::NO_MODEL); return 2; }
    let mut client = Client::new(&cfg);
    client.set_project(&cwd);
    // which server this is and what it can do (`backend.rs`): cloud hosts by name, others by a few cheap GETs
    let caps = crate::backend::detect(&cfg.transport, &cfg.base_url, &cfg.model, &cfg.api_key);
    let mut max_ctx = cfg.max_ctx.or_else(|| client.server_max_ctx()).or(caps.ctx).unwrap_or(200_000);
    // a window the server fixed (llama.cpp -c, LM Studio's loaded length, vLLM max_model_len) cannot be outgrown
    if let Some(has) = caps.ctx.filter(|&has| caps.ctx_hard && has < max_ctx) {
        fe.info(&format!("context: {} runs with {has}, not {max_ctx}; using {has}", caps.kind.label()));
        max_ctx = has;
    }
    if let Some(most) = caps.model_max.filter(|&most| most < max_ctx) {
        fe.info(&format!("context: {} reads at most {most} tokens, not {max_ctx}; using {most}", cfg.model));
        max_ctx = most;
    }
    client.set_caps(caps, max_ctx);
    if !cfg.provider.is_empty() {
        // a connection's size cannot grow the gateway's window: past it, every request would overflow
        if let Some(has) = client.server_max_ctx().filter(|&has| has < max_ctx) {
            fe.info(&format!("context: the gateway runs with {has}, not {max_ctx}; using {has} (restart it with a larger --max-ctx to go higher)"));
            max_ctx = has;
        }
        fe.info(&match &cfg.leaves { Some(h) => format!("connection {}: what this session reads goes to {h}", cfg.provider), None => format!("connection {}: stays on this machine", cfg.provider) });
    }

    let mut s = Session::new(fe, cfg, client, cwd, max_ctx, args.prompt.is_none(), args.yes);
    // an `openai` connection that is Ollama talks to its native API, which (unlike its OpenAI endpoint) keeps the
    // window (`Client::set_caps`)
    let ollama = s.client.caps().ollama.clone();
    if let Some(caps) = &ollama {
        s.fe.info(&format!("Ollama: native API, window {} tokens · {}", s.max_ctx, caps.join(", ")));
    }
    if s.cfg.transport == crate::provider::Transport::OpenAi && ollama.is_none() {
        s.fe.info(&format!("backend: {} ({})", s.client.caps().kind.label(), s.client.caps().detected_by));
    }
    if s.client.text_tools() { s.fe.info("this server or model has no native tool calling: tools are described in the prompt and calls read from the reply"); }
    s.vision = crate::provider::vision(&s.cfg.transport, ollama.as_deref(), s.cfg.vision.or(s.client.caps().vision.filter(|_| s.cfg.transport == crate::provider::Transport::OpenAi)));
    s.client.set_vision(s.vision);
    s.checks_off = args.no_checks;
    s.doc_paths = args.attach.iter().map(|p| p.display().to_string()).collect();
    s.open_memory();
    let mut resumed = None;
    if args.cont {
        // the saved session's mtime: when its last turn ended (the situation line's "back after …")
        // --continue-open (the app's restart on a model switch) takes the open conversation: the newest session, whatever
        // was said in it (BUGS #63); plain --continue skips a dead session (`newest_real_session`)
        let newest = if args.cont_open { s.store.newest_session() } else { s.store.newest_real_session() }.map(|(_, p)| p);
        let at = newest.as_deref().and_then(crate::situation::mtime);
        if let Some((h, notes)) = newest.as_deref().and_then(|p| Some((s.store.load_session(p).map(context::loaded)?, s.store.load_session_notes(p)))) {
            resumed = Some(h.len());
            if let Some(p) = &newest { s.slot_restore(p); }
            s.last_failed = crate::situation::last_turn_flags(&h).1;
            (s.history, s.last_turn_at) = (h, at);
            s.adopt_notes(notes);
            let id = newest.as_deref().and_then(|p| p.file_stem()?.to_str()?.parse().ok());
            s.restore_docs(id);
            s.load_topics(id);
        }
    }
    // only a continued session carries the last state card and works on its notes. A new one starts clean: no earlier
    // notes at all; its goal comes from what the user asks in this conversation (2026-11-09: a 2B model answered
    // "hey" with an earlier session's task when the old notes were carried, even marked as background). Earlier
    // facts reach a new session only through memory recall, when the request calls for them. Notes are per session
    // (`.elisa/notes/<id>.md`): a new session has none of its own, and an earlier session's stay as they were.
    if resumed.is_some() { s.carry_card(); s.notes_bg = false; } else { s.notes = None; }
    match args.prompt {
        Some(p) => {
            match (args.cont, resumed) {
                (true, Some(n)) => s.fe.info(&format!("resumed last session ({n} messages)")),
                (true, None) => s.fe.info("no saved session to continue"),
                _ => {}
            }
            s.budget_warning();
            let code = s.run_print(p);
            s.slot_save("quit");
            s.end_memory();
            code
        }
        None => {
            s.banner(resumed);
            if resumed.is_some() { s.show_history(); }
            if no_model { s.fe.error(crate::api::NO_MODEL); }
            s.budget_warning();
            // the card number is internal: say it plainly, and only when a conversation was really continued
            if resumed.is_some() && s.card_id.is_some() { s.fe.info("continuing your last conversation"); }
            // last, under the restored turns: the model that answers now (a switch restarts with --continue)
            if resumed.is_some() { let c = s.client.caps(); s.fe.info(&context::now_on(&s.cfg.model, s.vision, !s.client.text_tools(), c.thinking)); }
            if args.cont && resumed.is_none() { s.fe.info("no saved session to continue"); }
            // a plugin server that could not start for a missing image: ask once whether to download it
            s.offer_plugin_images();
            s.run_repl();
            s.slot_save("quit");
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
    /// pictures that came with the input line just read: the model is asked about them alone (`look`), and its
    /// answer goes into this turn's user message as text
    attach: Vec<Block>,
    /// the pictures of the last turn that had some, and whether the next turn asks about them again (`look`)
    last_pics: Vec<Block>,
    pics_next: bool,
    /// a session was just resumed: its first turn carries the user's facts in the third person (`resumed_facts`)
    resumed_facts: bool,
    /// the values of the user's facts that recall brought back this session (`claimed_facts_retry`)
    recalled_values: Vec<String>,
    /// documents that came with the input line just read (`attach.rs`): each one's reference line (the only thing
    /// the history keeps: `[Attached: report.pdf, 38 pages · .elisa/uploads/…]`) and whether it is large (then the
    /// turn is routed to the `docs` tools)
    docs: Vec<(String, bool)>,
    /// this turn's document text (the whole text, an overview or the parts the question needs): added to the turn's
    /// user message in this turn's requests only, never to the history, the saved session or a fold
    doc_req: Option<String>,
    /// its estimated tokens (counted with system + tools while the turn runs)
    doc_req_tokens: usize,
    /// documents waiting for the next turn: the app's `send` {documents} or `-p … --attach FILE`
    doc_paths: Vec<String>,
    /// documents attached in this session (text redacted), newest last: every later turn about one picks its parts
    /// again for that request (`doc_follow_up`)
    kept_docs: Vec<KeptDoc>,
    /// this turn carried document text: (document name, the block to retry with when the reply refuses it)
    doc_retry: Option<(String, String)>,
    /// every state card of this conversation with its topic key (`topics.rs`): a turn sends at most the one its
    /// words pick, never one of another topic
    topic_cards: Vec<crate::topics::Card>,
    /// this turn's topic (between turns: the last turn's), indexes into `kept_docs` and `topic_cards`
    topic: crate::topics::Pick,
    /// the kept document attached with the input line just read (it is this turn's topic)
    doc_new: Option<usize>,
    /// every document attached with the input line just read was refused (password-protected, not a document): the
    /// line that tells the model so, in the turn's own message; the turn gets no earlier document, no recall, no memory
    doc_refused: Option<String>,
    /// this turn's picked card as a `[state]` block: in the turn's user message for its requests only, like `doc_req`
    card_req: Option<String>,
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
    /// this input line already sent its turn_done (a panic after it must not send a second one)
    done_sent: bool,
    /// the current turn's steps in plain words for the display (src/activity.rs)
    acts: crate::activity::Turn,
    /// the last recall / session save was slow: show a progress line for the next one
    recall_slow: bool,
    save_slow: bool,
    /// this conversation's last turn (unix seconds; `--continue`: the saved session's mtime) and whether it failed:
    /// the situation line of a small-talk turn (`situation.rs`)
    last_turn_at: Option<i64>,
    last_failed: bool,
    /// this turn is small talk routed as chat ("hey", "thanks", "ok"): a tool call is not run or widened to, but
    /// asked again in words (`agent_loop`)
    small_talk_turn: bool,
    /// the note of a one-time words-only request with no tools (`agent_loop` starts with it; `denied_claim_retry`)
    retry_note: Option<String>,
    /// latest state card: text, memory id, and the exchange it came from (fresh = from the last <= 2 exchanges)
    card: Option<String>,
    card_id: Option<u64>,
    card_ex: Option<usize>,
    /// completed exchanges (user turn + final reply) in this run
    exchanges: usize,
    /// card parsed from the current turn's final reply (None + `card_broken` = none or broken)
    new_card: Option<String>,
    card_broken: bool,
    /// this turn's message came with a picture the model cannot see (`take_attached`; `no_picture_line`)
    pic_refused: bool,
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
    /// requests in a row that had nothing to do with `goal` (`context::goal_drift`)
    goal_misses: u8,
    /// `goal` came from an explicit goal statement ("our goal for this session: …"): it does not drift
    goal_stated: bool,
    /// notes facts this run already found in its conversation (`context::fact_key`): a later fold keeps them though
    /// their turns are folded away (`merge_kept`)
    facts_ok: std::collections::HashSet<String>,
    /// the current request in the user's words (one line)
    user_req: String,
    /// the gateway's prefill-only warm-up after a fold (config `warm`): WARM_UNKNOWN until /health or the first try
    /// says; a background thread sets it from the answer
    warm: Arc<AtomicU8>,
    /// the reasoning tools (`think` / `steps` / `step_done` / `calc`, `mcp_reason.rs`) are in this turn's request:
    /// decided once at turn start (`reason_turn`), so the tool list never changes mid-turn
    reason_on: bool,
    /// this turn's intent route (`route.rs`, config `route`): the tool groups its requests carry
    route: crate::route::Route,
    /// the hosted intent router (`router.rs`; providers.toml `[router]`): off without a key
    router: crate::router::Router,
    /// the router's answer for this turn; None = no router or a miss, and the word rules decide as before
    routed: Option<crate::router::Answer>,
    /// a picture was refused in this session (the router's `image_refused`)
    pic_refused_seen: bool,
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
    /// the plugin state file as last seen (`plugins::state_stamp`): a change made outside this session is synced
    plugin_stamp: Option<(std::time::SystemTime, u64)>,
    /// (tool, plugin): tools of plugins removed or switched off outside this session, for a clear refusal
    plugin_gone_tools: Vec<(String, String)>,
    /// the project's own commands and plugins, waiting for the one-time trust prompt (None: none, or loaded)
    proj_pending: Option<plugins::Project>,
    proj_infos: Vec<plugins::Info>,
    /// the user said no to the project's trust prompt in this session: not asked again until /plugin trust project
    proj_declined: bool,
}

struct ReqRun { ts: u64, t0: Instant, prompt: String }

/// a document attached in this session: its text, the last question about it, and the turns since it was last
/// referred to (`Session::doc_follow_up`)
struct KeptDoc { doc: crate::attach::Doc, query: String, topic: crate::topics::DocTopic }

impl KeptDoc {
    fn new(doc: crate::attach::Doc, query: String) -> Self {
        let mut topic = crate::topics::DocTopic::new(&doc.name, &crate::attach::title(&doc), &doc.text);
        if doc.part == "page" { topic.pages = doc.parts.len(); }
        KeptDoc { doc, query, topic }
    }
}

enum TurnEnd { Done, Stopped, Failed(String) }

fn end_name(e: &TurnEnd) -> &'static str {
    match e { TurnEnd::Done => "done", TurnEnd::Stopped => "stopped", TurnEnd::Failed(_) => "failed" }
}

impl<'f> Session<'f> {
    /// `/model`: what this session talks to, and the saved connections
    /// llama.cpp with `--slot-save-path`: save the slot's processed prompt (this session's KV) to the session's slot
    /// file in the server's slot dir, and remember which file is whose (`.elisa/slots.json`). On /clear, quit and a
    /// session switch. Any other server, or an empty session: nothing, quietly.
    fn slot_save(&mut self, why: &str) {
        if !self.client.caps().slot_save || self.history.is_empty() { return; }
        let id = context::session_id();
        let mut map = crate::backend::SlotMap::load(&self.store.dir);
        let project: String = self.client.project().chars().take(8).collect();
        let file = map.file_for(&id, self.client.base(), &project);
        match self.client.slot_save(&file) {
            Ok((n, ms)) => {
                let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                map.entries.insert(id, crate::backend::SlotEntry { file: file.clone(), server: self.client.base().to_string(), model: self.client.model().to_string(), tokens: n, at });
                if let Err(e) = map.save(&self.store.dir) { self.fe.error(&format!("slot map: {e}")); }
                self.fe.info(&format!("[llama.cpp: saved this session's {n} processed tokens to slot file {file} ({why}, {})]", ui::dur(ms / 1000.0, 1)));
            }
            Err(e) => self.fe.info(&format!("[llama.cpp: slot not saved: {e}]")),
        }
    }

    /// The saved session at `path` comes back (`/resume`, `--continue`): its slot file, if this server saved one for
    /// this model, goes back into the slot, so the next request reuses that prefix instead of reading it again.
    fn slot_restore(&mut self, path: &Path) {
        if !self.client.caps().slot_save { return; }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { return };
        let map = crate::backend::SlotMap::load(&self.store.dir);
        let Some(e) = map.find(id, self.client.base(), self.client.model()).cloned() else { return };
        match self.client.slot_restore(&e.file) {
            Ok((n, ms)) => self.fe.info(&format!("[llama.cpp: restored {n} processed tokens from slot file {} ({})]", e.file, ui::dur(ms / 1000.0, 1))),
            Err(err) => self.fe.info(&format!("[llama.cpp: slot not restored: {err}]")),
        }
    }

    fn model_command(&mut self) {
        use crate::provider::{path_in, summary, Providers};
        self.fe.info(&format!("model {} @ {} · connection {}", self.cfg.model, self.cfg.base_url, if self.cfg.provider.is_empty() { "none (config.toml)" } else { &self.cfg.provider }));
        let mut caps = self.client.caps().clone();
        if self.client.text_tools() { caps.tools = crate::backend::Tools::Text; }
        for l in crate::backend::lines(&caps, self.max_ctx) { self.fe.info(&l); }
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
        // no server process starts here: each starts on the first call to one of its tools (`Host::open`)
        let (mut mcp, mcp_notes) = crate::mcp_host::Host::open(&servers, &|n| own.iter().any(|o| o == n), home.as_deref().map(crate::pins::lists_in));
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
        let plugin_stamp = home.as_deref().and_then(plugins::state_stamp);
        let router = crate::router::Router::new(cfg.router.clone());
        let mut s = Session {
            fe, cfg, client, store, tctx, cwd, max_ctx, vision: false, attach: Vec::new(), last_pics: Vec::new(), pics_next: false, resumed_facts: false, recalled_values: Vec::new(), docs: Vec::new(), doc_req: None, doc_req_tokens: 0, doc_paths: Vec::new(), kept_docs: Vec::new(), doc_retry: None, topic_cards: Vec::new(), topic: Default::default(), doc_new: None, doc_refused: None, card_req: None, mcp, tools: Vec::new(), tools_tokens: 0, project, notes, notes_bg: true,
            system: String::new(), system_tokens: 0, history: Vec::new(), real: None, calib: None, input: String::new(), interactive, declined_images: Vec::new(), yolo: yes, always,
            mem: None, turn_no: 0, folded: 0, fold_ids: Vec::new(), fold_retry_at: 0, digest_only: false, in_turn: false, folded_this_turn: false, tool_lines: Vec::new(), unsent: false, done_sent: false, acts: crate::activity::Turn::new(),
            recall_slow: false, save_slow: false, last_turn_at: None, last_failed: false, small_talk_turn: false, retry_note: None, index_rate: None, unindexed: 0, watch,
            card: None, card_id: None, card_ex: None, exchanges: 0, new_card: None, card_broken: false, pic_refused: false, checks_off: false, outside_mode: outside::Mode::Ask, outside_ok: Vec::new(), ast_stats: AstStats::default(), cards: 0,
            req: None, stats: metrics::Stats::default(),
            card_log: Vec::new(), goal: None, goal_misses: 0, goal_stated: false, facts_ok: Default::default(), user_req: String::new(), warm: Arc::new(AtomicU8::new(WARM_UNKNOWN)),
            reason_on: false, route: crate::route::Route::default(), router, routed: None, pic_refused_seen: false, gate: (false, false), ledger: context::Ledger::default(), audit, answer: None,
            home, file_cmds: Vec::new(), cmd_user: Vec::new(), cmd_plugins: pl.commands, cmd_project: Vec::new(),
            plugin_stamp, plugin_gone_tools: Vec::new(), plugin_infos: pl.infos, plugin_servers, proj_pending: None, proj_infos: Vec::new(), proj_declined: false,
        };
        s.load_commands();
        // the app's plugin pane: what is installed, from the start (nothing installed: no event; `/plugin list` asks)
        if !s.plugin_infos.is_empty() || !s.proj_infos.is_empty() { s.plugins_event(); }
        s.outside_mode = outside::Mode::parse(&s.cfg.outside).unwrap_or(outside::Mode::Ask);
        if s.cfg.gate_tools == "off" { s.gate = (true, true); }
        if s.cfg.route == "off" { s.route = crate::route::Route::all(); }
        s.rebuild_system();
        s
    }

    /// The only place the system prompt and tools change: startup, compaction, /clear, /compact, /forget, and a turn
    /// whose route (`route.rs`, config `route`) or reasoning / relevance gate differs from the last turn's.
    fn rebuild_system(&mut self) {
        (self.system, self.tools) = (context::system_prompt(&self.cwd, self.cfg.system_prompt_file.as_deref(), self.project.as_deref(), None), tools::defs_for(self.cfg.ast));
        // stable tools first (Elisa's own, then every MCP server that is always offered); the per-turn ones after them,
        // in a fixed order (`docs`, `sec`, then the reasoning tools), so a flip only changes the end of the list
        let defs = self.mcp.defs();
        let m = &self.mcp;
        let gated = |name: &str| m.reason_tool(name) || m.gated_server(name).is_some();
        let routing = self.cfg.route == "auto";
        self.tools.extend(defs.iter().filter(|t| !gated(&t.name)).cloned());
        for (i, srv) in crate::mcp_reason::GATED.iter().enumerate() {
            // routed: the documents / security tools are route groups (`route.rs`), not the old relevance gate
            if routing || [self.gate.0, self.gate.1][i] { self.tools.extend(defs.iter().filter(|t| m.gated_server(&t.name) == Some(*srv)).cloned()); }
        }
        // intent routing: only the tool groups this turn's words call for, in the same fixed order (a chat turn: none)
        if routing {
            let r = &self.route;
            self.tools.retain(|t| {
                let srv = m.server_of(&t.name);
                let g = crate::route::tool_group(&t.name, srv);
                if g == crate::route::OTHER { srv.is_some_and(|s| r.others.iter().any(|o| o == s)) } else { r.groups.has(g) }
            });
            self.system = crate::route::system_for(&self.system, self.route.groups);
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
        // the model's detected facts, one line (fixed per connection, so the prompt stays byte-stable within a session)
        let c = self.client.caps();
        let facts = crate::situation::model_facts(&self.cfg.model, c.vision == Some(true), c.tools == crate::backend::Tools::Native, c.thinking, self.max_ctx);
        if !self.system.ends_with('\n') { self.system.push('\n'); }
        self.system.push_str(&facts);
        self.system.push('\n');
        // a tiny window (`context::tiny_window`, from its size alone): a few lines of prompt and no tools, or the
        // prompt alone fills the window (BUGS #41, #42)
        if self.tiny() { (self.system, self.tools) = (context::TINY_SYSTEM.to_string(), Vec::new()); }
        // + the chat template's tool preamble (Qwen: ~236 tokens, measured with count_tokens)
        self.tools_tokens = if self.tools.is_empty() { 0 } else { estimate_tokens(&serde_json::to_string(&self.tools).unwrap_or_default()) + TOOL_PREAMBLE };
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
        // `read` of an outside document (a PDF, a saved web page …) is fenced too (`untrusted::read_source`)
        let doc = || if name == "read" { crate::untrusted::read_source(input, &content) } else { None };
        let Some(src) = self.untrusted_source(name, input).or_else(doc) else { return content };
        let (fenced, hit) = crate::untrusted::fence(&src, &content);
        if let Some(h) = hit {
            self.fe.info(&format!("[{name}: text that looks like a prompt injection (\"{h}\"); marked for the model as data]"));
        }
        fenced
    }

    /// the window is tiny (`context::tiny_window`): short prompt, no tools, no header, no hint lines
    fn tiny(&self) -> bool { context::tiny_window(self.max_ctx) }

    fn fixed(&self) -> usize { self.system_tokens + self.tools_tokens + self.doc_req_tokens + self.card_req.as_deref().map_or(0, estimate_tokens) }
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

    /// A resumed conversation's earlier turns on screen (`context::earlier_turns`): the app shows them as blocks.
    fn show_history(&mut self) {
        let items = context::earlier_turns(&self.history, &self.cwd, context::EARLIER_TURNS);
        self.fe.history(&items);
    }

    fn banner(&mut self, resumed: Option<usize>) {
        let b = Banner {
            model: self.cfg.model.clone(), base_url: self.cfg.base_url.clone(), cwd: self.cwd.display().to_string(),
            max_ctx: self.max_ctx, notes: self.notes.clone(), resumed, vision: self.vision,
        };
        self.fe.banner(&b);
    }

    /// Elisa's answer for a turn about a picture the model cannot see (`context::no_picture_line`): a model without
    /// vision and no picture in the conversation.
    fn no_picture_line(&mut self, input: &str) -> Option<String> {
        let now = std::mem::take(&mut self.pic_refused);
        let pics = self.history.iter().any(|m| m.content.iter().any(|b| match b { Block::Image { .. } => true, Block::Text { text } => context::is_picture_block(text), _ => false }));
        if self.vision || pics || !self.attach.is_empty() { return None; }
        let vi = crate::reply_lang::detect(input) == Some(crate::reply_lang::Lang::Vi);
        let model = self.cfg.model.clone();
        let mine = |vi: bool| context::no_picture_line("this", &model, vi, true, false).unwrap_or_default();
        let last = self.history.last().filter(|m| m.role == Role::Assistant).map(texts).unwrap_or_default();
        let after = last.trim() == mine(false) || last.trim() == mine(true);
        // the router's `refused_picture` replaces the word check when it answered (BUGS #36)
        match self.router_says("refused_picture") {
            Some(asked) => (now || asked).then(|| context::no_picture_text(&model, vi)),
            None => context::no_picture_line(input, &model, vi, now, after),
        }
    }

    /// the pictures sent with the line just read; a model without vision gets none, and the user is told why
    fn take_attached(&mut self) -> Vec<Block> {
        let imgs = self.fe.take_images();
        // refused by the display already (the app blocks pictures at attach on a model without vision, BUGS #61)
        if self.fe.take_refused_images() > 0 && !self.vision { self.pic_refused = true; }
        if imgs.is_empty() || self.vision { return imgs; }
        self.fe.error(&format!("{} can't see images, so the {} attached {} not sent. Choose a model with vision in Model Connections, or set vision = true on this connection if it has it.",
            self.cfg.model, imgs.len(), if imgs.len() == 1 { "image was" } else { "images were" }));
        self.pic_refused = true;
        Vec::new()
    }

    /// The facts the user told about themself in this (resumed) history and not yet in its notes, in the third person
    /// (`context::user_facts`: "The user's name is Linh").
    fn resumed_facts(&self) -> Vec<String> {
        let mut facts: Vec<String> = Vec::new();
        for m in self.history.iter().filter(|m| m.role == Role::User) {
            for b in &m.content {
                if let Block::Text { text } = b {
                    if context::is_picture_block(text) || crate::attach::is_block(text) { continue; }
                    for f in context::user_facts(context::request_body(text)) { if !facts.contains(&f) { facts.push(f); } }
                }
            }
        }
        context::facts_not_in(self.notes.as_deref(), facts)
    }

    /// The pictures this turn asks about (BUGS #37, #17), and whether they came with it: the ones sent with it, else,
    /// on the one turn right after a picture turn, that turn's pictures again. No wording is read: a follow-up ("and
    /// its ears?") needs the picture, and telling one from a change of subject by its words would differ by language
    /// and model, so the next turn always looks once more (one short extra request), and the pictures are dropped
    /// after it. An answer there that tells nothing about the picture is not carried (`look`).
    fn turn_pictures(&mut self) -> Option<(Vec<Block>, bool)> {
        let new = std::mem::take(&mut self.attach);
        let again = std::mem::take(&mut self.pics_next);
        if !new.is_empty() {
            (self.last_pics, self.pics_next) = (new.clone(), true);
            return Some((new, true));
        }
        let old = std::mem::take(&mut self.last_pics);
        (again && self.vision && !old.is_empty()).then_some((old, false))
    }

    /// The isolated picture request (`context::picture_request`): the same model, no system prompt, no history, only
    /// the pictures and the user's question. Its answer is the turn's picture text (`context::picture_block`), so the
    /// conversation itself never carries a picture: no re-sending every turn (BUGS #17: the first token took ~30 s by
    /// turn 7), and a small vision model that reads pictures only when asked alone still reads them (BUGS #37).
    /// An answer that tells nothing about the picture (`context::picture_non_answer`: a refusal, "I'm not sure what
    /// you're asking") is never carried as what the picture shows (BUGS #57, #58: granite3.2-vision:2b's "I'm sorry,
    /// but I can't assist with that." to the read ask after a picture became that turn's reply): on the turn the
    /// pictures came with, they are asked about once more with the plain describe question, and still nothing gives
    /// `PICTURE_UNSEEN`; on the turn after, there is no picture text (Ok(None)). Ok(Some((block, answer))): `answer`
    /// is the picture's real answer, None for `PICTURE_UNSEEN`.
    fn look(&mut self, question: &str, pics: &[Block], new: bool) -> Result<Option<(String, Option<String>)>, TurnEnd> {
        let n = pics.len();
        let what = if n == 1 { "the picture".to_string() } else { format!("the {n} pictures") };
        let aid = self.acts.begin(&mut *self.fe, &crate::activity::Phrase::new("model", "picture", "", format!("Looking at {what}"), format!("Looked at {what}")));
        CANCEL.store(false, Ordering::SeqCst);
        self.client.set_kind("picture");
        let question = context::request_body(question).trim().to_string();
        let mut asks = vec![question.clone()];
        // the plain describe question as the second try (never the same question twice)
        if new && !question.is_empty() { asks.push(String::new()); }
        let last = asks.len() - 1;
        for (i, q) in asks.into_iter().enumerate() {
            let msgs = context::picture_request(&q, pics);
            let req = Request { system: "", tools: &[], messages: &msgs, max_tokens: PICTURE_OUT.min(self.cfg.max_output) };
            let t0 = Instant::now();
            let res = self.client.send(&req, &CANCEL, &mut |_| {});
            self.debug(&format!("[looked at {what} alone in {:.1}s]", t0.elapsed().as_secs_f64()));
            match res {
                Ok(r) if r.stop_reason == "cancelled" => {
                    self.acts.end_with(&mut *self.fe, aid, "cancelled", true);
                    self.unsent = true;
                    return Err(TurnEnd::Stopped);
                }
                Ok(r) => {
                    let answer = context::split_state(&texts(&Message::new(Role::Assistant, r.content))).0.trim().to_string();
                    if !context::picture_non_answer(&answer) {
                        self.acts.end_with(&mut *self.fe, aid, "", false);
                        return Ok(Some((context::picture_block(&answer), Some(answer))));
                    }
                    self.debug(&format!("[the picture answer told nothing about it: {}]", context::one_line(&answer, 80)));
                }
                // the server failed on this question (moondream:1.8b: "prediction aborted, token repeat limit
                // reached" on "Describe this picture in one sentence.", read at once with the plain question): the
                // next question, or on the turn after, no picture text (that turn's own ask goes on)
                Err(e) if i < last || !new => self.debug(&format!("[the picture request failed: {}]", api_error_line(&e))),
                Err(e) => {
                    self.acts.end_with(&mut *self.fe, aid, "failed", true);
                    self.unsent = true;
                    return Err(TurnEnd::Failed(api_error_line(&e)));
                }
            }
        }
        self.acts.end_with(&mut *self.fe, aid, "", false);
        // nothing came back that tells about the picture: said so on its own turn, so the model does not make it up
        Ok(new.then(|| (context::picture_block(context::PICTURE_UNSEEN), None)))
    }

    /// The safety net on the turn a picture came with (BUGS #59: granite3.2-vision:2b, picture text "cat", replied
    /// "I'm sorry, I don't have the ability to view or interpret images. However, … shows a cat."): a reply that is a
    /// refusal (`context::picture_non_answer`) while the picture's own answer told what it shows is replaced by that
    /// answer, the model's own words about the picture.
    fn picture_refusal_fix(&mut self, answer: &str) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        if !self.tool_lines.is_empty() || !context::picture_non_answer(&texts(last)) { return; }
        self.history.pop();
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(answer); }
        self.history.push(Message::assistant_text(answer));
        self.debug("[the reply said it could not see the picture; showed the picture's own answer]");
    }

    /// The documents sent with the line just read (`attach.rs`; images need vision, documents do not): each one copied
    /// into the session's upload area and read now, before the model is asked, and its block built for `question` (the
    /// user's words) within the turn's document budget: the whole text, an overview, or the parts that answer it. One
    /// that cannot be attached (too large, not a document) is left out with the reason. With no new document, a
    /// question about one attached earlier gets the parts that answer it (`attach::follow_up`).
    fn read_documents(&mut self, question: &str) {
        self.docs.clear();
        self.doc_retry = None;
        self.doc_new = None;
        self.doc_refused = None;
        let mut refused: Vec<String> = Vec::new();
        let mut paths = std::mem::take(&mut self.doc_paths);
        paths.extend(self.fe.take_documents());
        let question = context::request_body(question).trim();
        if paths.is_empty() { return; }
        let budget = crate::attach::budget(self.max_ctx, self.req_history_tokens());
        let mut req: Vec<String> = Vec::new();
        if paths.len() > crate::attach::DOCS_MAX {
            self.fe.error(&format!("{} documents attached: at most {} per message; the first {} are read", paths.len(), crate::attach::DOCS_MAX, crate::attach::DOCS_MAX));
            paths.truncate(crate::attach::DOCS_MAX);
        }
        let budget = budget / paths.len();
        let root = outside::root(&self.cwd);
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        for p in paths {
            let src = PathBuf::from(&p);
            let src = if src.is_absolute() { src } else { self.cwd.join(src) };
            let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
            // "Reading report.pdf…" in the activity line (src/activity.rs), "Read report.pdf · 12 pages" after
            let aid = self.acts.begin(&mut *self.fe, &crate::activity::reading_doc(&name, None));
            let cache_before = crate::cache::session_hits();
            let r = crate::attach::read(&root, context::run_id(), &src);
            if !crate::cache::new_hits(&cache_before, &crate::cache::session_hits()).is_empty() { self.debug(&format!("[cache hit: docs · {name}]")); }
            match &r {
                Ok(d) => self.acts.end_with(&mut *self.fe, aid, &d.units, false),
                Err(_) => self.acts.end_with(&mut *self.fe, aid, "not attached", true),
            }
            match r {
                Ok(mut d) => {
                    self.scrub_doc(&mut d, mode);
                    let (block, pick) = crate::attach::block_for(&d, question, budget, &[]);
                    // no tool definitions for it: the text is in the turn, and on the measured 2B model the docs tools made
                    // the reply a state card only (one more call); the routed words still decide the turn's tools
                    let large = false;
                    if let Some(it) = &d.info {
                        let mut v = crate::attach::info_json(&p, it, self.max_ctx, estimate_messages(&self.history));
                        v["line"] = serde_json::json!(crate::attach::check_line(it, match &pick { crate::attach::Pick::Whole => "whole", crate::attach::Pick::Overview => "overview", _ => "selection" }));
                        self.fe.doc_info(&v);
                    }
                    self.debug(&format!("[attached {} · {} · ~{} tokens · budget {budget} · {:?} · block ~{} tokens]", d.name, d.units, d.tokens, pick, estimate_tokens(&block)));
                    // the retry block when the reply refuses: the overview (or the same block for a whole text)
                    let retry = if matches!(pick, crate::attach::Pick::Parts(_)) { crate::attach::render(&d, &crate::attach::Pick::Overview, budget) } else { block.clone() };
                    self.doc_retry = Some((d.name.clone(), retry));
                    if pick != crate::attach::Pick::Whole { let _ = crate::attach::prefetch_embeddings(&d); }
                    self.docs.push((crate::attach::reference(&d), large));
                    req.push(block);
                    self.kept_docs.push(KeptDoc::new(d, question.to_string()));
                    self.doc_new = Some(self.kept_docs.len() - 1);
                }
                Err(e) => { self.fe.error(&format!("not attached: {e}")); refused.push(e); }
            }
        }
        if !req.is_empty() { self.set_doc_req(Some(req.join("\n\n"))); }
        // nothing could be read: the model hears it in this turn's message, so it never answers from an earlier file
        // (measured on qwen3-vl:2b: a locked PDF got "The PDF says the code word is WORD8", from the n8.txt before it)
        else if !refused.is_empty() { self.doc_refused = Some(crate::attach::refused_note(&refused)); }
    }

    /// outside text, as tool output: a secret in an attached document is hidden from the model (config `redact`)
    fn scrub_doc(&mut self, d: &mut crate::attach::Doc, mode: crate::secrets::Mode) {
        crate::secrets::learn(&d.text);
        if !mode.applies(self.model_leaves()) { return; }
        let (r, n) = crate::secrets::redact(&d.text);
        if n > 0 {
            self.fe.info(&format!("[{}: {} hidden from the model]", d.name, crate::secrets::count_words(n)));
            d.text = r;
            for part in d.parts.iter_mut() { *part = crate::secrets::redact(part).0; }
        }
    }

    /// After a saved session is loaded (`--continue`, `/resume`, "continue the last one", the dev reload): every
    /// document its history or notes name is attached again from this folder's upload area (the intake is a cache
    /// hit), so the next question about it gets its pages (`doc_follow_up`); one whose copy is gone gets a one-line
    /// note. An older session's inline document text (`[Attached: …; overview below…`, `[From … picked for this
    /// question]`) is replaced by the reference line, so stale text is not sent again.
    fn restore_docs(&mut self, session: Option<u64>) {
        self.kept_docs.clear();
        let root = outside::root(&self.cwd);
        let mut refs: Vec<crate::attach::DocRef> = Vec::new();
        let mut rewritten = 0;
        for m in self.history.iter_mut().filter(|m| m.role == Role::User) {
            let before = m.content.len();
            m.content.retain(|b| !matches!(b, Block::Text { text } if text.starts_with("[From ") && crate::attach::is_block(text)));
            rewritten += before - m.content.len();
            for b in m.content.iter_mut() {
                let Block::Text { text } = b else { continue };
                let Some(mut r) = crate::attach::parse_ref(text) else { continue };
                if r.rel.is_none() { r.rel = crate::attach::find_upload(&root, &r.name); }
                if r.old { *text = r.line(); rewritten += 1; }
                refs.push(r);
            }
        }
        if let Some(n) = &self.notes {
            for (i, _) in n.match_indices(crate::attach::MARK) {
                let t = &n[i..];
                let line = &t[..t.find(']').map_or(t.len(), |j| j + 1)];
                if let Some(r) = crate::attach::parse_ref(line) { refs.push(r); }
            }
        }
        if rewritten > 0 { self.real = None; self.debug(&format!("[resumed session: {rewritten} inline document block(s) replaced by their reference]")); }
        // each document once, the newest mention last
        let mut seen: Vec<crate::attach::DocRef> = Vec::new();
        for r in refs.into_iter().rev() { if !seen.iter().any(|x| x.name == r.name && x.rel == r.rel) { seen.push(r); } }
        seen.truncate(crate::attach::DOCS_MAX);
        seen.reverse();
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        if seen.is_empty() { self.recover_docs(&root, mode, session); }
        let query = self.history.iter().rev().filter(|m| m.role == Role::User)
            .find_map(|m| m.content.iter().find_map(|b| match b { Block::Text { text } if !crate::attach::is_block(text) => Some(context::request_body(text).trim().to_string()), _ => None }))
            .unwrap_or_default();
        for k in self.kept_docs.iter_mut() { k.query = query.clone(); }
        for r in seen {
            let got = r.rel.as_deref().map(|rel| crate::attach::open(&root, rel));
            match got {
                Some(Ok(mut d)) => {
                    self.scrub_doc(&mut d, mode);
                    self.debug(&format!("[re-attached {} · {} from {}]", d.name, d.units, d.rel));
                    self.kept_docs.push(KeptDoc::new(d, query.clone()));
                }
                _ => self.fe.info(&format!("{} is no longer in this folder's uploads; attach it again", r.name)),
            }
        }
    }

    /// A resumed conversation that names no document by reference (folded before references were kept): the files
    /// in that session's own upload folder, else the one upload whose whole title (or file name) its words or notes
    /// contain (`attach::named_in`). Never "the latest upload": with no match, or more than one, nothing is attached
    /// and nothing is said (2026-11-09: a TradingAgents chat got a newer, unrelated paper's pages).
    fn recover_docs(&mut self, root: &Path, mode: crate::secrets::Mode, session: Option<u64>) {
        let own = session.map(|s| crate::attach::uploads_of(root, s)).unwrap_or_default();
        let mut got: Vec<(crate::attach::Doc, &str)> = own.iter().filter_map(|rel| crate::attach::open(root, rel).ok().map(|d| (d, "its own upload"))).collect();
        if got.is_empty() {
            let mut text = self.notes.clone().unwrap_or_default();
            for m in &self.history {
                for b in &m.content { if let Block::Text { text: t } = b { text.push('\n'); text.push_str(t); } }
            }
            let hits: Vec<crate::attach::Doc> = crate::attach::uploads(root).iter()
                .filter_map(|rel| crate::attach::open(root, rel).ok()).filter(|d| crate::attach::named_in(d, &text)).collect();
            if hits.len() == 1 { got = hits.into_iter().map(|d| (d, "named by its title in the resumed conversation")).collect(); }
            else if hits.len() > 1 { self.debug(&format!("[resumed session: {} uploads match its words; none attached]", hits.len())); }
        }
        for (mut d, why) in got {
            self.scrub_doc(&mut d, mode);
            self.debug(&format!("[re-attached {} · {} from {} ({why})]", d.name, d.units, d.rel));
            self.kept_docs.push(KeptDoc::new(d, String::new()));
        }
    }

    /// This turn's request-only document text (None: none); a reply already measured with it keeps its real token
    /// count without it once it is gone, so the next estimate (and the fold check) sees only the history.
    fn set_doc_req(&mut self, text: Option<String>) {
        if text.is_none() && self.doc_req_tokens > 0 {
            if let Some((n, t)) = self.real { self.real = Some((n, t.saturating_sub(self.doc_req_tokens))); }
            if let Some((r, e)) = self.calib { self.calib = Some((r.saturating_sub(self.doc_req_tokens), e.saturating_sub(self.doc_req_tokens))); }
        }
        self.doc_req_tokens = text.as_deref().map_or(0, |t| estimate_tokens(t) + 8);
        self.doc_req = text;
    }

    /// The history's size as the next turn's requests send it (old answers shortened when it is long,
    /// `context::shorten_old_replies`): what the document budget leaves room for. Called before the turn's request
    /// is in the history, so the boundary is its end (the same answers the turn's requests shorten).
    fn req_history_tokens(&self) -> usize {
        match context::shorten_old_replies(&self.history, self.history.len(), self.max_ctx) {
            Some(v) => estimate_messages(&v),
            None => estimate_messages(&self.history),
        }
    }

    /// This turn's topic (`topics::pick`: a document named, its words, a card's goal words, a follow-up of the last
    /// turn's, else none) and its card as this turn's `[state]` block (requests only). A card of another topic is
    /// never sent (2026-11-09: card #647 of an earlier PDF steered every turn after the user moved to another paper).
    fn pick_topic(&mut self, input: &str) {
        self.card_req = None;
        let q = context::request_body(input).trim();
        let docs: Vec<crate::topics::DocTopic> = self.kept_docs.iter().map(|k| k.topic.clone()).collect();
        let p = if q.starts_with('/') { crate::topics::Pick::default() } else {
            let own = context::own_words(q).unwrap_or(q);
            crate::topics::pick(own, &docs, &self.topic_cards, &self.topic, self.doc_new.take())
        };
        let doc = p.doc.and_then(|d| docs.get(d)).map_or("none".to_string(), |d| d.name.clone());
        let card = p.card.and_then(|c| self.topic_cards.get(c)).map_or("none".to_string(), |c| c.id.map_or("this session's".to_string(), |i| format!("#{i}")));
        if !docs.is_empty() || !self.topic_cards.is_empty() { self.debug(&format!("[topic: document {doc} · card {card} · {}]", p.why)); }
        self.card_req = p.card.and_then(|c| self.topic_cards.get(c)).map(|c| format!("[state]\n{}\n[/state]", c.text.trim()));
        self.topic = p;
    }

    /// A turn with no new document, in a session that has some: the document this turn's topic picked
    /// (`pick_topic`) gets the parts that answer the message in this request (the whole text when it fits, an
    /// overview for "summarise it again"). The query is the last question about it plus the new words when those are
    /// few. A turn about something else gets no document text; "back to the paper" / "back to TradingAgents" picks it
    /// again.
    fn doc_follow_up(&mut self, input: &str) {
        if self.kept_docs.is_empty() || self.doc_req.is_some() { return; }
        let Some(i) = self.topic.doc.filter(|&i| i < self.kept_docs.len()) else { return };
        let q = context::request_body(input).trim().to_string();
        if q.is_empty() || q.starts_with('/') { return; }
        let budget = crate::attach::budget(self.max_ctx, self.req_history_tokens());
        let carry = crate::topics::follow_up(&q);
        let k = &mut self.kept_docs[i];
        let vague = crate::attach::is_vague(&q);
        let few = q.split_whitespace().count() <= 4 && !vague;
        // a general first question ("what is this paper about?") is not carried: merged in, it picked the overview again
        // and the model repeated its summary instead of answering the follow-up (measured on qwen3-vl:2b)
        let query = if (carry || few) && !crate::attach::is_vague(&k.query) { format!("{} {q}", k.query) } else { q.clone() };
        if !carry && !vague && !few { k.query = q.clone(); }
        let (block, pick) = crate::attach::block_for(&k.doc, &query, budget, &[]);
        let log = format!("{} · {:?} · ~{} tokens", k.doc.name, pick, estimate_tokens(&block));
        let r = if matches!(pick, crate::attach::Pick::Parts(_)) { crate::attach::render(&k.doc, &crate::attach::Pick::Overview, budget) } else { block.clone() };
        self.doc_retry = Some((k.doc.name.clone(), r));
        self.debug(&format!("[documents in this request: {log} · budget {budget}]"));
        // a follow-up about a document already discussed: answer that question, not the whole document again
        // (measured on qwen3-vl:2b: "what kind of data does it work with?" got its first summary repeated)
        self.set_doc_req(Some(format!("{block}\nAnswer only the user's new question from the text above, in a few sentences; do not summarize the whole document again.")));
    }

    /// The safety net: a turn that carried document text and got a reply saying it cannot read the document (and
    /// called no tool) is asked once more, the reply replaced, with the overview (or the same text) and one line saying
    /// the text is right there. True when it retried.
    fn doc_refusal_retry(&mut self) -> Option<TurnEnd> {
        let (name, block) = self.doc_retry.take()?;
        if !self.tool_lines.is_empty() { return None; }
        let n = self.history.len();
        if n < 2 || self.history[n - 1].role != Role::Assistant || self.history[n - 2].role != Role::User { return None; }
        if !crate::attach::refusal(&texts(&self.history[n - 1])) { return None; }
        self.history.pop();
        self.fe.reply_retracted();
        self.set_doc_req(Some(format!("{block}\nThe document's text above was extracted by Elisa from the attached file: answer from it directly.")));
        let phrase = crate::activity::Phrase::new("docs", "doc", &name, format!("Giving the model the text of {name} again"), format!("Gave the model the text of {name} again"));
        let aid = self.acts.begin(&mut *self.fe, &phrase);
        self.acts.end_with(&mut *self.fe, aid, "the reply said it could not read it", false);
        self.debug(&format!("[retry: the reply refused {name}; asked again with its text]"));
        Some(self.agent_loop(CheckRun::default(), true))
    }

    /// The safety net for a reply that says a file was saved when no write / edit succeeded this turn: one was denied
    /// or failed (`route::claims_saved`; measured on qwen3-vl:2b: "save the word hello to deny.txt", the user said
    /// no, and the reply was "I've saved "hello" to deny.txt for you."), or none was made at all and the reply names
    /// the saved file (`route::claims_file_saved`; BUGS #60, gemma3:1b: "Okay, here's the marketing plan saved to
    /// marketing-plan.md" with no write call). It checks the reply against the turn's tool calls, not the user's
    /// words. The reply is replaced: asked once more in words, with no tools and a note saying nothing was written; a
    /// reply that still claims it gets a plain line.
    fn denied_claim_retry(&mut self) -> Option<TurnEnd> {
        let file = |l: &&String| ["[write ", "[edit ", "[edit_symbol "].iter().any(|p| l.starts_with(p));
        let (failed, ok): (Vec<&String>, Vec<&String>) = self.tool_lines.iter().filter(file).partition(|l| l.contains(" → "));
        if !ok.is_empty() { return None; }
        // no write at all: only a reply naming a saved file, and not after a command that may have written it
        let none = failed.is_empty();
        if none && self.tool_lines.iter().any(|l| l.starts_with("[bash ") || l.starts_with("[run_code ")) { return None; }
        let claim = move |t: &str| if none { crate::route::claims_file_saved(t) } else { crate::route::claims_saved(t) };
        let claims = |h: &[Message]| h.last().is_some_and(|m| m.role == Role::Assistant && claim(&texts(m)));
        if !claims(&self.history) { return None; }
        let said_no = failed.iter().any(|l| l.contains("→ denied by user"));
        let why = if none { "no write was made" } else if said_no { "the user said no" } else { "the write did not go through" };
        self.history.pop();
        self.fe.reply_retracted();
        self.debug(&format!("[retry: the reply said the file was saved, but {why}; asked again in words]"));
        self.retry_note = Some(format!("(Nothing was saved: {why}, so no file was written or changed. Tell the user plainly, \
            in one or two sentences, that it was not saved and why ({why}), giving no other reason; never say it was saved.)"));
        let end = self.agent_loop(CheckRun::default(), false);
        self.retry_note = None;
        if !matches!(end, TurnEnd::Done) || !claims(&self.history) { return Some(end); }
        // still claims it: a plain line instead
        self.history.pop();
        self.fe.reply_retracted();
        let vi = crate::reply_lang::detect(&self.input) == Some(crate::reply_lang::Lang::Vi);
        let line = match (none, said_no, vi) {
            (true, _, false) => "I have not saved it: no file was written.",
            (true, _, true) => "Tôi chưa lưu: chưa có tệp nào được ghi.",
            (false, true, false) => "Okay, I did not save it: you said no, so nothing was written.",
            (false, false, false) => "I could not save it: the write did not go through, so nothing was written.",
            (false, true, true) => "Được rồi, tôi chưa lưu: bạn đã từ chối nên chưa có gì được ghi.",
            (false, false, true) => "Tôi chưa lưu được: lệnh ghi không thành công nên chưa có gì được ghi.",
        };
        if self.interactive { self.fe.text_delta(line); }
        self.history.push(Message::assistant_text(line));
        self.debug("[retry: the reply still said it was saved; showed a plain line]");
        Some(end)
    }

    /// A reply that says the user's own facts as Elisa's (`context::claimed_user_facts`; bug #69: "I'm Elisa, and I
    /// live in Hanoi. My meeting code is ORCHID-42.", "I'm from Hanoi too") is asked once more in words with a note
    /// naming those facts. The values come from what the user told in this conversation and what recall brought back
    /// (`recalled_values`), never from fixed words. Measured on qwen3-vl:2b(-instruct): about a third of the replies
    /// to "I'm Linh, from Hanoi…" claimed a fact, and one retry with the note fixed 9 of 11. The retry stands whatever
    /// it says (one extra request, only on such a reply).
    fn claimed_facts_retry(&mut self) -> Option<TurnEnd> {
        let mut values = self.recalled_values.clone();
        for m in self.history.iter().filter(|m| is_request(m)) {
            for b in &m.content {
                let Block::Text { text } = b else { continue };
                if text.starts_with(context::NUDGE_PREFIX) || context::is_picture_block(text) || crate::attach::is_block(text) { continue; }
                for v in context::user_fact_values(text) { if !values.contains(&v) { values.push(v); } }
            }
        }
        if values.is_empty() { return None; }
        let last = self.history.last().filter(|m| m.role == Role::Assistant && m.content.iter().all(|b| matches!(b, Block::Text { .. })))?;
        let claimed = context::claimed_user_facts(&texts(last), &values);
        if claimed.is_empty() { return None; }
        self.history.pop();
        self.fe.reply_retracted();
        self.debug(&format!("[retry: the reply said the user's facts as Elisa's own ({}); asked again in words]", claimed.join(", ")));
        self.retry_note = Some(format!("(Your reply was not sent: it spoke of the user's facts ({}) as if they were yours. They are \
            the user's, not yours: write the reply again, saying them as \"you\" and \"your\", never \"I\" or \"my\".)", claimed.join(", ")));
        let end = self.agent_loop(CheckRun::default(), false);
        self.retry_note = None;
        Some(end)
    }

    fn save(&mut self) {
        if self.history.is_empty() { return; }
        let t0 = Instant::now();
        let show = self.save_slow;
        if show { self.fe.progress(Some(&Progress { label: "saving session".into(), ..Default::default() })); }
        // who wrote each reply, kept in the session (bug #66: the next model must not take them as its own)
        context::stamp_model(&mut self.history, &self.cfg.model);
        if let Err(e) = self.store.save_session(&self.history) { self.fe.error(&format!("session not saved: {e}")); }
        self.store.save_cards(&self.topic_cards);
        self.save_slow = t0.elapsed() > SLOW;
        if show { self.fe.progress(None); }
    }

    fn run_repl(&mut self) {
        loop {
            self.watch_poll();
            let Some(line) = self.fe.read_input() else { break };
            // a panic anywhere in the line's work (a turn, a command, the fold after it) ends that line with an error
            // and its turn_done; the session takes the next message (longrun2 F1: a panic in elisa-mem killed the
            // agent thread silently and the app showed the turn as running forever)
            self.done_sent = false;
            let go_on = match guarded(|| self.one_line(line)) {
                Ok(g) => g,
                Err(msg) => { self.recover(&msg); if !self.done_sent { self.fe.turn_done(); } true }
            };
            if !go_on { break; }
        }
        // piped input: the last notes and view before exit
        if !self.watch.pushes() { self.watch.flush(WATCH_EXIT_WAIT); }
        self.watch_poll();
    }

    /// One input line of the interactive loop; false = quit.
    fn one_line(&mut self, line: String) -> bool {
        self.attach = self.take_attached();
        self.watch_poll();
        let line = line.trim();
        // pictures with no words: the default question (the app sends its own words; a bare `send` may not)
        let ask = if self.attach.len() > 1 { context::PICTURES_QUESTION } else { context::PICTURE_QUESTION };
        let line = if line.is_empty() && !self.attach.is_empty() { ask } else { line };
        if line.is_empty() { return true; }
        self.fe.user(line);
        let mut prompt = line.to_string();
        if line.starts_with('/') {
            match self.expand_command(line) {
                Some(Some(p)) => prompt = p,
                Some(None) => { self.turn_done(); return true; }
                None => {
                    let go_on = self.command(line);
                    self.turn_done();
                    return go_on;
                }
            }
        }
        self.req_begin(line);
        self.acts.reset();
        self.read_documents(&prompt);
        let end = self.turn(prompt);
        self.acts.close(&mut *self.fe);
        let outcome = end_name(&end);
        if let TurnEnd::Failed(e) = end { self.fe.error(&e); }
        self.save();
        self.turn_done();
        self.watch.refresh();
        self.maybe_fold();
        self.req_end(outcome);
        true
    }

    fn turn_done(&mut self) { self.fe.turn_done(); self.done_sent = true; }

    /// After a panic caught by `guarded`: say so, and leave the session usable for the next message (the turn's
    /// user message gets a closing reply so roles keep alternating; a panic inside elisa-mem turns memory off for the
    /// rest of the session, since the same call would only fail again).
    fn recover(&mut self, msg: &str) {
        SINK.with(|c| c.borrow_mut().take());
        CANCEL.store(false, Ordering::SeqCst);
        self.in_turn = false;
        self.attach.clear();
        self.docs.clear();
        self.set_doc_req(None);
        self.card_req = None;
        if self.history.last().is_some_and(|m| m.role == Role::User) {
            self.history.push(Message::assistant_text("[stopped: internal error]"));
        }
        self.real = None;
        self.fe.progress(None);
        self.fe.error(&format!("internal error: {msg}. This turn was stopped; the session goes on (please report this)"));
        if msg.contains("mem/src/") && self.mem.take().is_some() {
            self.fe.info("[memory: off for the rest of this session after that error; nothing on disk was removed]");
        }
    }

    fn run_print(&mut self, prompt: String) -> i32 {
        // `-p "/review src"`: a file command becomes its prompt (a project one needs its trust given before)
        let prompt = match prompt.starts_with('/').then(|| self.expand_command(prompt.trim())).flatten() {
            Some(Some(p)) => p,
            Some(None) => return 1,
            None => prompt,
        };
        self.read_documents(&prompt);
        self.req_begin(&prompt);
        let end = match guarded(|| self.turn(prompt)) {
            Ok(e) => e,
            Err(msg) => { self.recover(&msg); self.fe.turn_done(); return 1; }
        };
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
        // its file changed since it was read: read again (a project's or plugin's needs its trust again first)
        if let Some(src) = self.file_cmds.iter().find(|c| c.name == cmd && c.changed_on_disk()).map(|c| c.source.clone()) {
            if !self.reread_commands(cmd, &src) { return Some(None); }
            return match self.file_cmds.iter().find(|c| c.name == cmd) {
                Some(c) => Some(Some(c.prompt(args))),
                None => { self.fe.error(&format!("{cmd}: its file is gone or no longer a valid command (/help lists the commands)")); Some(None) }
            };
        }
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

    /// A command file changed on disk since it was loaded: its source read again. The user's own: the new text is used.
    /// A plugin's: loaded again only while the plugin's files still match what the user approved (else it is off
    /// until /trust). A project's: the trust prompt again, with the new text, when the content no longer matches the
    /// trusted hash. False: nothing to run (declined, or off).
    fn reread_commands(&mut self, cmd: &str, src: &crate::commands::Source) -> bool {
        use crate::commands::Source;
        match src {
            Source::User => {
                let mut notes = Vec::new();
                if let Some(h) = &self.home { self.cmd_user = crate::commands::load_dir(&h.join(".elisa/commands"), &Source::User, &mut notes); }
                for n in notes.into_iter().chain(self.merge_commands()) { self.fe.info(&n); }
                self.fe.info(&format!("{cmd}: its file changed since it was loaded; using the new text"));
                true
            }
            Source::Plugin(p) => {
                let p = p.clone();
                self.reload_plugins(None);
                if self.plugin_infos.iter().any(|i| i.name == p && i.enabled && i.trusted) {
                    self.fe.info(&format!("{cmd}: its file changed since it was loaded; using the new text"));
                    return true;
                }
                self.fe.error(&format!("{cmd}: plugin {p}'s files changed since you approved them, so its commands are off and nothing ran — review with /plugin list, then /trust {p}"));
                self.plugins_event();
                false
            }
            Source::Project | Source::ProjectPlugin(_) => {
                self.cfg.perms.rules.retain(|r| !r.source.starts_with("project plugin"));
                self.cmd_project.clear();
                let Some(p) = plugins::load_project(&self.cwd) else {
                    for n in self.merge_commands() { self.fe.info(&n); }
                    return true;
                };
                if self.home.as_ref().is_some_and(|h| plugins::project_trusted(h, &self.cwd, &p.hash)) {
                    self.apply_project(p);
                    for n in self.merge_commands() { self.fe.info(&n); }
                    return true;
                }
                // never the old cached text, never the new text unseen: the same trust prompt as at the start
                self.fe.info(&format!("{cmd}: the project's commands changed on disk since you trusted them; review the new text"));
                self.proj_infos = p.infos.clone();
                self.proj_pending = Some(p);
                self.proj_declined = false;
                for n in self.merge_commands() { self.fe.info(&n); }
                self.trust_project()
            }
        }
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
        // a server this session could not start is marked failed, with docker's reason (longrun2 F5)
        plugins::set_live(&mut self.plugin_infos, &self.plugin_servers, &|n| self.mcp.down_reason(n));
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

    /// Plugins changed outside this session (another window, `elisa plugin remove`): when the state file's stamp moved,
    /// the plugins are loaded again as `/plugin enable` / `disable` would — servers of removed or switched-off plugins
    /// stop and their tools, rules and commands go; newly switched-on ones start — with a line for each. Cheap when
    /// nothing changed (one stat). Run before each turn and before a plugin tool call (longrun2 F4).
    fn sync_plugins(&mut self) {
        let Some(home) = self.home.clone() else { return };
        let now = plugins::state_stamp(&home);
        if now == self.plugin_stamp { return; }
        self.plugin_stamp = now;
        let on = |s: &Self| s.plugin_infos.iter().filter(|i| i.enabled && i.trusted).map(|i| i.name.clone()).collect::<Vec<_>>();
        let before = on(self);
        let owned: Vec<(String, String)> = self.mcp.defs().into_iter().filter_map(|d| {
            let srv = self.mcp.server_of(&d.name)?;
            self.plugin_servers.iter().find(|(_, s)| s == srv).map(|(p, _)| (d.name.clone(), p.clone()))
        }).collect();
        self.reload_plugins(None);
        let after = on(self);
        let gone: Vec<&String> = before.iter().filter(|p| !after.contains(p)).collect();
        self.plugin_gone_tools.extend(owned.into_iter().filter(|(_, p)| gone.contains(&p)));
        let mcp = &self.mcp;
        self.plugin_gone_tools.retain(|(t, _)| !mcp.owns(t));
        let new: Vec<&String> = after.iter().filter(|p| !before.contains(p)).collect();
        for p in &gone { self.fe.info(&format!("plugin {p} was removed or switched off outside this session (another Elisa window or `elisa plugin`): its servers stopped, and its tools, rules and commands are off")); }
        for p in &new { self.fe.info(&format!("plugin {p} was switched on outside this session: its servers, rules and commands are in use from now on")); }
        if !gone.is_empty() || !new.is_empty() { self.plugins_event(); }
    }

    /// A call to a plugin server's tool whose plugin was removed or switched off elsewhere (checked just now): why it
    /// does not run. None: the tool is not a plugin's, or its plugin is still on.
    fn plugin_tool_gone(&mut self, name: &str) -> Option<String> {
        let plugin_tool = self.mcp.server_of(name).is_some_and(|srv| self.plugin_servers.iter().any(|(_, s)| s == srv));
        if plugin_tool { self.sync_plugins(); }
        let (_, plugin) = self.plugin_gone_tools.iter().find(|(t, _)| t == name)?;
        Some(format!("`{name}` belongs to plugin {plugin}, which was removed or switched off outside this session; its tools are off and nothing ran"))
    }

    /// one plugin operation's outcome: the app's `plugin_result` event, the audit line, and a line for the user
    fn plugin_result(&mut self, op: &str, name: &str, r: Result<String, String>) { self.plugin_result_working(op, name, r, true) }

    /// `plugin_result`; `working` false: done, but a server did not start (said as an error; `"working": false`)
    fn plugin_result_working(&mut self, op: &str, name: &str, r: Result<String, String>, working: bool) {
        let (ok, text) = match &r { Ok(t) => (true, t.clone()), Err(e) => (false, e.clone()) };
        plugins::audit(&self.audit, op, name, if ok && working { "ok" } else if text == DECLINED { "declined" } else { "error" }, if ok && working { "" } else { &text });
        if ok && working { self.fe.info(&text) } else { self.fe.error(&text) }
        self.fe.event("plugin_result", &json!({"op": op, "name": name, "ok": ok, "working": ok && working, "text": text}));
        // this session's own change is not news at the next turn
        self.plugin_stamp = self.home.as_deref().and_then(plugins::state_stamp);
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
    /// … and the closing line (`plugins::outcome_line`): (every server started, the text)
    fn plugin_probe(&mut self, name: &str, done: &str) -> (bool, String) {
        let Some(home) = self.home.clone() else { return (true, done.to_string()) };
        let pr = plugins::probe(&home, &self.cwd, name);
        let (ok, line) = plugins::outcome_line(done, &pr);
        let mut lines: Vec<String> = pr.into_iter().map(|(s, r)| match r {
            Ok((n, t)) => format!("{s}: {n} tool{} · ~{} tokens", if n == 1 { "" } else { "s" }, ui::k(t)),
            Err(e) => format!("{s}: did not start: {e}"),
        }).collect();
        lines.push(line);
        (ok, lines.join("\n"))
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
    /// The MCP server of tool `name` started for its first call (`mcp_host::Host::ensure`), before the approval: a
    /// tool list that differs from its pin shows its warning here and the call asks as usual. A plugin server whose
    /// image is missing: one question, then one more try. Err: why it could not start (the tool's result).
    fn mcp_start(&mut self, name: &str) -> Result<(), String> {
        let mut r = self.mcp.ensure(name);
        if r.is_err() && self.mcp.take_image_start_failed() {
            self.offer_plugin_images();
            if self.mcp.owns(name) { r = self.mcp.ensure(name); }
        }
        for n in self.mcp.take_notes() { self.fe.error(&n); }
        r
    }

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
                let (working, t) = self.plugin_probe(&m.name, &format!("plugin {} {} installed and on", m.name, m.version));
                self.reload_plugins(Some(&m.name));
                self.plugin_result_working("add", &m.name, Ok(t), working);
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
                let (working, t) = if r.is_ok() { self.plugin_probe(rest, &format!("plugin {rest} updated to {}", m.version)) } else { (true, String::new()) };
                self.reload_plugins(Some(rest));
                self.plugin_result_working("update", rest, r.map(|()| t), working);
            }
            "trust" if rest == "project" => {
                if self.proj_pending.is_none() { self.fe.info("this project has no commands or plugins waiting for trust"); return; }
                self.proj_declined = false;
                self.trust_project();
            }
            "trust" => {
                if rest.is_empty() { return need(self, "NAME | project"); }
                let (m, p) = match plugins::prepare_trust(&home, rest) {
                    Ok(x) => x,
                    Err(e) => {
                        // a plugin.toml that does not parse cannot be trusted: offer the removal, or keep it off
                        let broken = e.contains(plugins::UNPARSABLE);
                        self.plugin_result("trust", rest, Err(e));
                        if broken && self.interactive && self.fe.ask_yes_no(&format!("plugin remove {rest}"), &format!("Remove plugin {rest} now? (no keeps it off)")) {
                            self.plugin_command(&format!("remove {rest}"));
                        } else if broken { self.fe.info(&format!("plugin {rest} stays off")); }
                        return;
                    }
                };
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
                // the conversation's KV, before the summary call and the fresh start replace it (/resume brings it back)
                if cmd == "/clear" { self.slot_save("/clear"); }
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
                        let v = self.verified(&n);
                        let m = self.merge_kept(old.as_deref(), &v);
                        let n = self.finish_notes(m);
                        self.mem_note(&n, usize::MAX);
                        // /clear is a fresh start: the notes become background and the card goes (the notes carry it)
                        let clear = cmd == "/clear";
                        if clear {
                            self.new_memory_session();
                            // the pre-clear conversation stays in its own session file (resumable)
                            context::new_session();
                            self.goal = None;
                            (self.card, self.card_id, self.card_ex) = (None, None, None);
                            self.topic_cards.clear();
                            (self.topic, self.card_req) = (Default::default(), None);
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
                                Some(_) if after >= before => "notes saved to .elisa/notes/, whole: the conversation was shorter than its facts, decisions and file changes",
                                Some(_) => "notes saved to .elisa/notes/",
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
                self.fe.info(&format!("route (last turn): {} · config route = \"{}\"", self.route.describe(), self.cfg.route));
                if self.router.on() {
                    let said = self.routed.as_ref().map_or("no answer: word rules used".to_string(), |a| a.describe());
                    self.fe.info(&format!("intent router (last turn): {said}"));
                }
                if self.cfg.route == "auto" && self.mcp.has_gated() {
                    let on = |srv: &str| if self.tools.iter().any(|t| self.mcp.gated_server(&t.name) == Some(srv)) { "offered" } else { "not offered" };
                    self.fe.info(&format!("documents tools {} · security tools {} this turn (routed: only on turns about them)", on("docs"), on("sec")));
                } else if self.cfg.gate_tools == "auto" && self.mcp.has_gated() {
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
            "/state" => {
                if self.topic_cards.is_empty() { self.fe.info("no state card yet"); return true; }
                let docs: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
                // the topics: each document (attach order), then the cards about no document
                let mut keys: Vec<Option<String>> = docs.iter().cloned().map(Some).collect();
                if self.topic_cards.iter().any(|c| c.doc.as_ref().is_none_or(|d| !docs.contains(d))) { keys.push(None); }
                let active = self.topic.card;
                let mut out = Vec::new();
                for k in keys {
                    let mine: Vec<(usize, &crate::topics::Card)> = self.topic_cards.iter().enumerate()
                        .filter(|(_, c)| match &k { Some(d) => c.doc.as_ref() == Some(d), None => c.doc.as_ref().is_none_or(|d| !docs.contains(d)) }).collect();
                    let doc_on = k.as_ref().is_some_and(|d| self.topic.doc.and_then(|i| docs.get(i)) == Some(d));
                    let head = match &k { Some(d) => format!("topic: {d}{}", if doc_on { "  (active)" } else { "" }), None => "topic: no document".to_string() };
                    let mut t = vec![head];
                    if mine.is_empty() { t.push("  (no card yet)".into()); }
                    for (i, c) in mine {
                        let n = c.id.map_or("card".to_string(), |id| format!("card #{id}"));
                        t.push(format!("  {n}{}:", if active == Some(i) { " (active)" } else { "" }));
                        t.extend(c.text.lines().map(|l| format!("    {l}")));
                    }
                    out.push(t.join("\n"));
                }
                let why = format!("last turn's pick: {}", self.topic.why);
                self.fe.info(&format!("{}\n\n{why}", out.join("\n\n")));
            }
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
            "/cache" => match crate::cache::command(line[cmd.len()..].trim()) {
                Ok(t) => self.fe.info(&t),
                Err(e) => self.fe.error(&e),
            },
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
                // what earlier chats in this project left in memory (their turns, notes, cards) is not recalled again:
                // a new chat asked "what's my favourite colour?" and memory still said green (2026-11-09)
                let next = self.mem.as_mut().map(|m| { let _ = m.db.refresh(); m.db.next_id() });
                let recalled = match next.map(|id| self.store.save_forgot(id)) {
                    Some(Ok(())) => " · memories of earlier chats in this project forgotten (saved conversations kept)",
                    Some(Err(e)) => { self.fe.error(&format!("forgetting memories failed: {e}")); "" }
                    None => "",
                };
                match n {
                    Some(Ok(n)) => self.fe.info(&format!("notes deleted · {n} pinned facts unpinned{recalled}")),
                    Some(Err(e)) => self.fe.error(&format!("notes deleted; unpinning facts failed: {e}{recalled}")),
                    None => self.fe.info("notes deleted"),
                }
            }
            "/resume" => {
                let list = self.store.list_sessions();
                if list.is_empty() { self.fe.info("no saved session"); return true; }
                let Some(i) = self.fe.pick_session(&list).filter(|&i| i < list.len()) else { self.fe.info("resume cancelled"); return true; };
                match self.store.load_session(&list[i].path).map(context::loaded) {
                    Some(h) => {
                        // the session left behind keeps its KV too (llama.cpp slot save), then the resumed one's comes back
                        self.slot_save("session switch");
                        self.slot_restore(&list[i].path.clone());
                        self.fe.info(&format!("resumed \"{}\": {} messages restored (~{} tokens)", list[i].title, h.len(), ui::k(estimate_messages(&h))));
                        // the session left behind keeps its own file and notes; the resumed work goes on in a new one
                        context::new_session();
                        self.history = h;
                        self.real = None;
                        // back to that work: its card and notes are the task again
                        let notes = self.store.load_session_notes(&list[i].path);
                        self.adopt_notes(notes);
                        self.restore_docs(Some(list[i].ts));
                        self.load_topics(Some(list[i].ts));
                        self.carry_card();
                        self.notes_bg = false;
                        self.show_history();
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
            "/chat" | "/code" | "/mode" => self.fe.info("modes were removed: each turn gets the tools its words call for (config route = \"auto\"; /context shows the last turn's groups)"),
            "/water" | "/feed" | "/food" | "/play" | "/toy" | "/poke" => self.fe.info("Elisa's character lives in the full-screen view (not in plain mode)"),
            "/stats" => {
                let file = self.store.dir.join(metrics::FILE);
                if self.cfg.metrics { self.fe.info(self.stats.text(&file).trim_end()); }
                else { self.fe.info("metrics are off (config `metrics = false`)"); }
                self.fe.info(&crate::cache::stats_line());
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
        let mut goal = context::next_goal(self.goal.as_deref(), prompt);
        // the conversation moved to another task (GOAL_DRIFT unrelated requests in a row): the goal follows it
        // (a goal the user stated in so many words stays until they state another)
        if let Some(g) = goal.as_deref().filter(|g| Some(*g) == self.goal.as_deref() && !self.goal_stated) {
            match context::goal_drift(g, prompt) {
                Some(true) => self.goal_misses += 1,
                Some(false) => self.goal_misses = 0,
                None => {}
            }
            if self.goal_misses >= context::GOAL_DRIFT { goal = context::next_goal(None, prompt); }
        }
        if goal != self.goal {
            self.goal_misses = 0;
            self.goal_stated = goal.is_some() && context::own_words(context::request_body(prompt)).and_then(context::goal_statement).is_some();
        }
        if goal != self.goal && self.goal.is_some() { self.fe.info(&format!("[session goal: {}]", goal.as_deref().unwrap_or("none"))); }
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

    /// A resumed session's notes (`--continue` / `/resume`) become this session's own, saved under this session's id
    /// (the resumed session's file is not touched), so the next `--continue` finds them again.
    fn adopt_notes(&mut self, notes: Option<String>) {
        self.resumed_facts = true;
        if let Some(n) = &notes { if let Err(e) = self.store.save_notes(n) { self.fe.error(&format!("notes: {e}")); } }
        else if let Err(e) = self.store.clear_notes() { self.fe.error(&format!("notes: {e}")); }
        self.notes = notes;
    }

    /// The notes in use from now on, the session goal on top; `save`: also to `.elisa/notes.md` (a fold tries them
    /// first and saves only the version it keeps).
    fn put_notes(&mut self, n: Option<String>, save: bool) {
        self.notes_bg = false;
        // the session goal rides on top of the notes (a fold summary's `## Goal` is the newest request)
        let n = match (n, self.goal.as_deref()) { (Some(t), Some(g)) => Some(context::with_session_goal(&t, g)), (n, _) => n };
        // the attached documents stay known after a fold: their reference lines (Elisa picks their parts again)
        let missing: Vec<String> = self.kept_docs.iter().map(|k| crate::attach::reference(&k.doc))
            .filter(|r| n.as_deref().is_some_and(|t| !t.contains(r.as_str()))).collect();
        let n = match n {
            Some(t) if !missing.is_empty() => Some(format!("{t}\n\n## Attached documents (Elisa adds the parts each question needs)\n{}",
                missing.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"))),
            n => n,
        };
        if save {
            let r = match &n { Some(t) => self.store.save_notes(t), None => self.store.clear_notes() };
            if let Err(e) = r { self.fe.error(&format!("notes: {e}")); }
        }
        self.notes = n;
        self.rebuild_system();
    }

    /// New notes text (a summary, a digest, state cards) with its "done / changed" claims checked against Elisa's own
    /// edit records (`context::verify_claims`): a claim naming code no edit wrote is dropped.
    /// Text the model took from outside sources (`<untrusted>` fences in this history's tool results) is kept out of
    /// the goal / done / next-step lines (`context::quarantine_notes`): only a quoted fact with its source.
    fn verified(&self, new: &str) -> String {
        let cwd = self.cwd.clone();
        // what the conversation holds first: code claims no reply backs, attached documents as files touched, raw
        // tool lines (`context::ground_notes`)
        let said: String = self.history.iter().filter(|m| m.role == Role::Assistant).map(texts).collect::<Vec<_>>().join("\n");
        let new = context::ground_notes(new, &said, &self.doc_names(), &self.ledger);
        let v = context::verify_claims(&new, &self.ledger, &move |p: &str| std::fs::read_to_string(cwd.join(p)).ok());
        let o = context::Outside::of(&self.history);
        if o.is_empty() { v } else { context::quarantine_notes(&v, &o) }
    }

    /// The attached documents of this conversation (file names): the kept ones and every `[Attached: NAME, …`
    /// reference in the history or the notes.
    fn doc_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
        let mut scan = |t: &str| for part in t.split(crate::attach::MARK).skip(1) {
            if let Some(n) = part.split([',', ']']).next().map(str::trim).filter(|n| !n.is_empty()) {
                if !v.iter().any(|x| x == n) { v.push(n.to_string()); }
            }
        };
        for m in &self.history { for b in &m.content { if let Block::Text { text } = b { scan(text); } } }
        if let Some(n) = &self.notes { scan(n); }
        v
    }

    /// The user's and the assistant's words of this conversation (the notes block and turn hints of the first
    /// message left out), for telling whether an old note is about it (`context::fact_in_talk`).
    fn talk(&self) -> String {
        self.history.iter().flat_map(|m| m.content.iter().filter_map(move |b| match b {
            Block::Text { text } if m.role == Role::User && !text.starts_with(context::NUDGE_PREFIX) => Some(context::request_body(text).to_string()),
            Block::Text { text } if m.role == Role::Assistant => Some(text.clone()),
            _ => None,
        })).collect::<Vec<_>>().join("\n")
    }

    /// `new` merged into `old` (`context::merge_notes_kept`): an old fact, done line or session goal stays only when this
    /// conversation talks about it, it was pinned (/pin), or this run already found it in its talk (`facts_ok`).
    fn merge_kept(&mut self, old: Option<&str>, new: &str) -> String {
        let talk = self.talk();
        let pinned = self.mem.as_ref().map(|m| m.db.user_pinned_facts(&m.project)).unwrap_or_default();
        let ok = &self.facts_ok;
        let merged = context::merge_notes_kept(old, new, &|l: &str| ok.contains(&context::fact_key(l)) || context::fact_pinned(l, &pinned) || context::fact_in_talk(l, &talk));
        // the old notes' lines are checked against this conversation too (`context::ground_notes`: attached documents
        // listed as files touched, raw tool lines, invented files)
        let said: String = self.history.iter().filter(|m| m.role == Role::Assistant).map(texts).collect::<Vec<_>>().join("\n");
        let merged = context::ground_notes(&merged, &said, &self.doc_names(), &self.ledger);
        self.facts_ok.extend(context::fact_keys(&merged));
        merged
    }

    /// Merged notes as they are kept: Elisa's own list of the files changed this session merged in (ground truth),
    /// duplicates gone, cut by priority to the notes' share of the window (`context::notes_budget`).
    /// The facts the user stated about themself in this history go in too, in the third person (`context::user_facts`:
    /// a 2B model's summary wrote them as Elisa's, "My name is Elisa"), and count as found in this run's talk.
    fn finish_notes(&mut self, merged: String) -> String {
        let merged = match self.ledger.section() { Some(sec) => context::merge_notes(Some(&merged), &sec), None => merged };
        let mut facts: Vec<String> = Vec::new();
        for m in self.history.iter().filter(|m| m.role == Role::User) {
            for b in &m.content { if let Block::Text { text } = b { if !context::is_picture_block(text) { facts.extend(context::user_facts(text)); } } }
        }
        self.facts_ok.extend(facts.iter().map(|f| context::fact_key(f)));
        let merged = context::with_user_facts(&merged, &facts);
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
        // the in-context form pays only where the server reuses the prefix; elsewhere the whole history would be read
        // again, and the plain transcript is smaller (no tools, small system: ~1.4K tokens less on llama.cpp without cache)
        let reuse = self.client.caps().reuses_prefix();
        if reuse && self.estimate() + estimate_tokens(context::compact_instructions()) + out + 64 <= self.max_ctx {
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
    ///
    /// Over the limit, the room is made in this order, and the turn goes on by itself (BUGS #41: a 2K window stopped
    /// with "say continue" every turn): earlier documents' text becomes their reference lines (`context::drop_doc_text`),
    /// then the fold, then the oldest turns go (`context::drop_old_turns`; their words stay in memory and the notes),
    /// then this turn's own document text is cut to fit.
    fn mid_turn_fold(&mut self) -> bool {
        let over = |s: &Self| s.est_safe() > s.limit();
        if !over(self) { return true; }
        let before = self.estimate();
        let cap = self.turn_cap(&self.history);
        if let Some(h) = context::drop_doc_text(&self.history, cap) { self.history = h; self.real = None; }
        if !self.folded_this_turn && self.history.len() >= 2 && over(self) {
            self.folded_this_turn = true;
            self.fold(false);
        }
        if over(self) {
            let cap = self.turn_cap(&self.history);
            if let Some(h) = context::drop_old_turns(&self.history, cap, self.est_safe() - self.limit()) { self.history = h; self.real = None; }
        }
        if over(self) && self.doc_req.is_some() {
            // this turn's picked document parts (request-only): cut to what is left
            let room = self.limit().saturating_sub(self.est_safe() - self.doc_req_tokens);
            let d = self.doc_req.take().unwrap_or_default();
            let keep: String = d.chars().take(room * 3).collect();
            self.set_doc_req((!keep.is_empty()).then(|| format!("{keep}\n[the rest of the document text did not fit]")));
        }
        if self.estimate() != before { self.shrunk(before); }
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
        // plugins removed / switched off or on by another Elisa since the last turn (one stat when nothing changed)
        self.sync_plugins();
        let mode = crate::mcp_reason::Mode::parse(&self.cfg.reason).unwrap_or(crate::mcp_reason::Mode::Auto);
        let body = context::request_body(input);
        // the hosted intent router (`router.rs`), when configured: one short call; None = off or a miss (word rules)
        self.routed = self.ask_router(body);
        let plan = self.routed.as_ref().map(crate::router::plan);
        // its `reasoning` turns the reasoning gate on (the word gate still can, so a multi-step task keeps it)
        let on = crate::mcp_reason::decide(mode, body, self.reason_on) || (mode == crate::mcp_reason::Mode::Auto && plan.as_ref().is_some_and(|p| p.reasoning));
        // the same check, whatever `reason` is: a simple turn ("hi", "2+2=?") asks the model not to reason and gets a
        // smaller output cap (config `think_simple`; a follow-up like "ok" keeps the last turn's choice)
        let simple = crate::mcp_reason::wanted(body).map_or(self.client.simple(), |multi| !multi);
        self.client.set_simple(simple && self.cfg.think_simple == "off");
        self.mcp.new_turn();
        // the documents and security servers: only on turns about them (config `gate_tools`), same once-per-turn rule
        let gate = crate::mcp_reason::gate(self.cfg.gate_tools == "off", body, &self.recent_text(), self.gate);
        // intent routing (config `route`): the tool groups this turn's words call for (`route::classify`)
        let large_doc = self.docs.iter().any(|d| d.1);
        let doc_text = large_doc || self.doc_req.is_some();
        let others = || -> Vec<String> { self.mcp_names().into_iter()
            .filter(|n| crate::route::tool_group("", Some(n)) == crate::route::OTHER && n != crate::mcp_reason::NAME).collect() };
        let route = match &plan {
            _ if self.cfg.route != "auto" => crate::route::Route::all(),
            // the router's answer: its groups (`route::from_router`); the widen path still repairs a miss
            Some(p) => crate::route::from_router(p, body, &self.route, crate::route::servers_named(body, &others()), doc_text, large_doc),
            None => crate::route::classify(body, &self.route, &others()),
        };
        // the documents / security servers follow the router's groups too
        let gate = match &plan { Some(p) => (gate.0 || p.groups.has(crate::route::DOCS), gate.1 || p.groups.has(crate::route::SEC)), None => gate };
        // document text in this turn's message (attached now, or a follow-up's pages): only the tools its own words
        // ask for; one too large to go in whole adds the documents tools (`route::with_documents`)
        let route = if doc_text && route.why != crate::route::Why::Router {
            let names: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
            crate::route::with_documents(route, body, &names, large_doc)
        } else { route };
        let gate = if large_doc { (true, gate.1) } else { gate };
        let groups_changed = route.groups != self.route.groups || route.others != self.route.others;
        if let Some(a) = &self.routed { self.debug(&format!("[router: {}]", a.describe())); }
        self.debug(&format!("[route: {}]", route.describe()));
        if on != self.reason_on || gate != self.gate || groups_changed {
            (self.reason_on, self.gate, self.route) = (on, gate, route);
            self.rebuild_system();
        } else { self.route = route; }
        if std::env::var("ELISA_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0") {
            let names: Vec<String> = self.tools.iter().map(|t| t.name.clone()).collect();
            self.debug(&format!("[tools: {}]", if names.is_empty() { "none".into() } else { names.join(", ") }));
        }
    }

    /// The hosted intent router's answer for this turn's words (`router.rs`), with only the contract's context: what
    /// is attached now, whether a document is in the chat, vision, a refused picture, an earlier session to resume,
    /// and the last user message and reply (cut to 400 characters). None when it is off (no key), `route` is not
    /// "auto", or it missed (timeout, HTTP error, bad JSON, offline): the word rules then decide, as without it.
    fn ask_router(&mut self, body: &str) -> Option<crate::router::Answer> {
        if !self.router.on() || self.cfg.route != "auto" { return None; }
        if self.pic_refused { self.pic_refused_seen = true; }
        let words = |m: &Message| m.content.iter().find_map(|b| match b {
            Block::Text { text } if !text.starts_with(crate::attach::MARK) && !context::is_picture_block(text) && !text.starts_with(context::NUDGE_PREFIX) => Some(crate::route::strip_ask(context::request_body(text)).trim().to_string()),
            _ => None,
        });
        let last_user = self.history.iter().rev().filter(|m| m.role == Role::User).find_map(|m| words(m).filter(|t| !t.is_empty())).unwrap_or_default();
        let last_reply = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default();
        let attachment = self.docs.first().map(|d| {
            let line = crate::attach::first_line(&d.0);
            let name = line.strip_prefix(crate::attach::MARK).unwrap_or(line);
            crate::router::attachment_label(name.split_once(", ").map_or(name, |x| x.0).trim_end_matches(']'))
        });
        let ctx = crate::router::Context {
            attachment, doc_in_chat: !self.kept_docs.is_empty() || self.doc_req.is_some(), vision: self.vision,
            image_refused: self.pic_refused || self.pic_refused_seen,
            prev_session: self.history.is_empty() && self.store.newest_real_session().is_some(),
            last_user: crate::router::cut(&last_user, crate::router::CONTEXT_CHARS), last_reply: crate::router::cut(&last_reply, crate::router::CONTEXT_CHARS),
        };
        let mut info: Option<String> = None;
        let (a, miss) = self.router.ask(body, &ctx, &mut |l| info = Some(l.to_string()));
        if let Some(l) = info { self.fe.info(&l); }
        if let Some(e) = miss { self.debug(&format!("[router: no answer ({e}); word rules]")); }
        a
    }

    /// The router said (Some) whether this turn has `label`; None = no router answer this turn (the word rules decide).
    fn router_says(&self, label: &str) -> Option<bool> { self.routed.as_ref().map(|a| a.has(label)) }

    /// Routed (`route = "auto"`): the working directory reaches the model only once a turn carries tools. A chat
    /// first turn leaves the `Working directory:` line out of its header (measured on qwen3-vl:2b: with it, "what can
    /// you do?" was answered as a coding helper for "your current project"); the first tool turn after it starts with
    /// that line instead, so file paths still resolve. The history stays append-only (no rewrite, no cache loss).
    fn route_cwd(&self, text: String) -> String {
        if self.cfg.route != "auto" || self.tiny() { return text; }
        let line = format!("Working directory: {}\n", self.cwd.display());
        let sent = self.history.iter().any(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if text.contains(&line))));
        let has = text.starts_with(&line);
        match (self.route.groups.tool_free() && self.route.others.is_empty(), has, sent) {
            (true, true, _) => text[line.len()..].to_string(),
            (false, false, false) => format!("{line}\n{text}"),
            _ => text,
        }
    }

    /// A line only in debug mode (environment `ELISA_DEBUG=1`): routing decisions and the like.
    fn debug(&mut self, line: &str) {
        if std::env::var("ELISA_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0") { self.fe.info(line); }
    }

    /// A call to a session tool the route left out of this request (`route.rs`): the route widens to that tool's
    /// group (one tool-list change) and the call goes on to the usual approval, so a wrong guess never fails the turn.
    /// False for anything else (a tool the session does not have, the reasoning tools, `route = "off"`).
    fn widen_for(&mut self, name: &str) -> bool {
        if self.cfg.route != "auto" || self.mcp.reason_tool(name) { return false; }
        let srv = self.mcp.server_of(name).map(str::to_string);
        let known = srv.is_some() || tools::defs_for(self.cfg.ast).iter().any(|d| d.name == name);
        if !known { return false; }
        let g = crate::route::tool_group(name, srv.as_deref());
        self.route.groups = self.route.groups.with(g);
        if let Some(s) = srv.filter(|_| g == crate::route::OTHER) { if !self.route.others.contains(&s) { self.route.others.push(s); } }
        self.route.why = crate::route::Why::Widened;
        self.rebuild_system();
        self.debug(&format!("[route: `{name}` was outside this turn's tools; widened to {}]", self.route.describe()));
        self.tools.iter().any(|t| t.name == name)
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
        // a later turn about a document attached earlier: its parts for this request (before routing, which leaves
        // the documents tools out when the text is there)
        // (a turn whose attachments were all refused is about that refusal: no earlier document, no card; the last
        // topic stays for the turn after it)
        let refused = self.doc_refused.is_some();
        if refused { self.card_req = None; } else {
            self.pick_topic(&input);
            self.doc_follow_up(&input);
        }
        self.reason_turn(&input);
        self.tool_lines.clear();
        self.unsent = false;
        (self.new_card, self.card_broken) = (None, false);
        self.in_turn = true;
        self.folded_this_turn = false;
        // memory keeps an attached document's name line only, never its text
        let attached: String = self.docs.iter().map(|d| format!("\n{}", crate::attach::first_line(&d.0))).collect();
        let end = self.turn_inner(input.clone());
        self.doc_refused = None;
        self.in_turn = false;
        (self.last_turn_at, self.last_failed) = (Some(crate::situation::now_secs()), !matches!(end, TurnEnd::Done));
        // a refused attachment's turn is not remembered: its answer is about a file Elisa never read
        // a turn whose tool the user said no to or cancelled keeps no answer: its result is not something that happened
        let said_no = self.tool_lines.iter().any(|l| l.contains("→ denied by user") || l.contains("→ cancelled by user"))
            || self.history.last().is_some_and(|m| m.role == Role::Assistant && texts(m).trim() == "[cancelled]");
        if !self.unsent && !refused {
            let mut reply = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default().trim().to_string();
            // a reply that called no tools (plain chat) is not expected to carry a state card
            let used_tools = !self.tool_lines.is_empty();
            for l in std::mem::take(&mut self.tool_lines) { reply += &format!("\n{l}"); }
            // the user's words are kept; the answer only when it came from tools that ran (a plain-chat answer is the
            // model's guess: "You are Johnathan…", a wrong "9"), and never a refusal, a failed tool's or an error
            // turn's (BUGS #16, #27; an empty turn keeps the exchange's place in memory and is never recalled)
            if !used_tools || !matches!(end, TurnEnd::Done) || said_no || context::unfit_memory(&reply) { reply.clear(); }
            let ids: Vec<u64> = [self.record(MRole::User, &format!("{input}{attached}")), self.record(MRole::Assistant, reply.trim())].into_iter().flatten().collect();
            self.exchanges += 1;
            // a card is the next turn's instructions: nothing in it may come from outside text (`context::Outside`)
            let o = context::Outside::of(&self.history);
            let card = self.new_card.take().map(|c| if o.is_empty() { c } else { context::quarantine_card(&c, &o) }).filter(|c| !c.trim().is_empty());
            match card {
                // plain chat carries no card (BUGS #22: a card from a no-tool turn was carried and repeated); a turn
                // about an attached document keeps its card: the document's topic card (`topics.rs`)
                Some(c) if used_tools || !attached.is_empty() => self.set_card(c, &ids),
                None if self.card_broken && used_tools && matches!(end, TurnEnd::Done) => {
                    let keep = self.card_id.map_or("none yet".to_string(), |k| format!("keeping card #{k}"));
                    // internal: never in the chat (BUGS #23)
                    self.debug(&format!("[no state card in this reply; {keep}]"));
                }
                _ => {}
            }
        }
        end
    }

    fn turn_inner(&mut self, input: String) -> TurnEnd {
        // "continue the last one" in a session with no history: the user's own ask to go back to the last session
        let resumed = match self.resume_on_ask(&input) {
            Ok(r) => r,
            Err(end) => return end,
        };
        // recalled memories go in the USER message (the system prompt stays byte-stable); only `input` is recorded
        let mut lines = if self.doc_refused.is_some() { Vec::new() } else { self.recall(&input) };
        if !lines.is_empty() { self.fe.recalled(&lines); }
        // the first turn of a resumed session: what the user told about themself, in the third person, with the
        // recalled lines (BUGS #43: gemma3:1b, resumed, answered "Where do I live and what's my name?" with "You are
        // Elisa. You live in Hanoi."; the old turns' "my name is Linh" no longer read as the user's)
        if std::mem::take(&mut self.resumed_facts) { lines.extend(self.resumed_facts()); }

        let text = if self.history.is_empty() && self.tiny() {
            context::first_message_bare(self.first_notes().as_deref(), self.state_block().as_deref(), &lines, &input)
        } else if self.history.is_empty() {
            context::first_message_full(&self.cwd, self.first_notes().as_deref(), self.state_block().as_deref(), &lines, &input)
        } else {
            context::with_memory(&lines, &input)
        };
        let mut text = self.route_cwd(text);
        // a turn about the time / date: the local clock as one line (the first message's header has it already), so
        // the model answers directly, conversions included, with no tool (the time server only through the widen path)
        if self.route.groups.has(crate::route::TIME) {
            // before the user's words (measured on qwen3-vl:2b: after them, the model redid the offset arithmetic and got
            // Tokyo wrong; right after the local line it read the answer off 3 of 3 times)
            // (a what_about's new subject from the router: "and in Osaka?" → its clock line)
            let topic = self.routed.as_ref().and_then(|a| a.topic.clone()).unwrap_or_default();
            let lines = crate::route::time_lines(&format!("{input} {topic}"));
            let local = context::local_now();
            match text.find(&local) {
                // the first message's header has the local line: the places named go right under it
                Some(i) => { let places = lines.strip_prefix(&local).unwrap_or(""); text.insert_str(i + local.len(), places); }
                None => text = format!("{lines}\n\n{text}"),
            }
        }
        // a session mixing English and Vietnamese: a short hint after the user's words (`reply_lang.rs`; measured on
        // qwen3-vl:2b, a time turn in English after a Vietnamese one was answered in Vietnamese)
        let earlier = self.history.iter().filter(|m| m.role == Role::User).flat_map(|m| &m.content)
            .filter_map(|b| match b { Block::Text { text } => Some(context::request_body(text)), _ => None });
        // a small-talk turn with context: one situation line right before the user's words (`situation.rs`), in this
        // last user message only, so the cached prefix (system, tools, history) is unchanged; no context, no line
        // a short follow-up ("why?", "more", "the second one") after an answer: one hint line there instead
        // (`followup.rs`); a new session gets none
        let resume_hint = resumed.map(|r| match r {
            crate::route::Resume::Continue if self.last_failed => crate::followup::RESUME_RETRY.to_string(),
            // a task (its last turn ran tools): go on with it; a plain chat: recap it and offer to go on (measured on
            // qwen3-vl:2b: a chat ending in "just reply OK" turns got a bare "OK." to "continue our last conversation")
            crate::route::Resume::Continue if crate::situation::last_turn_flags(&self.history).0 => crate::followup::RESUME_CONTINUE.to_string(),
            crate::route::Resume::Continue | crate::route::Resume::Recap => {
                let asks: Vec<String> = self.history.iter().filter(|m| m.role == Role::User).flat_map(|m| &m.content)
                    .filter_map(|b| match b { Block::Text { text } if !text.starts_with(crate::attach::MARK) => Some(context::request_body(text).to_string()), _ => None }).collect();
                crate::followup::resume_recap(&asks)
            }
        });
        // what Elisa can do on this model, from the detected caps (bug #46: the fixed list claimed images with no vision)
        let vision = self.client.caps().vision == Some(true);
        let about = self.router_says("about_elisa").unwrap_or_else(|| crate::route::about_elisa(&input)).then(|| crate::situation::about(vision));
        // (a tiny window gets none: a small model there read such lines back as its answer, BUGS #42)
        let hint = if self.tiny() { None } else { resume_hint.or(about).or_else(|| self.situation(&input)).or_else(|| self.follow_up_hint(&input)) };
        if let Some(l) = hint {
            if text.ends_with(input.as_str()) { let at = text.len() - input.len(); text.insert_str(at, &format!("{l}\n\n")); }
        }
        // an explicit "search the web: …" or "read notes.txt" with that tool offered: one line after the user's words,
        // in this turn's own message (so every request of the turn is byte-identical up to it), stripped from saved
        // views by `context::request_body` (measured on qwen3-vl:2b: without it the model answered from memory, 0/3
        // web calls and "I don't see notes.txt"; with it 3/3)
        let lang_hint = crate::reply_lang::hint(earlier, &input);
        // a picture the model cannot see: Elisa says so herself, so no model invents one (BUGS #36)
        if let Some(line) = self.no_picture_line(&input) {
            self.input = input;
            if self.interactive { self.fe.text_delta(&line); }
            self.history.push(Message::user_text(text));
            self.history.push(Message::assistant_text(line));
            return TurnEnd::Done;
        }
        if let Some(n) = self.doc_refused.clone().or_else(|| self.ask_note(&input)) { text.push_str(&n); }
        if let Some(h) = lang_hint { text.push_str(h); }
        // a small-talk turn routed as chat runs no tool: a call there is asked again in words (`agent_loop`)
        // (a bare "ok" too, unless it answers an offer: the last answer ended with a question)
        let offer = self.history.iter().rev().find(|m| m.role == Role::Assistant).is_some_and(|m| texts(m).trim_end().ends_with('?'));
        let small = match &self.routed {
            // the router: a greeting or a closing (a bare "ok" to an offer is a yes: closing only without one)
            Some(a) => a.has("greeting") || (a.has("closing") && !offer),
            None => crate::route::small_talk(&input) || (crate::route::ack(&input) && !offer),
        };
        self.small_talk_turn = self.cfg.route == "auto" && self.route.groups.tool_free() && self.route.others.is_empty() && small;
        self.input = input;
        // pictures: asked about alone first (`look`), the answer kept as text; the turn then runs as text only
        let (mut picture, mut pic_answer) = (None, None);
        if let Some((pics, new)) = self.turn_pictures() {
            match self.look(&self.input.clone(), &pics, new) {
                Ok(Some((block, answer))) => { picture = Some(block); pic_answer = answer.filter(|_| new); }
                Ok(None) => {}
                Err(end) => return end,
            }
        }
        // picture text belongs to the turn that looked: earlier turns' is dropped before this turn's message is added
        // (BUGS #57: it stuck to later turns and small models copied it into replies)
        if context::drop_picture_blocks(&mut self.history) { self.debug("[earlier picture text dropped from the history]"); }
        let mut content = vec![Block::Text { text }];
        content.extend(picture.map(|text| Block::Text { text }));
        // attached documents after the user's words, each its own block (`attach::MARK` first: notes keep only that line)
        content.extend(self.docs.drain(..).map(|(text, _)| Block::Text { text }));
        self.history.push(Message::new(Role::User, content));
        // tests: a request containing $ELISA_TEST_PANIC panics here (the recovery path, tests/fake_server.rs)
        if std::env::var("ELISA_TEST_PANIC").is_ok_and(|m| !m.is_empty() && self.input.contains(&m)) { panic!("injected test panic"); }
        let end = self.agent_loop(CheckRun::default(), true);
        if matches!(end, TurnEnd::Done) { if let Some(e) = self.doc_refusal_retry() { return e; } }
        if matches!(end, TurnEnd::Done) { if let Some(e) = self.denied_claim_retry() { return e; } }
        if matches!(end, TurnEnd::Done) { if let Some(e) = self.claimed_facts_retry() { return e; } }
        if let (TurnEnd::Done, Some(a)) = (&end, &pic_answer) { self.picture_refusal_fix(a); }
        self.doc_retry = None;
        self.set_doc_req(None);
        self.card_req = None;
        end
    }

    /// The line for an explicit web search or file read ask (`route::web_ask`, `route::file_ask`) when that tool is
    /// offered this turn
    /// A call to `sh`, `shell`, `terminal` or `zsh` when the session has no tool of that name: bash, its command from
    /// `command` or the first of `cmd` / `script` / `code` / `input`.
    fn shell_alias(&self, name: String, input: serde_json::Value) -> (String, serde_json::Value) {
        let alias = matches!(name.to_lowercase().as_str(), "sh" | "shell" | "terminal" | "zsh" | "run_shell" | "shell_command");
        if !alias || self.mcp.owns(&name) || self.tools.iter().any(|t| t.name == name) { return (name, input); }
        let cmd = ["command", "cmd", "script", "code", "input", "commands"].iter().find_map(|k| input.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .or_else(|| input.as_str().map(str::to_string)).unwrap_or_default();
        ("bash".to_string(), json!({"command": cmd}))
    }

    fn ask_note(&self, input: &str) -> Option<String> {
        let has = |n: &str| self.tools.iter().any(|t| t.name == n);
        // the router's answer: its web_search_now / save_file / read_file (and `file`) instead of the word checks
        if let Some(a) = &self.routed {
            if a.has("web_search_now") && has("web_search") { return Some(WEB_ASK_NOTE.to_string()); }
            if a.has("save_file") && has("write") {
                if let Some(f) = a.file.clone().or_else(|| crate::route::save_ask(input)) {
                    return Some(format!("{}to save it to {f}: call write now with the full content; never say it is saved before write succeeds.)", crate::route::ASK_PREFIX));
                }
            }
            if a.has("read_file") && has("read") {
                let f = a.file.clone().or_else(|| crate::route::file_ask(input))?;
                return Some(format!("{}to read {f}: call read on it now, then answer from what it returns; never guess what it says.)", crate::route::ASK_PREFIX));
            }
            return None;
        }
        if crate::route::web_ask(input) && has("web_search") { return Some(WEB_ASK_NOTE.to_string()); }
        if let Some(f) = crate::route::save_ask(input).filter(|_| has("write")) {
            // measured on qwen3-vl:2b: "save that plan to marketing-plan.md" got "I've saved it" with no write call
            return Some(format!("{}to save it to {f}: call write now with the full content; never say it is saved before write succeeds.)", crate::route::ASK_PREFIX));
        }
        let f = crate::route::file_ask(input).filter(|_| has("read"))?;
        Some(format!("{}to read {f}: call read on it now, then answer from what it returns; never guess what it says.)", crate::route::ASK_PREFIX))
    }

    /// An explicit ask to go back to the last conversation ("continue the last one", "resume", "what were we talking
    /// about?", "tiếp tục cuộc trò chuyện trước": `route::resume_intent`) in a session with no history yet: the
    /// newest earlier session of this folder is loaded the way `--continue` / `/resume` load it (its history, its own
    /// notes, the card), with an info line, and the turn goes on in that context (Ok(Some(kind)): the turn's hint).
    /// No earlier session: a plain line, no model call (Err: the turn's end). Anything else: Ok(None), and a new
    /// session carries nothing (the clean-new-session rule stands; only the user's explicit ask lifts it).
    fn resume_on_ask(&mut self, input: &str) -> Result<Option<crate::route::Resume>, TurnEnd> {
        if !self.history.is_empty() { return Ok(None); }
        let kind = match &self.routed {
            Some(a) if a.has("resume_last") => Some(crate::route::Resume::Continue),
            Some(a) if a.has("recap") => Some(crate::route::Resume::Recap),
            Some(_) => None,
            None => crate::route::resume_intent(input),
        };
        let Some(kind) = kind else { return Ok(None) };
        // the newest earlier session with something to continue: not one that only said hello or asked this already
        // (a first "continue the last one" that found nothing is a session too)
        // (`SessionStore::newest_real_session`, as `--continue`: a first "continue the last one" that got the intro is a
        // session too, and must not shadow the real one)
        let real = self.store.newest_real_session();
        let Some(info) = real.and_then(|(ts, _)| self.store.list_sessions().into_iter().find(|i| i.ts == ts)) else {
            let line = match crate::reply_lang::detect(input) {
                Some(crate::reply_lang::Lang::Vi) => "Chưa có cuộc trò chuyện nào trước đó trong thư mục này.",
                _ => "There's no earlier conversation in this folder yet.",
            };
            if self.interactive { self.fe.text_delta(line); }
            self.history.push(Message::user_text(input.to_string()));
            self.history.push(Message::assistant_text(line));
            return Err(TurnEnd::Done);
        };
        let Some(h) = self.store.load_session(&info.path).map(context::loaded).filter(|h| !h.is_empty()) else { return Ok(None) };
        let at = crate::situation::mtime(&info.path);
        self.slot_restore(&info.path.clone());
        self.last_failed = crate::situation::last_turn_flags(&h).1;
        (self.history, self.last_turn_at, self.real) = (h, at, None);
        let notes = self.store.load_session_notes(&info.path);
        self.adopt_notes(notes);
        self.restore_docs(Some(info.ts));
        self.load_topics(Some(info.ts));
        self.carry_card();
        self.notes_bg = false;
        let title = crate::secrets::redact(info.title.trim()).0;
        let title = if title.is_empty() { "untitled".to_string() } else { crate::context::one_line(&title, 60) };
        let age = at.map_or(String::new(), |t| format!(" · {} ago", crate::situation::ago(crate::situation::now_secs() - t)));
        self.fe.info(&format!("[continued your last session: {title}{age}]"));
        self.show_history();
        Ok(Some(kind))
    }

    /// A text reply cut by the output limit, continued: `sent` (the request that got it) plus the reply so far as an
    /// assistant message and CONTINUE_NOTE (in that request only), streamed into the same reply, up to CONTINUE_MAX
    /// times while it is still cut. Returns the whole text and the last stop reason.
    /// `st`: the status of the cut reply; the reply ends (`reply_done`) with the last part's.
    fn continue_reply(&mut self, sent: Vec<Message>, mut text: String, max_tokens: usize, in_words: bool, mut st: crate::frontend::Status) -> (String, String) {
        let mut stop = "max_tokens".to_string();
        for n in 0..CONTINUE_MAX {
            let mut msgs = sent.clone();
            msgs.push(Message::assistant_text(text.clone()));
            msgs.push(Message::user_text(CONTINUE_NOTE));
            let room = self.max_ctx.saturating_sub(self.fixed() + estimate_messages(&msgs) + context::ctx_margin(self.max_ctx) + 64 + self.client.reasoning_room());
            if room < MIN_OUT { break; }
            self.debug(&format!("[the reply hit the {max_tokens}-token output limit; continuing it ({}/{CONTINUE_MAX})]", n + 1));
            CANCEL.store(false, Ordering::SeqCst);
            let no_tools: [crate::types::ToolDef; 0] = [];
            let req = Request { system: &self.system, tools: if in_words { &no_tools } else { &self.tools }, messages: &msgs, max_tokens: max_tokens.min(room) };
            let interactive = self.interactive;
            let fe = &mut *self.fe;
            let mut on_text = |t: &str| if interactive { fe.text_delta(t); };
            self.client.set_kind("agent");
            let r = match self.client.send(&req, &CANCEL, &mut on_text) {
                Ok(r) => r,
                Err(e) => { self.debug(&format!("[continuing the reply failed: {e}]")); break; }
            };
            let (pc, estimated) = crate::backend::ctx_from(r.usage.input_tokens + r.usage.cache_read_input_tokens, self.fixed() + estimate_messages(&msgs));
            st = ui::status(&r, pc + r.usage.output_tokens, self.max_ctx);
            st.estimated = estimated;
            let more = texts(&Message::new(Role::Assistant, r.content.into_iter().filter(|b| matches!(b, Block::Text { .. })).collect()));
            stop = r.stop_reason;
            text.push_str(&more);
            // nothing more came (a reasoning model spent the budget): no use asking again
            if stop != "max_tokens" || more.trim().is_empty() { break; }
        }
        self.fe.reply_done(&st);
        (text, stop)
    }

    /// The follow-up hint for this turn (`followup.rs`): only when the last turn ended with an answer (its text, not
    /// Elisa's own `[stopped: …]` line). "more" / "go on" on a turn that carries tools means "resume the task", so
    /// it gets no hint there.
    fn follow_up_hint(&self, input: &str) -> Option<String> {
        let last = self.history.iter().rev().find(|m| m.role == Role::Assistant)?;
        // (not after a greeting or the generic intro either: nothing to go deeper into)
        let answered = last.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty() && !text.trim_start().starts_with('[') && !context::is_intro(text)));
        // a bare "yes" to an answer that offered several options: ask which one (the model picked the first and wrote code)
        if answered { if let Some(h) = crate::followup::pick_option(input, &texts(last)) { return Some(h.to_string()); } }
        let k = self.router_kind().unwrap_or_else(|| crate::followup::kind(input)).filter(|_| answered)?;
        let chat = self.route.groups.tool_free() && self.route.others.is_empty();
        // about an attached document: the hint keeps to what it says (2026-11-09: "go deeper" got content not in the paper)
        let doc = !self.kept_docs.is_empty() || self.doc_req.is_some() || self.route.groups.has(crate::route::DOCS)
            || self.history.iter().any(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if text.contains(crate::attach::MARK))));
        if k == crate::followup::Kind::More && !chat && !doc { return None; }
        Some(crate::followup::line_for(&k, doc))
    }

    /// The follow-up kind from the router's answer (Some(None): it saw no follow-up); None = no router answer.
    fn router_kind(&self) -> Option<Option<crate::followup::Kind>> {
        use crate::followup::Kind;
        let a = self.routed.as_ref()?;
        Some(if a.has("followup_why") { Some(Kind::Why) }
            else if a.has("followup_more") { Some(Kind::More) }
            else if a.has("pick_item") { a.item.map(|n| Kind::Pick(n as usize)) }
            else if a.has("what_about") { a.topic.clone().map(Kind::About) }
            else { None })
    }

    /// The situation line for this turn (`situation.rs`): only on a small-talk turn routed as chat ("hey", "thanks",
    /// "chào"), with `greet_context` on, and only when there is context to give.
    fn situation(&self, input: &str) -> Option<String> {
        if !self.cfg.greet_context || !self.route.groups.tool_free() || !self.route.others.is_empty() || !self.router_says("greeting").unwrap_or_else(|| crate::route::greeting(input)) { return None; }
        let now = crate::situation::now_secs();
        let vi = crate::reply_lang::detect(input) == Some(crate::reply_lang::Lang::Vi);
        let mut f = crate::situation::Facts { now, hour: crate::situation::local_hour(now), vi, ..Default::default() };
        if self.history.is_empty() {
            // a new conversation: the newest earlier session of this project, its title (or its last card's goal when
            // the title is small talk) to offer; never its notes
            let s = self.store.list_sessions().into_iter().next()?;
            let at = crate::situation::mtime(&s.path).unwrap_or(s.ts as i64);
            if now - at > crate::situation::RECENT_S { return None; }
            let topic = if !s.title.trim().is_empty() && !crate::route::small_talk(&s.title) && !crate::route::is_follow_up(&s.title) { Some(s.title.clone()) }
                else { crate::situation::saved_goal(&s.path) };
            f.past = topic.map(|t| (at, crate::secrets::redact(&t).0));
        } else {
            f.last_turn = self.last_turn_at.or(Some(now));
            (f.last_tools, f.last_failed) = { let (t, failed) = crate::situation::last_turn_flags(&self.history); (t, failed || self.last_failed) };
            // the topic: the latest request of this conversation that was not small talk or a short follow-up, from the
            // last few user turns only (bug #8: an old one is stale)
            f.topic = self.history.iter().rev().filter(|m| m.role == Role::User)
                .filter_map(|m| m.content.iter().find_map(|b| match b { Block::Text { text } => Some(context::request_body(text).trim()), _ => None }))
                .take(crate::situation::TOPIC_TURNS)
                .find(|t| !t.is_empty() && !crate::route::small_talk(t) && !crate::route::is_follow_up(t)).map(|t| crate::secrets::redact(t).0);
            // the next step: the current state card (the memory store's latest on `--continue`), else the history's last card
            // the next step: the card this turn's topic picked (none: no next step; never another topic's card)
            f.next_step = self.topic.card.and_then(|c| self.topic_cards.get(c)).and_then(|c| crate::situation::card_field(&c.text, "next"))
                .map(|n| crate::secrets::redact(&n).0);
        }
        crate::situation::line(&f)
    }

    /// The agent loop of a turn: the history ends with a user message. `first` = that message is the turn's first
    /// (a failed send drops it again).
    fn agent_loop(&mut self, mut checks: CheckRun, mut first: bool) -> TurnEnd {
        // one correction per turn when a reply writes a tool call as text
        let mut nudged = false;
        let mut repeat = Repeat::default();
        // repeated identical calls, short cycles, steps without progress (`loop_guard.rs`)
        let mut guard = crate::loop_guard::Guard::default();
        // a document whose text is in this turn's message is not opened with a file tool (BUGS #54)
        if self.doc_req.is_some() { guard.attached(self.kept_docs.iter().map(|k| k.doc.name.clone()).collect()); }
        // a loop stop asks the model once for an answer with no tools before the turn stops (BUGS #54): its reason
        let mut final_ask: Option<String> = None;
        // a reply with no visible text (only its state card): asked once more, with this nudge after the history for
        // that one request (it never enters the history, so the next requests' prefix is unchanged)
        let mut empty_retry: Option<Message> = None;
        let mut retried = false;
        let mut stashed_card: Option<String> = None;
        // a simple turn's reply that used its whole small budget with no text for the user: sent once more without
        // the simple-turn limits (longrun2: a two-question turn judged simple showed only the stop line)
        let mut widened = false;
        // a small-talk turn whose reply called a tool: asked once more with no tools and a note in that request only
        // (or a turn asked again in words with its own note: `retry_note`)
        let mut words_retry = self.retry_note.is_some();
        let mut words_retried = words_retry;
        // one step past the limit: only for the no-tools final answer after the last tool step
        let max_steps = self.cfg.max_steps.max(1);
        for step in 0..=max_steps {
            if step == max_steps && final_ask.is_none() { break; }
            if !self.mid_turn_fold() {
                // run2 call 30: sent anyway, 8,199 tokens against 8,192, and the server returned nothing
                // only this turn is left and it is still too big (a very long message): nothing more to make room from
                self.history.push(Message::assistant_text("[stopped: context full]"));
                self.fe.info(&format!("[stopped: this message (~{} tokens) is too long for the model's {}-token context; send a shorter one]", ui::k(self.est_safe()), ui::k(self.max_ctx)));
                return TurnEnd::Stopped;
            }
            CANCEL.store(false, Ordering::SeqCst);
            let max_tokens = self.cfg.max_output.min(self.max_ctx.saturating_sub(self.est_safe() + context::ctx_margin(self.max_ctx) + 64)).max(MIN_OUT);
            // a simple turn where the model does not reason: a smaller cap (`Client::simple_cap`)
            let capped = self.client.simple_cap(self.max_ctx);
            let max_tokens = capped.map_or(max_tokens, |c| max_tokens.min(c));
            let interactive = self.interactive;
            let now_line = crate::situation::model_now(&self.cfg.model, self.client.caps().vision == Some(true));
            self.fe.waiting();
            self.acts.model_wait_on(&mut *self.fe, crate::provider::is_local(&self.cfg.base_url), self.client.caps().always_loaded());
            let fe = &mut *self.fe;
            let acts = &mut self.acts;
            let mut on_text = |t: &str| if interactive {
                if !t.trim().is_empty() { acts.model_text(&mut *fe); }
                fe.text_delta(t);
            };
            let retry_msgs: Vec<Message>;
            let retrying = empty_retry.is_some();
            let in_words = std::mem::take(&mut words_retry);
            // the same reply twice running: the older copy is left out of the request (`context::drop_repeated_replies`)
            // turns another model answered (a model switch): out of the assistant role (`context::other_model_view`)
            let switched = context::other_model_view(&self.history, &self.cfg.model, &now_line, context::tiny_window(self.max_ctx));
            let base: &[Message] = switched.as_deref().unwrap_or(&self.history);
            let dedup = context::drop_repeated_replies(base);
            let base: &[Message] = dedup.as_deref().unwrap_or(base);
            // a long history: old answers shortened, for this request only (`context::shorten_old_replies`); the
            // current turn's request is the boundary, so every request of a turn has the same prefix
            let short = context::shorten_old_replies(base, base.iter().rposition(is_request).unwrap_or(base.len()), self.max_ctx);
            let base: &[Message] = short.as_deref().unwrap_or(base);
            // this turn's document text, in the turn's user message for these requests only (`doc_req`)
            let with_doc: Vec<Message>;
            // and this turn's card (`card_req`) before it
            let extra: Vec<String> = self.card_req.iter().chain(self.doc_req.iter()).cloned().collect();
            let base: &[Message] = match extra.is_empty() {
                false if !base.is_empty() => {
                    // the turn's user message (`turn_cap`; spelled out: `self.fe` is borrowed here)
                    let at = base.iter().rposition(|m| m.role == Role::User && m.content.iter()
                        .any(|b| matches!(b, Block::Text { text } if !text.starts_with(context::NUDGE_PREFIX)))).unwrap_or(base.len() - 1);
                    with_doc = base.iter().enumerate().map(|(i, m)| if i == at {
                        let mut m = m.clone();
                        m.content.extend(extra.iter().map(|t| Block::Text { text: t.clone() }));
                        m
                    } else { m.clone() }).collect();
                    &with_doc
                }
                _ => base,
            };
            let messages: &[Message] = match empty_retry.take() {
                Some(n) => { retry_msgs = base.iter().cloned().chain([n]).collect(); &retry_msgs }
                None if in_words => { retry_msgs = with_note(base, if final_ask.is_some() { FINAL_ANSWER_NOTE } else { self.retry_note.as_deref().unwrap_or(SMALL_TALK_NOTE) }); &retry_msgs }
                None => base,
            };
            let no_tools: [crate::types::ToolDef; 0] = [];
            let req = Request { system: &self.system, tools: if in_words { &no_tools } else { &self.tools }, messages, max_tokens };
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
            // a reply cut by the output limit is continued from this same request (`continue_reply`)
            let sent: Option<Vec<Message>> = (reply.stop_reason == "max_tokens").then(|| messages.to_vec());
            let prompt_real = reply.usage.input_tokens + reply.usage.cache_read_input_tokens;
            // a cancelled (Esc) or unreported reply has no usage: the partial output's estimate stands in for it
            if prompt_real == 0 && reply.usage.output_tokens == 0 {
                reply.usage.output_tokens = estimate_messages(&[Message::new(Role::Assistant, reply.content.clone())]).saturating_sub(MSG_OVERHEAD_TOKENS);
            }
            // Anthropic usage: input_tokens excludes the cached (reused) part. Without it the context is the estimate
            // of the request just sent plus that output (uitest: ctx_used 0 after Esc reset the app's ctx meter)
            // no count from the server: Elisa's estimate, marked in the status line (`backend::ctx_from`)
            let (prompt_ctx, estimated) = crate::backend::ctx_from(prompt_real, self.estimate());
            let ctx_used = prompt_ctx + if estimated { reply.usage.output_tokens } else { reply.usage.output_tokens.saturating_sub(reply.usage.reasoning_tokens) };
            if !estimated && !retrying { self.calib = Some((prompt_real, self.fixed() + estimate_messages(&self.history))); }
            let mut st = ui::status(&reply, ctx_used, self.max_ctx);
            st.estimated = estimated;
            // a text reply cut by the output limit is about to be continued into the same reply: it ends after that
            let cut_text = sent.is_some() && !reply.content.iter().any(|b| matches!(b, Block::ToolUse { .. }))
                && reply.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty()));
            let mut pending_done = cut_text.then_some(st);
            if !cut_text { self.fe.reply_done(&st); }
            self.acts.model_done();
            let Reply { content, stop_reason, id, usage, .. } = reply;
            let reasoning = usage.reasoning_tokens;
            let id = Some(id).filter(|i| !i.is_empty());
            let has_tools = content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
            let run_tools = has_tools && stop_reason != "cancelled" && stop_reason != "max_tokens";
            // asked for a final answer with no tools and it called one anyway: the turn stops as the loop guard said
            if let (true, true, Some(why)) = (in_words, has_tools, &final_ask) {
                if let Some(st) = pending_done.take() { self.fe.reply_done(&st); }
                self.history.push(Message::assistant_text(format!("[stopped: {why}]")));
                self.fe.info(&format!("[stopped: {why}; say how to proceed]"));
                return TurnEnd::Stopped;
            }
            // small talk ("hey", "thanks", "ok") routed as chat: a tool call there (a real one, or one written as text)
            // is not run and does not widen the route (measured on qwen3-vl:2b: "hey" after a turn the loop guard
            // stopped called tools, the widen path ran them, and the turn looped to 11-31K tokens). It is asked once
            // more in words with no tools; still calls: a plain fallback line instead of a loop.
            if self.small_talk_turn && stop_reason != "cancelled" && (has_tools || context::looks_like_text_tool_call(&texts(&Message::new(Role::Assistant, content.clone())))) {
                if let Some(st) = pending_done.take() { self.fe.reply_done(&st); }
                if !words_retried {
                    (words_retry, words_retried) = (true, true);
                    self.debug("[small talk: the reply called a tool; asking once more in words, with no tools]");
                    continue;
                }
                (self.new_card, self.card_broken) = (None, false);
                let line = small_talk_fallback(&self.input);
                if self.interactive { self.fe.text_delta(line); }
                self.history.push(Message { role: Role::Assistant, content: vec![Block::Text { text: line.into() }], id, model: None });
                self.real = Some((self.history.len(), ctx_used));
                self.debug("[small talk: the reply called a tool again; showed the fallback line]");
                return TurnEnd::Done;
            }
            // a tool call cut by the output limit does not run (mario.html: a cut write was saved as finished)
            let cut = (has_tools && stop_reason == "max_tokens").then(|| content.iter().filter_map(|b| match b {
                Block::ToolUse { name, input, .. } => Some(input["path"].as_str().map_or(name.clone(), |p| format!("{name} {p}"))),
                _ => None,
            }).collect::<Vec<_>>().join(", "));
            if !run_tools {
                // the state card never enters history (append-only, prefix stays valid): only the visible text is kept
                let mut raw = texts(&Message { role: Role::Assistant, content, ..Default::default() });
                // a text reply cut by the output limit mid-answer is continued, into the same reply, up to
                // CONTINUE_MAX times (2026-11-09: three "continue" turns in a row stopped mid-sentence at 1024 tokens)
                let mut stop_reason = stop_reason;
                let mut continued = false;
                if let (Some(sent), Some(st), None) = (sent, pending_done.take(), &cut) {
                    if !context::split_state(&raw).0.trim().is_empty() {
                        (raw, stop_reason) = self.continue_reply(sent, raw, max_tokens, in_words, st);
                        continued = true;
                    } else { self.fe.reply_done(&st); }
                }
                let (vis, card, broken) = context::split_state(&raw);
                // one of Elisa's request-only notes repeated as the reply: never shown (`route::strip_echoed_notes`)
                // picture text copied into the reply too (`context::strip_picture_echo`, BUGS #57)
                let clean = crate::route::strip_echoed_notes(&context::strip_picture_echo(&vis));
                let vis = if clean != vis.trim() && clean != vis {
                    self.fe.reply_retracted();
                    if self.interactive && !clean.is_empty() { self.fe.text_delta(&clean); }
                    self.debug("[the reply repeated one of Elisa's notes; that part was removed]");
                    clean
                } else { vis };
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
                if !wrote && cut.is_none() && stop_reason == "max_tokens" && capped.is_some() && !widened {
                    widened = true;
                    self.client.set_simple(false);
                    stashed_card = self.new_card.take().or(stashed_card);
                    self.fe.info(&format!("[the reply used its whole {max_tokens}-token simple-turn budget and wrote no text for you; asking once more without that limit]"));
                    continue;
                }
                // reasoned through its whole budget, no answer (BUGS #78): once more without reasoning, full budget
                if !wrote && cut.is_none() && stop_reason == "max_tokens" && reasoning > 0 && self.client.stop_reasoning() {
                    stashed_card = self.new_card.take().or(stashed_card);
                    empty_retry = Some(Message::user_text(REASONED_OUT_NUDGE));
                    self.fe.info("[the model spent its whole budget reasoning and wrote no answer; asking once more without reasoning]");
                    continue;
                }
                if !wrote && normal_end {
                    self.fe.info("[no answer: the model wrote no text for you twice; ask again or rephrase]");
                }
                let mut content: Vec<Block> = if vis.is_empty() { Vec::new() } else { vec![Block::Text { text: vis }] };
                if let Some(c) = &cut { content.push(Block::Text { text: format!("[cut off at the {max_tokens}-token output limit: `{c}` did not run]") }); }
                if content.is_empty() {
                    content.push(Block::Text { text: crate::route::no_answer_note(&stop_reason).into() });
                }
                // a call cut by the limit: tell the model why, once, so it can use smaller pieces
                if let (Some(c), false) = (&cut, nudged) {
                    nudged = true;
                    (self.new_card, self.card_broken) = (None, false);
                    self.history.push(Message { role: Role::Assistant, content, id, model: None });
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
                    self.history.push(Message { role: Role::Assistant, content, id, model: None });
                    // routed: the call written as text may be for a tool this turn left out: offer every group
                    if self.cfg.route == "auto" && self.route.groups.0 & (crate::route::ALL & !crate::route::OTHER) != (crate::route::ALL & !crate::route::OTHER) {
                        self.route.groups = crate::route::Groups(crate::route::ALL & !crate::route::OTHER);
                        self.route.why = crate::route::Why::Widened;
                        self.rebuild_system();
                    }
                    let names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
                    self.history.push(Message::user_text(context::tool_text_nudge(&names)));
                    self.real = Some((self.history.len() - 1, ctx_used));
                    self.fe.info("[the reply wrote a tool call as text, so nothing ran; asking the model once to call it properly]");
                    continue;
                }
                self.history.push(Message { role: Role::Assistant, content, id: id.clone(), model: None });
                // the gateway audits every reply: its audit line shows once it finishes
                if let Some(i) = &id { self.watch.mine(i); }
                // a continued reply is longer than the server's count of the first part
                self.real = (!continued).then_some((self.history.len(), ctx_used));
                return match stop_reason.as_str() {
                    "cancelled" => { self.fe.info("[cancelled]"); TurnEnd::Stopped }
                    "max_tokens" => {
                        match &cut {
                            Some(c) => self.fe.info(&format!("[stopped: `{c}` was cut off at the {max_tokens}-token output limit, so it did not run and nothing was saved; say \"continue\" to go on]")),
                            None if !wrote && reasoning > 0 => self.fe.info(&format!("[stopped: the model spent its whole output budget ({reasoning} tokens) reasoning and wrote no answer; ask again, or more simply]")),
                            None if !wrote => self.fe.info(&format!("[stopped: the reply used its whole {max_tokens}-token output limit and wrote no text for you; ask again, or more simply]")),
                            None if continued => self.fe.info("[reply was long; say \"continue\" for more]"),
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
            // `sh`, `shell`, `terminal`: the model meant bash (qwen3-vl:2b called `sh` for "run ls", 2 of 2)
            let content: Vec<Block> = content.into_iter().map(|b| match b {
                Block::ToolUse { id, name, input } => { let (name, input) = self.shell_alias(name, input); Block::ToolUse { id, name, input } }
                b => b,
            }).collect();
            let calls: Vec<(String, String, serde_json::Value)> = content.iter().filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            }).collect();
            self.history.push(Message { role: Role::Assistant, content, id, model: None });
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
                    let plugin_gone = self.plugin_tool_gone(name);
                    let mut offered = self.tools.iter().any(|t| t.name == *name);
                    if !offered && plugin_gone.is_none() && self.widen_for(name) { offered = true; }
                    let verdict = guard.check(name, input, !self.mcp.owns(name));
                    let phrase = crate::activity::describe(name, input, &self.cwd, self.mcp.server_of(name), self.mcp.remote_of(name));
                    let aid = self.acts.begin(&mut *self.fe, &phrase);
                    self.fe.tool_start(&summary);
                    // an MCP server starts on the first call to one of its tools, before the approval (`mcp_start`)
                    let start_err = if plugin_gone.is_none() && offered && self.mcp.owns(name) && !matches!(verdict, crate::loop_guard::Verdict::Block(_)) {
                        // its first, lazy start says so ("Starting the web reader…")
                        let srv = self.mcp.server_of(name).map(String::from);
                        let cold = srv.as_deref().is_some_and(|s| !self.mcp.started(s));
                        if let (true, Some(s)) = (cold, &srv) { self.acts.say(&mut *self.fe, aid, Some(&crate::activity::starting(s)), false); }
                        let r = self.mcp_start(name).err();
                        if cold { self.acts.say(&mut *self.fe, aid, None, false); }
                        r
                    } else { None };
                    let (content, is_error) = if let Some(why) = plugin_gone {
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "plugin_removed")]);
                        self.fe.tool_done(&why, true);
                        (why, true)
                    } else if !offered && self.mcp.gated_server(name).is_some() {
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
                    } else if let Some(why) = start_err {
                        self.audit_tool("tool_call", name, &summary, &[("outcome", "error"), ("detail", &why)]);
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
                                let cache_before = if self.mcp.owns(name) { Some(crate::cache::session_hits()) } else { None };
                                let mut o = if self.mcp.owns(name) {
                                    self.mcp.call(name, input, &CANCEL, self.cfg.tool_output_max_chars)
                                } else { tools::run(name, input, &self.tctx) };
                                // served from Elisa's result cache: a line in debug mode only
                                if let Some(b) = cache_before {
                                    let hit = crate::cache::new_hits(&b, &crate::cache::session_hits());
                                    if !hit.is_empty() { self.debug(&format!("[cache hit: {}]", hit.join(", "))); }
                                }
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
                    self.acts.end(&mut *self.fe, aid, name, &content, is_error);
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
            // the same call and the same result again (Test 2026-11-01 ran one failing command ~15 times until the
            // context was full), or the loop guard's stop: the model is asked once to answer with what it has, with no
            // tools (BUGS #54: the turn ended with no reply); a second stop ends the turn
            // the step limit after a tool step: the same one no-tools answer, so the turn never ends with no reply
            let at_limit = step + 1 >= max_steps;
            let stop = if repeat.max >= REPEAT_MAX { Some(repeat.why()) } else { guard_stop };
            if final_ask.is_none() && (stop.is_some() || at_limit) {
                let who = if stop.is_some() { "loop guard" } else { "step limit" };
                let why = stop.unwrap_or_else(|| format!("the {max_steps}-step limit was reached"));
                self.fe.info(&format!("[{who}: {why}; asking the model to answer with what it has, no tools]"));
                (final_ask, words_retry) = (Some(why), true);
                continue;
            }
            if at_limit && stop.is_none() { break; }
            if let Some(why) = stop {
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

    /// `--continue` / `/resume`: the next card chains to the project's latest one in memory (a card /reset or
    /// /forget dropped excepted). The card is not sent because of this: a turn sends the card its topic picks
    /// (`load_topics`, `pick_topic`). A new session never calls this.
    fn carry_card(&mut self) {
        if self.card_id.is_some() { return; }
        let reset = self.store.card_reset();
        let Some(m) = self.mem.as_ref() else { return };
        self.card_id = m.db.latest_card(&m.project).map(|c| c.0).filter(|id| reset.is_none_or(|r| *id > r));
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
        if self.tiny() { return context::first_message_bare(self.first_notes().as_deref(), self.state_block().as_deref(), &[], input); }
        context::first_message_with(&self.cwd, self.first_notes().as_deref(), self.state_block().as_deref(), input)
    }

    /// The notes as a first message carries them: as they are while they are this session's own, marked as background
    /// (without the earlier session goal) in a new session and after /clear or /reset.
    fn first_notes(&self) -> Option<String> {
        let n = self.notes.as_deref()?;
        Some(if self.notes_bg { context::background_notes(n) } else { n.to_string() })
    }

    /// The `[state]` body of a first message: the session goal, then this project's pinned facts (newest first within
    /// PINNED_CAP tokens, listed oldest first). None when there is neither.
    fn state_block(&self) -> Option<String> {
        // only the facts the user pinned (/pin): the facts of a session's notes stay with that session (its notes
        // carry them; a new session starts clean and gets earlier facts only through recall)
        let facts = self.mem.as_ref().map(|m| m.db.user_pinned_facts(&m.project)).unwrap_or_default();
        // the first message carries the notes too: a fact they already say is not repeated here
        let facts = context::facts_not_in(self.notes.as_deref(), facts);
        let (mut keep, mut used) = (Vec::new(), 0);
        for f in facts.iter().rev() {
            // the user typed the fact: "my name is Linh" is shown as theirs (`context::users_words`), never as Elisa's
            let l = format!("- {}", context::users_words(&context::one_line(f, 200)));
            let t = estimate_tokens(&l);
            if used + t > PINNED_CAP { break; }
            used += t;
            keep.push(l);
        }
        keep.reverse();
        // no state card here: a turn's card is the one its topic picks, in that turn's message (`pick_topic`)
        // the session goal apart from the current request (the card's `goal:` follows the newest request)
        // (not while the current request is the goal itself: the request section says it)
        let req = self.user_req.clone();
        let asking = self.in_turn;
        // in the user's voice: "fix my build" reads as the user's, not Elisa's (`context::users_words`)
        // (a goal the request holds, "Hi, I'm Linh…" for "I'm Linh…", is the request too)
        let goal = self.goal.as_deref().filter(|g| !(asking && req.contains(*g))).map(|g| format!("session goal: {}", context::users_words(g)));
        let cur = goal.as_ref().filter(|_| asking && !req.is_empty()).map(|_| format!("current request: {}", context::one_line(&req, 160)));
        if keep.is_empty() && goal.is_none() { return None; }
        let mut s = String::new();
        for l in [goal, cur].into_iter().flatten() {
            if !s.is_empty() { s.push('\n'); }
            s += &l;
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
        self.topic_cards.clear();
        (self.topic, self.card_req) = (Default::default(), None);
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
        // kept with its topic (the document it names or this turn's), and it is this topic's card from now on
        let docs: Vec<crate::topics::DocTopic> = self.kept_docs.iter().map(|k| k.topic.clone()).collect();
        let doc = crate::topics::card_doc(&card, &docs, self.topic.doc);
        if self.topic_cards.len() >= 64 { self.topic_cards.remove(0); self.topic.card = None; }
        self.topic_cards.push(crate::topics::Card { id: saved, doc: doc.clone(), text: card.clone() });
        self.topic.card = Some(self.topic_cards.len() - 1);
        if self.topic.doc.is_none() { self.topic.doc = doc.and_then(|n| docs.iter().position(|d| d.name == n)); }
        self.card = Some(card);
        self.card_ex = Some(self.exchanges);
    }

    /// After a saved session is loaded (`--continue`, `/resume`, "continue the last one"): all of its cards with their
    /// topics (`.elisa/session-meta/<id>.cards.json`; an older session without one: the memory store's recent cards
    /// that name one of its documents), and nothing picked: the next message's words pick a topic (`pick_topic`).
    /// A card an older Elisa put into the saved first message's `[state]` is taken out of it.
    fn load_topics(&mut self, session: Option<u64>) {
        self.topic = crate::topics::Pick::default();
        self.card_req = None;
        let mut cards = session.and_then(|id| self.store.load_cards(id)).unwrap_or_default();
        if cards.is_empty() {
            let reset = self.store.card_reset();
            let docs: Vec<crate::topics::DocTopic> = self.kept_docs.iter().map(|k| k.topic.clone()).collect();
            let chain = self.mem.as_ref().and_then(|m| m.db.latest_card(&m.project).map(|(id, _)| m.db.card_chain(id, 16))).unwrap_or_default();
            for (id, text) in chain.into_iter().rev() {
                if reset.is_some_and(|r| id <= r) { continue; }
                let Some(c) = context::clean_card(&text) else { continue };
                if let Some(doc) = crate::topics::card_doc(&c, &docs, None) { cards.push(crate::topics::Card { id: Some(id), doc: Some(doc), text: c }); }
            }
        }
        self.topic_cards = cards;
        let mut stripped = 0;
        for m in self.history.iter_mut().filter(|m| m.role == Role::User) {
            for b in m.content.iter_mut() {
                if let Block::Text { text } = b { if let Some(t) = context::strip_state_card(text) { *text = t; stripped += 1; } }
            }
        }
        if stripped > 0 { self.real = None; }
        self.debug(&format!("[resumed topics: {} card(s), {} document(s); none picked until a message names one{}]", self.topic_cards.len(), self.kept_docs.len(),
            if stripped > 0 { "; the saved first message's card taken out" } else { "" }));
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
        // what other Elisa processes stored since this one last wrote is searched too (`Memory::refresh`)
        let Some((hits, shown)) = self.mem_call(|m| { let _ = m.db.refresh(); m.db.recall(query, &m.project, k, true) }) else { return Vec::new() };
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
        // (what the user told about themselves is the best memory there is: no x0.5 for it)
        for (h, i) in hits.iter_mut().zip(&info) { if matches!(i, Some((true, _))) && context::user_facts(&h.text).is_empty() { h.score *= 0.5; } }
        let asked: Vec<u64> = hits.iter().zip(&info).filter(|(_, i)| matches!(i, Some((true, _)))).map(|(h, _)| h.id).collect();
        let better: Vec<u64> = hits.iter().zip(&info)
            .filter(|(h, i)| h.score >= min && (h.kind == HitKind::Card || matches!(i, Some((false, _))) && context::has_tool_line(&h.text)))
            .filter(|(h, _)| context::strong_recall(query, &h.text) && !context::unfit_memory(&h.text))
            .map(|(h, _)| h.id).collect();
        let mut hits: Vec<elisa_mem::Hit> = hits.into_iter().zip(info)
            .filter(|(_, i)| !matches!(i, Some((true, rel)) if rel.iter().any(|r| better.contains(r))))
            .map(|(h, _)| h).collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut cand: Vec<elisa_mem::Hit> = Vec::new();
        // /forget: this project's memories from before it are gone for the user
        let forgot = self.store.forgot();
        for h in hits {
            if h.score < min || !context::recall_passes(h.lex, h.support, mid, min, min_support) { continue; }
            // only a clear match, never a refusal / failed tool / error answer, and of the user's own words only what
            // they told about themselves (an old question or command adds nothing; BUGS #16, #27, #34)
            if !context::strong_recall(query, &h.text) || context::unfit_memory(&h.text) { continue; }
            if asked.contains(&h.id) && context::user_facts(&h.text).is_empty() { continue; }
            if h.kind == HitKind::Turn && !asked.contains(&h.id) && !context::has_tool_line(&h.text) { continue; }
            if forgot.is_some_and(|f| h.id < f) && m.db.in_project(h.id, &m.project) { continue; }
            match h.kind {
                // this session's own turns are in the window or its notes already: recalling one only repeats it
                // (2026-11-09: Elisa's own reply from a moment ago came back as [memory])
                HitKind::Turn if h.session == m.session => continue,
                HitKind::Card if cards.contains(&h.id) => continue,
                _ => {}
            }
            // an earlier lookup's answer on a turn that can look again: the model reads / searches now instead
            let g = self.route.groups;
            if context::stale_lookup(&h.text, g.has(crate::route::CODE), g.has(crate::route::WEB)) { continue; }
            if h.lex < strong && !m.db.in_project(h.id, &m.project) { continue; }
            // hits come best first: a Card/Turn twin of a kept hit is the weaker copy
            let twin = |a: &elisa_mem::Hit| matches!((&a.kind, &h.kind), (HitKind::Card, HitKind::Turn) | (HitKind::Turn, HitKind::Card))
                && context::word_overlap(&a.text, &h.text) >= 0.6;
            if cand.iter().any(twin) { continue; }
            let flat = context::one_line(&h.text, usize::MAX);
            let key: String = flat.chars().take(80).collect();
            if key.is_empty() || hay.contains(&key) { continue; }
            // the same text, worded a little differently, is already in the visible conversation
            if self.history.iter().any(|msg| context::word_overlap(&h.text, &texts(msg)) >= 0.7) { continue; }
            // the user's own words as theirs ("The user's name is Linh"), never read as Elisa's ("My name is Elisa")
            let mut h = h;
            if asked.contains(&h.id) {
                for v in context::user_fact_values(&h.text) { if !self.recalled_values.contains(&v) { self.recalled_values.push(v); } }
                h.text = context::users_words(&h.text);
            }
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
        let (v, old) = (self.verified(&n), self.notes.clone());
        let m = self.merge_kept(old.as_deref(), &v);
        let n = self.finish_notes(m);
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
        self.acts.waiting_ok(&mut *self.fe, true);
        let a = self.fe.approve_outside(name, summary, o);
        if a != Approval::No { self.acts.waiting_ok(&mut *self.fe, false); }
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
            Approval::No => Err(format!("denied by user ({what} outside the project folder: {list}): {SAID_NO}")),
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
        let (n, note) = match crate::audit::count_arg(arg) { Ok(x) => x, Err(u) => { self.fe.error(&u); return } };
        let lines = crate::audit::last(&path, n);
        if let Some(t) = note { self.fe.info(&t); }
        if !lines.is_empty() { self.fe.info(&lines.join("\n")); }
        match crate::audit::verify(&path) {
            Ok(k) => self.fe.info(&format!("chain ok ({k} entries) · {}", path.display())),
            Err((l, why)) => self.fe.error(&format!("chain broken at line {l}: {why} · {}", path.display())),
        }
    }

    /// The call's `path` is a file in this session's upload area (`attach.rs`), and `docs` is Elisa's own server (no user
    /// server of that name).
    fn reads_upload(&self, input: &serde_json::Value) -> bool {
        if self.cfg.mcp.iter().any(|c| c.name == "docs") { return false; }
        let Some(p) = input["path"].as_str() else { return false };
        let root = outside::root(&self.cwd);
        outside::resolve(&root, std::path::Path::new(p)).starts_with(crate::attach::upload_dir(&root, context::run_id()))
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
            // the built-in documents server reading a document the person attached this session: they gave it to read
            if !untrusted && server.as_deref() == Some("docs") && self.reads_upload(input) { return Ok(()); }
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
        self.acts.waiting_ok(&mut *self.fe, true);
        let a = match &offer {
            Some(p) => self.fe.approve_rule(name, &summary, server.as_deref(), p),
            None => self.fe.approve(name, &summary, server.as_deref()),
        };
        if a != Approval::No { self.acts.waiting_ok(&mut *self.fe, false); }
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
            Approval::No => Err(format!("denied by user: {SAID_NO}")),
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

/// added to a denial: nothing happened, say so, and do not ask again for the same thing (measured on qwen3-vl:2b:
/// with only "do not call this tool again…; answer without it" a denied write got "I've saved "hello" to deny.txt")
const NO_RETRY: &str = "NOTHING was done: no file was written or changed and no command ran. Tell the user plainly that it was \
    not done and why; never say it was saved or done. Do not call this tool again for this request";
/// a call the user said no to: nothing happened, and a sample sentence (the 2B model, told only "say why", said
/// "I don't have the capability to write files")
const SAID_NO: &str = "the user said no, so NOTHING was done: no file was written or changed and no command ran. Reply in one \
    short sentence like \"Okay, I didn't do that, since you said no.\"; never say it was saved or done. Do not call this tool again for this request";

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
/// The one request after a reply that reasoned through its whole budget (BUGS #78): sent with reasoning off, after
/// the history for that request only. Without it qwen3-vl:2b answered a long pasted "just reply OK" message with a
/// copy of its earlier greeting (2 of 2 retries).
const REASONED_OUT_NUDGE: &str = "[elisa] Your last reply ran out of room while reasoning and wrote nothing. Answer \
    the user's last message above now, directly, doing what it asks.";

const EMPTY_REPLY_NUDGE: &str ="[elisa] Your last reply had no text for the user, only the <state> card. Answer the user's \
    message in one plain sentence first; the <state> card goes after it.";

/// The note of the one-time words-only retry of a small-talk turn whose reply called a tool (`agent_loop`): after
/// the user's words, in that request only (it never enters the history).
const SMALL_TALK_NOTE: &str = "(Reply in words; no tools for this message.)";
/// the request-only note of the no-tools request after a loop stop (`agent_loop`)
const FINAL_ANSWER_NOTE: &str = "(No more tool calls. Answer the user now, in words, from what you already have: the tool results above and any document text in the user's message. If something is still missing, say what.)";
/// The note of a turn whose user explicitly asked to search the web (`route::web_ask`; `agent_loop`): after the user's
/// words, in the requests before a tool runs only. Measured on qwen3-vl:2b with the web tools offered: "search the web:
/// what is the capital of Australia?" was answered from memory 3/3 without it (a system-prompt rule alone: 0/3), a
/// web_search call 3/3 with it.
const WEB_ASK_NOTE: &str = "\n\n(The user asked for a web search: call web_search now, then answer from its results, even if you know the answer.)";
/// the user note of a request that continues a reply cut by the output limit (`continue_reply`), in that request only
const CONTINUE_NOTE: &str = "Continue exactly where you stopped.";
/// continuations of one cut reply at most
const CONTINUE_MAX: usize = 2;

/// The history with `note` after the last user message's words (a copy for one request; the history is unchanged).
fn with_note(history: &[Message], note: &str) -> Vec<Message> {
    let mut h = history.to_vec();
    if let Some(m) = h.iter_mut().rev().find(|m| m.role == Role::User) {
        match m.content.iter_mut().find_map(|b| match b { Block::Text { text } => Some(text), _ => None }) {
            Some(t) => { t.push_str("\n\n"); t.push_str(note); }
            None => m.content.push(Block::Text { text: note.into() }),
        }
    }
    h
}

/// The plain line a small-talk turn shows when the reply called a tool even when asked in words, in the user's
/// language (Vietnamese when the message is: `reply_lang::detect`).
fn small_talk_fallback(input: &str) -> &'static str {
    match crate::reply_lang::detect(input) {
        Some(crate::reply_lang::Lang::Vi) => "Chào bạn! Bạn muốn làm gì tiếp theo?",
        _ => "Hi! What would you like to do next?",
    }
}

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
