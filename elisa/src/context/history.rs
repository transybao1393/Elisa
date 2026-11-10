use super::*;

fn is_user_text(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }))
}

/// a user message that is a request (the user's words, not tool results or Elisa's own correction note)
fn is_request_msg(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.starts_with(NUDGE_PREFIX)))
}

/// `msgs[..end]` with every attached document's text cut to its reference line (`[Attached: report.pdf, 12 pages ·
/// …]`): the first room-maker when a request is over the window. A later question about the document
/// gets its parts again (`attach::follow_up`). None when there is nothing to cut.
pub fn drop_doc_text(msgs: &[Message], end: usize) -> Option<Vec<Message>> {
    let mut out = msgs.to_vec();
    let mut changed = false;
    for m in out[..end.min(msgs.len())].iter_mut().filter(|m| m.role == Role::User) {
        for b in &mut m.content {
            if let Block::Text { text } = b {
                if crate::attach::is_block(text) && text.trim_end().contains('\n') {
                    *text = crate::attach::first_line(text).to_string();
                    changed = true;
                }
            }
        }
    }
    changed.then_some(out)
}

/// `msgs` without its oldest whole turns (a turn: a request and everything up to the next one), dropped until at
/// least `free` tokens are gone, never the turn at `end` (the current request) or after it: the second room-maker
/// (so a small window goes on by itself instead of stopping with "say continue"). The first message's notes / `[state]` / recalled
/// lines (the part before `## Request`) move to the new first message, so the notes stay in the request. None when
/// nothing can go.
pub fn drop_old_turns(msgs: &[Message], end: usize, free: usize) -> Option<Vec<Message>> {
    let end = end.min(msgs.len());
    let starts: Vec<usize> = (0..end).filter(|&i| is_request_msg(&msgs[i])).chain([end]).collect();
    let mut cut = 0;
    for &s in starts.iter().skip(1) {
        cut = s;
        if estimate_messages(&msgs[..cut]) >= free { break; }
    }
    if cut == 0 { return None; }
    let head = msgs.first().and_then(|m| m.content.iter().find_map(|b| match b { Block::Text { text } => text.rsplit_once("## Request\n").map(|(h, _)| h.to_string()), _ => None }));
    let mut out = msgs[cut..].to_vec();
    if let (Some(h), Some(Block::Text { text })) = (head, out.first_mut().and_then(|m| m.content.first_mut())) {
        if !text.contains("## Request\n") { *text = format!("{h}## Request\n{text}"); }
    }
    Some(out)
}

/// the start of a picture's text block in a user message: what the model saw when asked about the picture alone
/// (`picture_request`); the turn itself then runs as text, and the picture is never sent again with the history
pub const PICTURE_MARK: &str = "[picture: ";
/// the question asked of a picture sent with no words
pub const PICTURE_QUESTION: &str = "Describe this picture.";
pub const PICTURES_QUESTION: &str = "Describe these pictures.";

/// the picture text when no answer told anything about the picture (`picture_non_answer`, asked twice)
pub const PICTURE_UNSEEN: &str = "the model could not make out the picture";

/// A picture's text block for the turn's user message: `[picture: ANSWER]`.
pub fn picture_block(answer: &str) -> String { format!("{PICTURE_MARK}{}]", answer.trim()) }

/// a user message block that is a picture's text (`picture_block`), not the user's words
pub fn is_picture_block(text: &str) -> bool { text.starts_with(PICTURE_MARK) }

/// A picture answer that tells nothing about the picture : a refusal (`unfit_memory`: "I'm sorry, but
/// I can't assist with that.") or a
/// non-answer ("I'm sorry, I'm not sure what you're asking", "I didn't understand your question"). Carried into the
/// turn, the main model would take it as the answer and repeat it on later turns.
pub fn picture_non_answer(answer: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)not sure what you(?:'re| are) asking|(?:didn't|did not|don't|do not) understand (?:your|the) question|\bunanswerable\b").unwrap());
    let t = answer.trim().replace('’', "'");
    // a refusal anywhere, not only first (a small model asked about a picture can describe it first and refuse
    // at the end)
    t.is_empty() || re.is_match(&t) || t.split_inclusive(['.', '!', '?', '\n']).any(|s| unfit_memory(s.trim()))
}

/// A reply without any `[picture: …]` text a model copied from the turn's message (a small model may begin
/// replies with "[picture: …"): each such span removed through its `]` (an unclosed
/// one through the end of its line). Picture text is Elisa's request-only context, never reply text.
pub fn strip_picture_echo(reply: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?im)\[picture:[^\]\n]*(?:\]|$)").unwrap());
    if !re.is_match(reply) { return reply.to_string(); }
    let out = re.replace_all(reply, "");
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let mut s = lines.join("\n");
    while s.contains("\n\n\n") { s = s.replace("\n\n\n", "\n\n"); }
    s.trim().to_string()
}

/// The history without the picture text of earlier turns: a picture's text belongs to the turn that looked
/// at it (`picture_request`); later turns have the reply that answered it. Run before a new turn's message is added,
/// never within a turn (the prefix rule). Whether anything was removed.
pub fn drop_picture_blocks(history: &mut [Message]) -> bool {
    let mut any = false;
    for m in history.iter_mut().filter(|m| m.role == Role::User) {
        // never the first block: that is the user's own words, even when they look like picture text
        let n = m.content.len();
        let mut i = 0;
        m.content.retain(|b| { i += 1; i == 1 || !matches!(b, Block::Text { text } if is_picture_block(text)) });
        any |= m.content.len() != n;
    }
    any
}

/// the MiniLM similarity at which a message is about what a picture showed (measured with all-MiniLM-L6-v2 against
/// two answers about a cat picture and one about a sales chart: asks about the picture 0.29–0.55, unrelated asks
/// ("Tell me a joke.", "thanks!", "What is the capital of France?") -0.10–0.19)
pub const PICTURE_MEANING_MIN: f32 = 0.25;

/// The MiniLM cosine similarity of two texts (`elisa_mem::embed`, local, no network); None without the model.
pub fn meaning_similarity(a: &str, b: &str) -> Option<f32> {
    let dir = elisa_mem::embed::model_dir()?;
    let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
    let v = elisa_mem::embed::embed(&dir, &vocab, &[a, b], &mut |_| {}).ok()?;
    let (x, y) = (v.first()?, v.get(1)?);
    Some(x.iter().zip(y.iter()).map(|(p, q)| p * q).sum())
}

/// Whether `msg`, the turn right after a picture turn, is about that picture (otherwise the picture is resent and
/// its answer goes into every next turn, so an unrelated ask like "Tell me a joke." gets the picture answered).
/// `seen`: what the picture turn learned (the picture's answer
/// and the ask). About it when the message names a picture or points back at something ("its eyes", "that one":
/// `picture_words`), is a short follow-up of the last answer (`followup::kind`), shares a content word with `seen`
/// (`topics::words`), or is close to it in meaning (`meaning_similarity`, when the embedding model is present).
pub fn about_picture(msg: &str, seen: &str) -> bool {
    let msg = request_body(msg).trim();
    if msg.is_empty() { return false; }
    picture_words(msg, true) || crate::followup::kind(msg).is_some() || shares_meaning(msg, seen)
}

/// Whether two texts share a content word (`topics::words`) or are close in meaning (`meaning_similarity` at
/// `PICTURE_MEANING_MIN`, when the embedding model is present).
pub fn shares_meaning(a: &str, b: &str) -> bool {
    if a.trim().is_empty() || b.trim().is_empty() { return false; }
    !crate::topics::words(a).is_disjoint(&crate::topics::words(b)) || meaning_similarity(a, b).is_some_and(|s| s >= PICTURE_MEANING_MIN)
}

/// A reply on a turn with picture text that has nothing to do with what the picture showed (`pic`: the picture's
/// answer), such as a small model carrying on from its own earlier replies instead of answering about the picture.
pub fn off_picture(reply: &str, pic: &str) -> bool { !reply.trim().is_empty() && !shares_meaning(reply, pic) }

/// a reply's text for comparing: lowercase, whitespace collapsed
fn norm_reply(t: &str) -> String { t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase() }

/// the chars two replies share from their start before they differ, at which a reply is a copy of the other
const COPY_PREFIX_CHARS: usize = 60;
/// a reply shorter than this is never called a copy ("Yes.", "Paris", a name): short answers repeat for real
const COPY_MIN_CHARS: usize = 12;

fn word_grams(t: &str) -> std::collections::HashSet<String> {
    let w: Vec<String> = t.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_string).collect();
    w.windows(4).map(|g| g.join(" ")).collect()
}

/// What a reply to one request must not be (small models in a small window repeat themselves): a copy of an
/// earlier reply to another ask, a reply with no word while the ask has words and no number (such as a box
/// "[0.0, 0.17, 0.99, 0.29]" from a vision model's detection mode), or the wording of a heading or tag Elisa put
/// in the request ("[Elisa, not the user]" from "…recalled by Elisa…, not written by the
/// user…"). Built from the request that was sent, so it holds for any heading Elisa adds; no reply wording is listed.
#[derive(Debug, Default)]
pub struct ReplyGuard {
    /// the user's words of this turn
    ask: String,
    /// the earlier replies (normalized) that answered another ask: an earlier turn with the same ask may get the
    /// same reply
    sources: Vec<String>,
    /// four-word runs of the request's heading and tag lines (a user-message line starting with "## " or "[",
    /// picture text excepted), less those in the user's own words
    grams: std::collections::HashSet<String>,
}

impl ReplyGuard {
    pub fn new(ask: &str, msgs: &[Message]) -> ReplyGuard {
        let ask = request_body(ask).trim().to_string();
        let a = norm_reply(&ask);
        let own = word_grams(&ask);
        let mut g = ReplyGuard { ask, ..Default::default() };
        let mut last_ask = String::new();
        for m in msgs {
            let blocks = || m.content.iter().filter_map(|b| match b { Block::Text { text } if !is_picture_block(text) => Some(text.as_str()), _ => None });
            if m.role == Role::User {
                for l in blocks().flat_map(str::lines).map(str::trim).filter(|l| l.starts_with("## ") || l.starts_with('[')) { g.grams.extend(word_grams(l)); }
                last_ask = norm_reply(request_body(&blocks().collect::<Vec<_>>().join("\n")));
                continue;
            }
            // (a first message ends with the user's words after Elisa's blocks)
            if last_ask == a || last_ask.ends_with(&format!(" {a}")) { continue; }
            let e = norm_reply(&blocks().collect::<Vec<_>>().join("\n"));
            if !e.is_empty() { g.sources.push(e); }
        }
        g.grams.retain(|x| !own.contains(x));
        g
    }

    /// Why `reply` cannot stand ("copy", "no words", "heading"), or None.
    pub fn broken(&self, reply: &str) -> Option<&'static str> {
        let r = norm_reply(reply);
        if r.is_empty() { return None; }
        if r.chars().count() >= COPY_MIN_CHARS && self.sources.iter().any(|e| *e == r || r.chars().zip(e.chars()).take_while(|(a, b)| a == b).count() >= COPY_PREFIX_CHARS) {
            return Some("copy");
        }
        // a word: two letters in a row ("Â©" from moondream:1.8b has none; "Paris", "OK", "4 giờ" do)
        let has_word = |t: &str| t.chars().collect::<Vec<_>>().windows(2).any(|w| w[0].is_alphabetic() && w[1].is_alphabetic());
        let a = &self.ask;
        if !has_word(&r) && has_word(a) && !a.chars().any(|c| c.is_ascii_digit()) { return Some("no words"); }
        if word_grams(reply).iter().any(|x| self.grams.contains(x)) { return Some("heading"); }
        None
    }
}


/// what an image part becomes in a request to a model without vision (`without_images`)
pub const IMAGE_STUB: &str = "[a picture was shared here; this model can't see pictures]";

/// The messages with every image part replaced by `IMAGE_STUB`, for a model without vision (a session saved
/// by an older Elisa may still hold image parts, and a model without vision
/// rejects every request that has one). None when there is none. The
/// same messages always give the same request, so the prefix stays stable.
pub fn without_images(msgs: &[Message]) -> Option<Vec<Message>> {
    if !msgs.iter().any(|m| m.content.iter().any(|b| matches!(b, Block::Image { .. }))) { return None; }
    Some(msgs.iter().map(|m| {
        let mut m = m.clone();
        for b in m.content.iter_mut() {
            if matches!(b, Block::Image { .. }) { *b = Block::Text { text: IMAGE_STUB.into() }; }
        }
        m
    }).collect())
}

/// The line after a resumed conversation's restored turns (after a model switch, the capabilities and
/// "resumed N messages" drawn above the history leave only the model chip showing the change): which model now
/// answers and what it can do, e.g. "Now on granite4.2:3b · no vision · tools · thinking".
pub fn now_on(model: &str, vision: bool, native_tools: bool, thinking: bool) -> String {
    let mut s = format!("Now on {model} · {}", if vision { "vision" } else { "no vision" });
    s.push_str(if native_tools { " · tools" } else { " · tools via the prompt" });
    if thinking { s.push_str(" · thinking"); }
    s
}

/// The isolated picture request: no system prompt, no history, the pictures alone in one message and
/// then the question (the user's words, or `PICTURE_QUESTION` when there are none). Some small vision models refuse
/// or call the image corrupted with any system prompt, with earlier history, or with the question in the pictures'
/// own message, and read the picture with this shape. The question ends with a line break: without one, a model
/// may end its answer at once (empty reply) on a question ending in "?".
pub fn picture_request(question: &str, pics: &[Block]) -> Vec<Message> {
    let q = question.trim();
    let q = if !q.is_empty() { q } else if pics.len() > 1 { PICTURES_QUESTION } else { PICTURE_QUESTION };
    vec![Message::new(Role::User, pics.to_vec()), Message::user_text(format!("{q}\n"))]
}

fn trim_result(content: &str) -> String {
    let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    format!("[trimmed: {} … {} lines]", head(first, 80), content.lines().count())
}

/// Whether a fold's one-line shrink would really make this result shorter: "False\n[exit 0]" (14 chars) would become
/// "[trimmed: False … 2 lines]" (25), a longer rewrite that still costs a full re-prefill.
fn shrinkable(content: &str) -> bool {
    !content.starts_with("[trimmed:") && trim_result(content).chars().count() < content.chars().count()
}

fn trim_strings(v: &mut Value) {
    match v {
        Value::String(s) if s.chars().count() > 300 => *s = format!("[trimmed {} chars]", s.chars().count()),
        Value::Array(a) => a.iter_mut().for_each(trim_strings),
        Value::Object(o) => o.values_mut().for_each(trim_strings),
        _ => {}
    }
}

/// a capped tool result keeps at least this many chars (~300 tokens: its head, its tail and the marker); the session
/// folds older turns rather than cut below it, and says so plainly when even that does not fit (`not_shown`).
/// Results cut to almost nothing get re-requested or answered with invented numbers.
pub const RESULT_MIN_CHARS: usize = 1000;

/// Follows the marker of a cut `read`: the cut took its `next: offset=N` footer, and more reads would only be cut too
/// (otherwise the model reads a few lines at a time, guessing offsets, until the context is full).
pub const READ_CUT: &str = "[the context window is too full to read more of this file; answer with what you have]";

/// One result cut to `keep` chars: its head (to a line end when one is near) + `[trimmed N chars]`; a cut `read`
/// (its last line is the `[lines a-b of n]` footer) says why instead of leaving the model to page on.
/// `msgs` with every secret in text, tool input and tool output redacted (for saved sessions: disk never holds one).
pub fn redact_messages(msgs: &[Message]) -> Vec<Message> {
    let mut out = msgs.to_vec();
    for m in &mut out {
        for b in &mut m.content {
            match b {
                Block::Text { text: s } | Block::ToolResult { content: s, .. } => {
                    let (r, n) = crate::secrets::redact(s);
                    if n > 0 { *s = r; }
                }
                Block::ToolUse { input, .. } => { crate::secrets::redact_json(input); }
                Block::Image { .. } => {}
            }
        }
    }
    out
}

fn cut_result(content: &str, keep: usize) -> String {
    // an untrusted fence keeps its closing tag (the cut is head-only)
    let close = crate::untrusted::CLOSE;
    if let Some(body) = content.strip_suffix(close) {
        if content.chars().count() > keep {
            return format!("{}\n{close}", cut_result(body.trim_end_matches('\n'), keep.saturating_sub(close.len() + 1).max(1)));
        }
    }
    let n = content.chars().count();
    if n <= keep { return content.to_string(); }
    let read = content.rsplit('\n').next().is_some_and(|l| l.starts_with("[lines "));
    let room = keep.saturating_sub(if read { 24 + READ_CUT.len() } else { 26 }).max(1);
    if read {
        // a read: its head and why (its `next: offset` footer would only lead to more cut reads)
        let mut h = head(content, room);
        if let Some(i) = h.rfind('\n').filter(|&i| i >= h.len() * 3 / 4) { h = &h[..i]; }
        let cut = n - h.chars().count();
        return format!("{h}\n[trimmed {cut} chars] {READ_CUT}");
    }
    // anything else: head and tail (a command's summary, a count or an error is at its end)
    let tail_room = room * 2 / 5;
    let mut h = head(content, room - tail_room);
    if let Some(i) = h.rfind('\n').filter(|&i| i >= h.len() * 3 / 4) { h = &h[..i]; }
    let mut t = tail(content, tail_room);
    if let Some(i) = t.find('\n').filter(|&i| i <= t.len() / 4) { t = &t[i + 1..]; }
    let cut = n - h.chars().count() - t.chars().count();
    if t.is_empty() { format!("{h}\n[trimmed {cut} chars]") } else { format!("{h}\n[trimmed {cut} chars]\n{t}") }
}

/// One step's tool results fitted into `room_tokens` (what the window has left before the output reserve): when
/// they are over, the largest are cut first, water-filling, so every result keeps the same share and small ones stay
/// whole (each keeps >= RESULT_MIN_CHARS). Otherwise a few parallel reads of big files can push the next request
/// over the limit. Returns how many results were cut.
pub fn cap_step_results(results: &mut [Block], room_tokens: usize) -> usize {
    let mut sizes: Vec<usize> = Vec::new();
    let mut overhead = 0;
    for b in results.iter() {
        if let Block::ToolResult { tool_use_id, content, .. } = b {
            sizes.push(content.chars().count());
            overhead += estimate_tokens(tool_use_id) + 8;
        }
    }
    if sizes.is_empty() { return 0; }
    // 10% under the estimate: JSON and code tokenize denser than prose
    let budget = chars_for_tokens(room_tokens.saturating_sub(overhead)) * 9 / 10;
    if sizes.iter().sum::<usize>() <= budget { return 0; }
    // the largest share c with sum(min(len, c)) <= budget
    let mut sorted = sizes.clone();
    sorted.sort_unstable();
    let (mut left, mut c) = (budget, 0);
    for (i, &len) in sorted.iter().enumerate() {
        let rest = sorted.len() - i;
        if len * rest <= left { left -= len; c = len; } else { c = left / rest; break; }
    }
    let c = c.max(RESULT_MIN_CHARS);
    let mut cut = 0;
    for b in results.iter_mut() {
        if let Block::ToolResult { content, .. } = b {
            if content.chars().count() > c { *content = cut_result(content, c); cut += 1; }
        }
    }
    cut
}

/// what an old assistant answer keeps in a request when the history is long (`shorten_old_replies`), in tokens
pub const SHORT_REPLY_TOKENS: usize = 120;
/// the marker after a shortened answer
pub const SHORTENED: &str = " …[shortened]";
/// the history share of the window (percent) above which old answers are shortened in requests
pub const SHORTEN_AT_PCT: usize = 35;

/// The history as a request sends it when it is long (over SHORTEN_AT_PCT of `max_ctx`): every assistant answer
/// before `end` (the current turn's request) except the last 2 keeps only its first SHORT_REPLY_TOKENS tokens plus
/// SHORTENED. For requests only, never the saved history; None when nothing changes. Otherwise a resumed document
/// chat's long old answers squeeze each question's document budget to its floor, and answers come out partly
/// made up.
pub fn shorten_old_replies(msgs: &[Message], end: usize, max_ctx: usize) -> Option<Vec<Message>> {
    if estimate_messages(msgs) * 100 <= max_ctx * SHORTEN_AT_PCT { return None; }
    let answers: Vec<usize> = msgs[..end.min(msgs.len())].iter().enumerate()
        .filter(|(_, m)| m.role == Role::Assistant && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty())))
        .map(|(i, _)| i).collect();
    let old = &answers[..answers.len().saturating_sub(2)];
    let max_chars = SHORT_REPLY_TOKENS * 33 / 10;
    let mut out: Option<Vec<Message>> = None;
    for &i in old {
        let m = &msgs[i];
        if !m.content.iter().any(|b| matches!(b, Block::Text { text } if text.chars().count() > max_chars + SHORTENED.len())) { continue; }
        let v = out.get_or_insert_with(|| msgs.to_vec());
        for b in v[i].content.iter_mut() {
            if let Block::Text { text } = b {
                if text.chars().count() <= max_chars + SHORTENED.len() { continue; }
                let cut: String = text.chars().take(max_chars).collect();
                // at a word boundary when there is one near the end
                let cut = match cut.rfind(char::is_whitespace) { Some(j) if j > max_chars * 3 / 4 => cut[..j].to_string(), _ => cut };
                *text = format!("{}{SHORTENED}", cut.trim_end());
            }
        }
    }
    out
}

/// Shrink old turns in place (oldest first, never the last `keep_recent` user turns) until the
/// estimate is <= target_tokens. Returns estimated tokens freed.
pub fn trim_old(msgs: &mut Vec<Message>, keep_recent: usize, target_tokens: usize) -> usize {
    let before = estimate_messages(msgs);
    let mut est = before;
    if est <= target_tokens { return 0; }
    let mut boundary = msgs.len();
    let mut seen = 0;
    if keep_recent > 0 {
        boundary = 0;
        for (i, m) in msgs.iter().enumerate().rev() {
            if is_user_text(m) {
                seen += 1;
                if seen == keep_recent { boundary = i; break; }
            }
        }
    }
    'passes: for pass in 0..3 {
        for m in msgs[..boundary].iter_mut() {
            let old = message_tokens(m);
            let mut changed = false;
            for b in m.content.iter_mut() {
                match (pass, &mut *b) {
                    (0, Block::ToolResult { content, .. }) if content.chars().count() > 300 => {
                        *content = trim_result(content);
                        changed = true;
                    }
                    (1, Block::ToolUse { input, .. }) => {
                        let prev = input.to_string().len();
                        trim_strings(input);
                        changed |= input.to_string().len() != prev;
                    }
                    (2, Block::Text { text }) if m.role == Role::Assistant && text.chars().count() > 600 => {
                        *text = format!("{}…", head(text, 300));
                        changed = true;
                    }
                    _ => {}
                }
            }
            if changed {
                est = est - old + message_tokens(m);
                if est <= target_tokens { break 'passes; }
            }
        }
    }
    before.saturating_sub(est)
}

/// Tool results older than the last `keep_recent` user text turns become one line (`[trimmed: first line … N lines]`).
pub fn shrink_old_results(msgs: &mut [Message], keep_recent: usize) {
    let mut seen = 0;
    let boundary = msgs.iter().enumerate().rev().find(|(_, m)| { if is_user_text(m) { seen += 1; } seen >= keep_recent.max(1) }).map_or(0, |(i, _)| i);
    for m in msgs[..boundary].iter_mut() {
        for b in m.content.iter_mut() {
            if let Block::ToolResult { content, .. } = b {
                if shrinkable(content) { *content = trim_result(content); }
            }
        }
    }
}

/// Every tool result becomes one line and long tool_use string inputs (> 300 chars) a marker (between-turns fold:
/// the state card carries what they said).
pub fn shrink_all_results(msgs: &mut [Message]) {
    for m in msgs.iter_mut() {
        for b in m.content.iter_mut() {
            match b {
                Block::ToolResult { content, .. } if shrinkable(content) => *content = trim_result(content),
                Block::ToolUse { input, .. } => trim_strings(input),
                _ => {}
            }
        }
    }
}

/// Tool results in every message except the newest one carrying tool results become one line.
/// Returns whether anything changed.
pub fn shrink_results_but_newest(msgs: &mut [Message]) -> bool {
    let has = |m: &Message| m.content.iter().any(|b| matches!(b, Block::ToolResult { .. }));
    let Some(newest) = msgs.iter().rposition(has) else { return false };
    let mut changed = false;
    for m in msgs[..newest].iter_mut() {
        for b in m.content.iter_mut() {
            if let Block::ToolResult { content, .. } = b {
                if shrinkable(content) {
                    *content = trim_result(content);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The user's own words of a user message: without the `first_message` header and a leading recalled `[memory]` block.
pub fn request_body(text: &str) -> &str {
    // "OS: …": a routed chat first turn's header, without the working directory (`repl.rs` `route_cwd`);
    // "Local time now: …": the clock lines in front of a routed time turn (`repl.rs`): both may lead, in either order
    let header = |t: &str| t.starts_with("Working directory: ") || t.starts_with("Local time now: ") || (t.starts_with("OS: ") && t.contains("\nDate: "));
    let body = match text.rsplit_once("## Request\n") {
        Some((_, r)) => r,
        None => {
            let mut t = text;
            for _ in 0..2 { if header(t) { t = t.split_once("\n\n").map_or(t, |(_, r)| r); } }
            t
        }
    };
    // a small-talk turn's situation line, or a short follow-up's hint, sits right before the user's words
    // (`situation.rs`, `followup.rs`)
    crate::followup::strip(crate::situation::strip(strip_memory(crate::route::strip_ask(crate::reply_lang::strip_hint(body)))))
}

/// turns of a resumed conversation shown again (`earlier_turns`)
pub const EARLIER_TURNS: usize = 40;

/// A resumed conversation's earlier turns as the user saw them, for the display (the app's `history` event, the
/// TUI): the user's own words (no Working directory / OS / Date / clock header, notes, `[state]`, recalled memories,
/// situation or follow-up hints; an attached document as its reference line only) and the replies as written, a
/// tool call as one line ("used read on README.md"), tool results left out. The last `max_turns` user turns.
pub fn earlier_turns(history: &[Message], cwd: &Path, max_turns: usize) -> Vec<crate::frontend::Earlier> {
    use crate::frontend::Earlier;
    let mut out: Vec<Earlier> = Vec::new();
    for m in history {
        match m.role {
            Role::User => {
                let mut words = String::new();
                let mut refs: Vec<String> = Vec::new();
                for b in &m.content {
                    let Block::Text { text } = b else { continue };
                    if text.starts_with(NUDGE_PREFIX) || is_picture_block(text) { continue; }
                    if crate::attach::is_block(text) {
                        if text.starts_with(crate::attach::MARK) { refs.push(crate::attach::first_line(text).to_string()); }
                        continue;
                    }
                    let t = request_body(text);
                    let t = strip_turn_lines(t).unwrap_or_else(|| t.to_string());
                    if !t.trim().is_empty() { if !words.is_empty() { words.push('\n'); } words += t.trim(); }
                }
                for r in refs { if !words.is_empty() { words.push('\n'); } words += &r; }
                if !words.is_empty() { out.push(Earlier { role: "user".into(), text: words }); }
            }
            Role::Assistant => {
                let mut t = String::new();
                for b in &m.content {
                    let line = match b {
                        Block::Text { text } => split_state(text).0.trim().to_string(),
                        Block::ToolUse { name, input, .. } => match crate::tui::act_target(name, input, cwd).filter(|x| !x.is_empty()) {
                            Some(x) => format!("used {name} on {}", one_line(&x, 80)),
                            None => format!("used {name}"),
                        },
                        _ => String::new(),
                    };
                    if line.is_empty() { continue; }
                    if !t.is_empty() { t.push_str(if matches!(b, Block::ToolUse { .. }) { "\n" } else { "\n\n" }); }
                    t += &line;
                }
                if t.is_empty() { continue; }
                // the steps of one turn (tool calls, then the answer) are one reply
                match out.last_mut() {
                    Some(l) if l.role == "assistant" => { l.text.push_str("\n\n"); l.text += &t; }
                    _ => out.push(Earlier { role: "assistant".into(), text: t }),
                }
            }
        }
    }
    let users: Vec<usize> = out.iter().enumerate().filter(|(_, e)| e.role == "user").map(|(i, _)| i).collect();
    if users.len() > max_turns { out.drain(..users[users.len() - max_turns]); }
    out
}

/// `text` without its situation line / follow-up hint paragraph (`situation.rs`, `followup.rs`), None when it has
/// neither.
pub fn strip_turn_lines(text: &str) -> Option<String> {
    let line = |p: &str| (p.starts_with(crate::situation::PREFIX) || p.starts_with(crate::followup::PREFIX)) && p.ends_with(')') && !p.contains('\n');
    if !text.split("\n\n").any(line) { return None; }
    Some(text.split("\n\n").filter(|p| !line(p)).collect::<Vec<_>>().join("\n\n"))
}

/// A reply that is only a greeting or Elisa's generic intro ("Hello! How can I help you today?", "I'm Elisa, a
/// personal assistant on your Mac…"), or Elisa's own "no earlier conversation" line: nothing to follow up on or
/// continue.
pub fn is_intro(text: &str) -> bool {
    let l = text.trim().to_lowercase();
    if l.is_empty() { return true; }
    let intro = ["i'm elisa", "i am elisa", "a personal assistant on your mac", "your personal assistant on mac", "your personal assistant on your mac", "there's no earlier conversation", "chưa có cuộc trò chuyện nào trước đó", "tôi là elisa"];
    let hello = ["how can i help", "what can i help", "how can i assist", "what would you like to do next", "giúp gì", "bạn muốn làm gì tiếp theo"];
    intro.iter().any(|p| l.contains(p)) || (l.chars().count() <= 60 && hello.iter().any(|p| l.contains(p)))
}

/// A saved session worth continuing: some user turn that is more than a greeting, small talk, a short follow-up or
/// a continue ask (or carries an attached document), and some reply that is more than a greeting / the intro.
pub fn real_content(h: &[Message]) -> bool {
    let user = h.iter().filter(|m| m.role == Role::User).any(|m| {
        let t: Vec<&str> = m.content.iter().filter_map(|b| match b { Block::Text { text } => Some(text.as_str()), _ => None }).collect();
        t.len() > 1 || t.first().is_some_and(|t| {
            let w = request_body(t).trim();
            !w.is_empty() && !crate::route::small_talk(w) && !crate::route::ack(w) && !crate::route::is_follow_up(w) && crate::route::resume_intent(w).is_none() && crate::followup::kind(w).is_none()
        })
    });
    let reply = h.iter().filter(|m| m.role == Role::Assistant).any(|m| m.content.iter().any(|b| match b {
        Block::ToolUse { .. } => true,
        Block::Text { text } => !text.trim_start().starts_with('[') && !is_intro(text),
        _ => false,
    }));
    user && reply
}

/// The request's messages with a repeated reply dropped: when the last two answered turns got the same reply (the
/// model then tends to copy it a third time), the older turn (its user message and
/// reply) goes from the request, until the last two differ. None when nothing repeats. The history is unchanged.
pub fn drop_repeated_replies(msgs: &[Message]) -> Option<Vec<Message>> {
    let user_text = |m: &Message| m.role == Role::User && m.content.iter().all(|b| matches!(b, Block::Text { .. }));
    let reply = |m: &Message| (m.role == Role::Assistant && m.content.iter().all(|b| matches!(b, Block::Text { .. })))
        .then(|| m.content.iter().filter_map(|b| match b { Block::Text { text } => Some(text.trim()), _ => None }).collect::<Vec<_>>().join("\n"));
    let mut v = msgs.to_vec();
    let mut changed = false;
    loop {
        // answered turns: (index of the user message, its reply text)
        let pairs: Vec<(usize, String)> = (0..v.len().saturating_sub(1)).filter(|&i| user_text(&v[i])).filter_map(|i| reply(&v[i + 1]).map(|r| (i, r))).collect();
        let n = pairs.len();
        if n < 2 { break; }
        let ((i1, r1), (i2, r2)) = (&pairs[n - 2], &pairs[n - 1]);
        // adjacent turns with the same non-empty reply; never the first message (it carries the session's header)
        if *i1 == 0 || i1 + 2 != *i2 || r1.is_empty() || r1 != r2 { break; }
        v.drain(*i1..i1 + 2);
        changed = true;
    }
    changed.then_some(v)
}

/// A loaded session: an assistant message with no recorded model gets "" (unknown: saved before models were
/// recorded), so stamping on the next save (`stamp_model`) never claims it for this run's model.
pub fn loaded(mut h: Vec<Message>) -> Vec<Message> {
    for m in h.iter_mut().filter(|m| m.role == Role::Assistant && m.model.is_none()) { m.model = Some(String::new()); }
    drop_cancelled_turns(h)
}

/// A saved session's cancelled turns (an older Elisa ended them with a "[cancelled]" stand-in reply), each from its
/// user message to that line, so a resumed chat never shows the model the stand-in to copy. The first turn
/// stays (its message carries the conversation's header); there only the stand-in goes.
pub fn drop_cancelled_turns(h: Vec<Message>) -> Vec<Message> {
    let stand_in = |m: &Message| m.role == Role::Assistant && m.content.iter().all(|b| matches!(b, Block::Text { text } if text.trim() == "[cancelled]"));
    if !h.iter().any(|m| !m.content.is_empty() && stand_in(m)) { return h; }
    let mut out: Vec<Message> = Vec::with_capacity(h.len());
    for m in h {
        if m.content.is_empty() || !stand_in(&m) { out.push(m); continue; }
        match out.iter().rposition(is_request_msg) {
            Some(0) | None => {
                // the first turn: its words stay, with the no-answer stand-in (`route::NO_ANSWER`) so roles alternate
                while out.last().is_some_and(|x| !is_request_msg(x)) { out.pop(); }
                out.push(Message::assistant_text(crate::route::NO_ANSWER));
            }
            Some(at) => out.truncate(at),
        }
    }
    out
}

/// The assistant messages written in this run (no model recorded yet) get `model`.
pub fn stamp_model(h: &mut [Message], model: &str) {
    for m in h.iter_mut().filter(|m| m.role == Role::Assistant && m.model.is_none()) { m.model = Some(model.to_string()); }
}

/// how the block of another model's turns starts in a request (`other_model_view`)
pub const EARLIER_OPEN: &str = "[earlier in this chat";
/// an earlier ask in that block, at most this many chars
pub const EARLIER_ASK_CHARS: usize = 200;

/// The request's messages when the history holds turns another model answered (after a model switch,
/// small models take the old model's replies in the assistant role as their own and copy them, the old model's
/// name included). Each such turn leaves the
/// assistant role and its reply leaves the request: the user's words (as Elisa writes them, `users_words`: a fact in
/// the third person, other first-person words quoted) become one line in an `[earlier in this chat …]` block at the
/// front of the next turn this model answers (the first message's header stays first). The old replies, kept in
/// that block with a label like "Elisa on <model>: …", were continued as a transcript; with the replies left out
/// every model answers as itself. A turn
/// whose ask the user repeats later is left out too. `now` is the current model's facts (`situation::model_now`).
/// A tiny window gets the user's lines only. Decided from the recorded models alone, never from reply wording; for
/// requests only (the history is unchanged). None when no turn was answered by another model.
pub fn other_model_view(msgs: &[Message], model: &str, now: &str, tiny: bool) -> Option<Vec<Message>> {
    let other = |m: &Message| m.role == Role::Assistant && m.model.as_deref().is_some_and(|x| !x.is_empty() && x != model);
    if !msgs.iter().any(other) { return None; }
    let starts: Vec<usize> = (0..msgs.len()).filter(|&i| is_request_msg(&msgs[i])).collect();
    if starts.first() != Some(&0) { return None; }
    // the user's own words of a request message (no header, hints, memory; a document as its reference line)
    let words = |m: &Message| -> String {
        let mut v: Vec<String> = Vec::new();
        for b in &m.content {
            let Block::Text { text } = b else { continue };
            if text.starts_with(NUDGE_PREFIX) || is_picture_block(text) { continue; }
            if crate::attach::is_block(text) { v.push(crate::attach::first_line(text).to_string()); continue; }
            let t = request_body(text);
            v.push(strip_turn_lines(t).unwrap_or_else(|| t.to_string()));
        }
        one_line(&v.join(" "), usize::MAX)
    };
    let norm = |s: &str| s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>().join(" ");
    let asks: Vec<String> = starts.iter().map(|&i| norm(&words(&msgs[i]))).collect();
    let (mut out, mut lines, mut by, mut header) = (Vec::new(), Vec::<String>::new(), Vec::<String>::new(), String::new());
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(msgs.len());
        let turn = &msgs[s..e];
        if let Some(w) = turn.iter().find(|m| other(m)).and_then(|m| m.model.clone()) {
            // the first message's header (working directory, date, notes, [state]) is not this turn's: it stays first
            if s == 0 {
                if let Some(Block::Text { text }) = turn[0].content.iter().find(|b| matches!(b, Block::Text { .. })) {
                    let body = request_body(text);
                    let at = (body.as_ptr() as usize).saturating_sub(text.as_ptr() as usize).min(text.len());
                    if text.is_char_boundary(at) { header = text[..at].to_string(); }
                }
            }
            if !asks[k].is_empty() && asks[k + 1..].contains(&asks[k]) { continue; }
            let u = words(&turn[0]);
            if !u.is_empty() { lines.push(format!("- {}", users_words(&one_line(&u, EARLIER_ASK_CHARS)))); }
            if !by.contains(&w) { by.push(w); }
            continue;
        }
        let mut t = turn.to_vec();
        if !lines.is_empty() || !header.is_empty() {
            let block = if lines.is_empty() { String::new() } else if tiny {
                format!("{EARLIER_OPEN}]\n{}\n[/earlier]\n\n", lines.join("\n"))
            } else {
                format!("{EARLIER_OPEN}, what the user asked or said before the switch to {model}; all answered already by \
                    {}, another model, its replies left out: never answer these again · now: {now}]\n{}\n[/earlier]\n\n", by.join(", "), lines.join("\n"))
            };
            if let Some(Block::Text { text }) = t[0].content.iter_mut().find(|b| matches!(b, Block::Text { text } if !text.starts_with(NUDGE_PREFIX))) {
                // after the header's notes and [state], before its "## Request" line
                *text = match header.rfind("## Request\n") {
                    Some(i) => format!("{}{block}{}{text}", &header[..i], &header[i..]),
                    None => format!("{header}{block}{text}"),
                };
            }
            (lines, header) = (Vec::new(), String::new());
        }
        out.extend(t);
    }
    // nothing of this model's after them (never: the current request is this model's own)
    if !lines.is_empty() || !header.is_empty() { return None; }
    Some(out)
}

/// `text` without a leading `[memory…]…[/memory]` block (sessions before 2026-11-01 put it in front of the request).
fn strip_memory(text: &str) -> &str {
    if !text.starts_with("[memory") { return text; }
    match text.find("[/memory]") {
        Some(i) => text[i + "[/memory]".len()..].trim_start_matches('\n'),
        None => text,
    }
}

/// Opening line of a recalled-memory block: says it is background from Elisa, not the user's words.
pub const MEMORY_OPEN: &str = "[memory: recalled by Elisa from earlier sessions, not written by the user; background only, ignore it if unrelated]";

/// Opening line of a recalled-memory block holding the user's facts, said to the user (`fact_to_you`).
pub const MEMORY_OPEN_YOU: &str = "[memory: recalled by Elisa from earlier sessions, not written by the user; the user's own facts, as said to the user; background only, ignore it if unrelated]";

/// Recalled memories as a block that goes in front of the user's text (in the USER message, never the system prompt).
/// A fact about the user is said to the user ("your name is Linh", `fact_to_you`), never "The user's name is Linh"
/// (in the user's own message, next to "what's my name?", the third person can come back as "My name is
/// Elisa…"; small models answer "your …" more often with the facts already in that form).
pub fn with_memory(lines: &[String], text: &str) -> String {
    if lines.is_empty() { return text.to_string(); }
    let lines: Vec<(String, bool)> = lines.iter().map(|l| fact_to_you(l).map_or((l.clone(), false), |y| (y, true))).collect();
    let open = if lines.iter().any(|(_, y)| *y) { MEMORY_OPEN_YOU } else { MEMORY_OPEN };
    let mut s = format!("{open}\n");
    for (l, _) in &lines { s += &format!("- {l}\n"); }
    s + "[/memory]\n\n" + text
}
