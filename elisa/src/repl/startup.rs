use super::*;
use super::memory::texts;

pub static CANCEL: AtomicBool = AtomicBool::new(false);
pub static AT_PROMPT: AtomicBool = AtomicBool::new(false);
pub static ASKING: AtomicBool = AtomicBool::new(false);

const USAGE: &str = "usage: elisa [-p PROMPT] [--yes] [--continue | --continue-open] [--provider NAME] [--base-url U [--type T]] [--model M] [--max-ctx N] [--cwd DIR] [--no-character] [--companion] [--serve-jsonl] [--no-checks] [--attach FILE]… [--theme dark|light] [--version] [-h]\n       elisa eval [--suite NAME|all] [--case ID] [--model M] [--json FILE] [--keep]\n       elisa plugin list | add PATH [--yes] | remove NAME | enable NAME | disable NAME | update NAME [--yes] | trust NAME [--yes]\n       elisa sandbox list [--json] | pull NAME|IMAGE|stable|project|all [--yes] [--json]";

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

pub(super) fn help_text() -> String {
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
        // was said in it; plain --continue skips a dead session (`newest_real_session`)
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
    // notes at all; its goal comes from what the user asks in this conversation (a small model can answer
    // "hey" with an earlier session's task when the old notes are carried, even marked as background). Earlier
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
impl<'f> Session<'f> {

    /// A resumed conversation's earlier turns on screen (`context::earlier_turns`): the app shows them as blocks.
    pub(super) fn show_history(&mut self) {
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

    fn run_repl(&mut self) {
        loop {
            self.watch_poll();
            let Some(line) = self.fe.read_input() else { break };
            // a panic anywhere in the line's work (a turn, a command, the fold after it) ends that line with an error
            // and its turn_done; the session takes the next message (otherwise a panic kills the agent thread
            // silently and the app shows the turn as running forever)
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
}
