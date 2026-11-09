use super::*;

pub(super) enum Ev {
    History(Vec<crate::frontend::Earlier>),
    Banner(Banner), User(String), Waiting, Delta(String), Done(Status), ToolStart(String), ToolDone(String, bool),
    Approve(String, String, Sender<Approval>), Info(String), Error(String), Notes(String), Compacted(usize, usize),
    Pick(Vec<SessionInfo>, Sender<Option<usize>>), Input(Sender<Option<String>>), TurnDone, Exit(i32), Detail(Detail),
    Act(String, Option<String>, Info), Progress(Option<Progress>), Recalled(Vec<String>), Card(String),
    Watch(WatchUpdate), WatchView(Option<WatcherView>), Full(String, String), Check(CheckEvent),
    /// what Elisa is doing, in plain words (src/activity.rs): the status line's live text
    Activity(crate::activity::Activity),
}

struct ChannelFrontend(Sender<Ev>);

impl ChannelFrontend {
    fn send(&self, e: Ev) { let _ = self.0.send(e); }
    fn ask<T>(&self, mk: impl FnOnce(Sender<T>) -> Ev, dflt: T) -> T {
        let (tx, rx) = mpsc::channel();
        self.send(mk(tx));
        rx.recv().unwrap_or(dflt)
    }
}
impl Frontend for ChannelFrontend {
    fn banner(&mut self, b: &Banner) { self.send(Ev::Banner(b.clone())) }
    fn user(&mut self, t: &str) { self.send(Ev::User(t.into())) }
    fn waiting(&mut self) { self.send(Ev::Waiting) }
    fn text_delta(&mut self, s: &str) { self.send(Ev::Delta(s.into())) }
    fn reply_done(&mut self, s: &Status) { self.send(Ev::Done(*s)) }
    fn tool_start(&mut self, s: &str) { self.send(Ev::ToolStart(s.into())) }
    fn tool_detail(&mut self, n: &str, i: &serde_json::Value, cwd: &std::path::Path) {
        self.send(Ev::Act(n.into(), act_target(n, i, cwd), act_info(n, i, cwd)));
        if let Some(d) = detail(n, i, cwd) { self.send(Ev::Detail(d)) }
    }
    fn tool_done(&mut self, o: &str, e: bool) { self.send(Ev::ToolDone(o.into(), e)) }
    fn approve(&mut self, tool: &str, summary: &str, server: Option<&str>) -> Approval {
        let tool = match server { Some(s) => format!("{MCP}{s}\u{1}{tool}"), None => tool.to_string() };
        self.ask(|t| Ev::Approve(tool, summary.into(), t), Approval::No)
    }
    /// shown by the same permission box: the tool carries the OUTSIDE mark, the summary the outside line after `\n`
    fn approve_outside(&mut self, tool: &str, summary: &str, o: &Outside) -> Approval {
        self.ask(|t| Ev::Approve(format!("{OUTSIDE}{tool}"), format!("{summary}\n{}", o.line()), t), Approval::No)
    }
    fn approve_rule(&mut self, tool: &str, summary: &str, server: Option<&str>, rule: &str) -> Approval {
        let tool = match server { Some(s) => format!("{MCP}{s}\u{1}{tool}"), None => tool.to_string() };
        self.ask(|t| Ev::Approve(format!("{tool}{RULE}{rule}"), summary.into(), t), Approval::No)
    }
    fn ask_yes_no(&mut self, what: &str, question: &str) -> bool {
        self.ask(|t| Ev::Approve(format!("{QUESTION}{what}"), question.into(), t), Approval::No) == Approval::Yes
    }
    fn info(&mut self, s: &str) { self.send(Ev::Info(s.into())) }
    fn error(&mut self, s: &str) { self.send(Ev::Error(s.into())) }
    fn notes_saved(&mut self, n: &str) { self.send(Ev::Notes(n.into())) }
    fn compacted(&mut self, a: usize, b: usize) { self.send(Ev::Compacted(a, b)) }
    fn progress(&mut self, p: Option<&Progress>) { self.send(Ev::Progress(p.cloned())) }
    fn recalled(&mut self, hits: &[String]) { self.send(Ev::Recalled(hits.to_vec())) }
    fn history(&mut self, items: &[crate::frontend::Earlier]) { if !items.is_empty() { self.send(Ev::History(items.to_vec())) } }
    fn state_card(&mut self, c: &str) { self.send(Ev::Card(c.into())) }
    fn pick_session(&mut self, l: &[SessionInfo]) -> Option<usize> { self.ask(|t| Ev::Pick(l.to_vec(), t), None) }
    fn read_input(&mut self) -> Option<String> { self.ask(Ev::Input, None) }
    fn turn_done(&mut self) { self.send(Ev::TurnDone) }
    fn check(&mut self, c: &CheckEvent) { self.send(Ev::Check(c.clone())) }
    fn activity(&mut self, a: &crate::activity::Activity) { self.send(Ev::Activity(a.clone())) }
    fn watcher_update(&mut self, u: &WatchUpdate) { self.send(Ev::Watch(u.clone())) }
    fn watcher(&mut self, w: Option<&WatcherView>) { self.send(Ev::WatchView(w.cloned())) }
    fn full_answer(&mut self, t: &str, x: &str) { self.send(Ev::Full(t.into(), x.into())) }
    fn watcher_push(&mut self) -> Option<WatchPush> {
        let tx = self.0.clone();
        Some(Box::new(move |u| { let _ = tx.send(Ev::Watch(u)); }))
    }
}

// ---------- terminal setup / restore ----------
static KB_PUSHED: AtomicBool = AtomicBool::new(false);

fn setup() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut o = io::stdout();
    // mouse capture: the wheel scrolls the chat instead of the terminal's own scrollback (which shows the shell);
    // native text selection then needs fn-drag in Terminal.app / option-drag in iTerm2
    execute!(o, terminal::EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(o, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        KB_PUSHED.store(true, Ordering::SeqCst);
    }
    Ok(())
}

fn restore() {
    let mut o = io::stdout();
    if KB_PUSHED.swap(false, Ordering::SeqCst) { let _ = execute!(o, PopKeyboardEnhancementFlags); }
    let _ = execute!(o, DisableMouseCapture, DisableBracketedPaste, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();
}

pub fn run(args: Args) -> i32 {
    let yolo = args.yes;
    // look: theme (--theme > config / ELISA_THEME), colours the terminal can show, the companion
    let cfg = crate::config::Config::load(&args.cwd.clone().or_else(|| std::env::current_dir().ok()).unwrap_or_default()).unwrap_or_default();
    crate::brand::set_theme(args.theme.as_deref().unwrap_or(&cfg.theme) == "light");
    crate::brand::set_truecolor(crate::brand::detect_truecolor());
    let character = cfg.character && !args.no_character;
    let args_companion = args.companion || cfg.companion;
    if let Err(e) = setup() { restore(); eprintln!("elisa: terminal setup failed: {e}"); return 1; }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| { restore(); prev(i); }));
    let mut term = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(t) => t,
        Err(e) => { restore(); eprintln!("elisa: {e}"); return 1; }
    };
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut fe = ChannelFrontend(tx.clone());
        let c = crate::repl::run_session(args, &mut fe);
        let _ = tx.send(Ev::Exit(c));
    });
    let mut app = App::new(yolo);
    if character { app = app.with_shade(cfg.sleep_after_s * 1000); }
    if args_companion { app.link = Some(crate::shade::link::Link::new(crate::shade::link::socket_path())); }
    let mut dump = ScreenDump::from_env();
    let code = loop {
        loop {
            match rx.try_recv() {
                Ok(e) => app.on_ev(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => { app.code.get_or_insert(1); break; }
            }
        }
        if let Some(c) = app.code { break c; }
        // the agent did not notice the cancel (stalled server): give up waiting
        if app.quit_at.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) { break 0; }
        match term.draw(|f| { app.draw(f); crate::brand::finish(f.buffer_mut(), &app.keep()); }) {
            Ok(fr) => { if let Some(d) = dump.as_mut() { d.write(fr.buffer); } }
            Err(_) => break 1,
        }
        let mut wait = Duration::from_millis(33);
        while event::poll(wait).unwrap_or(false) {
            wait = Duration::ZERO;
            match event::read() {
                Ok(Event::Key(ke)) => app.on_key(ke),
                Ok(Event::Paste(s)) => app.paste(&s),
                Ok(Event::Mouse(me)) => app.mouse(me),
                _ => {}
            }
        }
    };
    restore();
    let _ = std::panic::take_hook();
    if app.code.is_some() { let _ = worker.join(); }
    code
}
