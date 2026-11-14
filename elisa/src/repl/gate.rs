//! The one gate every text about to be shown passes, whatever stage made it (`Session::gated`): the model's reply,
//! a retry, the words asked alone, the language re-ask, the model's wording of the results, the answer a loop stop
//! got. Every one of them gets the same content checks (`gate_fault`); a text that fails falls to the next stage
//! (`fallen`), down to Elisa's plain answer from the results or her own no-answer line, and the note is written from
//! what is shown. Elisa's own verified lines (`said_own`) pass as they are; the model's wording of the results
//! (`said_worded`) never counts as hers.
use super::*;
use super::memory::is_request;

/// A text after the gate's mends: the text and, when parts of it went, which.
pub(super) struct Mended { pub text: String, pub cut: Option<&'static str> }

/// the faults a text fails the gate for, in the words of the note
const F_EMPTY: &str = "had nothing left to show";
const F_LANG: &str = "was not in the language of the user's message";
const F_GARBLED: &str = "was garbled";
const F_MARKUP: &str = "wrote Elisa's context back as markup";
const F_CANT: &str = "only said it could not do what ran or had no information";
const F_VERDICT: &str = "gave a verdict the results do not back";
const F_LEAD_IN: &str = "was a lead-in with nothing after it";
const F_NOTE: &str = "was only Elisa's language note said back";
const F_MORE: &str = "only claimed more content than the file read holds";
const F_NAME: &str = "was only Elisa's name";
const F_COPY: &str = "only said the user's message back";
const F_PIECE: &str = "copied a passage of the results that is not about the name they agree on";
const F_FRAGMENT: &str = "was a fragment of no word";
const F_TALK_FRAGMENT: &str = "was a piece of a sentence with no reply in it";
const F_OFF_MOMENT: &str = "answered nothing of what the user told";
const F_NO_NAME: &str = "said the user's question back and named nothing the results agree on";
const F_OFF_RESULTS: &str = "named nothing of the results or the question";
const F_CLOCK: &str = "gave a date or a day that is not the clock's";
/// the mends, in the words of the note
const CUT_SIGN_OFF: &str = "ended with a letter's sign-off";
const CUT_REPEAT: &str = "said the same sentence twice";
const CUT_STRAY: &str = "listed bare values of the results beside its answer";
const CUT_LEAD_IN: &str = "ended in a lead-in or a list mark with nothing after it";
const CUT_CANT: &str = "said it could not do what ran, or had no information, beside its answer";
const CUT_ORDER: &str = "said Elisa's language note back beside its answer";
const CUT_MORE: &str = "claimed more content than the file read holds beside its answer";
const CUT_ECHO: &str = "said the user's question back beside its answer";
const CUT_MARK: &str = "carried Elisa's result markers";
const CUT_END: &str = "ended in a bracketed marker";
const CUT_META: &str = "ended with a sentence about the reply itself";
const CUT_REASONING: &str = "carried the model's reasoning";
const CUT_OPENER: &str = "began with an opener nobody asked for";
const CUT_OFFER: &str = "ended with an offer or a question back nobody asked for";
const CUT_GARBLED: &str = "had a garbled sentence beside its answer";
const F_LEAK: &str = "only said Elisa's own context markers back";
const CUT_LEAK: &str = "said Elisa's own context markers back; those lines were taken out";
const CUT_LINK: &str = "gave a source no tool returned";
const CUT_REF: &str = "pointed at a message or item by a number nothing holds";
const CUT_FIGURES: &str = "listed times or dates nobody asked for";

/// sentences holding none of the results' facts a retry has more than the grounded reply before it, from which it
/// added things the results never said (`worse_than`)
const CLAIMS_ADDED_MIN: usize = 2;
/// words a parenthetical needs before it counts as a copy of a sentence outside it
const PAREN_COPY_WORDS: usize = 3;
/// words a label before a parenthetical copy has at most ("Vietnamese:", "English translation:")
const PAREN_LABEL_WORDS: usize = 2;
/// words a bare line of result values has at most, and bare lines a stray list needs
const STRAY_LINE_WORDS: usize = 2;
const STRAY_LINES_MIN: usize = 2;
/// words a line needs to be the answer sentence beside a stray list
const ANSWER_LINE_WORDS: usize = 4;

impl Session<'_> {
    /// `vis`, about to be shown for `ask`, through the gate: Elisa's own verified line as it is; the model's words,
    /// from any stage, with their stray parts out (`gate_fault`), or, when they cannot stand, what stands in their
    /// place (`fallen`). `on_screen`: `vis` is shown already, so a change takes it back and shows the new text;
    /// `checked`: `vis` went through `content_fixes` in this stage already (its one model request is never repeated).
    /// The note names the fault and what is shown.
    pub(super) fn gated(&mut self, vis: String, ask: &str, on_screen: bool, checked: bool) -> String {
        if self.is_own_words(&vis) { return self.own_tidied(vis, on_screen); }
        // (the content checks first: a claim no call made, the held file's own words, a count, a figure from nowhere)
        let vis = if checked { vis } else if on_screen { self.content_fixes(vis, ask) } else {
            match self.content_fixed(&vis, ask) {
                Some((t, why, own)) => { self.debug(why); if own { self.said_own(t) } else { t } }
                None => vis,
            }
        };
        if self.is_own_words(&vis) { return self.own_tidied(vis, on_screen); }
        // (a punctuation mark standing alone after a sentence's end, "…make up everything! .": it goes from any text
        // shown — `reply_check::without_stray_marks`)
        let vis = match crate::reply_check::without_stray_marks(&vis) {
            Some(t) => {
                if on_screen { self.fe.reply_retracted(); if self.interactive { self.fe.text_delta(&t); } }
                self.debug("[the reply had a punctuation mark standing alone after a sentence; it was taken out]");
                t
            }
            None => vis,
        };
        let (text, note) = match self.gate_fault(&vis, ask) {
            Ok(Mended { cut: None, .. }) => return vis,
            Ok(Mended { text, cut: Some(why) }) => (text, format!("[the reply {why}; that part was taken out]")),
            Err(why) => self.fallen(ask, why, &vis),
        };
        if on_screen {
            self.fe.reply_retracted();
            if self.interactive && !text.trim().is_empty() { self.fe.text_delta(&text); }
        }
        self.debug(&format!("{note} {}", super::agent_loop::shown_quote(&vis)));
        text
    }

    /// Elisa's own line `vis`, verified as it is, with the one tidy every shown text gets: a punctuation mark standing
    /// alone after a sentence's end goes ("Lotus launch on Friday..", a saved line's own full stop after the text's:
    /// `reply_check::without_stray_marks`). The tidied line is hers still; when `on_screen`, it is shown anew.
    fn own_tidied(&mut self, vis: String, on_screen: bool) -> String {
        let Some(t) = crate::reply_check::without_stray_marks(&vis) else { return vis };
        if on_screen { self.fe.reply_retracted(); if self.interactive { self.fe.text_delta(&t); } }
        self.said_own(t)
    }

    /// The gate's judgement of `vis` (the model's words, from any stage) for `ask`, with no request: Ok(the text with
    /// its stray parts out: a letter's sign-off, a parenthetical repeating a sentence, a sentence said twice, bare
    /// result values listed beside the answer, a lead-in ending in a colon with nothing after it, a false "can't" or
    /// the model's talk of its input, Elisa's language note said back, more content claimed of a file read whole), or
    /// Err(why) when it cannot stand: nothing left but a bracketed note, Elisa's name alone, the user's message said
    /// back, Elisa's context written back as markup, a verdict the results do not back to a question whether code has
    /// a mistake, a results passage copied in place of their sentence about the name they agree on, garbled text, or
    /// another language than the user's message.
    pub(super) fn gate_fault(&self, vis: &str, ask: &str) -> Result<Mended, &'static str> {
        self.gate_fault_in(vis, ask, true)
    }

    /// `gate_fault` for a text the model may have written in another language than the message's (a judgement of
    /// code asked for in words): the text after the gate's mends, or, when its only fault was its language, the same
    /// answer said in the message's language by the model (`turn::same_answer_in`) after the gate's mends. None: it
    /// cannot stand either way.
    pub(super) fn gated_in_language(&mut self, text: &str, ask: &str) -> Option<String> {
        match self.gate_fault(text, ask) {
            Ok(m) => Some(m.text),
            Err(why) if why == F_LANG => { let t = self.same_answer_in(ask, text)?; self.gate_fault(&t, ask).ok().map(|m| m.text) }
            Err(_) => None,
        }
    }

    /// `gate_fault`, judging the language only when `lang`.
    fn gate_fault_in(&self, vis: &str, ask: &str, lang: bool) -> Result<Mended, &'static str> {
        let (results, args) = self.turn_tool_text();
        let ran = !results.is_empty();
        let got = results.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        let mut text = vis.trim().to_string();
        let mut cut: Option<&'static str> = None;
        let letters = |t: &str| t.chars().any(char::is_alphanumeric);
        // (the model's reasoning written as text, "</think> Paris: …", "<｜ reasoning … </think> The joke's…": only
        // what follows the last close tag is the reply; a run opened and never closed goes)
        if let Some(t) = crate::reply_check::without_reasoning(&text) {
            if !letters(&t) { return Err(F_EMPTY); }
            text = t;
            cut = Some(CUT_REASONING);
        }
        // (Elisa's own context markers said back, "[trimmed: … 19 lines]", the clock line with its mark: those lines are
        // hers, never an answer — `context::internal_marks` lists them where they are written)
        if let Some(t) = crate::reply_check::without_internal_marks(&text) {
            if !letters(&t) { return Err(F_LEAK); }
            text = t;
            cut = Some(CUT_LEAK);
        }
        // (a day the clock does not give on a time turn, " Thu, 11 Nov 2013 13:30:00" to "bây giờ mấy giờ rồi": no answer;
        // Elisa's own clock line stands in — `reply_check::clock_disagrees` against this turn's clock lines)
        if !ran && self.time_asked(ask) {
            if let Some(turn) = self.history.iter().rev().find(|m| is_request(m)).map(super::memory::texts) {
                if crate::reply_check::clock_disagrees(&text, ask, &turn) { return Err(F_CLOCK); }
            }
        }
        let ends = crate::reply_check::without_stray_ends(&text);
        if ends.trim() != text { text = ends.trim().to_string(); cut = Some(CUT_SIGN_OFF); }
        // (a greeting when the user did not greet, an apology when nothing was wrong: B1's `without_unasked_opener`;
        // a greeting fits a reply to the conversation's first message, so the cut waits for the second user turn)
        if past_first_turn(&self.history) {
            if let Some(t) = crate::reply_check::without_unasked_opener(&text, ask) { text = t; cut = Some(CUT_OPENER); }
        }
        // (a bracketed marker the reply ends in, "[End of the conversation]")
        if let Some(t) = crate::reply_check::without_end_marker(&text) {
            if !letters(&t) { return Err(F_EMPTY); }
            text = t;
            cut = Some(CUT_END);
        }
        // (Elisa's own result markers written into the reply, "[exit 0]")
        if ran {
            if let Some(t) = crate::reply_check::without_result_marks(&text, &got) {
                if !letters(&t) { return Err(F_EMPTY); }
                text = t;
                cut = Some(CUT_MARK);
            }
        }
        // (a sentence said twice goes once, unless the message asked for words to be said again: `reply_check::asks_repeat`)
        if !crate::reply_check::asks_repeat(ask) {
            if let Some(t) = without_parenthetical_copies(&text) { text = t; cut = Some(CUT_REPEAT); }
            let once = crate::reply_check::without_repeated_copies(&text);
            if once.trim() != text { text = once.trim().to_string(); cut = Some(CUT_REPEAT); }
        }
        if ran { if let Some(t) = without_stray_value_lines(&text, &got) { text = t; cut = Some(CUT_STRAY); } }
        if let Some(t) = without_hollow_parts(&text) {
            if !letters(&t) { return Err(F_LEAD_IN); }
            text = t;
            cut = Some(CUT_LEAD_IN);
        }
        // (a tool of hers returned something in this conversation, or her clock line went with this message: the
        // model's words for its own input, "snippet", "the provided local time information", speak of her insides,
        // this turn or turns later)
        let held = ran || !self.tool_text_from(0).0.is_empty() || self.time_asked(ask);
        if let Some(t) = crate::reply_check::without_false_cant(&text, ask, ran, held) {
            if !letters(&t) { return Err(F_CANT); }
            text = t;
            cut = Some(CUT_CANT);
        }
        // (Elisa's own language note said back, "(Trả lời bằng tiếng Việt.)": the user never sees her notes)
        if let Some(t) = crate::reply_check::without_lang_orders(&text) {
            if !letters(&t) { return Err(F_NOTE); }
            text = t;
            cut = Some(CUT_ORDER);
        }
        // (more content claimed of a file read whole this turn: nothing behind it)
        if self.read_whole() {
            if let Some(t) = crate::reply_check::without_more_claims(&text) {
                if !letters(&t) { return Err(F_MORE); }
                text = t;
                cut = Some(CUT_MORE);
            }
        }
        // (a last sentence about the reply itself, "No repetition—just the reason behind the punchline.")
        if let Some(t) = crate::reply_check::without_reply_meta(&text) { text = t; cut = Some(CUT_META); }
        // (a web address neither the message nor this turn's results hold: a source the model made up, "(source:
        // https://www.forvo.com/…)" — it goes with its parenthetical or sentence: `reply_check::without_unbacked_links`)
        // (a lead-in pointing at something by a number nothing numbered, "Giống như Message #1,":
        // `reply_check::without_unbacked_numbering`)
        if let Some(t) = crate::reply_check::without_unbacked_numbering(&text, ask, &got) {
            if !letters(&t) { return Err(F_EMPTY); }
            text = t;
            cut = Some(CUT_REF);
        }
        if let Some(t) = crate::reply_check::without_unbacked_links(&text, ask, &got) {
            if !letters(&t) { return Err(F_EMPTY); }
            text = t;
            cut = Some(CUT_LINK);
        }
        // (a source named that no tool returned, "(nguồn: 25/100*480)" on a turn that ran nothing, "(source: my training
        // data)" beside web results: `reply_check::without_unbacked_sources`)
        if let Some(t) = crate::reply_check::without_unbacked_sources(&text, &format!("{got}\n{args}")) {
            if !letters(&t) { return Err(F_EMPTY); }
            text = t;
            cut = Some(CUT_LINK);
        }
        // (times and dates listed from nowhere at a sentence's end, on a turn that ran nothing and was not asked the
        // time, "… 10:41:58 11/13/2026 11:35:55" to a moment told: `reply_check::without_stray_figures`)
        if !ran && !self.time_asked(ask) {
            let known = self.tool_text_from(0).0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
            if let Some(t) = crate::reply_check::without_stray_figures(&text, ask, &known) {
                if !letters(&t) { return Err(F_EMPTY); }
                text = t;
                cut = Some(CUT_FIGURES);
            }
        }
        // (an offer or a question back nobody asked for at the end, "Want me to add something? Or is this what you
        // were looking for?" after a file's lines: `reply_check::without_offer_tail`)
        if let Some(t) = crate::reply_check::without_offer_tail(&text, ask) { text = t; cut = Some(CUT_OFFER); }
        // (what the cuts since left hollow: a list mark with nothing after it, a lead-in whose content went)
        if let Some(t) = without_hollow_parts(&text) {
            if !letters(&t) { return Err(F_LEAD_IN); }
            text = t;
            cut = Some(CUT_LEAD_IN);
        }
        // (a bracketed note alone, "[trimmed: …", or a tag alone, "</s>", that a screen shows as nothing: no answer)
        if !crate::reply_check::has_visible_words(&text) { return Err(F_EMPTY); }
        if crate::reply_check::own_name_only(&text, &self.system) { return Err(F_NAME); }
        // (a fragment of no word, " es", to a question that is no small talk; a piece of a sentence with no reply in
        // it, " Tìm thấy " to "ok thx" — the start of an earlier line, cut off — to small talk, which gets Elisa's own
        // line of its kind: `reply_check::talk_fragment`)
        let told = crate::reply_check::shares_a_moment(ask);
        let talk = self.small_talk_turn || crate::route::small_talk(ask) || crate::route::opens_with_greeting(ask) || told;
        if !talk && crate::reply_check::letter_fragment(&text, ask) { return Err(F_FRAGMENT); }
        if talk && crate::reply_check::talk_fragment(&text, ask) { return Err(F_TALK_FRAGMENT); }
        // (to a moment told, nothing but the user's words said back, "I'm hungry", or a question after their day instead
        // of about it, "How's your day going so far?": Elisa's own line for a moment told — `reply_check::off_the_moment`)
        if told && crate::reply_check::off_the_moment(&text, ask).is_some() { return Err(F_OFF_MOMENT); }
        // (the user's question said back before the answer, "Check giúp bạn với file notes.txt, mã hội nghị là gì?
        // Trả lời: …": the echo goes, and a label left in front of the answer, "Trả lời:", with it)
        if let Some(t) = crate::reply_check::without_ask_echo(&text, ask) {
            if !letters(&t) { return Err(F_COPY); }
            text = crate::reply_check::without_leading_label(&t).filter(|l| letters(l)).unwrap_or(t);
            cut = Some(CUT_ECHO);
            // (and what is left names nothing the results agree on, " Thủ đô của Úc là gì? Tìm hiểu những thế giới,
            // nhiều người vẫn bất ngờ về thủ đô này." where every result says Canberra: no answer in it)
            if ran && crate::reply_check::misses_shared_name(&text, ask, &got) { return Err(F_NO_NAME); }
        }
        // (nothing beyond the user's own words: this message back, a piece of it, its words alone, or an earlier
        // message of theirs)
        if crate::reply_check::copies_user_words(&text, ask, &self.user_messages()) { return Err(F_COPY); }
        if crate::reply_check::leaks_markup(&text, ask) { return Err(F_MARKUP); }
        // (nothing of the results nor of the question in it while the results agree on a name, "I can only provide
        // information based on the data I have." where every result says Argentina: the results' own answer)
        if ran && crate::reply_check::off_the_results(&text, ask, &got) { return Err(F_OFF_RESULTS); }
        // (a verdict of no fault to a question whether code has one, that the code read does not say itself)
        if ran && self.check_ask(ask) && crate::reply_check::unbacked_verdict(&text, &got) { return Err(F_VERDICT); }
        // (a passage of the results copied whole, nothing of the model's own in it, that is not about the name the
        // results agree on while a sentence of theirs is: the first snippet's venue sentence to "who won…")
        if ran && crate::reply_check::piece_of_results(&text, &got) {
            if let Some(s) = crate::reply_check::agreed_name_sentence(ask, &got) {
                // (the results' sentence that answers the question by meaning, holding the agreed name, is an answer
                // whoever says it: "Thủ đô của Úc là Canberra." to "thủ đô của Úc là gì?" — `turn::meaning_sentence`)
                let answers = !crate::reply_check::misses_shared_name(&text, ask, &got)
                    && super::turn::meaning_sentence(ask, &got).is_some_and(|m| crate::reply_check::same_passage(&m, &text));
                if first_word(&s) != first_word(&text) && !answers { return Err(F_PIECE); }
            }
        }
        // (one sentence of nonsense beside a sound answer, "Đwiąng: …" after "Có lỗi trong add, …": it goes and the rest
        // stands when it is clean on its own and keeps half the words at least; a garbled whole is thrown out)
        if crate::reply_check::garbled(&text, ask) {
            let kept = crate::reply_check::without_garbled_sentences(&text, ask)
                .filter(|t| letters(t) && !crate::reply_check::garbled(t, ask) && t.split_whitespace().count() * 2 >= text.split_whitespace().count());
            let Some(t) = kept else { return Err(F_GARBLED) };
            text = t;
            cut = Some(CUT_GARBLED);
        }
        if lang && self.lang_off(ask, &text) { return Err(F_LANG); }
        // (a mark left standing alone by a cut, "everything! .")
        Ok(Mended { text: crate::reply_check::without_stray_marks(&text).unwrap_or(text), cut })
    }

    /// Whether the file in question was read whole (a `read` result not cut at the context's size): one of this turn's
    /// reads, else the conversation's newest read ("what else is in that file?" turns after it). All the file holds is
    /// on the table, so a claim of more content in it has nothing behind it.
    fn read_whole(&self) -> bool {
        // (each `read` result from `from` on, in order: whether it came whole)
        let reads = |from: usize| -> Vec<bool> {
            let mut ids: Vec<&str> = Vec::new();
            let mut out = Vec::new();
            for b in self.history[from..].iter().flat_map(|m| &m.content) {
                match b {
                    Block::ToolUse { id, name, .. } if name == "read" => ids.push(id),
                    Block::ToolResult { tool_use_id, content, is_error: false } if ids.contains(&tool_use_id.as_str()) && !content.starts_with("[elisa: not run") => out.push(!content.contains("[trimmed")),
                    _ => {}
                }
            }
            out
        };
        let now = reads(self.turn_cap(&self.history).min(self.history.len()));
        if !now.is_empty() { return now.contains(&true); }
        reads(0).last().copied().unwrap_or(false)
    }

    /// Whether `ask` asks whether something is wrong in one symbol of a file ("file app.py có lỗi gì không? xem hàm
    /// add": the `[check]` side of `data/symbol_asks.txt`, by meaning).
    fn check_ask(&self, ask: &str) -> bool {
        let body = context::request_body(ask);
        self.symbol_plain(body).and_then(|p| super::turn::symbol_ask_kind(&p)).as_deref() == Some("check")
    }

    /// What stands in place of `failed` (the model's words that failed the gate for `why`), with the note saying what
    /// is shown; never a note to the user about the model. Only their language at fault, on a tool turn: the same
    /// answer said in the user's language (`same_answer_in`, one request, gated again). Another fault, on a turn whose
    /// tools returned something: Elisa's own answer from the results (worded from them and gated,
    /// `own_answer_from_turn`; not when the failed words were that wording already, nor for a question whether code
    /// has a mistake, which gets its own judgement at the turn's end, `symbol_check_fix`). Then the results' own
    /// sentence about the name they agree on (`agreed_name_sentence`), else the results in plain words
    /// (`plain_answer_from_turn`). Nothing to answer from: her no-answer line; on a turn that ran no tool, her own
    /// line of the ask's kind (`own_line`).
    fn fallen(&mut self, ask: &str, why: &'static str, failed: &str) -> (String, String) {
        let note = |shown: &str| format!("[the reply {why}; {shown}]");
        let results = !self.turn_results().is_empty();
        if why == F_LANG {
            if results {
                if let Some(t) = self.same_answer_in(ask, failed) {
                    if let Ok(m) = self.gate_fault(&t, ask) {
                        return (self.said_worded(m.text), note("Elisa had the same answer said in the user's language"));
                    }
                }
            }
        // (a question said back with nothing the results agree on: straight to the results' own sentence about the
        // name they agree on, below, not another wording by the model)
        } else if results && why != F_VERDICT && why != F_NO_NAME && why != F_OFF_RESULTS && !self.check_ask(ask) && !self.is_worded(failed) {
            if let Some(t) = self.own_answer_from_turn() {
                if self.is_own_words(&t) { return (t, note("Elisa answered from the results")); }
                if let Some(t) = self.worded_fixed(t, ask) { return (t, note("Elisa answered from the results")); }
            }
        }
        if !results {
            if self.tool_lines.is_empty() { return (self.own_line(ask), note("Elisa answered with her own line")); }
            return (self.no_answer_line(ask), note("Elisa had no answer from the results"));
        }
        // (the results' own sentence about the name they agree on: "Argentina defeated France…" to "who won…", in the
        // user's language; the results whole, Elisa's own copy of them: a fold mid-turn — a language retry on a small
        // window — shortens the history's copy to its first entry, `turn_results`)
        let got = self.turn_results().iter().map(|r| r.body.as_str()).collect::<Vec<_>>().join("\n");
        // (whatever the fault: first the results' sentence by what the ask asks beyond its subject, anchored on the
        // name they agree on, `turn::meaning_sentence`, "Thủ đô của Úc là Canberra." to "thủ đô của Úc là gì?" rather
        // than Canberra's population sentence; then the sentence opening with that name)
        if let Some(s) = super::turn::meaning_sentence(ask, &got).filter(|s| !self.lang_off(ask, s)) {
            return (self.said_own(s), note("Elisa answered with the results' sentence that answers the question"));
        }
        if let Some(s) = crate::reply_check::agreed_name_sentence(ask, &got).filter(|s| !self.lang_off(ask, s)) {
            return (self.said_own(s), note("Elisa answered with the results' sentence about the name they agree on"));
        }
        match self.plain_answer_from_turn() {
            Some(t) => (t, note("Elisa answered from the results in plain words")),
            None => (self.no_answer_line(ask), note("Elisa had no answer from the results")),
        }
    }

    /// `shown`, what the turn ends with, unless `first` is a reply thrown out for its language alone (the language
    /// retry and what came after it) and `shown` is one of Elisa's no-answer lines, or the model's words that are
    /// worse than `first` (`worse_than`: garbled, or with fewer of the results' facts, "Kia là khu vực có bán chạy
    /// nhất là Nam với 340 chiếc." after "The region with the highest sales units is South, with 340 units sold."):
    /// that answer said in the user's language (`same_answer_in`, one request, gated; not asked again of words that
    /// came from it), else the answer as it came, gated but for its language. A right answer in the wrong language
    /// beats no answer and a worse one. The screen shows the change.
    pub(super) fn answer_over_none(&mut self, shown: String, ask: &str, first: Option<String>) -> String {
        let Some(first) = first.filter(|f| f.chars().any(char::is_alphabetic)) else { return shown };
        let none = self.is_no_answer(&shown, ask);
        let worse = !none && !self.is_own_words(&shown) && self.worse_than(&shown, &first, ask);
        if !none && !worse { return shown; }
        // (its language was its only fault)
        let Ok(as_came) = self.gate_fault_in(&first, ask, false) else { return shown };
        let why = if none { "the retry was no better" } else { "the retry was worse" };
        // (the answer said again is held to the same measure: worse than `first` too, `first` as it came)
        let said = if self.is_worded(&shown) { None } else {
            self.same_answer_in(ask, &first).and_then(|t| self.gate_fault(&t, ask).ok()).filter(|m| !self.worse_than(&m.text, &first, ask))
        };
        let (text, note) = match said {
            Some(m) => (m.text, format!("[the reply's only fault was its language and {why}; Elisa had it said in the user's language]")),
            None => (as_came.text, format!("[the reply's only fault was its language and {why}; it stands as it came]")),
        };
        if !text.chars().any(char::is_alphabetic) { return shown; }
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&text); }
        self.debug(&note);
        self.said_worded(text)
    }

    /// Whether `shown`, the model's words after a retry, is worse than `first`, the reply before it: garbled
    /// (`reply_check::garbled`), or with fewer of this turn's results' facts in it (`reply_check::result_facts`: the
    /// names and numbers the results hold) while `first` has some.
    /// Or, `first` grounded in them (some of their facts in it), with CLAIMS_ADDED_MIN or more sentences more that
    /// hold none of their facts (`reply_check::claims_without_facts`): things the results never said, added by the
    /// retry ("Gặp gỡ vào thứ Hai, không biết thời gian cụ thể." beside a file's two lines told right before).
    fn worse_than(&self, shown: &str, first: &str, ask: &str) -> bool {
        if crate::reply_check::garbled(shown, ask) { return true; }
        let got = self.turn_tool_text().0.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        if got.trim().is_empty() { return false; }
        let was = crate::reply_check::result_facts(first, &got);
        if was.is_empty() { return false; }
        crate::reply_check::result_facts(shown, &got).len() < was.len()
            || crate::reply_check::claims_without_facts(shown, &got) >= crate::reply_check::claims_without_facts(first, &got) + CLAIMS_ADDED_MIN
    }

    /// Whether `t` is one of Elisa's lines that give no answer (`reply_check::no_answer_line`, `cant_answer_line`).
    fn is_no_answer(&self, t: &str, ask: &str) -> bool {
        let t = t.trim();
        t == crate::reply_check::no_answer_line(ask) || t == crate::reply_check::cant_answer_line(ask, self.client.model())
    }

    /// The model's wording of the results (`words_from_results`), when it can be shown: through the content checks
    /// (`content_fixed`: Elisa's own line in its place when one applies) and the gate's. None when it fails.
    fn worded_fixed(&mut self, t: String, ask: &str) -> Option<String> {
        let t = match self.content_fixed(&t, ask) {
            Some((f, why, own)) => { self.debug(why); if own { return Some(self.said_own(f)); } f }
            None => t,
        };
        self.gate_fault(&t, ask).ok().map(|m| self.said_worded(m.text))
    }

    /// `t`, the model's wording (of the results, or an answer said again in the user's language): never Elisa's own
    /// words, whatever stage shows it (`said_own` leaves it to the gate's checks).
    pub(super) fn said_worded(&self, t: String) -> String {
        if !t.trim().is_empty() { self.worded.borrow_mut().push(t.trim().to_string()); }
        t
    }

    /// Whether `t` is the model's wording (`said_worded`).
    pub(super) fn is_worded(&self, t: &str) -> bool {
        !t.trim().is_empty() && self.worded.borrow().iter().any(|w| w == t.trim())
    }
}

/// `text` without a last line that is a lead-in ending in a colon with nothing after it ("Bạn hãy xem kết quả này ở
/// đây:", "Here is what I found:"), as many as end it; a colon inside a code block is code. None: it ends in none.
/// Empty when the lead-in was all there was.
pub(super) fn without_dangling_lead_in(text: &str) -> Option<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    let fenced = |lines: &[&str], upto: usize| lines[..upto].iter().filter(|l| l.trim_start().starts_with("```")).count() % 2 == 1;
    let mut cut = false;
    loop {
        while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
        let Some(last) = lines.last() else { break };
        let t = last.trim_end();
        let dangling = (t.ends_with(':') || t.ends_with('：')) && t.chars().any(char::is_alphabetic) && !fenced(&lines, lines.len() - 1);
        if !dangling { break; }
        lines.pop();
        cut = true;
    }
    cut.then(|| lines.join("\n").trim().to_string())
}

/// `text` without its hollow parts: each list item that is its mark alone ("1.", "-": its words went, or never
/// came), outside code blocks, and then a last lead-in with nothing after it (`without_dangling_lead_in`), "Here
/// are the details:" above items that were all copies. None: it has none.
pub(super) fn without_hollow_parts(text: &str) -> Option<String> {
    let mut fenced = false;
    let kept: Vec<&str> = text.lines().filter(|l| {
        if l.trim_start().starts_with("```") { fenced = !fenced; return true; }
        fenced || !crate::reply_check::list_mark_only(l)
    }).collect();
    let marks_cut = kept.len() < text.lines().count();
    let mut t = kept.join("\n");
    while t.contains("\n\n\n") { t = t.replace("\n\n\n", "\n\n"); }
    let t = t.trim().to_string();
    match without_dangling_lead_in(&t) {
        Some(l) => Some(l),
        None => marks_cut.then_some(t),
    }
}

/// The lowercase alphanumeric words of `t`.
fn word_list(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The first word of `t` (a sentence's subject, for one opening with a name).
fn first_word(t: &str) -> Option<String> { word_list(t).into_iter().next() }

/// `text` without each parenthetical that only repeats a sentence outside it, with or without a label before it
/// ("Tôi không có thông tin. (Vietnamese: Tôi không có thông tin.)"): its words, PAREN_COPY_WORDS or more after a label
/// of PAREN_LABEL_WORDS words at most, run inside the rest of the text. None: it has none.
pub(super) fn without_parenthetical_copies(text: &str) -> Option<String> {
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for &(i, c) in &bytes {
        match c {
            '(' => { if depth == 0 { start = i; } depth += 1; }
            ')' if depth > 0 => { depth -= 1; if depth == 0 { spans.push((start, i + 1)); } }
            _ => {}
        }
    }
    if spans.is_empty() { return None; }
    let mut out = text.to_string();
    let mut cut = false;
    for (a, b) in spans.into_iter().rev() {
        let inner = &text[a + 1..b - 1];
        let body = match inner.split_once(':') {
            Some((label, rest)) if word_list(label).len() <= PAREN_LABEL_WORDS && !rest.trim().is_empty() => rest,
            _ => inner,
        };
        let w = word_list(body);
        if w.len() < PAREN_COPY_WORDS { continue; }
        let rest = format!("{} {}", &text[..a], &text[b..]);
        let r = word_list(&rest);
        if r.windows(w.len()).any(|x| x == w.as_slice()) {
            out.replace_range(a..b, "");
            cut = true;
        }
    }
    if !cut { return None; }
    let mut t = out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    while t.contains("\n\n\n") { t = t.replace("\n\n\n", "\n\n"); }
    Some(t.trim().to_string())
}

/// `text` without a list of bare values copied from the results beside its answer ("North\nSouth\nEast\n\nHighest
/// region: South (340 units)."): STRAY_LINES_MIN or more lines of STRAY_LINE_WORDS words at most, no list mark and no
/// sentence end, each a token of `results`, where a line of ANSWER_LINE_WORDS words or more names one of them. A list
/// with no such sentence is the answer and stays; so does a reply that lists the results itself with a mark
/// ("1. app.py … 3. injected.txt", STRAY_LINES_MIN such items or more): a line of it with no mark is an item that
/// lost its mark, never a stray value. None: it has none.
pub(super) fn without_stray_value_lines(text: &str, results: &str) -> Option<String> {
    let have: std::collections::HashSet<String> = word_list(results).into_iter().collect();
    let item = |l: &str| {
        let t = l.trim();
        let b = t.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | '•')).trim();
        b.len() < t.len() && !word_list(b).is_empty() && word_list(b).iter().all(|x| have.contains(x))
    };
    if text.lines().filter(|l| item(l)).count() >= STRAY_LINES_MIN { return None; }
    let bare = |l: &str| {
        let t = l.trim();
        let w = word_list(t);
        !t.is_empty() && w.len() <= STRAY_LINE_WORDS && !t.starts_with(['-', '*', '•', '#', '|', '`'])
            && !t.ends_with(['.', '!', '?', ':', ';']) && !t.contains([':', '=']) && w.iter().all(|x| have.contains(x))
    };
    let stray: Vec<&str> = text.lines().filter(|l| bare(l)).collect();
    if stray.len() < STRAY_LINES_MIN { return None; }
    let names: Vec<String> = stray.iter().flat_map(|l| word_list(l)).collect();
    let answer = text.lines().any(|l| { let w = word_list(l); w.len() >= ANSWER_LINE_WORDS && !bare(l) && w.iter().any(|x| names.contains(x)) });
    if !answer { return None; }
    let kept: Vec<&str> = text.lines().filter(|l| !bare(l)).collect();
    let mut t = kept.join("\n");
    while t.contains("\n\n\n") { t = t.replace("\n\n\n", "\n\n"); }
    Some(t.trim().to_string())
}

/// Whether the conversation is past its first user turn: `history` holds more than one request (this turn's is in
/// it). A greeting fits a reply to the first message; later, one nobody asked for goes (`gate_fault_in`), and Elisa's
/// own small-talk line greets on the first turn only (`repl::agent_loop`).
pub(super) fn past_first_turn(history: &[Message]) -> bool { history.iter().filter(|m| is_request(m)).count() > 1 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lead_in_with_nothing_after_it_goes_and_alone_is_nothing() {
        assert_eq!(without_dangling_lead_in("Bạn hãy xem kết quả này ở đây:").as_deref(), Some(""));
        assert_eq!(without_dangling_lead_in("Here is what I found:\n\n").as_deref(), Some(""));
        assert_eq!(without_dangling_lead_in("Tiêu đề trang là Example Domain.\n\nChi tiết:").as_deref(), Some("Tiêu đề trang là Example Domain."));
        // several lead-ins at the end go together; a colon mid-text, before content, stays
        assert_eq!(without_dangling_lead_in("The title is Example Domain.\nDetails:\nMore:").as_deref(), Some("The title is Example Domain."));
        assert_eq!(without_dangling_lead_in("The lines are:\n- buy bread\n- send report"), None);
        assert_eq!(without_dangling_lead_in("The meeting code is ORCHID-42."), None);
        // a colon ending a line of code is code
        assert_eq!(without_dangling_lead_in("Here is the fix:\n```\nif x:\n```"), None);
        assert_eq!(without_dangling_lead_in("12:30:"), None, "no letters: not a lead-in");
    }

    #[test]
    fn list_marks_with_nothing_after_them_go_and_so_does_the_lead_in_above_them() {
        assert_eq!(without_hollow_parts("The room is B12.\n\nHere are the details:\n\n1.\n2.").as_deref(), Some("The room is B12."));
        assert_eq!(without_hollow_parts("Rooms:\n- \n- B12\n* ").as_deref(), Some("Rooms:\n- B12"));
        // items with words, and a mark inside a code block, stay
        assert_eq!(without_hollow_parts("Steps:\n1. Open it.\n2. Save it."), None);
        assert_eq!(without_hollow_parts("Output:\n```\n-\n1.\n```"), None);
        assert_eq!(without_hollow_parts("Details:\n1.").as_deref(), Some(""));
        // the measure of added claims: a sentence without a name or number of the results counts, a quoted block not
        let got = "1\tRoom: B12\n2\tOwner: Mina Tran";
        assert_eq!(crate::reply_check::claims_without_facts("plan.txt says:\n```\nRoom: B12\n```\nThe room is B12.", got), 0);
        assert_eq!(crate::reply_check::claims_without_facts("Phòng là B12. Buổi họp diễn ra vào sáng thứ Hai, chưa rõ giờ.\n- Có lẽ là một sự kiện chính thức.", got), 2);
    }

    #[test]
    fn a_parenthetical_that_repeats_a_sentence_goes_with_or_without_a_label() {
        let t = "Tôi không thể trả lời câu hỏi này vì tôi không có thông tin.\n\n(Vietnamese: Tôi không thể trả lời câu hỏi này vì tôi không có thông tin.)";
        assert_eq!(without_parenthetical_copies(t).as_deref(), Some("Tôi không thể trả lời câu hỏi này vì tôi không có thông tin."));
        let t = "The page is for documentation examples. (The page is for documentation examples.)";
        assert_eq!(without_parenthetical_copies(t).as_deref(), Some("The page is for documentation examples."));
        let t = "Trang này dùng làm ví dụ trong tài liệu. (English translation: Trang này dùng làm ví dụ trong tài liệu.)";
        assert_eq!(without_parenthetical_copies(t).as_deref(), Some("Trang này dùng làm ví dụ trong tài liệu."));
        // a parenthetical that says something else, a short one, or one that is the only copy stays
        assert_eq!(without_parenthetical_copies("Good morning! (hoặc: Chào buổi sáng!)"), None);
        assert_eq!(without_parenthetical_copies("Iceland has 394,324 people (2024 estimate)."), None);
        assert_eq!(without_parenthetical_copies("The sum (a + b) is 3 (three)."), None);
        assert_eq!(without_parenthetical_copies("No parentheses here."), None);
    }

    #[test]
    fn bare_result_values_listed_beside_the_answer_go_and_a_list_that_is_the_answer_stays() {
        let results = "1\tregion,units\n2\tNorth,120\n3\tSouth,340\n4\tEast,95";
        let t = "North\nSouth\nEast\n\nHighest region: South (340 units).";
        assert_eq!(without_stray_value_lines(t, results).as_deref(), Some("Highest region: South (340 units)."));
        // the same list with no sentence naming one of them is the answer
        assert_eq!(without_stray_value_lines("North\nSouth\nEast", results), None);
        assert_eq!(without_stray_value_lines("North\nSouth\nEast\n\nThese are the regions in the file.", results), None);
        // one bare value before the sentence is the answer's value, not a stray list
        assert_eq!(without_stray_value_lines("ORCHID-42\n\nThat is the meeting code from notes.txt.", "1\tMeeting code: ORCHID-42"), None);
        // list items and labelled lines are content
        assert_eq!(without_stray_value_lines("- North\n- South\n\nSouth sold the most, 340 units.", results), None);
        assert_eq!(without_stray_value_lines("Vùng bán nhiều nhất là South với 340 đơn vị.", results), None);
        // a reply that lists the results itself, a mark lost here and there, is the answer; bare lines beside prose
        // with no such list are still stray
        let files = "app.py\nconfig.toml\ninjected.txt\nnotes.txt\nsales.csv";
        assert_eq!(without_stray_value_lines("1. app.py\n2. config.toml\ninjected.txt\nsales.csv\n\nThe folder holds injected.txt and sales.csv among these.", files), None);
        assert_eq!(without_stray_value_lines("Các file trong thư mục này:\n1) app.py\n2) config.toml\ninjected.txt\n4) notes.txt\n\nTrong đó notes.txt và injected.txt là file văn bản.", files), None);
        assert_eq!(without_stray_value_lines("app.py\nnotes.txt\n\nThe folder has app.py and a few notes.", files).as_deref(), Some("The folder has app.py and a few notes."));
    }

    fn user(t: &str) -> Message { Message::user_text(t) }
    fn reply(t: &str) -> Message { Message::assistant_text(t) }

    #[test]
    fn verdict_wordings_cover_softened_no_error_claims_in_both_languages() {
        let code = "def add(a, b):\n    return a - b";
        for t in ["There is no obvious error in the add function.", "No apparent errors here; it returns a - b.", "Nothing obviously wrong with it.",
            "Không có lỗi rõ ràng trong hàm add.", "Mình không thấy lỗi nào trong hàm này.", "Hàm add không có vấn đề gì.", "Hàm add là đúng.", "Code này đúng rồi."] {
            assert!(crate::reply_check::unbacked_verdict(t, code), "{t}");
        }
        // (a fault named, with "đúng ra" / "should" saying what it ought to be, is no verdict of no fault)
        for t in ["The add function returns a - b, which subtracts instead of adding.", "Hàm add trả về a - b, tức là trừ chứ không cộng.",
            "Hàm add bị sai: đúng ra nó phải trả về a + b.", "The tests passed with no errors.\n[exit 0]"] {
            let backed = if t.contains("tests passed") { "ran 3 tests: no errors" } else { code };
            assert!(!crate::reply_check::unbacked_verdict(t, backed), "{t}");
        }
    }

    #[test]
    fn a_denial_of_information_after_tools_ran_is_a_false_cant_and_a_bold_file_name_is_named() {
        let empty = |t: Option<String>| t.is_some_and(|t| !t.chars().any(char::is_alphanumeric));
        // after a fetch or a read, "I have no information" denies what came back: nothing of it stands
        for t in ["Tôi không thể trả lời câu hỏi này vì tôi không có thông tin.", "I cannot answer this question because I do not have the information.",
            "Mình không có quyền truy cập thông tin đó.", "I'm not able to access that info."] {
            assert!(empty(crate::reply_check::without_false_cant(t, "", true, true)), "{t}");
            // (on a turn that ran nothing, the words stand: they may be true)
            assert_eq!(crate::reply_check::without_false_cant(t, "", false, false), None, "{t}");
        }
        // what a result says it lacks is an answer, not a denial
        for t in ["The page does not mention a deadline.", "Tệp notes.txt không nhắc đến hạn chót.", "The meeting code is ORCHID-42; I do not have the room number from the file."] {
            assert!(!empty(crate::reply_check::without_false_cant(t, "", true, true)), "{t}");
        }
        // a file name in emphasis marks before a sentence end is named all the same
        for t in ["The file mentioning the meeting code is **notes.txt**.", "Tệp có mã cuộc họp là `notes.txt`.", "It is in *notes.txt*!", "Found it in \"notes.txt\"."] {
            assert_eq!(crate::reply_check::named_file(t).as_deref(), Some("notes.txt"), "{t}");
        }
        // a greeting fits a reply to the conversation's first message: the opener cut waits for the second user turn
        let mut h = vec![user("hi")];
        assert!(!past_first_turn(&h));
        h.push(reply("Hi! What can I do for you?"));
        h.push(user("what's in notes.txt?"));
        assert!(past_first_turn(&h));
    }
}
