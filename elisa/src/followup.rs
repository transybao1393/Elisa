//! The follow-up hint: a short follow-up ("why?", "more", "the second one", "and in Osaka?", "tiếp đi", "cái thứ hai")
//! gets one line saying what it asks for, right before the user's words in the last user message only (like the
//! situation line, `situation.rs`), so the cached prefix stays warm. `context::request_body` strips it from titles,
//! history views and memory. <= 25 tokens.
//!
//! Why: without it, after a long answer (say, a 3-day Tokyo plan) a small model answers "more", "the second one"
//! or "tiếp đi" with the whole answer again word for word, "why?" with a refusal, and "and in Osaka?" with the same
//! list, the city swapped.
//!
//! Wording: "don't repeat what you said" still gets the answer again; asking to go deeper beyond the last answer,
//! with no repeats, gets an expanded one. "tell more about that item" works better than "answer about that item
//! only", which often just restates the item.
//!
//! About an attached document, asking for new tips and details got details that are not in the document and
//! unrequested extras. "More" no longer asks for new tips; about an attached document
//! the hints say to stay with what the document says (`line_for`). A bare "yes" / "ok" after an answer that offered
//! several options gets a hint to ask which one (`pick_option`), not the model's own pick of the first (it wrote code).
//!
//! Only with a previous answer: a new session (or a last turn with no answer, e.g. a stopped task) sends no hint,
//! so nothing invents context the model does not have. Vietnamese and English words both parse.

/// how every hint starts (`strip` keys on it)
pub const PREFIX: &str = "(Follow-up: ";

/// The hint of an explicit "continue the last one" in a new session, once `repl.rs` loaded the last session
/// (a bare "continue your last answer or task" gets the last answer again word for word)
pub const RESUME_CONTINUE: &str = "(Follow-up: the user is back to this chat: go on from your last answer with concrete details, no repeats.)";
/// The same when that session's last turn stopped (the loop guard, an error): the task again
pub const RESUME_RETRY: &str = "(Follow-up: the user is back to this chat: try the task that stopped again.)";
/// The hint of "what were we talking about?" (and of "continue our last conversation" after a plain chat) in a new
/// session, once the last session is loaded: `resume_recap` puts the user's own earlier messages in it (asked to
/// continue or recap without them, a small model answers only the last turns' topic, or a bare "OK." after a chat
/// that ended in "just reply OK" turns; with the messages listed it names the earlier topics; quoting the first
/// message alone, or "from its first message on", makes it repeat its first reply)
pub const RESUME_RECAP: &str = "(Follow-up: the user is back after a break. Talking to them directly, tell them in 1-2 short sentences what this chat covered: every topic, and details they gave such as their name or codes; then end with: Want to go on?)";

/// RESUME_RECAP with the user's own short earlier messages (`asks`, oldest first) named in it: those of <= 25 words
/// (a long one is pasted text, and may carry its own instructions: "…then just reply OK: <text>" quoted here got
/// "OK" back), each one line of <= 60 chars, repeats once, at most the first 3 and the last 3 (<= ~100 tokens, in this
/// turn's message only)
pub fn resume_recap(asks: &[String]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for a in asks.iter().filter(|a| a.split_whitespace().count() <= 25) {
        let l = crate::context::one_line(a, 60);
        if !l.is_empty() && !seen.contains(&l) { seen.push(l); }
    }
    if seen.is_empty() { return RESUME_RECAP.to_string(); }
    if seen.len() > 6 { seen.drain(3..seen.len() - 3); }
    let list = seen.iter().map(|l| format!("\"{l}\"")).collect::<Vec<_>>().join("; ");
    RESUME_RECAP.replacen("or codes;", &format!("or codes (their messages included {list});"), 1)
}

/// What a short follow-up asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// "why?", "tại sao?"
    Why,
    /// "more", "go on", "tiếp đi"
    More,
    /// "the second one", "number 2", "cái thứ hai": the item number (0 = the last one)
    Pick(usize),
    /// "and in Osaka?", "what about X?", "còn X thì sao?": X
    About(String),
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '#' || c == '\'')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

const WHY: &[&str] = &["why", "why so", "why not", "how come", "but why", "why is that", "why that", "tại sao", "vì sao", "sao vậy", "sao thế", "tại sao vậy", "tại sao thế", "vì sao vậy", "sao", "là sao"];
const MORE: &[&str] = &["more", "more please", "tell me more", "go on", "continue", "keep going", "carry on", "please continue", "and then", "then what", "what else", "anything else", "more details", "go deeper", "say more",
    "tiếp", "tiếp đi", "tiếp tục", "tiếp tục đi", "thêm nữa", "thêm đi", "nói thêm", "nói tiếp", "kể tiếp", "còn gì nữa", "rồi sao", "rồi sao nữa", "nữa đi", "chi tiết hơn"];
const EN_ORD: &[(&str, usize)] = &[("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("fifth", 5), ("sixth", 6), ("seventh", 7), ("eighth", 8), ("ninth", 9), ("tenth", 10), ("last", 0),
    ("1st", 1), ("2nd", 2), ("3rd", 3), ("4th", 4), ("5th", 5), ("6th", 6), ("7th", 7), ("8th", 8), ("9th", 9), ("10th", 10)];
const VI_NUM: &[(&str, usize)] = &[("nhất", 1), ("hai", 2), ("ba", 3), ("tư", 4), ("bốn", 4), ("năm", 5), ("sáu", 6), ("bảy", 7), ("tám", 8), ("chín", 9), ("mười", 10)];
/// words around an item pick that say nothing more ("the second one please", "cái thứ hai đi", "option 2")
const PICK_FILLER: &[&str] = &["the", "one", "please", "pls", "option", "item", "choice", "pick", "number", "no", "#", "i", "want", "like", "take", "choose", "go", "with", "ok", "okay",
    "cái", "món", "phương", "án", "lựa", "chọn", "mục", "ý", "số", "thứ", "đi", "nhé", "nha", "chọn", "lấy", "tôi", "mình", "cuối", "cùng", "đầu", "tiên"];

/// The item number a short pick names, 0 for "the last one"; None when it is not a pick.
fn pick(w: &[String]) -> Option<usize> {
    if w.is_empty() || w.len() > 6 { return None; }
    let digit = |s: &str| s.trim_start_matches('#').parse::<usize>().ok().filter(|n| (1..=20).contains(n));
    // a bare number ("2") alone is a pick too
    if w.len() == 1 { if let Some(n) = digit(&w[0]) { return Some(n); } }
    let mut found: Option<usize> = None;
    let mut i = 0;
    while i < w.len() {
        let x = w[i].as_str();
        let next = w.get(i + 1).map(String::as_str);
        let hit = if let Some(&(_, n)) = EN_ORD.iter().find(|(o, _)| *o == x) { Some(n) }
            // "thứ hai", "thứ 2": Vietnamese ordinals
            else if x == "thứ" { next.and_then(|n| VI_NUM.iter().find(|(v, _)| *v == n).map(|p| p.1).or_else(|| digit(n))).inspect(|_| i += 1) }
            // "số 2", "number 2", "no 2", "option 2", "#2"
            else if matches!(x, "số" | "number" | "no" | "option" | "item" | "choice" | "mục" | "án") { next.and_then(digit).inspect(|_| i += 1) }
            else if x.starts_with('#') { digit(x) }
            // "cuối cùng" / "cái cuối": the last; "đầu tiên": the first
            else if x == "cuối" { Some(0) }
            else if x == "đầu" && next == Some("tiên") { i += 1; Some(1) }
            else if !PICK_FILLER.contains(&x) { return None; }
            else { None };
        if let Some(n) = hit { if found.is_some() { return None; } found = Some(n); }
        i += 1;
    }
    found
}

/// "and in Osaka?" → "Osaka", "what about the price?" → "the price", "còn Osaka thì sao?" → "Osaka".
fn about(msg: &str) -> Option<String> {
    let t = msg.trim().trim_end_matches(['?', '!', '.', ' ']).trim();
    let lower = t.to_lowercase();
    // (opening, closing): the subject sits between them
    const FORMS: &[(&str, &str)] = &[("what about ", ""), ("and what about ", ""), ("and in ", ""), ("and for ", ""), ("and at ", ""), ("and on ", ""), ("and with ", ""),
        ("and ", ""), ("còn ", " thì sao"), ("còn ở ", " thì sao"), ("thế còn ", ""), ("vậy còn ", ""), ("còn ", ""), ("ở ", " thì sao"), ("với ", " thì sao")];
    for (open, close) in FORMS {
        let Some(rest) = lower.strip_prefix(open) else { continue };
        let rest = match close.is_empty() { true => rest, false => match rest.strip_suffix(close) { Some(r) => r, None => continue } };
        let rest = rest.trim().strip_prefix("ở ").unwrap_or(rest.trim()).trim();
        // the original casing ("Osaka", not "osaka"): the same span of the trimmed message (lowercase when the
        // byte offsets differ)
        let start = rest.as_ptr() as usize - lower.as_ptr() as usize;
        let subject = t.get(start..start + rest.len()).filter(|s| s.to_lowercase() == rest).unwrap_or(rest).trim().trim_end_matches([',', ';']).trim();
        let n = subject.split_whitespace().count();
        // a short subject only ("and also add tests to the parser module" is a new request, not a follow-up)
        if n == 0 || n > 4 || subject.contains(['(', ')', '\n']) { return None; }
        // "and then", "and why": not a new subject
        if matches!(subject.to_lowercase().as_str(), "then" | "why" | "more" | "so" | "also" | "now" | "you" | "sao") { return None; }
        return Some(subject.to_string());
    }
    None
}

/// What a short follow-up asks for, or None when the message is not one.
pub fn kind(msg: &str) -> Option<Kind> {
    let t = msg.trim();
    if t.is_empty() || t.contains('\n') || t.chars().count() > 60 { return None; }
    let w = words(t);
    if w.is_empty() || w.len() > 7 { return None; }
    let joined = w.join(" ");
    let polite = |list: &[&str]| { let j = joined.trim_end_matches(" please").trim_end_matches(" đi").trim_end_matches(" nhé").trim_end_matches(" nha"); list.contains(&joined.as_str()) || list.contains(&j) };
    if polite(WHY) { return Some(Kind::Why); }
    if polite(MORE) { return Some(Kind::More); }
    if let Some(n) = pick(&w) { return Some(Kind::Pick(n)); }
    about(t).map(Kind::About)
}

/// The hint line for a kind (no trailing newline).
pub fn line(k: &Kind) -> String { line_for(k, false) }

/// The hint line for a kind; `doc`: the conversation is about an attached document, so the hint keeps the model to
/// what the document says.
pub fn line_for(k: &Kind, doc: bool) -> String {
    match k {
        Kind::Why if doc => format!("{PREFIX}explain the reason behind your last answer using the document.)"),
        Kind::More if doc => format!("{PREFIX}continue from the document: the next relevant part, only what it says, no repeats.)"),
        Kind::Pick(n) if doc && *n > 0 => format!("{PREFIX}user picks item {n} of your last answer; more on it from the document.)"),
        Kind::Why => format!("{PREFIX}explain the reason behind your last answer. Don't repeat it.)"),
        Kind::More => format!("{PREFIX}go deeper on your last answer with concrete details; no repeats; say if you're unsure.)"),
        Kind::Pick(0) => format!("{PREFIX}the user picks the last item of your last answer; tell more about it only.)"),
        Kind::Pick(n) => format!("{PREFIX}the user picks item {n} from your last answer; tell more about that item only.)"),
        Kind::About(x) => format!("{PREFIX}same question as before, now about {x}.)"),
    }
}

/// The hint for `msg` when there is a previous answer to follow up on (`has_answer`), else None.
pub fn hint(msg: &str, has_answer: bool) -> Option<String> {
    if !has_answer { return None; }
    kind(msg).map(|k| line(&k))
}

/// The hint of a bare "yes" / "ok" after an answer that offered several options (`pick_option`)
pub const PICK_OPTION: &str = "(Follow-up: you offered several options; ask which one the user wants. Don't start one.)";

/// a bare yes ("yes", "ok", "sure", "vâng")
fn bare_yes(msg: &str) -> bool {
    const YES: &[&str] = &["yes", "y", "yeah", "yep", "yup", "ok", "okay", "k", "sure", "yes please", "ok please", "okay please", "sure please", "please",
        "go ahead", "do it", "sounds good", "yes go ahead", "ok go ahead", "vâng", "ừ", "có", "ok đi", "được", "ừ đi", "có chứ"];
    YES.contains(&words(msg.trim()).join(" ").as_str())
}

/// The options an answer ends by offering ("Would you like me to: 1. … 2. …", "I can … or …: which one?"): the list
/// items right after its last offer line, else right before it; 0 when its end offers nothing.
pub fn offered_options(answer: &str) -> usize {
    let lines: Vec<&str> = answer.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(14)..];
    let item = |l: &str| {
        let t = l.trim_start_matches(['*', '_', ' ']);
        let d = t.chars().take_while(char::is_ascii_digit).count();
        (d > 0 && d <= 2 && t[d..].starts_with(['.', ')'])) || l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ")
    };
    const OFFER: &[&str] = &["would you like", "do you want", "want me to", "shall i", "should i", "which one", "which would", "let me know which", "i can also", "if you'd like me to", "if you would like me to", "if you want me to", "let me know if you'd like", "let me know if you want", "elisa can help with", "i can help with", "bạn muốn", "bạn có muốn"];
    let Some(i) = tail.iter().rposition(|l| { let x = l.to_lowercase(); OFFER.iter().any(|o| x.contains(o)) && !item(l) }) else { return 0 };
    let after = tail[i + 1..].iter().take_while(|l| item(l)).count();
    if after >= 2 { return after; }
    tail[..i].iter().rev().take_while(|l| item(l)).count()
}

/// A bare "yes" after an answer that offered several options: the hint to ask which one (None otherwise, so a yes to
/// one offer goes ahead with it).
pub fn pick_option(msg: &str, last_answer: &str) -> Option<&'static str> {
    (bare_yes(msg) && offered_options(last_answer) >= 2).then_some(PICK_OPTION)
}

/// `text` without a leading follow-up hint (and the blank line after it)
pub fn strip(text: &str) -> &str {
    if !text.starts_with(PREFIX) { return text; }
    match text.split_once(")\n\n") { Some((_, r)) => r, None => text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_recap_names_short_messages_only() {
        let filler = format!("Keep this in mind, then just reply OK: {}", "the report grew steadily ".repeat(10));
        let asks = ["My name is Linh and my meeting code is ORCHID-42.".to_string(), "tell me a short joke".into(), filler.clone(), filler];
        let h = resume_recap(&asks);
        assert!(h.starts_with(PREFIX) && h.ends_with("Want to go on?)"), "{h}");
        assert!(h.contains("\"My name is Linh and my meeting code is ORCHID-42.\"; \"tell me a short joke\""), "{h}");
        assert!(!h.contains("reply OK"), "a long pasted message (with its own instruction) is not quoted: {h}");
        assert_eq!(resume_recap(&[]), RESUME_RECAP);
        // at most the first 3 and the last 3, repeats once
        let many: Vec<String> = (1..=9).map(|i| format!("topic {i}")).chain(["topic 9".to_string()]).collect();
        let h = resume_recap(&many);
        assert!(h.contains("\"topic 3\"; \"topic 7\"") && !h.contains("topic 4") && h.matches("topic 9").count() == 1, "{h}");
    }

    #[test]
    fn kinds_in_english_and_vietnamese() {
        for m in ["why?", "Why", "why so?", "tại sao?", "vì sao", "sao vậy?"] { assert_eq!(kind(m), Some(Kind::Why), "{m}"); }
        for m in ["more", "more please", "go on", "tell me more", "tiếp đi", "tiếp tục", "thêm nữa", "nói tiếp đi"] { assert_eq!(kind(m), Some(Kind::More), "{m}"); }
        for (m, n) in [("the second one", 2), ("number 2", 2), ("#3", 3), ("2", 2), ("the 2nd one please", 2), ("option 1", 1), ("the last one", 0),
            ("cái thứ hai", 2), ("số 2", 2), ("thứ 3", 3), ("cái thứ ba đi", 3), ("cái cuối cùng", 0), ("cái đầu tiên", 1)] {
            assert_eq!(kind(m), Some(Kind::Pick(n)), "{m}");
        }
        assert_eq!(kind("and in Tokyo?"), Some(Kind::About("Tokyo".into())));
        assert_eq!(kind("and in Osaka?"), Some(Kind::About("Osaka".into())));
        assert_eq!(kind("what about New York?"), Some(Kind::About("New York".into())));
        assert_eq!(kind("còn Osaka thì sao?"), Some(Kind::About("Osaka".into())));
        assert_eq!(kind("còn ở Osaka thì sao?"), Some(Kind::About("Osaka".into())));
        // not follow-ups
        for m in ["hey", "thanks", "plan a 3-day trip to Tokyo", "and also add unit tests to the parser module please", "why is the sky blue?", "the second law of thermodynamics", "and you?", "how about a joke?", "2 + 2", "what is 2?", "ok"] {
            assert_eq!(kind(m), None, "{m}");
        }
    }

    #[test]
    fn hints_only_with_a_previous_answer_and_short() {
        assert_eq!(hint("why?", false), None, "a new session: no invented context");
        assert_eq!(hint("the second one", false), None);
        assert_eq!(hint("why?", true).as_deref(), Some("(Follow-up: explain the reason behind your last answer. Don't repeat it.)"));
        assert_eq!(hint("tiếp đi", true).as_deref(), Some("(Follow-up: go deeper on your last answer with concrete details; no repeats; say if you're unsure.)"));
        assert!(!line(&Kind::More).contains("new tips"), "no wording that invites made-up content");
        assert_eq!(hint("cái thứ hai", true).as_deref(), Some("(Follow-up: the user picks item 2 from your last answer; tell more about that item only.)"));
        assert_eq!(hint("and in Osaka?", true).as_deref(), Some("(Follow-up: same question as before, now about Osaka.)"));
        // <= 25 tokens at the repo's chars/3.3 estimate
        for k in [Kind::Why, Kind::More, Kind::Pick(2), Kind::Pick(0), Kind::About("New York City".into())] {
            for doc in [false, true] {
                let l = line_for(&k, doc);
                assert!(l.chars().count() as f32 / 3.3 <= 30.0 && l.split_whitespace().count() <= 16, "{l}");
            }
        }
        assert!(PICK_OPTION.chars().count() as f32 / 3.3 <= 30.0);
    }

    /// about an attached document, "continue" must not bring in details that are not in it
    #[test]
    fn document_hints_stay_with_the_document() {
        assert_eq!(line_for(&Kind::More, true), "(Follow-up: continue from the document: the next relevant part, only what it says, no repeats.)");
        assert!(line_for(&Kind::Why, true).ends_with("explain the reason behind your last answer using the document.)"));
        assert!(line_for(&Kind::Pick(2), true).contains("from the document"));
        assert_eq!(line_for(&Kind::About("Osaka".into()), true), line(&Kind::About("Osaka".into())));
        assert_ne!(line_for(&Kind::More, false), line_for(&Kind::More, true));
    }

    /// a bare "ok" after a reply that offered several options must not just pick the first one
    #[test]
    fn yes_after_several_options_asks_which() {
        let several = "TradingAgents beats the baselines.\n\nWould you like me to:\n1. **Create a code example** to build it?\n2. **Generate a visual chart**?\n3. **Help with prompting**?\n\nLet me know!";
        assert_eq!(offered_options(several), 3);
        for m in ["ok", "yes", "Yes!", "sure", "vâng"] { assert_eq!(pick_option(m, several), Some(PICK_OPTION), "{m}"); }
        // the real reply's shape: "Let me know if you'd like to:" and bullets
        assert_eq!(offered_options("Roles: …\n\nLet me know if you'd like to:\n- Generate a **code example**\n- Create a **dashboard**\n\nI can help with that too!"), 2);
        // a list answer closing with a plain "let me know if you have questions" is no offer
        assert_eq!(offered_options("Steps:\n- Install\n- Run\n\nLet me know if you have questions."), 0);
        // items before the question
        assert_eq!(offered_options("I can:\n- summarise section 3\n- compare the tables\nWhich one would you like?"), 2);
        // one offer: a yes goes ahead with it
        let one = "Here is the summary.\n\nWould you like me to write a code example for it?";
        assert_eq!(offered_options(one), 0);
        assert_eq!(pick_option("yes", one), None);
        // a list that is the answer, not an offer
        let answer = "Steps:\n1. Install\n2. Run\n3. Test\n\nHope this helps.";
        assert_eq!(pick_option("ok", answer), None);
        // not a bare yes
        assert_eq!(pick_option("yes, the chart", several), None);
        assert_eq!(pick_option("2", several), None);
    }

    #[test]
    fn strips() {
        assert_eq!(strip("(Follow-up: same question as before, now about Osaka.)\n\nand in Osaka?"), "and in Osaka?");
        assert_eq!(strip("why?"), "why?");
    }
}
