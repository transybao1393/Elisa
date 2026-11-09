use super::*;
use super::agent_loop::{CheckRun, api_error_line};
use super::memory::{is_request, texts};

impl<'f> Session<'f> {

    /// Elisa's answer for a turn about a picture the model cannot see (`context::no_picture_line`): a model without
    /// vision and no picture in the conversation.
    pub(super) fn no_picture_line(&mut self, input: &str) -> Option<String> {
        let now = std::mem::take(&mut self.pic_refused);
        let pics = self.history.iter().any(|m| m.content.iter().any(|b| match b { Block::Image { .. } => true, Block::Text { text } => context::is_picture_block(text), _ => false }));
        if self.vision || pics || !self.attach.is_empty() { return None; }
        let vi = crate::reply_lang::detect(input) == Some(crate::reply_lang::Lang::Vi);
        let model = self.cfg.model.clone();
        let mine = |vi: bool| context::no_picture_line("this", &model, vi, true, false).unwrap_or_default();
        let last = self.history.last().filter(|m| m.role == Role::Assistant).map(texts).unwrap_or_default();
        let after = last.trim() == mine(false) || last.trim() == mine(true);
        // the router's `refused_picture` replaces the word check when it answered (BUGS #36)
        match self.router_says("refused_picture") {
            Some(asked) => (now || asked).then(|| context::no_picture_text(&model, vi)),
            None => context::no_picture_line(input, &model, vi, now, after),
        }
    }

    /// the pictures sent with the line just read; a model without vision gets none, and the user is told why
    pub(super) fn take_attached(&mut self) -> Vec<Block> {
        let imgs = self.fe.take_images();
        // refused by the display already (the app blocks pictures at attach on a model without vision, BUGS #61)
        if self.fe.take_refused_images() > 0 && !self.vision { self.pic_refused = true; }
        if imgs.is_empty() || self.vision { return imgs; }
        self.fe.error(&format!("{} can't see images, so the {} attached {} not sent. Choose a model with vision in Model Connections, or set vision = true on this connection if it has it.",
            self.cfg.model, imgs.len(), if imgs.len() == 1 { "image was" } else { "images were" }));
        self.pic_refused = true;
        Vec::new()
    }

    /// The pictures this turn asks about (BUGS #37, #17), and whether they came with it: the ones sent with it, else,
    /// on the one turn right after a picture turn, that turn's pictures again when `input` is about them
    /// (`about_last_pictures`); the pictures are dropped after that turn. An answer there that tells nothing about the
    /// picture is not carried (`look`).
    pub(super) fn turn_pictures(&mut self, input: &str) -> Option<(Vec<Block>, bool)> {
        let new = std::mem::take(&mut self.attach);
        let again = std::mem::take(&mut self.pics_next);
        if !new.is_empty() {
            (self.last_pics, self.pics_next) = (new.clone(), true);
            self.last_pic_seen = context::request_body(input).trim().to_string();
            return Some((new, true));
        }
        let old = std::mem::take(&mut self.last_pics);
        let seen = std::mem::take(&mut self.last_pic_seen);
        if !(again && self.vision && !old.is_empty()) { return None; }
        if self.about_last_pictures(input, &seen) { return Some((old, false)); }
        self.debug("[this message is not about the last picture; it was not sent again]");
        None
    }

    /// Whether the turn right after a picture turn is about its pictures (BUGS #57: resent with every next turn, the
    /// picture's answer went into "Tell me a joke." and the model answered the picture). The router's answer when it
    /// gave one: a follow-up of the last turn (why / more / a pick / "what about …") is about it, a greeting or a
    /// closing is not; else, and for anything the router leaves open, `context::about_picture` on the message and
    /// what the picture turn learned (`seen`).
    fn about_last_pictures(&self, input: &str, seen: &str) -> bool {
        if matches!(self.router_kind(), Some(Some(_))) { return true; }
        if self.router_says("greeting") == Some(true) || self.router_says("closing") == Some(true) { return false; }
        context::about_picture(input, seen)
    }

    /// The isolated picture request (`context::picture_request`): the same model, no system prompt, no history, only
    /// the pictures and the user's question. Its answer is the turn's picture text (`context::picture_block`), so the
    /// conversation itself never carries a picture: no re-sending every turn (BUGS #17: the first token took ~30 s by
    /// turn 7), and a small vision model that reads pictures only when asked alone still reads them (BUGS #37).
    /// An answer that tells nothing about the picture (`context::picture_non_answer`: a refusal, "I'm not sure what
    /// you're asking") is never carried as what the picture shows (BUGS #57, #58: granite3.2-vision:2b's "I'm sorry,
    /// but I can't assist with that." to the read ask after a picture became that turn's reply): on the turn the
    /// pictures came with, they are asked about once more with the plain describe question, and still nothing gives
    /// `PICTURE_UNSEEN`; on the turn after, there is no picture text (Ok(None)). Ok(Some((block, answer))): `answer`
    /// is the picture's real answer, None for `PICTURE_UNSEEN`.
    pub(super) fn look(&mut self, question: &str, pics: &[Block], new: bool) -> Result<Option<(String, Option<String>)>, TurnEnd> {
        let n = pics.len();
        let what = if n == 1 { "the picture".to_string() } else { format!("the {n} pictures") };
        let aid = self.acts.begin(&mut *self.fe, &crate::activity::Phrase::new("model", "picture", "", format!("Looking at {what}"), format!("Looked at {what}")));
        CANCEL.store(false, Ordering::SeqCst);
        self.client.set_kind("picture");
        let question = context::request_body(question).trim().to_string();
        let mut asks = vec![question.clone()];
        // the plain describe question as the second try (never the same question twice)
        if new && !question.is_empty() { asks.push(String::new()); }
        let last = asks.len() - 1;
        for (i, q) in asks.into_iter().enumerate() {
            let msgs = context::picture_request(&q, pics);
            let req = Request { system: "", tools: &[], messages: &msgs, max_tokens: PICTURE_OUT.min(self.cfg.max_output) };
            let t0 = Instant::now();
            let res = self.client.send(&req, &CANCEL, &mut |_| {});
            self.debug(&format!("[looked at {what} alone in {:.1}s]", t0.elapsed().as_secs_f64()));
            match res {
                Ok(r) if r.stop_reason == "cancelled" => {
                    self.acts.end_with(&mut *self.fe, aid, "cancelled", true);
                    self.unsent = true;
                    return Err(TurnEnd::Stopped);
                }
                Ok(r) => {
                    let answer = context::split_state(&texts(&Message::new(Role::Assistant, r.content))).0.trim().to_string();
                    if !context::picture_non_answer(&answer) {
                        self.acts.end_with(&mut *self.fe, aid, "", false);
                        return Ok(Some((context::picture_block(&answer), Some(answer))));
                    }
                    self.debug(&format!("[the picture answer told nothing about it: {}]", context::one_line(&answer, 80)));
                }
                // the server failed on this question (moondream:1.8b: "prediction aborted, token repeat limit
                // reached" on "Describe this picture in one sentence.", read at once with the plain question): the
                // next question, or on the turn after, no picture text (that turn's own ask goes on)
                Err(e) if i < last || !new => self.debug(&format!("[the picture request failed: {}]", api_error_line(&e))),
                Err(e) => {
                    self.acts.end_with(&mut *self.fe, aid, "failed", true);
                    self.unsent = true;
                    return Err(TurnEnd::Failed(api_error_line(&e)));
                }
            }
        }
        self.acts.end_with(&mut *self.fe, aid, "", false);
        // nothing came back that tells about the picture: said so on its own turn, so the model does not make it up
        Ok(new.then(|| (context::picture_block(context::PICTURE_UNSEEN), None)))
    }

    /// The safety net on the turn a picture came with (BUGS #59: granite3.2-vision:2b, picture text "cat", replied
    /// "I'm sorry, I don't have the ability to view or interpret images. However, … shows a cat."): a reply that is a
    /// refusal (`context::picture_non_answer`) while the picture's own answer told what it shows is replaced by that
    /// answer, the model's own words about the picture.
    pub(super) fn picture_refusal_fix(&mut self, answer: &str) {
        let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) else { return };
        if !self.tool_lines.is_empty() || !context::picture_non_answer(&texts(last)) { return; }
        self.history.pop();
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(answer); }
        self.history.push(Message::assistant_text(answer));
        self.debug("[the reply said it could not see the picture; showed the picture's own answer]");
    }

    /// The documents sent with the line just read (`attach.rs`; images need vision, documents do not): each one copied
    /// into the session's upload area and read now, before the model is asked, and its block built for `question` (the
    /// user's words) within the turn's document budget: the whole text, an overview, or the parts that answer it. One
    /// that cannot be attached (too large, not a document) is left out with the reason. With no new document, a
    /// question about one attached earlier gets the parts that answer it (`attach::follow_up`).
    pub(super) fn read_documents(&mut self, question: &str) {
        self.docs.clear();
        self.doc_retry = None;
        self.doc_new = None;
        self.doc_refused = None;
        let mut refused: Vec<String> = Vec::new();
        let mut paths = std::mem::take(&mut self.doc_paths);
        paths.extend(self.fe.take_documents());
        let question = context::request_body(question).trim();
        if paths.is_empty() { return; }
        let budget = crate::attach::budget(self.max_ctx, self.req_history_tokens());
        let mut req: Vec<String> = Vec::new();
        if paths.len() > crate::attach::DOCS_MAX {
            self.fe.error(&format!("{} documents attached: at most {} per message; the first {} are read", paths.len(), crate::attach::DOCS_MAX, crate::attach::DOCS_MAX));
            paths.truncate(crate::attach::DOCS_MAX);
        }
        let budget = budget / paths.len();
        let root = outside::root(&self.cwd);
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        for p in paths {
            let src = PathBuf::from(&p);
            let src = if src.is_absolute() { src } else { self.cwd.join(src) };
            let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
            // "Reading report.pdf…" in the activity line (src/activity.rs), "Read report.pdf · 12 pages" after
            let aid = self.acts.begin(&mut *self.fe, &crate::activity::reading_doc(&name, None));
            let cache_before = crate::cache::session_hits();
            let r = crate::attach::read(&root, context::run_id(), &src);
            if !crate::cache::new_hits(&cache_before, &crate::cache::session_hits()).is_empty() { self.debug(&format!("[cache hit: docs · {name}]")); }
            match &r {
                Ok(d) => self.acts.end_with(&mut *self.fe, aid, &d.units, false),
                Err(_) => self.acts.end_with(&mut *self.fe, aid, "not attached", true),
            }
            match r {
                Ok(mut d) => {
                    self.scrub_doc(&mut d, mode);
                    let (block, pick) = crate::attach::block_for(&d, question, budget, &[]);
                    // no tool definitions for it: the text is in the turn, and on the measured 2B model the docs tools made
                    // the reply a state card only (one more call); the routed words still decide the turn's tools
                    let large = false;
                    if let Some(it) = &d.info {
                        let mut v = crate::attach::info_json(&p, it, self.max_ctx, estimate_messages(&self.history));
                        v["line"] = serde_json::json!(crate::attach::check_line(it, match &pick { crate::attach::Pick::Whole => "whole", crate::attach::Pick::Overview => "overview", _ => "selection" }));
                        self.fe.doc_info(&v);
                    }
                    self.debug(&format!("[attached {} · {} · ~{} tokens · budget {budget} · {:?} · block ~{} tokens]", d.name, d.units, d.tokens, pick, estimate_tokens(&block)));
                    // the retry block when the reply refuses: the overview (or the same block for a whole text)
                    let retry = if matches!(pick, crate::attach::Pick::Parts(_)) { crate::attach::render(&d, &crate::attach::Pick::Overview, budget) } else { block.clone() };
                    self.doc_retry = Some((d.name.clone(), retry));
                    if pick != crate::attach::Pick::Whole { let _ = crate::attach::prefetch_embeddings(&d); }
                    self.docs.push((crate::attach::reference(&d), large));
                    req.push(block);
                    self.kept_docs.push(KeptDoc::new(d, question.to_string()));
                    self.doc_new = Some(self.kept_docs.len() - 1);
                }
                Err(e) => { self.fe.error(&format!("not attached: {e}")); refused.push(e); }
            }
        }
        if !req.is_empty() { self.set_doc_req(Some(req.join("\n\n"))); }
        // nothing could be read: the model hears it in this turn's message, so it never answers from an earlier file
        // (measured on qwen3-vl:2b: a locked PDF got "The PDF says the code word is WORD8", from the n8.txt before it)
        else if !refused.is_empty() { self.doc_refused = Some(crate::attach::refused_note(&refused)); }
    }

    /// outside text, as tool output: a secret in an attached document is hidden from the model (config `redact`)
    fn scrub_doc(&mut self, d: &mut crate::attach::Doc, mode: crate::secrets::Mode) {
        crate::secrets::learn(&d.text);
        if !mode.applies(self.model_leaves()) { return; }
        let (r, n) = crate::secrets::redact(&d.text);
        if n > 0 {
            self.fe.info(&format!("[{}: {} hidden from the model]", d.name, crate::secrets::count_words(n)));
            d.text = r;
            for part in d.parts.iter_mut() { *part = crate::secrets::redact(part).0; }
        }
    }

    /// After a saved session is loaded (`--continue`, `/resume`, "continue the last one", the dev reload): every
    /// document its history or notes name is attached again from this folder's upload area (the intake is a cache
    /// hit), so the next question about it gets its pages (`doc_follow_up`); one whose copy is gone gets a one-line
    /// note. An older session's inline document text (`[Attached: …; overview below…`, `[From … picked for this
    /// question]`) is replaced by the reference line, so stale text is not sent again.
    pub(super) fn restore_docs(&mut self, session: Option<u64>) {
        self.kept_docs.clear();
        let root = outside::root(&self.cwd);
        let mut refs: Vec<crate::attach::DocRef> = Vec::new();
        let mut rewritten = 0;
        for m in self.history.iter_mut().filter(|m| m.role == Role::User) {
            let before = m.content.len();
            m.content.retain(|b| !matches!(b, Block::Text { text } if text.starts_with("[From ") && crate::attach::is_block(text)));
            rewritten += before - m.content.len();
            for b in m.content.iter_mut() {
                let Block::Text { text } = b else { continue };
                let Some(mut r) = crate::attach::parse_ref(text) else { continue };
                if r.rel.is_none() { r.rel = crate::attach::find_upload(&root, &r.name); }
                if r.old { *text = r.line(); rewritten += 1; }
                refs.push(r);
            }
        }
        if let Some(n) = &self.notes {
            for (i, _) in n.match_indices(crate::attach::MARK) {
                let t = &n[i..];
                let line = &t[..t.find(']').map_or(t.len(), |j| j + 1)];
                if let Some(r) = crate::attach::parse_ref(line) { refs.push(r); }
            }
        }
        if rewritten > 0 { self.real = None; self.debug(&format!("[resumed session: {rewritten} inline document block(s) replaced by their reference]")); }
        // each document once, the newest mention last
        let mut seen: Vec<crate::attach::DocRef> = Vec::new();
        for r in refs.into_iter().rev() { if !seen.iter().any(|x| x.name == r.name && x.rel == r.rel) { seen.push(r); } }
        seen.truncate(crate::attach::DOCS_MAX);
        seen.reverse();
        let mode = crate::secrets::Mode::parse(&self.cfg.redact).unwrap_or(crate::secrets::Mode::Remote);
        if seen.is_empty() { self.recover_docs(&root, mode, session); }
        let query = self.history.iter().rev().filter(|m| m.role == Role::User)
            .find_map(|m| m.content.iter().find_map(|b| match b { Block::Text { text } if !crate::attach::is_block(text) => Some(context::request_body(text).trim().to_string()), _ => None }))
            .unwrap_or_default();
        for k in self.kept_docs.iter_mut() { k.query = query.clone(); }
        for r in seen {
            let got = r.rel.as_deref().map(|rel| crate::attach::open(&root, rel));
            match got {
                Some(Ok(mut d)) => {
                    self.scrub_doc(&mut d, mode);
                    self.debug(&format!("[re-attached {} · {} from {}]", d.name, d.units, d.rel));
                    self.kept_docs.push(KeptDoc::new(d, query.clone()));
                }
                _ => self.fe.info(&format!("{} is no longer in this folder's uploads; attach it again", r.name)),
            }
        }
    }

    /// A resumed conversation that names no document by reference (folded before references were kept): the files
    /// in that session's own upload folder, else the one upload whose whole title (or file name) its words or notes
    /// contain (`attach::named_in`). Never "the latest upload": with no match, or more than one, nothing is attached
    /// and nothing is said (2026-11-09: a TradingAgents chat got a newer, unrelated paper's pages).
    fn recover_docs(&mut self, root: &Path, mode: crate::secrets::Mode, session: Option<u64>) {
        let own = session.map(|s| crate::attach::uploads_of(root, s)).unwrap_or_default();
        let mut got: Vec<(crate::attach::Doc, &str)> = own.iter().filter_map(|rel| crate::attach::open(root, rel).ok().map(|d| (d, "its own upload"))).collect();
        if got.is_empty() {
            let mut text = self.notes.clone().unwrap_or_default();
            for m in &self.history {
                for b in &m.content { if let Block::Text { text: t } = b { text.push('\n'); text.push_str(t); } }
            }
            let hits: Vec<crate::attach::Doc> = crate::attach::uploads(root).iter()
                .filter_map(|rel| crate::attach::open(root, rel).ok()).filter(|d| crate::attach::named_in(d, &text)).collect();
            if hits.len() == 1 { got = hits.into_iter().map(|d| (d, "named by its title in the resumed conversation")).collect(); }
            else if hits.len() > 1 { self.debug(&format!("[resumed session: {} uploads match its words; none attached]", hits.len())); }
        }
        for (mut d, why) in got {
            self.scrub_doc(&mut d, mode);
            self.debug(&format!("[re-attached {} · {} from {} ({why})]", d.name, d.units, d.rel));
            self.kept_docs.push(KeptDoc::new(d, String::new()));
        }
    }

    /// This turn's request-only document text (None: none); a reply already measured with it keeps its real token
    /// count without it once it is gone, so the next estimate (and the fold check) sees only the history.
    pub(super) fn set_doc_req(&mut self, text: Option<String>) {
        if text.is_none() && self.doc_req_tokens > 0 {
            if let Some((n, t)) = self.real { self.real = Some((n, t.saturating_sub(self.doc_req_tokens))); }
            if let Some((r, e)) = self.calib { self.calib = Some((r.saturating_sub(self.doc_req_tokens), e.saturating_sub(self.doc_req_tokens))); }
        }
        self.doc_req_tokens = text.as_deref().map_or(0, |t| estimate_tokens(t) + 8);
        self.doc_req = text;
    }

    /// The history's size as the next turn's requests send it (old answers shortened when it is long,
    /// `context::shorten_old_replies`): what the document budget leaves room for. Called before the turn's request
    /// is in the history, so the boundary is its end (the same answers the turn's requests shorten).
    fn req_history_tokens(&self) -> usize {
        match context::shorten_old_replies(&self.history, self.history.len(), self.max_ctx) {
            Some(v) => estimate_messages(&v),
            None => estimate_messages(&self.history),
        }
    }

    /// This turn's topic (`topics::pick`: a document named, its words, a card's goal words, a follow-up of the last
    /// turn's, else none) and its card as this turn's `[state]` block (requests only). A card of another topic is
    /// never sent (2026-11-09: card #647 of an earlier PDF steered every turn after the user moved to another paper).
    pub(super) fn pick_topic(&mut self, input: &str) {
        self.card_req = None;
        let q = context::request_body(input).trim();
        let docs: Vec<crate::topics::DocTopic> = self.kept_docs.iter().map(|k| k.topic.clone()).collect();
        let p = if q.starts_with('/') { crate::topics::Pick::default() } else {
            let own = context::own_words(q).unwrap_or(q);
            crate::topics::pick(own, &docs, &self.topic_cards, &self.topic, self.doc_new.take())
        };
        let doc = p.doc.and_then(|d| docs.get(d)).map_or("none".to_string(), |d| d.name.clone());
        let card = p.card.and_then(|c| self.topic_cards.get(c)).map_or("none".to_string(), |c| c.id.map_or("this session's".to_string(), |i| format!("#{i}")));
        if !docs.is_empty() || !self.topic_cards.is_empty() { self.debug(&format!("[topic: document {doc} · card {card} · {}]", p.why)); }
        self.card_req = p.card.and_then(|c| self.topic_cards.get(c)).map(|c| format!("[state]\n{}\n[/state]", c.text.trim()));
        self.topic = p;
    }

    /// A turn with no new document, in a session that has some: the document this turn's topic picked
    /// (`pick_topic`) gets the parts that answer the message in this request (the whole text when it fits, an
    /// overview for "summarise it again"). The query is the last question about it plus the new words when those are
    /// few. A turn about something else gets no document text; "back to the paper" / "back to TradingAgents" picks it
    /// again.
    pub(super) fn doc_follow_up(&mut self, input: &str) {
        if self.kept_docs.is_empty() || self.doc_req.is_some() { return; }
        let Some(i) = self.topic.doc.filter(|&i| i < self.kept_docs.len()) else { return };
        let q = context::request_body(input).trim().to_string();
        if q.is_empty() || q.starts_with('/') { return; }
        let budget = crate::attach::budget(self.max_ctx, self.req_history_tokens());
        let carry = crate::topics::follow_up(&q);
        let k = &mut self.kept_docs[i];
        let vague = crate::attach::is_vague(&q);
        let few = q.split_whitespace().count() <= 4 && !vague;
        // a general first question ("what is this paper about?") is not carried: merged in, it picked the overview again
        // and the model repeated its summary instead of answering the follow-up (measured on qwen3-vl:2b)
        let query = if (carry || few) && !crate::attach::is_vague(&k.query) { format!("{} {q}", k.query) } else { q.clone() };
        if !carry && !vague && !few { k.query = q.clone(); }
        let (block, pick) = crate::attach::block_for(&k.doc, &query, budget, &[]);
        let log = format!("{} · {:?} · ~{} tokens", k.doc.name, pick, estimate_tokens(&block));
        let r = if matches!(pick, crate::attach::Pick::Parts(_)) { crate::attach::render(&k.doc, &crate::attach::Pick::Overview, budget) } else { block.clone() };
        self.doc_retry = Some((k.doc.name.clone(), r));
        self.debug(&format!("[documents in this request: {log} · budget {budget}]"));
        // a follow-up about a document already discussed: answer that question, not the whole document again
        // (measured on qwen3-vl:2b: "what kind of data does it work with?" got its first summary repeated)
        self.set_doc_req(Some(format!("{block}\nAnswer only the user's new question from the text above, in a few sentences; do not summarize the whole document again.")));
    }

    /// The safety net: a turn that carried document text and got a reply saying it cannot read the document (and
    /// called no tool) is asked once more, the reply replaced, with the overview (or the same text) and one line saying
    /// the text is right there. True when it retried.
    pub(super) fn doc_refusal_retry(&mut self) -> Option<TurnEnd> {
        let (name, block) = self.doc_retry.take()?;
        if !self.tool_lines.is_empty() { return None; }
        let n = self.history.len();
        if n < 2 || self.history[n - 1].role != Role::Assistant || self.history[n - 2].role != Role::User { return None; }
        if !crate::attach::refusal(&texts(&self.history[n - 1])) { return None; }
        self.history.pop();
        self.fe.reply_retracted();
        self.set_doc_req(Some(format!("{block}\nThe document's text above was extracted by Elisa from the attached file: answer from it directly.")));
        let phrase = crate::activity::Phrase::new("docs", "doc", &name, format!("Giving the model the text of {name} again"), format!("Gave the model the text of {name} again"));
        let aid = self.acts.begin(&mut *self.fe, &phrase);
        self.acts.end_with(&mut *self.fe, aid, "the reply said it could not read it", false);
        self.debug(&format!("[retry: the reply refused {name}; asked again with its text]"));
        Some(self.agent_loop(CheckRun::default(), true))
    }

    /// The safety net for a reply that says a file was saved when no write / edit succeeded this turn: one was denied
    /// or failed (`route::claims_saved`; measured on qwen3-vl:2b: "save the word hello to deny.txt", the user said
    /// no, and the reply was "I've saved "hello" to deny.txt for you."), or none was made at all and the reply names
    /// the saved file (`route::claims_file_saved`; BUGS #60, gemma3:1b: "Okay, here's the marketing plan saved to
    /// marketing-plan.md" with no write call). It checks the reply against the turn's tool calls, not the user's
    /// words. The reply is replaced: asked once more in words, with no tools and a note saying nothing was written; a
    /// reply that still claims it gets a plain line.
    pub(super) fn denied_claim_retry(&mut self) -> Option<TurnEnd> {
        let file = |l: &&String| ["[write ", "[edit ", "[edit_symbol "].iter().any(|p| l.starts_with(p));
        let (failed, ok): (Vec<&String>, Vec<&String>) = self.tool_lines.iter().filter(file).partition(|l| l.contains(" → "));
        if !ok.is_empty() { return None; }
        // no write at all: only a reply naming a saved file, and not after a command that may have written it
        let none = failed.is_empty();
        if none && self.tool_lines.iter().any(|l| l.starts_with("[bash ") || l.starts_with("[run_code ")) { return None; }
        let claim = move |t: &str| if none { crate::route::claims_file_saved(t) } else { crate::route::claims_saved(t) };
        let claims = |h: &[Message]| h.last().is_some_and(|m| m.role == Role::Assistant && claim(&texts(m)));
        if !claims(&self.history) { return None; }
        // no write this turn, but every file the reply says is saved was written earlier in this session (the
        // ledger): a true recall, not a claim to correct (BUGS #87: "what do you remember about me?" after `write
        // poem.md` got "…I saved the poem to poem.md", was retried with "nothing was saved", and answered "we
        // couldn't write to it")
        if none {
            let said = self.history.last().map(texts).unwrap_or_default();
            let backed = crate::route::file_claims(&said).iter().all(|s| self.ledger.backs(s, crate::route::file_name(s).as_deref()));
            if backed { return None; }
        }
        let said_no = failed.iter().any(|l| l.contains("→ denied by user"));
        let why = if none { "no write was made" } else if said_no { "the user said no" } else { "the write did not go through" };
        self.history.pop();
        self.fe.reply_retracted();
        self.debug(&format!("[retry: the reply said the file was saved, but {why}; asked again in words]"));
        self.retry_note = Some(format!("(For this reply: nothing was saved in this message ({why}), so no file was written or \
            changed. Say so plainly in one or two sentences, giving no other reason; never say it was saved.)"));
        let end = self.agent_loop(CheckRun::default(), false);
        self.retry_note = None;
        if !matches!(end, TurnEnd::Done) || !claims(&self.history) { return Some(end); }
        // still claims it: a plain line instead
        self.history.pop();
        self.fe.reply_retracted();
        let vi = crate::reply_lang::detect(&self.input) == Some(crate::reply_lang::Lang::Vi);
        let line = match (none, said_no, vi) {
            (true, _, false) => "I have not saved it: no file was written.",
            (true, _, true) => "Tôi chưa lưu: chưa có tệp nào được ghi.",
            (false, true, false) => "Okay, I did not save it: you said no, so nothing was written.",
            (false, false, false) => "I could not save it: the write did not go through, so nothing was written.",
            (false, true, true) => "Được rồi, tôi chưa lưu: bạn đã từ chối nên chưa có gì được ghi.",
            (false, false, true) => "Tôi chưa lưu được: lệnh ghi không thành công nên chưa có gì được ghi.",
        };
        if self.interactive { self.fe.text_delta(line); }
        self.history.push(Message::assistant_text(line));
        self.debug("[retry: the reply still said it was saved; showed a plain line]");
        Some(end)
    }

    /// A reply that says the user's own facts as Elisa's (`context::claimed_user_facts`; bug #69: "I'm Elisa, and I
    /// live in Hanoi. My meeting code is ORCHID-42.", "I'm from Hanoi too") is asked once more in words with a note
    /// naming those facts. The values come from what the user told in this conversation and what recall brought back
    /// (`recalled_values`), never from fixed words. Measured on qwen3-vl:2b(-instruct): about a third of the replies
    /// to "I'm Linh, from Hanoi…" claimed a fact, and one retry with the note fixed 9 of 11. The retry stands whatever
    /// it says (one extra request, only on such a reply).
    pub(super) fn claimed_facts_retry(&mut self) -> Option<TurnEnd> {
        let mut values = self.recalled_values.clone();
        for m in self.history.iter().filter(|m| is_request(m)) {
            for b in &m.content {
                let Block::Text { text } = b else { continue };
                if text.starts_with(context::NUDGE_PREFIX) || context::is_picture_block(text) || crate::attach::is_block(text) { continue; }
                for v in context::user_fact_values(text) { if !values.contains(&v) { values.push(v); } }
            }
        }
        if values.is_empty() { return None; }
        let last = self.history.last().filter(|m| m.role == Role::Assistant && m.content.iter().all(|b| matches!(b, Block::Text { .. })))?;
        let mut claimed = context::claimed_user_facts(&texts(last), &values);
        // a value this turn's tool calls or results carry is what the tools searched, wrote or found, not the user's
        // fact taken as Elisa's own ("I found some weather information for Hanoi" after a web search for Hanoi,
        // "I wrote a poem about Hanoi" after that write; BUGS #86)
        let from = self.history.iter().rposition(is_request).unwrap_or(0);
        let found: String = self.history[from..].iter().flat_map(|m| &m.content).filter_map(|b| match b {
            Block::ToolResult { content, is_error: false, .. } => Some(content.to_lowercase()),
            Block::ToolUse { input, .. } => Some(input.to_string().to_lowercase()),
            _ => None,
        }).collect();
        claimed.retain(|v| !found.contains(&v.to_lowercase()));
        if claimed.is_empty() { return None; }
        self.history.pop();
        self.fe.reply_retracted();
        self.debug(&format!("[retry: the reply said the user's facts as Elisa's own ({}); asked again in words]", claimed.join(", ")));
        // worded as how to write this reply, never as a verdict on one the model cannot see (it was taken back):
        // "Your reply was not sent…" got "You're right, I should…" as the answer (BUGS #86)
        let (are, them) = if claimed.len() == 1 { ("is", "it") } else { ("are", "them") };
        self.retry_note = Some(format!("(For this reply: {} {are} the user's own, not yours. Say {them} with \"you\" and \"your\", \
            never \"I\" or \"my\".)", claimed.join(", ")));
        let end = self.agent_loop(CheckRun::default(), false);
        self.retry_note = None;
        Some(end)
    }

    /// The attached documents of this conversation (file names): the kept ones and every `[Attached: NAME, …`
    /// reference in the history or the notes.
    pub(super) fn doc_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect();
        let mut scan = |t: &str| for part in t.split(crate::attach::MARK).skip(1) {
            if let Some(n) = part.split([',', ']']).next().map(str::trim).filter(|n| !n.is_empty()) {
                if !v.iter().any(|x| x == n) { v.push(n.to_string()); }
            }
        };
        for m in &self.history { for b in &m.content { if let Block::Text { text } = b { scan(text); } } }
        if let Some(n) = &self.notes { scan(n); }
        v
    }

    /// The call's `path` is a file in this session's upload area (`attach.rs`), and `docs` is Elisa's own server (no user
    /// server of that name).
    pub(super) fn reads_upload(&self, input: &serde_json::Value) -> bool {
        if self.cfg.mcp.iter().any(|c| c.name == "docs") { return false; }
        let Some(p) = input["path"].as_str() else { return false };
        let root = outside::root(&self.cwd);
        outside::resolve(&root, std::path::Path::new(p)).starts_with(crate::attach::upload_dir(&root, context::run_id()))
    }
}
