//! Shade, Elisa's companion (brand kit 05-character + 07-motion), drawn in the terminal with truecolor half blocks.
//!
//! - `state_for` / `tool_state`: the app event → state table of `07-motion/state-mapping.md` (pure, unit tested).
//! - `Companion`: the live controller. A continuous *base* state (thinking, streaming, a tool, permission, idle) plus
//!   one-shot *flashes* that play one loop (greeting, success, celebrating, error, confused, interrupted, memory,
//!   waking) and the user's care actions (poked, drinking, eating, playing: kit v2), the idle → sleeping timeout, the attention pop (1.22x spring) and the bubble copy (VERB + target).
//! - `render`: one frame as terminal cells, a pure function of (state, phase, size, pop, theme).
//!
//! Time is passed in as milliseconds so everything here is deterministic.
pub mod draw;
/// the native companion window (app/ElisaCompanion) over a Unix socket
pub mod link;
pub mod motion;
pub mod raster;
mod tables;

pub use motion::St;
use raster::{mix, Canvas, Rgb, Tf};
use std::collections::VecDeque;

/// terminal frame rate of the animation: ~12 fps
pub const FRAME_MS: u64 = 83;

/// An app event, the left column of state-mapping.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// launch, or a click on Elisa
    Launch,
    /// the user is typing at the prompt
    Typing,
    /// a model request started (reasoning until the first token)
    Waiting,
    /// tokens are streaming
    Delta,
    /// a tool call starts: tool name + its target (path, command or pattern)
    Tool(&'a str, &'a str),
    /// a Write/Exec tool waits for approval
    Permission,
    /// the tool finished; true = it failed
    ToolDone(bool),
    /// the reply finished; true = tools ran in this turn
    Finished(bool),
    UnknownCommand,
    /// an error shown to the user (tool failure, API error)
    Failed,
    /// Esc: the user interrupted
    Esc,
    /// memories were recalled for this turn
    Recalled,
    /// a summary of earlier turns is running
    Summarizing,
    /// idle timeout reached / the model's weights were moved to disk
    IdleTimeout,
    ModelUnloaded,
    /// the model is coming back into memory
    ModelReloading,
    /// care (kit v2): the user pokes Elisa (a click on the dock, or a key)
    Poke,
    /// care: the user gives water / food / a toy
    Water,
    Food,
    Toy,
}

/// the tool → action state rule (prototype `toolAct`): read → reading; grep/glob/search → searching; edit/write →
/// editing; a test command → testing; build/check/clippy/compile → building; anything else → running
pub fn tool_state(name: &str, target: &str) -> St {
    let t = target.to_ascii_lowercase();
    let word = |w: &str| t.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_').any(|x| x == w || x.starts_with(w));
    match name {
        "read" | "outline" | "read_symbol" => St::Reading,
        "edit" | "write" | "edit_symbol" => St::Editing,
        "grep" | "glob" => St::Searching,
        _ if ["grep", "rg", "find", "fd", "glob", "search"].iter().any(|w| t.split_whitespace().next() == Some(w)) => St::Searching,
        _ if word("test") || t.contains("nextest") || t.contains("pytest") => St::Testing,
        _ if word("build") || word("check") || word("clippy") || word("compile") || t.starts_with("make") => St::Building,
        _ => St::Running,
    }
}

/// The state an event shows (state-mapping.md).
pub fn state_for(ev: Event) -> St {
    match ev {
        Event::Launch => St::Greeting,
        Event::Typing => St::Listening,
        Event::Waiting => St::Thinking,
        Event::Delta => St::Streaming,
        Event::Tool(n, t) => tool_state(n, t),
        Event::Permission => St::Permission,
        Event::ToolDone(true) | Event::Failed => St::Error,
        Event::ToolDone(false) => St::Thinking,
        Event::Finished(false) => St::Success,
        Event::Finished(true) => St::Celebrating,
        Event::UnknownCommand => St::Confused,
        Event::Esc => St::Interrupted,
        Event::Recalled => St::Memory,
        Event::Summarizing => St::Reading,
        Event::IdleTimeout | Event::ModelUnloaded => St::Sleeping,
        Event::ModelReloading => St::Waking,
        Event::Poke => St::Poked,
        Event::Water => St::Drinking,
        Event::Food => St::Eating,
        Event::Toy => St::Playing,
    }
}

/// bubble verb (prototype `WORD`); idle, sleeping and listening show no bubble (unless the controller names the
/// activity: `WAITING`)
pub fn verb(st: St) -> &'static str {
    match st {
        St::Idle | St::Sleeping | St::Listening => "",
        St::Thinking => "THINKING", St::Streaming => "WRITING", St::Running => "RUNNING", St::Permission => "NEEDS YOU",
        St::Success => "DONE", St::Error => "ERROR", St::Interrupted => "STOPPED", St::Memory => "REMEMBERED",
        St::Waking => "WAKING UP", St::Greeting => "HI!", St::Reading => "READING", St::Searching => "LOOKING FOR",
        St::Editing => "EDITING", St::Building => "BUILDING", St::Testing => "TESTING", St::Celebrating => "ALL DONE!",
        St::Confused => "HUH?",
        St::Poked => "HEY!", St::Drinking => "GLUG", St::Eating => "NOM NOM", St::Playing => "PLAY",
    }
}

/// a care state's bubble detail and how long it plays (prototype `dockClick` / `giveWater` / `giveFood` / `giveToy`)
pub fn care_of(st: St) -> (&'static str, u64) {
    match st {
        St::Poked => ("that tickles", 1400),
        St::Drinking => ("water", 4800),
        St::Eating => ("snack", 4800),
        St::Playing => ("yarn ball", 6000),
        _ => ("", st.dur_ms() as u64),
    }
}

/// the bubble's verb colour role: error / permission (warn) / accent
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone { Accent, Warn, Error }

#[derive(Clone, Debug, PartialEq)]
pub struct Bubble { pub verb: &'static str, pub target: String, pub tone: Tone, pub big: bool }

/// the most characters a path keeps in the bubble before it is shortened from the left
pub const PATH_MAX: usize = 28;
/// the most characters a command or pattern keeps in the bubble (the bubble wraps; this only bounds a huge command)
pub const CMD_MAX: usize = 60;

/// a path shortened from the left on whole components: `crates/core/src/split.rs` → `…/core/src/split.rs` (at most
/// `max` chars; a file name longer than that keeps its last `max - 1` chars)
pub fn short_path(p: &str, max: usize) -> String {
    if p.chars().count() <= max { return p.to_string(); }
    let parts: Vec<&str> = p.split('/').collect();
    let mut out = String::new();
    for c in parts.iter().rev() {
        let next = if out.is_empty() { c.to_string() } else { format!("{c}/{out}") };
        if next.chars().count() + 2 > max { break; }
        out = next;
    }
    if out.is_empty() {
        let tail: String = { let v: Vec<char> = p.chars().collect(); v[v.len().saturating_sub(max.saturating_sub(1))..].iter().collect() };
        return format!("…{tail}");
    }
    format!("…/{out}")
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_string(); }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// the number of tests a test command names, when it names them (pytest node ids / files, `cargo test a b`)
fn named_tests(cmd: &str) -> Option<usize> {
    let w: Vec<&str> = cmd.split_whitespace().collect();
    let i = w.iter().position(|x| *x == "test" || x.ends_with("pytest") || *x == "nextest")?;
    let names = w[i + 1..].iter().take_while(|x| **x != "--").filter(|x| !x.starts_with('-') && **x != "run").count();
    (names > 0).then_some(names)
}

/// the tests a finished test run reports passing (`test result: ok. 3 passed`, pytest `3 passed`, `Ran 3 tests`)
pub fn passed_tests(out: &str) -> Option<usize> {
    let num_before = |s: &str, word: &str| -> Option<usize> {
        let mut total = None;
        for (i, _) in s.match_indices(word) {
            let n: String = s[..i].trim_end().chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
            if let Ok(n) = n.parse::<usize>() { total = Some(total.unwrap_or(0) + n); }
        }
        total
    };
    if out.contains("test result:") {
        return out.lines().filter(|l| l.contains("test result:")).map(|l| num_before(l, " passed").unwrap_or(0)).sum::<usize>().into();
    }
    num_before(out, " passed").or_else(|| out.lines().find_map(|l| l.trim().strip_prefix("Ran ")?.split_whitespace().next()?.parse().ok()))
}

/// the target shown in the bubble for a tool: a path (shortened from the left) with its symbol (`split.rs › split_words`),
/// the pattern looked for, `3 tests` for a test command that names them, nothing for a build, else the command
pub fn short_target(name: &str, target: &str) -> String {
    let t = target.split_whitespace().collect::<Vec<_>>().join(" ");
    let w: Vec<&str> = t.split(' ').filter(|x| !x.is_empty()).collect();
    let path = |i: usize| w.get(i).map(|p| short_path(p, PATH_MAX)).unwrap_or_default();
    match name {
        "read" | "write" | "edit" | "outline" => path(0),
        "read_symbol" | "edit_symbol" => match w.get(1) { Some(sym) if !sym.starts_with('(') => format!("{} › {sym}", path(0)), _ => path(0) },
        "grep" | "glob" => cap_chars(&t, CMD_MAX),
        _ => match tool_state(name, target) {
            St::Building => String::new(),
            St::Testing => match named_tests(&t) { Some(n) => format!("{n} test{}", if n == 1 { "" } else { "s" }), None => cap_chars(t.strip_prefix("check ").unwrap_or(&t), CMD_MAX) },
            // `rg create_item src` → `create_item`: the pattern, not the command
            St::Searching => cap_chars(w.iter().skip(1).find(|x| !x.starts_with('-')).copied().unwrap_or(&t), CMD_MAX),
            _ => cap_chars(&t, CMD_MAX),
        },
    }
}

/// what to draw now
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub st: St, pub ms: u64, pub pop: f32, pub bubble: Option<Bubble>,
    /// a long tool (past `FIDGET_AFTER_MS`): an occasional idle fidget plays now (displays draw the `listening` pose; a
    /// display with Reduce Motion keeps `st`)
    pub fidget: bool,
}

/// a tool shows at least this long, even when it finished at once (a read takes a few ms: Shade would only ever say
/// THINKING); a new event cuts it short
pub const MIN_TOOL_MS: u64 = 1200;
/// the gateway queue / prefill show once they last this long (a local gateway starts in ms: no one-frame flicker)
pub const PHASE_SHOW_MS: u64 = 250;
/// a tool running this long starts fidgeting, `FIDGET_MS` every `FIDGET_EVERY_MS`, so Shade never looks frozen
pub const FIDGET_AFTER_MS: u64 = 20_000;
pub const FIDGET_EVERY_MS: u64 = 9_000;
pub const FIDGET_MS: u64 = 2_000;
/// the brief reactions after a check / test / build: happy, or worried (then back to work)
pub const HAPPY_MS: u64 = 1_100;
pub const WORRIED_MS: u64 = 1_500;

/// The live controller. Feed it events with `on`, ask `view(now)` every frame.
pub struct Companion {
    base: St,
    base_detail: String,
    flashes: VecDeque<(St, String, u64)>,
    /// the running flash: (state, detail, start ms, duration ms)
    flash: Option<(St, String, u64, u64)>,
    /// tools run in this turn; the last tool's state; a test/build that passed in this turn
    tools: usize, last_tool: St, last_detail: String, passed: Option<St>,
    /// a model reply arrived in this turn, and whether it ended badly (error, Esc)
    replied: bool, failed: bool,
    /// when the base state was set (the fidget clock), and when the last tool started (the minimum show)
    base_at: u64, tool_at: u64,
    /// a tool that finished before `MIN_TOOL_MS`: shown until this time unless something else happens
    hold: Option<(St, String, u64)>,
    /// the gateway's phase for the running model call (`api::phase`) and when it was first seen
    gw: crate::api::Phase, gw_seq: u64, gw_at: u64,
    /// the tool call the model is writing (`Phase::Call`): name, path
    call: (String, String),
    pub sleep_after_ms: Option<u64>,
    last_act: u64,
    unloaded: bool,
    // what view() last returned: for the phase clock and the pop spring
    shown: St, shown_at: u64, pop_from: f32, pop_to: f32, pop_at: u64,
}

const POP: f32 = 1.22;
const POP_MS: u64 = 350;

impl Companion {
    pub fn new(now: u64, sleep_after_ms: Option<u64>) -> Companion {
        Companion {
            base: St::Idle, base_detail: String::new(), flashes: VecDeque::new(), flash: None, tools: 0, last_tool: St::Running, last_detail: String::new(),
            passed: None, replied: false, failed: false, base_at: now, tool_at: now, hold: None, gw: crate::api::Phase::Idle, gw_seq: 0, gw_at: now,
            call: (String::new(), String::new()), sleep_after_ms, last_act: now, unloaded: false,
            shown: St::Idle, shown_at: now, pop_from: 1.0, pop_to: 1.0, pop_at: now,
        }
    }

    fn flash(&mut self, st: St, detail: impl Into<String>, ms: u64) { self.flashes.push_back((st, detail.into(), ms)); }
    fn flash_now(&mut self, now: u64, st: St, detail: impl Into<String>) {
        self.flashes.clear();
        self.flash = Some((st, detail.into(), now, st.dur_ms() as u64));
    }
    fn base(&mut self, st: St, detail: impl Into<String>) { self.base = st; self.base_detail = detail.into(); self.hold = None; }
    /// the base, with its clock (fidget) restarted when the state changes
    fn base_t(&mut self, now: u64, st: St, detail: impl Into<String>) {
        if st != self.base { self.base_at = now; }
        self.base(st, detail);
    }
    /// an agent reaction (success, error, memory…) still playing gives way to a new activity; care actions stay
    fn cut_reaction(&mut self) {
        if self.flash.as_ref().is_some_and(|f| !f.0.care()) { self.flash = None; }
        self.flashes.retain(|f| f.0.care());
    }

    /// something the user did: resets the idle clock and wakes Shade (waking, then greeting, as in the prototype)
    pub fn activity(&mut self, now: u64) {
        let asleep = self.current(now).0 == St::Sleeping;
        // never backwards: a running care action has pushed the idle clock to its end
        self.last_act = self.last_act.max(now);
        if asleep {
            self.unloaded = false;
            self.flash_now(now, St::Waking, "");
            self.flash(St::Greeting, "ready when you are", St::Greeting.dur_ms() as u64);
        }
    }

    /// the prompt's input box became empty / non-empty while the agent is idle
    pub fn typing(&mut self, now: u64, nonempty: bool) {
        self.activity(now);
        if matches!(self.base, St::Idle | St::Listening) { self.base(if nonempty { St::Listening } else { St::Idle }, ""); }
    }

    pub fn on(&mut self, now: u64, ev: Event) {
        let st = state_for(ev);
        match ev {
            // the prototype greets for 2.2 s at launch
            Event::Launch => { self.last_act = now; self.flashes.clear(); self.flash = Some((st, "ready when you are".into(), now, 2200)); }
            Event::Typing => self.typing(now, true),
            Event::Waiting => {
                self.activity(now);
                // a tool that just finished keeps its minimum show (the hold survives the new model call)
                let hold = self.hold.take();
                self.base_t(now, st, "give me a sec");
                self.hold = hold;
            }
            Event::Delta => {
                self.replied = true;
                self.last_act = now;
                if self.base != St::Streaming { self.cut_reaction(); self.base_t(now, st, "the answer"); }
            }
            Event::Tool(name, target) => {
                self.last_act = now;
                self.tools += 1;
                self.last_tool = st;
                self.last_detail = short_target(name, target);
                self.tool_at = now;
                self.cut_reaction();
                self.base_t(now, st, self.last_detail.clone());
            }
            Event::Permission => {
                self.last_act = now;
                // the user is needed: nothing may cover this
                self.flashes.clear();
                self.flash = None;
                let d = self.base_detail.clone();
                self.base(st, d);
            }
            Event::ToolDone(err) => self.tool_done(now, err, ""),
            Event::Finished(_) => {
                self.last_act = now;
                let tools = self.tools > 0;
                if self.replied && !self.failed {
                    let detail = match (tools, self.passed) {
                        (false, _) => String::new(),
                        (true, Some(St::Testing)) => "tests green".into(),
                        (true, Some(_)) => "build ok".into(),
                        (true, None) => format!("{} tool{}", self.tools, if self.tools == 1 { "" } else { "s" }),
                    };
                    let st = state_for(Event::Finished(tools));
                    self.flash(st, detail, st.dur_ms() as u64);
                }
                self.base_t(now, St::Idle, "");
                self.tools = 0;
                self.passed = None;
                self.replied = false;
                self.failed = false;
            }
            Event::UnknownCommand => { self.last_act = now; self.flash_now(now, st, "try /help"); }
            Event::Failed => { self.last_act = now; self.failed = true; self.flash_now(now, st, "see transcript"); }
            Event::Esc => { self.last_act = now; self.failed = true; self.hold = None; self.flash_now(now, st, ""); }
            Event::Recalled => self.flash(st, "", st.dur_ms() as u64),
            Event::Summarizing => self.base(st, "earlier turns"),
            Event::IdleTimeout => self.last_act = now.saturating_sub(self.sleep_after_ms.unwrap_or(0) + 1),
            Event::ModelUnloaded => self.unloaded = true,
            Event::ModelReloading => { self.unloaded = false; self.flash_now(now, st, ""); }
            Event::Poke | Event::Water | Event::Food | Event::Toy => self.care(now, st),
        }
    }

    /// a tool finished (`out` = its output, for the passing-test count): a failure plays a brief worried reaction, a
    /// passing test / build / check a brief happy one, then back to work (THINKING: the model reads the result). A
    /// tool that ran shorter than `MIN_TOOL_MS` stays on show until then, unless something else happens first.
    pub fn tool_done(&mut self, now: u64, err: bool, out: &str) {
        self.last_act = now;
        let (st, detail) = (self.base, self.base_detail.clone());
        let was_tool = matches!(st, St::Reading | St::Searching | St::Editing | St::Building | St::Testing | St::Running);
        self.base_t(now, St::Thinking, "give me a sec");
        if err {
            self.flash = Some((St::Error, "see transcript".into(), now, WORRIED_MS));
            self.flashes.clear();
            return;
        }
        if matches!(self.last_tool, St::Testing | St::Building) {
            self.passed = Some(self.last_tool);
            let d = match (self.last_tool, passed_tests(out)) {
                (St::Testing, Some(n)) => format!("{n} test{} pass", if n == 1 { "" } else { "s" }),
                (St::Testing, None) => "tests pass".into(),
                _ => "build ok".into(),
            };
            self.flash = Some((St::Success, d, now, HAPPY_MS));
            self.flashes.clear();
            return;
        }
        if was_tool && now < self.tool_at + MIN_TOOL_MS { self.hold = Some((st, detail, self.tool_at + MIN_TOOL_MS)); }
    }

    /// the gateway's phase for the running model call (`api::phase()`, polled by the display loop): while the model is
    /// working with no tool running, WAITING in line (its queue) and READING the context (prefill) show instead of
    /// THINKING. Stale phases never cover a tool, a reply or a prompt: they only refine the THINKING base.
    /// `call` = `api::call_target()` (the tool call being written: name, path), read only in `Phase::Call`.
    pub fn gateway(&mut self, now: u64, (ph, seq): (crate::api::Phase, u64), call: impl FnOnce() -> (String, String)) {
        if seq == self.gw_seq { return; }
        self.gw_seq = seq;
        if ph == crate::api::Phase::Call { self.call = call(); }
        if ph != self.gw { self.gw = ph; self.gw_at = now; }
    }

    /// a care action (kit v2): plays at once, over the agent's state, and wakes a sleeping Shade straight into it.
    /// Never covers a permission prompt: the user is needed there first.
    fn care(&mut self, now: u64, st: St) {
        let (detail, ms) = care_of(st);
        // the idle clock starts when the care action ends: no dozing off mid-snack
        self.last_act = now + ms;
        self.unloaded = false;
        if self.base == St::Permission { return; }
        self.flashes.clear();
        self.flash = Some((st, detail.into(), now, ms));
    }

    /// the permission was answered: the tool runs now (yes), or the model goes on without it (no)
    pub fn approved(&mut self, now: u64, yes: bool) {
        self.last_act = now;
        if self.base != St::Permission { return; }
        if yes { let d = self.last_detail.clone(); self.tool_at = now; self.base_t(now, self.last_tool, d); } else { self.base_t(now, St::Thinking, "give me a sec"); }
    }

    /// memories recalled for this turn: the bubble's target
    pub fn recalled(&mut self, now: u64, n: usize) {
        self.on(now, Event::Recalled);
        if let Some(f) = self.flashes.back_mut().filter(|f| f.0 == St::Memory) { f.1 = format!("{n} memor{}", if n == 1 { "y" } else { "ies" }); }
    }

    /// a summary of earlier turns ended
    pub fn summary_done(&mut self) { if self.base == St::Reading && self.base_detail == "earlier turns" { self.base(St::Idle, ""); } }

    /// the agent is idle at the prompt again (no turn ran: a slash command, a cancelled pick)
    pub fn idle(&mut self) { if !matches!(self.base, St::Listening | St::Permission) { self.base(St::Idle, ""); } }

    /// the state shown at `now`, its bubble detail and a verb that names the activity instead of the state's own
    /// (advances the flash queue)
    fn current(&mut self, now: u64) -> (St, String, Option<&'static str>) {
        let (st, d) = self.current_base(now);
        use crate::api::Phase as P;
        let writes_code = self.gw == P::Call && matches!(self.call.0.as_str(), "edit" | "write" | "edit_symbol");
        // reply text, then a tool call: WRITING its file (code), or THINKING (any other call)
        if st == St::Streaming && self.base == St::Streaming && self.gw == P::Call {
            return if writes_code { (St::Streaming, self.call_path(), None) } else { (St::Thinking, String::new(), None) };
        }
        if (st, d.as_str()) != (St::Thinking, "give me a sec") || self.base != St::Thinking { return (st, d, None); }
        // the model is working, no tool running: what the gateway says it is doing
        if let Some((hs, hd, until)) = &self.hold {
            if now < *until { return (*hs, hd.clone(), None); }
            self.hold = None;
        }
        let seen = now.saturating_sub(self.gw_at) >= PHASE_SHOW_MS;
        match self.gw {
            P::Sent if seen => (St::Idle, "in line".into(), Some("WAITING")),
            P::Prefill if seen => (St::Reading, "the context".into(), None),
            // the model writes the code of an edit / write call (EDITING is the moment the tool applies it)
            P::Call if writes_code => (St::Streaming, self.call_path(), None),
            _ => (St::Thinking, String::new(), None),
        }
    }

    /// the file the tool call being written goes to (`code` until its path is known)
    fn call_path(&self) -> String { if self.call.1.is_empty() { "code".into() } else { short_path(&self.call.1, PATH_MAX) } }

    fn current_base(&mut self, now: u64) -> (St, String) {
        loop {
            match &self.flash {
                Some((_, _, at, dur)) if now >= at + dur => {
                    let end = at + dur;
                    self.flash = None;
                    if let Some((st, d, ms)) = self.flashes.pop_front() {
                        // a chained greeting only plays if Elisa is still idle
                        if st == St::Greeting && !matches!(self.base, St::Idle | St::Listening) { continue; }
                        self.flash = Some((st, d, end.max(now.saturating_sub(ms)), ms));
                    }
                }
                Some((st, d, _, _)) => return (*st, d.clone()),
                None => match self.flashes.pop_front() {
                    Some((st, d, ms)) => self.flash = Some((st, d, now, ms)),
                    None => break,
                },
            }
        }
        if self.base == St::Idle && (self.unloaded || self.sleep_after_ms.is_some_and(|s| now.saturating_sub(self.last_act) > s)) {
            return (St::Sleeping, String::new());
        }
        (self.base, self.base_detail.clone())
    }

    pub fn view(&mut self, now: u64) -> View {
        let (st, detail, over) = self.current(now);
        if st != self.shown { self.shown = st; self.shown_at = now; }
        let to = if st.pops() { POP } else { 1.0 };
        if to != self.pop_to {
            self.pop_from = self.pop_at_time(now);
            self.pop_to = to;
            self.pop_at = now;
        }
        let pop = self.pop_at_time(now);
        let v = over.unwrap_or(verb(st));
        let bubble = (!v.is_empty()).then(|| Bubble {
            verb: v, target: detail, big: st.pops(),
            tone: match st { St::Error => Tone::Error, St::Permission => Tone::Warn, _ => Tone::Accent },
        });
        // a long tool: fidget now and then (only the base tool state, never over a reaction or a prompt)
        let long = now.saturating_sub(self.base_at);
        let fidget = st == self.base && matches!(st, St::Reading | St::Searching | St::Editing | St::Building | St::Testing | St::Running)
            && over.is_none() && self.hold.is_none() && long >= FIDGET_AFTER_MS && (long - FIDGET_AFTER_MS) % FIDGET_EVERY_MS < FIDGET_MS;
        View { st, ms: now - self.shown_at, pop, bubble, fidget }
    }

    fn pop_at_time(&self, now: u64) -> f32 {
        let x = (now.saturating_sub(self.pop_at)) as f32 / POP_MS as f32;
        if x >= 1.0 { return self.pop_to; }
        self.pop_from + (self.pop_to - self.pop_from) * motion::bezier(0.34, 1.7, 0.64, 1.0, x)
    }
}

/// one terminal cell of the character: a half block with its two pixels, or a space (None = the theme background)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell { pub ch: char, pub fg: Option<Rgb>, pub bg: Option<Rgb> }

/// a rendered frame: `w` x `h` cells, row-major
#[derive(Clone, Debug, PartialEq)]
pub struct Art { pub w: usize, pub h: usize, pub cells: Vec<Cell> }

/// how to render
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// theme background (the glow and anti-aliased edges blend onto it)
    pub bg: Rgb,
    /// glow colours: accent, warn, error, and the accent glow strength (dark .24, light .16)
    pub accent: Rgb, pub warn: Rgb, pub error: Rgb, pub accent_a: f32,
}

/// the animation frame index of `ms` into a state (12 fps, looped)
pub fn frame_of(st: St, ms: u64) -> u64 {
    let n = (st.dur_ms() as u64).div_ceil(FRAME_MS).max(1);
    (ms / FRAME_MS) % n
}

/// Shade in `st` at `ms` into the state, in `w` x `h` cells (each cell = 2 stacked pixels; the stage is the bottom
/// `w` x `w` pixels, the rest is headroom for jumps and the pop). `pop` = attention scale (1.0 .. 1.22).
/// Deterministic: equal arguments give equal cells.
pub fn render(st: St, ms: u64, w: usize, h: usize, pop: f32, look: &Look) -> Art {
    let (pw, ph) = (w, h * 2);
    let ss = if CRISP { SUPER } else { 1 };
    let mut c = Canvas::new(pw * ss, ph * ss);
    c.scale = ss as f32;
    c.crisp = CRISP;
    let u = (frame_of(st, ms) * FRAME_MS) as f32 / st.dur_ms() as f32;
    let mini = pw < MINI_BELOW;
    let stage = pw as f32;
    let top = ph as f32 - stage;
    // stage → canvas, scaled about 50% 88% of the stage (the kit's transform-origin)
    let (ox, oy) = (stage * 0.5, top + stage * 0.88);
    let popt = Tf::sc(ss as f32, ss as f32).then(Tf::tr(ox, oy)).then(Tf::sc(pop, pop)).then(Tf::tr(-ox, -oy)).then(Tf::tr(0.0, top));
    // the radial glow behind (left/right 8%, top 18%, bottom 4% of the stage)
    let (gc, ga) = match st { St::Error => (look.error, 0.28), St::Permission => (look.warn, 0.26), _ => (look.accent, look.accent_a) };
    let gop = if st.pops() { 1.0 } else if matches!(st, St::Idle | St::Sleeping) { 0.0 } else { 0.5 };
    // pixel art has no soft gradients: the glow is left out (the bubble's accent border carries the attention)
    if gop > 0.0 && !CRISP {
        let (cx, cy) = popt.ap(stage * 0.5, stage * 0.57);
        c.glow(cx, cy, stage * 0.42 * pop, stage * 0.39 * pop, gc, ga * gop);
    }
    // artboard → stage: full = the 200 x 200 artboard, mini = viewBox 27 32 146 146
    let (vx, vy, vw) = if mini { (27.0, 32.0, 146.0) } else { (0.0, 0.0, 200.0) };
    let k = stage / vw;
    let base = popt.then(Tf::sc(k, k)).then(Tf::tr(-vx, -vy));
    draw::shade(&mut c, &base, st, u, mini, true);
    cells_of(&c, w, h, look.bg)
}

/// the 12 states the kit draws a status-bar face for (05-character/shade/svg-statusbar); the others fall back as in
/// the prototype's `FALLBACK`
pub fn face_state(st: St) -> St {
    match st {
        St::Poked => St::Interrupted,
        St::Drinking | St::Eating | St::Playing | St::Greeting | St::Celebrating => St::Success,
        St::Reading | St::Editing | St::Building | St::Testing => St::Running,
        St::Searching => St::Thinking,
        St::Confused => St::Error,
        s => s,
    }
}

/// the face's window on the artboard: (x, y, size). The kit's is 40 28 120 120 (`FACE_KIT`); terminal cells are
/// coarse, so the status bar uses `FACE_VIEW`, the same centre cropped to the ears and eyes.
pub const FACE_KIT: (f32, f32, f32) = (40.0, 28.0, 120.0);
pub const FACE_VIEW: (f32, f32, f32) = (FACE_VIEW_X, FACE_VIEW_Y, FACE_VIEW_S);
const FACE_VIEW_X: f32 = 46.0;
const FACE_VIEW_Y: f32 = 32.0;
const FACE_VIEW_S: f32 = 108.0;

/// the status-bar face (kit v2): ears and eyes only, `w` x `h` cells of half blocks on the window background `bg`,
/// animated like the dock (the kit's face follows the same pose)
pub fn render_face(st: St, ms: u64, w: usize, h: usize, bg: Rgb, view: (f32, f32, f32)) -> Art {
    let st = face_state(st);
    let (pw, ph) = (w as f32, (h * 2) as f32);
    let ss = if CRISP { SUPER } else { 1 };
    let mut c = Canvas::new(w * ss, h * 2 * ss);
    c.scale = ss as f32;
    c.crisp = CRISP;
    let u = (frame_of(st, ms) * FRAME_MS) as f32 / st.dur_ms() as f32;
    let (vx, vy, vs) = view;
    let k = (pw / vs).min(ph / vs);
    let base = Tf::sc(ss as f32, ss as f32).then(Tf::tr((pw - vs * k) / 2.0, (ph - vs * k) / 2.0)).then(Tf::sc(k, k)).then(Tf::tr(-vx, -vy));
    draw::face(&mut c, &base, st, u, bg);
    cells_of(&c, w, h, bg)
}

/// Crisp pixel art for the half-block path (terminals without image support, e.g. macOS Terminal): every pixel
/// is either the theme background or one solid colour of the kit's character palette. No anti-aliased grey edges,
/// no soft glow. The art grid is the dock's own pixel grid (1 art pixel = 1 half-block pixel, scale 1 at the
/// 32 / 24 / 16 px stages the dock picks).
pub const CRISP: bool = true;
/// a pixel is ink when at least this much of it is covered (below: the background); a bit under one half, so
/// one-pixel lines that straddle two pixels keep one of them
pub const INK: f32 = 0.4;

/// the kit's character and prop colours (02-color "character" + the renderer's props): what crisp pixels snap to
pub const PALETTE: [Rgb; 27] = [
    (0x0E, 0x0E, 0x12), (0x14, 0x14, 0x18), (0x1C, 0x1C, 0x22), (0x1E, 0x1E, 0x24), (0x1E, 0x5A, 0x6A), (0x2A, 0x2A, 0x33),
    (0x3A, 0x3A, 0x46), (0x46, 0x46, 0x5A), (0x78, 0xC8, 0x78), (0x82, 0xC8, 0xDC), (0x8A, 0x6A, 0x46), (0x8A, 0x5A, 0x3E),
    (0x96, 0x96, 0xA0), (0xA8, 0x6E, 0x48), (0xB4, 0x8A, 0x5E), (0xB8, 0xB8, 0xC4), (0xC8, 0xA2, 0x7A), (0xD7, 0x77, 0x57),
    (0xD9, 0x66, 0x8E), (0xDD, 0xEF, 0xF4), (0xE6, 0xBE, 0x50), (0xE8, 0xE8, 0xE8), (0xEE, 0xF0, 0xE6), (0xF0, 0x64, 0x64),
    (0xFF, 0x8F, 0xB1), (0xFF, 0xFF, 0xFF), (0x00, 0x00, 0x00),
];

/// the nearest palette colour (weighted RGB distance)
pub fn snap(c: Rgb) -> Rgb {
    let d = |p: &Rgb| { let f = |a: u8, b: u8| (a as i32 - b as i32).pow(2); f(p.0, c.0) * 3 + f(p.1, c.1) * 4 + f(p.2, c.2) * 2 };
    *PALETTE.iter().min_by_key(|p| d(p)).unwrap_or(&c)
}

/// crisp rendering supersamples each art pixel `SUPER` x `SUPER` and lets the sub-pixels vote
pub const SUPER: usize = 4;

/// one crisp art pixel from its `SUPER` x `SUPER` sub-pixels: each sub-pixel is snapped to the palette (they are
/// nearly pure: inside one shape), then the pixel is background unless at least `INK` of them are ink, and else the
/// most common ink colour. No colour is ever averaged, so there are no in-between greys.
fn vote(c: &Canvas, x: usize, y: usize) -> Option<Rgb> {
    let mut n: Vec<(Rgb, usize)> = Vec::with_capacity(4);
    let mut ink = 0;
    for sy in 0..SUPER {
        for sx in 0..SUPER {
            let (rgb, a) = c.get(x * SUPER + sx, y * SUPER + sy);
            if a < 0.5 { continue; }
            ink += 1;
            let k = snap(rgb);
            match n.iter_mut().find(|e| e.0 == k) { Some(e) => e.1 += 1, None => n.push((k, 1)) }
        }
    }
    if (ink as f32) < INK * (SUPER * SUPER) as f32 { return None; }
    // ties: the first colour found (top-left), so equal inputs give equal pixels
    let best = n.iter().map(|e| e.1).max()?;
    n.into_iter().find(|e| e.1 == best).map(|e| e.0)
}

/// canvas pixels → half-block cells: crisp (palette colours, hard edges, a `SUPER`-sampled canvas) or, with `CRISP`
/// off, anti-aliased and blended onto the theme background
fn cells_of(c: &Canvas, w: usize, h: usize, bg: Rgb) -> Art {
    let px = |x: usize, y: usize| -> Option<Rgb> {
        if CRISP { return vote(c, x, y); }
        let (rgb, a) = c.get(x, y);
        (a >= 0.06).then(|| mix(bg, rgb, a))
    };
    let mut cells = Vec::with_capacity(w * h);
    for row in 0..h {
        for x in 0..w {
            let (t, b) = (px(x, row * 2), px(x, row * 2 + 1));
            cells.push(match (t, b) {
                (None, None) => Cell { ch: ' ', fg: None, bg: None },
                (Some(t), None) => Cell { ch: '▀', fg: Some(t), bg: None },
                (None, Some(b)) => Cell { ch: '▄', fg: Some(b), bg: None },
                (Some(t), Some(b)) if t == b => Cell { ch: ' ', fg: None, bg: Some(t) },
                (Some(t), Some(b)) => Cell { ch: '▀', fg: Some(t), bg: Some(b) },
            });
        }
    }
    Art { w, h, cells }
}

/// below this stage width (pixels = columns) the mini build is used (no rim, paws or props)
pub const MINI_BELOW: usize = 20;

#[cfg(test)]
mod tests;
