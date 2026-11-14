//! Reply language: Elisa answers in the language the user writes in (English or Vietnamese).
//!
//! The model already does this on its own for a turn in one language (a turn
//! in Vietnamese or English is answered in kind). It slips on a SWITCH: after a
//! Vietnamese turn, "what time is it in Tokyo?" (a routed time turn with the clock line) can come back in Vietnamese. So
//! no system prompt line (it is near its size cap, and the old "use the session's language" line went on 2026-11-08):
//! only in a mixed session (an earlier user turn was in the other language) a turn carries a short hint after the
//! user's words (`repl.rs`), ~6 tokens per such turn; a one-language session never pays. `context::request_body`
//! strips it again.
//!
//! Detection is deterministic and cheap: Vietnamese = a letter only Vietnamese uses (đ ă ơ ư or a tone-marked vowel
//! in U+1EA0..U+1EF9) or a common Vietnamese word ("xin chào"); English = Latin letters only plus a common English word,
//! or a word of Elisa's English small-talk example lines ("ok thx", "sup": `data/small_talk.txt`) with no Vietnamese
//! word beside it; else Vietnamese typed without marks when most of its words are words of Elisa's Vietnamese example
//! lines without their marks (`data/*.txt`). Anything else (other languages, code) is unknown and gets no hint.

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

/// Vietnamese by its own letters or a common Vietnamese word (`detect`'s first rule)
fn marked_vi(text: &str) -> bool { text.chars().any(is_vi_char) || words(text).any(|w| VI_WORDS.contains(&w.as_str())) }

/// `c` lowercase without its Vietnamese marks ("ạ" → 'a', "đ" → 'd'): how unaccented Vietnamese is typed.
pub(crate) fn fold_mark(c: char) -> char {
    const FOLDS: &[(char, &str)] = &[('a', "àáảãạăằắẳẵặâầấẩẫậ"), ('e', "èéẻẽẹêềếểễệ"), ('i', "ìíỉĩị"), ('o', "òóỏõọôồốổỗộơờớởỡợ"),
        ('u', "ùúủũụưừứửữự"), ('y', "ỳýỷỹỵ"), ('d', "đ")];
    let l = c.to_lowercase().next().unwrap_or(c);
    FOLDS.iter().find(|(_, v)| v.contains(l)).map_or(l, |(b, _)| *b)
}

/// The words of Elisa's Vietnamese example lines (`data/small_talk.txt`, `data/tool_asks.txt`: the lines with
/// Vietnamese letters), without their marks.
fn vi_vocabulary() -> &'static std::collections::HashSet<String> {
    static V: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| include_str!("../data/small_talk.txt").lines().chain(include_str!("../data/tool_asks.txt").lines())
        .filter(|l| !l.starts_with('#') && marked_vi(l))
        .flat_map(|l| plain_words(l).into_iter().map(|w| w.chars().map(fold_mark).collect::<String>()).collect::<Vec<_>>())
        .collect())
}

/// The words of `text` outside file names and paths ("notes.txt", "src/app.py" are no word of any language).
fn plain_words(text: &str) -> Vec<String> {
    text.split_whitespace().filter(|t| {
        let t = t.trim_matches(|c: char| !c.is_alphanumeric());
        !t.contains(['/', '\\']) && !t.split_once('.').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
    }).flat_map(|t| t.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>()).collect()
}

/// Vietnamese typed without marks ("hom nay la thu may?", "tim tren mang: thu do cua Uc la gi?"): at least half of
/// its words, and two or more, are words of Elisa's Vietnamese example lines without their marks.
fn unmarked_vi(text: &str) -> bool {
    let w: Vec<String> = plain_words(text).into_iter().map(|w| w.chars().map(fold_mark).collect()).collect();
    let hits = w.iter().filter(|x| vi_vocabulary().contains(*x)).count();
    hits >= 2 && 2 * hits >= w.len()
}

/// Whether `text` holds two Vietnamese words in a row, typed without marks ("cam on", "chao buoi sang"): words of
/// Elisa's Vietnamese example lines without their marks or Vietnamese function words (`vi_function_words`), neither a common English word ("on the" is English: "the").
/// One such word alone ("on") may be English.
pub(crate) fn vi_run(text: &str) -> bool {
    let w: Vec<String> = plain_words(text).into_iter().map(|w| w.chars().map(fold_mark).collect()).collect();
    w.windows(2).any(|p| p.iter().all(|x| (vi_vocabulary().contains(x) || vi_function_words().contains(x)) && !EN_WORDS.contains(&x.as_str())))
}

/// the sections of `data/small_talk.txt` whose lines are what a user says (the rest: asks, Elisa's replies, pitches)
const EN_TALK_SECTIONS: &[&str] = &["[small_talk]", "[thanks]", "[bye]", "[greet_words]", "[checkin_words]", "[wellbeing]", "[wellbeing_answers]"];

/// The words of Elisa's English small-talk example lines (`data/small_talk.txt`, the sections of what a user says,
/// `EN_TALK_SECTIONS`, and her own "en:" reply lines: "ok thx", "sup Elisa", "see you later"): a line of two words or
/// more with no Vietnamese letter, not all of whose words are Vietnamese words typed without marks ("cam on nhe",
/// "ban roi" are Vietnamese lines). A word that is also such a Vietnamese word ("on"), a bare acknowledgement ("ok":
/// `route::ack`, said in both languages) or one letter long is left out: it tells no language.
fn en_small_talk_words() -> &'static std::collections::HashSet<String> {
    static V: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut out = std::collections::HashSet::new();
        let mut talk = false;
        for l in include_str!("../data/small_talk.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { talk = EN_TALK_SECTIONS.contains(&l); continue; }
            let (own, l) = match l.strip_prefix("en:") { Some(r) => (true, r), None => (false, l) };
            if !(talk || own) || l.starts_with("vi:") || marked_vi(l) { continue; }
            let w = plain_words(l);
            if w.len() < 2 || w.iter().all(|x| vi_vocabulary().contains(&x.chars().map(fold_mark).collect::<String>())) { continue; }
            out.extend(w.into_iter().map(|x| x.to_lowercase()).filter(|x| x.chars().count() >= 2 && !vi_vocabulary().contains(x) && !crate::route::ack(x)));
        }
        out
    })
}

/// English small talk by Elisa's own example lines ("ok thx", "sup"): more of its words are English small-talk
/// words (`en_small_talk_words`) than Vietnamese words typed without marks, and at least one is.
fn en_small_talk(text: &str) -> bool {
    let w: Vec<String> = plain_words(text).into_iter().map(|w| w.to_lowercase()).collect();
    let en = w.iter().filter(|x| en_small_talk_words().contains(*x)).count();
    let vi = w.iter().filter(|x| vi_vocabulary().contains(&x.chars().map(fold_mark).collect::<String>())).count();
    en >= 1 && en > vi
}

/// Vietnamese function words typed without marks (`data/reply_terms.txt`, the `plain_function` key of its [vi]
/// section: "nao", "nhi", "khong", "cua").
pub(crate) fn vi_function_words() -> &'static std::collections::HashSet<String> {
    static V: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| include_str!("../data/reply_terms.txt").lines().filter_map(|l| l.trim().strip_prefix("plain_function:"))
        .flat_map(|v| v.split('|').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect::<Vec<_>>()).collect())
}

/// Vietnamese function words a message needs before they alone make it Vietnamese typed without marks (`function_vi`)
const FUNCTION_VI_MIN: usize = 3;

/// Vietnamese typed without marks by its function words ("gia vang hom nay the nao nhi": hom, nay, nao, nhi):
/// FUNCTION_VI_MIN or more of them (`vi_function_words`), at least half the message's words and more than its common
/// English words. "the" is also "thế" without its mark, so it decides nothing here: "the nao" alone tells too little,
/// and an English sentence has English words round its one or two look-alikes ("a la carte, for the whole table").
fn function_vi(text: &str) -> bool {
    let w: Vec<String> = plain_words(text).into_iter().map(|w| w.to_lowercase()).collect();
    let f = w.iter().filter(|x| vi_function_words().contains(*x)).count();
    let en = w.iter().filter(|x| EN_WORDS.contains(&x.as_str())).count();
    f >= FUNCTION_VI_MIN && 2 * f >= w.len() && f > en
}

/// Common English words that are Vietnamese words too when typed without marks ("a", "an", "me", "as": `EN_WORDS`
/// leaves them out), from `data/reply_terms.txt` (`plain_english`): they tell English only beside words no Vietnamese
/// syllable can be (`plain_english`).
fn en_function_words() -> &'static std::collections::HashSet<String> {
    static V: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| include_str!("../data/reply_terms.txt").lines().filter_map(|l| l.trim().strip_prefix("plain_english:"))
        .flat_map(|v| v.split('|').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect::<Vec<_>>()).collect())
}

/// Whether `w` is a function word of English or Vietnamese ("the", "you", "a", "bạn", "của"): a word that says nothing
/// of what a text is about.
pub(crate) fn function_word(w: &str) -> bool {
    let l = w.to_lowercase();
    EN_WORDS.contains(&l.as_str()) || en_function_words().contains(&l) || vi_function_words().contains(&l.chars().map(fold_mark).collect::<String>())
}

/// The parts of a Vietnamese syllable as typed without marks: the onsets it writes (or none), the codas (or none).
const VI_ONSETS: &[&str] = &["ngh", "ch", "gh", "gi", "kh", "ng", "nh", "ph", "qu", "th", "tr", "b", "c", "d", "g", "h", "k", "l", "m", "n", "p", "r", "s", "t", "v", "x", ""];
const VI_CODAS: &[&str] = &["", "c", "ch", "m", "n", "ng", "nh", "p", "t"];
/// most vowels in a row a Vietnamese syllable has ("khuya", "yeu")
const VI_VOWELS_MAX: usize = 3;

/// Whether a word of plain letters could be a Vietnamese syllable typed without its marks: an onset Vietnamese writes
/// (or none), one to three vowels, a coda it writes (or none), and nothing else. "tac", "an", "toe", "minh" could be;
/// "make", "simple", "html", "as" and "file" could not: no Vietnamese syllable ends in a consonant cluster, in "s",
/// "l" or "k", or in a vowel after its coda, and none has an "f", "j", "w" or "z".
pub(crate) fn vi_syllable(w: &str) -> bool {
    let w = w.to_lowercase();
    if w.is_empty() || !w.chars().all(|c| c.is_ascii_lowercase()) { return false; }
    let vowel = |c: char| "aeiouy".contains(c);
    VI_ONSETS.iter().filter(|o| w.starts_with(**o)).any(|o| {
        let rest = &w[o.len()..];
        let n = rest.chars().take_while(|c| vowel(*c)).count();
        (1..=VI_VOWELS_MAX).contains(&n) && VI_CODAS.contains(&&rest[n..])
    })
}

/// words no Vietnamese syllable can be that a text needs before its plain English words tell its language
const PLAIN_ENGLISH_SHAPES: usize = 2;
/// the common English words it needs then ("me", "a", "as", "an" in "make me a quiz page as an html file")
const PLAIN_ENGLISH_WORDS: usize = 2;

/// Whether `text` is plain English with none of the common English words `EN_WORDS` lists ("make me a small quiz page
/// as an html file" has only ones Vietnamese shares when typed without marks): two words at least that no Vietnamese
/// syllable can be (`vi_syllable`), two of the shared common words (`en_function_words`), and no Vietnamese word of
/// three letters or more that is not an English one too ("lam cho minh game bang html" has "lam", "cho", "minh").
/// Another Latin-script language ("Hallo, wie geht es dir?") has the shapes but not the words.
fn plain_english(text: &str) -> bool {
    if text.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) { return false; }
    let w: Vec<String> = plain_words(text).into_iter().map(|w| w.to_lowercase()).collect();
    let shapes = w.iter().filter(|x| !vi_syllable(x)).count();
    let en = w.iter().filter(|x| en_function_words().contains(*x)).count();
    let english = |x: &String| EN_WORDS.contains(&x.as_str()) || en_function_words().contains(x);
    let vi = w.iter().filter(|x| x.chars().count() >= 3 && vi_syllable(x) && !english(x) && (vi_function_words().contains(*x) || vi_vocabulary().contains(*x))).count();
    shapes >= PLAIN_ENGLISH_SHAPES && en >= PLAIN_ENGLISH_WORDS && vi == 0
}

/// The language of `text` when it is clearly English or Vietnamese (Vietnamese with or without its marks).
pub fn detect(text: &str) -> Option<Lang> {
    if marked_vi(text) { return Some(Lang::Vi); }
    // English: no letters outside ASCII, and a common English word
    if text.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) { return None; }
    // (unless its Vietnamese function words outweigh that word: "the" in "cai nay the nao" is "thế")
    if function_vi(text) { return Some(Lang::Vi); }
    let en = words(text).any(|w| EN_WORDS.contains(&w.as_str()));
    if en || en_small_talk(text) || plain_english(text) { return Some(Lang::En); }
    unmarked_vi(text).then_some(Lang::Vi)
}

/// Whether `text` has a common English word (`EN_WORDS`).
pub fn has_english_word(text: &str) -> bool { words(text).any(|w| EN_WORDS.contains(&w.as_str())) }

/// The hint for a user turn in `now` given the session's earlier user turns: only in a mixed session, when an earlier
/// turn was in the other known language (one hint on the switch turn alone is not enough: the turn after
/// it, in the same language, drifts back), so a one-language session never pays for it.
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
    fn a_file_name_is_no_vietnamese_word() {
        // ("notes.txt" is in a Vietnamese example line: "read notes.txt" was taken for Vietnamese)
        for t in ["read notes.txt", "open src/app.py", "summarize notes.txt"] { assert_ne!(detect(t), Some(Lang::Vi), "{t:?}"); }
        for t in ["kiem tra requirements.txt co goi nao bi lo hong khong", "doc tep notes.txt giup minh"] { assert_eq!(detect(t), Some(Lang::Vi), "{t:?}"); }
    }

    #[test]
    fn detects_vietnamese_typed_without_marks() {
        for t in ["hom nay la thu may?", "hom nay la thuu may?", "tim tren mang: thu do cua Uc la gi?", "chay doan python nay: print(sum(range(11)))", "an com do"] {
            assert_eq!(detect(t), Some(Lang::Vi), "{t:?}");
        }
        for t in ["what day is it today?", "do you have a minute?", "run this python: print(2)", "print(sum(range(11)))"] {
            assert_ne!(detect(t), Some(Lang::Vi), "{t:?}");
        }
    }

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
        // French accents, code, numbers: no guess (unaccented Vietnamese is Vietnamese: `detects_vietnamese_typed_without_marks`)
        for t in ["où est la gare", "fn main() {}", "42", "", "café crème"] {
            assert_eq!(detect(t), None, "{t}");
        }
        // (plain English with none of the common words listed, "make me … as an html file", was no known language: no
        // hint went to the model in a Vietnamese chat, and a Vietnamese reply to it passed)
        for t in ["make me a simple tic-tac-toe game as an html file", "make me a small quiz page as an html file", "give me three fun facts on penguins", "explain photosynthesis simply, like a teacher"] {
            assert_eq!(detect(t), Some(Lang::En), "{t:?}");
        }
        // (unaccented Vietnamese round an English loanword, another Latin-script language, code: still not English)
        for t in ["lam cho minh game tic tac toe bang html", "Hallo, wie geht es dir? Ich brauche Hilfe.", "hola amigos, como estan hoy", "fn main() {}"] {
            assert_ne!(detect(t), Some(Lang::En), "{t:?}");
        }
        for w in ["tac", "an", "toe", "minh", "nghe", "khuya", "qua"] { assert!(vi_syllable(w), "{w}"); }
        for w in ["make", "simple", "html", "as", "file", "quiz", "small", "Hanoi", "fn", ""] { assert!(!vi_syllable(w), "{w:?}"); }
    }

    #[test]
    fn english_small_talk_is_english_by_elisas_own_examples() {
        // ("ok thx" was no known language, so a Russian or Vietnamese reply to it passed)
        for t in ["ok thx", "thx", "sup", "k thx", "ok bye", "see ya later"] { assert_eq!(detect(t), Some(Lang::En), "{t:?}"); }
        // a bare acknowledgement is said in both languages, "alo" is Vietnamese, and Vietnamese words beside an
        // English one outweigh it
        for t in ["ok", "ok ok", "alo"] { assert_eq!(detect(t), None, "{t:?}"); }
        for t in ["ban roi", "thx ban oi", "cam on"] { assert_eq!(detect(t), Some(Lang::Vi), "{t:?}"); }
        assert_eq!(hint(["xin chào", "đọc file notes.txt"], "ok thx"), Some(HINT_EN), "a closer follows the current message's language");
    }

    #[test]
    fn unaccented_vietnamese_round_an_english_look_alike_is_vietnamese() {
        // ("the" is "thế" typed without marks: these were read as English, by the word "the")
        for t in ["gia vang hom nay the nao nhi", "cai nay the nao", "hom nay the nao", "ban co the giup minh khong"] {
            assert_eq!(detect(t), Some(Lang::Vi), "{t:?}");
        }
        // ("the nao" alone tells too little; an English sentence has English words round a look-alike or two)
        for t in ["the nao", "What is the capital of France", "is the file there?", "a la carte, for the whole table", "the co-founder may call"] {
            assert_ne!(detect(t), Some(Lang::Vi), "{t:?}");
        }
        assert_eq!(detect("the nao"), Some(Lang::En));
    }

    #[test]
    fn two_vietnamese_words_in_a_row_typed_without_marks() {
        assert!(vi_run("In Vietnamese, 'thank you' is 'cam on'."));
        assert!(vi_run("chao buoi sang"));
        assert!(!vi_run("It is on the table."));
        assert!(!vi_run("Thank you very much."));
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
