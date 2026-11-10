//! The situation line of a small-talk turn ("hey", "thanks", "chào", "bye"): how a thoughtful assistant would read
//! the moment, from data Elisa already keeps, so the same "hey" is answered differently in different situations.
//!
//! No context, no line: a first session, or a new session with no recent earlier one worth offering, sends the
//! user's words alone (the cheapest request; the model just says hello). With context, ONE line goes right before
//! the user's words in the last user message only (after the stable system / tools / history prefix, so the KV
//! prefix cache stays warm), <= ~40 tokens, and `context::request_body` strips it from titles and history views:
//!
//! `(Situation: same chat, 3 min later · topic: "plan a 3-day trip to Tokyo" · greet back briefly, e.g. "Hi! More on <topic>?")`
//! `(Situation: same chat, back after 5 h · after a task · next: fix parse_date for ISO dates · reply like "Welcome back! Want me to <next>?")`
//! `(Situation: returning user · last chat 2 days ago: "summarize README.md" · one line: greet, ask: continue it or something new?)`
//! `(Situation: last task stopped: "run ./test.sh and tell me what…" · reply like "Hi! Last task stopped. Try again or something new?")`
//!
//! Only on an opening greeting ("hey", "chào", "good morning"): a thanks / bye / "cool" reads well from the history
//! alone (a line there makes the model repeat the whole plan), so it never gets one.
//!
//! Sources: the clock (`ELISA_FAKE_NOW`, unix seconds, for tests only); this conversation's last turn (its time, its
//! tool calls in the history, a failed end); the current state card (the memory store's latest card on
//! `--continue`) for the next step; the newest saved session of this project (`.elisa/sessions`, its mtime and
//! `.meta` title, or the goal of its last state card when the title is small talk). Never notes content: a new
//! session only gets a title to offer, and the user decides (a small model can answer "hey" with an old task
//! when the notes are carried).

/// how every situation line starts (`strip` keys on it)
pub const PREFIX: &str = "(Situation: ";
/// a turn asking what Elisa can do or who she is: the capability list, on that turn only (the system prompt has none,
/// so a small model does not tack it onto every reply, or give it instead of
/// the user's name to "What's my name?")
/// The "what can you do?" line for this model (a fixed `ABOUT` would claim images on a model with no vision).
/// Search, reading files and pages, saving and running commands are Elisa's own (`route::intent`): every model.
pub fn about(vision: bool) -> String {
    // "you can: chat …" gets copied as the reply: the line says it in Elisa's own voice
    format!("(Situation: asked about you · answer as Elisa, in the first person, \"I can …\": {} · list only these, in that order, briefly; \
        claim nothing else)", can_list(vision))
}

/// What Elisa can do on this model (vision from the detected caps)
fn can_list(vision: bool) -> String {
    let see = if vision { ", read PDFs, spreadsheets and images" } else { ", read PDFs and spreadsheets you attach (not pictures: this model has no vision)" };
    format!("chat and answer questions, search the web for news and facts, read files and web pages{see}, \
        save text to files, run commands with the user's OK, say what time it is anywhere, and help with code")
}

/// The current model as the earlier-turns block of a switched conversation says it (`context::other_model_view`).
pub fn model_now(model: &str, vision: bool) -> String {
    format!("you are Elisa on {model} · vision: {}", if vision { "yes" } else { "no" })
}

/// One system-prompt line with the model's detected facts (otherwise "what can this model do?" can be answered about
/// a model an attached paper describes). From the connection's detection only, fixed for the session (byte-stable).
pub fn model_facts(model: &str, vision: bool, native_tools: bool, thinking: bool, ctx: usize) -> String {
    // after a model switch a small model may copy the old model's answers from the history:
    // the line says that earlier replies may be another model's and are never copied
    // (told which model it runs on, a small model may take that name as its own: the model is what Elisa runs on,
    // she stays Elisa)
    format!("Model facts (detected, true now): {}. \
        A question about the model is answered from these only, never from a document or an earlier reply (earlier replies \
        may be another model's); write every answer fresh, never copy an earlier one.", facts_head(model, vision, native_tools, thinking, ctx))
}

/// the facts themselves: what the model is and what it has
fn facts_head(model: &str, vision: bool, native_tools: bool, thinking: bool, ctx: usize) -> String {
    format!("Elisa runs on {model} · vision: {} · tool calling: {} · {}context {ctx} tokens",
        if vision { "yes" } else { "no" }, if native_tools { "yes" } else { "no" }, if thinking { "reasons first · " } else { "" })
}

/// The content words of the model facts (`model_facts`: "model", the model's name, what it has: "vision", "tool",
/// "context" …; `topics::words`). A question made only of these is about the model Elisa runs on, not about an
/// attached document that happens to use the same words ("What can this model do for me?" after a paper
/// about a model must not get the paper's pages and be answered about that model).
/// The message's content words are all the model facts' words (`model_words`): "What can this model do for me?",
/// "which model is this?", "does this model have vision?".
pub fn asks_about_model(model: &str, vision: bool, native_tools: bool, thinking: bool, ctx: usize, msg: &str) -> bool {
    let w = crate::topics::words(msg);
    !w.is_empty() && w.is_subset(&model_words(model, vision, native_tools, thinking, ctx))
}

/// The situation line of a turn asking about the model itself (`model_words_of`): the facts again, in this turn's
/// message only, with the model named as what "this model" means and what Elisa can do on it (`about`'s list).
/// With the facts in the system prompt alone, "What can this model do for me?" after a summary of a paper about a
/// model gets answered about the paper's model; a line with the facts alone is often still wrong, this one is not.
pub fn about_model(model: &str, vision: bool, native_tools: bool, thinking: bool, ctx: usize) -> String {
    format!("(Situation: \"this model\" here means {model}, the AI model you run on, never a model from a document or an \
        earlier reply · {} · say what {model} lets you do here: {})", facts_head(model, vision, native_tools, thinking, ctx), can_list(vision))
}

pub fn model_words(model: &str, vision: bool, native_tools: bool, thinking: bool, ctx: usize) -> std::collections::HashSet<String> {
    crate::topics::words(&format!("model {}", facts_head(model, vision, native_tools, thinking, ctx)))
}

pub const ABOUT: &str = "(Situation: asked about you · you can: chat and answer questions, search the web for news and facts, read PDFs, spreadsheets and images, reason step by step and calculate, say what time it is anywhere, and help with code · list only these, in that order, briefly; claim nothing else)";
/// an earlier session older than this is not offered
pub const RECENT_S: i64 = 3 * 86_400;
/// a gap from this on is "back after …" instead of "… since last message"
pub const LONG_GAP_S: i64 = 3_600;

/// What the line is built from (all optional; nothing known = no line).
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// unix seconds now
    pub now: i64,
    /// local hour now (0..=23)
    pub hour: u32,
    /// this conversation's last turn (unix seconds); None = no earlier turn in this conversation
    pub last_turn: Option<i64>,
    /// the last turn called tools
    pub last_tools: bool,
    /// the last turn did not finish (an error, a stop, or no reply was saved)
    pub last_failed: bool,
    /// the state card's next step, when the last turn was a task
    pub next_step: Option<String>,
    /// what this conversation is about: its latest request that was not small talk
    pub topic: Option<String>,
    /// a new conversation: the newest earlier session of this project (unix seconds, its topic)
    pub past: Option<(i64, String)>,
    /// the greeting is Vietnamese (`reply_lang::detect`): a stopped task's example reply is in Vietnamese
    pub vi: bool,
}

/// "3 min", "2 h", "2 days"
pub fn ago(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => "under a minute".into(),
        60..=3_599 => format!("{} min", s / 60),
        3_600..=86_399 => format!("{} h", s / 3_600),
        86_400..=172_799 => "1 day".into(),
        _ => format!("{} days", s / 86_400),
    }
}

/// the part of the day for a local hour
pub fn part_of_day(hour: u32) -> &'static str {
    match hour {
        5..=11 => "morning",
        12..=17 => "afternoon",
        18..=22 => "evening",
        _ => "late night",
    }
}

/// one line of at most `n` chars, cut at a word boundary (`…` when cut), double quotes made single
fn clip(s: &str, n: usize) -> String {
    let one = crate::context::one_line(s, usize::MAX).replace('"', "'");
    if one.chars().count() <= n { return one; }
    let mut out = String::new();
    for w in one.split_whitespace() {
        if out.chars().count() + w.chars().count() + 1 > n { break; }
        if !out.is_empty() { out.push(' '); }
        out.push_str(w);
    }
    if out.is_empty() { out = one.chars().take(n).collect(); }
    out.trim_end_matches([',', '.', ';', ':']).to_string() + "…"
}

/// The line for these facts, or None when there is no context worth a line. Each line ends with a short ask the
/// small model follows (with bare facts, such as "same conversation · 3 min since last
/// message", it still greets as if the chat were new; with the ask it picks up the topic).
pub fn line(f: &Facts) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match f.last_turn {
        Some(t) => {
            let gap = f.now - t;
            let long = gap >= LONG_GAP_S;
            parts.push(if long { format!("same chat, back after {}", ago(gap)) } else { format!("same chat, {} later", ago(gap)) });
            let task = f.last_failed || f.last_tools;
            // the part of the day after a long gap (a task's next step outweighs it within the ~40 tokens)
            if long && !task { parts.push(part_of_day(f.hour).into()); }
            // a stopped task (the loop guard, a step limit, an error): said briefly, with its goal, and the ask names
            // no tool or retry. The model may still call tools on such a turn (and loops for
            // thousands of tokens when a line names the task); `repl.rs` runs no tool on a
            // small-talk turn, so that can no longer loop.
            // The line is only that (no gap, no part of the day): its goal and the ask fill the ~40 tokens.
            if f.last_failed {
                let goal = f.next_step.as_deref().or(f.topic.as_deref()).filter(|t| !t.trim().is_empty());
                let what = match goal { Some(g) => format!("last task stopped: \"{}\"", clip(g, 30)), None => "last task stopped".into() };
                // the example in the greeting's language (an English one makes "chào" get an answer half in English)
                let ex = match (f.vi, long) {
                    (true, _) => "Chào bạn! Việc trước đã dừng. Thử lại hay việc khác?",
                    (false, true) => "Welcome back! Last task stopped. Try again or something new?",
                    (false, false) => "Hi! Last task stopped. Try again or something new?",
                };
                return Some(format!("{PREFIX}{what} · reply like \"{ex}\")"));
            }
            if f.last_tools { parts.push("after a task".into()); }
            let next = f.next_step.as_deref().filter(|_| task);
            // a topic is offered only when there is something to go on with: a question already got its answer
            // ("hey" after "what's your favourite colour?" must not get "Hi! More on what's your favourite colour?")
            let topic = f.topic.as_deref().filter(|t| offer_topic(t));
            match next {
                Some(n) => parts.push(format!("next: {}", clip(n, 48))),
                None => if let Some(t) = topic { parts.push(format!("topic: \"{}\"", clip(t, 32))); },
            }
            let has_topic = topic.is_some();
            parts.push(match (long, next.is_some(), has_topic) {
                (true, true, _) => "reply like \"Welcome back! Want me to <next>?\"",
                // a greeting mid-chat is greeted back, briefly (not with a topic or an old answer)
                (false, true, _) => "greet back briefly, e.g. \"Hi! Want me to <next>?\"",
                (true, false, true) => "reply like \"Welcome back! More on <topic>?\"",
                (true, false, false) => "reply like \"Welcome back!\"",
                (false, false, true) => "greet back briefly, e.g. \"Hi! More on <topic>?\"",
                (false, false, false) => "greet back briefly, e.g. \"Hi! What's next?\"",
            }.into());
        }
        None => {
            let (t, topic) = f.past.as_ref()?;
            let gap = f.now - t;
            if !(0..=RECENT_S).contains(&gap) || topic.trim().is_empty() { return None; }
            parts.push("returning user".into());
            parts.push(format!("last chat {} ago: \"{}\"", ago(gap), clip(topic, 40)));
            parts.push("one line: greet, ask: continue it or something new?".into());
        }
    }
    Some(format!("{PREFIX}{})", parts.join(" · ")))
}

/// Whether a conversation topic is worth offering back on a mid-chat greeting ("Hi! More on <topic>?"): a request
/// with something to go on with, not a question (it got its answer already; offering "more on" it reads oddly).
pub fn offer_topic(topic: &str) -> bool {
    let t = topic.trim();
    !t.is_empty() && !t.trim_end_matches(['"', '\'', ')', ' ']).ends_with(['?', '？'])
}

/// A mid-chat greeting's topic comes from the last this-many user turns only: an older request is stale.
pub const TOPIC_TURNS: usize = 3;

/// `text` without a leading situation line (and the blank line after it)
pub fn strip(text: &str) -> &str {
    if !text.starts_with(PREFIX) { return text; }
    match text.split_once(")\n\n") { Some((_, r)) => r, None => text }
}

/// unix seconds now: `ELISA_FAKE_NOW` (tests only) or the system clock
pub fn now_secs() -> i64 {
    if let Some(t) = std::env::var("ELISA_FAKE_NOW").ok().and_then(|v| v.trim().parse().ok()) { return t; }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// the local hour of a unix time
pub fn local_hour(t: i64) -> u32 {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() { return 12; }
        tm.tm_hour as u32
    }
}

/// a file's mtime in unix seconds
pub fn mtime(p: &std::path::Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// A card field's value (`next: …` → "…"), when it says something.
pub fn card_field(card: &str, field: &str) -> Option<String> {
    let key = format!("{field}:");
    card.lines().map(str::trim).find_map(|l| l.strip_prefix(&key)).map(str::trim)
        .filter(|v| !v.is_empty() && *v != "…" && *v != "-" && !v.eq_ignore_ascii_case("none")).map(str::to_string)
}

/// The goal of the last state card in a saved session's messages (files up to 1 MB; a bigger one is skipped).
pub fn saved_goal(path: &std::path::Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > 1_000_000 { return None; }
    let text = std::fs::read_to_string(path).ok()?;
    let msgs: Vec<crate::types::Message> = serde_json::from_str(&text).ok()?;
    msgs.iter().rev().filter(|m| m.role == crate::types::Role::Assistant).flat_map(|m| m.content.iter().rev()).find_map(|b| match b {
        crate::types::Block::Text { text } => text.rfind("<state>").and_then(|i| card_field(&text[i..], "goal")),
        _ => None,
    })
}

/// The `next:` of the last state card in the history's assistant texts (a session continued without the memory store's
/// card, or a card the store did not keep).
pub fn history_next(history: &[crate::types::Message]) -> Option<String> {
    history.iter().rev().filter(|m| m.role == crate::types::Role::Assistant).flat_map(|m| m.content.iter().rev()).find_map(|b| match b {
        crate::types::Block::Text { text } => text.rfind("<state>").map(|i| card_field(&text[i..], "next")),
        _ => None,
    }).flatten()
}

/// (tools used, failed) of the history's last turn: tool calls after the last user text, or no assistant reply.
pub fn last_turn_flags(history: &[crate::types::Message]) -> (bool, bool) {
    use crate::types::{Block, Role};
    let is_user_text = |m: &crate::types::Message| m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }));
    let Some(i) = history.iter().rposition(is_user_text) else { return (false, false) };
    let tools = history[i..].iter().any(|m| m.content.iter().any(|b| matches!(b, Block::ToolUse { .. })));
    // a reply with text that is not Elisa's own stop line ("[stopped: …]": the loop guard, a step limit, an interrupt)
    let replied = history.last().is_some_and(|m| m.role == Role::Assistant && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty() && !text.trim_start().starts_with("[stopped"))));
    (tools, !replied)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn a_question_about_the_model_is_told_by_the_facts_words() {
        // a question made only of the model facts' words
        let asks = |m: &str| asks_about_model("granite4.2:3b", false, true, true, 16384, m);
        for m in ["What can this model do for me?", "which model is this?", "does this model have vision?", "does this model do tool calling?"] {
            assert!(asks(m), "{m:?}");
        }
        for m in ["what can the Kronos model do?", "which model is best for coding?", "hey", "what is 15% of 240?"] {
            assert!(!asks(m), "{m:?}");
        }
        let l = about_model("granite4.2:3b", false, true, true, 16384);
        assert!(l.starts_with(PREFIX) && l.contains("granite4.2:3b") && l.contains("vision: no") && l.contains("context 16384 tokens"), "{l}");
    }

    #[test]
    fn no_context_no_line() {
        assert_eq!(line(&Facts { now: NOW, hour: 21, ..Default::default() }), None, "first session ever");
        // an earlier session too old, or with no topic: still nothing
        assert_eq!(line(&Facts { now: NOW, past: Some((NOW - 5 * 86_400, "README summary".into())), ..Default::default() }), None);
        assert_eq!(line(&Facts { now: NOW, past: Some((NOW - 3_600, " ".into())), ..Default::default() }), None);
    }

    #[test]
    fn a_recent_session_is_offered_never_continued() {
        let l = line(&Facts { now: NOW, hour: 21, past: Some((NOW - 2 * 86_400, "summarize \"README.md\"".into())), ..Default::default() }).unwrap();
        assert_eq!(l, "(Situation: returning user · last chat 2 days ago: \"summarize 'README.md'\" · one line: greet, ask: continue it or something new?)");
        assert!(short(&l), "{l}");
    }

    /// <= 40 tokens at the repo's chars/3.3 estimate
    fn short(l: &str) -> bool { l.chars().count() as f32 / 3.3 <= 40.0 }

    #[test]
    fn mid_conversation_lines() {
        let topic = Some("plan a 3-day trip to Tokyo".to_string());
        let quick = line(&Facts { now: NOW, hour: 10, last_turn: Some(NOW - 180), topic: topic.clone(), ..Default::default() }).unwrap();
        assert_eq!(quick, "(Situation: same chat, 3 min later · topic: \"plan a 3-day trip to Tokyo\" · greet back briefly, e.g. \"Hi! More on <topic>?\")");
        let bare = line(&Facts { now: NOW, hour: 10, last_turn: Some(NOW - 30), ..Default::default() }).unwrap();
        assert_eq!(bare, "(Situation: same chat, under a minute later · greet back briefly, e.g. \"Hi! What's next?\")");
        let back = line(&Facts { now: NOW, hour: 21, last_turn: Some(NOW - 2 * 3_600), topic, ..Default::default() }).unwrap();
        assert_eq!(back, "(Situation: same chat, back after 2 h · evening · topic: \"plan a 3-day trip to Tokyo\" · reply like \"Welcome back! More on <topic>?\")");
        let failed = line(&Facts { now: NOW, hour: 2, last_turn: Some(NOW - 4 * 3_600), last_failed: true, last_tools: true, next_step: Some("rerun cargo test".into()), ..Default::default() }).unwrap();
        assert_eq!(failed, "(Situation: last task stopped: \"rerun cargo test\" · reply like \"Welcome back! Last task stopped. Try again or something new?\")");
        let task = line(&Facts { now: NOW, hour: 14, last_turn: Some(NOW - 60), last_tools: true, ..Default::default() }).unwrap();
        assert_eq!(task, "(Situation: same chat, 1 min later · after a task · greet back briefly, e.g. \"Hi! What's next?\")");
        // a stopped task: said briefly with its goal (the request), else with none
        let stopped = line(&Facts { now: NOW, hour: 14, last_turn: Some(NOW - 120), last_tools: true, last_failed: true, topic: Some("Run ./test.sh and report what failed".into()), ..Default::default() }).unwrap();
        assert_eq!(stopped, "(Situation: last task stopped: \"Run ./test.sh and report what…\" · reply like \"Hi! Last task stopped. Try again or something new?\")");
        assert_eq!(line(&Facts { now: NOW, hour: 14, last_turn: Some(NOW - 60), last_failed: true, ..Default::default() }).unwrap(),
            "(Situation: last task stopped · reply like \"Hi! Last task stopped. Try again or something new?\")");
        let vi = line(&Facts { now: NOW, hour: 14, last_turn: Some(NOW - 60), last_failed: true, topic: Some("chạy ./test.sh".into()), vi: true, ..Default::default() }).unwrap();
        assert_eq!(vi, "(Situation: last task stopped: \"chạy ./test.sh\" · reply like \"Chào bạn! Việc trước đã dừng. Thử lại hay việc khác?\")");
        assert!(short(&vi), "{vi}");
        // a question already got its answer: no "More on <topic>?"
        let asked = line(&Facts { now: NOW, hour: 10, last_turn: Some(NOW - 120), topic: Some("what's your favourite colour?".into()), ..Default::default() }).unwrap();
        assert_eq!(asked, "(Situation: same chat, 2 min later · greet back briefly, e.g. \"Hi! What's next?\")");
        assert!(!offer_topic("thủ đô của Pháp là gì？") && !offer_topic("  ") && offer_topic("tell me about Hanoi"));
        // a past session is never mentioned mid-conversation
        let mid = line(&Facts { now: NOW, last_turn: Some(NOW - 60), past: Some((NOW - 60, "old topic".into())), ..Default::default() }).unwrap();
        assert!(!mid.contains("old topic"));
        for l in [&quick, &back, &failed, &task, &stopped] { assert!(short(l), "{l}"); }
    }

    #[test]
    fn strips_and_parses() {
        assert_eq!(strip("(Situation: new session · x)\n\nhey"), "hey");
        assert_eq!(strip("hey"), "hey");
        assert_eq!(card_field("goal: summarize README\nnext: …\nkeep: -", "goal").as_deref(), Some("summarize README"));
        assert_eq!(card_field("goal: x\nnext: …", "next"), None);
        assert_eq!(clip("plan a 3-day trip to Tokyo, short list please", 32), "plan a 3-day trip to Tokyo…");
        assert_eq!(ago(30), "under a minute");
        assert_eq!(ago(90_000), "1 day");
        assert_eq!(part_of_day(7), "morning");
        assert_eq!(part_of_day(23), "late night");
    }

    #[test]
    fn last_turn_flags_from_history() {
        use crate::types::{Block, Message, Role};
        let u = |t: &str| Message::new(Role::User, vec![Block::Text { text: t.into() }]);
        let a = |t: &str| Message::new(Role::Assistant, vec![Block::Text { text: t.into() }]);
        assert_eq!(last_turn_flags(&[]), (false, false));
        assert_eq!(last_turn_flags(&[u("hi"), a("Hello!")]), (false, false));
        assert_eq!(last_turn_flags(&[u("hi")]), (false, true), "no reply saved: failed");
        assert_eq!(last_turn_flags(&[u("run it"), a("[stopped: the model kept repeating calls that already ran]")]), (false, true), "stopped");
        let call = Message::new(Role::Assistant, vec![Block::ToolUse { id: "1".into(), name: "read".into(), input: serde_json::json!({}) }]);
        assert_eq!(last_turn_flags(&[u("read it"), call, a("done")]), (true, false));
    }
}
