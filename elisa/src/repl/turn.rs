use super::*;
use super::agent_loop::{CheckRun, WEB_ASK_NOTE};
use super::memory::texts;

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
        if let Some(e) = miss { self.debug(&format!("[router: no answer ({e}); word rules]")); }
        a
    }

    /// The router said (Some) whether this turn has `label`; None = no router answer this turn (the word rules decide).
    pub(super) fn router_says(&self, label: &str) -> Option<bool> { self.routed.as_ref().map(|a| a.has(label)) }

    /// Routed (`route = "auto"`): the working directory reaches the model only once a turn carries tools. A chat
    /// first turn leaves the `Working directory:` line out of its header (with it, a model answers
    /// "what can you do?" as a coding helper for "your current project"); the first tool turn after it starts with
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
    pub(super) fn debug(&mut self, line: &str) {
        if std::env::var("ELISA_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0") { self.fe.info(line); }
    }

    /// A call to a session tool the route left out of this request (`route.rs`): the route widens to that tool's
    /// group (one tool-list change) and the call goes on to the usual approval, so a wrong guess never fails the turn.
    /// False for anything else (a tool the session does not have, the reasoning tools, `route = "off"`).
    pub(super) fn widen_for(&mut self, name: &str) -> bool {
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
        // a turn about the time / date: the local clock as one line (the first message's header has it already), so
        // the model answers directly, conversions included, with no tool (the time server only through the widen path)
        if self.route.groups.has(crate::route::TIME) {
            // before the user's words (after them, a model redoes the offset arithmetic and can get
            // it wrong; right after the local line it reads the answer off)
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
        let about = self.router_says("about_elisa").unwrap_or_else(|| crate::route::about_elisa(&input)).then(|| crate::situation::about(vision));
        // a question about the model itself ("What can this model do for me?"): its detected facts
        let about = about.or_else(|| {
            let (m, v, t, th, ctx) = self.facts();
            let own = context::own_words(context::request_body(&input)).unwrap_or(&input).to_string();
            crate::situation::asks_about_model(&m, v, t, th, ctx, &own).then(|| crate::situation::about_model(&m, v, t, th, ctx))
        });
        // (a tiny window gets none: a small model there read such lines back as its answer)
        let hint = if self.tiny() { None } else { resume_hint.or(about).or_else(|| self.situation(&input)).or_else(|| self.follow_up_hint(&input)) };
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
        // the language line before the ask's own line: what the turn must do is the last thing a small model reads
        // (after "(Reply in English.)" a model answered "search the web: …" from what it knew, with no search)
        if let Some(h) = lang_hint { text.push_str(h); }
        if let Some(n) = self.doc_refused.clone().or_else(|| self.ask_note(&input)).or_else(|| self.unknown_fact_note(&input, &lines)) { text.push_str(&n); }
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
        if !self.route.groups.tool_free() { return None; }
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
        if !self.mcp.builtin_owns(name) { return input; }
        let Some(def) = self.tools.iter().find(|t| t.name == name) else { return input };
        let (out, set) = crate::mcp_args::set_by_meaning(&def.input_schema, &input, context::request_body(&self.input));
        if !set.is_empty() { self.debug(&format!("[{name}: {} set by Elisa from the ask]", set.join(", "))); }
        out
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
            // without it, "save that plan to plan.md" can get "I've saved it" with no write call
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
/// the user note of a request that continues a reply cut by the output limit (`continue_reply`), in that request only
const CONTINUE_NOTE: &str = "Continue exactly where you stopped.";
/// continuations of one cut reply at most
const CONTINUE_MAX: usize = 2;
