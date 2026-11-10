use super::*;
use super::memory::{is_request, texts};

impl<'f> Session<'f> {

    /// The user's words of this turn asked once more alone: the same model and system prompt, no history, no tools,
    /// and this turn's picture text when it has one (a small model may copy an earlier reply into every later
    /// turn, or answer Elisa's bracketed blocks instead of the ask; asked alone it answers the
    /// ask, as the picture request does). The answer when it is usable (`guard`), else None.
    fn isolated_reply(&mut self, max_tokens: usize, guard: &context::ReplyGuard) -> Option<String> {
        let mut text = context::request_body(&self.input).trim().to_string();
        if text.is_empty() { return None; }
        if let Some(p) = &self.turn_pic { text = format!("{text}\n{}", context::picture_block(p)); }
        let msgs = [Message::user_text(text)];
        let no_tools: [crate::types::ToolDef; 0] = [];
        let req = Request { system: &self.system, tools: &no_tools, messages: &msgs, max_tokens };
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
        let raw = texts(&Message::new(Role::Assistant, r.content));
        let t = crate::route::strip_echoed_notes(&context::strip_picture_echo(context::split_state(&raw).0.trim()));
        let off = self.turn_pic.as_deref().is_some_and(|p| context::off_picture(&t, p));
        (!t.trim().is_empty() && guard.broken(&t).is_none() && !off).then(|| t.trim().to_string())
    }

    /// One step's tool results against what the window has left (the limit minus the request so far): over it, the
    /// NEW results are cut before they are appended (largest first, `[trimmed N chars]`), so the next request never
    /// goes over the limit (a few big parallel reads can otherwise push it over).
    ///
    /// Append-only within a turn: older messages are never rewritten to make room (shrinking one earlier result per
    /// step, often to a marker longer than the original, made each call re-prefill everything after the system
    /// prompt and tools). Only when even the cut-to-minimum results do not fit does the turn's one fold run.
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
        // a result cut to nothing (results "trimmed to fit (0 tokens left)" get re-requested or
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
    /// the request after a fold never goes over it either (a fold that keeps big reads would overflow again).
    pub(super) fn fit_newest_results(&mut self) {
        if self.est_safe() <= self.limit() { return; }
        let Some(i) = self.history.iter().rposition(|m| m.content.iter().any(|b| matches!(b, Block::ToolResult { .. }))) else { return };
        let mine = estimate_messages(&self.history[i..=i]);
        let room = self.limit().saturating_sub(self.est_safe().saturating_sub(mine) + MSG_OVERHEAD_TOKENS);
        let before = self.est_safe();
        if context::cap_step_results(&mut self.history[i].content, room) > 0 { self.real = None; self.shrunk(before); }
    }

    /// The agent loop of a turn: the history ends with a user message. `first` = that message is the turn's first
    /// (a failed send drops it again).
    pub(super) fn agent_loop(&mut self, checks: CheckRun, first: bool) -> TurnEnd {
        // repeated identical calls, short cycles, steps without progress (`loop_guard.rs`): the turn's one guard, so
        // a retry of this turn counts the calls its first try made
        let mut guard = std::mem::take(&mut self.turn_guard);
        let end = self.agent_loop_with(checks, first, &mut guard);
        self.turn_guard = guard;
        end
    }

    fn agent_loop_with(&mut self, mut checks: CheckRun, mut first: bool, guard: &mut crate::loop_guard::Guard) -> TurnEnd {
        // one correction per turn when a reply writes a tool call as text
        let mut nudged = false;
        let mut repeat = Repeat::default();
        // a document whose text is in this turn's message is not opened with a file tool
        if self.doc_req.is_some() { guard.attached(self.kept_docs.iter().map(|k| k.doc.name.clone()).collect()); }
        // a loop stop asks the model once for an answer with no tools before the turn stops: its reason
        let mut final_ask: Option<String> = None;
        // a reply with no visible text (only its state card): asked once more, with this nudge after the history for
        // that one request (it never enters the history, so the next requests' prefix is unchanged)
        let mut empty_retry: Option<&'static str> = None;
        let mut retried = false;
        let mut stashed_card: Option<String> = None;
        // a simple turn's reply that used its whole small budget with no text for the user: sent once more without
        // the simple-turn limits (otherwise a two-question turn judged simple shows only the stop line)
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
                // sent anyway, a request over the window gets nothing back from the server
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
            // earlier turns' corrections ("nothing ran", "nothing was saved") and the attempts they corrected: left
            // out, so a later turn never takes a fixed failure for the outcome (`drop_old_nudges`)
            let fresh = drop_old_nudges(base);
            let base: &[Message] = fresh.as_deref().unwrap_or(base);
            let dedup = context::drop_repeated_replies(base);
            let base: &[Message] = dedup.as_deref().unwrap_or(base);
            // a long history: old answers shortened, for this request only (`context::shorten_old_replies`); the
            // current turn's request is the boundary, so every request of a turn has the same prefix
            let short = context::shorten_old_replies(base, base.iter().rposition(is_request).unwrap_or(base.len()), self.max_ctx);
            let base: &[Message] = short.as_deref().unwrap_or(base);
            // this turn's document text (`doc_req`) and card (`card_req`), in the turn's user message for these
            // requests only (`with_turn_extras`: the documents before the user's words, the card after them)
            let with_doc: Vec<Message>;
            let base: &[Message] = match (&self.doc_req, &self.card_req) {
                (None, None) => base,
                (doc, card) => { with_doc = with_turn_extras(base, doc.as_deref(), card.as_deref()); &with_doc }
            };
            let messages: &[Message] = match empty_retry.take() {
                Some(n) => { retry_msgs = with_note(base, n); &retry_msgs }
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
            // what a reply to this request must not be (`context::ReplyGuard`)
            let reply_guard = context::ReplyGuard::new(&self.input, messages);
            let prompt_real = reply.usage.input_tokens + reply.usage.cache_read_input_tokens;
            // a cancelled (Esc) or unreported reply has no usage: the partial output's estimate stands in for it
            if prompt_real == 0 && reply.usage.output_tokens == 0 {
                reply.usage.output_tokens = estimate_messages(&[Message::new(Role::Assistant, reply.content.clone())]).saturating_sub(MSG_OVERHEAD_TOKENS);
            }
            // Anthropic usage: input_tokens excludes the cached (reused) part. Without it the context is the estimate
            // of the request just sent plus that output (a ctx_used of 0 after Esc would reset the app's ctx meter)
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
            // is not run and does not widen the route (otherwise a "hey" after a turn the loop guard
            // stopped can call tools, the widen path runs them, and the turn loops for thousands of tokens). It is asked once
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
                // CONTINUE_MAX times (so the user need not type "continue" turn after turn)
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
                // picture text copied into the reply too (`context::strip_picture_echo`)
                // and a line that only restates the user's message (`route::strip_echoed_ask`)
                let clean = crate::route::strip_echoed_ask(&crate::route::strip_echoed_notes(&context::strip_picture_echo(&vis)), &self.input);
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
                // nothing for the user to read (after the no-think prefill a small model can answer "thanks" with
                // only a state card): ask once more, any provider, then say so rather than show nothing
                let normal_end = cut.is_none() && stop_reason != "cancelled" && stop_reason != "max_tokens";
                if !wrote && normal_end && !retried {
                    retried = true;
                    stashed_card = self.new_card.take();
                    empty_retry = Some(EMPTY_REPLY_NUDGE);
                    self.fe.info(EMPTY_REPLY_LINE);
                    continue;
                }
                if !wrote && cut.is_none() && stop_reason == "max_tokens" && capped.is_some() && !widened {
                    widened = true;
                    self.client.set_simple(false);
                    stashed_card = self.new_card.take().or(stashed_card);
                    self.fe.info(&format!("[the reply used its whole {max_tokens}-token simple-turn budget and wrote no text for you; asking once more without that limit]"));
                    continue;
                }
                // reasoned through its whole budget, no answer: once more without reasoning, full budget
                if !wrote && cut.is_none() && stop_reason == "max_tokens" && reasoning > 0 && self.client.stop_reasoning() {
                    stashed_card = self.new_card.take().or(stashed_card);
                    empty_retry = Some(REASONED_OUT_NUDGE);
                    self.fe.info("[the model spent its whole budget reasoning and wrote no answer; asking once more without reasoning]");
                    continue;
                }
                // a reply that copies an earlier one, has no words or repeats Elisa's headings (`context::ReplyGuard`),
                // or still none after the nudge: the user's words asked once more alone (`isolated_reply`)
                // (and on a turn with picture text, a reply with nothing of what the picture's answer to this ask said)
                let off_pic = |t: &str| self.turn_pic.as_deref().is_some_and(|p| context::off_picture(t, p));
                let why = if !wrote { Some("empty") }
                    else if context::looks_like_text_tool_call(&vis) { None }
                    else { reply_guard.broken(&vis).or_else(|| off_pic(&vis).then_some("off the picture")) };
                // (a copy may run to the output limit: moondream:1.8b's 1,024-token "I hope this email finds you well…")
                let vis = match why.filter(|_| self.tool_lines.is_empty()).filter(|_| if wrote { cut.is_none() && stop_reason != "cancelled" } else { normal_end }) {
                    Some(why) => match self.isolated_reply(max_tokens, &reply_guard) {
                        Some(t) => {
                            self.debug(&format!("[the reply was unusable ({why}); asked once more with the user's words alone]"));
                            if wrote { self.fe.reply_retracted(); }
                            if self.interactive { self.fe.text_delta(&t); }
                            stop_reason = "end_turn".into();
                            t
                        }
                        // nothing usable alone either: a turn with a picture has the model's own words about it
                        // (a reply that was only the picture text is removed as an echo, and asked alone a model may
                        // answer with it again)
                        None => match self.turn_pic.clone() {
                            Some(p) => {
                                self.debug(&format!("[the reply was unusable ({why}); showed the picture's own answer]"));
                                if wrote { self.fe.reply_retracted(); }
                                if self.interactive { self.fe.text_delta(&p); }
                                stop_reason = "end_turn".into();
                                p
                            }
                            None => {
                                if !wrote { self.fe.info("[no answer: the model wrote no text for you twice; ask again or rephrase]"); }
                                vis
                            }
                        },
                    },
                    None => {
                        if !wrote && normal_end { self.fe.info("[no answer: the model wrote no text for you twice; ask again or rephrase]"); }
                        vis
                    }
                };
                // cancelled before any words: the turn did not happen (`drop_cancelled_turn`)
                if !wrote && cut.is_none() && stop_reason == "cancelled" {
                    self.drop_cancelled_turn();
                    return TurnEnd::Stopped;
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
            // `sh`, `shell`, `terminal`: the model meant bash (a small model may call `sh` for "run ls")
            let content: Vec<Block> = content.into_iter().map(|b| match b {
                Block::ToolUse { id, name, input } => {
                    let (name, input) = self.shell_alias(name, input);
                    let input = self.set_by_meaning(&name, input);
                    Block::ToolUse { id, name, input }
                }
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
                self.drop_cancelled_turn();
                return TurnEnd::Stopped;
            }
            // the same call and the same result again (a failing command can be run over and over until the
            // context is full), or the loop guard's stop: the model is asked once to answer with what it has, with no
            // tools (so the turn never ends with no reply); a second stop ends the turn
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

    /// A turn the user cancelled (Esc) with no words of a reply: it leaves the history whole, from the user's message
    /// on (its tool calls and their "cancelled by user" results too), and is not recorded in memory (`unsent`). Never
    /// a "[cancelled]" stand-in reply (a model copies it into the next
    /// unrelated replies, and the /clear notes would keep the cancelled call as the next step). The chat on screen keeps it.
    fn drop_cancelled_turn(&mut self) {
        let at = self.turn_cap(&self.history).min(self.history.len());
        self.history.truncate(at);
        (self.real, self.unsent) = (None, true);
        // nor its words as the session goal
        (self.goal, self.goal_misses, self.goal_stated) = std::mem::take(&mut self.goal_before);
        (self.new_card, self.card_broken) = (None, false);
        self.fe.info("[cancelled]");
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
}

/// Tokens the model read through outline / read_symbol, against what reading those files whole would have cost
/// (each file counted once per session; a directory outline counts on the outline side only). Shown by /context.
#[derive(Default)]
pub(super) struct AstStats { calls: usize, tokens: usize, whole: usize, files: HashSet<PathBuf> }
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
    pub(super) fn line(&self) -> Option<String> {
        if self.calls == 0 { return None; }
        Some(ast_line(self.calls, self.tokens, self.whole, self.files.len()))
    }
}

/// The /context line on the syntax-tree tools: their tokens against reading the same files whole, and which side is
/// cheaper (a ratio under 1 must say "more", never "0.2x fewer").
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
pub(super) struct CheckRun {
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

    /// Records one call's result as fitted to the context (reads cut to a few lines, a new offset each time, would
    /// otherwise never fire `see` before the context is full).
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
pub(super) fn api_error_line(e: &str) -> String {
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
/// The one-time retry after a reply with no visible text (`agent_loop`), and the one request after a reply that
/// reasoned through its whole budget (sent with reasoning off; without it a small model may answer a long pasted
/// message with a copy of an earlier reply): request-only notes (`with_note`),
/// after the user's words or in the last tool result. Never a user message of their own, and never "the user" in
/// the third person (small models mirror that wording back).
const REASONED_OUT_NUDGE: &str = "(Answer this message now, directly, doing what it asks; no long reasoning first.)";
const EMPTY_REPLY_NUDGE: &str = "(Answer this message in one plain sentence first; a <state> card, if any, goes after it.)";
/// the line the person sees when that retry runs: plain words, none of Elisa's own terms (the hidden card is not
/// something the person knows about)
const EMPTY_REPLY_LINE: &str = "[the reply came back empty; asking the model once more]";

/// The note of the one-time words-only retry of a small-talk turn whose reply called a tool (`agent_loop`): after
/// the user's words, in that request only (it never enters the history).
const SMALL_TALK_NOTE: &str = "(Reply in words; no tools for this message.)";
/// the request-only note of the no-tools request after a loop stop (`agent_loop`), in the last tool result
/// (`with_note`); worded as how to answer, never as a reaction to what the model did
const FINAL_ANSWER_NOTE: &str = "(No more tool calls for this reply: answer the user's message in words from the tool results and any document text in the conversation; if something is still missing, say what.)";
/// The note of a turn whose user explicitly asked to search the web (`route::web_ask`; `agent_loop`): after the user's
/// words, in the requests before a tool runs only. Without it (or with a system-prompt rule alone) a small model
/// answers an explicit search request from memory; with it, it calls web_search.
pub(super) const WEB_ASK_NOTE: &str = "\n\n(The user asked for a web search: call web_search now, then answer from its results, even if you know the answer.)";

/// The history with `note` added for one request (a copy; the history is unchanged), always to the last message so
/// the prefix stays the same: after a tool step, inside the last tool result, as Elisa's `[elisa: …]` line like the
/// loop guard's; otherwise after the user's words. Never a user message of its own after the tool results: there it
/// read as the user speaking, and the model answered it ("You're right, I should reframe my response…").
fn with_note(history: &[Message], note: &str) -> Vec<Message> {
    let mut h = history.to_vec();
    let last_result = h.last_mut().filter(|m| m.role == Role::User).and_then(|m| m.content.iter_mut().rev().find_map(|b| match b {
        Block::ToolResult { content, .. } => Some(content),
        _ => None,
    }));
    if let Some(c) = last_result {
        c.push_str(&format!("\n[elisa: {}]", note.trim_start_matches('(').trim_end_matches(')')));
        return h;
    }
    if let Some(m) = h.iter_mut().rev().find(|m| m.role == Role::User) {
        match m.content.iter_mut().find_map(|b| match b { Block::Text { text } => Some(text), _ => None }) {
            Some(t) => { t.push_str("\n\n"); t.push_str(note); }
            None => m.content.push(Block::Text { text: note.into() }),
        }
    }
    h
}

/// The history with this turn's request-only blocks in the turn's user message (a copy for one request): the
/// document text (`doc`) goes with the documents' reference lines, before the user's words and Elisa's notes, and
/// the card (`card`) after them. The words come last, right after the text they ask about (with the
/// words first, mid-chat, a model can take them as about an earlier file and call the attachments "only metadata";
/// the text first, with `docs::ATTACHED_NOTE` after it, is read as the attachments).
fn with_turn_extras(history: &[Message], doc: Option<&str>, card: Option<&str>) -> Vec<Message> {
    let mut h = history.to_vec();
    // the turn's user message (`turn_cap`): the last one with words that are not Elisa's correction
    let Some(at) = h.iter().rposition(|m| m.role == Role::User && m.content.iter()
        .any(|b| matches!(b, Block::Text { text } if !text.starts_with(context::NUDGE_PREFIX)))) else { return h };
    let content = std::mem::take(&mut h[at].content);
    let (refs, rest): (Vec<Block>, Vec<Block>) = content.into_iter().partition(|b| matches!(b, Block::Text { text } if text.starts_with(crate::attach::MARK)));
    let mut out = Vec::new();
    if let Some(d) = doc { out.extend(refs); out.push(Block::Text { text: d.to_string() }); out.extend(rest); } else { out.extend(rest); out.extend(refs); }
    if let Some(c) = card { out.push(Block::Text { text: c.to_string() }); }
    h[at].content = out;
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

/// The history for one request without the corrections of earlier turns: each `[elisa] …` note before the current
/// request (a tool call cut by the output limit, one written as text, a failed check) together with the reply it
/// corrected. The turn that followed holds the real outcome (the write that then succeeded); left in, the old "so no
/// tool in it ran and nothing was saved" read as the outcome in later turns. None when there is none.
fn drop_old_nudges(msgs: &[Message]) -> Option<Vec<Message>> {
    let cur = msgs.iter().rposition(is_request)?;
    let nudge = |m: &Message| m.role == Role::User && !m.content.is_empty()
        && m.content.iter().all(|b| matches!(b, Block::Text { text } if text.starts_with(context::NUDGE_PREFIX)));
    let attempt = |m: &Message| m.role == Role::Assistant && !m.content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
    let drop: Vec<usize> = (1..cur).filter(|&i| nudge(&msgs[i]) && attempt(&msgs[i - 1])).flat_map(|i| [i - 1, i]).collect();
    if drop.is_empty() { return None; }
    Some(msgs.iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, m)| m.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_turn() -> Vec<Message> {
        vec![
            Message::user_text("search the web: weather in Hanoi?"),
            Message::new(Role::Assistant, vec![Block::ToolUse { id: "1".into(), name: "web_search".into(), input: json!({"query": "weather in Hanoi"}) }]),
            Message::new(Role::User, vec![Block::ToolResult { tool_use_id: "1".into(), content: "sunny".into(), is_error: false }]),
        ]
    }

    #[test]
    fn document_text_goes_before_the_users_words_and_the_card_after() {
        // the attached text first, then the words, then the card
        let t = |s: &str| Block::Text { text: s.into() };
        let text = |m: &Message| m.content.iter().map(|b| match b { Block::Text { text } => text.clone(), _ => String::new() }).collect::<Vec<_>>();
        let h = vec![Message::user_text("hi"), Message::assistant_text("hello"),
            Message::new(Role::User, vec![t("what is in these two files?"), t("[Attached: a.xlsx, 2 rows · .elisa/uploads/1/a.xlsx]")])];
        let r = with_turn_extras(&h, Some("[Attached: a.xlsx, 2 rows]\n<untrusted>x</untrusted>"), Some("[state]\ngoal: g\n[/state]"));
        assert_eq!(r[..2], h[..2], "the prefix is unchanged");
        assert_eq!(text(&r[2]), ["[Attached: a.xlsx, 2 rows · .elisa/uploads/1/a.xlsx]", "[Attached: a.xlsx, 2 rows]\n<untrusted>x</untrusted>",
            "what is in these two files?", "[state]\ngoal: g\n[/state]"]);
        // a card alone: after the words and references, as before
        let r = with_turn_extras(&h, None, Some("[state]"));
        assert_eq!(text(&r[2]), ["what is in these two files?", "[Attached: a.xlsx, 2 rows · .elisa/uploads/1/a.xlsx]", "[state]"]);
        assert_eq!(with_turn_extras(&[], Some("d"), None), Vec::<Message>::new());
    }

    #[test]
    fn a_request_note_goes_in_the_last_tool_result_never_a_user_message_after_it() {
        // a note as its own user message after the results reads as the user speaking
        let h = tool_turn();
        let n = with_note(&h, FINAL_ANSWER_NOTE);
        assert_eq!(n.len(), h.len());
        assert_eq!(n[..2], h[..2], "the prefix is unchanged");
        assert!(matches!(&n[2].content[..], [Block::ToolResult { content, .. }] if content.starts_with("sunny\n[elisa: No more tool calls") && content.ends_with("say what.]")), "{:?}", n[2]);
        // no tool turn: after the user's words, as before
        let n = with_note(&[Message::user_text("hey")], SMALL_TALK_NOTE);
        assert!(matches!(&n[0].content[0], Block::Text { text } if text == &format!("hey\n\n{SMALL_TALK_NOTE}")));
    }

    #[test]
    fn earlier_turns_corrections_are_left_out_of_later_requests() {
        // a cut write, its "[elisa] … nothing was saved" note, then the write that worked
        let mut h = vec![
            Message::user_text("write a poem to poem.md"),
            Message::assistant_text("[cut off at the 2048-token output limit: `write poem.md` did not run]"),
            Message::user_text(context::cut_call_nudge("write poem.md", 2048)),
            Message::new(Role::Assistant, vec![Block::ToolUse { id: "w".into(), name: "write".into(), input: json!({"path": "poem.md", "content": "x"}) }]),
            Message::new(Role::User, vec![Block::ToolResult { tool_use_id: "w".into(), content: "wrote 1 lines to poem.md".into(), is_error: false }]),
            Message::assistant_text("Saved."),
        ];
        // still in the turn: the correction stays
        assert!(drop_old_nudges(&h).is_none());
        h.push(Message::user_text("what do you remember?"));
        let v = drop_old_nudges(&h).expect("dropped");
        assert_eq!(v.len(), h.len() - 2);
        assert!(!v.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::Text { text } if text.contains("nothing was saved") || text.contains("did not run"))));
        assert!(v.windows(2).all(|w| w[0].role != w[1].role), "roles still alternate");
        assert!(drop_old_nudges(&tool_turn()).is_none());
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
        // more tokens than reading whole says "more", never "0.2x fewer"; one file is "that file"
        assert_eq!(ast_line(3, 181, 30, 1), "ast: outline + read_symbol 3 calls · 181 tokens vs 30 for reading that file whole · 6.0x more tokens (small files: reading them whole is cheaper)");
        assert_eq!(ast_line(1, 200, 2400, 2), "ast: outline + read_symbol 1 call · 200 tokens vs 2.4K for reading those 2 files whole · 12.0x fewer tokens");
        assert_eq!(ast_line(2, 50, 0, 0), "ast: outline + read_symbol 2 calls · 50 tokens vs 0 for reading those 0 files whole");
    }
}
