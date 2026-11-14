use super::*;
use super::agent_loop::CheckRun;
use super::memory::{is_request, texts};

/// the line after the text of documents attached with this message (request only)
const ATTACHED_NOTE: &str = "The text above is the content of the attached files, extracted by Elisa: answer from it.";

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
        // the router's `refused_picture` replaces the word check when it answered; with Elisa's own rules off, a miss
        // leaves only the fact of a picture refused now
        // (Elisa's rules: words that ask for something else, a file, a URL, a search, any `route::signals` group, and
        // name no picture are that other ask, a picture refused with them or not: "read note.txt and tell me the word
        // in it" is about the file)
        let elsewhere = || !crate::route::signals(context::request_body(input)).is_empty() && !context::picture_words(input, false);
        match self.router_says("refused_picture") {
            Some(asked) => (now || asked).then(|| context::no_picture_text(&model, vi)),
            None if self.own_rules() => if elsewhere() { None } else { context::no_picture_line(input, &model, vi, now, after) },
            None => now.then(|| context::no_picture_text(&model, vi)),
        }
    }

    /// the pictures sent with the line just read; a model without vision gets none, and the user is told why
    pub(super) fn take_attached(&mut self) -> Vec<Block> {
        let imgs = self.fe.take_images();
        // refused by the display already (the app blocks pictures at attach on a model without vision)
        if self.fe.take_refused_images() > 0 && !self.vision { self.pic_refused = true; }
        if imgs.is_empty() || self.vision { return imgs; }
        self.fe.error(&format!("{} can't see images, so the {} attached {} not sent. Choose a model with vision in Model Connections, or set vision = true on this connection if it has it.",
            self.cfg.model, imgs.len(), if imgs.len() == 1 { "image was" } else { "images were" }));
        self.pic_refused = true;
        Vec::new()
    }

    /// The pictures this turn asks about, and whether they came with it: the ones sent with it, else the last
    /// pictures again when `input` is about them (`about_last_pictures`), on the turn right after a turn that looked
    /// at them or any later one (a question about the picture after some other talk is still answered from the
    /// picture, not from what such things usually look like). They are kept until new pictures come or the
    /// conversation is cleared. An answer there that tells nothing about the picture is not carried (`look`).
    pub(super) fn turn_pictures(&mut self, input: &str) -> Option<(Vec<Block>, bool)> {
        let new = std::mem::take(&mut self.attach);
        let next = std::mem::take(&mut self.pics_next);
        if !new.is_empty() {
            (self.last_pics, self.pics_next) = (new.clone(), true);
            self.last_pic_ask = context::request_body(input).trim().to_string();
            self.last_pic_seen = self.last_pic_ask.clone();
            return Some((new, true));
        }
        if !self.vision || self.last_pics.is_empty() { return None; }
        // a turn that brings its own documents asks about them ("what is this paper about?" with a PDF), not the picture
        if self.docs.is_empty() && self.about_last_pictures(input, next) {
            self.pics_next = true;
            return Some((self.last_pics.clone(), false));
        }
        self.debug("[this message is not about the last picture; it was not sent again]");
        None
    }

    /// the pictures and what was learned about them are forgotten (/clear, /reset: a fresh start)
    pub(super) fn forget_pictures(&mut self) {
        (self.last_pics, self.pics_next, self.last_pic_seen, self.last_pic_ask) = (Vec::new(), false, String::new(), String::new());
    }

    /// Whether this turn is about the last pictures (resent with every next turn, the picture's answer would go
    /// into an unrelated ask and the model would answer the picture). Right after a turn that looked at them
    /// (`next`): the router's answer when it gave one (a follow-up of the last turn — why / more / a pick / "what
    /// about …" — is about it, a greeting or a closing is not), else `context::about_picture` on the message and
    /// what the picture turns learned (`last_pic_seen`). Later, when other talk came between, a follow-up of the last
    /// turn is about that talk: only a message that names a picture or is close in meaning to what was learned
    /// about it (`context::about_picture_later`), the pictures' answers alone: the ask that brought them ("what is
    /// this?") is close in meaning to any other "what is this …?". The word and meaning checks are Elisa's own rules:
    /// with them off (`router.rs` `decision`), only the router's follow-up right after the picture turn.
    fn about_last_pictures(&self, input: &str, next: bool) -> bool {
        let seen = &self.last_pic_seen;
        let rules = self.own_rules();
        if !next { return rules && context::about_picture_later(input, seen.strip_prefix(self.last_pic_ask.as_str()).unwrap_or(seen)); }
        if matches!(self.router_kind(), Some(Some(_))) { return true; }
        if self.router_says("greeting") == Some(true) || self.router_says("closing") == Some(true) { return false; }
        rules && context::about_picture(input, seen)
    }

    /// The isolated picture request (`context::picture_request`): the same model, no system prompt, no history, only
    /// the pictures and the user's question. Its answer is the turn's picture text (`context::picture_block`), so the
    /// conversation itself never carries a picture: no re-sending every turn (that
    /// slows every turn down), and a small vision model that reads pictures only when asked alone still reads them.
    /// An answer that tells nothing about the picture (`context::picture_non_answer`: a refusal, "I'm not sure what
    /// you're asking") is never carried as what the picture shows (it would become
    /// that turn's reply): the next question of `context::picture_asks` is tried (the user's words alone, and on the
    /// turn the pictures came with the plain describe question), and still nothing gives
    /// `PICTURE_UNSEEN`; on a later turn, there is no picture text (Ok(None)). Ok(Some((block, answer))): `answer`
    /// is the picture's real answer, None for `PICTURE_UNSEEN`.
    ///
    /// Several pictures are looked at one by one (`context::pictures_text`): a small vision model may read only one
    /// picture of a request (asked how many images came with two, it answers 1). On the turn they came with each is
    /// described, and the turn answers the question about them together from those descriptions; on the turn after,
    /// each is asked the question.
    pub(super) fn look(&mut self, question: &str, pics: &[Block], new: bool) -> Result<Option<(String, Option<String>)>, TurnEnd> {
        let n = pics.len();
        let what = if n == 1 { "the picture".to_string() } else { format!("the {n} pictures") };
        let aid = self.acts.begin(&mut *self.fe, &crate::activity::Phrase::new("model", "picture", "", format!("Looking at {what}"), format!("Looked at {what}")));
        CANCEL.store(false, Ordering::SeqCst);
        self.client.set_kind("picture");
        let question = context::request_body(question).trim().to_string();
        if n == 1 {
            let answer = self.look_one(&question, pics, new, &what, aid)?;
            self.acts.end_with(&mut *self.fe, aid, "", false);
            return Ok(match answer {
                Some(a) => Some((context::picture_block(&a), Some(a))),
                // nothing came back that tells about the picture: said so on its own turn, so the model does not make it up
                None => new.then(|| (context::picture_block(context::PICTURE_UNSEEN), None)),
            });
        }
        let q = if new { String::new() } else { question };
        let mut answers = Vec::with_capacity(n);
        for p in pics.chunks(1) { answers.push(self.look_one(&q, p, new, &what, aid)?); }
        self.acts.end_with(&mut *self.fe, aid, "", false);
        if answers.iter().all(Option::is_none) { return Ok(new.then(|| (context::picture_block(context::PICTURE_UNSEEN), None))); }
        let text = context::pictures_text(&answers);
        Ok(Some((context::picture_block(&text), Some(text))))
    }

    /// One look (`look`) at `pics`, the questions of `context::picture_asks` in turn: the answer, or None when none
    /// told anything about them (or, on a later turn, the request failed).
    fn look_one(&mut self, question: &str, pics: &[Block], new: bool, what: &str, aid: u64) -> Result<Option<String>, TurnEnd> {
        let asks = context::picture_asks(question, new);
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
                    if !context::picture_non_answer(&answer) { return Ok(Some(answer)); }
                    self.debug(&format!("[the picture answer told nothing about it: {}]", context::one_line(&answer, 80)));
                }
                // the server failed on this question (moondream:1.8b: "prediction aborted, token repeat limit
                // reached" on "Describe this picture in one sentence.", read at once with the plain question): the
                // next question, or on the turn after, no picture text (that turn's own ask goes on)
                Err(e) if i < last || !new => self.debug(&format!("[the picture request failed: {}]", context::one_line(&e, 200))),
                Err(e) => {
                    self.acts.end_with(&mut *self.fe, aid, "failed", true);
                    self.unsent = true;
                    return Err(TurnEnd::Failed(self.failed_line(&e)));
                }
            }
        }
        Ok(None)
    }

    /// The safety net on the turn a picture came with (a small vision model may say it
    /// cannot view images though its own picture answer told what the picture shows): a reply that is a
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
                    // no tool definitions for it: the text is in the turn, and on a small model the docs tools can make
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
                    // (every document of the message: the retry gives them all again)
                    self.doc_retry = Some(match self.doc_retry.take() {
                        Some((n, b)) => (format!("{n}, {}", d.name), format!("{b}\n\n{retry}")),
                        None => (d.name.clone(), retry),
                    });
                    if pick != crate::attach::Pick::Whole { let _ = crate::attach::prefetch_embeddings(&d); }
                    self.docs.push((crate::attach::reference(&d), large));
                    req.push(block);
                    self.kept_docs.push(KeptDoc::new(d, question.to_string()));
                    self.doc_new = Some(self.kept_docs.len() - 1);
                }
                Err(e) => { self.fe.error(&format!("not attached: {e}")); refused.push(e); }
            }
        }
        // the text goes before the user's words in the request (`agent_loop::with_turn_extras`), with one line saying
        // what it is (with the
        // words first and no line, a model may take the attachments for metadata only)
        if !req.is_empty() { self.set_doc_req(Some(format!("{}\n{ATTACHED_NOTE}", req.join("\n\n")))); }
        // nothing could be read: the model hears it in this turn's message, so it never answers from an earlier file
        // (otherwise a model answers from an earlier file)
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
    /// and nothing is said (the latest upload can be an unrelated document).
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
        // (a cut to fit keeps its place; a new text is after the words unless `doc_follow_up` says so)
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
    /// never sent (an earlier document's card would steer every turn after the user moves to another one).
    pub(super) fn pick_topic(&mut self, input: &str) {
        self.card_req = None;
        let q = context::request_body(input).trim();
        let docs: Vec<crate::topics::DocTopic> = self.kept_docs.iter().map(|k| k.topic.clone()).collect();
        let p = if q.starts_with('/') { crate::topics::Pick::default() } else {
            let own = context::own_words(q).unwrap_or(q);
            let (m, v, t, th, ctx) = self.facts();
            crate::topics::pick_for(own, &docs, &self.topic_cards, &self.topic, self.doc_new.take(), &crate::situation::model_words(&m, v, t, th, ctx))
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
        // (a long text about something else, "Last one, just reply OK: <…>", is not about the document: no text of it)
        let names = self.kept_docs.iter().map(|k| k.doc.name.clone()).collect::<Vec<_>>().join(", ");
        if !self.asks_about_docs(&q, &names) { return; }
        let budget = crate::attach::budget(self.max_ctx, self.req_history_tokens());
        let carry = crate::topics::follow_up(&q);
        let k = &mut self.kept_docs[i];
        let vague = crate::attach::is_vague(&q);
        let few = q.split_whitespace().count() <= 4 && !vague;
        // a general first question ("what is this paper about?") is not carried: merged in, it picked the overview again
        // and the model repeated its summary instead of answering the follow-up
        let query = if (carry || few) && !crate::attach::is_vague(&k.query) { format!("{} {q}", k.query) } else { q.clone() };
        if !carry && !vague && !few { k.query = q.clone(); }
        let (block, pick) = crate::attach::block_for(&k.doc, &query, budget, &[]);
        let log = format!("{} · {:?} · ~{} tokens", k.doc.name, pick, estimate_tokens(&block));
        let r = if matches!(pick, crate::attach::Pick::Parts(_)) { crate::attach::render(&k.doc, &crate::attach::Pick::Overview, budget) } else { block.clone() };
        self.doc_retry = Some((k.doc.name.clone(), r));
        self.debug(&format!("[documents in this request: {log} · budget {budget}]"));
        // a follow-up about a document already discussed: answer that question, not the whole document again
        // (otherwise the model repeats its first summary). The parts
        // go before the user's words, so the question is what the model reads last; the line after them says what they
        // are for without naming "the user" or "the question" to repeat (a note that names them gets
        // echoed back as the reply)
        self.set_doc_req(Some(format!("{block}\n(Only the parts of the document that the message below asks about: answer that message in a few sentences, not with a summary of the whole document.)")));
    }

    /// Whether a message after the documents were attached asks about them (`names`): it names one, is a short
    /// question or a vague one ("what is this about?"), or shares words with what they hold
    /// (`DOC_ASK_SHARED_WORDS`). A long text about something else is not.
    fn asks_about_docs(&self, ask: &str, names: &str) -> bool {
        let words = |t: &str| t.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4).map(str::to_lowercase).collect::<std::collections::HashSet<_>>();
        let held: String = names.split(", ").filter_map(|n| self.kept_docs.iter().rev().find(|k| k.doc.name == n)).map(|k| k.doc.text.as_str()).collect::<Vec<_>>().join("\n");
        names.split(", ").any(|n| ask.contains(n)) || ask.split_whitespace().count() <= DOC_ASK_SHORT_WORDS || crate::attach::is_vague(ask)
            || words(ask).intersection(&words(&held)).count() >= DOC_ASK_SHARED_WORDS
    }

    /// The safety net: a turn that carried document text and got a reply saying it cannot read the document (and
    /// called no tool) is asked once more, the reply replaced, with the overview (or the same text) and one line saying
    /// the text is right there. True when it retried.
    pub(super) fn doc_refusal_retry(&mut self) -> Option<TurnEnd> {
        let (name, block) = self.doc_retry.take()?;
        if !self.tool_lines.is_empty() { return None; }
        let n = self.history.len();
        if n < 2 || self.history[n - 1].role != Role::Assistant || self.history[n - 2].role != Role::User { return None; }
        // a refusal, or a reply that asks for or denies what the documents hold (`reply_check::ignores_given`)
        let ask = context::request_body(&self.input).to_string();
        // (or, on the turn that attached them, one that says nothing of what they hold besides their names: "Sales and
        // plan documents." for sales.xlsx and plan.ods)
        let attached_now = texts(&self.history[n - 2]).contains(crate::attach::MARK);
        let text_held: String = name.split(", ").filter_map(|n| self.kept_docs.iter().rev().find(|k| k.doc.name == n)).map(|k| k.doc.text.as_str()).collect::<Vec<_>>().join("\n");
        let held: String = if !attached_now { String::new() } else { text_held.clone() };
        // (a later message about something else, "Last one, just reply OK: <a long text>", is no ask about the
        // documents: its reply stands, and Elisa gives no answer from them)
        if !attached_now && !self.asks_about_docs(&ask, &name) { return None; }
        // (and a word or two to a question about them that their text never says, "Visual data" about a paper on
        // K-line data: `reply_check::few_words_of_nothing`)
        let few = |t: &str| crate::reply_check::few_words_of_nothing(t, &ask, &text_held);
        let refuses = |t: &str| crate::attach::refusal(t) || crate::reply_check::ignores_given(t, &ask, &block, true) || crate::reply_check::says_nothing_of(t, &ask, &name, &held) || few(t);
        if !refuses(&texts(&self.history[n - 1])) { return None; }
        self.history.pop();
        self.fe.reply_retracted();
        self.set_doc_req(Some(format!("{block}\nThe document's text above was extracted by Elisa from the attached file: answer from it directly.")));
        let phrase = crate::activity::Phrase::new("docs", "doc", &name, format!("Giving the model the text of {name} again"), format!("Gave the model the text of {name} again"));
        let aid = self.acts.begin(&mut *self.fe, &phrase);
        self.acts.end_with(&mut *self.fe, aid, "the reply said it could not read it", false);
        self.debug(&format!("[retry: the reply refused {name}; asked again with its text]"));
        let end = self.agent_loop(CheckRun::default(), true);
        // still refused: Elisa answers from the documents herself (`docs_answer`); a word or two again, with the
        // sentence of the text that answers the question best (`reply_check::best_sentence`) when one does
        let last = self.history.last().filter(|m| m.role == Role::Assistant).map(texts);
        if matches!(end, TurnEnd::Done) && last.as_deref().is_some_and(refuses) {
            match last.filter(|t| few(t)).and_then(|_| crate::reply_check::best_sentence(&ask, &text_held)) {
                Some(s) => self.doc_sentence_answer(&name, s),
                None => self.docs_answer(&name),
            }
        }
        Some(end)
    }

    /// The last reply, a word or two to a question about the document `name` twice, replaced by the sentence of its
    /// text that answers the question best.
    fn doc_sentence_answer(&mut self, name: &str, sentence: String) {
        self.fe.reply_retracted();
        if self.interactive { self.fe.text_delta(&sentence); }
        if let Some(m) = self.history.last_mut().filter(|m| m.role == Role::Assistant) { m.content = vec![Block::Text { text: sentence }]; }
        self.debug(&format!("[the reply to a question about {name} was a word or two twice; Elisa answered with its text]"));
    }

    /// The safety net for a reply that says a file was saved when no write / edit succeeded this turn: one was denied
    /// or failed (`route::claims_saved`: the user said no to a write
    /// and the reply still says it was saved), or none was made at all and the reply names
    /// the saved file (`route::claims_file_saved`: "here's the plan saved to plan.md" with no write call). It checks the reply against the turn's tool calls, not the user's
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
        // ledger): a true recall, not a claim to correct (retried as a false claim, a true
        // "I saved the poem to poem.md" would turn into "we couldn't write to it")
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

    /// A reply that says the user's own facts as Elisa's (`context::claimed_user_facts`: "I'm Elisa, and I
    /// live in Hanoi.", "I'm from Hanoi too") is asked once more in words with a note
    /// naming those facts. The values come from what the user told in this conversation and what recall brought back
    /// (`recalled_values`), never from fixed words. Small models often claim such a fact, and one
    /// retry with the note mostly fixes it. The retry stands whatever
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
        // (a line Elisa wrote herself, "Last time you asked: … You live in Hanoi.", says the facts as the user's:
        // never asked again, `said_own`)
        if self.is_own_words(&texts(last)) { return None; }
        let mut claimed = context::claimed_user_facts(&texts(last), &values);
        // a value this turn's tool calls or results carry is what the tools searched, wrote or found, not the user's
        // fact taken as Elisa's own ("I found some weather information for Hanoi" after a web search for Hanoi,
        // "I wrote a poem about Hanoi" after that write)
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
        // worded as a plain fact, never as a verdict on a reply the model cannot see (it was taken back): a verdict
        // gets "You're right, I should…", or an apology for text the user never saw and the claim again, and other
        // wordings still brought the claim back. This one gets no claim, no apology, and a short reply.
        // No "the user" in the third person either.
        let (is, facts, them) = if claimed.len() == 1 { ("is", "fact", "it") } else { ("are", "facts", "them") };
        self.retry_note = Some(format!("({} {is} their {facts}, not yours: in a short reply, say {them} with \"you\" or \"your\", \
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

    // ---------- the prompt said back, false tool claims, figures from nowhere, the held file's own words ----------

    /// Elisa's own text the model saw this turn besides the user's words: her system prompt (less the project's
    /// instructions) and her clock lines (`context::CLOCK_MARK`: the ones in the conversation's messages, and the
    /// current one, `context::local_now`, which her time fixes write a reply's clock from on a turn whose message
    /// carries none). Not the rest of what she put there (a picture's description, a file's content among her facts):
    /// an answer may well say that back.
    fn own_turn_text(&self) -> String {
        let clock: Vec<String> = self.history.iter().filter(|m| is_request(m)).map(texts)
            .flat_map(|t| t.lines().filter(|l| l.contains(context::CLOCK_MARK)).map(str::to_string).collect::<Vec<_>>()).collect();
        format!("{}\n{}\n{}", context::own_prompt(&self.system, self.project.as_deref()), clock.join("\n"), context::local_now())
    }

    /// The words a reply may share with the prompt and still answer (`reply_check::paraphrases_prompt`): the user's
    /// words of `ask`, and the names and descriptions of every tool Elisa has (an answer about what she can do).
    fn prompt_allowed(&self, ask: &str) -> String {
        let tools: Vec<String> = self.tools.iter().cloned().chain(tools::defs()).map(|t| format!("{} {}", t.name, t.description)).collect();
        format!("{}\n{}", context::request_body(ask), tools.join("\n"))
    }

    /// Elisa's own names (the one her prompt gives and the model's) when `ask` is about her (`route::about_elisa`: "who
    /// are you?"): a sentence naming her answers that. To any other question, "I am Elisa, a friendly assistant." is
    /// her prompt said back, so no name is spared then.
    fn own_names(&self, ask: &str) -> Vec<String> {
        if !crate::route::about_elisa(context::request_body(ask)) { return Vec::new(); }
        crate::reply_check::own_name(&self.system).into_iter().chain([self.client.model().to_string()]).collect()
    }

    /// Whether this turn asks the clock: `ask` asks the time, the date or the weekday (`situation::asks_clock`), or
    /// the turn's message carries Elisa's clock line for a place ("and in Tokyo?" after the time:
    /// `situation::has_place_lines`). Then her clock line is an answer, not an echo.
    pub(super) fn time_asked(&self, ask: &str) -> bool {
        crate::situation::asks_clock(context::request_body(ask))
            || self.history.iter().rev().find(|m| is_request(m)).is_some_and(|m| crate::situation::has_place_lines(&texts(m)))
    }

    /// Whether `vis` only says Elisa's own prompt or clock line back in other words ("You are a friendly assistant who
    /// is helping you." to "why is that funny?", her clock line to a question about the price of gold:
    /// `reply_check::restates_prompt`): no answer.
    pub(super) fn restates_prompt(&self, vis: &str, ask: &str) -> bool {
        !self.states_abilities(vis, ask) && crate::reply_check::restates_prompt(vis, &self.own_turn_text(), &self.prompt_allowed(ask), &self.own_names(ask), self.time_asked(ask))
    }

    /// `t` shown in place of the reply (taken back), with `why` in the debug log; `own`: it is Elisa's own line
    /// (`said_own`), not the model's words with parts cut.
    fn shown_instead(&mut self, t: String, why: &str, own: bool) -> String {
        let t = if own { self.said_own(t) } else { t };
        self.fe.reply_retracted();
        if self.interactive && !t.is_empty() { self.fe.text_delta(&t); }
        self.debug(why);
        t
    }

    /// What Elisa holds as text: the whole conversation as the model saw it (her clock and date lines, every earlier
    /// reply and result: a figure said before is not from nowhere) and what this turn's tools returned.
    fn held_text(&self) -> String {
        let said = self.history.iter().map(texts).collect::<Vec<_>>().join("\n");
        let results = self.turn_tool_text().0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        format!("{said}\n{results}")
    }

    /// The last checks on a reply about to be kept, on whatever `checked_reply` hands back (a model's words or its
    /// wording of the results), each one Elisa's own from what she holds:
    /// - call tags written as text (`reply_check::without_call_tags`), a picture described where the chat holds none
    ///   (`without_picture_claims`), a sentence copied from her own note (`without_own_note_copies`) and clauses
    ///   restating an earlier turn's facts on a tool turn (`without_earlier_facts`) go; nothing left, the turn's
    ///   results in plain words;
    /// - sentences that only say her prompt or clock line back beside an answer go (`reply_check::without_prompt_paraphrase`);
    /// - a claim of a tool action or result no call of this turn made is replaced by her line saying what did and did
    ///   not happen (`false_claim_line`);
    /// - a figure stated as today's with no web result behind it is not shown as fact: her `no_live` line, offering
    ///   to search (`reply_check::live_number_unbacked`);
    /// - an answer about a file she holds that misses what it says gets the file's own words (`held_file_answer`);
    /// - a greeting where this turn's tools returned results ("Chào bạn, rất vui được giúp đỡ! Bạn có cần gì nữa
    ///   không?" after her own web search for Iceland's population: `reply_check::greets_instead`) answers nothing:
    ///   her own answer from the results (`own_answer_from_turn`: worded from the question and the results, else the
    ///   results in plain words, `plain_answer_from_turn`).
    pub(super) fn content_fixes(&mut self, vis: String, ask: &str) -> String {
        // (a line Elisa wrote herself in place of the reply is hers, not the model's: `said_own`)
        if self.is_own_words(&vis) { return vis; }
        // (not when the message asks to be greeted, nor when the greeting word is what the results say: `reply_check::
        // asks_greeting`, `reply_check::greeting_from_results`)
        let results = self.turn_tool_text().0.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n");
        if !results.is_empty() && crate::reply_check::greets_instead(&vis, ask) && !crate::reply_check::asks_greeting(ask)
            && !crate::reply_check::greeting_from_results(&vis, &results) {
            let own = self.own_answer_from_turn().filter(|t| !crate::reply_check::greets_instead(t, ask)).or_else(|| self.plain_answer_from_turn());
            if let Some(t) = own.filter(|t| !t.trim().is_empty()) {
                // (her plain words are hers; the model's wording of the results is marked the model's already)
                return self.shown_instead(t, "[the reply greeted instead of answering from the results; Elisa answered from them]", true);
            }
        }
        match self.content_fixed(&vis, ask) {
            Some((t, why, own)) => self.shown_instead(t, why, own),
            None => vis,
        }
    }

    /// `content_fixes`' judgement without showing it (for an answer not shown yet, `turn.rs` `answer_from_tools`,
    /// `gate.rs`): the text to show in place of `vis`, the debug line saying why, and whether that text is Elisa's own
    /// line (else the model's words with parts cut, which the gate still checks); None when `vis` stands.
    pub(super) fn content_fixed(&self, vis: &str, ask: &str) -> Option<(String, &'static str, bool)> {
        // (nothing left after a cut below: the results in plain words, Elisa's step lines, else her plain line)
        let own_or_plain = || self.plain_answer_from_turn().unwrap_or_else(|| crate::reply_check::cant_answer_line(ask, self.client.model()));
        let empty = |t: &str| !t.chars().any(char::is_alphanumeric);
        // a command run on request whose output is short ("chạy lệnh echo xin-chao giúp mình"), answered with a yes or
        // no that nothing asked for ("Có, câu "xin-chao" đã được hiển thị…"), or without reporting what it printed as
        // its output ("Bài viết "xin-chao" là một câu chào…" speaks of the word: `turn::reports_run_output`): what it
        // printed, in Elisa's step line, the same on any model. A reply that tells it ("It fails with boom (exit 3).")
        // stands, without its talk beside the run ("It's nice to assist you… Have a great day!": `turn::run_talk_cut`);
        // and any reply to a question about the run stands ("todo.txt có mấy dòng? dùng wc")
        let body = context::request_body(ask);
        let res = self.turn_results();
        if crate::reply_check::about_run(body) && !body.trim_end().ends_with('?') && super::turn::short_run_output(&res) {
            let first = vis.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric());
            if crate::reply_check::lone_yes_no(first) || !super::turn::reports_run_output(vis, &res) {
                if let Some(t) = self.plain_answer_from_turn().filter(|t| t.trim() != vis.trim()) {
                    return Some((t, "[a command's short output asked to be run: Elisa showed what it printed]", true));
                }
            } else if let Some(t) = super::turn::run_talk_cut(vis, body, &res) {
                return Some(self.content_fixed(&t, ask).unwrap_or((t, "[the reply talked beside the command's output; that part was taken out]", false)));
            }
        }
        // a sentence saying where in the week it is now on a day that does not fit ("a great way to start the week" on
        // a Thursday): it goes (`situation::without_wrong_week_words`)
        if let Some(t) = crate::situation::without_wrong_week_words(vis, body, crate::situation::now_secs()) {
            // (and what is left gets the other checks)
            return Some(self.content_fixed(&t, ask).unwrap_or((t, "[the reply said where in the week it is now, which the clock does not fit; that sentence was taken out]", false)));
        }
        // a call written in tags that reached the reply (`<function=read>…</function>`): the tags go
        let tags = crate::reply_check::without_call_tags(vis);
        if tags.as_deref().is_some_and(empty) { return Some((own_or_plain(), "[the reply was only a call written in tags; Elisa said what the turn did]", true)); }
        // a picture described where the chat holds none ("Hình ảnh này hiển thị…" for a command's output): those
        // sentences go
        let base = tags.clone().unwrap_or_else(|| vis.to_string());
        let pic = if self.chat_has_picture() { None } else { crate::reply_check::without_picture_claims(&base, ask) };
        if pic.as_deref().is_some_and(empty) { return Some((own_or_plain(), "[the reply described a picture the chat does not hold; Elisa said what the turn did]", true)); }
        // a short sentence copied from Elisa's own note ("The steps are done.") and clauses restating an earlier
        // turn's facts with nothing of this ask or this turn's results ("North sold 120 units, …" after an echo)
        let base = pic.clone().unwrap_or(base);
        let notes = format!("{}\n{}", self.sent_notes, self.retry_note.as_deref().unwrap_or(""));
        let now: String = self.turn_tool_text().0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        // (a reply to the general question about what Elisa can do that states what she has is that answer, whatever
        // words it shares with her prompt or her note of what she can do: `states_abilities`)
        let abilities = self.states_abilities(&base, ask);
        let noted = if abilities { None } else { crate::reply_check::without_own_note_copies(&base, &notes, ask, &now) };
        let base = noted.clone().unwrap_or(base);
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let before: String = self.history[..cap].iter().map(texts).collect::<Vec<_>>().join("\n");
        let facts = crate::reply_check::without_earlier_facts(&base, ask, &now, &before);
        let base = facts.clone().unwrap_or(base);
        // context markup written back ("<?xml …?><doc><name>…"): no answer; the conversation told back ("Câu hỏi của
        // bạn là …", "Elisa answered …"), and a self-introduction or pitch beside the answer ("I'm Elisa, I'm a Phi
        // assistant…"): those sentences go
        // a how-to for what Elisa ran this turn, with nothing of its results ("you can try web_fetch https://…"): no
        // answer; her own from the results
        let (ran, got) = self.turn_calls();
        if crate::reply_check::tells_how(&base, ask, &ran, &got) { return Some((own_or_plain(), "[the reply told how to do what Elisa already ran; Elisa answered from the results]", true)); }
        if crate::reply_check::leaks_markup(&base, ask) { return Some((own_or_plain(), "[the reply wrote Elisa's context back as markup; Elisa said what the turn did]", true)); }
        let own: Vec<String> = crate::reply_check::own_name(&self.system).into_iter().chain([self.client.model().to_string()]).collect();
        let told = crate::reply_check::without_retelling(&base, &own);
        if told.as_deref().is_some_and(empty) { return Some((own_or_plain(), "[the reply told the conversation back instead of answering; Elisa said what the turn did]", true)); }
        let base = told.clone().unwrap_or(base);
        let pitched = crate::reply_check::without_pitch_beside_answer(&base, ask, &own);
        if pitched.as_deref().is_some_and(empty) { return Some((own_or_plain(), "[the reply was a self-introduction or pitch beside nothing; Elisa said what the turn did]", true)); }
        let base = pitched.clone().unwrap_or(base);
        // a weekday or number contradicting the file read this turn: that line from the file
        let held: Vec<String> = self.turn_tool_text().0.into_iter().filter(|(p, _)| p.is_some()).flat_map(|(_, c)| c.lines()
            .filter_map(|l| l.split_once('\t').filter(|(n, _)| !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r.trim().to_string()))
            .filter(|l| !l.is_empty()).collect::<Vec<_>>()).collect();
        let righted = crate::reply_check::held_contradictions_fixed(&base, &held);
        let cut = righted.is_some() || pitched.is_some() || told.is_some() || facts.is_some() || noted.is_some() || pic.is_some() || tags.is_some();
        let base = righted.unwrap_or(base);
        let vis = base.as_str();
        let stripped = if abilities { None } else { crate::reply_check::without_prompt_paraphrase(vis, &self.own_turn_text(), &self.prompt_allowed(ask), &self.own_names(ask), self.time_asked(ask)) };
        // (nothing left: the whole reply was her prompt or clock line said back, "Bây giờ là 12:00 trưa Thứ Năm…" to a
        // translation ask, kept after the other checks' own fixes: her plain line of the ask's kind, never the echo)
        if stripped.as_deref().is_some_and(|t| !t.chars().any(char::is_alphanumeric)) {
            return Some((crate::reply_check::cant_answer_line(ask, self.client.model()), "[the reply only said Elisa's own prompt or clock line back; Elisa said she could not answer with this model]", true));
        }
        let stripped = stripped.filter(|t| !t.is_empty()).or_else(|| cut.then(|| vis.to_string()));
        let vis = stripped.clone().unwrap_or_else(|| vis.to_string());
        // (and a sentence of an earlier reply copied before this one's answer: `reply_check::without_earlier_reply_lines`)
        let earlier: String = self.history[..cap].iter().filter(|m| m.role == Role::Assistant).map(texts).collect::<Vec<_>>().join("\n");
        let stripped = match crate::reply_check::without_earlier_reply_lines(&vis, ask, &earlier, &now) { Some(t) => Some(t), None => stripped };
        let vis = stripped.clone().unwrap_or(vis);
        // (her line alone is hers; after the reply's other sentences, when they say more than the claim
        // (`reply_check::claims_replaced` joins the two with a blank line), the model's words still)
        // (a closer or a greeting answered with an earlier message of the user's said back, most of its words, "xem
        // hàm add trong app.py có lỗi hay không." to "cảm ơn nhé": her own line of the ask's kind, `says_earlier_message`)
        if self.tool_lines.is_empty() && crate::reply_check::plain_talk(ask) && says_earlier_message(&vis, &self.user_messages()) {
            return Some((self.own_line(ask), "[the reply to small talk said an earlier message of the user's back; Elisa answered with her own line]", true));
        }
        // (a status the files read this turn do not give, "các việc đã gửi đi" for "- send report": the files' own
        // lines as Elisa tells them, `status_unheld`)
        if super::turn::reads_only_text(&self.turn_results()).is_some_and(|held| status_unheld(&vis, &held)) {
            if let Some(h) = self.held_file_answer("", ask) {
                return Some((h, "[the reply gave the file's items a status its lines do not hold; Elisa told its lines]", true));
            }
        }
        // (a code a character off one the session's results hold and held by none of them, "ORCHID-422" where the
        // search found "ORCHID-42": the held one in its place, `near_miss_codes_fixed`)
        let results: String = self.tool_text_from(0).0.into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n");
        if let Some(t) = near_miss_codes_fixed(&vis, &format!("{results}\n{}", context::request_body(ask))) {
            let t = self.content_fixed(&t, ask).map_or(t, |f| f.0);
            return Some((t, "[the reply gave a code one character off what the results hold; Elisa put the held one in its place]", false));
        }
        if let Some(t) = self.false_claim_line(&vis, ask) {
            let own = !t.contains("\n\n");
            return Some((t, "[the reply claimed a tool action or result that no call made; Elisa said what did and did not happen]", own));
        }
        if let Some(t) = self.count_fix(&vis, ask) {
            return Some((t, "[a count of a project file answered in another language or without the number; Elisa counted it herself]", true));
        }
        if !self.web_ran() && crate::reply_check::live_number_unbacked(&vis, ask, &self.held_text()) {
            return Some((crate::reply_check::own_line_of("no_live", ask, ""), "[the reply gave today's figure with no result behind it; Elisa offered to search instead]", true));
        }
        if let Some(t) = self.held_file_answer(&vis, ask) {
            return Some((t, "[the reply missed what the file it is about holds; Elisa answered from the file]", true));
        }
        stripped.map(|t| (t, "[the reply said Elisa's own prompt, clock line, a note, an earlier reply's line or fact, a picture or call tags beside its answer; those parts were taken out]", false))
    }

    /// The files read this session: each successful read-only call's result, newest last, as (the file's name, its text).
    fn session_reads(&self) -> Vec<(String, String)> {
        let mut paths: Vec<(String, String)> = Vec::new();
        let mut out = Vec::new();
        for b in self.history.iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if tools::access(name) == Access::ReadOnly => {
                    if let Some(p) = input["path"].as_str() { paths.push((id.clone(), p.rsplit('/').next().unwrap_or(p).to_string())); }
                }
                Block::ToolResult { tool_use_id, content, is_error: false } if !content.starts_with("[elisa: not run") => {
                    if let Some((_, n)) = paths.iter().find(|(i, _)| i == tool_use_id) { out.push((n.clone(), content.clone())); }
                }
                _ => {}
            }
        }
        out
    }

    /// Whether a web tool ran this turn (`route::tool_group`): its results back today's figures.
    fn web_ran(&self) -> bool {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        self.history[cap..].iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::ToolUse { name, .. }
            if crate::route::tool_group(name, self.mcp.server_of(name)) == crate::route::WEB))
    }

    /// Whether a change or a command ran or was tried this turn (a write, edit or command line among the tool lines):
    /// then a claim of a change is the other checks' (`denied_claim_retry`, `edit_claim_retry`), and a file's content
    /// may have changed.
    fn change_tried(&self) -> bool {
        self.tool_lines.iter().any(|l| ["[write ", "[edit ", "[edit_symbol ", "[bash ", "[run_code "].iter().any(|p| l.starts_with(p)))
    }

    /// Elisa's honest line for a reply that claims a tool action or result no call of this turn made
    /// (`reply_check::tool_claims`): a file read or its content told where no read of that file this session holds it
    /// ("Đọc file notes.txt, tôi thấy mã cuộc họp là MEETING001."; the `read_failed` line when this turn's read of it
    /// failed, `failed_reads`), a save or an edit where no write, edit or command ran or was tried this turn ("Đã ghi
    /// lại 'xin chào thế giới' vào file hello.txt.", "Đã đổi 'buy milk' thành 'buy bread'…"; a change the session's
    /// records hold is a true recall, `Ledger::backs`). The reply's other sentences stay before the line when they say
    /// more than the claim (`reply_check::claims_replaced`). None: no such claim, or a change was tried this turn (then
    /// a claim of one is `denied_claim_retry`'s and `edit_claim_retry`'s). A saved-file claim those two retries find
    /// themselves (`route::claims_file_saved`, `reply_check::is_done_claim_for`) is left to them: they ask the model
    /// once more first. This catches the wordings they miss (Vietnamese, a content told of a file never read). And a
    /// command's run or output reported ("It printed hello-elisa again." to "again") on a turn that called nothing,
    /// where the message asks for a thing done, not about an earlier run ("what did it print?" is a recall): the
    /// `not_run` line.
    fn false_claim_line(&self, vis: &str, ask: &str) -> Option<String> {
        if self.change_tried() { return None; }
        let body = context::request_body(ask);
        let asked_file = crate::reply_check::named_file(body);
        let files: Vec<String> = asked_file.iter().cloned().collect();
        let question = body.trim_end().ends_with('?') || crate::reply_check::yes_no_question(body);
        let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
        let reads = self.session_reads();
        let read_ever: Vec<String> = reads.iter().map(|(n, _)| n.to_lowercase()).chain(self.kept_docs.iter().map(|k| k.doc.name.to_lowercase())).collect();
        let failed = self.failed_reads();
        // (a write or edit wording in a sentence that tells what a file Elisa read holds, "Mã cuộc họp đã ghi trong
        // notes.txt là ORCHID-42": the file's content, not an action of hers: `reply_check::tells_held_content`)
        // (not a sentence quoting text the file does not hold, "Đã viết `return a - b` thay cho `return a + b`", "Mình
        // đã sửa thành `return a + b`": new text written in, a change claimed: `quotes_unheld`)
        let tells_read = |s: &str, file: Option<&str>| file.is_some_and(|f| reads.iter().filter(|(n, _)| n.to_lowercase() == base(f))
            .any(|(_, c)| crate::reply_check::tells_held_content(s, c, f) && !quotes_unheld(s, c)));
        // (on a small-talk turn, or to a message telling the user's own doing, "mình vừa chạy bộ xong", nothing was
        // asked to run: a run wording in the reply speaks of that, not of a tool: `reply_check::tells_own_doing`)
        let talk = self.small_talk_turn || crate::reply_check::tells_own_doing(body);
        let found: Vec<(String, String, Option<String>)> = crate::reply_check::tool_claims(vis, &["write", "edit", "read", "run", "run_result"], asked_file.is_some()).into_iter()
            .map(|(k, s, f)| { let file = f.or_else(|| asked_file.clone()); (k, s, file) })
            .filter(|(k, s, file)| match k.as_str() {
                // (no file named anywhere: too vague to call false; one read before: a recall, held to its text by `held_file_answer`)
                "read" => file.as_deref().is_some_and(|f| !read_ever.contains(&base(f))),
                "write" => !crate::route::claims_file_saved(s) && !self.ledger.backs(s, file.as_deref()) && !tells_read(s, file.as_deref()),
                // (a run reported on a turn that called nothing, to a message that asks for a thing done, not a question)
                "run" => self.tool_lines.is_empty() && !question && !talk,
                // (a result told with no run in its words, "kết quả là 120": a run claimed only to a message about a run)
                "run_result" => self.tool_lines.is_empty() && !question && !talk && crate::reply_check::about_run(body),
                _ => !crate::reply_check::is_done_claim_for(s, &files, body) && !self.ledger.backs(s, file.as_deref()) && !tells_read(s, file.as_deref()),
            }).collect();
        // (a claim in one clause of a sentence that denies in another, "I created a file named .elisa/u06.txt, but I
        // can't write anything into it": the whole sentence is the claim, `clause_claims`)
        let found = if found.is_empty() {
            clause_claims(vis, &["write", "edit"], asked_file.is_some()).into_iter()
                .map(|(k, s, c, f)| (k, s, c, f.or_else(|| asked_file.clone())))
                .filter(|(_, _, c, file)| !crate::route::claims_file_saved(c) && !self.ledger.backs(c, file.as_deref()) && !tells_read(c, file.as_deref()))
                .map(|(k, s, _, f)| (k, s, f)).collect()
        } else { found };
        let (kind, _, file) = found.first()?;
        let line = match (kind.as_str(), file.as_deref()) {
            ("read", Some(f)) if failed.contains(&base(f)) => crate::reply_check::own_line_of("read_failed", ask, f),
            ("read", Some(f)) => crate::reply_check::own_line_of("not_read", ask, f),
            ("read", None) => return None,
            ("write", Some(f)) => crate::reply_check::own_line_of("not_written", ask, f),
            ("write", None) => crate::reply_check::own_line_of("not_written_any", ask, ""),
            ("run" | "run_result", _) => crate::reply_check::own_line_of("not_run", ask, ""),
            _ => crate::reply_check::not_changed_line(ask).to_string(),
        };
        let drop: Vec<String> = found.iter().map(|(_, s, _)| s.clone()).collect();
        Some(crate::reply_check::claims_replaced(vis, &drop, &line))
    }

    /// Elisa's own count for an ask that counts a project file's lines, words or characters ("đếm số dòng trong
    /// todo.txt bằng wc", "how many lines does todo.txt have?": `turn::asked_counts`), from the file itself, in the
    /// ask's language (`turn::count_text`: "todo.txt có 2 dòng."), when the reply gives the count in another language
    /// (a line of it in the other language: "todo.txt: 2 lines" under a Vietnamese sentence) or without the number.
    /// None: no count ask, no such file in the project (or a large one), or the reply gives it in the ask's language.
    pub(super) fn count_fix(&self, vis: &str, ask: &str) -> Option<String> {
        let body = context::request_body(ask);
        let units = super::turn::asked_counts(body);
        if units.is_empty() { return None; }
        let name = crate::reply_check::named_file(body)?;
        let path = self.cwd.join(&name);
        std::fs::metadata(&path).ok().filter(|m| m.is_file() && m.len() <= COUNT_FILE_MAX_BYTES)?;
        let text = std::fs::read_to_string(&path).ok()?;
        let want = crate::reply_lang::detect(body);
        let line = super::turn::count_text(&name, &text, &units, want == Some(crate::reply_lang::Lang::Vi));
        let runs = |t: &str| t.split(|c: char| !c.is_ascii_digit()).filter(|d| !d.is_empty()).map(str::to_string).collect::<Vec<_>>();
        let have = runs(vis);
        let numbers_given = runs(&line).iter().all(|n| have.contains(n));
        // (every line of the reply in the ask's language: "todo.txt: 2 lines" under a Vietnamese sentence is not; a
        // bare number, "8", is in any language, and Vietnamese is told by its marks, so to an English ask only a
        // Vietnamese line is foreign)
        let lines: Vec<&str> = vis.lines().filter(|l| l.chars().any(char::is_alphabetic)).collect();
        let same_lang = match want {
            None => true,
            Some(crate::reply_lang::Lang::Vi) => lines.iter().all(|l| crate::reply_lang::detect(l) == want),
            Some(crate::reply_lang::Lang::En) => lines.iter().all(|l| crate::reply_lang::detect(l) != Some(crate::reply_lang::Lang::Vi)),
        };
        (!numbers_given || !same_lang).then_some(line)
    }

    /// The files (their names, lowercase) a read-only call of this turn failed on (the file is not there, or unreadable):
    /// their content told in the reply is from nowhere.
    fn failed_reads(&self) -> Vec<String> {
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut paths: Vec<(String, String)> = Vec::new();
        let mut out = Vec::new();
        for b in self.history[cap..].iter().flat_map(|m| &m.content) {
            match b {
                Block::ToolUse { id, name, input } if tools::access(name) == Access::ReadOnly => {
                    if let Some(p) = input["path"].as_str() { paths.push((id.clone(), p.rsplit('/').next().unwrap_or(p).to_lowercase())); }
                }
                Block::ToolResult { tool_use_id, is_error: true, .. } => {
                    if let Some((_, n)) = paths.iter().find(|(i, _)| i == tool_use_id) { out.push(n.clone()); }
                }
                _ => {}
            }
        }
        out
    }

    /// The index of the held line that answers `body`, by a tiny question to the user's own model: the numbered lines
    /// and the ask, a reply of the line number only (`reply_check::pick_held_line`), asked once in the file's order
    /// and once in the reverse order. Taken only when both name the same line: a small model that answers by place
    /// ("2", "1") rather than by meaning names another line the second time. None when they differ, a reply gives no
    /// valid number, or a call fails.
    fn line_by_model(&self, body: &str, lines: &[String]) -> Option<usize> {
        if CANCEL.load(Ordering::SeqCst) { return None; }
        let no_tools: [ToolDef; 0] = [];
        let ask_model = |q: &str| {
            let msgs = [Message::user_text(q.to_string())];
            let req = Request { system: HELD_PICK_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: HELD_PICK_TOKENS };
            let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
            Some(context::split_state(&texts(&Message::new(Role::Assistant, r.content))).0.trim().to_string())
        };
        if let Some(i) = pick_in_both_orders(body, lines, ask_model) { return Some(i); }
        // (no line numbered alike both ways, as small models often do: the thing asked said by the same model in
        // the language of the file's labels, "mã cuộc họp" → "meeting code", then found by the labels, or by meaning
        // when no label has its words: one line only)
        if CANCEL.load(Ordering::SeqCst) { return None; }
        let labels: Vec<&str> = lines.iter().filter_map(|l| l.split_once([':', '=']).map(|(a, _)| a.trim())).filter(|a| !a.is_empty()).collect();
        let msgs = [Message::user_text(format!("Question: {body}\nLabels: {}", labels.join("; ")))];
        let req = Request { system: HELD_RESTATE_SYSTEM, tools: &no_tools, messages: &msgs, max_tokens: HELD_PICK_TOKENS };
        let r = self.client.send(&req, &CANCEL, &mut |_| {}).ok().filter(|r| r.stop_reason != "cancelled")?;
        let said = context::split_state(&texts(&Message::new(Role::Assistant, r.content))).0.trim().to_string();
        // (an answer giving a line's value is no restatement of the ask: the model picked, unchecked)
        if lines.iter().any(|l| crate::reply_check::gives_line(&said, l)) { return None; }
        pick_by_restated(&said, lines).or_else(|| {
            (crate::reply_check::words_of(&said).len() <= HELD_RESTATE_WORDS).then(|| self.line_by_meaning(&said, lines)).flatten().and_then(|l| lines.iter().position(|x| x == l))
        })
    }

    /// The file's line nearest in meaning to the ask (all-MiniLM-L6-v2, `route::meaning_scores` on the lines' labels),
    /// when it is near enough and clearly nearer than the next ("mã cuộc họp" → "Meeting code: …"); None without
    /// the embedding model or a clear pick.
    fn line_by_meaning<'a>(&self, body: &str, lines: &'a [String]) -> Option<&'a String> {
        let labelled: Vec<(&String, String)> = lines.iter().filter_map(|l| l.split_once([':', '=']).map(|(a, _)| (l, a.trim().to_string()))).filter(|(_, a)| !a.is_empty()).collect();
        if labelled.len() < 2 { return None; }
        let labels: Vec<&str> = labelled.iter().map(|(_, a)| a.as_str()).collect();
        let scores = crate::route::meaning_scores(body, &labels)?;
        let mut ranked: Vec<(f32, usize)> = scores.iter().enumerate().map(|(i, s)| (*s, i)).collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (top, second) = (ranked[0], ranked[1]);
        (top.0 >= HELD_MEANING_MIN && top.0 - second.0 >= HELD_MEANING_LEAD).then(|| labelled[top.1].0)
    }

    /// Elisa's answer from an earlier exchange of this conversation to a question it asks again, on a turn with no
    /// tools (`earlier_answer_of`): the history before this turn and the notes' digest of folded turns, so a resumed
    /// or folded chat still has what was found then. None otherwise.
    pub(super) fn earlier_answer(&self, ask: &str) -> Option<String> {
        if !self.tool_lines.is_empty() { return None; }
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let mut pairs: Vec<(String, String)> = Vec::new();
        for m in &self.history[..cap] {
            if super::memory::is_request(m) {
                let q = context::request_body(&texts(m)).trim().to_string();
                if !q.is_empty() { pairs.push((q, String::new())); }
            } else if m.role == Role::Assistant {
                let t = context::split_state(&texts(m)).0.trim().to_string();
                if let (Some(p), false) = (pairs.last_mut(), t.is_empty()) { p.1 = t; }
            }
        }
        earlier_answer_of(ask, &exchanges(self.notes.as_deref().unwrap_or(""), &pairs), self.client.model())
    }

    /// Elisa's answer from a small file she holds (a `read` of it this session: its line-numbered text) when `vis`
    /// misses what the ask asks of it: the ask names the file, or says "that file" / asks what else there is
    /// (`reply_check::asks_what_else`) after a turn about one ("that file" is the file the reply or the question
    /// before named when one is named there, else the one whose labels the question before asked by name, else the
    /// one read now, else the last one read). The asked lines are
    /// those whose label shares a word with the ask (`reply_check::asked_lines`), else the line nearest in meaning
    /// (`line_by_meaning`), else the lines the reply itself speaks of by their label without their value
    /// (`reply_check::named_lines`: "…một mã c hộp và một code meeting."), else, where the ask is for the content
    /// (`reply_check::asks_content`: "what's in it?", "nội dung", not "did it ask you to do anything?" or a bare "read
    /// a.txt") or for a value out of the file read this turn (`reply_check::asks_value`: "đọc file notes.txt và cho
    /// mình biết mã cuộc họp"), the whole file; for a "what else" ask, the lines earlier replies did not give. A reply
    /// that misses them (`reply_check::gives_line`: "contains a meeting code and a project name", "MEETING001",
    /// garbled text; one line of a file of HELD_WHOLE_LINES lines or fewer told whole, "ORCHID-42" for "what's in
    /// notes.txt?") gets the lines themselves (`reply_check::answer_from`); nothing left to tell gets the
    /// `nothing_else` line. None: the reply gives them (one of them, to a value ask), the turn changed or ran things,
    /// counted something, a reply saying the file lacks a thing no line was asked for by name (it may be right), or
    /// "that file" naming one Elisa does not hold (never another file's lines in its place).
    pub(super) fn held_file_answer(&self, vis: &str, ask: &str) -> Option<String> {
        if self.doc_req.is_some() || self.change_tried() { return None; }
        let body = context::request_body(ask);
        if !crate::mcp_reason::count_units(body).is_empty() { return None; }
        let reads = self.session_reads();
        let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
        let named = crate::reply_check::named_file(body).map(|f| base(&f));
        let more = crate::reply_check::asks_what_else(ask);
        let cap = self.turn_cap(&self.history).min(self.history.len());
        let read_now = self.turn_tool_text().0.iter().rev().find_map(|(p, _)| p.as_deref().map(base));
        let prev_reply = self.history[..cap].iter().rev().find(|m| m.role == Role::Assistant).map(texts).unwrap_or_default();
        let prev_ask = self.history[..cap].iter().rev().find(|m| is_request(m)).map(|m| context::request_body(&texts(m)).to_string()).unwrap_or_default();
        // (named there, else the file read this session whose labels the question before asked by name: "which file in
        // this folder mentions a meeting code?" → the one with "Meeting code: …", whatever the reply to it said)
        let prev_about = || reads.iter().rev().find(|(_, c)| {
            let ls: Vec<String> = c.lines().filter_map(|l| l.split_once('\t').map(|(_, r)| r.trim().to_string())).collect();
            !crate::reply_check::asked_lines(&ls, &crate::reply_check::words_of(&prev_ask)).is_empty()
        }).map(|(n, _)| n.to_lowercase());
        let prev_named = crate::reply_check::named_file(&prev_reply).or_else(|| crate::reply_check::named_file(&prev_ask)).map(|f| base(&f)).or_else(prev_about);
        let target = match named {
            Some(f) => f,
            None if more || crate::reply_check::names_file_word(body) => match prev_named {
                Some(f) => f,
                // (the question before asked which file holds a thing no file read holds, "which file mentions a
                // meeting code?" with only todo.txt read: "that file" is one Elisa has not read, never the last read)
                None if prev_ask.trim_end().ends_with('?') && crate::reply_check::names_file_word(&prev_ask) && {
                    let asked: Vec<String> = crate::topics::words(&prev_ask).into_iter().filter(|w| !crate::reply_check::names_file_word(w)).collect();
                    !asked.is_empty() && !reads.iter().any(|(_, c)| { let have = crate::reply_check::words_of(c); asked.iter().any(|w| have.contains(w)) })
                } => return None,
                None => read_now.clone().or_else(|| more.then(|| reads.last().map(|(n, _)| n.to_lowercase())).flatten())?,
            },
            None => return None,
        };
        let (name, content) = reads.iter().rev().find(|(n, _)| n.to_lowercase() == target)?;
        // (a `read` result: every line numbered "N<tab>…"; an outline or a search result is not the file's text)
        let numbered = |l: &str| l.split_once('\t').filter(|(n, _)| !n.trim().is_empty() && n.trim().chars().all(|c| c.is_ascii_digit())).map(|(_, rest)| rest.trim().to_string());
        let body_lines: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("<untrusted") && !l.starts_with("</untrusted") && !l.starts_with("[elisa:")).collect();
        let lines: Vec<String> = body_lines.iter().filter_map(|l| numbered(l)).filter(|l| !l.is_empty()).collect();
        if body_lines.iter().any(|l| numbered(l).is_none()) || lines.is_empty() || lines.len() > HELD_LINES_MAX || lines.iter().any(|l| l.chars().count() > HELD_LINE_CHARS) { return None; }
        let own: std::collections::HashSet<String> = crate::reply_check::words_of(name);
        let asked: std::collections::HashSet<String> = crate::reply_check::words_of(body).into_iter().filter(|w| !own.contains(w)).collect();
        let by_words: Vec<&String> = crate::reply_check::asked_lines(&lines, &asked);
        // (never by meaning for a "what else" ask: it names no line, so the line nearest in meaning to "what else is in
        // it?" is no line asked; the lines not yet told are)
        // (nor for an ask for the file's content that asks no value and names no line, "hmm I wonder what's in
        // notes.txt": the whole file is asked, never the line nearest in meaning to its words or the one the reply
        // names)
        let content_only = crate::reply_check::asks_content(ask) && !crate::reply_check::asks_value(ask);
        let by_meaning = if by_words.is_empty() && !more && !content_only { self.line_by_meaning(body, &lines) } else { None };
        // (a value asked in another language than the file's labelled lines, where meaning finds none (all-MiniLM is
        // English-only): the line the user's own model numbers, `line_by_model`; no valid number and a file of a few
        // lines, all of them. Never on an ask in the file's own language: words and meaning judge that one, and a
        // reply that gives a line stands without another request)
        let labelled = lines.iter().filter(|l| l.split_once([':', '=']).is_some_and(|(a, _)| !a.trim().is_empty())).count();
        let cross = by_words.is_empty() && by_meaning.is_none() && !more && labelled >= 2 && crate::reply_check::asks_value(ask)
            && crate::reply_check::lang_mismatch(body, &lines.join("\n"));
        let by_meaning = if cross { self.line_by_model(body, &lines).map(|i| &lines[i]) } else { by_meaning };
        let all_few = cross && by_meaning.is_none() && lines.len() <= crate::reply_check::held_whole_lines();
        // (a reply that gives none of the file's lines and speaks of one by its label is about that line; not a reply
        // that denies a thing, which may be right)
        let none_given = lines.iter().all(|l| !crate::reply_check::gives_line(vis, l));
        // (never for a "what else" ask or a content ask: the reply naming a line is no ask for it)
        let by_reply: Vec<&String> = if by_words.is_empty() && by_meaning.is_none() && !more && !content_only && none_given && !crate::reply_check::says_not(vis) {
            crate::reply_check::named_lines(&lines, vis) } else { Vec::new() };
        let specific = !by_words.is_empty() || by_meaning.is_some() || !by_reply.is_empty();
        // (a value asked of the file read this turn on that ask: a reply giving none of its lines missed it)
        let value = read_now.as_deref() == Some(target.as_str()) && crate::reply_check::asks_value(ask);
        let whole = more || crate::reply_check::asks_content(ask) || (value && none_given) || all_few;
        // (no line asked for by name: the whole file is the answer only to an ask for its content, for a value out of
        // it, or for what else it holds)
        if !specific && !whole { return None; }
        // (no line found for a value asked across languages: every line of the few, never a line guessed from the
        // reply's words)
        let picked: Vec<&String> = if !by_words.is_empty() { by_words } else if let Some(l) = by_meaning { vec![l] }
            else if !by_reply.is_empty() && !all_few { by_reply } else { lines.iter().collect() };
        // (for a "what else" ask, what was said is what every reply of the conversation shown so far gave, Elisa's own
        // short follow-up answers among them ("and the project?" → "Project: …."), however many exchanges ago: a line
        // told is never given again as more; the history holds each reply as it was finally shown)
        let said: String = self.history[..cap].iter().filter(|m| m.role == Role::Assistant).map(texts).collect::<Vec<_>>().join("\n");
        // (a line a reply since named by its label, "The file notes.txt mentions a meeting code.", was told too, so
        // "what else" asks for the others; when no other is left, the named ones are still untold)
        let remaining: Vec<&String> = if more {
            let untold: Vec<&String> = picked.into_iter().filter(|l| !crate::reply_check::gives_line(&said, l)).collect();
            let named = crate::reply_check::named_lines(&lines, &said);
            let unnamed: Vec<&String> = untold.iter().copied().filter(|l| !named.contains(l)).collect();
            if unnamed.is_empty() { untold } else { unnamed }
        } else { picked };
        // (every line told already: Elisa says so, in the ask's language, with the lines told — `all_told`, never one
        // of them given again as if it were more)
        if remaining.is_empty() {
            return more.then(|| crate::reply_check::own_line_of("all_told", ask, name).replace("{lines}", &lines.join("; ")));
        }
        let given = remaining.iter().filter(|l| crate::reply_check::gives_line(vis, l)).count();
        // (lines asked for by name, and all of a file this short, must each be given; of a longer file told whole, one)
        let each = (specific && !more) || remaining.len() <= HELD_WHOLE_LINES;
        // (the ask said back round the value, "Check giúp mình cái file notes.txt,-meeting code: ORCHID-42.": every word
        // of the reply the ask's or the lines', most of the ask's in it; the file's line, in Elisa's words, instead)
        // (or a line asked by name given among things the file never says, "What needs to be done next? - Confirm the
        // meeting code ORCHID-42 … Next step: Confirm meeting details with the team.": the line itself, plainly)
        let invented = specific && !more && crate::reply_check::asks_value(ask) && invented_beside(vis, ask, &lines);
        let echoed = invented || crate::reply_check::says_ask_round_value(vis, ask, &remaining);
        if echoed { return Some(held_value_answer(ask, name, &lines, &remaining)).filter(|t| !t.is_empty() && t.trim() != vis.trim()); }
        // (what else there is, answered with the lines not yet told and again those told, "The file contains the
        // meeting code ORCHID-42 and … the Lotus launch on Friday." after the meeting code was told: the untold ones)
        if more && given == remaining.len() && lines.iter().any(|l| !remaining.contains(&l) && crate::reply_check::gives_line(&said, l) && crate::reply_check::gives_line(vis, l)) {
            return Some(held_value_answer(ask, name, &lines, &remaining)).filter(|t| !t.is_empty() && t.trim() != vis.trim());
        }
        // (a line asked by name answered with a list of the file's lines, "File notes.txt có nội dung: 1 Meeting code:
        // ORCHID-42 2 Project: …", "region,units\nNorth,120": the asked line or row in a sentence, `held_value_answer`)
        if specific && !more && given >= 1 && lists_lines(vis, ask, name, &lines, &remaining) {
            return Some(held_value_answer(ask, name, &lines, &remaining)).filter(|t| !t.is_empty() && t.trim() != vis.trim());
        }
        if given == remaining.len() || (!each && given >= 1) { return None; }
        // (a reply saying the file lacks a thing is left be when it speaks of the file or the ask, "todo.txt không có
        // gì cả"; a "không" in words about neither, "Tudo ngon không?", denies nothing of it)
        let about = { let have = crate::topics::words(vis); crate::topics::words(body).iter().chain(crate::topics::words(name).iter()).any(|w| have.contains(w)) };
        if !specific && !more && about && crate::reply_check::says_not(vis) { return None; }
        // (a sheet's rows in sentences, "North: 120 units.", never a fragment of its cells, "North,120")
        Some(held_value_answer(ask, name, &lines, &remaining)).filter(|t| !t.is_empty())
    }
}

/// Whether `reply` says a status (`data/status_words.txt`: "done", "sent", "đã gửi") that `held`, the text of the
/// files it speaks of, does not hold: a status the files do not give ("Todo.txt bây giờ có những việc đã đọc và gửi
/// đi" for "- buy bread" / "- send report").
pub(super) fn status_unheld(reply: &str, held: &str) -> bool {
    static W: std::sync::OnceLock<Vec<Vec<String>>> = std::sync::OnceLock::new();
    let split = |t: &str| t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<String>>();
    let words = W.get_or_init(|| include_str!("../../data/status_words.txt").lines().map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('[')).map(&split).collect());
    let (r, h) = (split(reply), split(held));
    let has = |t: &[String], p: &[String]| t.windows(p.len()).any(|x| x == p);
    words.iter().any(|p| !p.is_empty() && has(&r, p) && !has(&h, p))
}

/// content words (`topics::words`) an ask must share with an earlier one for that one's answer to stand for it
/// (`earlier_answer`)
const EARLIER_ASK_SHARED: usize = 2;

/// The exchanges of the conversation before this turn, oldest first: (the user's message, Elisa's reply to it), from
/// the notes' digest of folded turns (`context::digest`: its "User: …" and "Elisa: …" lines) and then the history.
pub(super) fn exchanges(notes: &str, history: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut asked: Option<String> = None;
    for l in notes.lines().map(|l| l.trim().trim_start_matches("- ")) {
        if let Some(u) = l.strip_prefix(context::USER_LABEL) { asked = Some(u.trim().to_string()); }
        else if let Some(e) = l.strip_prefix(context::ELISA_LABEL) { if let Some(u) = asked.take() { out.push((u, e.trim().to_string())); } }
    }
    out.extend(history.iter().cloned());
    out
}

/// The answer an earlier exchange gave (`exchanges`) to the question `ask` asks again: the newest one whose message
/// holds every content word of the question (`topics::words`, the back-pointing words of `data/recall_words.txt`
/// aside), EARLIER_ASK_SHARED of them at least, and every name, path, number or code the question gives ("what was
/// the meeting code you found?" after "read notes.txt and tell me the meeting code" answered "ORCHID-42"), its reply a
/// real one, never one of Elisa's own lines (`from_own_lines`: no answer, not written, not read), in her line saying
/// when it was given (`earlier_answer` in `data/no_answer_lines.txt`).
pub(super) fn earlier_answer_of(ask: &str, past: &[(String, String)], model: &str) -> Option<String> {
    static R: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    let recall = R.get_or_init(|| include_str!("../../data/recall_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect());
    let body = context::request_body(ask);
    let want: HashSet<String> = crate::topics::words(body).into_iter().filter(|w| !recall.contains(w)).collect();
    if want.len() < EARLIER_ASK_SHARED { return None; }
    // (a name, path, number or code of the question: a token with a digit, a dot inside, a slash or a capital
    // after its first letter, "/tmp/x.txt", "ORCHID-42", "app.py")
    let names: Vec<String> = body.split_whitespace().map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.').trim_end_matches('.'))
        .filter(|t| t.chars().any(|c| c.is_ascii_digit()) || t.contains('/') || t.trim_matches('.').contains('.') || t.chars().skip(1).any(char::is_uppercase))
        .map(str::to_lowercase).collect();
    let (q, a) = past.iter().rev().find(|(q, a)| {
        let a = a.trim();
        let low = q.to_lowercase();
        a.chars().any(char::is_alphanumeric) && q.trim() != body.trim()
            && a != crate::reply_check::cant_answer_line(q, model) && a != crate::reply_check::no_answer_line(q) && !from_own_lines(a)
            && want.is_subset(&crate::topics::words(q)) && names.iter().all(|n| low.contains(n.as_str()))
    })?;
    Some(crate::reply_check::own_line_of("earlier_answer", ask, "").replace("{ask}", &context::one_line(q, 120)).replace("{reply}", &context::one_line(a, 300)))
}

/// The claims (`reply_check::tool_claims` of `kinds`) made in one clause of a sentence of `reply` (its parts between
/// commas, colons, semicolons and dashes), where the whole sentence makes none, as it also denies ("I created a file
/// named .elisa/u06.txt, but I can't write anything into it"): (kind, the whole sentence, the clause, the file named).
pub(super) fn clause_claims(reply: &str, kinds: &[&str], about_file: bool) -> Vec<(String, String, String, Option<String>)> {
    let mut out = Vec::new();
    for s in super::turn::sentence_pieces(reply).into_iter().map(str::trim).filter(|s| !s.is_empty()) {
        if !crate::reply_check::tool_claims(s, kinds, about_file).is_empty() { continue; }
        for c in s.split([',', ':', ';', '—']).map(str::trim).filter(|c| !c.is_empty()) {
            if let Some((k, _, f)) = crate::reply_check::tool_claims(c, kinds, about_file).into_iter().next() {
                out.push((k, s.to_string(), c.to_string(), f));
                break;
            }
        }
    }
    out
}

/// shortest fixed start of one of Elisa's own lines (`from_own_lines`) that marks a reply as hers
const OWN_LINE_START_CHARS: usize = 12;

/// Whether `reply` holds one of Elisa's own lines of `data/no_answer_lines.txt` (no answer, not written, not read, not
/// run…): the fixed words of a line up to its first placeholder, OWN_LINE_START_CHARS characters or more.
fn from_own_lines(reply: &str) -> bool {
    include_str!("../../data/no_answer_lines.txt").lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| l.split_once(':').map(|(_, v)| v.trim()))
        .map(|v| v.split('{').next().unwrap_or("").trim())
        .any(|start| start.chars().count() >= OWN_LINE_START_CHARS && reply.contains(start))
}

/// fewest words a reply has to be judged a saying-back of an earlier message (`says_earlier_message`), and the share
/// of them, in quarters, that must be that message's
const EARLIER_SAID_MIN_WORDS: usize = 4;
const EARLIER_SAID_QUARTERS: usize = 3;

/// Whether `reply` is mostly one of the user's earlier messages (`earlier`) said back, in any order: EARLIER_SAID_MIN_WORDS
/// words or more, three quarters of them or more that message's ("xem hàm add trong app.py có lỗi hay không." after
/// "file app.py có lỗi gì không? xem hàm add").
pub(super) fn says_earlier_message(reply: &str, earlier: &[String]) -> bool {
    let r: Vec<String> = reply.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    if r.len() < EARLIER_SAID_MIN_WORDS { return false; }
    earlier.iter().any(|m| {
        let have = crate::reply_check::words_of(context::request_body(m));
        r.iter().filter(|w| have.contains(*w)).count() * 4 >= r.len() * EARLIER_SAID_QUARTERS
    })
}

/// shortest code (`code_tokens`) judged for a near miss
const CODE_MIN_CHARS: usize = 4;

/// The codes of a text: its words of CODE_MIN_CHARS characters or more with both a letter and a digit ("ORCHID-42",
/// "A7X9", "v2.3.1"), the marks round them left off.
fn code_tokens(t: &str) -> Vec<String> {
    t.split(|c: char| c.is_whitespace() || ",;:()[]{}<>\"'`*|".contains(c)).map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.chars().count() >= CODE_MIN_CHARS && w.chars().any(|c| c.is_alphabetic()) && w.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_string).collect()
}

/// Whether `a` and `b` differ by one character added, dropped or changed (case aside).
fn one_off(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    if a == b || a.len().abs_diff(b.len()) > 1 { return false; }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] { i += 1; j += 1; continue; }
        edits += 1;
        if edits > 1 { return false; }
        match a.len().cmp(&b.len()) { std::cmp::Ordering::Greater => i += 1, std::cmp::Ordering::Less => j += 1, std::cmp::Ordering::Equal => { i += 1; j += 1; } }
    }
    edits + (a.len() - i) + (b.len() - j) == 1
}

/// `reply` with each code (`code_tokens`) that `held` (what the results returned, and the user's words) does not hold
/// but holds one a character off ("ORCHID-422" beside "ORCHID-42") put right, as held. None: no such code.
pub(super) fn near_miss_codes_fixed(reply: &str, held: &str) -> Option<String> {
    let have = code_tokens(held);
    let known = |w: &str| have.iter().any(|h| h.eq_ignore_ascii_case(w));
    let mut out = reply.to_string();
    let mut fixed = false;
    for w in code_tokens(reply) {
        if known(&w) { continue; }
        let near: Vec<&String> = have.iter().filter(|h| one_off(h, &w)).collect();
        let Some(h) = near.first().filter(|_| near.iter().all(|x| x.eq_ignore_ascii_case(near[0]))) else { continue };
        out = out.replace(&w, h);
        fixed = true;
    }
    fixed.then_some(out)
}

/// Whether sentence `s` quotes a piece of code or text (between backticks or double quotes) that the file's read text
/// `held` (its numbered lines) does not hold, spaces aside: text it says was written in, not text the file has.
pub(super) fn quotes_unheld(s: &str, held: &str) -> bool {
    let flat = |t: &str| t.split_whitespace().collect::<String>();
    let have = flat(&held.lines().map(|l| l.split_once('\t').filter(|(n, _)| n.trim().chars().all(|c| c.is_ascii_digit())).map_or(l, |(_, r)| r)).collect::<Vec<_>>().join("\n"));
    ['`', '"'].iter().any(|q| s.split(*q).skip(1).step_by(2).map(flat).any(|p| !p.is_empty() && !have.contains(&p)))
}

/// words a reply that lists a held file's lines has at most besides them, the file's name and the ask's words: its
/// lead-in ("File notes.txt có nội dung:") and list marks (`lists_lines`)
const LIST_LEAD_WORDS: usize = 5;

/// Whether `reply`, to a question (`ask`) about held file `name` that asks for some of its `lines` (`asked`), only
/// lists the file's lines: one beside the asked ones copied whole, and no more than LIST_LEAD_WORDS other words
/// (numbers and list marks aside) once every copied line, the file's name and the ask's words are taken out.
pub(super) fn lists_lines(reply: &str, ask: &str, name: &str, lines: &[String], asked: &[&String]) -> bool {
    let flat = |t: &str| t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rest = flat(reply);
    let copied: Vec<&String> = lines.iter().filter(|l| { let f = flat(l); !f.is_empty() && rest.contains(&f) }).collect();
    if !copied.iter().any(|l| !asked.contains(l)) { return false; }
    for l in &copied { rest = rest.replace(&flat(l), " "); }
    let skip: std::collections::HashSet<String> = crate::reply_check::words_of(name).into_iter().chain(crate::reply_check::words_of(context::request_body(ask))).collect();
    let own = crate::reply_check::words_of(&rest).into_iter().filter(|w| !skip.contains(w) && !w.chars().all(|c| c.is_ascii_digit())).count();
    own <= LIST_LEAD_WORDS
}

/// The asked lines of held file `name` in a sentence: a sheet's rows by their label and their cells with the
/// header's names ("North: 120 units."), any other file's lines as `reply_check::answer_from` gives them
/// ("notes.txt says: Meeting code: ORCHID-42").
pub(super) fn held_value_answer(ask: &str, name: &str, lines: &[String], asked: &[&String]) -> String {
    if crate::mcp_docs::is_sheet(std::path::Path::new(name)) {
        let rows = super::turn::held_rows(name, &lines.join("\n"));
        let said: Vec<String> = asked.iter().filter_map(|l| {
            let key = l.split([',', '\t', ';']).next().unwrap_or("").trim();
            rows.iter().find(|(k, _)| k == key).map(|(k, rest)| if rest.is_empty() { format!("{k}.") } else { format!("{k}: {rest}.") })
        }).collect();
        if said.len() == asked.len() { return said.join("\n"); }
    }
    crate::reply_check::answer_from(ask, name, &asked.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"))
}

/// content words a reply's sentence needs to be judged by `invented_beside`
const INVENTED_MIN_WORDS: usize = 3;
/// sentences of a reply whose words are mostly the model's own that make it a dump beside the answer
const INVENTED_SENTENCES_MIN: usize = 2;

/// Whether `reply`, to `ask` about a held file's `lines`, says things the file never does beside its answer: two
/// sentences or list items or more of INVENTED_MIN_WORDS content words or more (`topics::words`) of which fewer than
/// half are the file's or the ask's ("Next step: Confirm meeting details with the team.", "Ensure any necessary
/// documents are ready."). A reply that only words the file's lines in its own way has few such sentences. Never for
/// a reply in another language than the file's (its words are not the file's anyway).
pub(super) fn invented_beside(reply: &str, ask: &str, lines: &[String]) -> bool {
    if crate::reply_check::lang_mismatch(reply, &lines.join("\n")) { return false; }
    let known: std::collections::HashSet<String> = lines.iter().flat_map(|l| crate::topics::words(l)).chain(crate::topics::words(ask)).collect();
    reply.split(['.', '?', '!', '\n']).filter(|s| {
        let w = crate::topics::words(s);
        w.len() >= INVENTED_MIN_WORDS && w.iter().filter(|x| known.contains(*x)).count() * 2 < w.len()
    }).count() >= INVENTED_SENTENCES_MIN
}

/// The held line `ask_model` numbers for `body` both with `lines` in the file's order and reversed
/// (`reply_check::pick_held_line`): its index when the two name the same line, else None (`line_by_model`).
pub(super) fn pick_in_both_orders(body: &str, lines: &[String], ask_model: impl Fn(&str) -> Option<String>) -> Option<usize> {
    let first = crate::reply_check::pick_held_line(body, lines, &ask_model)?;
    let reversed: Vec<String> = lines.iter().rev().cloned().collect();
    let second = crate::reply_check::pick_held_line(body, &reversed, &ask_model)?;
    (lines.len() - 1 - second == first).then_some(first)
}

/// The one held line whose label holds every content word (`topics::words`) of `restated` (the thing an ask asks for,
/// said in the file's language by the user's model: "meeting code" → "Meeting code: ORCHID-42"): its index. None
/// when no line or more than one has them, or `restated` is longer than HELD_RESTATE_WORDS words (no such phrase).
pub(super) fn pick_by_restated(restated: &str, lines: &[String]) -> Option<usize> {
    let want = crate::topics::words(restated);
    if want.is_empty() || crate::reply_check::words_of(restated).len() > HELD_RESTATE_WORDS { return None; }
    let hits: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.split_once([':', '=']).is_some_and(|(label, _)| {
        let have = crate::topics::words(label);
        want.iter().all(|w| have.contains(w))
    })).map(|(i, _)| i).collect();
    match hits.as_slice() { [i] => Some(*i), _ => None }
}

/// bytes a project file may have at most for Elisa to count its lines, words or characters herself (`count_fix`)
const COUNT_FILE_MAX_BYTES: u64 = 1 << 20;
/// lines a held file may have, and chars a line of it, for Elisa to answer from it line by line (`held_file_answer`)
const HELD_LINES_MAX: usize = 12;
const HELD_LINE_CHARS: usize = 160;
/// lines a held file has at most to be told whole: an answer about it that misses one of them gets the file's own
/// words ("ORCHID-42" alone for "what's in notes.txt?" with two lines in it)
const HELD_WHOLE_LINES: usize = 4;
/// the similarity (all-MiniLM-L6-v2) from which a held file's label is what an ask in other words asks for, and how
/// much nearer than the next label it must be (`line_by_meaning`)
const HELD_MEANING_MIN: f32 = 0.3;
/// the system line and output budget of Elisa's question for the held line's number (`line_by_model`)
const HELD_PICK_SYSTEM: &str = "You pick the line that answers a question. Reply with its number only.";
const HELD_PICK_TOKENS: usize = 16;
/// the system line of Elisa's question for the thing an ask asks for, said in the language of the file's labels
/// (`line_by_model`), and the most words that answer may have
const HELD_RESTATE_SYSTEM: &str = "Say what thing the question asks for, in at most five words, in the language of the labels given. Reply with those words only.";
const HELD_RESTATE_WORDS: usize = 5;
const HELD_MEANING_LEAD: f32 = 0.08;

/// a message of at most this many words after documents were attached may be a follow-up about them (`asks_about_docs`)
const DOC_ASK_SHORT_WORDS: usize = 12;
/// words (4 letters or more) a longer message must share with the documents to be about them (`asks_about_docs`)
const DOC_ASK_SHARED_WORDS: usize = 2;
