//! Reply language: Elisa answers in the language the user writes in (English or Vietnamese).
//!
//! The model already does this on its own for a turn in one language (measured on qwen3-vl:2b-instruct, 2026-11-09:
//! "xin chào", "thủ đô của Pháp là gì", "tell me a joke" … all answered in kind). It slipped on a SWITCH: after a
//! Vietnamese turn, "what time is it in Tokyo?" (a routed time turn with the clock line) came back in Vietnamese. So
//! no system prompt line (it is near its size cap, and the old "use the session's language" line went on 2026-11-08):
//! only in a mixed session (an earlier user turn was in the other language) a turn carries a short hint after the
//! user's words (`repl.rs`), ~6 tokens per such turn; a one-language session never pays. `context::request_body`
//! strips it again.
//!
//! Detection is deterministic and cheap: Vietnamese = a letter only Vietnamese uses (đ ă ơ ư or a tone-marked vowel
//! in U+1EA0..U+1EF9) or a common Vietnamese word ("xin chào"); English = Latin letters only plus a common English word. Anything else (unaccented Vietnamese,
//! other languages, code) is unknown and gets no hint.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang { En, Vi }

/// the hint after the user's words when the language switched to English
pub const HINT_EN: &str = "\n\n(Reply in English.)";
/// the hint after the user's words when the language switched to Vietnamese
pub const HINT_VI: &str = "\n\n(Trả lời bằng tiếng Việt.)";

/// a letter that is Vietnamese and (practically) no other Latin-script language's
pub fn is_vi_char(c: char) -> bool {
    matches!(c, 'đ' | 'Đ' | 'ă' | 'Ă' | 'ơ' | 'Ơ' | 'ư' | 'Ư' | '\u{1EA0}'..='\u{1EF9}')
}

const EN_WORDS: &[&str] = &[
    // not "a", "an", "do", "to", "no", "me", "on": unaccented Vietnamese words too
    "the", "is", "are", "was", "be", "does", "did", "what", "who", "how", "why", "when", "where", "which",
    "you", "your", "i", "my", "it", "this", "that", "of", "in", "for", "and", "or", "with", "can", "could",
    "would", "will", "please", "tell", "hi", "hello", "hey", "thanks", "thank", "yes", "now", "time", "joke", "about",
];

/// common Vietnamese words written with no Vietnamese-only letter ("xin chào" has only the shared à)
const VI_WORDS: &[&str] = &["xin", "chào", "không", "có", "là", "gì", "tôi", "và", "mà", "nào", "thì", "giúp", "mình", "nhé", "sao"];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphabetic()).map(str::to_lowercase)
}

/// The language of `text` when it is clearly English or Vietnamese.
pub fn detect(text: &str) -> Option<Lang> {
    if text.chars().any(is_vi_char) || words(text).any(|w| VI_WORDS.contains(&w.as_str())) { return Some(Lang::Vi); }
    // English: no letters outside ASCII, and a common English word
    if text.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) { return None; }
    let en = words(text).any(|w| EN_WORDS.contains(&w.as_str()));
    en.then_some(Lang::En)
}

/// The hint for a user turn in `now` given the session's earlier user turns: only in a mixed session, when an earlier
/// turn was in the other known language (measured: one hint on the switch turn alone was not enough, the turn after
/// it, in the same language, drifted back), so a one-language session never pays for it.
pub fn hint<'a>(earlier: impl IntoIterator<Item = &'a str>, now: &str) -> Option<&'static str> {
    let n = detect(now)?;
    if !earlier.into_iter().any(|t| detect(t).is_some_and(|p| p != n)) { return None; }
    Some(match n { Lang::En => HINT_EN, Lang::Vi => HINT_VI })
}

/// `text` without a trailing language hint
pub fn strip_hint(text: &str) -> &str {
    text.strip_suffix(HINT_EN).or_else(|| text.strip_suffix(HINT_VI)).unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vietnamese_by_its_own_letters() {
        for t in ["xin chào", "bạn có thể làm gì?", "kể cho tôi một câu chuyện cười", "thủ đô của Pháp là gì", "ĐƯỢC", "cảm ơn", "xin chao ban"] {
            assert_eq!(detect(t), Some(Lang::Vi), "{t}");
        }
    }

    #[test]
    fn detects_english_and_leaves_the_rest_unknown() {
        for t in ["hello", "what can you do?", "tell me a joke", "What is the capital of France", "it's broken"] {
            assert_eq!(detect(t), Some(Lang::En), "{t}");
        }
        // French accents, unaccented Vietnamese, code, numbers: no guess
        for t in ["où est la gare", "an com do", "fn main() {}", "42", "", "café crème"] {
            assert_eq!(detect(t), None, "{t}");
        }
    }

    #[test]
    fn hints_only_in_a_mixed_session() {
        assert_eq!(hint(["tell me a joke", "cảm ơn, kể chuyện khác đi"], "what time is it in Tokyo?"), Some(HINT_EN));
        assert_eq!(hint(["tell me a joke"], "thủ đô của Pháp là gì"), Some(HINT_VI));
        // the turn after the switch, same language as the one before it: still hinted (the session is mixed)
        assert_eq!(hint(["xin chào", "what is the capital of Japan?"], "tell me a joke"), Some(HINT_EN));
        assert_eq!(hint(["tell me a joke"], "what is the capital of Japan?"), None, "one language: no hint");
        assert_eq!(hint(["xin chào"], "bạn khỏe không"), None);
        assert_eq!(hint([], "hello"), None, "the first turn needs no hint");
        assert_eq!(hint(["xin chào"], "fn main() {}"), None, "unknown language: no hint");
    }

    #[test]
    fn strips_the_hint() {
        assert_eq!(strip_hint(&format!("what time is it?{HINT_EN}")), "what time is it?");
        assert_eq!(strip_hint(&format!("xin chào{HINT_VI}")), "xin chào");
        assert_eq!(strip_hint("plain"), "plain");
    }
}
