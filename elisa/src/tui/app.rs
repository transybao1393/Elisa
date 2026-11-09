use super::*;

/// marks an approval for a call outside the project folder (the tool name follows; the summary's 2nd line says where)
pub(super) const OUTSIDE: &str = "\u{1}outside:";
/// an MCP tool's approval: `{MCP}server\u{1}tool` (the box offers "always allow MCP server …")
pub(super) const MCP: &str = "\u{1}mcp:";
/// a yes / no question (plugin setup, project trust), not a tool call: the summary is the question
pub(super) const QUESTION: &str = "\u{1}question:";
/// after the tool: the pattern offered for "always allow this pattern" (`Frontend::approve_rule`)
pub(super) const RULE: char = '\u{2}';

/// The choices of an approval box, in order (the last is always No): Yes / Always / [always allow PATTERN] / No, or
/// Yes / No for a question.
pub(super) fn approve_choices(tool: &str) -> Vec<(String, Approval)> {
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
pub(super) const DONE_KEEP: Duration = Duration::from_secs(6);
// memory bounds: Elisa stays tiny (the model server owns the RAM)
const MAX_ENTRIES: usize = 1500;
const MAX_BYTES: usize = 2 << 20;
const MAX_VIEWS: usize = 50;
const MAX_HIST: usize = 200;
pub(super) const RSS_WARN: u64 = 50 << 20;

// ---------- channel frontend ----------
/// turns of a resumed conversation the terminal shows again (the app shows more)
const TUI_EARLIER: usize = 3;

impl App {
    pub(super) fn new(yolo: bool) -> Self {
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
    pub(super) fn now(&self) -> u64 { self.clock.elapsed().as_millis() as u64 }
    /// turn the companion on; it sleeps after `sleep_after_s` idle seconds (0 = never)
    pub(super) fn with_shade(mut self, sleep_after_ms: u64) -> Self {
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

    pub(super) fn on_ev(&mut self, e: Ev) {
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
    pub(super) fn mouse(&mut self, me: MouseEvent) {
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
    pub(super) fn menu(&self) -> Vec<(&'static str, &'static str)> {
        if self.menu_off || !self.editable() || self.input.first() != Some(&'/')
            || self.input.iter().any(|c| c.is_whitespace()) { return vec![]; }
        let p: String = self.input.iter().collect();
        COMMANDS.iter().copied().filter(|(n, _)| n.starts_with(&p)).collect()
    }

    /// input box takes keys: at the prompt, or while the agent is busy (queueing); paused while a summary runs
    fn editable(&self) -> bool { matches!(self.pending, Some(Pending::Input(_)) | None) && !self.summarizing() }
    /// a summary / fold of earlier turns is running (its progress job): the input box is paused until it ends
    pub(super) fn summarizing(&self) -> bool {
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

    pub(super) fn on_key(&mut self, ke: KeyEvent) {
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

    pub(super) fn panel_on(&self) -> bool { self.panel_pref.unwrap_or(self.term_w >= 100) }
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

    pub(super) fn paste(&mut self, s: &str) {
        if !self.editable() { return; }
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() { self.ins(c); }
        let (now, typed) = (self.now(), !self.input.is_empty());
        if let Some(c) = &mut self.shade { c.typing(now, typed); }
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
}

fn first_word(s: &str) -> Option<String> { s.split_whitespace().next().map(str::to_lowercase) }

/// this process's resident size in bytes (0 if unknown)
pub(super) fn own_rss() -> u64 {
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
    pub(super) fn draw(a: &mut App) {
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
        let src = include_str!("frontend.rs");
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
