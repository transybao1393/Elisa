use super::*;
use super::memory::{is_request, texts};

impl<'f> Session<'f> {

    /// This turn's message asked once more alone: the same model and system prompt, no history, no tools. The
    /// message keeps what Elisa put in it for this turn (clock and "Time in …" lines, computed facts, follow-up hint,
    /// results Elisa ran), since the user's words alone ("and in Tokyo?") lose what the answer needs; only the
    /// conversation before it and the model's failed reply are left out. Also this turn's picture text when it has one (a small model may copy an earlier reply into every later
    /// turn, or answer Elisa's bracketed blocks instead of the ask; asked alone it answers the
    /// ask, as the picture request does), and the document text this turn carries (request-only too: asked alone
    /// about a document without its text, a model answers about something else). The answer when it is usable
    /// (`guard`), else None.
    /// With `lang`: the language note (`reply_check::lang_note`) follows the words, and an answer still not in the
    /// message's language is not usable either.
    fn isolated_reply(&mut self, max_tokens: usize, guard: &context::ReplyGuard, lang: Option<&str>) -> Option<String> {
        let ask = context::request_body(&self.input).trim().to_string();
        if ask.is_empty() { return None; }
        // (the turn's own message as the model first saw it, when it is this ask's: else the words alone)
        let mut text = self.history.get(self.turn_cap(&self.history)).filter(|m| m.role == Role::User).map(texts)
            .filter(|t| context::request_body(t).trim() == ask).unwrap_or_else(|| ask.clone());
        if let Some(p) = &self.turn_pic { text = format!("{text}\n{}", context::picture_block(p)); }
        // (the document text before the words, as `with_turn_extras` places it)
        if let Some(d) = &self.doc_req { text = format!("{d}\n{text}"); }
        // (the message as sent, before the language note: the note names a language, which the garble check would
        // read as a translation ask, "I'm hungry" to "'tôi đói rồi' tiếng Anh là gì?" thrown out as no Vietnamese)
        let seen = text.clone();
        if let Some(n) = lang { text = format!("{text}\n\n{n}"); }
        let msgs = [Message::user_text(text.clone())];
        let no_tools: [crate::types::ToolDef; 0] = [];
        let req = Request { system: &self.system, tools: &no_tools, messages: &msgs, max_tokens };
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
        let raw = texts(&Message::new(Role::Assistant, r.content));
        let echo = crate::reply_check::Echo::new(&context::own_prompt(&self.system, self.project.as_deref()), &text);
        let pre = crate::route::strip_echoed_notes(&context::strip_picture_echo(&crate::reply_check::without_wrappers(&crate::reply_check::unwrapped(&crate::reply_check::without_stray_ends(context::split_state(&raw).0.trim())), &ask)));
        // (Elisa's own first line alone beside a lone yes/no or junk; a loop of a few words is no answer)
        let t = crate::reply_check::beside_own_line(&echo.strip(&pre), &pre, &ask);
        // (nor one that repeats itself or shuffles the user's words, `reply_check::shapeless`)
        let marks_only = crate::reply_check::without_internal_marks(&t).is_some_and(|x| !x.chars().any(char::is_alphanumeric));
        if marks_only || crate::reply_check::word_cycle(&t) || crate::reply_check::shapeless(&t, &ask).is_some() || self.retry_unusable(&t, &ask) {
            self.debug(&format!("[the reply asked alone was unusable too (no words, shapeless or Elisa's own markers)] {}", shown_quote(&t)));
            return None;
        }
        let off = self.turn_pic.as_deref().is_some_and(|p| context::off_picture(&t, p));
        // (a translation is judged by the user's words alone: the note after them names languages too)
        let garbled = crate::reply_check::garbled(&t, &seen) || crate::reply_check::translation_missing(&t, &ask);
        let other_lang = lang.is_some() && self.lang_off(&ask, &t);
        let small = self.small_talk_turn || crate::route::small_talk(&ask) || crate::route::opens_with_greeting(&ask);
        // (a lone yes/no to a greeting or a message that asks no question, or a reply with no letter to a greeting)
        let no_answer = ((small || !ask.trim_end().ends_with('?')) && crate::reply_check::lone_yes_no(&t)) || (small && !t.chars().any(char::is_alphabetic));
        // (and a lone yes/no to an ask wanting more, an earlier line copied, a picture described where there is none)
        let no_answer = no_answer || (crate::reply_check::lone_yes_no(&t) && crate::reply_check::one_word_non_answer(&t, &ask))
            || self.answers_nothing(&t, &ask).is_some_and(|w| w == "copy" || !crate::reply_check::one_word_non_answer(&t, &ask));
        // (an ask for a table answered with none again)
        let no_answer = no_answer || crate::reply_check::missing_table(&t, &ask);
        // (and, on a turn whose picture was looked at, a reply saying it cannot see images)
        let denies = self.picture_turn() && crate::reply_check::denies_sight(&t);
        // (nothing to read, a tag alone, "</s>", among it: `reply_check::has_visible_words`)
        let empty = !crate::reply_check::has_visible_words(&t);
        // (a shell session written out where no command ran: `reply_check::fakes_shell_run`)
        let faked = crate::reply_check::fakes_shell_run(&t);
        let usable = !empty && guard.broken(&t).is_none() && !off && !garbled && !other_lang && !no_answer && !denies && !faked;
        // (the reply asked alone, thrown out too: quoted for the review of a debug transcript)
        if !usable {
            let why = [(empty, "empty"), (guard.broken(&t).is_some(), guard.broken(&t).unwrap_or("")), (off, "off the picture"), (garbled, "garbled"),
                (other_lang, "another language"), (no_answer, "no answer"), (denies, "denied sight"), (faked, "faked run")].iter().filter(|(b, _)| *b).map(|(_, w)| *w).collect::<Vec<_>>().join(", ");
            self.debug(&format!("[the reply asked alone was unusable too ({why})] {}", shown_quote(&t)));
        }
        usable.then(|| t.trim().to_string())
    }

    /// Whether this turn carries a picture's text (`context::picture_block`): the look at a picture gave its answer
    /// (`turn_pic`), or the turn's message holds the block saying none could be made out.
    fn picture_turn(&self) -> bool {
        self.turn_pic.is_some() || self.history.get(self.turn_cap(&self.history)).is_some_and(|m| m.role == Role::User
            && m.content.iter().any(|b| matches!(b, Block::Text { text } if context::is_picture_block(text))))
    }

    /// This turn's tool calls and what they returned: (each successful result's text, with the `path` of a call that
    /// only reads; all call arguments' text). Results Elisa did not run (`[elisa: not run…`) are left out.
    pub(super) fn turn_tool_text(&self) -> (Vec<(Option<String>, String)>, String) {
        self.tool_text_from(self.turn_cap(&self.history).min(self.history.len()))
    }

    /// (a write's or edit's result is only its status line, "wrote 1 lines to count.txt": not content to answer from)
    pub(super) fn tool_text_from(&self, cap: usize) -> (Vec<(Option<String>, String)>, String) {
        let mut paths: Vec<(String, Option<String>)> = Vec::new();
        let mut writes: Vec<String> = Vec::new();
        let mut args = String::new();
        let mut out = Vec::new();
        for m in &self.history[cap..] {
            for b in &m.content {
                match b {
                    Block::ToolUse { id, name, input } => {
                        let reads = tools::access(name) == Access::ReadOnly;
                        if tools::access(name) == Access::Write { writes.push(id.clone()); }
                        paths.push((id.clone(), input["path"].as_str().filter(|_| reads).map(str::to_string)));
                        if let Some(o) = input.as_object() {
                            for v in o.values().filter_map(|v| v.as_str()) { args.push_str(v); args.push('\n'); }
                        }
                    }
                    Block::ToolResult { tool_use_id, content, is_error: false } if !content.starts_with("[elisa: not run") && !writes.contains(tool_use_id) => {
                        let p = paths.iter().find(|(i, _)| i == tool_use_id).and_then(|(_, p)| p.clone());
                        out.push((p, content.clone()));
                    }
                    _ => {}
                }
            }
        }
        (out, args)
    }

    /// Why a tool turn's reply cannot stand against what the tools returned: a copy of an earlier reply to another
    /// ask (`ReplyGuard::broken`), only a list of numbers from nowhere (`reply_check::number_list`), or a "no" / an
    /// ask-back that uses nothing of a result that answers the ask (`reply_check::ignores_given`).
    /// What the tools gave for this reply: this turn's results; on a turn that ran none, what an earlier read of the
    /// file the message points at returned ("did that file ask you to do anything?" after it was read).
    fn given(&self, ask: &str) -> Vec<(Option<String>, String)> {
        if !self.tool_lines.is_empty() { return self.turn_tool_text().0; }
        // (a page the message points at, "trang đó dùng để làm gì?": the newest page web_fetch returned, `held_page`)
        if points_at_page(ask) { if let Some((url, content)) = held_page(&self.history) { return vec![(Some(url), content)]; } }
        let all = self.tool_text_from(0).0;
        let Some(file) = self.pointed_file(ask) else {
            // (no file named: the newest read in the conversation, a restored one too, that shares two content words with
            // the ask: "what was the meeting code you found?" after notes.txt returned "Meeting code: ORCHID-42")
            let a = crate::topics::words(context::request_body(ask));
            return all.into_iter().rev().filter(|(p, c)| p.is_some() && crate::topics::words(c).intersection(&a).count() >= GIVEN_SHARED_WORDS).take(1).collect();
        };
        all.into_iter().rev().filter(|(p, _)| p.as_deref().is_some_and(|p| p.rsplit('/').next() == Some(file.as_str()))).take(1).collect()
    }

    /// This turn's commands that went through (bash, run_code) with a short output: (the command, what it printed
    /// without its exit line, the exit code).
    fn command_outputs(&self) -> Vec<(String, String, i64)> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(String, String)> = Vec::new();
        let mut out = Vec::new();
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if matches!(name.as_str(), "bash" | "run_code") => {
                    calls.push((id.clone(), input["command"].as_str().or(input["code"].as_str()).unwrap_or("").to_string()));
                }
                Block::ToolResult { tool_use_id, content, is_error: false } => {
                    let Some((_, cmd)) = calls.iter().find(|(i, _)| i == tool_use_id) else { continue };
                    if let Some((o, code)) = crate::reply_check::command_output(content) { out.push((cmd.clone(), o, code)); }
                }
                _ => {}
            }
        }
        out
    }

    /// The turn's last command run (`bash`, `run_code`) when it failed (an error result, or an exit code not 0): the
    /// command and its own output (`reply_check::failed_output`). None when the last run went through.
    fn last_failed_run(&self) -> Option<(String, String)> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(String, String)> = Vec::new();
        let mut last = None;
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if matches!(name.as_str(), "bash" | "run_code") => {
                    calls.push((id.clone(), input["command"].as_str().or(input["code"].as_str()).unwrap_or("").to_string()));
                }
                Block::ToolResult { tool_use_id, content, is_error } => {
                    let Some((_, cmd)) = calls.iter().find(|(i, _)| i == tool_use_id) else { continue };
                    let code = content.lines().rev().find_map(|l| l.trim().strip_prefix("[exit ").and_then(|r| r.strip_suffix(']')).and_then(|n| n.trim().parse::<i64>().ok()));
                    last = (*is_error || code.is_some_and(|c| c != 0)).then(|| (cmd.clone(), crate::reply_check::failed_output(content)));
                }
                _ => {}
            }
        }
        last
    }

    /// Elisa's own exact facts for this ask, as text holding their numbers: her count of each file read
    /// (`mcp_reason::count_note`) and the sum the message holds (`reply_check::computed_answer`).
    /// Whether a web tool ran this turn (`route::tool_group`).
    fn web_turn(&self) -> bool {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[cap..].iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { name, .. }
            if crate::route::tool_group(name, self.mcp.server_of(name)) == crate::route::WEB))
    }

    /// `text` (a reply, or Elisa's own answer from the results) about what a read returned that holds an instruction
    /// aimed at an assistant (`reply_check::planted`): with that said plainly, never pinned on the user or denied
    /// (`reply_check::planted_corrected`). `text` as it is otherwise.
    pub(super) fn with_planted(&self, text: &str, ask: &str) -> String {
        // (the actions of every tool Elisa has, not only this turn's: a small window offers few or none, and the
        // planted line is then named by its action, "write a file named pwned.txt", never by its framing)
        let verbs = crate::reply_check::tool_verbs(&self.tools.iter().cloned().chain(tools::defs()).collect::<Vec<_>>());
        let name = |p: &Option<String>| p.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
        let found = |given: &[(Option<String>, String)]| given.iter().filter(|(p, _)| p.is_some()).find_map(|(p, c)| crate::reply_check::planted(c, &verbs).map(|o| (name(p), o, c.clone())));
        let given = self.given(ask);
        // (an instruction an earlier read of the file showed counts, though the file has changed since, the line taken
        // out by an edit: the newest earlier read of that file, the one read now or named, "that file", that holds one)
        let earlier = || {
            let names: Vec<String> = given.iter().map(|(p, _)| name(p)).filter(|n| !n.is_empty()).chain(self.pointed_file(ask)).collect();
            let same: Vec<(Option<String>, String)> = self.tool_text_from(0).0.into_iter().rev().filter(|(p, _)| names.contains(&name(p))).collect();
            found(&same)
        };
        let Some((file, order, c)) = found(&given).or_else(earlier) else { return text.to_string() };
        let line = crate::reply_check::planted_line(ask, &file, &order);
        // (Elisa's answer from the file's other lines, for a reply that gives none of them)
        let real: String = c.lines().filter(|l| crate::reply_check::planted(l, &verbs).is_none()).collect::<Vec<_>>().join("\n");
        // (read this turn and holding it now: the reply answers the ask about the file, and Elisa's answer from its real
        // lines stands in for a reply without one; a later yes/no question about it, "did that file ask you to do
        // anything?", is answered plainly: what it asked and that it was ignored, though the file was read again)
        let holds = |g: &[(Option<String>, String)]| g.iter().filter(|(p, c)| name(p) == file && crate::reply_check::planted(c, &verbs).is_some()).count();
        let now = holds(&self.turn_tool_text().0);
        // (an earlier turn already showed it: the yes line, whatever this turn read)
        let before = holds(&self.tool_text_from(0).0) > now;
        if (now == 0 || before) && crate::reply_check::yes_no_question(ask) {
            let yes = crate::reply_check::planted_yes_line(ask, &order);
            return if text.trim() == yes { text.to_string() } else { yes };
        }
        let summary = if now > 0 { crate::reply_check::answer_from(ask, &file, &real) } else { String::new() };
        crate::reply_check::planted_corrected(text, &order, &line, &summary).unwrap_or_else(|| text.to_string())
    }

    /// The files a read was asked for this turn that were not read: the user (or a rule) said no to it, and no other
    /// read of that file this turn went through. (each file's path, whether the user said no)
    fn denied_reads(&self) -> Vec<(String, bool)> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut calls: Vec<(String, String)> = Vec::new();
        let (mut denied, mut read): (Vec<(String, bool)>, Vec<String>) = (Vec::new(), Vec::new());
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if tools::access(name) == Access::ReadOnly => {
                    if let Some(p) = input["path"].as_str() { calls.push((id.clone(), p.to_string())); }
                }
                Block::ToolResult { tool_use_id, content, is_error } => {
                    let Some((_, p)) = calls.iter().find(|(i, _)| i == tool_use_id) else { continue };
                    if *is_error && content.starts_with("denied") { denied.push((p.clone(), content.starts_with("denied by user"))); }
                    else if !*is_error { read.push(p.clone()); }
                }
                _ => {}
            }
        }
        denied.retain(|(p, _)| !read.contains(p));
        denied
    }

    pub(super) fn own_numbers(&self, ask: &str, results: &[(Option<String>, String)]) -> String {
        let body = context::request_body(ask);
        let mut out: Vec<String> = results.iter().filter_map(|(p, _)| p.as_deref()).filter_map(|p| {
            std::fs::read_to_string(self.cwd.join(p)).ok().and_then(|t| crate::mcp_reason::count_note(body, p, &t))
        }).collect();
        out.extend(crate::reply_check::computed_answer(ask));
        out.join("\n")
    }

    fn given_fault(&self, vis: &str, ask: &str, guard: &context::ReplyGuard) -> Option<&'static str> {
        // a command run this turn whose short output the reply does not carry ("Hello, this is Elisa." for what
        // `echo hello-elisa` printed, or the command written back)
        // (only a command the message names: "run the command: echo hello-elisa", "use wc"; a command the model ran
        // on its own to check its work need not be quoted)
        if let Some((cmd, o, _)) = self.command_outputs().last() {
            if crate::reply_check::names_command(ask, cmd) && !crate::reply_check::carries_output(vis, cmd, o) { return Some(LEFT_OUT_OUTPUT); }
        }
        let results = self.given(ask);
        if results.is_empty() { return None; }
        let all: String = results.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        // a reply that reports an action done or says the result in its own words stands: what the calls were given
        // (the edit's new text, the command, the file written) counts as said too
        let args = if self.tool_lines.is_empty() { String::new() } else { self.turn_tool_text().1 };
        let said = format!("{all}\n{args}");
        // a change made this turn (a write or edit that went through): the reply reports it, never a denial of a result
        let changed = self.tool_lines.iter().any(|l| ["[write ", "[edit ", "[edit_symbol "].iter().any(|p| l.starts_with(p)) && !l.contains(" → "));
        // (a short answer stands: "c", "4", "Yes"; only a copy, a list of numbers or a denial does not)
        if guard.broken(vis) == Some("copy") { return Some("copied an earlier reply"); }
        let known: std::collections::HashSet<String> = crate::reply_check::numbers(&format!("{ask}\n{all}")).into_iter().collect();
        if crate::reply_check::number_list(vis, &known) { return Some("was only a list of numbers"); }
        // a lone number that nothing returned ("1." after notes.txt was read and counted at 8)
        // (a number Elisa worked out herself stands: a count of a read file, a computed result, what a call wrote)
        if crate::reply_check::bare_number_unbacked(vis, ask, &format!("{said}\n{}", self.own_numbers(ask, &results))) { return Some("was a lone number that no result holds"); }
        if !changed && crate::reply_check::ignores_given(vis, ask, &said, false) { return Some("denied having what a tool returned"); }
        // a reply about a file read this turn that holds nothing of what it returned (another file's lines)
        if crate::reply_check::strays_from_named_file(vis, ask, &results) { return Some("said things the file it names does not hold"); }
        // a reply about a page the message points at that holds nothing of what the page says (`strays_from_held`)
        if self.tool_lines.is_empty() && results.iter().any(|(p, c)| p.as_deref().is_some_and(|p| p.starts_with("http")) && crate::reply_check::strays_from_held(vis, ask, c)) {
            return Some("said things the page it points at does not hold");
        }
        None
    }

    /// Whether the chat holds a picture: one sent, one this turn looked at, or one waiting to be sent.
    pub(super) fn chat_has_picture(&self) -> bool {
        self.turn_pic.is_some() || !self.last_pics.is_empty() || !self.attach.is_empty()
            || self.history.iter().any(|m| m.content.iter().any(|b| match b { Block::Image { .. } => true, Block::Text { text } => context::is_picture_block(text), _ => false }))
    }

    /// What came before this turn that a reply may copy a line of (`reply_check::copies_earlier_line`): what the
    /// session's tools returned, the user's earlier messages and the notes Elisa keeps in her prompt.
    fn earlier_lines(&self) -> String {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let results = self.tool_text_from(0).0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        let said = self.history[..cap].iter().filter(|m| m.role == Role::User).map(texts).collect::<Vec<_>>().join("\n");
        format!("{results}\n{said}\n{}", self.system)
    }

    /// A reply with no tool call this turn that answers nothing: one word to an ask that wants more ("Image"), a bare
    /// line copied from earlier in the chat ("Meeting code: ORCHID-42" to "tell me a short joke"), a claim to be a
    /// human being ("I'm not a robot, I'm a human.", against Elisa's own facts), a description of the message's own
    /// words where none was asked for ("\"I\" and \"my\" are used in your messages…"), or a picture described where
    /// the chat holds none.
    fn answers_nothing(&self, vis: &str, ask: &str) -> Option<&'static str> {
        // (on a turn that carries a document's text, a word or two the text never says, "Visual data", is no answer
        // to a question about it, while one it does say, "Financial", stands: `reply_check::few_words_of_nothing`)
        let doc = self.doc_text_held();
        if doc.is_empty() { if crate::reply_check::one_word_non_answer(vis, ask) { return Some("no answer"); } }
        else if crate::reply_check::few_words_of_nothing(vis, ask, &doc) { return Some("no answer"); }
        if crate::reply_check::copies_earlier_line(vis, ask, &self.earlier_lines()) { return Some("copy"); }
        if crate::reply_check::claims_human(vis) { return Some("claims human"); }
        if crate::reply_check::about_message_words(vis, ask) { return Some("about the words"); }
        // (or Elisa's own prompt or clock line said back in other words, "You are a friendly assistant who is helping you.")
        if self.restates_prompt(vis, ask) { return Some("about the words"); }
        // (and a reply that only tells the conversation back, or writes Elisa's context as markup)
        let own: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().chain([self.client.model().to_string()]).collect();
        if crate::reply_check::retells_only(vis, &own) || crate::reply_check::leaks_markup(vis, ask) { return Some("about the words"); }
        // (or a greeting alone where something else was asked, "Hello! How can I assist you today?" to a price)
        if crate::reply_check::greets_instead(vis, ask) { return Some("no answer"); }
        // (or, to small talk, nothing but an offer or a question back once its list and pitch are out, "Đã hiểu! Cần
        // gì để giúp đỡ?" to a bit of the day: `reply_check::only_offers`)
        if self.small_talk_turn || crate::route::small_talk(ask) || crate::reply_check::shares_a_moment(ask) {
            let trimmed = crate::reply_check::small_talk_reply_trimmed(vis, ask, &own).unwrap_or_else(|| vis.to_string());
            if crate::reply_check::only_offers(&trimmed, ask) { return Some("no answer"); }
        }
        (!self.chat_has_picture() && crate::reply_check::describes_picture(vis, ask)).then_some("no answer")
    }

    /// The text of the attached documents this turn's request carries (`doc_req`: the kept documents' text), empty
    /// on a turn that carries none.
    fn doc_text_held(&self) -> String {
        if self.doc_req.is_none() { return String::new(); }
        self.kept_docs.iter().map(|k| k.doc.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The finished reply checked against what this turn gave, before it is kept:
    /// - `ignored`: it still ignored the tool results after the note (`given_fault`): Elisa answers from them;
    /// - a code block that quotes a file this turn's tools touched but holds lines the file does not (the model never
    ///   read them): the file's real lines in its place (`reply_check::invented_code`);
    /// - a question about a file no tool read this session, on a turn that ran none: the plain "I haven't read it"
    ///   line (`unread_file`), whatever the reply claimed about it.
    fn checked_reply(&mut self, vis: String, ask: &str, fault: Option<&'static str>) -> String {
        let ignored = fault.is_some();
        // the tools' names are Elisa's, not the user's words: a clause naming one goes
        let named: Vec<(String, String)> = self.tools.iter().map(|t| (t.name.clone(), t.description.clone())).collect();
        // a reply that copies a tool's own text (its description, or the note its result ends with) answers nothing:
        // Elisa's own answer from the results, else her plain line
        let results = self.turn_tool_text().0.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        let vis = if crate::reply_check::copies_tool_text(&vis, &named, &results) {
            let t = self.own_answer_from_turn().unwrap_or_else(|| self.no_answer_line(ask));
            let t = self.said_own(t);
            self.fe.reply_retracted();
            if self.interactive { self.fe.text_delta(&t); }
            self.debug("[the reply copied a tool's own text; Elisa answered from the results]");
            t
        } else { vis };
        // a result claimed after the turn's last command failed ("Python 3.8.5" after "python3: command not
        // found"): Elisa's plain line that it did not run
        let vis = match self.last_failed_run().filter(|(cmd, err)| crate::reply_check::claims_after_failed_run(&vis, ask, cmd, err, &results)) {
            Some((cmd, err)) => {
                let t = crate::reply_check::failed_run_line(ask, &cmd, &err, crate::sandbox::mode() != crate::sandbox::Mode::Off);
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply claimed a result after the command failed; Elisa said it did not run]");
                t
            }
            None => vis,
        };
        let vis = match crate::reply_check::without_tool_names(&vis, &named).filter(|t| !t.is_empty() && *t != vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply named a tool; that part was put in plain words]");
                t
            }
            None => vis,
        };
        // a question about one thing Elisa can do ("can you search the web?", "bạn có đọc được file trên máy mình
        // không?"): her own answer from every tool she has and the model's facts, whatever the reply said
        // (`situation::ability_answer`)
        if self.tool_lines.is_empty() {
            if let Some(t) = self.ability_line_for(ask, &vis) {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[a question about what Elisa can do; she answered from her tools]");
                return self.said_own(t);
            }
        }
        // a question pointing at a page Elisa holds ("trang đó dùng để làm gì?", "what is that page for?"), naming no
        // URL of its own (`held_page_turn`: a turn that ran no tool, or one whose search the page makes needless): her
        // own line from the page leads, verified first and the same on every model (the page's first sentence quoted,
        // or its meaning in the question's language, checked: `held_page_line`); what the model said follows as a
        // short note (PAGE_NOTE_SENTENCES sentences) only when it passes the gate and says more than the quote
        if self.held_page_turn(ask) {
            if let Some(line) = self.held_page_line(ask) {
                // (nor a piece of this turn's other results, a search title the loop guard left, nor mostly passages
                // of any result of the conversation, an earlier search's snippet: `piece_of_results`, `mostly_results`)
                let got = self.turn_calls().1;
                let all = self.tool_text_from(0).0.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n");
                let note = self.gate_fault(&vis, ask).ok().map(|m| crate::reply_check::leading_sentences(&m.text, PAGE_NOTE_SENTENCES))
                    .filter(|n| n.chars().any(char::is_alphabetic) && !line.contains(n.trim_end_matches(['.', '!'])) && !crate::reply_check::piece_of_results(n, &got)
                        && !crate::reply_check::mostly_results(n, &format!("{got}\n{all}")));
                let t = match note { Some(n) => format!("{line}\n\n{n}"), None => line };
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[a question about a page Elisa holds: her line from the page leads; the model's short note follows when it passes the gate]");
                return self.said_own(t);
            }
        }
        // a reply to a plain greeting that does not greet, or says goodbye: Elisa's greeting line
        let own: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().collect();
        if let Some(t) = crate::reply_check::greeting_reply_fixed(&vis, ask, self.first_turn(), &own) {
            self.fe.reply_retracted();
            if self.interactive { self.fe.text_delta(&t); }
            self.debug("[the reply to a greeting did not greet; Elisa greeted back]");
            return self.said_own(t);
        }
        // a reply to a plain closer ("ok thx", "cảm ơn nhé") that pitches, runs long or carries a fact of the chat
        // ("…với ORCHID!"): Elisa's own closing line, or the reply without that sentence (`closer_fixed`)
        // (an offer list or an unasked pitch after a greeting or a feeling: `reply_check::small_talk_reply_trimmed`)
        if self.tool_lines.is_empty() {
            if let Some(t) = crate::reply_check::small_talk_reply_trimmed(&vis, ask, &own) {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply to small talk held an offer list or an unasked pitch; those parts went]");
                return t;
            }
        }
        if let Some(t) = self.closer_fixed(&vis, ask, &own) {
            self.fe.reply_retracted();
            if self.interactive { self.fe.text_delta(&t); }
            self.debug("[the reply to a closer pitched or carried a fact of the chat; Elisa closed in her own words]");
            return t;
        }
        // a message that only tells facts about the user, answered with just a fact's value or one word ("Linh" to
        // "My name is Linh and I live in Hanoi."): Elisa's own line taking note of them
        if self.tool_lines.is_empty() {
            if let Some(t) = crate::reply_check::fact_ack_for_value(&vis, ask) {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply to a told fact was only its value; Elisa took note of the fact]");
                return self.said_own(t);
            }
        }
        let own: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().chain([self.client.model().to_string()]).collect();
        // a question about the user's own facts answered with Elisa's own name ("Elisa" to "what did I tell you my
        // name was?", moondream:1.8b after a model switch into the saved conversation), with a word or two that are
        // no value of theirs, or with the asked thing given another value: Elisa's answer from her facts (the notes,
        // the recalled memories, the conversation)
        if self.tool_lines.is_empty() {
            if let Some(t) = self.reply_facts().recall_answer(&vis, &own) {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply to a question about the user gave none of their facts; Elisa answered from them]");
                return self.said_own(t);
            }
        }
        // a sentence naming the assistant with one of the user's names ("Bo here." for the user's dog) goes
        let known = format!("{}\n{}\n{}", self.system, self.history.iter().filter(|m| m.role == Role::User).map(texts).collect::<Vec<_>>().join("\n"), self.input);
        // (a reply that is only such a claim is asked again after the turn, `claimed_facts_retry`)
        let vis = match crate::reply_check::without_false_self(&vis, &own, &known).filter(|t| !t.is_empty()) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply named itself with one of the user's names; that sentence was taken out]");
                t
            }
            None => vis,
        };
        // a fact about the user that nothing Elisa holds backs ("Your name is Jonathan." where no name was ever
        // given), or a thing the user is said to have given in an earlier message that they never did ("the timezone
        // you provided earlier"): those sentences go; nothing left, the answer Elisa holds (a fact asked that is not
        // known, the results) or her plain line
        let current = self.history.iter().rposition(is_request);
        let earlier: String = self.history.iter().enumerate().filter(|(i, m)| m.role == Role::User && Some(*i) != current)
            .map(|(_, m)| context::request_body(&texts(m)).to_string()).chain(context::fact_lines(&known)).collect::<Vec<_>>().join("\n");
        let held = format!("{known}\n{results}\n{}\n{}", self.first_notes().unwrap_or_default(), self.state_block().unwrap_or_default());
        let vis = match crate::reply_check::without_unbacked_user_facts(&vis, &held, &earlier, ask) {
            Some(t) => {
                let t = if !t.is_empty() { t } else if let Some(s) = self.unknown_own_fact(ask) { crate::reply_check::unknown_fact_line(ask, &s) }
                    else { self.own_answer_from_turn().unwrap_or_else(|| self.no_answer_line(ask)) };
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply stated a fact about the user that nothing given backs; that sentence was taken out]");
                t
            }
            None => vis,
        };
        // a guess opening a sentence that states a value Elisa gave this turn ("I think the current time is 19:25"
        // after her clock line): the value is exact, so the guess goes
        let given = format!("{}\n{results}", self.history.iter().rev().find(|m| is_request(m)).map(texts).unwrap_or_default());
        let vis = match crate::reply_check::without_hedged_given(&vis, &given) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply hedged a value Elisa gave; the hedge was taken out]");
                t
            }
            None => vis,
        };
        // a "can't" about tools that ran this turn, and Elisa's own terms ("state card"): those sentences go; nothing
        // left, Elisa answers from the results
        let ran = !self.turn_tool_text().0.is_empty();
        let held = ran || !self.tool_text_from(0).0.is_empty();
        let vis = match crate::reply_check::without_false_cant(&vis, ask, ran, held) {
            Some(t) => {
                let t = if t.is_empty() { self.own_answer_from_turn().unwrap_or_else(|| self.no_answer_line(ask)) } else { t };
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply said it could not do what ran, or used Elisa's own terms; those sentences were taken out]");
                t
            }
            None => vis,
        };
        // a sentence telling the model's own instructions ("The user wants me to answer…") goes; one crediting what a
        // file or tool returned to the user ("The user provided a shopping list…") is said of the source
        let vis = match crate::reply_check::without_meta_narration(&vis) {
            Some(t) => {
                let t = if t.is_empty() { self.own_answer_from_turn().unwrap_or_else(|| self.no_answer_line(ask)) } else { t };
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply told its own instructions; those sentences were taken out]");
                t
            }
            None => vis,
        };
        // a lead-in crediting the answer to the model's output ("The model output showed that …") goes, the rest kept
        let vis = match crate::reply_check::without_source_lead(&vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply credited its answer to the model's output; that lead-in was taken out]");
                t
            }
            None => vis,
        };
        // a sentence blaming the user's message ("there seems to be a mistake in your message") beside an answer goes
        let vis = match crate::reply_check::without_false_blame(&vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply blamed the user's message; that sentence was taken out]");
                t
            }
            None => vis,
        };
        // a sentence describing the message's own words ("\"I\" is used to refer to the user") where nothing about
        // words was asked goes; a reply of nothing else gets Elisa's answer from the turn, else her plain line
        let vis = match crate::reply_check::without_message_meta(&vis, ask) {
            Some(t) => {
                let t = if t.is_empty() { self.own_answer_from_turn().unwrap_or_else(|| self.no_answer_line(ask)) } else { t };
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply described the message's words; those sentences were taken out]");
                t
            }
            None => vis,
        };
        // a status line ("No new commands run.") on a turn that ran nothing, and pasted text said back after a short
        // reply ("OK. <the user's own sentence>"), go
        let vis = match crate::reply_check::without_status_and_paste(&vis, ask, ran) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply gave a status line or said back the user's pasted text; that part was taken out]");
                t
            }
            None => vis,
        };
        // a bare list of values ("[ 'Linh', 'ORCHID-42' ]"): Elisa's answer from the facts she holds, her resume line
        // for a resume ask, else the values in words
        let vis = match crate::reply_check::bare_values(&vis) {
            Some(v) => {
                let t = self.reply_facts().facts_answer()
                    .unwrap_or_else(|| if self.resume_ask(ask) { self.no_answer_line(ask) } else { v.join(", ") });
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply was a bare list of values; Elisa said it in words]");
                self.said_own(t)
            }
            None => vis,
        };
        // a list of rows for an asked table: Elisa's markdown table of them
        let vis = match crate::reply_check::rows_as_table(&vis, ask) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply gave a table's rows as a list; Elisa showed them as a table]");
                t
            }
            None => vis,
        };
        // one short word said over and over ("OK OK"): said once
        let vis = match crate::reply_check::one_repeated_word(&vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply said one word over and over; it was said once]");
                t
            }
            None => vis,
        };
        let (results, _) = self.turn_tool_text();
        let vi = crate::reply_lang::detect(ask) == Some(crate::reply_lang::Lang::Vi);
        let source = results.iter().find_map(|(p, _)| p.clone()).unwrap_or_else(|| if vi { "Kết quả".into() } else { "The result".into() });
        let vis = match crate::reply_check::credited_to_source(&vis, &source, vi).filter(|_| !results.is_empty()) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply credited what was read to the user; it was said of the source]");
                t
            }
            None => vis,
        };
        // the user spoken of in the third person ("The user should consider updating…"): spoken to
        let vis = match crate::reply_check::said_to_user(&vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply spoke of the user in the third person; it was said to them]");
                t
            }
            None => vis,
        };
        // a near miss of a distinctive token this turn's tools returned ("Papaaya." for PAPAYA): the exact token
        // (and of a value Elisa keeps: the user's own messages and her fact lines, "Orchid-42" for ORCHID-42)
        let given_now = self.turn_tool_text().0.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        let said: Vec<String> = self.history.iter().filter(|m| m.role == Role::User).map(|m| context::request_body(&texts(m)).to_string()).collect();
        // (her notes ride in the system prompt or at the top of the first message)
        let notes = format!("{}\n{}", self.system, self.history.iter().filter(|m| m.role == Role::User).map(texts).collect::<Vec<_>>().join("\n"));
        let stored = format!("{given_now}\n{}\n{}\n{}", context::fact_lines(&notes).join("\n"), said.join("\n"), context::request_body(&self.input));
        let vis = match crate::reply_check::near_misses_fixed(&vis, &stored) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply mistyped a value the result holds; it was put right]");
                t
            }
            None => vis,
        };
        // an id cut off short ("CVE-2024-47." for CVE-2024-47081 in the results): the whole id
        let vis = match crate::reply_check::cut_ids_completed(&vis, &given_now) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply cut an id short; it was given whole from the results]");
                t
            }
            None => vis,
        };
        // a closing promise of more work: the turn ended without it
        let vis = match crate::reply_check::without_closing_promise(&vis) {
            Some(t) => {
                self.fe.reply_retracted();
                if self.interactive { self.fe.text_delta(&t); }
                self.debug("[the reply closed with a promise of work it did not do; that part was taken out]");
                t
            }
            None => vis,
        };
        let (results, args) = self.turn_tool_text();
        let results = if ignored { self.given(ask) } else { results };
        let shown = |s: &mut Self, t: String, why: &str| {
            let t = s.said_own(t);
            s.fe.reply_retracted();
            if s.interactive && !t.is_empty() { s.fe.text_delta(&t); }
            s.debug(why);
            t
        };
        // a command's output left out twice: Elisa says what it printed
        if fault == Some(LEFT_OUT_OUTPUT) {
            if let Some(t) = self.command_outputs().last().map(|(_, o, code)| crate::reply_check::printed_line(ask, o, *code)) {
                return shown(self, t, "[the reply still left out what the command printed; Elisa said it]");
            }
        }
        // a read that was said no to: a reply that does not say so shows content Elisa never saw
        // (a "no" somewhere does not cover content shown beside it)
        if let Some((p, by_user)) = self.denied_reads().into_iter().next() {
            // (and one that repeats the note the refused read returned)
            if !crate::reply_check::says_not(&vis) || crate::reply_check::presents_content(&vis) || crate::reply_check::copies_run(&vis, &self.denial_notes()) {
                return shown(self, crate::reply_check::not_read_line(ask, &p, by_user), "[a read was said no to and the reply showed content; Elisa said it was not read]");
            }
        }
        // a scan's findings when the reply ignored them, or is the user's message again: Elisa's own list
        if ignored || crate::reply_check::copies_ask(&vis, ask) {
            if let Some(f) = results.iter().map(|(_, c)| crate::reply_check::scan_findings(c)).find(|f| !f.is_empty()) {
                return shown(self, crate::reply_check::findings_line(ask, &f), "[the reply did not give the scan's findings; Elisa listed them]");
            }
        }
        // (a reply that is the user's message again after tools ran: Elisa answers from the results)
        let ignored = ignored || (crate::reply_check::copies_ask(&vis, ask) && !results.is_empty());
        if ignored && !results.is_empty() {
            // Elisa's own answer from the question and the results (`own_answer_from_turn`; never page text)
            if let Some(t) = self.own_answer_from_turn() {
                let t = self.with_planted(&t, ask);
                return shown(self, t, "[the reply still ignored what the tools returned; Elisa answered from the results]");
            }
            // (a small file held whose lines the ask asks of, "what else is in that file?" among them: the lines it asks
            // for and not yet told, as `held_file_answer` gives them, never the whole result again)
            if let Some(t) = self.held_file_answer("", ask) {
                return shown(self, t, "[the reply still ignored what the tools returned; Elisa answered from the file]");
            }
            // the result that answers the ask: the one sharing its words, else the last
            let pick = results.iter().rev().find(|(_, c)| crate::reply_check::shares_words(ask, c))
                .or(results.last()).cloned().unwrap_or_default();
            let label = pick.0.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
            let t = crate::reply_check::answer_from(ask, &label, &pick.1);
            let t = if t.is_empty() { self.no_answer_line(ask) } else { self.with_planted(&t, ask) };
            return shown(self, t, "[the reply still ignored what the tools returned; Elisa answered from the results]");
        }
        // a verdict against a warning a check raised this turn ("the token is safe" after alg none): the warning stated
        if let Some(t) = results.iter().map(|(_, c)| crate::reply_check::warnings(c)).find(|w| !w.is_empty())
            .and_then(|w| crate::reply_check::verdict_corrected(&vis, ask, &w)) {
            return shown(self, t, "[the reply's verdict went against a check's warning; Elisa stated the warning]");
        }
        // a reply that is only a piece of the results list after a web search ("1", "1. Iceland Population - Worldometer"):
        // Elisa's own answer
        // from the results (the number asked for is in a sentence there: "the Icelandic population is 394,324")
        if crate::reply_check::list_piece(&vis) && self.web_turn() {
            // (Elisa's worded answer, unless it is a piece of the list too; else the results' sentence that answers)
            let all = results.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
            if let Some(t) = self.own_answer_from_turn().filter(|t| !crate::reply_check::list_piece(t)).or_else(|| crate::reply_check::best_sentence(ask, &all)) {
                return shown(self, t, "[the reply was only a piece of the web results; Elisa answered from the results]");
            }
        }
        // a number after a web search that none of the results holds: Elisa's own answer from them (one that holds
        // its numbers), else the results' sentence that answers
        if self.web_turn() {
            let all = results.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
            if crate::reply_check::number_not_in(&vis, ask, &all) {
                if let Some(t) = self.own_answer_from_turn().filter(|t| !crate::reply_check::list_piece(t) && !crate::reply_check::number_not_in(t, ask, &all))
                    .or_else(|| crate::reply_check::best_sentence(ask, &all)) {
                    return shown(self, t, "[the reply gave a number none of the web results holds; Elisa answered from the results]");
                }
            }
        }
        // a scan's findings: a reply leaving out a file the scan found something in gets Elisa's own list
        for (_, c) in &results {
            let f = crate::reply_check::scan_findings(c);
            if crate::reply_check::misses_findings(&vis, &f, c) {
                return shown(self, crate::reply_check::findings_line(ask, &f), "[the reply did not match the scan's findings; Elisa listed them]");
            }
        }
        let given = self.given(ask);
        let name = |p: &Option<String>| p.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
        // an instruction in what a read returned aimed at an assistant: said plainly, never pinned on the user or denied
        let t = self.with_planted(&vis, ask);
        if t != vis { return shown(self, t, "[a read file holds an instruction aimed at an assistant; the reply says so plainly]"); }
        // the ask names a label a result holds as "Label: value" and the reply gives another value: the result's line
        if let Some((p, line)) = given.iter().find_map(|(p, c)| crate::reply_check::wrong_value(&vis, ask, c).map(|l| (p, l))) {
            return shown(self, crate::reply_check::answer_from(ask, &name(p), &line), "[the reply gave another value than the result holds; Elisa answered from it]");
        }
        // code quoted from a file: on a tool turn, from the files it touched; on a turn that ran none, a listing with line
        // numbers, or any block in a reply that advises nothing, from any file this session touched (a quote of the
        // model's own unapplied fix as the file)
        let tool_turn = !results.is_empty() || !args.is_empty();
        let from = if tool_turn { self.turn_cap(&self.history).min(self.history.len()) } else { 0 };
        let root = self.cwd.clone();
        let mut files: Vec<(String, String)> = Vec::new();
        for p in self.history[from..].iter().flat_map(|m| &m.content).filter_map(|b| match b {
            Block::ToolUse { input, .. } => input["path"].as_str().map(str::to_string),
            _ => None,
        }) {
            let full = root.join(&p);
            if files.iter().any(|f| f.0 == p) || !full.starts_with(&root) || !full.is_file() { continue; }
            if let Ok(t) = std::fs::read_to_string(&full) { if t.len() <= CODE_CHECK_MAX_BYTES { files.push((p, t)); } }
        }
        let seen = format!("{args}\n{}", results.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n"));
        if let Some(t) = crate::reply_check::invented_code(&vis, &files, &seen, !tool_turn && crate::reply_check::advises(&vis)) {
            return shown(self, t, "[the reply quoted code its file does not hold; the file's real lines are shown]");
        }
        // a question about the user's own thing answered with the assistant's own name: not known
        if self.tool_lines.is_empty() {
            let names: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().chain([self.client.model().to_string()]).collect();
            let said: String = self.history.iter().filter(|m| m.role == Role::User).map(|m| context::request_body(&texts(m)).to_string()).collect::<Vec<_>>().join("\n");
            if crate::reply_check::gives_own_name(&vis, ask, &said, &names) {
                return shown(self, crate::reply_check::dont_know_line(ask).to_string(), "[a question about the user's own thing was answered with the assistant's own name]");
            }
        }
        // a fact about the user (their favourite colour) that nothing Elisa holds tells, given as known: not known
        if self.tool_lines.is_empty() && crate::reply_check::states_value(&vis) {
            if let Some(subject) = self.unknown_own_fact(ask) {
                return shown(self, crate::reply_check::unknown_fact_line(ask, &subject), "[a fact about the user that nothing told was given; Elisa said it is not known]");
            }
        }
        if self.tool_lines.is_empty() {
            if let Some(f) = self.unread_file(ask) {
                let t = crate::reply_check::unread_line(ask, Some(&f));
                if vis.trim() != t { return shown(self, t, "[a question about a file Elisa has not read; the reply's claim was replaced]"); }
            }
        }
        // (the prompt said back beside an answer, a tool action or result claimed with no such call, a figure stated as
        // today's with nothing behind it, an answer that misses what the held file says: `content_fixes`, on whatever
        // this check hands back, at its call)
        vis
    }

    /// The thing of the user's own a question asks about ("What's my favourite colour?": "favourite colour") when
    /// nothing Elisa holds tells it: the notes, pinned facts, what this message carries (recalled memories), and what
    /// the user said in this conversation (an earlier asking of the same thing tells nothing).
    fn unknown_own_fact(&self, ask: &str) -> Option<String> {
        let subject = crate::situation::own_fact_asked(ask)?;
        let mut known = String::new();
        for t in [self.first_notes(), self.state_block()].into_iter().flatten() { known += &t; known += "\n"; }
        let current = self.history.iter().rposition(is_request);
        for m in self.history.iter().enumerate().filter(|(i, m)| m.role == Role::User && (Some(*i) == current || crate::situation::own_fact_asked(context::request_body(&texts(m))).is_none())).map(|(_, m)| texts(m)) {
            known += &m;
            known += "\n";
        }
        // (this message less the user's words and Elisa's note that the thing is not known)
        let known = known.replace(&crate::situation::unknown_fact_note(&subject), " ").replace(ask.trim(), " ");
        (!crate::situation::subject_known(&subject, &known)).then_some(subject)
    }

    /// The notes this turn's refused reads returned ("denied by user (…): the user said no, so it was NOT read…").
    fn denial_notes(&self) -> String {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[cap..].iter().flat_map(|m| &m.content).filter_map(|b| match b {
            Block::ToolResult { content, is_error: true, .. } if content.starts_with("denied") => Some(content.as_str()),
            _ => None,
        }).collect::<Vec<_>>().join("\n")
    }

    /// `vis` on a turn whose read was said no to: when it shows content, the plain "not read" line instead.
    fn denied_reply(&mut self, vis: String, ask: &str) -> String {
        let Some((p, by_user)) = self.denied_reads().into_iter().next() else { return vis };
        if crate::reply_check::says_not(&vis) && !crate::reply_check::presents_content(&vis) && !crate::reply_check::copies_run(&vis, &self.denial_notes()) { return vis; }
        let t = crate::reply_check::not_read_line(ask, &p, by_user);
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&t); }
        self.debug("[a read was said no to and the reply showed content; Elisa said it was not read]");
        t
    }

    /// The file a question is about (named in it, or "that file" after a message that named one) when no tool read it
    /// in this session and no attached document is it: Elisa knows nothing of its content.
    fn unread_file(&self, ask: &str) -> Option<String> {
        let key = self.pointed_file(ask)?;
        let read = self.history.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { input, .. }
            if input.as_object().is_some_and(|o| o.values().filter_map(|v| v.as_str()).any(|v| v.contains(&key)))))
            || self.kept_docs.iter().any(|k| k.doc.name == key)
            // (a folded session's notes keep what its turns did)
            || self.notes.as_deref().is_some_and(|n| n.contains(&key));
        (!read).then_some(key)
    }

    /// The file (its name) a question is about: named in it, or "that file" after a message that named one; one of
    /// this project's files. None for anything else, and on a turn with attached documents.
    fn pointed_file(&self, ask: &str) -> Option<String> {
        if !ask.trim_end().ends_with('?') || self.doc_req.is_some() { return None; }
        let named = |t: &str| t.split_whitespace().filter_map(|w| crate::route::file_name(w.trim_matches(|c: char| "\"'`<>()[],;:?!".contains(c))))
            .find(|f| self.cwd.join(f).is_file());
        let low = ask.to_lowercase();
        // "that file", "this file", "the file" (and the Vietnamese "tệp đó" / "tệp này"): the file the last message
        // that named one meant
        let points = ["that file", "this file", "the file", "tệp đó", "tệp này", "file đó", "file này"].iter().any(|p| low.contains(p));
        let file = named(ask).or_else(|| points.then(|| self.history.iter().rev().filter(|m| is_request(m)).find_map(|m| named(context::request_body(&texts(m))))).flatten())?;
        Some(file.rsplit('/').next().unwrap_or(&file).to_string())
    }

    /// The last reply, which refused the attached documents `names` ("a.xlsx, b.ods") twice, replaced by Elisa's own
    /// short answer from each one's text (`reply_check::answer_from`).
    pub(super) fn docs_answer(&mut self, names: &str) {
        let ask = context::request_body(&self.input).to_string();
        let parts: Vec<String> = names.split(", ").filter_map(|n| self.kept_docs.iter().rev().find(|k| k.doc.name == n))
            // (what each holds in plain words, never its raw rows or sheet markers: `turn::doc_in_words`)
            .map(|k| super::turn::doc_in_words(&k.doc.name, &k.doc.text, crate::route::vietnamese(&ask)))
            .filter(|t| !t.is_empty()).collect();
        if parts.is_empty() { return; }
        let t = parts.join("\n\n");
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&t); }
        if let Some(m) = self.history.last_mut().filter(|m| m.role == Role::Assistant) { m.content = vec![Block::Text { text: t }]; }
        self.debug("[the reply refused the attached documents twice; Elisa answered from their text]");
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
        let mut empty_retry: Option<std::borrow::Cow<'static, str>> = None;
        let mut retried = false;
        let mut stashed_card: Option<String> = None;
        // a simple turn's reply that used its whole small budget with no text for the user: sent once more without
        // the simple-turn limits (otherwise a two-question turn judged simple shows only the stop line)
        let mut widened = false;
        // a small-talk turn whose reply called a tool: asked once more with no tools and a note in that request only
        // (or a turn asked again in words with its own note: `retry_note`)
        let mut words_retry = self.retry_note.is_some();
        let mut words_retried = words_retry;
        // a reply in another language than the user's message: asked once more with a note naming it
        let mut lang_retried = false;
        // (a short follow-up the reply asked to have put another way: asked once more spelt out, `follow_up_note`)
        let mut follow_retried = false;
        // the reply that the language retry took back: kept when the retry comes back no better (another language
        // again, or garbled)
        let mut lang_first: Option<String> = None;
        // a reply that repeated the user's earlier words: asked once more with the conversation (`copies_said`)
        let mut copy_retried = false;
        // a reply on a tool turn that ignores what the tools returned (a "no" or a copy of an earlier reply, or only
        // numbers): asked once more with a note (`given_fault`); still so, Elisa answers from the results
        let mut given_retried = false;
        let ask = context::request_body(&self.input).trim().to_string();
        // one step past the limit: only for the no-tools final answer after the last tool step
        let max_steps = self.cfg.max_steps.max(1);
        for step in 0..=max_steps {
            if step == max_steps && final_ask.is_none() { break; }
            if !self.mid_turn_fold() {
                // sent anyway, a request over the window gets nothing back from the server
                // the user's own message too long on its own (no tool step yet, `message_alone_too_big`): nothing can
                // make room for it, so they are told, and it leaves the conversation as it was (a resumed chat never
                // starts with it); otherwise the results or the history outgrew the window, which is no fault of
                // theirs: a quiet stop marker, and Elisa answers from the results she kept (`turn::answer_from_tools`)
                if step == 0 && self.message_alone_too_big() {
                    self.history.pop();
                    let n = grouped(self.max_ctx);
                    let line = if crate::reply_lang::detect(&ask) == Some(crate::reply_lang::Lang::Vi) {
                        format!("Tin nhắn này quá dài cho {} (mỗi lần chỉ đọc được khoảng {n} token); bạn gửi tin ngắn hơn nhé.", self.cfg.model)
                    } else { format!("This message is too long for {} (it reads about {n} tokens at a time); please send a shorter one.", self.cfg.model) };
                    self.fe.info(&line);
                } else {
                    self.history.push(Message::assistant_text("[stopped: context full]"));
                    self.debug("[the results or the history outgrew the window, not the user's message; Elisa answers from the results]");
                }
                return TurnEnd::Stopped;
            }
            CANCEL.store(false, Ordering::SeqCst);
            let max_tokens = self.cfg.max_output.min(self.max_ctx.saturating_sub(self.est_safe() + context::ctx_margin(self.max_ctx) + 64)).max(MIN_OUT);
            // a simple turn where the model does not reason: a smaller cap (`Client::simple_cap`)
            let capped = self.client.simple_cap(self.max_ctx);
            let max_tokens = capped.map_or(max_tokens, |c| max_tokens.min(c));
            let interactive = self.interactive;
            let deps_note = self.deps_model_note();
            let now_line = crate::situation::model_now(&self.cfg.model, self.client.caps().vision == Some(true));
            self.fe.waiting();
            self.acts.model_wait_on(&mut *self.fe, crate::provider::is_local(&self.cfg.base_url), self.client.caps().always_loaded());
            let fe = &mut *self.fe;
            let acts = &mut self.acts;
            // what the user saw stream in (a loop cut after the send is shown again without its copies)
            let mut streamed = String::new();
            // (a model's reasoning written as text, "<think>…</think>" or a bare "</think>" before the answer, never
            // reaches the screen: `reply_check::ReasoningFilter` holds it back and takes back what a late close tag
            // turns out to have been reasoning)
            let mut reasoning = crate::reply_check::ReasoningFilter::default();
            let shown = &mut reasoning;
            let mut on_text = |t: &str| if interactive {
                let (t, retract) = shown.feed(t);
                if retract { fe.reply_retracted(); streamed.clear(); }
                if t.is_empty() { return; }
                if !t.trim().is_empty() { acts.model_text(&mut *fe); }
                streamed.push_str(&t);
                fe.text_delta(&t);
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
                Some(n) => { retry_msgs = with_note(base, &n); &retry_msgs }
                None if in_words => { retry_msgs = with_note(base, if final_ask.is_some() { FINAL_ANSWER_NOTE } else { self.retry_note.as_deref().unwrap_or(SMALL_TALK_NOTE) }); &retry_msgs }
                // (a turn whose dependency check ran: Elisa's verified list, and to add only a short note on what to do)
                None => match deps_note { Some(n) => { retry_msgs = with_note(base, &n); &retry_msgs } None => base },
            };
            let no_tools: [crate::types::ToolDef; 0] = [];
            // (Elisa's own calls did the asked steps: answers only, `answer_only`)
            let req = Request { system: &self.system, tools: if in_words || self.answer_only { &no_tools } else { &self.tools }, messages, max_tokens };
            self.sent_notes = super::turn::notes_sent(messages);
            self.own_said.borrow_mut().clear();
            self.worded.borrow_mut().clear();
            self.client.set_kind("agent");
            let res = self.client.send(&req, &CANCEL, &mut on_text);
            // (a piece held back as a possible tag start that was none, "<": shown now)
            let rest = reasoning.flush();
            if interactive && !rest.is_empty() { streamed.push_str(&rest); self.fe.text_delta(&rest); }
            let mut reply = match res {
                Ok(r) => r,
                Err(e) => {
                    // (words streamed in before the failure that make no finished answer — a run of repeats the server
                    // aborted, ": : : :" — are taken back: the failure line alone stands, `reply_check::unfinished`)
                    if !streamed.trim().is_empty() && crate::reply_check::unfinished(&streamed) { self.fe.reply_retracted(); }
                    if first { self.history.pop(); self.unsent = true; } else { self.history.push(Message::assistant_text("[request failed]")); }
                    return TurnEnd::Failed(self.failed_line(&e));
                }
            };
            first = false;
            // a reply cut by the output limit is continued from this same request (`continue_reply`), and so is a
            // finished one whose last sentence stopped after its first words (`reply_check::ends_mid_sentence`), on a turn
            // that ran no tools (a task's report may end on a name or a label)
            let mid = reply.stop_reason == "end_turn" && self.tool_lines.is_empty() && !reply.content.iter().any(|b| matches!(b, Block::ToolUse { .. }))
                && crate::reply_check::ends_mid_sentence(context::split_state(&texts(&Message::new(Role::Assistant, reply.content.clone()))).0.trim());
            let sent: Option<Vec<Message>> = (reply.stop_reason == "max_tokens" || mid).then(|| messages.to_vec());
            // what a reply to this request must not be (`context::ReplyGuard`)
            let reply_guard = context::ReplyGuard::new(&self.input, messages);
            // sentences copied from Elisa's own system prompt (`reply_check::Echo`)
            let echo = crate::reply_check::Echo::new(&context::own_prompt(&self.system, self.project.as_deref()),
                &messages.iter().rev().find(|m| m.role == Role::User).map(texts).unwrap_or_default()).with_facts(self.reply_facts())
                .with_header(&messages.iter().find(|m| m.role == Role::User).map(texts).unwrap_or_default())
                .with_notes(&messages.iter().rev().find(|m| m.role == Role::User).map(|m| m.content.iter().map(|b| match b {
                    Block::Text { text } => text.clone(), Block::ToolResult { content, .. } => content.clone(), _ => String::new() }).collect::<Vec<_>>().join("\n")).unwrap_or_default());
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
                let line = self.said_own(small_talk_fallback(&self.input, self.first_turn()));
                if self.interactive { self.fe.text_delta(&line); }
                self.history.push(Message { role: Role::Assistant, content: vec![Block::Text { text: line }], id, model: None });
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
                    // (garbled text is not continued: more of it would only take longer, `reply_check::garbled`; nor
                    // is a reply that cannot stand, such as a copy of the user's earlier words, `ReplyGuard::broken`)
                    let so_far = context::split_state(&raw).0;
                    // (nor a reply that already repeats itself: `reply_check::repetitive`)
                    if !so_far.trim().is_empty() && !crate::reply_check::garbled(&so_far, &ask) && !crate::reply_check::repetitive(&so_far) && !crate::reply_check::loops_so_far(&so_far) && reply_guard.broken(&so_far).is_none() {
                        let before = (raw.len(), stop_reason.clone());
                        (raw, stop_reason) = self.continue_reply(sent, raw, max_tokens, in_words, st);
                        // an unfinished sentence that got nothing more keeps its own end
                        if mid && raw.len() == before.0 { stop_reason = before.1; }
                        continued = true;
                    } else { self.fe.reply_done(&st); }
                }
                let (vis, card, broken) = context::split_state(&raw);
                // (whether it came as a loop, judged before its copies are cut: `reply_check::looped`)
                let looped = crate::reply_check::looped(&vis);
                // (and whether it ran into a loop at all: the one copy kept of one must be a whole sentence)
                let loop_tail = crate::reply_check::loop_end(&vis).is_some();
                // one of Elisa's request-only notes repeated as the reply: never shown (`route::strip_echoed_notes`)
                // picture text copied into the reply too (`context::strip_picture_echo`)
                // and a line that only restates the user's message (`route::strip_echoed_ask`)
                // and sentences copied from Elisa's system prompt (`reply_check::Echo`)
                // and one pair of tags wrapped round the whole reply (`reply_check::unwrapped`)
                // and a few letters of a script neither the user's nor the reply's (`reply_check::without_stray_script`)
                // and a call written as text that never ran (`reply_check::without_fake_calls`)
                // and a sentence's second copy (`reply_check::without_repeated_copies`)
                // and the sentence marks the server hid put back (`with_hidden_marks`)
                let clean = echo.strip(&crate::reply_check::strip_echoed_ask(&crate::route::strip_echoed_notes(&context::strip_picture_echo(&crate::reply_check::without_stray_script(&crate::reply_check::without_repeated_copies(&crate::reply_check::without_fake_calls(&self.with_hidden_marks(&crate::reply_check::without_wrappers(&crate::reply_check::unwrapped(&crate::reply_check::without_stray_ends(&vis)), &self.input)))), &self.input))), &self.input));
                // (a lone "yes" or "no" beside a line Elisa put first, her clock line "Bây giờ là 05:41.": her line alone)
                // (and the same when the model's own words beside it are garbled or in another language)
                let clean = crate::reply_check::beside_own_line(&clean, &vis, &self.input);
                let vis = if clean != vis.trim() && clean != vis {
                    self.fe.reply_retracted();
                    if self.interactive && !clean.is_empty() { self.fe.text_delta(&clean); }
                    self.debug(&format!("[the reply was cleaned before it was kept: copied lines, wrapper tags or a fact Elisa holds] {}", shown_quote(&vis)));
                    clean
                } else if crate::reply_check::loop_end(&streamed).is_some() {
                    // the stream showed the loop's copies; the reply keeps one (`reply_check::loop_end`)
                    self.fe.reply_retracted();
                    if self.interactive && !vis.trim().is_empty() { self.fe.text_delta(&vis); }
                    self.debug("[the reply wrote the same words over and over; it was stopped and kept with one copy]");
                    vis
                } else { vis };
                // a loop that came in a continued part (not stopped while streaming): kept with one copy
                // (and a numbered question of its own with its answer, "4. What is…? Answer: …": cut there)
                let vis = match crate::reply_check::loop_end(&vis).filter(|e| *e < vis.trim_end().len()).map(|e| (e, "the same words over and over; it was kept with one copy"))
                    .or_else(|| crate::reply_check::invented_question(&vis, &ask).map(|e| (e, "a question and answer of its own; it was cut there"))) {
                    Some((e, why)) => {
                        let t = vis[..e].trim_end().to_string();
                        self.fe.reply_retracted();
                        if self.interactive && !t.is_empty() { self.fe.text_delta(&t); }
                        self.debug(&format!("[the reply went on with {why}]"));
                        t
                    }
                    None => vis,
                };
                // a reply opening with an earlier reply to another question, word for word, before its own words
                let vis = match reply_guard.without_copied_start(&vis) {
                    Some(t) => {
                        self.fe.reply_retracted();
                        if self.interactive { self.fe.text_delta(&t); }
                        self.debug("[the reply opened with an earlier reply to another question; that part was removed]");
                        t
                    }
                    None => vis,
                };
                let card = card.or_else(|| stashed_card.take()).and_then(|c| context::drop_copied(&c, &context::own_prompt(&self.system, self.project.as_deref())));
                (self.new_card, self.card_broken) = (card, broken);
                // the reply showed words (not only the "[max_tokens]" placeholder below)
                let wrote = !vis.trim().is_empty();
                // nothing for the user to read (after the no-think prefill a small model can answer "thanks" with
                // only a state card): ask once more, any provider, then say so rather than show nothing
                let normal_end = cut.is_none() && stop_reason != "cancelled" && stop_reason != "max_tokens";
                if !wrote && normal_end && !retried {
                    retried = true;
                    stashed_card = self.new_card.take();
                    empty_retry = Some(EMPTY_REPLY_NUDGE.into());
                    // (what the model wrote before nothing was left of it, quoted: a state card, a copied line)
                    self.debug(&format!("{EMPTY_REPLY_LINE} {}", shown_quote(&streamed)));
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
                    empty_retry = Some(REASONED_OUT_NUDGE.into());
                    self.fe.info("[the model spent its whole budget reasoning and wrote no answer; asking once more without reasoning]");
                    continue;
                }
                // a reply in another language than the user's message (`reply_check::lang_mismatch`): asked once
                // more with a note saying which, in that request only (after tools ran too: the request keeps their
                // results, so the answer is the same one in the user's language)
                // (or in a script the user never wrote in this conversation: `lang_off`)
                // (never a call written as text: its words are the call's, not a language; the text-call correction
                // below asks for it properly, so the one language retry is not spent on it)
                if wrote && normal_end && !lang_retried && !context::looks_like_text_tool_call(&vis) && self.lang_off(&ask, &vis) {
                    lang_retried = true;
                    lang_first = Some(vis.clone());
                    stashed_card = self.new_card.take().or(stashed_card);
                    self.fe.reply_retracted();
                    empty_retry = Some(crate::reply_check::lang_note(&ask));
                    self.debug(&format!("[the reply was not in the language of the user's message; asking once more with a note] {}", shown_quote(&vis)));
                    continue;
                }
                // a reply that repeats the user's words of an earlier turn (`ReplyGuard::copies_said`): asked once more
                // with the whole conversation and a note to answer this message, before the words alone (which lose
                // what the conversation told, such as the user's name)
                if wrote && cut.is_none() && stop_reason != "cancelled" && !copy_retried && self.tool_lines.is_empty() && reply_guard.copies_said(&vis) {
                    copy_retried = true;
                    stashed_card = self.new_card.take().or(stashed_card);
                    self.fe.reply_retracted();
                    empty_retry = Some(REASONED_OUT_NUDGE.into());
                    self.debug(&format!("[the reply repeated the user's earlier words; asking once more with a note] {}", shown_quote(&vis)));
                    continue;
                }
                // a reply asking for a short follow-up to be put another way ("why?" after a joke → "Could you please
                // rephrase it or provide more context?"): asked once more with the follow-up spelt out from the last
                // exchange (`reply_check::asks_rephrase`, `follow_up_note`); asked back again, that reply stands
                if wrote && normal_end && !follow_retried && self.tool_lines.is_empty() && crate::reply_check::short_follow_up(&ask) && crate::reply_check::asks_rephrase(&vis) {
                    if let Some(note) = self.follow_up_note(&ask) {
                        follow_retried = true;
                        stashed_card = self.new_card.take().or(stashed_card);
                        self.fe.reply_retracted();
                        empty_retry = Some(note.into());
                        self.debug("[the reply asked for the short follow-up to be put another way; asking once more with it spelt out from the last exchange]");
                        continue;
                    }
                }
                // a tool turn's reply that ignores what the tools returned (`given_fault`): asked once more with a note
                // in the last result; still so, Elisa answers from the results (below)
                let given_fault = if wrote && normal_end { self.given_fault(&vis, &ask, &reply_guard) } else { None };
                if let (Some(w), false) = (given_fault, given_retried) {
                    given_retried = true;
                    stashed_card = self.new_card.take().or(stashed_card);
                    self.fe.reply_retracted();
                    // (on a turn that ran no tool: which earlier result, so the model finds it in the conversation)
                    empty_retry = Some(match self.given(&ask).first().and_then(|(p, _)| p.clone()).filter(|_| self.tool_lines.is_empty()) {
                        Some(p) => format!("(What reading {p} returned earlier in this conversation holds what this message asks for: answer it \
                            from that, quoting what it says. Never say you do not have it, and never repeat an earlier reply.)").into(),
                        None => GIVEN_NOTE.into(),
                    });
                    self.debug(&format!("[the reply {w}; asking once more with a note that the results hold the answer] {}", shown_quote(&vis)));
                    continue;
                }
                // a reply that copies an earlier one, has no words or repeats Elisa's headings (`context::ReplyGuard`),
                // or still none after the nudge: the user's words asked once more alone (`isolated_reply`)
                // (and on a turn with picture text, a reply with nothing of what the picture's answer to this ask said)
                // (and a reply still in another language after the language note: asked alone with the note, where the
                // history's earlier replies in another language do not pull a small model back to it)
                let off_pic = |t: &str| self.turn_pic.as_deref().is_some_and(|p| context::off_picture(t, p));
                // (and a lone "yes" or "no" to a greeting or to a message that asks no question, which answers nothing)
                // (and a reply with no letter to a greeting or thanks, "4" to "thanks!", whatever numbers the message holds)
                // (but digits that answer by a fact Elisa holds stand: "00:20" to "and in Tokyo?" on a time turn, a
                // number on a computed one — `Echo::answers_in_digits`; the facts check above has made them hers)
                let small = self.small_talk_turn || crate::route::small_talk(&ask) || crate::route::opens_with_greeting(&ask);
                let greeting = small || !ask.trim_end().ends_with('?');
                let digits_answer = echo.answers_in_digits(&vis);
                let why = if !wrote { Some("empty") }
                    // (nor a line Elisa wrote herself in place of the reply: never asked again)
                    else if context::looks_like_text_tool_call(&vis) || self.is_own_words(&vis) { None }
                    else if small && self.tool_lines.is_empty() && !vis.chars().any(char::is_alphabetic) && !digits_answer { Some("no words") }
                    else { reply_guard.broken(&vis).filter(|w| *w != "no words" || !digits_answer).or_else(|| crate::reply_check::garbled(&vis, &ask).then_some("garbled"))
                        // (a reply saying it cannot see images on a turn whose picture was looked at already: its content is
                        // in the turn as text, so this is no answer; before the picture check, which words shared with that
                        // text would pass)
                        .or_else(|| (self.picture_turn() && crate::reply_check::denies_sight(&vis)).then_some("denied sight"))
                        .or_else(|| off_pic(&vis).then_some("off the picture"))
                        .or_else(|| (greeting && self.tool_lines.is_empty() && crate::reply_check::lone_yes_no(&vis)).then_some("no answer"))
                        .or_else(|| reply_guard.copies_last(&vis).then_some("copy"))
                        .or_else(|| crate::reply_check::copies_ask(&vis, &ask).then_some("copy"))
                        // (a shell session written out, prompt and command, on a turn that ran nothing: it reads as if
                        // the command ran — `reply_check::fakes_shell_run`)
                        .or_else(|| (self.tool_lines.is_empty() && crate::reply_check::fakes_shell_run(&vis)).then_some("faked run"))
                        // (a reply to a check-in that answers the message before it: "…bạn's favorite color")
                        .or_else(|| self.history[..self.turn_cap(&self.history).min(self.history.len())].iter().rev()
                            .find(|m| m.role == Role::User && !m.content.iter().any(|b| matches!(b, Block::ToolResult { .. })))
                            .is_some_and(|m| self.tool_lines.is_empty() && crate::reply_check::about_last_ask(&vis, &ask, &texts(m))).then_some("copy"))
                        .or_else(|| crate::reply_check::word_cycle(&vis).then_some("no words"))
                        // (a reply that repeats itself or shuffles the user's words: `reply_check::shapeless`)
                        .or_else(|| crate::reply_check::shapeless(&vis, &ask))
                        // (the one copy kept of a loop, to plain small talk that Elisa's own line answers in full:
                        // `reply_check::plain_talk`)
                        .or_else(|| (looped && self.tool_lines.is_empty() && crate::reply_check::plain_talk(&ask)).then_some("no words"))
                        // (an earlier message of the user's said back, or only a piece of this one: `reply_check::echoes_said`,
                        // `reply_check::fragment_of_ask`)
                        // (never to plain small talk: "Hi there." to "hi there" greets back)
                        .or_else(|| (self.tool_lines.is_empty() && !crate::reply_check::plain_talk(&ask) && (crate::reply_check::echoes_said(&vis, &self.user_messages()) || crate::reply_check::fragment_of_ask(&vis, &ask))).then_some("copy"))
                        // (the one copy kept of a loop that is no whole sentence, "Bây giờ là ú": `reply_check::whole_sentence`)
                        .or_else(|| (loop_tail && !crate::reply_check::whole_sentence(&vis)).then_some("no words"))
                        // (nothing but a pitch to a question: `reply_check::pitch_only`)
                        .or_else(|| (self.tool_lines.is_empty() && crate::reply_check::pitch_only(&vis, &ask, &crate::reply_check::own_name(&self.system).into_iter().collect::<Vec<_>>())).then_some("no answer"))
                        .or_else(|| self.tool_lines.is_empty().then(|| self.answers_nothing(&vis, &ask)).flatten())
                        // (an ask for a table answered with none: asked once more with a note on the format)
                        .or_else(|| (self.tool_lines.is_empty() && crate::reply_check::missing_table(&vis, &ask)).then_some("no table"))
                        .or_else(|| (lang_retried && self.lang_off(&ask, &vis)).then_some("another language")) };
                // the language retry came back no better: the first reply stands (never a worse one in its place)
                // (kept beyond this: a turn that ends with no answer gets it back, `answer_over_none`)
                let (vis, why) = match (why, lang_first.clone()) {
                    // (garbled after a first reply in another language: both unusable; a greeting gets Elisa's own
                    // greeting in the user's language, never the garbled text)
                    // (so too a first reply with another language's words in it, "Halo! … Bagaimana có thể giúp bạn…",
                    // and a retry no better)
                    (Some(w @ ("garbled" | "another language")), Some(first)) if crate::reply_check::lang_mismatch(&ask, &first)
                        && (w == "garbled" || crate::reply_check::other_language_words(&first, &ask))
                        && (self.small_talk_turn || crate::route::small_talk(&ask)) => {
                        let line = self.said_own(small_talk_fallback(&ask, self.first_turn()));
                        self.fe.reply_retracted();
                        if self.interactive { self.fe.text_delta(&line); }
                        self.debug("[the reply was in another language and the retry no better; Elisa greeted back]");
                        stop_reason = "end_turn".into();
                        (line, None)
                    }
                    // (garbled is worse than any first reply; another language again is worse than a first reply in the
                    // user's own language, `route::vietnamese`)
                    // (never a first reply with another language's words in it)
                    (Some(w @ ("another language" | "garbled")), Some(first)) if !crate::reply_check::garbled(&first, &ask)
                        && !crate::reply_check::other_language_words(&first, &ask)
                        && (w == "garbled" || (crate::route::vietnamese(&ask) && crate::route::vietnamese(&first))) => {
                        self.fe.reply_retracted();
                        if self.interactive { self.fe.text_delta(&first); }
                        self.debug("[the language retry came back no better; the first reply stands]");
                        (first, None)
                    }
                    (w, _) => (vis, w),
                };
                // a count of the pictures this message carries ("how many pictures did I send?"): Elisa knows it
                // (the pictures this message carries, whether or not looking at them told anything)
                let pics = if self.pics_next { self.last_pics.len() } else { 0 };
                let (vis, why) = match crate::reply_check::picture_count_line(&ask, &vis, pics, self.turn_pic.as_deref().unwrap_or("")) {
                    Some(line) if wrote || why.is_some() => {
                        if wrote { self.fe.reply_retracted(); }
                        if self.interactive { self.fe.text_delta(&line); }
                        self.debug("[a count of the pictures sent; Elisa gave it]");
                        stop_reason = "end_turn".into();
                        (line, None)
                    }
                    _ => (vis, why),
                };
                let note = match why {
                    Some("another language") => Some(crate::reply_check::lang_note(&ask)),
                    Some("denied sight") => Some(SIGHT_NOTE.into()),
                    // (any unusable reply to an ask for a table: the note on the format)
                    Some(_) if crate::reply_check::asks_table(&ask) => Some(crate::reply_check::table_note(&ask).into()),
                    // (unusable for any other reason, when the message's language is plain: the note goes with the
                    // words asked alone, so a small model asked once more does not drift into English — granite3.2-
                    // vision:2b's "I can help you with that…" to "bạn có đọc được file trên máy mình không?")
                    Some(_) if lang_retried || crate::reply_lang::detect(&ask).is_some() => Some(crate::reply_check::lang_note(&ask)),
                    _ => None,
                };
                // (whether the words asked alone replaced the reply: the replacement gets the same checks a reply that
                // ended normally gets, `checked_reply`, whatever stop ended the first one)
                let mut replaced = false;
                // (a copy may run to the output limit: moondream:1.8b's 1,024-token "I hope this email finds you well…")
                let vis = match why.filter(|_| self.tool_lines.is_empty()).filter(|_| if wrote { cut.is_none() && stop_reason != "cancelled" } else { normal_end }) {
                    Some(why) => match self.isolated_reply(max_tokens, &reply_guard, note.as_deref()) {
                        Some(t) => {
                            self.debug(&format!("[the reply was unusable ({why}); asked once more with this turn's message alone] {}", shown_quote(&vis)));
                            if wrote { self.fe.reply_retracted(); }
                            if self.interactive { self.fe.text_delta(&t); }
                            stop_reason = "end_turn".into();
                            replaced = true;
                            t
                        }
                        // nothing usable alone either: a turn with a picture has the model's own words about it
                        // (a reply that was only the picture text is removed as an echo, and asked alone a model may
                        // answer with it again)
                        // (never a picture answer that is itself a copy of an earlier reply)
                        None => match self.turn_pic.clone().filter(|p| reply_guard.broken(p) != Some("copy")) {
                            Some(p) => {
                                self.debug(&format!("[the reply was unusable ({why}); showed the picture's own answer]"));
                                if wrote { self.fe.reply_retracted(); }
                                if self.interactive { self.fe.text_delta(&p); }
                                stop_reason = "end_turn".into();
                                // (the picture's own answer is the turn's verified text about it, shown as it is)
                                self.said_own(p)
                            }
                            // sight denied twice while the look at the picture told nothing of it: Elisa says so
                            // (when it did tell, the picture's own answer stood above)
                            None if why == "denied sight" => {
                                let line = crate::reply_check::picture_unseen_line(&ask, self.client.model());
                                self.fe.reply_retracted();
                                if self.interactive { self.fe.text_delta(&line); }
                                self.debug("[the reply said it could not see the picture twice and the look told nothing; Elisa said so]");
                                stop_reason = "end_turn".into();
                                line
                            }
                            // garbled twice: its garbled words and runs taken out (`reply_check::repair`); nothing
                            // left (made-up words alone, "'ýàn hýa'"), a plain line of Elisa's own
                            None if why == "garbled" => {
                                let fixed = crate::reply_check::repair(&vis, &ask);
                                // (pieces of no language asked, twice: Elisa's own line, never the words round them)
                                let fixed = if fixed.chars().any(char::is_alphabetic) && !crate::reply_check::foreign_parts(&vis, &ask) { fixed } else { self.own_line(&ask) };
                                self.fe.reply_retracted();
                                if self.interactive { self.fe.text_delta(&fixed); }
                                self.debug(&format!("[the reply was garbled twice; its garbled parts were taken out] {}", shown_quote(&vis)));
                                // (not "continue" after a cut: more would be garbled too)
                                stop_reason = "end_turn".into();
                                fixed
                            }
                            // a word or two to a question about a document, twice: the sentence of its text that
                            // answers the question best (`reply_check::best_sentence`), else Elisa's plain line
                            None if wrote && why == "no answer" && self.doc_req.is_some() => {
                                let line = self.said_own(crate::reply_check::best_sentence(&ask, &self.doc_text_held()).unwrap_or_else(|| self.own_line(&ask)));
                                self.fe.reply_retracted();
                                if self.interactive { self.fe.text_delta(&line); }
                                self.debug("[the reply to a question about a document answered nothing twice; Elisa answered with its text]");
                                stop_reason = "end_turn".into();
                                line
                            }
                            // a shell session written out, prompt and command, where no command ran, twice: Elisa's
                            // line that nothing ran (the `not_run` line of `data/no_answer_lines.txt`), never the
                            // session
                            None if why == "faked run" => {
                                let line = self.said_own(crate::reply_check::own_line_of("not_run", &ask, ""));
                                if wrote { self.fe.reply_retracted(); }
                                if self.interactive { self.fe.text_delta(&line); }
                                self.debug(&format!("[the reply wrote out a shell session where no command ran, twice; Elisa said none ran] {}", shown_quote(&vis)));
                                stop_reason = "end_turn".into();
                                line
                            }
                            // a copy of an earlier reply, no words or none at all, no answer to a greeting, another
                            // language, a claim to be human or words about the message's words, twice: a plain line
                            // of Elisa's own, never the copy and never an empty reply
                            None if !wrote || matches!(why, "copy" | "no words" | "no answer" | "another language" | "no table" | "claims human" | "about the words") => {
                                if wrote { self.fe.reply_retracted(); }
                                let line = self.own_line(&ask);
                                if self.interactive { self.fe.text_delta(&line); }
                                self.debug(&format!("[the reply was unusable ({why}) twice; showed a plain line] {}", shown_quote(&vis)));
                                stop_reason = "end_turn".into();
                                line
                            }
                            None => vis,
                        },
                    },
                    // (after tools ran, Elisa answers from their results: no line about it)
                    None if !wrote && normal_end && self.tool_lines.is_empty() => {
                        let line = self.own_line(&ask);
                        if self.interactive { self.fe.text_delta(&line); }
                        self.debug("[the model wrote no text twice; showed a plain line]");
                        stop_reason = "end_turn".into();
                        line
                    }
                    None => vis,
                };
                // checked against what the turn gave: still ignoring the results after the note, code quoted from a
                // file that the file does not hold, a claim about a file never read (`checked_reply`)
                // (a reply cut at the output limit is checked against a read said no to all the same)
                // (not a call written as text: the text-call handling below decides what stands in its place, the one
                // correction, then once in words, then a plain line, so no check puts a line of its own there first and
                // the call still runs)
                // then the one gate every text about to be shown passes, whatever stage made it: the content checks
                // (`content_fixes`) and the gate's own (`gate.rs` `gated`: a dangling lead-in, a false "can't", the
                // language, garble, repetition), with Elisa's answer from the results when the text cannot stand
                let written_call = !has_tools && stop_reason != "cancelled" && context::looks_like_text_tool_call(&vis);
                let vis = if written_call { vis }
                    else if (wrote && normal_end) || replaced { let v = self.checked_reply(vis, &ask, given_fault); self.gated(v, &ask, true, false) }
                    else if wrote && stop_reason != "cancelled" { self.denied_reply(vis, &ask) } else { vis };
                // a turn ending with no answer after a reply whose only fault was its language: that answer, in the
                // user's language when the model can say it so, else as it came (`answer_over_none`)
                let vis = self.answer_over_none(vis, &ask, lang_first.take());
                // a reply cut inside its last word ("Insecure Temp File Re"): the word completed from this turn's
                // results; at the output limit and not found there, the broken end goes
                let vis = if wrote && stop_reason != "cancelled" { self.cut_end_fixed(vis, stop_reason == "max_tokens") } else { vis };
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
                let is_call = !has_tools && stop_reason != "cancelled" && context::looks_like_text_tool_call(&texts(&Message::new(Role::Assistant, content.clone())));
                // a call written as text is never the reply: on a turn whose answer Elisa computed, the answer is hers
                // (`reply_check::computed_answer`); after the nudge, asked once in words with no tools, then a plain line
                if is_call && (nudged || crate::reply_check::computed_answer(&ask).is_some()) {
                    let line = match crate::reply_check::computed_answer(&ask) {
                        Some(l) => l,
                        None if !words_retried => {
                            (words_retry, words_retried) = (true, true);
                            self.fe.reply_retracted();
                            self.debug("[the reply was a call written as text again; asking once more in words, with no tools]");
                            continue;
                        }
                        None => self.no_answer_line(&ask),
                    };
                    self.fe.reply_retracted();
                    if self.interactive { self.fe.text_delta(&line); }
                    self.debug("[the reply was a call written as text; Elisa answered in its place]");
                    content = vec![Block::Text { text: line }];
                }
                let text_call = is_call && !nudged && !matches!(content.first(), Some(Block::Text { text }) if !context::looks_like_text_tool_call(text));
                if text_call {
                    nudged = true;
                    (self.new_card, self.card_broken) = (None, false);
                    // (the call's text is no answer: off the screen now; the history keeps it, append-only within the
                    // turn, so each request only grows)
                    self.fe.reply_retracted();
                    self.history.push(Message { role: Role::Assistant, content, id, model: None });
                    // routed: the call written as text may be for a tool this turn left out: offer every group
                    // (but the web on a turn it is kept out of, the user's files or a plain statement: `route::web_kept_out`)
                    let all = crate::route::ALL & !crate::route::OTHER;
                    let all = if crate::route::web_kept_out(&self.route, context::request_body(&self.input)) { all & !crate::route::WEB } else { all };
                    if self.cfg.route == "auto" && self.route.groups.0 & all != all {
                        // (a read-only turn stays so: `route::READ_ONLY`)
                        self.route.groups = crate::route::Groups(all | (self.route.groups.0 & crate::route::READ_ONLY));
                        self.route.why = crate::route::Why::Widened;
                        self.rebuild_system();
                    }
                    let names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
                    self.history.push(Message::user_text(context::tool_text_nudge(&names)));
                    self.real = Some((self.history.len() - 1, ctx_used));
                    self.debug("[the reply wrote a tool call as text, so nothing ran; asking the model once to call it properly]");
                    continue;
                }
                self.history.push(Message { role: Role::Assistant, content, id: id.clone(), model: None });
                // the gateway audits every reply: its audit line shows once it finishes
                if let Some(i) = &id { self.watch.mine(i); }
                // a continued reply is longer than the server's count of the first part
                self.real = (!continued).then_some((self.history.len(), ctx_used));
                return match stop_reason.as_str() {
                    "cancelled" => { self.fe.info(STOPPED_LINE); TurnEnd::Stopped }
                    "max_tokens" => {
                        match &cut {
                            Some(c) => self.fe.info(&format!("[stopped: `{c}` was cut off at the {max_tokens}-token output limit, so it did not run and nothing was saved; say \"continue\" to go on]")),
                            None if !wrote && reasoning > 0 => self.fe.info(&format!("[stopped: the model spent its whole output budget ({reasoning} tokens) reasoning and wrote no answer; ask again, or more simply]")),
                            None if !wrote => self.fe.info(&format!("[stopped: the reply used its whole {max_tokens}-token output limit and wrote no text for you; ask again, or more simply]")),
                            None if continued => self.fe.info("[reply was long; say \"continue\" for more]"),
                            // (not continued because it was going round in circles: the one copy is all there is)
                            None if looped => self.fe.info("[the reply went on repeating itself; it was cut to one copy]"),
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
                    // a web tool on a turn the web is kept out of (the user's files, a plain statement:
                    // `route::web_call_kept_out`): not widened to; refused below with the wrong-tool note
                    // (and a tool that changes files on an ask that only asks or checks, a file or shell tool on a time
                    // ask: `route::call_kept_out`)
                    let kept_out = crate::route::call_kept_out(name, self.mcp.server_of(name), &self.route, context::request_body(&self.input));
                    // (a change the user did not ask for, offered or not: on a list or read ask, `unasked_change`)
                    let unasked = self.unasked_change(name, self.mcp.server_of(name));
                    if !offered && plugin_gone.is_none() && kept_out.is_none() && self.widen_for(name) { offered = true; }
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
                    } else if let Some(why) = unasked {
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "unasked_change")]);
                        self.fe.tool_done("not run: this tool does not apply to this message", true);
                        (why, true)
                    } else if let Some(why) = kept_out.filter(|_| !offered) {
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "kept_out")]);
                        self.fe.tool_done("not run: this tool does not apply to this message", true);
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
                        // (the note is the model's: the user's row says only that it was skipped)
                        self.fe.tool_done("not run: already done in this turn, with the same result", true);
                        (why, true)
                    } else if let Some(why) = start_err {
                        self.audit_tool("tool_call", name, &summary, &[("outcome", "error"), ("detail", &why)]);
                        self.fe.tool_done(&why, true);
                        (why, true)
                    } else if let Some(why) = (!self.mcp.owns(name)).then(|| super::turn::wrong_tool_for_shell(name, context::request_body(&self.input))).flatten() {
                        // a file write on an ask for the shell ("use the shell to write hello into …"): the shell does it
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "shell_ask")]);
                        self.fe.tool_done("not run: this was asked of the shell", true);
                        (why, true)
                    } else if let Some(why) = crate::route::word_as_command(name, input, context::request_body(&self.input)) {
                        // a bash command that is one bare word of the user's sentence ("starving"): no command, not run
                        self.audit_tool("denial", name, &summary, &[("outcome", "denied"), ("answer", "policy"), ("reason", "word_as_command")]);
                        self.fe.tool_done("not run: that is a word of your message, not a command", true);
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
                                let out = self.guard_output(name, input, o.content);
                                // a read's counts the user's words ask about, for the model (`with_count_note`)
                                (if o.is_error { out } else { self.with_count_note(name, input, out) }, o.is_error)
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
                    self.debug(&format!("[loop guard: {}]", n.trim_start_matches("[elisa: ").trim_end_matches(']')));
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
                self.debug(&format!("[{who}: {why}; asking the model to answer with what it has, no tools]"));
                if who == "loop guard" { self.audit.note("loop_stop", &[("reason", &why), ("then", "answer")]); }
                (final_ask, words_retry) = (Some(why), true);
                continue;
            }
            if at_limit && stop.is_none() { break; }
            if let Some(why) = stop {
                self.audit.note("loop_stop", &[("reason", &why), ("then", "stopped")]);
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
        self.fe.info(STOPPED_LINE);
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
                self.fe.info(STOPPED_LINE);
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

/// The host and port of a server address ("http://user:key@localhost:11434/v1" → "localhost:11434"): what the user
/// knows the server by, without a path or credentials.
fn server_address(base: &str) -> &str {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let rest = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    rest.rsplit_once('@').map_or(rest, |(_, h)| h)
}

impl<'f> Session<'f> {
    /// A failed model call: the full error goes to the audit log (and the debug lines), the user gets the plain line
    /// (`api_error_line`).
    pub(super) fn failed_line(&mut self, e: &str) -> String {
        let detail = context::one_line(e, 500);
        self.audit.note("model_request", &[("outcome", "error"), ("detail", &detail)]);
        self.debug(&format!("[model request failed: {detail}]"));
        api_error_line(e, &self.cfg.base_url)
    }
}

/// A failed model call as one line for the user: what happened and what to do (the server's words, cut short).
/// No answer from the server at all (it is not running, or the address is wrong): said plainly with its address,
/// without the system's error text (`failed_line` keeps that).
pub(super) fn api_error_line(e: &str, base: &str) -> String {
    if e.starts_with("request failed:") {
        let at = server_address(base);
        let at = if at.is_empty() { String::new() } else { format!(" at {at}") };
        return format!("the model server{at} isn't answering; check that it's running, then ask again");
    }
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
/// the note a picture turn is asked once more with when the reply said it could not see images (the picture was
/// looked at already: its content is the turn's `[picture: …]` text)
const SIGHT_NOTE: &str = "(The picture was already looked at: what it shows is the [picture: …] text above. Answer the question from that text; never say you cannot see images.)";
const EMPTY_REPLY_NUDGE: &str = "(Answer this message in one plain sentence first; a <state> card, if any, goes after it.)";
/// the line the person sees when that retry runs: plain words, none of Elisa's own terms (the hidden card is not
/// something the person knows about)
/// `given_fault`'s reason for a reply that left out what this turn's command printed
const LEFT_OUT_OUTPUT: &str = "left out what the command printed";
/// largest file the code-quote check reads (`checked_reply`)
const CODE_CHECK_MAX_BYTES: usize = 256 * 1024;
/// the note in the last tool result when the reply ignored what the tools returned (`given_fault`)
/// content words an earlier read must share with an ask that names no file before it counts as what the ask is about
const GIVEN_SHARED_WORDS: usize = 2;
const GIVEN_NOTE: &str = "(What the tools returned in this conversation holds what this message asks for: answer it from that, \
    quoting what it says. Never say you do not have it, and never repeat an earlier reply.)";
const EMPTY_REPLY_LINE: &str = "[the reply came back empty; asking the model once more]";

/// The note of the one-time words-only retry of a small-talk turn whose reply called a tool (`agent_loop`): after
/// the user's words, in that request only (it never enters the history).
const SMALL_TALK_NOTE: &str = "(Reply in words; no tools for this message.)";
/// the request-only note of the no-tools request after a loop stop (`agent_loop`), in the last tool result
/// (`with_note`); worded as how to answer, never as a reaction to what the model did
/// A count with thousands commas, as people write it: 2048 -> "2,048".
fn grouped(n: usize) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() { if i > 0 && (d.len() - i).is_multiple_of(3) { out.push(','); } out.push(c); }
    out
}

/// What the user sees when they stop a turn before any reply text (the old "[cancelled]" tag showed raw in the app).
pub(super) const STOPPED_LINE: &str = "Stopped.";

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

impl Session<'_> {
    /// `reply` with the sentence marks the server never shows for this model put back (`reply_check::with_hidden_marks`;
    /// the marks from Ollama's `/api/show`, asked once per model).
    /// Elisa's own line when the model gave no usable answer to `ask`: from the user's facts for a question about
    /// them (`situation::ReplyFacts::facts_answer`), a resume line from the saved conversation for "continue our
    /// last conversation" (`situation::resume_line`), else the plain "couldn't answer that" line.
    /// `vis` with a last word cut short completed from this turn's results (`reply_check::cut_word_completed`), or,
    /// when `cut` (the output limit stopped it) and no result has it, without its unfinished last sentence.
    fn cut_end_fixed(&mut self, vis: String, cut: bool) -> String {
        let given = self.turn_tool_text().0.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        let t = crate::reply_check::cut_word_completed(&vis, &given)
            .or_else(|| if cut { crate::reply_check::without_cut_fragment(&vis) } else { None });
        let Some(t) = t else { return vis };
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&t); }
        self.debug("[the reply was cut inside its last words; they were completed from the results or taken out]");
        t
    }

    /// Whether `ask` asks to go on with the last conversation or for a recap of it (the router's labels, else the
    /// word rule).
    fn resume_ask(&self, ask: &str) -> bool {
        self.routed.as_ref().map_or_else(|| crate::route::resume_intent(ask).is_some(), |a| a.has("resume_last") || a.has("recap"))
    }

    /// Elisa's own answer to a question about one thing she can do, when `reply` does not give it
    /// (`situation::ability_answer`): from every tool she has, routed in this turn or not, and the model's sight.
    /// Verified first: the answer comes from what she holds, whatever the reply said (a reply that says yes "if you
    /// upload the file" to "bạn có đọc được file trên máy mình không?" fits the words but not her tools), so the reply
    /// is not weighed, on any model and under any router label.
    fn ability_line(&self, ask: &str) -> Option<String> { self.ability_line_for(ask, "") }

    /// `ability_line` weighing the model's `reply`: an ability nothing she has gives, and no chat ability ("read
    /// minds"), gets her no only when the reply claims it.
    fn ability_line_for(&self, ask: &str, reply: &str) -> Option<String> {
        let all: Vec<(String, String)> = tools::defs_for(self.cfg.ast).into_iter().chain(self.mcp.defs()).map(|t| (t.name, t.description)).collect();
        crate::situation::ability_answer(ask, reply, &all, self.client.caps().vision == Some(true))
    }

    pub(super) fn no_answer_line(&self, ask: &str) -> String { let t = self.no_answer_line_of(ask); self.said_own(t) }

    /// Elisa's own answer to a question pointing at a page read earlier this session (`held_page_answer`), with the
    /// meaning of a page in another language said in the question's (`page_meaning`).
    /// Elisa's own answer about the page the message points at (`held_page_answer`): on a turn that ran tools, the
    /// page held before the turn (a search or a fetch of another page this turn is not the page meant).
    fn held_page_line(&self, ask: &str) -> Option<String> {
        let cap = if self.tool_lines.is_empty() { self.history.len() } else { self.turn_cap(&self.history).min(self.history.len()) };
        held_page_answer(&self.history[..cap], ask, &|s| self.page_meaning(ask, s))
    }

    /// Whether a question about a page Elisa holds is answered from the page this turn: it names no URL of its own
    /// (one that does wants that page read), on a turn that ran no tool or ran tools the page makes needless (a
    /// search the model ran anyway: the page answers, not its results).
    fn held_page_turn(&self, ask: &str) -> bool {
        self.tool_lines.is_empty() || crate::web::urls_in(context::request_body(ask), 1).is_empty()
    }

    /// A page's sentence (`sentence`) said in the language of the question (`ask`), by one request to the model with
    /// no history and no tools (`said_in`). Kept only when it is in that language, not garbled, has every number of
    /// the sentence (its facts kept), is no copy of the sentence, is short, and — the page in the language the
    /// embedding model reads (`EMBED_LANG`), that model installed — says the sentence: said back in the page's language
    /// by a second request, it is as near the sentence in meaning as a restatement is (`meaning_kept`; "Đây là một
    /// trang web được sử dụng để giảm bảo về các cách thực hiện các tính năng." for "This domain is for use in
    /// documentation examples without needing permission." is not). None otherwise.
    fn page_meaning(&self, ask: &str, sentence: &str) -> Option<String> {
        let t = self.said_in(sentence, context::request_body(ask))?;
        let digits = |x: &str| x.split(|c: char| !c.is_ascii_digit()).filter(|d| !d.is_empty()).map(str::to_string).collect::<Vec<_>>();
        let kept = digits(sentence).iter().all(|d| digits(&t).contains(d));
        if t.chars().count() > PAGE_MEANING_CHARS || !kept || t.contains(sentence)
            || crate::reply_check::lang_mismatch(ask, &t) || crate::reply_check::garbled(&t, ask) { return None; }
        if crate::reply_lang::detect(sentence) == Some(EMBED_LANG) && elisa_mem::embed::model_dir().is_some() {
            let back = self.said_in(&t, sentence)?;
            if !meaning_kept(sentence, &back)? { return None; }
        }
        Some(t)
    }

    /// What `text` means in one plain sentence, in the language of `like`, by one request to the model with no
    /// history and no tools (`PAGE_MEANING_SYSTEM`); without markers or quotes around it. None when cancelled, failed
    /// or empty.
    fn said_in(&self, text: &str, like: &str) -> Option<String> {
        if CANCEL.load(Ordering::SeqCst) { return None; }
        let q = format!("Sentence: \u{201c}{text}\u{201d}\n\nSay what this sentence means in one plain sentence, in the language of this text: {like}\n\
            Add nothing and leave out no fact of it.\n{}", crate::reply_check::lang_note(like));
        let msgs = [Message::user_text(q)];
        let no_tools: [crate::types::ToolDef; 0] = [];
        let req = Request { system: PAGE_MEANING_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: PAGE_MEANING_TOKENS };
        self.client.set_kind("agent");
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
        let t = texts(&Message::new(Role::Assistant, r.content));
        let t = crate::route::strip_stop_markers(context::split_state(&t).0.trim()).trim().trim_matches(['"', '\u{201c}', '\u{201d}']).trim().to_string();
        (!t.is_empty()).then_some(t)
    }

    fn no_answer_line_of(&self, ask: &str) -> String {
        let facts = self.reply_facts();
        if let Some(l) = facts.facts_answer() { return l; }
        // (no usable reply to "are you able to run shell commands?": the answer is Elisa's own, from her tools)
        if self.tool_lines.is_empty() { if let Some(l) = self.ability_line(ask) { return l; } }
        // (a sum she works out herself, "144 chia 12 bằng mấy?": `reply_check::computed_answer`)
        if let Some(l) = crate::reply_check::computed_answer(ask) { return l; }
        // (a question about a page read earlier this session, "what is that page for?": its own first sentence, quoted)
        if self.held_page_turn(ask) { if let Some(l) = self.held_page_line(ask) { return l; } }
        if self.resume_ask(ask) {
            // the user's own earlier messages of the loaded conversation, this ask left out
            let now = self.history.iter().rposition(is_request).unwrap_or(self.history.len());
            let asks: Vec<String> = self.history[..now].iter().filter(|m| is_request(m)).flat_map(|m| &m.content)
                .filter_map(|b| match b { Block::Text { text } if !crate::attach::is_block(text) => Some(context::request_body(text).trim().to_string()), _ => None })
                .filter(|t| !t.is_empty() && !t.starts_with('[')).collect();
            if let Some(l) = facts.resume_answer(&asks) { return l; }
        }
        // (a question an earlier message asked too, its answer in the history or the notes of folded turns: that
        // answer, `earlier_answer`)
        if let Some(l) = self.earlier_answer(ask) { return l; }
        crate::reply_check::cant_answer_line(ask, self.client.model())
    }

    fn with_hidden_marks(&self, reply: &str) -> String {
        static HIDDEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Vec<char>>>> = std::sync::OnceLock::new();
        if self.client.caps().kind != crate::backend::Kind::Ollama { return reply.to_string(); }
        let key = format!("{}|{}", self.client.base(), self.client.model());
        let cache = HIDDEN.get_or_init(Default::default);
        let known = cache.lock().ok().and_then(|c| c.get(&key).cloned());
        let hidden = known.unwrap_or_else(|| {
            let h = crate::backend::ollama_hidden_marks(crate::provider::ollama_root(self.client.base()), self.client.model(), HIDDEN_MARKS_MS);
            if let Ok(mut c) = cache.lock() { c.insert(key, h.clone()); }
            h
        });
        if hidden.is_empty() { return reply.to_string(); }
        let said: String = self.history.iter().filter(|m| m.role == Role::User).map(|m| context::request_body(&texts(m)).to_string()).collect::<Vec<_>>().join("\n");
        // (how many tokens the reply's stream never showed: a mark goes back only where one was)
        // (and what the session's tools returned: an advisory title or a phrase quoted from a result is no sentence start)
        let results = self.tool_text_from(0).0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        crate::reply_check::with_hidden_marks(reply, &hidden, &format!("{said}\n{}\n{results}", self.input), crate::openai::unshown_tokens())
    }

    /// Elisa's own short line when the model gave nothing usable: a greeting back for small talk, else a plain "no
    /// usable answer", in the user's language.
    /// Whether this turn is the chat's first: no earlier message from the user.
    fn first_turn(&self) -> bool {
        !super::gate::past_first_turn(&self.history)
    }

    /// What the user wrote in this conversation, every message of theirs and this turn's (their words only).
    fn user_said(&self) -> String {
        self.history.iter().filter(|m| m.role == Role::User && !m.content.iter().any(|b| matches!(b, Block::ToolResult { .. })))
            .map(|m| context::request_body(&texts(m)).to_string()).chain(std::iter::once(context::request_body(&self.input).to_string()))
            .collect::<Vec<_>>().join("\n")
    }

    /// The note a short follow-up (`ask`) is asked once more with (`reply_check::follow_up_note`): from the user's
    /// message before this turn and the reply to it. None on a first turn.
    fn follow_up_note(&self, ask: &str) -> Option<String> {
        let before = &self.history[..self.turn_cap(&self.history).min(self.history.len())];
        let prev = before.iter().rev().find(|m| m.role == Role::User && !m.content.iter().any(|b| matches!(b, Block::ToolResult { .. }))).map(texts)?;
        let reply = before.iter().rev().find(|m| m.role == Role::Assistant && m.content.iter().any(|b| matches!(b, Block::Text { .. }))).map(texts)?;
        let reply = context::split_state(&reply).0;
        if !reply.chars().any(char::is_alphabetic) { return None; }
        Some(crate::reply_check::follow_up_note(ask, &prev, &reply))
    }

    /// The user's messages before this turn, each as written (its request body); this turn's own message is left out
    /// (a greeting answered with the same greeting, "Hi there." to "hi there", is no echo of the conversation).
    pub(super) fn user_messages(&self) -> Vec<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[..cap].iter().filter(|m| m.role == Role::User && !m.content.iter().any(|b| matches!(b, Block::ToolResult { .. })))
            .map(|m| context::request_body(&texts(m)).to_string()).collect()
    }

    /// Whether `t`, the reply to the words asked alone (`isolated_reply`), is unusable by its shape: a sentence of it
    /// said again (`reply_check::without_repeated_copies` would cut it; the first reply had the same fault), a loop,
    /// an earlier message of the user's said back or only a piece of this one, or nothing but a pitch.
    fn retry_unusable(&self, t: &str, ask: &str) -> bool {
        let own: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().collect();
        crate::reply_check::without_repeated_copies(t).trim() != t.trim() || crate::reply_check::loop_end(t).is_some()
            || crate::reply_check::echoes_said(t, &self.user_messages()) || crate::reply_check::fragment_of_ask(t, ask)
            || crate::reply_check::pitch_only(t, ask, &own)
    }

    /// Whether `vis` is not in the language of the user's message (`reply_check::lang_mismatch`), or in a script the
    /// user never wrote in this conversation (`reply_check::unseen_script`: Russian to "ok thx" after English turns).
    pub(super) fn lang_off(&self, ask: &str, vis: &str) -> bool {
        let said = self.user_said();
        crate::reply_check::lang_mismatch(ask, vis) || crate::reply_check::unseen_script(vis, &said) || crate::reply_check::third_language(vis, ask, &said)
    }

    /// `reply_check::closer_reply_fixed` for this turn: with what the session's tools returned and what the user wrote.
    fn closer_fixed(&self, vis: &str, ask: &str, own: &[String]) -> Option<String> {
        let results = self.tool_text_from(0).0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        crate::reply_check::closer_reply_fixed(vis, ask, own, &results, &self.user_said())
    }

    /// `t`, a line Elisa wrote herself for this request, marked as her own words (`own_said`). The model's wording
    /// (`gate.rs` `said_worded`: its answer from the results, an answer said again in the user's language) is never
    /// marked hers, whichever stage shows it: the gate's checks apply to it.
    pub(super) fn said_own(&self, t: String) -> String {
        if self.is_worded(&t) { return t; }
        if !t.trim().is_empty() { self.own_said.borrow_mut().push(t.trim().to_string()); }
        t
    }

    /// Whether `vis` is a line Elisa wrote herself for this request (`said_own`), not the model's words.
    pub(super) fn is_own_words(&self, vis: &str) -> bool {
        !vis.trim().is_empty() && self.own_said.borrow().iter().any(|l| l == vis.trim())
    }

    pub(super) fn own_line(&self, ask: &str) -> String { let t = self.own_line_of(ask); self.said_own(t) }

    fn own_line_of(&self, ask: &str) -> String {
        // a message that gives a fact ("hi! remember that my dog is Bo") is answered by taking note of it
        if let Some(l) = crate::reply_check::fact_ack_line(ask) { return l; }
        // a message that names the exact reply it wants ("…, then just reply OK: …") gets that reply
        if let Some(r) = crate::reply_check::asked_reply(ask) { return r; }
        // a question about one thing Elisa can do, routed as talk about her: her own answer, never a greeting
        if self.tool_lines.is_empty() { if let Some(l) = self.ability_line(ask) { return l; } }
        // (a feeling or a bit of the day told, "mình vừa chạy bộ xong, mệt quá", is small talk too: never a can line)
        if self.small_talk_turn || crate::route::small_talk(ask) || crate::route::opens_with_greeting(ask) || crate::reply_check::shares_a_moment(ask) { small_talk_fallback(ask, self.first_turn()) } else { self.no_answer_line(ask) }
    }
}

/// Elisa's lines about a page read earlier (`data/held_page.txt`): per language, the wordings that point at it and
/// the answer's templates (with a title, without one).
struct PageLines {
    vi: bool, asks: Vec<String>, line: String, untitled: String, meaning: String,
    /// the line before a quote in another language, naming it (`{language}`), and the one for a language it cannot name
    other: String, unknown: String,
    /// language names in this language, by code ("en" → "tiếng Anh")
    languages: Vec<(String, String)>,
}

fn page_lines() -> &'static [PageLines] {
    static L: std::sync::OnceLock<Vec<PageLines>> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut out: Vec<PageLines> = Vec::new();
        for l in include_str!("../../data/held_page.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(code) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                out.push(PageLines { vi: code == "vi", asks: Vec::new(), line: String::new(), untitled: String::new(), meaning: String::new(), other: String::new(), unknown: String::new(), languages: Vec::new() });
                continue;
            }
            let (Some(cur), Some((k, v))) = (out.last_mut(), l.split_once('=')) else { continue };
            match k.trim() {
                "asks" => cur.asks = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "line" => cur.line = v.trim().to_string(),
                "line untitled" => cur.untitled = v.trim().to_string(),
                "meaning" => cur.meaning = v.trim().to_string(),
                "other language" => cur.other = v.trim().to_string(),
                "other language unknown" => cur.unknown = v.trim().to_string(),
                "languages" => cur.languages = v.split('|').filter_map(|p| p.split_once(':')).map(|(c, n)| (c.trim().to_lowercase(), n.trim().to_string())).filter(|(c, n)| !c.is_empty() && !n.is_empty()).collect(),
                _ => {}
            }
        }
        out
    })
}

/// Elisa's own answer to a question pointing at a page read earlier this session (an `asks` wording of
/// `data/held_page.txt`: "trang đó dùng để làm gì?", "what is that page for?"): the newest page `web_fetch`
/// returned, as its address, title and first full sentence, quoted, in the user's language. None without such a
/// wording or page.
pub(super) fn held_page_answer(history: &[Message], ask: &str, meaning: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if !points_at_page(ask) { return None; }
    let lines = page_lines();
    let (url, content) = held_page(history)?;
    let (title, sentence) = page_title_sentence(&content);
    let sentence = sentence?;
    let vi = crate::route::vietnamese(ask);
    let l = lines.iter().find(|l| l.vi == vi).or_else(|| lines.first())?;
    let t = if title.is_empty() { &l.untitled } else { &l.line };
    let quote = t.replace("{url}", &url).replace("{title}", &title).replace("{sentence}", &sentence);
    // a page in another language than the question: its sentence's meaning in the question's language first (`meaning`,
    // checked), else a line naming the page's language ("Trang này viết bằng tiếng Anh; câu đầu trang:": `languages`
    // of the data, by `reply_lang::detect`; one it cannot name: `other language unknown`) before the quote
    let asked = crate::reply_lang::detect(ask);
    let page_lang = crate::reply_lang::detect(&sentence);
    if asked.is_none() || page_lang == asked { return Some(quote); }
    let named = page_lang.and_then(|p| l.languages.iter().find(|(code, _)| *code == lang_code(p))).map(|(_, name)| l.other.replace("{language}", name))
        .filter(|_| !l.other.is_empty());
    let other = named.or_else(|| (!l.unknown.is_empty()).then(|| l.unknown.clone()));
    Some(match (meaning(&sentence), other) {
        (Some(m), _) if !l.meaning.is_empty() => format!("{}\n\n{quote}", l.meaning.replace("{meaning}", &m)),
        (_, Some(o)) => format!("{o}\n{quote}"),
        _ => quote,
    })
}

/// A language's code as `data/held_page.txt` keys it (`languages`).
fn lang_code(l: crate::reply_lang::Lang) -> &'static str {
    match l { crate::reply_lang::Lang::En => "en", crate::reply_lang::Lang::Vi => "vi" }
}

/// Whether `ask` points at a page read earlier (an `asks` wording of `data/held_page.txt`: "trang đó", "that page").
pub(super) fn points_at_page(ask: &str) -> bool {
    let body = context::request_body(ask).to_lowercase();
    let padded = format!(" {} ", body.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>());
    page_lines().iter().any(|l| l.asks.iter().any(|a| padded.contains(&format!(" {a} "))))
}

/// The newest page `web_fetch` returned in `history`: (its address, its text). None without one.
pub(super) fn held_page(history: &[Message]) -> Option<(String, String)> {
    let mut urls: Vec<(String, String)> = Vec::new();
    let mut page: Option<(String, String)> = None;
    for b in history.iter().flat_map(|m| &m.content) {
        match b {
            Block::ToolUse { id, name, input } if name == "web_fetch" => urls.push((id.clone(), input["url"].as_str().unwrap_or("").to_string())),
            Block::ToolResult { tool_use_id, content, is_error: false } => {
                if let Some((_, u)) = urls.iter().find(|(i, _)| i == tool_use_id) { page = Some((u.clone(), content.clone())); }
            }
            _ => {}
        }
    }
    page.filter(|(u, _)| !u.is_empty())
}

/// sentences of the model's reply kept as the short note after Elisa's own line about a page she holds
const PAGE_NOTE_SENTENCES: usize = 2;

/// the request that says a page's sentence in the question's language (`page_meaning`)
const PAGE_MEANING_SYSTEM: &str = "You say what a given sentence means in another language, keeping every fact of it.";
const PAGE_MEANING_TOKENS: usize = 120;
/// chars a page sentence's meaning may have at most (more is talk beside it)
const PAGE_MEANING_CHARS: usize = 400;
/// nearness in meaning (all-MiniLM) a page's sentence said back keeps to the sentence: a restatement is 0.78 or
/// nearer, a sentence about something else 0.6 or further (`meaning_kept`)
const PAGE_MEANING_NEAR: f32 = 0.7;
/// the language the embedding model reads (all-MiniLM-L6-v2 is English-only): a page sentence in it is one whose
/// meaning said in another language can be checked (`page_meaning`)
const EMBED_LANG: crate::reply_lang::Lang = crate::reply_lang::Lang::En;

/// Whether `back`, a page's sentence said in another language and then said back in the sentence's own, keeps its
/// meaning: `PAGE_MEANING_NEAR` near the sentence (all-MiniLM; "This domain may be used in illustrative examples in
/// documents, no permission required." to "This domain is for use in documentation examples without needing
/// permission."; not "This is a website used to ensure ways of implementing features."). None without the model.
pub(super) fn meaning_kept(sentence: &str, back: &str) -> Option<bool> {
    let v = super::turn::embed_texts(&[sentence, back])?;
    Some(v[0].iter().zip(&v[1]).map(|(a, b)| a * b).sum::<f32>() >= PAGE_MEANING_NEAR)
}

/// fewest words a page's sentence needs to be quoted as what it says (a menu word or a heading is not)
const PAGE_SENTENCE_WORDS: usize = 4;
/// chars of a page's sentence quoted at most
const PAGE_SENTENCE_CHARS: usize = 240;

/// A page's text (`web::fetch`'s: "redirected to …", "title: …", then the text) as its title and first sentence of
/// PAGE_SENTENCE_WORDS words or more; Elisa's fence lines and notes left out.
fn page_title_sentence(content: &str) -> (String, Option<String>) {
    let mut title = String::new();
    let mut text = String::new();
    for l in content.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('<') || l.starts_with('[') || l.starts_with("redirected to ") { continue; }
        if let Some(t) = l.strip_prefix("title: ") { if title.is_empty() { title = t.trim().to_string(); } continue; }
        // (a heading or menu line, short and with no sentence end: no part of a sentence)
        if l.split_whitespace().count() < PAGE_SENTENCE_WORDS && !l.ends_with(['.', '!', '?']) { continue; }
        text.push_str(l);
        text.push(' ');
    }
    let mut sentence = None;
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') {
            let s = cur.trim().to_string();
            cur.clear();
            if s.split_whitespace().count() >= PAGE_SENTENCE_WORDS { sentence = Some(s); break; }
        }
    }
    let sentence = sentence.or_else(|| Some(cur.trim().to_string()).filter(|s| s.split_whitespace().count() >= PAGE_SENTENCE_WORDS))
        .map(|s| if s.chars().count() > PAGE_SENTENCE_CHARS { format!("{}…", s.chars().take(PAGE_SENTENCE_CHARS).collect::<String>()) } else { s });
    (title, sentence)
}

/// chars of a thrown-out reply a debug line quotes (`shown_quote`)
const SHOWN_QUOTE_CHARS: usize = 160;

/// A thrown-out reply for a debug line: “…”, its first SHOWN_QUOTE_CHARS chars on one line.
pub(super) fn shown_quote(t: &str) -> String {
    let one = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = one.chars().take(SHOWN_QUOTE_CHARS).collect();
    format!("\u{201c}{cut}{}\u{201d}", if one.chars().count() > SHOWN_QUOTE_CHARS { "…" } else { "" })
}

/// ms the one `/api/show` for the marks a model's server hides may take
const HIDDEN_MARKS_MS: u64 = 5000;

/// The plain line a small-talk turn shows when the reply called a tool even when asked in words, or gave nothing
/// usable: of the small talk's kind (thanks, a goodbye, a greeting) and in the user's language (`route::small_talk_line`).
/// A message that gives a fact ("hi! remember that my dog is Bo") gets Elisa's line taking note of it instead; the
/// chat's first greeting (`first`) gets the start line.
fn small_talk_fallback(input: &str, first: bool) -> String {
    crate::reply_check::fact_ack_line(input).unwrap_or_else(|| crate::route::small_talk_line_at(input, first).to_string())
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
    fn a_question_about_a_page_read_earlier_gets_its_own_sentence() {
        let page = "<untrusted source=\"web\">\ntitle: Example Domain\n\nExample Domain\nThis domain is for use in documentation examples without needing permission. Avoid use in operations.\n</untrusted>";
        let h = vec![
            Message::user_text("mở https://example.com và cho mình biết tiêu đề trang"),
            Message::new(Role::Assistant, vec![Block::ToolUse { id: "1".into(), name: "web_fetch".into(), input: json!({"url": "https://example.com"}) }]),
            Message::new(Role::User, vec![Block::ToolResult { tool_use_id: "1".into(), content: page.into(), is_error: false }]),
            Message::assistant_text("Tiêu đề trang là Example Domain."),
        ];
        let none = |_: &str| None;
        let said = |_: &str| Some("Tên miền này dùng làm ví dụ trong tài liệu, không cần xin phép.".to_string());
        let quote_vi = "Trang https://example.com (Example Domain) viết: “This domain is for use in documentation examples without needing permission.”";
        // (an English page to a Vietnamese question: its meaning in Vietnamese first, then the quote)
        assert_eq!(held_page_answer(&h, "trang đó dùng để làm gì?", &said).as_deref(),
            Some(format!("Tên miền này dùng làm ví dụ trong tài liệu, không cần xin phép.\n\n{quote_vi}").as_str()));
        // (no meaning could be had: the page's language named, the quote after it)
        assert_eq!(held_page_answer(&h, "trang đó dùng để làm gì?", &none).as_deref(),
            Some(format!("Trang này viết bằng tiếng Anh; câu đầu trang:\n{quote_vi}").as_str()));
        // (the page in the question's language: the quote alone, no request)
        assert_eq!(held_page_answer(&h, "what is that page for?", &|_| panic!("no request")).as_deref(),
            Some("https://example.com (Example Domain) says: “This domain is for use in documentation examples without needing permission.”"));
        // no wording pointing at a page, or no page read: none
        assert_eq!(held_page_answer(&h, "kể chuyện cười đi", &none), None);
        assert_eq!(held_page_answer(&tool_turn(), "what is that page for?", &none), None);
    }

    /// a page sentence's meaning said back keeps the sentence's meaning or does not (`meaning_kept`; checks only with
    /// the embedding model installed)
    #[test]
    fn a_page_meaning_said_back_is_held_against_the_sentence() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        let s = "This domain is for use in documentation examples without needing permission.";
        assert_eq!(meaning_kept(s, "This domain may be used in illustrative examples in documents, no permission required."), Some(true));
        assert_eq!(meaning_kept(s, "You can use this domain in documentation examples freely."), Some(true));
        assert_eq!(meaning_kept(s, "This is a website used to ensure ways of implementing features."), Some(false));
        assert_eq!(meaning_kept(s, "This website is a demo for testing web applications."), Some(false));
        let s = "Iceland's population was 396,500 on June 30, 2026, according to Statistics Iceland's quarterly estimate.";
        assert_eq!(meaning_kept(s, "According to Statistics Iceland's quarterly estimate, Iceland had 396,500 people on 30 June 2026."), Some(true));
        assert_eq!(meaning_kept(s, "Iceland is a Nordic island country."), Some(false));
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
        let b = "http://localhost:11434";
        let l = api_error_line("HTTP 500: invalid character '}' after object key (tried twice)", b);
        assert!(l.starts_with("the model wrote a malformed tool call"), "{l}");
        assert!(api_error_line("HTTP 502: bad gateway (tried twice)", b).starts_with("the model server failed, twice"));
        assert!(api_error_line("HTTP 401: invalid x-api-key", b).contains("API key"));
        assert_eq!(api_error_line("request failed: io: Connection refused (os error 61)", b),
            "the model server at localhost:11434 isn't answering; check that it's running, then ask again");
        assert_eq!(api_error_line("request failed: io: Connection refused (os error 61)", "https://u:secret@example.com:8443/v1?x=1"),
            "the model server at example.com:8443 isn't answering; check that it's running, then ask again");
        assert!(!api_error_line(&"x".repeat(1000), b).contains('\n'));
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
