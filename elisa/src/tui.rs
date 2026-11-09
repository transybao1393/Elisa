//! Full-screen terminal UI (ratatui + crossterm). The UI owns the main thread; `repl::run_session` runs on a
//! worker thread against a `ChannelFrontend` that forwards every call over mpsc (blocking calls wait on a reply).
use crate::frontend::*;
use crate::ui::k;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};
use ratatui::{prelude::*, widgets::*};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::{Duration, Instant};
use crate::shade::{Art, Companion, Event as ShEv, Look, St, View as ShView};

// ---------- palette: the brand's dark theme (02-color); `brand::finish` maps it to light / 256 colours per frame ----------
use crate::brand::{c as bc, DARK as P};
/// body text
const FG: Color = bc(P.text);
/// titles, headlines, the user's turns
const TITLE: Color = bc(P.fg);
/// secondary text: the dimmest text Elisa draws
const DIM: Color = bc(P.muted);
const ACCENT: Color = bc(P.accent);
const CODE: Color = bc(P.code);
const GREEN: Color = bc(P.ok);
const RED: Color = bc(P.error);
const YELLOW: Color = bc(P.warn);
const SEL_BG: Color = bc(P.sel);
/// marks an approval for a call outside the project folder (the tool name follows; the summary's 2nd line says where)
const OUTSIDE: &str = "\u{1}outside:";
/// an MCP tool's approval: `{MCP}server\u{1}tool` (the box offers "always allow MCP server …")
const MCP: &str = "\u{1}mcp:";
/// a yes / no question (plugin setup, project trust), not a tool call: the summary is the question
const QUESTION: &str = "\u{1}question:";
/// after the tool: the pattern offered for "always allow this pattern" (`Frontend::approve_rule`)
const RULE: char = '\u{2}';

/// The choices of an approval box, in order (the last is always No): Yes / Always / [always allow PATTERN] / No, or
/// Yes / No for a question.
fn approve_choices(tool: &str) -> Vec<(String, Approval)> {
    if tool.starts_with(QUESTION) { return vec![("Yes".into(), Approval::Yes), ("No".into(), Approval::No)]; }
    let (tool, rule) = match tool.split_once(RULE) { Some((t, r)) => (t, Some(r)), None => (tool, None) };
    let (tool, is_out) = match tool.strip_prefix(OUTSIDE) { Some(t) => (t, true), None => (tool, false) };
    let (tool, server) = match tool.strip_prefix(MCP).and_then(|r| r.split_once('\u{1}')) { Some((s, t)) => (t, Some(s)), None => (tool, None) };
    let always = if is_out { "Yes, allow these paths for this session".to_string() }
        else if let Some(srv) = server { format!("Yes, always allow MCP server {srv} in this project") }
        else { format!("Yes, always allow {} in this project", cap(tool)) };
    let mut v = vec![((if is_out { "Yes, once" } else { "Yes" }).to_string(), Approval::Yes), (always, Approval::Always)];
    if let Some(r) = rule { v.push((format!("Yes, and always allow {r} (saved to ~/.config/elisa/config.toml)"), Approval::Rule)); }
    v.push(("No".into(), Approval::No));
    v
}
const CODE_BG: Color = bc(P.bg2);
const BG: Color = bc(P.bg);
const BG2: Color = bc(P.bg2);
const H3: Color = bc(P.fg);
const GUTTER: Color = bc(P.muted);
const CTX: Color = bc(P.text);
const SPLIT: f32 = 0.70;
const SEP: Color = bc(P.sep);
const DONE_KEEP: Duration = Duration::from_secs(6);
// memory bounds: Elisa stays tiny (the model server owns the RAM)
const MAX_ENTRIES: usize = 1500;
const MAX_BYTES: usize = 2 << 20;
const MAX_VIEWS: usize = 50;
const MAX_HIST: usize = 200;
const RSS_WARN: u64 = 50 << 20;
const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// spinner frame time (the kit: 90 ms)
const SPIN_MS: u128 = 90;

fn st(c: Color) -> Style { Style::new().fg(c) }

// ---------- channel frontend ----------
/// turns of a resumed conversation the terminal shows again (the app shows more)
const TUI_EARLIER: usize = 3;

enum Ev {
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

// ---------- app state ----------
enum Entry { Recalled(Vec<String>), Foot(Status), Banner(Banner), User(String), Text(String), Tool(Box<Act>), Info(String), Err(String), Notes(String), Card(String, bool), Watch(String), WatchView(Box<WatcherView>), Full(String, String) }
impl Entry {
    /// rough heap bytes, for the transcript cap
    fn bytes(&self) -> usize {
        64 + match self {
            Entry::Recalled(h) => h.iter().map(String::len).sum(),
            Entry::Foot(_) => 0,
            Entry::Full(a, b) => a.len() + b.len(),
            Entry::Banner(b) => 256 + b.notes.as_ref().map_or(0, String::len),
            Entry::User(t) | Entry::Text(t) | Entry::Info(t) | Entry::Err(t) | Entry::Notes(t) | Entry::Card(t, _) | Entry::Watch(t) => t.len(),
            Entry::WatchView(v) => 512 + v.plan.per_layer_cold.len() * 8,
            Entry::Tool(a) => 128 + a.target.len() + a.end.as_ref().map_or(0, |e| e.note.len() + e.prev.iter().map(String::len).sum::<usize>()),
        }
    }
}
/// a transcript entry plus its render cache: height at width `w` (0 = stale) and the lines, kept only near the view
struct Item { e: Entry, w: usize, h: usize, lines: Option<Vec<Line<'static>>> }
impl Item {
    fn new(e: Entry) -> Self { Item { e, w: 0, h: 0, lines: None } }
    /// animated (running tool): re-rendered every frame
    fn live(&self) -> bool { matches!(&self.e, Entry::Tool(a) if a.end.is_none()) }
}
/// one background job under the input, keyed by the first word of its label
struct Job { key: String, p: Progress, at: Instant }
enum Pending { Input(Sender<Option<String>>), Approve(String, String, usize, Sender<Approval>), Pick(Vec<SessionInfo>, usize, Sender<Option<usize>>) }
#[derive(PartialEq)]
enum Phase { Idle, Thinking, Streaming, Tool }

struct App {
    entries: Vec<Item>, dropped: bool, new_text: bool,
    input: Vec<char>, cur: usize, hist: Vec<String>, hist_i: Option<usize>, draft: Vec<char>,
    menu_sel: usize, menu_off: bool, queue: VecDeque<String>,
    pending: Option<Pending>, phase: Phase, since: Instant,
    status: Option<Status>, model: String, max_ctx: usize, yolo: bool,
    first_delta: Option<Instant>, dchars: usize, sum_in: usize, sum_out: usize,
    scroll: usize, last_total: usize, view_h: usize,
    quit_at: Option<Instant>, flash: Option<(String, Instant)>, code: Option<i32>,
    // edit panel
    views: Vec<View>, sel: Option<usize>, last_read: Option<ReadV>, last_cmd: Option<(String, Option<bool>)>, awaiting: Await,
    pscroll: usize, panel_h: usize, split: f32, panel_pref: Option<bool>, term_w: u16,
    main_r: Rect,
    // memory job under the input: the last update and when it arrived; `memory` = any memory event seen
    jobs: Vec<Job>, memory: bool,
    // self-check: own resident size, refreshed at most once per second
    rss: u64, rss_at: Option<Instant>,
    // state cards: the stream filter hides <state>…</state> (`hold` = a possible partial tag at the end of the stream)
    cardf: crate::context::CardFilter, cards: usize,
    // the gateway's domain watcher (None = no view yet, or an older gateway): one line under the status bar
    watch: Option<WatcherView>,
    /// the latest audit result, shown on its own line under the input box (not in the transcript)
    audit: Option<String>,
    /// the companion (None = off); its clock; where its art was drawn this frame (kept out of the theme remap);
    /// the last rendered frame (re-used while the frame index is unchanged); this frame's view
    shade: Option<Companion>, clock: Instant, dock: Option<Rect>, art: Option<(ArtKey, Art)>, view: Option<ShView>,
    /// the care tray's buttons (kit v2 layout.json `careTray`): where they were drawn, for clicks
    tray: Vec<(Rect, St)>,
    /// where the status-bar face was drawn (None: one-line text status)
    face: Option<Rect>,
    /// the companion window link (--companion / config `companion`; None = off): Shade mirrored to app/ElisaCompanion
    link: Option<crate::shade::link::Link>,
    /// the status line's live text (src/activity.rs): the shown step and when it (or its words) came; a new tool step
    /// waits in `act_next` until it has run `activity::QUICK_MS` (a quicker one never takes the line)
    act: Option<(crate::activity::Activity, Instant)>,
    act_next: Option<(crate::activity::Activity, Instant)>,
}

type ArtKey = (St, u64, usize, usize, i32, bool);

impl App {
    fn new(yolo: bool) -> Self {
        App {
            entries: vec![], dropped: false, new_text: true, input: vec![], cur: 0, hist: vec![], hist_i: None, draft: vec![],
            menu_sel: 0, menu_off: false, queue: VecDeque::new(), pending: None, phase: Phase::Thinking, since: Instant::now(),
            status: None, first_delta: None, dchars: 0, sum_in: 0, sum_out: 0, model: String::new(), max_ctx: 0, yolo, scroll: 0, last_total: 0, view_h: 10,
            quit_at: None, flash: None, code: None,
            views: vec![], sel: None, last_read: None, last_cmd: None, awaiting: Await::No, pscroll: 0, panel_h: 10,
            split: SPLIT, panel_pref: None, term_w: 0, main_r: Rect::default(),
            jobs: vec![], memory: false, rss: 0, rss_at: None,
            cardf: Default::default(), cards: 0, watch: None, audit: None,
            shade: None, clock: Instant::now(), dock: None, art: None, view: None, tray: vec![], face: None, link: None,
            act: None, act_next: None,
        }
    }
    fn now(&self) -> u64 { self.clock.elapsed().as_millis() as u64 }
    /// turn the companion on; it sleeps after `sleep_after_s` idle seconds (0 = never)
    fn with_shade(mut self, sleep_after_ms: u64) -> Self {
        self.shade = Some(Companion::new(self.now(), (sleep_after_ms > 0).then_some(sleep_after_ms)));
        self
    }
    fn sh(&mut self, ev: ShEv) { let now = self.now(); if let Some(c) = &mut self.shade { c.on(now, ev); } }
    /// a care action (kit v2: poke, water, food, toy); UI-only, the agent never sees it
    fn care(&mut self, ev: ShEv) {
        if self.shade.is_none() { return self.flash("the character is off (--no-character / character = false)"); }
        self.sh(ev);
    }
    fn flash(&mut self, s: &str) { self.flash = Some((s.into(), Instant::now())); }
    /// the latest tool entry (its render cache is marked stale: the caller is about to change it)
    fn last_act(&mut self) -> Option<&mut Act> {
        self.entries.iter_mut().rev().find_map(|it| match &mut it.e { Entry::Tool(a) => { it.w = 0; Some(&mut **a) } _ => None })
    }
    /// streamed text minus any state-card text; a partial tag or card line at the end is held until it resolves
    fn unstate(&mut self, s: &str) -> String { self.cardf.push(s) }
    /// expand / collapse a state card
    fn toggle_card(&mut self, i: usize) {
        if let Some(Item { e: Entry::Card(_, x), w, .. }) = self.entries.get_mut(i) { *x = !*x; *w = 0; }
    }
    fn push(&mut self, e: Entry) { self.entries.push(Item::new(e)); self.trim(); }
    /// drop the oldest entries past ~1,500 entries or ~2 MB of text (down to 90%, so this is not O(n) per event)
    fn trim(&mut self) {
        let mut bytes: usize = self.entries.iter().map(|i| i.e.bytes()).sum();
        if self.entries.len() <= MAX_ENTRIES && bytes <= MAX_BYTES { return; }
        let mut n = 0;
        while self.entries.len() - n > 1 && (self.entries.len() - n > MAX_ENTRIES * 9 / 10 || bytes > MAX_BYTES * 9 / 10) {
            bytes -= self.entries[n].e.bytes();
            n += 1;
        }
        self.entries.drain(..n);
        self.dropped = true;
    }

    fn on_ev(&mut self, e: Ev) {
        let quitting = self.quit_at.is_some();
        match e {
            Ev::Banner(b) => {
                self.model = b.model.clone();
                self.max_ctx = b.max_ctx;
                self.push(Entry::Banner(b));
                self.sh(ShEv::Launch);
            }
            Ev::User(t) => { self.push(Entry::User(t)); self.scroll = 0; }
            Ev::Waiting => {
                self.sh(ShEv::Waiting);
                self.phase = Phase::Thinking; self.since = Instant::now(); self.new_text = true;
                self.first_delta = None; self.dchars = 0;
                self.cardf = Default::default();
            }
            Ev::Delta(s) => {
                self.phase = Phase::Streaming;
                self.first_delta.get_or_insert_with(Instant::now);
                let s = self.unstate(&s);
                if s.is_empty() { return; }
                self.sh(ShEv::Delta);
                self.dchars += s.chars().count();
                match self.entries.last_mut() {
                    Some(Item { e: Entry::Text(t), w, .. }) if !self.new_text => { t.push_str(&s); *w = 0; }
                    _ => self.entries.push(Item::new(Entry::Text(s))),
                }
                self.trim();
                self.new_text = false;
            }
            Ev::Done(s) => {
                // a held `<sta…` that never became a tag is ordinary text
                let h = self.cardf.finish();
                if !h.is_empty() {
                    match self.entries.last_mut() {
                        Some(Item { e: Entry::Text(t), w, .. }) if !self.new_text => { t.push_str(&h); *w = 0; }
                        _ => self.entries.push(Item::new(Entry::Text(h))),
                    }
                }
                self.max_ctx = s.max_ctx;
                self.status = Some(s);
                self.new_text = true;
                self.first_delta = None;
                self.sum_in += s.ctx_used.saturating_sub(s.output_tokens);
                self.sum_out += s.output_tokens;
                // a card that arrived before its footer stays under it
                let at = self.entries.len() - self.entries.iter().rev().take_while(|i| matches!(i.e, Entry::Card(..))).count();
                self.entries.insert(at, Item::new(Entry::Foot(s)));
                self.trim();
            }
            Ev::ToolStart(s) => {
                let (n, t) = s.split_once(' ').unwrap_or((&s, ""));
                self.sh(ShEv::Tool(n, t));
                let a = Act { name: n.into(), verb: cap(n), target: one(t), info: Info::None, t0: Instant::now(), wait: false, end: None };
                self.phase = Phase::Tool; self.push(Entry::Tool(Box::new(a))); self.new_text = true; self.awaiting = Await::No;
            }
            Ev::Act(n, t, i) => if let Some(a) = self.last_act() {
                if let Some(t) = t { a.target = one(&t); }
                a.verb = cap(&n);
                a.name = n;
                a.info = i;
            },
            Ev::Detail(d) => match d {
                Detail::View(v) => {
                    self.views.push(v);
                    if self.views.len() > MAX_VIEWS { self.views.remove(0); }
                    self.sel = None;
                    self.pscroll = 0;
                    self.awaiting = Await::View(self.views.len() - 1);
                }
                Detail::Read(r) => { if self.views.is_empty() { self.pscroll = 0; } self.last_read = Some(r); }
                Detail::Cmd(c) => { self.last_cmd = Some((c, None)); self.awaiting = Await::Cmd; }
            },
            Ev::ToolDone(o, e) => {
                match std::mem::replace(&mut self.awaiting, Await::No) {
                    Await::View(i) => if let Some(v) = self.views.get_mut(i) {
                        v.status = Some(if e { Err(o.lines().next().unwrap_or("failed").to_string()) } else { Ok(()) });
                    },
                    Await::Cmd => if let Some(c) = &mut self.last_cmd { c.1 = Some(!e); },
                    Await::No => {}
                }
                let mut failed = e && !o.starts_with("denied") && !o.starts_with("cancelled");
                if let Some(a) = self.last_act() {
                    a.wait = false;
                    let end = finish(a, &o, e);
                    failed |= end.red && end.note != "denied";
                    a.end = Some(end);
                }
                let now = self.now();
                if let Some(c) = &mut self.shade { c.tool_done(now, failed, &o); }
                self.phase = Phase::Thinking;
            }
            Ev::Check(c) => {
                let line = c.line();
                // the running line becomes its result line (one row per check)
                let running = CheckEvent { status: CheckStatus::Running, summary: String::new(), ..c.clone() }.line();
                if c.status != CheckStatus::Running && matches!(self.entries.last(), Some(Item { e: Entry::Info(t), .. }) if *t == running) {
                    self.entries.pop();
                }
                match c.status {
                    CheckStatus::Running => {
                        self.sh(ShEv::Tool("check", &format!("check {}", c.cmd)));
                        self.phase = Phase::Tool;
                        self.new_text = true;
                    }
                    CheckStatus::Pass => self.sh(ShEv::ToolDone(false)),
                    CheckStatus::Fail | CheckStatus::Timeout => self.sh(ShEv::ToolDone(true)),
                    CheckStatus::Stopped => self.sh(ShEv::Failed),
                }
                if c.status != CheckStatus::Running { self.phase = Phase::Thinking; }
                self.push(if c.status == CheckStatus::Stopped { Entry::Err(line) } else { Entry::Info(line) });
            }
            Ev::Info(s) => {
                if s.contains("approve-all ON") { self.yolo = true; }
                if s.contains("approve-all OFF") { self.yolo = false; }
                self.push(Entry::Info(s));
            }
            Ev::Error(s) => {
                self.sh(if s.starts_with("unknown command") { ShEv::UnknownCommand } else { ShEv::Failed });
                self.push(Entry::Err(s));
            }
            Ev::Notes(n) => self.push(Entry::Notes(n)),
            Ev::Compacted(a, b) => {
                // the context shrank: the ctx bar shows it now (not at the next reply), and the screen is cleaned up to
                // the current / last exchange; the folded turns live in the notes / state card
                if let Some(s) = &mut self.status { s.ctx_used = b; }
                let keep = self.entries.iter().rposition(|i| matches!(i.e, Entry::User(_))).unwrap_or(self.entries.len());
                self.entries.drain(..keep);
                self.entries.insert(0, Item::new(Entry::Info(format!(
                    "[compacted: {} → {} tokens · earlier turns summarised (/notes)]", k(a), k(b)))));
                self.scroll = 0;
            }
            Ev::Approve(t, s, tx) => {
                if quitting { let _ = tx.send(Approval::No); return; }
                if let Some(a) = self.last_act() { a.wait = true; a.t0 = Instant::now(); }
                self.sh(ShEv::Permission);
                self.pending = Some(Pending::Approve(t, s, 0, tx));
                self.scroll = 0;
            }
            Ev::Pick(l, tx) => {
                if quitting || l.is_empty() { let _ = tx.send(None); } else { self.pending = Some(Pending::Pick(l, 0, tx)); }
            }
            Ev::Input(tx) => {
                if quitting { let _ = tx.send(None); return; }
                self.pending = Some(Pending::Input(tx));
                self.phase = Phase::Idle;
                if let Some(c) = &mut self.shade { c.idle(); }
                if let Some(q) = self.queue.pop_front() { self.send(q); }
            }
            Ev::TurnDone => { self.phase = Phase::Idle; self.act = None; self.act_next = None; self.sh(ShEv::Finished(false)); }
            Ev::Activity(a) => self.on_activity(a),
            Ev::Progress(p) => {
                self.memory = true;
                match p {
                    Some(p) => {
                        // same first word (verb) = same job: update in place, else a new job
                        let key = first_word(&p.label).or_else(|| p.done.as_deref().and_then(first_word)).unwrap_or_default();
                        match self.jobs.iter_mut().find(|j| j.key == key) {
                            Some(j) => { j.p = p; j.at = Instant::now(); }
                            None => self.jobs.push(Job { key, p, at: Instant::now() }),
                        }
                    }
                    // clear the running jobs now; finished messages keep their 6 s
                    None => self.jobs.retain(|j| j.p.done.is_some()),
                }
                if self.summarizing() { self.sh(ShEv::Summarizing); } else if let Some(c) = &mut self.shade { c.summary_done(); }
            }
            Ev::History(items) => {
                // a resumed conversation: its last few turns again, between two divider lines (/history-like; the
                // app shows up to `context::EARLIER_TURNS`)
                let users: Vec<usize> = items.iter().enumerate().filter(|(_, e)| e.role == "user").map(|(i, _)| i).collect();
                let from = users.len().checked_sub(TUI_EARLIER).map_or(0, |k| users[k]);
                self.push(Entry::Info("— earlier in this conversation —".into()));
                for e in items.into_iter().skip(from) {
                    self.push(if e.role == "user" { Entry::User(e.text) } else { Entry::Text(e.text) });
                }
                self.push(Entry::Info("— continued —".into()));
            }
            Ev::Recalled(h) => {
                self.memory = true;
                if h.is_empty() { return; }
                let now = self.now();
                if let Some(c) = &mut self.shade { c.recalled(now, h.len()); }
                // directly under the latest user turn (after any recall block already there)
                let at = self.entries.iter().rposition(|i| matches!(i.e, Entry::User(_)))
                    .map_or(self.entries.len(), |i| i + 1 + self.entries[i + 1..].iter().take_while(|i| matches!(i.e, Entry::Recalled(_))).count());
                self.entries.insert(at, Item::new(Entry::Recalled(h)));
                self.trim();
            }
            Ev::Card(c) => {
                self.cards += 1;
                // under this reply's footer (after the latest user turn), else at the end
                let u = self.entries.iter().rposition(|i| matches!(i.e, Entry::User(_))).map_or(0, |i| i + 1);
                let at = self.entries[u..].iter().rposition(|i| matches!(i.e, Entry::Foot(..))).map_or(self.entries.len(), |i| u + i + 1);
                self.entries.insert(at, Item::new(Entry::Card(c, false)));
                self.trim();
            }
            Ev::Watch(u) => {
                if let (true, Some(v)) = (u.changed && u.was.is_some(), &u.view) { self.push(Entry::Watch(crate::watcher::change_line(v, u.was.as_deref()))); }
                for a in &u.audits { if let Some(l) = crate::watcher::audit_line(a) { self.audit = Some(l); } }
                self.watch = u.view;
            }
            Ev::Full(title, text) => self.push(Entry::Full(title, text)),
            Ev::WatchView(w) => match w {
                Some(v) => { self.watch = Some(v.clone()); self.push(Entry::WatchView(Box::new(v))); }
                None => self.push(Entry::Info("watcher: not available on this gateway".into())),
            },
            Ev::Exit(c) => self.code = Some(c),
        }
    }

    /// A mouse event: a left click on Shade pokes her (kit v2); the wheel scrolls; the rest is ignored.
    fn mouse(&mut self, me: MouseEvent) {
        let on_dock = self.dock.is_some_and(|d| me.column >= d.x && me.column < d.right() && me.row >= d.y && me.row < d.bottom());
        let left = me.kind == MouseEventKind::Down(MouseButton::Left);
        if left && on_dock { return self.care(ShEv::Poke); }
        let hit = |r: &Rect| me.column >= r.x && me.column < r.right() && me.row >= r.y && me.row < r.bottom();
        if let Some(&(_, st)) = self.tray.iter().find(|(r, _)| left && hit(r)) {
            return self.care(match st { St::Drinking => ShEv::Water, St::Eating => ShEv::Food, _ => ShEv::Toy });
        }
        self.wheel(me.kind)
    }

    /// Mouse wheel: scroll the chat 3 lines (other mouse events are ignored).
    fn wheel(&mut self, k: MouseEventKind) {
        match k {
            MouseEventKind::ScrollUp => self.scroll += 3,
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            _ => {}
        }
    }

    fn quit(&mut self) {
        if self.quit_at.is_none() { self.quit_at = Some(Instant::now()); }
        crate::repl::CANCEL.store(true, Ordering::SeqCst);
        match self.pending.take() {
            Some(Pending::Input(tx)) => { let _ = tx.send(None); }
            Some(Pending::Approve(.., tx)) => { let _ = tx.send(Approval::No); }
            Some(Pending::Pick(_, _, tx)) => { let _ = tx.send(None); }
            None => {}
        }
        self.flash("quitting…");
    }

    // ----- slash menu -----
    fn menu(&self) -> Vec<(&'static str, &'static str)> {
        if self.menu_off || !self.editable() || self.input.first() != Some(&'/')
            || self.input.iter().any(|c| c.is_whitespace()) { return vec![]; }
        let p: String = self.input.iter().collect();
        COMMANDS.iter().copied().filter(|(n, _)| n.starts_with(&p)).collect()
    }

    /// input box takes keys: at the prompt, or while the agent is busy (queueing); paused while a summary runs
    fn editable(&self) -> bool { matches!(self.pending, Some(Pending::Input(_)) | None) && !self.summarizing() }
    /// a summary / fold of earlier turns is running (its progress job): the input box is paused until it ends
    fn summarizing(&self) -> bool {
        self.jobs.iter().any(|j| j.p.done.is_none() && matches!(j.key.as_str(), "summarizing" | "folding"))
    }

    /// Enter: send at the prompt, queue while busy (`/exit` while busy quits now)
    fn submit(&mut self, text: String) {
        let t = text.trim().to_string();
        if t.is_empty() || !self.editable() { return; }
        if self.hist.last() != Some(&t) { self.hist.push(t.clone()); }
        if self.hist.len() > MAX_HIST { self.hist.remove(0); }
        self.hist_i = None;
        self.input.clear();
        self.cur = 0;
        self.menu_off = false;
        self.menu_sel = 0;
        self.scroll = 0;
        if let Some(ev) = care_command(&t) { return self.care(ev); }
        if self.pending.is_some() { self.send(t); }
        else if t == "/exit" || t == "/quit" { self.quit(); }
        else { self.queue.push_back(t); }
    }

    /// hand a line to the agent (it echoes it back as a user turn)
    fn send(&mut self, t: String) {
        let Some(Pending::Input(tx)) = self.pending.take() else { return self.queue.push_front(t) };
        self.phase = Phase::Thinking;
        self.since = Instant::now();
        crate::repl::CANCEL.store(false, Ordering::SeqCst);
        let _ = tx.send(Some(t));
    }

    fn set_input(&mut self, v: Vec<char>) { self.cur = v.len(); self.input = v; }
    fn ins(&mut self, c: char) { self.input.insert(self.cur, c); self.cur += 1; self.menu_off = false; }
    fn line_start(&self) -> usize { self.input[..self.cur].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1) }
    fn line_end(&self) -> usize { self.input[self.cur..].iter().position(|&c| c == '\n').map_or(self.input.len(), |i| self.cur + i) }

    /// move the cursor one logical line up/down; false when already on the first/last line
    fn move_line(&mut self, up: bool) -> bool {
        let (s, col) = (self.line_start(), self.cur - self.line_start());
        if up {
            if s == 0 { return false; }
            let ps = self.input[..s - 1].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
            self.cur = (ps + col).min(s - 1);
        } else {
            let e = self.line_end();
            if e == self.input.len() { return false; }
            let ne = self.input[e + 1..].iter().position(|&c| c == '\n').map_or(self.input.len(), |i| e + 1 + i);
            self.cur = (e + 1 + col).min(ne);
        }
        true
    }

    fn history(&mut self, up: bool) {
        if self.hist.is_empty() { return; }
        let i = match (self.hist_i, up) {
            (None, true) => { self.draft = self.input.clone(); self.hist.len() - 1 }
            (None, false) => return,
            (Some(i), true) => i.saturating_sub(1),
            (Some(i), false) if i + 1 < self.hist.len() => i + 1,
            (Some(_), false) => { self.hist_i = None; let d = std::mem::take(&mut self.draft); self.set_input(d); return; }
        };
        self.hist_i = Some(i);
        self.set_input(self.hist[i].chars().collect());
    }

    fn on_key(&mut self, ke: KeyEvent) {
        if ke.kind == KeyEventKind::Release { return; }
        self.key(ke);
        let (now, typed) = (self.now(), matches!(self.pending, Some(Pending::Input(_))) && !self.input.is_empty());
        if let Some(c) = &mut self.shade { c.typing(now, typed); }
    }

    fn key(&mut self, ke: KeyEvent) {
        let (code, m) = (ke.code, ke.modifiers);
        let ctrl = m.contains(KeyModifiers::CONTROL);
        if ctrl && code == KeyCode::Char('c') { return self.quit(); }
        if ctrl && code == KeyCode::Char('d') { return self.quit(); }
        let alt = m.contains(KeyModifiers::ALT);
        let free = !self.editable() || self.input.is_empty();
        match code {
            // care keys (kit v2): ctrl-g pokes Elisa, F2 water, F3 food, F4 a toy (same as a click / /water /feed /play)
            KeyCode::Char('g') if ctrl => return self.care(ShEv::Poke),
            KeyCode::F(n @ 2..=4) => return self.care([ShEv::Water, ShEv::Food, ShEv::Toy][n as usize - 2]),
            KeyCode::Char('o') if ctrl => { self.panel_pref = Some(!self.panel_on()); return; }
            KeyCode::Char('s') if ctrl => {
                if let Some(i) = self.entries.iter().rposition(|i| matches!(i.e, Entry::Card(..))) { self.toggle_card(i); }
                return;
            }
            KeyCode::Left | KeyCode::Right if ctrl => {
                let d = if code == KeyCode::Left { -2.0 } else { 2.0 };
                self.set_split(self.split + d / self.main_r.width.max(1) as f32);
                return;
            }
            KeyCode::Left | KeyCode::Right if alt => return self.step(code == KeyCode::Right),
            KeyCode::Char(c @ ('[' | ']')) if free && !ctrl && !alt => return self.step(c == ']'),
            KeyCode::Up if alt => { self.pscroll = self.pscroll.saturating_sub(3); return; }
            KeyCode::Down if alt => { self.pscroll += 3; return; }
            _ => {}
        }
        match code {
            KeyCode::PageUp => { self.scroll += self.view_h.saturating_sub(2).max(1); return; }
            KeyCode::PageDown => { self.scroll = self.scroll.saturating_sub(self.view_h.saturating_sub(2).max(1)); return; }
            // keyboard stand-in for the wheel (no mouse capture: the terminal keeps its own text selection)
            KeyCode::Up if m.contains(KeyModifiers::SHIFT) => { self.scroll += 3; return; }
            KeyCode::Down if m.contains(KeyModifiers::SHIFT) => { self.scroll = self.scroll.saturating_sub(3); return; }
            _ => {}
        }
        match self.pending.take() {
            None => {
                if code == KeyCode::Esc && self.menu().is_empty() { self.cancel(); } else { self.edit(code, m); }
            }
            Some(Pending::Approve(t, s, mut i, tx)) => {
                let choices = approve_choices(&t);
                let n = choices.len();
                let pick = match code {
                    KeyCode::Up | KeyCode::Left | KeyCode::Char('k' | 'h') => { i = (i + n - 1) % n; None }
                    KeyCode::Down | KeyCode::Right | KeyCode::Char('j' | 'l') => { i = (i + 1) % n; None }
                    KeyCode::Enter => Some(i),
                    KeyCode::Char('y' | 'Y') => Some(0),
                    KeyCode::Char('a' | 'A') => choices.iter().position(|c| c.1 == Approval::Always),
                    KeyCode::Char('r' | 'R') => choices.iter().position(|c| c.1 == Approval::Rule),
                    KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(n - 1),
                    KeyCode::Char(c @ '1'..='9') => Some(c as usize - '1' as usize).filter(|&k| k < n),
                    _ => None,
                };
                match pick {
                    Some(p) => {
                        let a = choices[p].1;
                        if a == Approval::Rule { self.flash("pattern saved"); }
                        if let Some(t) = self.last_act() { t.wait = false; t.t0 = Instant::now(); }
                        if a == Approval::Always { self.flash(if t.starts_with(OUTSIDE) { "allowed for this session" } else { "always allowed" }); }
                        let now = self.now();
                        if let Some(c) = &mut self.shade { c.approved(now, a != Approval::No); }
                        let _ = tx.send(a);
                    }
                    None => self.pending = Some(Pending::Approve(t, s, i, tx)),
                }
            }
            Some(Pending::Pick(l, mut i, tx)) => match code {
                KeyCode::Up => { i = i.saturating_sub(1); self.pending = Some(Pending::Pick(l, i, tx)); }
                KeyCode::Down => { i = (i + 1).min(l.len() - 1); self.pending = Some(Pending::Pick(l, i, tx)); }
                KeyCode::Enter => { let _ = tx.send(Some(i)); }
                KeyCode::Esc => { let _ = tx.send(None); }
                _ => self.pending = Some(Pending::Pick(l, i, tx)),
            },
            Some(p @ Pending::Input(_)) => { self.pending = Some(p); self.edit(code, m); }
        }
    }

    fn panel_on(&self) -> bool { self.panel_pref.unwrap_or(self.term_w >= 100) }
    fn set_split(&mut self, v: f32) { self.split = v.clamp(0.40, 0.85); }
    /// step through this session's edits (newest = follow mode)
    fn step(&mut self, fwd: bool) {
        let n = self.views.len();
        if n == 0 { return; }
        let i = self.sel.unwrap_or(n - 1);
        let j = if fwd { (i + 1).min(n - 1) } else { i.saturating_sub(1) };
        self.sel = if j == n - 1 { None } else { Some(j) };
        self.pscroll = 0;
    }

    fn cancel(&mut self) {
        if self.pending.is_none() { self.sh(ShEv::Esc); }
        crate::repl::CANCEL.store(true, Ordering::SeqCst);
        self.flash("interrupting…");
    }

    fn edit(&mut self, code: KeyCode, m: KeyModifiers) {
        let ctrl = m.contains(KeyModifiers::CONTROL);
        // summary running: typing is paused, the wheel (↑↓) still scrolls; esc still interrupts (handled before)
        if self.summarizing() {
            match code {
                KeyCode::Up => self.scroll += 3,
                KeyCode::Down => self.scroll = self.scroll.saturating_sub(3),
                _ => {}
            }
            return;
        }
        let menu = self.menu();
        if !menu.is_empty() { self.menu_sel = self.menu_sel.min(menu.len() - 1); }
        match code {
            KeyCode::Enter if m.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => self.ins('\n'),
            KeyCode::Char('j') if ctrl => self.ins('\n'),
            KeyCode::Enter if !menu.is_empty() => self.submit(menu[self.menu_sel].0.to_string()),
            KeyCode::Enter => self.submit(self.input.iter().collect()),
            KeyCode::Tab if !menu.is_empty() => self.set_input(menu[self.menu_sel].0.chars().collect()),
            KeyCode::Esc if !menu.is_empty() => self.menu_off = true,
            KeyCode::Up if !menu.is_empty() => self.menu_sel = self.menu_sel.checked_sub(1).unwrap_or(menu.len() - 1),
            // the wheel: Terminal.app sends it as plain ↑/↓ (no mouse capture), so with an empty input box they scroll the chat;
            // history and queued-message recall move to ctrl-p / ctrl-n
            KeyCode::Up if self.input.is_empty() && self.hist_i.is_none() && !ctrl => self.scroll += 3,
            KeyCode::Down if self.input.is_empty() && self.hist_i.is_none() && !ctrl => self.scroll = self.scroll.saturating_sub(3),
            KeyCode::Char('p') if ctrl && self.input.is_empty() && !self.queue.is_empty() => {
                let q = self.queue.pop_back().unwrap_or_default();
                self.set_input(q.chars().collect());
            }
            KeyCode::Char('u') if ctrl && self.input.is_empty() => { self.queue.pop_back(); }
            KeyCode::Backspace if self.input.is_empty() && self.pending.is_none() => { self.queue.pop_back(); }
            KeyCode::Down if !menu.is_empty() => self.menu_sel = (self.menu_sel + 1) % menu.len(),
            KeyCode::Up => { if !self.move_line(true) { self.history(true); } }
            KeyCode::Down => { if !self.move_line(false) { self.history(false); } }
            KeyCode::Char('p') if ctrl => self.history(true),
            KeyCode::Char('n') if ctrl => self.history(false),
            KeyCode::Left => self.cur = self.cur.saturating_sub(1),
            KeyCode::Right => self.cur = (self.cur + 1).min(self.input.len()),
            KeyCode::Home => self.cur = self.line_start(),
            KeyCode::End => self.cur = self.line_end(),
            KeyCode::Char('a') if ctrl => self.cur = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cur = self.line_end(),
            KeyCode::Char('u') if ctrl => { let s = self.line_start(); self.input.drain(s..self.cur); self.cur = s; }
            KeyCode::Char('w') if ctrl => {
                let mut i = self.cur;
                while i > 0 && self.input[i - 1].is_whitespace() { i -= 1; }
                while i > 0 && !self.input[i - 1].is_whitespace() { i -= 1; }
                self.input.drain(i..self.cur);
                self.cur = i;
            }
            KeyCode::Backspace if self.cur > 0 => { self.cur -= 1; self.input.remove(self.cur); self.menu_off = false; }
            KeyCode::Delete if self.cur < self.input.len() => { self.input.remove(self.cur); }
            KeyCode::Char(c) if !ctrl => { self.ins(c); self.menu_sel = 0; }
            _ => {}
        }
    }

    fn paste(&mut self, s: &str) {
        if !self.editable() { return; }
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() { self.ins(c); }
        let (now, typed) = (self.now(), !self.input.is_empty());
        if let Some(c) = &mut self.shade { c.typing(now, typed); }
    }

    // ---------- drawing ----------
    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Fill(BG), area);
        let now = self.now();
        // the gateway's phase for the running model call (queue, prefill, reasoning, a tool call being written)
        if let Some(c) = &mut self.shade { c.gateway(now, crate::api::phase(), crate::api::call_target); }
        // with a companion window linked, the view is mirrored to it and its care clicks come back (shade::link)
        self.view = self.shade.as_mut().map(|c| match &mut self.link { Some(l) => l.pump(c, now, crate::brand::light()), None => c.view(now) });
        self.dock = None;
        self.tray.clear();
        self.face = None;
        let iw = area.width.saturating_sub(4).max(4) as usize;
        let (in_lines, cpos) = self.input_lines(iw);
        let ql = self.queue_lines(area.width as usize);
        let qh = (ql.len() as u16).min(area.height.saturating_sub(6));
        self.jobs.retain(|j| j.p.done.is_none() || j.at.elapsed() < DONE_KEEP);
        if self.rss_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) { self.rss = own_rss(); self.rss_at = Some(Instant::now()); }
        let ph = !self.jobs.is_empty() as u16;
        let wrows = self.watch.as_ref().map_or(vec![], |v| watch_rows(v, area.width as usize));
        let wh = wrows.len() as u16;
        let ah = self.audit.is_some() as u16;
        let stats = self.stats_line();
        let xh = stats.is_some() as u16;
        // the status area: Shade's ears-and-eyes face with a row of padding above and below on a tall enough terminal
        // (kit v2 layout.json statusBar "face"), else one text line under a blank row
        let face = self.face_size(area.height);
        let sb_h = face.map_or(1, |(_, h)| h as u16 + 2);
        let gap = (face.is_none() && area.height >= 12) as u16;
        // the permission box takes the input box's rows (no input field while it asks)
        let asking = matches!(self.pending, Some(Pending::Approve(..)));
        let in_h = if asking { 0 } else { (in_lines.len() as u16 + 2).min(area.height.saturating_sub(3 + sb_h + gap + xh + qh + ph + wh + ah).max(3)) };
        // under the input box: context bar, then speed / Σ (both right-aligned), watcher, audit, background jobs
        let [tr, _gap, qa, sb, inp, cl, xl, wl, al, pa] = Layout::vertical([Constraint::Min(1), Constraint::Length(gap),
            Constraint::Length(qh), Constraint::Length(sb_h), Constraint::Length(in_h), Constraint::Length(1), Constraint::Length(xh),
            Constraint::Length(wh), Constraint::Length(ah), Constraint::Length(ph)]).areas(area);
        f.render_widget(Paragraph::new(self.ctx_line()), cl);
        if let Some(l) = stats { f.render_widget(Paragraph::new(l), xl); }
        f.render_widget(Paragraph::new(wrows), wl);
        if let Some(t) = &self.audit { f.render_widget(Paragraph::new(audit_cells_line(t, al.width as usize)), al); }
        if let Some(l) = self.progress_line(pa.width as usize) { f.render_widget(Paragraph::new(l), pa); }
        f.render_widget(Paragraph::new(ql), qa);

        // chat | divider | edit panel
        self.term_w = area.width;
        self.main_r = tr;
        let cw = ((tr.width as f32 * self.split).round() as u16).min(tr.width.saturating_sub(12));
        let (tr, pr) = if self.panel_on() && cw >= 20 {
            let [c, d, p] = Layout::horizontal([Constraint::Length(cw), Constraint::Length(1), Constraint::Min(1)]).areas(tr);
            let bar = vec![Line::styled("│", st(SEP)); d.height as usize];
            f.render_widget(Paragraph::new(bar), d);
            (c, Some(p.inner(Margin { horizontal: 1, vertical: 0 })))
        } else { (tr, None) };
        if let Some(p) = pr { self.draw_panel(f, p); }

        // approval box at the bottom of the chat column
        let tr = match &self.pending {
            Some(Pending::Approve(t, s, i, _)) => {
                let w = tr.width.saturating_sub(4) as usize;
                let lines = self.approve_lines(t, s, *i, w);
                let h = (lines.len() as u16 + 2).min(tr.height.saturating_sub(1));
                let [c, b] = Layout::vertical([Constraint::Min(1), Constraint::Length(h)]).areas(tr);
                let b = b.inner(Margin { horizontal: 1, vertical: 0 });
                f.render_widget(Fill(BG2), b);
                f.render_widget(Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded)
                    .border_style(st(YELLOW))), b);
                c
            }
            _ => tr,
        };

        // the companion: bottom-right of the chat column, its care tray on the left; the transcript never runs under it
        let tr = match self.dock_size(tr) {
            Some((w, h)) => {
                let dw = w as u16 + 3 + TRAY_W;
                let [t, d] = Layout::horizontal([Constraint::Min(1), Constraint::Length(dw)]).areas(tr);
                let d = Rect { y: d.bottom() - (h as u16 + 3), height: h as u16 + 3, ..d };
                self.draw_dock(f, d, w, h);
                t
            }
            None => tr,
        };
        // transcript
        let tr = tr.inner(Margin { horizontal: 1, vertical: 0 });
        let lines = self.transcript(tr.width as usize, tr.height as usize);
        f.render_widget(Paragraph::new(lines), tr);

        // input box (none while the permission box asks)
        let blk = Block::new().borders(Borders::TOP | Borders::BOTTOM).border_style(st(ACCENT)).style(Style::new().bg(BG2));
        let body = blk.inner(inp);
        if !asking { f.render_widget(blk, inp); }
        match &self.pending {
            Some(Pending::Input(_)) | None if self.summarizing() => {
                f.render_widget(Paragraph::new(Span::styled("summarizing earlier turns… input paused (esc interrupts)", st(DIM))), body);
            }
            Some(Pending::Input(_)) | None => {
                f.render_widget(Paragraph::new(in_lines), body);
                let y = body.y + (cpos.1 as u16).min(body.height.saturating_sub(1));
                f.set_cursor_position((body.x + cpos.0 as u16, y));
            }
            Some(Pending::Approve(..)) => {}
            Some(Pending::Pick(..)) => {
                f.render_widget(Paragraph::new(Span::styled("pick a session · ↑↓ enter · esc cancels", st(DIM))), body);
            }
        }

        // status bar above the input box: the face on the left, the text centred beside it
        match face {
            Some((w, h)) => {
                let fr = Rect { x: sb.x + 1, y: sb.y + 1, width: w as u16, height: h as u16 };
                self.draw_face(f, fr);
                let tx = fr.right() + 2;
                let tl = Rect { x: tx, y: fr.y + h as u16 / 2, width: sb.right().saturating_sub(tx), height: 1 };
                f.render_widget(Paragraph::new(self.status_line(tl.width as usize)), tl);
            }
            None => f.render_widget(Paragraph::new(self.status_line(sb.width as usize)), sb),
        }

        // popups
        let menu = self.menu();
        if !menu.is_empty() {
            let inp = sb;
            let sel = self.menu_sel.min(menu.len() - 1);
            let h = (menu.len() as u16 + 2).min(inp.y);
            let w = area.width.min(64);
            let r = Rect { x: inp.x, y: inp.y - h, width: w, height: h };
            let items: Vec<ListItem> = menu.iter().map(|(n, d)| ListItem::new(Line::from(vec![
                Span::styled(format!("{n:<10}"), st(TITLE).bold()), Span::styled(*d, st(DIM)),
            ]))).collect();
            let mut ls = ListState::default().with_selected(Some(sel));
            f.render_widget(Fill(BG2), r);
            f.render_stateful_widget(List::new(items).block(popup_block("commands")).highlight_style(Style::new().bg(SEL_BG)), r, &mut ls);
        }
        if let Some(Pending::Pick(l, i, _)) = &self.pending {
            let w = area.width.saturating_sub(4).min(100);
            let h = (l.len() as u16 + 2).min(area.height.saturating_sub(4)).max(3);
            let r = Rect { x: (area.width - w) / 2, y: area.height.saturating_sub(h) / 2, width: w, height: h };
            let items: Vec<ListItem> = l.iter().map(|s| ListItem::new(Line::from(vec![
                Span::styled(fmt_ts(s.ts), st(CODE)), Span::styled(format!("  {:>4} msgs  ", s.n_msgs), st(DIM)),
                Span::styled(s.title.clone(), st(FG)),
            ]))).collect();
            let mut ls = ListState::default().with_selected(Some(*i));
            f.render_widget(Fill(BG2), r);
            f.render_stateful_widget(List::new(items).block(popup_block("resume a session"))
                .highlight_style(Style::new().bg(SEL_BG)).highlight_symbol("› "), r, &mut ls);
        }
    }

    /// the rects drawn for the theme already (`brand::finish` leaves them alone): Shade's dock art and the face
    fn keep(&self) -> Vec<Rect> { self.dock.into_iter().chain(self.face).collect() }

    /// the status-bar face size (cells) for a terminal height: 10 x 5 on tall terminals, 8 x 4 on medium ones (the
    /// smallest that reads: below it the eyes are grey mush), else None = the one-line text status. Off with the
    /// character (--no-character / character = false).
    fn face_size(&self, rows: u16) -> Option<(usize, usize)> {
        self.shade.as_ref()?;
        [(10usize, 5usize, FACE_TALL), (8, 4, FACE_MIN)].into_iter().find(|&(_, _, r)| rows >= r).map(|(w, h, _)| (w, h))
    }

    /// the status-bar face (kit v2 `faceSvg`): ears and eyes, animated with Shade's state, on the theme background
    fn draw_face(&mut self, f: &mut Frame, r: Rect) {
        let Some(v) = &self.view else { return };
        let bg = crate::brand::pal().bg;
        let art = crate::shade::render_face(v.st, v.ms, r.width as usize, r.height as usize, bg, crate::shade::FACE_VIEW);
        let rgb = |c: (u8, u8, u8)| Color::Rgb(c.0, c.1, c.2);
        let buf = f.buffer_mut();
        for (i, c) in art.cells.iter().enumerate() {
            let cell = &mut buf[(r.x + (i % art.w) as u16, r.y + (i / art.w) as u16)];
            cell.set_char(c.ch);
            cell.set_fg(c.fg.map_or(Color::Reset, rgb));
            cell.set_bg(rgb(c.bg.unwrap_or(bg)));
        }
        self.face = Some(r);
    }

    /// the companion's art size (cells) for a chat column: large / medium / small (mini build), or None when the
    /// character is off or the column is too small (the transcript and the input always win)
    fn dock_size(&self, r: Rect) -> Option<(usize, usize)> {
        self.shade.as_ref()?;
        // the companion window shows Shade instead (the status line stays)
        if self.link.as_ref().is_some_and(|l| l.connected()) { return None; }
        let fits = |w: u16, h: u16| r.width >= w && r.height >= h + 3 + 4;
        [(32usize, 19usize, 128u16), (24, 15, 84), (16, 9, 56)].into_iter().find(|&(_, h, cw)| fits(cw, h as u16)).map(|(w, h, _)| (w, h))
    }

    /// bubble (3 rows: VERB target, tail ▽) over Shade's art, right-aligned in `r`
    fn draw_dock(&mut self, f: &mut Frame, r: Rect, w: usize, h: usize) {
        let Some(v) = self.view.clone() else { return };
        let pal = crate::brand::pal();
        // a long tool's fidget: the listening pose for a moment (the bubble stays the tool's)
        let pose = if v.fidget { St::Listening } else { v.st };
        let key: ArtKey = (pose, crate::shade::frame_of(pose, v.ms), w, h, (v.pop * 100.0).round() as i32, crate::brand::light());
        if self.art.as_ref().is_none_or(|(k, _)| *k != key) {
            let look = Look { bg: pal.bg, accent: pal.accent, warn: pal.warn, error: pal.error, accent_a: pal.glow as f32 / 100.0 };
            self.art = Some((key, crate::shade::render(pose, v.ms, w, h, v.pop, &look)));
        }
        let Some((_, art)) = &self.art else { return };
        let ax = r.right().saturating_sub(w as u16 + 1);
        let ay = r.bottom() - h as u16;
        let rgb = |c: (u8, u8, u8)| Color::Rgb(c.0, c.1, c.2);
        let buf = f.buffer_mut();
        for (i, c) in art.cells.iter().enumerate() {
            let (x, y) = (ax + (i % w) as u16, ay + (i / w) as u16);
            let cell = &mut buf[(x, y)];
            cell.set_char(c.ch);
            cell.set_fg(c.fg.map_or(Color::Reset, rgb));
            cell.set_bg(rgb(c.bg.unwrap_or(pal.bg)));
        }
        self.dock = Some(Rect { x: ax, y: ay, width: w as u16, height: h as u16 });
        // the care tray (layout.json `careTray`): water, food, toy stacked left of Shade, bottom-aligned, a row apart,
        // one row of padding below; the button of the running care state is lit (the kit's hover/active accent)
        let tx = ax.saturating_sub(TRAY_W);
        for (i, (glyph, cs)) in [(crate::brand::icon::WATER, St::Drinking), (crate::brand::icon::FOOD, St::Eating), (crate::brand::icon::TOY, St::Playing)].into_iter().enumerate() {
            let y = ay + h as u16 - 2 - 2 * (2 - i as u16);
            let b = Rect { x: tx, y, width: TRAY_W - 1, height: 1 };
            let sty = if v.st == cs { st(ACCENT).bg(SEL_BG).bold() } else { st(DIM).bg(BG2) };
            f.render_widget(Paragraph::new(Line::styled(format!(" {glyph} "), sty)), b);
            self.tray.push((b, cs));
        }
        let Some(b) = v.bubble else { return };
        let maxw = r.width.saturating_sub(1) as usize;
        let tone = match b.tone { crate::shade::Tone::Error => RED, crate::shade::Tone::Warn => YELLOW, _ => ACCENT };
        let mut body = cells(b.verb, st(tone).bold().bg(BG2));
        if !b.target.is_empty() { body.extend(cells(&format!(" {}", b.target), st(DIM).bg(BG2))); }
        let body = cut(body, maxw.saturating_sub(4));
        let bw = width(&body) + 4;
        let border = st(if b.big { ACCENT } else { SEP });
        // right edge 18% of the art in from its right side (the kit: marginRight size*.18), kept inside the dock
        let right = (ax as usize + w).saturating_sub(w * 18 / 100).max(r.x as usize + bw).min(r.right() as usize);
        let bx = (right - bw) as u16;
        let by = ay.saturating_sub(3);
        let tail = (bw - 2).saturating_sub((w * 14 / 100).max(1)).max(1);
        let top = format!("╭{}╮", "─".repeat(bw - 2));
        let bot: String = (0..bw).map(|i| if i == 0 { '╰' } else if i == bw - 1 { '╯' } else if i == tail { '▽' } else { '─' }).collect();
        let mut mid = vec![('│', border), (' ', Style::new().bg(BG2))];
        mid.extend(body);
        mid.extend([(' ', Style::new().bg(BG2)), ('│', border)]);
        let lines = vec![Line::styled(top, border), group(mid), Line::styled(bot, border)];
        f.render_widget(Paragraph::new(lines), Rect { x: bx, y: by, width: bw as u16, height: 3 });
    }

    fn draw_panel(&mut self, f: &mut Frame, r: Rect) {
        let w = r.width as usize;
        let one = |s: String, sty: Style| group(fit(cells(&s, sty), w, Style::new()));
        let mut top: Vec<Line> = vec![];
        if let Some((c, ok)) = &self.last_cmd {
            let mark = match ok { Some(true) => ("✓ ", GREEN), Some(false) => ("✗ ", RED), None => ("› ", ACCENT) };
            let mut cl = cells(mark.0, st(mark.1));
            cl.extend(cells(c, st(DIM)));
            top.push(group(fit(cl, w, Style::new())));
        }
        let vi = if self.views.is_empty() { None } else { Some(self.sel.unwrap_or(self.views.len() - 1)) };
        let empty = vec![PL::Note(if self.last_read.is_some() { "" } else { "edits will show here" }.into())];
        let body: &[PL] = match (vi, &self.last_read) {
            (Some(i), rd) => {
                if let Some(rd) = rd { top.push(one(format!("read {} L{}–{}", rd.path, rd.a, rd.b), st(DIM))); }
                let v = &self.views[i];
                top.push(one(v.path.clone(), st(TITLE).bold()));
                let mut h = cells(&format!("{} {}/{}", v.kind, i + 1, self.views.len()), st(ACCENT));
                match &v.status {
                    Some(Ok(())) => h.extend(cells("  ✓ applied", st(GREEN))),
                    Some(Err(e)) => h.extend(cells(&format!("  ✗ {e}"), st(RED))),
                    None => h.extend(cells("  …", st(DIM))),
                }
                top.push(group(fit(h, w, Style::new())));
                &v.body
            }
            (None, Some(rd)) => {
                top.push(one(rd.path.clone(), st(TITLE).bold()));
                top.push(one(format!("read L{}–{}", rd.a, rd.b), st(DIM)));
                &rd.body
            }
            (None, None) => { top.push(one("edits".into(), st(TITLE).bold())); &empty }
        };
        top.push(Line::styled("─".repeat(w), st(SEP)));
        let th = (top.len() as u16).min(r.height);
        f.render_widget(Paragraph::new(top), Rect { height: th, ..r });
        let h = r.height.saturating_sub(th) as usize;
        self.panel_h = h;
        self.pscroll = self.pscroll.min(body.len().saturating_sub(h));
        let nw = body.iter().map(|p| match p { PL::Ctx(n, _) | PL::Del(n, _) | PL::Add(n, _) => n.to_string().len(), _ => 0 }).max().unwrap_or(0).max(3);
        let lines: Vec<Line> = body.iter().skip(self.pscroll).take(h).map(|p| pl_line(p, nw, w)).collect();
        f.render_widget(Paragraph::new(lines), Rect { y: r.y + th, height: h as u16, ..r });
    }

    /// approval box body: title, optional Write/Edit detail, the choices (`approve_choices`, `sel` highlighted)
    fn approve_lines(&self, tool: &str, summary: &str, sel: usize, w: usize) -> Vec<Line<'static>> {
        let choices = approve_choices(tool);
        let choice_lines = |out: &mut Vec<Line<'static>>| for (i, (l, _)) in choices.iter().enumerate() {
            let on = i == sel;
            let sty = if on { st(FG).bg(SEL_BG).bold() } else { st(FG) };
            let mut c = cells(if on { "› " } else { "  " }, if on { st(ACCENT).bg(SEL_BG).bold() } else { sty });
            c.extend(cells(&format!("{}. {l}", i + 1), sty));
            out.push(group(fit(c, w, if on { Style::new().bg(SEL_BG) } else { Style::new() })));
        };
        if tool.starts_with(QUESTION) {
            let mut out: Vec<Line<'static>> = summary.lines().map(|l| group(cut(cells(l, st(TITLE).bold()), w))).collect();
            choice_lines(&mut out);
            return out;
        }
        let tool = tool.split_once(RULE).map_or(tool, |(t, _)| t);
        let is_out = tool.starts_with(OUTSIDE);
        let (summary, where_) = summary.split_once('\n').unwrap_or((summary, ""));
        let act = self.entries.iter().rev().find_map(|i| match &i.e { Entry::Tool(a) => Some(&**a), _ => None });
        let (verb, target) = act.map_or_else(|| { let (n, t) = summary.split_once(' ').unwrap_or((summary, "")); (cap(n), one(t)) },
            |a| (a.verb.clone(), a.target.clone()));
        let mut c = cells("Allow ", st(TITLE).bold());
        c.extend(cells(&verb, st(TITLE).bold()));
        if !target.is_empty() {
            c.push((' ', st(FG)));
            let tw = w.saturating_sub(width(&c) + 1);
            c.extend(cut(cells(&target, st(CODE)), tw));
        }
        c.push(('?', st(TITLE).bold()));
        let mut out = vec![group(c)];
        let det = match act.map(|a| (a.name.as_str(), a.info)) {
            Some(("write", Info::Write(new, n))) => Some(format!("{} · {}", if new { "new file" } else { "overwrite" }, plural(n, "line"))),
            Some(("edit", Info::Edit(p, m, all))) => Some(format!("+{p} −{m} lines{}", if all { " · all matches" } else { "" })),
            _ => None,
        };
        if let Some(d) = det { out.push(group(cut(cells(&d, st(DIM)), w))); }
        // the summary's later lines: the outside line (⚠), or a note such as the rule that asked
        for l in where_.lines().filter(|l| !l.trim().is_empty()) {
            let warn = is_out && l.starts_with("outside the project");
            out.push(group(cut(cells(&if warn { format!("⚠ {l}") } else { l.to_string() }, if warn { st(YELLOW).bold() } else { st(DIM) }), w)));
        }
        choice_lines(&mut out);
        out
    }

    /// wrapped input lines and the cursor (col, row) inside the box
    fn input_lines(&self, w: usize) -> (Vec<Line<'static>>, (usize, usize)) {
        let mut rows: Vec<Vec<(char, Style)>> = vec![vec![('›', st(ACCENT).bold()), (' ', st(FG))]];
        let mut pos = (2, 0);
        for (i, &c) in self.input.iter().enumerate() {
            if i == self.cur { pos = (rows.last().unwrap().len(), rows.len() - 1); }
            if c == '\n' { rows.push(vec![(' ', st(FG)); 2]); continue; }
            if rows.last().unwrap().len() >= w + 2 { rows.push(vec![(' ', st(FG)); 2]); }
            rows.last_mut().unwrap().push((c, st(FG)));
        }
        if self.cur >= self.input.len() {
            let last = rows.last().unwrap().len();
            pos = if last >= w + 2 { rows.push(vec![(' ', st(FG)); 2]); (2, rows.len() - 1) } else { (last, rows.len() - 1) };
        }
        let total = rows.len();
        // keep at most 8 rows visible, scrolled to the cursor
        let skip = (pos.1 + 1).saturating_sub(8).min(total.saturating_sub(1));
        let lines = rows.into_iter().skip(skip).take(8).map(group).collect();
        (lines, (pos.0, pos.1 - skip))
    }

    /// a step in plain words: the live line follows the newest step (a tool step only once it ran `QUICK_MS`)
    fn on_activity(&mut self, a: crate::activity::Activity) {
        use crate::activity::Phase;
        let now = Instant::now();
        let same = |x: &Option<(crate::activity::Activity, Instant)>| x.as_ref().is_some_and(|(b, _)| b.id == a.id);
        match a.phase {
            Phase::Start if a.kind != "model" => {
                if let Some(n) = self.act_next.take().filter(|(_, t)| t.elapsed().as_millis() as u64 >= crate::activity::QUICK_MS) { self.act = Some(n); }
                self.act_next = Some((a, now));
            }
            Phase::Start => { self.act_next = None; self.act = Some((a, now)); }
            Phase::Update if same(&self.act_next) && !a.waiting_ok => if let Some((b, _)) = &mut self.act_next { b.text = a.text; },
            Phase::Update => {
                // an approval (or a model update) shows at once
                let since = if same(&self.act) && a.kind != "model" { self.act.as_ref().map_or(now, |(_, t)| *t) } else { now };
                self.act_next = None;
                self.act = Some((crate::activity::Activity { slow: None, ..a }, since));
            }
            Phase::Done => {
                if same(&self.act) { self.act = None; }
                if same(&self.act_next) { self.act_next = None; }
            }
            Phase::Summary => {}
        }
    }

    /// the live line's words and how long that step has run
    fn live_text(&self) -> Option<(String, u128)> {
        let cur = match &self.act_next {
            Some(n) if n.1.elapsed().as_millis() as u64 >= crate::activity::QUICK_MS => Some(n),
            _ => self.act.as_ref(),
        }?;
        let ms = cur.1.elapsed().as_millis();
        Some((cur.0.live_text(ms as u64).to_string(), ms))
    }

    fn status_line(&self, w: usize) -> Line<'static> {
        let sep = || Span::styled(" · ", st(DIM));
        let mut v = vec![];
        let shown = self.view.as_ref().map(|v| v.st);
        match &self.pending {
            Some(Pending::Approve(..)) => v.extend([Span::styled(format!("{} waiting for permission", crate::brand::icon::PERMISSION), st(YELLOW).bold()), sep()]),
            Some(Pending::Input(_)) if shown == Some(St::Sleeping) => v.extend([Span::styled("sleeping · type to wake", st(DIM)), sep()]),
            Some(Pending::Input(_)) if self.quit_at.is_none() => v.extend([Span::styled("● ready", st(GREEN)), sep()]),
            _ => {}
        }
        if self.pending.is_none() {
            let ms = self.since.elapsed().as_millis();
            let live = self.live_text();
            let what = match self.phase {
                _ if self.quit_at.is_some() => "quitting…".to_string(),
                // the step in plain words; a wait longer than a few seconds shows its time
                _ if live.is_some() => {
                    let (t, ms) = live.unwrap_or_default();
                    if ms >= 3000 && !t.starts_with("Writing") { format!("{t} {}", crate::ui::dur((ms / 1000) as f64, 0)) } else { t }
                }
                Phase::Streaming => "streaming…".to_string(),
                Phase::Tool => "running tool…".to_string(),
                _ => format!("thinking… {}", crate::ui::dur((ms / 1000) as f64, 0)),
            };
            v.extend([Span::styled(format!("{} {what}", SPIN[(ms / SPIN_MS) as usize % SPIN.len()]), st(ACCENT)),
                Span::styled(" (esc to interrupt)", st(DIM)), sep()]);
        }
        v.push(Span::styled(if self.model.is_empty() { "elisa".into() } else { self.model.clone() }, st(TITLE).bold()));
        if self.yolo { v.extend([sep(), Span::styled("yolo", st(YELLOW).bold())]); }
        if let Some(j) = self.jobs.iter().rev().find(|j| j.p.done.is_none()) { v.extend([sep(), Span::styled(format!("{}…", j.key), st(ACCENT))]); }
        if !self.queue.is_empty() { v.extend([sep(), Span::styled(format!("queued {}", self.queue.len()), st(YELLOW))]); }
        if self.cards > 0 { v.extend([sep(), Span::styled(format!("card #{}", self.cards), st(DIM))]); }
        if self.rss > RSS_WARN { v.extend([sep(), Span::styled(format!("elisa {} MB", self.rss >> 20), st(YELLOW))]); }
        let hint = match &self.flash {
            Some((s, t)) if t.elapsed() < Duration::from_secs(3) => Span::styled(s.clone(), st(YELLOW)),
            _ if self.scroll > 0 => Span::styled(format!("↓ {} more lines (PgDn)", self.scroll), st(DIM)),
            _ if !self.panel_on() && !self.views.is_empty() => Span::styled("ctrl-o show edits · / commands · ctrl-c quit", st(DIM)),
            _ if !self.panel_on() => Span::styled("/ commands · ctrl-o edit panel · ctrl-c quit", st(DIM)),
            _ => Span::styled("/ commands · [ ] edits · alt-↑↓ panel · ctrl-o hide · esc interrupt", st(DIM)),
        };
        let used_w: usize = v.iter().map(|s| s.width()).sum();
        if used_w + hint.width() + 2 < w {
            v.push(Span::raw(" ".repeat(w - used_w - hint.width())));
            v.push(hint);
        }
        Line::from(v)
    }

    /// the context bar, right-aligned under the input box
    fn ctx_line(&self) -> Line<'static> {
        let (used, max) = self.status.map_or((0, self.max_ctx), |s| (s.ctx_used, s.max_ctx));
        let est = if self.status.is_some_and(|s| s.estimated) { "~" } else { "" };
        let pct = (used * 100).checked_div(max).unwrap_or(0);
        let col = if pct > 85 { RED } else if pct > 70 { YELLOW } else { GREEN };
        let fill = (pct.min(100) + 5) / 10;
        Line::from(vec![
            Span::styled(format!("ctx {est}{}/{} ", k(used), k(max)), st(FG)),
            Span::styled("▓".repeat(fill), st(col)), Span::styled("░".repeat(10 - fill), st(DIM)),
            Span::styled(format!(" {pct}% "), st(col)),
        ]).right_aligned()
    }

    /// speed · reused · Σ in/out · memory, right-aligned under the context bar; None while there is nothing to show
    fn stats_line(&self) -> Option<Line<'static>> {
        let sep = || Span::styled(" · ", st(DIM));
        let mut v = vec![];
        let live = self.first_delta.map(|t| t.elapsed().as_secs_f64()).filter(|&e| e > 0.2 && self.phase == Phase::Streaming);
        if let Some(e) = live {
            v.push(Span::styled(format!("~{:.1} tok/s", self.dchars as f64 / 3.3 / e), st(ACCENT)));
        } else if let Some(s) = self.status {
            v.push(Span::styled(crate::ui::speed(&s), st(FG)));
            if s.reused > 0 { v.extend([sep(), Span::styled(format!("reused {}", k(s.reused)), st(FG))]); }
        }
        if self.sum_out > 0 {
            if !v.is_empty() { v.push(sep()); }
            v.push(Span::styled(format!("Σ in {} · out {}", k(self.sum_in), k(self.sum_out)), st(FG)));
        }
        if self.memory && !self.jobs.iter().any(|j| j.p.done.is_none()) {
            if !v.is_empty() { v.push(sep()); }
            v.push(Span::styled("memory", st(DIM)));
        }
        if v.is_empty() { return None; }
        v.push(Span::raw(" "));
        Some(Line::from(v).right_aligned())
    }

    /// the background-job line under the input box: every job joined by ` · ` (running: spinner · label · bar ·
    /// elapsed; finished: ✓/✗ + the done text); when that does not fit, the bars go, then only the newest + `(+N)`
    fn progress_line(&self, w: usize) -> Option<Line<'static>> {
        if self.jobs.is_empty() { return None; }
        let join = |segs: Vec<Cells>| {
            let mut c: Cells = vec![(' ', Style::new())];
            for (i, s) in segs.into_iter().enumerate() {
                if i > 0 { c.extend(cells(" · ", st(DIM))); }
                c.extend(s);
            }
            c
        };
        for bars in [true, false] {
            let c = join(self.jobs.iter().map(|j| job_cells(j, bars, usize::MAX)).collect());
            if width(&c) <= w { return Some(group(c)); }
        }
        let newest = self.jobs.iter().max_by_key(|j| j.at)?;
        let more = cells(&format!(" (+{})", self.jobs.len() - 1), st(DIM));
        let mut c = join(vec![job_cells(newest, true, w.saturating_sub(1 + width(&more)))]);
        c.extend(more);
        Some(group(fit(c, w, Style::new())))
    }

    /// queued messages, oldest first: at most 3 lines, then `+N more`
    fn queue_lines(&self, w: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line> = self.queue.iter().take(3).enumerate().map(|(i, q)| {
            let t: String = q.chars().map(|c| if c == '\n' { ' ' } else { c }).take(60).collect();
            let more = if q.chars().count() > 60 { "…" } else { "" };
            group(fit(cells(&format!(" ⏳ {}. {t}{more}", i + 1), st(DIM)), w, Style::new()))
        }).collect();
        if self.queue.len() > 3 { out.push(Line::styled(format!("    +{} more", self.queue.len() - 3), st(DIM))); }
        out
    }

    /// the visible transcript lines. Heights are cached per entry for the width; rendered lines are kept only for
    /// entries within one screen of the view and re-rendered when scrolled back in.
    fn transcript(&mut self, w: usize, h: usize) -> Vec<Line<'static>> {
        let w = w.max(10);
        // heights, bottom up; keep the lines of what is probably on screen (the view is measured from the bottom)
        let (lo, hi) = (self.scroll.saturating_sub(h), self.scroll + 2 * h);
        let mut from_bot = 0;
        for it in self.entries.iter_mut().rev() {
            if it.w != w || it.live() {
                let keep = from_bot < hi && from_bot + it.h.max(1) > lo;
                let mut v = vec![];
                render_entry(&it.e, w, &mut v);
                it.w = w;
                it.h = v.len();
                it.lines = keep.then_some(v);
            }
            from_bot += it.h;
        }
        let top = self.dropped as usize;
        let total = top + from_bot;
        if self.scroll > 0 && total > self.last_total { self.scroll += total - self.last_total; }
        self.last_total = total;
        self.view_h = h;
        self.scroll = self.scroll.min(total.saturating_sub(h));
        let end = total - self.scroll;
        let start = end.saturating_sub(h);
        let (ks, ke) = (start.saturating_sub(h), end + h);
        let mut out = vec![];
        if self.dropped && start == 0 { out.push(Line::styled("  … earlier messages are saved (see /resume)", st(DIM))); }
        let mut y = top;
        for it in self.entries.iter_mut() {
            let (a, b) = (y, y + it.h);
            y = b;
            if b <= ks || a >= ke { it.lines = None; continue; }
            let ls = it.lines.get_or_insert_with(|| { let mut v = vec![]; render_entry(&it.e, w, &mut v); v });
            if b > start && a < end {
                let e = (end.min(b) - a).min(ls.len());
                let s0 = (start.max(a) - a).min(e);
                out.extend(ls[s0..e].iter().cloned());
            }
        }
        out
    }
}

fn first_word(s: &str) -> Option<String> { s.split_whitespace().next().map(str::to_lowercase) }

/// one job of the progress line; `lw` caps the label width
fn job_cells(j: &Job, bar: bool, lw: usize) -> Cells {
    let p = &j.p;
    if let Some(d) = &p.done {
        let l = d.trim_start().to_lowercase();
        let bad = l.starts_with("failed") || l.starts_with("error") || l.contains(" failed");
        let mut c = cells(if bad { "✗ " } else { "✓ " }, st(if bad { RED } else { GREEN }).bold());
        c.extend(cut(cells(d, st(if bad { RED } else { FG })), lw.saturating_sub(2)));
        return c;
    }
    let ms = j.at.elapsed().as_millis();
    let mut tail: Cells = vec![];
    if bar {
        const BW: usize = 10;
        let b: String = match p.frac {
            Some(fr) => { let n = ((fr.clamp(0.0, 1.0) * BW as f32).round() as usize).min(BW); "█".repeat(n) + &"░".repeat(BW - n) }
            None => {
                // a 3-cell block bouncing across the track
                let span = BW - 3;
                let k = (ms / 90) as usize % (2 * span);
                let x = if k <= span { k } else { 2 * span - k };
                (0..BW).map(|i| if i >= x && i < x + 3 { '█' } else { '░' }).collect()
            }
        };
        tail.extend(cells(" ▕", st(DIM)));
        tail.extend(cells(&b, st(ACCENT)));
        tail.extend(cells("▏", st(DIM)));
    }
    if let Some(fr) = p.frac { tail.extend(cells(&format!(" {:.0}%", fr.clamp(0.0, 1.0) * 100.0), st(FG))); }
    tail.extend(cells(&format!(" {}", crate::ui::dur(p.elapsed_s + j.at.elapsed().as_secs_f64(), 1)), st(DIM)));
    let mut c = cells(&format!("{} ", SPIN[(ms / SPIN_MS) as usize % SPIN.len()]), st(ACCENT));
    let lw = lw.saturating_sub(width(&c) + width(&tail));
    c.extend(cut(cells(&p.label, st(FG)), lw));
    c.extend(tail);
    c
}

/// this process's resident size in bytes (0 if unknown)
fn own_rss() -> u64 {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let sz = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut libc::c_void, sz);
        if n == sz { ti.pti_resident_size } else { 0 }
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        // peak, not current: the best portable proxy (ru_maxrss is KiB on Linux)
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) == 0 { ru.ru_maxrss as u64 * 1024 } else { 0 }
    }
}

/// one transcript entry: a blank separator line (for most kinds) + its lines
fn render_entry(e: &Entry, w: usize, out: &mut Vec<Line<'static>>) {
    if !matches!(e, Entry::Foot(..) | Entry::Recalled(_) | Entry::Card(..)) && !matches!(e, Entry::Info(s) if s.starts_with("  ↳")) { out.push(Line::default()); }
    match e {
        Entry::Foot(s) => {
            let mut t = format!("  in {}", k(s.ctx_used.saturating_sub(s.output_tokens)));
            if s.reused > 0 { t += &format!(" (reused {})", k(s.reused)); }
            wrap(cells(&(t + &format!(" · out {} · {}", s.output_tokens, crate::ui::speed(s))), st(DIM)), w, 4, out);
        }
        Entry::Full(title, text) => {
            let mut body = vec![];
            md(text, w.saturating_sub(4).max(8), &mut body);
            boxed_lines(title, body, w, out);
        }
        Entry::Banner(b) => banner(b, w, out),
        Entry::User(t) => {
            for (i, l) in t.lines().enumerate() {
                let mut c = cells(if i == 0 { "› " } else { "  " }, st(ACCENT).bold());
                c.extend(cells(l, st(TITLE).bold()));
                wrap(c, w, 2, out);
            }
        }
        Entry::Text(t) => md(t, w, out),
        Entry::Recalled(h) => {
            out.push(Line::styled(format!("  ↺ recalled {} {}", h.len(), if h.len() == 1 { "memory" } else { "memories" }), st(DIM)));
            for l in h {
                let mut c = cells("    · ", st(DIM));
                c.extend(cut(cells(&one(l), st(DIM)), w.saturating_sub(6)));
                out.push(group(c));
            }
        }
        Entry::Tool(a) => {
            let ms = a.t0.elapsed().as_millis();
            // the kit's tool badge: OK / ERR / RUN / ASK / NO on the status colour
            let (badge, col) = match &a.end {
                Some(e) if e.note == "denied" => ("NO", RED),
                Some(e) => if e.red { ("ERR", RED) } else { ("OK", GREEN) },
                None if a.wait => ("ASK", YELLOW),
                None => ("RUN", ACCENT),
            };
            let tail = if a.end.is_none() && !a.wait && ms >= 1000 { format!(" {}", crate::ui::dur((ms / 1000) as f64, 0)) } else { String::new() };
            let mut c = cells(&format!(" {badge:<3} "), Style::new().fg(BG).bg(col).bold());
            c.push((' ', Style::new()));
            c.extend(cells(&a.verb, st(TITLE).bold()));
            if !a.target.is_empty() {
                c.push((' ', st(FG)));
                let tw = w.saturating_sub(width(&c) + tail.chars().count());
                c.extend(cut(cells(&a.target, st(FG)), tw));
            }
            c.extend(cells(&tail, st(DIM)));
            out.push(group(c));
            match &a.end {
                None if a.wait => out.push(Line::styled("  ↳ waiting for approval", st(DIM))),
                None => {}
                Some(e) => {
                    let mut c = cells("  ↳ ", st(DIM));
                    c.extend(cut(cells(&e.note, st(if e.red { RED } else { DIM })), w.saturating_sub(4)));
                    out.push(group(c));
                    for l in &e.prev {
                        let mut c = cells("    ", st(DIM));
                        c.extend(cut(cells(l, st(DIM)), w.saturating_sub(4)));
                        out.push(group(c));
                    }
                    if e.more > 0 { out.push(Line::styled(format!("    … +{} lines", e.more), st(DIM))); }
                }
            }
        }
        Entry::Info(s) => for l in s.lines() { wrap(cells(l, st(DIM)), w, 2, out) },
        Entry::Err(s) => for l in s.lines() { wrap(cells(l, st(RED)), w, 2, out) },
        Entry::Notes(n) => boxed("notes saved", n, w, out),
        Entry::Watch(s) => {
            let (a, b) = s.split_once(" · ").unwrap_or((s, ""));
            let mut c = cells("  ", st(FG));
            c.extend(cells(a, st(ACCENT).bold()));
            if !b.is_empty() { c.extend(cells(&format!(" · {b}"), st(FG))); }
            wrap(c, w, 4, out);
        }
        Entry::WatchView(v) => watch_box(v, w, out),
        Entry::Card(c, false) => {
            let now = c.lines().map(str::trim).find_map(|l| l.strip_prefix("now:").or_else(|| l.strip_prefix("Now:")))
                .or_else(|| c.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
            let mut t = cells("  ◇ now: ", st(DIM));
            t.extend(cells(&one(now), st(DIM)));
            out.push(group(cut(t, w)));
        }
        Entry::Card(c, true) => {
            out.push(Line::styled("  ◇ state card (ctrl-s to collapse)", st(DIM)));
            for l in c.lines().filter(|l| !l.trim().is_empty()) { wrap(cells(&format!("    {l}"), st(DIM)), w, 6, out) }
        }
    }
}

/// paint a rect: spaces on `bg` (the brand background everywhere; raised surfaces on bg2)
struct Fill(Color);
impl Widget for Fill {
    fn render(self, a: Rect, buf: &mut Buffer) {
        for y in a.top()..a.bottom() { for x in a.left()..a.right() { buf[(x, y)].reset(); buf[(x, y)].set_bg(self.0); } }
    }
}

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded).border_style(st(ACCENT))
        .title(Span::styled(format!(" {title} "), st(TITLE).bold()))
}

/// the welcome box (the kit's prototype `BOX.welcome`): ELISA label, model / server / folder / context, the notes
fn banner(b: &Banner, w: usize, out: &mut Vec<Line<'static>>) {
    let kv = |k: &str, v: &str, s: Style| { let mut c = cells(&format!("{k:<9}"), st(DIM)); c.extend(cells(v, s)); c };
    let mut ctx = format!("{} tokens", k(b.max_ctx));
    if let Some(n) = b.resumed { ctx += &format!(" · resumed {n} messages"); }
    let mut rows = vec![kv("model", &b.model, st(TITLE)), kv("server", &b.base_url, st(CODE)), kv("folder", &b.cwd, st(CODE)), kv("context", &ctx, st(FG))];
    if let Some(n) = &b.notes {
        rows.push(cells(&format!("notes · loaded ({} words)", n.split_whitespace().count()), st(DIM)));
        let ls: Vec<&str> = n.lines().filter(|l| !l.trim().is_empty()).collect();
        for l in ls.iter().take(6) { let mut c = cells("– ", st(DIM)); c.extend(cells(l, st(FG))); rows.push(c); }
        if ls.len() > 6 { rows.push(cells(&format!("… +{} lines (/notes)", ls.len() - 6), st(DIM))); }
    }
    let mut body = vec![];
    for c in rows { wrap(c, w.max(12) - 4, 9, &mut body); }
    labeled_box("ELISA", "local assistant", body, w, out);
    out.push(Line::styled("  / commands · esc interrupts · ctrl-o edits · /resume past sessions · ctrl-c quits", st(DIM)));
    out.push(Line::styled("  wheel, ↑↓ (empty input), PgUp/PgDn scroll · ctrl-p/n history · ctrl-s state card · /copy copies the last reply", st(DIM)));
    out.push(Line::styled("  click Elisa or ctrl-g to poke · /water /feed /play or F2 F3 F4 · idle, and it dozes off", st(DIM)));
}

/// terminal heights for the status-bar face: 10 x 5 from `FACE_TALL` rows, 8 x 4 from `FACE_MIN`, else text only
const FACE_TALL: u16 = 40;
const FACE_MIN: u16 = 30;

/// the care tray's width in cells: a 3-cell button and a 1-cell gap to Shade (layout.json gap 6 px)
const TRAY_W: u16 = 4;

/// the care commands (kit v2 tray: water, food, toy; plus a poke): handled by the TUI, never sent to the agent
fn care_command(t: &str) -> Option<ShEv<'static>> {
    match t {
        "/poke" => Some(ShEv::Poke),
        "/water" => Some(ShEv::Water),
        "/feed" | "/food" => Some(ShEv::Food),
        "/play" | "/toy" => Some(ShEv::Toy),
        _ => None,
    }
}

/// a rounded accent box with a pixel-style label (uppercase, accent, bold: the terminal's stand-in for Silkscreen)
fn labeled_box(label: &str, sub: &str, body: Vec<Line<'static>>, w: usize, out: &mut Vec<Line<'static>>) {
    let w = w.max(12);
    let b = st(ACCENT);
    let head = format!(" {label} ");
    let sub = if sub.is_empty() { String::new() } else { format!("{sub} ") };
    let used = 2 + head.chars().count() + sub.chars().count() + 1;
    let sub: String = if used > w { String::new() } else { sub };
    let used = 2 + head.chars().count() + sub.chars().count() + 1;
    out.push(Line::from(vec![
        Span::styled("╭─", b), Span::styled(head, st(ACCENT).bold()), Span::styled(sub, st(DIM)),
        Span::styled(format!("{}╮", "─".repeat(w.saturating_sub(used))), b),
    ]));
    for l in body {
        let pad = (w - 4).saturating_sub(l.width());
        let mut s = vec![Span::styled("│ ", b)];
        s.extend(l.spans);
        s.push(Span::raw(" ".repeat(pad)));
        s.push(Span::styled(" │", b));
        out.push(Line::from(s));
    }
    out.push(Line::styled(format!("╰{}╯", "─".repeat(w - 2)), b));
}

/// a rounded box around full text (the notes summary)
fn boxed(title: &str, text: &str, w: usize, out: &mut Vec<Line<'static>>) {
    boxed_cells(title, text.trim().lines().map(|l| inline(l, st(FG))).collect(), w, out)
}

fn boxed_cells(title: &str, rows: Vec<Cells>, w: usize, out: &mut Vec<Line<'static>>) {
    let mut body = vec![];
    for c in rows { wrap(c, w.max(12) - 4, 0, &mut body); }
    boxed_lines(title, body, w, out)
}

/// a rounded box around lines already wrapped to `w - 4`
fn boxed_lines(title: &str, body: Vec<Line<'static>>, w: usize, out: &mut Vec<Line<'static>>) {
    let w = w.max(12);
    let b = st(ACCENT);
    let t = format!(" {title} ");
    out.push(Line::from(vec![
        Span::styled("╭─", b), Span::styled(t.clone(), st(ACCENT).bold()),
        Span::styled(format!("{}╮", "─".repeat(w.saturating_sub(3 + t.chars().count()))), b),
    ]));
    for l in body {
        let pad = (w - 4).saturating_sub(l.width());
        let mut s = vec![Span::styled("│ ", b)];
        s.extend(l.spans);
        s.push(Span::raw(" ".repeat(pad)));
        s.push(Span::styled(" │", b));
        out.push(Line::from(s));
    }
    out.push(Line::styled(format!("╰{}╯", "─".repeat(w - 2)), b));
}

/// the audit line under the input box: ✓ green, ✗ red, the rest in the normal text colour
fn audit_cells_line(t: &str, w: usize) -> Line<'static> {
    let mut c = cells(" ", st(FG));
    for ch in t.chars() { c.push((ch, match ch { '✓' => st(GREEN).bold(), '✗' => st(RED).bold(), _ => st(FG) })); }
    group(fit(c, w, Style::new()))
}

/// the watcher under the input box, right-aligned like the ctx / stats lines, one part per row:
/// `watcher: <domain> <conf>` / `since turn … · would unload …` / `−… ms/pass · dry run`
fn watch_rows(v: &WatcherView, w: usize) -> Vec<Line<'static>> {
    crate::watcher::lines(v).into_iter().enumerate().map(|(i, s)| {
        let mut c = match (i, s.strip_prefix("watcher:")) {
            (0, Some(rest)) => {
                let mut c = cells("watcher:", st(ACCENT).bold());
                let (d, tail) = rest.trim_start().split_once(' ').unwrap_or((rest.trim_start(), ""));
                c.extend(cells(&format!(" {d}"), st(TITLE).bold()));
                if !tail.is_empty() { c.extend(cells(&format!(" {tail}"), st(FG))); }
                c
            }
            _ if s.ends_with("dry run") => {
                let (a, _) = s.rsplit_once("dry run").unwrap_or((&s, ""));
                let mut c = cells(a, st(FG));
                c.extend(cells("dry run", st(YELLOW)));
                c
            }
            _ => cells(&s, st(FG)),
        };
        c.push((' ', Style::new()));
        let c = if width(&c) > w { fit(c, w, Style::new()) } else { c };
        group(c).right_aligned()
    }).collect()
}

/// /watcher: the full view in a box; section titles bold, the per-layer bars in the accent colour
fn watch_box(v: &WatcherView, w: usize, out: &mut Vec<Line<'static>>) {
    let rows = crate::watcher::full_lines(v).into_iter().map(|l| {
        if let Some(t) = l.strip_prefix("## ") { return cells(t, st(TITLE).bold()); }
        if l.starts_with("dry run") { return cells(&l, st(YELLOW).bold()); }
        match l.trim_start().starts_with('L').then(|| l.rfind(' ')).flatten() {
            Some(i) if l[i + 1..].chars().all(|c| ('▁'..='█').contains(&c)) => {
                let mut c = cells(&l[..=i], st(FG));
                c.extend(cells(&l[i + 1..], st(ACCENT)));
                c
            }
            _ => cells(&l, st(FG)),
        }
    }).collect();
    boxed_cells(if v.dry() { "watcher · dry run" } else { "watcher" }, rows, w, out);
}

type Cells = Vec<(char, Style)>;

fn sp(n: usize) -> Cells { vec![(' ', Style::new()); n] }
/// the reply marker: a short accent bar on the first line of each reply (the kit puts a tiny Shade there)
fn lead(first: &mut bool) -> Cells { if std::mem::take(first) { cells("▍ ", st(ACCENT)) } else { cells("  ", st(FG)) } }
fn width(c: &[(char, Style)]) -> usize { c.iter().map(|&(c, _)| cw(c)).sum() }

/// cut to `w` columns (… when cut), then pad with `pad` style
fn fit(mut c: Cells, w: usize, pad: Style) -> Cells {
    if width(&c) > w {
        let mut n = 0;
        let keep = c.iter().take_while(|&&(ch, _)| { n += cw(ch); n < w }).count();
        let s = c.get(keep).map_or(pad, |x| x.1);
        c.truncate(keep);
        c.push(('…', s));
    }
    let n = width(&c);
    c.extend(std::iter::repeat_n((' ', pad), w.saturating_sub(n)));
    c
}

/// Markdown for a (possibly still streaming) reply: headings, lists, quotes, rules, pipe tables, fenced code.
fn md(text: &str, w: usize, out: &mut Vec<Line<'static>>) {
    let lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    let (mut first, mut i) = (true, 0);
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with("```") { j += 1; }
            code_block(lang.trim(), &lines[i + 1..j], w, &mut first, out);
            i = j + 1;
            continue;
        }
        if t.starts_with('|') {
            let j = i + lines[i..].iter().take_while(|l| l.trim_start().starts_with('|')).count();
            table(&lines[i..j], w, &mut first, out);
            i = j;
            continue;
        }
        i += 1;
        if t.is_empty() { out.push(Line::default()); continue; }
        let mut c = lead(&mut first);
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if is_hr(t) {
            c.extend(cells(&"─".repeat(w.saturating_sub(2)), st(DIM)));
            out.push(group(c));
        } else if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let s = match hashes { 1 => st(ACCENT).bold().underlined(), 2 => st(ACCENT).bold(), _ => st(H3).bold() };
            c.extend(inline(t[hashes..].trim(), s));
            wrap_p(c, w, sp(2), out);
        } else if let Some(q) = t.strip_prefix('>') {
            c.extend(cells("▌ ", st(DIM)));
            c.extend(inline(q.trim_start(), st(FG).italic()));
            let mut cont = sp(2);
            cont.extend(cells("▌ ", st(DIM)));
            wrap_p(c, w, cont, out);
        } else if let Some((m, rest)) = list_item(t, l.len() - t.len()) {
            let nest = ((l.len() - t.len()) / 2 * 2).min(8);
            c.extend(sp(nest));
            c.extend(cells(&m, st(ACCENT)));
            c.extend(inline(rest, st(FG)));
            wrap_p(c, w, sp(2 + nest + m.chars().count()), out);
        } else {
            c.extend(inline(t, st(FG)));
            wrap_p(c, w, sp(2), out);
        }
    }
}

fn is_hr(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && ["-", "*", "_"].iter().any(|m| s.chars().all(|c| c.to_string() == *m))
}

/// "- x" / "* x" / "+ x" / "12. x" / "3) x" → (marker, rest)
fn list_item(t: &str, ind: usize) -> Option<(String, &str)> {
    for p in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(p) { return Some((if ind >= 2 { "◦ " } else { "• " }.into(), r)); }
    }
    let d = t.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &t[d..];
    if d > 0 && d < 4 && (rest.starts_with(". ") || rest.starts_with(") ")) { return Some((format!("{}. ", &t[..d]), &rest[2..])); }
    None
}

fn code_block(lang: &str, body: &[&str], w: usize, first: &mut bool, out: &mut Vec<Line<'static>>) {
    let bw = w.saturating_sub(2).max(8);
    let bg = Style::new().bg(CODE_BG);
    let mut c = lead(first);
    c.extend(fit(cells(&format!(" {}", if lang.is_empty() { "code" } else { lang }), st(DIM).bg(CODE_BG).italic()), bw, bg));
    out.push(group(c));
    for l in body {
        let mut c = lead(first);
        c.push((' ', bg));
        c.extend(fit(cells(l, st(CODE).bg(CODE_BG)), bw - 1, bg));
        out.push(group(c));
    }
    let mut c = lead(first);
    c.extend(fit(vec![], bw, bg));
    out.push(group(c));
}

fn table(rows: &[&str], w: usize, first: &mut bool, out: &mut Vec<Line<'static>>) {
    let split = |r: &str| -> Vec<String> {
        let r = r.trim();
        let r = r.strip_prefix('|').unwrap_or(r);
        let r = r.strip_suffix('|').unwrap_or(r);
        r.split('|').map(|c| c.trim().to_string()).collect()
    };
    let is_sep = |r: &[String]| r.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
    let rows: Vec<Vec<String>> = rows.iter().map(|r| split(r)).collect();
    let header = rows.len() > 1 && is_sep(&rows[1]);
    let body: Vec<(bool, Vec<Cells>)> = rows.iter().enumerate().filter(|(i, r)| !(header && *i == 1) && !is_sep(r))
        .map(|(i, r)| {
            let s = if header && i == 0 { st(TITLE).bold() } else { st(FG) };
            (header && i == 0, r.iter().map(|c| inline(c, s)).collect())
        }).collect();
    let n = body.iter().map(|r| r.1.len()).max().unwrap_or(0);
    if n == 0 { return; }
    let mut cw_: Vec<usize> = (0..n).map(|j| body.iter().map(|r| r.1.get(j).map_or(0, |c| width(c))).max().unwrap_or(0).max(1)).collect();
    let avail = w.saturating_sub(2 + 3 * (n - 1)).max(n * 3);
    while cw_.iter().sum::<usize>() > avail {
        let (j, &m) = cw_.iter().enumerate().max_by_key(|x| x.1).unwrap();
        if m <= 3 { break; }
        cw_[j] -= 1;
    }
    for (is_head, r) in body {
        let mut c = lead(first);
        for (j, &cwj) in cw_.iter().enumerate() {
            if j > 0 { c.extend(cells(" │ ", st(DIM))); }
            c.extend(fit(r.get(j).cloned().unwrap_or_default(), cwj, Style::new()));
        }
        out.push(group(c));
        if is_head {
            let mut c = lead(first);
            let s: Vec<String> = cw_.iter().map(|&x| "─".repeat(x)).collect();
            c.extend(cells(&s.join("─┼─"), st(DIM)));
            out.push(group(c));
        }
    }
}

/// inline markdown: **bold**, *italic*, `code`, [link](url)
fn inline(l: &str, base: Style) -> Cells {
    let ch: Vec<char> = l.chars().collect();
    let (mut bold, mut ital, mut code, mut out, mut i) = (false, false, false, vec![], 0);
    while i < ch.len() {
        let c = ch[i];
        if c == '`' { code = !code; i += 1; continue; }
        if !code {
            if c == '*' && ch.get(i + 1) == Some(&'*') { bold = !bold; i += 2; continue; }
            if c == '*' && (ital || (ch.get(i + 1).is_some_and(|n| !n.is_whitespace()) && ch[i + 1..].contains(&'*'))) {
                ital = !ital;
                i += 1;
                continue;
            }
            if c == '[' {
                let rest: String = ch[i + 1..].iter().collect();
                if let Some(p) = rest.find("](") {
                    if let Some(q) = rest[p + 2..].find(')') {
                        let text = &rest[..p];
                        out.extend(text.chars().map(|c| (c, base.fg(CODE).underlined())));
                        i += 1 + rest[..p + 2 + q + 1].chars().count();
                        continue;
                    }
                }
            }
        }
        let s = if code { st(CODE).bg(CODE_BG) } else {
            let mut s = base;
            if bold { s = s.bold(); }
            if ital { s = s.italic(); }
            s
        };
        out.push(if c == '\t' { (' ', s) } else { (c, s) });
        i += 1;
    }
    out
}

fn cells(s: &str, style: Style) -> Vec<(char, Style)> {
    s.chars().flat_map(|c| if c == '\t' { vec![(' ', style); 4] } else { vec![(c, style)] }).collect()
}

fn cw(c: char) -> usize { let mut b = [0u8; 4]; Span::raw(&*c.encode_utf8(&mut b)).width() }

fn wrap(c: Cells, w: usize, indent: usize, out: &mut Vec<Line<'static>>) { wrap_p(c, w, sp(indent), out) }

/// word-wrap styled cells to `w` columns; continuation rows start with `cont`
fn wrap_p(cells: Cells, w: usize, cont: Cells, out: &mut Vec<Line<'static>>) {
    let ci = cont.len();
    let w = w.max(width(&cont) + 4);
    let mut row: Cells = vec![];
    let mut col = 0;
    for (c, s) in cells {
        let x = cw(c);
        if col + x > w && !row.is_empty() {
            let sp_ = row.iter().rposition(|&(c, _)| c == ' ').filter(|&p| p > ci);
            let rest = match sp_ { Some(p) => row.split_off(p + 1), None => vec![] };
            while row.last().is_some_and(|&(c, _)| c == ' ') && row.len() > 1 { row.pop(); }
            out.push(group(std::mem::take(&mut row)));
            row = cont.clone();
            row.extend(rest);
            col = width(&row);
        }
        row.push((c, s));
        col += x;
    }
    out.push(group(row));
}

fn group(cells: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![];
    let mut cur = String::new();
    let mut cs = Style::new();
    for (c, s) in cells {
        if s != cs && !cur.is_empty() { spans.push(Span::styled(std::mem::take(&mut cur), cs)); }
        cs = s;
        cur.push(c);
    }
    if !cur.is_empty() { spans.push(Span::styled(cur, cs)); }
    Line::from(spans)
}

fn fmt_ts(ts: u64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

// ---------- action entries (one per tool call) ----------
#[derive(Clone, Copy)]
enum Info { None, Write(bool, usize), Edit(usize, usize, bool) }
struct Act { name: String, verb: String, target: String, info: Info, t0: Instant, wait: bool, end: Option<End> }
struct End { note: String, red: bool, prev: Vec<String>, more: usize }

fn cap(n: &str) -> String { let mut c = n.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect()) }
fn one(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }
/// cut to `w` columns with … (no padding)
fn cut(c: Cells, w: usize) -> Cells { if width(&c) > w { fit(c, w, Style::new()) } else { c } }
fn plural(n: usize, s: &str) -> String { format!("{n} {s}{}", if n == 1 { "" } else { "s" }) }

pub(crate) fn act_target(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Option<String> {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    Some(match name {
        "read" | "write" | "edit" | "outline" => { let f = cwd.join(sv("path")); f.strip_prefix(cwd).unwrap_or(&f).display().to_string() }
        "read_symbol" | "edit_symbol" => { let f = cwd.join(sv("path")); format!("{} · {}", f.strip_prefix(cwd).unwrap_or(&f).display(), sv("name")) }
        "bash" => sv("command").into(),
        "glob" | "grep" => match sv("path") { "" | "." => sv("pattern").into(), p => format!("{} in {p}", sv("pattern")) },
        _ => return None,
    })
}

fn act_info(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Info {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "write" => Info::Write(!cwd.join(sv("path")).exists(), sv("content").lines().count()),
        "edit" => {
            let (o, n): (Vec<&str>, Vec<&str>) = (sv("old").lines().collect(), sv("new").lines().collect());
            let d = diff(&o, &n);
            let all = matches!(input.get("all"), Some(serde_json::Value::Bool(true))) || sv("all") == "true";
            Info::Edit(d.iter().filter(|x| x.0 == '+').count(), d.iter().filter(|x| x.0 == '-').count(), all)
        }
        _ => Info::None,
    }
}

/// `24 symbols` from an outline header (`path · rust · 375 lines · 24 symbols`), `7 files` for a directory
pub(crate) fn outline_note(o: &str) -> String {
    let first = o.lines().next().unwrap_or("");
    match first.rsplit(" · ").next().filter(|s| s.ends_with("symbols") || s.ends_with("symbol")) {
        Some(s) => s.to_string(),
        None => plural(o.lines().filter(|l| l.contains(" lines): ")).count(), "file"),
    }
}

/// `lines 12–30` from a read_symbol header (`path:12-30 fn Foo::bar`)
pub(crate) fn symbol_note(o: &str) -> String {
    let first = o.lines().next().unwrap_or("");
    let r = first.split_whitespace().next().and_then(|p| p.rsplit(':').next()).unwrap_or("");
    format!("lines {}", r.replacen('-', "–", 1))
}

/// the file part of a grep hit `file:line: text`
fn grep_file(l: &str) -> &str {
    for (i, _) in l.match_indices(':') {
        let r = &l[i + 1..];
        let d = r.bytes().take_while(u8::is_ascii_digit).count();
        if d > 0 && r[d..].starts_with(':') { return &l[..i]; }
    }
    l
}

/// the gray note under an action, from the tool output
fn finish(a: &Act, o: &str, err: bool) -> End {
    let first = || crate::jsonl::first_note(o).unwrap_or_else(|| (if err { "failed" } else { "(no output)" }).to_string());
    let red = |note: String| End { note, red: true, prev: vec![], more: 0 };
    if err && o.starts_with("denied") { return red("denied".into()); }
    if a.name == "bash" {
        let mut ls: Vec<&str> = o.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
        let code = ls.last().and_then(|l| l.strip_prefix("[exit ")).and_then(|r| r.strip_suffix(']')).map(str::to_string);
        if code.is_some() { ls.pop(); }
        let bad = err || code.as_deref().is_some_and(|c| c != "0");
        let mut parts: Vec<String> = if bad { ls.last() } else { ls.first() }.map(|l| l.trim().to_string()).into_iter().collect();
        if let Some(c) = code { parts.push(format!("exit {c}")); }
        parts.push(crate::ui::dur(a.t0.elapsed().as_secs_f64(), 1));
        let prev = if ls.len() > 1 { ls.iter().take(3).map(|l| l.to_string()).collect() } else { vec![] };
        return End { note: parts.join(" · "), red: bad, prev, more: ls.len().saturating_sub(3) };
    }
    if err { return red(first()); }
    let last = o.lines().last().unwrap_or("");
    let note = match (a.name.as_str(), a.info) {
        ("outline", _) => outline_note(o),
        ("read_symbol", _) => symbol_note(o),
        ("read", _) => match last.strip_prefix("[lines ") {
            Some(r) => format!("lines {}", r.split(']').next().unwrap_or("").replacen('-', "–", 1)),
            None if o.starts_with('[') => "empty file".into(),
            None => plural(o.lines().count(), "line"),
        },
        ("write", Info::Write(new, n)) => format!("{} · {}", if new { "new file" } else { "rewrote" }, plural(n, "line")),
        ("edit", Info::Edit(_, _, true)) => {
            let n = o.rsplit('(').next().and_then(|r| r.split_whitespace().next()).and_then(|d| d.parse().ok()).unwrap_or(1);
            plural(n, "replacement")
        }
        ("edit", Info::Edit(p, m, false)) => format!("+{p} −{m} lines"),
        ("glob", _) if o == "no files found" => "0 files".into(),
        ("glob", _) => match last.strip_prefix('[').and_then(|r| r.split_whitespace().nth(2)) {
            Some(n) => format!("{n} files"),
            None => plural(o.lines().count(), "file"),
        },
        ("grep", _) if o == "no matches" => "0 matches".into(),
        ("grep", _) => {
            let hits: Vec<&str> = o.lines().filter(|l| !l.starts_with("[stopped")).collect();
            let mut files: Vec<&str> = hits.iter().map(|l| grep_file(l)).collect();
            files.sort_unstable();
            files.dedup();
            if hits.len() < o.lines().count() { format!("{}+ matches (capped) in {}", hits.len(), plural(files.len(), "file")) }
            else { format!("{} {} in {}", hits.len(), if hits.len() == 1 { "match" } else { "matches" }, plural(files.len(), "file")) }
        }
        _ => first(),
    };
    End { note, red: false, prev: vec![], more: 0 }
}

// ---------- edit panel model (built on the worker thread in tool_detail, before the tool runs) ----------
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PL { Ctx(usize, String), Del(usize, String), Add(usize, String), Gap, Note(String) }
pub(crate) struct View { pub(crate) path: String, pub(crate) kind: &'static str, pub(crate) body: Vec<PL>, status: Option<Result<(), String>> }
pub(crate) struct ReadV { pub(crate) path: String, pub(crate) a: usize, pub(crate) b: usize, body: Vec<PL> }
pub(crate) enum Detail { View(View), Read(ReadV), Cmd(String) }
#[derive(PartialEq)]
enum Await { No, View(usize), Cmd }

const MAXL: usize = 400; // chars kept per code line
const MAXN: usize = 500; // lines kept per view

fn own(l: &str) -> String { l.chars().take(MAXL).collect() }

pub(crate) fn detail(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Option<Detail> {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let num = |k: &str| input.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).map(|v| v as usize);
    let full = cwd.join(sv("path"));
    let path = full.strip_prefix(cwd).unwrap_or(&full).display().to_string();
    match name {
        "edit" => {
            let (old, new) = (sv("old"), sv("new"));
            let all = matches!(input.get("all"), Some(serde_json::Value::Bool(true))) || sv("all") == "true";
            let body = match std::fs::read_to_string(&full) {
                Err(e) => vec![PL::Note(format!("cannot read: {e}"))],
                Ok(t) => edit_body(&t, old, new, all),
            };
            Some(Detail::View(View { path, kind: "edit", body, status: None }))
        }
        "write" => {
            let new: Vec<&str> = sv("content").lines().collect();
            let (kind, body) = match std::fs::read_to_string(&full) {
                Ok(old) => {
                    let old: Vec<&str> = old.lines().collect();
                    ("rewrite", hunks(&diff(&old, &new), &old, &new, 1, 1, 2))
                }
                Err(_) => {
                    let mut b: Vec<PL> = new.iter().take(MAXN).enumerate().map(|(i, l)| PL::Add(i + 1, own(l))).collect();
                    if new.len() > MAXN { b.push(PL::Note(format!("… +{} lines", new.len() - MAXN))); }
                    ("new file", b)
                }
            };
            Some(Detail::View(View { path, kind, body, status: None }))
        }
        "read" => {
            let t = std::fs::read_to_string(&full).ok()?;
            let ls: Vec<&str> = t.lines().collect();
            let a = num("offset").unwrap_or(1).max(1);
            let b = (a - 1 + num("limit").filter(|&l| l > 0).unwrap_or(400)).min(ls.len());
            let body = (a..=b).take(MAXN).map(|i| PL::Ctx(i, own(ls[i - 1]))).collect();
            Some(Detail::Read(ReadV { path, a, b, body }))
        }
        "read_symbol" => {
            let ctx = crate::tools::ToolCtx { cwd: cwd.to_path_buf(), ..Default::default() };
            let (a, b) = crate::tools::symbol_lines(input, &ctx)?;
            let t = std::fs::read_to_string(&full).ok()?;
            let ls: Vec<&str> = t.lines().collect();
            let body = (a..=b.min(ls.len())).take(MAXN).map(|i| PL::Ctx(i, own(ls[i - 1]))).collect();
            Some(Detail::Read(ReadV { path, a, b, body }))
        }
        "edit_symbol" => {
            let ctx = crate::tools::ToolCtx { cwd: cwd.to_path_buf(), ..Default::default() };
            let body = match crate::tools::edit_symbol_preview(input, &ctx) {
                Some((old, new)) => {
                    let (o, n): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
                    hunks(&diff(&o, &n), &o, &n, 1, 1, 3)
                }
                None => vec![PL::Note(format!("symbol `{}` not found (or ambiguous) in the file", sv("name")))],
            };
            Some(Detail::View(View { path, kind: "edit", body, status: None }))
        }
        "outline" => Some(Detail::Cmd(format!("outline {path}"))),
        "bash" => Some(Detail::Cmd(format!("$ {}", sv("command").split_whitespace().collect::<Vec<_>>().join(" ")))),
        "glob" | "grep" => Some(Detail::Cmd(format!("{name} {}", sv("pattern")))),
        _ => None,
    }
}

/// The edit as a diff with 3 context lines, computed from the file text the edit would write (the same
/// `replacen` / `replace` as `tools::edit`), so the preview is what lands on disk. Splicing `new` in as whole lines
/// showed an old text ending in a newline replaced by a new one without it as "no changes" / two separate lines,
/// while the file got the two lines joined into one (soak M2).
fn edit_body(text: &str, old: &str, new: &str, all: bool) -> Vec<PL> {
    let n = if old.is_empty() { 0 } else { text.matches(old).count() };
    if n == 0 { return vec![PL::Note("old text not found in the file".into())]; }
    let after = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    let (o, nw): (Vec<&str>, Vec<&str>) = (text.lines().collect(), after.lines().collect());
    let ops = diff(&o, &nw);
    let mut out = if ops.iter().all(|x| x.0 == ' ') && after != text {
        vec![PL::Note("only line endings change (a newline added or removed at the end of the file)".into())]
    } else {
        hunks(&ops, &o, &nw, 1, 1, 3)
    };
    if n > 1 { out.push(PL::Note(format!("{n} matches{}", if all { ", all replaced" } else { " (not unique: the edit will fail)" }))); }
    out
}

/// line diff: (op, old idx, new idx); op ' ' / '-' / '+'. Prefix/suffix trim, LCS on the middle (bounded).
fn diff(a: &[&str], b: &[&str]) -> Vec<(char, usize, usize)> {
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[p..a.len() - s], &b[p..b.len() - s]);
    let mut ops: Vec<(char, usize, usize)> = (0..p).map(|i| (' ', i, i)).collect();
    let (n, m) = (am.len(), bm.len());
    if n * m > 2_000_000 {
        ops.extend((0..n).map(|i| ('-', p + i, p)));
        ops.extend((0..m).map(|j| ('+', p + n, p + j)));
    } else {
        let mut dp = vec![0u32; (n + 1) * (m + 1)];
        let ix = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[ix(i, j)] = if am[i] == bm[j] { dp[ix(i + 1, j + 1)] + 1 } else { dp[ix(i + 1, j)].max(dp[ix(i, j + 1)]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] { ops.push((' ', p + i, p + j)); i += 1; j += 1; }
            else if j < m && (i == n || dp[ix(i, j + 1)] >= dp[ix(i + 1, j)]) { ops.push(('+', p + i, p + j)); j += 1; }
            else { ops.push(('-', p + i, p + j)); i += 1; }
        }
    }
    let (ea, eb) = (a.len() - s, b.len() - s);
    ops.extend((0..s).map(|k| (' ', ea + k, eb + k)));
    ops
}

/// changed hunks with `c` context lines; line numbers start at `ao` / `bo`
fn hunks(ops: &[(char, usize, usize)], a: &[&str], b: &[&str], ao: usize, bo: usize, c: usize) -> Vec<PL> {
    let ch: Vec<usize> = ops.iter().enumerate().filter(|o| o.1 .0 != ' ').map(|o| o.0).collect();
    if ch.is_empty() { return vec![PL::Note("no changes".into())]; }
    let (mut out, mut gap, mut k) = (vec![], false, 0);
    for (i, &(op, x, y)) in ops.iter().enumerate() {
        while k < ch.len() && ch[k] + c < i { k += 1; }
        let near = k < ch.len() && ch[k] <= i + c;
        if !near { if !gap && !out.is_empty() { out.push(PL::Gap); } gap = true; continue; }
        gap = false;
        if out.len() >= MAXN { out.push(PL::Note("… (diff cut)".into())); break; }
        out.push(match op { ' ' => PL::Ctx(bo + y, own(b[y])), '-' => PL::Del(ao + x, own(a[x])), _ => PL::Add(bo + y, own(b[y])) });
    }
    out
}

fn pl_line(p: &PL, nw: usize, w: usize) -> Line<'static> {
    let (no, sign, text, col) = match p {
        PL::Ctx(n, t) => (n.to_string(), ' ', t.as_str(), DIM),
        PL::Del(n, t) => (n.to_string(), '-', t.as_str(), RED),
        PL::Add(n, t) => (n.to_string(), '+', t.as_str(), GREEN),
        PL::Gap => (String::new(), '⋯', "", DIM),
        PL::Note(t) => return group(fit(cells(t, st(DIM).italic()), w, Style::new())),
    };
    let mut c = cells(&format!("{no:>nw$} "), st(GUTTER));
    c.extend(cells(&format!("{sign} "), st(col)));
    c.extend(cells(text, st(if matches!(p, PL::Ctx(..)) { CTX } else { col })));
    group(fit(c, w, Style::new()))
}

// ---------- entry point ----------
// ---- offscreen screen dump (screenshots without screen-recording permission) ----------------------------------------

/// `ELISA_SCREEN_DUMP=FILE`: after every draw that changed the screen, the frame is written to FILE as HTML (one
/// fixed-width cell per column, the real colors), atomically. A dev aid for screenshots (render the HTML with a
/// headless browser); off unless the variable is set.
struct ScreenDump { path: std::path::PathBuf, last: String }

impl ScreenDump {
    fn from_env() -> Option<ScreenDump> {
        std::env::var_os("ELISA_SCREEN_DUMP").filter(|p| !p.is_empty()).map(|p| ScreenDump { path: p.into(), last: String::new() })
    }
    fn write(&mut self, b: &Buffer) {
        let html = buffer_html(b);
        if html == self.last { return; }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &html).is_ok() { let _ = std::fs::rename(&tmp, &self.path); }
        self.last = html;
    }
}

fn css_color(c: Color, fg: bool) -> Option<String> {
    const ANSI: [(u8, u8, u8); 16] = [(0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
        (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255)];
    let (r, g, b) = match c {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) if n < 16 => ANSI[n as usize],
        Color::Indexed(n) if n >= 232 => { let v = 8 + 10 * (n - 232); (v, v, v) }
        Color::Indexed(n) => {
            let i = n - 16;
            let s = |x: u8| if x == 0 { 0 } else { 55 + 40 * x };
            (s(i / 36), s((i / 6) % 6), s(i % 6))
        }
        Color::Black => ANSI[0], Color::Red => ANSI[1], Color::Green => ANSI[2], Color::Yellow => ANSI[3], Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5], Color::Cyan => ANSI[6], Color::Gray => ANSI[7], Color::DarkGray => ANSI[8], Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10], Color::LightYellow => ANSI[11], Color::LightBlue => ANSI[12], Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14], Color::White => ANSI[15],
    };
    let _ = fg;
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// The buffer as a standalone HTML page (dark terminal colors; wide symbols span two cells).
fn buffer_html(b: &Buffer) -> String {
    let a = b.area;
    let mut s = format!("<!doctype html><meta charset=utf-8><style>body{{margin:0;background:{}}}#t{{font:15px/19px Menlo,monospace;color:#d4d4d4;\
        padding:8px;white-space:pre}}.r{{height:19px}}.c{{display:inline-block;width:9.03px;height:19px;overflow:visible;vertical-align:top}}.w{{width:18.06px}}</style><div id=t data-cols={} data-rows={}>",
        css_color(crate::brand::c(crate::brand::pal().bg), false).unwrap_or_default(), a.width, a.height);
    for y in a.top()..a.bottom() {
        s += "<div class=r>";
        let mut x = a.left();
        while x < a.right() {
            let c = &b[(x, y)];
            let sym = c.symbol();
            let w = Span::raw(sym).width().max(1);
            let m = c.modifier;
            let (mut fg, mut bg) = (css_color(c.fg, true), css_color(c.bg, false));
            if m.contains(Modifier::REVERSED) { std::mem::swap(&mut fg, &mut bg); fg.get_or_insert("#1e1e1e".into()); bg.get_or_insert("#d4d4d4".into()); }
            let mut st = String::new();
            if let Some(f) = &fg { st += &format!("color:{f};"); }
            if let Some(g) = &bg { st += &format!("background:{g};"); }
            if m.contains(Modifier::BOLD) { st += "font-weight:bold;"; }
            if m.contains(Modifier::DIM) { st += "opacity:.6;"; }
            if m.contains(Modifier::ITALIC) { st += "font-style:italic;"; }
            if m.contains(Modifier::UNDERLINED) { st += "text-decoration:underline;"; }
            // half blocks: paint the two pixels exactly (a font's block glyph may not fill the line box)
            let half = match sym { "▀" => Some((fg.clone(), bg.clone())), "▄" => Some((bg.clone(), fg.clone())), _ => None };
            if let Some((t, u)) = half.clone() {
                let d = css_color(crate::brand::c(crate::brand::pal().bg), false).unwrap_or_default();
                st += &format!("background:linear-gradient({} 50%,{} 50%);", t.unwrap_or(d.clone()), u.unwrap_or(d));
            }
            let esc = if half.is_some() { String::new() } else { sym.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") };
            s += &format!("<span class=\"c{}\" style=\"{st}\">{esc}</span>", if w > 1 { " w" } else { "" });
            x += w as u16;
        }
        s += "</div>";
    }
    s + "</div>"
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

#[cfg(test)]
mod watch_tests {
    use super::*;
    fn view(d: &str, gb: f64) -> WatcherView {
        let mut v = WatcherView { domain: d.into(), confidence: 0.86, since_turn: 12, note: "dry run".into(), ..Default::default() };
        v.plan.gb_unload = gb;
        v.plan.per_layer_cold = (0..64).map(|i| i as f64 / 63.0).collect();
        v
    }
    fn text(l: &Line) -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() }

    #[test]
    fn domain_change_line_and_hide() {
        let mut a = App::new(false);
        a.on_ev(Ev::Watch(WatchUpdate { view: Some(view("code-rust", 2.1)), changed: true, was: None, audits: vec![] }));
        assert!(a.watch.is_some() && !a.entries.iter().any(|i| matches!(i.e, Entry::Watch(_))), "first domain: line only");
        a.on_ev(Ev::Watch(WatchUpdate { view: Some(view("chat-vi", 1.8)), changed: true, was: Some("code-rust".into()), audits: vec![] }));
        let lines: Vec<&String> = a.entries.iter().filter_map(|i| match &i.e { Entry::Watch(s) => Some(s), _ => None }).collect();
        assert_eq!(lines, ["watcher → chat-vi (was code-rust) · would unload 1.8 GB"]);
        // three right-aligned rows: domain / since + unload / ms + dry run
        let rows: Vec<String> = watch_rows(a.watch.as_ref().unwrap(), 200).iter().map(|l| text(l).trim().to_string()).collect();
        assert_eq!(rows[0], "watcher: chat-vi 0.86");
        assert!(rows[1].starts_with("since turn 12 · would unload 1.8 GB"), "{rows:?}");
        assert!(rows.len() <= 3 && watch_rows(a.watch.as_ref().unwrap(), 200).iter().all(|l| l.alignment == Some(ratatui::layout::Alignment::Right)));
        a.on_ev(Ev::Watch(WatchUpdate::default()));
        assert!(a.watch.is_none(), "404: hidden");
    }

    #[test]
    fn full_view_renders_at_any_width() {
        for w in [8, 30, 120] {
            let mut out = vec![];
            render_entry(&Entry::WatchView(Box::new(view("code-rust", 2.1))), w, &mut out);
            assert!(out.iter().any(|l| text(l).contains("dry run")), "w={w}");
        }
    }

    fn st0() -> Status { Status { tok_s: Some(20.0), ttft_s: 0.5, ctx_used: 900, max_ctx: 4096, reused: 0, output_tokens: 40, estimated: false } }
    /// a reply drawn on an 80x24 screen; the agent waits for input on the returned channel
    fn replied() -> (App, mpsc::Receiver<Option<String>>) {
        let mut a = App::new(false);
        a.on_ev(Ev::Delta("The answer.".into()));
        a.on_ev(Ev::Done(st0()));
        let (tx, rx) = mpsc::channel();
        a.on_ev(Ev::Input(tx));
        draw(&mut a);
        (a, rx)
    }
    fn draw(a: &mut App) {
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| a.draw(f)).unwrap();
    }

    #[test]
    fn ctx_and_stats_right_aligned_under_input() {
        let (mut a, _rx) = replied();
        a.sum_in = 139_200; a.sum_out = 7_500;
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| a.draw(f)).unwrap();
        let b = t.backend().buffer();
        let rows: Vec<String> = (0..24).map(|y| (0..80).map(|x| b[(x, y)].symbol()).collect()).collect();
        let find = |s: &str| rows.iter().position(|r| r.contains(s)).unwrap_or_else(|| panic!("{s} missing: {rows:#?}"));
        let (bar, ctx, stats) = (find("commands"), find("ctx "), find("Σ in"));
        assert!(!rows[bar].contains("ctx ") && !rows[bar].contains("Σ"), "status bar keeps no ctx / Σ: {}", rows[bar]);
        assert!(bar < ctx && ctx + 1 == stats, "{rows:#?}");
        // the input box (top + bottom border) sits between the status bar and the ctx line
        assert!(rows[ctx - 1].starts_with('─'), "{rows:#?}");
        // right-aligned: text ends one column before the edge, starts well right of the left edge
        for r in [ctx, stats] {
            assert_eq!(rows[r].trim_end().chars().count(), 79, "{:?}", rows[r]);
            assert!(rows[r].starts_with("          "), "{:?}", rows[r]);
        }
    }

    fn screen(a: &mut App) -> Vec<String> {
        // the test binary's own RSS (many tests in one process) must not put the memory warning on the status line
        (a.rss, a.rss_at) = (0, Some(Instant::now()));
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| a.draw(f)).unwrap();
        let b = t.backend().buffer();
        (0..24).map(|y| (0..80).map(|x| b[(x, y)].symbol()).collect()).collect()
    }

    #[test]
    fn compaction_cleans_screen_and_ctx_bar() {
        let mut a = App::new(false);
        for t in ["first question", "second question", "third question"] {
            a.on_ev(Ev::User(t.into()));
            a.on_ev(Ev::Delta(format!("answer to {t}")));
            a.on_ev(Ev::Done(st0()));
        }
        a.on_ev(Ev::Compacted(3_900, 1_200));
        assert_eq!(a.status.map(|s| s.ctx_used), Some(1_200));
        let rows = screen(&mut a).join("\n");
        assert!(!rows.contains("first question") && !rows.contains("second question"), "{rows}");
        assert!(rows.contains("third question") && rows.contains("earlier turns summarised"), "{rows}");
        assert!(rows.contains("ctx 1.2K/4.1K"), "{rows}");
    }

    #[test]
    fn summary_pauses_input_and_permission_hides_it() {
        let (mut a, _rx) = replied();
        // summary running: keys and paste do not type, the box says it is paused; ↑ still scrolls
        a.on_ev(Ev::Progress(Some(Progress { label: "summarizing turns 1–4".into(), frac: Some(0.3), ..Default::default() })));
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        a.paste("pasted");
        assert!(a.input.is_empty());
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(a.scroll, 3);
        assert!(screen(&mut a).iter().any(|r| r.contains("input paused")));
        // summary done: typing works again
        a.on_ev(Ev::Progress(Some(Progress { label: "summarizing turns 1–4".into(), frac: Some(1.0), done: Some("saved summary".into()), ..Default::default() })));
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(a.input, vec!['x']);
        // permission box: no input field (no prompt row, no box borders under the status bar)
        let (tx, _arx) = mpsc::channel();
        a.on_ev(Ev::Approve("bash".into(), "run cargo test".into(), tx));
        let rows = screen(&mut a);
        let bar = rows.iter().position(|r| r.contains("ctrl-c quit") || r.contains("commands")).unwrap();
        assert!(!rows.iter().any(|r| r.starts_with('›')), "{rows:#?}");
        assert!(!rows[bar + 1].starts_with('─'), "input box still drawn: {rows:#?}");
    }

    #[test]
    fn outside_permission_box_shows_paths_and_session_choice() {
        let (mut a, _rx) = replied();
        let (tx, arx) = mpsc::channel();
        a.on_ev(Ev::Approve(format!("{OUTSIDE}write"), "write ../other/x.txt (1 lines)\noutside the project · write · /tmp/other/x.txt".into(), tx));
        let rows = screen(&mut a);
        assert!(rows.iter().any(|r| r.contains("⚠ outside the project · write · /tmp/other/x.txt")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("1. Yes, once")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("2. Yes, allow these paths for this session")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("always allow")), "{rows:#?}");
        a.on_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(arx.recv().unwrap(), Approval::Always);
    }

    #[test]
    fn mcp_permission_box_names_the_server() {
        let (mut a, _rx) = replied();
        let (tx, _arx) = mpsc::channel();
        a.on_ev(Ev::Approve(format!("{MCP}fake\u{1}fake_greet"), "fake_greet {}".into(), tx));
        let rows = screen(&mut a);
        assert!(rows.iter().any(|r| r.contains("2. Yes, always allow MCP server fake in this project")), "{rows:#?}");
    }

    #[test]
    fn a_rule_choice_and_a_question_box() {
        let (mut a, _rx) = replied();
        let (tx, arx) = mpsc::channel();
        a.on_ev(Ev::Approve(format!("bash{RULE}shell(cargo test *)"), "bash cargo test\nasked by nothing".into(), tx));
        let rows = screen(&mut a);
        assert!(rows.iter().any(|r| r.contains("3. Yes, and always allow shell(cargo test *)")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("4. No")), "{rows:#?}");
        a.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(arx.recv().unwrap(), Approval::Rule);
        let (tx, arx) = mpsc::channel();
        a.on_ev(Ev::Approve(format!("{QUESTION}plugin add notes"), "Install plugin notes 1.0.0?".into(), tx));
        let rows = screen(&mut a);
        assert!(rows.iter().any(|r| r.contains("Install plugin notes 1.0.0?")) && rows.iter().any(|r| r.contains("2. No")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("always")), "{rows:#?}");
        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(arx.recv().unwrap(), Approval::No);
    }

    #[test]
    fn plain_footer_keys_scroll_and_type() {
        let (mut a, rx) = replied();
        let mut out = vec![];
        let f = a.entries.iter().find(|i| matches!(i.e, Entry::Foot(_))).unwrap();
        render_entry(&f.e, 80, &mut out);
        assert_eq!(out.len(), 1);
        assert!(text(&out[0]).starts_with("  in 860 · out 40 · ") && text(&out[0]).trim_end() == text(&out[0]), "{:?}", text(&out[0]));
        // shift-↑↓: three lines
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(a.scroll, 3);
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(a.scroll, 0);
        // captured mouse wheel: scrolls the chat, never the input
        a.wheel(MouseEventKind::ScrollUp);
        assert_eq!((a.scroll, a.input.is_empty()), (3, true));
        a.wheel(MouseEventKind::ScrollDown);
        a.wheel(MouseEventKind::ScrollDown);
        assert_eq!(a.scroll, 0);
        // plain ↑↓ with an empty input = the wheel in Terminal.app: scroll, input untouched; ctrl-p/n = history
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!((a.scroll, a.input.is_empty()), (3, true));
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(a.scroll, 0);
        a.hist = vec!["earlier".into()];
        a.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(a.input.iter().collect::<String>(), "earlier");
        a.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(a.input.is_empty() && a.hist_i.is_none());
        // no key shortcuts: + and - type
        for c in ['+', '-'] { a.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)); }
        assert_eq!(a.input, vec!['+', '-']);
        assert!(rx.try_recv().is_err());
        a.input = vec!['/'];
        let m: Vec<&str> = a.menu().iter().map(|c| c.0).collect();
        assert!(m.contains(&"/full") && m.contains(&"/copy") && !m.contains(&"/like"), "{m:?}");
    }

    #[test]
    fn mouse_capture_on_and_released() {
        // the wheel scrolls the chat, never the terminal's scrollback: setup captures the mouse, restore releases it
        let src = include_str!("tui.rs");
        for s in [concat!("Bracketed", "Paste, Enable", "MouseCapture)"), concat!("Disable", "MouseCapture, Disable"), concat!("Event::", "Mouse(me)) => app.mouse")] {
            assert!(src.contains(s), "{s}");
        }
    }

    #[test]
    fn audit_lines_and_full_box() {
        let (mut a, _rx) = replied();
        let au = |r: &str, c: &str| crate::watcher::Audit { message_id: "msg_3".into(), result: r.into(), cause: c.into(), tokens: 312, diverge_at: Some(87), ..Default::default() };
        a.on_ev(Ev::Watch(WatchUpdate { view: Some(view("code-rust", 1.0)), audits: vec![au("match", ""), au("mismatch", "mask"), au("mismatch", "model")], ..Default::default() }));
        let n = a.entries.len();
        assert_eq!(a.audit.as_deref(), Some("audit ✗ masked weights changed this reply at token 87 · profile tightened"), "latest shown audit under the input");
        a.on_ev(Ev::Watch(WatchUpdate { view: Some(view("code-rust", 1.0)), audits: vec![au("mismatch", "mask")], ..Default::default() }));
        assert_eq!(a.audit.as_deref(), Some("audit ✗ masked weights changed this reply at token 87 · profile tightened"));
        assert_eq!(a.entries.len(), n, "audits stay out of the transcript");
        draw(&mut a);
        a.on_ev(Ev::Full("full model · code-rust".into(), "# Title\n- **bold** item".into()));
        let mut out = vec![];
        render_entry(&a.entries.last().unwrap().e, 60, &mut out);
        let all: Vec<String> = out.iter().map(text).collect();
        assert!(all[1].starts_with("╭─ full model · code-rust") && all.iter().any(|l| l.contains("Title")) && all.iter().any(|l| l.contains("bold item")), "{all:?}");
        assert!(!all.iter().any(|l| l.contains("**")), "markdown rendered: {all:?}");
    }

    // ---------- the companion ----------
    fn shaded() -> App { App::new(false).with_shade(45_000) }
    fn st_now(a: &mut App) -> St { let n = a.now(); a.shade.as_mut().unwrap().view(n).st }

    #[test]
    fn app_events_drive_the_companion() {
        let mut a = shaded();
        a.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        assert_eq!(st_now(&mut a), St::Greeting);
        a.on_ev(Ev::Waiting);
        a.on_ev(Ev::ToolStart("bash cargo test --workspace".into()));
        let (tx, _rx) = mpsc::channel();
        a.on_ev(Ev::Approve("bash".into(), "bash cargo test --workspace".into(), tx));
        assert_eq!(st_now(&mut a), St::Permission, "permission cuts the greeting short");
        let rows = screen_wide(&mut a, 120, 36).join("\n");
        assert!(rows.contains("NEEDS YOU cargo test") && rows.contains("waiting for permission"), "{rows}");
        a.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(st_now(&mut a), St::Testing);
        a.on_ev(Ev::ToolDone("test result: ok\n[exit 0]".into(), false));
        a.on_ev(Ev::Error("unknown command /frob (try /help)".into()));
        assert_eq!(st_now(&mut a), St::Confused);
        // a bash exit code ≠ 0 is a failed tool even when the tool itself reports no error
        let mut b = shaded();
        b.on_ev(Ev::ToolStart("bash cargo run".into()));
        b.on_ev(Ev::ToolDone("error: no bin target\n[exit 101]".into(), false));
        assert_eq!(st_now(&mut b), St::Error);
        // esc while busy: interrupted
        let mut c = shaded();
        c.on_ev(Ev::Waiting);
        c.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(st_now(&mut c), St::Interrupted);
    }

    #[test]
    fn care_controls_click_keys_and_commands() {
        let mut a = shaded();
        let (tx, rx) = mpsc::channel();
        a.on_ev(Ev::Input(tx));
        let rows = screen_wide(&mut a, 120, 36).join("\n");
        assert!(rows.contains("ctrl-g to poke") || !rows.contains("ELISA"), "{rows}");
        let d = a.dock.expect("dock drawn at 120 columns");
        let click = |x: u16, y: u16| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
        // a click beside the dock does nothing; a click on Shade pokes
        a.mouse(click(d.x.saturating_sub(2), d.y + d.height / 2));
        assert_ne!(st_now(&mut a), St::Poked);
        a.mouse(click(d.x + d.width / 2, d.y + d.height / 2));
        assert_eq!(st_now(&mut a), St::Poked);
        // ctrl-g pokes; F2 / F3 / F4 = water / food / toy; none of them touch the input box
        a.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(st_now(&mut a), St::Poked);
        for (k, st) in [(2, St::Drinking), (3, St::Eating), (4, St::Playing)] {
            a.on_key(KeyEvent::new(KeyCode::F(k), KeyModifiers::NONE));
            assert_eq!(st_now(&mut a), st, "F{k}");
        }
        assert!(a.input.is_empty());
        // /water /feed /play /poke are the TUI's own: the agent never receives them
        for (cmd, st) in [("/water", St::Drinking), ("/feed", St::Eating), ("/play", St::Playing), ("/poke", St::Poked)] {
            for ch in cmd.chars() { a.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)); }
            a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert_eq!(st_now(&mut a), st, "{cmd}");
            assert!(a.input.is_empty() && matches!(a.pending, Some(Pending::Input(_))), "{cmd} stays at the prompt");
        }
        assert!(rx.try_recv().is_err(), "nothing was sent to the agent");
        // the care tray (layout.json careTray): three buttons left of Shade, water / food / toy top to bottom
        screen_wide(&mut a, 120, 36);
        let tray = a.tray.clone();
        assert_eq!(tray.iter().map(|t| t.1).collect::<Vec<_>>(), [St::Drinking, St::Eating, St::Playing]);
        assert!(tray.iter().all(|(r, _)| r.right() < d.x && r.y < d.bottom() && r.y >= d.y), "{tray:?} left of {d:?}");
        for (r, st) in tray {
            a.mouse(click(r.x + 1, r.y));
            assert_eq!(st_now(&mut a), st);
        }
        let rows = screen_wide(&mut a, 120, 36).join("\n");
        assert!(rows.contains(" ≈ ") && rows.contains(" ∪ ") && rows.contains(" @ "), "{rows}");
        // the wheel still scrolls
        a.mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: 0, row: 0, modifiers: KeyModifiers::NONE });
        assert_eq!(a.scroll, 3);
        // character off: a care action only says so
        let mut b = App::new(false);
        b.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(b.flash.as_ref().is_some_and(|(s, _)| s.contains("character is off")));
    }

    fn screen_wide(a: &mut App, w: u16, h: u16) -> Vec<String> {
        (a.rss, a.rss_at) = (0, Some(Instant::now()));
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        t.draw(|f| { a.draw(f); crate::brand::finish(f.buffer_mut(), &a.keep()); }).unwrap();
        let b = t.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| b[(x, y)].symbol()).collect()).collect()
    }

    #[test]
    fn dock_sizes_and_switches_off() {
        let mut a = shaded();
        a.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        let (tx, _rx) = mpsc::channel();
        a.on_ev(Ev::Input(tx));
        for (w, h, size) in [(190u16, 44u16, Some(32u16)), (160, 40, Some(24)), (120, 36, Some(24)), (80, 24, Some(16)), (50, 24, None), (120, 14, None)] {
            let rows = screen_wide(&mut a, w, h);
            assert_eq!(a.dock.map(|d| d.width), size, "{w}x{h}");
            // the input box is always there
            assert!(rows.iter().any(|r| r.starts_with('›')), "{w}x{h}: input visible");
            if a.dock.is_some() {
                assert!(rows.join("\n").contains("HI!"), "{w}x{h}: greeting bubble");
            }
        }
        // character off (config `character = false` / --no-character): no dock, the transcript keeps the full width
        let mut b = App::new(false);
        b.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        screen_wide(&mut b, 160, 40);
        assert!(b.dock.is_none());
    }

    #[test]
    fn companion_window_hides_the_dock() {
        use crate::shade::link::{decode, encode_care, Care, Link};
        use std::io::{BufRead, Write};
        let p = std::env::temp_dir().join(format!("elisa-tui-link-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut a = shaded();
        a.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        let (tx, _rx) = mpsc::channel();
        a.on_ev(Ev::Input(tx));
        // --companion with no window running: the terminal dock as before
        a.link = Some(Link::new(&p));
        assert!(screen_wide(&mut a, 160, 40).join("\n").contains("HI!") && a.dock.is_some());
        // the window is up: no dock, no terminal bubble, the input stays; the state goes to the window
        let srv = std::os::unix::net::UnixListener::bind(&p).unwrap();
        a.link = Some(Link::new(&p));
        let rows = screen_wide(&mut a, 160, 40);
        assert!(a.dock.is_none() && a.tray.is_empty(), "dock hidden");
        assert!(!rows.join("\n").contains("HI!"), "no terminal bubble");
        assert!(rows.iter().any(|r| r.starts_with('›')), "input visible");
        let (s, _) = srv.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut r = std::io::BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("hello"), "{line}");
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(decode(&line).map(|f| f.state), Some(St::Greeting));
        // a click on Shade in the window pokes her here too
        (&s).write_all(encode_care(Care::Poke).as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        screen_wide(&mut a, 160, 40);
        assert_eq!(st_now(&mut a), St::Poked);
        // the window quits: the dock comes back
        drop(r);
        drop(s);
        screen_wide(&mut a, 160, 40);
        screen_wide(&mut a, 160, 40);
        assert!(a.dock.is_some(), "dock back when the window is gone");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn status_face_layout_and_fallback() {
        let mut a = shaded();
        a.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        let (tx, _rx) = mpsc::channel();
        a.on_ev(Ev::Input(tx));
        // tall: 10 x 5 face, padding row above and below, text centred beside it; 30+: 8 x 4; shorter: one text line
        for (h, size) in [(44u16, Some((10u16, 5u16))), (40, Some((10, 5))), (36, Some((8, 4))), (30, Some((8, 4))), (29, None), (20, None)] {
            let rows = screen_wide(&mut a, 120, h);
            assert_eq!(a.face.map(|r| (r.width, r.height)), size, "120x{h}");
            let inp = rows.iter().position(|r| r.starts_with('›')).unwrap_or_else(|| panic!("120x{h}: input visible"));
            let text = rows.iter().position(|r| r.contains("● ready")).unwrap_or_else(|| panic!("120x{h}: status text"));
            match a.face {
                Some(f) => {
                    assert_eq!(text as u16, f.y + f.height / 2, "120x{h}: text centred on the face");
                    assert!(rows[text].chars().position(|c| c == '●').unwrap() as u16 > f.right(), "text right of the face");
                    // one blank padding row under the face, then the input box's top border
                    assert_eq!(f.bottom() + 2, inp as u16, "120x{h}: padding below");
                    assert!(rows[f.y as usize - 1].trim().is_empty() || a.dock.is_some(), "120x{h}: padding above");
                }
                None => assert_eq!(text + 2, inp, "120x{h}: one status line right above the input box"),
            }
            // the hero view keeps rows: the transcript shows the banner
            assert!(rows.iter().take(text).any(|r| r.contains("ELISA")), "120x{h}: transcript visible");
        }
        // --no-character: no face either
        let mut b = App::new(false);
        b.on_ev(Ev::Banner(Banner { model: "m".into(), ..Default::default() }));
        let (tx, _rx) = mpsc::channel();
        b.on_ev(Ev::Input(tx));
        screen_wide(&mut b, 120, 44);
        assert!(b.face.is_none() && b.dock.is_none());
    }

    #[test]
    fn brand_look_has_no_claude_marks() {
        let mut a = shaded();
        a.on_ev(Ev::Banner(Banner { model: "m".into(), base_url: "http://x".into(), cwd: "/p".into(), max_ctx: 8192, ..Default::default() }));
        a.on_ev(Ev::ToolStart("read src/a.rs".into()));
        a.on_ev(Ev::ToolDone("1\tx\n[lines 1-1 of 1]".into(), false));
        a.on_ev(Ev::Delta("Done.".into()));
        a.on_ev(Ev::Done(st0()));
        let rows = screen_wide(&mut a, 120, 36).join("\n");
        assert!(!rows.contains('✻') && !rows.contains('⎿') && !rows.contains("● Read"), "{rows}");
        assert!(rows.contains("ELISA") && rows.contains("OK   Read src/a.rs") && rows.contains("↳ lines"), "{rows}");
        // colours: brand pink accent, no orange
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        t.draw(|f| a.draw(f)).unwrap();
        let buf = t.backend().buffer();
        let all: Vec<Color> = buf.content.iter().flat_map(|c| [c.fg, c.bg]).collect();
        assert!(all.contains(&ACCENT) && !all.contains(&Color::Rgb(215, 119, 87)));
        assert!(buf.content.iter().all(|c| c.bg != Color::Reset), "the brand background is painted everywhere");
    }
}

#[cfg(test)]
mod edit_preview_tests {
    use super::*;

    /// soak M2: old ends in a newline, new does not: the file gets two lines joined, and the preview must say so
    #[test]
    fn preview_shows_joined_lines() {
        let text = "def t():\n    a = 1\n    assert a\n    b = 2\n";
        let body = edit_body(text, "    assert a\n", "    assert a", false);
        assert!(body.contains(&PL::Add(3, "    assert a    b = 2".into())), "{body:?}");
        assert!(body.contains(&PL::Del(3, "    assert a".into())) && body.contains(&PL::Del(4, "    b = 2".into())), "{body:?}");
    }

    #[test]
    fn preview_matches_written_text() {
        let text = "a\nb\nc\n";
        let body = edit_body(text, "b", "b1\nb2", false);
        assert!(body.contains(&PL::Add(2, "b1".into())) && body.contains(&PL::Add(3, "b2".into())), "{body:?}");
        let tail = edit_body("a\nb\n", "b\n", "b", false);
        assert!(matches!(&tail[..], [PL::Note(t)] if t.contains("line endings")), "{tail:?}");
        let two = edit_body("x\ny\nx\n", "x", "z", true);
        assert!(two.contains(&PL::Add(1, "z".into())) && two.contains(&PL::Add(3, "z".into())), "{two:?}");
    }
}
