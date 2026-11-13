use super::*;
use super::agent_loop::{CheckRun, STOPPED_LINE, WEB_ASK_NOTE};
use super::memory::{is_request, texts};

impl<'f> Session<'f> {

    /// Index of the current user request in `msgs` while a turn runs (the fold keeps it and all after it);
    /// `msgs.len()` between turns.
    pub(super) fn turn_cap(&self, msgs: &[Message]) -> usize {
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
        // the hosted intent router (`router.rs`), when configured: one short call; None = off or a miss (the hardcoded
        // rules decide only when it is off or can't be reached: `own_rules`)
        self.routed = self.ask_router(body);
        let plan = self.routed.as_ref().map(crate::router::plan);
        let rules = self.own_rules();
        let reasoning = mode == crate::mcp_reason::Mode::Auto && plan.as_ref().is_some_and(|p| p.reasoning);
        // its `reasoning` turns the reasoning gate on (the word gate still can, so a multi-step task keeps it; with
        // Elisa's own rules off only the router's answer, or `reason = "always"`)
        let on = if rules { crate::mcp_reason::decide(mode, body, self.reason_on) || reasoning } else { mode == crate::mcp_reason::Mode::Always || reasoning };
        // the same check, whatever `reason` is: a simple turn ("hi", "2+2=?") asks the model not to reason and gets a
        // smaller output cap (config `think_simple`; a follow-up like "ok" keeps the last turn's choice); rules off:
        // a router answer without `reasoning` is simple, a miss is not
        let simple = if rules { crate::mcp_reason::wanted(body).map_or(self.client.simple(), |multi| !multi) } else { plan.as_ref().is_some_and(|p| !p.reasoning) };
        self.client.set_simple(simple && self.cfg.think_simple == "off");
        self.mcp.new_turn();
        // the documents and security servers: only on turns about them (config `gate_tools`), same once-per-turn rule
        // (rules off: the router's groups below, both on a miss, which offers every tool)
        let gate = if rules { crate::mcp_reason::gate(self.cfg.gate_tools == "off", body, &self.recent_text(), self.gate) } else { (plan.is_none(), plan.is_none()) };
        // intent routing (config `route`): the tool groups this turn's words call for (`route::classify`)
        let large_doc = self.docs.iter().any(|d| d.1);
        let doc_text = large_doc || self.doc_req.is_some();
        let others = || -> Vec<String> { self.mcp_names().into_iter()
            .filter(|n| crate::route::tool_group("", Some(n)) == crate::route::OTHER && n != crate::mcp_reason::NAME).collect() };
        let route = match &plan {
            _ if self.cfg.route != "auto" => crate::route::Route::all(),
            // the router's answer: its groups (`route::from_router`); the widen path still repairs a miss
            Some(p) => crate::route::from_router(p, body, &self.route, crate::route::servers_named(body, &others()), doc_text, large_doc),
            // a miss with Elisa's own rules off: no word signals, every tool, and the model decides
            None if !rules => crate::route::Route::router_miss(crate::route::servers_named(body, &others())),
            None => crate::route::classify(body, &self.route, &others()),
        };
        // the documents / security servers follow the router's groups too
        let gate = match &plan { Some(p) => (gate.0 || p.groups.has(crate::route::DOCS), gate.1 || p.groups.has(crate::route::SEC)), None => gate };
        // document text in this turn's message (attached now, or a follow-up's pages): only the tools its own words
        // ask for; one too large to go in whole adds the documents tools (`route::with_documents`)
        let route = if doc_text && rules && route.why != crate::route::Why::Router {
            let names: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
            crate::route::with_documents(route, body, &names, large_doc)
        } else { route };
        // a question pointing at a page Elisa holds ("trang đó dùng để làm gì?" after example.com was read), naming
        // no URL of its own: the page answers it (`agent_loop` leads with her line from it, whatever the router or
        // the words said), so no tool belongs to the turn — a new search would answer from results that are not the
        // page (`route::Why::Held`)
        let route = if self.cfg.route == "auto" && super::agent_loop::points_at_page(body) && crate::web::urls_in(body, 1).is_empty()
            && super::agent_loop::held_page(&self.history).is_some() { crate::route::Route::held_page() } else { route };
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

    /// The model's detected facts (`situation::model_facts`): the model's name, vision, native tool calling, whether it
    /// reasons first, and the window.
    pub(super) fn facts(&self) -> (String, bool, bool, bool, usize) {
        let c = self.client.caps();
        (self.cfg.model.clone(), c.vision == Some(true), c.tools == crate::backend::Tools::Native, c.thinking, self.max_ctx)
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
        if let Some(e) = miss { self.debug(&format!("[router: no answer ({e}); {}]", if self.own_rules() { "word rules" } else { "every tool, no rules" })); }
        a
    }

    /// The router said (Some) whether this turn has `label`; None = no router answer this turn (the word rules decide).
    pub(super) fn router_says(&self, label: &str) -> Option<bool> { self.routed.as_ref().map(|a| a.has(label)) }

    /// The hardcoded rules (word signals, meaning checks) decide this turn: no router in use (none set, or `[router]
    /// decision = "rules"`), or the router can't be reached this turn. With the router reached, only its answer decides.
    pub(super) fn own_rules(&self) -> bool { self.router.rules_on() || self.cfg.route != "auto" }

    /// The router's answer for `label`, else (no answer) Elisa's own `rule` when her rules are on; else false.
    pub(super) fn says_or(&self, label: &str, rule: impl FnOnce() -> bool) -> bool {
        self.router_says(label).unwrap_or_else(|| self.own_rules() && rule())
    }

    /// Routed (`route = "auto"`): the working directory reaches the model only once a turn carries tools. A chat
    /// first turn leaves the `Working directory:` and `OS:` lines out of its header (with it, a model answers
    /// "what can you do?" as a coding helper for "your current project"); the first tool turn after it starts with
    /// those lines instead, so file paths still resolve. The history stays append-only (no rewrite, no cache loss).
    fn route_cwd(&self, text: String) -> String {
        if self.cfg.route != "auto" || self.tiny() { return text; }
        // the OS line goes with it: on a chat turn a model read "OS: macos" as a topic and offered Mac apps unasked
        let line = format!("Working directory: {}\n", self.cwd.display());
        let os = format!("OS: {}\n", std::env::consts::OS);
        let sent = self.history.iter().any(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if text.contains(&line))));
        let has = text.starts_with(&format!("{line}{os}"));
        match (self.route.groups.tool_free() && self.route.others.is_empty(), has, sent) {
            (true, true, _) => text[line.len() + os.len()..].to_string(),
            (false, false, false) => format!("{line}{os}\n{text}"),
            _ => text,
        }
    }

    /// A line only in debug mode (environment `ELISA_DEBUG=1`): routing decisions and the like.
    pub(super) fn debug(&mut self, line: &str) {
        if std::env::var("ELISA_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0") { self.fe.info(line); }
    }

    /// A call to a session tool the route left out of this request (`route.rs`): the route widens to that tool's
    /// group (one tool-list change) and the call goes on to the usual approval, so a wrong guess never fails the turn.
    /// False for anything else (a tool the session does not have, the reasoning tools, `route = "off"`).
    pub(super) fn widen_for(&mut self, name: &str) -> bool {
        if self.cfg.route != "auto" || self.mcp.reason_tool(name) { return false; }
        // small talk or a question about what Elisa can do (`route::Why::Talk`), or about a page she holds
        // (`Why::Held`): no tool belongs to the turn, so a call there widens nothing ("I went for a run this morning
        // and now I'm starving" ran `starving` in bash)
        if matches!(self.route.why, crate::route::Why::Talk | crate::route::Why::Held) { return false; }
        let srv = self.mcp.server_of(name).map(str::to_string);
        let known = srv.is_some() || tools::defs_for(self.cfg.ast).iter().any(|d| d.name == name);
        if !known { return false; }
        let g = crate::route::tool_group(name, srv.as_deref());
        // (Elisa's own change call, asked for by the words: a read-only turn's file tools whole again; the model's
        // change call on such a turn never gets here, `route::change_kept_out`)
        if crate::route::changes_files(name, srv.as_deref()) { self.route.groups = crate::route::Groups(self.route.groups.0 & !crate::route::READ_ONLY); }
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
    pub(super) fn turn(&mut self, input: String) -> TurnEnd {
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
        self.own_results.clear();
        self.turn_guard = Default::default();
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
        // (a cancelled turn with no reply left the history and is not recorded at all: `drop_cancelled_turn`)
        let said_no = self.tool_lines.iter().any(|l| l.contains("→ denied by user") || l.contains("→ cancelled by user"));
        if !self.unsent && !refused {
            let mut reply = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default().trim().to_string();
            // a reply that called no tools (plain chat) is not expected to carry a state card
            let used_tools = !self.tool_lines.is_empty();
            for l in std::mem::take(&mut self.tool_lines) { reply += &format!("\n{l}"); }
            // the user's words are kept; the answer only when it came from tools that ran (a plain-chat answer is the
            // model's guess: "You are Johnathan…", a wrong "9"), and never a refusal, a failed tool's or an error
            // turn's (an empty turn keeps the exchange's place in memory and is never recalled)
            if !used_tools || !matches!(end, TurnEnd::Done) || said_no || context::unfit_memory(&reply) { reply.clear(); }
            let ids: Vec<u64> = [self.record(MRole::User, &format!("{input}{attached}")), self.record(MRole::Assistant, reply.trim())].into_iter().flatten().collect();
            self.exchanges += 1;
            // a card is the next turn's instructions: nothing in it may come from outside text (`context::Outside`)
            let o = context::Outside::of(&self.history);
            let card = self.new_card.take().map(|c| if o.is_empty() { c } else { context::quarantine_card(&c, &o) }).filter(|c| !c.trim().is_empty());
            match card {
                // plain chat carries no card (a card from a no-tool turn would be carried and repeated); a turn
                // about an attached document keeps its card: the document's topic card (`topics.rs`)
                Some(c) if used_tools || !attached.is_empty() => self.set_card(c, &ids),
                None if self.card_broken && used_tools && matches!(end, TurnEnd::Done) => {
                    let keep = self.card_id.map_or("none yet".to_string(), |k| format!("keeping card #{k}"));
                    // internal: never in the chat
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
        // a clear tool ask ("read notes.txt", "run the command: …", "search the web: …"): the calls Elisa makes herself
        // before the model's reply (`own_asks`), found now so a tool left out of the route is offered before the
        // message is built (its working-directory line goes with it)
        let own = self.own_asks(&input);
        // a window too small for tools: her own steps reach the model as data after the user's words
        self.client.set_fold_own(self.tiny());
        // recalled memories go in the USER message (the system prompt stays byte-stable); only `input` is recorded
        let mut lines = if self.doc_refused.is_some() { Vec::new() } else { self.recall(&input) };
        if !lines.is_empty() { self.fe.recalled(&lines); }
        // the first turn of a resumed session: what the user told about themself, in the third person, with the
        // recalled lines (resumed, a model may no longer
        // read the old turns' "my name is Linh" as the user's and answer with its own name)
        if std::mem::take(&mut self.resumed_facts) { lines.extend(self.resumed_facts()); }

        let text = if self.history.is_empty() && self.tiny() {
            context::first_message_bare(self.first_notes().as_deref(), self.state_block().as_deref(), &lines, &input)
        } else if self.history.is_empty() {
            context::first_message_full(&self.cwd, self.first_notes().as_deref(), self.state_block().as_deref(), &lines, &input)
        } else {
            context::with_memory(&lines, &input)
        };
        let mut text = self.route_cwd(text);
        // a greeting or small talk opening a chat: no clock line in its header (granite4.2 answered "chào bạn" by
        // reading out the date and time, with a wrong day); a time ask gets it back below (`route::TIME`)
        // (a time ask by its wordings too, `situation::asks_time`: whatever the route, it never loses the clock)
        let own_now = context::own_words(context::request_body(&input)).unwrap_or(&input).to_string();
        // (or a place named right after a time turn: "còn ở Tokyo thì sao?" after "mấy giờ rồi?")
        let after_time = || self.history.iter().rev().find(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. })))
            .is_some_and(|m| m.content.iter().any(|b| matches!(b, Block::Text { text } if crate::situation::asks_time(context::own_words(context::request_body(text)).unwrap_or(text))
                || text.starts_with("Time in ") || text.contains("\nTime in "))));
        let time_ask = self.route.groups.has(crate::route::TIME) || crate::situation::asks_time(&own_now)
            || crate::route::place_after_time(&self.route, &own_now, after_time);
        if self.history.is_empty() && !time_ask
            && (self.says_or("greeting", || crate::route::greeting(&input)) || crate::route::small_talk(&input)) {
            text = crate::situation::without_date_line(&crate::situation::without_clock_line(&text));
        } else if !self.history.is_empty() && !time_ask && !crate::route::small_talk(&input) && !crate::route::greeting(&input) {
            // the date with the first turn after a greeting opener that is not small talk (only a time ask got the
            // clock since), so the model still knows today's date
            let sent = |t: &str| t.starts_with("Date: ") || t.contains("\nDate: ") || t.starts_with("Local time now: ") || t.contains("\nLocal time now: ");
            let dated = self.history.iter().filter(|m| m.role == Role::User).flat_map(|m| &m.content).any(|b| matches!(b, Block::Text { text } if sent(text)));
            if !dated {
                // one header block: after the working directory / OS lines when the turn has them
                let date = format!("Date: {}", context::local_date());
                text = match text.split_once("\n\n").filter(|(h, _)| context::is_header_block(h)) {
                    Some((h, rest)) => format!("{h}\n{date}\n\n{rest}"),
                    None => format!("{date}\n\n{text}"),
                };
            }
        }
        // a turn about the time / date: the local clock as one line (the first message's header has it already), so
        // the model answers directly, conversions included, with no tool (the time server only through the widen path)
        if time_ask {
            // before the user's words (after them, a model redoes the offset arithmetic and can get
            // it wrong; right after the local line it reads the answer off)
            // (a what_about's new subject from the router: "and in Osaka?" → its clock line)
            let topic = self.routed.as_ref().and_then(|a| a.topic.clone()).unwrap_or_default();
            let lines = crate::route::time_lines(&format!("{input} {topic}"));
            // the first message's header has a local line (written a moment ago, maybe a minute behind): these lines
            // take its place
            let at = if text.starts_with("Local time now: ") { Some(0) } else { text.find("\nLocal time now: ").map(|i| i + 1) };
            match at {
                Some(i) => { let end = text[i..].find('\n').map_or(text.len(), |j| i + j); text.replace_range(i..end, &lines); }
                None => text = format!("{lines}\n\n{text}"),
            }
        }
        // a session mixing English and Vietnamese: a short hint after the user's words (`reply_lang.rs`; otherwise
        // a turn in English after a Vietnamese one may be answered in Vietnamese)
        let earlier = self.history.iter().filter(|m| m.role == Role::User).flat_map(|m| &m.content)
            .filter_map(|b| match b { Block::Text { text } => Some(context::request_body(text)), _ => None });
        // a small-talk turn with context: one situation line right before the user's words (`situation.rs`), in this
        // last user message only, so the cached prefix (system, tools, history) is unchanged; no context, no line
        // a short follow-up ("why?", "more", "the second one") after an answer: one hint line there instead
        // (`followup.rs`); a new session gets none
        let resume_hint = resumed.map(|r| match r {
            crate::route::Resume::Continue if self.last_failed => crate::followup::RESUME_RETRY.to_string(),
            // a task (its last turn ran tools): go on with it; a plain chat: recap it and offer to go on (a plain
            // "continue" after a chat ending in "just reply OK" turns gets a bare "OK.")
            crate::route::Resume::Continue if crate::situation::last_turn_flags(&self.history).0 => crate::followup::RESUME_CONTINUE.to_string(),
            crate::route::Resume::Continue | crate::route::Resume::Recap => {
                let asks: Vec<String> = self.history.iter().filter(|m| m.role == Role::User).flat_map(|m| &m.content)
                    .filter_map(|b| match b { Block::Text { text } if !text.starts_with(crate::attach::MARK) => Some(context::request_body(text).to_string()), _ => None }).collect();
                crate::followup::resume_recap(&asks)
            }
        });
        // what Elisa can do on this model, from the detected caps (a fixed list would claim images with no vision)
        let vision = self.client.caps().vision == Some(true);
        let about = self.says_or("about_elisa", || crate::route::about_elisa(&input)).then(|| crate::situation::about(vision));
        // a question about the model itself ("What can this model do for me?"): its detected facts (a meaning check:
        // Elisa's own rules only)
        let about = about.or_else(|| {
            if !self.own_rules() { return None; }
            let (m, v, t, th, ctx) = self.facts();
            let own = context::own_words(context::request_body(&input)).unwrap_or(&input).to_string();
            crate::situation::asks_about_model(&m, v, t, th, ctx, &own).then(|| crate::situation::about_model(&m, v, t, th, ctx))
        });
        // (a tiny window gets none: a small model there read such lines back as its answer)
        let hint = if self.tiny() { None } else { resume_hint.or(about).or_else(|| self.situation(&input)).or_else(|| self.follow_up_hint(&input)) };
        // what Elisa works out herself from the user's words (`situation::turn_facts`): arithmetic computed, the real
        // part of the day, a facts-only message acknowledged
        let own_words = context::own_words(context::request_body(&input)).unwrap_or(&input).to_string();
        // a number question in words (`mcp_reason::word_problem`): one extra call for the expression alone, checked
        // against the message and computed by Elisa; chat turns only, never in a tiny window, and only where its line
        // can go
        // (a tiny window too: the result is a few tokens, and a small model there needs it most)
        let wp = if !self.route.groups.tool_free() || !own.is_empty()
            || hint.as_deref().is_some_and(|h| !h.starts_with(crate::situation::PREFIX)) { None } else {
            CANCEL.store(false, Ordering::SeqCst);
            let client = &self.client;
            let wp = crate::mcp_reason::word_problem(&own_words, |q| {
                let msgs = [Message::user_text(q.to_string())];
                let no_tools: [ToolDef; 0] = [];
                let req = Request { system: crate::mcp_reason::EXPR_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: crate::mcp_reason::EXPR_MAX_TOKENS };
                client.set_kind("agent");
                client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled").map(|r| texts(&Message::new(Role::Assistant, r.content)))
            });
            // Stop during that call ends the turn: nothing was sent for the user yet
            if CANCEL.load(Ordering::SeqCst) {
                self.unsent = true;
                self.fe.info(STOPPED_LINE);
                return TurnEnd::Stopped;
            }
            wp
        };
        // and the facts of the turn's table that the words ask for (`table_part`)
        let extra = wp.map(|w| w.part()).into_iter().chain(self.table_part(&own_words)).reduce(|a, b| format!("{a} · {b}"));
        // a tiny window: Elisa's exact facts only (`situation::exact_facts`), no other line
        let hint = if self.tiny() { crate::situation::with_exact_facts(&own_words, crate::situation::now_secs(), extra) }
            else { crate::situation::with_turn_facts_plus(hint, &own_words, crate::situation::now_secs(), extra) };
        if let Some(l) = hint {
            if text.ends_with(input.as_str()) { let at = text.len() - input.len(); text.insert_str(at, &format!("{l}\n\n")); }
        }
        // an explicit "search the web: …" or "read notes.txt" with that tool offered: one line after the user's words,
        // in this turn's own message (so every request of the turn is byte-identical up to it), stripped from saved
        // views by `context::request_body` (without it a
        // model answers from memory instead of searching, or says it doesn't see the file)
        let lang_hint = crate::reply_lang::hint(earlier, &input);
        // a picture the model cannot see: Elisa says so herself, so no model invents one
        if let Some(line) = self.no_picture_line(&input) {
            self.input = input;
            if self.interactive { self.fe.text_delta(&line); }
            self.history.push(Message::user_text(text));
            self.history.push(Message::assistant_text(line));
            return TurnEnd::Done;
        }
        // a clear ask for a tool this session cannot offer (the code runner off, a window too small for tools): Elisa
        // says so herself, so no model guesses the output ("run this python: …" → a made-up "55")
        if let Some(line) = self.unoffered_line(&own, &input) {
            self.input = input;
            if self.interactive { self.fe.text_delta(&line); }
            self.history.push(Message::user_text(text));
            self.history.push(Message::assistant_text(line));
            return TurnEnd::Done;
        }
        // the language line before the ask's own line: what the turn must do is the last thing a small model reads
        // (after "(Reply in English.)" a model answered "search the web: …" from what it knew, with no search)
        if let Some(h) = lang_hint { text.push_str(h); }
        // (an ask Elisa runs herself gets no note: its result is in the request, and a small model copied the note)
        let note = if own.iter().all(|o| matches!(o, Own::Unoffered(_))) { self.ask_note(&input) } else { None };
        if let Some(n) = self.doc_refused.clone().or(note).or_else(|| self.unknown_fact_note(&input, &lines)) { text.push_str(&n); }
        // a small-talk turn routed as chat runs no tool: a call there is asked again in words (`agent_loop`)
        // (a bare "ok" too, unless it answers an offer: the last answer ended with a question)
        let offer = self.history.iter().rev().find(|m| m.role == Role::Assistant).is_some_and(|m| texts(m).trim_end().ends_with('?'));
        let small = match &self.routed {
            // the router: a greeting or a closing (a bare "ok" to an offer is a yes: closing only without one)
            Some(a) => a.has("greeting") || (a.has("closing") && !offer),
            // (the rules' route says so too: small talk by meaning, or a question about what Elisa can do)
            None => self.own_rules() && (self.route.why == crate::route::Why::Talk || crate::route::small_talk(&input) || (crate::route::ack(&input) && !offer)),
        };
        // ("lại lần nữa đi" right after a call of her own is that call once more, never small talk: `repeat_asked`)
        let again = own.iter().any(|o| matches!(o, Own::Call(..))) && self.repeat_asked(context::request_body(&input)).is_some();
        self.small_talk_turn = self.cfg.route == "auto" && self.route.groups.tool_free() && self.route.others.is_empty() && small && !again;
        self.input = input;
        // pictures: asked about alone first (`look`), the answer kept as text; the turn then runs as text only
        let (mut picture, mut pic_answer) = (None, None);
        self.turn_pic = None;
        if let Some((pics, new)) = self.turn_pictures(&self.input.clone()) {
            match self.look(&self.input.clone(), &pics, new) {
                Ok(Some((block, answer))) => {
                    // what the picture showed, for telling whether the next turn is about it (`about_last_pictures`)
                    if let (true, Some(a)) = (new, &answer) { self.last_pic_seen = format!("{}\n{a}", self.last_pic_seen); }
                    self.turn_pic = answer.clone();
                    picture = Some(block);
                    pic_answer = answer.filter(|_| new);
                }
                Ok(None) => {}
                Err(end) => return end,
            }
        }
        // picture text belongs to the turn that looked: earlier turns' is dropped before this turn's message is added
        // (kept, it sticks to later turns and small models copy it into replies)
        if context::drop_picture_blocks(&mut self.history) { self.debug("[earlier picture text dropped from the history]"); }
        let mut content = vec![Block::Text { text }];
        content.extend(picture.map(|text| Block::Text { text }));
        // attached documents after the user's words, each its own block (`attach::MARK` first: notes keep only that line)
        content.extend(self.docs.drain(..).map(|(text, _)| Block::Text { text }));
        self.history.push(Message::new(Role::User, content));
        // tests: a request containing $ELISA_TEST_PANIC panics here (the recovery path, tests/fake_server.rs)
        if std::env::var("ELISA_TEST_PANIC").is_ok_and(|m| !m.is_empty() && self.input.contains(&m)) { panic!("injected test panic"); }
        // "show the full list" after a dependency list cut short: Elisa's own list, with no model call
        if let Some(line) = self.full_list_line() {
            if self.interactive { self.fe.text_delta(&line); }
            self.history.push(Message::assistant_text(line));
            return TurnEnd::Done;
        }
        // a short follow-up on a file Elisa holds ("and South?" after a question about sales.csv, "và cái kia?" after
        // the first task of todo.txt): her own answer from the file, with no model call (`held_line`)
        if own.is_empty() && !self.small_talk_turn {
            // (shown with no gate, so with the one tidy every shown text gets: a full stop after the row's own goes)
            if let Some(line) = self.held_line(context::request_body(&self.input), true).map(|t| crate::reply_check::without_stray_marks(&t).unwrap_or(t)) {
                if self.interactive { self.fe.text_delta(&line); }
                self.history.push(Message::assistant_text(line));
                self.debug("[a short follow-up on a file read before; Elisa answered from it]");
                return TurnEnd::Done;
            }
        }
        // Elisa's own calls run first, through the same approval; the model answers from their results
        let (ran, saved) = if own.is_empty() || self.small_talk_turn { (false, None) } else {
            match self.run_own(own) {
                Ok(r) => r,
                // Elisa's own line ends the turn (a result too long for the window, nothing to save)
                Err(line) => {
                    if self.interactive { self.fe.text_delta(&line); }
                    self.history.push(Message::assistant_text(line));
                    return TurnEnd::Done;
                }
            }
        };
        // "giá vàng hôm nay thế nào nhỉ" after her own search whose snippets hold no figure for it: she reads the top
        // pages herself, so the model answers from a page that has the figure (`fetch_for_live_figure`)
        if ran { self.fetch_for_live_figure(); }
        // "show me the full contents of config.toml": the file Elisa just read is the answer, as she read it (a
        // secret cut short, as everywhere on screen); no model writes it again or reads it once more
        // a yes/no "is todo.txt still there?" after her own look (`exists_answer`), or a save that was the whole ask
        // (`saved_line`): Elisa's own line is the answer
        // (a symbol shown as it reads: `shown_symbol`)
        let own_line = ran.then(|| self.shown_file().or_else(|| self.shown_symbol()).or_else(|| self.asked_value()).or_else(|| self.exists_answer())
            .or_else(|| saved.as_ref().and_then(|(f, c)| self.saved_line(f, c)))).flatten();
        if let Some(line) = own_line {
            if self.interactive { self.fe.text_delta(&line); }
            self.history.push(Message::assistant_text(line));
            return TurnEnd::Done;
        }
        // Elisa's own calls did what the words ask (a change she made herself, or nothing to change; and on a model
        // that writes its calls as text, or with her change or save done): the turn's answer requests offer no tools
        // (`answer_only`), the first with a note that the steps ran (`OWN_DONE_NOTE`), so the model words the answer
        // instead of writing the call again
        // (a command Elisa ran on the user's words, "delete todo.txt with rm", is done too: the reply says it was)
        let changed = self.tool_lines.iter().any(|l| ["[write ", "[edit ", "[edit_symbol ", "[bash "].iter().any(|p| l.starts_with(p)) && !l.contains(" → "));
        let words_only = ran && !self.tiny() && self.retry_note.is_none() && (changed || !asks_change(context::request_body(&self.input)))
            && (self.client.caps().tools != crate::backend::Tools::Native || changed);
        if words_only { (self.retry_note, self.answer_only) = (Some(OWN_DONE_NOTE.to_string()), true); }
        // after her own outline, a reply is held to its names as it streams (`api::set_reply_names`)
        let names = self.turn_results().into_iter().filter(|r| r.name == "outline" && !r.is_error)
            .filter_map(|r| r.input["path"].as_str().and_then(|p| tools::outline_syms(p, &self.tctx))).flatten().map(|(n, _, _)| n).collect::<Vec<_>>();
        self.client.set_reply_names(if ran && self.own_ran("outline") { names } else { Vec::new() });
        let end = self.agent_loop(CheckRun::default(), !ran);
        self.client.set_reply_names(Vec::new());
        if words_only { (self.retry_note, self.answer_only) = (None, false); }
        // a reply that wrote a tool and its argument as text with no call ("`read` /etc/hosts" and made-up contents):
        // that call runs, and the model answers from its result (`text_named_call`)
        let end = if matches!(end, TurnEnd::Done) { self.text_named_call().unwrap_or(end) } else { end };
        // a change asked for in a named file that no edit made: Elisa makes it (`own_fix`)
        let mut end = if matches!(end, TurnEnd::Done) { self.own_fix().unwrap_or(end) } else { end };
        let mut retried = false;
        for k in 0..4 {
            if retried || !matches!(end, TurnEnd::Done) { break; }
            let r = match k { 0 => self.doc_refusal_retry(), 1 => self.denied_claim_retry(), 2 => self.edit_claim_retry(), _ => self.claimed_facts_retry() };
            if let Some(e) = r { (end, retried) = (e, true); }
        }
        // Elisa's own notes never reach the reply; tools ran and the model gave no answer: Elisa gives their results
        self.strip_internal_marks();
        self.answer_from_tools(&mut end);
        // (a turn a loop or a limit stopped after one of Elisa's own checks ran still answers with her verified result)
        // (a reply the output limit cut short is held to the results like a finished one: a figure or date it gives
        // that no result holds never stands for being cut — a run of invented dated prices ending mid-list)
        if matches!(end, TurnEnd::Stopped) { self.deps_reply_fix(); self.check_reply_fix(); self.edit_reply_fix(); self.weather_claim_fix(); self.live_figure_fix(); }
        if matches!(end, TurnEnd::Done) { self.listed_fix(); self.outline_fix(); self.grep_fix(); self.fragment_fix(); self.command_reply_fix(); self.count_reply_fix(); self.item_reply_fix(); self.weather_claim_fix(); self.live_figure_fix(); self.symbol_check_fix(); self.said_no_fix(); self.edit_reply_fix(); self.field_fix(); self.deps_reply_fix(); self.check_reply_fix(); self.asked_value_fix(); }
        if retried { return end; }
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
    pub(super) fn shell_alias(&self, name: String, input: serde_json::Value) -> (String, serde_json::Value) {
        let alias = matches!(name.to_lowercase().as_str(), "sh" | "shell" | "terminal" | "zsh" | "run_shell" | "shell_command");
        if !alias || self.mcp.owns(&name) || self.tools.iter().any(|t| t.name == name) { return (name, input); }
        let cmd = ["command", "cmd", "script", "code", "input", "commands"].iter().find_map(|k| input.get(*k).and_then(|v| v.as_str()).map(str::to_string))
            .or_else(|| input.as_str().map(str::to_string)).unwrap_or_default();
        ("bash".to_string(), json!({"command": cmd}))
    }

    /// A no-tool question about the user's own X ("what's my favourite colour?") that nothing this turn carries
    /// answers: memory was searched (`recalled`: what it found) and the notes, pinned facts and conversation hold no
    /// word of X. The note says so, so the answer is "you haven't told me" (`situation::unknown_fact_note`).
    fn unknown_fact_note(&self, input: &str, recalled: &[String]) -> Option<String> {
        if !self.route.groups.tool_free() || !self.own_rules() { return None; }
        let subject = crate::situation::own_fact_asked(input)?;
        let mut known = recalled.join("\n");
        for t in [self.first_notes(), self.state_block()].into_iter().flatten() { known += "\n"; known += &t; }
        // (an earlier asking of the same thing tells nothing: only what was said)
        for m in self.history.iter().map(texts).filter(|t| crate::situation::own_fact_asked(t).is_none()) { known += "\n"; known += &m; }
        let known = known.replace(input.trim(), " ");
        (!crate::situation::subject_known(&subject, &known)).then(|| crate::situation::unknown_fact_note(&subject))
    }

    /// A call to one of Elisa's own MCP tools with its "true for …" switches set from the user's words
    /// (`mcp_args::set_by_meaning`): web_search's `recent` for an ask about news or the latest, whatever the model sent.
    pub(super) fn set_by_meaning(&mut self, name: &str, input: serde_json::Value) -> serde_json::Value {
        if !self.mcp.builtin_owns(name) || !self.own_rules() { return input; }
        let Some(def) = self.tools.iter().find(|t| t.name == name) else { return input };
        let (out, set) = crate::mcp_args::set_by_meaning(&def.input_schema, &input, context::request_body(&self.input));
        if !set.is_empty() { self.debug(&format!("[{name}: {} set by Elisa from the ask]", set.join(", "))); }
        out
    }

    fn ask_note(&self, input: &str) -> Option<String> {
        let has = |n: &str| self.tools.iter().any(|t| t.name == n);
        // the router's answer: its web_search_now / save_file / read_file (and `file`) instead of the word checks
        // (the file name from the words only when Elisa's own rules are on)
        let rules = self.own_rules();
        if let Some(a) = &self.routed {
            if a.has("web_search_now") && has("web_search") { return Some(WEB_ASK_NOTE.to_string()); }
            if a.has("save_file") && has("write") {
                if let Some(f) = a.file.clone().or_else(|| crate::route::save_ask(input).filter(|_| rules)) {
                    return Some(format!("{}to save it to {f}: call write now with the full content; never say it is saved before write succeeds.)", crate::route::ASK_PREFIX));
                }
            }
            if a.has("read_file") && has("read") {
                let f = a.file.clone().or_else(|| crate::route::file_ask(input).filter(|_| rules))?;
                return Some(format!("{}to read {f}: call read on it now, then answer from what it returns; never guess what it says.)", crate::route::ASK_PREFIX));
            }
            return None;
        }
        if !rules { return None; }
        if crate::route::web_ask(input) && has("web_search") { return Some(WEB_ASK_NOTE.to_string()); }
        if let Some(f) = crate::route::save_ask(input).filter(|_| has("write")) {
            // without it, "save that plan to plan.md" can get "I've saved it" with no write call
            return Some(format!("{}to save it to {f}: call write now with the full content; never say it is saved before write succeeds.)", crate::route::ASK_PREFIX));
        }
        let f = crate::route::file_ask(input).filter(|_| has("read"))?;
        Some(format!("{}to read {f}: call read on it now, then answer from what it returns; never guess what it says.)", crate::route::ASK_PREFIX))
    }

    /// The calls a clear ask makes Elisa run herself this turn, before the model's reply (any model then answers from
    /// real results instead of being asked to call, which small models ignore and answer from memory). Routed turns
    /// only (`route = "auto"`), not on a turn about an attached document. The decision part says what is asked: the
    /// router's labels (web_search_now, read_file, save_file), and beside them the user's own words by their meaning
    /// where the label agrees (a routed "web" turn with "could you look it up?" still searches; a "files_code" turn
    /// with "chạy lệnh: echo hi" still runs it; a "chat" turn runs nothing), or wherever the hardcoded rules decide:
    /// the words against the tools' own names and descriptions (`described_call`), a URL (web_fetch), an explicit
    /// web search or a question only today's web answers (`route::web_ask`), a command (`shell_command`,
    /// `command_ask`), a change of quoted or framed words (`quoted_edit`), a search of the project's files for a word
    /// (`search_ask`), a named file to read (`route::file_ask`, `read_ask` by meaning, or a file of the project the
    /// words ask about), "again" right after her own call (`repeat_asked`; a follow-up label of the router agrees).
    /// A named new file to save to (`route::save_ask`, `save_target`, or the text given for it: `given_text`) comes
    /// last: its content is the model's, in words, after the reads (`run_own`). The same call asked twice (the
    /// router's search and the words') runs once. A web call never comes from the words on a turn the web is kept out
    /// of (`route::web_kept_out`: a plain statement, a files turn naming files). A tool the route left out is offered
    /// first (`widen_for`); one the session does not have is no call.
    fn own_asks(&mut self, input: &str) -> Vec<Own> {
        if self.cfg.route != "auto" || self.doc_refused.is_some() || !self.docs.is_empty() || self.doc_req.is_some() { return Vec::new(); }
        let body = context::request_body(input).to_string();
        // "again" right after a call of her own ("run the command: echo hello-elisa", then "again"): that call once
        // more (`repeat_asked`)
        let repeat = self.repeat_asked(&body);
        // (small talk, or a question about what Elisa can do, by the rules' route: no ask in it, `route::Why::Talk`;
        // only "lại lần nữa đi" right after her own call is a follow-up on that call: `repeat_asked`)
        // (a question about a page Elisa holds, `Why::Held`: the page answers it, no call)
        if matches!(self.route.why, crate::route::Why::Talk | crate::route::Why::Held) && repeat.is_none() { return Vec::new(); }
        let rules = self.own_rules();
        // with the decision model on, its label is the one decision for the message: the words' own run only where
        // the label agrees, the web tools on a web label, the file, shell and code tools on files_code
        // (`router::plan`), nothing on chat, greeting, closing or about_elisa ("can you search the web?" is a
        // question about her, not a search); with no answer, or the router off, the rules decide
        let labelled = self.routed.as_ref().filter(|_| !rules).map(|a| crate::router::plan(a).groups);
        // ("again" right after her own call repeats it under any label, chat included: a bare "lại đi" reads as chat
        // to the router; `repeat_asked` finds nothing when the last turn ran no call of hers)
        let again = repeat.is_some();
        let words = rules || labelled.is_some_and(|g| g.has(crate::route::WEB | crate::route::CODE)) || again;
        // the web is kept out of this turn (a plain statement, "My name is Linh and I live in Hanoi."; a files turn
        // whose words name a file, "tim giup minh file nao co chu ORCHID": `route::web_kept_out`): no web call of
        // Elisa's own from the words, whatever they come near in meaning, as the model's own is refused
        let web_out = crate::route::web_kept_out(&self.route, &body);
        let mut calls: Vec<(String, serde_json::Value)> = Vec::new();
        // a save the words name ("save that plan to plan.md": `route::save_ask`; "lưu một dòng tóm tắt notes.txt vào
        // file tomtat.md", "jot down 'call Minh at 3pm' in reminders.txt": `save_target`), where the rules decide or
        // the label agrees (a files_code or read_file turn naming a source and a target file is a read and a write)
        let save_named = || crate::route::save_ask(&body).or_else(|| save_target(&body));
        let save = match &self.routed {
            Some(a) => a.has("save_file").then(|| a.file.clone().or_else(|| save_named().filter(|_| rules))).flatten()
                .or_else(|| (rules || labelled.is_some_and(|g| g.has(crate::route::CODE))).then(save_named).flatten()),
            None if rules => save_named(),
            None => None,
        };
        // a save the word rules miss that gives its text to a new file, by meaning ("lưu từ hello vào a.txt", "it'd be
        // nice to have hello.md with just the word hello in it": `given_text`), or that saves something said before with
        // a saving word of the examples ("lưu kế hoạch đó vào plan.md": `back_save`)
        let save = save.or_else(|| words.then(|| crate::route::file_name(&body)).flatten()
            .filter(|f| !self.cwd.join(f).exists() && (given_text(&body, f).is_some() || back_save(&body, f))));
        let save = save.filter(|f| !self.cwd.join(f).exists() && labelled.is_none_or(|g| g.has(crate::route::CODE)));
        // an ask for the shell ("use the shell to write hello into .elisa/u06.txt"): the shell does it, never a write
        // of Elisa's own beside it (`shell_ask`)
        let shell = words && shell_ask(&body);
        let save = save.filter(|_| !shell);
        let read = |p: &str| ("read".to_string(), json!({"path": p}));
        let bash = |c: String| ("bash".to_string(), json!({"command": c}));
        let topic = self.web_topic(&body);
        let query = |b: &str| ("web_search".to_string(), json!({"query": explicit_span(b).map_or(topic.clone(), |s| s.0), "recent": false}));
        if let Some(a) = &self.routed {
            if a.has("web_search_now") { calls.push(query(&body)); }
            // (only a slot this message names or points back at, `route::slot_named`: never one left from an earlier turn)
            if let Some(f) = a.file.clone().filter(|f| a.has("read_file") && Some(f) != save.as_ref() && crate::route::slot_named(&body, f)) { calls.push(read(&f)); }
        }
        if words {
            let url = body.split_whitespace().map(|t| t.trim_matches(|c: char| "\"'`<>()[],;".contains(c)).trim_end_matches(['.', '!', '?']))
                .find(|t| t.starts_with("http://") || t.starts_with("https://")).map(str::to_string);
            // a command the words name with a file of the project ("delete todo.txt with rm", "use wc"): that
            // command on it (bash asks first, as always)
            // a quoted old → new change in a named file ("in todo.txt change 'buy milk' to 'buy bread'", "đổi 'buy milk'
            // thành 'buy bread' trong todo.txt"): the edit itself, with the usual approval (`quoted_edit`); unquoted
            // words by a frame of `data/edit_asks.txt` too ("in e1.txt change hello to goodbye")
            // a command the words give for the shell ("run python3 --version in the shell"): that command, as written
            // (`shell_command`); an ask for the shell naming only the program to use ("how much disk space is free
            // here? use df") runs that program (`command_ask`); one with no command in it goes to the model with the
            // shell, and a command it writes as text runs (`text_named_call`)
            if let Some(c) = repeat.clone() {
                // (a short "lại lần nữa đi" reads as small talk to the route, which then offers no tool: after her own
                // call it is a follow-up on that call)
                if self.route.why == crate::route::Why::Talk { self.route.why = crate::route::Why::FollowUp; self.debug("[\"again\" right after Elisa's own call: a follow-up on it, not small talk]"); }
                calls.push(c);
            }
            else if let Some(cmd) = shell.then(|| shell_command(&body)).flatten() { calls.push(bash(cmd)); }
            else if shell { if let Some(cmd) = self.command_ask(&body) { calls.push(bash(cmd)); } }
            else if let Some(c) = self.quoted_edit(&body) { calls.push(c); }
            else if let Some(c) = code_ask(&body) { calls.push(c); }
            else if let Some(cmd) = self.command_ask(&body) { calls.push(bash(cmd)); }
            // "show me just the add function in app.py", "show me the add function again": Elisa reads the
            // symbol herself and shows its real lines (`shown_symbol`)
            else if let Some((f, sym)) = self.symbol_shown(&body) { calls.push(("read_symbol".to_string(), json!({"path": f, "name": sym}))); }
            // "tìm giúp mình file nào có chữ ORCHID", "tim giup minh file nao co chu ORCHID", "which file mentions
            // ORCHID?": a search of the project's files for that word (`search_ask`), in any spelling, never the web
            // (or the thing a which-file ask names, "which file in this folder mentions a meeting code?":
            // `route::which_file_term`, searched in any case)
            else if let Some(pattern) = search_pattern(&body) { calls.push(("grep".to_string(), json!({"pattern": pattern}))); }
            else if let Some(c) = self.described_call(&body).or_else(|| self.meant_check(&body)) {
                // one of her own checks on a project file the words name: the file is read too, so the answer has
                // what it holds beside the check's findings
                if self.mcp.builtin_owns(&c.0) {
                    let files: Vec<String> = path_tokens(&body).into_iter().filter(|p| !Path::new(p).is_absolute() && self.cwd.join(p).is_file()).take(OWN_READS_MAX).collect();
                    calls.extend(files.iter().map(|f| read(f)));
                }
                calls.push(c);
            }
            // a URL in the words ("mở https://example.com và cho mình biết tiêu đề trang", "open https://… and tell
            // me the title"): the page itself, in any language
            else if let Some(u) = url { calls.push(("web_fetch".to_string(), json!({"url": u}))); }
            else if !web_out && crate::route::web_ask(&body) { calls.push(query(&body)); }
            // "is todo.txt still there?": Elisa looks for it, and answers yes or no herself (`exists_answer`)
            else if let Some(p) = self.exists_asked(&body) { calls.push(("glob".to_string(), json!({"pattern": p}))); }
            else {
                // the files the words name: an explicit ask reads them (a missing one gets its real "not found"),
                // other words only the project's own files that exist
                // ("đọc todo.txt", "mo todo.txt": a reading word of the examples and the file's name alone: `bare_read`)
                let asked = crate::route::file_ask(&body).or_else(|| bare_read(&body));
                // or an ask by meaning to read a file of the project ("đọc file notes.txt và cho mình biết mã cuộc
                // họp", "lemme see whats in todo.txt": `read_ask`, the file's name put as "the file")
                let meant = asked.is_none().then(|| path_tokens(&body).into_iter()
                    .filter(|p| Some(p) != save.as_ref() && !Path::new(p).is_absolute() && !p.starts_with('~') && self.cwd.join(p).is_file())
                    .find(|p| read_ask(&body.replace(p.as_str(), "the file")))).flatten();
                let explicit = asked.is_some() || meant.is_some();
                let named = path_tokens(&body).into_iter().filter(|p| Some(p) != save.as_ref())
                    .filter(|p| if explicit { self.resolve(p).is_file() } else { !Path::new(p).is_absolute() && !p.starts_with('~') && self.cwd.join(p).is_file() })
                    // a file named in a question about it ("which region sold the most in sales.csv?") or in a
                    // change ask is read; one only mentioned while saying something else ("I renamed notes.txt") is not
                    .filter(|_| explicit || save.is_some() || asks_about_file(&body));
                let mut files: Vec<String> = asked.into_iter().chain(meant).filter(|f| Some(f) != save.as_ref()).chain(named).collect();
                let mut seen = HashSet::new();
                files.retain(|f| seen.insert(f.clone()));
                calls.extend(files.iter().take(OWN_READS_MAX).map(|f| read(f)));
                // "is the token in session.py safe?" (by meaning: `route::tool_ask_by_meaning`): the tokens in
                // the files decoded too (`sec_jwt`), so the answer comes from what they hold, never a guess at a
                // masked value
                if crate::route::tool_ask_by_meaning(&body) == Some("token") {
                    for f in files.iter().take(OWN_READS_MAX) {
                        let text = std::fs::read_to_string(self.resolve(f)).unwrap_or_default();
                        calls.extend(jwt_tokens(&text).into_iter().take(2).map(|t| ("sec_jwt".to_string(), json!({"token": t}))));
                    }
                }
                // a question about a project file that is not there ("is todo.txt still there?" after it was
                // deleted): Elisa looks for it, so the answer comes from the folder, not from memory
                if calls.is_empty() && !explicit && body.trim_end().ends_with('?') {
                    let gone = path_tokens(&body).into_iter().find(|p| Some(p) != save.as_ref() && !Path::new(p).is_absolute() && !p.starts_with('~')
                        && crate::route::file_name(p).as_deref() == Some(p.as_str()) && !self.cwd.join(p).exists());
                    if let Some(p) = gone { calls.push(("glob".to_string(), json!({"pattern": p}))); }
                }
                // no file named: a clear ask by meaning ("list the files in this folder", "liệt kê các file trong thư
                // mục này": `meant_call`)
                if calls.is_empty() && save.is_none() && path_tokens(&body).is_empty() {
                    if let Some(c) = self.meant_call(&body) { calls.push(c); }
                }
            }
        }
        // (the label's groups only: a web label runs no read of a file the words name, a files_code label no search;
        // the call "again" repeats is the one a follow-up label agrees with)
        if let Some(g) = labelled { calls.retain(|c| Some(c) == repeat.as_ref() || g.has(crate::route::tool_group(&c.0, self.mcp.server_of(&c.0)))); }
        // a chat label on words that name files and ask to search them ("tim giup minh file nao co chu ORCHID"): no
        // dead end, Elisa's own search of the project's files runs all the same (`search_ask`, `route::names_files`)
        if calls.is_empty() && labelled.is_some() && self.routed.as_ref().is_some_and(|a| a.has("chat")) && crate::route::names_files(&body) {
            if let Some(pattern) = search_pattern(&body) { calls.push(("grep".to_string(), json!({"pattern": pattern}))); }
        }
        // (no web call of her own on a turn the web is kept out of, `web_out`: a widening by a call is never an ask)
        if web_out { calls.retain(|c| Some(c) == repeat.as_ref() || crate::route::tool_group(&c.0, self.mcp.server_of(&c.0)) != crate::route::WEB); }
        // one call once: the router's search and the words' are the same search ("search the web: X" on a
        // web_search_now turn), its read and the named file's the same read
        let mut kept: Vec<(String, serde_json::Value)> = Vec::new();
        for c in calls { if !kept.iter().any(|k| same_call(k, &c)) { kept.push(c); } }
        // (never a command that is one bare word of the user's own sentence, "starving" from "… and now I'm
        // starving": no command of this machine, `route::word_as_command`, as the wrong-tool check holds the model to)
        kept.retain(|c| crate::route::word_as_command(&c.0, &c.1, &body).is_none());
        let calls = kept;
        let mut own = Vec::new();
        // a window too small for tools offers none: Elisa still runs her own tools herself, and their results go in
        // the turn's message as text when they fit (`run_own_inline`)
        // (her own checks too: a dependency or token check's findings are short)
        let builtin = |n: &str, s: &Self| s.tiny() && s.own_builtin(n);
        for (name, args) in calls {
            if builtin(&name, self) {
                // (on a tiny window too: its switches from the words, by the tool's own description)
                let args = match self.mcp.builtin_owns(&name).then(|| self.mcp.defs().into_iter().find(|d| d.name == name)).flatten() {
                    Some(d) => crate::mcp_args::set_by_meaning(&d.input_schema, &args, &body).0,
                    None => args,
                };
                own.push(Own::Call(name, args));
                continue;
            }
            if self.tools.iter().any(|t| t.name == name) || self.widen_for(&name) {
                // its "true for …" switches from this message's words (web_search's `recent` for news / the latest)
                let args = match self.tools.iter().find(|t| t.name == name) {
                    Some(d) if self.mcp.builtin_owns(&name) => crate::mcp_args::set_by_meaning(&d.input_schema, &args, &body).0,
                    _ => args,
                };
                own.push(Own::Call(name, args));
            } else { own.push(Own::Unoffered(name)); }
        }
        if let Some(f) = save.filter(|_| builtin("write", self) || self.tools.iter().any(|t| t.name == "write") || self.widen_for("write")) { own.push(Own::Save(f)); }
        if own.iter().any(|o| !matches!(o, Own::Unoffered(_))) { self.debug(&format!("[a clear ask: Elisa runs {} herself]", own.iter().filter_map(|o| match o { Own::Call(n, _) => Some(n.as_str()), Own::Save(_) => Some("write"), Own::Unoffered(_) => None }).collect::<Vec<_>>().join(", "))); }
        own
    }

    /// The project file a yes/no question asks is (still) there ("is todo.txt still there?", by meaning:
    /// `exists_ask`): a plain file name the words give, not an explicit ask to read it. None otherwise.
    fn exists_asked(&self, body: &str) -> Option<String> {
        if crate::route::file_ask(body).is_some() { return None; }
        let p = path_tokens(body).into_iter().find(|p| !Path::new(p).is_absolute() && !p.starts_with('~') && crate::route::file_name(p).as_deref() == Some(p.as_str()))?;
        exists_ask(&body.replace(p.as_str(), "the file")).then_some(p)
    }

    /// Elisa's answer to a yes/no question whether a file is there, after her own look for it this turn: yes with a
    /// short fact (its lines), or no. None when the question is not one or no such look ran.
    fn exists_answer(&self) -> Option<String> {
        let body = context::request_body(&self.input);
        let p = self.exists_asked(body)?;
        let at = self.history.iter().rposition(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. })))?;
        let looked = self.history[at..].iter().flat_map(|m| &m.content)
            .any(|b| matches!(b, Block::ToolUse { id, name, input } if id.starts_with("elisa_") && name == "glob" && input["pattern"] == p.as_str()));
        if !looked { return None; }
        let vi = crate::route::vietnamese(body);
        let f = self.cwd.join(&p);
        Some(match (f.is_file(), vi) {
            (true, _) => {
                let n = std::fs::read_to_string(&f).map(|t| t.lines().count()).ok();
                match (n, vi) {
                    (Some(n), false) => format!("Yes, {p} is there ({n} line{}).", if n == 1 { "" } else { "s" }),
                    (Some(n), true) => format!("Có, {p} vẫn còn ở đây ({n} dòng)."),
                    (None, false) => format!("Yes, {p} is there."),
                    (None, true) => format!("Có, {p} vẫn còn ở đây."),
                }
            }
            (false, false) => format!("No, {p} isn't there."),
            (false, true) => format!("Không, {p} không còn ở đây."),
        })
    }

    /// Elisa's reply to an ask whose whole job was her own save (after reads, a count): what went into the file, its
    /// first line. None when the write did not go through.
    fn saved_line(&self, f: &str, content: &str) -> Option<String> {
        let ok = self.tool_lines.last().is_some_and(|l| l.starts_with("[write ") && !l.contains(" → "));
        if !ok || !self.cwd.join(f).is_file() { return None; }
        // (in the language of the ask's own words, not of the text it gives: "sav this to hello.txt: xin chào thế
        // giới" is an English ask)
        let frame = context::request_body(&self.input).replace(content.trim(), " ");
        let lead = if crate::route::vietnamese(&frame) { format!("Đã lưu vào {f}") } else { format!("Saved to {f}") };
        // (short content whole, a haiku or a count; longer only its first line; never one of Elisa's own marks)
        let content = without_internal_lines(content);
        let text = content.trim();
        if text.lines().count() > 1 && text.lines().count() <= SAVED_SHOWN_LINES && text.chars().count() <= SAVED_SHOWN_CHARS {
            return Some(format!("{lead}:\n\n{text}"));
        }
        // (a heading's marks and a bullet off; bold and code marks kept in pairs)
        let mut lines = content.lines().map(|l| { let t = l.trim().trim_start_matches('#').trim(); t.strip_prefix("- ").or_else(|| t.strip_prefix("> ")).unwrap_or(t).trim() }).filter(|l| !l.is_empty());
        let first = lines.next().unwrap_or_default();
        let more = lines.next().is_some();
        let cut = paired_marks(&first.chars().take(120).collect::<String>());
        let tail = if more || cut.len() < first.len() { " …" } else { "" };
        Some(format!("{lead}: {cut}{tail}"))
    }

    /// A listing this turn (a glob with a wildcard) whose reply names files the listing does not hold ("…, notes.txt,
    /// and Elisa") or leaves some out with no count of them: Elisa states the list herself.
    fn listed_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        if reply.trim().is_empty() { return; }
        let res = self.turn_results();
        // (the listing: a glob with a wildcard, else a command Elisa ran herself for the ask that printed only names of
        // the folder's files, "list the files here with ls" run as `ls`)
        let ran_listing = |r: &&OwnResult| !r.is_error && r.input["command"].is_string() && self.own_results.iter().any(|o| o.input == r.input) && {
            let l: Vec<&str> = r.body.lines().map(str::trim).filter(|l| !l.is_empty() && !(l.starts_with("[exit ") && l.ends_with(']'))).collect();
            !l.is_empty() && l.iter().all(|x| !x.contains(' ') && self.cwd.join(x).exists())
        };
        let Some(g) = res.iter().find(|r| r.name == "glob" && !r.is_error && r.input["pattern"].as_str().is_some_and(|p| p.contains('*')))
            .or_else(|| res.iter().find(ran_listing)) else { return };
        let lines: Vec<&str> = g.body.lines().map(str::trim).filter(|l| !l.is_empty() && !(l.starts_with("[exit ") && l.ends_with(']'))).collect();
        let base = g.input["path"].as_str().map_or(self.cwd.clone(), |d| self.resolve(d));
        let names: Vec<String> = lines.iter().filter(|l| base.join(l).exists() || self.cwd.join(l).exists()).map(|l| l.to_string()).collect();
        // (a listing cut short, or not a list of paths: nothing to hold the reply to)
        if names.len() != lines.len() && !names.is_empty() || names.len() > 40 { return; }
        // (a listing Elisa ran herself for the ask, "liệt kê các file trong thư mục này": the answer is the listing,
        // so a reply that names only some of it and gives no count of them, or none, lacks it)
        let own = self.own_results.iter().any(|r| r.name == g.name && r.input == g.input);
        let named = names.iter().filter(|n| reply.contains(n.rsplit('/').next().unwrap_or(n))).count();
        let lacks = own && !names.is_empty() && named < names.len() && !crate::reply_check::numbers(&reply).contains(&names.len().to_string());
        if !lacks && !listed_mismatch(&reply, &names) { return; }
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        let line = match (names.len(), vi) {
            (0, false) => "There are no files here.".to_string(),
            (0, true) => "Không có tệp nào ở đây.".to_string(),
            (n, false) => format!("{n} file{} here:\n{}", if n == 1 { "" } else { "s" }, names.iter().map(|x| format!("- {x}")).collect::<Vec<_>>().join("\n")),
            (n, true) => format!("{n} tệp ở đây:\n{}", names.iter().map(|x| format!("- {x}")).collect::<Vec<_>>().join("\n")),
        };
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply's file list did not match the listing; Elisa listed the files]");
    }

    /// The words of a message without the names of `file`'s symbols ("add", "greet" in app.py).
    fn without_symbols(&self, body: &str, file: &str) -> String {
        body.split_whitespace().filter(|t| {
            let w = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            w.is_empty() || tools::symbol_lines(&json!({"path": file, "name": w}), &self.tctx).is_none()
        }).collect::<Vec<_>>().join(" ")
    }

    /// The code files this session's tools named, newest first (for an ask naming only a symbol: "show me the add
    /// function again").
    fn session_files(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in self.history.iter().rev() {
            for b in &m.content {
                if let Block::ToolUse { input, .. } = b {
                    if let Some(p) = input["path"].as_str().or(input["file_path"].as_str()) {
                        if !out.iter().any(|o| o == p) && self.resolve(p).is_file() { out.push(p.to_string()); }
                    }
                }
            }
        }
        out
    }

    /// The (file, symbol) an ask to see one symbol as it is names (by meaning: `symbol_ask_kind`): a word of the
    /// message that is a symbol of the file it names, else of a file this session's tools named. Never a change ask.
    fn symbol_shown(&self, body: &str) -> Option<(String, String)> {
        let named: Vec<String> = path_tokens(body).into_iter().filter(|p| !Path::new(p).is_absolute() && self.cwd.join(p).is_file()).collect();
        let files = if named.is_empty() { self.session_files() } else { named };
        let words: Vec<String> = body.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string())
            .filter(|w| w.chars().count() >= 2).collect();
        for f in files.iter().take(OWN_READS_MAX) {
            let Some(sym) = words.iter().find(|w| tools::symbol_lines(&json!({"path": f, "name": w}), &self.tctx).is_some()) else { continue };
            let plain = symbol_frame(body, f, sym);
            if asks_change(&self.without_symbols(body, f)) || symbol_ask_kind(&plain).is_none() { return None; }
            return Some((f.clone(), sym.clone()));
        }
        None
    }

    /// The words of an ask about a symbol in the terms of `data/symbol_asks.txt` (`symbol_frame`), for the file and
    /// symbol it names (`symbol_shown`). None when it names none.
    pub(super) fn symbol_plain(&self, body: &str) -> Option<String> {
        let named: Vec<String> = path_tokens(body).into_iter().filter(|p| !Path::new(p).is_absolute() && self.cwd.join(p).is_file()).collect();
        let files = if named.is_empty() { self.session_files() } else { named };
        let words: Vec<String> = body.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string()).collect();
        files.iter().take(OWN_READS_MAX).find_map(|f| words.iter().find(|w| w.chars().count() >= 2 && tools::symbol_lines(&json!({"path": f, "name": w}), &self.tctx).is_some()).map(|s| symbol_frame(body, f, s)))
    }

    /// A reply to a question about a symbol Elisa read this turn ("file app.py có lỗi gì không? xem hàm add": the
    /// `[check]` side of `data/symbol_asks.txt`) that only gives its code back, no judgement of it: asked once more in
    /// words with the code in hand; still none, Elisa's honest line that she could not judge it, with the code.
    /// Elisa's answer to a question whether code she read this turn has a mistake (the `[check]` side of
    /// `data/symbol_asks.txt`) when the model's reply could not stand: one request with the question and the code alone
    /// for a judgement (`symbol_check_note`), kept when it judges the code (`judges_code`), claims no fault-free verdict the
    /// code does not hold (`reply_check::unbacked_verdict`) and passes the gate; else her honest line that she could
    /// not judge it, with the code. None: no such question or no such read.
    fn symbol_verdict(&self) -> Option<String> {
        let body = context::request_body(&self.input).trim().to_string();
        if symbol_ask_kind(&self.symbol_plain(&body)?).as_deref() != Some("check") { return None; }
        let (f, sym, code) = self.read_symbol_code()?;
        let judged = (!CANCEL.load(Ordering::SeqCst)).then(|| {
            let no_tools: [ToolDef; 0] = [];
            let msgs = [Message::user_text(symbol_check_note(&body, &f, &sym, &code).trim().to_string())];
            let req = Request { system: &self.system, tools: &no_tools, messages: &msgs, max_tokens: self.cfg.max_output.min(CHECK_ANSWER_TOKENS) };
            let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
            let t = without_wrapper_tag(context::split_state(&texts(&Message::new(Role::Assistant, r.content))).0.trim());
            (judges_code(&t, &code) && !crate::reply_check::unbacked_verdict(&t, &code)).then_some(t)
        }).flatten().and_then(|t| self.gate_fault(&t, &body).ok()).map(|m| m.text).filter(|t| judges_code(t, &code) && !says_no_judgement(t, &f, &sym));
        Some(judged.unwrap_or_else(|| no_judge_line(&body, &f, &sym, &code)))
    }

    fn symbol_check_fix(&mut self) {
        let Some(plain) = self.symbol_plain(context::request_body(&self.input)) else { return };
        if symbol_ask_kind(&plain).as_deref() != Some("check") { return; }
        let Some((f, sym, code)) = self.read_symbol_code() else { return };
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        if judges_code(&texts(last), &code) { return; }
        let body = context::request_body(&self.input).trim().to_string();
        let note = symbol_check_note(&body, &f, &sym, &code);
        // (a reply wrapped in a tag of its own, "<answer>…</answer>", is its words alone)
        // (and no verdict of no fault the code does not hold itself: Elisa cannot verify one, `reply_check::unbacked_verdict`;
        // and the gate every shown text passes, `gated_in_language`: no garble, no dangling lead-in; a judgement in
        // another language than the question's is said again in its language, as any other answer)
        let again = self.own_words(&note, "answer", false).map(|t| without_wrapper_tag(&t)).filter(|t| judges_code(t, &code) && !crate::reply_check::unbacked_verdict(t, &code))
            .and_then(|t| self.gated_in_language(&t, &body)).filter(|t| judges_code(t, &code) && !says_no_judgement(t, &f, &sym));
        let line = again.unwrap_or_else(|| no_judge_line(&body, &f, &sym, &code));
        self.replace_reply(line, "[the reply only showed the code asked about; Elisa asked for a judgement of it]");
    }

    /// "giá vàng hôm nay thế nào nhỉ" after Elisa's own web search for a figure (`figure_ask`): she reads the top
    /// result pages herself (`web_fetch`, at most `LIVE_FETCH_MAX`, stopping at the first that has a figure,
    /// `live_sentence`) when the snippets hold none, and, for an ask about the present (`live_ask`), when no snippet
    /// dates its figure today (a snippet may be weeks old: "Sáng 13/10"), so the model answers from a page, not from
    /// memory or an old snippet.
    fn fetch_for_live_figure(&mut self) {
        let body = context::request_body(&self.input).to_string();
        if !self.own_ran("web_search") || !figure_ask(&body) { return; }
        let res = self.turn_results();
        let results: String = res.iter().filter(|r| r.name == "web_search" && !r.is_error).map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        if results.is_empty() { return; }
        let snippet = live_sentence(&body, &results, false);
        // (a snippet dated today by a day written answers an ask about the present; one saying only "hôm nay"
        // (`When::Now`) may be an older article's today: the page is read, its stamped table or dated lines decide)
        if snippet.as_ref().is_some_and(|f| !live_ask(&body) || f.when == When::Today) { return; }
        // (a window too small for tools offers the model none, `tiny`: Elisa still reads the page herself, as she ran
        // her own search, the page cut to fit, `run_own_inline`; her answer comes from the page, not the model's)
        let tiny = self.tiny();
        if tiny { if !self.own_builtin("web_fetch") { return; } }
        else if !self.tools.iter().any(|t| t.name == "web_fetch") && !self.widen_for("web_fetch") { return; }
        let why = if snippet.is_some() { "[the snippets' figure for an ask about the present may be old; Elisa reads the top page]" } else { "[the results hold no figure for a live ask; Elisa reads the top page]" };
        for url in result_urls(&results).into_iter().take(LIVE_FETCH_MAX) {
            self.debug(why);
            let call = vec![("web_fetch".to_string(), json!({"url": url}))];
            if tiny { if self.run_own_inline(call).is_err() { continue; } } else { self.own_step(call); }
            let page = self.turn_results().into_iter().rev().find(|r| r.name == "web_fetch" && !r.is_error && r.input["url"] == url.as_str()).map(|r| r.body).unwrap_or_default();
            if live_sentence(&body, &page, true).is_some() { break; }
        }
    }

    /// A tool Elisa can run herself on a window too small to offer the model any (`tiny`): one of her built-in tools,
    /// or of the built-in servers but the reasoning tools (`run_own_inline` puts the result in the turn as text).
    fn own_builtin(&self, name: &str) -> bool {
        tools::defs_for(self.cfg.ast).iter().any(|d| d.name == name) || (self.mcp.builtin_owns(name) && !self.mcp.reason_tool(name))
    }

    /// A reply to an ask for a figure (`figure_ask`) after Elisa's own search (`fetch_for_live_figure`): when a result
    /// or page she read holds the figure (`live_candidates`, the best first: the asked subject's single current value,
    /// of the kind asked, `LiveFigure::rank`) and the reply does not give it or one as good, gives a figure none of
    /// them holds, asks a question back or only copies a result (`copied_results`), that sentence with the site it came
    /// from in its place; to an ask about the present, a figure dated before today (`dated_when`, by its snippet or
    /// page line) is said with its date. A reply giving the figure but naming another result's site as its source
    /// (`named_sites`) gets the holding sentence with its own site. A date the reply puts on its figure comes from the
    /// results like the figure: one no result writes ("vào ngày 18/11/2026" where the result says 12/11/2026,
    /// `dates_stand`) gets the holding sentence too. When none holds one, for an ask about the present or a copied
    /// result, her honest line naming the sites the results do hold (never a site the model made up), and saying
    /// pages were read only when one was (`no_figure` / `no_figure_unread`). Runs after `weather_claim_fix`, so what
    /// the weather check leaves (a sentence of another day taken out) is held to the page's figure too.
    fn live_figure_fix(&mut self) {
        let body = context::request_body(&self.input).to_string();
        if !self.own_ran("web_search") || !figure_ask(&body) { return; }
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let res = self.turn_results();
        let vi = crate::route::vietnamese(&body);
        let strict = live_ask(&body);
        let searched: String = res.iter().filter(|r| !r.is_error && (r.name == "web_search" || r.name == "web_fetch")).map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        let copied = copied_results(&reply, &searched);
        // (a result's figure names that result's site, a page's figure the page's)
        let mut all: Vec<LiveFigure> = Vec::new();
        for r in res.iter().filter(|r| !r.is_error && (r.name == "web_fetch" || r.name == "web_search")) {
            let page = (r.name == "web_fetch").then(|| r.input["url"].as_str().map(host_of).unwrap_or_default()).filter(|h| !h.is_empty());
            for mut c in live_candidates(&body, &r.body, r.name == "web_fetch") {
                if c.source.is_none() { c.source = page.clone(); }
                all.push(c);
            }
        }
        all.sort_by_key(|c| std::cmp::Reverse(c.rank()));
        let mut why = "[the reply did not give the figure the results hold; Elisa gave it from them]";
        let line = match all.first() {
            Some(best) => {
                let (mut s, mut src) = (best.sentence.clone(), best.source.clone().unwrap_or_default());
                // (to an ask about the present, a figure of a day before today is said with its date, never as now:
                // Elisa's `dated_figure` line, since a reply giving it may still call it today's: "Hôm nay (13/10)")
                if let (When::Before { date, .. }, true) = (&best.when, strict) {
                    let line = crate::reply_check::own_line_of("dated_figure", &body, "").replace("{date}", date).replace("{figure}", s.trim_end_matches('.')).replace("{source}", &src);
                    if reply.trim() != line { self.replace_reply(line, "[the figure found is from a day before today; Elisa said it with its date]"); }
                    return;
                }
                // (the reply stands when it states a figure of a sentence as good as the best — the subject's, single,
                // as fresh, the figure of its newest day (`fresh_figures`) — and no other value of the asked quantity
                // (an older figure beside the latest makes a range of two dates: "347.736 đến 403.547 người"), gives
                // no figure the results and pages do not hold, takes its values from one result, and is no question
                // back; a figure it labels as an aside of the quantity, "22°C, cảm giác như 24°C", is held to the
                // pages' asides instead (`said_figures`, `asides_stand`): one given as the quantity, "22°C và 24°C",
                // is another value of it)
                let held: HashSet<String> = res.iter().filter(|r| !r.is_error).flat_map(|r| live_figures(&r.body)).collect();
                let (said, said_aside) = said_figures(&reply);
                let top = (best.subject, best.single, best.freshness());
                let good: HashSet<String> = all.iter().filter(|c| !c.aside && (c.subject, c.single, c.freshness()) == top).flat_map(|c| fresh_figures(&c.sentence)).collect();
                let values: HashSet<String> = all.iter().flat_map(|c| live_figures(&c.sentence)).collect();
                let asides: HashSet<String> = all.iter().filter(|c| c.aside).flat_map(|c| live_figures(&c.sentence)).collect();
                let mut holders: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
                for r in res.iter().filter(|r| !r.is_error && (r.name == "web_fetch" || r.name == "web_search")) {
                    web_holders(&r.body, (r.name == "web_fetch").then(|| r.input["url"].as_str().map(host_of).unwrap_or_default()).as_deref(), &mut holders);
                }
                // (a date the reply puts on its figure comes from the results like the figure itself: one no result
                // writes is made up, "403.556 người vào ngày 18/11/2026" where the result says 12/11/2026)
                let dated = dates_stand(&reply, &searched);
                if !dated { why = "[the reply dated its figure to a day no result holds; Elisa gave the figure from them]"; }
                if !copied && dated && figures_stand(&said, &held, &good, &values, &holders) && asides_stand(&said_aside, &held, &good, &values, &asides) && !reply.trim_end().ends_with('?') {
                    // (a reply naming a result's site as its figure's source ("380,312 according to Worldometer",
                    // `named_sites`) while another result holds that figure: the holding sentence with its own site;
                    // it stands when a holder is that site or itself names it ("Giá vàng SJC …" from another site))
                    let holders: Vec<&LiveFigure> = all.iter().filter(|c| live_figures(&c.sentence).iter().any(|n| good.contains(n) && said.contains(n))).collect();
                    let named = named_sites(&reply, &self.turn_hosts(), &body);
                    let truthful = |c: &&LiveFigure| named.iter().any(|h| c.source.as_deref() == Some(h) || word_set(&c.sentence).contains(&host_label(h)));
                    let Some(h) = holders.first().filter(|_| !named.is_empty() && !holders.iter().any(truthful)) else { return };
                    (s, src) = (h.sentence.clone(), h.source.clone().unwrap_or_default());
                    why = "[the reply named a site that does not hold its figure; Elisa gave the figure with its site]";
                }
                match (vi, src.is_empty()) { (_, true) => s, (true, false) => format!("{s} (nguồn: {src})"), (false, false) => format!("{s} (source: {src})") }
            }
            // (no figure found: an ask about the present, or a reply only copying a result, gets her honest line; a
            // number ask's reply stands, its figure may be a plain count: "Jupiter has 95 moons")
            None if !live_ask(&body) && !copied => return,
            None => {
                let results: String = res.iter().filter(|r| r.name == "web_search" && !r.is_error).map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
                let mut hosts: Vec<String> = Vec::new();
                for h in result_urls(&results).iter().map(|u| host_of(u)) { if !h.is_empty() && !hosts.contains(&h) { hosts.push(h); } }
                if hosts.is_empty() { return; }
                let list = hosts.iter().take(LIVE_FETCH_MAX + 1).cloned().collect::<Vec<_>>().join(", ");
                // (the line says only what happened: pages were read when a fetch came back this turn, else the
                // results alone were looked at)
                let read = res.iter().any(|r| r.name == "web_fetch" && !r.is_error);
                crate::reply_check::own_line_of(if read { "no_figure" } else { "no_figure_unread" }, &body, "").replace("{sites}", &list)
            }
        };
        self.replace_reply(line, why);
    }

    /// The sites of this turn's web results and of the pages read, in order, each once (`host_of`).
    fn turn_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        for r in self.turn_results().iter().filter(|r| !r.is_error) {
            let urls = match r.name.as_str() { "web_search" => result_urls(&r.body), "web_fetch" => r.input["url"].as_str().map(str::to_string).into_iter().collect(), _ => Vec::new() };
            for h in urls.iter().map(|u| host_of(u)) { if !h.is_empty() && !hosts.contains(&h) { hosts.push(h); } }
        }
        hosts
    }

    /// A reply to an ask about the weather (`ask_kinds`: degrees) after Elisa's own search read a page, with a claim
    /// the page contradicts (`weather_rewrite`: a condition the page shows otherwise for the same day, "trời mưa nhẹ"
    /// over "U ám" and "Lượng mưa 0 mm", `page_conditions`; a temperature the page does not give for that day,
    /// "Nhiệt độ 13°C" where today's readings are 18oC and 24oC, `page_degrees`; a sentence dated to another day than
    /// the one asked; or no condition at all for the day asked while the page shows one), is replaced by Elisa's own
    /// line for the day asked (`weather_answer`): the page's current figure sentence with its condition and site, or
    /// the page's sentences for the day — never by what the rewrite leaves of the reply, which carries the model's
    /// hourly and forecast pieces and its cut words ("Về từng giờ: bây giờ 24°C 13 %; … phố cuối ngày: …"); what it
    /// leaves stands only when the page gives no line for the day. Runs before `live_figure_fix`: a reply left
    /// without the page's current figure gets it there.
    fn weather_claim_fix(&mut self) {
        let body = context::request_body(&self.input).to_string();
        if !self.own_ran("web_search") || !ask_kinds(&body).0.contains(&"degrees") { return; }
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let res = self.turn_results();
        let Some(page) = res.iter().rev().find(|r| r.name == "web_fetch" && !r.is_error) else { return };
        let shown = page_conditions(&page.body);
        if shown.is_empty() { return; }
        let degrees = page_degrees(&page.body);
        let day = crate::situation::day_offset(&body).unwrap_or(0);
        let Some(left) = weather_rewrite(&reply, &shown, &degrees, day) else { return };
        let src = page.input["url"].as_str().map(host_of).unwrap_or_default();
        let text = weather_answer(&body, &page.body, &shown, day, &src).unwrap_or(left);
        if !text.chars().any(char::is_alphabetic) || text.trim() == reply.trim() { return; }
        self.replace_reply(text, "[a weather claim in the reply went against the page read; Elisa said what the page says]");
    }

    /// The symbol Elisa read this turn for an ask about it (`symbol_shown`): (file, symbol, its real lines without
    /// their numbers). None when no such read ran.
    fn read_symbol_code(&self) -> Option<(String, String, String)> {
        let (f, sym) = self.symbol_shown(context::request_body(&self.input))?;
        let r = self.turn_results().into_iter().find(|r| r.name == "read_symbol" && !r.is_error && r.input["name"] == sym.as_str() && r.input["path"] == f.as_str())?;
        // (the first line is the symbol's place: "app.py:1-2 def add")
        let code = unnumbered(&r.body.lines().skip(1).filter(|l| !l.starts_with('[')).collect::<Vec<_>>().join("\n"));
        (!code.trim().is_empty()).then_some((f, sym, code))
    }

    /// The answer to an ask to see one symbol (`symbol_shown`, the `[show]` side) after Elisa's own read of it this
    /// turn: its real lines, without their numbers. A question about the symbol ("có lỗi gì không? xem hàm add": the
    /// `[check]` side) is the model's to answer, with the symbol in hand (`symbol_check_fix`).
    fn shown_symbol(&self) -> Option<String> {
        if symbol_ask_kind(&self.symbol_plain(context::request_body(&self.input))?).as_deref() != Some("show") { return None; }
        let (f, sym, code) = self.read_symbol_code()?;
        let name = Path::new(&f).file_name().map_or(f.clone(), |n| n.to_string_lossy().to_string());
        Some(if crate::route::vietnamese(context::request_body(&self.input)) { format!("Đây là {sym} trong {name}:\n```\n{}\n```", code.trim_end()) }
            else { format!("Here is {sym} in {name}:\n```\n{}\n```", code.trim_end()) })
    }

    /// A call was refused this turn (the user said no, or no one could be asked).
    fn said_no_this_turn(&self) -> bool {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[cap..].iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolResult { content, .. } if content.starts_with("denied")))
    }

    /// Elisa's line for a change or command of hers the user said no to this turn ("Okay, I didn't delete it, since
    /// you said no."): the ask's change word when it has one (`asks_change`), else "do that". None when no such call
    /// was refused (a refused read or search has its own words).
    fn said_no_line(&self) -> Option<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(&str, &str)> = Vec::new();
        let mut refused = false;
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, .. } if id.starts_with("elisa_") => calls.push((id, name)),
                Block::ToolResult { tool_use_id, content, .. } if content.starts_with("denied by user") => {
                    // (a change or command of the built-in tools: a refused search or read has its own words)
                    refused |= calls.iter().any(|(i, n)| i == tool_use_id && !self.mcp.owns(n) && tools::access(n) != Access::ReadOnly);
                }
                _ => {}
            }
        }
        if !refused { return None; }
        let body = context::request_body(&self.input);
        // the change word right before the file it is about ("delete todo.txt"), else none
        let toks: Vec<String> = body.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_' && c != '-').to_lowercase()).collect();
        let verb = toks.windows(2).find(|w| w[0].chars().count() >= 3 && w[0].chars().all(char::is_alphabetic) && asks_change(&w[0]) && self.cwd.join(&w[1]).exists())
            .map(|w| w[0].clone())
            // (a new file: the ask's first change word, "save the word hello to deny.txt")
            .or_else(|| toks.iter().find(|w| w.chars().count() >= 3 && w.chars().all(char::is_alphabetic)
                && (asks_change(w) || crate::route::save_ask(&format!("{w} it to it.txt")).is_some())).cloned());
        Some(match (crate::route::vietnamese(body), verb) {
            (true, _) => "Được, mình không làm việc đó, vì bạn đã từ chối.".to_string(),
            (false, Some(v)) => format!("Okay, I didn't {v} it, since you said no."),
            (false, None) => "Okay, I didn't do that, since you said no.".to_string(),
        })
    }

    /// A reply to a command Elisa ran on the user's words that does not say what happened: it leaves out what the
    /// command printed (or the count asked), or talks of it in future or guessing words (`data/future_words.txt`:
    /// "The command will print …"). Elisa's own line in its place (`own_command_line`).
    fn command_reply_fix(&mut self) {
        let Some(line) = self.own_command_line() else { return };
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        // the fact: the count's number, else what it printed (its first line)
        let fact = crate::reply_check::numbers(&line).into_iter().next().filter(|_| !line.starts_with("It printed") && !line.starts_with("Lệnh in ra"))
            .or_else(|| self.command_printed().map(|o| o.lines().next().unwrap_or("").trim().to_string())).unwrap_or_default();
        let words: Vec<String> = reply.split(|c: char| c.is_whitespace() || ".,;:!?\"()".contains(c)).map(|w| w.to_lowercase()).collect();
        let future = future_words().iter().any(|f| words.iter().any(|w| w == f || (f.starts_with('\'') && w.ends_with(f.as_str()))));
        // (a number is said as a whole number: a copy of df's table holds "29" inside "1294…" and says nothing)
        // (and said outside the command's own text: "echo xin-chao-elisa" quoted back holds the output's words and says nothing)
        let plain = |t: &str| t.replace(['"', '\'', '`'], "");
        let outside = match self.own_command() { Some(c) => plain(&reply).replace(&plain(&c), ""), None => reply.clone() };
        let said = if fact.chars().all(|c| c.is_ascii_digit() || c == '.') { crate::reply_check::numbers(&outside).contains(&fact) } else { outside.contains(&fact) };
        if !future && (fact.is_empty() || said) { return; }
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply did not say what the command did; Elisa said it]");
    }

    /// A reply after the user said no to a change or command of Elisa's, when the model made no step of its own: her
    /// own plain line in its place ("Okay, I didn't delete it, since you said no.").
    fn said_no_fix(&mut self) {
        let Some(line) = self.said_no_line() else { return };
        if self.history.last().is_none_or(|m| m.role != Role::Assistant) { return; }
        // (the model went on with other steps of the ask: its reply is its own)
        let cap = self.turn_cap(&self.history).min(self.history.len());
        if self.history[cap..].iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { id, .. } if !id.starts_with("elisa_"))) { return; }
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply did not say the refused step did not happen; Elisa said it]");
    }

    /// A call of a tool that changes files (`route::changes_files`) the user did not ask for: on a turn whose ask Elisa
    /// read as one to list or read and nothing more (every call she ran for it herself only reads, and its words ask
    /// for no change or save: "liệt kê các file trong thư mục này"). The note for the model (the call is not run);
    /// None for any other call. (A change outside the working folder is the outside-the-project approval's to decide.)
    pub(super) fn unasked_change(&self, name: &str, server: Option<&str>) -> Option<String> {
        if !crate::route::changes_files(name, server) { return None; }
        let body = context::request_body(&self.input);
        // (and whose words ask for no change or save: "add 'x' to todo.txt" after Elisa's own read of it asks for one)
        let read_ask = !self.own_results.is_empty() && self.own_results.iter().all(|r| tools::access(&r.name) != Access::Write)
            && !asks_change(body) && crate::route::save_ask(body).is_none();
        read_ask.then(|| format!("Not run: the user asked only to list or read, and asked for no file to be written or changed, so `{name}` does not apply. Answer with what was listed or read."))
    }

    /// Elisa ran a call of this tool herself this turn (in the history, or kept whole past a fold: `own_results`).
    fn own_ran(&self, name: &str) -> bool {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.own_results.iter().any(|r| r.name == name)
            || self.history[cap..].iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { id, name: n, .. } if id.starts_with("elisa_") && n == name))
    }

    /// What Elisa's own command this turn printed.
    fn command_printed(&self) -> Option<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut id = None;
        let mut out = None;
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id: i, name, .. } if i.starts_with("elisa_") && name == "bash" => id = Some(i.clone()),
                Block::ToolResult { tool_use_id, content, is_error: false } if Some(tool_use_id) == id.as_ref() => out = crate::reply_check::command_output(content).map(|o| o.0),
                _ => {}
            }
        }
        out
    }

    /// The command Elisa ran herself this turn (her own `bash` call's command), if any.
    fn own_command(&self) -> Option<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[cap..].iter().flat_map(|m| &m.content).find_map(|b| match b {
            Block::ToolUse { id, name, input } if id.starts_with("elisa_") && name == "bash" => input["command"].as_str().map(str::to_string),
            _ => None,
        })
    }

    /// Elisa's answer to a command she ran on the user's words this turn (`command_ask`): the count the words ask
    /// for of the one file it names ("todo.txt has 2 lines."), the free space an ask about disk space got from `df`
    /// ("29 GB free on /.": `free_space_text`), else what it printed (`reply_check::printed_line`).
    fn own_command_line(&self) -> Option<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut id = None;
        let mut command = String::new();
        let mut out = None;
        let mut raw = String::new();
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id: i, name, input } if i.starts_with("elisa_") => {
                    if name != "bash" { return None; }
                    (id, command) = (Some(i.clone()), input["command"].as_str().unwrap_or("").to_string());
                }
                Block::ToolResult { tool_use_id, content, is_error: false } if Some(tool_use_id) == id.as_ref() => {
                    out = crate::reply_check::command_output(content);
                    raw = content.clone();
                }
                _ => {}
            }
        }
        let body = context::request_body(&self.input);
        // (df lists every volume: a table too long to say whole still has the one row the ask is about)
        let ok = !raw.is_empty() && !raw.lines().any(|l| l.trim().starts_with("[exit ") && l.trim() != "[exit 0]");
        if ok {
            let table: String = raw.lines().filter(|l| { let t = l.trim(); !t.starts_with('[') && !t.starts_with("<untrusted") && !t.starts_with("</untrusted") }).collect::<Vec<_>>().join("\n");
            if let Some(l) = free_space_text(body, &command, &table, &self.cwd) { return Some(l); }
        }
        let (output, code) = out?;
        let units = asked_counts(body);
        let files: Vec<String> = path_tokens(body).into_iter().filter(|f| self.cwd.join(f).is_file()).collect();
        if let ([f], false, 0) = (files.as_slice(), units.is_empty(), code) {
            let text = std::fs::read_to_string(self.cwd.join(f)).ok()?;
            return Some(count_text(f, &text, &units, crate::route::vietnamese(body)));
        }
        Some(crate::reply_check::printed_line(&self.input, output.trim_end(), code))
    }

    /// Elisa's own count for a counting ask ("đếm số dòng trong todo.txt bằng wc", "how many lines does todo.txt
    /// have?": `asked_counts`) of the one project file her read or command touched this turn: "todo.txt có 2 dòng." /
    /// "todo.txt has 2 lines.". None when the words ask no count or no such file was touched.
    fn count_line(&self) -> Option<String> {
        let body = context::request_body(&self.input);
        let units = asked_counts(body);
        if units.is_empty() { return None; }
        let mut files: Vec<String> = Vec::new();
        for r in self.turn_results().iter().filter(|r| !r.is_error) {
            let named: Vec<String> = match r.name.as_str() {
                "read" => r.input["path"].as_str().map(str::to_string).into_iter().collect(),
                "bash" => r.input["command"].as_str().map(path_tokens).unwrap_or_default(),
                _ => Vec::new(),
            };
            for f in named.into_iter().filter(|f| !Path::new(f).is_absolute() && self.cwd.join(f).is_file()) { if !files.contains(&f) { files.push(f); } }
        }
        let [f] = files.as_slice() else { return None };
        let text = std::fs::read_to_string(self.cwd.join(f)).ok()?;
        Some(count_text(f, &text, &units, crate::route::vietnamese(body)))
    }

    /// A reply to a counting ask after Elisa's own read or command that does not give the count ("đếm số dòng trong
    /// todo.txt bằng wc" answered with the file's lines and no number): her own count in its place (`count_line`).
    fn count_reply_fix(&mut self) {
        let Some(line) = self.count_line() else { return };
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let said = crate::reply_check::numbers(&texts(last));
        if crate::reply_check::numbers(&line).iter().all(|n| said.contains(n)) { return; }
        self.replace_reply(line, "[the reply did not give the asked count; Elisa counted it]");
    }

    /// Elisa's own answer to an ask for one item of a list by its place ("trong todo.txt, việc đầu tiên là gì?",
    /// "what's the last item in todo.txt?": `asked_item`) from the file she holds (`held_file`: this turn's own read,
    /// or one before), when the words name that file or no file: "Mục đầu tiên trong todo.txt là buy milk." / "The
    /// first item in todo.txt is buy milk." (`item_line`). None otherwise.
    fn held_item_line(&self) -> Option<String> {
        let body = context::request_body(&self.input);
        let own = context::own_words(body).unwrap_or(body).trim();
        asked_item(own)?;
        let (path, text, _) = self.held_file()?;
        let named = path_tokens(own);
        if !named.is_empty() && !named.iter().any(|p| *p == path || self.resolve(p) == self.resolve(&path)) { return None; }
        item_line(own, &path, &held_rows(&path, &text))
    }

    /// A reply to an ask for one item of a held list that does not give that item alone (the whole file read out,
    /// "Đã đọc todo.txt: - buy milk - send report"): Elisa's own line in its place (`held_item_line`).
    fn item_reply_fix(&mut self) {
        let Some(line) = self.held_item_line() else { return };
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last).to_lowercase();
        let Some((path, text, _)) = self.held_file() else { return };
        let rows = held_rows(&path, &text);
        let body = context::request_body(&self.input);
        let Some(item) = asked_item(context::own_words(body).unwrap_or(body)).and_then(|(n, _, _)| item_of(&rows, n)) else { return };
        let key = |r: &(String, String)| r.0.to_lowercase();
        let others = rows.iter().filter(|r| key(r) != item.0.to_lowercase() && !item.0.to_lowercase().contains(&key(r))).any(|r| reply.contains(&key(r)));
        if reply.contains(&item.0.to_lowercase()) && !others { return; }
        self.replace_reply(line, "[the reply did not give the one item asked; Elisa gave it from the file]");
    }

    /// "again" right after a call Elisa made herself ("run the command: echo hello-elisa", then "again"; by meaning:
    /// the `[again]` section of `data/own_tools.txt`): that call once more. Only the last turn's own call, and only a
    /// command, a web call or a tool that reads (a write or an edit is never repeated blind). None otherwise.
    fn repeat_asked(&self, body: &str) -> Option<(String, serde_json::Value)> {
        if body.split_whitespace().count() > REPEAT_WORDS || !path_tokens(body).is_empty() { return None; }
        if meant_tool(body).is_none_or(|(n, _)| n != "again") { return None; }
        // (this turn's message is not in the history yet: the last request is the turn before)
        let prev = self.history.iter().rposition(is_request)?;
        let (name, input) = self.history[prev..].iter().filter(|m| m.role == Role::Assistant).flat_map(|m| m.content.iter().rev())
            .find_map(|b| match b { Block::ToolUse { id, name, input } if id.starts_with("elisa_") => Some((name.clone(), input.clone())), _ => None })?;
        let web = crate::route::tool_group(&name, self.mcp.server_of(&name)) == crate::route::WEB;
        (name == "bash" || web || (!self.mcp.owns(&name) && tools::access(&name) == Access::ReadOnly)).then_some((name, input))
    }

    /// The newest file a read of this conversation returned that is still there: its path, its text now, and the
    /// index of the message that read it.
    fn held_file(&self) -> Option<(String, String, usize)> {
        for (i, m) in self.history.iter().enumerate().rev() {
            for b in m.content.iter().rev() {
                let Block::ToolUse { name, input, .. } = b else { continue };
                if name != "read" { continue; }
                let Some(p) = input["path"].as_str() else { continue };
                let full = self.resolve(p);
                if !full.starts_with(&self.cwd) { continue; }
                if let Ok(t) = std::fs::read_to_string(&full) { return Some((p.to_string(), t, i)); }
            }
        }
        None
    }

    /// Elisa's own answer to a short follow-up from the file she holds (`held_file`): "and South?" → the row whose
    /// key the words name ("South: 340 units."); "the other one?" / "và cái kia?" (`next_item_ask`) → the one row no
    /// message since the read has named ("The other one is East: 95 units." / "Cái còn lại là send report.").
    /// `strict` (before the model, with no model call): the words are a question or a follow-up of a few words, the
    /// row's name with at most `HELD_FRAME_WORDS` others ("and South?", "what about East?"; "is the project late?"
    /// asks more: the model's), and every request since the read was as short (the chat stayed on that file); loose
    /// (a reply that answered nothing): any such words. None otherwise: the model answers, or Elisa's honest line.
    fn held_line(&self, body: &str, strict: bool) -> Option<String> {
        let own = context::own_words(body).unwrap_or(body).trim();
        let short = |t: &str| t.split_whitespace().count() <= HELD_FOLLOW_UP_WORDS && path_tokens(t).is_empty() && !t.contains('\n');
        if !short(own) { return None; }
        if strict && !own.ends_with('?') && crate::followup::kind(own).is_none() { return None; }
        let (path, text, at) = self.held_file()?;
        let cur = self.history.iter().rposition(is_request).unwrap_or(self.history.len());
        if strict && at < cur && !self.history[at..cur].iter().filter(|m| is_request(m)).all(|m| short(context::request_body(&texts(m)).trim())) { return None; }
        let rows = held_rows(&path, &text);
        if rows.len() < 2 { return None; }
        // "the first one?", "cái cuối cùng?": that item of the list (`asked_item`)
        if let Some(line) = item_line(own, &path, &rows) { return Some(line); }
        let words: HashSet<String> = word_set(own).into_iter().collect();
        let item = |k: &str, rest: &str| if rest.is_empty() { k.to_string() } else { format!("{k}: {rest}") };
        // the row the words name whole ("and South?": South; "the report?" names no row of "send report")
        let named: Vec<&(String, String)> = rows.iter().filter(|(k, _)| { let kw = word_set(k); !kw.is_empty() && kw.iter().all(|w| words.contains(w)) }).collect();
        if let [(k, rest)] = named.as_slice() {
            let others = words.iter().filter(|w| !word_set(k).contains(w)).count();
            return (!strict || others <= HELD_FRAME_WORDS).then(|| format!("{}.", item(k, rest)));
        }
        if !named.is_empty() || !next_item_ask(own) { return None; }
        // the one row nothing has named since the request that read the file ("how many units did North sell …?"
        // names North; the user's words and Elisa's)
        let from = self.history[..at].iter().rposition(is_request).unwrap_or(at);
        let said: HashSet<String> = self.history[from..].iter().flat_map(|m| word_set(&texts(m))).chain(word_set(own)).collect();
        let left: Vec<&(String, String)> = rows.iter().filter(|(k, _)| !word_set(k).iter().all(|w| said.contains(w))).collect();
        let [(k, rest)] = left.as_slice() else { return None };
        Some(if crate::route::vietnamese(own) { format!("Cái còn lại là {}.", item(k, rest)) } else { format!("The other one is {}.", item(k, rest)) })
    }

    /// New code that copies a reply of this conversation (a line of it, Elisa's own lines among them) that the file
    /// does not hold: no code.
    fn copies_reply(&self, code: &str, file: &str) -> bool {
        let replies: Vec<String> = self.history.iter().filter(|m| m.role == Role::Assistant).map(texts).collect();
        code.lines().map(str::trim).filter(|l| l.chars().count() >= 12 && !file.contains(*l)).any(|l| replies.iter().any(|r| r.contains(l)))
    }

    /// An outline of one file this turn whose reply names none of its symbols (an invented list): Elisa's own answer
    /// from it ("app.py defines add (lines 1-2) and greet (lines 5-6).").
    fn outline_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let Some(r) = self.turn_results().into_iter().find(|r| r.name == "outline" && !r.is_error) else { return };
        let Some(p) = r.input["path"].as_str().map(str::to_string) else { return };
        let Some(syms) = tools::outline_syms(&p, &self.tctx) else { return };
        let words: HashSet<&str> = reply.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect();
        if syms.is_empty() || syms.iter().any(|(n, _, _)| words.contains(n.as_str())) { return; }
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        let name = Path::new(&p).file_name().map_or(p.clone(), |n| n.to_string_lossy().to_string());
        let parts: Vec<String> = syms.iter().map(|(n, a, b)| if vi { format!("{n} (dòng {a}–{b})") } else { format!("{n} (lines {a}–{b})") }).collect();
        let list = match parts.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{} {} {last}", rest.join(", "), if vi { "và" } else { "and" }),
            [] => return,
        };
        let line = if vi { format!("{name} có {list}.") } else { format!("{name} defines {list}.") };
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply named nothing the outline holds; Elisa gave the outline]");
    }

    /// Elisa's answer to an ask for the value of one setting of a file she read herself this turn ("what is the api
    /// key in config.toml?", by meaning: `value_ask`): that one value, a secret cut short. None when the ask is not
    /// one, no setting of the file is named in it, or more than one is.
    fn asked_value(&self) -> Option<String> { self.asked_value_parts().map(|p| p.2) }

    /// After a read this turn (Elisa's or the model's), a reply that does not give the asked value exactly as the
    /// file has it ("Papaya is a type of fruit." for "The secret word is PAPAYA."): Elisa's own line (`asked_value`).
    fn asked_value_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let Some((_, value, line)) = self.asked_value_parts() else { return };
        // (a value cut short on screen is never asked to stand in full)
        let shown = crate::secrets::mask_shown(&value);
        if reply.contains(value.as_str()) || (shown != value && reply.contains(shown.as_str())) { return; }
        self.replace_reply(line, "[the reply did not give the asked value as the file has it; Elisa gave it]");
    }

    /// The asked setting or named value of this turn's read file: (its name, its value, Elisa's line saying it).
    fn asked_value_parts(&self) -> Option<(String, String, String)> {
        let body = context::request_body(&self.input);
        if asks_change(body) { return None; }
        let r = self.turn_results().into_iter().find(|r| r.name == "read" && !r.is_error)?;
        let file = r.input["path"].as_str()?.to_string();
        let text = std::fs::read_to_string(self.resolve(&file)).ok()?;
        let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        // the file's settings: "key = value", "key: value", "KEY=value"
        let settings: Vec<(String, String)> = text.lines().filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || l.starts_with('[') || l.starts_with("//") { return None; }
            let (k, v) = l.split_once('=').or_else(|| l.split_once(':'))?;
            let k = k.trim().trim_start_matches("export ").trim();
            (!k.is_empty() && !k.contains(' ') && !v.trim().is_empty()).then(|| (k.to_string(), v.trim().trim_matches(['"', '\'', ',']).to_string()))
        }).chain(
            // and its named values in words: "The secret word is PAPAYA." (a short name, a short value)
            text.split(['.', '!', '?', '\n']).filter_map(|sentence| {
                let (k, v) = sentence.split_once(" is ").or_else(|| sentence.split_once(" là "))?;
                let k = k.trim();
                let k = k.strip_prefix("The ").or_else(|| k.strip_prefix("the ")).unwrap_or(k).trim();
                let v = v.trim().trim_matches(['"', '\'', ',', ':']).trim();
                let short = |t: &str, n: usize| !t.is_empty() && t.split_whitespace().count() <= n;
                (short(k, VALUE_NAME_WORDS) && short(v, VALUE_WORDS) && !k.contains(['=', ':'])).then(|| (k.to_string(), v.to_string()))
            })).collect();
        // the ask's words, one to three running together, against each setting's name
        let words: Vec<String> = body.replace(file.as_str(), " ").split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
        let mut grams: Vec<(String, usize, usize)> = Vec::new();
        for n in 1..=3 { for (i, w) in words.windows(n).enumerate() { grams.push((w.concat(), i, n)); } }
        let hits: Vec<&(String, String)> = settings.iter().filter(|(k, _)| grams.iter().any(|g| g.0 == norm(k))).collect();
        let [(key, value)] = hits.as_slice() else { return None };
        let (_, at, n) = grams.iter().filter(|g| g.0 == norm(key)).max_by_key(|g| g.2)?.clone();
        let plain: String = words.iter().enumerate().map(|(i, w)| if i == at { "foo".to_string() } else if i > at && i < at + n { String::new() } else { w.clone() })
            .filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        if !value_ask(&format!("{plain} the file")) { return None; }
        let shown = crate::secrets::mask_shown(value);
        let name = Path::new(&file).file_name().map_or(file.clone(), |n| n.to_string_lossy().to_string());
        let vi = crate::route::vietnamese(body);
        let cut = shown != *value;
        let line = match (vi, cut) {
            (false, false) => format!("The {key} in {name} is {shown}."),
            (false, true) => format!("The {key} in {name} is {shown} (shown cut short)."),
            (true, false) => format!("{key} trong {name} là {shown}."),
            (true, true) => format!("{key} trong {name} là {shown} (rút gọn để giữ kín)."),
        };
        Some((key.clone(), value.clone(), line))
    }

    /// A reply after Elisa's own edit this turn that does not speak to it (a copy of an earlier answer): her own
    /// line saying what changed ("Changed 'buy milk' to 'buy bread' in todo.txt.").
    fn edit_reply_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut edit = None;
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if id.starts_with("elisa_") && name == "edit" => edit = Some((id.clone(), input.clone(), false)),
                Block::ToolResult { tool_use_id, is_error: false, .. } if edit.as_ref().is_some_and(|e| &e.0 == tool_use_id) => { if let Some(e) = edit.as_mut() { e.2 = true; } }
                _ => {}
            }
        }
        let Some((_, input, true)) = edit else { return };
        let (Some(old), Some(new), Some(file)) = (input["old"].as_str(), input["new"].as_str(), input["path"].as_str()) else { return };
        // (a reply narrating the ask, "The user asked to change "buy milk" to "buy bread"", confirms nothing)
        // (only a sentence that says it was done, "has been updated to 'buy bread'", confirms it: "The task is to change
        // … to "buy bread"" does not, `says_done`)
        let plain = crate::reply_check::without_meta_narration(&reply).unwrap_or(reply.clone());
        let body = context::request_body(&self.input);
        // (a done-report in another language than the ask's, "buy milk has been replaced with buy bread in the todo.txt
        // file." to a Vietnamese order: Elisa's own line in the ask's language)
        if says_done(&plain, new.trim()) && !crate::reply_check::lang_mismatch(body, &plain) { return; }
        let line = if crate::route::vietnamese(body) { format!("Đã đổi '{old}' thành '{new}' trong {file}.") }
            else { format!("Changed '{old}' to '{new}' in {file}.") };
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply did not speak to Elisa's edit, or not in the ask's language; Elisa said what changed]");
    }

    /// The turn's last reply replaced by Elisa's own line, with a note why.
    fn replace_reply(&mut self, line: String, why: &str) {
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug(why);
    }

    /// An ask naming a field a tool's result gives on its own line ("tell me the page title" after a fetch whose
    /// result has "title: Example Domain") and a reply that does not give that value: Elisa's own line from the result
    /// (in place of a short reply, after a longer one).
    fn field_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let body = context::request_body(&self.input);
        let asked: HashSet<String> = body.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).filter(|w| w.chars().count() >= 3).collect();
        let mut hits: Vec<(String, String, serde_json::Value)> = Vec::new();
        for r in self.turn_results().into_iter().filter(|r| !r.is_error && r.name != "read") {
            for l in r.body.lines().take(FIELD_LINES) {
                let Some((k, v)) = l.split_once(": ") else { continue };
                let (k, v) = (k.trim(), v.trim());
                if k.chars().count() >= 3 && k.chars().all(|c| c.is_ascii_lowercase()) && !v.is_empty() && asked.contains(k) && !hits.iter().any(|h| h.0 == k) {
                    hits.push((k.to_string(), v.to_string(), r.input.clone()));
                }
            }
        }
        let [(key, value, input)] = hits.as_slice() else { return };
        // (the tool's own line as the reply, "title: Example Domain", gives the value but says nothing in words)
        let raw = reply.lines().any(|l| l.trim().to_lowercase().starts_with(&format!("{key}: ")));
        if !raw && reply.to_lowercase().contains(&value.to_lowercase()) { return; }
        let vi = crate::route::vietnamese(body);
        let line = match (input["url"].as_str(), vi) {
            (Some(u), false) => format!("The {key} of {u} is \"{value}\"."),
            (Some(u), true) => format!("{key} của {u} là \"{value}\"."),
            (None, false) => format!("The {key} is \"{value}\"."),
            (None, true) => format!("{key} là \"{value}\"."),
        };
        let line = if !raw && reply.split_whitespace().count() > FIELD_REPLY_WORDS { format!("{}\n\n{line}", reply.trim_end()) } else { line };
        self.replace_reply(line, &format!("[the reply did not give the {key} the result has; Elisa gave it]"));
    }

    /// A reply to a dependency check that advises an update below the highest fixed version the check gave, or no
    /// update for a package the check found: the versions put right, and the check's summary (`deps_summary`) after it when a package's update is still missing.
    fn deps_reply_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let Some(r) = self.turn_results().into_iter().find(|r| r.name == "sec_deps" && !r.is_error) else { return };
        let Some((_, found)) = deps_found(&r.body) else { return };
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        // a sentence pairing an advisory with a package the check does not pair it with ("CVE-2019-1010083 … in the
        // requests library" for a flask advisory) is left out: the check's summary says it right
        let (out, mispaired) = drop_mispaired(&reply, &advisory_owners(&r.body));
        // a line pairing an advisory with a fixed version or a level the check does not give it ("0.12.3
        // (CVE-2019-1010083) - CRITICAL") is left out too; when most of them are wrong, Elisa's own list from the check
        // stands in their place
        let currents: Vec<Vec<u64>> = found.iter().filter_map(|((_, v), _)| version_places(v).first().map(|x| x.2.clone())).collect();
        let facts = advisory_facts(&r.body);
        let (out, misstated) = drop_misstated(&out, &facts, &currents, vi);
        // a sentence crediting one package's advisories to another ("…in the requests library … the JSON encoding, the
        // Pallets project"), or making an advisory source the one who did it ("The `PYSEC` tool has fixed…"), goes;
        // when most of the reply goes, Elisa's summary alone answers
        let (out, credited, most) = drop_credited_wrong(&out, &facts);
        // a count of advisories said for a package other than the check's ("Flask 0.12 has three known
        // vulnerabilities" for 8) is put right, or its sentence goes when it lists them one by one
        let counts: Vec<(String, usize)> = found.iter().map(|((n, _), (c, _, _))| (n.to_lowercase(), *c)).collect();
        let (out, recounted, most_counts) = fix_counts(&out, &counts);
        // a change made this turn (the model rewrote the lockfile): its versions are what the file holds now, so the
        // reply's words stay and the check's versions go after them
        let changed = self.turn_results().iter().any(|r| !r.is_error && tools::access(&r.name) == Access::Write);
        // a package said to be updated when nothing changed, or "no other known vulnerabilities" when the check found
        // some, goes: the summary says what to update
        let names: Vec<String> = found.iter().map(|((n, _), _)| n.to_lowercase()).collect();
        let in_use: Vec<String> = found.iter().map(|((_, v), _)| v.clone()).collect();
        let (out, wrong_status, most_status) = drop_wrong_status(&out, &names, &in_use, changed, !found.is_empty());
        // a severity score the check never gave ("a score of 10.0 on the CVSS") takes its sentence out
        let (out, unbacked) = drop_unbacked_scores(&out, &r.body);
        // a fixed version said to be vulnerable ("versions `0.12` and `3.1.3` are vulnerable") leaves that list
        let (out, fixed_as_vulnerable) = drop_fixed_as_vulnerable(&out, &names, &found, &facts);
        let unbacked = unbacked || fixed_as_vulnerable;
        let wrong_status = wrong_status || unbacked;
        let mut out = if most || most_counts || most_status { String::new() } else { out };
        let missing = mispaired || misstated || credited || recounted || wrong_status;
        // each version in a sentence goes with the package named last before it ("flask 0.12 … update to 2.3.2");
        // one below that package's highest fix (not the one in use) becomes the fix
        let mut said = vec![false; found.len()];
        let mut next = String::new();
        for s in out.split_inclusive(['\n', '!', '?']).flat_map(|s| s.split_inclusive(". ")) {
            let mut t = raised(s, &names, &found, &facts, changed, &mut said);
            // (a list the raise made all one version, "(3.1.3, 3.1.3, 3.1.3)": that version once)
            for fix in found.iter().filter_map(|(_, (_, _, f))| f.as_ref()) {
                let f = fix.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
                for sep in [", ", " and ", " or ", " / ", ",", "/"] {
                    // (in code marks too: "`3.1.3` and `3.1.3`")
                    for (two, one) in [(format!("{f}{sep}{f}"), f.clone()), (format!("`{f}`{sep}`{f}`"), format!("`{f}`"))] {
                        while t.contains(&two) { t = t.replace(&two, &one); }
                    }
                }
                // (an aside naming that version again, "(or at least **3.1.3** for the earliest patch)", says nothing)
                t = without_same_version_aside(&t, fix);
            }
            next.push_str(&t);
        }
        // (a reply left opening in lower case after its first words went: its first letter a capital)
        out = if next != reply && next.trim_start().chars().next().is_some_and(char::is_lowercase) {
            let lead = next.len() - next.trim_start().len();
            let mut c = next[lead..].chars();
            format!("{}{}", &next[..lead], c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default())
        } else { next };
        // Elisa's verified list from the check leads, the same on every model; what is left of the model's text, held
        // to the check and cut to a short note on what to do, follows it under its label (`deps_block`, `deps_note`)
        let _ = (missing, said);
        let Some(block) = deps_block(&r.body, vi, false) else { return };
        let note = deps_note(&out, &r.body);
        let answer = if note.is_empty() { block } else { format!("{block}\n\n{} {note}", block_text("todo", vi)) };
        if answer != reply { self.replace_reply(answer, "[Elisa's verified list from the check leads; the model's short note follows]"); }
    }

    /// Elisa's note to the model on a turn whose dependency check ran: the verified list the user sees, and to write
    /// only a short note on what to do (`data/check_block.txt` `note`). Never shown; None on any other turn.
    pub(super) fn deps_model_note(&self) -> Option<String> {
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        let res = self.turn_results();
        if let Some(r) = res.iter().find(|r| r.name == "sec_deps" && !r.is_error) {
            let block = deps_block(&r.body, vi, false)?;
            return Some(block_text("note", vi).replace("{block}", &format!("\n{block}\n")));
        }
        // (her other checks: the secrets scan, the binary and the token checks)
        let blocks: Vec<String> = res.iter().filter(|r| !r.is_error).filter_map(|r| check_block(&r.name, &r.body, &r.input, vi)).collect();
        (!blocks.is_empty()).then(|| block_text("check_note", vi).replace("{block}", &format!("\n{}\n", blocks.join("\n\n"))))
    }

    /// A turn whose secrets scan, binary check or token check ran (and no dependency check): Elisa's verified result
    /// leads, the same on every model, and what is left of the model's text follows as a short note on what to do
    /// (`check_block`, `check_note`).
    fn check_reply_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let res = self.turn_results();
        if res.iter().any(|r| r.name == "sec_deps" && !r.is_error) { return; }
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        let ran: Vec<&OwnResult> = res.iter().filter(|r| !r.is_error).collect();
        let blocks: Vec<String> = ran.iter().filter_map(|r| check_block(&r.name, &r.body, &r.input, vi)).collect();
        if blocks.is_empty() { return; }
        let block = blocks.join("\n\n");
        let bodies: String = ran.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        let note = check_note(&reply, &block, &bodies);
        let answer = if note.is_empty() { block } else { format!("{block}\n\n{} {note}", block_text("todo", vi)) };
        if answer != reply { self.replace_reply(answer, "[Elisa's verified result from the check leads; the model's short note follows]"); }
    }

    /// "show the full list" after Elisa cut a dependency list short: the whole list from the last check she ran,
    /// with no model call (`data/full_list_asks.txt`). None for any other ask.
    fn full_list_line(&self) -> Option<String> {
        let last = self.history.iter().rev().filter(|m| m.role == Role::Assistant).map(texts).find(|t| !t.trim().is_empty())?;
        // (the last answer was Elisa's list cut short: it holds the words of her "more" line, in either language)
        let cut = [false, true].iter().any(|vi| block_text("more", *vi).split_once("{count}").is_some_and(|(_, tail)| !tail.trim().is_empty() && last.contains(tail.trim())));
        if !cut || !full_list_ask(context::request_body(&self.input)) { return None; }
        let mut calls: Vec<(&str, &str)> = Vec::new();
        let mut body = None;
        for m in &self.history {
            for b in &m.content {
                match b {
                    Block::ToolUse { id, name, .. } => calls.push((id, name)),
                    Block::ToolResult { tool_use_id, content, is_error: false } if calls.iter().any(|c| c.0 == tool_use_id && c.1 == "sec_deps") => body = Some(content.clone()),
                    _ => {}
                }
            }
        }
        deps_block(&body?, crate::route::vietnamese(context::request_body(&self.input)), true)
    }

    /// A search of the project this turn that found lines, whose reply names none of the files they are in: the
    /// places added after it ("(app.py:6)") when the reply is about what was found (it quotes two of the lines' words or
    /// names a symbol of their file: "the greet function contains Hello"); after Elisa's own search, a reply about
    /// none of it (or the search's own lines) gets her own line in its place ("Found "Hello" in app.py, line 6: …").
    fn grep_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        let Some(r) = self.turn_results().into_iter().find(|r| r.name == "grep" && !r.is_error) else { return };
        let hits: Vec<(String, String, String)> = r.body.lines().filter_map(|l| {
            let (f, rest) = l.split_once(':')?;
            let (n, t) = rest.split_once(':')?;
            (n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()).then(|| (f.to_string(), n.to_string(), t.trim().to_string()))
        }).collect();
        if hits.is_empty() || reply.trim().is_empty() { return; }
        let base = |f: &str| f.rsplit('/').next().unwrap_or(f).to_string();
        // (the search's own lines as the reply, "grep Hello: app.py:6: return …", are no answer: Elisa's line below)
        let raw = hits.iter().any(|(f, n, _)| reply.contains(&format!("{f}:{n}:")));
        // (an ask for the file that holds something gets the file named in a sentence in the ask's language: a reply
        // that names none, or is in another language, gets Elisa's own line, never the places added after it)
        let which = which_file_reply_falls_short(context::request_body(&self.input), &reply);
        if !raw && !which && hits.iter().any(|(f, _, _)| reply.contains(f.as_str()) || reply.contains(base(f).as_str())) { return; }
        // about what was found: a word of the lines (3 letters or more, not the ask's) or a symbol of their files
        let ask: HashSet<String> = word_set(context::request_body(&self.input)).into_iter().collect();
        let said: HashSet<String> = word_set(&reply).into_iter().collect();
        // (two of them at least: one shared word, "name" in "Hello, your name is Jonathan", is chance)
        let quoted: HashSet<String> = hits.iter().flat_map(|(_, _, t)| word_set(t)).filter(|w| w.chars().count() >= 3 && !ask.contains(w) && said.contains(w)).collect();
        let quotes = quoted.len() >= GREP_QUOTED_WORDS;
        let words: HashSet<&str> = reply.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect();
        let symbol = hits.iter().filter_map(|(f, _, _)| tools::outline_syms(f, &self.tctx)).flatten().any(|(n, _, _)| words.contains(n.as_str()));
        let own = self.own_ran("grep");
        let which = which || crate::route::which_file_term(context::request_body(&self.input)).is_some();
        let (text, why) = if !raw && !which && (quotes || symbol || !own) {
            let places: Vec<String> = hits.iter().take(GREP_SHOWN).map(|(f, n, _)| format!("{f}:{n}")).collect();
            (format!("{} ({})", reply.trim_end(), places.join(", ")), "[the reply named no place the search found; Elisa added them]")
        } else {
            let vi = crate::route::vietnamese(context::request_body(&self.input));
            // (the thing in the user's own words, "meeting code", never the search's pattern, "(?i)meeting code";
            // none when the words name none)
            let body = context::request_body(&self.input);
            let what = searched_words(body).or_else(|| r.input["pattern"].as_str().and_then(|p| pattern_in_words(p, body)))
                .map(|t| format!(" \"{t}\"")).unwrap_or_default();
            let one = |(f, n, t): &(String, String, String)| if vi { format!("{f}, dòng {n}: {t}") } else { format!("{f}, line {n}: {t}") };
            (match hits.as_slice() {
                [h] => if vi { format!("Tìm thấy{what} trong {}", one(h)) } else { format!("Found{what} in {}", one(h)) },
                _ => format!("{}\n{}", if vi { format!("Tìm thấy{what} ở:") } else { format!("Found{what} in:") }, hits.iter().take(GREP_SHOWN).map(|h| format!("- {}", one(h))).collect::<Vec<_>>().join("\n")),
            }, if raw { "[the reply was the search's own lines; Elisa said where the matches are]" } else { "[the reply named no file the search found; Elisa said where the matches are]" })
        };
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&text); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text }]; }
        self.debug(why);
    }

    /// A reply that is only a fragment (" 2.19.0", " No") after Elisa's own checks ran this turn: their findings, in
    /// plain words, in its place.
    fn fragment_fix(&mut self) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let reply = texts(last);
        if reply.split_whitespace().count() >= FRAGMENT_WORDS { return; }
        let res: Vec<OwnResult> = self.turn_results().into_iter().filter(|r| self.mcp.builtin_owns(&r.name) && !self.mcp.reason_tool(&r.name) && !r.is_error).collect();
        if res.is_empty() { return; }
        // (from the web, a short answer the results hold stands: "394,324", "Canberra"; a lone "No" does not)
        let web = res.iter().all(|r| crate::route::tool_group(&r.name, self.mcp.server_of(&r.name)) == crate::route::WEB);
        let all: String = res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        let backed = reply.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric())).any(|t| (t.chars().count() >= 3 || t.chars().any(|c| c.is_ascii_digit())) && all.contains(t));
        if web && backed { return; }
        let line = self.plain_results(&res);
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[the reply was only a fragment; Elisa gave her checks' findings]");
    }

    /// A change of quoted text in a named file ("in todo.txt change 'buy milk' to 'buy bread'", "replace "a" with "b"
    /// in notes.txt", "đổi 'buy milk' thành 'buy bread' trong todo.txt"): the words ask a change (`asks_change`, or a
    /// frame of `data/edit_asks.txt` around the quoted parts: `framed_edit`), name a file that exists and quote
    /// exactly two parts, one of them found once in the file (the old text) and the other not the same (the new).
    /// The edit call; None otherwise.
    fn quoted_edit(&self, body: &str) -> Option<(String, serde_json::Value)> {
        let is_file = |p: &str| self.resolve(p).is_file();
        let parts = quoted_parts(body);
        // no two quoted parts: the words of a change frame (`framed_edit`: "in e1.txt change hello to goodbye")
        if parts.len() != 2 {
            let (file, old, new) = framed_edit(body, &is_file)?;
            let text = std::fs::read_to_string(self.resolve(&file)).ok()?;
            if old == new || text.matches(old.as_str()).count() != 1 { return None; }
            return Some(("edit".to_string(), json!({"path": file, "old": old, "new": new})));
        }
        let [a, b] = parts.as_slice() else { return None };
        if a == b || !(asks_change(body) || framed_edit(body, &is_file).is_some()) { return None; }
        let file = path_tokens(body).into_iter().find(|p| !parts.iter().any(|q| q.contains(p.as_str())) && self.resolve(p).is_file())?;
        let text = std::fs::read_to_string(self.resolve(&file)).ok()?;
        let n = |s: &str| if s.trim().is_empty() { 0 } else { text.matches(s).count() };
        let (old, new) = match (n(a), n(b)) {
            (1, _) => (a, b),
            (0, 1) => (b, a),
            _ => return None,
        };
        Some(("edit".to_string(), json!({"path": file, "old": old, "new": new})))
    }

    /// A clear ask by meaning, with no file named and no tool named in the words ("list the files in this folder"):
    /// the examples in `data/own_tools.txt` (all-MiniLM-L6-v2): the message close to one tool's examples
    /// (`OWN_MEANING_MIN`) and nearer to them than to every `[other]` example by `OWN_MEANING_LEAD`. Its call: the
    /// section's default arguments, a required text argument from the explicit part of the words or a `*` pattern
    /// in them, a folder the words name as `path`. Only a tool that reads (Elisa's own, offered or not; never a write).
    fn meant_call(&mut self, body: &str) -> Option<(String, serde_json::Value)> {
        let (name, mut args) = meant_tool(body)?;
        let def = tools::defs_for(self.cfg.ast).into_iter().chain(self.tools.iter().cloned()).find(|d| d.name == name)?;
        if tools::access(&name) == Access::Write || self.mcp.owns(&name) { return None; }
        let star = body.split_whitespace().map(|t| t.trim_matches(|c: char| "\"'`,;?!()".contains(c))).find(|t| t.contains('*')).map(str::to_string);
        let text = explicit_span(body).map(|s| s.0).or(star);
        let props = def.input_schema["properties"].as_object().cloned().unwrap_or_default();
        for r in def.input_schema["required"].as_array().into_iter().flatten().filter_map(|r| r.as_str()) {
            if props.get(r).and_then(|p| p["type"].as_str()) == Some("string") && !r.contains("path") {
                if let Some(t) = &text { args[r] = json!(t); }
            }
            args.get(r)?;
        }
        if props.contains_key("path") {
            if let Some(d) = body.split_whitespace().map(|t| t.trim_matches(|c: char| "\"'`,;?!()".contains(c)))
                .find(|t| (t.contains('/') || t.starts_with('~')) && self.resolve(t).is_dir()) { args["path"] = json!(d); }
        }
        self.debug(&format!("[a clear ask by meaning: {name}]"));
        Some((name, args))
    }

    /// A clear ask by meaning for one of Elisa's own checks (a built-in server's tool: `sec_deps`), files named or
    /// not, in words its description does not use ("kiểm tra requirements.txt xem có gói nào bị lỗ hổng không"):
    /// its section of `data/own_tools.txt` (`meant_tool`), the section's default arguments and its required ones from
    /// the words (`fill_args`).
    fn meant_check(&mut self, body: &str) -> Option<(String, serde_json::Value)> {
        // (a file of the folder named in the words that is no text, a program, is a sign of its own: `meant_tool_signed`)
        let program = body.split_whitespace().map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.')).trim_end_matches('.'))
            .any(|w| !w.is_empty() && is_program(&self.resolve(w)));
        let (name, defaults) = meant_tool(body).or_else(|| program.then(|| meant_tool_signed(body)).flatten())?;
        if !self.mcp.builtin_owns(&name) || self.mcp.reason_tool(&name) { return None; }
        let def = self.mcp.defs().into_iter().find(|d| d.name == name)?;
        // (a file of the folder named by a bare word counts as a path too: "check the binary hello_bin")
        let mut paths = path_tokens(body);
        for w in body.split_whitespace().map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.')).trim_end_matches('.')) {
            if !w.is_empty() && !paths.iter().any(|p| p == w) && self.resolve(w).is_file() { paths.push(w.to_string()); }
        }
        let mut args = fill_args(&def.input_schema, explicit_span(body).map(|s| s.0).as_deref(), &paths, None, |p| self.resolve(p).exists())?;
        if let (Some(a), Some(d)) = (args.as_object_mut(), defaults.as_object()) { for (k, v) in d { a.entry(k.clone()).or_insert(v.clone()); } }
        self.debug(&format!("[a clear ask by meaning: {name}]"));
        Some((name, args))
    }

    /// A write or edit whose new text copies Elisa's own system prompt or one of her internal notes ("[elisa: …]",
    /// the request-only notes), whoever made the call: refused with this reason. Text the file already has, or the
    /// user's own words, is no copy.
    pub(super) fn prompt_leak(&self, name: &str, input: &serde_json::Value) -> Option<String> {
        if tools::access(name) != Access::Write || self.mcp.owns(name) { return None; }
        let text = ["content", "new", "source"].iter().filter_map(|k| input.get(*k).and_then(|v| v.as_str())).collect::<Vec<_>>().join("\n");
        if text.trim().is_empty() { return None; }
        let path = input.get("path").or_else(|| input.get("file_path")).and_then(|v| v.as_str()).unwrap_or("");
        let had = std::fs::read_to_string(self.resolve(path)).unwrap_or_default();
        let own = format!("{}\n{}", context::own_prompt(&self.system, self.project.as_deref()), self.sent_notes);
        let copied = copies_own(&text, &had, context::request_body(&self.input), &own);
        copied.then(|| "refused: the new text copies Elisa's own instructions or notes, which never go into a file. Write only the content the user asked for.".to_string())
    }

    /// A path as the tools read it: `~/` from home, a relative one from the working directory.
    fn resolve(&self, p: &str) -> PathBuf {
        match p.strip_prefix("~/") {
            Some(r) => std::env::var("HOME").map(|h| Path::new(&h).join(r)).unwrap_or_else(|_| self.cwd.join(p)),
            None => self.cwd.join(p),
        }
    }

    /// A tool the words ask for by its own name or description, with every required argument found in the words:
    /// the words outside the explicit part (`explicit_span`) share at least two of the tool's name parts and the words
    /// of its description's first sentence, among them its full name or the description's first word (its verb:
    /// "Run a zsh command", "Search the web"). A path argument is a path the words name, a URL one their URL, a
    /// true/false one false (`set_by_meaning` may set it), any other text argument the explicit part, else a word
    /// that stands out (`search_term`: capitalised, or outside the dictionary only for a tool named outright). A
    /// description's verb alone is no ask without such an argument ("I went for a run this morning and now I'm
    /// starving" names no command: "run" is the user's noun), and a command argument must start with a program of
    /// this machine (`command_args_run`). Only one best tool, else none. Tools whose content the model writes (write,
    /// edit) are never made this way.
    fn described_call(&self, body: &str) -> Option<(String, serde_json::Value)> {
        let span = explicit_span(body);
        let ask = span.as_ref().map_or(body.to_string(), |s| s.1.clone());
        let words = word_set(&ask);
        let mut own_defs = tools::defs_for(self.cfg.ast);
        // Elisa's own MCP tools this turn does not offer too (a gated server, a window too small for tools): a clear
        // ask for one is widened to it or told plainly that it cannot run (`Own::Unoffered`); only a server the words
        // are about (`route::signals`), so "read notes.txt" never becomes a PDF read
        let about = crate::route::signals(body);
        own_defs.extend(self.mcp.defs().into_iter().filter(|d| self.mcp.builtin_owns(&d.name) && !self.mcp.reason_tool(&d.name)
            && about.has(crate::route::tool_group(&d.name, self.mcp.server_of(&d.name)))));
        own_defs.retain(|d| !self.tools.iter().any(|t| t.name == d.name));
        let paths = path_tokens(body);
        let url = body.split_whitespace().map(|t| t.trim_matches(|c: char| "\"'`<>()[],;".contains(c)))
            .find(|t| t.starts_with("http://") || t.starts_with("https://"));
        let signals = crate::route::signals(body);
        // an explicit web ask ("search the web: …") is for a web tool, never a local one its verb also fits
        let web_ask = crate::route::web_ask(body);
        let mut best: Vec<(usize, String, serde_json::Value, String)> = Vec::new();
        for t in self.tools.iter().chain(&own_defs) {
            if tools::access(&t.name) == Access::Write && !self.mcp.owns(&t.name) { continue; }
            // a web tool only when the words are about the web ("search the project for …" is a local search)
            let group = crate::route::tool_group(&t.name, self.mcp.server_of(&t.name));
            let web = group == crate::route::WEB;
            if web && !signals.has(crate::route::WEB) { continue; }
            if web_ask && !web && !words.contains(&t.name.to_lowercase()) { continue; }
            let first = t.description.split(". ").next().unwrap_or("");
            let desc: Vec<String> = first.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(str::to_lowercase).collect();
            let named = words.contains(&t.name.to_lowercase());
            let keys: HashSet<String> = t.name.to_lowercase().split('_').map(String::from).chain(desc.iter().cloned()).collect();
            // (a word of the ask that is a form or a short form of one of its words counts: "vulnerable" for
            // "vulnerabilities", "dependencies" for "deps": `same_word`)
            let shared = keys.iter().filter(|k| words.iter().any(|w| same_word(k, w))).count() + if named { 1 } else { 0 };
            let verb_hit = desc.first().is_some_and(|v| words.contains(v));
            // (or two of its words in an ask of its own group: "scan the project for leaked keys" for "Find leaked
            // secrets in project files")
            if !(named || verb_hit || (shared >= 2 && group != crate::route::CODE && signals.has(group))) { continue; }
            // the text argument: the explicit part, else for a web tool the topic, else the one word that stands out
            // (capitalised or not a dictionary word: "search the project for the word Hello"); one shared word is
            // enough only with such an argument
            // (a word the tool's own name or description uses is the kind of thing it takes, not the value: "decode the
            // JWT in session.py" gave `sec_jwt` the token "JWT"; the value is then read from the named file)
            let term = search_term(body, named).filter(|w| !keys.contains(&w.to_lowercase()));
            let text = span.as_ref().map(|s| s.0.clone()).or_else(|| if web { Some(self.web_topic(body)) } else { term });
            if shared < 2 && text.is_none() { continue; }
            let Some(args) = fill_args(&t.input_schema, text.as_deref(), &paths, url, |p| self.resolve(p).exists()) else { continue };
            // (a command from the words starts with a program of this machine, never a word of the sentence)
            if !command_args_run(&args) { continue; }
            best.push((shared, t.name.clone(), args, first.to_string()));
        }
        best.sort_by_key(|b| std::cmp::Reverse(b.0));
        // a tie: the description nearest the ask in meaning, when it is clearly nearer (`route::meaning_scores`)
        if let [a, b, ..] = best.as_slice() {
            if a.0 == b.0 {
                let top: Vec<&str> = best.iter().take_while(|x| x.0 == a.0).map(|x| x.3.as_str()).collect();
                let scores = crate::route::meaning_scores(&ask, &top)?;
                let mut order: Vec<usize> = (0..top.len()).collect();
                order.sort_by(|i, j| scores[*j].total_cmp(&scores[*i]));
                return (scores[order[0]] - scores[order[1]] >= MEANING_LEAD).then(|| (best[order[0]].1.clone(), best[order[0]].2.clone()));
            }
        }
        best.first().map(|a| (a.1.clone(), a.2.clone()))
    }

    /// Runs Elisa's own calls (`own_asks`) as one step of the turn: the calls as an assistant message, then their
    /// results, as if the model had made them, so the model's reply comes from what they returned. A save gets its
    /// content first: Elisa's own exact count when the words ask to save a count of what was read (`counted_content`),
    /// else the model writes it in words, with no tools, after the reads (`own_content`); no usable content, no write.
    /// In a window too small for tools the results go in the turn's message as text (`run_own_inline`).
    /// Ok(false) when nothing ran (the model then has the turn as before); Err(line): Elisa's own reply ends the turn
    /// (a result that cannot fit the window, a save with nothing to save).
    /// Ok((whether anything ran, the file saved and its content when a save was the whole ask: reads and counts
    /// besides, nothing else)).
    fn run_own(&mut self, own: Vec<Own>) -> Result<(bool, Option<(String, String)>), String> {
        let mut calls = Vec::new();
        let mut save = None;
        for o in own { match o { Own::Call(n, a) => calls.push((n, a)), Own::Save(f) => save = Some(f), Own::Unoffered(_) => {} } }
        let reads: Vec<String> = calls.iter().filter(|c| c.0 == "read").filter_map(|c| c.1["path"].as_str().map(str::to_string)).collect();
        let only_reads = calls.iter().all(|c| c.0 == "read");
        let mut saved = None;
        let mut ran = false;
        let tiny = self.tiny();
        if !calls.is_empty() {
            if tiny { self.run_own_inline(calls)?; } else { self.own_step(calls); }
            ran = true;
        }
        if let Some(f) = save {
            // the files read this turn, and the last answer when it is one (a plan, a list: `SAVED_ANSWER_MIN_WORDS` or
            // more; Elisa's own short lines never are; decorations and numbering are fine)
            let read_text: String = reads.iter().filter_map(|p| std::fs::read_to_string(self.resolve(p)).ok()).collect::<Vec<_>>().join("\n");
            let last = self.history.iter().rev().find(|m| m.role == Role::Assistant).map(texts)
                .filter(|t| t.split_whitespace().count() >= SAVED_ANSWER_MIN_WORDS && !t.trim_start().starts_with('[')).map(|t| format!("{}\n", t.trim()));
            let body = context::request_body(&self.input).replace(f.as_str(), " ");
            let given = reads.is_empty().then(|| given_text(context::request_body(&self.input), &f)).flatten();
            // "save that plan to plan.md", "save it as list.csv": a save of something said before (`save_points_back`)
            // takes the earlier reply that holds it (`answer_in_chat`), never new words of the model's; with none,
            // Elisa says so (below)
            let back = reads.is_empty() && given.is_none() && save_points_back(context::request_body(&self.input), &f);
            let thing = back.then(|| save_thing(context::request_body(&self.input), &f)).flatten();
            let content = self.counted_content(&reads)
                // "export the three region names from sales.csv to regions.txt": that column of the table read, as it is
                .or_else(|| self.exported_content(&reads))
                // "save the word hello to deny.txt": the text the words give, as given
                .or(given)
                // "save that plan to plan.md" after the plan: the plan itself, as it was shown (no rewrite to wait
                // for, nothing changed on the way); another save right after an answer sharing its words: that answer
                .or_else(|| if back { answer_in_chat(&self.history, thing.as_deref().unwrap_or("")) } else { last.clone().filter(|t| reads.is_empty() && crate::reply_check::shares_words(&body, t)) })
                // else the model writes it, from the files read this turn when there are any: content that has
                // nothing of them ("The content of notes.txt is not provided…") is no summary of them
                // (never the model's own bracketed asides, "[This meeting is scheduled for …]": notes to itself, no
                // part of the file, `without_bracket_notes`)
                .or_else(|| if back { None } else { self.own_content(&f).map(|c| without_bracket_notes(&unnumbered(&c))).filter(|c| usable_content(c) && adds_words(c, &f)
                    && (read_text.is_empty() || crate::reply_check::shares_words(c, &read_text))) })
                .or_else(|| last.filter(|_| reads.is_empty() && !back))
                // the model's words had nothing of the files read, and the ask gives a count of lines ("a one-line
                // summary of notes.txt"): the files' own lines held to that count (`fit_lines` below), never a save
                // left undone
                .or_else(|| (!read_text.trim().is_empty() && asked_lines(context::request_body(&self.input)).is_some()).then(|| read_text.clone()))
                // (never one of Elisa's own marks: an attachment marker a model copied, a note)
                .map(|c| format!("{}\n", without_internal_lines(&c))).filter(|c| !c.trim().is_empty())
                .filter(|c| self.prompt_leak("write", &json!({"path": f, "content": c})).is_none())
                // "a one-line summary": that many lines (`fit_lines`)
                // (with no sentence narrating the ask: "The user wants me to write…" is never the user's text; nor a
                // lead-in to the user, "Okay, here's a short marketing plan …:")
                // (line breaks written as text, "…ORCHID-42\\nProject: …", from a model that escaped them: real ones)
                .map(|c| if c.trim_end().contains('\n') || !c.contains("\\n") { c } else { c.replace("\\n", "\n") })
                .map(|c| crate::reply_check::without_meta_narration(&c).map_or(c, |t| format!("{t}\n"))).map(|c| without_lead_in(&c)).filter(|c| !c.trim().is_empty())
                // (held to a count of lines from files read: only what has something of them, never made-up text)
                .map(|c| asked_lines(context::request_body(&self.input)).map_or_else(|| c.clone(), |n| fit_lines(&from_read(&c, &read_text), n)));
            match content {
                Some(content) => {
                    let call = vec![("write".to_string(), json!({"path": f, "content": content}))];
                    if tiny { self.run_own_inline(call)?; } else { self.own_step(call); }
                    ran = true;
                    if only_reads { saved = Some((f.clone(), content)); }
                }
                // nothing to save: said plainly, never a made-up "saved" (after the reads too: a reply about what was
                // read would leave the user thinking the file was written); a save of something said before that no
                // reply holds: that there is no such thing in the chat yet (`nothing_to_save_line`)
                None if back => return Err(nothing_to_save_line(&self.input, thing.as_deref(), &f)),
                None => return Err(match crate::reply_lang::detect(&self.input) {
                    Some(crate::reply_lang::Lang::Vi) => format!("Tôi chưa lưu {f}: không có nội dung nào để ghi vào tệp."),
                    _ => format!("I didn't save {f}: I had no text to put in it."),
                }),
            }
        }
        Ok((ran, saved))
    }

    /// Elisa's own calls in a window too small for tools (none are offered there): each runs through the usual
    /// approval (`own_call`: a path outside the folder asks first), and goes in the history as her own step when its
    /// results fit the window whole (a model without tool calling gets them as text: `api::send_tools`; an injection
    /// in a file stays fenced as data: `guard_output`). Results that cannot fit are not sent: Err(Elisa's line).
    fn run_own_inline(&mut self, calls: Vec<(String, serde_json::Value)>) -> Result<(), String> {
        let at = self.history.len();
        let mut results = Vec::new();
        for (i, (name, input)) in calls.iter().enumerate() {
            let (content, is_error) = self.own_call(name, input);
            let content = if is_error { content } else { self.with_count_note(name, input, content) };
            self.keep_own_result(name, input, &content, is_error);
            results.push(Block::ToolResult { tool_use_id: format!("elisa_{at}_{i}"), content, is_error });
        }
        let uses: Vec<Block> = calls.iter().enumerate().map(|(i, (name, input))| Block::ToolUse { id: format!("elisa_{at}_{i}"), name: name.clone(), input: input.clone() }).collect();
        let mut step = [Message::new(Role::Assistant, uses), Message::new(Role::User, results)];
        let room = self.max_ctx.saturating_sub(self.est_safe() + context::ctx_margin(self.max_ctx) + MIN_OUT);
        // web results are cut to fit (their first results, the ones that matter most); a file is never cut
        let web = calls.iter().all(|(n, _)| crate::route::tool_group(n, self.mcp.server_of(n)) == crate::route::WEB);
        for _ in 0..WEB_CUT_TRIES {
            if !web || estimate_messages(&step) <= room { break; }
            for b in step[1].content.iter_mut() {
                if let Block::ToolResult { content, .. } = b { let keep = content.chars().count() * 2 / 3; *content = content.chars().take(keep).collect(); }
            }
        }
        if estimate_messages(&step) > room {
            let what = calls.iter().map(|(n, i)| i["path"].as_str().map_or_else(|| n.clone(), str::to_string)).collect::<Vec<_>>().join(", ");
            return Err(match crate::reply_lang::detect(&self.input) {
                Some(crate::reply_lang::Lang::Vi) => format!("{what} quá dài so với cửa sổ {} token của {}, nên tôi không thể đọc hết ở đây.", self.max_ctx, self.cfg.model),
                _ => format!("{what} is too long for {}'s {}-token window, so I can't take it in here.", self.cfg.model, self.max_ctx),
            });
        }
        self.history.extend(step);
        Ok(())
    }

    /// The content of a save whose words ask for a count of the files this turn read ("read notes.txt, count the
    /// words, save the count to count.txt"): Elisa's exact count (`mcp_reason::count_units`), the number alone for one
    /// file and one unit. None when the words ask no count or nothing was read.
    fn counted_content(&self, reads: &[String]) -> Option<String> {
        let units = asked_counts(context::request_body(&self.input));
        if units.is_empty() || reads.is_empty() { return None; }
        let texts: Vec<(String, String)> = reads.iter().filter_map(|p| std::fs::read_to_string(self.resolve(p)).ok().map(|t| (p.clone(), t))).collect();
        if texts.is_empty() { return None; }
        let n = |u: &str, t: &str| match u { "words" => t.split_whitespace().count(), "lines" => t.lines().count(), _ => t.chars().count() };
        if let ([(_, t)], [u]) = (texts.as_slice(), units.as_slice()) { return Some(format!("{}\n", n(u, t))); }
        Some(texts.iter().map(|(p, t)| format!("{p}: {}\n", units.iter().map(|u| format!("{} {u}", n(u, t))).collect::<Vec<_>>().join(", "))).collect())
    }

    /// The content of a save that asks for one column of the one table this turn read ("export the three region names
    /// from sales.csv to regions.txt"): the values of the column whose header the words name, without the header, one
    /// per line (`exported_column`). None when no one table was read, the words name no one column, or they ask a fact
    /// computed from the table (`situation::table_note`: "save the total to t.txt" is the model's turn, with Elisa's
    /// number beside it).
    fn exported_content(&self, reads: &[String]) -> Option<String> {
        let [path] = reads else { return None };
        let full = self.resolve(path);
        if !crate::mcp_docs::is_sheet(&full) { return None; }
        let ask = context::request_body(&self.input);
        if crate::situation::table_note(ask, &full).is_some() { return None; }
        let (_, rows) = crate::mcp_docs::sheet_rows(&full).ok()?.into_iter().next()?;
        exported_column(&ask.replace(path.as_str(), " "), &rows)
    }

    /// One step of Elisa's own calls into the history: the calls, then their results (the same approval, audit,
    /// activity rows and tool lines as the model's calls).
    fn own_step(&mut self, calls: Vec<(String, serde_json::Value)>) {
        let at = self.history.len();
        let uses: Vec<Block> = calls.iter().enumerate().map(|(i, (name, input))| Block::ToolUse { id: format!("elisa_{at}_{i}"), name: name.clone(), input: input.clone() }).collect();
        self.history.push(Message::new(Role::Assistant, uses));
        let mut results = Vec::new();
        for (i, (name, input)) in calls.iter().enumerate() {
            let (content, is_error) = self.own_call(name, input);
            let content = if is_error { content } else { self.with_count_note(name, input, content) };
            self.keep_own_result(name, input, &content, is_error);
            results.push(Block::ToolResult { tool_use_id: format!("elisa_{at}_{i}"), content, is_error });
        }
        self.history.push(Message::new(Role::User, results));
        self.fit_newest_results();
    }

    /// A `read` result with the counts this turn's words ask about (`mcp_reason::count_note`: words, lines …, counted
    /// by Elisa from the file), for the model only: the user's tool row already showed the result.
    pub(super) fn with_count_note(&self, name: &str, input: &serde_json::Value, content: String) -> String {
        let p = input.get("path").or_else(|| input.get("file_path")).and_then(|v| v.as_str()).unwrap_or("");
        let ask = context::request_body(&self.input);
        // a table file: the facts the ask names, computed by Elisa (`situation::table_note`)
        let content = match crate::situation::table_note(ask, &self.resolve(p)) { Some(n) => format!("{content}\n{n}"), None => content };
        // a read's file, or the one project file a command names ("wc todo.txt": Elisa's count, not the model's
        // reading of wc's columns)
        let named = input.get("command").and_then(|v| v.as_str())
            .map(|c| path_tokens(c).into_iter().filter(|f| self.cwd.join(f).is_file()).collect::<Vec<_>>()).filter(|f| f.len() == 1).and_then(|mut f| f.pop());
        let p = match (name, &named) { ("read", _) => p, ("bash", Some(f)) => f.as_str(), _ => return content };
        // (a counting ask in other words, "đếm số dòng trong todo.txt": the same note from `asked_counts`)
        let own_note = |t: &str| { let u = asked_counts(ask); (!u.is_empty()).then(|| format!("[elisa: counted exactly: {}; use this number]", count_text(p, t, &u, false).trim_end_matches('.'))) };
        match std::fs::read_to_string(self.resolve(p)).ok().and_then(|t| crate::mcp_reason::count_note(ask, p, &t).or_else(|| own_note(&t))) {
            Some(n) => format!("{content}\n{n}"),
            None => content,
        }
    }

    /// One of Elisa's own calls, through the approval path the model's calls take (`approve`: writes, commands and
    /// paths outside the folder ask the user as always).
    fn own_call(&mut self, name: &str, input: &serde_json::Value) -> (String, bool) {
        let summary = tools::summary(name, input);
        let phrase = crate::activity::describe(name, input, &self.cwd, self.mcp.server_of(name), self.mcp.remote_of(name));
        let aid = self.acts.begin(&mut *self.fe, &phrase);
        self.fe.tool_start(&summary);
        let owns = self.mcp.owns(name);
        let start_err = if owns { self.mcp_start(name).err() } else { None };
        let (content, is_error) = if let Some(why) = start_err {
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
                    // (the user sees the decision; the model also gets what to tell them)
                    (self.denial_for_model(name, &why), true)
                }
                Ok(()) => {
                    let answer = self.answer.take().unwrap_or("auto");
                    self.audit_tool("approval", name, &summary, &[("answer", answer)]);
                    let write = tools::access(name) == Access::Write;
                    if write && !owns { self.ledger.before(input, &self.cwd); }
                    let o = if owns { self.mcp.call(name, input, &CANCEL, self.cfg.tool_output_max_chars) } else { tools::run(name, input, &self.tctx) };
                    if !o.is_error && !owns { self.ledger.record(name, input, &self.cwd); }
                    self.turn_guard.ran(name, input, write, !o.is_error, true);
                    if !owns { self.turn_guard.result(name, input, &o.content, o.is_error); }
                    self.audit_tool("tool_call", name, &summary, &[("outcome", if o.is_error { "error" } else { "ok" })]);
                    self.fe.tool_done(&o.content, o.is_error);
                    (self.guard_output(name, input, o.content), o.is_error)
                }
            }
        };
        self.acts.end(&mut *self.fe, aid, name, &content, is_error);
        let s = context::one_line(&summary, 100);
        self.tool_lines.push(if is_error { format!("[{s} → {}]", context::one_line(&content, 60)) } else { format!("[{s}]") });
        (content, is_error)
    }

    /// The content of a file the user asked to save to (`route::save_ask`): the model writes it in words, with no
    /// tools, from the conversation so far (an earlier answer it refers to, a read this turn made), not shown while it
    /// comes. None when it wrote nothing, was cut by the output limit, or the request failed.
    fn own_content(&mut self, path: &str) -> Option<String> {
        self.own_words(&format!("\n\n(Write only the full content for the file {path}, nothing before or after it: it is saved exactly as you write it.)"), "content", true)
    }

    /// The model's words for one argument of a call Elisa makes (a file's content, a symbol's new source): the
    /// conversation so far plus `note`, with no tools, not shown while it comes. A reply that is a call anyway (any
    /// shape `backend::text_calls` reads) gives that call's `arg`; markup is never taken as the words. None when it
    /// wrote nothing usable, was cut by the output limit, or the request failed.
    /// `whole`: the whole conversation (a save of an earlier answer); else this turn alone, its ask and what its
    /// tools returned (new code: no earlier reply, Elisa's own lines among them, can become its source).
    fn own_words(&mut self, note: &str, arg: &str, whole: bool) -> Option<String> {
        let from = if whole { 0 } else { self.turn_cap(&self.history).min(self.history.len()) };
        let mut msgs = self.history[from..].to_vec();
        // a reply already given this turn (one that made no change: `own_fix` takes it back) is not the request's
        // end: a request ending on the model's own words gets nothing more from many servers
        while msgs.len() > 1 && msgs.last().is_some_and(|m| m.role == Role::Assistant) { msgs.pop(); }
        let last = msgs.last_mut()?;
        match last.content.iter_mut().rev().find_map(|b| match b { Block::ToolResult { content, .. } | Block::Text { text: content } => Some(content), _ => None }) {
            Some(c) => c.push_str(note),
            None => last.content.push(Block::Text { text: note.to_string() }),
        }
        let room = self.max_ctx.saturating_sub(self.est_safe() + context::ctx_margin(self.max_ctx) + 64);
        let max_tokens = self.cfg.max_output.min(room).max(MIN_OUT);
        let no_tools: [ToolDef; 0] = [];
        let req = Request { system: &self.system, tools: &no_tools, messages: &msgs, max_tokens };
        self.fe.waiting();
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).map_err(|e| self.debug(&format!("[the model's words for Elisa's call failed: {e}]"))).ok()?;
        if r.stop_reason == "max_tokens" || r.stop_reason == "cancelled" {
            self.debug(&format!("[the model's words for Elisa's call stopped ({}); not used]", r.stop_reason));
            return None;
        }
        // a call the server read out of the reply itself (no tools were offered, so it is the words, not a call)
        let structured: Vec<(String, serde_json::Value)> = r.content.iter().filter_map(|b| match b { Block::ToolUse { name, input, .. } => Some((name.clone(), input.clone())), _ => None }).collect();
        let raw = texts(&Message::new(Role::Assistant, r.content));
        let raw = context::split_state(&raw).0;
        self.debug(&format!("[the model's words for Elisa's call: {}]", context::one_line(&if raw.trim().is_empty() { format!("{structured:?}") } else { raw.clone() }, 160)));
        // a call written anyway: its argument (every tool, so any name the model picked is read)
        let mut defs = self.tools.clone();
        defs.extend(tools::defs_for(self.cfg.ast));
        let (_, mut calls) = crate::backend::text_calls(&raw, &defs);
        calls.extend(structured);
        // a call written as plain JSON that the call reader did not take (an extra key beside `arguments`)
        if calls.is_empty() { calls.extend(loose_call(&raw)); }
        // (that argument, else the longest text argument of the call: a model may name it `code` or `new`)
        // (never the text being replaced, `old`, nor a path)
        let longest = |a: &serde_json::Value| a.as_object().and_then(|o| o.iter().filter(|(k, _)| *k != "old" && !k.contains("path"))
            .filter_map(|(_, v)| v.as_str()).max_by_key(|v| v.len()).map(str::to_string));
        if let Some(v) = calls.iter().find_map(|(_, a)| a.get(arg).and_then(|v| v.as_str()).map(str::to_string)).or_else(|| calls.iter().find_map(|(_, a)| longest(a))) {
            return (!v.trim().is_empty()).then(|| format!("{}\n", v.trim_end()));
        }
        if context::looks_like_text_tool_call(&raw) { return None; }
        let t = crate::route::strip_echoed_notes(raw.trim());
        let t = unfenced(t.trim());
        // words around one fenced block ("Here is the fix: ```…``` Done."): the block is the content
        let t = one_block(t).unwrap_or(t);
        (!t.trim().is_empty()).then(|| format!("{}\n", t.trim_end()))
    }

    /// New code for one symbol from a request of its own (`own_fix`): no tools, no conversation, a short system
    /// prompt (`OWN_FIX_SYSTEM`), the symbol's code as it is and the user's words. The code alone (a fenced block taken
    /// out of words around it); None when it failed or wrote nothing.
    fn fix_words(&mut self, sym: &str, file: &str, now: &str, ask: &str) -> Option<String> {
        if CANCEL.load(Ordering::SeqCst) || now.trim().is_empty() { return None; }
        let q = format!("`{sym}` in {file}:\n```\n{now}\n```\n\nThe user says: \"{}\"\n\nWrite the corrected `{sym}` with that fault fixed, as one code block, nothing else.", ask.trim());
        let msgs = [Message::user_text(q)];
        let no_tools: [ToolDef; 0] = [];
        let req = Request { system: OWN_FIX_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: self.cfg.max_output.clamp(MIN_OUT, OWN_FIX_TOKENS) };
        self.fe.waiting();
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).map_err(|e| self.debug(&format!("[the new code request failed: {e}]"))).ok()?;
        if r.stop_reason == "max_tokens" || r.stop_reason == "cancelled" { return None; }
        let raw = texts(&Message::new(Role::Assistant, r.content));
        self.debug(&format!("[the new code for {sym}: {}]", context::one_line(&raw, 160)));
        let t = unfenced(raw.trim());
        let t = one_block(t).unwrap_or(t);
        (!t.trim().is_empty()).then(|| format!("{}\n", t.trim_end()))
    }

    /// The topic of a web search ask: the words without the ask's frame and politeness (`strip_query_frames`: "can u
    /// serach the web for who won the world cup in 2022" → "who won the world cup in 2022", "giúp mình dân số
    /// Iceland" → "dân số Iceland"), then those after the search tool's verb, without the words its description
    /// shares ("search the web for the latest news about Apple" → "latest news about Apple"); the whole message when
    /// nothing is left.
    fn web_topic(&self, body: &str) -> String {
        let body = &strip_query_frames(body);
        let Some(def) = self.tools.iter().find(|t| crate::route::tool_group(&t.name, self.mcp.server_of(&t.name)) == crate::route::WEB && t.input_schema["properties"].get("query").is_some()) else { return body.trim().to_string() };
        let desc: HashSet<String> = word_set(&format!("{} {}", def.name.replace('_', " "), def.description)).into_iter().collect();
        let verb = word_set(&def.description).into_iter().next().unwrap_or_default();
        let toks: Vec<&str> = body.split_whitespace().collect();
        let low = |t: &str| t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let from = toks.iter().position(|t| low(t) == verb).map_or(0, |i| i + 1);
        let rest: Vec<&str> = toks[from..].iter().copied().skip_while(|t| low(t).is_empty() || desc.contains(&low(t))).collect();
        let topic = rest.join(" ").trim_end_matches(['?', '.', '!']).trim().to_string();
        if topic.is_empty() { body.trim().to_string() } else { topic }
    }

    /// A command the words name to use on a file of the project: a word that is a program on this machine's PATH and
    /// not a word of its dictionary (`/usr/share/dict/words`: "rm", "wc", "grep", not "cat" or "look"), with the flags
    /// after it and the project files the words name ("delete todo.txt with rm" → `rm todo.txt`, "use wc -l on
    /// notes.txt" → `wc -l notes.txt`). With no file named, the program the ask ends with, right after a word the
    /// `[bash]` frames put before their command ("how much disk space is free here? use df" → `df`:
    /// `named_program`). None otherwise.
    fn command_ask(&self, body: &str) -> Option<String> {
        let files: Vec<String> = path_tokens(body).into_iter().filter(|p| !Path::new(p).is_absolute() && self.cwd.join(p).is_file()).collect();
        if dictionary().is_empty() { return None; }
        if files.is_empty() { return named_program(body); }
        let toks: Vec<&str> = body.split_whitespace().map(|t| t.trim_matches(|c: char| "?,!;:\"'`()".contains(c))).collect();
        let at = toks.iter().position(|t| is_program_word(t))?;
        let mut cmd: Vec<String> = std::iter::once(toks[at].to_string()).chain(toks[at + 1..].iter().take_while(|t| t.starts_with('-')).map(|t| t.to_string())).collect();
        for f in files { if !cmd.contains(&f) { cmd.push(f); } }
        Some(cmd.join(" "))
    }

    /// The answer to an ask to see a whole file (`route::tool_ask_by_meaning` "show") that Elisa read herself this
    /// turn: that one file as it reads (no line numbers, none of her notes), when it is short enough to show whole
    /// (`OWN_ANSWER_CHARS`). Secrets come out cut short ("sk-l…", `secrets::mask_shown`). None otherwise.
    fn shown_file(&self) -> Option<String> {
        if crate::route::tool_ask_by_meaning(context::request_body(&self.input)) != Some("show") { return None; }
        let at = self.history.iter().rposition(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. })))?;
        let mut reads = Vec::new();
        for m in &self.history[at + 1..] {
            for b in &m.content {
                if let Block::ToolUse { id, name, input } = b { if name == "read" { reads.push((id.clone(), input["path"].as_str().unwrap_or("").to_string())); } else { return None; } }
            }
        }
        let [(id, path)] = reads.as_slice() else { return None };
        let raw = self.history[at + 1..].iter().flat_map(|m| &m.content).find_map(|b| match b {
            Block::ToolResult { tool_use_id, content, is_error: false } if tool_use_id == id => Some(content.clone()),
            _ => None,
        })?;
        let body: Vec<String> = raw.lines().filter(|l| !l.starts_with("<untrusted") && !l.starts_with("</untrusted") && !l.trim_start().starts_with("[elisa"))
            .map(|l| { let t = l.trim_start(); match t.split_once('\t') { Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => rest.to_string(), _ => l.to_string() } })
            .collect();
        let text = crate::secrets::mask_shown(body.join("\n").trim_end());
        if text.trim().is_empty() || text.len() > OWN_ANSWER_CHARS { return None; }
        let name = Path::new(path).file_name().map_or(path.clone(), |n| n.to_string_lossy().to_string());
        let hidden = text.contains('…') && text != body.join("\n").trim_end() || raw.contains("[redacted:");
        let vi = crate::route::vietnamese(context::request_body(&self.input));
        let note = match (hidden, vi) {
            (false, _) => "",
            (true, false) => "\n\nSecret values are shown cut short to keep them private.",
            (true, true) => "\n\nCác giá trị bí mật được rút gọn để giữ kín.",
        };
        Some(format!("{} {name}:\n```\n{text}\n```{note}", if vi { "Đây là" } else { "Here is" }))
    }

    /// Elisa's own line for a clear ask whose tool this session cannot offer (`Own::Unoffered`), when nothing else
    /// was asked: why, from the session's facts (the code runner's setting, the model's window).
    fn unoffered_line(&self, own: &[Own], input: &str) -> Option<String> {
        if own.iter().any(|o| !matches!(o, Own::Unoffered(_))) { return None; }
        let Own::Unoffered(name) = own.first()? else { return None };
        // in plain words, in the user's language: what the tool does (its description), never its name
        let vi = crate::route::vietnamese(context::request_body(input));
        let desc = self.tools.iter().map(|t| (t.name.clone(), t.description.clone()))
            .chain(tools::defs_for(self.cfg.ast).into_iter().map(|t| (t.name, t.description)))
            .chain(self.mcp.defs().into_iter().map(|t| (t.name, t.description)))
            .find(|(n, _)| n == name).map(|(_, d)| d).unwrap_or_default();
        let what = crate::route::tool_action(&desc);
        let (model, ctx) = (&self.cfg.model, self.max_ctx);
        let code = crate::mcp_run::tools().as_array().is_some_and(|a| a.iter().any(|t| t["name"] == name.as_str()));
        if code && self.cfg.sandbox == "off" {
            let tiny = match (self.tiny(), vi) {
                (false, _) => String::new(),
                (true, false) => format!(" Also, {model} has too small a context window ({ctx} tokens) for tools."),
                (true, true) => format!(" Ngoài ra, cửa sổ ngữ cảnh của {model} ({ctx} token) quá nhỏ để dùng công cụ."),
            };
            return Some(if vi {
                format!("Mình không chạy được mã ở đây: trình chạy mã đang tắt, nên mình sẽ không đoán kết quả. Để bật, đặt sandbox = \"colima\" trong ~/.config/elisa/config.toml (mã sẽ chạy ngoại tuyến trong Colima).{tiny}")
            } else {
                format!("I can't run code here: the code runner is off, so I won't guess the output. To turn it on, set sandbox = \"colima\" in ~/.config/elisa/config.toml (snippets then run offline in Colima).{tiny}")
            });
        }
        let can_t = what.unwrap_or_else(|| "do that".to_string());
        Some(match (self.tiny(), vi) {
            (true, false) => format!("I can't {can_t} with {model}: its context window ({ctx} tokens) is too small for tools, so I won't guess what it would give."),
            (true, true) => format!("Mình không làm được việc này với {model}: cửa sổ ngữ cảnh ({ctx} token) quá nhỏ để dùng công cụ, nên mình sẽ không đoán kết quả."),
            (false, false) => format!("I can't {can_t} in this session, so I won't guess what it would give."),
            (false, true) => "Mình không làm được việc này trong phiên này, nên mình sẽ không đoán kết quả.".to_string(),
        })
    }


    /// A change the user asked for in a named file of the project ("the add function in app.py subtracts; fix it")
    /// that no edit made this turn: the model writes the new source in words (`said_own`) and Elisa makes the edit
    /// through the usual approval: `edit_symbol` when the words name a symbol of the file, else `write` of the whole
    /// file (both show the change before asking). The model then answers from the edit's result. A change ask is
    /// one the notes' claim check reads as a change in the user's words (`context::verify_claims` on a done list).
    fn own_fix(&mut self) -> Option<TurnEnd> {
        // (a change call that failed, "missing string field 'content'", changed nothing)
        let changed = |l: &String| ["[write ", "[edit ", "[edit_symbol ", "[bash ", "[run_code "].iter().any(|p| l.starts_with(p)) && !l.contains(" → ");
        if self.tool_lines.iter().any(changed) || self.small_talk_turn { return None; }
        let body = context::request_body(&self.input).to_string();
        // (a command or a change the user said no to is no fix to make: "delete todo.txt with rm", then "no")
        if self.tool_lines.iter().any(|l| l.starts_with("[bash ") || l.starts_with("[run_code ")) || self.said_no_this_turn() { return None; }
        let file = path_tokens(&body).into_iter().find(|p| !Path::new(p).is_absolute() && self.cwd.join(p).is_file())?;
        // the change words without the file's own names ("show me just the add function": `add` is a name here)
        if !asks_change(&self.without_symbols(&body, &file)) { return None; }
        // (a window too small for tools offers none: Elisa's own built-in calls still run there, `run_own_inline`)
        let can = |n: &str, s: &mut Self| s.tools.iter().any(|t| t.name == n) || (s.tiny() && tools::defs_for(s.cfg.ast).iter().any(|d| d.name == n)) || s.widen_for(n);
        let symbol = word_set(&body).into_iter().filter(|w| w.len() >= 2)
            .find(|w| tools::symbol_lines(&json!({"path": file, "name": w}), &self.tctx).is_some());
        let by_symbol = match symbol {
            Some(sym) if can("edit_symbol", self) => {
                let before = std::fs::read_to_string(self.cwd.join(&file)).unwrap_or_default();
                // (the user's words and the code as it is now, next to the request: a small model fixes what it sees)
                let now = tools::symbol_lines(&json!({"path": file, "name": sym}), &self.tctx)
                    .map(|(a, b)| before.lines().skip(a - 1).take(b + 1 - a).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                let ask = format!("\n\n(The user says: \"{}\". `{sym}` in {file} is now:\n```\n{now}\n```\nWrite only the corrected source of `{sym}`, with that fault fixed: the whole symbol as it should be, nothing before or after it.)", body.trim());
                // (from this turn's ask and the lines read alone, never an earlier reply: code that copies one is none)
                let words = |s: &mut Self, note: &str| s.own_words(note, "source", false).map(|t| code_unnumbered(&t, &before))
                    .filter(|t| usable_content(t) && !s.copies_reply(t, &before));
                // first a request of its own: the code and the user's words alone (no conversation to get lost in)
                let mut src = self.fix_words(&sym, &file, &now, &body).map(|t| code_unnumbered(&t, &before))
                    .filter(|t| usable_content(t) && !same_code(t, &before) && tools::edit_would_parse("edit_symbol", &json!({"path": file, "name": sym, "source": t.trim_end(), "action": "replace"}), &self.tctx).is_ok());
                if src.is_none() { src = words(self, &ask); }
                // the code as it already is: asked once more, that it still has the fault the user describes
                if src.as_deref().is_some_and(|t| same_code(t, &before)) {
                    self.debug(&format!("[the new source of {sym} is the code as it is; asked once more]"));
                    src = words(self, &format!("\n\n(That is the same as the code now, which still has the fault described above. Write only the corrected source of `{sym}` in {file}: the whole symbol with the fault fixed, nothing before or after it.)"))
                        .filter(|t| !same_code(t, &before));
                }
                // (no usable source: the whole file below)
                if src.is_none() { self.debug(&format!("[no usable new source of {sym}; asking for the whole of {file}]")); }
                src.map(|src| ("edit_symbol".to_string(), json!({"path": file, "name": sym, "source": src.trim_end(), "action": "replace"})))
            }
            _ => None,
        };
        let mut call = match by_symbol {
            Some(c) => c,
            None if can("write", self) => {
                let before = std::fs::read_to_string(self.cwd.join(&file)).unwrap_or_default();
                let Some(content) = self.own_words(&format!("\n\n(Write only the full corrected content of {file}, nothing before or after it: it replaces the file as you write it.)"), "content", false)
                    .filter(|c| !self.copies_reply(c, &before)) else {
                    self.debug(&format!("[a change asked in {file}, but no usable new content came; nothing written]"));
                    return Some(self.unfixed(&file));
                };
                // a whole-file rewrite that keeps too little of the file is not the asked change (a small model wrote
                // "state:" over a two-line list): no write
                let content = code_unnumbered(&content, &before);
                // (nor one with no new line: the file as it was, its fault kept)
                if same_code(&content, &before) {
                    self.debug(&format!("[the rewrite of {file} changes nothing; not written]"));
                    return Some(self.unfixed(&file));
                }
                if !keeps_most(&before, &content) || !usable_content(&content) {
                    self.debug(&format!("[the rewrite of {file} kept too little of it; not written]"));
                    return Some(self.unfixed(&file));
                }
                ("write".to_string(), json!({"path": file, "content": content}))
            }
            _ => return None,
        };
        // new code that would not parse (or change nothing) is asked for once more with the reason, before any
        // approval; still so: the file stays as it is, said plainly
        if let Err(why) = tools::edit_would_parse(&call.0, &call.1, &self.tctx) {
            let why = why.lines().next().unwrap_or("").to_string();
            self.debug(&format!("[the new code for {file} would not stand ({why}); asked once more]"));
            let (arg, what) = if call.0 == "edit_symbol" { ("source", format!("the corrected source of `{}` in {file}: the whole symbol", call.1["name"].as_str().unwrap_or(""))) }
                else { ("content", format!("the full corrected content of {file}")) };
            let before = std::fs::read_to_string(self.cwd.join(&file)).unwrap_or_default();
            let again = self.own_words(&format!("\n\n(That code was refused: {why} Write only {what}, as valid code, nothing before or after it.)"), arg, false)
                .map(|t| code_unnumbered(&t, &before)).filter(|t| usable_content(t) && !self.copies_reply(t, &before) && !same_code(t, &before));
            let Some(t) = again else { return Some(self.unfixed(&file)) };
            call.1[arg] = json!(if arg == "source" { t.trim_end().to_string() } else { t });
            if tools::edit_would_parse(&call.0, &call.1, &self.tctx).is_err() { return Some(self.unfixed(&file)); }
        }
        // the reply that made no change is taken back; the model answers from the edit's result instead
        if self.history.last().is_some_and(|m| m.role == Role::Assistant) { self.history.pop(); self.fe.reply_retracted(); }
        self.real = None;
        self.debug(&format!("[a change asked in {file} and no edit made: Elisa makes it with {}]", call.0));
        if self.tiny() { if self.run_own_inline(vec![call.clone()]).is_err() { return Some(self.unfixed(&file)); } } else { self.own_step(vec![call.clone()]); }
        // the change went through: Elisa says what she changed, the new code as it now reads (no model to word it
        // wrong, "add now subtracts"); one that did not: the model answers from the result
        let done = self.tool_lines.last().is_some_and(|l| !l.contains(" → "));
        if !done { return Some(if self.said_no_this_turn() { self.agent_loop(CheckRun::default(), false) } else { self.unfixed(&file) }); }
        let vi = crate::route::vietnamese(&body);
        let (what, code) = match call.0.as_str() {
            "edit_symbol" => (format!("{} {} {file}", call.1["name"].as_str().unwrap_or(""), if vi { "trong" } else { "in" }), call.1["source"].as_str().unwrap_or("").to_string()),
            _ => (file.clone(), call.1["content"].as_str().unwrap_or("").to_string()),
        };
        let code = code.trim_end();
        let shown = code.lines().count() <= FIXED_SHOWN_LINES && !code.is_empty();
        let line = match (shown, vi) {
            (true, false) => format!("Fixed {what}. It now reads:\n\n```\n{code}\n```"),
            (true, true) => format!("Đã sửa {what}. Giờ nó là:\n\n```\n{code}\n```"),
            (false, false) => format!("Fixed {what}."),
            (false, true) => format!("Đã sửa {what}."),
        };
        if self.interactive { self.fe.text_delta(&line); }
        self.history.push(Message::assistant_text(line));
        Some(TurnEnd::Done)
    }

    /// The end of a change ask Elisa could not make (no usable new code came for it): said plainly in place of the
    /// reply, the file left as it was.
    fn unfixed(&mut self, file: &str) -> TurnEnd {
        // (on a tiny window: that this model could not give it)
        let line = match (crate::route::vietnamese(context::request_body(&self.input)), self.tiny()) {
            (true, false) => format!("Mình chưa sửa được {file}, nên tệp vẫn giữ nguyên."),
            (true, true) => format!("Mình không sửa được {file} với {}, nên tệp vẫn giữ nguyên.", self.cfg.model),
            (false, false) => format!("I couldn't work out the change for {file}, so it is unchanged."),
            (false, true) => format!("I couldn't make that change in {file} with {}, so it is unchanged.", self.cfg.model),
        };
        if self.history.last().is_some_and(|m| m.role == Role::Assistant) { self.history.pop(); self.fe.reply_retracted(); }
        if self.interactive { self.fe.text_delta(&line); }
        self.history.push(Message::assistant_text(line));
        TurnEnd::Done
    }

    /// Elisa's own note words copied into the last reply (the claim check's "(unverified: …)" marker) are taken out.
    fn strip_internal_marks(&mut self) {
        let Some(m) = self.history.last_mut().filter(|m| m.role == Role::Assistant) else { return };
        let mark = context::UNVERIFIED;
        let inner = mark.trim_start_matches('(').trim_end_matches(')');
        let mut hit = false;
        for b in m.content.iter_mut() {
            if let Block::Text { text } = b {
                if text.contains(inner) {
                    *text = crate::reply_check::without_marked(text, mark);
                    hit = true;
                }
            }
        }
        if hit {
            let t = texts(m);
            self.fe.reply_retracted();
            if self.interactive && !t.trim().is_empty() { self.fe.text_delta(&t); }
            self.debug("[the reply repeated one of Elisa's own notes; that part was removed]");
        }
    }

    /// Tools ran this turn and the turn ends with no answer from the model (no text, only a state card, a loop
    /// stop, or just the call it made repeated back): Elisa answers herself. The question and the results alone go in
    /// one short request with no tools and no history (`words_from_results`: "todo.txt has 2 lines."); when that gives
    /// nothing usable, the results in plain words (`plain_results`). Identical results are given once; Elisa's own
    /// notes never. A stopped turn then ends as answered.
    fn answer_from_tools(&mut self, end: &mut TurnEnd) {
        if !matches!(end, TurnEnd::Done | TurnEnd::Stopped) || self.unsent { return; }
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(String, String, serde_json::Value)> = Vec::new();
        let mut args: Vec<String> = Vec::new();
        for m in &self.history[cap..] {
            for b in &m.content {
                if let Block::ToolUse { id, name, input } = b {
                    calls.push((id.clone(), name.clone(), input.clone()));
                    if let Some(o) = input.as_object() { args.extend(o.values().filter_map(|v| v.as_str()).map(|v| v.trim().to_string())); }
                }
            }
        }
        if calls.is_empty() { return; }
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        let said = texts(last);
        // (a one-line reply in code marks, "`glob *`", without them; a reply that opens with a code span and ends
        // with a code fence, "`notes.txt` has:\n```…```", is left whole)
        let bare = said.trim();
        let bare = match bare.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
            Some(inner) if !inner.contains('`') && !inner.contains('\n') => inner.trim(),
            _ => bare,
        };
        let stopped = bare.starts_with("[stopped") || bare == "[step limit reached]";
        // (a number Elisa worked out herself, a count she wrote, is the answer, not the call's argument repeated back)
        let own = crate::reply_check::numbers(bare).len() == 1 && !crate::reply_check::bare_number_unbacked(bare, "", &self.own_numbers(&self.input.clone(), &self.turn_tool_text().0));
        let none = bare.is_empty() || bare == crate::route::NO_ANSWER || stopped || (!own && args.iter().any(|a| !a.is_empty() && a == bare))
            || calls.iter().any(|(_, n, i)| tools::summary(n, i) == bare);
        let body = context::request_body(&self.input);
        let ask = context::own_words(body).unwrap_or(body).trim().to_string();
        if !none {
            // the turn's answer, from whatever stage, through the one gate once more at the turn's end (`gate.rs`
            // `gated`: the language among its checks; an answer still in another language is said again in the user's,
            // else Elisa answers from the results in plain words); her own line passes as it is
            let shown = self.gated(bare.to_string(), &ask, true, true);
            if shown.trim() != bare {
                if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: shown }]; }
                *end = TurnEnd::Done;
            }
            return;
        }
        let res = self.turn_results();
        let (line, own) = self.answer_from_results(&res);
        // held to Elisa's own exact facts too (a count, a computed result: `situation::ReplyFacts`)
        let line = self.reply_facts().fix(&line);
        // (a read file's instruction aimed at an assistant is said plainly here too)
        let line = self.with_planted(&line, &self.input.clone());
        // (her own plain words are hers; the model's wording of the results is not, and passes the gate: a count in
        // the user's language, a claim no call made, the held file's own words, the language, a dangling lead-in)
        let line = if own { self.said_own(line) } else { self.said_worded(line) };
        let line = self.gated(line, &ask, false, false);
        // what the model showed (a repeated call, a path) is taken back first
        if !bare.is_empty() { self.fe.reply_retracted(); }
        if self.interactive { self.fe.text_delta(&line); }
        if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: line }]; }
        self.debug("[no answer after the tools ran; Elisa answered from their results]");
        *end = TurnEnd::Done;
    }

    /// `answer` said again in the language of `ask` by the user's own model, the same meaning and nothing added: one
    /// request with no history and no tools. None when it fails, stays in another language or is garbled. What it
    /// says goes through the content checks too (`content_fixed`): a day the held file does not give, "thứ Bảy hôm
    /// nay" for "Friday", is put right from the file as in any other reply.
    pub(super) fn same_answer_in(&mut self, ask: &str, answer: &str) -> Option<String> {
        if CANCEL.load(Ordering::SeqCst) { return None; }
        let q = format!("Question: {ask}\n\nAnswer:\n{answer}\n\nSay this answer again in the language of the question, with the same meaning: add nothing, leave nothing out, keep code as it is.\n{}", crate::reply_check::lang_note(ask));
        let msgs = [Message::user_text(q)];
        let no_tools: [ToolDef; 0] = [];
        let req = Request { system: SAME_ANSWER_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: OWN_ANSWER_TOKENS };
        self.fe.waiting();
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
        let t = texts(&Message::new(Role::Assistant, r.content));
        let t = crate::route::strip_stop_markers(context::split_state(&t).0.trim()).trim().to_string();
        (!t.is_empty() && usable_content(&t) && !crate::reply_check::lang_mismatch(ask, &t) && !crate::reply_check::garbled(&t, ask)
            // (a verdict the answer did not give is another claim)
            && !(crate::reply_check::unbacked_verdict(&t, "") && !crate::reply_check::unbacked_verdict(answer, ""))).then_some(())?;
        Some(match self.content_fixed(&t, &self.input.clone()) { Some((f, why, _)) => { self.debug(why); f } None => t })
    }

    /// This turn's tool results for Elisa's own answer: each with its call, without fence lines or Elisa's notes,
    /// identical ones once, calls that did not run left out.
    pub(super) fn turn_results(&self) -> Vec<OwnResult> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(&str, &str, &serde_json::Value)> = Vec::new();
        // (Elisa's own calls' results first, whole: a fold or a cut mid-turn (a tiny window) takes them out of the
        // history or shortens them there; the history's copy of the same call is then the same result)
        let mut res: Vec<OwnResult> = self.own_results.clone();
        for m in &self.history[cap..] {
            for b in &m.content {
                match b {
                    Block::ToolUse { id, name, input } => calls.push((id, name, input)),
                    Block::ToolResult { tool_use_id, content, is_error } if !content.starts_with("[elisa: not run") => {
                        let Some((_, name, input)) = calls.iter().find(|c| c.0 == tool_use_id) else { continue };
                        let r = own_result_of(name, input, content, *is_error);
                        if res.iter().any(|x| x.name == r.name && x.input == r.input && (x.body == r.body || tool_use_id.starts_with("elisa_"))) { continue; }
                        res.push(r);
                    }
                    _ => {}
                }
            }
        }
        res
    }

    /// One of Elisa's own calls' results kept whole for this turn (`own_results`; `own_step`, `run_own_inline`).
    fn keep_own_result(&mut self, name: &str, input: &serde_json::Value, content: &str, is_error: bool) {
        let r = own_result_of(name, input, content, is_error);
        if !self.own_results.iter().any(|x| x.name == r.name && x.input == r.input) { self.own_results.push(r); }
    }

    /// Elisa's own answer to this turn's question from its tool results, for a reply that ignored them
    /// (`checked_reply`): worded from the question and the results alone (`words_from_results`), else in plain words
    /// (`plain_results`: never page text from the web). None when no tool returned anything.
    /// Her own lines (a held line, the results in plain words) are marked hers (`said_own`); the model's wording of
    /// the results is marked the model's (`said_worded`), so the gate's checks apply to it wherever it is shown.
    pub(super) fn own_answer_from_turn(&mut self) -> Option<String> {
        let res = self.turn_results();
        // no tool ran this turn: a short follow-up on a file read before is answered from it ("and South?" after
        // sales.csv: `held_line`), else nothing
        if res.is_empty() { return self.held_line(context::request_body(&self.input), false).map(|t| self.said_own(t)); }
        // "trong todo.txt, việc đầu tiên là gì?" after her own read: that item (`held_item_line`)
        if let Some(line) = self.held_item_line() { return Some(self.said_own(line)); }
        let (t, own) = self.answer_from_results(&res);
        Some(if own { self.said_own(t) } else { self.said_worded(t) })
    }

    /// Elisa's answer from this turn's results: worded from the question and the results (`words_from_results`), else
    /// in plain words (`plain_results`). Words that are only a piece of a web results list ("1. Iceland Population -
    /// Worldometer") are no answer; nor, from the web, words that leave out what the results agree on ("The city is
    /// located…" where they all say Canberra: `reply_check::misses_shared_name`), nor the question given back (a
    /// result's title "Dân số Iceland hiện tại là bao nhiêu?" to that very ask: `question_back`). For an ask that
    /// wants a figure, the results' sentence that holds one comes before words without one (`figure_sentence`);
    /// without a wording, the results' sentence nearest in meaning to the ask (`meaning_sentence`) comes before the
    /// results in plain words.
    /// (the answer, whether it is Elisa's own: a results sentence or her plain words, not the model's wording)
    fn answer_from_results(&mut self, res: &[OwnResult]) -> (String, bool) {
        let web: String = res.iter().filter(|r| crate::route::tool_group(&r.name, self.mcp.server_of(&r.name)) == crate::route::WEB).map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        let ask = context::request_body(&self.input).to_string();
        // (commands with a short output: what they printed, plainly, never the model's words about it, "Kết quả là
        // thông điệp đó được hiển thị" for `echo xin-chao-elisa`)
        if short_run_output(res) { return (self.plain_results(res), true); }
        let figure = (!web.is_empty()).then(|| figure_sentence(&ask, &web)).flatten();
        let worded = self.words_from_results(res).filter(|t| !crate::reply_check::list_piece(t))
            .filter(|t| web.is_empty() || (!crate::reply_check::misses_shared_name(t, &ask, &web) && !question_back(t, &ask)))
            .filter(|t| figure.is_none() || has_figure(t));
        match worded {
            Some(t) => (t, false),
            None => (figure.or_else(|| (!web.is_empty()).then(|| meaning_sentence(&ask, &web)).flatten()).unwrap_or_else(|| self.plain_results(res)), true),
        }
    }

    /// What tells the calls Elisa ran this turn: each tool's name and its plain text arguments (a URL, a path, a
    /// command), for `reply_check::tells_how`; with what they returned.
    pub(super) fn turn_calls(&self) -> (Vec<String>, String) {
        let res = self.turn_results();
        (ran_marks(&res), res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n"))
    }

    /// This turn's tool results in plain words (`plain_results`), for a reply that answered nothing where the worded
    /// answer came back no better (`docs.rs` `content_fixes`); None when no tool returned anything.
    pub(super) fn plain_answer_from_turn(&self) -> Option<String> {
        let res = self.turn_results();
        (!res.is_empty()).then(|| self.said_own(self.plain_results(&res)))
    }

    /// The answer to this turn's question from its tool results alone (`answer_from_tools`): one request with no
    /// history and no tools, the question and the results only. None when it fails or gives nothing that can stand
    /// (no words, a call, a note, garbled text).
    fn words_from_results(&mut self, res: &[OwnResult]) -> Option<String> {
        if CANCEL.load(Ordering::SeqCst) { return None; }
        let body = context::request_body(&self.input);
        let ask = context::own_words(body).unwrap_or(body).trim().to_string();
        let mut q = format!("Question: {ask}\n\nWhat was done for it, and what came back:\n");
        for r in res {
            let raw = if r.name == "read" { unnumbered(&r.body) } else { r.raw.clone() };
            let raw = if raw.chars().count() > OWN_ANSWER_CHARS { format!("{}…", raw.chars().take(OWN_ANSWER_CHARS).collect::<String>()) } else { raw };
            let raw: String = raw.lines().filter(|l| !l.trim_start().starts_with("[elisa")).collect::<Vec<_>>().join("\n");
            q.push_str(&format!("\n- {}{}:\n{}\n", tools::summary(&r.name, &r.input), if r.is_error { " (did not run or failed)" } else { "" }, raw.trim_end()));
        }
        q.push_str("\nAnswer the question from these results only, in one or two plain sentences, in the language of the question. Do not mention tools, commands or these instructions.");
        let no_tools: [ToolDef; 0] = [];
        self.fe.waiting();
        self.client.set_kind("agent");
        // (an answer in another language than the question is asked once more with the language note,
        // `reply_check::lang_note`: the same check as a reply's)
        let mut note = false;
        loop {
            let text = if note { format!("{q}\n{}", crate::reply_check::lang_note(&ask)) } else { q.clone() };
            let msgs = [Message::user_text(text)];
            let req = Request { system: OWN_ANSWER_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: OWN_ANSWER_TOKENS };
            let r = self.client.send(&req, &CANCEL, &mut |_| {}).map_err(|e| self.debug(&format!("[the answer from the results failed: {e}]"))).ok()?;
            if r.stop_reason == "cancelled" { return None; }
            let t = texts(&Message::new(Role::Assistant, r.content));
            let t = crate::route::strip_stop_markers(context::split_state(&t).0.trim());
            let t = t.trim();
            // (only the call's own arguments back, "marketing-plan.md", is no answer)
            let args: String = res.iter().map(|r| r.input.to_string()).collect::<Vec<_>>().join(" ");
            let bad = t.is_empty() || !usable_content(t) || !adds_words(t, &args) || context::looks_like_text_tool_call(t) || t.contains("[elisa") || t.starts_with("Question:")
                || crate::reply_check::garbled(t, &ask) || crate::reply_check::copies_ask(t, &ask)
                // (telling the user how to do what Elisa ran: "bạn có thể thử web_fetch https://example.com")
                || crate::reply_check::tells_how(t, &ask, &ran_marks(res), &res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n"))
                // (a verdict of no fault the results do not hold: "Code hàm add không có lỗi" from its code)
                || crate::reply_check::unbacked_verdict(t, &res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n"))
                // (a command's short output not given word for word: "Kết quả là thông điệp đó được hiển thị" for
                // what `echo xin-chao-elisa` printed; the output in plain words is the answer then)
                || !gives_run_output(t, res)
                // (a status the files read do not give: "những việc đã gửi đi" for "- send report")
                || reads_only_text(res).is_some_and(|held| super::docs::status_unheld(t, &held));
            if bad { return None; }
            if !crate::reply_check::lang_mismatch(&ask, t) { return Some(t.to_string()); }
            if note { return None; }
            self.debug("[the answer from the results was not in the language of the user's message; asking once more with a note]");
            note = true;
        }
    }

    /// The results in plain words, in Elisa's voice (`answer_from_tools` when no answer could be worded): a short
    /// lead line in the user's language, then each step as the activity rows say it ("Read todo.txt") with what it
    /// returned (a read's lines without their numbers).
    fn plain_results(&self, res: &[OwnResult]) -> String {
        // (an ask for a count of a project file, "todo.txt có mấy dòng? dùng wc": the count in words, from the file,
        // in the ask's language, "todo.txt có 2 dòng.", never a command's raw line "2 6 26 todo.txt": `count_fix`)
        if let Some(c) = self.count_fix("", &self.input) { return c; }
        // (a question whether code read this turn has a mistake, "file app.py có lỗi gì không? xem hàm add": a
        // judgement of it, never its lines given back, `symbol_verdict`)
        if let Some(v) = self.symbol_verdict() { return v; }
        // (a fact of a sheet read this turn that Elisa computed, "không biết trong sales.csv vùng nào bán nhiều nhất
        // nhỉ": that fact in a sentence, never the sheet's rows, `computed_table_line`)
        if res.iter().any(|r| r.name == "read" && !r.is_error && r.input["path"].as_str().is_some_and(|p| crate::mcp_docs::is_sheet(Path::new(p)))) {
            let body = context::request_body(&self.input);
            if let Some(l) = self.table_part(context::own_words(body).unwrap_or(body)).and_then(|p| computed_table_line(&p, body)) { return l; }
        }
        // (a line asked of a file only read this turn, "đọc file notes.txt và cho mình biết mã cuộc họp": that line,
        // as `held_file_answer` picks it, never every line of the file; an ask for its content, the file told whole;
        // not an ask about a function or other symbol of a code file, whose lines are no labelled values)
        if !res.is_empty() && res.iter().all(|r| r.name == "read" && !r.is_error) && self.symbol_plain(context::request_body(&self.input)).is_none() {
            if let Some(h) = self.held_file_answer("", &self.input) { return h; }
        }
        let lead = match crate::reply_lang::detect(&self.input) {
            Some(crate::reply_lang::Lang::Vi) => "Đây là kết quả:",
            _ => "Here's what I found:",
        };
        // (changes alone need no lead: their rows say it all)
        let changes = res.iter().all(|r| !r.is_error && tools::access(&r.name) == Access::Write);
        // page text from the web is never the answer: what came back there is told in words or not at all
        let web = |r: &OwnResult| crate::route::tool_group(&r.name, self.mcp.server_of(&r.name)) == crate::route::WEB;
        // from the web: the sentence that answers the ask, or for news (or no such sentence) the results' titles
        if res.iter().all(web) {
            let all: String = res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
            let recent = res.iter().any(|r| r.input["recent"] == true);
            let ask = context::request_body(&self.input);
            // (a sentence of the pages, never a title line of the list, nor the question given back)
            let text: String = all.lines().filter(|l| web_titles(l).is_empty()).collect::<Vec<_>>().join("\n");
            if let Some(s) = (!recent).then(|| crate::reply_check::best_sentence(ask, &text)).flatten().filter(|s| !question_back(s, ask)) { return s; }
            // (a title that is itself a question, "Thủ đô của Úc là gì? Sydney, Canberra hay Melbourne?", answers nothing)
            let titles: Vec<String> = web_titles(&all).into_iter().filter(|t| !t.contains('?')).collect();
            if titles.is_empty() { return crate::reply_check::no_answer_line(&self.input).to_string(); }
            let vi = crate::route::vietnamese(ask);
            return format!("{}\n{}", if vi { "Đây là các kết quả hàng đầu:" } else { "Here are the top results:" },
                titles.iter().take(WEB_TITLES_SHOWN).map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n"));
        }
        // a dependency check in plain words alone ("flask 0.12 (8, worst high): update to 3.1.3 or later")
        let summaries: Vec<String> = res.iter().filter_map(|r| (r.name == "sec_deps" && !r.is_error).then(|| deps_summary(&r.body, crate::route::vietnamese(&self.input))).flatten()).collect();
        if !summaries.is_empty() && res.iter().all(|r| r.name == "sec_deps" || r.name == "read") { return summaries.join("\n"); }
        let mut out = if changes { Vec::new() } else { vec![lead.to_string()] };
        // (the lines of each file told so far: a read of a file whose lines an earlier step of the turn already told,
        // "Đã đọc app.py: …" then its function "app.py:1-2 def add …", is not told again)
        let mut told: Vec<(String, HashSet<String>)> = Vec::new();
        for r in res {
            if !r.is_error && (r.name == "read" || r.name == "read_symbol") {
                if let Some(p) = r.input["path"].as_str() {
                    let base = Path::new(p).file_name().map_or(p.to_string(), |n| n.to_string_lossy().to_string());
                    // (a symbol's first line is its place, "app.py:1-2 def add"; a "[…]" line and the untrusted wrapper are Elisa's marks)
                    let lines: HashSet<String> = r.body.lines().map(|l| l.trim_start().split_once('\t').filter(|(n, _)| !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit())).map_or(l, |(_, t)| t).trim())
                        .filter(|l| !l.is_empty() && !l.starts_with('[') && !l.starts_with("<untrusted") && !l.starts_with("</untrusted") && !l.starts_with(&format!("{base}:")) && !l.starts_with(&format!("{p}:")))
                        .map(str::to_string).collect();
                    if !lines.is_empty() && told.iter().any(|(f, t)| *f == base && lines.is_subset(t)) { continue; }
                    match told.iter_mut().find(|(f, _)| *f == base) { Some((_, t)) => t.extend(lines), None => told.push((base, lines)) }
                }
            }
            let phrase = crate::activity::describe(&r.name, &r.input, &self.cwd, self.mcp.server_of(&r.name), self.mcp.remote_of(&r.name));
            // (the step's line in the user's language, `data/step_lines.txt`; the activity row's words when it has none)
            // (a command's step names the command it ran: "Đã chạy echo hello-elisa", never the English row)
            let target = if phrase.target.is_empty() { r.input["command"].as_str().map(|c| context::one_line(c, 80)).unwrap_or_default() } else { phrase.target.clone() };
            let head = crate::reply_check::step_line(&phrase.group, &target, &phrase.act, &r.raw, r.is_error, crate::route::vietnamese(&self.input))
                .unwrap_or_else(|| crate::activity::done_text(&phrase, &r.raw, r.is_error));
            let body = if r.name == "read" {
                r.body.lines().map(|l| l.trim_start().split_once('\t').filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map_or(l, |(_, t)| t)).collect::<Vec<_>>().join("\n")
            // (a command that ended well needs no "[exit 0]" mark: only a failing one's code is news)
            } else { r.body.lines().filter(|l| l.trim() != "[exit 0]").collect::<Vec<_>>().join("\n") };
            let body = if body.chars().count() > OWN_ANSWER_CHARS { format!("{}…", body.chars().take(OWN_ANSWER_CHARS).collect::<String>()) } else { body };
            // (a change that went through says it all in its row: "Edited todo.txt")
            let done = r.raw.starts_with("denied") || r.raw.starts_with("cancelled") || (!r.is_error && tools::access(&r.name) == Access::Write) || web(r);
            out.push(if done || body.is_empty() { format!("{head}.") } else { format!("{head}:\n{body}") });
        }
        out.join("\n\n")
    }

    /// A reply on a turn where no tool ran that writes one of the session's tools and its argument as text, at the
    /// start of a line ("`read` /etc/hosts ==> 127.0.0.1 …", "`write` /tmp/x test", "`bash` echo hi"): the model
    /// tried to call it in words, and what follows is made up. The reply is taken back, the call runs as Elisa's own
    /// (`own_step`, the same approval), and the model answers from its result. A name without backticks counts only
    /// before a path or URL that fills the call whole (prose like "read the notes first" is no call).
    fn text_named_call(&mut self) -> Option<TurnEnd> {
        if !self.tool_lines.is_empty() || self.small_talk_turn { return None; }
        let last = self.history.last().filter(|m| m.role == Role::Assistant)?;
        let reply = texts(last);
        let mut defs = self.tools.clone();
        defs.extend(tools::defs_for(self.cfg.ast).into_iter().filter(|d| !self.tools.iter().any(|t| t.name == d.name)));
        // (or, on an ask for the shell, a reply that is only a command: "nc -z 1.1.1.1 80", `text_command`)
        let body = context::request_body(&self.input).to_string();
        let (name, args) = reply.lines().find_map(|l| named_call(l, &defs))
            .or_else(|| (self.own_rules() && shell_ask(&body)).then(|| text_command(&reply)).flatten().map(|c| ("bash".to_string(), json!({"command": c}))))?;
        if !self.tools.iter().any(|t| t.name == name) && !self.widen_for(&name) { return None; }
        self.history.pop();
        self.real = None;
        self.fe.reply_retracted();
        self.debug(&format!("[the reply wrote `{name}` and its argument as text, with no call; Elisa runs it]"));
        self.own_step(vec![(name, args)]);
        Some(self.agent_loop(CheckRun::default(), false))
    }

    /// A reply that says it changed a file ("fixed", "changed", "updated" …: the change words the notes' claim
    /// check reads, `context::verify_claims`) on a turn about a file of the project where no edit, write or command
    /// ran: asked once more in words with a note that nothing was changed; still claimed, a plain line (as
    /// `denied_claim_retry` does for a saved file). A change the session's records hold is a true recall.
    fn edit_claim_retry(&mut self) -> Option<TurnEnd> {
        // (a write or edit that was refused or failed changed nothing; a command may have)
        let changed = |l: &String| ["[bash ", "[run_code "].iter().any(|p| l.starts_with(p))
            || (["[write ", "[edit ", "[edit_symbol "].iter().any(|p| l.starts_with(p)) && !l.contains(" → "));
        if self.tool_lines.iter().any(changed) { return None; }
        let body = context::request_body(&self.input).to_string();
        if !path_tokens(&body).iter().any(|p| self.cwd.join(p).is_file()) { return None; }
        // only a report of a change as done ("I updated it", "app.py is now fixed"), never advice (`reply_check::is_done_claim`)
        let files = path_tokens(&body);
        // (what the named files hold: a passive claim about a thing in them, "the requests version has been fixed")
        let held: String = files.iter().filter_map(|p| std::fs::read_to_string(self.cwd.join(p)).ok()).filter(|t| t.len() <= HELD_CHECK_MAX_BYTES).collect::<Vec<_>>().join("\n");
        let found = |h: &[Message], ledger: &context::Ledger| h.last().filter(|m| m.role == Role::Assistant).map(|m| change_claims(&texts(m)).into_iter()
            .filter(|s| crate::reply_check::is_done_claim_in(s, &files, &body, &held) && !ledger.backs(s, crate::route::file_name(s).as_deref())).collect::<Vec<_>>()).unwrap_or_default();
        let claims = |h: &[Message], ledger: &context::Ledger| !found(h, ledger).is_empty();
        // (on an ask that changes nothing, "check requirements.txt…", only a claim about the named file itself counts:
        // "the library release has fixed it" is no claim that the user's file was changed; a claim comes split at its
        // dots, "I updated requirements", so the file's name without its ending is what it names)
        let stem = |f: &String| { let b = f.rsplit('/').next().unwrap_or(f); b.split('.').next().unwrap_or(b).to_lowercase() };
        let first: Vec<String> = found(&self.history, &self.ledger).into_iter()
            .filter(|s| asks_change(&body) || files.iter().any(|f| word_set(s).contains(&stem(f)))).collect();
        if first.is_empty() { return None; }
        // an answer with more in it than the claim (findings, advice): kept, the claim taken out and the correction added
        if let Some(t) = self.history.last().map(texts).and_then(|r| crate::reply_check::claim_corrected(&r, &first, &self.input)) {
            self.fe.reply_retracted();
            if self.interactive { self.fe.text_delta(&t); }
            if let Some(m) = self.history.last_mut() { m.content = vec![Block::Text { text: t }]; }
            self.debug("[the reply said a file was changed, but no edit was made; that sentence was replaced by a correction]");
            return None;
        }
        self.history.pop();
        self.fe.reply_retracted();
        self.debug("[retry: the reply said a file was changed, but no edit was made; asked again in words]");
        self.retry_note = Some("(For this reply: no edit was made in this message, so no file was changed. Say so plainly in one or two \
            sentences; never say it was changed or fixed.)".to_string());
        let end = self.agent_loop(CheckRun::default(), false);
        self.retry_note = None;
        if !matches!(end, TurnEnd::Done) || !claims(&self.history, &self.ledger) { return Some(end); }
        self.history.pop();
        self.fe.reply_retracted();
        let line = crate::reply_check::not_changed_line(&self.input);
        if self.interactive { self.fe.text_delta(line); }
        self.history.push(Message::assistant_text(line));
        self.debug("[retry: the reply still said the file was changed; showed a plain line]");
        Some(end)
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
            None => crate::route::resume_intent(input).filter(|_| self.own_rules()),
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
            // kept as the first message it is (`turn_inner`): the pinned facts and notes go with it, or no later
            // request of this session would carry them
            let (notes, state) = (self.first_notes(), self.state_block());
            let first = if self.tiny() { context::first_message_bare(notes.as_deref(), state.as_deref(), &[], input) }
                else { context::first_message_full(&self.cwd, notes.as_deref(), state.as_deref(), &[], input) };
            self.history.push(Message::user_text(first));
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
        // one plain line: the chat's first ask (never Elisa's fold placeholder) and how long ago
        let title = crate::secrets::redact(info.title.trim()).0;
        let title = Some(title.trim()).filter(|t| !t.is_empty() && !crate::context::fold_placeholder(t)).map(|t| crate::context::one_line(t, 60));
        let vi = crate::reply_lang::detect(input) == Some(crate::reply_lang::Lang::Vi);
        let line = match (vi, title) {
            (true, Some(t)) => format!("Đã mở lại cuộc trò chuyện trước: \"{t}\"."),
            (true, None) => "Đã mở lại cuộc trò chuyện trước.".to_string(),
            (false, t) => {
                let t = t.map_or(String::new(), |t| format!(" (\"{t}\")"));
                let age = at.map_or(String::new(), |a| format!(" from {} ago", crate::situation::ago(crate::situation::now_secs() - a)));
                format!("Picked up your last chat{t}{age}.")
            }
        };
        self.fe.info(&line);
        self.show_history();
        Ok(Some(kind))
    }

    /// A text reply cut by the output limit, continued: `sent` (the request that got it) plus the reply so far as an
    /// assistant message and CONTINUE_NOTE (in that request only), streamed into the same reply, up to CONTINUE_MAX
    /// times while it is still cut. Returns the whole text and the last stop reason.
    /// `st`: the status of the cut reply; the reply ends (`reply_done`) with the last part's.
    pub(super) fn continue_reply(&mut self, sent: Vec<Message>, mut text: String, max_tokens: usize, in_words: bool, mut st: crate::frontend::Status) -> (String, String) {
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
            // the reply's status counts all of its parts' output, not the last part's alone
            let mut part = ui::status(&r, pc + r.usage.output_tokens, self.max_ctx);
            part.estimated = estimated;
            st = ui::joined(&st, &part);
            let more = texts(&Message::new(Role::Assistant, r.content.into_iter().filter(|b| matches!(b, Block::Text { .. })).collect()));
            // the note Elisa sent, echoed back, is no part of the reply
            let kept = strip_continue_note(&more);
            stop = r.stop_reason;
            text.push_str(&kept);
            // (it was shown as it streamed: the reply is shown again without it)
            if kept != more && self.interactive { self.fe.reply_retracted(); self.fe.text_delta(&text); }
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
        // (the last turn's message, when it was a greeting or other small talk, had no answer to go on from: Elisa's own
        // greeting line in its reply is no answer either)
        let last_ask = self.history.iter().rev().filter(|m| is_request(m)).nth(usize::from(self.history.last().is_some_and(is_request))).map(|m| context::request_body(&texts(m)).to_string());
        let greeted = last_ask.is_some_and(|a| crate::route::small_talk(&a));
        let answered = !greeted && last.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty() && !text.trim_start().starts_with('[') && !context::is_intro(text)));
        // a bare "yes" to an answer that offered several options: ask which one (the model picked the first and wrote code)
        // (both word checks are Elisa's own rules: with them off, only the router's follow-up labels)
        let rules = self.own_rules();
        if answered && rules { if let Some(h) = crate::followup::pick_option(input, &texts(last)) { return Some(h.to_string()); } }
        let k = self.router_kind().unwrap_or_else(|| crate::followup::kind(input).filter(|_| rules)).filter(|_| answered)?;
        let chat = self.route.groups.tool_free() && self.route.others.is_empty();
        // about an attached document: the hint keeps to what it says (otherwise "go deeper" can get content that is not in it)
        let doc = !self.kept_docs.is_empty() || self.doc_req.is_some() || self.route.groups.has(crate::route::DOCS)
            || self.history.iter().any(|m| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if text.contains(crate::attach::MARK))));
        if k == crate::followup::Kind::More && !chat && !doc { return None; }
        Some(crate::followup::line_for(&k, doc))
    }

    /// The follow-up kind from the router's answer (Some(None): it saw no follow-up); None = no router answer.
    pub(super) fn router_kind(&self) -> Option<Option<crate::followup::Kind>> {
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
        if !self.cfg.greet_context || !self.route.groups.tool_free() || !self.route.others.is_empty() || !self.says_or("greeting", || crate::route::greeting(input)) { return None; }
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
            // last few user turns only (an old one is stale)
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
}
/// One call Elisa makes herself on a clear ask (`Session::own_asks`).
enum Own {
    Call(String, serde_json::Value),
    /// a new file to save to: its content comes from the model in words (`Session::own_content`)
    Save(String),
    /// a tool the ask means that this session cannot offer (its server is off, or the model's window takes no tools)
    Unoffered(String),
}

/// the most characters of one tool result Elisa gives as the answer herself (`Session::answer_from_tools`)
const OWN_ANSWER_CHARS: usize = 1500;
/// the system prompt and output cap of the request that words an answer from the results alone (`words_from_results`)
const OWN_ANSWER_SYSTEM: &str = "You answer a question from the results given with it, briefly and plainly.";
const OWN_ANSWER_TOKENS: usize = 200;
/// the system line of Elisa's ask to say an answer again in the user's language (`same_answer_in`)
const SAME_ANSWER_SYSTEM: &str = "You say a given answer again in another language, keeping its meaning exactly.";

/// One tool result of the turn, for Elisa's own answer (`Session::answer_from_tools`).
#[derive(Clone)]
pub(super) struct OwnResult { pub(super) name: String, pub(super) input: serde_json::Value, pub(super) raw: String, pub(super) body: String, pub(super) is_error: bool }

/// The text of this turn's results when every one is a read that went through (their bodies, joined); None when
/// there are none or another tool ran.
pub(super) fn reads_only_text(res: &[OwnResult]) -> Option<String> {
    (!res.is_empty() && res.iter().all(|r| r.name == "read" && !r.is_error)).then(|| res.iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n"))
}

/// A tool result as this turn's results hold it (`turn_results`): the content as returned, and its body without the
/// `<untrusted>` fences and Elisa's `[elisa …]` notes.
fn own_result_of(name: &str, input: &serde_json::Value, content: &str, is_error: bool) -> OwnResult {
    let body: String = content.lines().filter(|l| !l.starts_with("<untrusted") && !l.starts_with("</untrusted") && !l.trim_start().starts_with("[elisa"))
        .collect::<Vec<_>>().join("\n").trim().to_string();
    OwnResult { name: name.to_string(), input: input.clone(), raw: content.to_string(), body, is_error }
}

/// lines of a command's output at most for an answer from it to have to give them word for word (`gives_run_output`)
const RUN_OUTPUT_QUOTE_LINES: usize = 5;

/// Whether `answer` gives what each command of `res` printed, word for word, where that is short (a command that
/// ran without error and printed RUN_OUTPUT_QUOTE_LINES lines or fewer, its "[exit 0]" mark aside): every output line
/// is in it. True when no such command ran.
pub(super) fn gives_run_output(answer: &str, res: &[OwnResult]) -> bool {
    let low = answer.to_lowercase();
    res.iter().filter(|r| !r.is_error && r.input["command"].is_string()).all(|r| {
        // (Elisa's own exit mark, "[exit 3]", is hers: a reply may say it in its own words, "(exit 3)")
        let out: Vec<&str> = r.body.lines().map(str::trim).filter(|l| !l.is_empty() && !(l.starts_with("[exit ") && l.ends_with(']'))).collect();
        out.is_empty() || out.len() > RUN_OUTPUT_QUOTE_LINES || out.iter().all(|l| low.contains(&l.to_lowercase()))
    })
}

/// Whether `res` are only commands that ran and printed a short output (RUN_OUTPUT_QUOTE_LINES lines or fewer, or
/// failed): what they printed, in plain words, is the whole answer, and no worded answer can add to it.
pub(super) fn short_run_output(res: &[OwnResult]) -> bool {
    !res.is_empty() && res.iter().all(|r| r.input["command"].is_string()
        && r.body.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "[exit 0]").count() <= RUN_OUTPUT_QUOTE_LINES)
}

/// The output lines of the commands of `res` that ran without error, Elisa's exit mark aside.
fn run_output_lines(res: &[OwnResult]) -> Vec<String> {
    res.iter().filter(|r| !r.is_error && r.input["command"].is_string())
        .flat_map(|r| r.body.lines().map(str::trim).filter(|l| !l.is_empty() && !(l.starts_with("[exit ") && l.ends_with(']'))).map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// The sentences of `t`, each with its end mark: cut after "!", "?", a line break, or a "." before a space or the
/// end ("todo.txt", "3.14" stay whole).
pub(super) fn sentence_pieces(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = t.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let next_space = it.peek().is_none_or(|(_, n)| n.is_whitespace());
        if matches!(c, '!' | '?' | '\n') || (c == '.' && next_space) {
            out.push(&t[start..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
    }
    if start < t.len() { out.push(&t[start..]); }
    out
}

/// Whether a sentence speaks of a command's run or output: a `run_asks` wording ("printed", "command", "in ra",
/// "lệnh") or a run claim of `data/tool_claims.txt` ("the result is", "kết quả là").
fn speaks_of_run(s: &str) -> bool {
    crate::reply_check::about_run(s) || !crate::reply_check::tool_claims(s, &["run", "run_result"], false).is_empty()
}

/// Whether `answer` reports what the commands of `res` printed as their output: each output line is in a sentence
/// that is that line alone ("xin-chao", "hello-elisa!") or that speaks of the run (`speaks_of_run`: "It printed
/// xin-chao.", "The command fails with boom (exit 3)."). A sentence that only names the word ("Bài viết "xin-chao" là
/// một câu chào…") reports nothing.
pub(super) fn reports_run_output(answer: &str, res: &[OwnResult]) -> bool {
    let alone = |s: &str, l: &str| s.trim().trim_matches(|c: char| !c.is_alphanumeric()).eq_ignore_ascii_case(l.trim_matches(|c: char| !c.is_alphanumeric()));
    run_output_lines(res).iter().all(|l| {
        let low = l.to_lowercase();
        sentence_pieces(answer).iter().any(|s| s.to_lowercase().contains(&low) && (alone(s, l) || speaks_of_run(s)))
    })
}

/// `answer` (it reports the commands' output, `reports_run_output`) without its sentences that hold nothing of the
/// run: no output line, no run wording, no content word of `ask` ("It's nice to assist you.", "Have a great day!").
/// None: nothing goes, or nothing would be left.
pub(super) fn run_talk_cut(answer: &str, ask: &str, res: &[OwnResult]) -> Option<String> {
    let out: Vec<String> = run_output_lines(res).iter().map(|l| l.to_lowercase()).collect();
    let asked = crate::topics::words(ask);
    let keep = |s: &str| {
        let low = s.to_lowercase();
        !s.chars().any(char::is_alphanumeric) || out.iter().any(|l| low.contains(l.as_str())) || speaks_of_run(s) || !crate::topics::words(s).is_disjoint(&asked)
    };
    let pieces = sentence_pieces(answer);
    if pieces.iter().all(|s| keep(s)) { return None; }
    let kept: String = pieces.into_iter().filter(|s| keep(s)).collect();
    let kept = kept.trim().to_string();
    kept.chars().any(char::is_alphanumeric).then_some(kept)
}

/// The calls of `res` as words that name them: each tool's name and its text arguments.
fn ran_marks(res: &[OwnResult]) -> Vec<String> {
    res.iter().flat_map(|r| std::iter::once(r.name.clone())
        .chain(r.input.as_object().into_iter().flat_map(|o| o.values().filter_map(|v| v.as_str().map(str::to_string))).collect::<Vec<_>>())).collect()
}

/// An ask to run a snippet in a language the code runner lists ("run this python: print(…)"): its call, from
/// the runner's own tool list (`mcp_run::tools`: its verb, its `language` values) whether it is on or not, the code
/// being the explicit part of the words.
fn code_ask(body: &str) -> Option<(String, serde_json::Value)> {
    let (code, around) = explicit_span(body)?;
    let words = word_set(&around);
    crate::mcp_run::tools().as_array()?.iter().find_map(|t| {
        let verb = word_set(t["description"].as_str()?).into_iter().next()?;
        let langs = t["inputSchema"]["properties"]["language"]["enum"].as_array()?;
        let lang = langs.iter().filter_map(|l| l.as_str()).find(|l| words.iter().any(|w| w == l))?;
        // its verb in the words, else an ask to run it by meaning ("chạy đoạn python này: …", `route::tool_ask_by_meaning`)
        (words.contains(&verb) || crate::route::tool_ask_by_meaning(body) == Some("run"))
            .then(|| (t["name"].as_str().unwrap_or_default().to_string(), json!({"language": lang, "code": code})))
    })
}

/// The words of this machine's dictionary (`/usr/share/dict/words`), lowercase; empty when it has none.
fn dictionary() -> &'static HashSet<String> {
    static D: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    D.get_or_init(|| std::fs::read_to_string("/usr/share/dict/words").map(|t| t.lines().map(|l| l.trim().to_lowercase()).collect()).unwrap_or_default())
}

/// A word of this machine's dictionary, or an inflected form of one ("units" is "unit": `/usr/bin/units` is a
/// program, but in "which region sold the most units in sales.csv?" it is a word). Stems of three letters or more.
fn in_dictionary(w: &str) -> bool {
    let d = dictionary();
    d.contains(w) || ["s", "es", "ed", "ing"].iter().filter_map(|x| w.strip_suffix(x)).any(|stem| stem.chars().count() >= 3 && d.contains(stem))
}

/// A program of that name in a folder of PATH.
fn on_path(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty())
        .any(|d| std::fs::metadata(Path::new(d).join(cmd)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The one word of a search ask that stands out as what to find: the last capitalised word after the first, else
/// the last word that is not in the dictionary (no file names, paths or URLs).
fn search_term(body: &str, named: bool) -> Option<String> {
    let toks: Vec<&str> = body.split_whitespace().skip(1).map(|t| t.trim_matches(|c: char| "?,!;:\"'`()".contains(c)).trim_end_matches('.'))
        .filter(|t| t.len() >= 2 && !t.contains("://") && !t.contains('/') && crate::route::file_name(t).is_none()).collect();
    let cap = toks.iter().rev().find(|t| t.chars().next().is_some_and(char::is_uppercase) && t.chars().all(|c| c.is_alphanumeric() || c == '_'));
    // (a word outside the dictionary only when the tool is named outright: in "I went for a run this morning and now
    // I'm starving", "starving" is the user's own word, no argument for a tool its verb "run" happened to match)
    let odd = || toks.iter().rev().find(|t| named && t.chars().all(|c| c.is_alphanumeric() || c == '_') && !dictionary().is_empty() && !dictionary().contains(&t.to_lowercase()));
    cap.or_else(odd).map(|t| t.to_string())
}

/// Whether a call's command argument (a schema key holding "command") starts with a program of this machine
/// (`on_path`): a word of the user's sentence ("Bob", "starving") is none. True for calls with no such argument.
fn command_args_run(args: &serde_json::Value) -> bool {
    args.as_object().is_none_or(|o| o.iter().all(|(k, v)| !k.contains("command") || v.as_str().and_then(|c| c.split_whitespace().next()).is_some_and(on_path)))
}

/// the most files one clear read ask reads
const OWN_READS_MAX: usize = 3;

/// The explicit part of a request and the words around it: a `backticked` part, the rest after a short lead-in and a
/// colon ("run the command: echo hi"), or a "quoted" part.
fn explicit_span(msg: &str) -> Option<(String, String)> {
    let cut = |a: usize, b: usize, inner: &str| Some((inner.trim().to_string(), format!("{} {}", &msg[..a], &msg[b..])));
    if let Some(a) = msg.find('`').filter(|&a| !msg[a..].starts_with("```")) {
        if let Some(n) = msg[a + 1..].find('`').filter(|&n| n > 0) { return cut(a, a + 1 + n + 1, &msg[a + 1..a + 1 + n]); }
    }
    if let Some(i) = msg.find(": ") {
        let after = msg[i + 2..].trim().trim_end_matches('.');
        if msg[..i].split_whitespace().count() <= 6 && !after.is_empty() { return Some((after.to_string(), msg[..i].to_string())); }
    }
    for (o, c) in [('"', '"'), ('“', '”')] {
        if let Some(a) = msg.find(o) {
            let s = a + o.len_utf8();
            if let Some(n) = msg[s..].find(c).filter(|&n| n > 0) { return cut(a, s + n + c.len_utf8(), &msg[s..s + n]); }
        }
    }
    None
}

/// The lowercase words of `s` (letters, digits, `_`), in order.
/// The word right before "foo" in an example or a message put in an example's terms ("từ" in "lưu từ foo").
fn word_before_foo(s: &str) -> Option<String> {
    let w = word_set(s);
    w.iter().position(|x| x == "foo").filter(|i| *i > 0).map(|i| w[i - 1].clone())
}

fn word_set(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The paths the words name: a file name (`route::file_name`) or anything with a `/` that is not a URL.
fn path_tokens(msg: &str) -> Vec<String> {
    msg.split_whitespace()
        .map(|t| t.trim_matches(|c: char| "\"'`<>()[],;:?!".contains(c)).trim_end_matches('.'))
        .filter(|t| !t.contains("://") && (crate::route::file_name(t).as_deref() == Some(*t) || (t.contains('/') && t.chars().any(char::is_alphanumeric))))
        .map(str::to_string).collect()
}

/// The JSON Web Tokens in a text, by their shape (three base64url parts, the first a JSON object: `eyJ…`).
fn jwt_tokens(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*\.[A-Za-z0-9_-]*").unwrap());
    let mut v: Vec<String> = Vec::new();
    for m in re.find_iter(text) { if !v.iter().any(|x| x == m.as_str()) { v.push(m.as_str().to_string()); } }
    v
}

/// how much nearer in meaning one tool's description must be than the next before a tie of shared words is broken
const MEANING_LEAD: f32 = 0.05;

/// A tool's arguments from the words (`Session::described_call`): None when a required one is not there.
fn fill_args(schema: &serde_json::Value, span: Option<&str>, paths: &[String], url: Option<&str>, exists: impl Fn(&str) -> bool) -> Option<serde_json::Value> {
    let mut args = serde_json::Map::new();
    let mut span = span.filter(|s| !s.is_empty());
    // (a tool with no required argument, `sec_secrets`, is called with none)
    for r in schema["required"].as_array().into_iter().flatten().filter_map(|r| r.as_str()) {
        let prop = &schema["properties"][r];
        let v = match prop["type"].as_str() {
            Some("boolean") => json!(false),
            Some("string") if r.contains("url") => json!(url?),
            Some("string") if r.contains("path") || r == "file" => json!(paths.iter().find(|p| exists(p)).or(paths.first())?),
            Some("string") => json!(span.take()?),
            _ => return None,
        };
        args.insert(r.to_string(), v);
    }
    Some(serde_json::Value::Object(args))
}

/// One line that starts with a tool's name and its argument (`Session::text_named_call`): the call, with its required
/// arguments in order (a path or URL as one word, the last text argument the rest of the line, up to an arrow the
/// model put before its made-up result).
fn named_call(line: &str, defs: &[ToolDef]) -> Option<(String, serde_json::Value)> {
    let t = line.trim().trim_start_matches(['-', '*', '>', ' ']);
    let (name, rest, ticked) = match t.strip_prefix('`') {
        Some(r) => { let (n, rest) = r.split_once('`')?; (n.trim(), rest, true) }
        None => { let (n, rest) = t.split_once(' ')?; (n, rest, false) }
    };
    let def = defs.iter().find(|d| d.name == name)?;
    let rest = rest.trim().trim_start_matches(':').trim();
    let rest = ["==>", "=>", "->", "→"].iter().filter_map(|a| rest.find(a)).min().map_or(rest, |i| rest[..i].trim());
    let mut words: std::collections::VecDeque<&str> = rest.split_whitespace().collect();
    let req: Vec<&str> = def.input_schema["required"].as_array()?.iter().filter_map(|r| r.as_str()).collect();
    let mut args = serde_json::Map::new();
    let mut whole = false;
    for (k, r) in req.iter().enumerate() {
        let prop = &def.input_schema["properties"][*r];
        let v = match prop["type"].as_str() {
            Some("boolean") => json!(false),
            Some("string") if r.contains("url") => { let w = words.pop_front().filter(|w| w.starts_with("http://") || w.starts_with("https://"))?; whole |= k == 0; json!(w) }
            Some("string") if r.contains("path") || *r == "file" => { let w = words.pop_front().filter(|w| path_tokens(w).len() == 1)?; whole |= k == 0; json!(w) }
            Some("string") if k + 1 == req.len() => { let all: Vec<&str> = words.drain(..).collect(); if all.is_empty() { return None; } json!(all.join(" ")) }
            Some("string") => json!(words.pop_front()?),
            _ => return None,
        };
        args.insert(r.to_string(), v);
    }
    (ticked || (whole && words.is_empty())).then(|| (def.name.clone(), serde_json::Value::Object(args)))
}

/// bytes of a named file read to tell what a passive change claim is about (`edit_claim_retry`)
const HELD_CHECK_MAX_BYTES: usize = 64 * 1024;

/// The sentences of `reply` that say something was changed: the notes' own claim check (`context::verify_claims`,
/// its change words and negations) on each sentence with nothing to check it against.
fn change_claims(reply: &str) -> Vec<String> {
    let none = context::Ledger::default();
    reply.split(['.', '!', '?', '\n']).map(|s| s.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'' || *c == '’').collect::<String>())
        .filter(|s| !s.trim().is_empty())
        .filter(|s| context::verify_claims(s.trim(), &none, &|_| None).contains(context::UNVERIFIED))
        .map(|s| s.trim().to_string()).collect()
}

/// Text copied from a read result keeps its line numbers ("1\tMeeting code: …"): without them when every
/// non-empty line has one.
fn unnumbered(c: &str) -> String {
    let num = |l: &str| l.split_once('\t').filter(|(n, _)| !n.trim().is_empty() && n.trim().chars().all(|ch| ch.is_ascii_digit())).map(|(_, t)| t.to_string());
    if c.lines().filter(|l| !l.trim().is_empty()).any(|l| num(l).is_none()) { return c.to_string(); }
    let mut out: String = c.lines().map(|l| num(l).unwrap_or_default()).collect::<Vec<_>>().join("\n");
    if c.ends_with('\n') { out.push('\n'); }
    out
}

/// A call written as JSON in a reply (`{"name": …, "arguments": {…}}`, alone or first in a list, fenced or not):
/// its name and arguments. None when the text is no such JSON.
fn loose_call(raw: &str) -> Option<(String, serde_json::Value)> {
    let t = unfenced(raw.trim()).trim();
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let o = if v.is_array() { v.get(0)?.clone() } else { v };
    let args = o.get("arguments").or_else(|| o.get("parameters")).cloned().filter(|a| a.is_object())?;
    Some((o.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(), args))
}

/// The inside of the one fenced block in a text with words around it; None with no block or several.
fn one_block(t: &str) -> Option<&str> {
    let a = t.find("```")?;
    let rest = &t[a + 3..];
    let b = rest.find("```")?;
    if rest[b + 3..].contains("```") { return None; }
    let inner = &rest[..b];
    let inner = inner.split_once('\n').map_or(inner, |(_, i)| i);
    (!inner.trim().is_empty()).then_some(inner)
}

/// A reply that is only one fenced block: its inside.
fn unfenced(t: &str) -> &str {
    let Some(rest) = t.strip_prefix("```") else { return t };
    let Some(body) = rest.strip_suffix("```") else { return t };
    if body.contains("```") { return t; }
    body.split_once('\n').map_or(t, |(_, inner)| inner)
}

/// Elisa's notes in a request's last message (`Session::sent_notes`): its `[elisa …]` lines and the request notes
/// (paragraphs in parentheses), never what a tool returned, a card or a situation line, which a file may well share
/// words with. Attached documents and picture text are no notes.
pub(super) fn notes_sent(msgs: &[Message]) -> String {
    let Some(m) = msgs.iter().rev().find(|m| m.role == Role::User) else { return String::new() };
    let mut out = String::new();
    for b in &m.content {
        let text = match b {
            Block::Text { text } if !text.starts_with(crate::attach::MARK) && !context::is_picture_block(text) => text.as_str(),
            Block::ToolResult { content, .. } => content.as_str(),
            _ => continue,
        };
        // (a state card, a situation line, a clock line sum up the conversation: a file may share their words)
        for p in text.split("\n\n").map(str::trim).filter(|p| p.starts_with("[elisa") || p.starts_with(context::NUDGE_PREFIX.trim()) || p.starts_with('(')) {
            out.push_str(p);
            out.push('\n');
        }
        for l in text.lines().map(str::trim).filter(|l| l.starts_with("[elisa")) { out.push_str(l); out.push('\n'); }
    }
    out
}

/// Whether new file text copies Elisa's own text (`own`: her system prompt and what she added to the last request):
/// one of her `[elisa: …]` / request notes, or a sentence of `own` (`reply_check::Echo`). Lines the file already
/// `had` and the user's own words are no copy.
fn copies_own(text: &str, had: &str, user: &str, own: &str) -> bool {
    let fresh: String = text.lines().filter(|l| !l.trim().is_empty() && !had.contains(l.trim()) && !user.contains(l.trim())).collect::<Vec<_>>().join("\n");
    if fresh.is_empty() { return false; }
    let note = ["[elisa:", context::NUDGE_PREFIX.trim(), crate::route::ASK_PREFIX.trim()].iter().any(|m| fresh.contains(m));
    note || crate::reply_check::Echo::new(own, user).strip(&fresh).trim() != fresh.trim()
}

/// Whether words that name a file ask about it or for something done to it: a question (it ends with "?"), or a
/// change ask (`asks_change`).
fn asks_about_file(body: &str) -> bool {
    body.trim_end().ends_with('?') || asks_change(body)
}

/// Whether the user's words ask for a change: the notes' claim check reads its change words in them
/// (`context::verify_claims` on a done list).
fn asks_change(body: &str) -> bool {
    // (an ask that only asks or checks, "file app.py có lỗi gì không? xem hàm add", is no change: `route::check_only`)
    !crate::route::check_only(body) && context::verify_claims(&format!("## Done\n{}", word_set(body).join(" ")), &context::Ledger::default(), &|_| None).contains(context::UNVERIFIED)
}

/// A change of words in a file asked without quote marks, by the frames of `data/edit_asks.txt` ("in the file change
/// foo to bar", "đổi foo thành bar trong the file"): (the file, the old words, the new words). The frame's words are
/// the ask's, in order, to its end (words may come first: "please …"); "the file" is one word that names a file
/// (`is_file`); "foo" and "bar" are one word or more each. The frame with the most words decides. None otherwise.
fn framed_edit(msg: &str, is_file: &dyn Fn(&str) -> bool) -> Option<(String, String, String)> {
    static FRAMES: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let frames = FRAMES.get_or_init(|| {
        let mut f: Vec<Vec<String>> = include_str!("../../data/edit_asks.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.replace("the file", "\u{1}").split_whitespace().map(str::to_lowercase).collect()).collect();
        f.sort_by_key(|w| std::cmp::Reverse(w.len()));
        f
    });
    let toks: Vec<&str> = msg.split_whitespace().collect();
    let bare = |t: &str| t.trim_matches(|c: char| "\"'`‘’“”,;:?!.()".contains(c)).to_string();
    // the frame's words from `fi` on against the ask's from `ti` on, to the end of both: the file, old and new words
    fn fit(fr: &[String], toks: &[&str], bare: &dyn Fn(&str) -> String, is_file: &dyn Fn(&str) -> bool, got: &mut [Option<String>; 3]) -> bool {
        let Some((w, rest)) = fr.split_first() else { return toks.is_empty() };
        match w.as_str() {
            "\u{1}" => {
                let Some((t, more)) = toks.split_first() else { return false };
                let f = t.trim_matches(|c: char| "\"'`<>()[],;:?!".contains(c)).trim_end_matches('.');
                if f.is_empty() || !is_file(f) { return false; }
                got[0] = Some(f.to_string());
                fit(rest, more, bare, is_file, got)
            }
            "foo" | "bar" => {
                let k = if w == "foo" { 1 } else { 2 };
                (1..=toks.len()).any(|n| {
                    got[k] = Some(toks[..n].iter().map(|t| bare(t)).collect::<Vec<_>>().join(" "));
                    got[k].as_deref().is_some_and(|s| !s.is_empty()) && fit(rest, &toks[n..], bare, is_file, got)
                })
            }
            lit => toks.split_first().is_some_and(|(t, more)| bare(t).to_lowercase() == lit && fit(rest, more, bare, is_file, got)),
        }
    }
    frames.iter().find_map(|fr| (0..toks.len()).find_map(|s| {
        let mut got = [None, None, None];
        fit(fr, &toks[s..], &bare, is_file, &mut got).then_some(())?;
        let [Some(f), Some(o), Some(n)] = got else { return None };
        Some((f, o, n))
    }))
}

/// The quoted parts of a message, in order: text between a pair of quote marks ('…', "…", `…`, ‘…’, “…”) that opens
/// after a space or the start and closes before a space, punctuation or the end (an apostrophe inside a word, as in
/// "don't", is no quote).
fn quoted_parts(msg: &str) -> Vec<String> {
    const PAIRS: [(char, char); 5] = [('\'', '\''), ('"', '"'), ('`', '`'), ('‘', '’'), ('“', '”')];
    let cs: Vec<(usize, char)> = msg.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let (at, c) = cs[i];
        let opens = i == 0 || !cs[i - 1].1.is_alphanumeric();
        if let Some(&(_, close)) = PAIRS.iter().find(|p| p.0 == c).filter(|_| opens) {
            let end = (i + 2..cs.len()).find(|&j| cs[j].1 == close && cs.get(j + 1).is_none_or(|n| !n.1.is_alphanumeric()));
            if let Some(j) = end {
                out.push(msg[at + c.len_utf8()..cs[j].0].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Content a model wrote for a file that can stand as text: words of two or more letters, at least two of them (a
/// reply of box coordinates, "[0.0, 0.68, 0.99, 0.77]", or "1. to" is no file content).
/// Code the model wrote with file-style line numbers in front ("1 def add(a, b):", "2     return a + b", "3:",
/// "4\t…"): the numbers taken off, with the one separator after each. Only when every non-empty line has one, they
/// count up by one, and the file's own lines do not start that way; otherwise as written.
fn code_unnumbered(c: &str, file: &str) -> String {
    let split = |l: &str| -> Option<(u64, String)> {
        let t = l.trim_start();
        let digits: String = t.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if digits.is_empty() || digits.len() > 6 { return None; }
        let rest = &t[digits.len()..];
        let rest = match rest.chars().next() {
            None => "",
            Some('\t' | ' ') => &rest[1..],
            Some(':' | '|' | '.') => rest[1..].strip_prefix(' ').unwrap_or(&rest[1..]),
            _ => return None,
        };
        Some((digits.parse().ok()?, rest.to_string()))
    };
    let lines: Vec<&str> = c.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 || file.lines().find(|l| !l.trim().is_empty()).is_some_and(|l| split(l).is_some()) { return c.to_string(); }
    let parts: Vec<(u64, String)> = match lines.iter().map(|l| split(l)).collect::<Option<Vec<_>>>() { Some(p) => p, None => return c.to_string() };
    if parts.windows(2).any(|w| w[1].0 < w[0].0 || w[1].0 > w[0].0 + 2) { return c.to_string(); }
    let mut out: String = c.lines().map(|l| if l.trim().is_empty() { String::new() } else { split(l).map(|p| p.1).unwrap_or_default() }).collect::<Vec<_>>().join("\n");
    if c.ends_with('\n') { out.push('\n'); }
    out
}

/// A reply about a listing that does not hold to it: a list item (a bullet line, or a part of a sentence listing two
/// or more of the names) that is no name of the listing, or names left out with no count of them.
fn listed_mismatch(reply: &str, names: &[String]) -> bool {
    let base = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    let known = |piece: &str| names.iter().any(|n| piece.contains(n.as_str()) || piece.contains(base(n).as_str()));
    let clean = |s: &str| s.trim().trim_matches(|c: char| "`*_\"'.,;:!?()[]".contains(c) || c.is_whitespace()).to_string();
    for l in reply.lines() {
        let t = l.trim_start();
        let item = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• "))
            .or_else(|| t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r));
        if let Some(item) = item {
            let first = clean(item.split_whitespace().next().unwrap_or(""));
            if !first.is_empty() && !known(&first) && !known(item.split([':', '—']).next().unwrap_or("")) { return true; }
            continue;
        }
        // (sentences end at a stop followed by a space or the line's end, never at a file name's dot)
        let mut sentences = Vec::new();
        let mut from = 0;
        for (i, ch) in l.char_indices() {
            let next = l[i + ch.len_utf8()..].chars().next();
            if matches!(ch, '.' | '!' | '?') && next.is_none_or(char::is_whitespace) { sentences.push(&l[from..i + 1]); from = i + 1; }
        }
        if from < l.len() { sentences.push(&l[from..]); }
        for sentence in sentences {
            if names.iter().filter(|n| sentence.contains(base(n).as_str())).count() < 2 { continue; }
            let first_at = names.iter().filter_map(|n| sentence.find(base(n).as_str())).min().unwrap_or(0);
            let tail = &sentence[first_at..];
            let pieces = tail.split(',').flat_map(|p| p.split(" and ")).flat_map(|p| p.split(" và ")).flat_map(|p| p.split(';'));
            for p in pieces {
                let c = clean(p);
                // (an empty item, "and ``", is no file either)
                if p.contains("``") && c.is_empty() { return true; }
                if !c.is_empty() && !known(&c) && c.split_whitespace().count() <= 3 { return true; }
            }
        }
    }
    let all_named = names.iter().all(|n| reply.contains(base(n).as_str()));
    let counted = crate::reply_check::numbers(reply).iter().any(|x| x == &names.len().to_string());
    !(all_named || counted || names.is_empty())
}

/// Text with its markdown marks in pairs: an unpaired "**" or "`" (a line cut short, a bold opened on another
/// line) taken out.
fn paired_marks(t: &str) -> String {
    let mut s = t.to_string();
    if s.matches("**").count() % 2 == 1 { if let Some(i) = s.rfind("**") { s.replace_range(i..i + 2, ""); } }
    if s.matches('`').count() % 2 == 1 { if let Some(i) = s.rfind('`') { s.replace_range(i..i + 1, ""); } }
    s.trim().to_string()
}

/// Two words that are one: the same, sharing their first `WORD_STEM` letters ("vulnerable", "vulnerabilities"), or
/// the first a short form of the second (its first three letters, then its other letters in order: "deps" of
/// "dependencies").
fn same_word(a: &str, b: &str) -> bool {
    if a == b { return true; }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    if common >= WORD_STEM { return true; }
    let (short, long) = if a.chars().count() <= b.chars().count() { (a, b) } else { (b, a) };
    if short.chars().count() < 4 || common < 3 || long.chars().count() <= short.chars().count() + 2 { return false; }
    let mut rest = long.chars();
    short.chars().all(|c| rest.any(|x| x == c))
}

/// The titles of web search results ("1. Title" lines of `web_search`'s result).
fn web_titles(results: &str) -> Vec<String> {
    results.lines().filter_map(|l| {
        let (n, t) = l.trim_start().split_once(". ")?;
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !t.trim().is_empty()).then(|| t.trim().to_string())
    }).collect()
}

/// the advisory levels a dependency check gives, least to worst
const DEPS_LEVELS: [&str; 5] = ["low", "moderate", "medium", "high", "critical"];
/// (package, version) → (advisories, worst level, highest fixed version)
type Found = ((String, String), (usize, Option<usize>, Option<Vec<u64>>));

/// A dependency check's result (`sec_deps`) read: how many packages it checked, and each package with advisories.
fn deps_found(result: &str) -> Option<(usize, Vec<Found>)> {
    let checked: usize = result.split_once(" packages checked")?.0.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    let levels = DEPS_LEVELS;
    let mut pk: Vec<Found> = Vec::new();
    let mut seen: Vec<Vec<String>> = Vec::new();
    for l in result.lines() {
        let Some((head, rest)) = l.split_once(" → ") else { continue };
        let mut hw = head.split_whitespace();
        let (Some(name), Some(ver)) = (hw.next(), hw.next()) else { continue };
        let level = rest.split('[').skip(1).filter_map(|x| x.split(']').next()).filter_map(|x| levels.iter().position(|l| x.eq_ignore_ascii_case(l))).max();
        let fixed = rest.split_once("fixed: ").map(|x| x.1.split(" — ").next().unwrap_or("").to_string()).unwrap_or_default();
        // (each version as given, without a cut mark: "2.32.4 …")
        let best = fixed.split(',').filter_map(|v| v.split_whitespace().next()).filter_map(|v| {
            let parts: Option<Vec<u64>> = v.split('.').map(|p| p.parse().ok()).collect();
            parts.filter(|p| !p.is_empty() && p.len() <= 4)
        }).max();
        let key = (name.to_string(), ver.to_string());
        let at = match pk.iter().position(|(k, _)| *k == key) { Some(i) => i, None => { pk.push((key, (0, None, None))); seen.push(Vec::new()); pk.len() - 1 } };
        // (one advisory counted once: its GHSA and PYSEC rows name the same id in brackets)
        let id = advisory_ids(rest.split(" fixed: ").next().unwrap_or(rest)).pop().unwrap_or_default();
        let e = &mut pk[at].1;
        if !seen[at].contains(&id) { seen[at].push(id); e.0 += 1; }
        e.1 = e.1.max(level);
        if best > e.2 { e.2 = best; }
    }
    Some((checked, pk))
}

/// A dependency check's result (`sec_deps`) in plain words: each package with known advisories, how many, the
/// worst level and the version to update to (the highest fixed version given). None when it is not such a result.
fn deps_summary(result: &str, vi: bool) -> Option<String> {
    let (checked, pk) = deps_found(result)?;
    let levels = DEPS_LEVELS;
    if pk.is_empty() {
        return Some(if vi { format!("Không có lỗ hổng đã biết trong {checked} gói đã kiểm tra.") } else { format!("No known security problems in the {checked} packages checked.") });
    }
    let parts: Vec<String> = pk.iter().map(|((n, v), (c, lv, fx))| {
        let worst = lv.map(|i| if vi { format!(", nặng nhất: {}", levels[i]) } else { format!(", worst {}", levels[i]) }).unwrap_or_default();
        let upd = fx.as_ref().map(|f| f.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("."))
            .map(|f| if vi { format!(" — nâng lên {f} trở lên") } else { format!(" — update to {f} or later") }).unwrap_or_default();
        if vi { format!("{n} {v} ({c} cảnh báo{worst}){upd}") } else { format!("{n} {v} ({c} advisor{}{worst}){upd}", if *c == 1 { "y" } else { "ies" }) }
    }).collect();
    // (a list cut short: the check's own total too)
    let total: Option<usize> = result.split("known advisories (").nth(1).and_then(|x| x.split(')').next()).and_then(|x| x.trim().parse().ok());
    let listed = result.lines().filter(|l| l.contains(" → ")).count();
    let all = total.filter(|t| *t > listed).map(|t| if vi { format!(" Tổng cộng {t} cảnh báo.") } else { format!(" {t} advisories in all.") }).unwrap_or_default();
    Some(if vi { format!("{} trên {checked} gói có lỗi bảo mật đã biết: {}.{all}", pk.len(), parts.join("; ")) }
        else { format!("{} of {checked} packages have known security problems: {}.{all}", pk.len(), parts.join("; ")) })
}

/// Code that is already in the file as it is (each non-empty line found among the file's lines, spaces aside).
fn same_code(code: &str, file: &str) -> bool {
    let norm = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let have: HashSet<String> = file.lines().map(norm).collect();
    let lines: Vec<String> = code.lines().map(norm).filter(|l| !l.is_empty() && !l.starts_with("```")).collect();
    // (the same code laid out otherwise is the same too: "def add(a, b): return a - b" for the two lines it was)
    let flat = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    !lines.is_empty() && (lines.iter().all(|l| have.contains(l)) || flat(file).contains(&flat(&lines.concat())))
}

fn usable_content(c: &str) -> bool {
    c.split(|ch: char| !ch.is_alphabetic()).filter(|w| w.chars().count() >= 2).count() >= 2
}

/// Whether `content` is more than the name of the file it is for: content whose words are all the target's
/// ("marketing-plan.md" for marketing-plan.md) is nothing to save.
fn adds_words(content: &str, target: &str) -> bool {
    let have: HashSet<String> = word_set(target).into_iter().collect();
    word_set(content).iter().any(|w| !have.contains(w))
}

/// Whether a line is one of Elisa's own marks, never content to save or to show as saved: an attachment marker
/// (`attach::MARK`), picture text, a nudge or an `[elisa: …]` note.
fn internal_line(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with(crate::attach::MARK) || context::is_picture_block(t) || t.starts_with(context::NUDGE_PREFIX.trim()) || t.to_lowercase().starts_with("[elisa")
}

/// `text` without its internal lines (`internal_line`), trimmed.
fn without_internal_lines(text: &str) -> String {
    text.lines().filter(|l| !internal_line(l)).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// most words a bracketed piece has and still be part of the text (a checkbox "[x]", a mark "[1]", a tag "[draft]"),
/// not an aside of the model's (`without_bracket_notes`)
const BRACKET_TAG_WORDS: usize = 2;

/// `text` without the model's bracketed asides: a "[…]" piece of more than BRACKET_TAG_WORDS words that is no link's
/// text ("[docs](https://…)"), "[This meeting is scheduled for Friday …]", "[Agenda: Review progress …]", cut to the
/// line's end when it is left open; a line that had words and has none left goes too (blank lines stay).
fn without_bracket_notes(text: &str) -> String {
    let cut = |line: &str| -> String {
        let mut out = String::new();
        let mut rest = line;
        while let Some(i) = rest.find('[') {
            let after = &rest[i + 1..];
            let (inner, tail, closed) = match after.find(']') { Some(j) => (&after[..j], &after[j + 1..], true), None => (after, "", false) };
            if !(closed && tail.starts_with('(')) && inner.split_whitespace().count() > BRACKET_TAG_WORDS {
                out.push_str(rest[..i].trim_end());
                rest = tail;
            } else {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
        out.push_str(rest);
        out
    };
    let kept: Vec<String> = text.lines().filter_map(|l| { let c = cut(l); (l.trim().is_empty() || !c.trim().is_empty()).then_some(c) }).collect();
    let t = kept.join("\n");
    if text.ends_with('\n') { format!("{t}\n") } else { t }
}

/// Whether a save ask points back at something said before ("save that plan to plan.md", "save it as list.csv",
/// "lưu kế hoạch đó vào plan.md": a `points_back` word of `data/reply_terms.txt` among its words once the file's
/// name is out), so its content is an earlier reply, never new words of the model's.
fn save_points_back(body: &str, file: &str) -> bool {
    crate::reply_check::refers_back(&body.replace(file, " "))
}

/// the saving words of `data/given_text_asks.txt`'s examples, their first words ("save", "write", "lưu", "ghi",
/// "tạo"; never a short word or one that points back, "it'd be nice to have …")
fn save_verbs() -> &'static HashSet<String> {
    static W: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| GIVEN_DATA.lines().filter(|l| !l.starts_with('#') && !l.starts_with('[')).filter_map(|l| word_set(l).first().cloned())
        .filter(|w| w.chars().count() >= 3 && !crate::reply_check::refers_back(w)).collect())
}

/// the saving words (`save_verbs`) and the words leading to the file in those examples ("to", "vào"): no part of
/// what a save names (`save_thing`)
fn save_frame_words() -> &'static HashSet<String> {
    static W: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| GIVEN_DATA.lines().filter(|l| !l.starts_with('#') && !l.starts_with('[')).filter_map(|l| {
        let w = word_set(l);
        w.windows(3).find(|x| x[1] == "the" && x[2] == "file").map(|x| x[0].clone())
    }).chain(save_verbs().iter().cloned()).collect())
}

/// Whether words ask to save something said before to `file` where the English word rules see no save
/// (`route::save_ask`): a saving word of the examples ("lưu", "ghi") with a word that points back ("đó", "nó":
/// `save_points_back`), as in "lưu kế hoạch đó vào plan.md".
fn back_save(body: &str, file: &str) -> bool {
    word_set(body).iter().any(|w| save_verbs().contains(w)) && save_points_back(body, file)
}

/// What a save that points back names ("save that plan to plan.md" → "plan", "lưu kế hoạch đó vào plan.md" → "kế
/// hoạch"): its words in order without the file, the frame's words (`save_frame_words`) and the words that point
/// back (`reply_check::refers_back`); in English only content words (`topics::words`). None when nothing is left
/// ("save it as list.csv").
fn save_thing(body: &str, file: &str) -> Option<String> {
    let rest = body.replace(file, " ");
    let vi = crate::route::vietnamese(body);
    let content = crate::topics::words(&rest);
    let skip = save_frame_words();
    let words: Vec<String> = rest.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_string()).filter(|t| !t.is_empty())
        .filter(|t| { let l = t.to_lowercase(); !skip.contains(&l) && !crate::reply_check::refers_back(&l) && (vi || content.contains(&l) || content.contains(l.trim_end_matches('s'))) })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The user's own words of a request message: its first text block that is no attachment, picture or nudge, without
/// Elisa's notes. None for any other message.
fn ask_words(m: &Message) -> Option<String> {
    if !is_request(m) { return None; }
    m.content.iter().find_map(|b| match b {
        Block::Text { text } if !text.starts_with(crate::attach::MARK) && !context::is_picture_block(text) && !text.starts_with(context::NUDGE_PREFIX) => Some(crate::route::strip_ask(context::request_body(text)).trim().to_string()),
        _ => None,
    })
}

/// The newest earlier reply that holds `thing` ("plan") for a save that points back at it: an answer of
/// `SAVED_ANSWER_MIN_WORDS` or more (none of Elisa's own lines or marks) that shares a content word with the ask
/// it answered, where that ask or the reply holds a word of `thing` (any answer when `thing` is empty: "save it").
/// None when no reply does (the plan ask got talk about pronouns): nothing in the chat is that thing yet.
fn answer_in_chat(history: &[Message], thing: &str) -> Option<String> {
    let want = crate::topics::words(thing);
    let mut ask: Option<String> = None;
    let mut best = None;
    for m in history {
        if let Some(a) = ask_words(m) { ask = Some(a); continue; }
        if m.role != Role::Assistant { continue; }
        let t = without_internal_lines(&texts(m));
        if t.split_whitespace().count() < SAVED_ANSWER_MIN_WORDS || t.starts_with('[') || no_answer_line(&t) { continue; }
        let Some(a) = ask.as_deref() else { continue };
        let (asked, said) = (crate::topics::words(a), crate::topics::words(&t));
        let holds = want.is_empty() || !want.is_disjoint(&said) || !want.is_disjoint(&asked);
        if holds && (!asked.is_disjoint(&said) || !want.is_disjoint(&said)) { best = Some(format!("{t}\n")); }
    }
    best
}

/// Elisa's own lines for a reply the model never gave, and for a save with nothing to save (`nothing_to_save_line`)
const NO_ANSWER_DATA: &str = include_str!("../../data/no_answer_lines.txt");

/// Elisa's line for a save that points back at something the chat does not hold yet (`no_thing_to_save` /
/// `nothing_to_save` in `data/no_answer_lines.txt`), in the ask's language, with what the ask names (`thing`) and the
/// file.
fn nothing_to_save_line(ask: &str, thing: Option<&str>, file: &str) -> String {
    let lang = if crate::reply_lang::detect(context::request_body(ask)) == Some(crate::reply_lang::Lang::Vi) { "[vi]" } else { "[en]" };
    let key = if thing.is_some() { "no_thing_to_save" } else { "nothing_to_save" };
    let line = NO_ANSWER_DATA.lines().map(str::trim).skip_while(|l| *l != lang).skip(1).take_while(|l| !l.starts_with('['))
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':'))).map(str::trim).unwrap_or("{file}: nothing to save yet.");
    line.replace("{thing}", thing.unwrap_or("")).replace("{file}", file)
}

/// Whether a whole-file rewrite keeps most of the file: at least half of its non-empty lines, or the file had at
/// most one line.
fn keeps_most(before: &str, after: &str) -> bool {
    let lines: Vec<&str> = before.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() <= 1 { return true; }
    let kept = lines.iter().filter(|l| after.contains(**l)).count();
    kept * 2 >= lines.len()
}

/// words an earlier answer needs before a save may take it whole when the model wrote no content (`run_own`)
const SAVED_ANSWER_MIN_WORDS: usize = 20;
/// a saved file shown whole in Elisa's reply after her save (`saved_line`) up to these many lines and characters
const SAVED_SHOWN_LINES: usize = 8;
const SAVED_SHOWN_CHARS: usize = 600;
/// words of the matched lines a reply must hold to count as about them (`grep_fix`)
const GREP_QUOTED_WORDS: usize = 2;
/// new code shown whole in Elisa's reply after her own fix (`own_fix`) up to these many lines
const FIXED_SHOWN_LINES: usize = 30;
/// a reply of fewer words than this after Elisa's own checks is only a fragment (`fragment_fix`)
const FRAGMENT_WORDS: usize = 4;
/// times a tiny window's web results are cut by a third to fit (`run_own_inline`)
const WEB_CUT_TRIES: usize = 8;
/// most result titles Elisa lists herself for a web ask (`plain_results`)
const WEB_TITLES_SHOWN: usize = 5;
/// the system prompt of `fix_words`' request, and most output tokens it may use
const OWN_FIX_SYSTEM: &str = "You fix code. You answer with the corrected code only.";
const OWN_FIX_TOKENS: usize = 1024;
/// first letters two words share to be one word (`same_word`)
const WORD_STEM: usize = 6;
/// most matching lines Elisa lists herself (`grep_fix`)
const GREP_SHOWN: usize = 10;

/// the request-only note of the answer request after Elisa's own calls did what the words asked (`turn_inner`):
/// the steps ran and their results are above, so the model answers in words, with no tools offered
const OWN_DONE_NOTE: &str = "(These steps are done. Now give the user the answer itself from what they returned: the facts, names or \
    numbers the message asks for, in plain words.)";

/// Example data for `meant_tool`: a section per tool (`[name {default args}]`) and an `[other]` section.
const OWN_TOOLS_DATA: &str = include_str!("../../data/own_tools.txt");
/// the all-MiniLM-L6-v2 similarity to a tool's nearest example from which a message is that tool's ask, and how much
/// nearer than the nearest `[other]` example it must be
const OWN_MEANING_MIN: f32 = 0.6;
const OWN_MEANING_LEAD: f32 = 0.1;
/// examples this close to the nearest one are equally near (`nearest_section_from`)
const NEAR_TIE: f32 = 0.001;
/// most letters of a word that names an example's "foo" right before it ("từ", "chữ") and counts as the shared sign
const SHORT_NAME: usize = 3;

/// the tools' sections (name, default arguments, examples with their vectors)
type Sections = Vec<(String, serde_json::Value, Vec<(String, Vec<f32>)>)>;
/// the sections, the `[other]` examples' vectors and their words
type Examples = (Sections, Vec<Vec<f32>>, HashSet<String>);

/// Example data read into sections and embedded (all-MiniLM-L6-v2): `[name {default args}]` headers with one example
/// per line, and an `[other]` section. None when the embedding model is not installed.
fn load_examples(data: &str) -> Option<Examples> {
    let mut secs: Vec<(String, serde_json::Value, Vec<&str>)> = Vec::new();
    for l in data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            Some(h) => {
                let (name, args) = h.split_once(' ').map_or((h, "{}"), |(n, a)| (n, a));
                secs.push((name.to_string(), serde_json::from_str(args).unwrap_or(json!({})), Vec::new()));
            }
            None => if let Some(s) = secs.last_mut() { s.2.push(l); },
        }
    }
    let other = secs.iter().find(|s| s.0 == "other").map(|s| s.2.clone()).unwrap_or_default();
    let other_words: HashSet<String> = other.iter().flat_map(|l| word_set(l)).map(|w| plain_word(&w)).collect();
    let other = embed_texts(&other)?;
    let mut out = Vec::new();
    for (n, a, ex) in secs.into_iter().filter(|s| s.0 != "other") {
        let v = embed_texts(&ex)?;
        out.push((n, a, ex.iter().map(|e| e.to_string()).zip(v).collect()));
    }
    Some((out, other, other_words))
}

/// all-MiniLM-L6-v2 vectors of the texts; None when the model is not installed.
pub(super) fn embed_texts(t: &[&str]) -> Option<Vec<Vec<f32>>> {
    let dir = elisa_mem::embed::model_dir()?;
    let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
    elisa_mem::embed::embed(&dir, &vocab, t, &mut |_| {}).ok().map(|v| v.into_iter().map(|x| x.to_vec()).collect())
}

/// The section a message is closest to in meaning: near enough to its nearest example (`OWN_MEANING_MIN`), nearer
/// than to every `[other]` example by `OWN_MEANING_LEAD`, and sharing a word of 3 letters or more with that nearest
/// example that no `[other]` example has (MiniLM reads Vietnamese poorly, so meaning alone is not enough). A short
/// word (`SHORT_NAME` letters or fewer) counts too where both put it right before "foo" (the word an example names
/// its "foo" by: "lưu từ foo vào the file", "ghi chữ foo vào the file"), and then being nearer than every `[other]`
/// example is enough.
fn nearest_section(ex: &Examples, msg: &str) -> Option<(String, serde_json::Value)> {
    nearest_section_signed(ex, msg, false)
}

/// `nearest_section`, where `signed` says the message already carries a sign of its own (it names a file of the
/// folder that is no text, a program): then nearer than every `[other]` example is enough, as for a word named
/// before "foo" (MiniLM reads Vietnamese poorly, and "kiểm tra … có … không" is near any check)
fn nearest_section_signed(ex: &Examples, msg: &str, signed: bool) -> Option<(String, serde_json::Value)> {
    nearest_section_from(ex, msg, signed, OWN_MEANING_MIN)
}

/// a word without its Vietnamese marks (`reply_lang::fold_mark`): the words of a message and of the examples are
/// compared this way, since an example with marks and its twin without embed alike (the embedder folds them), so
/// either may be the nearest, and the `[other]` side's words count with or without marks
fn plain_word(w: &str) -> String { w.chars().map(crate::reply_lang::fold_mark).collect() }

/// `nearest_section_signed` with the nearness its nearest example needs at least (`min`)
fn nearest_section_from(ex: &Examples, msg: &str, signed: bool, min: f32) -> Option<(String, serde_json::Value)> {
    let (tools, other, other_words) = ex;
    let q = embed_texts(&[msg.trim()])?.pop()?;
    let sim = |e: &[f32]| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>();
    let o = other.iter().map(|e| sim(e)).fold(f32::MIN, f32::max);
    let mine: HashSet<String> = word_set(msg).into_iter().filter(|w| w.chars().count() >= 3).map(|w| plain_word(&w)).filter(|w| !other_words.contains(w)).collect();
    let want = word_before_foo(msg).map(|w| plain_word(&w));
    tools.iter().filter_map(|(n, a, ex)| {
        let scored: Vec<(&String, f32)> = ex.iter().map(|(t, e)| (t, sim(e))).collect();
        let top = scored.iter().map(|x| x.1).fold(f32::MIN, f32::max);
        // (examples equally near, as MiniLM reads Vietnamese ones: the one naming its text by the message's own word
        // before "foo", when there is one)
        let (text, t) = scored.iter().filter(|x| x.1 >= top - NEAR_TIE).max_by_key(|x| want.is_some() && word_before_foo(x.0).map(|w| plain_word(&w)) == want).copied()?;
        let named = word_before_foo(text).map(|w| plain_word(&w)).filter(|w| w.chars().count() <= SHORT_NAME && !other_words.contains(w) && want.as_ref() == Some(w));
        let shared = named.is_some() || word_set(text).iter().any(|w| mine.contains(&plain_word(w)));
        // (a word named that way is the sign itself: nearer than every `[other]` example is enough)
        let lead = if named.is_some() || signed { 0.0 } else { OWN_MEANING_LEAD };
        (t >= min && t - o >= lead && ((named.is_none() && !signed) || t > o) && shared).then_some((t, n, a))
    }).max_by(|x, y| x.0.total_cmp(&y.0)).map(|(_, n, a)| (n.clone(), a.clone()))
}

/// whether a path is a file that holds no text: a NUL byte in its first 4 KiB (a program, a library)
fn is_program(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else { return false };
    if !p.is_file() { return false; }
    let mut buf = [0u8; 4096];
    f.read(&mut buf).is_ok_and(|n| buf[..n].contains(&0))
}

/// `meant_tool` for a message that names a program of the folder (`nearest_section_signed`). Not kept.
fn meant_tool_signed(msg: &str) -> Option<(String, serde_json::Value)> {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    nearest_section_signed(EX.get_or_init(|| load_examples(OWN_TOOLS_DATA)).as_ref()?, msg, true)
}

/// The tool a message asks for by meaning (`Session::meant_call`) and its default arguments; None when the embedding
/// model is not installed or no tool's examples are near enough. Also the routing's sign of a security ask in any
/// language (`mcp_reason::about_sec`), so one message is measured once.
pub(crate) fn meant_tool(msg: &str) -> Option<(String, serde_json::Value)> {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    type Seen = std::collections::HashMap<String, Option<(String, serde_json::Value)>>;
    static SEEN: std::sync::OnceLock<std::sync::Mutex<Seen>> = std::sync::OnceLock::new();
    let key = msg.trim();
    if let Some(b) = SEEN.get_or_init(Default::default).lock().ok().and_then(|s| s.get(key).cloned()) { return b; }
    let ex = EX.get_or_init(|| load_examples(OWN_TOOLS_DATA)).as_ref()?;
    let b = nearest_section(ex, msg);
    if let Ok(mut s) = SEEN.get_or_init(Default::default).lock() { if s.len() < 512 { s.insert(key.to_string(), b.clone()); } }
    b
}

/// The command a message gives to run in the shell, by the frames of `data/own_tools.txt`'s `[bash]` section ("run foo
/// in the shell", "chạy lệnh foo trong terminal"): the message's words around "foo" are the frame's (any words may come
/// first), and what stands in its place is the command, as written ("run echo $((40+5)) in the shell" → `echo
/// $((40+5))`, "run python3 --version in the shell" → `python3 --version`). The frame with the most words decides. Its
/// first word must read as a program: a name of letters, digits and `._-/`, that is a program on PATH or no dictionary
/// word ("run tests in the shell" gives no command). The closing words of any frame left on the command's end ("chạy
/// lệnh echo xin-chao giúp mình": "giúp mình"; "could you run echo hi for me please": "for me", "please") are never
/// part of it (`without_frame_tail`). None otherwise.
pub(crate) fn shell_command(msg: &str) -> Option<String> {
    let frames = bash_frames();
    let toks: Vec<&str> = msg.split_whitespace().collect();
    let low: Vec<String> = toks.iter().map(|t| frame_word(t)).collect();
    let mut best: Option<(usize, String)> = None;
    for (pre, post) in frames {
        if toks.len() < pre.len() + post.len() + 1 || low[toks.len() - post.len()..] != post[..] { continue; }
        let end = toks.len() - post.len();
        // the frame's first words, anywhere before the command ("please run …", "can you run …")
        let starts: Vec<usize> = if pre.is_empty() { vec![0] } else { (0..=end.saturating_sub(pre.len() + 1)).filter(|&i| low[i..i + pre.len()] == pre[..]).collect() };
        for s in starts {
            let from = s + pre.len();
            if from >= end { continue; }
            // (a comma or colon closing the frame's words: "in the shell, run foo")
            let cmd = toks[from..end].join(" ");
            let cmd = cmd.trim().trim_matches('`').trim_end_matches(['?', '.', '!']).trim().to_string();
            let first = cmd.split_whitespace().next().unwrap_or("");
            let program = !first.is_empty() && first.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) && !first.starts_with(|c: char| c.is_ascii_uppercase())
                && (on_path(first) || (!in_dictionary(&first.to_lowercase()) && first.chars().any(|c| c.is_ascii_alphabetic())));
            let n = pre.len() + post.len();
            if program && best.as_ref().is_none_or(|b| n > b.0) { best = Some((n, cmd)); }
        }
    }
    best.map(|b| without_frame_tail(&b.1, frames))
}

/// a word of a message as the `[bash]` frames spell theirs: its letters and digits, lowercase
fn frame_word(t: &str) -> String { t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase() }

/// The command frames of `data/own_tools.txt`'s `[bash]` section (the examples with "foo"): each one's words before
/// and after "foo" (`frame_word`).
fn bash_frames() -> &'static Vec<(Vec<String>, Vec<String>)> {
    static FRAMES: std::sync::OnceLock<Vec<(Vec<String>, Vec<String>)>> = std::sync::OnceLock::new();
    FRAMES.get_or_init(|| {
        let mut inside = false;
        let mut out = Vec::new();
        for l in OWN_TOOLS_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { inside = l.trim_start_matches('[').starts_with("bash"); continue; }
            if !inside { continue; }
            let toks: Vec<&str> = l.split_whitespace().collect();
            let Some(at) = toks.iter().position(|t| frame_word(t) == "foo") else { continue };
            let side = |s: &[&str]| s.iter().map(|t| frame_word(t)).filter(|t| !t.is_empty()).collect::<Vec<String>>();
            out.push((side(&toks[..at]), side(&toks[at + 1..])));
        }
        out
    })
}

/// `cmd` without the closing words of any `[bash]` frame left on its end ("echo xin-chao giúp mình nhé" → `echo
/// xin-chao`), the command's first word always kept.
fn without_frame_tail(cmd: &str, frames: &[(Vec<String>, Vec<String>)]) -> String {
    let tails: Vec<&[String]> = frames.iter().map(|f| f.1.as_slice()).collect();
    without_tail(cmd.split_whitespace().collect(), &tails).join(" ")
}

/// `toks` without the closing words of any frame left on its end (`tails`: each frame's words after "foo", as
/// `frame_word` spells them), the first token always kept.
fn without_tail<'a>(mut toks: Vec<&'a str>, tails: &[&[String]]) -> Vec<&'a str> {
    loop {
        let low: Vec<String> = toks.iter().map(|t| frame_word(t)).collect();
        let cut = tails.iter().filter(|p| !p.is_empty() && low.len() > p.len() && low[low.len() - p.len()..] == p[..]).map(|p| p.len()).max();
        match cut {
            Some(n) => toks.truncate(toks.len() - n),
            None => return toks,
        }
    }
}

/// the words the `[bash]` frames put right before their command ("run", "use", "chạy", "lệnh", "with")
fn frame_words_before() -> &'static HashSet<String> {
    static W: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| bash_frames().iter().filter_map(|(pre, _)| pre.last().cloned()).collect())
}

/// A word that reads as a program to run: short, lowercase letters, digits, `-` or `_`, a program on PATH and no
/// word of the dictionary ("wc", "df", "grep"; not "cat", "look" or "units").
fn is_program_word(t: &str) -> bool {
    (2..=12).contains(&t.len()) && t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !t.starts_with('-') && !in_dictionary(t) && on_path(t)
}

/// The program an ask that names no file ends with, right after a word the `[bash]` frames put before their command
/// ("how much disk space is free here? use df" → `df`, "dung lượng đĩa còn bao nhiêu? dùng df" → `df`). None otherwise
/// ("I love vim" names no frame word; "use the shell" names no program).
fn named_program(body: &str) -> Option<String> {
    let toks: Vec<&str> = body.split_whitespace().map(|t| t.trim_matches(|c: char| "?,!;:\"'`()".contains(c))).collect();
    let [.., before, last] = toks.as_slice() else { return None };
    (is_program_word(last) && frame_words_before().contains(&frame_word(before))).then(|| last.to_string())
}

/// most lines a reply written as commands may have to be run as them (`text_command`)
const TEXT_COMMAND_LINES: usize = 5;

/// A reply that is only shell commands, in a code block or not ("nc -z 1.1.1.1 80", "echo u06 > /tmp/x.txt\ncat
/// /tmp/x.txt"): each line starts with a program on PATH (`on_path`), carries no sentence end, and there are at most
/// `TEXT_COMMAND_LINES`. The lines as one command, else None (prose, an answer).
fn text_command(reply: &str) -> Option<String> {
    let lines: Vec<&str> = reply.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```"))
        .map(|l| l.trim_start_matches("$ ").trim_matches('`').trim()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() || lines.len() > TEXT_COMMAND_LINES { return None; }
    let command = |l: &str| {
        let first = l.split_whitespace().next().unwrap_or("");
        first.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) && !first.starts_with(|c: char| c.is_ascii_uppercase()) && on_path(first)
            && !(l.ends_with('.') && l.split_whitespace().count() > 3 && l.split_whitespace().filter(|w| in_dictionary(&w.to_lowercase())).count() * 2 > l.split_whitespace().count())
    };
    lines.iter().all(|l| command(l)).then(|| lines.join("\n"))
}

/// A file-changing call of Elisa's own tools (`write`, `edit`, `edit_symbol`) on an ask for the shell (`shell_ask`):
/// the wrong tool, refused with a note that says to run it in the shell. None for any other call or ask.
pub(crate) fn wrong_tool_for_shell(name: &str, msg: &str) -> Option<String> {
    (crate::tools::access(name) == crate::tools::Access::Write && shell_ask(msg))
        .then(|| format!("Not run: this ask is for the shell. Do it with a bash command, as asked, not with `{name}`."))
}

/// A turn about the shell: a command the words give (`shell_command`), or an ask by meaning for the shell (the
/// `[bash]` section of `data/own_tools.txt`: "use nc in the shell to test a connection to 1.1.1.1 port 80"). Its
/// words go to the shell: never a web fetch of a URL in them, nor a write of Elisa's own.
///
/// The shell named in a longer ask ("use the shell to write hello into .elisa/u06.txt") is further in meaning from
/// every short example than a plain ask is from its own, so here the lead over every `[other]` example and a word
/// shared with the nearest `[bash]` example that no `[other]` example has are enough, without `OWN_MEANING_MIN`. Only
/// the examples that speak of the shell count there, not the command frames ("run foo"): "run it" names no shell.
pub(crate) fn shell_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    let spoken = || load_examples(OWN_TOOLS_DATA).map(|(secs, other, ow)| {
        let secs = secs.into_iter().filter(|s| s.0 == "bash").map(|(n, a, ex)| (n, a, ex.into_iter().filter(|(t, _)| !word_set(t).iter().any(|w| w == "foo")).collect())).collect();
        (secs, other, ow)
    });
    shell_command(msg).is_some() || meant_tool(msg).is_some_and(|(n, _)| n == "bash")
        || EX.get_or_init(spoken).as_ref().and_then(|ex| nearest_section_from(ex, msg, false, 0.0)).is_some_and(|(n, _)| n == "bash")
}

/// Example data for `symbol_ask_kind`: a `[show]` section and an `[other]` section, the symbol written as "foo" and
/// the file as "the file".
const SYMBOL_DATA: &str = include_str!("../../data/symbol_asks.txt");

/// An ask to see one symbol of a code file as it is, by meaning (`data/symbol_asks.txt`), with the symbol's name put
/// as "foo" and the file's as "the file". False without the embedding model.
/// The kind of an ask about one symbol, by meaning (`data/symbol_asks.txt`, the symbol put as "foo" and the file as
/// "the file"): "show" (see it as it is: Elisa shows its lines) or "check" (a question whether something is wrong in
/// it: the model answers with its lines in hand). None for anything else, or without the embedding model.
pub(super) fn symbol_ask_kind(msg: &str) -> Option<String> {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(SYMBOL_DATA)).as_ref().and_then(|ex| nearest_section(ex, msg)).map(|s| s.0)
}

/// An ask in the terms of `data/symbol_asks.txt`: the file's name put as "the file" and the symbol's as "foo".
fn symbol_frame(body: &str, file: &str, sym: &str) -> String {
    body.replace(file, "the file").split_whitespace()
        .map(|t| if t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_') == sym { t.replace(sym, "foo") } else { t.to_string() })
        .collect::<Vec<_>>().join(" ")
}

/// Whether a reply about a piece of code says something of it beyond the code: words outside its code blocks and
/// outside the code's own lines, `JUDGE_WORDS` or more ("Đây là add trong app.py:" and the code says nothing of it).
fn judges_code(reply: &str, code: &str) -> bool {
    let code_lines: HashSet<String> = code.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let mut inside = false;
    let mut words = 0;
    for l in reply.lines() {
        if l.trim_start().starts_with("```") { inside = !inside; continue; }
        // (a line of the code with its read number before it, "1\tdef add(a, b):", is the code too)
        let bare = l.trim_start().split_once('\t').filter(|(n, _)| !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit())).map_or(l, |(_, t)| t);
        if inside || code_lines.contains(l.trim()) || code_lines.contains(bare.trim()) { continue; }
        // (a lead-in to the code, "Here is add in app.py:", judges nothing)
        if l.trim_end().ends_with(':') { continue; }
        words += l.split_whitespace().count();
    }
    words >= JUDGE_WORDS
}

/// Words wrapped whole in one tag of the model's ("<answer>\n…\n</answer>"): the words inside. As they were otherwise.
fn without_wrapper_tag(t: &str) -> String {
    let s = t.trim();
    let Some(rest) = s.strip_prefix('<') else { return t.to_string() };
    let Some((name, inner)) = rest.split_once('>') else { return t.to_string() };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { return t.to_string(); }
    match inner.trim_end().strip_suffix(&format!("</{name}>")) { Some(body) => body.trim().to_string(), None => t.to_string() }
}

/// Elisa's computed facts of a sheet (`situation::table_facts`: "Elisa computed it exactly from sales.csv: highest
/// units = South (340) · answer with these results") as her answer, in the language of `ask`: each fact in its
/// sentence of `data/table_fact_lines.txt`, its parts taken from the note's own shape ("{kind} {column} = {label}
/// ({value})" or "{kind} {column} = … = {value}"): "South có units cao nhất trong sales.csv (340)." A fact of a kind
/// with no sentence there keeps the note's words ("Theo sales.csv: … ."). None: no such part.
fn computed_table_line(part: &str, ask: &str) -> Option<String> {
    let rest = part.strip_prefix(crate::situation::COMPUTED)?.strip_prefix(" from ")?;
    let (name, facts) = rest.split_once(": ")?;
    let facts = facts.split(" · ").next()?.trim();
    if facts.is_empty() { return None; }
    let vi = crate::route::vietnamese(ask);
    let mut lines = table_fact_lines(vi);
    // (tied rows' labels joined with the language's own word, never the note's English "and")
    let ties = lines.iter().position(|(k, _)| k == "ties").map(|i| lines.remove(i).1);
    // (a kind's own line for tied rows, "{kind} tie": "North and South have the highest …")
    let (tie_lines, lines): (Vec<_>, Vec<_>) = lines.into_iter().partition(|(k, _)| k.ends_with(" tie"));
    let said: Vec<Option<String>> = facts.split("; ").map(|item| {
        let (left, right) = item.split_once(" = ")?;
        // (the longest kind whose words begin the fact's left side; the rest is the column)
        let (kind, line) = lines.iter().filter(|(k, _)| left.strip_prefix(k.as_str()).is_some_and(|r| r.starts_with(' '))).max_by_key(|(k, _)| k.len())?;
        let column = left[kind.len()..].trim();
        let last = right.rsplit(" = ").next()?.trim();
        let (label, value) = match last.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) { Some((l, v)) => (l.trim(), v.trim()), None => ("", last) };
        if column.is_empty() || value.is_empty() || (line.contains("{label}") && label.is_empty()) { return None; }
        let tied = label.contains(" and ");
        let line = tied.then(|| tie_lines.iter().find(|(k, _)| k.strip_suffix(" tie") == Some(kind.as_str())).map(|(_, l)| l)).flatten().unwrap_or(line);
        let label = match &ties { Some(j) => label.split(" and ").collect::<Vec<_>>().join(&format!(" {j} ")), None => label.to_string() };
        Some(line.replace("{label}", &label).replace("{column}", column).replace("{value}", value).replace("{file}", name))
    }).collect();
    if said.iter().all(Option::is_some) { return Some(said.into_iter().flatten().collect::<Vec<_>>().join(" ")); }
    Some(if vi { format!("Theo {name}: {facts}.") } else { format!("From {name}: {facts}.") })
}

/// Elisa's sentence for each kind of computed table fact, in English or Vietnamese (`data/table_fact_lines.txt`):
/// (the kind's words, the sentence).
fn table_fact_lines(vi: bool) -> Vec<(String, String)> {
    let lang = if vi { "[vi]" } else { "[en]" };
    TABLE_FACT_LINES.lines().map(str::trim).skip_while(|l| *l != lang).skip(1).take_while(|l| !l.starts_with('['))
        .filter(|l| !l.starts_with('#')).filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

/// Elisa's sentences for computed table facts
const TABLE_FACT_LINES: &str = include_str!("../../data/table_fact_lines.txt");

/// The note asking for a judgement of `sym` in file `f` (`code`) for the user's question `body`.
fn symbol_check_note(body: &str, f: &str, sym: &str, code: &str) -> String {
    format!("\n\n(The user asks: \"{body}\". `{sym}` in {f} is:\n```\n{}\n```\nAnswer that question about it in one to three plain sentences, in the language of the question: say whether something is wrong in it and what. Do not only repeat the code.)", code.trim_end())
}

/// Elisa's honest line that she could not judge `sym` in file `f`, in the language of `body`, with its code.
fn no_judge_line(body: &str, f: &str, sym: &str, code: &str) -> String {
    let name = Path::new(f).file_name().map_or(f.to_string(), |n| n.to_string_lossy().to_string());
    format!("{}\n```\n{}\n```", crate::reply_check::own_line_of("no_judge", body, &name).replace("{sym}", sym), code.trim_end())
}

/// Whether `t`, a model's judgement of `sym` in file `f`, is only Elisa's own line that she could not judge it said
/// back (`no_judge` of `data/no_answer_lines.txt`, in either language, the code and its lead-in left off): no
/// judgement, so her line with the code stands instead, never that line without it.
fn says_no_judgement(t: &str, f: &str, sym: &str) -> bool {
    let flat = |x: &str| x.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    let name = Path::new(f).file_name().map_or(f.to_string(), |n| n.to_string_lossy().to_string());
    let said = flat(t);
    // (the line's first sentence, in English and in Vietnamese: an ask in each language picks its line)
    ["", "có lỗi không?"].iter().map(|a| crate::reply_check::own_line_of("no_judge", a, &name).replace("{sym}", sym)).any(|l| {
        let first = flat(l.split(". ").next().unwrap_or(&l));
        !said.is_empty() && !first.is_empty() && (said.contains(&first) || flat(&l).contains(&said))
    })
}

/// most tokens of a judgement of code asked for on its own (`symbol_verdict`)
const CHECK_ANSWER_TOKENS: usize = 300;

/// fewest words outside the code a reply about it needs to count as saying something of it (`judges_code`)
const JUDGE_WORDS: usize = 5;

/// result pages Elisa reads at most for a live figure (`fetch_for_live_figure`)
const LIVE_FETCH_MAX: usize = 2;


/// Words of an ask about the present (`data/live_asks.txt`)
const LIVE_DATA: &str = include_str!("../../data/live_asks.txt");

/// The units of figures by kind, the ask words that take each kind, and the range frames (`data/amount_words.txt`)
const AMOUNT_DATA: &str = include_str!("../../data/amount_words.txt");

/// The wordings that ask for a number (`data/number_asks.txt`, `reply_check::asks_number`): no subject of an ask
const NUMBER_ASK_DATA: &str = include_str!("../../data/number_asks.txt");

/// Whether an ask is about the present ("giá vàng hôm nay thế nào nhỉ", "population of Iceland these days"): a `now`
/// wording of `data/live_asks.txt` in it as whole words, and no number of its own.
fn live_ask(body: &str) -> bool {
    if body.chars().any(|c| c.is_ascii_digit()) { return false; }
    live_key(body, "now:")
}

/// Whether `body` holds a wording of `key` ("now:") of `data/live_asks.txt` as whole words.
fn live_key(body: &str, key: &str) -> bool { !live_wordings(body, key).is_empty() }

/// The wordings of `key` ("now:") of `data/live_asks.txt` that `body` holds as whole words.
fn live_wordings(body: &str, key: &str) -> Vec<String> {
    let low = body.to_lowercase();
    LIVE_DATA.lines().map(str::trim).filter(|l| !l.starts_with('#')).filter_map(|l| l.strip_prefix(key))
        .flat_map(|l| l.split('|')).map(str::trim).filter(|w| !w.is_empty() && has_words(&low, w)).map(str::to_string).collect()
}

/// Whether an ask wants a figure from the web: about the present (`live_ask`), asking for a number
/// (`reply_check::asks_number`: "dân số Iceland là bao nhiêu"), or naming a quantity (an `ask` word of
/// `data/amount_words.txt`: "look up the population of Iceland online"); never one with a number of its own.
fn figure_ask(body: &str) -> bool {
    live_ask(body) || (!body.chars().any(|c| c.is_ascii_digit()) && (crate::reply_check::asks_number(body) || !ask_kinds(body).0.is_empty()))
}

/// A kind of figure of `data/amount_words.txt` ("count", "money", "degrees", "share"): its unit words and signs, the
/// words of an ask that take it, the longer wordings those do not count inside, and whether only an ask naming it
/// takes it (a share).
struct FigureKind { name: String, units: Vec<String>, asks: Vec<String>, nots: Vec<String>, asked_only: bool }

/// `data/amount_words.txt` read: the kinds, the multiplier words ("triệu", "million"), the range frames as tokens
/// ("between {} and {}"), and every unit and multiplier word (`amount_words`).
struct AmountData { kinds: Vec<FigureKind>, multipliers: Vec<String>, ranges: Vec<Vec<String>>, words: HashSet<String> }

fn amount_data() -> &'static AmountData {
    static D: std::sync::OnceLock<AmountData> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let mut d = AmountData { kinds: Vec::new(), multipliers: Vec::new(), ranges: Vec::new(), words: HashSet::new() };
        let list = |v: &str| v.split('|').map(str::trim).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<String>>();
        let mut section = String::new();
        for l in AMOUNT_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(rest) = l.strip_prefix('[') {
                let (name, flag) = rest.split_once(']').unwrap_or((rest, ""));
                section = name.trim().to_lowercase();
                if section != "multiplier" && section != "range" {
                    d.kinds.push(FigureKind { name: section.clone(), units: Vec::new(), asks: Vec::new(), nots: Vec::new(), asked_only: flag.trim() == "only when asked" });
                }
                continue;
            }
            let Some((key, val)) = l.split_once(':') else { continue };
            let val = list(val);
            match (section.as_str(), key.trim()) {
                ("multiplier", _) => { d.words.extend(val.iter().cloned()); d.multipliers = val; }
                ("range", _) => d.ranges = val.iter().map(|f| f.split_whitespace().map(str::to_string).collect()).collect(),
                (_, "unit") => { d.words.extend(val.iter().cloned()); if let Some(k) = d.kinds.last_mut() { k.units = val; } }
                (_, "ask") => if let Some(k) = d.kinds.last_mut() { k.asks = val; },
                (_, "not") => if let Some(k) = d.kinds.last_mut() { k.nots = val; },
                _ => {}
            }
        }
        d
    })
}

/// The kind a unit word or sign gives a figure ("người" a count, "$" money, "°" degrees, "%" a share); None for a
/// multiplier or any other word.
fn unit_kind(tok: &str) -> Option<&'static str> {
    amount_data().kinds.iter().find(|k| k.units.iter().any(|u| u == tok)).map(|k| k.name.as_str())
}

/// The kinds of value an ask takes, with the ask words that name them: an `ask` word of a kind of
/// `data/amount_words.txt` in it as whole words ("population" a count, "giá vàng" money, "nhiệt độ" degrees), none
/// inside a `not` wording of that kind ("đánh giá" is a review). Empty when it names none.
fn ask_kinds(body: &str) -> (Vec<&'static str>, Vec<String>) {
    let low = body.to_lowercase();
    let (mut kinds, mut words) = (Vec::new(), Vec::new());
    for k in &amount_data().kinds {
        let mut t = low.clone();
        for n in &k.nots { t = t.replace(n.as_str(), " "); }
        let found: Vec<&String> = k.asks.iter().filter(|a| has_words(&t, a)).collect();
        if found.is_empty() { continue; }
        kinds.push(k.name.as_str());
        words.extend(found.into_iter().cloned());
    }
    (kinds, words)
}

/// nearness of a results sentence to the ask it needs at least to answer it by meaning (`meaning_sentence`)
const MEANING_SENTENCE_MIN: f32 = 0.2;
/// sentences this near the nearest one are equally near; the one sharing most words with the ask wins then
const MEANING_SENTENCE_TIE: f32 = 0.03;
/// nearness of a sentence to what the ask asks beyond its subject ("who won the") from which that relation is found
/// in the sentence (a winner's sentence: 0.45–0.55; a frame alone, "what is", "gì", "how many", reaches no sentence
/// beyond 0.25: `meaning_sentence`)
const MEANING_ASKED_MIN: f32 = 0.3;

/// The sentence of web results that answers an ask by its meaning (all-MiniLM-L6-v2, `embed_texts`). The ask is its
/// topic without the frame (`strip_query_frames`: "who won the 2022 fifa world cup", "thủ đô của Úc"); the sentences
/// are the snippets' (no title line, no URL, no question, four words or more) sharing a word of three letters or more
/// with it, those in the ask's language when any are. The ask's words that half the result titles or half the
/// sentences hold are the search's subject ("2022 fifa world cup"); what the ask asks beyond them ("who won the") is
/// set against each
/// sentence, and where that relation is found in one (`MEANING_ASKED_MIN`) the nearest wins ("Argentina defeated
/// France in the final match to win its third World Cup title.", not the final's venue sentence, which names both
/// sides and the whole subject) among the sentences holding most of the asked words themselves ("vô địch"), the one
/// holding most of the names the results agree on (`reply_check::shared_names`: the sides and the host), then most
/// of the ask's words, when equally near (`MEANING_SENTENCE_TIE`).
/// Else — the ask asks nothing beyond its subject, or only a question frame ("what is", "là gì") — the sentence
/// holding most of the ask's words wins ("Thủ đô của Úc là Canberra, …", not Canberra's population sentence), then
/// most of the agreed names, the nearest to the ask among those. The ask has to be `MEANING_SENTENCE_MIN` near some
/// sentence. With a full stop.
/// None without the model or such a sentence.
pub(super) fn meaning_sentence(ask: &str, web: &str) -> Option<String> {
    let core = strip_query_frames(context::request_body(ask));
    let mut a: Vec<String> = Vec::new();
    for w in word_set(&core) { if w.chars().count() >= 2 && !a.contains(&w) { a.push(w); } }
    let vi = crate::route::vietnamese(&core);
    let mut all: Vec<(String, Vec<String>, bool)> = Vec::new();
    for (block, _) in web_blocks(web, false) {
        for l in block.lines().map(str::trim) {
            if l.is_empty() || l.contains("://") || !web_titles(l).is_empty() || l.starts_with('<') || l.starts_with('[') { continue; }
            for s in split_sentences(l) {
                let s = s.trim();
                if s.ends_with('?') || s.split_whitespace().count() < 4 || s.chars().count() > LIVE_SENTENCE_CHARS || table_line(s) { continue; }
                let w = word_set(s);
                let held: Vec<String> = a.iter().filter(|x| w.contains(x)).cloned().collect();
                if !held.iter().any(|x| x.chars().count() >= 3) || all.iter().any(|x| x.0 == s) { continue; }
                all.push((s.to_string(), held, crate::route::vietnamese(s) == vi));
            }
        }
    }
    if all.iter().any(|x| x.2) { all.retain(|x| x.2); }
    if all.is_empty() { return None; }
    // (the names the results agree on for the ask — Canberra in four of five; a contest's two sides and its host,
    // "Argentina", "France", "Qatar" — anchor the answer: a sentence holding none of them never wins while another
    // holds one (the capital's "Thủ đô của nước Úc có dân số…" sentence), and among sentences otherwise equal the
    // one holding most of them wins, the capital's sentence over its population's, the winner's sentence naming
    // both sides over the third-place play-off's naming only the host. Any of the names, never the most widely
    // held alone: that one is the host, named by the titles and the venue sentences in more entries than the
    // winner — `reply_check::shared_names`)
    let names = crate::reply_check::shared_names(&crate::reply_check::subject_words(ask), web);
    let agreed = |s: &str| { let w = crate::reply_check::words_of(s); names.iter().filter(|n| w.contains(n.as_str())).count() };
    if all.iter().any(|x| agreed(&x.0) > 0) { all.retain(|x| agreed(&x.0) > 0); }
    let titles: Vec<Vec<String>> = web_titles(web).iter().map(|t| word_set(t)).collect();
    let half = |n: usize, of: usize| of > 0 && n * 2 >= of;
    let subject: HashSet<&String> = a.iter().filter(|x| half(all.iter().filter(|c| c.1.contains(x)).count(), all.len())
        || half(titles.iter().filter(|t| t.contains(x)).count(), titles.len())).collect();
    let asked: String = core.split_whitespace().filter(|t| !word_set(t).iter().any(|w| subject.contains(w))).collect::<Vec<_>>().join(" ");
    // (the content words of what is asked beyond the subject, "vô địch": a sentence holding them says the asked
    // thing in the ask's own words, and comes before one only near it by meaning — the embedding is sure of
    // English and guesses at Vietnamese: a fixtures line came nearest to "ai vô địch")
    let asked_content: HashSet<String> = crate::topics::words(&asked).into_iter().collect();
    let literal = |s: &str| { let w = word_set(s); asked_content.iter().filter(|x| w.contains(*x)).count() };
    let texts: Vec<&str> = [core.trim(), asked.trim()].into_iter().chain(all.iter().map(|x| x.0.as_str())).collect();
    let v = embed_texts(&texts)?;
    let (qs, rest) = v.split_at(2);
    let near = |e: &Vec<f32>, q: &Vec<f32>| e.iter().zip(q).map(|(a, b)| a * b).sum::<f32>();
    // a sentence's nearness to the ask, to what it asks beyond the subject; the asked words, the agreed names and the
    // ask's words it holds
    struct Near<'a> { ask: f32, asked: f32, literal: usize, agreed: usize, held: usize, s: &'a str }
    let scored: Vec<Near> = rest.iter().zip(&all).map(|(e, c)| Near { ask: near(e, &qs[0]), asked: near(e, &qs[1]), literal: literal(&c.0), agreed: agreed(&c.0), held: c.1.len(), s: &c.0 }).collect();
    let top = |f: fn(&Near) -> f32| scored.iter().map(f).fold(f32::MIN, f32::max);
    if top(|x| x.ask) < MEANING_SENTENCE_MIN { return None; }
    let best = if !asked.trim().is_empty() && top(|x| x.asked) >= MEANING_ASKED_MIN {
        // (among the sentences holding most of the asked words themselves — all of them when none does — the nearest
        // to what is asked; equally near, the one holding most of the agreed names, then most of the ask's words)
        let most = scored.iter().map(|x| x.literal).max()?;
        let pool: Vec<&Near> = scored.iter().filter(|x| x.literal == most).collect();
        let t = pool.iter().map(|x| x.asked).fold(f32::MIN, f32::max);
        pool.into_iter().filter(|x| x.asked >= t - MEANING_SENTENCE_TIE).max_by_key(|x| (x.agreed, x.held))?
    } else {
        // (most of the ask's words, then most of the agreed names, then the nearest to the ask)
        let most = scored.iter().map(|x| (x.held, x.agreed)).max()?;
        scored.iter().filter(|x| (x.held, x.agreed) == most).max_by(|x, y| x.ask.total_cmp(&y.ask))?
    };
    let s = best.s;
    Some(if s.ends_with(['.', '!']) { s.to_string() } else { format!("{s}.") })
}

/// Whether a line of web text is a score or table line, no sentence: more than a third of its tokens hold no letter
/// ("Argentina (4) 3 - 3 (2) France Argentina won 4 - 2 on penalties Full Time 108' Lionel MESSI"; a sentence
/// giving a figure or two is prose, "Canberra có dân số 381.488 người, là thành phố …").
fn table_line(s: &str) -> bool {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let bare = toks.iter().filter(|t| !t.chars().any(char::is_alphabetic)).count();
    !toks.is_empty() && bare * 3 > toks.len()
}

/// Whether a reply only copies search results: it links a result (a URL) and every other word of it is in the
/// results ("1. Iceland Population https://www.worldpopulationclock.net/iceland-population/"), so it answers nothing.
fn copied_results(reply: &str, results: &str) -> bool {
    if !reply.contains("://") { return false; }
    let held = word_set(results);
    let own: String = reply.split_whitespace().filter(|w| !w.contains("://")).collect::<Vec<_>>().join(" ");
    word_set(&own).iter().all(|w| held.contains(w))
}

/// Whether a sentence gives the value of another quantity than `ask` names: it holds a word of
/// `data/other_quantity_words.txt` ("urban", "rank", "mật độ", "đứng thứ") that the ask does not hold.
fn other_quantity(ask: &str, s: &str) -> bool {
    static W: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let words = W.get_or_init(|| include_str!("../../data/other_quantity_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect());
    let (a, s) = (ask.to_lowercase(), s.to_lowercase());
    words.iter().any(|w| has_words(&s, w) && !has_words(&a, w))
}

/// A figure of a sentence (`figure_spans`): its digits without separators, the kind its unit gives (`unit_kind`; None
/// for a plain number: "394,324", "139 triệu"), and its span in chars, from the number to the end of its units.
struct Figure { digits: String, kind: Option<String>, start: usize, end: usize }

/// The figures of a sentence that read as values: numbers written with a separator ("84.500.000", "2,345"), with a
/// currency or unit sign right beside them ("$950", "28°C"), or with an amount word after them
/// (`data/amount_words.txt`: "139 triệu", "2.3 million", "5 nghìn", "21 độ", or written on when of two letters or more:
/// "21oC", "5km"); never a bare run of digits (a label like "9999", gold's purity; a phone number "1800545457"), "24k",
/// nor a year or a date's parts. The units after a number run on, with a "/" between ("triệu đồng/lượng",
/// "USD/ounce"): the first that is no multiplier gives the kind, else a sign beside the number.
fn figure_spans(s: &str) -> Vec<Figure> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].is_ascii_digit() { i += 1; continue; }
        let start = i;
        while i < cs.len() && (cs[i].is_ascii_digit() || (matches!(cs[i], '.' | ',') && cs.get(i + 1).is_some_and(char::is_ascii_digit))) { i += 1; }
        let written: String = cs[start..i].iter().collect();
        let digits: String = written.chars().filter(char::is_ascii_digit).collect();
        let sign_kind = |c: Option<&char>| c.filter(|c| !c.is_alphanumeric()).and_then(|c| unit_kind(&c.to_string()));
        let separated = written.contains(['.', ',']) && digits.len() >= 4;
        let year = digits.len() == 4 && digits.parse::<u32>().is_ok_and(|y| (1900..=2100).contains(&y));
        let date = start > 0 && cs[start - 1] == '/' || cs.get(i) == Some(&'/');
        let (units, end) = unit_run(&cs, i);
        let before = sign_kind(start.checked_sub(1).and_then(|p| cs.get(p)));
        if date || (units.is_empty() && before.is_none() && (year || !separated)) { continue; }
        let kind = units.iter().find_map(|u| unit_kind(u)).or(before).map(str::to_string);
        out.push(Figure { digits, kind, start, end: end.max(i) });
    }
    out
}

/// The unit words and signs running on from `at` (right after a number): a sign counts only written onto the number
/// ("28°C", "5%"), a word of two letters or more written on or after a space ("21oC", "139 triệu"), a single letter
/// only after a space ("5 k"; "24k" is gold's purity); the next units after spaces or a "/" ("đồng/lượng", "km/h").
/// With where they end.
fn unit_run(cs: &[char], at: usize) -> (Vec<String>, usize) {
    let mut units = Vec::new();
    let mut i = at;
    loop {
        let mut j = i;
        while j < cs.len() && cs[j].is_whitespace() { j += 1; }
        if !units.is_empty() && cs.get(j) == Some(&'/') { j += 1; while j < cs.len() && cs[j].is_whitespace() { j += 1; } }
        let spaced = j > i;
        let Some(&c) = cs.get(j) else { break };
        let tok: String = if c.is_alphabetic() {
            cs[j..].iter().take_while(|c| c.is_alphabetic() || matches!(c, '²' | '³')).collect::<String>().to_lowercase()
        } else if !c.is_alphanumeric() && !c.is_whitespace() && c != '/' { c.to_string() } else { break };
        let sign = !tok.chars().next().is_some_and(char::is_alphabetic);
        let fits = if sign { !spaced } else { spaced || tok.chars().count() >= 2 };
        if !fits || !amount_words().contains(&tok) { break; }
        i = j + tok.chars().count();
        units.push(tok);
        // (a sign ends the run: "28°C" has its degrees, the letter after is no unit)
        if sign { break; }
    }
    (units, i)
}

/// The figures of a sentence as digits (`figure_spans`): "84,5 triệu đồng" → "845".
fn live_figures(s: &str) -> Vec<String> { figure_spans(s).into_iter().map(|f| f.digits).collect() }

/// The kind of the first figure of a text (`figure_spans`), for tests and checks: "28°C" → "degrees".
#[cfg(test)]
fn figure_kind(s: &str) -> Option<String> { figure_spans(s).into_iter().next().and_then(|f| f.kind) }

/// The amount and unit words of `data/amount_words.txt`, lowercase.
fn amount_words() -> &'static HashSet<String> { &amount_data().words }

/// Whether a sentence gives a range of two figures, not one value: a `range` frame of `data/amount_words.txt` over
/// its tokens, each figure (with its units) read as "{}" ("wavered between about 30,000 and 80,000", "dao động từ
/// 40.000 đến 60.000 người"); a buy and a sell price side by side are no range.
fn figure_range(s: &str) -> bool {
    let figs = figure_spans(s);
    if figs.len() < 2 { return false; }
    let cs: Vec<char> = s.chars().collect();
    let mut toks: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if let Some(f) = figs.iter().find(|f| f.start == i) { toks.push("{}".to_string()); i = f.end; continue; }
        let c = cs[i];
        if c.is_whitespace() { i += 1; continue; }
        if c.is_alphanumeric() {
            let st = i;
            while i < cs.len() && cs[i].is_alphanumeric() { i += 1; }
            toks.push(cs[st..i].iter().collect::<String>().to_lowercase());
            continue;
        }
        toks.push(c.to_string());
        i += 1;
    }
    amount_data().ranges.iter().any(|f| (0..toks.len()).any(|at| frame_at(&toks[at..], f)))
}

/// Whether a range frame's tokens ("between", "{}", "and", "{}") open `t`, one word allowed before each figure
/// ("between about 30,000 and 80,000").
fn frame_at(t: &[String], f: &[String]) -> bool {
    match (t.first(), f.first()) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) if b == "{}" => (a == "{}" && frame_at(&t[1..], &f[1..])) || (a != "{}" && t.get(1).is_some_and(|x| x == "{}") && frame_at(&t[2..], &f[1..])),
        (Some(a), Some(b)) => a == b && frame_at(&t[1..], &f[1..]),
    }
}

/// The words of an ask that can name its subject ("Iceland" in "population of Iceland these days", "vàng" in "giá
/// vàng hôm nay"): its words of three letters or more without those of its `now` wordings (`data/live_asks.txt`), of
/// its quantity words (`ask_kinds`) and of its number-ask wordings (`data/number_asks.txt`).
fn subject_words(body: &str) -> Vec<String> {
    let low = body.to_lowercase();
    let mut drop = live_wordings(body, "now:");
    drop.extend(ask_kinds(body).1);
    drop.extend(NUMBER_ASK_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && has_words(&low, l)).map(str::to_string));
    let dropped: HashSet<String> = drop.iter().flat_map(|w| word_set(w)).collect();
    word_set(body).into_iter().filter(|w| w.chars().count() >= 3 && !dropped.contains(w)).collect()
}

/// Whether a sentence's words name the ask's subject (`subject_words`): one of them, or a word starting with one of
/// four letters or more ("Icelandic" for "Iceland"). True when the ask names none.
fn names_subject(subject: &[String], words: &[String]) -> bool {
    subject.is_empty() || subject.iter().any(|s| words.iter().any(|w| w == s || (s.chars().count() >= 4 && w.starts_with(s.as_str()))))
}

/// When a block of web text dates its figures (`dated_when`): a day written that is today (or the page's stamp or a
/// day written above it, `live_candidates`); only a `now` word, no day written ("Giá vàng hôm nay ở vàng miếng SJC
/// giảm…": the article's today, which may not be ours, so a page is still read for it, `fetch_for_live_figure`); a
/// day before today; or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum When { Undated, Now, Today, Before { date: String, days: i64 } }

/// When a block of web text (a result's snippet, a page's line) dates its figures: by the latest day or year it
/// writes (`situation::latest_dated`, its figures blanked first so "2000 người" is no year): today or later, a day or
/// year before today (as written, and the days back), today by a `now` wording when no day is written, or nothing.
fn dated_when(text: &str) -> When {
    match crate::situation::latest_dated(&blank_figures(text), crate::route::vietnamese(text), crate::situation::now_secs()) {
        Some(d) if d.before => When::Before { date: d.written, days: d.days_ago },
        Some(d) if d.day => When::Today,
        // (no day written: a `now` wording of `data/live_asks.txt` says now, "Dân số hiện tại của Iceland là
        // 403.547 người", "Giá vàng SJC hôm nay…" — the text's today, `When::Now`; "as of 2026 (live estimate)"
        // says no day)
        _ if live_key(text, "now:") => When::Now,
        _ => When::Undated,
    }
}

/// The day or year a text writes for itself (`situation::latest_dated`, its figures blanked; `vi`: the day first, as
/// the page's language writes it): today, or a day or year before as written with the days back ("(2022 est.)").
/// None when it writes none (a `now` word alone is none here: `dated_when`).
fn own_when(text: &str, vi: bool) -> Option<When> {
    match crate::situation::latest_dated(&blank_figures(text), vi, crate::situation::now_secs()) {
        Some(d) if d.before => Some(When::Before { date: d.written, days: d.days_ago }),
        Some(d) if d.day => Some(When::Today),
        _ => None,
    }
}

/// The full day a text writes ("13/11/2026", "June 30, 2026"; never a bare year), as `own_when`: what a headline, a
/// caption or an update stamp dates the lines under it with. None without one.
fn written_when(text: &str, vi: bool) -> Option<When> {
    crate::situation::latest_dated(&blank_figures(text), vi, crate::situation::now_secs()).filter(|d| d.day).and_then(|_| own_when(text, vi))
}

/// The day a page stamps its figures with: its first line holding an `updated:` wording of `data/live_asks.txt` and
/// a day written ("Nguồn: giavang.net, … - Cập nhật lúc 05:58 (13/11/2026)", "Last updated: 13 Nov 2026"). None
/// without one.
fn page_stamp(page: &str) -> Option<When> {
    let vi = crate::route::vietnamese(page);
    page_text_lines(page).into_iter().filter(|l| live_key(l, "updated:")).find_map(|l| written_when(l, vi))
}

/// The sites holding each figure of web text, added to `holders` (`live_figure_fix`, `figures_stand`): a search
/// result's figures under its link's host, a page's (`host`) under the page's — every figure, a rate beside a count
/// too, so a reply taking its values from two sites is seen ("396,500 … growing at 0.92% per year").
fn web_holders(web: &str, host: Option<&str>, holders: &mut std::collections::HashMap<String, HashSet<String>>) {
    for (block, url) in web_blocks(web, host.is_some()) {
        let h = host.map(str::to_string).or_else(|| url.as_deref().map(host_of)).unwrap_or_default();
        for n in live_figures(&block) { holders.entry(n).or_default().insert(h.clone()); }
    }
}

/// `text` with the digits of its figures (`figure_spans`) blanked, so a date read in it is no figure ("2000 người").
fn blank_figures(text: &str) -> String {
    let mut blank: Vec<char> = text.chars().collect();
    for f in figure_spans(text) { for c in blank.iter_mut().take(f.end).skip(f.start) { if c.is_ascii_digit() { *c = ' '; } } }
    blank.into_iter().collect()
}

/// The day a sentence dates itself to, as days from today (0 today, 1 tomorrow, -1 yesterday): the latest day it
/// writes ("vào 14/11/2026", "on Nov 13"; `situation::latest_dated`, its figures blanked first; `vi`: the day comes
/// first, as in the reply's language). None without one (a bare year is none).
fn sentence_day(s: &str, vi: bool) -> Option<i64> {
    crate::situation::latest_dated(&blank_figures(s), vi, crate::situation::now_secs()).filter(|d| d.day).map(|d| -d.days_ago)
}

/// A sentence of web text that gives a figure for an ask (`live_candidates`): the sentence with its full stop, when
/// its block dates it, the host of its result's link (None on a page: the page's own), and what ranks it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveFigure {
    sentence: String,
    when: When,
    source: Option<String>,
    /// it names the ask's subject (`names_subject`)
    subject: bool,
    /// it gives a single value, no range (`figure_range`)
    single: bool,
    /// the sentence itself says now ("Nhiệt độ hiện tại là 18°C", "Giá vàng SJC hôm nay": a `now` wording, `live_key`)
    now: bool,
    /// it holds a word of the ask itself, not only through the page heading it sits under (`live_candidates`)
    named: bool,
    /// it holds a figure of a kind the ask names (`ask_kinds`: "403.547 người" for a population)
    kinded: bool,
    /// from a page read whole, not a snippet
    page: bool,
    /// words of the ask in it
    hits: usize,
    /// its money is in the currency the ask's language means (1), in another language's alone (-1), or in none (0)
    /// (`home_currency`)
    home: i8,
    /// its figures are all asides of the asked quantity — a reading labelled feels-like, dew point, high or low
    /// (`aside:` of `data/live_asks.txt`, `aside_sentence`): never the answer in the current reading's place
    aside: bool,
}

/// What ranks a figure sentence (`LiveFigure::rank`).
type LiveRank = (bool, bool, bool, (i64, i8, bool), bool, bool, bool, usize);

impl LiveFigure {
    /// What makes one figure sentence a better answer than another, first part first: it gives the asked quantity
    /// itself, no aside of it ("Cảm giác như 24°C" never over the reading "22°C"), names the ask's subject, gives a
    /// single value, is the freshest (`freshness`: its day, its currency, said as now), holds a word of the ask
    /// itself, holds a figure of the kind asked, comes from a page read whole, and shares the most words with the ask.
    fn rank(&self) -> LiveRank {
        (!self.aside, self.subject, self.single, self.freshness(), self.named, self.kinded, self.page, self.hits)
    }

    /// How fresh the figure is, the higher the fresher: dated today (by its own day, a day written above it or the
    /// page's update stamp: a price table on a page updated today), then said as now with no day written (a
    /// snippet's "Giá vàng hôm nay…": its page's today, which the stamped table of that page outranks), then the
    /// newest dated day before today, an undated one last — a figure with its day ("396,500 on June 30, 2026") over
    /// one with none ("approximately 380,317 (live estimate)"), in any language.
    fn fresh(&self) -> i64 {
        match &self.when { When::Today => i64::MAX, When::Now => i64::MAX - 1, When::Before { days, .. } => -days, When::Undated => i64::MIN }
    }

    /// What tells figures of one day apart, after `fresh`: the one in the currency the ask's language means, over
    /// one naming no currency, over one in another language's alone (`home`: today's SJC table over today's world
    /// price in USD, to a Vietnamese ask about gold), then the one said as now ("Nhiệt độ hiện tại là 18°C" over the
    /// day's other readings).
    fn freshness(&self) -> (i64, i8, bool) { (self.fresh(), self.home, self.now) }
}

/// When a sentence of a block dates its figure: by its own day or year when it writes one ("404,827 as of
/// November 11, 2026"; "Iceland 2026 population is estimated at 402,329 at mid-year" writes a year and no day: today
/// by a `now` word, else undated), else as its block does (`block`: a day written in the sentence before it, the
/// day written above a page line, the page's stamp, a `now` word). A snippet's date goes with the figure of its own
/// sentence, never with the next sentence's.
fn sentence_when(s: &str, vi: bool, block: &When) -> When {
    match crate::situation::latest_dated(&blank_figures(s), vi, crate::situation::now_secs()) {
        Some(d) if d.before => When::Before { date: d.written, days: d.days_ago },
        Some(d) if d.day => When::Today,
        Some(_) if live_key(s, "now:") => When::Now,
        Some(_) => When::Undated,
        None => block.clone(),
    }
}

/// The blocks of web text a figure's date and source hold for: a search result's entries (the snippet lines of
/// each, with the entry's link), or, for a page (`page`), its lines (no link: the page's own).
fn web_blocks(web: &str, page: bool) -> Vec<(String, Option<String>)> {
    if page { return web.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| (l.to_string(), None)).collect(); }
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for l in web.lines().map(str::trim) {
        if !web_titles(l).is_empty() { out.push((String::new(), None)); continue; }
        // (the lines before the first entry: the results' header)
        let Some(cur) = out.last_mut() else { continue };
        if (l.starts_with("http://") || l.starts_with("https://")) && !l.contains(char::is_whitespace) {
            if cur.1.is_none() { cur.1 = Some(l.to_string()); }
            continue;
        }
        if !cur.0.is_empty() { cur.0.push('\n'); }
        cur.0.push_str(l);
    }
    out
}

/// The sentences of web text (a search's results, or a page when `page`) that give a figure for an ask: a snippet
/// or page sentence (no title line of the list, no URL, no question) holding a figure (`figure_spans`) of a kind the
/// ask takes (`ask_kinds`: a count or a plain number for a population, never a share; a kind the ask names, or any
/// but a share when it names none) and a word of the ask, not giving another quantity's value (`other_quantity`: the
/// urban count, a density, a rank). Each with when its block dates it (`dated_when`: a result's snippet, a page's
/// line), its result's site, and what ranks it (`LiveFigure`). In the order read. A page without such a sentence
/// gives its readings under a heading naming the ask instead ("Hà Nội" / "18oC": `READING_LINES`). A page figure
/// with no unit of its own that a table of the page gives in a unit note is given with that unit ("SJC Mua 135,000
/// Bán 138,000 (nghìn đồng/lượng)": `table_units`, `with_table_unit`). A sentence or reading giving only an aside of
/// the quantity ("Cảm giác như 24°C", "Điểm sương" / "17°C": `aside_sentence`, the `aside:` labels of
/// `data/live_asks.txt`) is marked one, ranked after every other (`LiveFigure::rank`), and a page with no other
/// sentence gives its readings as well (the current "22°C" over the feels-like).
fn live_candidates(ask: &str, web: &str, page: bool) -> Vec<LiveFigure> {
    let body = context::request_body(ask);
    // (the ask's words a sentence is about the ask by: not its `now` wordings — a forecast row "Hôm nay 100 %17°C"
    // shares only "hôm nay" with "thời tiết Hà Nội hôm nay" and is not about it)
    let now_words: HashSet<String> = live_wordings(body, "now:").iter().flat_map(|w| word_set(w)).collect();
    let a: Vec<String> = word_set(body).into_iter().filter(|w| w.chars().count() >= 3 && !now_words.contains(w)).collect();
    let subject = subject_words(body);
    let (kinds, _) = ask_kinds(body);
    let asked_only = |k: &str| amount_data().kinds.iter().any(|x| x.name == k && x.asked_only);
    let takes = |k: &Option<String>| k.as_deref().is_none_or(|k| kinds.contains(&k) || (kinds.is_empty() && !asked_only(k)));
    let mut out = Vec::new();
    let label = |l: &str| l.split_whitespace().count() <= 6 && l.chars().any(char::is_alphabetic) && !l.chars().any(|c| c.is_ascii_digit());
    let hits_in = |l: &str| { let w = word_set(l); a.iter().filter(|x| w.contains(x)).count() };
    // (a page's heading naming the ask, "Dự báo thời tiết Hà Nội", "Giá vàng": the sentences under it, within
    // `READING_LINES` lines, are about what it names and are given with it — "Giá vàng: SJC Mua 136,000 Bán 139,000.";
    // "Nhiệt độ hiện tại là 18°C, cảm giác như 19°C." names no word of the ask itself)
    let mut heading: Option<(usize, Vec<String>, String, usize)> = None;
    // (a page line's day: the day it writes itself; else the nearest day written above it within `READING_LINES`
    // lines — an article's dated headline, a table's dated caption, "Giá vàng hôm nay 12/11: …" over its paragraph;
    // else the day the page stamps its figures with, `page_stamp`; else its own `now` word, or none: `dated_when`)
    let stamp = page.then(|| page_stamp(web)).flatten();
    // (a page's tables' units by figure, from a unit note by the table, "Đơn vị: nghìn đồng/lượng": a bare figure
    // of the table, or the same figure in a ticker line, is given with it)
    let units = if page { table_units(web) } else { Default::default() };
    let vi_page = crate::route::vietnamese(web);
    let vi_ask = crate::route::vietnamese(body);
    let mut dated_above: Option<(When, usize)> = None;
    for (i, (block, url)) in web_blocks(web, page).into_iter().enumerate() {
        let vi_block = if page { vi_page } else { crate::route::vietnamese(&block) };
        // (a line's own day or year first, "(2022 est.)"; a full day written sets the day for the lines under it)
        let own = page.then(|| own_when(&block, vi_page)).flatten();
        if let Some(w) = page.then(|| written_when(&block, vi_page)).flatten() { dated_above = Some((w, i)); }
        if page && label(&block) && hits_in(&block) > 0 { heading = Some((hits_in(&block), word_set(&block), block.trim().trim_end_matches(':').to_string(), i)); continue; }
        let under = heading.as_ref().filter(|(_, _, _, at)| i <= at + READING_LINES);
        let when = match (own, page) {
            (Some(w), _) => w,
            (None, true) => dated_above.as_ref().filter(|(_, at)| i <= at + READING_LINES).map(|(w, _)| w.clone()).or_else(|| stamp.clone()).unwrap_or_else(|| dated_when(&block)),
            (None, false) => dated_when(&block),
        };
        let source = url.as_deref().map(host_of).filter(|h| !h.is_empty());
        for l in block.lines().map(str::trim) {
            if l.is_empty() || l.contains("://") || !web_titles(l).is_empty() || l.starts_with('<') || l.starts_with('[') || l.starts_with("title:") { continue; }
            for s in split_sentences(l) {
                let s = s.trim();
                if s.ends_with('?') || s.chars().count() > LIVE_SENTENCE_CHARS || other_quantity(body, s) { continue; }
                let figs = figure_spans(s);
                if !figs.iter().any(|f| takes(&f.kind)) { continue; }
                let mut w = word_set(s);
                let own = a.iter().filter(|x| w.contains(x)).count();
                let sentence = if s.ends_with(['.', '!']) { s.to_string() } else { format!("{s}.") };
                let (hits, sentence) = match under {
                    Some((h, hw, text, _)) if own == 0 && s.split_whitespace().count() >= 4 => { w.extend(hw.iter().cloned()); (*h, format!("{text}: {sentence}")) }
                    _ => (own, sentence),
                };
                if hits == 0 { continue; }
                let kinded = figs.iter().any(|f| f.kind.as_deref().is_some_and(|k| kinds.contains(&k)));
                let sentence = with_table_unit(&sentence, &figs, &units);
                let home = home_currency(&sentence, vi_ask);
                let aside = aside_sentence(s, figs.iter().filter(|f| takes(&f.kind)));
                // (the sentence's own date over its block's: a snippet's "as of November 11, 2026" dates its own
                // figure, not the mid-year estimate in the sentence after it)
                out.push(LiveFigure { sentence, when: sentence_when(s, vi_block, &when), source: source.clone(), subject: names_subject(&subject, &w), single: !figure_range(s), now: live_key(s, "now:"), named: own > 0, kinded, page, hits, home, aside });
            }
        }
    }
    // (a page that writes its figures as readings, no sentence around them — a weather page: "Thời tiết Hà Nội" /
    // "12/11/2026," / "Nhiều mây, trời âm u" / "18oC" / "0 mm", a price table's rows: each reading of a kind the ask
    // takes under a heading naming the ask, within `READING_LINES`, is one sentence with that heading and the label
    // line right before it ("Thời tiết Hà Nội: Nhiều mây, trời âm u, 18oC."), dated by the lines between; a page
    // whose only sentences are asides, "Cảm giác như 24°C" under "Hà Nội", gives its readings too: the "22°C" above)
    if page && out.iter().all(|c| c.aside) {
        let lines = page_text_lines(web);
        let reading = |l: &str| l.split_whitespace().count() <= 4 && l.starts_with(|c: char| c.is_ascii_digit());
        let mut i = 0;
        while i < lines.len() {
            let h = lines[i];
            i += 1;
            let hits = hits_in(h);
            if !label(h) || hits == 0 { continue; }
            for j in i..lines.len().min(i + READING_LINES) {
                let l = lines[j];
                if label(l) && hits_in(l) > 0 { break; }
                if !reading(l) { continue; }
                let figs = figure_spans(l);
                if !figs.iter().any(|f| takes(&f.kind)) { continue; }
                let lab = Some(lines[j - 1]).filter(|p| j > i && label(p));
                let sentence = match lab { Some(p) if p.contains(',') => format!("{h}: {p}, {l}."), Some(p) => format!("{h}: {p} {l}."), None => format!("{h}: {l}.") };
                let w = word_set(&format!("{h} {}", lab.unwrap_or_default()));
                let kinded = figs.iter().any(|f| f.kind.as_deref().is_some_and(|k| kinds.contains(&k)));
                let around = lines[i - 1..=j].join("\n");
                let sentence = with_table_unit(&sentence, &figs, &units);
                let home = home_currency(&format!("{around}\n{sentence}"), vi_ask);
                // (a reading under an aside's label, "Điểm sương" / "17°C", "Nhiệt độ cảm giác" / "24°C", is one)
                let aside = lab.into_iter().chain([h]).any(|p| live_key(p, "aside:"));
                out.push(LiveFigure { sentence, when: dated_when(&around), source: None, subject: names_subject(&subject, &w), single: true, now: live_key(&around, "now:"), named: true, kinded, page, hits: hits + lab.map_or(0, hits_in), home, aside });
                i = j + 1;
                break;
            }
        }
    }
    out
}

/// lines a page's reading may sit below the heading it belongs to ("Thời tiết Hà Nội" … "18oC"; `live_candidates`)
const READING_LINES: usize = 8;

/// tokens a line of a page's table has at most (a cell, a row label, a column header; `table_units`)
const TABLE_CELL_WORDS: usize = 4;

/// A page line that names a unit and no figure — a table's unit note, "Đơn vị: nghìn đồng/lượng", "Unit: USD per
/// ounce", "(triệu đồng/lượng)", a column header "Giá bán (VND)" — gives the unit as the page writes it, from its
/// first amount word (`data/amount_words.txt`) on ("nghìn đồng/lượng"). A line that is a unit word alone ("USD", a
/// column of a rates table) is none, nor any line with a digit. None for any other line.
fn unit_note(l: &str) -> Option<String> {
    let toks: Vec<&str> = l.split_whitespace().collect();
    if toks.len() < 2 && !l.contains('/') || toks.len() > 6 || l.chars().any(|c| c.is_ascii_digit()) { return None; }
    let plain = |t: &str| t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let is_unit = |t: &str| t.split('/').any(|p| amount_words().contains(&plain(p)));
    let at = toks.iter().position(|t| is_unit(t))?;
    let unit = toks[at..].join(" ");
    let unit = unit.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    (!unit.is_empty()).then_some(unit)
}

/// The units a page's tables give their bare figures (`unit_note`): the runs of table lines (`TABLE_CELL_WORDS`
/// tokens or fewer) right above and right below a unit note are its table — "SJC" / "135,000" / "138,000" … /
/// "Đơn vị: nghìn đồng/lượng" — and each bare figure of them (`figure_spans`, no unit of its own) is in that unit,
/// by its digits; the first note that holds a figure names it.
fn table_units(page: &str) -> std::collections::HashMap<String, String> {
    let lines = page_text_lines(page);
    fn cell(l: &&str) -> bool { l.split_whitespace().count() <= TABLE_CELL_WORDS }
    let mut out = std::collections::HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(unit) = unit_note(l) else { continue };
        let above = lines[..i].iter().rev().copied().take_while(cell);
        let below = lines[i + 1..].iter().copied().take_while(cell);
        for f in above.chain(below).flat_map(figure_spans).filter(|f| f.kind.is_none()) { out.entry(f.digits).or_insert_with(|| unit.clone()); }
    }
    out
}

/// A page's figure sentence with the unit the page's table gives its figure (`table_units`, `units`), when the
/// sentence gives no unit of its own (`figs`: none with a kind) and a bare figure of it is the table's: "Giá vàng:
/// SJC Mua 135,000 Bán 138,000." → "Giá vàng: SJC Mua 135,000 Bán 138,000 (nghìn đồng/lượng).". As it was otherwise.
fn with_table_unit(sentence: &str, figs: &[Figure], units: &std::collections::HashMap<String, String>) -> String {
    if figs.iter().any(|f| f.kind.is_some()) { return sentence.to_string(); }
    let Some(unit) = figs.iter().find_map(|f| units.get(&f.digits)) else { return sentence.to_string() };
    let core = sentence.trim_end_matches(['.', '!']);
    format!("{core} ({unit}).")
}

/// Whether a figure of a text is an aside of the asked quantity: an `aside:` wording of `data/live_asks.txt` ("cảm
/// giác như", "feels like", "dew point", "high", "low") stands before it in its clause (`cs`: the text's chars; a
/// clause ends at a comma, semicolon, full stop, exclamation or question mark or line break, none inside a number).
fn aside_figure(cs: &[char], f: &Figure) -> bool {
    let inside = |k: usize| k > 0 && cs[k - 1].is_ascii_digit() && cs.get(k + 1).is_some_and(char::is_ascii_digit);
    let from = (0..f.start).rev().find(|&k| matches!(cs[k], ';' | '!' | '?' | '\n') || matches!(cs[k], ',' | '.') && !inside(k)).map_or(0, |k| k + 1);
    live_key(&cs[from..f.start].iter().collect::<String>(), "aside:")
}

/// Whether a sentence gives its figures only as asides (`aside_figure`): each of `figs` (the figures the ask takes)
/// has an aside's label before it in its clause — "Cảm giác như 24°C", "High 26°C Low 19°C"; one giving the quantity
/// itself beside an aside is none ("Nhiệt độ hiện tại là 18°C, cảm giác như 19°C"), nor one with no figure.
fn aside_sentence<'a>(s: &str, figs: impl Iterator<Item = &'a Figure>) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let figs: Vec<&Figure> = figs.collect();
    !figs.is_empty() && figs.iter().all(|f| aside_figure(&cs, f))
}

/// A reply's figures (`live_figures`) split into those it gives as the quantity and those it labels as asides
/// (`aside_figure`): "Hà Nội 22°C, cảm giác như 24°C" → (["22"], ["24"]).
fn said_figures(reply: &str) -> (Vec<String>, Vec<String>) {
    let cs: Vec<char> = reply.chars().collect();
    let (aside, plain): (Vec<Figure>, Vec<Figure>) = figure_spans(reply).into_iter().partition(|f| aside_figure(&cs, f));
    (plain.into_iter().map(|f| f.digits).collect(), aside.into_iter().map(|f| f.digits).collect())
}

/// Whether the figures a reply labels as asides let it stand beside its figure of the quantity (`figures_stand` for
/// that one): each is held by a result or page (`held`) and is one the pages give as an aside (`asides`), as good as
/// the best (`good`), or no value of the asked quantity at all (`values`) — never a value of it dressed as an aside.
fn asides_stand(said: &[String], held: &HashSet<String>, good: &HashSet<String>, values: &HashSet<String>, asides: &HashSet<String>) -> bool {
    said.iter().all(|n| held.contains(n) && (asides.contains(n) || good.contains(n) || !values.contains(n)))
}

/// The sites of web results a reply names as its source, by the site's name ("according to Worldometer":
/// worldometers.info, `host_label`): a word of the reply that is the name, or, of five letters or more, starts it or
/// is started by it ("Worldometer" for "worldometers"); never a name that is a word of the ask ("Iceland" for
/// iceland.org names the subject, no site).
fn named_sites(reply: &str, hosts: &[String], ask: &str) -> Vec<String> {
    let asked = word_set(ask);
    let words = word_set(reply);
    hosts.iter().filter(|h| {
        let l = host_label(h);
        !l.is_empty() && !asked.contains(&l) && words.iter().any(|w| *w == l || (w.chars().count() >= 5 && l.chars().count() >= 5 && (w.starts_with(&l) || l.starts_with(w.as_str()))))
    }).cloned().collect()
}

/// A site's name as a reply would write it: the longest part of its host ("worldometers.info" → "worldometers",
/// "en.wikipedia.org" → "wikipedia", "sjc.com.vn" → "sjc").
fn host_label(host: &str) -> String {
    host.split('.').fold("", |best, p| if p.chars().count() > best.chars().count() { p } else { best }).to_lowercase()
}

/// The best figure sentence of web text for an ask (`live_candidates`, `LiveFigure::rank`; the first of equals).
/// None when none holds one.
fn live_sentence(ask: &str, web: &str, page: bool) -> Option<LiveFigure> {
    let mut best: Option<LiveFigure> = None;
    for c in live_candidates(ask, web, page) {
        if best.as_ref().is_none_or(|b| c.rank() > b.rank()) { best = Some(c); }
    }
    best
}

/// Whether a reply's figures let it stand for a figure ask (`live_figure_fix`): one of them is a figure of a sentence
/// as good as the best (`good`), every one is held by a result or page (`held`), none is another value of the asked
/// quantity (`values`: every candidate sentence's figures) that is not as good (an older count beside the latest:
/// "347.736 đến 403.547 người"), and one site holds every figure it gives (`holders`: the sites of every figure of
/// the results and pages, `web_holders`; "396,500 … growing at 0.92% per year" takes a count from one site and a
/// rate from another).
fn figures_stand(said: &[String], held: &HashSet<String>, good: &HashSet<String>, values: &HashSet<String>, holders: &std::collections::HashMap<String, HashSet<String>>) -> bool {
    if !said.iter().any(|n| good.contains(n)) || !said.iter().all(|n| held.contains(n) && (good.contains(n) || !values.contains(n))) { return false; }
    let mut sites: Option<HashSet<String>> = None;
    for n in said.iter().filter(|n| holders.contains_key(*n)) {
        let h = holders.get(n).cloned().unwrap_or_default();
        sites = Some(match sites { None => h, Some(s) => s.intersection(&h).cloned().collect() });
    }
    sites.is_none_or(|s| !s.is_empty())
}

/// The figures of a sentence that go with its newest day (`live_figures` otherwise): a sentence dating two figures
/// to two days ("396,500 on June 30, 2026, …, and 394,324 on January 1, 2026") gives the newer; a figure's day is the
/// first written after it, else the last before it (`situation::day_spans`).
fn fresh_figures(s: &str) -> Vec<String> {
    let blank = blank_figures(s);
    let days = crate::situation::day_spans(&blank, crate::route::vietnamese(s), crate::situation::now_secs());
    let newest = days.iter().map(|d| d.2).min();
    if days.iter().any(|d| Some(d.2) != newest) {
        let at: Vec<usize> = s.char_indices().map(|(b, _)| b).chain(std::iter::once(s.len())).collect();
        return figure_spans(s).into_iter().filter(|f| {
            let end = at[f.end.min(at.len() - 1)];
            let day = days.iter().find(|d| d.0 >= end).or_else(|| days.iter().rev().find(|d| d.1 <= at[f.start]));
            day.map(|d| d.2) == newest
        }).map(|f| f.digits).collect();
    }
    live_figures(s)
}

/// The days a text writes, as days back from today (`situation::day_spans`, its figures blanked), read both ways a
/// day is written (day first as in Vietnamese, month first as in English), so a reply's day is found in its results
/// however either writes it.
fn days_written(text: &str) -> HashSet<i64> {
    let blank = blank_figures(text);
    let now = crate::situation::now_secs();
    [true, false].into_iter().flat_map(|vi| crate::situation::day_spans(&blank, vi, now)).map(|d| d.2).collect()
}

/// Whether every day a reply writes is one its results write (`days_written`), or today (the clock's, which the
/// model holds): a date a reply puts on a figure comes from the results like the figure itself — "403.556 người vào
/// ngày 18/11/2026" where the result says 12/11/2026 dates it to a day no result holds, five days ahead. A day read
/// either way counts ("11/12/2026" in a result read month first is the "12/11/2026" of a Vietnamese reply).
fn dates_stand(reply: &str, results: &str) -> bool {
    let held = days_written(results);
    let blank = blank_figures(reply);
    let now = crate::situation::now_secs();
    let (a, b) = (crate::situation::day_spans(&blank, true, now), crate::situation::day_spans(&blank, false, now));
    let ok = |n: i64| n == 0 || held.contains(&n);
    let fine = |spans: &[(usize, usize, i64)], others: &[(usize, usize, i64)]| spans.iter().all(|d| ok(d.2) || others.iter().any(|e| (e.0, e.1) == (d.0, d.1) && ok(e.2)));
    fine(&a, &b) && fine(&b, &a)
}

/// The wordings of `key` ("currency:") of `data/live_asks.txt` by section: each section's name ("en", "vi") with
/// its list, in the file's order.
fn live_key_by_lang(key: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for l in LIVE_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(h) = l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) { out.push((h.trim().to_lowercase(), Vec::new())); continue; }
        let (Some(rest), Some(cur)) = (l.strip_prefix(key), out.last_mut()) else { continue };
        cur.1.extend(rest.split('|').map(str::trim).filter(|w| !w.is_empty()).map(str::to_lowercase));
    }
    out
}

/// Whether a sentence's money is in the currency an ask's language means (`currency:` of `data/live_asks.txt`, by
/// section: `live_key_by_lang`): 1 when it writes that currency ("139 triệu đồng/lượng" to a Vietnamese ask), -1
/// when it writes only another language's ("3.900 USD/ounce"), 0 when none ("SJC Mua 136,000 Bán 139,000"). A sign
/// counts written anywhere ("$3,877"), a word as whole words.
fn home_currency(s: &str, vi: bool) -> i8 {
    let low = s.to_lowercase();
    let lang = if vi { "vi" } else { "en" };
    let writes = |w: &str| if w.chars().any(char::is_alphanumeric) { has_words(&low, w) } else { low.contains(w) };
    let mut home = 0;
    for (section, words) in live_key_by_lang("currency:") {
        if !words.iter().any(|w| writes(w)) { continue; }
        if section == lang { return 1; }
        home = -1;
    }
    home
}

/// The weather conditions and their wordings (`data/weather_conditions.txt`)
const WEATHER_DATA: &str = include_str!("../../data/weather_conditions.txt");

/// A weather condition of `data/weather_conditions.txt`: its name, its wordings and the conditions it cannot hold with.
struct Condition { name: String, words: Vec<String>, against: Vec<String> }

/// `data/weather_conditions.txt` read: the conditions (the `[other]` section left out).
fn conditions() -> &'static Vec<Condition> {
    static C: std::sync::OnceLock<Vec<Condition>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let mut out: Vec<Condition> = Vec::new();
        let mut other = false;
        for l in WEATHER_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(h) = l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
                other = h.trim() == "other";
                if !other { out.push(Condition { name: h.trim().to_lowercase(), words: Vec::new(), against: Vec::new() }); }
                continue;
            }
            let Some(c) = out.last_mut().filter(|_| !other) else { continue };
            match l.strip_prefix("against:") {
                Some(v) => c.against = v.split('|').map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect(),
                None => c.words.push(l.to_lowercase()),
            }
        }
        out
    })
}

/// Whether condition `a` cannot hold with condition `b` (`against` of `data/weather_conditions.txt`).
fn conditions_clash(a: &str, b: &str) -> bool {
    conditions().iter().any(|c| c.name == a && c.against.iter().any(|x| x == b))
}

/// The conditions a text names (`conditions`): each wording as whole words, the longest first and taken out once
/// matched ("không mưa" is dry, not rain), never right before a number ("Nắng 7g 51p" counts sunshine, "mưa 94%" a
/// chance); and whether the text has letters beyond them (a line of a condition alone has none: "U ám").
fn condition_classes(text: &str) -> (Vec<String>, bool) {
    let low = text.to_lowercase();
    let mut blank = low.clone();
    let mut pairs: Vec<(&str, &str)> = conditions().iter().flat_map(|c| c.words.iter().map(move |w| (w.as_str(), c.name.as_str()))).collect();
    pairs.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count()));
    let mut found: Vec<String> = Vec::new();
    for (w, name) in pairs {
        let mut from = 0;
        while let Some(i) = blank[from..].find(w).map(|i| from + i) {
            let end = i + w.len();
            from = end;
            if !blank[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) || !blank[end..].chars().next().is_none_or(|c| !c.is_alphanumeric()) { continue; }
            if low[end..].trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()) { continue; }
            blank.replace_range(i..end, &" ".repeat(w.len()));
            if !found.iter().any(|f| f == name) { found.push(name.to_string()); }
        }
    }
    let letters = blank.chars().any(char::is_alphabetic);
    (found, letters)
}

/// A condition a page Elisa read shows (`page_conditions`): for which day (0 today, 1 tomorrow), which, the page's
/// sentence saying so (None for a line of the condition alone or a reading), the page's words for it as written, as a
/// sentence ("Mưa Rào Nhẹ.", "Lượng mưa 0 mm.", or the sentence), and the line it is on (`page_text_lines`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown { day: i64, name: String, sentence: Option<String>, said: String, at: usize }

/// A page's text lines: trimmed, no empty line, URL, title line, fence or note (`page_conditions`, `live_candidates`).
fn page_text_lines(page: &str) -> Vec<&str> {
    page.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("://") && !l.starts_with("title:") && !l.starts_with('<') && !l.starts_with('[')).collect()
}

/// A page's line as a sentence of its own ("Mưa Rào Nhẹ," → "Mưa Rào Nhẹ.").
fn line_sentence(l: &str) -> String {
    format!("{}.", l.trim().trim_end_matches([',', ';', ':', '.', '!']).trim_end())
}

/// The conditions a page Elisa read shows, by day: a line that is a condition alone ("U ám") and a reading with its
/// value on the next line ("Lượng mưa" / "0 mm") count for today; a sentence counts for the day it names
/// (`situation::day_offset`: "Ngày mai …", "Tomorrow: …"; a `now` word of `data/live_asks.txt`: "Tối nay …",
/// "Currently …"), with the sentence. A sentence naming no day (the climate of the place) counts for none.
fn page_conditions(page: &str) -> Vec<Shown> {
    let lines = page_text_lines(page);
    let bare = |l: &str| l.chars().next().is_some_and(|c| c.is_ascii_digit()) && l.split_whitespace().count() <= 4;
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        // (a label with its value on the next line counts as the pair: "Lượng mưa" / "0 mm", "Nắng" / "7g 51p", "Khả
        // năng mưa" / "94%"; a condition alone over a reading of another quantity, "Mưa vừa" / "18oC" — a figure of a
        // kind that stands on its own, `asked_only` not — is no label of it and counts on its own below)
        if l.split_whitespace().count() <= 4 && !l.chars().any(|c| c.is_ascii_digit()) && lines.get(i + 1).is_some_and(|n| bare(n)) {
            let own = figure_spans(lines[i + 1]).iter().any(|f| f.kind.as_deref().is_some_and(|k| amount_data().kinds.iter().any(|x| x.name == k && !x.asked_only)));
            if !own {
                let pair = format!("{l} {}", lines[i + 1]);
                out.extend(condition_classes(&pair).0.into_iter().map(|name| Shown { day: 0, name, sentence: None, said: line_sentence(&pair), at: i }));
                i += 2;
                continue;
            }
        }
        i += 1;
        let (cs, letters) = condition_classes(l);
        if !cs.is_empty() && !letters {
            out.extend(cs.into_iter().map(|name| Shown { day: 0, name, sentence: None, said: line_sentence(l), at: i - 1 }));
            continue;
        }
        for s in split_sentences(l) {
            let s = s.trim();
            let Some(day) = crate::situation::day_offset(s).or_else(|| live_key(s, "now:").then_some(0)) else { continue };
            let sentence = if s.ends_with(['.', '!']) { s.to_string() } else { format!("{s}.") };
            out.extend(condition_classes(s).0.into_iter().map(|name| Shown { day, name, sentence: Some(sentence.clone()), said: sentence.clone(), at: i - 1 }));
        }
    }
    out
}

/// The conditions a sentence of a reply claims, by day: each clause's wordings (`condition_classes`), for the day the
/// clause names or the one before it named (none: `day`); a sentence whose clauses name none, with the embedding
/// model, the condition its meaning is nearest to (`nearest_section` over the wordings, the `[other]` section the
/// sentences claiming none), for `day`.
fn claimed_conditions(s: &str, day: i64) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    let mut on = day;
    for clause in clauses(s) {
        on = crate::situation::day_offset(clause).unwrap_or(on);
        out.extend(condition_classes(clause).0.into_iter().map(|c| (on, c)));
    }
    if !out.is_empty() { return out; }
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    let ex = EX.get_or_init(|| load_examples(&WEATHER_DATA.lines().filter(|l| !l.trim_start().starts_with("against:")).collect::<Vec<_>>().join("\n")));
    ex.as_ref().and_then(|ex| nearest_section(ex, s)).map(|(n, _)| vec![(day, n)]).unwrap_or_default()
}

/// The clauses of a sentence: its parts between commas and semicolons, a comma inside a number kept ("18,5°C").
fn clauses(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    for (i, &(at, c)) in cs.iter().enumerate() {
        let inside = c == ',' && i > 0 && cs[i - 1].1.is_ascii_digit() && cs.get(i + 1).is_some_and(|n| n.1.is_ascii_digit());
        if matches!(c, ',' | ';') && !inside { out.push(&s[start..at]); start = at + c.len_utf8(); }
    }
    out.push(&s[start..]);
    out
}

/// The temperatures a sentence of a reply claims, by day, as `claimed_conditions`: each clause's degrees figures
/// (`figure_spans`), for the day the clause names or the one before it named (none: `day`).
fn claimed_degrees(s: &str, day: i64) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    let mut on = day;
    for clause in clauses(s) {
        on = crate::situation::day_offset(clause).unwrap_or(on);
        out.extend(figure_spans(clause).into_iter().filter(|f| f.kind.as_deref() == Some("degrees")).map(|f| (on, f.digits)));
    }
    out
}

/// The temperatures a page Elisa read gives, by day (0 today, 1 tomorrow): each degrees figure of a line
/// (`figure_spans`), for the day the line names itself (a day written, "Ngày mai … 23°C": the latest, a day without
/// its year read as ahead when it lies ahead, `situation::day_spans_ahead`; a day word, `situation::day_offset`; a
/// `now` word of `data/live_asks.txt`: today), else the last a short line named before it — a forecast's day
/// headings, "13/11/2026,", "T7 14/11", "Ngày mai" — today before any.
fn page_degrees(page: &str) -> Vec<(i64, String)> {
    let vi = crate::route::vietnamese(page);
    let now = crate::situation::now_secs();
    let mut day = 0;
    let mut out = Vec::new();
    for l in page_text_lines(page) {
        let written = crate::situation::day_spans_ahead(&blank_figures(l), vi, now).into_iter().map(|d| -d.2).max();
        let own = written.or_else(|| crate::situation::day_offset(l)).or_else(|| live_key(l, "now:").then_some(0));
        if let Some(d) = own.filter(|_| l.split_whitespace().count() <= 6) { day = d; }
        let d = own.unwrap_or(day);
        out.extend(figure_spans(l).into_iter().filter(|f| f.kind.as_deref() == Some("degrees")).map(|f| (d, f.digits)));
    }
    out
}

/// A reply about the weather of a day (`day`: 0 today, 1 tomorrow, the day the ask names) with its claims the page
/// read contradicts put right (`page_conditions`, `shown`; `page_degrees`, `degrees`): a sentence claiming a
/// condition the page does not show for that day while it shows one it cannot hold with (`conditions_clash`), a
/// temperature the page gives none of for that day while it gives others ("Nhiệt độ 13°C" over today's 18oC and
/// 24oC, 13oC being a later day's: `claimed_degrees`), or dating itself to another day than the one asked ("vào
/// 14/11/2026" to an ask about today, `sentence_day`), is replaced by the page's sentence for that day saying
/// otherwise (once), else taken out; a line emptied goes. A reply left claiming no condition for the day asked while
/// the page shows one ("18oC với chất lượng không khí…" over the page's "Mưa vừa") answers nothing: nothing of it
/// stands. None when nothing was contradicted; Some("") when nothing of the reply stands.
fn weather_rewrite(reply: &str, shown: &[Shown], degrees: &[(i64, String)], day: i64) -> Option<String> {
    let mut changed = false;
    let mut claimed = false;
    let vi = crate::route::vietnamese(reply);
    let mut lines: Vec<String> = Vec::new();
    for l in reply.lines() {
        if l.trim().is_empty() { lines.push(String::new()); continue; }
        let mut kept: Vec<String> = Vec::new();
        for s in split_sentences(l) {
            let s = s.trim();
            let on = crate::situation::day_offset(s).unwrap_or(day);
            let claims = claimed_conditions(s, on);
            let wrong = claims.iter().find(|(d, c)| {
                let on_day: Vec<&Shown> = shown.iter().filter(|x| x.day == *d).collect();
                !on_day.iter().any(|x| x.name == *c) && on_day.iter().any(|x| conditions_clash(c, &x.name))
            }).map(|(d, c)| (*d, Some(c.as_str())));
            let wrong = wrong.or_else(|| claimed_degrees(s, on).into_iter().find(|(d, t)| {
                let on_day: Vec<&String> = degrees.iter().filter(|x| x.0 == *d).map(|x| &x.1).collect();
                !on_day.is_empty() && !on_day.contains(&t)
            }).map(|(d, _)| (d, None)));
            let wrong = wrong.or_else(|| sentence_day(s, vi).filter(|d| *d != day).map(|_| (day, None)));
            let Some((d, c)) = wrong else {
                if claims.iter().any(|(d, c)| *d == day && conditions().iter().any(|x| x.name == *c)) { claimed = true; }
                kept.push(s.to_string());
                continue;
            };
            changed = true;
            let truth = shown.iter().filter(|x| x.day == d && c.is_none_or(|c| conditions_clash(c, &x.name))).find_map(|x| x.sentence.clone());
            if let Some(t) = truth.filter(|t| !lines.iter().chain(kept.iter()).any(|k| k.contains(t.as_str()))) {
                if d == day { claimed = true; }
                kept.push(t);
            }
        }
        if !kept.is_empty() { lines.push(kept.join(" ")); }
    }
    if !claimed && shown.iter().any(|x| x.day == day) { return Some(String::new()); }
    changed.then(|| lines.join("\n").trim().to_string())
}

/// A page's figure sentence for a weather ask with the condition the page shows for the day asked, when the sentence
/// itself claims none (`condition_classes`): the page's own words for the condition nearest the sentence on the page
/// (`Shown::said`, `at`: "Mưa Rào Nhẹ." right under "Nhiệt độ hiện tại là 18°C, cảm giác như 19°C."), after it. The
/// sentence as it was when it claims one, or the page shows none for that day.
fn with_page_condition(sentence: &str, page: &str, shown: &[Shown], day: i64) -> String {
    if !condition_classes(sentence).0.is_empty() { return sentence.to_string(); }
    // (a sentence given with its heading, "Dự báo thời tiết Hà Nội: Nhiệt độ hiện tại là 18°C…": found by its own part)
    let core = sentence.trim_end_matches(['.', '!']);
    let tail = core.rsplit_once(": ").map_or(core, |(_, t)| t);
    let at = page_text_lines(page).iter().position(|l| l.contains(core) || l.contains(tail));
    let near = shown.iter().filter(|x| x.day == day && !x.said.is_empty() && !sentence.contains(x.said.trim_end_matches('.')))
        .min_by_key(|x| at.map_or(x.at, |a| x.at.abs_diff(a)));
    match near { Some(x) => format!("{sentence} {}", x.said), None => sentence.to_string() }
}

/// Elisa's own weather line for the day asked (`day`: 0 today, 1 tomorrow) from a page she read
/// (`weather_claim_fix`): for today, the page's figure sentence (`live_sentence`: its current reading, an aside never
/// in its place, whole page sentences only) with the condition the page shows (`with_page_condition`) and the page's
/// site (`src`, none when empty), else the page's sentences naming the day (`shown`), each once; for another day the
/// page's sentences for it first, then its figure sentence. None when the page gives neither.
fn weather_answer(body: &str, page: &str, shown: &[Shown], day: i64, src: &str) -> Option<String> {
    let figure = || live_sentence(body, page, true).map(|f| {
        let s = with_page_condition(&f.sentence, page, shown, day);
        match (crate::route::vietnamese(body), src.is_empty()) { (_, true) => s, (true, false) => format!("{s} (nguồn: {src})"), (false, false) => format!("{s} (source: {src})") }
    });
    let days = || {
        let mut own: Vec<String> = Vec::new();
        for s in shown.iter().filter(|x| x.day == day).filter_map(|x| x.sentence.clone()) { if !own.contains(&s) { own.push(s); } }
        (!own.is_empty()).then(|| own.join(" "))
    };
    if day == 0 { figure().or_else(days) } else { days().or_else(figure) }
}

/// longest sentence a live figure is given in (`live_sentence`): a page's run-on table line is no sentence
const LIVE_SENTENCE_CHARS: usize = 300;

/// The URLs of a web search's results, in order (one per result: the lines that are a URL alone).
fn result_urls(results: &str) -> Vec<String> {
    results.lines().map(str::trim).filter(|l| (l.starts_with("http://") || l.starts_with("https://")) && !l.contains(char::is_whitespace)).map(str::to_string).collect()
}

/// A URL's host without "www." ("https://www.sjc.com.vn/gia-vang" → "sjc.com.vn").
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").trim_start_matches("www.").to_string()
}

/// Content without a first line that is a lead-in to the user: it starts with wordings of `data/lead_in_words.txt`
/// and ends with a colon ("Okay, here's a short marketing plan for a small coffee shop in Hanoi:"). As it was otherwise.
fn without_lead_in(content: &str) -> String {
    static W: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let leads = W.get_or_init(|| include_str!("../../data/lead_in_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(word_set).collect());
    let body = content.trim_start();
    let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
    let line = first.trim().trim_end_matches(['*', '_']).trim_end();
    if !line.ends_with(':') || rest.trim().is_empty() { return content.to_string(); }
    let w = word_set(line);
    let mut at = 0;
    while let Some(n) = leads.iter().filter(|d| !d.is_empty() && w.len() >= at + d.len() && w[at..at + d.len()] == d[..]).map(Vec::len).max() { at += n; }
    if at == 0 { return content.to_string(); }
    rest.trim_start_matches('\n').to_string()
}

/// Whether a text says a change to `new` was made: a sentence holding `new` with a wording of `data/done_words.txt` and
/// no future word (`data/future_words.txt`).
fn says_done(text: &str, new: &str) -> bool {
    static W: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let done = W.get_or_init(|| include_str!("../../data/done_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(word_set).collect());
    let new_low = new.to_lowercase();
    text.split_inclusive(['\n', '!', '?']).flat_map(|l| l.split_inclusive(". ")).any(|s| {
        let w = word_set(s);
        s.to_lowercase().contains(&new_low)
            && done.iter().any(|d| !d.is_empty() && w.windows(d.len()).any(|x| x == d.as_slice()))
            && !future_words().iter().any(|f| w.iter().any(|x| x == f) || (f.starts_with('\'') && s.to_lowercase().contains(f.as_str())))
    })
}

/// The words of `data/future_words.txt`.
fn future_words() -> &'static Vec<String> {
    static W: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| include_str!("../../data/future_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect())
}

/// Example data for `value_ask`: a `[value]` section and an `[other]` section, the setting written as "foo" and the
/// file as "the file".
/// most words of a value's name, and of the value, in a sentence that names it ("The secret word is PAPAYA.")
const VALUE_NAME_WORDS: usize = 3;
const VALUE_WORDS: usize = 4;
/// a result's first lines, where a tool gives its fields ("title: Example Domain"), for `field_fix`
const FIELD_LINES: usize = 10;
/// a reply longer than this keeps its words and gets the field after it (`field_fix`)
const FIELD_REPLY_WORDS: usize = 40;

/// Example data for `given_ask`: a `[given]` section and an `[other]` section.
const GIVEN_DATA: &str = include_str!("../../data/given_text_asks.txt");

/// The examples of `data/given_text_asks.txt`, embedded once; None without the embedding model.
fn given_examples() -> Option<&'static Examples> {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(GIVEN_DATA)).as_ref()
}

/// A save ask that gives the text to save itself, by meaning (`data/given_text_asks.txt`), with that text put as
/// "foo" and the file's name as "the file". False without the embedding model.
fn given_ask(msg: &str) -> bool {
    given_examples().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "given")
}

/// `given_ask` for a frame with one bare word as its text ("write foo to the file"): its nearest example is a
/// `[given]` one, however near an `[other]` one comes (the bare word itself is the sign, so no shared word is asked
/// for). None without the embedding model.
fn given_bare(frame: &str) -> Option<bool> {
    let (secs, other, _) = given_examples()?;
    let q = embed_texts(&[frame])?.pop()?;
    let sim = |e: &[f32]| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>();
    let g = secs.iter().filter(|s| s.0 == "given").flat_map(|s| s.2.iter().map(|(_, e)| sim(e))).fold(f32::MIN, f32::max);
    let o = other.iter().map(|e| sim(e)).fold(f32::MIN, f32::max);
    Some(g >= OWN_MEANING_MIN && g > o)
}

/// The nearness of a frame to the `[given]` examples when it reads as one (`given_ask`): None otherwise, or without
/// the embedding model.
fn given_sim(frame: &str) -> Option<f32> {
    if !given_ask(frame) { return None; }
    let (secs, _, _) = given_examples()?;
    let q = embed_texts(&[frame])?.pop()?;
    let sim = |e: &[f32]| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>();
    Some(secs.iter().filter(|s| s.0 == "given").flat_map(|s| s.2.iter().map(|(_, e)| sim(e))).fold(f32::MIN, f32::max))
}

/// Whether a frame reads as a save to a file at all, on either side of `data/given_text_asks.txt`'s examples ("sav
/// this to the file" is near "save this to the file"): its nearest example is near enough (`OWN_MEANING_MIN`) and
/// one of its words is a saving word of the examples (`save_verbs`), whole or short of its last letter ("sav"; MiniLM
/// puts "read the file" as near "write it to the file"). False without the embedding model.
fn save_frame(frame: &str) -> bool {
    let saving = word_set(frame).iter().any(|w| save_verbs().iter().any(|v| v == w || (w.chars().count() >= 3 && v.chars().count() == w.chars().count() + 1 && v.starts_with(w.as_str()))));
    if !saving { return false; }
    let Some((secs, other, _)) = given_examples() else { return false };
    let Some(q) = embed_texts(&[frame]).and_then(|mut v| v.pop()) else { return false };
    let sim = |e: &[f32]| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>();
    let g = secs.iter().flat_map(|s| s.2.iter().map(|(_, e)| sim(e))).fold(f32::MIN, f32::max);
    let o = other.iter().map(|e| sim(e)).fold(f32::MIN, f32::max);
    g.max(o) >= OWN_MEANING_MIN
}

/// the words the examples of `data/given_text_asks.txt` put right before their text ("word", "text", "chữ",
/// "containing"): the frame's words, never the text's start
fn given_leads() -> &'static HashSet<String> {
    static LEADS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    LEADS.get_or_init(|| GIVEN_DATA.lines().filter(|l| !l.starts_with('#') && !l.starts_with('[')).filter_map(word_before_foo).collect())
}

/// the quote marks a given text may stand in, opening and closing
const QUOTE_PAIRS: [(char, char); 5] = [('"', '"'), ('\'', '\''), ('“', '”'), ('‘', '’'), ('`', '`')];

/// The text a save ask gives to put in `file` ("save the word hello to deny.txt" → "hello", "write 'buy eggs' to
/// todo.txt" → "buy eggs", "write hello to x.txt" → "hello"): quoted text right after the first word, else the words
/// before the one leading to the file when the ask reads as giving them (`given_ask`), else one bare word after the
/// saving verb when its frame reads so (`given_bare`) and the word names no kind of content the `[other]` examples
/// speak of ("code", "output") and points at nothing said before ("it", "that": `reply_check::refers_back`), else
/// text after the file ("make hello.md with just the word hello in it" → "hello": `given_after`). Text after the
/// file's name and a colon ("sav this to hello.txt: xin chào thế giới") is the text when the words before read as a
/// save at all (`save_frame`: "this" then points at that text, not back at the chat). None otherwise (a summary, a
/// poem: the model writes those).
fn given_text(body: &str, file: &str) -> Option<String> {
    let at = body.find(file)?;
    let after = body[at + file.len()..].trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    if let Some(text) = after.strip_prefix(':').map(str::trim).filter(|t| !t.is_empty()) {
        let frame = format!("{} the file", body[..at].trim_end()).to_lowercase();
        if save_frame(&frame) { return Some(format!("{text}\n")); }
    }
    // (one word before the file, "make hello.md with …": no words before the one leading to it)
    let before = body[..at].trim_end();
    let (head, lead) = before.rsplit_once(char::is_whitespace).unwrap_or(("", before));
    let head = head.trim();
    for (o, c) in QUOTE_PAIRS {
        let (Some(s), Some(e)) = (head.find(o), head.rfind(c)) else { continue };
        if e <= s + o.len_utf8() { continue; }
        let inner = head[s + o.len_utf8()..e].trim();
        let before = head[..s].trim();
        let frame = format!("{before} foo {lead} the file").to_lowercase();
        return (!inner.is_empty() && (before.split_whitespace().count() <= 1 || given_ask(&frame))).then(|| format!("{inner}\n"));
    }
    let words: Vec<&str> = head.split_whitespace().collect();
    // (the frame's words: two or three first ("save the word foo …"), else one ("write foo …"), else more when the ask
    // opens with a clause of its own ("use the shell to write foo …"): the first frame that reads as one)
    // (a word the examples put right before their text, "the text foo", "dòng foo", is the frame's, never the text's start)
    let leads = given_leads();
    let framed = [2, 3, 1].into_iter().chain(4..words.len()).filter(|&k| k < words.len() && !leads.contains(&words[k].to_lowercase())).find_map(|k| {
        let frame = format!("{} foo {lead} the file", words[..k].join(" ")).to_lowercase();
        given_ask(&frame).then(|| (frame, words[k..].join(" ")))
    });
    // text after the file too ("it'd be nice to have a hello.md with just the word hello in it"): of the two readings
    // the nearer to the examples
    let after = given_after(body[..at].trim_end(), after);
    let text = match (framed, after) {
        (Some((frame, text)), Some((s, t))) => Some(if given_sim(&frame).is_some_and(|b| b >= s) { text } else { t }),
        (Some((_, text)), None) => Some(text),
        (None, Some((_, t))) => Some(t),
        (None, None) => {
            let &[verb, word] = words.as_slice() else { return None };
            let kinds = &given_examples()?.2;
            let frame = format!("{verb} foo {lead} the file").to_lowercase();
            let plain = !kinds.contains(&word.to_lowercase()) && !crate::reply_check::refers_back(word) && word.chars().any(char::is_alphanumeric);
            (plain && given_bare(&frame) == Some(true)).then(|| word.to_string())
        }
    };
    text.map(|t| format!("{t}\n"))
}

/// the closing words of the examples of `data/given_text_asks.txt` that name the file before the text ("make the
/// file with foo in it": "in it"; "tạo the file với chữ foo nhé": "nhé"), as `frame_word` spells them
fn given_tails() -> &'static Vec<Vec<String>> {
    static T: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    T.get_or_init(|| GIVEN_DATA.lines().filter(|l| !l.starts_with('#') && !l.starts_with('[')).filter_map(|l| {
        let (before, after) = l.split_once(" foo")?;
        let tail: Vec<String> = after.split_whitespace().map(frame_word).filter(|w| !w.is_empty()).collect();
        (before.contains("the file") && !tail.is_empty()).then_some(tail)
    }).collect())
}

/// The text a save ask gives after the file's name ("make hello.md with just the word hello in it" → "hello", "tạo
/// a.txt với chữ xin chào nhé" → "xin chào", "create a.txt with 'hi there' in it" → "hi there"), with its frame's
/// nearness to the `[given]` examples: a quoted part of the words after the file, else the words after one the
/// examples put before their text (`given_leads`) without the closing words of the examples (`given_tails`: "in
/// it", "nhé"); of the readings whose frame reads as a `[given]` example (`given_sim`), the nearest. None when none
/// reads as one.
fn given_after(head: &str, after: &str) -> Option<(f32, String)> {
    let after = after.trim();
    if after.is_empty() || head.is_empty() { return None; }
    // the readings to weigh: (the frame with "foo" for the text, the text)
    let mut readings: Vec<(String, String)> = Vec::new();
    for (o, c) in QUOTE_PAIRS {
        let (Some(s), Some(e)) = (after.find(o), after.rfind(c)) else { continue };
        if e <= s + o.len_utf8() { continue; }
        readings.push((format!("{head} the file {} foo {}", after[..s].trim(), after[e + c.len_utf8()..].trim()), after[s + o.len_utf8()..e].to_string()));
    }
    if readings.is_empty() {
        let toks: Vec<&str> = after.split_whitespace().collect();
        let tails: Vec<&[String]> = given_tails().iter().map(Vec::as_slice).collect();
        for p in (0..toks.len()).filter(|&p| given_leads().contains(&toks[p].to_lowercase())) {
            let text = without_tail(toks[p + 1..].to_vec(), &tails);
            let closing = toks[p + 1 + text.len()..].join(" ");
            readings.push((format!("{head} the file {} foo {closing}", toks[..=p].join(" ")), text.join(" ")));
        }
    }
    let mut best: Option<(f32, String)> = None;
    for (frame, text) in readings {
        let text = text.trim().trim_end_matches(['.', '!']).trim();
        if text.is_empty() { continue; }
        if let Some(s) = given_sim(&frame.to_lowercase()) { if best.as_ref().is_none_or(|b| s > b.0) { best = Some((s, text.to_string())); } }
    }
    best
}

/// numbers as words, for a count of lines a save asks (`data/number_words.txt`; words also everyday ones left out)
const NUMBER_DATA: &str = include_str!("../../data/number_words.txt");

/// The number of lines a save asks for ("a one-line summary", "3 lines", "một dòng"): a number, in digits or words,
/// right before a word for lines. None when the words give none.
fn asked_lines(body: &str) -> Option<usize> {
    let num = |w: &str| w.parse::<usize>().ok().or_else(|| NUMBER_DATA.lines().filter(|l| !l.trim_start().starts_with('#')).find_map(|l| {
        let (n, ws) = l.split_once('=')?;
        ws.split('|').map(str::trim).any(|x| !x.ends_with('?') && x == w).then(|| n.trim().parse().ok()).flatten()
    }));
    let low = body.to_lowercase();
    let toks: Vec<&str> = low.split(|c: char| c.is_whitespace() || c == '-').map(|t| t.trim_matches(|c: char| !c.is_alphanumeric())).filter(|t| !t.is_empty()).collect();
    toks.windows(2).find(|w| matches!(w[1], "line" | "lines" | "dòng")).and_then(|w| num(w[0])).filter(|n| (1..=FIT_LINES_MAX).contains(n))
}

/// most lines a save may ask for by count (`asked_lines`)
const FIT_LINES_MAX: usize = 50;

/// Content held to `n` lines: headings ("Notes.txt:") and empty lines left out, and when still more, one line
/// joins them all (`n` = 1) or the first `n` stay.
fn fit_lines(content: &str, n: usize) -> String {
    let all: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if all.len() <= n { return format!("{}\n", all.join("\n")); }
    // (a heading: "Notes.txt:", "# Summary", "**Summary of Notes**"; a code fence line, "```")
    // (a bold line holding a "name: value" is content: "**Meeting Code: ORCHID-42**")
    let heading = |l: &str| l.ends_with(':') || l.starts_with('#') || l.starts_with("```") || l.starts_with("~~~")
        || (l.len() > 4 && !l.contains(": ") && ((l.starts_with("**") && l.ends_with("**")) || (l.starts_with("__") && l.ends_with("__"))));
    let unbold = |l: &str| unemphasized(l).trim_matches(|c| c == '*' || c == '_').trim().to_string();
    let kept: Vec<String> = all.iter().copied().filter(|l| !heading(l)).map(unbold).collect();
    let kept = if kept.is_empty() { all.iter().map(|l| l.to_string()).collect() } else { kept };
    if kept.len() <= n { return format!("{}\n", kept.join("\n")); }
    // (joined into one: each line without its list mark, "- ", "* ", "• ", "1. ")
    let unmarked = |l: &str| -> String {
        let t = l.trim_start();
        let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")).or_else(|| t.strip_prefix("+ ")).unwrap_or(t);
        let t = match t.split_once(". ").or_else(|| t.split_once(") ")) { Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => rest, _ => t };
        t.trim_end_matches('.').to_string()
    };
    if n == 1 { return format!("{}.\n", kept.iter().map(|l| unmarked(l)).collect::<Vec<_>>().join("; ")); }
    format!("{}\n", kept[..n].join("\n"))
}

/// A line without its bold marks, taken out as pairs ("**Meeting Code:** ORCHID-42" → "Meeting Code: ORCHID-42"):
/// every "**" and "__" when they pair up, else the unpaired last one first (`paired_marks`). Single "*" and "_" stay
/// (a glob, a snake_case name).
fn unemphasized(l: &str) -> String {
    let mut s = paired_marks(l);
    for m in ["**", "__"] {
        if s.matches(m).count() % 2 == 1 { if let Some(i) = s.rfind(m) { s.replace_range(i..i + 2, ""); } }
        s = s.replace(m, "");
    }
    s.trim().to_string()
}

/// Model-made content for a save from files read, with each sentence left out that has no word (4 letters or more)
/// of them (made-up text, "The team reviewed progress…"), that hedges or guesses (`data/hedge_words.txt`: "likely",
/// "may", "có lẽ"), or that adds no word to the sentences before it. As it was when nothing was read or nothing
/// would stay.
fn from_read(content: &str, read: &str) -> String {
    static H: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let hedges = H.get_or_init(|| include_str!("../../data/hedge_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(word_set).collect());
    // (the model planning its work, "The task is to create a file named tomtat.md", "It should include …", and the
    // words a sentence cut off mid-way ends on: `data/planning_words.txt`)
    static P: std::sync::OnceLock<(Vec<Vec<String>>, Vec<String>)> = std::sync::OnceLock::new();
    let (plans, cut_ends) = P.get_or_init(|| {
        let (mut plan, mut cut, mut sec) = (Vec::new(), Vec::new(), "");
        for l in include_str!("../../data/planning_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { sec = l; continue; }
            if sec == "[plan]" { plan.push(word_set(l)); } else if sec == "[cut]" { cut.push(l.to_string()); }
        }
        (plan, cut)
    });
    let have: HashSet<String> = word_set(read).into_iter().filter(|w| w.chars().count() >= 4).collect();
    if have.is_empty() { return content.to_string(); }
    // (a piece cut off mid-way: no end mark of a sentence, and ending on "…", a comma or a word of the [cut] list)
    let cut_off = |part: &str| {
        let t = part.trim();
        t.ends_with('…') || t.ends_with("...") || t.ends_with(',')
            || (!t.ends_with(['.', '!', '?', ':']) && t.split_whitespace().count() > 3 && word_set(t).last().is_some_and(|w| cut_ends.contains(w)))
    };
    // (what the sentences kept so far say: one that adds no word of its own to them says nothing new)
    let mut said: HashSet<String> = HashSet::new();
    let mut keep = |part: &str| -> bool {
        let w = word_set(part);
        let hedged = hedges.iter().chain(plans.iter()).any(|h| !h.is_empty() && w.windows(h.len()).any(|x| x == h.as_slice())) || cut_off(part);
        let long: Vec<String> = w.iter().filter(|x| x.chars().count() >= 4).cloned().collect();
        let new = long.iter().any(|x| !said.contains(x));
        let ok = w.iter().any(|x| have.contains(x)) && !hedged && new;
        if ok { said.extend(long); }
        ok
    };
    let kept: String = content.split_inclusive('\n').map(|l| {
        let cs: Vec<(usize, char)> = l.char_indices().collect();
        let mut out = String::new();
        let mut from = 0;
        for (k, &(i, c)) in cs.iter().enumerate() {
            let end = ".!?".contains(c) && cs.get(k + 1).is_none_or(|n| n.1.is_whitespace());
            if end || k + 1 == cs.len() {
                let part = &l[from..i + c.len_utf8()];
                if part.trim().is_empty() || keep(part) { out.push_str(part); }
                from = i + c.len_utf8();
            }
        }
        out
    }).collect();
    if kept.trim().is_empty() { content.to_string() } else { kept }
}

/// What an attached document holds, in plain words from its text (`docs_answer`): each sheet's table by its columns
/// with its first rows, and any other lines as notes ("sales.xlsx has a table of Region, Units and Date (North: 42,
/// 2023-03-15; South: 7.5, 2023-03-16) and a note: "remember the milk"."). A text with no table: its first lines.
pub(super) fn doc_in_words(name: &str, text: &str, vi: bool) -> String {
    let and = if vi { " và " } else { " and " };
    let list = |xs: &[String]| match xs { [] => String::new(), [one] => one.clone(), [rest @ .., last] => format!("{}{and}{last}", rest.join(", ")) };
    let mut parts: Vec<String> = Vec::new();
    let mut sections: Vec<Vec<&str>> = vec![Vec::new()];
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if l.starts_with("--- ") && l.ends_with(" ---") { sections.push(Vec::new()); } else if let Some(s) = sections.last_mut() { s.push(l); }
    }
    for sec in sections.iter().filter(|s| !s.is_empty()) {
        let rows: Vec<Vec<String>> = sec.iter().filter(|l| l.contains('\t')).map(|l| l.split('\t').map(|c| c.trim().to_string()).collect()).collect();
        let notes: Vec<String> = sec.iter().filter(|l| !l.contains('\t')).map(|l| format!("\"{l}\"")).collect();
        let mut p = String::new();
        if let Some((head, data)) = rows.split_first() {
            let shown: Vec<String> = data.iter().take(DOC_ROWS_SHOWN).map(|r| match r.split_first() {
                Some((first, rest)) if !rest.is_empty() => format!("{first}: {}", rest.join(", ")),
                _ => r.join(", "),
            }).collect();
            let more = data.len().saturating_sub(DOC_ROWS_SHOWN);
            let more = if more == 0 { String::new() } else if vi { format!("; và {more} dòng nữa") } else { format!("; and {more} more rows") };
            p = if vi { format!("một bảng gồm {}", list(head)) } else { format!("a table of {}", list(head)) };
            if !shown.is_empty() { p.push_str(&format!(" ({}{more})", shown.join("; "))); }
        }
        if !notes.is_empty() {
            let n = list(&notes.iter().take(DOC_ROWS_SHOWN).cloned().collect::<Vec<_>>());
            let note = match (vi, notes.len() > 1) { (true, _) => format!("ghi chú {n}"), (false, false) => format!("a note: {n}"), (false, true) => format!("notes: {n}") };
            p = if p.is_empty() { note } else { format!("{p}{and}{note}") };
        }
        parts.push(p);
    }
    let what = parts.join("; ");
    if what.is_empty() { return String::new(); }
    if vi { format!("{name} có {what}.") } else { format!("{name} has {what}.") }
}

/// rows (and notes) of a document shown in Elisa's own words about it (`doc_in_words`)
const DOC_ROWS_SHOWN: usize = 5;

/// The advisory ids of a dependency check's result ("GHSA-…", "CVE-…", "PYSEC-…"), each with its package's name.
fn advisory_owners(result: &str) -> Vec<(String, String)> {
    result.lines().filter_map(|l| l.split_once(" → ")).flat_map(|(head, rest)| {
        let name = head.split_whitespace().next().unwrap_or("").to_lowercase();
        advisory_ids(rest.split(" fixed: ").next().unwrap_or(rest)).into_iter().map(move |id| (id, name.clone()))
    }).collect()
}

/// The advisory ids in a text: an upper-case prefix, a dash, then digits and letters ("CVE-2019-1010083").
fn advisory_ids(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '-')).filter(|t| {
        let Some((pre, rest)) = t.split_once('-') else { return false };
        pre.len() >= 2 && pre.chars().all(|c| c.is_ascii_uppercase()) && rest.chars().any(|c| c.is_ascii_digit())
    }).map(str::to_string).collect()
}

/// A reply with each sentence or line left out that names an advisory of one package (`owners`) and another of the
/// check's packages but not its own; true when one was.
fn drop_mispaired(reply: &str, owners: &[(String, String)]) -> (String, bool) {
    let names: HashSet<&str> = owners.iter().map(|(_, n)| n.as_str()).collect();
    let mut out = String::new();
    let mut dropped = false;
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        let words: HashSet<String> = word_set(piece).into_iter().collect();
        let wrong = advisory_ids(piece).iter().filter_map(|id| owners.iter().find(|(i, _)| i == id)).any(|(_, own)| {
            !words.contains(own) && names.iter().any(|n| n != own && words.contains(*n))
        });
        if wrong { dropped = true; if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); } } else { out.push_str(piece); }
    }
    (out, dropped)
}

/// One advisory of a dependency check's result: its ids, its package, its fixed versions and its level.
struct Advisory { ids: Vec<String>, pkg: String, fixed: Vec<Vec<u64>>, level: Option<String>, about: String }

/// The advisories of a dependency check's result ("flask 0.12 → GHSA-… (CVE-…) [HIGH] fixed: 0.12.3, 1.0 — …").
fn advisory_facts(result: &str) -> Vec<Advisory> {
    result.lines().filter_map(|l| l.split_once(" → ")).map(|(head, rest)| {
        let (ids, after) = rest.split_once(" fixed: ").unwrap_or((rest, ""));
        let fixed = after.split(" — ").next().unwrap_or("").split(',').filter_map(|v| version_places(v.trim()).first().map(|x| x.2.clone())).collect();
        let level = ids.split('[').nth(1).and_then(|x| x.split(']').next()).map(str::to_lowercase).filter(|l| DEPS_LEVELS.contains(&l.as_str()));
        let about = after.split_once(" — ").map(|x| x.1.to_string()).unwrap_or_default();
        Advisory { ids: advisory_ids(ids), pkg: head.split_whitespace().next().unwrap_or("").to_lowercase(), fixed, level, about }
    }).filter(|a| !a.ids.is_empty()).collect()
}

/// A reply with each sentence or line left out that names a known advisory with a fixed version the check does not
/// give it, or with a level none of its advisories has ("HIGH to CRITICAL" with no critical one). The versions in use
/// (`currents`) are no fixed versions. When most such lines are wrong, all of them go and Elisa's own list from the
/// check (`advisory_list`) stands where the first one was. True when any went.
fn drop_misstated(reply: &str, advs: &[Advisory], currents: &[Vec<u64>], vi: bool) -> (String, bool) {
    let pieces: Vec<&str> = reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")).collect();
    let judged: Vec<Option<bool>> = pieces.iter().map(|piece| {
        // (each id with every advisory that names it: a CVE may come as a GHSA and a PYSEC advisory)
        let ids = advisory_ids(piece);
        let named: Vec<&Advisory> = advs.iter().filter(|a| a.ids.iter().any(|i| ids.contains(i))).collect();
        if named.is_empty() { return None; }
        let fixed_of = |id: &String| -> Vec<Vec<u64>> { advs.iter().filter(|a| a.ids.contains(id)).flat_map(|a| a.fixed.clone()).collect() };
        let places = version_places(piece);
        let versions: Vec<Vec<u64>> = places.iter().map(|v| v.2.clone()).filter(|v| !currents.contains(v)).collect();
        // a version written right before the ids it is paired with, "0.12.3 (CVE-…)", must be one of their fixed
        // versions; elsewhere ("upgrade to 3.1.3 to fix CVE-…") one at or above a fixed version is right too
        let paired = places.iter().any(|(at, len, _)| piece[at + len..].trim_start().starts_with('('));
        let bad_fix = !versions.is_empty() && ids.iter().any(|id| {
            let f = fixed_of(id);
            !f.is_empty() && !versions.iter().any(|v| if paired { f.contains(v) } else { f.iter().any(|x| v >= x) })
        });
        let w = word_set(piece);
        let levels: HashSet<&str> = named.iter().filter_map(|a| a.level.as_deref()).collect();
        let bad_level = w.iter().any(|x| DEPS_LEVELS.contains(&x.as_str()) && !levels.contains(x.as_str()));
        Some(bad_fix || bad_level)
    }).collect();
    let listed = judged.iter().flatten().count();
    let wrong = judged.iter().flatten().filter(|w| **w).count();
    if wrong == 0 { return (reply.to_string(), false); }
    let most = wrong * 2 > listed;
    let mut out = String::new();
    let mut placed = false;
    for (piece, j) in pieces.iter().zip(&judged) {
        let drop = matches!(j, Some(true)) || (most && j.is_some());
        if !drop { out.push_str(piece); continue; }
        if most && !placed { out.push_str(&advisory_list(advs, vi)); out.push('\n'); placed = true; }
        else if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
    }
    (out, true)
}

/// A reply without the sentences that credit one package's advisories to another: a sentence about a package (the
/// last one named, carried on to the sentences after it in the same paragraph) holding more of the words only another
/// package's advisories use than of its own. Also without a sentence that names an advisory source by its id prefix
/// alone ("The `PYSEC` tool"), and a lower-case leftover right after a sentence that went. (text, any went, most went)
fn drop_credited_wrong(reply: &str, advs: &[Advisory]) -> (String, bool, bool) {
    let mut pkgs: Vec<String> = Vec::new();
    for a in advs { if !pkgs.contains(&a.pkg) { pkgs.push(a.pkg.clone()); } }
    let words_of = |p: &str| -> HashSet<String> { advs.iter().filter(|a| a.pkg == p).flat_map(|a| word_set(&a.about)).filter(|w| w.chars().count() >= 4).collect() };
    let all: Vec<(String, HashSet<String>)> = pkgs.iter().map(|p| (p.clone(), words_of(p))).collect();
    let only = |p: &str| -> HashSet<String> {
        let mine = &all.iter().find(|x| x.0 == p).map(|x| x.1.clone()).unwrap_or_default();
        mine.iter().filter(|w| !all.iter().any(|(q, ws)| q != p && ws.contains(*w)) && !pkgs.contains(w)).cloned().collect()
    };
    let prefixes: HashSet<String> = advs.iter().flat_map(|a| a.ids.iter().filter_map(|i| i.split('-').next().map(str::to_lowercase))).collect();
    let mut out = String::new();
    let (mut dropped_words, mut total_words) = (0, 0);
    let mut about: Option<String> = None;
    let mut last_dropped = false;
    let mut any = false;
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        if piece.trim().is_empty() { about = None; out.push_str(piece); continue; }
        let w = word_set(piece);
        total_words += w.len();
        if let Some(p) = pkgs.iter().rfind(|p| w.contains(p)) { about = Some(p.clone()); }
        let foreign = about.as_ref().is_some_and(|p| {
            // (more of another package's own words than of its own: "leak" alone does not keep a sentence about the
            // JSON encoding, the Pallets project and a session cookie)
            let count = |ws: HashSet<String>| { let mut seen: Vec<&String> = w.iter().filter(|x| ws.contains(*x)).collect(); seen.dedup(); seen.len() };
            let mine = count(only(p));
            pkgs.iter().any(|q| q != p && { let theirs = count(only(q)); theirs >= 2 && theirs > mine })
        });
        // (an id prefix standing alone as a word: "PYSEC", not "PYSEC-2018-66")
        let source = piece.split(|c: char| !(c.is_alphanumeric() || c == '-')).any(|t| !t.contains('-') && t.len() >= 3 && t.chars().all(|c| c.is_ascii_uppercase()) && prefixes.contains(&t.to_lowercase()));
        let leftover = last_dropped && piece.trim_start().chars().next().is_some_and(char::is_lowercase);
        if foreign || source || leftover {
            any = true;
            last_dropped = true;
            dropped_words += w.len();
            if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
        } else { last_dropped = false; out.push_str(piece); }
    }
    let out = if any { out.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n") } else { out };
    (out, any, any && dropped_words * 2 > total_words)
}

/// The wordings of `data/version_words.txt` under `key`, each as its words.
fn version_words(key: &str) -> Vec<Vec<String>> {
    static W: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    let data = W.get_or_init(|| include_str!("../../data/version_words.txt").lines().map(str::trim).filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(':')).map(|(k, w)| (k.trim().to_string(), w.trim().to_string())).collect());
    data.iter().filter(|(x, _)| x == key).map(|(_, w)| word_set(w)).filter(|w| !w.is_empty()).collect()
}

/// whether a phrase of `ps` ends `w`, with up to `reach` words after it
fn phrase_ends(w: &[String], ps: &[Vec<String>], reach: usize) -> bool {
    ps.iter().any(|p| (0..=reach).any(|gap| w.len() >= p.len() + gap && w[w.len() - gap - p.len()..w.len() - gap] == p[..]))
}

/// whether a phrase of `ps` starts `w`
fn phrase_starts(w: &[String], ps: &[Vec<String>]) -> bool {
    ps.iter().any(|p| w.len() >= p.len() && w[..p.len()] == p[..])
}

/// a version without its trailing zeros: 0.12.0 is 0.12
fn bare_version(v: &[u64]) -> Vec<u64> {
    let mut v = v.to_vec();
    while v.len() > 1 && v.last() == Some(&0) { v.pop(); }
    v
}

/// A dependency check's reply with each fixed version of a package (`found`, `advs`) that stands in a vulnerable or
/// in-use place for it ("versions `0.12` and `3.1.3` are vulnerable", `data/version_words.txt`) taken out of its
/// list; a sentence left with one version is said of one ("version `0.12` is vulnerable"), one left with none goes.
/// (text, any changed)
fn drop_fixed_as_vulnerable(reply: &str, names: &[String], found: &[Found], advs: &[Advisory]) -> (String, bool) {
    let (before, after, in_use) = (version_words("before"), version_words("after"), version_words("in_use"));
    let one: Vec<(String, String)> = include_str!("../../data/version_words.txt").lines().filter_map(|l| l.trim().strip_prefix("one:"))
        .filter_map(|x| x.split_once('=')).map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).collect();
    let mut out = String::new();
    let mut any = false;
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        let low = piece.to_ascii_lowercase();
        let places = version_places(piece);
        let mut t = piece.to_string();
        let mut removed = 0;
        for &(at, len, ref v) in places.iter().rev() {
            let Some(k) = names.iter().enumerate().filter_map(|(k, n)| low[..at].rfind(n.as_str()).map(|p| (p, k))).max().map(|x| x.1) else { continue };
            let ((pkg, ver), (_, _, fix)) = &found[k];
            if version_places(ver).first().is_some_and(|c| bare_version(&c.2) == bare_version(v)) { continue; }
            let fixes: Vec<Vec<u64>> = advs.iter().filter(|a| a.pkg == pkg.to_lowercase()).flat_map(|a| a.fixed.iter().map(|f| bare_version(f))).chain(fix.iter().map(|f| bare_version(f))).collect();
            if !fixes.contains(&bare_version(v)) { continue; }
            let (pre, post) = (word_set(&piece[..at]), word_set(&piece[at + len..]));
            if phrase_ends(&pre, &before, 2) || phrase_starts(&post, &after) { continue; }
            let post4: Vec<String> = post.iter().take(4).cloned().collect();
            if !(phrase_ends(&pre, &in_use, 3) || (0..post4.len()).any(|i| phrase_starts(&post4[i..], &in_use))) { continue; }
            // (the version with its code marks, and the list mark joining it to the one before or after)
            let (mut a, mut b) = (at, at + len);
            while a > 0 && piece.as_bytes()[a - 1] == b'`' { a -= 1; }
            while b < piece.len() && piece.as_bytes()[b] == b'`' { b += 1; }
            let seps = [" and ", ", ", " or ", " / ", " và ", " hoặc "];
            if let Some(sep) = seps.iter().find(|x| t[..a].ends_with(*x)) { a -= sep.len(); }
            else if let Some(sep) = seps.iter().find(|x| t[b..].starts_with(*x)) { b += sep.len(); }
            t.replace_range(a..b, "");
            removed += 1;
        }
        if removed == 0 { out.push_str(piece); continue; }
        any = true;
        if version_places(&t).is_empty() {
            if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
            continue;
        }
        if version_places(&t).len() == 1 {
            // (each plural wording once, as a whole word, keeping a capital)
            for (many, single) in &one {
                let tl = t.to_lowercase();
                if tl.len() != t.len() { break; }
                let found_at = tl.match_indices(many.as_str()).map(|x| x.0).find(|&p| {
                    let before_ok = p == 0 || !tl[..p].chars().last().is_some_and(char::is_alphanumeric);
                    let after_ok = !tl[p + many.len()..].chars().next().is_some_and(char::is_alphanumeric);
                    before_ok && after_ok
                });
                if let Some(p) = found_at {
                    let cap = t[p..].chars().next().is_some_and(char::is_uppercase);
                    let mut rep = single.clone();
                    if cap { let mut c = rep.chars(); rep = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default(); }
                    t.replace_range(p..p + many.len(), &rep);
                }
            }
        }
        out.push_str(&t);
    }
    let out = if any { out.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n") } else { out };
    (out, any)
}

/// A sentence of a dependency check's reply with each version in an update or fixed-in place
/// (`data/version_words.txt`) that is below its package's fix (the package named last before it) raised to that fix,
/// unless a change ran (`changed`). A version that is the one in use ("0.12.0" for a pinned 0.12) or stands in an
/// in-use place ("associated with Flask 0.12.0") stays. `said[k]` is set for each package whose fix the sentence gives.
fn raised(s: &str, names: &[String], found: &[Found], advs: &[Advisory], changed: bool, said: &mut [bool]) -> String {
    let (before, after, in_use) = (version_words("before"), version_words("after"), version_words("in_use"));
    let ends_with = phrase_ends;
    let starts_with = phrase_starts;
    let bare = bare_version;
    // (ASCII case only: byte places stay the same)
    let low = s.to_ascii_lowercase();
    let mut t = s.to_string();
    // (right to left, so each place stays where it was found)
    for (at, len, v) in version_places(s).into_iter().rev() {
        let Some(k) = names.iter().enumerate().filter_map(|(k, n)| low[..at].rfind(n.as_str()).map(|p| (p, k))).max().map(|x| x.1) else { continue };
        let ((_, ver), (_, _, fix)) = &found[k];
        let Some(fix) = fix else { continue };
        if s[at..at + len] == *ver || version_places(ver).first().is_some_and(|c| bare(&c.2) == bare(&v)) { continue; }
        let (pre, post) = (word_set(&s[..at]), word_set(&s[at + len..]));
        if ends_with(&pre, &in_use, 3) { continue; }
        let fix_place = ends_with(&pre, &before, 2) || starts_with(&post, &after);
        // (a fix said of one advisory, "(CVE-2018-1000656) which is fixed in version 3.1.3": that advisory's own fix,
        // the advisory being the one of this package named last before the version, or the only one named)
        let pkg = &found[k].0.0.to_lowercase();
        let ids = advisory_ids(&s[..at]);
        let ids = if ids.is_empty() { advisory_ids(s) } else { ids };
        let own = ids.iter().rev().find_map(|id| advs.iter().find(|a| a.pkg == *pkg && a.ids.contains(id)));
        if let Some(a) = own.filter(|a| !a.fixed.is_empty() && fix_place && !changed) {
            if !a.fixed.iter().any(|f| bare(f) == bare(&v)) {
                let best = a.fixed.iter().max().cloned().unwrap_or_default();
                t.replace_range(at..at + len, &best.iter().map(u64::to_string).collect::<Vec<_>>().join("."));
            }
            continue;
        }
        if v >= *fix { said[k] = true; } else if !changed && fix_place {
            t.replace_range(at..at + len, &fix.iter().map(u64::to_string).collect::<Vec<_>>().join("."));
            said[k] = true;
        }
    }
    t
}

/// the id an advisory is known by in a check's line: the one in brackets after its own ("GHSA-… (CVE-…)"), the last
fn advisory_name(a: &Advisory) -> &str {
    a.ids.last().map(String::as_str).unwrap_or("")
}

/// the wording of `data/check_block.txt` for `key` in the reply's language
fn block_text(key: &str, vi: bool) -> String {
    let want = format!("{key}.{}", if vi { "vi" } else { "en" });
    include_str!("../../data/check_block.txt").lines().filter(|l| !l.trim_start().starts_with('#'))
        .find_map(|l| l.split_once(" = ").filter(|(k, _)| k.trim() == want).map(|(_, v)| v.to_string())).unwrap_or_default()
}

/// most advisory lines a dependency answer shows before it keeps to each package's worst (`deps_block`)
const DEPS_LINES_MAX: usize = 8;
/// advisory lines kept per package in a list cut short
const DEPS_LINES_KEPT: usize = 3;

/// One advisory of a check, its GHSA and PYSEC rows made one: (package, id, level, fixed versions, title).
type MergedAdvisory = (String, String, Option<usize>, Vec<Vec<u64>>, String);

/// A check's advisories, each once under the id its rows share; the level from the row that gives one, the fixed
/// versions of all its rows (release versions only), and the title of the row with a level, whole where one is.
fn merged_advisories(advs: &[Advisory]) -> Vec<MergedAdvisory> {
    let mut out: Vec<MergedAdvisory> = Vec::new();
    for a in advs {
        let name = advisory_name(a).to_string();
        let level = a.level.as_deref().and_then(|l| DEPS_LEVELS.iter().position(|x| *x == l));
        let at = match out.iter().position(|m| m.0 == a.pkg && m.1 == name) { Some(i) => i, None => { out.push((a.pkg.clone(), name, None, Vec::new(), String::new())); out.len() - 1 } };
        let m = &mut out[at];
        for f in &a.fixed { if !m.3.contains(f) { m.3.push(f.clone()); } }
        let title = a.about.trim().to_string();
        let better = m.4.is_empty() || (level.is_some() && m.2.is_none()) || (m.4.ends_with('…') && !title.ends_with('…') && (level.is_some() || m.2.is_none()));
        if better && !title.is_empty() { m.4 = title; }
        m.2 = m.2.max(level);
    }
    for m in &mut out { m.3.sort(); }
    out
}

/// Elisa's verified answer to a dependency check, from its result alone: the summary line, then each package with
/// advisories and under it each advisory (`data/check_block.txt`). Past `DEPS_LINES_MAX` lines, each package keeps
/// its `DEPS_LINES_KEPT` worst and one line says how many more (`full` shows all). None when it is no such result.
fn deps_block(result: &str, vi: bool, full: bool) -> Option<String> {
    let summary = deps_summary(result, vi)?;
    let (_, found) = deps_found(result)?;
    if found.is_empty() { return Some(summary); }
    let merged = merged_advisories(&advisory_facts(result));
    let cut = !full && merged.len() > DEPS_LINES_MAX;
    let join = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let mut lines = vec![summary];
    let mut hidden = 0;
    for ((name, ver), (count, worst, fix)) in &found {
        let fill = |t: String| t.replace("{name}", name).replace("{version}", ver).replace("{count}", &count.to_string())
            .replace("{worst}", &worst.map(|i| DEPS_LEVELS[i].to_string()).unwrap_or_else(|| block_text("no_level", vi)))
            .replace("{fix}", &fix.as_ref().map(|f| join(f)).unwrap_or_else(|| block_text("no_fix", vi)));
        lines.push(fill(block_text(if *count == 1 { "package_one" } else { "package" }, vi)));
        let mut mine: Vec<&MergedAdvisory> = merged.iter().filter(|m| m.0 == name.to_lowercase()).collect();
        // (the worst first; the check's order among the same level)
        mine.sort_by_key(|m| std::cmp::Reverse(m.2.map_or(0, |l| l + 1)));
        let shown = if cut { DEPS_LINES_KEPT.min(mine.len()) } else { mine.len() };
        hidden += mine.len() - shown;
        for m in &mine[..shown] {
            let fixed = if m.3.is_empty() { block_text("no_fix", vi) } else { m.3.iter().map(|f| join(f)).collect::<Vec<_>>().join(", ") };
            lines.push(block_text("advisory", vi).replace("{id}", &m.1).replace("{level}", &m.2.map(|i| DEPS_LEVELS[i].to_string()).unwrap_or_else(|| block_text("no_level", vi)))
                .replace("{fixed}", &fixed).replace("{title}", m.4.trim_end_matches('.')));
        }
    }
    if hidden > 0 { lines.push(block_text("more", vi).replace("{count}", &hidden.to_string())); }
    Some(lines.join("\n"))
}

/// What is left of the model's text on a dependency check, as a short note on what to do: no list lines and no line
/// naming an advisory (Elisa's list has them), no sentence with a version or a level the check does not give, and
/// at most three sentences. Empty when nothing is left.
fn deps_note(text: &str, result: &str) -> String {
    // (the versions a note on what to do may name: the ones in use and the ones to update to; each advisory's own
    // fixes are in Elisa's list)
    let join = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let held_versions: HashSet<String> = deps_found(result).map(|(_, f)| f.into_iter().flat_map(|((_, ver), (_, _, fix))| [Some(ver), fix.map(|x| join(&x))]).flatten().collect()).unwrap_or_default();
    // (a sentence Elisa's list already says, "2 of 2 packages have known security problems: …", is no note)
    let said: String = [false, true].iter().filter_map(|vi| deps_block(result, *vi, true)).collect::<Vec<_>>().join("\n");
    let held_levels: HashSet<&str> = DEPS_LEVELS.iter().copied().filter(|l| result.to_lowercase().contains(&format!("[{l}]"))).collect();
    let marked = |l: &str| l.split_whitespace().next().is_some_and(|t| ["-", "*", "•", "+"].contains(&t)
        || t.strip_suffix(['.', ')']).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())));
    let mut kept: Vec<String> = Vec::new();
    // (no code or copied tool text either: "```tool_output} requests==2.19.0 flask==0.12")
    let code = |l: &str| l.contains("```") || l.contains('{') || l.contains('}') || l.contains("==");
    for line in text.lines().map(unlabelled).filter(|l| !l.is_empty() && !marked(l) && advisory_ids(l).is_empty() && !l.ends_with(':') && !code(l)) {
        let line = line.as_str();
        for s in line.split_inclusive(". ").map(str::trim).filter(|s| !s.is_empty()) {
            let versions_ok = version_places(s).iter().all(|(at, len, _)| held_versions.contains(&s[*at..*at + *len]));
            let levels_ok = word_set(s).iter().all(|w| !DEPS_LEVELS.contains(&w.as_str()) || held_levels.contains(w.as_str()));
            // (a sentence of words: three of them at least)
            let words = s.split_whitespace().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2).count() >= 3;
            let repeated = said.contains(s.trim_end_matches('.')) || no_answer_line(s) || !gives_advice(s);
            if versions_ok && levels_ok && words && !repeated && kept.len() < 3 { kept.push(s.to_string()); }
        }
    }
    kept.join(" ")
}

/// Elisa's verified answer to one of her other security checks, from its result alone (`data/check_block.txt`):
/// the secrets scan's findings (masked as the check gives them), the binary check's and the token check's lines as
/// the check gives them under a first line of her own. None for any other tool or a result of another shape.
fn check_block(name: &str, body: &str, input: &serde_json::Value, vi: bool) -> Option<String> {
    let indent = |lines: Vec<&str>| lines.into_iter().map(|l| format!("  {}", l.trim())).collect::<Vec<_>>().join("\n");
    match name {
        "sec_secrets" => {
            let head = body.lines().next()?.strip_prefix("[secrets in ")?;
            let num = |after: &str| -> Option<usize> { head.split(after).next()?.rsplit(|c: char| !c.is_ascii_digit()).find(|t| !t.is_empty())?.parse().ok() };
            // ("… · 2 findings in 2 files]": the files are the number after the last " in ")
            let files: usize = head.rsplit(" in ").next()?.split_whitespace().next()?.parse().ok()?;
            let (scanned, hits) = (num(" text files")?, num(" finding")?);
            if hits == 0 { return Some(block_text("no_secrets", vi).replace("{scanned}", &scanned.to_string())); }
            let mut lines = vec![block_text(if hits == 1 { "secrets_head_one" } else { "secrets_head" }, vi)
                .replace("{hits}", &hits.to_string()).replace("{files}", &files.to_string()).replace("{scanned}", &scanned.to_string())];
            let mut shown = 0;
            for l in body.lines().skip(1).filter(|l| !l.starts_with('[')) {
                let Some((loc, rest)) = l.split_once(' ') else { continue };
                let Some((file, line)) = loc.rsplit_once(':') else { continue };
                let Some((rule, masked)) = rest.rsplit_once(' ') else { continue };
                lines.push(block_text("secret", vi).replace("{file}", file).replace("{line}", line).replace("{rule}", rule).replace("{masked}", masked));
                shown += 1;
            }
            if hits > shown { lines.push(block_text("secrets_more", vi).replace("{count}", &(hits - shown).to_string())); }
            Some(lines.join("\n"))
        }
        "sec_binary" => {
            let (first, rest) = body.split_once('\n')?;
            let (file, bytes) = first.strip_prefix('[')?.strip_suffix(" bytes]")?.rsplit_once(": ")?;
            let file = input["path"].as_str().unwrap_or(file);
            Some(format!("{}\n{}", block_text("binary_head", vi).replace("{file}", file).replace("{bytes}", bytes), indent(rest.lines().filter(|l| !l.trim().is_empty()).collect())))
        }
        "sec_jwt" => {
            // (its first line, that the signature is not checked, is said in Elisa's own first line)
            let rest: Vec<&str> = body.lines().skip(1).filter(|l| !l.trim().is_empty()).collect();
            (body.starts_with("JWT decoded") && !rest.is_empty()).then(|| format!("{}\n{}", block_text("jwt_head", vi), indent(rest)))
        }
        _ => None,
    }
}

/// whether a sentence gives advice on what to do (`data/advice_words.txt`): an advice wording anywhere ("You should
/// …"), or a doing word as an order at the start of the sentence or of a clause, past any leading words ("Then run
/// your tests.", "To fix this, upgrade Requests."). A doing word inside a statement of the result ("… because it
/// doesn't verify the signature", "The expiration is set to none") is no advice.
fn gives_advice(s: &str) -> bool {
    type Words = (Vec<Vec<String>>, Vec<Vec<String>>, Vec<Vec<String>>);
    static W: std::sync::OnceLock<Words> = std::sync::OnceLock::new();
    let (ask, act, lead) = W.get_or_init(|| {
        let data = include_str!("../../data/advice_words.txt");
        let keyed = |k: &str| -> Vec<Vec<String>> { data.lines().map(str::trim).filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once(':')).filter(|(x, _)| x.trim() == k).map(|(_, w)| word_set(w)).filter(|w| !w.is_empty()).collect() };
        (keyed("ask"), keyed("do"), keyed("lead"))
    });
    if holds_phrase(&word_set(s), ask) { return true; }
    let opens = |w: &[String], list: &[Vec<String>]| list.iter().filter(|p| w.starts_with(p)).map(Vec::len).max();
    s.split([',', ';', ':', '—', '(', ')']).any(|clause| {
        let mut w: &[String] = &word_set(clause);
        while let Some(n) = opens(w, lead) { w = &w[n..]; }
        opens(w, act).is_some()
    })
}

/// whether a sentence is Elisa's own line for a reply the model never gave (`data/no_answer_lines.txt`: the words
/// before the model's name or the command): no note on what to do
fn no_answer_line(s: &str) -> bool {
    include_str!("../../data/no_answer_lines.txt").lines().filter(|l| !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| l.split_once(": ")).map(|(_, t)| t.split('{').next().unwrap_or("").trim())
        .any(|lead| lead.chars().count() >= 8 && s.trim_start().starts_with(lead))
}

/// a line of the model's note without its own leading label, which Elisa's "What to do:" already gives: "**Action**:
/// Replace the token" → "Replace the token"; "Recommendation: rotate it" → "rotate it"; "**Rotate** the key" →
/// "Rotate the key". A label is a word before a colon at the line start, or up to three before a new sentence.
fn unlabelled(line: &str) -> String {
    let l = line.trim();
    // a bold first word or label: its stars go ("**Action**: …", "**Rotate** the key")
    let bold;
    let l = match l.strip_prefix("**").and_then(|r| r.find("**").map(|e| (r, e))) {
        Some((r, e)) if r[..e].split_whitespace().count() <= 3 => { bold = format!("{}{}", &r[..e], &r[e + 2..]); bold.as_str() }
        _ => l,
    };
    match l.split_once(':') {
        // (one word; or up to three before a new sentence: "Next step: Update …", never "Update flask: it is old")
        Some((label, rest)) if !rest.trim().is_empty() && label.chars().all(|c| c.is_alphabetic() || c == ' ')
            && (label.split_whitespace().count() == 1 || (label.split_whitespace().count() <= 3 && rest.trim_start().starts_with(char::is_uppercase))) => rest.trim_start().to_string(),
        _ => l.to_string(),
    }
}

/// What is left of the model's text on one of Elisa's other checks, as a short note on what to do: no list lines,
/// no code, no line naming an advisory, no sentence her result (`said`) already says, no sentence with a number or
/// a value the check's result (`result`) does not hold (a secret written out, an address made up), at most three.
fn check_note(text: &str, said: &str, result: &str) -> String {
    let marked = |l: &str| l.split_whitespace().next().is_some_and(|t| ["-", "*", "•", "+"].contains(&t)
        || t.strip_suffix(['.', ')']).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())));
    let code = |l: &str| l.contains("```") || l.contains('{') || l.contains('}') || l.contains("==");
    let mut kept: Vec<String> = Vec::new();
    for line in text.lines().map(unlabelled).filter(|l| !l.is_empty() && !marked(l) && advisory_ids(l).is_empty() && !l.ends_with(':') && !code(l)) {
        let line = line.as_str();
        for s in line.split_inclusive(". ").map(str::trim).filter(|s| !s.is_empty()) {
            let held = s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).filter(|w| w.chars().any(|c| c.is_ascii_digit())).all(|w| result.contains(w));
            let words = s.split_whitespace().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2).count() >= 3;
            let repeated = said.contains(s.trim_end_matches('.')) || no_answer_line(s) || !gives_advice(s);
            if held && words && !repeated && kept.len() < 3 { kept.push(s.to_string()); }
        }
    }
    kept.join(" ")
}

/// An ask to see the whole list Elisa cut short, by meaning (`data/full_list_asks.txt`). False without the embedding
/// model.
fn full_list_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(include_str!("../../data/full_list_asks.txt"))).as_ref().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "full")
}

/// The wordings of a data file, each as its words (`word_set`), comment lines left out.
fn phrase_list(data: &str) -> Vec<Vec<String>> {
    data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(word_set).filter(|w| !w.is_empty()).collect()
}

/// whether `words` holds one of `phrases` word for word
fn holds_phrase(words: &[String], phrases: &[Vec<String>]) -> bool {
    phrases.iter().any(|p| words.windows(p.len()).any(|x| x == p.as_slice()))
}

/// A dependency check's reply without the sentences that say a checked package (`names`) was updated
/// (`data/package_changed_words.txt`) when no change ran (`changed` false), and, when the check found advisories
/// (`found_any`), those saying none or no other were found (`data/none_found_words.txt`), those calling the packages
/// safe, and those calling a version in use (`in_use`) fixed with no other version beside it (`data/safe_words.txt`).
/// (text, any went, most went)
fn drop_wrong_status(reply: &str, names: &[String], in_use: &[String], changed: bool, found_any: bool) -> (String, bool, bool) {
    static U: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    static N: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let updated = U.get_or_init(|| phrase_list(include_str!("../../data/package_changed_words.txt")));
    let none = N.get_or_init(|| phrase_list(include_str!("../../data/none_found_words.txt")));
    static S: std::sync::OnceLock<Vec<(String, Vec<String>)>> = std::sync::OnceLock::new();
    let safe_data = S.get_or_init(|| include_str!("../../data/safe_words.txt").lines().map(str::trim).filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(':')).map(|(k, w)| (k.trim().to_string(), word_set(w))).filter(|(_, w)| !w.is_empty()).collect());
    let keyed = |k: &str| -> Vec<Vec<String>> { safe_data.iter().filter(|(x, _)| x == k).map(|(_, w)| w.clone()).collect() };
    let (safe, not, about, then, fixed, later) = (keyed("safe"), keyed("not"), keyed("about"), keyed("then"), keyed("fixed"), keyed("later"));
    // (a safe wording with no turning word up to two words before it)
    let says = |w: &[String], list: &[Vec<String>]| list.iter().any(|p| (0..w.len().saturating_sub(p.len() - 1)).any(|i| w[i..i + p.len()] == p[..]
        && !w[i.saturating_sub(2)..i].iter().any(|x| not.iter().any(|n| n.len() == 1 && n[0] == *x))));
    let calls_safe = |w: &[String]| safe.iter().any(|p| (0..w.len().saturating_sub(p.len() - 1)).any(|i| w[i..i + p.len()] == p[..]
        && !w[i.saturating_sub(2)..i].iter().any(|x| not.iter().any(|n| n.len() == 1 && n[0] == *x))));
    let mut out = String::new();
    // (whether the reply's opening sentence went: only then may what is left open on a leaning word)
    let mut deps_named = false;
    let mut opening_went = false;
    let (mut dropped, mut total, mut any) = (0, 0, false);
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        let w = word_set(piece);
        total += w.len();
        let claims_update = !changed && names.iter().any(|n| w.contains(n)) && holds_phrase(&w, updated);
        let claims_none = found_any && holds_phrase(&w, none);
        // (the packages named here or earlier in the paragraph: "… are `requests` and `flask`. These are safe to use …")
        if piece.trim().is_empty() { deps_named = false; }
        deps_named = deps_named || names.iter().any(|n| w.contains(n)) || holds_phrase(&w, &about);
        let claims_safe = found_any && deps_named && calls_safe(&w);
        // (every version in the sentence one in use: "Flask version 0.12 has been fixed for …"; "0.12 … fixed in 3.1.3" stays)
        let versions: Vec<&str> = version_places(piece).into_iter().map(|(at, len, _)| &piece[at..at + len]).collect();
        let claims_fixed = found_any && !versions.is_empty() && versions.iter().all(|v| in_use.iter().any(|u| u == v)) && says(&w, &fixed);
        // ("Flask 0.12 has advisories, but all have been addressed in later versions" while 0.12 is still affected: the
        // clause goes, or the sentence when it is all of it; release history naming another version stays)
        let other_version = versions.iter().any(|v| !in_use.iter().any(|u| u == v));
        if found_any && deps_named && !other_version && says(&w, &later) {
            let low = piece.to_lowercase();
            let at = (low.len() == piece.len()).then(|| later.iter().filter_map(|p| low.find(&p.join(" "))).min()).flatten();
            let cut = at.and_then(|a| [", ", "; "].iter().filter_map(|sep| piece[..a].rfind(sep)).max());
            any = true;
            match cut {
                Some(c) => {
                    let end = &piece[piece.trim_end().len()..];
                    dropped += word_set(&piece[c..]).len();
                    out.push_str(&format!("{}.{end}", piece[..c].trim_end_matches(['.', ','])));
                }
                None => {
                    dropped += w.len();
                    if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
                }
            }
            continue;
        }
        // (a verdict with a turn after it, "… are safe, but there are known advisories …": the turn's words stay, as
        // their own sentence)
        let kept_turn = (claims_safe && !(claims_update || claims_none || claims_fixed)).then(|| {
            let low = piece.to_lowercase();
            if low.len() != piece.len() { return None; }
            then.iter().filter_map(|t| { let k = format!(" {} ", t.join(" ")); low.find(&k).map(|p| p + k.len()) }).min()
                .map(|cut| { let tail = piece[cut..].trim_start(); let mut c = tail.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() })
                .filter(|t| !t.trim().is_empty())
        }).flatten();
        if let Some(tail) = kept_turn {
            any = true;
            dropped += w.len().saturating_sub(word_set(&tail).len());
            out.push_str(&tail);
            continue;
        }
        if claims_update || claims_none || claims_safe || claims_fixed {
            if out.trim().is_empty() { opening_went = true; }
            any = true;
            dropped += w.len();
            if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
        } else { out.push_str(piece); }
    }
    let mut out = if any { out.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n") } else { out };
    // (what is left no longer opens on a word leaning on a sentence that went: "However, there are …" → "There are …")
    if opening_went {
        let lead = out.len() - out.trim_start().len();
        let first: Vec<String> = word_set(out.trim_start()).into_iter().take(3).collect();
        if let Some(t) = then.iter().find(|t| first.len() >= t.len() && first[..t.len()] == t[..]) {
            let rest = &out[lead..];
            // (past the connector's words and the comma or space after them)
            let mut cut = 0;
            for word in t {
                let at = rest[cut..].to_lowercase().find(word.as_str()).map(|p| cut + p + word.len()).unwrap_or(cut);
                cut = at;
            }
            let tail = rest[cut..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
            let mut chars = tail.chars();
            let tail = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            out = format!("{}{tail}", &out[..lead]);
        }
    }
    (out, any, any && dropped * 2 > total)
}

/// A dependency check's reply without the sentences giving a severity score (a number from 0 to 10 near a word of
/// `data/score_words.txt`) that the check's result does not hold. (text, any went)
fn drop_unbacked_scores(reply: &str, result: &str) -> (String, bool) {
    static W: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let words = W.get_or_init(|| phrase_list(include_str!("../../data/score_words.txt")));
    let held: HashSet<&str> = result.split(|c: char| !(c.is_ascii_digit() || c == '.')).map(|t| t.trim_matches('.')).filter(|t| !t.is_empty()).collect();
    let mut out = String::new();
    let mut any = false;
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        // (each word as written, so "10.0" stays one: "2.32.4" has two dots and is a version)
        let toks: Vec<String> = piece.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '`' | ':' | ';' | '/'))
            .map(|t| t.trim_end_matches('.').to_lowercase()).filter(|t| !t.is_empty()).collect();
        let is_score = |t: &str| t.matches('.').count() <= 1 && t.chars().all(|c| c.is_ascii_digit() || c == '.') && t.parse::<f64>().is_ok_and(|v| (0.0..=10.0).contains(&v));
        let near = |i: usize| (i.saturating_sub(6)..(i + 4).min(toks.len())).any(|j| j != i && words.iter().any(|w| w.len() == 1 && w[0] == toks[j]));
        let unbacked = (0..toks.len()).any(|i| is_score(&toks[i]) && near(i) && !held.contains(toks[i].as_str()));
        if unbacked {
            any = true;
            if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
        } else { out.push_str(piece); }
    }
    let out = if any { out.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n") } else { out };
    (out, any)
}

/// A number written in digits or as a word of `data/number_words.txt` (words that are also everyday ones left out).
fn number_in(w: &str) -> Option<usize> {
    w.parse::<usize>().ok().or_else(|| NUMBER_DATA.lines().filter(|l| !l.trim_start().starts_with('#')).find_map(|l| {
        let (n, ws) = l.split_once('=')?;
        ws.split('|').map(str::trim).any(|x| !x.ends_with('?') && x == w).then(|| n.trim().parse().ok()).flatten()
    }))
}

/// A reply with each count of advisories it gives a package (a number up to three words before a wording of
/// `data/advisory_words.txt`, the package the last one named, carried on in the paragraph) made the check's count.
/// When the sentence then lists them (a ':' after the count) or the count is one either way, the sentence goes
/// instead: the list or the word's form would not hold to the new number. (text, any changed, most went)
fn fix_counts(reply: &str, counts: &[(String, usize)]) -> (String, bool, bool) {
    static W: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let nouns = W.get_or_init(|| include_str!("../../data/advisory_words.txt").lines().map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| l.split_whitespace().map(str::to_string).collect()).collect());
    let mut out = String::new();
    let (mut dropped, mut total, mut any) = (0, 0, false);
    let mut about: Option<usize> = None;
    // (a dropped count that ends its line with ':' heads a list, "Flask has two known vulnerabilities:\n1. …": its
    // list lines go with it)
    let (mut in_list, mut rest_of_line, mut line_start) = (false, false, true);
    let marked = |p: &str| p.split_whitespace().next().is_some_and(|t| ["-", "*", "•", "+"].contains(&t)
        || t.strip_suffix(['.', ')']).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())));
    for piece in reply.split_inclusive('\n').flat_map(|l| l.split_inclusive(". ")) {
        let at_start = line_start;
        line_start = piece.ends_with('\n');
        if rest_of_line || (at_start && in_list && marked(piece)) {
            rest_of_line = !piece.ends_with('\n');
            dropped += word_set(piece).len();
            total += word_set(piece).len();
            continue;
        }
        if at_start { in_list = false; }
        if piece.trim().is_empty() { about = None; out.push_str(piece); continue; }
        // (each word with its place; a version, "2.19.0", is no number)
        let toks: Vec<(usize, usize, String)> = piece.split_whitespace().map(|t| {
            let at = t.as_ptr() as usize - piece.as_ptr() as usize;
            let core = t.trim_matches(|c: char| !c.is_alphanumeric());
            let start = at + t.find(core).unwrap_or(0);
            (start, start + core.len(), core.to_lowercase())
        }).filter(|t| !t.2.is_empty()).collect();
        total += toks.len();
        let mut edits: Vec<(usize, usize, usize)> = Vec::new();
        let mut drop = false;
        for i in 0..toks.len() {
            if let Some(k) = counts.iter().position(|(p, _)| *p == toks[i].2) { about = Some(k); continue; }
            // (a number of two words first: "mười một")
            let (n, len) = match (i + 1 < toks.len()).then(|| number_in(&format!("{} {}", toks[i].2, toks[i + 1].2))).flatten() {
                Some(n) => (n, 2),
                None => match number_in(&toks[i].2) { Some(n) => (n, 1), None => continue },
            };
            let near = (i + len..(i + len + 4).min(toks.len())).any(|j| nouns.iter().any(|w| toks.len() >= j + w.len() && w.iter().zip(&toks[j..]).all(|(a, b)| *a == b.2)));
            let Some(k) = about.filter(|_| near) else { continue };
            let real = counts[k].1;
            if n == real { continue; }
            if n == 1 || real == 1 || piece[toks[i].1..].contains(':') { drop = true; break; }
            edits.push((toks[i].0, toks[i + len - 1].1, real));
        }
        if drop {
            any = true;
            dropped += toks.len();
            in_list = piece.trim_end().ends_with(':') && piece.ends_with('\n');
            if piece.ends_with('\n') && !out.ends_with('\n') { out.push('\n'); }
            continue;
        }
        let mut t = piece.to_string();
        for (a, b, real) in edits.into_iter().rev() { any = true; t.replace_range(a..b, &real.to_string()); }
        out.push_str(&t);
    }
    let out = if any { out.split('\n').map(str::trim_end).collect::<Vec<_>>().join("\n") } else { out };
    (out, any, any && dropped * 2 > total)
}

/// Elisa's own per-package list of a check's advisories: "flask:\n- CVE-2018-1000656 (high): fixed in 0.12.3".
fn advisory_list(advs: &[Advisory], vi: bool) -> String {
    let mut pkgs: Vec<&str> = Vec::new();
    for a in advs { if !pkgs.contains(&a.pkg.as_str()) { pkgs.push(&a.pkg); } }
    pkgs.iter().map(|p| {
        let rows: Vec<String> = advs.iter().filter(|a| a.pkg == *p).map(|a| {
            // (the CVE id when there is one: the name people know it by)
            let id = a.ids.iter().find(|i| i.starts_with("CVE-")).unwrap_or(&a.ids[0]);
            let level = a.level.as_ref().map(|l| format!(" ({l})")).unwrap_or_default();
            let fixed = a.fixed.iter().map(|v| v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")).collect::<Vec<_>>().join(", ");
            let fixed = if fixed.is_empty() { String::new() } else if vi { format!(": đã sửa ở {fixed}") } else { format!(": fixed in {fixed}") };
            format!("- {id}{level}{fixed}")
        }).collect();
        format!("{p}:\n{}", rows.join("\n"))
    }).collect::<Vec<_>>().join("\n")
}

/// `text` without each aside in brackets whose only version is `fix` when the text names `fix` outside it too
/// ("Upgrade to Flask ≥ 3.1.3 (or at least 3.1.3 for the earliest patch)").
fn without_same_version_aside(text: &str, fix: &[u64]) -> String {
    let f = fix.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let mut t = text.to_string();
    let mut from = 0;
    while let Some((open, close)) = t[from..].find('(').map(|o| from + o).and_then(|o| t[o..].find(')').map(|c| (o, o + c + 1))) {
        let inside = &t[open..close];
        let outside = format!("{}{}", &t[..open], &t[close..]);
        let vs = version_places(inside);
        if !vs.is_empty() && vs.iter().all(|v| v.2 == fix) && outside.contains(f.as_str()) {
            let start = if t[..open].ends_with(' ') { open - 1 } else { open };
            t.replace_range(start..close, "");
            from = start;
        } else { from = close; }
    }
    t
}

/// The version numbers in a text with their places: (byte offset, length, numbers).
fn version_places(text: &str) -> Vec<(usize, usize, Vec<u64>)> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = text.as_bytes();
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-' || b[i - 1] == b'.')) { i += 1; continue; }
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') { j += 1; }
        let tok = text[i..j].trim_end_matches('.');
        let v: Option<Vec<u64>> = tok.split('.').map(|p| p.parse().ok()).collect();
        if let Some(v) = v.filter(|v| v.len() >= 2 && v.len() <= 4) { out.push((i, tok.len(), v)); }
        i = j;
    }
    out
}

const VALUE_DATA: &str = include_str!("../../data/value_asks.txt");

/// An ask for the value of one setting of a file, by meaning (`data/value_asks.txt`), with the setting's name put as
/// "foo" and the file's as "the file". False without the embedding model.
fn value_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(VALUE_DATA)).as_ref().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "value")
}

/// Example data for `exists_ask`: an `[exists]` section and an `[other]` section, file names written as "the file".
const EXISTS_DATA: &str = include_str!("../../data/exists_asks.txt");

/// A yes/no question whether a file is (still) there, by meaning (`data/exists_asks.txt`), with the file's name
/// already put as "the file". False without the embedding model.
fn exists_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(EXISTS_DATA)).as_ref().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "exists")
}

/// Example data for `read_ask`: a `[read]` section and an `[other]` section, file names written as "the file".
const READ_DATA: &str = include_str!("../../data/read_asks.txt");

/// An ask to read a file of the project and answer from it, by meaning (`data/read_asks.txt`: "đọc file the file và
/// cho mình biết mã cuộc họp", "lemme see whats in the file"), with the file's name already put as "the file". False
/// without the embedding model.
fn read_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(READ_DATA)).as_ref().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "read")
}

/// Example data for `next_item_ask`: a `[next]` section and an `[other]` section.
const HELD_DATA: &str = include_str!("../../data/held_asks.txt");

/// A short follow-up asking for the one item not named yet of a file Elisa holds ("the other one?", "và cái kia?"),
/// by meaning (`data/held_asks.txt`). False without the embedding model.
fn next_item_ask(msg: &str) -> bool {
    static EX: std::sync::OnceLock<Option<Examples>> = std::sync::OnceLock::new();
    EX.get_or_init(|| load_examples(HELD_DATA)).as_ref().and_then(|ex| nearest_section(ex, msg)).is_some_and(|s| s.0 == "next")
}

/// most words a follow-up Elisa answers from a held file may have (`held_line`), and a repeat ask (`repeat_asked`)
const HELD_FOLLOW_UP_WORDS: usize = 6;
const REPEAT_WORDS: usize = 6;
/// most words beside the row's name a follow-up may have for Elisa to answer it before the model ("and South?",
/// "what about East?": `held_line`)
const HELD_FRAME_WORDS: usize = 2;

/// The rows of a held file, (key, the rest) per line: a sheet's rows as their label and the other cells with the
/// header's names ("South" → "340 units"), a "key: value" line as its two sides, any other line as its own key.
pub(super) fn held_rows(path: &str, text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if crate::mcp_docs::is_sheet(Path::new(path)) {
        let Some((head, rows)) = lines.split_first() else { return Vec::new() };
        let sep = [',', '\t', ';'].into_iter().max_by_key(|s| head.matches(*s).count()).unwrap_or(',');
        let header: Vec<&str> = head.split(sep).map(str::trim).collect();
        return rows.iter().filter_map(|r| {
            let cells: Vec<&str> = r.split(sep).map(str::trim).collect();
            let (key, rest) = cells.split_first()?;
            let rest = rest.iter().enumerate().map(|(i, v)| match header.get(i + 1) { Some(h) if !h.is_empty() => format!("{v} {h}"), _ => v.to_string() }).collect::<Vec<_>>().join(", ");
            (!key.is_empty()).then(|| (key.to_string(), rest))
        }).collect();
    }
    lines.iter().map(|l| match l.split_once(": ") {
        Some((k, v)) if !k.is_empty() && !v.trim().is_empty() => (k.trim().to_string(), v.trim().to_string()),
        _ => (l.trim_end_matches('.').to_string(), String::new()),
    }).collect()
}

/// Two calls of the same tool for the same thing: equal arguments, or the same first text argument (the router's
/// search and the words' search differ only in a switch).
fn same_call(a: &(String, serde_json::Value), b: &(String, serde_json::Value)) -> bool {
    if a.0 != b.0 { return false; }
    if a.1 == b.1 { return true; }
    let first = |v: &serde_json::Value| v.as_object().and_then(|o| o.values().find_map(|x| x.as_str().map(str::to_string)));
    first(&a.1).is_some() && first(&a.1) == first(&b.1)
}

/// Counting wordings, one section per unit (`data/count_asks.txt`)
const COUNT_DATA: &str = include_str!("../../data/count_asks.txt");

/// The units a message asks a count of ("how many lines", "đếm số dòng", "số từ"): the English count words
/// (`mcp_reason::count_units`) and the sections of `data/count_asks.txt` whose wording it holds as whole words.
/// Empty when it asks none.
pub(super) fn asked_counts(body: &str) -> Vec<&'static str> {
    static W: std::sync::OnceLock<Vec<(&'static str, Vec<&'static str>)>> = std::sync::OnceLock::new();
    let sections = W.get_or_init(|| {
        let mut out: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
        for l in COUNT_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
                Some(h) => out.push((h, Vec::new())),
                None => if let Some(s) = out.last_mut() { s.1.push(l); },
            }
        }
        out
    });
    let low = body.to_lowercase();
    let mut units = crate::mcp_reason::count_units(body);
    for (unit, ws) in sections {
        if !units.contains(unit) && ws.iter().any(|w| has_words(&low, &w.to_lowercase())) { units.push(unit); }
    }
    units
}

/// Whether `low` holds the phrase `p` as whole words.
fn has_words(low: &str, p: &str) -> bool {
    low.match_indices(p).any(|(i, _)| low[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) && low[i + p.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// Elisa's count of `text` in the units asked, as a line: "todo.txt has 2 lines." / "todo.txt có 2 dòng.".
pub(super) fn count_text(file: &str, text: &str, units: &[&str], vi: bool) -> String {
    let n = |u: &str| match u { "words" => text.split_whitespace().count(), "lines" => text.lines().count(), _ => text.chars().count() };
    let list = units.iter().map(|u| {
        let k = n(u);
        if vi { format!("{k} {}", match *u { "words" => "từ", "lines" => "dòng", _ => "ký tự" }) } else { format!("{k} {}", if k == 1 { u.trim_end_matches('s') } else { u }) }
    }).collect::<Vec<_>>().join(", ");
    if vi { format!("{file} có {list}.") } else { format!("{file} has {list}.") }
}

/// The file a save names after a word leading to it, where the English word rules see no save (`route::save_ask`): a
/// saving word of `data/given_text_asks.txt`'s examples ("lưu", "ghi", "jot", "thêm": `save_verbs`), and after it one
/// of the words those examples put before "the file" ("to", "into", "in", "vào": `save_frame_words`) with a file name
/// in the next few words ("lưu một dòng tóm tắt notes.txt vào file tomtat.md" → tomtat.md, never notes.txt; "jot down
/// 'call Minh at 3pm' in reminders.txt" → reminders.txt). None otherwise.
fn save_target(body: &str) -> Option<String> {
    let toks: Vec<&str> = body.split_whitespace().collect();
    let low: Vec<String> = toks.iter().map(|t| frame_word(t)).collect();
    let verbs = save_verbs();
    let at = low.iter().position(|w| verbs.contains(w))?;
    let leads = save_frame_words();
    (at + 1..low.len()).filter(|&i| leads.contains(&low[i]) && !verbs.contains(&low[i]))
        .find_map(|i| toks[i + 1..].iter().take(SAVE_TARGET_WORDS).find_map(|t| crate::route::file_name(t)))
}

/// most words between the word leading to a save's file and the file's name ("vào file tomtat.md", "to the file x.md")
const SAVE_TARGET_WORDS: usize = 3;

/// The values of the one column of a table whose header the words name ("the three region names" → the "region"
/// column; "regions" names it too: `same_word`, or the same word but for an "s"), without the header, one per line.
/// None when the words name no one column, or it is empty.
fn exported_column(ask: &str, rows: &[Vec<String>]) -> Option<String> {
    let (head, data) = rows.split_first()?;
    let words = word_set(ask);
    let same = |w: &str, x: &str| same_word(w, x) || (w.len() >= 3 && w.trim_end_matches('s') == x.trim_end_matches('s'));
    let cols: Vec<usize> = head.iter().enumerate()
        .filter(|(_, h)| { let hw = word_set(h); !hw.is_empty() && hw.iter().all(|w| words.iter().any(|x| same(w, x))) }).map(|(i, _)| i).collect();
    let [col] = cols.as_slice() else { return None };
    let values: Vec<&str> = data.iter().filter_map(|r| r.get(*col)).map(|v| v.trim()).filter(|v| !v.is_empty()).collect();
    (!values.is_empty()).then(|| values.iter().map(|v| format!("{v}\n")).collect())
}

/// The word a search of the project's files asks for ("tìm giúp mình file nào có chữ ORCHID", "tim giup minh file nao
/// co chu ORCHID", "which file mentions ORCHID?", "search the project for the word Hello"), by the `[grep]` frames of
/// `data/own_tools.txt`: the one quoted part, else the word that stands out (`search_term`: capitalised), where the
/// message with that word put as "foo" is near the frames in meaning (`meant_tool`). None otherwise: a web search
/// names no file, and a statement asks nothing.
fn search_ask(body: &str) -> Option<String> {
    let (term, framed) = match quoted_parts(body).as_slice() {
        [q] => (q.clone(), body.replace(q.as_str(), "foo")),
        [] => {
            let t = search_term(body, false)?;
            (t.clone(), body.split_whitespace().map(|w| if w.trim_matches(|c: char| !c.is_alphanumeric()) == t { "foo" } else { w }).collect::<Vec<_>>().join(" "))
        }
        _ => return None,
    };
    (meant_tool(&framed).is_some_and(|(n, _)| n == "grep")).then_some(term)
}

/// The thing a search of the project's files asks for, as the user wrote it (`search_ask`, `route::which_file_term`):
/// what Elisa's own answer names, never the grep pattern made from it.
fn searched_words(body: &str) -> Option<String> {
    search_ask(body).or_else(|| crate::route::which_file_term(body))
}

/// An ask for the file that holds something (`route::which_file_term`: "tim giup minh file nao co chu ORCHID") whose
/// reply is clearly in the other language than the ask (`reply_lang::detect`, Vietnamese with or without its marks):
/// the reply does not answer in the user's words, so Elisa's own line names the file. False for any other ask or reply.
fn which_file_reply_falls_short(body: &str, reply: &str) -> bool {
    use crate::reply_lang::{detect, Lang};
    if crate::route::which_file_term(body).is_none() { return false; }
    let ask = if crate::route::vietnamese(body) { Lang::Vi } else { Lang::En };
    detect(reply).is_some_and(|l| l != ask)
}

/// A grep `pattern` that is, once its case switch and escapes are off, words the user wrote in `body` ("Hello",
/// "(?i)ORCHID\-42"): those words. None for a pattern with any other regex syntax ("meeting.*code").
fn pattern_in_words(pattern: &str, body: &str) -> Option<String> {
    let plain = pattern.strip_prefix("(?i)").unwrap_or(pattern).replace('\\', "");
    (!plain.trim().is_empty() && body.to_lowercase().contains(&plain.to_lowercase())).then_some(plain)
}

/// The grep pattern of a search of the project's files the words ask for: `search_ask`'s word, else the thing a
/// which-file ask names (`route::which_file_term`: "a meeting code" → any case of "meeting code"). None otherwise.
fn search_pattern(body: &str) -> Option<String> {
    search_ask(body).map(|t| regex::escape(&t)).or_else(|| crate::route::which_file_term(body).map(|t| format!("(?i){}", regex::escape(&t))))
}

/// Phrases around the thing a web search ask looks up (`data/web_query_frames.txt`)
const QUERY_FRAME_DATA: &str = include_str!("../../data/web_query_frames.txt");

/// A web search ask without its frame (`data/web_query_frames.txt`): a phrase of the frames found as whole words
/// starting in the message's first half (the latest-ending, then the longest; one of a single word only at the very
/// start) is cut with everything before it, one found at its end is cut (the longest), again until none is left ("can
/// u serach the web for who won the world cup in 2022" → "who won the world cup in 2022", "thủ đô của Úc là gì? tra
/// google giúp mình" → "thủ đô của Úc"). The message as it was when nothing would be left.
fn strip_query_frames(body: &str) -> String {
    static FRAMES: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let frames = FRAMES.get_or_init(|| QUERY_FRAME_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().map(frame_word).filter(|w| !w.is_empty()).collect::<Vec<_>>()).filter(|f| !f.is_empty()).collect());
    let mut toks: Vec<&str> = body.split_whitespace().collect();
    loop {
        let low: Vec<String> = toks.iter().map(|t| frame_word(t)).collect();
        let n = low.len();
        // the end first ("… là gì? tra google giúp mình": each tail in turn), then a lead-in
        if let Some(k) = frames.iter().filter(|f| f.len() < n && low[n - f.len()..] == f[..]).map(|f| f.len()).max() { toks.truncate(n - k); continue; }
        let lead = frames.iter().filter_map(|f| {
            let starts = (0..=n.checked_sub(f.len() + 1)?).filter(|&i| 2 * i < n && (f.len() > 1 || i == 0) && low[i..i + f.len()] == f[..]);
            starts.max().map(|i| (i + f.len(), f.len()))
        }).max();
        match lead {
            Some((end, _)) => { toks.drain(..end); }
            None => break,
        }
    }
    if toks.is_empty() { body.trim().to_string() } else { toks.join(" ") }
}

/// Wordings of an ask about free disk space (`data/space_asks.txt`)
const SPACE_DATA: &str = include_str!("../../data/space_asks.txt");

/// Whether words ask about free disk space ("how much disk space is free here?", "ổ đĩa còn trống bao nhiêu?"), by
/// the wordings of `data/space_asks.txt` as whole words.
fn space_asked(body: &str) -> bool {
    let low = body.to_lowercase();
    SPACE_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).any(|w| has_words(&low, &w.to_lowercase()))
}

/// Elisa's answer to an ask about free disk space (`space_asked`) whose own command was `df`: the free space of the
/// root file system, else of the one `cwd` is on, from df's table in words ("14 GB free on /." / "Còn trống 14 GB trên
/// /."), with or without `-h` (its "Avail" column: blocks of the header's size, or "13Gi"). None when the words ask no
/// such thing, the command was another, or the table has no such row.
fn free_space_text(body: &str, command: &str, output: &str, cwd: &Path) -> Option<String> {
    if command.split_whitespace().next() != Some("df") || !space_asked(body) { return None; }
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let (head, rows) = lines.split_first()?;
    let cols: Vec<&str> = head.split_whitespace().collect();
    let avail = cols.iter().position(|c| c.to_lowercase().starts_with("avail"))?;
    // the numbers' unit: blocks of the header's size ("512-blocks", "1K-blocks"), else written with their unit ("13Gi")
    let block = cols.get(1).filter(|c| c.contains("blocks")).and_then(|c| c.split('-').next()).and_then(human_bytes);
    let parsed: Vec<(Vec<&str>, &str)> = rows.iter().map(|r| { let t: Vec<&str> = r.split_whitespace().collect(); let m = t.last().copied().unwrap_or(""); (t, m) }).collect();
    let here = cwd.to_string_lossy();
    let (cells, mount) = parsed.iter().find(|(_, m)| *m == "/")
        .or_else(|| parsed.iter().filter(|(_, m)| m.starts_with('/') && here.starts_with(*m)).max_by_key(|(_, m)| m.len()))?;
    let raw = cells.get(avail)?;
    let bytes = match block { Some(b) => raw.parse::<f64>().ok()? * b, None => human_bytes(raw)? };
    let gb = bytes / 1e9;
    let n = if gb >= 10.0 { format!("{}", gb.round() as u64) } else { format!("{gb:.1}") };
    Some(if crate::route::vietnamese(body) { format!("Còn trống {n} GB trên {mount}.") } else { format!("{n} GB free on {mount}.") })
}

/// Bytes of a size written as a number with an optional unit ("512" → 512, "1K" → 1024, "13Gi" → 13 × 2^30, "1.5T").
/// None for anything else.
fn human_bytes(s: &str) -> Option<f64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f64 = digits.parse().ok()?;
    let unit = s[digits.len()..].trim_end_matches(['i', 'B', 'b']).to_ascii_uppercase();
    let p = match unit.as_str() { "" => 0, "K" => 1, "M" => 2, "G" => 3, "T" => 4, "P" => 5, _ => return None };
    Some(n * 1024f64.powi(p))
}

/// Asks for one item of a list by its place (`data/item_asks.txt`)
const ITEM_DATA: &str = include_str!("../../data/item_asks.txt");

/// The place of the one item of a list a message asks for ("the first item", "việc đầu tiên là gì?", "the last
/// one?"), by the wordings of `data/item_asks.txt` as whole words: (the place, 1 the first and -1 the last; its name
/// in English; in Vietnamese). None when the words name no place.
fn asked_item(body: &str) -> Option<(i64, &'static str, &'static str)> {
    type Section = (i64, &'static str, &'static str, Vec<&'static str>);
    static S: std::sync::OnceLock<Vec<Section>> = std::sync::OnceLock::new();
    let secs = S.get_or_init(|| {
        let mut out: Vec<Section> = Vec::new();
        for l in ITEM_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
                Some(h) => {
                    let p: Vec<&str> = h.split('|').map(str::trim).collect();
                    if let [n, en, vi] = p.as_slice() { if let Ok(k) = n.parse::<i64>() { out.push((k, *en, *vi, Vec::new())); } }
                }
                None => if let Some(s) = out.last_mut() { s.3.push(l); },
            }
        }
        out
    });
    let low = body.to_lowercase();
    secs.iter().find(|s| s.3.iter().any(|w| has_words(&low, &w.to_lowercase()))).map(|s| (s.0, s.1, s.2))
}

/// The item at a place of a list's rows (`held_rows`): 1 the first, -1 the last. None when the list is shorter.
fn item_of(rows: &[(String, String)], place: i64) -> Option<&(String, String)> {
    match place.signum() {
        1 => rows.get(place as usize - 1),
        -1 => rows.len().checked_sub(place.unsigned_abs() as usize).and_then(|i| rows.get(i)),
        _ => None,
    }
}

/// Elisa's line for the item a message asks for by its place (`asked_item`) of a held file's rows: "The first item
/// in todo.txt is buy milk." / "Mục đầu tiên trong todo.txt là buy milk." (a sheet's row with its cells: "South: 340
/// units"). None when the words name no place or the list has no such item.
fn item_line(body: &str, path: &str, rows: &[(String, String)]) -> Option<String> {
    let (place, en, vi) = asked_item(body)?;
    let (k, rest) = item_of(rows, place)?;
    let item = if rest.is_empty() { k.clone() } else { format!("{k}: {rest}") };
    Some(if crate::route::vietnamese(body) { format!("Mục {vi} trong {path} là {item}.") } else { format!("The {en} item in {path} is {item}.") })
}

/// Words that give the user's question back instead of an answer ("Dân số Iceland hiện tại là bao nhiêu?" to "dân số
/// Iceland là bao nhiêu?": a result's title): they end with a question mark, or hold no figure and no content word the
/// ask has not (`topics::words`).
fn question_back(t: &str, ask: &str) -> bool {
    let t = t.trim();
    if t.ends_with('?') { return true; }
    if has_figure(t) { return false; }
    let a = crate::topics::words(context::request_body(ask));
    let w = crate::topics::words(t);
    !w.is_empty() && w.iter().all(|x| a.iter().any(|y| y.starts_with(x.as_str()) || x.starts_with(y.as_str())))
}

/// Whether words hold a figure: a run of two digits or more ("393.600", "2024"; not a list's "1.").
fn has_figure(t: &str) -> bool {
    t.split(|c: char| !c.is_ascii_digit()).any(|d| d.len() >= 2)
}

/// The sentences of a line: cut after a full stop, an exclamation or a question mark that a space or the end follows
/// ("393.600" and "1.5" stay whole).
fn split_sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    for (i, &(at, c)) in cs.iter().enumerate() {
        if matches!(c, '.' | '!' | '?') && cs.get(i + 1).is_none_or(|n| n.1.is_whitespace()) {
            let end = at + c.len_utf8();
            if !line[start..end].trim().is_empty() { out.push(&line[start..end]); }
            start = end;
        }
    }
    if !line[start..].trim().is_empty() { out.push(&line[start..]); }
    out
}

/// The sentence of web results that gives a figure for an ask that wants one (`reply_check::asks_number`: "how
/// many", "bao nhiêu"): among the snippet lines (no title line of the list, no URL), the sentences holding a figure
/// (`has_figure`) and not ending in a question mark, the one sharing the most content words with the ask (the first
/// of equals), with a full stop. None when the ask wants no figure or no sentence holds one.
fn figure_sentence(ask: &str, web: &str) -> Option<String> {
    if !crate::reply_check::asks_number(ask) { return None; }
    let a = crate::topics::words(context::request_body(ask));
    let mut best: Option<(usize, String)> = None;
    for l in web.lines().map(str::trim) {
        if l.is_empty() || l.contains("://") || !web_titles(l).is_empty() || l.starts_with('<') || l.starts_with('[') { continue; }
        for s in split_sentences(l) {
            let s = s.trim();
            if s.ends_with('?') || !has_figure(s) { continue; }
            let w = crate::topics::words(s);
            let hits = a.iter().filter(|x| w.iter().any(|y| y.starts_with(x.as_str()))).count();
            if hits > 0 && best.as_ref().is_none_or(|b| hits > b.0) { best = Some((hits, s.to_string())); }
        }
    }
    best.map(|(_, s)| if s.ends_with(['.', '!']) { s } else { format!("{s}.") })
}

/// the reading words of the short `[read]` examples of `data/read_asks.txt` ("đọc the file", "read the file": "đọc",
/// "read"), as `frame_word` spells them
fn read_verbs() -> &'static HashSet<String> {
    static W: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let mut inside = false;
        let mut out = HashSet::new();
        for l in READ_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { inside = l == "[read]"; continue; }
            let w: Vec<String> = l.split_whitespace().map(frame_word).collect();
            if inside && w.len() <= 3 && w.ends_with(&["the".to_string(), "file".to_string()]) { out.insert(w[0].clone()); }
        }
        out
    })
}

/// The file a bare read ask names ("đọc todo.txt", "mở notes.txt", "mo todo.txt", "read todo.txt"): a reading word of
/// the short examples (`read_verbs`) and the file's name, nothing else (one word between at most: "đọc file
/// todo.txt"), in any spelling and with no meaning measured. None otherwise.
fn bare_read(body: &str) -> Option<String> {
    let words: Vec<&str> = body.split_whitespace().collect();
    if !(2..=3).contains(&words.len()) || !read_verbs().contains(&frame_word(words[0])) { return None; }
    let paths = path_tokens(body);
    let [f] = paths.as_slice() else { return None };
    let last = words.last()?.trim_matches(|c: char| "\"'`<>()[],;:?!".contains(c)).trim_end_matches('.');
    (last == f).then(|| f.clone())
}

/// A continued part without the request's own note (`CONTINUE_NOTE`) where the model wrote it back: each copy of
/// it taken out, the rest as written.
fn strip_continue_note(part: &str) -> String {
    if !part.contains(CONTINUE_NOTE) { return part.to_string(); }
    let out = part.replace(CONTINUE_NOTE, "");
    // (the note stood alone at the start: the space or line after it goes with it)
    if part.trim_start().starts_with(CONTINUE_NOTE) { out.trim_start().to_string() } else { out }
}

/// the user note of a request that continues a reply cut by the output limit (`continue_reply`), in that request only
const CONTINUE_NOTE: &str = "Continue exactly where you stopped.";
/// continuations of one cut reply at most
const CONTINUE_MAX: usize = 2;

#[cfg(test)]
mod own_tests {
    #[test]
    fn a_write_claim_clause_beside_a_denial_is_found() {
        use super::super::docs::clause_claims;
        let v = "I saved a note called .elisa/b9.txt, yet nothing could go in it: no text was put there. Shall we retry?";
        let c = clause_claims(v, &["write"], true);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(c[0].1.starts_with("I saved a note called .elisa/b9.txt") && c[0].1.ends_with("put there."), "{c:?}");
        // a plain denial, no claim in any clause: none
        assert!(clause_claims("I couldn't save .elisa/b9.txt, nothing was written.", &["write"], true).is_empty());
    }

    #[test]
    fn a_later_fold_keeps_its_newest_turns_in_the_digest() {
        use crate::types::{Message, Role, Block};
        let old = "## Session goal\nthe user said: \"tell me a short joke\"\n\n## Facts to remember\n- The user's name is Linh\n- The user lives in Hanoi\nEarlier turns (shortened, no summary):\n- User: hi Elisa!\n- Elisa: Hi! What can I do for you?\n- User: what colour are the insides of its ears?\n- Elisa: Blue\n";
        let msgs = vec![Message::user_text("open plan.txt and tell me the gate code"),
            Message::new(Role::Assistant, vec![Block::Text { text: "KESTREL-9".into() }]),
            Message::user_text("what is the capital of Peru?")];
        let merged = crate::context::merge_notes(Some(old), &crate::context::digest(&msgs, None, 600));
        let cut = crate::context::cap_notes_priority(&merged, crate::context::estimate_tokens(&merged) - 8);
        // the oldest turn goes first; the newest, and the facts, stay
        assert!(cut.contains("KESTREL-9") && cut.contains("capital of Peru"), "{cut}");
        assert!(!cut.contains("hi Elisa!") && cut.contains("The user's name is Linh"), "{cut}");
    }

    #[test]
    fn a_status_the_file_does_not_give_is_seen() {
        use super::super::docs::status_unheld;
        let held = "1\t- water plants\n2\t- post letter";
        assert!(status_unheld("Danh sách có những việc đã gửi đi: tưới cây và gửi thư.", held));
        assert!(status_unheld("Both tasks are already finished.", held));
        // the items as they are, or a status the file holds: none
        assert!(!status_unheld("Danh sách có: tưới cây, gửi thư.", held));
        assert!(!status_unheld("It lists two tasks: water plants and post letter.", held));
        assert!(!status_unheld("The first task is done.", "1\t- [x] water plants (done)"));
    }

    #[test]
    fn a_question_asked_again_gets_the_earlier_answer() {
        use super::super::docs::{earlier_answer_of, exchanges};
        let notes = "## Facts to remember\n- The user's name is Linh\nEarlier turns (shortened, no summary):\n- User: open plan.txt and tell me the gate code\n- Elisa: KESTREL-9\n- User: what is the capital of Peru?\n- Elisa: Lima\n";
        let past = exchanges(notes, &[("tóm tắt plan.txt giúp mình".to_string(), "Mã cổng là KESTREL-9.".to_string())]);
        let l = earlier_answer_of("which gate code did you find earlier?", &past, "tiny:1b").unwrap();
        assert!(l.contains("KESTREL-9") && l.contains("open plan.txt"), "{l}");
        // nothing asked before with its words: none
        assert!(earlier_answer_of("what is the weather in Lima tomorrow?", &past, "tiny:1b").is_none());
        // an earlier no-answer line is no answer
        let past = exchanges("", &[("open plan.txt and tell me the gate code".to_string(), crate::reply_check::cant_answer_line("open plan.txt and tell me the gate code", "tiny:1b"))]);
        assert!(earlier_answer_of("which gate code did you find earlier?", &past, "tiny:1b").is_none());
        // another ask sharing words, with a path of its own: no earlier answer stands for it
        let past = exchanges("", &[("with the shell make the file /srv/a1.txt holding the word hi".to_string(), "Done, the file holds hi.".to_string())]);
        assert!(earlier_answer_of("with the shell make the file /var/b2.txt holding the word yo, then print it", &past, "tiny:1b").is_none());
        assert!(earlier_answer_of("make the file holding the word", &past, "tiny:1b").is_some());
        // an earlier reply that was Elisa's own not-written line is no answer
        let nw = crate::reply_check::own_line_of("not_written", "write it", "a1.txt");
        let past = exchanges("", &[("with the shell make the file holding the word hi".to_string(), nw)]);
        assert!(earlier_answer_of("make the file holding the word hi", &past, "tiny:1b").is_none());
    }

    use super::*;

    #[test]
    fn quoted_parts_take_quotes_not_apostrophes() {
        assert_eq!(quoted_parts("in todo.txt change 'buy milk' to 'buy bread'"), vec!["buy milk", "buy bread"]);
        assert_eq!(quoted_parts("don't change \"a b\" to “c”"), vec!["a b", "c"]);
        assert!(quoted_parts("it's Bob's file").is_empty());
    }

    /// a change of plain words in a file, in either order and in Vietnamese, by the frames of `data/edit_asks.txt`
    #[test]
    fn a_change_of_plain_words_is_read_from_its_frame() {
        let is_file = |p: &str| matches!(p, "e1.txt" | "notes/a.md");
        let f = |m: &str| framed_edit(m, &is_file);
        let want = |a: &str, b: &str, c: &str| Some((a.to_string(), b.to_string(), c.to_string()));
        assert_eq!(f("in e1.txt change hello to goodbye"), want("e1.txt", "hello", "goodbye"));
        assert_eq!(f("change hello to goodbye in e1.txt"), want("e1.txt", "hello", "goodbye"));
        assert_eq!(f("please replace buy milk with buy bread in notes/a.md."), want("notes/a.md", "buy milk", "buy bread"));
        assert_eq!(f("In e1.txt, change hello to goodbye"), want("e1.txt", "hello", "goodbye"));
        assert_eq!(f("trong e1.txt đổi hello thành goodbye"), want("e1.txt", "hello", "goodbye"));
        assert_eq!(f("thay xin chào bằng tạm biệt trong e1.txt"), want("e1.txt", "xin chào", "tạm biệt"));
        // no file named, a file that is not there, no frame
        assert_eq!(f("change hello to goodbye"), None);
        assert_eq!(f("in gone.txt change hello to goodbye"), None);
        assert_eq!(f("what does e1.txt say?"), None);
    }

    #[test]
    fn change_asks_are_read_from_the_words() {
        assert!(asks_change("in todo.txt change 'buy milk' to 'buy bread'"));
        assert!(asks_change("replace 'a' with 'b' in notes.txt"));
        assert!(!asks_change("does todo.txt say 'buy milk' or 'buy bread'?"));
    }

    #[test]
    fn file_content_must_be_words_and_keep_the_file() {
        assert!(!usable_content("[0.0, 0.68, 0.99, 0.77]"));
        assert!(usable_content("A plan for the bakery"));
        assert!(!usable_content("1. to"));
        assert!(!adds_words("marketing-plan.md\n", "marketing-plan.md"));
        assert!(adds_words("Bake fresh bread daily", "plan.md"));
        assert!(!keeps_most("- buy milk\n- send report\n", "state:\n"));
        assert!(keeps_most("- buy milk\n- send report\n", "- buy bread\n- send report\n"));
    }

    #[test]
    fn a_word_is_no_command_and_a_mention_is_no_read() {
        if dictionary().is_empty() { return; }
        assert!(in_dictionary("units") && in_dictionary("unit"));
        assert!(!in_dictionary("wc") && !in_dictionary("rm"));
        assert!(asks_about_file("which region sold the most units in sales.csv?"));
        assert!(asks_about_file("the add function in app.py subtracts; fix it"));
        assert!(!asks_about_file("notes.txt is where I keep my ideas"));
    }

    #[test]
    fn a_call_written_as_json_gives_its_arguments() {
        let raw = r#"[{"name":"edit","arguments":{"path":"app.py","old":"return a - b","new":"def add(a, b):\n    return a + b"},"line":"2"}]"#;
        let (n, a) = loose_call(raw).unwrap();
        assert_eq!((n.as_str(), a["path"].as_str()), ("edit", Some("app.py")));
        assert!(loose_call("Here is the fix.").is_none());
        assert_eq!(unnumbered("1\tMeeting code: X\n2\tProject: Y\n"), "Meeting code: X\nProject: Y\n");
        assert_eq!(unnumbered("Step 1\tgo\n"), "Step 1\tgo\n");
    }

    #[test]
    fn a_continued_part_never_keeps_the_continue_note() {
        assert_eq!(strip_continue_note(&format!("{CONTINUE_NOTE}\nand the rest.")), "and the rest.");
        assert_eq!(strip_continue_note(&format!(" more words {CONTINUE_NOTE} end")), " more words  end");
        assert_eq!(strip_continue_note(" plain part"), " plain part");
    }

    #[test]
    fn file_text_never_copies_elisas_own_text() {
        let own = format!("{}\n{}", context::TINY_SYSTEM, crate::situation::model_facts("gemma3:1b", false, false, false, 16384));
        let ask = "save the count to count.txt";
        assert!(copies_own("8\nModel facts (detected, true now): Elisa runs on gemma3:1b. Write every answer fresh, never copy an earlier one.", "", ask, &own));
        assert!(copies_own("[elisa: counted exactly: notes.txt has 8 words; use this number]", "", ask, &own));
        assert!(!copies_own("8\n", "", ask, &own));
        assert!(!copies_own("A friendly bakery plan: fresh bread every morning.", "", ask, &own));
        // text the file already had is no copy
        assert!(!copies_own("Answer briefly and naturally.", "Answer briefly and naturally.", ask, &own));
    }

    #[test]
    fn numbered_code_loses_its_numbers() {
        let file = "def add(a, b):\n    return a - b\n";
        assert_eq!(code_unnumbered("1 def add(a, b):\n2     return a + b\n", file), "def add(a, b):\n    return a + b\n");
        assert_eq!(code_unnumbered("1: def add(a, b):\n2:     return a + b", file), "def add(a, b):\n    return a + b");
        assert_eq!(code_unnumbered("1\tdef add(a, b):\n\n3\t    return a + b", file), "def add(a, b):\n\n    return a + b");
        // (code as written, a numbered list in the file itself, numbers that do not count up: left alone)
        assert_eq!(code_unnumbered("def add(a, b):\n    return a + b\n", file), "def add(a, b):\n    return a + b\n");
        assert_eq!(code_unnumbered("1. milk\n2. bread\n", "1. milk\n2. eggs\n"), "1. milk\n2. bread\n");
        assert_eq!(code_unnumbered("10 x = 1\n3 y = 2", file), "10 x = 1\n3 y = 2");
    }

    #[test]
    fn a_file_list_holds_to_the_listing() {
        let names: Vec<String> = ["app.py", "notes.txt", "todo.txt"].iter().map(|s| s.to_string()).collect();
        assert!(!listed_mismatch("The files here are app.py, notes.txt and todo.txt.", &names));
        assert!(!listed_mismatch("- app.py\n- notes.txt\n- todo.txt", &names));
        assert!(!listed_mismatch("There are 3 files in this folder.", &names));
        assert!(!listed_mismatch("1. `app.py`: the code\n2. notes.txt — meeting notes\n3. todo.txt", &names));
        assert!(listed_mismatch("The files are app.py, notes.txt, todo.txt, and Elisa.", &names));
        assert!(listed_mismatch("The files are: `app.py`, `notes.txt`, `todo.txt`, and ``", &names));
        assert!(listed_mismatch("- app.py\n- notes.txt\n- main.rs", &names));
        assert!(listed_mismatch("Here they are: app.py and notes.txt.", &names));
    }

    #[test]
    fn a_dependency_check_in_plain_words() {
        let r = "[osv.dev: 2 packages checked (requirements.txt 2) · 2 with known advisories (18)]\n\
            flask 0.12 → GHSA-1 (CVE-1) [HIGH] fixed: 0.12.3 — a\nflask 0.12 → GHSA-2 (CVE-2) [LOW] fixed: 3.1.3 — b\n\
            flask 0.12 → PYSEC-1 (CVE-3) fixed: 70f906c51ce49c485f1d355703e9cc3386b1cc2b, 2.2.5 — c\n\
            requests 2.19.0 → GHSA-3 (CVE-4) [MODERATE] fixed: 2.32.4 …";
        assert_eq!(deps_summary(r, false).as_deref(), Some("2 of 2 packages have known security problems: flask 0.12 (3 advisories, worst high) — update to 3.1.3 or later; \
            requests 2.19.0 (1 advisory, worst moderate) — update to 2.32.4 or later. 18 advisories in all."));
        assert_eq!(deps_summary("[osv.dev: 3 packages checked (package.json 3) · 0 with known advisories (0)]", false).as_deref(), Some("No known security problems in the 3 packages checked."));
        assert!(deps_summary("Meeting code: ORCHID-42", false).is_none());
    }

    #[test]
    fn the_titles_of_web_results() {
        let r = "results for \"ai news\":\n1. AI News Today - Site\n   https://a\n   snippet one.\n2. Another headline\n   https://b\n";
        assert_eq!(web_titles(r), ["AI News Today - Site", "Another headline"]);
    }

    /// asks for one setting's value, by meaning (checks only with the embedding model installed)
    #[test]
    fn value_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, want) in [("what is the foo in the file", true), ("what port does the file use", false), ("what's the foo in the file?", true),
            ("is the foo in the file safe?", false), ("change the foo in the file to 9", false), ("giá trị của foo trong the file là gì?", true)] {
            assert_eq!(value_ask(m), want, "{m}");
        }
    }

    #[test]
    fn text_a_save_gives_by_meaning() {
        assert_eq!(given_text("save 'buy eggs' to todo.txt", "todo.txt").as_deref(), Some("buy eggs\n"));
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, f, want) in [("save the word hello to deny.txt", "deny.txt", Some("hello\n")), ("write the text hello world into a.txt", "a.txt", Some("hello world\n")),
            ("put the line buy eggs in todo.txt", "todo.txt", Some("buy eggs\n")), ("save a poem about cats to cats.txt", "cats.txt", None),
            ("save a one-line summary of notes.txt to summary.md", "summary.md", None), ("save that plan to plan.md", "plan.md", None),
            ("write a short story to story.txt", "story.txt", None), ("ghi dòng xin chào vào a.txt", "a.txt", Some("xin chào\n")),
            ("lưu từ hello vào a.txt", "a.txt", Some("hello\n")), ("ghi chữ xin chào vào b.txt", "b.txt", Some("xin chào\n")), ("lưu bài thơ về mèo vào a.txt", "a.txt", None),
            ("lưu tóm tắt ghi chú vào a.txt", "a.txt", None),
            ("write a poem called 'Rain' to rain.txt", "rain.txt", None),
            // ("use the shell to write hello into …" is the shell's to write: `shell_ask` keeps Elisa's own save off)
            ("write a summary of notes.txt to s.md", "s.md", None),
            // "write (the word / text / line) X to FILE" frames, and one bare word after the verb
            ("write the word layout to l1.txt", "l1.txt", Some("layout\n")), ("write the line buy milk into todo.txt", "todo.txt", Some("buy milk\n")),
            ("write the text ORCHID-42 to code.txt", "code.txt", Some("ORCHID-42\n")), ("write hello to x.txt", "x.txt", Some("hello\n")),
            ("save layout to l2.txt", "l2.txt", Some("layout\n")), ("viết chữ xin chào vào c.txt", "c.txt", Some("xin chào\n")),
            ("ghi hello vào d.txt", "d.txt", Some("hello\n")),
            // a bare word that points back or names a kind of content is no text
            ("write it to x.txt", "x.txt", None), ("save that to plan.md", "plan.md", None), ("write code to app.py", "app.py", None),
            ("save output to out.txt", "out.txt", None), ("write something to x.txt", "x.txt", None), ("lưu nó vào a.txt", "a.txt", None),
            // text after the file, and after the file's name and a colon
            ("it'd be nice to have a hello.md with just the word hello in it", "hello.md", Some("hello\n")),
            ("make hello.md with just the word hello in it", "hello.md", Some("hello\n")), ("create hello.md containing hello", "hello.md", Some("hello\n")),
            ("make notes2.txt with 'buy eggs' in it", "notes2.txt", Some("buy eggs\n")), ("tạo file a.txt với chữ xin chào nhé", "a.txt", Some("xin chào\n")),
            ("sav this to hello.txt: xin chào thế giới", "hello.txt", Some("xin chào thế giới\n")), ("save this to hello.txt: hello world", "hello.txt", Some("hello world\n")),
            ("I renamed it to notes2.txt", "notes2.txt", None), ("read notes.txt: what's the code?", "notes.txt", None),
            // a note jotted down or added, in both languages
            ("jot down 'call Minh at 3pm' in reminders.txt", "reminders.txt", Some("call Minh at 3pm\n")), ("note down 'call Minh' in r.txt", "r.txt", Some("call Minh\n")),
            ("add the line 'buy eggs' to list.txt", "list.txt", Some("buy eggs\n")), ("ghi chú 'gọi Minh lúc 3 giờ' vào nhacnho.txt", "nhacnho.txt", Some("gọi Minh lúc 3 giờ\n")),
            ("thêm dòng 'mua trứng' vào ds.txt", "ds.txt", Some("mua trứng\n"))] {
            assert_eq!(given_text(m, f).as_deref(), want, "{m}");
        }
    }

    /// the file a save names after a word leading to it, in both languages (`save_target`); the first file named is
    /// never the target when another follows the leading word
    #[test]
    fn a_save_target_follows_the_leading_word() {
        assert_eq!(save_target("jot down 'call Minh at 3pm' in reminders.txt").as_deref(), Some("reminders.txt"));
        assert_eq!(save_target("lưu một dòng tóm tắt notes.txt vào file tomtat.md").as_deref(), Some("tomtat.md"));
        assert_eq!(save_target("ghi chú 'gọi Minh lúc 3 giờ' vào nhacnho.txt").as_deref(), Some("nhacnho.txt"));
        assert_eq!(save_target("thêm dòng 'mua trứng' vào ds.txt").as_deref(), Some("ds.txt"));
        assert_eq!(save_target("note down 'call Minh' in the file r.txt").as_deref(), Some("r.txt"));
        for m in ["what's the first item in todo.txt?", "I renamed notes.txt", "read notes.txt", "trong todo.txt, việc đầu tiên là gì?", "đọc notes.txt giúp mình"] {
            assert_eq!(save_target(m), None, "{m}");
        }
    }

    /// one column of a table read, by the header the words name (`exported_column`)
    #[test]
    fn a_column_export_takes_the_column_without_its_header() {
        let rows: Vec<Vec<String>> = [["region", "units"], ["North", "120"], ["South", "340"], ["East", "95"]].iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect();
        assert_eq!(exported_column("export the three region names from   to regions.txt", &rows).as_deref(), Some("North\nSouth\nEast\n"));
        assert_eq!(exported_column("xuất cột units ra units.txt", &rows).as_deref(), Some("120\n340\n95\n"));
        assert_eq!(exported_column("save the regions and units to all.txt", &rows), None);
        assert_eq!(exported_column("save a summary to s.md", &rows), None);
        assert_eq!(exported_column("export the regions to r.txt", &[vec!["region".to_string()]]), None);
    }

    /// Elisa's own which-file answer names the thing in the user's words, never the grep pattern made from it
    #[test]
    fn a_which_file_answer_names_the_users_words_not_the_pattern() {
        let m = "which file in this folder mentions a meeting code?";
        assert_eq!(search_pattern(m).as_deref(), Some("(?i)meeting code"));
        assert_eq!(searched_words(m).as_deref(), Some("meeting code"));
        assert_eq!(searched_words("where is ORCHID-42 mentioned?").as_deref(), Some("ORCHID-42"));
        assert_eq!(search_pattern("where is ORCHID-42 mentioned?").as_deref(), Some("(?i)ORCHID\\-42"));
        assert_eq!(searched_words("read notes.txt"), None);
        // a pattern the model or a frame made: only words the user wrote
        assert_eq!(pattern_in_words("Hello", "search the project for the word Hello").as_deref(), Some("Hello"));
        assert_eq!(pattern_in_words("(?i)ORCHID\\-42", "where is orchid-42?").as_deref(), Some("ORCHID-42"));
        assert_eq!(pattern_in_words("meeting.*code", "which file mentions a meeting code?"), None);
    }

    /// a which-file ask typed without Vietnamese marks is found, answered in Vietnamese, and an English reply to it
    /// falls short (Elisa's own line names the file)
    #[test]
    fn a_which_file_ask_without_marks_gets_the_file_named_in_its_language() {
        let m = "tim giup minh file nao co chu ORCHID";
        assert_eq!(crate::route::which_file_term(m).as_deref(), Some("ORCHID"));
        assert_eq!(searched_words(m).as_deref(), Some("ORCHID"));
        assert!(crate::route::vietnamese(m));
        assert!(which_file_reply_falls_short(m, "The file notes.txt contains the meeting code ORCHID-42."));
        assert!(!which_file_reply_falls_short(m, "File notes.txt có chữ ORCHID ở dòng 1."));
        assert!(which_file_reply_falls_short("file nào có chữ ORCHID?", "The word ORCHID is in notes.txt."));
        let en = "which file in this folder mentions a meeting code?";
        assert!(!which_file_reply_falls_short(en, "The file notes.txt mentions the meeting code."));
        assert!(!which_file_reply_falls_short("read notes.txt", "Tệp notes.txt có một dòng."));
    }

    /// a search of the project's files by its frames, in any spelling (`search_ask`), and asks that are none
    #[test]
    fn file_searches_by_frame() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, want) in [("tim giup minh file nao co chu ORCHID", "ORCHID"), ("tìm giúp mình file nào có chữ ORCHID", "ORCHID"), ("which file mentions ORCHID?", "ORCHID"),
            ("search the project for the word Hello", "Hello"), ("file nào có chữ 'lotus'?", "lotus"), ("tìm chữ Lotus trong các file", "Lotus")] {
            assert_eq!(search_ask(m).as_deref(), Some(want), "{m}");
        }
        for m in ["My name is Linh and I live in Hanoi.", "what's the weather in Hanoi today?", "who wrote Hamlet?", "I went for a run this morning and now I'm starving",
            "read notes.txt", "say hello to Linh for me"] {
            assert_eq!(search_ask(m), None, "{m}");
        }
    }

    /// a web search ask without its frame and politeness, in both languages (`strip_query_frames`)
    #[test]
    fn a_query_loses_its_frame() {
        assert_eq!(strip_query_frames("can u serach the web for who won the world cup in 2022"), "who won the world cup in 2022");
        assert_eq!(strip_query_frames("can you search the web for the population of Iceland?"), "the population of Iceland?");
        assert_eq!(strip_query_frames("giúp mình dân số Iceland"), "dân số Iceland");
        assert_eq!(strip_query_frames("thời tiết Hà Nội hôm nay thế nào?"), "thời tiết Hà Nội hôm nay");
        assert_eq!(strip_query_frames("thủ đô của Úc là gì? tra google giúp mình"), "thủ đô của Úc");
        assert_eq!(strip_query_frames("search google for the capital of Australia"), "the capital of Australia");
        assert_eq!(strip_query_frames("what's the capital of Australia? google it for me"), "what's the capital of Australia?");
        assert_eq!(strip_query_frames("tra cứu giúp mình thủ đô của Úc"), "thủ đô của Úc");
        assert_eq!(strip_query_frames("google giúp mình thủ đô của Úc"), "thủ đô của Úc");
        // nothing to cut, and never everything
        assert_eq!(strip_query_frames("what is the gold price today?"), "what is the gold price today?");
        assert_eq!(strip_query_frames("Google Maps API pricing"), "Google Maps API pricing");
        assert_eq!(strip_query_frames("giúp mình"), "giúp mình");
    }

    /// free disk space said in words from df's table, with and without `-h`, in both languages (`free_space_text`)
    #[test]
    fn free_space_is_said_in_words() {
        let blocks = "Filesystem     512-blocks      Used Available Capacity iused     ifree %iused  Mounted on\n/dev/disk3s1s1  965595304  24523944  27468512    48%  458116 137342560    0%   /\n/dev/disk3s5    965595304 900000000  27468512    97%  458116 137342560    0%   /System/Volumes/Data\n";
        let human = "Filesystem        Size    Used   Avail Capacity iused ifree %iused  Mounted on\n/dev/disk3s1s1   460Gi    12Gi    13Gi    48%    458k  137M    0%   /\n";
        let linux = "Filesystem     1K-blocks     Used Available Use% Mounted on\n/dev/sda1      102400000 51200000  29000000  64% /\n";
        let cwd = Path::new("/Users/linh/proj");
        assert_eq!(free_space_text("how much disk space is free here? use df", "df", blocks, cwd).as_deref(), Some("14 GB free on /."));
        assert_eq!(free_space_text("how much disk space is free here? use df", "df -h", human, cwd).as_deref(), Some("14 GB free on /."));
        assert_eq!(free_space_text("ổ đĩa còn trống bao nhiêu? dùng df", "df", linux, cwd).as_deref(), Some("Còn trống 30 GB trên /."));
        assert_eq!(free_space_text("dung luong trong con bao nhieu? dung df", "df", linux, cwd).as_deref(), Some("Còn trống 30 GB trên /."));
        // no row for "/": the one this folder is on
        let data = "Filesystem     512-blocks      Used Available Capacity  Mounted on\n/dev/disk3s5    965595304 900000000  27468512    97%   /System/Volumes/Data\n";
        assert_eq!(free_space_text("how much free space is left?", "df", data, Path::new("/System/Volumes/Data/Users/linh")).as_deref(), Some("14 GB free on /System/Volumes/Data."));
        // another command, or no ask about space
        assert_eq!(free_space_text("how much disk space is free here? use df", "ls -la", blocks, cwd), None);
        assert_eq!(free_space_text("run df for me", "df", blocks, cwd), None);
        assert!(space_asked("how much disk space is free here?") && space_asked("dung lượng trống còn bao nhiêu?") && !space_asked("what time is it?"));
        assert_eq!(human_bytes("1K"), Some(1024.0));
        assert_eq!(human_bytes("13Gi"), Some(13.0 * 1024f64.powi(3)));
        assert_eq!(human_bytes("abc"), None);
    }

    /// one item of a held list by its place, in both languages (`asked_item`, `item_line`)
    #[test]
    fn an_item_by_its_place_is_answered_alone() {
        let r = |k: &str, v: &str| (k.to_string(), v.to_string());
        let todo = [r("buy milk", ""), r("send report", "")];
        let sales = [r("North", "120 units"), r("South", "340 units"), r("East", "95 units")];
        assert_eq!(asked_item("trong todo.txt, việc đầu tiên là gì?").map(|i| i.0), Some(1));
        assert_eq!(asked_item("what's the last item in todo.txt?").map(|i| i.0), Some(-1));
        assert_eq!(asked_item("the second one?").map(|i| i.0), Some(2));
        assert_eq!(asked_item("cái cuối cùng?").map(|i| i.0), Some(-1));
        assert_eq!(asked_item("cong viec dau tien trong todo.txt la gi?").map(|i| i.0), Some(1));
        for m in ["and South?", "what's in todo.txt?", "how many lines does todo.txt have?", "read notes.txt"] { assert_eq!(asked_item(m), None, "{m}"); }
        assert_eq!(item_line("trong todo.txt, việc đầu tiên là gì?", "todo.txt", &todo).as_deref(), Some("Mục đầu tiên trong todo.txt là buy milk."));
        assert_eq!(item_line("what's the last item in todo.txt?", "todo.txt", &todo).as_deref(), Some("The last item in todo.txt is send report."));
        assert_eq!(item_line("the second one?", "sales.csv", &sales).as_deref(), Some("The second item in sales.csv is South: 340 units."));
        assert_eq!(item_line("the third one?", "todo.txt", &todo), None);
        assert_eq!(item_line("and South?", "sales.csv", &sales), None);
    }

    /// a bare read ask of a reading word and a file name, in any spelling (`bare_read`), and asks that are none
    #[test]
    fn a_bare_read_names_its_file() {
        for (m, f) in [("đọc todo.txt", "todo.txt"), ("mở notes.txt", "notes.txt"), ("doc todo.txt", "todo.txt"), ("mo notes.txt.", "notes.txt"),
            ("read todo.txt", "todo.txt"), ("xem sales.csv", "sales.csv"), ("đọc file todo.txt", "todo.txt"), ("open src/main.rs", "src/main.rs")] {
            assert_eq!(bare_read(m).as_deref(), Some(f), "{m}");
        }
        for m in ["todo.txt", "đọc", "xóa todo.txt", "lưu vào todo.txt", "đọc todo.txt và notes.txt", "I renamed todo.txt", "đọc todo.txt rồi tóm tắt giúp mình"] {
            assert_eq!(bare_read(m), None, "{m}");
        }
        assert!(read_verbs().contains("đọc") && read_verbs().contains("read") && !read_verbs().contains("xóa"));
    }

    /// from web results, the question given back is no answer, and an ask for a figure takes the sentence holding one
    /// (`question_back`, `figure_sentence`)
    #[test]
    fn a_figure_beats_the_question_given_back() {
        let ask = "dân số Iceland là bao nhiêu?";
        assert!(question_back("Dân số Iceland hiện tại là bao nhiêu?", ask));
        assert!(question_back("Dân số Iceland", ask));
        assert!(!question_back("Dân số Iceland là khoảng 393.600 người.", ask));
        assert!(!question_back("Iceland has about 393,600 people.", "how many people live in Iceland?"));
        let results = "results for \"dân số Iceland\":\n1. Dân số Iceland hiện tại là bao nhiêu?\n   https://a.example\n   Dân số Iceland hiện tại là bao nhiêu? Theo thống kê, dân số Iceland là 393.600 người vào năm 2024.\n2. Iceland - Wikipedia\n   https://b.example\n   Iceland is a Nordic island country. Its capital is Reykjavík.\n";
        assert_eq!(figure_sentence(ask, results).as_deref(), Some("Theo thống kê, dân số Iceland là 393.600 người vào năm 2024."));
        assert_eq!(figure_sentence("how many people live in Iceland?", "1. Iceland population\n   https://c\n   Iceland's population is 393,600 as of 2024"), Some("Iceland's population is 393,600 as of 2024.".to_string()));
        // no figure asked, or none held
        assert_eq!(figure_sentence("what is the capital of Australia?", results), None);
        assert_eq!(figure_sentence(ask, "1. Iceland\n   https://d\n   Iceland is an island."), None);
        assert_eq!(split_sentences("Theo thống kê, dân số là 393.600 người. Năm 2024 tăng 1.5%! Thật không?"), ["Theo thống kê, dân số là 393.600 người.", " Năm 2024 tăng 1.5%!", " Thật không?"]);
        assert!(has_figure("393.600 người") && has_figure("in 2024") && !has_figure("1. Iceland") && !has_figure("no digits"));
    }

    #[test]
    fn statements_are_no_web_asks() {
        let chat = crate::route::Route { groups: crate::route::Groups::default(), others: Vec::new(), why: crate::route::Why::Chat };
        for m in ["My name is Linh and I live in Hanoi.", "I'm so tired today"] {
            assert!(crate::route::web_kept_out(&chat, m), "{m}");
        }
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["My name is Linh and I live in Hanoi.", "Mình tên là Linh và mình sống ở Hà Nội.", "tôi sống ở Hà Nội", "tim giup minh file nao co chu ORCHID",
            "which file mentions ORCHID?"] {
            assert!(!crate::route::web_ask(m), "{m}");
        }
        for m in ["thủ đô của Úc là gì? tra google giúp mình", "thu do cua Uc la gi? tra google giup minh", "google giúp mình thủ đô của Úc", "tra cứu giúp mình thủ đô của Úc",
            "search google for the capital of Australia", "what's the capital of Australia? google it for me"] {
            assert!(crate::route::web_ask(m), "{m}");
        }
    }

    /// "save that plan to marketing-plan.md" after a plan ask the model answered with talk about pronouns, in a chat
    /// with an attached document's marker: no earlier reply holds the plan, and a marker is never content
    #[test]
    fn a_save_of_something_said_before_takes_the_reply_that_holds_it() {
        let marker = "[Attached: kronos.pdf, 36 pages · .elisa/uploads/1794408899/kronos.pdf]";
        let pronouns = "Pronouns are words that stand in for nouns. They include I, you, he, she, it, we and they, and they change their form with case and number in many languages, which learners often find hard.";
        let plan = "Marketing plan for a small coffee shop in Hanoi:\n1. Morning combos for office workers near Hoan Kiem.\n2. A loyalty card: the tenth coffee is free.\n3. Weekend latte-art classes shared on Instagram and Facebook.";
        let doc_ask = Message::new(Role::User, vec![Block::Text { text: "what is this document about?".into() }, Block::Text { text: format!("{marker}\nKronos quarterly report: revenue grew 12 percent.") }]);
        let doc_reply = Message::assistant_text("kronos.pdf is a quarterly report on Kronos: its revenue grew 12 percent over the quarter, with notes on costs, staffing and the outlook for the next one.");
        let plan_ask = || Message::user_text("Create a short marketing plan for a small coffee shop in Hanoi.");
        let save = Message::user_text("save that plan to marketing-plan.md");
        assert!(save_points_back("save that plan to marketing-plan.md", "marketing-plan.md") && save_points_back("save it as list.csv", "list.csv") && save_points_back("lưu kế hoạch đó vào plan.md", "plan.md"));
        assert!(!save_points_back("write a short story to story.txt", "story.txt") && !save_points_back("save a summary of notes.txt to s.md", "s.md"));
        assert_eq!(save_thing("save that plan to marketing-plan.md", "marketing-plan.md").as_deref(), Some("plan"));
        assert_eq!(save_thing("save it as list.csv", "list.csv"), None);
        assert_eq!(save_thing("lưu kế hoạch đó vào plan.md", "plan.md").as_deref(), Some("kế hoạch"));
        assert_eq!(save_thing("lưu công thức đó vào cong-thuc.md", "cong-thuc.md").as_deref(), Some("công thức"));
        // a save of something said before with no English saving word: a saving word of the examples and a pointer
        assert!(back_save("lưu kế hoạch đó vào plan.md", "plan.md") && back_save("ghi cái đó vào a.txt", "a.txt") && back_save("save that plan to plan.md", "plan.md"));
        assert!(!back_save("đọc tệp đó: notes.txt", "notes.txt") && !back_save("lưu từ hello vào a.txt", "a.txt") && !back_save("write a poem to p.txt", "p.txt"));
        // the exact chat: the plan ask got no plan, so nothing holds one
        let h = vec![doc_ask.clone(), doc_reply.clone(), plan_ask(), Message::assistant_text(pronouns), save.clone()];
        assert_eq!(answer_in_chat(&h, "plan"), None);
        // the plan answered, then another exchange: the plan's reply, not the newest one
        let h = vec![doc_ask.clone(), doc_reply.clone(), plan_ask(), Message::assistant_text(plan), Message::user_text("what is a pronoun?"), Message::assistant_text(pronouns), save.clone()];
        assert_eq!(answer_in_chat(&h, "plan").as_deref(), Some(format!("{plan}\n").as_str()));
        // "save it": the newest answer
        assert_eq!(answer_in_chat(&h, "").as_deref(), Some(format!("{pronouns}\n").as_str()));
        // a reply that is only the marker, or the marker among lines, is never content
        let h = vec![plan_ask(), Message::assistant_text(marker), save.clone()];
        assert_eq!(answer_in_chat(&h, "plan"), None);
        assert_eq!(without_internal_lines(&format!("{marker}\n{plan}\n[elisa: a note]")), plan);
        assert_eq!(without_internal_lines(marker), "");
        let en = nothing_to_save_line("save that plan to marketing-plan.md", Some("plan"), "marketing-plan.md");
        assert!(en.contains("no plan in this chat") && en.contains("marketing-plan.md") && !en.contains('{'), "{en}");
        let vi = nothing_to_save_line("lưu kế hoạch đó vào plan.md", Some("kế hoạch"), "plan.md");
        assert!(vi.contains("chưa có kế hoạch nào") && vi.contains("plan.md"), "{vi}");
        assert!(nothing_to_save_line("save it as list.csv", None, "list.csv").contains("nothing for list.csv"));
    }

    /// a command given for the shell, by the `[bash]` frames: as written, arguments and all
    #[test]
    fn a_command_given_for_the_shell_runs_as_written() {
        assert_eq!(shell_command("run echo $((40+5)) in the shell").as_deref(), Some("echo $((40+5))"));
        assert_eq!(shell_command("run python3 --version in the shell").as_deref(), Some("python3 --version"));
        assert_eq!(shell_command("please run `ls -la` in the terminal").as_deref(), Some("ls -la"));
        assert_eq!(shell_command("list the files here with ls").as_deref(), Some("ls"));
        assert_eq!(shell_command("chạy lệnh ls -la trong terminal").as_deref(), Some("ls -la"));
        // no command in the words: the model has the shell
        for m in ["use nc in the shell to test a connection to 1.1.1.1 port 80", "use the shell to create the file /etc/u06.txt with the text hello",
            "check if this machine can reach the internet with wget: try http://1.1.1.1 in the shell", "run tests in the shell",
            "use the shell to write hello into .elisa/u06.txt", "what is the capital of France?", "run a marathon"] {
            assert_eq!(shell_command(m), None, "{m}");
        }
    }

    /// a file write by the model on a shell ask is the wrong tool; elsewhere it runs as usual
    #[test]
    fn a_file_write_on_a_shell_ask_is_the_wrong_tool() {
        assert!(wrong_tool_for_shell("write", "run echo hello > a.txt in the shell").is_some_and(|w| w.contains("bash")));
        assert_eq!(wrong_tool_for_shell("bash", "run echo hello > a.txt in the shell"), None);
        assert_eq!(wrong_tool_for_shell("write", "write hello to a.txt"), None);
        assert_eq!(wrong_tool_for_shell("read", "run cat a.txt in the shell"), None);
        if elisa_mem::embed::model_dir().is_none() { return; }
        for t in ["write", "edit", "edit_symbol"] {
            assert!(wrong_tool_for_shell(t, "use the shell to write hello into .elisa/u06.txt").is_some(), "{t}");
        }
    }

    /// a reply that is only commands is run as them on a shell ask; prose is not
    #[test]
    fn a_reply_of_commands_alone_is_a_command() {
        assert_eq!(text_command("nc -z 1.1.1.1 80").as_deref(), Some("nc -z 1.1.1.1 80"));
        assert_eq!(text_command("```bash\necho u06 > /tmp/x.txt\ncat /tmp/x.txt\n```").as_deref(), Some("echo u06 > /tmp/x.txt\ncat /tmp/x.txt"));
        assert_eq!(text_command("$ echo hello > /etc/u06.txt").as_deref(), Some("echo hello > /etc/u06.txt"));
        for r in ["45", "The machine can reach the internet.", "I can't run that here.", "Python 3.8.5", "echo is a command that prints its words back to you."] {
            assert_eq!(text_command(r), None, "{r}");
        }
    }

    /// the shell asks of the sandbox job: a turn about the shell by meaning too (with the embedding model)
    #[test]
    fn shell_asks_by_meaning() {
        assert!(shell_ask("run echo $((40+5)) in the shell"));
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["check if this machine can reach the internet with wget: try http://1.1.1.1 in the shell", "use nc in the shell to test a connection to 1.1.1.1 port 80",
            "use the shell to create the file /etc/u06.txt with the text hello", "use the shell to create /tmp/x.txt outside the project with the text u06, then show its contents",
            "use the shell to write hello into .elisa/u06.txt"] {
            assert!(shell_ask(m), "{m} -> {:?}", meant_tool(m));
        }
        for m in ["search the web: what is the capital of Australia?", "read notes.txt", "save a one-line summary of notes.txt to summary.md", "what time is it?",
            "list the files in this folder", "write hello to x.txt", "tell me a joke", "run it", "run the tests",
            // a plain save that gives its text names no shell: Elisa's own write (`given_text`)
            "write the word layout to l1.txt", "save the word hello to deny.txt", "put the line buy eggs in todo.txt", "ghi chữ xin chào vào b.txt"] {
            assert!(!shell_ask(m), "{m} -> {:?}", meant_tool(m));
        }
    }

    /// "I went for a run this morning and now I'm starving": bash's description verb "run" is the user's noun here, and
    /// "starving" (a word outside the dictionary) is no command, so the sentence gives `described_call` no argument
    /// and no call (`search_term`, `command_args_run`); "search the project for the word Hello" still gives "Hello"
    #[test]
    fn a_noun_run_is_no_described_call() {
        let starving = "I went for a run this morning and now I'm starving";
        assert_eq!(search_term(starving, false), None);
        assert_eq!(search_term("run a marathon", false), None);
        assert_eq!(search_term("search the project for the word Hello", false).as_deref(), Some("Hello"));
        assert!(!command_args_run(&json!({"command": "starving"})));
        assert!(!command_args_run(&json!({"command": "Bob"})));
        assert!(command_args_run(&json!({"command": "ls -la"})) && command_args_run(&json!({"pattern": "starving"})));
        // the bash schema filled from that sentence: no explicit part, path or URL, so no call at all
        let bash = tools::defs_for(false).into_iter().find(|d| d.name == "bash").unwrap();
        assert_eq!(fill_args(&bash.input_schema, search_term(starving, false).as_deref(), &path_tokens(starving), None, |_| false), None);
        if dictionary().is_empty() { return; }
        // (the word outside the dictionary counts only for a tool named outright)
        assert_eq!(search_term(starving, true).as_deref(), Some("starving"));
    }

    /// a command's politeness tail is the frame's, never the command's; the program named last after a frame word is
    /// the command
    #[test]
    fn a_command_loses_its_politeness_tail() {
        assert_eq!(shell_command("chạy lệnh echo xin-chao giúp mình").as_deref(), Some("echo xin-chao"));
        assert_eq!(shell_command("chạy lệnh: echo xin-chao-elisa").as_deref(), Some("echo xin-chao-elisa"));
        assert_eq!(shell_command("run the command: echo hello-elisa").as_deref(), Some("echo hello-elisa"));
        assert_eq!(shell_command("could you run echo hi for me please").as_deref(), Some("echo hi"));
        assert_eq!(shell_command("chạy lệnh ls -la nhé").as_deref(), Some("ls -la"));
        assert_eq!(without_frame_tail("echo", bash_frames()), "echo");
        if dictionary().is_empty() { return; }
        assert_eq!(named_program("how much disk space is free here? use df").as_deref(), Some("df"));
        assert_eq!(named_program("dung lượng đĩa còn bao nhiêu? dùng df").as_deref(), Some("df"));
        for m in ["I love my cat", "use the shell", "how much disk space is free here?", "run"] { assert_eq!(named_program(m), None, "{m}"); }
    }

    /// "again" and the Vietnamese list asks, by meaning (`data/own_tools.txt`)
    #[test]
    fn repeat_and_list_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["again", "run it again", "do that again", "one more time", "chạy lại đi", "lại lần nữa", "làm lại đi"] {
            assert_eq!(meant_tool(m).map(|t| t.0).as_deref(), Some("again"), "{m}");
        }
        for m in ["liệt kê các file trong thư mục này", "thư mục này có file gì?", "list the files here"] {
            assert_eq!(meant_tool(m).map(|t| t.0).as_deref(), Some("glob"), "{m}");
        }
        for m in ["what time is it?", "tell me a joke", "I went for a run this morning and now I'm starving", "run a marathon"] {
            assert!(meant_tool(m).is_none_or(|t| t.0 != "again" && t.0 != "bash"), "{m} -> {:?}", meant_tool(m));
        }
    }

    /// reads of a file by meaning (`data/read_asks.txt`), the file's name put as "the file"
    #[test]
    fn read_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["đọc file the file và cho mình biết mã cuộc họp", "check giúp mình cái file the file, meeting code là gì?", "the file giờ có gì?",
            "không biết trong the file vùng nào bán nhiều nhất nhỉ", "công việc đầu tiên trong the file là gì?", "lemme see whats in the file",
            "đếm số dòng trong the file bằng wc"] {
            assert!(read_ask(m), "{m}");
        }
        for m in ["lưu từ hello vào the file", "I renamed the file", "đổi 'buy milk' thành 'buy bread' trong the file", "is the file still there?",
            "the file còn không?", "tạo the file với chữ hello", "what time is it?"] {
            assert!(!read_ask(m), "{m}");
        }
    }

    /// counting asks in both languages (`data/count_asks.txt`), and Elisa's own count line
    #[test]
    fn counting_asks() {
        assert_eq!(asked_counts("đếm số dòng trong todo.txt bằng wc"), ["lines"]);
        assert_eq!(asked_counts("how many lines does todo.txt have?"), ["lines"]);
        assert_eq!(asked_counts("todo.txt có bao nhiêu từ?"), ["words"]);
        assert_eq!(asked_counts("count the words in notes.txt"), ["words"]);
        assert!(asked_counts("what's in todo.txt?").is_empty());
        assert_eq!(count_text("todo.txt", "buy milk\nsend report\n", &["lines"], true), "todo.txt có 2 dòng.");
        assert_eq!(count_text("todo.txt", "buy milk\nsend report\n", &["lines"], false), "todo.txt has 2 lines.");
        assert_eq!(count_text("a.txt", "one\n", &["lines", "words"], false), "a.txt has 1 line, 1 word.");
    }

    /// short follow-ups on a held file: the rows of a sheet or a list, one call asked twice, "the other one?" by meaning
    #[test]
    fn held_rows_and_next_item_asks() {
        let r = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(held_rows("sales.csv", "region,units\nNorth,120\nSouth,340\nEast,95\n"), [r("North", "120 units"), r("South", "340 units"), r("East", "95 units")]);
        assert_eq!(held_rows("todo.txt", "buy milk\nsend report\n"), [r("buy milk", ""), r("send report", "")]);
        assert_eq!(held_rows("notes.txt", "Meeting code: ORCHID-42\nProject: Lotus launch on Friday\n"), [r("Meeting code", "ORCHID-42"), r("Project", "Lotus launch on Friday")]);
        assert!(same_call(&("web_search".into(), json!({"query": "x", "recent": false})), &("web_search".into(), json!({"query": "x", "recent": true}))));
        assert!(!same_call(&("web_search".into(), json!({"query": "x"})), &("web_search".into(), json!({"query": "y"}))));
        assert!(!same_call(&("read".into(), json!({"path": "x"})), &("glob".into(), json!({"pattern": "x"}))));
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["the other one?", "and the other one?", "và cái kia?", "còn cái kia?", "cái còn lại là gì?"] { assert!(next_item_ask(m), "{m}"); }
        for m in ["and South?", "why?", "the first one?", "what time is it?", "tổng cộng là bao nhiêu?"] { assert!(!next_item_ask(m), "{m}"); }
    }

    /// a change of quoted words in a Vietnamese frame, in either order (`data/edit_asks.txt`)
    #[test]
    fn a_quoted_change_in_a_vietnamese_frame() {
        let is_file = |p: &str| p == "todo.txt";
        assert!(framed_edit("đổi 'buy milk' thành 'buy bread' trong todo.txt", &is_file).is_some());
        assert!(framed_edit("trong todo.txt, đổi 'buy milk' thành 'buy bread'", &is_file).is_some());
        assert!(framed_edit("todo.txt có 'buy milk' hay 'buy bread'?", &is_file).is_none());
    }

    /// web asks of today in both languages (`data/tool_asks.txt` `[web]`), and talk about the weather that is none
    #[test]
    fn web_asks_of_today_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["curious what the population of Iceland is these days, could you look it up?", "giá vàng hôm nay thế nào nhỉ", "thời tiết Hà Nội hôm nay thế nào?",
            "what's the weather in Hanoi today?", "any news today?"] {
            assert!(crate::route::web_ask(m), "{m}");
        }
        for m in ["hôm nay trời đẹp quá", "latest news about Apple", "what is the capital of France?", "it's so hot today", "read notes.txt"] {
            assert!(!crate::route::web_ask(m), "{m}");
        }
    }

    #[test]
    fn a_save_holds_to_the_lines_asked() {
        assert_eq!(asked_lines("save a one-line summary of notes.txt to summary.md"), Some(1));
        assert_eq!(asked_lines("write a 3 line poem to p.txt"), Some(3));
        assert_eq!(asked_lines("save a summary to s.md"), None);
        assert_eq!(fit_lines("Notes.txt:\nMeeting code: ORCHID-42\nProject: Lotus launch on Friday.\n", 1), "Meeting code: ORCHID-42; Project: Lotus launch on Friday.\n");
        assert_eq!(fit_lines("One line.\n", 1), "One line.\n");
        assert_eq!(fit_lines("First part.\n\nSecond part.\n", 1), "First part; Second part.\n");
        assert_eq!(fit_lines("Meeting code: ORCHID-42\n- Project: Lotus launch on Friday.\n", 1), "Meeting code: ORCHID-42; Project: Lotus launch on Friday.\n");
        assert_eq!(fit_lines("1. One\n2) Two\n• Three\n* Four\n", 1), "One; Two; Three; Four.\n");
        let guessed = "Meeting code: ORCHID-42\nProject: Lotus launch on Friday\nThe meeting is scheduled for Friday, likely related to the Lotus launch project. Further details and details about the launch are needed.\nThe project is the Lotus launch.\n";
        assert_eq!(fit_lines(&from_read(guessed, "Meeting code: ORCHID-42\nProject: Lotus launch on Friday.\n"), 1), "Meeting code: ORCHID-42; Project: Lotus launch on Friday.\n");
        let made = "Okay, here's the content for summary.md:\n**Meeting Code: ORCHID-42**\n**Project: Lotus launch on Friday.**\nThis meeting focused on the launch. The team reviewed progress on the core functionality.\n";
        assert_eq!(fit_lines(&from_read(made, "Meeting code: ORCHID-42\nProject: Lotus launch on Friday.\n"), 1),
            "Meeting Code: ORCHID-42; Project: Lotus launch on Friday; This meeting focused on the launch.\n");
        // (the model's narration of the ask never goes in the file: `reply_check::without_meta_narration`)
        let said = "Meeting code: ORCHID-42\nProject: Lotus launch on Friday.\nThe user wants me to write a one-line summary of notes.txt to summary.md.\n\nThe user has changed \"buy milk\" to \"buy bread\".\n";
        let clean = crate::reply_check::without_meta_narration(said).unwrap();
        assert_eq!(fit_lines(&clean, 1), "Meeting code: ORCHID-42; Project: Lotus launch on Friday.\n");
        assert_eq!(fit_lines("**Summary of Notes**\nMeeting code: ORCHID-42\nProject: Lotus launch on Friday.\n", 1), "Meeting code: ORCHID-42; Project: Lotus launch on Friday.\n");
        assert_eq!(fit_lines("Notes:\nA single line.\n", 1), "A single line.\n");
        // the model's planning and a piece cut off mid-way are no content
        let planned = "Gate code: KESTREL-9\nOwner: Mai Tran, Monday.\nThe task is to create a file named gon.md with a summary of plan.txt. It should include the gate code, the owner, and a short";
        assert_eq!(fit_lines(&from_read(planned, "Gate code: KESTREL-9\nOwner: Mai Tran, Monday.\n"), 1), "Gate code: KESTREL-9; Owner: Mai Tran, Monday.\n");
        assert_eq!(from_read("Gate code: KESTREL-9. The owner is Mai Tran and", "Gate code: KESTREL-9\nOwner: Mai Tran, Monday.\n").trim(), "Gate code: KESTREL-9.");
        // bold marks inside a line go as pairs, a fence and a heading line go: no "**" left dangling
        assert_eq!(fit_lines("```\n# Harbor Plan - KESTREL-9\n**Gate Code:** KESTREL-9\n**Owner:** Mai Tran - Monday\n```\n", 1), "Gate Code: KESTREL-9; Owner: Mai Tran - Monday.\n");
        assert_eq!(fit_lines("__Gate Code:__ KESTREL-9\nrun_all.sh uses *.log\nOwner: **Mai\n", 1), "Gate Code: KESTREL-9; run_all.sh uses *.log; Owner: Mai.\n");
    }

    #[test]
    fn an_advisory_given_to_the_wrong_package_is_left_out() {
        let r = "flask 0.12 → GHSA-5wv5 (CVE-2019-1010083) [HIGH] fixed: 1.0\nrequests 2.19.0 → GHSA-9wx4 (CVE-2024-35195) [MODERATE] fixed: 2.32.0";
        let o = advisory_owners(r);
        assert!(o.contains(&("CVE-2019-1010083".to_string(), "flask".to_string())) && o.contains(&("GHSA-9wx4".to_string(), "requests".to_string())), "{o:?}");
        let (out, d) = drop_mispaired("Both are old. CVE-2019-1010083 is in the `requests` library. CVE-2024-35195 affects requests.\n- CVE-2019-1010083: flask DoS\n", &o);
        assert!(d);
        assert_eq!(out, "Both are old. CVE-2024-35195 affects requests.\n- CVE-2019-1010083: flask DoS\n");
        assert!(!drop_mispaired("flask and requests are both affected (CVE-2019-1010083).", &o).1);
    }

    #[test]
    fn a_document_in_plain_words() {
        let sales = "--- sheet Sales ---\nRegion\tUnits\tDate\nNorth\t42\t2023-03-15\nSouth\t7.5\t2023-03-16\n--- sheet Notes ---\nremember the milk\n";
        assert_eq!(doc_in_words("sales.xlsx", sales, false), "sales.xlsx has a table of Region, Units and Date (North: 42, 2023-03-15; South: 7.5, 2023-03-16); a note: \"remember the milk\".");
        assert_eq!(doc_in_words("plan.ods", "Task\tHours\nWrite\t3\n", false), "plan.ods has a table of Task and Hours (Write: 3).");
        assert_eq!(doc_in_words("a.pdf", "Hello there.\n", false), "a.pdf has a note: \"Hello there.\".");
    }

    #[test]
    fn an_advisory_with_the_wrong_fix_or_level_is_left_out() {
        let r = "flask 0.12 → GHSA-a (CVE-2018-1000656) [HIGH] fixed: 0.12.3 — x\nflask 0.12 → GHSA-b (CVE-2019-1010083) [HIGH] fixed: 1.0 — y\n\
            requests 2.19.0 → GHSA-c (CVE-2018-18074) [HIGH] fixed: 2.20.0 — z\nrequests 2.19.0 → GHSA-d (CVE-2024-47081) [MODERATE] fixed: 2.32.4 — w";
        let advs = advisory_facts(r);
        assert_eq!(advs.len(), 4);
        let cur = [vec![0, 12], vec![2, 19, 0]];
        // one wrong of three: that line goes
        let (out, d) = drop_misstated("Flask 0.12:\n- 0.12.3 (CVE-2018-1000656) - HIGH\n- 1.0 (CVE-2019-1010083)\n- 2.32.0 (CVE-2024-47081)\nUpdate both.\n", &advs, &cur, false);
        assert!(d);
        assert_eq!(out, "Flask 0.12:\n- 0.12.3 (CVE-2018-1000656) - HIGH\n- 1.0 (CVE-2019-1010083)\nUpdate both.\n");
        // most wrong: Elisa's own list in their place, the advice kept
        let (out, _) = drop_misstated("- 0.12.3 (CVE-2018-1000656, CVE-2019-1010083) - HIGH to CRITICAL\n- 2.20.0 (CVE-2018-1000656)\n- 2.33.0 (CVE-2024-47081)\nUpdate both.\n", &advs, &cur, false);
        assert_eq!(out, "flask:\n- CVE-2018-1000656 (high): fixed in 0.12.3\n- CVE-2019-1010083 (high): fixed in 1.0\nrequests:\n- CVE-2018-18074 (high): fixed in 2.20.0\n- CVE-2024-47081 (moderate): fixed in 2.32.4\nUpdate both.\n");
        // right ones stand
        assert!(!drop_misstated("CVE-2024-47081 (moderate) is fixed in 2.32.4.", &advs, &cur, false).1);
        // advice at or above the fix stands; below it does not
        assert!(!drop_misstated("Upgrade Flask to 3.1.3 to mitigate CVE-2018-1000656 and requests to 2.33.0 for CVE-2018-18074.", &advs, &cur, false).1);
        assert!(drop_misstated("Upgrade requests to 2.19.5 for CVE-2018-18074.", &advs, &cur, false).1);
    }

    #[test]
    fn an_update_never_made_and_none_found_go() {
        let names = vec!["flask".to_string(), "requests".to_string()];
        let reply = "The `requests` and `flask` packages have been checked for known vulnerabilities in `requirements.txt`. The `requests` package has been updated to 2.33.0 to fix the vulnerability CVE-2024-47081. No other known vulnerabilities were found in the current requirements.";
        let (out, any, _) = drop_wrong_status(reply, &names, &[], false, true);
        assert!(any);
        assert_eq!(out, "The `requests` and `flask` packages have been checked for known vulnerabilities in `requirements.txt`.");
        // (a change that ran keeps the words; a release that fixed something is no update claim)
        assert!(!drop_wrong_status("The `requests` package has been updated to 2.33.0.", &names, &[], true, true).1);
        assert!(!drop_wrong_status("Requests 2.33.0 fixed the netrc leak.", &names, &[], false, true).1);
        assert!(drop_wrong_status("Gói requests không có lỗ hổng nào.", &names, &[], false, true).1);
        assert!(!drop_wrong_status("No known vulnerabilities were found.", &names, &[], false, false).1);
        let safe = "The dependencies listed in the `requirements.txt` file are safe. However, there are known vulnerabilities in flask 0.12 and requests 2.19.0.";
        assert_eq!(drop_wrong_status(safe, &names, &[], false, true).0, "There are known vulnerabilities in flask 0.12 and requests 2.19.0.");
        assert!(!drop_wrong_status("The dependencies in requirements.txt are not safe.", &names, &[], false, true).1);
        assert!(!drop_wrong_status("Các gói trong requirements.txt không an toàn.", &names, &[], false, true).1);
        assert!(drop_wrong_status("Các gói trong requirements.txt an toàn.", &names, &[], false, true).1);
        let these = "The dependencies are `requests==2.19.0` and `flask==0.12`. These are safe to use as they are common packages. However, keep them up to date.";
        let in_use = vec!["0.12".to_string(), "2.19.0".to_string()];
        let fixed = "Flask version 0.12 has been fixed for multiple security issues such as Denial of Service, unexpected memory usage, session security, and potential disclosure of permanent session cookies. Requests version 2.19.0 has been fixed for various advisories and vulnerabilities, such as .netrc credentials leak via malicious URLs, proxy authorization header leak, and insecure temp file reuse in extract\\_zipped\\_paths() utility function.";
        let (out, any, most) = drop_wrong_status(fixed, &names, &in_use, false, true);
        assert!(any && most && out.trim().is_empty(), "{out}");
        assert!(!drop_wrong_status("Flask 0.12 is vulnerable to a denial of service, fixed in 3.1.3.", &names, &in_use, false, true).1);
        assert!(!drop_wrong_status("Flask 0.12 has not been fixed.", &names, &in_use, false, true).1);
        assert!(drop_wrong_status("Gói flask 0.12 đã được vá.", &names, &in_use, false, true).1);
        assert_eq!(drop_wrong_status("The dependencies in requirements.txt are safe, but there are some known advisories and vulnerabilities. Update flask.", &names, &in_use, false, true).0,
            "There are some known advisories and vulnerabilities. Update flask.");
        let r1 = "The results of the dependency scanning tool indicate that the Flask and requests packages have known advisories related to security vulnerabilities. However, the vulnerabilities have been addressed in the latest versions of these packages. Requests has been affected by .netrc credentials leak via malicious URLs, but this vulnerability has been addressed in version 2.33.0 and later.";
        assert_eq!(drop_wrong_status(r1, &names, &in_use, false, true).0, "The results of the dependency scanning tool indicate that the Flask and requests packages have known advisories related to security vulnerabilities. Requests has been affected by .netrc credentials leak via malicious URLs, but this vulnerability has been addressed in version 2.33.0 and later.");
        let later = "Flask 0.12 has some security advisories, but all have been addressed in later versions. Update it.";
        assert_eq!(drop_wrong_status(later, &names, &in_use, false, true).0, "Flask 0.12 has some security advisories. Update it.");
        assert_eq!(drop_wrong_status("These issues have been fixed in later versions of Flask.\nUpdate.", &names, &in_use, false, true).0, "\nUpdate.");
        // (release history about a version the user is not on stays)
        assert!(!drop_wrong_status("Flask 1.0 had issues that were fixed in later versions.", &names, &in_use, false, true).1);
        let live = "Flask 0.12 has some security advisories. Requests 2.19.0 is also safe with reported security advisories addressed in later versions.";
        assert_eq!(drop_wrong_status(live, &names, &in_use, false, true).0, "Flask 0.12 has some security advisories.");
        let r2 = "The dependencies in `requirements.txt` are currently safe. However, there are a few known advisories and vulnerabilities associated with Flask 0.12 and requests 2.19.0.";
        assert_eq!(drop_wrong_status(r2, &names, &in_use, false, true).0, "There are a few known advisories and vulnerabilities associated with Flask 0.12 and requests 2.19.0.");
        let r3 = "Yes, the dependencies in requirements.txt are considered safe. However, it's important to note that some of the packages listed in the requirements.txt file have known advisories with various levels of severity.";
        assert_eq!(drop_wrong_status(r3, &names, &in_use, false, true).0, "It's important to note that some of the packages listed in the requirements.txt file have known advisories with various levels of severity.");
        assert_eq!(drop_wrong_status("The dependencies in requirements.txt are generally safe. Flask 0.12 has known advisories.", &names, &in_use, false, true).0,
            "Flask 0.12 has known advisories.");
        assert_eq!(drop_wrong_status(these, &names, &[], false, true).0, "The dependencies are `requests==2.19.0` and `flask==0.12`. However, keep them up to date.");
    }

    #[test]
    fn only_a_fix_version_is_raised() {
        let found: Vec<Found> = vec![(("flask".into(), "0.12".into()), (8, Some(2), Some(vec![3, 1, 3])))];
        let names = vec!["flask".to_string()];
        let mut said = vec![false];
        let s = "There are vulnerabilities associated with Flask 0.12.0. ";
        assert_eq!(raised(s, &names, &found, &[], false, &mut said), s);
        assert_eq!(raised("Flask versions 0.12.3 and later have fixed these.", &names, &found, &[], false, &mut said), "Flask versions 3.1.3 and later have fixed these.");
        let b = "There are known vulnerabilities associated with Flask 0.12.0. Flask versions 0.12.3 and later have fixed these.";
        assert_eq!(raised(b, &names, &found, &[], false, &mut said), "There are known vulnerabilities associated with Flask 0.12.0. Flask versions 3.1.3 and later have fixed these.");
        assert_eq!(raised("Upgrade flask to 2.3.2 to fix it.", &names, &found, &[], false, &mut said), "Upgrade flask to 3.1.3 to fix it.");
        assert_eq!(raised("Flask 1.0 is vulnerable too.", &names, &found, &[], false, &mut said), "Flask 1.0 is vulnerable too.");
        assert_eq!(raised("flask==0.12.0 is in use.", &names, &found, &[], false, &mut said), "flask==0.12.0 is in use.");
        let v = "2. `Flask` versions `0.12` and `2.3.2` are vulnerable to possible disclosure of a permanent session cookie.\n";
        assert_eq!(raised(v, &names, &found, &[], false, &mut said), v);
    }

    /// the g03 dependency check's result, as the live run gave it
    const G03_DEPS: &str = r#"[osv.dev: 2 packages checked (requirements.txt 2) · 2 with known advisories (18)]
flask 0.12 → GHSA-562c-5r94-xh97 (CVE-2018-1000656) [HIGH] fixed: 0.12.3 — Flask is vulnerable to Denial of Service via incorrect encoding of JSON data
flask 0.12 → GHSA-5wv5-4vpf-pj6m (CVE-2019-1010083) [HIGH] fixed: 1.0 — Pallets Project Flask is vulnerable to Denial of Service via Unexpected memory usage
flask 0.12 → GHSA-68rp-wp8r-4726 (CVE-2026-27205) [LOW] fixed: 3.1.3 — Flask session does not add `Vary: Cookie` header when accessed in some ways
flask 0.12 → GHSA-m2qf-hxjv-5gpq (CVE-2023-30861) [HIGH] fixed: 2.3.2, 2.2.5 — Flask vulnerable to possible disclosure of permanent session cookie due to missing Vary: C…
flask 0.12 → PYSEC-2018-66 (CVE-2018-1000656) fixed: 0.12.3 — The Pallets Project flask version Before 0.12.3 contains a CWE-20: Improper Input Validati…
flask 0.12 → PYSEC-2019-179 (CVE-2019-1010083) fixed: 1.0 — The Pallets Project Flask before 1.0 is affected by: unexpected memory usage. The impact i…
flask 0.12 → PYSEC-2023-62 (CVE-2023-30861) fixed: 70f906c51ce49c485f1d355703e9cc3386b1cc2b, afd63b16170b7c047f5758eb910c416511e9c965, 2.2.5 — Flask is a lightweight WSGI web application framework. When all of the following condition…
flask 0.12 → PYSEC-2026-2151 (CVE-2026-27205) [CVSS_V3 scored] fixed: 3.1.3 — Flask is a web server gateway interface (WSGI) web application framework. In versions 3.1.…
requests 2.19.0 → GHSA-9hjg-9r4m-mvj7 (CVE-2024-47081) [MODERATE] fixed: 2.32.4 — Requests vulnerable to .netrc credentials leak via malicious URLs
requests 2.19.0 → GHSA-9wx4-h78v-vm56 (CVE-2024-35195) [MODERATE] fixed: 2.32.0 — Requests `Session` object does not verify requests after making first request with verify=…
requests 2.19.0 → GHSA-gc5v-m9x4-r6x2 (CVE-2026-25645) [MODERATE] fixed: 2.33.0 — Requests has Insecure Temp File Reuse in its extract_zipped_paths() utility function
requests 2.19.0 → GHSA-j8r2-6x86-q33q (CVE-2023-32681) [MODERATE] fixed: 2.31.0 — Unintended leak of Proxy-Authorization header in requests
requests 2.19.0 → GHSA-x84v-xcm2-53pg (CVE-2018-18074) [HIGH] fixed: 2.20.0 — Insufficiently Protected Credentials in Requests
requests 2.19.0 → PYSEC-2018-28 (CVE-2018-18074) fixed: c45d7c49ea75133e52ab22a8e9e13173938e36ff, 2.20.0 — The Requests package before 2.20.0 for Python sends an HTTP Authorization header to an htt…
requests 2.19.0 → PYSEC-2023-74 (CVE-2023-32681) fixed: 74ea7cf7a6a27a4eeb2ae24e162bcc942a6706d5, 2.31.0 — Requests is a HTTP library. Since Requests 2.3.0, Requests has been leaking Proxy-Authoriz…
requests 2.19.0 → PYSEC-2026-1872 (CVE-2024-47081) [CVSS_V3 scored] fixed: 2.32.4 — Requests vulnerable to .netrc credentials leak via malicious URLs
requests 2.19.0 → PYSEC-2026-1873 (CVE-2024-35195) [CVSS_V3 scored] fixed: 2.32.0 — Requests `Session` object does not verify requests after making first request with verify=…
requests 2.19.0 → PYSEC-2026-2275 (CVE-2026-25645) [CVSS_V3 scored] fixed: 2.33.0 — Requests is a HTTP library. Prior to version 2.33.0, the `requests.utils.extract_zipped_pa…"#;

    #[test]
    fn the_verified_dependency_list_is_built_from_the_check_alone() {
        let full = deps_block(G03_DEPS, false, true).unwrap();
        assert_eq!(full, "2 of 2 packages have known security problems: flask 0.12 (4 advisories, worst high) — update to 3.1.3 or later; requests 2.19.0 (5 advisories, worst high) — update to 2.33.0 or later.
flask 0.12 — 4 advisories (worst: high) → update to 3.1.3 or later
  CVE-2018-1000656 · high · fixed in 0.12.3 · Flask is vulnerable to Denial of Service via incorrect encoding of JSON data
  CVE-2019-1010083 · high · fixed in 1.0 · Pallets Project Flask is vulnerable to Denial of Service via Unexpected memory usage
  CVE-2023-30861 · high · fixed in 2.2.5, 2.3.2 · Flask vulnerable to possible disclosure of permanent session cookie due to missing Vary: C…
  CVE-2026-27205 · low · fixed in 3.1.3 · Flask session does not add `Vary: Cookie` header when accessed in some ways
requests 2.19.0 — 5 advisories (worst: high) → update to 2.33.0 or later
  CVE-2018-18074 · high · fixed in 2.20.0 · Insufficiently Protected Credentials in Requests
  CVE-2024-47081 · moderate · fixed in 2.32.4 · Requests vulnerable to .netrc credentials leak via malicious URLs
  CVE-2024-35195 · moderate · fixed in 2.32.0 · Requests `Session` object does not verify requests after making first request with verify=…
  CVE-2026-25645 · moderate · fixed in 2.33.0 · Requests has Insecure Temp File Reuse in its extract_zipped_paths() utility function
  CVE-2023-32681 · moderate · fixed in 2.31.0 · Unintended leak of Proxy-Authorization header in requests");
        // (no id made up from a PYSEC one, none twice)
        for bad in ["CVE-2026-2151", "CVE-2023-74", "CVE-2026-2275", "PYSEC", "GHSA"] { assert!(!full.contains(bad), "{bad}"); }
        // (nine lines: cut to each package's three worst and one line for the rest; the same on every call)
        let cut = deps_block(G03_DEPS, false, false).unwrap();
        assert_eq!(cut.lines().filter(|l| l.starts_with("  CVE-")).count(), 6);
        assert!(cut.ends_with("3 more — ask for the full list"), "{cut}");
        assert!(!cut.contains("  CVE-2026-27205") && cut.contains("  CVE-2018-18074 · high"));
        // (in Vietnamese for a Vietnamese ask)
        let vi = deps_block(G03_DEPS, true, false).unwrap();
        assert!(vi.starts_with("2 trên 2 gói có lỗi bảo mật đã biết: flask 0.12 (4 cảnh báo, nặng nhất: high)"), "{vi}");
        assert!(vi.contains("flask 0.12 — 4 cảnh báo (nặng nhất: high) → nâng lên 3.1.3 trở lên") && vi.contains("· đã sửa ở 0.12.3 ·") && vi.ends_with("còn 3 cảnh báo nữa — hãy hỏi để xem toàn bộ danh sách"), "{vi}");
    }

    #[test]
    fn the_other_checks_answer_from_their_results() {
        let secrets = "[secrets in the project: 4 text files scanned (.gitignore respected) · 2 findings in 2 files]\nconfig.toml:3 openai-key sk-l…\nsession.py:1 generic-secret eyJh…";
        let none = serde_json::json!({});
        assert_eq!(check_block("sec_secrets", secrets, &none, false).unwrap(), "The check found 2 likely secrets in 2 files (4 text files scanned):\n  config.toml, line 3 · openai-key · sk-l…\n  session.py, line 1 · generic-secret · eyJh…");
        assert_eq!(check_block("sec_secrets", secrets, &none, true).unwrap().lines().nth(1), Some("  config.toml, dòng 3 · openai-key · sk-l…"));
        assert_eq!(check_block("sec_secrets", "[secrets in the project: 4 text files scanned (.gitignore respected) · 0 findings in 0 files]\nno likely secrets found", &none, false).as_deref(),
            Some("No likely secrets found in the 4 text files scanned."));
        let binary = "[hello_bin: 33608 bytes]\nMach-O arm64 EXECUTE, 64-bit, PIE\ncode signature: present (LC_CODE_SIGNATURE; not validated)\nentry: 0x100000460\nsymbols: 6 (stripped: no local symbols)\nlinked libraries (1): /usr/lib/libSystem.B.dylib\nimports: 0";
        assert_eq!(check_block("sec_binary", binary, &serde_json::json!({"path": "hello_bin"}), false).unwrap(), "What the check found in hello_bin (33608 bytes):\n  Mach-O arm64 EXECUTE, 64-bit, PIE\n  code signature: present (LC_CODE_SIGNATURE; not validated)\n  entry: 0x100000460\n  symbols: 6 (stripped: no local symbols)\n  linked libraries (1): /usr/lib/libSystem.B.dylib\n  imports: 0");
        assert!(check_block("sec_binary", binary, &serde_json::json!({"path": "hello_bin"}), true).unwrap().starts_with("Kiểm tra thấy trong hello_bin (33608 byte):"));
        let jwt = "JWT decoded locally. The signature is NOT verified: do not trust these claims.\nalg: none · typ: JWT\nWARNING: alg \"none\" (unsigned) — anyone can forge this token; a server that accepts it is vulnerable\nexp: none — this token never expires\nother claims: user=admin · role=admin";
        assert_eq!(check_block("sec_jwt", jwt, &none, false).unwrap(), "The token, decoded here (its signature is not checked):\n  alg: none · typ: JWT\n  WARNING: alg \"none\" (unsigned) — anyone can forge this token; a server that accepts it is vulnerable\n  exp: none — this token never expires\n  other claims: user=admin · role=admin");
        // (the note: a secret written out, a restated list and a made-up address go; the advice stays)
        let block = check_block("sec_secrets", secrets, &none, false).unwrap();
        let reply = "The api_key in config.toml is sk-live-4f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c.\n* config.toml line 3\nRotate the key now and move it to an environment variable. Remove the token from session.py.";
        assert_eq!(check_note(reply, &block, secrets), "Rotate the key now and move it to an environment variable. Remove the token from session.py.");
        assert_eq!(check_note("The entry point is at 0x100000999. Sign the binary with a valid certificate.", "", binary), "Sign the binary with a valid certificate.");
        assert_eq!(check_note("Mình chưa trả lời được câu này với minicpm-v:8b.", "", binary), "");
        assert_eq!(check_note("I couldn't answer that with moondream.", "", binary), "");
        // (a sentence that only restates the result is no note on what to do)
        assert_eq!(check_note("Scanning this project for leaked secrets resulted in 2 findings in 2 files. Rotate both keys.", &block, secrets), "Rotate both keys.");
        assert_eq!(check_note("Tệp `hello_bin` là một tệp aplikasi arm64 với ký tự thực thi (EXEC) và kích thước là 33608 ký tự. Bạn nên ký lại tệp này.", "", binary), "Bạn nên ký lại tệp này.");
    }

    #[test]
    fn the_models_note_keeps_to_what_to_do() {
        let reply = "The requirements.txt file contains the following vulnerable dependencies:\n* Flask version 0.12 has vulnerabilities:\n\t+ CVE-2026-2151: Denial of Service via unexpected memory usage.\nYou should update Flask to 3.1.3 and Requests to 2.33.0. Requests 2.4.0 is fine. This is critical. Then run your tests. Pin the new versions. Deploy.";
        assert_eq!(deps_note(reply, G03_DEPS), "You should update Flask to 3.1.3 and Requests to 2.33.0. Then run your tests. Pin the new versions.");
        assert_eq!(deps_note("* CVE-2018-1000656: x\n1. CVE-2019-1010083: y\n", G03_DEPS), "");
        assert_eq!(deps_note("```tool_output} requests==2.19.0 flask==0.12", G03_DEPS), "");
        // (each advisory's own fixes and Elisa's own summary are in her list already)
        assert_eq!(deps_note("The vulnerabilities in `flask` are fixed in versions 3.1.3, 2.3.2, 2.2.5, and 0.12.3. Update both packages.", G03_DEPS), "Update both packages.");
        let vi = deps_block(G03_DEPS, true, false).unwrap();
        assert_eq!(deps_note(vi.lines().next().unwrap(), G03_DEPS), "");
        // (the model's own label goes: Elisa's "What to do:" gives it)
        assert_eq!(deps_note("**Action**: Update both packages.", G03_DEPS), "Update both packages.");
        assert_eq!(check_note("**Action**: Replace the token with a new signed one.", "", "token"), "Replace the token with a new signed one.");
        assert_eq!(unlabelled("Recommendation: rotate the key now."), "rotate the key now.");
        assert_eq!(unlabelled("**Rotate** the key now."), "Rotate the key now.");
        assert_eq!(unlabelled("Update flask: it is old."), "Update flask: it is old.");
        // (a sentence that only restates the result with a doing word in it is no advice: the note goes, the block
        // stands alone; an order or a "should" stays)
        assert_eq!(deps_note("A potential security vulnerability has been identified in the Requests library, requiring a fix.", G03_DEPS), "");
        let jwt = "JWT decoded locally. The signature is NOT verified: do not trust these claims.\nalg: none · typ: JWT\nexp: none — this token never expires";
        let block = check_block("sec_jwt", jwt, &serde_json::json!({}), false).unwrap();
        assert_eq!(check_note("The server accepting this token is vulnerable because it doesn't verify the signature. The expiration is set to none, so the token never expires.", &block, jwt), "");
        assert_eq!(check_note("The server accepting this token is vulnerable. Reject tokens with alg none and set an expiry. Then verify every signature.", &block, jwt),
            "Reject tokens with alg none and set an expiry. Then verify every signature.");
        assert_eq!(deps_note("To fix this, upgrade Requests to 2.33.0.", G03_DEPS), "To fix this, upgrade Requests to 2.33.0.");
        assert_eq!(check_note("Mã này không kiểm tra chữ ký. Bạn nên từ chối mã có alg none.", "", jwt), "Bạn nên từ chối mã có alg none.");
    }

    #[test]
    fn a_fix_said_of_one_advisory_is_its_own() {
        let r = "flask 0.12 → GHSA-a (CVE-2018-1000656) [HIGH] fixed: 0.12.3 — Flask denial of service via JSON\n\
            flask 0.12 → GHSA-b (CVE-2019-1010083) [HIGH] fixed: 1.0 — Flask denial of service via memory\n\
            flask 0.12 → GHSA-c (CVE-2023-30861) [HIGH] fixed: 2.2.5, 3.1.3 — Flask session cookie";
        let advs = advisory_facts(r);
        let found: Vec<Found> = vec![(("flask".into(), "0.12".into()), (8, Some(2), Some(vec![3, 1, 3])))];
        let names = vec!["flask".to_string()];
        let mut said = vec![false];
        let fix = |s: &str| raised(s, &names, &found, &advs, false, &mut [false]);
        assert_eq!(fix("1. Flask 0.12 has a Denial of Service vulnerability (CVE-2018-1000656) which is fixed in version 3.1.3.\n"),
            "1. Flask 0.12 has a Denial of Service vulnerability (CVE-2018-1000656) which is fixed in version 0.12.3.\n");
        assert_eq!(fix("2. Flask 0.12 has another Denial of Service vulnerability (CVE-2019-1010083) which is fixed in version 3.1.3.\n"),
            "2. Flask 0.12 has another Denial of Service vulnerability (CVE-2019-1010083) which is fixed in version 1.0.\n");
        // (its own fix stays; the overall upgrade with no advisory named still goes to the highest)
        let own = "Flask 0.12 (CVE-2018-1000656) is fixed in 0.12.3.";
        assert_eq!(fix(own), own);
        assert_eq!(raised("Upgrade flask to 2.3.2.", &names, &found, &advs, false, &mut said), "Upgrade flask to 3.1.3.");
    }

    #[test]
    fn a_fixed_version_is_never_called_vulnerable() {
        let found: Vec<Found> = vec![(("flask".into(), "0.12".into()), (8, Some(2), Some(vec![3, 1, 3])))];
        let names = vec!["flask".to_string()];
        let advs = advisory_facts("flask 0.12 → GHSA-a (CVE-2023-30861) [HIGH] fixed: 2.2.5, 3.1.3 — Flask session cookie");
        let s = "2. `Flask` versions `0.12` and `3.1.3` are vulnerable to possible disclosure of a permanent session cookie due to missing `Vary: Cookie` header (CVE-2023-30861).\n";
        let (out, any) = drop_fixed_as_vulnerable(s, &names, &found, &advs);
        assert!(any);
        assert_eq!(out, "2. `Flask` version `0.12` is vulnerable to possible disclosure of a permanent session cookie due to missing `Vary: Cookie` header (CVE-2023-30861).\n");
        // (only the fixed one: the sentence goes)
        assert_eq!(drop_fixed_as_vulnerable("Flask 3.1.3 is vulnerable to it. Update.", &names, &found, &advs).0, "Update.");
        // (a fix in a fixed-in place, and versions not fixed, stay)
        let kept = "Flask 0.12 is vulnerable, fixed in 3.1.3. Flask versions 0.12 and 1.0 are vulnerable.";
        assert!(!drop_fixed_as_vulnerable(kept, &names, &found, &advs).1);
        // (the raise never makes a listed version a fixed one)
        let mut said = vec![false];
        let v = "2. `Flask` versions `0.12` and `1.0` are vulnerable to possible disclosure of a permanent session cookie.\n";
        assert_eq!(raised(v, &names, &found, &[], false, &mut said), v);
    }

    #[test]
    fn a_score_the_check_never_gave_goes() {
        let result = "requests 2.19.0 → GHSA-9hjg-9r4m-mvj7 (CVE-2024-47081) [MODERATE] CVSS_V3 scored fixed: 2.32.4 — Requests .netrc credentials leak";
        let reply = "The dependencies listed in `requirements.txt` are `requests==2.19.0` and `flask==0.12`. According to the information provided by the security analysis tool, `requests` has a moderate vulnerability (`MODERATE`) with a score of 10.0 on the Common Vulnerability Scoring System (CVSS) and the CVE-2024-47081 vulnerability. It is advised to update to the latest version (`2.32.4`) to fix this vulnerability.";
        let (out, any) = drop_unbacked_scores(reply, result);
        assert!(any);
        assert_eq!(out, "The dependencies listed in `requirements.txt` are `requests==2.19.0` and `flask==0.12`. It is advised to update to the latest version (`2.32.4`) to fix this vulnerability.");
        // (a score the check gives stays; a version near "score" is no score)
        assert!(!drop_unbacked_scores("It has a CVSS score of 7.5.", "x [HIGH] CVSS 7.5").1);
        assert!(!drop_unbacked_scores("Update to 2.32.4 to raise its score.", result).1);
    }

    #[test]
    fn a_wrong_count_of_advisories_is_put_right() {
        let counts = vec![("flask".to_string(), 8), ("requests".to_string(), 10)];
        let reply = "Flask 0.12 has three known vulnerabilities: a Denial of Service vulnerability, a Denial of Service vulnerability, and a Denial of Service vulnerability. Requests 2.19.0 has four known vulnerabilities: a .netrc credentials leak vulnerability, a vulnerability where the `Session` object does not verify requests.";
        let (out, any, most) = fix_counts(reply, &counts);
        assert!(any && most, "{out}");
        let listed = "The vulnerable dependencies found in the `requirements.txt` file are:\n1. Flask (version 0.12)\n2. Requests (version 2.19.0)\nFlask has two known vulnerabilities:\n1. Flask is vulnerable to Denial of Service via incorrect encoding of JSON data (fixed in Flask 3.1.3).\n2. Flask may disclose a permanent session cookie.\nUpdate both.\n";
        assert_eq!(fix_counts(listed, &counts).0, "The vulnerable dependencies found in the `requirements.txt` file are:\n1. Flask (version 0.12)\n2. Requests (version 2.19.0)\nUpdate both.\n");
        assert_eq!(fix_counts("Flask 0.12 has 3 known security issues. Requests has four advisories.\n", &counts).0, "Flask 0.12 has 8 known security issues. Requests has 10 advisories.\n");
        assert!(!fix_counts("flask có ba lỗ hổng; requests có mười lỗ hổng.", &counts).1);
        assert_eq!(fix_counts("Gói requests có bốn lỗ hổng.", &counts).0, "Gói requests có 10 lỗ hổng.");
        assert!(!fix_counts("Flask 0.12 has 8 vulnerabilities and requests 2.19.0 has ten.", &counts).1);
    }

    #[test]
    fn a_package_credited_with_anothers_advisories_goes() {
        let r = "flask 0.12 → GHSA-a (CVE-1) [HIGH] fixed: 0.12.3 — Pallets Project Flask denial of service via incorrect JSON encoding\n\
            flask 0.12 → GHSA-b (CVE-2) [HIGH] fixed: 2.3.2 — Flask session cookie disclosure\n\
            requests 2.19.0 → PYSEC-2018-28 (CVE-3) [MODERATE] fixed: 2.32.4 — Requests leaks proxy authorization headers\n\
            requests 2.19.0 → GHSA-c (CVE-4) [MODERATE] fixed: 2.32.0 — Requests credentials leak via netrc";
        let advs = advisory_facts(r);
        let reply = "The requirements.txt file lists vulnerabilities in the requests library. Specifically, there are credentials leaks.  including a denial-of-service issue with the JSON encoding, an issue with the Pallets project, and a session cookie leak. The `PYSEC` tool has fixed some vulnerabilities in the Requests library.\nUpdate both.\n";
        let (out, any, _) = drop_credited_wrong(reply, &advs);
        assert!(any);
        assert_eq!(out, "The requirements.txt file lists vulnerabilities in the requests library. Specifically, there are credentials leaks.\nUpdate both.\n");
        assert!(!drop_credited_wrong("Flask has a JSON encoding issue. Requests leaks proxy headers.", &advs).1);
    }

    #[test]
    fn an_aside_naming_the_same_version_goes() {
        assert_eq!(without_same_version_aside("Upgrade to **Flask ≥ 3.1.3** (or at least **3.1.3** for the earliest patch).", &[3, 1, 3]), "Upgrade to **Flask ≥ 3.1.3**.");
        assert_eq!(without_same_version_aside("Flask (HIGH) needs 3.1.3 (fixed in 3.1.3+).", &[3, 1, 3]), "Flask (HIGH) needs 3.1.3.");
        assert_eq!(without_same_version_aside("Update to 3.1.3 (see CVE-2023-30861).", &[3, 1, 3]), "Update to 3.1.3 (see CVE-2023-30861).");
        assert_eq!(without_same_version_aside("Flask (fixed in 3.1.3+).", &[3, 1, 3]), "Flask (fixed in 3.1.3+).");
    }

    #[test]
    fn each_version_goes_with_the_package_before_it() {
        let p = version_places("flask 0.12 — update to 3.1.3; requests 2.19.0 — update to 2.33.0. CVE-2024-35195");
        assert_eq!(p.iter().map(|x| x.2.clone()).collect::<Vec<_>>(), [vec![0, 12], vec![3, 1, 3], vec![2, 19, 0], vec![2, 33, 0]]);
        assert_eq!(version_places("Update Flask to 2.3.2+ and requests to 2.32.4.").iter().map(|x| x.2.clone()).collect::<Vec<_>>(), [vec![2, 3, 2], vec![2, 32, 4]]);
    }

    #[test]
    fn a_lead_in_to_the_user_is_no_file_content() {
        assert_eq!(without_lead_in("Okay, here's a short marketing plan for a small coffee shop in Hanoi:\n\n# Plan\n- Sell coffee\n"), "# Plan\n- Sell coffee\n");
        assert_eq!(without_lead_in("Đây là kế hoạch cho quán cà phê:\n- Bán cà phê\n"), "- Bán cà phê\n");
        assert_eq!(without_lead_in("Ingredients:\n- flour\n"), "Ingredients:\n- flour\n");
        assert_eq!(without_lead_in("Okay, this is fine.\nMore.\n"), "Okay, this is fine.\nMore.\n");
    }

    #[test]
    fn a_reply_says_a_change_was_made() {
        assert!(says_done("The 'buy milk' task in todo.txt has been updated to 'buy bread'.", "buy bread"));
        assert!(says_done("Done! todo.txt now reads: buy bread.", "buy bread"));
        assert!(says_done("Đã đổi thành 'buy bread' trong todo.txt.", "buy bread"));
        assert!(!says_done("The user wants the to-do list to be updated.  The task is to change \"buy milk\" to \"buy bread\".", "buy bread"));
        assert!(!says_done("I will change it to buy bread.", "buy bread"));
        assert!(!says_done("Changed it.", "buy bread"));
    }

    #[test]
    fn forms_and_short_forms_of_a_word() {
        assert!(same_word("vulnerabilities", "vulnerable") && same_word("deps", "dependencies") && same_word("secrets", "secrets"));
        assert!(!same_word("deps", "depth") && !same_word("read", "ready") && !same_word("file", "files2") && !same_word("token", "tool"));
    }

    #[test]
    fn quoted_text_keeps_its_marks_in_pairs() {
        assert_eq!(paired_marks("**Meeting code: ORCHID-42** – launch"), "**Meeting code: ORCHID-42** – launch");
        assert_eq!(paired_marks("Meeting code: ORCHID-42** – launch"), "Meeting code: ORCHID-42 – launch");
        assert_eq!(paired_marks("run `cargo test"), "run cargo test");
    }

    /// asks to see one symbol as it is, by meaning (checks only with the embedding model installed)
    #[test]
    fn a_binary_check_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["scan this project for leaked secrets", "quét dự án này xem có lộ bí mật không"] {
            assert_eq!(meant_tool(m).map(|x| x.0).as_deref(), Some("sec_secrets"), "{m}");
        }
        assert_eq!(meant_tool("check the binary hello_bin for security problems").map(|x| x.0).as_deref(), Some("sec_binary"));
        // (Vietnamese is near any "kiểm tra … có … không" check: a program named in the words is the sign)
        for m in ["kiểm tra tệp nhị phân hello_bin có vấn đề bảo mật không", "kiem tra tep nhi phan hello_bin co van de bao mat khong"] {
            assert_eq!(meant_tool_signed(m).map(|x| x.0).as_deref(), Some("sec_binary"), "{m}");
        }
        assert_eq!(meant_tool_signed("kiểm tra xem bài viết có lỗi chính tả không").map(|x| x.0), None);
    }

    #[test]
    fn symbol_show_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, want) in [("show me just the foo function in the file", true), ("show me the foo function again", true), ("let me see the foo function", true),
            ("cho mình xem hàm foo", true), ("what does the foo function do?", false), ("the foo function in the file subtracts; fix it", false),
            ("show me the outline of the file", false), ("explain foo", false), ("is foo correct?", false)] {
            assert_eq!(symbol_ask_kind(m).as_deref() == Some("show"), want, "{m}");
        }
        // a question whether something is wrong in it is no ask to see it, in either language
        for m in ["file the file có lỗi gì không? xem hàm foo", "hàm foo sai ở đâu?", "ham foo co loi gi khong?", "is there a mistake in the foo function?", "what's wrong with foo?"] {
            assert_eq!(symbol_ask_kind(m).as_deref(), Some("check"), "{m}");
        }
        assert_eq!(symbol_frame("file app.py có lỗi gì không? xem hàm add", "app.py", "add"), "file the file có lỗi gì không? xem hàm foo");
    }

    /// a reply that only gives code back says nothing of it (`judges_code`)
    #[test]
    fn a_held_value_wrapped_in_invented_items_and_a_resaid_day_are_put_right() {
        let notes = vec!["Meeting code: ORCHID-42".to_string(), "Project: Lotus launch on Friday.".to_string()];
        let dump = " What needs to be done next?\n- Confirm the meeting code ORCHID-42 for the Lotus launch project meeting.\n- Ensure any necessary documents or information for the Lotus launch are ready.\n\nNext step: Confirm meeting details with the team.";
        assert!(super::super::docs::invented_beside(dump, "read notes.txt and tell me the meeting code", &notes));
        let sales = vec!["North: 120".to_string(), "South: 95".to_string()];
        assert!(super::super::docs::invented_beside("North sold 120. Consider hiring more staff for the region. Plan a marketing push next quarter too.", "how much did North sell?", &sales));
        // the answer worded its own way, or in another language than the file: stands
        assert!(!super::super::docs::invented_beside("The meeting code is ORCHID-42.", "read notes.txt and tell me the meeting code", &notes));
        assert!(!super::super::docs::invented_beside("Mã cuộc họp là ORCHID-42. Dự án Lotus ra mắt vào thứ Sáu.", "mã cuộc họp là gì?", &notes));
        // a re-said answer naming another day than the held line gets the held line (`same_answer_in` runs the checks)
        assert_eq!(crate::reply_check::held_contradictions_fixed("1. Code meeting: ORCHID-42\n2. Thể loại dự án: Khởi động Lotus vào thứ Bảy hôm nay.", &notes).as_deref(),
            Some("1. Code meeting: ORCHID-42\n2. Project: Lotus launch on Friday."));
    }

    #[test]
    fn a_computed_sheet_fact_and_a_restated_ask_pick_the_answer() {
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: highest units = South (340) · answer with these results", "không biết trong sales.csv vùng nào bán nhiều nhất nhỉ").as_deref(),
            Some("South có units cao nhất trong sales.csv (340)."));
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: total units = 120 + 340 + 95 = 555 · answer with these results", "what's the total in sales.csv?").as_deref(),
            Some("The total units in sales.csv is 555."));
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: lowest units = East (95); average units = 555 / 3 = 185 · answer with these results", "which region sold the least, and the average?").as_deref(),
            Some("East has the lowest units in sales.csv (95). The average units in sales.csv is 185."));
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: highest units = North and South (340) · answer with these results", "vùng nào bán nhiều nhất?").as_deref(),
            Some("North và South có units cao nhất trong sales.csv (340)."));
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: highest units = North and South (340) · answer with these results", "which region sold the most?").as_deref(),
            Some("North and South have the highest units in sales.csv (340)."));
        assert_eq!(computed_table_line("Elisa computed it exactly from plan.csv: number of tasks = 4 · answer with these results", "có bao nhiêu tasks?").as_deref(),
            Some("plan.csv có 4 tasks."));
        // a kind with no sentence keeps the note's words
        assert_eq!(computed_table_line("Elisa computed it exactly from sales.csv: median units = 120 · answer with these results", "median?").as_deref(),
            Some("From sales.csv: median units = 120."));
        assert_eq!(computed_table_line("Elisa computed it exactly: (3*7) - 5 = 16 · answer with this result", "x"), None);
        // the thing asked, said in the labels' language by the user's model: the one line whose label holds it
        use super::super::docs::pick_by_restated;
        let notes = vec!["Meeting code: ORCHID-42".to_string(), "Project: Lotus launch on Friday.".to_string()];
        let plan = vec!["Owner: Mai".to_string(), "Budget: 4,000".to_string(), "Deadline: May 3".to_string()];
        assert_eq!(pick_by_restated("meeting code", &notes), Some(0));
        assert_eq!(pick_by_restated("The meeting code", &notes), Some(0));
        assert_eq!(pick_by_restated("budget", &plan), Some(1));
        assert_eq!(pick_by_restated("project deadline", &plan), None);
        assert_eq!(pick_by_restated("code", &["Meeting code: A".to_string(), "Door code: B".to_string()]), None);
        assert_eq!(pick_by_restated("the code that opens the meeting room door today", &notes), None);
    }

    #[test]
    fn a_value_asked_and_answered_with_the_files_lines_listed_is_said_in_a_sentence() {
        use super::super::docs::{held_value_answer, lists_lines};
        let notes = vec!["Meeting code: ORCHID-42".to_string(), "Project: Lotus launch on Friday.".to_string()];
        let code = [&notes[0]];
        // the lines listed, in either language, with or without a lead-in and list marks
        for (reply, ask) in [("File notes.txt có nội dung:\n1 Meeting code: ORCHID-42\n2 Project: Lotus launch on Friday.", "check giúp mình cái file notes.txt, meeting code là gì?"),
            ("The file notes.txt contains:\n- Meeting code: ORCHID-42\n- Project: Lotus launch on Friday.", "what is the meeting code in notes.txt?"),
            ("Meeting code: ORCHID-42 Project: Lotus launch on Friday.", "tell me the meeting code")] {
            assert!(lists_lines(reply, ask, "notes.txt", &notes, &code), "{reply}");
        }
        // an answer worded its own way, or only the asked line copied: stands
        assert!(!lists_lines("The meeting code is ORCHID-42; the project is the Lotus launch on Friday.", "what is the meeting code?", "notes.txt", &notes, &code));
        assert!(!lists_lines("Meeting code: ORCHID-42", "what is the meeting code?", "notes.txt", &notes, &code));
        assert!(!lists_lines("Mã cuộc họp là ORCHID-42. Ngoài ra, Project: Lotus launch on Friday là dự án sắp ra mắt mà nhóm đang chuẩn bị rất kỹ lưỡng.", "meeting code là gì?", "notes.txt", &notes, &code));
        assert_eq!(held_value_answer("meeting code là gì?", "notes.txt", &notes, &code), "Trong notes.txt có: Meeting code: ORCHID-42");
        assert_eq!(held_value_answer("what is the meeting code?", "notes.txt", &notes, &code), "notes.txt says: Meeting code: ORCHID-42");
        // a sheet's row by its label and its cells with the header's names
        let sales = vec!["region,units".to_string(), "North,120".to_string(), "South,340".to_string()];
        let north = [&sales[1]];
        assert!(lists_lines("region,units\nNorth,120", "how many units did North sell according to sales.csv?", "sales.csv", &sales, &north));
        assert_eq!(held_value_answer("how many units did North sell according to sales.csv?", "sales.csv", &sales, &north), "North: 120 units.");
        assert_eq!(held_value_answer("miền Bắc bán bao nhiêu?", "sales.csv", &sales, &[&sales[2]]), "South: 340 units.");
    }

    #[test]
    fn a_held_line_is_picked_only_when_the_model_names_it_in_both_orders() {
        let lines = vec!["Meeting code: ORCHID-42".to_string(), "Project: Lotus launch on Friday.".to_string()];
        let budget = vec!["Owner: Mai".to_string(), "Budget: 4,000".to_string(), "Deadline: May 3".to_string()];
        // a model that answers by meaning: the number of the line holding the asked label, whatever the order
        let by_label = |label: &'static str| move |q: &str| q.lines().find(|l| l.contains(label)).and_then(|l| l.split('.').next()).map(str::to_string);
        assert_eq!(super::super::docs::pick_in_both_orders("đọc file notes.txt và cho mình biết mã cuộc họp", &lines, by_label("Meeting code")), Some(0));
        assert_eq!(super::super::docs::pick_in_both_orders("cho mình biết ngân sách", &budget, by_label("Budget")), Some(1));
        // a model that answers by place ("2" whatever is asked): no pick, so a few lines are shown whole
        assert_eq!(super::super::docs::pick_in_both_orders("đọc file notes.txt và cho mình biết mã cuộc họp", &lines, |_| Some("2".to_string())), None);
        assert_eq!(super::super::docs::pick_in_both_orders("cho mình biết ngân sách", &budget, |_| Some("1".to_string())), None);
    }

    #[test]
    fn an_answer_from_a_command_gives_its_short_output_word_for_word() {
        let run = |cmd: &str, body: &str, is_error: bool| OwnResult { name: "bash".into(), input: json!({"command": cmd}), raw: body.into(), body: body.into(), is_error };
        let echo = [run("echo xin-chao-elisa", "xin-chao-elisa\n[exit 0]", false)];
        assert!(short_run_output(&echo));
        assert!(!short_run_output(&[run("seq 9", &(1..=9).map(|i| i.to_string()).collect::<Vec<_>>().join("\n"), false)]));
        assert!(!gives_run_output("Kết quả là thông điệp đó được hiển thị và chương trình kết thúc.", &echo));
        assert!(gives_run_output("Lệnh đã in ra: xin-chao-elisa", &echo));
        // Elisa's exit mark said in the reply's own words
        let boom = [run("echo boom; exit 3", "boom\n[exit 3]", false)];
        assert!(gives_run_output("The command fails with boom (exit 3).", &boom));
        assert!(!gives_run_output("The command failed.", &boom));
        // the output reported as output, not a word spoken of (r33); and the talk beside the run goes
        let hi = [run("echo xin-chao", "xin-chao\n[exit 0]", false)];
        assert!(!reports_run_output("Bài viết \"xin-chao\" là một câu chào rất phổ biến trong tiếng Việt.", &hi));
        assert!(reports_run_output("Lệnh in ra: xin-chao", &hi));
        assert!(reports_run_output("xin-chao", &hi));
        assert!(reports_run_output("The command fails with boom (exit 3).", &boom));
        let wc = [run("wc -l todo.txt", "2 todo.txt\n[exit 0]", false)];
        assert!(reports_run_output("The command printed 2 todo.txt.", &wc));
        let he = [run("echo hello-elisa", "hello-elisa\n[exit 0]", false)];
        assert_eq!(run_talk_cut("hello-elisa! It's nice to assist you. If you have any more questions or need further help, feel free to ask. Have a great day!", "run the command: echo hello-elisa", &he).as_deref(), Some("hello-elisa!"));
        assert_eq!(run_talk_cut("Lệnh đã in ra hello-elisa.", "chạy lệnh: echo hello-elisa", &he), None);
        let date = [run("date +%Y", "2026\n[exit 0]", false)];
        assert!(!gives_run_output("It printed the current year.", &date));
        assert!(gives_run_output("It printed 2026.", &date));
        // a failed command, or a long output, needs no quote; nor a turn with no command
        assert!(gives_run_output("It failed.", &[run("nope", "command not found", true)]));
        let long = (1..=9).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        assert!(gives_run_output("It printed nine lines.", &[run("seq 9", &long, false)]));
        assert!(gives_run_output("hi", &[]));
    }

    #[test]
    fn a_models_bracketed_asides_never_reach_saved_content() {
        assert_eq!(without_bracket_notes("Meeting code: ORCHID-42; Project: Lotus launch on Friday; [This meeting is scheduled for Friday to launch the Lotus project.] [Agenda: Review progress, a\n"),
            "Meeting code: ORCHID-42; Project: Lotus launch on Friday;\n");
        assert_eq!(without_bracket_notes("Meeting code: ORCHID-42\n[Ghi chú: cuộc họp này diễn ra vào thứ Sáu]\nProject: Lotus launch on Friday.\n"), "Meeting code: ORCHID-42\nProject: Lotus launch on Friday.\n");
        // a checkbox, a short tag, a link's text and blank lines between paragraphs stay
        assert_eq!(without_bracket_notes("- [x] buy milk\n- [ ] send report\n\nSee [the docs](https://example.com) [draft]\n"), "- [x] buy milk\n- [ ] send report\n\nSee [the docs](https://example.com) [draft]\n");
    }

    #[test]
    fn a_code_one_character_off_the_held_one_is_put_right() {
        use super::super::docs::near_miss_codes_fixed;
        let held = "notes.txt:1: Meeting code: ORCHID-42";
        assert_eq!(near_miss_codes_fixed("Trong notes.txt có: Meeting code: ORCHID-422", held).as_deref(), Some("Trong notes.txt có: Meeting code: ORCHID-42"));
        assert_eq!(near_miss_codes_fixed("The code is ORCHD-42.", held).as_deref(), Some("The code is ORCHID-42."));
        assert_eq!(near_miss_codes_fixed("Mã là orchid-43.", held).as_deref(), Some("Mã là ORCHID-42."));
        // the held code, another code far from it, a plain number or year: left as they are
        assert_eq!(near_miss_codes_fixed("The code is ORCHID-42.", held), None);
        assert_eq!(near_miss_codes_fixed("Use version v9.1 or LOTUS-7.", held), None);
        assert_eq!(near_miss_codes_fixed("It sold 421 units in 2027.", "North,420\n2026"), None);
    }

    #[test]
    fn an_earlier_message_said_back_in_any_order_is_seen() {
        use super::super::docs::says_earlier_message;
        let earlier = vec!["file app.py có lỗi gì không? xem hàm add".to_string(), "read notes.txt and tell me the meeting code".to_string()];
        assert!(says_earlier_message("xem hàm add trong app.py có lỗi hay không.", &earlier));
        assert!(says_earlier_message("Tell me the meeting code in notes.txt.", &earlier));
        // a closer's own words, or a short one: no saying-back
        assert!(!says_earlier_message("Không có gì! Chúc bạn một ngày vui vẻ.", &earlier));
        assert!(!says_earlier_message("You're welcome!", &earlier));
    }

    #[test]
    fn a_change_claim_quoting_text_the_file_lacks_is_no_telling_of_it() {
        use super::super::docs::quotes_unheld;
        let c = "1\tdef add(a, b):\n2\t    return a - b\n";
        assert!(quotes_unheld("Đã viết `return a - b` thay cho `return a + b`.", c));
        assert!(quotes_unheld("Mình đã sửa thành `return a + b`.", c));
        assert!(quotes_unheld("I changed it to \"return a + b\".", c));
        // only what the file holds: a telling of it
        assert!(!quotes_unheld("Hàm add có `return a - b`, nên nó trừ thay vì cộng.", c));
        assert!(!quotes_unheld("add returns a - b.", c));
    }

    #[test]
    fn code_given_back_is_no_judgement() {
        // Elisa's own can't-judge line said back is no judgement, in either language
        assert!(says_no_judgement("Mình chưa nhận xét được add trong app.py có lỗi hay không.", "app.py", "add"));
        assert!(says_no_judgement("I couldn't judge whether add in app.py has a mistake.", "src/app.py", "add"));
        assert!(!says_no_judgement("Hàm add có lỗi: nó trừ thay vì cộng, nên add(2, 3) trả về -1.", "app.py", "add"));
        assert!(!says_no_judgement("add subtracts b from a instead of adding them.", "app.py", "add"));
        let code = "def add(a, b):\n    return a - b";
        assert!(!judges_code("Đây là add trong app.py:\n```\ndef add(a, b):\n    return a - b\n```", code));
        assert!(!judges_code("def add(a, b):\n    return a - b", code));
        // the code with its read numbers, a fence left open
        assert!(!judges_code("1\tdef add(a, b):\n2\t    return a - b\n```", code));
        assert!(judges_code("Hàm add có lỗi: nó trừ thay vì cộng, nên add(2, 3) trả về -1.", code));
        assert!(judges_code("Yes: add subtracts b from a instead of adding them.\n```\ndef add(a, b):\n    return a - b\n```", code));
    }

    /// the best figure sentence of web text (`live_sentence`): search results by their entries, anything else as a page
    fn ls(ask: &str, web: &str) -> Option<String> {
        live_sentence(ask, web, !web.starts_with("results for")).map(|f| f.sentence)
    }

    /// an ask about the present, the figure sentence of web text, result URLs and hosts (`live_ask`, `live_sentence`)
    #[test]
    fn live_figures_from_web_text() {
        assert!(live_ask("giá vàng hôm nay thế nào nhỉ") && live_ask("what's the gold price today?") && live_ask("gia vang hom nay the nao"));
        assert!(!live_ask("giá vàng là gì?") && !live_ask("what was the gold price in 2020 today"));
        let snippets = "results for \"giá vàng hôm nay\":\n1. Giá vàng hôm nay 12/11/2026 - Báo X\n   https://www.baox.vn/gia-vang\n   Cập nhật giá vàng hôm nay mới nhất, nhanh nhất.\n2. Bảng giá vàng SJC\n   https://sjc.com.vn/\n   Xem bảng giá vàng trực tuyến.\n";
        assert_eq!(ls("giá vàng hôm nay thế nào nhỉ", snippets), None);
        assert_eq!(result_urls(snippets), ["https://www.baox.vn/gia-vang", "https://sjc.com.vn/"]);
        assert_eq!(host_of("https://www.baox.vn/gia-vang"), "baox.vn");
        let page = "title: Giá vàng hôm nay\n\nMenu\nGiá vàng SJC hôm nay mua vào 84.500.000 đồng/lượng, bán ra 86.700.000 đồng/lượng.\nNăm 2026 nhiều biến động.";
        assert_eq!(ls("giá vàng hôm nay thế nào nhỉ", page).as_deref(), Some("Giá vàng SJC hôm nay mua vào 84.500.000 đồng/lượng, bán ra 86.700.000 đồng/lượng."));
        assert_eq!(live_figures("ngày 12/11/2026 giá 84.500.000"), ["84500000"]);
        // a label, a karat, a date or a year is no value
        assert_eq!(ls("giá vàng hôm nay thế nào nhỉ", "Giá vàng hôm nay, vào 12/11 trong nước, Biến động giá vàng Sjc, 9999, 24k, 18k của PNJ."), None);
        assert_eq!(live_figures("$950 and 28°C, 5% and 2026"), ["950", "28", "5"]);
        assert_eq!(without_wrapper_tag("<answer>\nadd subtracts.\n</answer>\n"), "add subtracts.");
        assert_eq!(without_wrapper_tag("a < b"), "a < b");
        assert_eq!(ls("giá vàng hôm nay thế nào nhỉ", "Bảng Giá Vàng Hôm Nay: PNJ, SJC, Vàng 9999 ☎️1800545457 (Miễn Phí)"), None);
        // a number with an amount word after it is a value (`data/amount_words.txt`)
        assert_eq!(ls("giá vàng hôm nay thế nào nhỉ", "Giá vàng miếng SJC hôm nay là 139 triệu đồng/lượng.").as_deref(), Some("Giá vàng miếng SJC hôm nay là 139 triệu đồng/lượng."));
        for (s, n) in [("84,5 triệu đồng", "845"), ("2.3 million people", "23"), ("1.200 USD", "1200"), ("5 nghìn", "5"), ("3 tỷ", "3"), ("3 ty dong", "3")] {
            assert_eq!(live_figures(s), [n], "{s}");
        }
        assert!(live_figures("vàng 24k và 18k, 3 loại").is_empty());
        // Elisa's honest lines come from data/no_answer_lines.txt, in the ask's language
        let vi = crate::reply_check::own_line_of("no_figure", "giá vàng hôm nay thế nào nhỉ", "").replace("{sites}", "sjc.com.vn");
        assert!(vi.starts_with("Mình chưa tìm thấy") && vi.contains("sjc.com.vn"), "{vi}");
        let en = crate::reply_check::own_line_of("no_judge", "is there a mistake in add?", "app.py").replace("{sym}", "add");
        assert_eq!(en, "I couldn't judge whether add in app.py has a mistake. Here is its code:");
        assert_eq!(ls("what's the gold price today?", "Gold price today: $2,345 per ounce"), Some("Gold price today: $2,345 per ounce.".to_string()));
    }

    /// the figure of the quantity asked, never another quantity's; degree forms as figures; a copied result; a figure
    /// dated before today (`live_sentence`, `live_figures`, `copied_results`, `figure_ask`, `dated_before_today`)
    #[test]
    fn live_figures_quantity_degrees_dates() {
        // asks for a figure: about the present, a number, a named quantity; EN and VI, with and without accents
        for a in ["look up the population of Iceland online", "tìm trên mạng: dân số Iceland là bao nhiêu?", "dan so Iceland la bao nhieu",
            "what's the exchange rate of the euro?", "thời tiết Hà Nội hôm nay thế nào?", "nhiet do Da Lat", "what's the height of Everest"] {
            assert!(figure_ask(a), "{a}");
        }
        for a in ["look up flask docs online", "tìm trên mạng: phim hay", "đánh giá phim Mai", "what was the population in 2020"] { assert!(!figure_ask(a), "{a}"); }
        // another quantity's figure (urban count, density, rank, growth) is never the one asked
        let pop = "Currently, 94.3% of the population of Iceland is urban (379,229 people in 2026).\nThe 2026 population density in Iceland is 4 people per km².\nIceland ranks number 180 in the list of countries by population.\nThe current population of Iceland is 403,547 people.";
        assert_eq!(ls("curious what the population of Iceland is these days?", pop).as_deref(), Some("The current population of Iceland is 403,547 people."));
        assert_eq!(ls("look up the population of Iceland online", pop).as_deref(), Some("The current population of Iceland is 403,547 people."));
        let vi = "Dân số thành thị của Iceland là 379.229 người.\nIceland đứng thứ 180 trên thế giới, với 403.547 người.\nDân số Iceland hiện tại là 403.547 người.";
        assert_eq!(ls("dân số Iceland là bao nhiêu?", vi).as_deref(), Some("Dân số Iceland hiện tại là 403.547 người."));
        assert_eq!(ls("dan so Iceland la bao nhieu", "Mat do dan so Iceland la 4 nguoi/km2.\nDan so Iceland la 403.547 nguoi."), Some("Dan so Iceland la 403.547 nguoi.".to_string()));
        // (the ask naming that quantity takes it: "urban population")
        assert_eq!(ls("what is the urban population of Iceland?", pop).as_deref(), Some("Currently, 94.3% of the population of Iceland is urban (379,229 people in 2026)."));
        // (a share in % beside the asked figure is no other quantity)
        assert_eq!(ls("gold price today?", "Gold price today is $2,345 per ounce, up 0.5%."), Some("Gold price today is $2,345 per ounce, up 0.5%.".to_string()));
        // temperature and degree forms count as figures; a one-letter word written on ("24k") does not
        for (s, n) in [("Hà Nội 21oC, nhiều mây", "21"), ("nhiệt độ 21 độ C", "21"), ("high of 75°F", "75"), ("it is 5km away", "5"), ("vàng 139tr/lượng", "139"), ("about 3 km", "3")] {
            assert_eq!(live_figures(s), [n], "{s}");
        }
        assert!(live_figures("vàng 24k, 18k và 10k").is_empty());
        let page = "title: Thời tiết Hà Nội\nDự báo thời tiết Hà Nội hôm nay\nHà Nội hôm nay 21oC, trời nhiều mây, độ ẩm 80%.";
        assert_eq!(ls("thời tiết Hà Nội hôm nay thế nào?", page).as_deref(), Some("Hà Nội hôm nay 21oC, trời nhiều mây, độ ẩm 80%."));
        assert_eq!(ls("what's the weather in London today?", "London today: 12°C and light rain."), Some("London today: 12°C and light rain.".to_string()));
        // a reply only copying a result's title and link answers nothing; one with words of its own does
        let res = "results for \"Iceland population\":\n1. Iceland Population\n   https://www.worldpopulationclock.net/iceland-population/\n   Iceland has a population of 403,547.";
        assert!(copied_results(" 1. Iceland Population https://www.worldpopulationclock.net/iceland-population/", res));
        assert!(!copied_results("Iceland has about 403,547 people (source: https://www.worldpopulationclock.net/iceland-population/).", res));
        assert!(!copied_results("Iceland Population", res));
        // a dated figure before today by the clock, in each language's date order; this year's when the year is left out
        let nov12 = 1_794_484_800;
        assert_eq!(crate::situation::dated_before_today("Sáng 13/10, giá vàng thế giới ở mức 4.168 USD/ounce.", true, nov12).as_deref(), Some("13/10"));
        assert_eq!(crate::situation::dated_before_today("Hôm nay (13/10), giá vàng miếng SJC ở mức 143,5 triệu đồng/lượng.", true, nov12).as_deref(), Some("13/10"));
        assert_eq!(crate::situation::dated_before_today("Gold hit $4,168 an ounce on Oct 13.", false, nov12).as_deref(), Some("Oct 13"));
        assert_eq!(crate::situation::dated_before_today("Gold hit $4,168 an ounce on 2026-10-13.", false, nov12).as_deref(), Some("2026-10-13"));
        assert_eq!(crate::situation::dated_before_today("Giá vàng ngày 12/11 là 143,5 triệu.", true, nov12), None);
        assert_eq!(crate::situation::dated_before_today("Gold is $4,168 an ounce on November 12, 2026.", false, nov12), None);
        assert_eq!(crate::situation::dated_before_today("Gold is $4,168 an ounce.", false, nov12), None);
        // (a day after today without its year is last year's: on 12 Nov, "20/12" was last December)
        assert_eq!(crate::situation::dated_before_today("Giá vàng 20/12 là 80 triệu.", true, nov12).as_deref(), Some("20/12"));
        let en = crate::reply_check::own_line_of("dated_figure", "what's the gold price today?", "").replace("{date}", "Oct 13").replace("{figure}", "Gold hit $4,168").replace("{source}", "kitco.com");
        assert!(en.contains("Oct 13") && en.contains("not today") && en.contains("kitco.com"), "{en}");
        let vi = crate::reply_check::own_line_of("dated_figure", "giá vàng hôm nay thế nào nhỉ", "").replace("{date}", "13/10").replace("{figure}", "x").replace("{source}", "24h.com.vn");
        assert!(vi.contains("13/10") && vi.contains("chưa phải hôm nay"), "{vi}");
    }

    /// the asked quantity's current value by the sentence's shape (`live_candidates`): a share, a range, another kind's
    /// figure or a historical year is not it; a figure's date by its snippet or page line; the site it came from
    /// (`figure_spans`, `ask_kinds`, `subject_words`, `figure_range`, `dated_when`, `web_blocks`, `latest_dated`)
    #[test]
    fn live_figures_kind_date_source_range() {
        // the kind a unit gives a figure, EN and VI; a plain number has none
        for (s, k) in [("403.547 người", Some("count")), ("379,229 people", Some("count")), ("2.3 million people", Some("count")), ("$2,345 per ounce", Some("money")),
            ("143,5 triệu đồng/lượng", Some("money")), ("4.168 USD/ounce", Some("money")), ("28°C", Some("degrees")), ("21oC", Some("degrees")), ("18 độ C", Some("degrees")),
            ("0,00%", Some("share")), ("94.3%", Some("share")), ("8,849 m", Some("length")), ("5km", Some("length")), ("103.000 km²", Some("area")), ("394,324", None), ("139 triệu", None)] {
            assert_eq!(figure_kind(s).as_deref(), k, "{s}");
        }
        // the kinds an ask takes, and the words that can name its subject
        assert_eq!(ask_kinds("curious what the population of Iceland is these days, could you look it up?").0, ["count"]);
        assert_eq!(ask_kinds("giá vàng hôm nay thế nào nhỉ").0, ["money"]);
        assert_eq!(ask_kinds("đánh giá phim Mai").0, Vec::<&str>::new());
        assert_eq!(ask_kinds("nhiệt độ Đà Lạt hôm nay bao nhiêu độ?").0, ["degrees"]);
        assert_eq!(ask_kinds("what's the height of Everest").0, ["length"]);
        let subj = subject_words("giá vàng hôm nay thế nào nhỉ");
        assert!(subj.contains(&"vàng".to_string()) && !subj.contains(&"giá".to_string()) && !subj.contains(&"nay".to_string()), "{subj:?}");
        assert_eq!(subject_words("dân số Iceland là bao nhiêu?"), ["iceland"]);
        // a range of two figures is no single value; a buy and a sell price side by side are
        assert!(figure_range("The population of Iceland probably wavered between about 30,000 and 80,000 for most of the time since settlement."));
        assert!(figure_range("Trong lịch sử, dân số Iceland dao động từ 40.000 đến 60.000 người cho đến giữa thế kỷ XIX."));
        assert!(figure_range("Official statistics begin in 1703, since when the population has grown from 50,358 to 394,234."));
        assert!(!figure_range("Giá vàng SJC hôm nay mua vào 84.500.000 đồng/lượng, bán ra 86.700.000 đồng/lượng."));
        assert!(!figure_range("Gold price today is $2,345 per ounce, up 0.5%."));
        // a page: the current single value, not the historical range holding more words of the ask, nor the share
        let wiki = "title: Demographics of Iceland - Wikipedia\n\nPopulation394,324 (2026)\n\nGrowth rate0.93% (2022 est.)\n\nAs of 2026, the Icelandic population is 394,324. In 2026, 77,226 residents (19.6%) were immigrants.[2]\n\nThe population of Iceland probably wavered between about 30,000 and 80,000 for most of the time since settlement. Official statistics begin in 1703, since when the population has grown from 50,358 to 394,234 (1 January 2026).[3][4]";
        let best = live_sentence("curious what the population of Iceland is these days, could you look it up?", wiki, true).unwrap();
        assert_eq!(best.sentence, "As of 2026, the Icelandic population is 394,324.");
        assert_eq!((best.when.clone(), best.subject, best.single), (When::Undated, true, true));
        // VI results: the count, dated by its snippet, never the world share beside it; the figure names its own result's site
        let danso = "results for \"dân số Iceland\":\n1. Dân số Iceland mới nhất (2026) - cập nhật hằng ngày - DanSo.Org\n   https://danso.org/iceland/\n   Thông tin nhanh Dân số hiện tại của Iceland là 403.547 người vào ngày 11/11/2026 theo số liệu mới nhất từ Liên Hợp Quốc. Dân số Iceland hiện chiếm 0,00% dân số thế giới. Iceland đang đứng thứ 180 trên thế giới trong bảng xếp hạng dân số các nước và vùng lãnh thổ.\n2. Iceland - Wikipedia tiếng Việt\n   https://vi.wikipedia.org/wiki/Iceland\n   Cuộc điều tra dân số đầu tiên ở Iceland diễn ra vào năm 1703, lúc đó dân số nước này là 50.358 người.\n(web_fetch a url to read the page)";
        for ask in ["search giúp mình dân số Iceland", "tìm trên mạng: dân số Iceland là bao nhiêu?"] {
            let best = live_sentence(ask, danso, false).unwrap();
            assert!(best.sentence.contains("403.547 người") && !best.sentence.contains("0,00%"), "{ask}: {}", best.sentence);
            assert_eq!(best.source.as_deref(), Some("danso.org"));
            assert!(matches!(&best.when, When::Before { date, .. } if date == "11/11/2026"), "{:?}", best.when);
        }
        assert!(live_candidates("dân số Iceland là bao nhiêu?", danso, false).iter().all(|c| !c.sentence.contains("0,00%")));
        // the share sentence alone gives nothing for a count ask; it does for an ask naming a share
        assert_eq!(ls("dân số Iceland là bao nhiêu?", "Dân số Iceland hiện chiếm 0,00% dân số thế giới."), None);
        assert_eq!(ls("Iceland chiếm bao nhiêu phần trăm dân số thế giới?", "Dân số Iceland hiện chiếm 0,00% dân số thế giới.").as_deref(), Some("Dân số Iceland hiện chiếm 0,00% dân số thế giới."));
        // a snippet's figure is dated by its whole snippet, not its own sentence: "Sáng 13/10" dates the SJC price too
        let gold = "results for \"giá vàng hôm nay\":\n1. Giá vàng hôm nay các thương hiệu Sjc, 9999, 24k, 18k của PNJ, Doji mới nhất\n   https://www.24h.com.vn/gia-vang-hom-nay-c425.html\n   Sáng nay, giá vàng trong nước đồng loạt quay đầu giảm theo giá vàng thế giới. Theo đó, giá bán vàng miếng SJC về mốc 143,5 triệu đồng/lượng. Sáng 13/10, giá vàng thế giới được niêm yết trên Kitco ở mức 4.168 USD/ounce, giảm 16 USD/ounce so với đầu giờ sáng qua.\n2. Giá Vàng Hôm Nay 12/11/2026 - SJC, Nhẫn, Thế Giới\n   https://giavang.com.vn/gia-vang-hom-nay/\n   Giá vàng SJC, nhẫn 9999, nữ trang và XAU/USD hôm nay cập nhật liên tục mỗi 5 phút.";
        let best = live_sentence("giá vàng hôm nay thế nào nhỉ", gold, false).unwrap();
        assert!(best.sentence.contains("triệu đồng/lượng") || best.sentence.contains("USD/ounce"), "{}", best.sentence);
        assert!(matches!(&best.when, When::Before { date, .. } if date == "13/10"), "{:?}", best.when);
        assert_eq!(best.source.as_deref(), Some("24h.com.vn"));
        assert!(live_candidates("giá vàng hôm nay thế nào nhỉ", gold, false).iter().all(|c| matches!(c.when, When::Before { .. })));
        // a page figure said as today's ("hôm nay") outranks the dated snippet's; one with no day at all ranks below it
        let page = live_sentence("giá vàng hôm nay thế nào nhỉ", "Giá vàng SJC hôm nay niêm yết 145,2 triệu đồng/lượng.", true).unwrap();
        assert!(page.rank() > best.rank());
        let undated = live_sentence("giá vàng hôm nay thế nào nhỉ", "Giá vàng SJC niêm yết 145,2 triệu đồng/lượng.", true).unwrap();
        assert!(undated.rank() < best.rank());
        // the site is the result the figure came from, not the first result's
        let results = "results for \"the population of Iceland online\":\n1. Iceland Population (2026) - Worldometer\n   https://www.worldometers.info/world-population/iceland-population/\n   Population of Iceland: current, historical, and projected population, growth rate, immigration, median age.\n2. Iceland Population 2026 — Live Count\n   https://worldpopulation.online/country/iceland\n   As of 2026, the population of Iceland is 402,329, ranking #179 in the world (0.01 % of the global population), based on UN World Population Prospects 2024. The population is growing at 0.92% per year.\n3. Iceland Population: Dated Figures and Definitions\n   https://www.iceland.org/population\n   Iceland's population was 396,500 on June 30, 2026, according to Statistics Iceland's quarterly estimate.\n4. Iceland Population 2026\n   https://worldpopulationreview.com/countries/iceland\n   Iceland is home to an estimated 403,803 people, the 179th largest population in the world. Iceland makes up < 0.01% of the world's population and 0.05% of Europe's.";
        // (the dated figure of a result lower down wins over the undated ones above it)
        let best = live_sentence("look up the population of Iceland online", results, false).unwrap();
        assert_eq!(best.sentence, "Iceland's population was 396,500 on June 30, 2026, according to Statistics Iceland's quarterly estimate.");
        assert_eq!(best.source.as_deref(), Some("iceland.org"));
        assert_eq!(web_blocks(results, false).len(), 4);
        // other quantities, EN and VI: the figure of the kind asked, about the asked subject, never another kind's
        assert_eq!(ls("how much does gold cost today?", "Gold rose 0.5% today.\nGold costs $2,345 per ounce today."), Some("Gold costs $2,345 per ounce today.".to_string()));
        assert_eq!(ls("giá xăng hôm nay bao nhiêu?", "Giá xăng hôm nay tăng 2,3%.\nGiá xăng RON95 hôm nay là 21.500 đồng/lít."), Some("Giá xăng RON95 hôm nay là 21.500 đồng/lít.".to_string()));
        assert_eq!(ls("what's the temperature in Hanoi today?", "Hanoi has 8,500,000 people.\nHanoi today: 21°C with 80% humidity."), Some("Hanoi today: 21°C with 80% humidity.".to_string()));
        assert_eq!(ls("nhiệt độ Đà Lạt hôm nay bao nhiêu độ?", "Đà Lạt đón 2.000 lượt khách.\nĐà Lạt hôm nay 18 độ C, độ ẩm 90%."), Some("Đà Lạt hôm nay 18 độ C, độ ẩm 90%.".to_string()));
        assert_eq!(ls("how tall is Everest?", "Climbing Everest costs $11,000 in permits.\nEverest stands 8,849 m above sea level."), Some("Everest stands 8,849 m above sea level.".to_string()));
        assert_eq!(ls("chiều cao của Fansipan là bao nhiêu?", "Fansipan đón 2.000 lượt khách mỗi ngày.\nFansipan cao 3.143 m so với mực nước biển."), Some("Fansipan cao 3.143 m so với mực nước biển.".to_string()));
        assert_eq!(ls("how many people live in Reykjavik?", "Reykjavik covers 273 km².\nHousing prices rose 12% in Reykjavik.\nReykjavik has 139,875 inhabitants."), Some("Reykjavik has 139,875 inhabitants.".to_string()));
        assert_eq!(ls("Hà Nội có bao nhiêu dân?", "Hà Nội rộng 3.359 km².\nHà Nội có 8,5 triệu dân."), Some("Hà Nội có 8,5 triệu dân.".to_string()));
        // a different subject's figure comes after the asked subject's
        assert_eq!(ls("population of Iceland these days?", "The population of Greenland is 56,000 people.\nThe population of Iceland is 394,324 people."), Some("The population of Iceland is 394,324 people.".to_string()));
        // a bare year before this one dates a figure, this year's does not; a figure is no year; a day counts its days back
        let nov12 = 1_794_484_800;
        let d = crate::situation::latest_dated("Population in 2020 was 364,134.", false, nov12).unwrap();
        assert_eq!((d.written.as_str(), d.before, d.day), ("2020", true, false));
        let d = crate::situation::latest_dated("As of 2026, the Icelandic population is 394,324.", false, nov12).unwrap();
        assert_eq!((d.written.as_str(), d.before), ("2026", false));
        let d = crate::situation::latest_dated("Dân số hiện tại là 403.547 người vào ngày 11/11/2026.", true, nov12).unwrap();
        assert_eq!((d.written.as_str(), d.before, d.day, d.days_ago), ("11/11/2026", true, true, 1));
        assert_eq!(dated_when("Lễ hội thu hút 2000 người."), When::Undated);
        assert!(matches!(dated_when("The population in 2020 was 364,134."), When::Before { ref date, .. } if date == "2020"));
    }

    /// weather claims against the page read (`condition_classes`, `page_conditions`, `claimed_conditions`,
    /// `weather_rewrite`, `situation::day_offset`), and a reply mixing an older figure with the latest (`figures_stand`)
    #[test]
    fn weather_claims_against_the_page_and_mixed_figures() {
        let set = |v: Vec<String>| v.into_iter().collect::<HashSet<String>>();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<String>>();
        // conditions by wording: the longest first, none right before a number, a line of one alone
        assert_eq!(condition_classes("Giờ ở Hà Nội là 19°C, trời mưa nhẹ và có gió nhẹ.").0, ["rain"]);
        assert_eq!(set(condition_classes("U ám, mây che phủ 100%, không mưa.").0), names(&["cloudy", "dry"]));
        assert!(condition_classes("Nắng 7g 51p").0.is_empty() && condition_classes("Khả năng mưa 94%").0.is_empty());
        assert_eq!(condition_classes("Lượng mưa 0 mm").0, ["dry"]);
        assert_eq!(condition_classes("U ám"), (vec!["cloudy".to_string()], false));
        assert_eq!(condition_classes("Tomorrow: sunny spells, high 25°C").0, ["sunny"]);
        let (cs, letters) = condition_classes("It is lightly raining with no sunshine.");
        assert!(set(cs) == names(&["rain", "sunny"]) && letters);
        assert!(conditions_clash("sunny", "rain") && conditions_clash("rain", "dry") && !conditions_clash("cloudy", "rain"));
        // the day a sentence names
        assert_eq!(crate::situation::day_offset("Ngày mai dự báo nắng, nhiệt độ lên đến 23°C."), Some(1));
        assert_eq!(crate::situation::day_offset("tomorrow will be wet"), Some(1));
        assert_eq!(crate::situation::day_offset("the day after tomorrow"), Some(2));
        assert_eq!(crate::situation::day_offset("Giờ ở Hà Nội là 19°C"), None);
        // a page's conditions by day: the headline and the readings today, the summary sentences for their day, the climate paragraph for none
        let page = "title: Thời tiết Hà Nội\n\nHà Nội\n\nCập nhật lúc 21:04\n\n19°C\n\nU ám\n\nCảm giác như 21°C\n\nĐộ ẩm\n\n84%\n\nMây che phủ\n\n100%\n\nLượng mưa\n\n0 mm\n\nTóm tắt thời tiết\n\nTối nay bầu trời phủ mây, thời tiết mát mẻ 19°C.\n\nThời tiết lý tưởng cho hoạt động ngoài trời.\n\nNgày mai thời tiết tương tự, khoảng 23°C kèm theo mưa.\n\nMặt trời & Ánh sáng\n\nNắng\n\n7g 51p\n\nBiểu đồ mưa\n\nHôm nay 92 %18°C\n\nMùa mưa tập trung từ tháng 5 đến tháng 10 với lượng mưa trung bình 1.500-2.000mm/năm.";
        let shown = page_conditions(page);
        let degs = page_degrees(page);
        let on = |d: i64| shown.iter().filter(|x| x.day == d).map(|x| x.name.clone()).collect::<HashSet<String>>();
        assert_eq!(on(0), names(&["cloudy", "dry"]), "{shown:?}");
        assert_eq!(on(1), names(&["rain"]), "{shown:?}");
        assert!(shown.iter().any(|x| x.day == 1 && x.sentence.as_deref() == Some("Ngày mai thời tiết tương tự, khoảng 23°C kèm theo mưa.")));
        // a claim of light rain today against overcast and 0 mm: nothing of that one-sentence reply stands
        assert_eq!(weather_rewrite("Giờ ở Hà Nội là 19°C, trời mưa nhẹ và có gió nhẹ.", &shown, &degs, 0).as_deref(), Some(""));
        // a claim of sun tomorrow against the page's rain: the page's sentence in its place, the right lines kept
        let reply = "Hôm nay ở Hà Nội:\n- **Thời tiết**: U ám, mây che phủ 100%, không mưa.\n- **Nhiệt độ**: 19°C (cảm giác như 21°C).\n\nNgày mai dự báo nắng, nhiệt độ lên đến 23°C.";
        let fixed = weather_rewrite(reply, &shown, &degs, 0).unwrap();
        assert!(fixed.contains("U ám, mây che phủ 100%, không mưa.") && fixed.contains("19°C (cảm giác như 21°C)"), "{fixed}");
        assert!(!fixed.contains("dự báo nắng") && fixed.ends_with("Ngày mai thời tiết tương tự, khoảng 23°C kèm theo mưa."), "{fixed}");
        // a reply the page bears out stands; one claiming no condition while the page shows one answers nothing
        assert_eq!(weather_rewrite("Hà Nội hôm nay u ám, 19°C, không mưa. Ngày mai khoảng 23°C, có mưa.", &shown, &degs, 0), None);
        assert_eq!(weather_rewrite("Hà Nội hôm nay 19°C, độ ẩm 84%.", &shown, &degs, 0).as_deref(), Some(""));
        // a sentence dated to another day than the one asked goes, whatever it claims; the rest stands on its own
        assert!(sentence_day("Thời tiết Hà Nội vào 14/11/2020 là 19°C, u ám.", true).is_some_and(|d| d < 0) && sentence_day("Năm 2020 trời u ám.", true).is_none());
        assert!(sentence_day("Hanoi was overcast at 18°C on 11/14/2020.", false).is_some_and(|d| d < 0) && sentence_day("Hanoi is overcast at 18°C.", false).is_none());
        assert_eq!(weather_rewrite("Thời tiết Hà Nội vào 14/11/2020 là 19°C, u ám.", &shown, &degs, 0).as_deref(), Some("Tối nay bầu trời phủ mây, thời tiết mát mẻ 19°C."));
        let fixed = weather_rewrite("Dự báo ngày 14/11/2020 trời nắng. Hôm nay Hà Nội u ám, 19°C.", &shown, &degs, 0).unwrap();
        assert!(!fixed.contains("nắng") && fixed.ends_with("Hôm nay Hà Nội u ám, 19°C."), "{fixed}");
        // a page writing its condition alone over its reading ("Mưa vừa" / "18oC") shows rain today; a reply dated to
        // another day with no condition answers nothing, and the page's own sentence is the readings under the heading
        let rainy = "title: Dự báo thời tiết Hà Nội hôm nay\n\nThời tiết Hà Nội\n\n--:--\n\nMưa vừa\n\n18oC\n\n18oC\n/\n22oC\n\n|\n\n2.37 mm";
        let shown_rain = page_conditions(rainy);
        let degs_rain = page_degrees(rainy);
        assert!(shown_rain.iter().any(|x| x.day == 0 && x.name == "rain"), "{shown_rain:?}");
        assert_eq!(weather_rewrite("Thời tiết Hà Nội vào 14/11/2020 được ghi nhận ở 18oC với chất lượng không khí tốt.", &shown_rain, &degs_rain, 0).as_deref(), Some(""));
        assert_eq!(ls("thời tiết Hà Nội hôm nay thế nào?", rainy).as_deref(), Some("Thời tiết Hà Nội: Mưa vừa 18oC."));
        // the same in English, with a reading and a sentence per day; the day asked is the ask's ("tomorrow": 1)
        let en = "Tonight: Overcast. Low 18°C.\n\nPrecipitation\n\n0 mm\n\nTomorrow: Showers likely, high 25°C.";
        let shown = page_conditions(en);
        let degs_en = page_degrees(en);
        assert_eq!(on(0).len(), 2);
        let shown_en = shown;
        assert_eq!(weather_rewrite("It's lightly raining in Hanoi now at 19°C. Tomorrow looks sunny.", &shown_en, &degs_en, 0).as_deref(), Some(""));
        assert_eq!(weather_rewrite("Hanoi is overcast tonight at 18°C, and tomorrow brings showers.", &shown_en, &degs_en, 0), None);
        // a temperature the page gives none of for that day goes with its sentence (the page's sentence in its place)
        assert_eq!(weather_rewrite("Hanoi is overcast tonight at 19°C, and tomorrow brings showers.", &shown_en, &degs_en, 0).as_deref(), Some("Tonight: Overcast."));
        assert_eq!(weather_rewrite("On 11/14/2020 Hanoi was overcast at 18°C.", &shown_en, &degs_en, 0).as_deref(), Some("Tonight: Overcast."));
        assert_eq!(weather_rewrite("Tomorrow: showers, high 25°C.", &shown_en, &degs_en, 1), None);
        assert_eq!(weather_rewrite("Tomorrow: showers, high 25°C.", &shown_en, &degs_en, 0).as_deref(), Some(""));
        // a reply mixing an older count with the latest is no answer; the latest alone, or a figure of no candidate beside it, is
        let held = names(&["403547", "347736", "402894", "2026"]);
        let good = names(&["403547"]);
        let values = names(&["403547", "347736", "402894"]);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        let mut holders: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
        for (n, site) in [("403547", "danso.example"), ("347736", "hoidap.example"), ("402894", "hoidap.example")] { holders.entry(n.to_string()).or_default().insert(site.to_string()); }
        assert!(!figures_stand(&s(&["347736", "403547"]), &held, &good, &values, &holders));
        assert!(figures_stand(&s(&["403547"]), &held, &good, &values, &holders));
        assert!(figures_stand(&s(&["403547", "2026"]), &held, &good, &values, &holders));
        assert!(!figures_stand(&s(&["347736"]), &held, &good, &values, &holders) && !figures_stand(&s(&["403547", "999999"]), &held, &good, &values, &holders));
    }

    /// the newest dated figure over an undated or older one (`dated_when` by a now word, `LiveFigure::fresh`,
    /// `fresh_figures`), values mixed from two sites (`figures_stand`), and the results' sentence by meaning
    /// (`meaning_sentence`; checks only with the embedding model installed)
    #[test]
    fn newest_dated_figure_mixed_sites_and_meaning_sentence() {
        // a block with no day but a now word says now (its own today, under a day written as today); "as of 2026 (live estimate)" says nothing
        assert_eq!(dated_when("Dân số hiện tại của Iceland là 403.547 người theo số liệu mới nhất."), When::Now);
        assert_eq!(dated_when("Giá vàng SJC hôm nay mua vào 84.500.000 đồng/lượng."), When::Now);
        assert_eq!(dated_when("As of 2026, the population of Iceland is approximately 380,317 (live estimate)."), When::Undated);
        assert!(matches!(dated_when("Iceland's population was 396,500 on June 30, 2020."), When::Before { .. }));
        // the English results: the dated sentence wins over the undated live estimate, and gives its newer figure
        let web = "results for \"population of Iceland online\":\n1. Iceland Population 2026 - Live Counter\n   https://liveworldstats.example/country/is\n   As of 2026, the population of Iceland is approximately 380,317 (live estimate), growing at 0.9% per year.\n2. Iceland Population 2026 - WorldPopulation\n   https://worldpopulation.example/country/iceland\n   As of 2026, the population of Iceland is 402,329, based on UN World Population Prospects 2024. The population is growing at 0.92% per year.\n3. Demographics of Iceland - Wikipedia\n   https://wikipedia.example/wiki/Demographics_of_Iceland\n   As of 2026, the Icelandic population is 394,324. In 2026, 77,226 residents (19.6%) were immigrants.\n4. Iceland Population: Dated Figures\n   https://iceland.example/population\n   Iceland's population was 396,500 on June 30, 2020, according to Statistics Iceland's quarterly estimate, and 394,324 on January 1, 2020, in its separate annual estimate.";
        let ask = "look up the population of Iceland online";
        let best = live_sentence(ask, web, false).unwrap();
        assert!(best.sentence.starts_with("Iceland's population was 396,500 on June 30, 2020") && best.source.as_deref() == Some("iceland.example"), "{best:?}");
        assert_eq!(fresh_figures(&best.sentence), ["396500"]);
        assert_eq!(fresh_figures("Dân số Iceland là 403.547 người vào 14/9/2020, so với 347.736 người vào 1/1/2020."), ["403547"]);
        assert_eq!(fresh_figures("The population is 394,324 (2026) and 77,226 are immigrants."), ["394324", "77226"]);
        // a today figure beats the dated one; the Vietnamese ask keeps its now-worded latest figure over an older dated one
        let vi_web = "results for \"dân số Iceland\":\n1. Dân số Iceland\n   https://danso.example/iceland/\n   Dân số hiện tại của Iceland là 403.547 người theo số liệu mới nhất từ Liên Hợp Quốc.\n2. Hỏi đáp\n   https://hoidap.example/iceland\n   Dân số Iceland được ước tính khoảng 347.736 người vào ngày 14 tháng 9 năm 2020.";
        assert!(live_sentence("dân số Iceland là bao nhiêu?", vi_web, false).unwrap().sentence.starts_with("Dân số hiện tại của Iceland là 403.547"));
        // values from two sites in one reply do not stand: 394,324 (wikipedia) with 0.92% (worldpopulation)
        let all = live_candidates(ask, web, false);
        let held: HashSet<String> = live_figures(web).into_iter().collect();
        let good: HashSet<String> = fresh_figures(&best.sentence).into_iter().collect();
        let values: HashSet<String> = all.iter().flat_map(|c| live_figures(&c.sentence)).collect();
        let mut holders: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
        web_holders(web, None, &mut holders);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        assert!(!figures_stand(&s(&["394324", "092"]), &held, &good, &values, &holders));
        assert!(figures_stand(&s(&["396500"]), &held, &good, &values, &holders));
        // (the latest figure with a rate from another site: "396,500 … growing at 0.92% per year" does not stand)
        assert_eq!(holders.get("092").cloned().unwrap_or_default(), ["worldpopulation.example".to_string()].into_iter().collect::<HashSet<String>>());
        assert!(!figures_stand(&s(&["396500", "092"]), &held, &good, &values, &holders));
        assert!(!figures_stand(&s(&["394324"]), &held, &good, &values, &holders) && !figures_stand(&s(&["380317"]), &held, &good, &values, &holders));
        let good_both: HashSet<String> = ["402329".to_string(), "092".to_string()].into_iter().collect();
        assert!(figures_stand(&s(&["402329", "092"]), &held, &good_both, &values, &holders), "one site holds both");
        // the results' sentence by meaning: the winner, not the venue sentence; the capital, not the population
        if elisa_mem::embed::model_dir().is_none() { return; }
        let fifa = "results for \"who won the 2022 fifa world cup\":\n1. 2022 FIFA World Cup final - Wikipedia\n   https://wiki.example/final\n   The final match of the 2022 FIFA World Cup, the 22nd edition of FIFA's competition for men's national football teams, was played at Lusail Stadium in Lusail, Qatar, on 18 December 2022, and was contested by Argentina and defending champions France.\n2. 2022 FIFA World Cup - Britannica\n   https://britannica.example/world-cup\n   The 2022 FIFA World Cup was an international football tournament that took place in Qatar from November 20 to December 18, 2022. Argentina defeated France in the final match to win its third World Cup title.";
        let got = meaning_sentence("can u serach the web for who won the 2022 fifa world cup", fifa);
        assert!(got.as_deref().is_some_and(|s| s.contains("Argentina defeated France")), "{got:?}");
        // (five results, "2022" and "FIFA" in fewer than half the sentences but in every title: still the subject)
        let five = "results for \"who won the 2022 fifa world cup\":\n1. 2022 FIFA World Cup final - Wikipedia\n   https://wiki.example/final\n   The final match of the 2022 FIFA World Cup, the 22nd edition of FIFA's competition for men's national football teams, was played at Lusail Stadium in Lusail, Qatar, on 18 December 2022, and was contested by Argentina and defending champions France.\n2. 2022 FIFA World Cup - Wikipedia\n   https://wiki.example/cup\n   Argentina were crowned the champions after winning the final against the title holder France 4-2 on penalties following a 3-3 draw after extra time. It was Argentina's third title and their first since 1986. Argentina were the first nation from outside of Europe to win the tournament since 2002.\n3. FIFA World Cup Qatar 2022\n   https://fifa.example/qatar2022\n   Discover the story of the FIFA World Cup Qatar 2022, from standout moments to defining results, all in one place.\n4. 2022 FIFA World Cup - Britannica\n   https://britannica.example/world-cup\n   The 2022 FIFA World Cup was an international football tournament that took place in Qatar from November 20 to December 18, 2022. Argentina defeated France in the final match to win its third World Cup title.\n5. Who won the 2022 FIFA World Cup? Final score, result and highlights\n   https://sport.example/who-won\n   Final score: Argentina 3, France 3 (Argentina won 4-2 on penalties) Argentina beat France on penalties in the final at the Lusail Iconic Stadium.";
        let got = meaning_sentence("can u serach the web for who won the 2022 fifa world cup", five);
        assert!(got.as_deref().is_some_and(|s| s.contains("Argentina") && ["won", "winning", "win "].iter().any(|w| s.contains(w))), "{got:?}");
        let uc = "results for \"thủ đô của Úc\":\n1. Canberra - Wikipedia tiếng Việt\n   https://vi.wikipedia.example/wiki/Canberra\n   Canberra có dân số 381.488 người, là thành phố nội lục lớn nhất và thành phố đông dân thứ tám của Úc.\n2. Thủ đô của Úc\n   https://dulich.example/uc\n   Thủ đô của Úc là Canberra, thành phố lớn nhất là Sydney. Úc có ngôn ngữ quốc gia là tiếng Anh.";
        let got = meaning_sentence("thủ đô của Úc là gì? tra google giúp mình", uc);
        assert_eq!(got.as_deref(), Some("Thủ đô của Úc là Canberra, thành phố lớn nhất là Sydney."), "{got:?}");
        // a frame alone ("what is") asks no relation beyond the subject: the sentence holding most of the ask's words
        let au = "results for \"capital of australia\":\n1. Canberra - Wikipedia\n   https://wikipedia.example/wiki/Canberra\n   Canberra has a population of 381,488 and is the largest inland city of Australia.\n2. Australia's capital\n   https://travel.example/au\n   Canberra is the capital of Australia. Sydney is its largest city.";
        let got = meaning_sentence("what is the capital of australia?", au);
        assert_eq!(got.as_deref(), Some("Canberra is the capital of Australia."), "{got:?}");
        // a match page's score line is no sentence (`table_line`): the winner's sentence, not "Argentina (4) 3 - 3 (2) France …"
        assert!(table_line("Argentina (4) 3 - 3 (2) France Argentina won 4 - 2 on penalties Full Time 108' Lionel MESSI"));
        assert!(!table_line("Canberra có dân số 381.488 người, là thành phố nội lục lớn nhất và thành phố đông dân thứ tám của Úc.") && !table_line("Final score: Argentina 3, France 3 (Argentina won 4-2 on penalties) Argentina beat France on penalties."));
        let live = "results for \"who won the 2022 fifa world cup\":\n1. 2022 FIFA World Cup final - Wikipedia\n   https://wiki.example/final\n   The final match of the 2022 FIFA World Cup, the 22nd edition of FIFA 's competition for men's national football teams, was played at Lusail Stadium in Lusail, Qatar, on 18 December 2022 (Qatari National Day), and was contested by Argentina and defending champions France.\n2. FIFA World Cup Qatar 2022™\n   https://fifa.example/qatar2022\n   Discover the story of the FIFA World Cup Qatar 2022™, from standout moments to defining results, all in one place.\n3. 2022 FIFA World Cup - Wikipedia\n   https://wiki.example/cup\n   Argentina were crowned the champions after winning the final against the title holder France 4-2 on penalties following a 3-3 draw after extra time. It was Argentina's third title and their first since 1986. Argentina were the first nation from outside of Europe to win the tournament since 2002.\n4. Argentina v France 3-3 | Result, Stats & Highlights | Final - FIFA\n   https://fifa.example/match-centre/400128145\n   Argentina (4) 3 - 3 (2) France Argentina won 4 - 2 on penalties Full Time 108' Lionel MESSI\n5. Triumphant Argentina conclude unprecedented FIFA World Cup\n   https://inside.fifa.example/news/triumphant-argentina\n   Victorious Argentina held aloft the hallowed FIFA World Cup trophy last night before a packed house of 88,966 inside a resplendent Lusail Stadium, drawing the curtain on an epic,...";
        let got = meaning_sentence("can u serach the web for who won the 2022 fifa world cup", live);
        assert!(got.as_deref().is_some_and(|s| !s.contains("108'") && s.contains("Argentina") && ["won", "winning", "champions", "trophy"].iter().any(|w| s.contains(w))), "{got:?}");
        // the name the results agree on anchors the answer: Canberra in four of five, so the capital's population
        // sentence that leaves it out is no answer (the embedding tells Vietnamese sentences apart poorly)
        let uc5 = "results for \"thủ đô của Úc\":\n1. Thủ đô của Úc là gì? Sydney, Canberra hay Melbourne?\n   https://vnpc.example/thu-do-cua-uc\n   Canberra nằm giữa Sydney và Melbourne, một thành phố bình yên, vô cùng đáng sống và cũng chính là thủ đô của Úc. Tuy nhiên, khi tìm hiểu về xứ sở chuột túi, nhiều người vẫn lầm tưởng thủ đô của quốc gia này là Sydney.\n2. Canberra - Wikipedia tiếng Việt\n   https://vi.wikipedia.example/wiki/Canberra\n   Canberra có dân số 381.488 người, là thành phố nội lục lớn nhất và thành phố đông dân thứ tám của Úc. Thành phố nằm tại cực bắc của Lãnh thổ Thủ đô Úc, cách 280 km (170 mi) về phía tây nam của Sydney.\n3. Úc - Wikipedia tiếng Việt\n   https://vi.wikipedia.example/wiki/Uc\n   Thủ đô của Úc là Canberra, thành phố lớn nhất là Sydney. Úc có ngôn ngữ quốc gia là tiếng Anh và ngoài ra còn có hơn 200 ngôn ngữ bản địa khác được sử dụng.\n4. Thủ đô của Úc là gì? Sydney, Melbourne hay Canberra?\n   https://giacmouc.example/thu-do-cua-uc\n   Thủ đô của nước Úc có dân số 381.488, là thành phố nội lục lớn nhất và thành phố đông dân thứ 8 toàn quốc. Thành phố nằm tại cực bắc của Lãnh thổ Thủ đô Úc, cách 280 km (170 mi) về phía tây nam của Sydney.\n5. Thủ đô của Úc là gì? Bật mí các điều thú vị của Canberra\n   https://set-edu.example/thu-do-cua-uc/\n   Thủ đô của nước Úc là gì? Không phải Sydney hay Melbourne, thủ đô của Úc là Canberra - trung tâm hành chính, chính trị và giáo dục với nhiều cơ hội du học và định cư hấp dẫn.";
        let got = meaning_sentence("thủ đô của Úc là gì? tra google giúp mình", uc5);
        assert!(got.as_deref().is_some_and(|s| s.contains("Canberra") && !s.contains("381.488")), "{got:?}");
    }

    /// The results' sentence by meaning names the side the results agree on, in both languages, when the name held
    /// by most entries is the contest's host ("Japan": the titles, the venue sentence and the tournament sentence)
    /// and one snippet tells another match (the third-place play-off, with the host's name in it): the winner's
    /// sentence, never the play-off's (`meaning_sentence`; checks only with the embedding model installed)
    #[test]
    fn the_results_sentence_names_the_agreed_side_not_the_host_or_another_match() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        let en = "results for \"who won the 2019 rugby world cup\":\n1. 2019 Rugby World Cup final - Wikipedia\n   https://wiki.example/rugby-final\n   The final of the 2019 Rugby World Cup was played at International Stadium Yokohama in Yokohama, Japan, on 2 November 2019, and was contested by England and South Africa.\n2. Rugby World Cup Japan 2019™\n   https://rugby.example/japan2019\n   New Zealand beat Wales in the play-off for third place, and go home from Rugby World Cup Japan 2019™ with a medal for the second tournament in a row.\n3. 2019 Rugby World Cup - Wikipedia\n   https://wiki.example/rugby-cup\n   South Africa were crowned the champions after winning the final against England 32-12. It was South Africa's third title and their first since 2007.\n4. 2019 Rugby World Cup - Encyclopaedia\n   https://encyclopaedia.example/rugby-2019\n   The 2019 Rugby World Cup was an international rugby union tournament that took place in Japan from September 20 to November 2, 2019. South Africa defeated England in the final match to win its third Rugby World Cup title.\n5. Rugby World Cup Japan 2019™\n   https://rugby.example/japan2019/fixtures\n   Explore scores and fixtures for the Rugby World Cup 2019™ on the official site.";
        let names = crate::reply_check::shared_names(&crate::reply_check::subject_words("who won the 2019 rugby world cup"), en);
        assert!(names.first().is_some_and(|n| n == "japan") && names.iter().any(|n| n == "africa"), "the host is the most widely held name: {names:?}");
        let got = meaning_sentence("who won the 2019 rugby world cup", en);
        assert!(got.as_deref().is_some_and(|s| s.contains("South Africa") && ["won", "winning", "win ", "champions"].iter().any(|w| s.contains(w)) && !s.contains("third place")), "{got:?}");
        let vi = "results for \"ai vô địch rugby world cup 2019\":\n1. Chung kết Rugby World Cup 2019 – Wikipedia tiếng Việt\n   https://vi.wiki.example/rugby-chung-ket\n   Trận chung kết Rugby World Cup 2019 diễn ra tại sân vận động quốc tế Yokohama ở Nhật Bản ngày 2 tháng 11 năm 2019, giữa Anh và Nam Phi.\n2. Rugby World Cup Nhật Bản 2019™\n   https://rugby.example/vi/nhat-ban-2019\n   New Zealand thắng Wales trong trận tranh hạng ba và rời Rugby World Cup Nhật Bản 2019™ với tấm huy chương đồng lần thứ hai liên tiếp.\n3. Rugby World Cup 2019 – Wikipedia tiếng Việt\n   https://vi.wiki.example/rugby-2019\n   Nam Phi lên ngôi vô địch sau khi thắng Anh 32-12 trong trận chung kết. Đây là danh hiệu thứ ba của Nam Phi.\n4. Rugby World Cup 2019 – Bách khoa toàn thư\n   https://bachkhoa.example/rugby-2019\n   Rugby World Cup 2019 là giải bóng bầu dục quốc tế diễn ra tại Nhật Bản từ ngày 20 tháng 9 đến ngày 2 tháng 11 năm 2019. Nam Phi đánh bại Anh trong trận chung kết để giành chức vô địch Rugby World Cup lần thứ ba.\n5. Rugby World Cup Nhật Bản 2019™\n   https://rugby.example/vi/nhat-ban-2019/lich\n   Xem tỷ số và lịch thi đấu Rugby World Cup Nhật Bản 2019™ trên trang chính thức.";
        let got = meaning_sentence("ai vô địch rugby world cup 2019?", vi);
        assert!(got.as_deref().is_some_and(|s| s.contains("Nam Phi") && s.contains("vô địch") && !s.contains("hạng ba")), "{got:?}");
        // a capital: the city's distance sentence holds every ask word ("Lãnh thổ Thủ đô Úc") and two of the agreed
        // names (Sydney, Melbourne, said by every page as not the capital), the country's population sentence every
        // ask word and no name; the capital's own sentence wins in both languages
        let uc = "results for \"thủ đô của Úc\":\n1. Canberra – Bách khoa mở\n   https://bachkhoa.example/canberra\n   Canberra có 381.488 dân, là thành phố trong đất liền lớn nhất của Úc. Thành phố nằm ở phía bắc Lãnh thổ Thủ đô Úc, cách Sydney 280 km về phía tây nam và cách Melbourne 660 km về phía đông bắc.\n2. Thủ đô của Úc là thành phố nào?\n   https://hoidap.example/thu-do-uc\n   Canberra nằm giữa Sydney và Melbourne, một thành phố yên bình và chính là thủ đô của Úc. Nhiều người vẫn tưởng thủ đô của nước này là Sydney.\n3. Úc – Bách khoa mở\n   https://bachkhoa.example/uc\n   Thủ đô của Úc là Canberra, thành phố lớn nhất là Sydney. Tiếng Anh là ngôn ngữ quốc gia của Úc.\n4. Thủ đô của Úc: Sydney, Melbourne hay Canberra?\n   https://dulich.example/thu-do-uc\n   Thủ đô của nước Úc có 381.488 dân, là thành phố trong đất liền lớn nhất toàn quốc. Thành phố nằm ở phía bắc Lãnh thổ Thủ đô Úc, cách Sydney 280 km về phía tây nam và cách Melbourne 660 km về phía đông bắc.\n5. Thủ đô đặc biệt của Úc\n   https://pantravel.example/thu-do-uc\n   Canberra là thủ đô của Úc. Nhiều du khách vẫn nhầm Melbourne hay Sydney là thủ đô của Úc.";
        let got = meaning_sentence("thủ đô của Úc là gì?", uc);
        assert!(got.as_deref().is_some_and(|s| s.contains("Canberra") && !s.contains("280 km") && !s.contains("381.488")), "{got:?}");
        let au = "results for \"capital of australia\":\n1. Canberra - Encyclopaedia\n   https://encyclopaedia.example/canberra\n   Canberra has 381,488 residents and is the largest inland city of Australia. The city lies in the north of the Australian Capital Territory, 280 km south-west of Sydney and 660 km north-east of Melbourne.\n2. Which city is the capital of Australia?\n   https://quiz.example/capital-of-australia\n   Canberra sits between Sydney and Melbourne, a quiet city that is the capital of Australia. Many people still think the capital of the country is Sydney.\n3. Australia - Encyclopaedia\n   https://encyclopaedia.example/australia\n   The capital of Australia is Canberra; the largest city is Sydney. English is the national language of Australia.\n4. Capital of Australia: Sydney, Melbourne or Canberra?\n   https://travel.example/capital-of-australia\n   The capital of Australia has 381,488 residents and is the largest inland city in the country. The city lies in the north of the Australian Capital Territory, 280 km south-west of Sydney and 660 km north-east of Melbourne.\n5. Australia's special capital\n   https://pantravel.example/capital-of-australia\n   Canberra is the capital of Australia. Many visitors still mistake Melbourne or Sydney for the capital of Australia.";
        let got = meaning_sentence("what is the capital of australia?", au);
        assert!(got.as_deref().is_some_and(|s| s.contains("Canberra") && !s.contains("280 km") && !s.contains("381,488")), "{got:?}");
    }

    /// a page's readings under a heading (`live_candidates` on a page without a figure sentence), the honest line
    /// when no page was read, and the sites a reply names (`named_sites`, `host_label`)
    #[test]
    fn page_readings_unread_line_and_named_sites() {
        // a weather page writing its values one per line: the heading naming the ask, the condition line and the
        // degrees reading as one sentence, the dated block over the undated one; "0 mm" and "93%" are no temperature
        let page = |date: &str| format!("title: Dự báo thời tiết Hà Nội hôm nay, ngày mai và 30 ngày tới\n\nHà Nội\n\n18oC\n\n0 mm\n\nBài viết gợi ý\n\nĐịa điểm\nTin tức\n\n-\n\nThời tiết Hà Nội\n\n{date}\n--:--\n\nNhiều mây, trời âm u\n\n18oC\n\n18oC\n/\n22oC\n| Cảm giác như\n19oC\n\n0% mưa\n\n|\n\n0 mm\n\nĐộ ẩm\n\n93%");
        let got = live_candidates("thời tiết Hà Nội hôm nay thế nào?", &page("12/11/2020,"), true);
        let sentences: Vec<&str> = got.iter().map(|c| c.sentence.as_str()).collect();
        assert_eq!(sentences, ["Hà Nội: 18oC.", "Thời tiết Hà Nội: Nhiều mây, trời âm u, 18oC."], "{got:?}");
        assert!(got.iter().all(|c| c.subject && c.single && c.kinded && c.page), "{got:?}");
        assert!(matches!(&got[1].when, When::Before { date, .. } if date.contains("2020")) && got[0].when == When::Undated, "{got:?}");
        // the block of the old date still ranks above the undated one (a date, even old, beats none); dated no
        // earlier, its heading's words win as well
        assert_eq!(ls("thời tiết Hà Nội hôm nay thế nào?", &page("12/11/2020,")).as_deref(), Some("Thời tiết Hà Nội: Nhiều mây, trời âm u, 18oC."));
        assert_eq!(ls("thời tiết Hà Nội hôm nay thế nào?", &page("")).as_deref(), Some("Thời tiết Hà Nội: Nhiều mây, trời âm u, 18oC."));
        // the same in English, a label line before the reading; a page with a figure sentence keeps that sentence
        let en = "title: Hanoi weather\n\nHanoi\n\nCurrent conditions\n\nOvercast\n\n18°C\n\nHumidity\n\n93%";
        assert_eq!(ls("what's the temperature in Hanoi right now?", en).as_deref(), Some("Hanoi: Overcast 18°C."));
        let said = "title: Hanoi weather\n\nHanoi\n\n18°C\n\nRight now Hanoi is overcast at 19°C with light wind.";
        assert_eq!(ls("what's the temperature in Hanoi right now?", said).as_deref(), Some("Right now Hanoi is overcast at 19°C with light wind."));
        // a page whose readings sit under no heading of the ask, or too far below one, gives none
        assert_eq!(ls("what's the temperature in Hanoi right now?", "title: Weather\n\nMenu\n\n18°C"), None);
        assert_eq!(ls("what's the temperature in Hanoi right now?", &format!("title: Weather\n\nHanoi\n{}\n18°C", "-\n".repeat(READING_LINES))), None);
        // the honest line without a page read says only that, EN and VI
        for ask in ["what's the gold price today?", "giá vàng hôm nay thế nào nhỉ"] {
            let read = crate::reply_check::own_line_of("no_figure", ask, "").replace("{sites}", "sjc.com.vn");
            let unread = crate::reply_check::own_line_of("no_figure_unread", ask, "").replace("{sites}", "sjc.com.vn");
            assert!(read != unread && unread.contains("sjc.com.vn") && !unread.contains("{sites}"), "{unread}");
        }
        // the sites a reply names: by the host's name, a prefix of five letters or more, never the ask's subject
        let hosts: Vec<String> = ["worldometers.info", "liveworldstats.com", "iceland.org", "en.wikipedia.org", "sjc.com.vn"].iter().map(|s| s.to_string()).collect();
        assert_eq!(host_label("worldometers.info"), "worldometers");
        assert_eq!(host_label("en.wikipedia.org"), "wikipedia");
        assert_eq!(host_label("sjc.com.vn"), "sjc");
        let ask = "what is the population of Iceland?";
        assert_eq!(named_sites("The population of Iceland as of 2026 is approximately 380,312 according to Worldometer.", &hosts, ask), ["worldometers.info"]);
        assert_eq!(named_sites("Iceland's population is 394,324 (Wikipedia).", &hosts, ask), ["en.wikipedia.org"]);
        assert!(named_sites("Iceland has about 380,312 people.", &hosts, ask).is_empty());
        assert_eq!(named_sites("Theo SJC, giá vàng hôm nay là 84,5 triệu đồng/lượng.", &hosts, "giá vàng hôm nay thế nào nhỉ"), ["sjc.com.vn"]);
        assert!(named_sites("Giá vàng hôm nay khoảng 84,5 triệu đồng/lượng.", &hosts, "giá vàng hôm nay thế nào nhỉ").is_empty());
    }


    /// a weather page's current-conditions sentence, under a heading naming the ask, wins for an ask about now over a
    /// sentence naming the place about the hours ahead; the page's condition right under it goes with it
    #[test]
    fn a_pages_current_conditions_sentence_wins_for_a_now_ask() {
        let (d, m, y) = dmy(crate::situation::now_secs());
        let page = format!("title: Thời tiết Hà Nội hôm nay, ngày mai và 7 ngày tới\n\nHà Nội\n\nCập nhật thực tế\n\nDự báo thời tiết Hà Nội\n\n18°C\n\nNhiệt độ hiện tại là 18°C, cảm giác như 19°C.\nMưa Rào Nhẹ,\nđộ ẩm 90%.\n\n💧Độ ẩm: 90%\n\n💨Gió: 9 km/h\n\n🕐 Cập nhật lần cuối: 04:23:43 — {d}/{m}/{y}\n\nHà Nội 12 tiếng tới: so sánh giữa các xã, phường\n\nHai điều cần biết về Hà Nội 12 tiếng tới: chênh 1°C giữa hai đầu tỉnh, và xã Yên Bài lạnh nhất tỉnh, 17°C.\n\nLấy trung tâm tỉnh làm mốc, 12 tiếng tới trời 18–25°C; các xã bên dưới có thể lệch ít nhiều.");
        let page = page.as_str();
        let ask = "thời tiết Hà Nội hôm nay thế nào?";
        let all = live_candidates(ask, page, true);
        assert!(all.iter().any(|c| c.sentence.starts_with("Hai điều cần biết")), "{all:?}");
        let best = live_sentence(ask, page, true).unwrap();
        assert_eq!(best.sentence, "Dự báo thời tiết Hà Nội: Nhiệt độ hiện tại là 18°C, cảm giác như 19°C.");
        assert!(best.subject && best.now && best.when == When::Today, "{best:?}");
        // (the page's update stamp dates its other lines today too; said as now, the current sentence stays ahead)
        assert_eq!(page_stamp(page), Some(When::Today));
        assert!(all.iter().all(|c| c.when == When::Today) && all.iter().filter(|c| !c.now).all(|c| c.freshness() < best.freshness()), "{all:?}");
        // the condition line alone ("Mưa Rào Nhẹ,") counts for today, and follows the sentence that claims none
        let shown = page_conditions(page);
        assert!(shown.iter().any(|x| x.day == 0 && x.name == "rain" && x.said == "Mưa Rào Nhẹ."), "{shown:?}");
        assert_eq!(with_page_condition(&best.sentence, page, &shown, 0), "Dự báo thời tiết Hà Nội: Nhiệt độ hiện tại là 18°C, cảm giác như 19°C. Mưa Rào Nhẹ.");
        assert_eq!(with_page_condition("Hôm nay Hà Nội mưa rào nhẹ, 18°C.", page, &shown, 0), "Hôm nay Hà Nội mưa rào nhẹ, 18°C.");
        assert_eq!(with_page_condition(&best.sentence, page, &shown, 1), best.sentence);
        // the whole reply about the hours ahead answers nothing for today: the page's sentence and condition replace it
        assert_eq!(weather_rewrite("Hai điều cần biết về Hà Nội 12 tiếng tới: chênh 1°C giữa hai đầu tỉnh, và xã Yên Bài lạnh nhất tỉnh, 17°C.", &shown, &page_degrees(page), 0).as_deref(), Some(""));
    }

    /// a weather page's feels-like line is an aside (`aside:` of `data/live_asks.txt`): the current reading is the
    /// answer and the aside never stands in its place, as a sentence under the place heading or as a reading under
    /// its label; a reply giving the reading with the aside labelled beside it stands, the aside alone does not
    #[test]
    fn a_pages_feels_like_reading_never_stands_for_the_temperature() {
        let page = "title: Thời tiết Hà Nội - Dự báo hôm nay & 14 ngày | Báo Thời Tiết\n\nTrang chủ Thời tiết Hà Nội\n\nThời tiết Hà Nội\n\nHà Nội\n\nCập nhật lúc 12:24\n\n22°C\n\nU ám\n\nCảm giác như 24°C\n\nĐộ ẩm\n\n75%\n\nGió\n\n5 km/h\n\nĐiểm sương\n\n17°C\n\nTóm tắt thời tiết\n\nHiện tại trời âm u, nhiệt độ mát 22°C.\n\nThích hợp để đi dạo hoặc tập thể dục ngoài trời.\n\nNhiệt độ ngày mai không thay đổi nhiều, vẫn quanh 23°C và có khả năng mưa.\n\nDự báo theo giờ\n\nBây giờ 22°C 54 %\n\n13:00 22°C 39 %\n\n14:00 23°C 26 %\n\nDự báo 14 ngày\n\nHôm nay 100 %17°C\n\n23°C 11\n\nNgày mai 17°C\n\n26°C 10\n\nKhí hậu & thời tiết Hà Nội\n\nHà Nội thuộc vùng khí hậu nhiệt đới gió mùa với 4 mùa rõ rệt: Xuân (tháng 2-4), Hạ (tháng 5-8), Thu (tháng 9-11) và Đông (tháng 12-1). Mùa đông có thể xuống dưới 10°C tại vùng núi cao, trong khi mùa hè nắng nóng với nhiệt độ thường xuyên vượt 35°C.\n\nThời tiết Hà Nội hôm nay như thế nào?";
        let ask = "thời tiết Hà Nội hôm nay thế nào?";
        let all = live_candidates(ask, page, true);
        let feels = all.iter().find(|c| c.sentence == "Hà Nội: Cảm giác như 24°C.").unwrap();
        assert!(feels.aside && feels.subject, "{feels:?}");
        // (the forecast row sharing only "hôm nay" with the ask, and the climate paragraph under a heading too long
        // to be one, are no candidates)
        assert!(!all.iter().any(|c| c.sentence.contains("%17°C") || c.sentence.contains("35°C")), "{all:?}");
        let best = live_sentence(ask, page, true).unwrap();
        assert_eq!(best.sentence, "Tóm tắt thời tiết: Hiện tại trời âm u, nhiệt độ mát 22°C.");
        assert!(!best.aside && best.now && !best.subject && best.rank() > feels.rank(), "{best:?}");
        // a reply's figures: the reading with the aside labelled beside it stands; the aside alone, its figure given
        // as a second temperature, or an aside the page does not hold, does not
        let held: HashSet<String> = live_figures(page).into_iter().collect();
        let top = (best.subject, best.single, best.freshness());
        let good: HashSet<String> = all.iter().filter(|c| !c.aside && (c.subject, c.single, c.freshness()) == top).flat_map(|c| fresh_figures(&c.sentence)).collect();
        let values: HashSet<String> = all.iter().flat_map(|c| live_figures(&c.sentence)).collect();
        let asides: HashSet<String> = all.iter().filter(|c| c.aside).flat_map(|c| live_figures(&c.sentence)).collect();
        assert!(good.contains("22") && !good.contains("24") && asides.contains("24") && values.contains("24"), "{good:?} {asides:?}");
        let holders = std::collections::HashMap::new();
        let stands = |reply: &str| { let (said, aside) = said_figures(reply); figures_stand(&said, &held, &good, &values, &holders) && asides_stand(&aside, &held, &good, &values, &asides) };
        assert!(stands("Hà Nội hiện 22°C, cảm giác như 24°C.") && stands("Hà Nội hiện 22°C, trời u ám."));
        assert!(!stands("Hà Nội: cảm giác như 24°C.") && !stands("Hà Nội hiện 22°C và 24°C.") && !stands("Hà Nội hiện 22°C, cảm giác như 30°C."));
        assert_eq!(said_figures("Hà Nội 22°C, cảm giác như 24°C. Điểm sương 17°C."), (vec!["22".to_string()], vec!["24".to_string(), "17".to_string()]));
        assert_eq!(said_figures("Cảm giác như 24°C. Nhiệt độ 22°C."), (vec!["22".to_string()], vec!["24".to_string()]));
        // the readings block alone: the aside is its only sentence, so the readings count, and the current reading
        // under the place wins
        let short = page.split("Tóm tắt thời tiết").next().unwrap();
        let all = live_candidates(ask, short, true);
        assert!(all.iter().any(|c| c.aside) && all.iter().any(|c| c.sentence == "Hà Nội: 22°C." && !c.aside), "{all:?}");
        assert_eq!(live_sentence(ask, short, true).unwrap().sentence, "Hà Nội: 22°C.");
        // a reading under an aside's label line is one too
        let labelled = "Nhiệt độ Hà Nội\n\nCảm giác như\n\n24°C\n\nNhiệt độ\n\n22°C";
        let all = live_candidates("nhiệt độ Hà Nội hôm nay bao nhiêu?", labelled, true);
        assert!(all.iter().any(|c| c.sentence == "Nhiệt độ Hà Nội: Cảm giác như 24°C." && c.aside), "{all:?}");
        assert_eq!(ls("nhiệt độ Hà Nội hôm nay bao nhiêu?", labelled).as_deref(), Some("Nhiệt độ: 22°C."));
        // the aside labels, in a sentence's clause before its figure, in either language
        let aside = |s: &str| aside_sentence(s, figure_spans(s).iter());
        assert!(aside("Cảm giác như 24°C") && aside("High 26°C Low 19°C") && aside("Dew point: 17°C") && aside("Nhiệt độ cao nhất 31°C, thấp nhất 23°C"));
        assert!(!aside("Nhiệt độ hiện tại là 18°C, cảm giác như 19°C.") && !aside("Now 24°C, feels like 27°C") && !aside("22°C") && !aside("Hà Nội"));
        // an English page: the current sentence over the feels-like said as now under the place
        let en = "Hanoi\n\n22°C\n\nFeels like 24°C right now\n\nWeather now\n\nCurrently overcast with 22°C.";
        let all = live_candidates("what's the weather in Hanoi today?", en, true);
        assert!(all.iter().any(|c| c.sentence == "Hanoi: Feels like 24°C right now." && c.aside && c.now), "{all:?}");
        assert_eq!(ls("what's the weather in Hanoi today?", en).as_deref(), Some("Weather now: Currently overcast with 22°C."));
    }

    /// a weather reply with a claim against the page is answered by Elisa's own line for the day asked — the page's
    /// current sentence with its site, or the page's sentence for the day — never by what is left of the reply (the
    /// model's hourly pieces and cut words)
    #[test]
    fn a_flagged_weather_reply_gets_elisas_line_for_the_day() {
        let page = "Hà Nội\n\nCập nhật lúc 15:54\n\n24°C\n\nÍt mây\n\nCảm giác như 27°C\n\nLượng mưa\n\n0 mm\n\nTóm tắt thời tiết\n\nHiện tại trời nắng đẹp, nhiệt độ mát 24°C.\n\nNhiệt độ ngày mai không thay đổi nhiều, vẫn quanh 24°C và có khả năng mưa.\n\nDự báo theo giờ\n\nBây giờ 24°C 13 %\n\n16:00 24°C 4 %\n\n17:00 23°C\n\n18:00 22°C\n\n19:00 21°C\n\n10:00 26°C";
        let ask = "thời tiết Hà Nội hôm nay thế nào?";
        let shown = page_conditions(page);
        let degrees = page_degrees(page);
        // the model's reply: the page's current sentence, then its hourly pieces with a cut word, and a claim about
        // tomorrow the page contradicts; the rewrite alone would keep the pieces
        let reply = "Hiện tại trời nắng đẹp, nhiệt độ mát 24°C. Cảm giác ước tính 27°C. Về từng giờ: bây giờ 24°C 13 %; 16:00 – 17:00: nhiệt độ khoảng 24–23°C; phố cuối ngày: từ 21°C đến 26°C tùy giờ. Ngày mai trời nắng đẹp.";
        let left = weather_rewrite(reply, &shown, &degrees, 0).unwrap();
        assert!(left.contains("phố cuối ngày") && !left.contains("Ngày mai trời nắng"), "{left}");
        assert_eq!(weather_answer(ask, page, &shown, 0, "baothoitiet.example").as_deref(), Some("Tóm tắt thời tiết: Hiện tại trời nắng đẹp, nhiệt độ mát 24°C. (nguồn: baothoitiet.example)"));
        assert_eq!(weather_answer(ask, page, &shown, 0, "").as_deref(), Some("Tóm tắt thời tiết: Hiện tại trời nắng đẹp, nhiệt độ mát 24°C."));
        assert_eq!(weather_answer("thời tiết Hà Nội ngày mai thế nào?", page, &shown, 1, "baothoitiet.example").as_deref(), Some("Nhiệt độ ngày mai không thay đổi nhiều, vẫn quanh 24°C và có khả năng mưa."));
        // a page with no figure sentence and no sentence for the day gives no line: what the rewrite left stands
        assert_eq!(weather_answer(ask, "Hà Nội\n\nTrời đẹp.", &page_conditions("Hà Nội\n\nTrời đẹp."), 0, ""), None);
    }

    /// a day as a Vietnamese page writes it: (day, month, year) without leading zeros
    fn dmy(secs: i64) -> (String, String, String) {
        let t = crate::web::today(secs);
        let p: Vec<&str> = t.split('-').collect();
        (p[2].trim_start_matches('0').to_string(), p[1].trim_start_matches('0').to_string(), p[0].to_string())
    }

    /// a price page updated today: its ticker line under the "Giá vàng" heading, undated itself, is today's by the
    /// page's stamp and beats the article sentences dated yesterday by their headline — one saying "hôm nay" too —
    /// and today's world price in USD, another language's currency to a Vietnamese ask (`home_currency`); to an
    /// English ask the price in dollars comes first
    #[test]
    fn a_pages_stamped_table_beats_its_article_dated_yesterday() {
        let now = crate::situation::now_secs();
        let (d, m, y) = dmy(now);
        let (d1, m1, y1) = dmy(now - 86_400);
        let page = format!("title: Giá vàng hôm nay các thương hiệu Sjc, 9999, 24k, 18k của PNJ, Doji mới nhất\n\nGiá vàng\n\nSJC Mua 136,000 Bán 139,000\n\nBTMH Mua 134,700 Bán 138,700\n\nTỷ giá\n\nUSD Mua 25,460 Bán 25,870\n\nGiá vàng\n\nChia sẻ\n\nNguồn: giavang.net, pnj.com.vn và baotinmanhhai.vn - Cập nhật lúc 05:58 ({d}/{m}/{y})\n\nHôm nay ({d}/{m}/{y})\nHôm qua ({d1}/{m1}/{y1})\n\nGiá mua\nGiá bán\n\nSJC\n\n136,000\n\n1000\n\n139,000\n\n1000\n\n137,000\n\n140,000\n\nBTMH\n\n134,700\n\n700\n\n138,700\n\n700\n\n135,400\n\n139,400\n\nĐơn vị: nghìn đồng/lượng\n\nTăng giảm so sánh với ngày trước đó\n\nGiá vàng\n\nGiá vàng hôm nay {d}/{m}: Lại giảm hơn 100 USD/ounce, xuống dưới 3.900 USD\n\nSáng {d}/{m}, giá vàng thế giới được niêm yết trên Kitco ở mức 3.877 USD/ounce, giảm 110 USD/ounce so với đầu giờ sáng qua.\n\nChốt giá vàng hôm nay ngày {d1}/{m1} tiếp đà giảm rất mạnh\n\nGiá vàng hôm nay {d1}/{m1}: Vàng SJC, vàng nhẫn Bảo Tín Minh Châu, Doji giảm mạnh còn bao nhiêu?\n\nGiá vàng hôm nay ở vàng miếng SJC giảm 1 triệu đồng/lượng ở cả hai chiều, còn 139 triệu đồng/lượng ở chiều bán ra, vàng nhẫn SJC đứng yên ở 139,8 triệu đồng/lượng, cao hơn vàng miếng 800.000 đồng/lượng.\n\nGiá vàng tiếp tục giảm\n\nSáng nay ({d1}/{m1}), giá vàng trong nước tiếp tục giảm theo giá thế giới. Theo đó, giá bán vàng miếng SJC về mốc 139 triệu đồng/lượng và cao hơn thế giới gần 15 triệu đồng/lượng.");
        let page = page.as_str();
        let ask = "giá vàng hôm nay thế nào nhỉ";
        assert_eq!(page_stamp(page), Some(When::Today));
        let all = live_candidates(ask, page, true);
        let article: Vec<&LiveFigure> = all.iter().filter(|c| c.sentence.contains("139 triệu")).collect();
        assert!(article.len() >= 2 && article.iter().all(|c| matches!(c.when, When::Before { days: 1, .. })), "{all:?}");
        // today's world price: as fresh, said as now, naming the ask's words itself — but in dollars
        let world = all.iter().find(|c| c.sentence.contains("3.900 USD")).unwrap();
        assert!(world.when == When::Today && world.now && world.named && world.home == -1, "{world:?}");
        // the ticker's bare figures are the table's, and the table's unit note gives them their unit
        let best = live_sentence(ask, page, true).unwrap();
        assert_eq!(best.sentence, "Giá vàng: SJC Mua 136,000 Bán 139,000 (nghìn đồng/lượng).");
        assert!(best.when == When::Today && !best.now && best.subject && best.home == 1, "{best:?}");
        assert_eq!(fresh_figures(&best.sentence), ["136000", "139000"]);
        let units = table_units(page);
        assert_eq!((units.get("136000").map(String::as_str), units.get("138700").map(String::as_str), units.get("1000"), units.get("25460")), (Some("nghìn đồng/lượng"), Some("nghìn đồng/lượng"), None, None));
        assert_eq!(unit_note("Đơn vị: nghìn đồng/lượng").as_deref(), Some("nghìn đồng/lượng"));
        assert_eq!(unit_note("Unit: USD per ounce").as_deref(), Some("USD per ounce"));
        assert_eq!((unit_note("Giá bán (VND)").as_deref(), unit_note("(triệu đồng/lượng)").as_deref()), (Some("VND"), Some("triệu đồng/lượng")));
        assert_eq!((unit_note("USD"), unit_note("Giá vàng"), unit_note("SJC Mua 136,000 Bán 139,000"), unit_note("Nguồn: giavang.net và pnj.com.vn - Cập nhật lúc 05:58")), (None, None, None, None));
        // a note above its table names the table's figures too; a sentence with a unit of its own keeps it
        let above = "Giá vàng\n\nĐơn vị: triệu đồng/lượng\n\nSJC\n\n139,5\n\n140,5\n\nGiá vàng thế giới hôm nay giảm còn 3.877 USD/ounce theo Kitco.";
        let units = table_units(above);
        assert_eq!((units.get("1395").map(String::as_str), units.get("3877")), (Some("triệu đồng/lượng"), None));
        let own = "SJC bán ra 139,5 triệu đồng/lượng.";
        assert_eq!(with_table_unit(own, &figure_spans(own), &units), own);
        assert_eq!(with_table_unit("Giá vàng: SJC 139,5.", &figure_spans("Giá vàng: SJC 139,5."), &units), "Giá vàng: SJC 139,5 (triệu đồng/lượng).");
        assert_eq!(with_table_unit("Giá vàng: SJC 141,5.", &figure_spans("Giá vàng: SJC 141,5."), &units), "Giá vàng: SJC 141,5.");
        // the stale article sentence a model gives, or the world price, does not stand against it
        let held: HashSet<String> = live_figures(page).into_iter().collect();
        let good: HashSet<String> = all.iter().filter(|c| (c.subject, c.single, c.freshness()) == (best.subject, best.single, best.freshness())).flat_map(|c| fresh_figures(&c.sentence)).collect();
        assert!(good.contains("136000") && good.contains("138700") && !good.contains("3900"), "{good:?}");
        let values: HashSet<String> = all.iter().flat_map(|c| live_figures(&c.sentence)).collect();
        let mut holders: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
        web_holders(page, Some("24h.example"), &mut holders);
        let said = live_figures("Giá vàng hôm nay ở vàng miếng SJC giảm 1 triệu đồng/lượng ở cả hai chiều, còn 139 triệu đồng/lượng ở chiều bán ra.");
        assert!(!said.is_empty() && !figures_stand(&said, &held, &good, &values, &holders), "{said:?}");
        let said = live_figures(&format!("Giá vàng hôm nay {d}/{m}: Lại giảm hơn 100 USD/ounce, xuống dưới 3.900 USD."));
        assert!(!said.is_empty() && !figures_stand(&said, &held, &good, &values, &holders), "{said:?}");
        assert!(figures_stand(&live_figures("SJC mua 136,000, bán 139,000."), &held, &good, &values, &holders));
        // the currency an ask's language means: Vietnamese đồng, English dollars; none is neither
        assert_eq!((home_currency("còn 139 triệu đồng/lượng", true), home_currency("3.900 USD/ounce", true), home_currency("SJC Mua 136,000 Bán 139,000", true)), (1, -1, 0));
        assert_eq!((home_currency("Gold is at $3,877 an ounce.", false), home_currency("139 triệu đồng/lượng", false), home_currency("139 million per tael", false)), (1, -1, 0));
        let en = "Gold price\n\nGold price today: SJC bars at 139 triệu đồng per lượng in Vietnam.\n\nGold price today: $3,877 per ounce.";
        assert_eq!(ls("what's the gold price today?", en).as_deref(), Some("Gold price today: $3,877 per ounce."));
    }

    /// yes/no asks whether a file is there, by meaning (checks only with the embedding model installed)
    #[test]
    fn existence_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, want) in [("is the file still there?", true), ("does the file exist?", true), ("is the file still around?", true),
            ("the file còn không?", true), ("what does the file say?", false), ("how many words are in the file?", false),
            ("which region sold the most units in the file?", false), ("is the file correct?", false), ("what is the meeting code in the file?", false)] {
            assert_eq!(exists_ask(m), want, "{m}");
        }
    }

    /// the similarities of a few asks to the examples (prints them; checks only with the embedding model installed)
    #[test]
    fn listing_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for (m, want) in [("list the files in this folder", true), ("what files are in here?", true), ("show me the files", true),
            ("liệt kê các tệp trong thư mục", true), ("list three ideas for a party", false), ("read notes.txt", false),
            ("what is the capital of France?", false), ("tell me a joke", false), ("save that plan to plan.md", false),
            ("how many files are in this folder?", true), ("make a shopping list", false), ("what can you do?", false),
            ("search the web for the latest news", false), ("delete the old files", false),
            // held-out general questions, EN and VI with and without accents: never a call
            ("thủ đô của Úc là gì?", false), ("thu do cua Uc la gi?", false), ("what is the capital of Australia?", false),
            ("Thủ đô của Úc là gì", false), ("thủ đô nước Úc ở đâu?", false), ("Úc có thủ đô là thành phố nào?", false),
            ("ai phát minh ra bóng đèn?", false), ("bao nhiêu người sống ở Hà Nội?", false), ("how old is the universe?", false),
            ("who painted the Mona Lisa?", false), ("con mèo của tôi tên là gì?", false), ("cach nau pho bo?", false),
            ("which planet is the biggest?", false)] {
            let got = meant_tool(m).map(|t| t.0);
            println!("{m:?} -> {got:?}");
            assert_eq!(got.is_some(), want, "{m}");
        }
    }

    /// a dependency check asked in Vietnamese, accented, unaccented or mixed with English (held-out wordings: none is
    /// an example line): the check, and a turn about security; a question about the packages that is no check is not
    #[test]
    fn dependency_check_asks_by_meaning() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["kiểm tra requirements.txt xem có gói nào bị lỗ hổng không", "kiem tra requirements.txt co goi nao bi lo hong khong",
            "check giúp mình requirements.txt có lỗ hổng không", "check requirements.txt for vulnerable dependencies"] {
            let got = meant_tool(m).map(|t| t.0);
            println!("{m:?} -> {got:?}");
            assert_eq!(got.as_deref(), Some("sec_deps"), "{m}");
            assert!(crate::mcp_reason::about_sec(m), "{m}");
        }
        for m in ["requirements.txt có gói flask không?", "list the files in this folder", "thủ đô của Úc là gì?", "what does requirements.txt hold?",
            "kiểm tra chính tả đoạn này giúp mình", "kiem tra xem hom nay thu may"] {
            let got = meant_tool(m).map(|t| t.0);
            println!("{m:?} -> {got:?}");
            assert_ne!(got.as_deref(), Some("sec_deps"), "{m}");
        }
    }

    /// a month and its day as English writes them ("November 11"), with the year
    fn month_day(secs: i64) -> (String, String) {
        const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        let t = crate::web::today(secs);
        let p: Vec<&str> = t.split('-').collect();
        (format!("{} {}", MONTHS[p[1].parse::<usize>().unwrap() - 1], p[2].trim_start_matches('0')), p[0].to_string())
    }

    /// a reply's date must be one the results write, or today (`dates_stand`), and a snippet's date stays with its
    /// own sentence's figure (`sentence_when`): the mid-year estimate after "as of November 11" is undated
    #[test]
    fn a_replys_date_comes_from_the_results_like_its_figure() {
        let now = crate::situation::now_secs();
        let (d, m, y) = dmy(now);
        let (d1, m1, y1) = dmy(now - 86_400);
        let (d5, m5, y5) = dmy(now + 5 * 86_400);
        let results = format!("results for \"dân số Iceland\":\n1. Dân số Iceland mới nhất ({y}) - cập nhật hằng ngày - DanSo.Org\n   https://danso.example/iceland/\n   Thông tin nhanh Dân số hiện tại của Iceland là 403.556 người vào ngày {d1}/{m1}/{y1} theo số liệu mới nhất từ Liên Hợp Quốc. Dân số Iceland hiện chiếm 0,00% dân số thế giới.\n2. Dân số Iceland hiện tại là bao nhiêu? | AI Hay\n   https://ai-hay.example/dan-so-iceland\n   Dân số Iceland hiện tại là 402.894 người tính đến ngày 01/09/{y1} danso.");
        // the result's day, written either way, and today's stand; a day no result writes does not
        assert!(dates_stand(&format!("Dân số Iceland hiện tại là 403.556 người vào ngày {d1}/{m1}/{y1} theo số liệu mới nhất từ Liên Hợp Quốc."), &results));
        assert!(dates_stand(&format!("Iceland's population is 403,556 as of {m1}/{d1}/{y1}."), &results));
        assert!(dates_stand(&format!("Hôm nay {d}/{m}/{y}, dân số Iceland là 403.556 người."), &results));
        assert!(dates_stand("Dân số Iceland là 403.556 người.", &results));
        assert!(!dates_stand(&format!("Dân số Iceland hiện tại là 403.556 người vào ngày {d5}/{m5}/{y5} theo số liệu mới nhất từ Liên Hợp Quốc."), &results));
        assert!(!dates_stand("Dân số Iceland là 403.556 người tính đến ngày 03/09/2020.", &results));
        // the snippet's first sentence is dated by its own day; the estimate after it writes a year and no day
        let (md, my) = month_day(now - 2 * 86_400);
        let ask = "curious what the population of Iceland is these days, could you look it up?";
        let web = format!("results for \"the population of Iceland is\":\n1. Iceland Population ({y}) - Worldometer\n   https://www.worldometers.example/world-population/iceland-population/\n   The current population of Iceland is 404,827 as of {md}, {my}, based on Worldometer's elaboration of the latest United Nations data 1. Iceland {y} population is estimated at 402,329 people at mid-year. Iceland population is equivalent to < 0.01% of the total world population .");
        let all = live_candidates(ask, &web, false);
        let current = all.iter().find(|c| c.sentence.contains("404,827")).unwrap();
        let mid = all.iter().find(|c| c.sentence.contains("402,329")).unwrap();
        assert!(matches!(current.when, When::Before { days: 2, .. }), "{current:?}");
        assert_eq!(mid.when, When::Undated, "{mid:?}");
        assert!(live_sentence(ask, &web, false).unwrap().sentence.contains("404,827"), "{all:?}");
        // a sentence writing no day or year is dated as its block (the day in the sentence before it); one writing a year dates itself
        assert_eq!(sentence_when("SJC 139 triệu.", true, &When::Before { date: "12/11".into(), days: 1 }), When::Before { date: "12/11".into(), days: 1 });
        assert_eq!(sentence_when(&format!("As of {y}, the population is 394,324."), false, &When::Before { date: "12/11".into(), days: 1 }), When::Undated);
        assert_eq!(sentence_when(&format!("As of {y}, the population is currently 394,324."), false, &When::Undated), When::Now);
        // a snippet saying only "hôm nay" is its page's today, not a day written: a page is still read for it (`fetch_for_live_figure`)
        let gold = "results for \"giá vàng hôm nay\":\n1. Giá vàng hôm nay các thương hiệu Sjc, 9999, 24k, 18k\n   https://24h.example/gia-vang-hom-nay\n   Giá vàng hôm nay ở vàng miếng SJC giảm 1 triệu đồng/lượng ở cả hai chiều, còn 139 triệu đồng/lượng ở chiều bán ra, vàng nhẫn SJC đứng yên ở 139,8 triệu đồng/lượng.\n2. Giá vàng hôm nay - Bảng giá vàng\n   https://vtc.example/gia-vang\n   Xem giá vàng hôm nay mới nhất: vàng 9999, 24K, 18K, vàng nhẫn, vàng miếng, vàng quốc tế.";
        let snippet = live_sentence("giá vàng hôm nay thế nào nhỉ", gold, false).unwrap();
        assert!(snippet.when == When::Now && snippet.now && snippet.sentence.contains("139 triệu"), "{snippet:?}");
        assert!(snippet.fresh() < LiveFigure { when: When::Today, ..snippet.clone() }.fresh() && snippet.fresh() > LiveFigure { when: When::Before { date: "12/11".into(), days: 1 }, ..snippet.clone() }.fresh());
    }

    /// a forecast page's temperatures by day (`page_degrees`): a reply's temperature for the day asked must be one
    /// the page gives for it (`claimed_degrees`); the sentence giving a later day's low as today's goes, and what
    /// is left without the current reading does not stand against the page's own line (`live_figure_fix` after
    /// `weather_claim_fix`)
    #[test]
    fn a_weather_replys_temperature_is_one_the_page_gives_for_that_day() {
        let now = crate::situation::now_secs();
        let (d, m, y) = dmy(now);
        let (d1, m1, _) = dmy(now + 86_400);
        let (d5, m5, _) = dmy(now + 5 * 86_400);
        let page = format!("title: Dự báo thời tiết Hà Nội hôm nay, ngày mai và 30 ngày tới\n\nHà Nội\n\n18oC\n\n0.27 mm\n\nHôm nay\n\nNgày mai\n\n3 ngày\n\nThời tiết Hà Nội\n\n{d}/{m}/{y},\n\nMưa nhỏ\n\n18oC\n\n18oC\n\n24oC\n\n18oC\n\n30% mưa\n\nĐộ ẩm\n\n95%\n\nGió\n\n12.923 km/h\n\nT7\n\n{d1}/{m1}\n\n28oC\n\n20oC\n\nMưa nhỏ\n\n78%\n\nT4\n\n{d5}/{m5}\n\n20oC\n\n13oC\n\nMưa vừa\n\n72%");
        let page = page.as_str();
        let degs = page_degrees(page);
        let on = |day: i64| degs.iter().filter(|x| x.0 == day).map(|x| x.1.as_str()).collect::<HashSet<&str>>();
        assert_eq!(on(0), ["18", "24"].into_iter().collect(), "{degs:?}");
        assert_eq!(on(1), ["28", "20"].into_iter().collect(), "{degs:?}");
        assert_eq!(on(5), ["20", "13"].into_iter().collect(), "{degs:?}");
        assert_eq!(claimed_degrees("Nhiệt độ 13°C, khả năng mưa 61%.", 0), [(0, "13".to_string())]);
        assert_eq!(claimed_degrees("Hôm nay 18°C, ngày mai lên 28°C.", 0), [(0, "18".to_string()), (1, "28".to_string())]);
        assert_eq!(clauses("Nhiệt độ 18,5°C, có mưa; gió nhẹ"), ["Nhiệt độ 18,5°C", " có mưa", " gió nhẹ"]);
        let shown = page_conditions(page);
        // the sentence giving a later day's low as today's goes; the rest stands
        let reply = "Mưa nhỏ, tốc độ gió 12.923 km/h, độ ẩm 95%.\n\nKhả năng mưa trong 3 ngày tới: 30%. Nhiệt độ 13°C, khả năng mưa 61%.";
        let fixed = weather_rewrite(reply, &shown, &degs, 0).unwrap();
        assert!(fixed.starts_with("Mưa nhỏ, tốc độ gió 12.923 km/h, độ ẩm 95%.") && !fixed.contains("13°C") && fixed.contains("30%"), "{fixed}");
        // today's readings said stand, tomorrow's high said for tomorrow too; tomorrow's high said as today's does not
        assert_eq!(weather_rewrite("Hôm nay Hà Nội mưa nhỏ, 18°C, cao nhất 24°C. Ngày mai 28°C, có mưa nhỏ.", &shown, &degs, 0), None);
        assert_eq!(weather_rewrite("Hôm nay Hà Nội mưa nhỏ, 28°C.", &shown, &degs, 0).as_deref(), Some(""));
        // what the weather check leaves has no current figure: the page's own line replaces it in the figure check
        let ask = "thời tiết Hà Nội hôm nay thế nào?";
        let best = live_sentence(ask, page, true).unwrap();
        assert_eq!(best.sentence, "Thời tiết Hà Nội: Mưa nhỏ 18oC.");
        assert_eq!(best.when, When::Today);
        let held: HashSet<String> = live_figures(page).into_iter().collect();
        let good: HashSet<String> = fresh_figures(&best.sentence).into_iter().collect();
        let values: HashSet<String> = live_candidates(ask, page, true).iter().flat_map(|c| live_figures(&c.sentence)).collect();
        let mut holders: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
        web_holders(page, Some("thoitiet.example"), &mut holders);
        assert!(held.contains("13") && !figures_stand(&live_figures(&fixed), &held, &good, &values, &holders), "{fixed}");
        assert!(figures_stand(&live_figures("Hôm nay Hà Nội mưa nhỏ, 18°C."), &held, &good, &values, &holders));
    }
}
