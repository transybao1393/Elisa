use super::*;

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

impl<'f> Session<'f> {
    /// `/model`: what this session talks to, and the saved connections
    /// llama.cpp with `--slot-save-path`: save the slot's processed prompt (this session's KV) to the session's slot
    /// file in the server's slot dir, and remember which file is whose (`.elisa/slots.json`). On /clear, quit and a
    /// session switch. Any other server, or an empty session: nothing, quietly.
    pub(super) fn slot_save(&mut self, why: &str) {
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
    pub(super) fn slot_restore(&mut self, path: &Path) {
        if !self.client.caps().slot_save { return; }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { return };
        let map = crate::backend::SlotMap::load(&self.store.dir);
        let Some(e) = map.find(id, self.client.base(), self.client.model()).cloned() else { return };
        match self.client.slot_restore(&e.file) {
            Ok((n, ms)) => self.fe.info(&format!("[llama.cpp: restored {n} processed tokens from slot file {} ({})]", e.file, ui::dur(ms / 1000.0, 1))),
            Err(err) => self.fe.info(&format!("[llama.cpp: slot not restored: {err}]")),
        }
    }

    /// the window is tiny (`context::tiny_window`): short prompt, no tools, no header, no hint lines
    pub(super) fn tiny(&self) -> bool { context::tiny_window(self.max_ctx) }

    pub(super) fn fixed(&self) -> usize { self.system_tokens + self.tools_tokens + self.doc_req_tokens + self.card_req.as_deref().map_or(0, estimate_tokens) }
    /// Tokens of the next request. An empty history still sends a first message (cwd/OS/date + notes + [state]), so
    /// that header counts: after /clear the shown size is what the next request really starts with.
    pub(super) fn estimate(&self) -> usize {
        match self.real {
            Some((n, t)) if n <= self.history.len() => t + estimate_messages(&self.history[n..]),
            _ if self.history.is_empty() => self.fixed() + estimate_messages(&[Message::user_text(self.first(""))]),
            _ => self.fixed() + estimate_messages(&self.history),
        }
    }
    /// The next request's size where going over the context is fatal (a step's results, the send check): the largest
    /// of the text estimate, the server's last reported tokens + the estimate of what was appended since, and the text
    /// estimate scaled by the last measured real/estimate ratio (`context::safe_request_tokens`).
    pub(super) fn est_safe(&self) -> usize {
        let text = self.fixed() + estimate_messages(&self.history);
        let reported = self.real.filter(|&(n, _)| n <= self.history.len()).map(|(n, t)| (t, estimate_messages(&self.history[n..])));
        context::safe_request_tokens(text, reported, self.calib)
    }
    /// Output reserve, capped so a small max_ctx (below max_output) still leaves room for the prompt.
    /// the reply's room: its output, plus some of a reasoning model's reasoning (`Client::reasoning_room`)
    pub(super) fn reserve(&self) -> usize { self.cfg.max_output.min(self.max_ctx / 4) + self.client.reasoning_room().min(self.max_ctx / 8) }
    /// Prompt budget: max_ctx minus the output reserve and the safety margin (`context::ctx_margin`, 3%).
    pub(super) fn limit(&self) -> usize { self.max_ctx.saturating_sub(self.reserve() + context::ctx_margin(self.max_ctx)) }

    /// Once at startup: a warning when system + tools take more than FIXED_WARN_PCT of the usable context on a plain
    /// turn, naming the biggest servers (at a small window the conversation is left so little room that it folds
    /// before nearly every turn).
    pub(super) fn budget_warning(&mut self) {
        let (fixed, limit) = (self.fixed(), self.limit());
        if limit == 0 || fixed * 100 <= limit * FIXED_WARN_PCT { return; }
        let parts = self.tool_costs();
        let top: Vec<String> = parts.iter().take(3).map(|(n, t)| format!("{n} {}", ui::k(*t))).collect();
        self.fe.info(&format!("context: system + tools take {} of the {} usable tokens ({}%) before any conversation; biggest: {} · \
            a larger max_ctx, or fewer servers (web / sandbox = \"off\", [mcp.NAME] enabled = false), leaves more room",
            ui::k(fixed), ui::k(limit), fixed * 100 / limit, top.join(", ")));
    }

    /// The facts the user told about themself in this (resumed) history and not yet in its notes, in the third person
    /// (`context::user_facts`: "The user's name is Linh").
    pub(super) fn resumed_facts(&self) -> Vec<String> {
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

    pub(super) fn save(&mut self) {
        if self.history.is_empty() { return; }
        let t0 = Instant::now();
        let show = self.save_slow;
        if show { self.fe.progress(Some(&Progress { label: "saving session".into(), ..Default::default() })); }
        // who wrote each reply, kept in the session (the next model must not take them as its own)
        context::stamp_model(&mut self.history, &self.cfg.model);
        if let Err(e) = self.store.save_session(&self.history) { self.fe.error(&format!("session not saved: {e}")); }
        self.store.save_cards(&self.topic_cards);
        self.save_slow = t0.elapsed() > SLOW;
        if show { self.fe.progress(None); }
    }

    /// After a panic caught by `guarded`: say so, and leave the session usable for the next message (the turn's
    /// user message gets a closing reply so roles keep alternating; a panic inside elisa-mem turns memory off for the
    /// rest of the session, since the same call would only fail again).
    pub(super) fn recover(&mut self, msg: &str) {
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

    // ----- per-request metrics (src/metrics.rs) -----

    /// A user request starts: drop calls made outside a request (/clear, /compact summaries) and start the clock.
    pub(super) fn req_begin(&mut self, prompt: &str) {
        // the user's own words: the session goal and `current request:` come from it
        self.user_req = context::one_line(prompt, 240);
        self.goal_before = (self.goal.clone(), self.goal_misses, self.goal_stated);
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
    pub(super) fn req_end(&mut self, outcome: &str) {
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
    pub(super) fn shrink_notes(&mut self, before: usize, old: Option<String>) -> usize {
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

    pub(super) fn set_notes(&mut self, n: Option<String>) { self.put_notes(n, true); }

    /// A resumed session's notes (`--continue` / `/resume`) become this session's own, saved under this session's id
    /// (the resumed session's file is not touched), so the next `--continue` finds them again.
    pub(super) fn adopt_notes(&mut self, notes: Option<String>) {
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
    pub(super) fn verified(&self, new: &str) -> String {
        let cwd = self.cwd.clone();
        // what the conversation holds first: code claims no reply backs, attached documents as files touched, raw
        // tool lines (`context::ground_notes`)
        let said: String = self.history.iter().filter(|m| m.role == Role::Assistant).map(texts).collect::<Vec<_>>().join("\n");
        let new = context::ground_notes(new, &said, &self.doc_names(), &self.ledger);
        let v = context::verify_claims(&new, &self.ledger, &move |p: &str| std::fs::read_to_string(cwd.join(p)).ok());
        let o = context::Outside::of(&self.history);
        if o.is_empty() { v } else { context::quarantine_notes(&v, &o) }
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
    pub(super) fn merge_kept(&mut self, old: Option<&str>, new: &str) -> String {
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
    pub(super) fn finish_notes(&mut self, merged: String) -> String {
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
    pub(super) fn fold_point(&self) -> usize { context::fold_point(self.fixed(), self.limit(), self.cfg.fold_at) }
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
    pub(super) fn summarise(&mut self, label: &str) -> Result<String, String> {
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
    /// `min_gain`: at small windows that folds before almost every turn, some folds growing the prompt). After a
    /// failed or skipped fold, wait until the window grows by that share again.
    pub(super) fn maybe_fold(&mut self) {
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
    /// Over the limit, the room is made in this order, and the turn goes on by itself (rather than stopping
    /// with "say continue" every turn): earlier documents' text becomes their reference lines (`context::drop_doc_text`),
    /// then the fold, then the oldest turns go (`context::drop_old_turns`; their words stay in memory and the notes),
    /// then this turn's own document text is cut to fit.
    pub(super) fn mid_turn_fold(&mut self) -> bool {
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

    // ----- graph memory -----

    pub(super) fn open_memory(&mut self) {
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
    pub(super) fn carry_card(&mut self) {
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

    /// A note for the domain watcher (background; dropped when the gateway has none).
    pub(super) fn watch_note(&self, kind: &str, text: &str) { self.watch.note(kind, text); }
    /// Delivers a watcher update that arrived without a push sink (the plain display).
    pub(super) fn watch_poll(&mut self) { if let Some(u) = self.watch.take() { self.fe.watcher_update(&u); } }
    /// A shrink without a summary: the card (or the token counts) stands for the fold.
    pub(super) fn shrunk(&mut self, before: usize) {
        let after = self.estimate();
        self.fe.compacted(before, after);
        let t = self.card.clone().unwrap_or_else(|| format!("[compacted: {before} → {after} tokens]"));
        self.watch_note("fold", &t);
    }

    /// No model call here, so quitting stays instant; indexing runs only within EXIT_INDEX_BUDGET_S.
    pub(super) fn end_memory(&mut self) {
        let Some((r, mut shown)) = self.mem_call(|m| m.db.end_session(m.session)) else { return };
        if let Err(e) = r { self.fe.error(&format!("memory: {e}")); }
        shown |= self.index_pending(Some(EXIT_INDEX_BUDGET_S));
        if shown { self.fe.progress(None); }
    }

    pub(super) fn new_memory_session(&mut self) {
        self.end_memory();
        if let Some(m) = self.mem.as_mut() { m.session = m.db.begin_session(&m.project); }
        self.turn_no = 0;
        self.fold_reset();
    }

    pub(super) fn fold_reset(&mut self) {
        self.folded = self.turn_no;
        self.fold_ids.clear();
        self.fold_retry_at = 0;
    }

    /// Counts the turn for the fold (also without memory) and records it.
    pub(super) fn record(&mut self, role: MRole, text: &str) -> Option<u64> {
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
    pub(super) fn first_notes(&self) -> Option<String> {
        let n = self.notes.as_deref()?;
        Some(if self.notes_bg { context::background_notes(n) } else { n.to_string() })
    }

    /// The `[state]` body of a first message: the session goal, then this project's pinned facts (newest first within
    /// PINNED_CAP tokens, listed oldest first). None when there is neither.
    pub(super) fn state_block(&self) -> Option<String> {
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
    pub(super) fn drop_card(&mut self) {
        let newest = self.card_id.or_else(|| self.mem.as_ref().and_then(|m| m.db.latest_card(&m.project).map(|c| c.0)));
        if let Some(id) = newest { if let Err(e) = self.store.save_card_reset(id) { self.fe.error(&format!("card reset: {e}")); } }
        (self.card, self.card_id, self.card_ex) = (None, None, None);
        self.card_log.clear();
        self.topic_cards.clear();
        (self.topic, self.card_req) = (Default::default(), None);
    }

    /// A new card from this exchange: show it, chain it in memory (prev = the last card), make it current.
    pub(super) fn set_card(&mut self, card: String, ids: &[u64]) {
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
    pub(super) fn load_topics(&mut self, session: Option<u64>) {
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
    pub(super) fn mem_note(&mut self, notes: &str, n: usize) {
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
    pub(super) fn recall(&mut self, query: &str) -> Vec<String> {
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
            // they told about themselves (an old question or command adds nothing)
            if !context::strong_recall(query, &h.text) || context::unfit_memory(&h.text) { continue; }
            if asked.contains(&h.id) && context::user_facts(&h.text).is_empty() { continue; }
            if h.kind == HitKind::Turn && !asked.contains(&h.id) && !context::has_tool_line(&h.text) { continue; }
            if forgot.is_some_and(|f| h.id < f) && m.db.in_project(h.id, &m.project) { continue; }
            match h.kind {
                // this session's own turns are in the window or its notes already: recalling one only repeats it
                // (Elisa's own reply from a moment ago would come back as [memory])
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
    pub(super) fn fold(&mut self, by_turns: bool) {
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
    /// prompt smaller than `before` (a fold can make it bigger). Over it, the notes
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
}

/// A user message with the user's own text (not one of Elisa's `[elisa] …` notes).
pub(super) fn is_request(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.starts_with(context::NUDGE_PREFIX)))
}

pub(super) fn texts(m: &Message) -> String {
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
