//! Reply checks that hold for any model and any language, from the text alone (Unicode scripts, Elisa's own prompt
//! text, the user's message): nothing here lists a wording.
//!
//! - garbled text (`garbled`): one word written in two or more non-Latin scripts at once ("కొ้มπόt": Telugu, Thai and
//!   Greek), or a short piece of a script the user did not write in, repeated over and over ("งานียงานียงานีย…").
//!   No language writes either. `repair` takes them out when asking again did not help.
//! - prompt echo (`Echo`): a sentence of the reply that copies a run of words from Elisa's own system prompt (its
//!   model facts line, its rules) that this turn's message does not hold too.
//! - reply language (`lang_mismatch`): a reply, or one of its sentences, plainly not in the language of the user's
//!   message (judged by the message's script and its accented letters, and `reply_lang::detect`).
//! - an unfinished last sentence (`ends_mid_sentence`): the reply stopped right after the start of a new sentence.

use std::collections::{HashMap, HashSet};

const LATIN: u32 = 0;
const GREEK: u32 = 1;
const CYRILLIC: u32 = 2;
/// Han, kana and Hangul: written together in one word (Japanese, Korean with Han)
const EAST_ASIAN: u32 = 3;
const ARABIC: u32 = 4;
const HEBREW: u32 = 5;

/// The writing system of a letter (from its Unicode block), None for anything that is not a letter.
fn script(c: char) -> Option<u32> {
    if !c.is_alphabetic() { return None; }
    let u = c as u32;
    Some(match u {
        0..=0x036F | 0x1D00..=0x1DFF | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF | 0xAB30..=0xAB6F | 0xFB00..=0xFB06 | 0xFF21..=0xFF5A => LATIN,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => GREEK,
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => CYRILLIC,
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => HEBREW,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x0870..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => ARABIC,
        0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3040..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF | 0xFF66..=0xFFDC | 0x20000.. => EAST_ASIAN,
        // the Indic, Thai and Lao blocks are 128 code points each, one script per block
        0x0900..=0x0EFF => 0x1000 + (u >> 7),
        _ => 0x2000 + (u >> 8),
    })
}

/// a char that ends a word: white space and punctuation (marks inside a word stay with it)
fn word_break(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation() || matches!(c as u32, 0x2000..=0x206F | 0x3000..=0x303F | 0xFF01..=0xFF0F | 0xFF1A..=0xFF20)
}

/// a word written in two or more non-Latin scripts at once
fn mixed_word(w: &str) -> bool {
    let mut first = None;
    w.chars().filter_map(script).filter(|&s| s != LATIN).any(|s| *first.get_or_insert(s) != s)
}

/// the scripts `text`'s letters are written in
fn scripts(text: &str) -> HashSet<u32> { text.chars().filter_map(script).collect() }

/// shortest run of one repeated piece that counts as garbage (chars), and the fewest repeats
const RUN_MIN_CHARS: usize = 24;
const RUN_MIN_REPEATS: usize = 8;
/// longest repeated piece looked for (chars)
const RUN_UNIT_MAX: usize = 8;

/// The char ranges of `text` where a short piece with a letter of a script not in `own` repeats again and again.
fn foreign_runs(text: &str, own: &HashSet<u32>) -> Vec<(usize, usize)> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let mut found = 0;
        for l in 1..=RUN_UNIT_MAX.min(c.len() - i) {
            let unit = &c[i..i + l];
            if !unit.iter().filter_map(|&ch| script(ch)).any(|s| !own.contains(&s)) { continue; }
            let mut k = 1;
            while i + (k + 1) * l <= c.len() && c[i + k * l..i + (k + 1) * l] == *unit { k += 1; }
            if k >= RUN_MIN_REPEATS && k * l >= RUN_MIN_CHARS { found = found.max(k * l); }
        }
        if found > 0 { out.push((i, i + found)); i += found; } else { i += 1; }
    }
    out
}

/// Whether `reply` holds text no language writes: a word in two or more non-Latin scripts, or a short piece of a
/// script `ask` does not use, repeated many times in a row.
pub fn garbled(reply: &str, ask: &str) -> bool {
    reply.split(word_break).any(mixed_word) || !foreign_runs(reply, &scripts(ask)).is_empty()
}

/// `reply` without its garbled words and runs (`garbled`); empty when they were most of it.
pub fn repair(reply: &str, ask: &str) -> String {
    let letters = |t: &str| t.chars().filter(|c| c.is_alphabetic()).count();
    let runs = foreign_runs(reply, &scripts(ask));
    let chars: Vec<char> = reply.chars().collect();
    let mut kept = String::new();
    let mut i = 0;
    for (a, b) in runs { kept.extend(&chars[i..a]); i = b; }
    kept.extend(&chars[i..]);
    let lines: Vec<String> = kept.lines().map(|l| {
        let mut out = String::new();
        let mut w = String::new();
        for ch in l.chars().chain(std::iter::once('\n')) {
            if word_break(ch) {
                if !mixed_word(&w) { out.push_str(&w); }
                w.clear();
                if ch != '\n' { out.push(ch); }
            } else { w.push(ch); }
        }
        out.split(' ').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
    }).collect();
    let out = lines.join("\n").trim().to_string();
    if letters(&out) * 4 < letters(reply) { String::new() } else { out }
}

/// consecutive words of Elisa's prompt a reply sentence must copy to be an echo of it
const ECHO_WORDS: usize = 6;

fn words(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

fn grams(t: &str) -> HashSet<String> {
    words(t).windows(ECHO_WORDS).map(|g| g.join(" ")).collect()
}

/// A text's sentences, split after '.', '!' or '?' followed by a space (the rest of a line is its last sentence).
fn sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = line.as_bytes();
    for (i, ch) in line.char_indices() {
        let end = i + ch.len_utf8();
        if matches!(ch, '.' | '!' | '?') && (end == line.len() || b[end] == b' ') {
            out.push(&line[start..end]);
            start = end;
        }
    }
    if start < line.len() { out.push(&line[start..]); }
    out
}

/// The word runs of Elisa's own system prompt (`own`: the prompt less the user's project instructions) that this
/// turn's message (`turn`: everything sent in it, Elisa's lines included) does not hold: a reply that repeats one
/// copied the prompt, not the conversation.
#[derive(Debug, Default)]
pub struct Echo { grams: HashSet<String> }

impl Echo {
    pub fn new(own: &str, turn: &str) -> Echo {
        let t = grams(turn);
        Echo { grams: grams(own).into_iter().filter(|g| !t.contains(g)).collect() }
    }

    /// `reply` without the sentences that copy the prompt (unchanged when none does)
    pub fn strip(&self, reply: &str) -> String {
        let hit = |s: &str| grams(s).iter().any(|g| self.grams.contains(g));
        if self.grams.is_empty() || !reply.lines().any(|l| sentences(l).into_iter().any(hit)) { return reply.to_string(); }
        let mut out: Vec<String> = Vec::new();
        for line in reply.lines() {
            let kept: String = sentences(line).into_iter().filter(|s| !hit(s)).collect();
            if kept.trim().is_empty() && !line.trim().is_empty() { continue; }
            // (one blank line where the copied lines stood between blank lines)
            if kept.trim().is_empty() && out.last().is_some_and(|l| l.is_empty()) { continue; }
            out.push(kept.trim().to_string());
        }
        out.join("\n").trim().to_string()
    }
}

/// How a text is written: its letters per script, and its words with a non-ASCII Latin letter (an accent).
struct Profile { letters: usize, per_script: HashMap<u32, usize>, words: usize, marked_words: usize, marked: usize }

fn profile(t: &str) -> Profile {
    let mut p = Profile { letters: 0, per_script: HashMap::new(), words: 0, marked_words: 0, marked: 0 };
    for w in t.split(word_break).filter(|w| w.chars().any(char::is_alphabetic)) {
        p.words += 1;
        for s in w.chars().filter_map(script) {
            p.letters += 1;
            *p.per_script.entry(s).or_default() += 1;
        }
        let m = w.chars().filter(|c| c.is_alphabetic() && !c.is_ascii() && script(*c) == Some(LATIN)).count();
        p.marked += m;
        if m > 0 { p.marked_words += 1; }
    }
    p
}

impl Profile {
    fn main_script(&self) -> Option<u32> { self.per_script.iter().max_by_key(|(_, n)| **n).map(|(s, _)| *s) }
}

/// prose of a reply: code blocks, inline code and links left out (they are not in the user's language)
fn prose(reply: &str) -> String {
    let mut out = String::new();
    let mut fence = false;
    for l in reply.lines() {
        if l.trim_start().starts_with("```") { fence = !fence; continue; }
        if fence { continue; }
        let mut tick = false;
        let line: String = l.chars().filter(|&c| { if c == '`' { tick = !tick; } !tick && c != '`' }).collect();
        let line = line.split(' ').filter(|w| !w.contains("://")).collect::<Vec<_>>().join(" ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// letters a reply sentence needs before its language is judged against the message's script
const SCRIPT_MIN_LETTERS: usize = 12;
/// share of the message's words with an accented letter from which accents mark its language
const MARKED_WORDS_MIN: f32 = 0.4;
/// accented letters a sentence of the message's language would be expected to hold at least, before having none
/// counts as another language (the whole reply: one less)
const MARKS_EXPECTED: f32 = 3.0;

/// Whether `reply` (or one of its sentences) is plainly not in the language of `ask`: the message is mostly in a
/// non-Latin script and the reply's sentence is not, or the message's words mostly carry accents and a long enough
/// reply sentence has none; or `reply_lang::detect` tells the two apart.
pub fn lang_mismatch(ask: &str, reply: &str) -> bool {
    let a = profile(ask);
    if a.letters < 4 { return false; }
    let text = prose(reply);
    let r = profile(&text);
    if r.letters == 0 { return false; }
    let main = a.main_script();
    if main.is_some_and(|s| s != LATIN) && a.per_script.get(&main.unwrap_or(LATIN)).is_some_and(|n| *n * 2 > a.letters) {
        let s = main.unwrap_or(LATIN);
        let off = |p: &Profile| p.letters >= SCRIPT_MIN_LETTERS && p.per_script.get(&s).copied().unwrap_or(0) * 4 < p.letters;
        return off(&r) || text.lines().flat_map(sentences).any(|x| off(&profile(x)));
    }
    if a.words == 0 || (a.marked_words as f32 / a.words as f32) < MARKED_WORDS_MIN { return false; }
    // a reply mostly in a script the message does not use at all ("元気だよ!" to "bạn khỏe không?")
    let foreign: usize = r.per_script.iter().filter(|(s, _)| !a.per_script.contains_key(s)).map(|(_, n)| n).sum();
    if foreign * 2 > r.letters { return true; }
    let density = a.marked as f32 / a.letters as f32;
    if let (Some(x), Some(y)) = (crate::reply_lang::detect(ask), crate::reply_lang::detect(&text)) { if x != y { return true; } }
    let unmarked = |p: &Profile, need: f32| p.marked == 0 && p.letters as f32 * density >= need;
    unmarked(&r, MARKS_EXPECTED - 1.0) || text.lines().flat_map(sentences).any(|x| unmarked(&profile(x), MARKS_EXPECTED))
}

/// chars of the user's message quoted in the language note
const NOTE_QUOTE_CHARS: usize = 40;

/// The note added to the message (that request only) when the reply came back in another language: the
/// Vietnamese hint for a Vietnamese message, else a note quoting the message's start (a small model follows a
/// sample of the language better than "the language above").
pub fn lang_note(ask: &str) -> std::borrow::Cow<'static, str> {
    match crate::reply_lang::detect(ask) {
        Some(crate::reply_lang::Lang::Vi) => crate::reply_lang::HINT_VI.trim_start().into(),
        _ => {
            let q: String = ask.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(NOTE_QUOTE_CHARS).collect();
            format!("(Reply in the language of \"{q}\".)").into()
        }
    }
}

/// Whether a finished reply stops right after starting a new sentence: its last line has an earlier sentence end
/// and goes on for a few words with no end of its own ("How about a quick story? Let").
pub fn ends_mid_sentence(reply: &str) -> bool {
    // (inside or right after a code block: not prose)
    if reply.lines().filter(|l| l.trim_start().starts_with("```")).count() % 2 == 1 || reply.trim_end().ends_with("```") { return false; }
    let Some(last) = reply.lines().rev().find(|l| !l.trim().is_empty()) else { return false };
    let last = last.trim();
    if !last.chars().last().is_some_and(char::is_alphabetic) { return false; }
    let s = sentences(last);
    // (a list number's dot is no sentence end: "1. Kyoto 2. Nara 3. Osaka")
    let real_end = s.len() >= 2 && s[s.len() - 2].split_whitespace().last().is_some_and(|w| !w.trim_end_matches(['.', '!', '?']).chars().all(|c| c.is_ascii_digit()));
    real_end && s.last().is_some_and(|t| t.split_whitespace().count() <= 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_scripts_in_one_word_are_garbled() {
        assert!(garbled("Món bánh కొ้มπόt rất ngon.", "món gì ngon?"));
        // real text in one script, or a non-Latin script next to Latin, is not
        for t in ["Bánh mì rất ngon.", "東京タワーへ行きました。", "서울에서 만나요", "Привет, как дела?", "用Python写代码", "πr² is the area", "नमस्ते दोस्त", "สวัสดีครับ"] {
            assert!(!garbled(t, "hello"), "{t}");
        }
    }

    #[test]
    fn a_foreign_piece_repeated_is_garbled() {
        let moon = format!("รามา{}", "ยงานี".repeat(40));
        assert!(garbled(&moon, "xin chào, bạn khỏe không?"));
        // the same script as the ask, or a short repeat: no
        assert!(!garbled(&"ha".repeat(30), "write ha 30 times"));
        assert!(!garbled("ฮ่าฮ่าฮ่า", "hello"));
        assert!(!garbled(&"=".repeat(40), "hello"));
        assert_eq!(repair(&moon, "xin chào"), "", "a reply that was mostly garbage has nothing left");
        assert_eq!(repair("Món bánh కొ้มπόt rất ngon.", "món gì ngon?"), "Món bánh rất ngon.");
    }

    #[test]
    fn a_copied_prompt_sentence_is_stripped() {
        let own = "You are Elisa, a friendly assistant.\nModel facts (detected, true now): Elisa runs on gemma3:1b · vision: no · tool calling: no · context 8192 tokens.";
        let e = Echo::new(own, "hi");
        let r = "Model facts (detected, true now): Elisa runs on gemma3:1b · vision: no · tool calling: no · context 8192 tokens. Hi! How can I help?";
        assert_eq!(e.strip(r), "Hi! How can I help?");
        assert_eq!(e.strip("Hello there, nice to meet you."), "Hello there, nice to meet you.");
        let r = "Here:\n\nYou are Elisa, a friendly assistant: you chat.\n\nModel facts (detected, true now): Elisa runs on gemma3:1b · vision: no.\n\nA joke!";
        assert_eq!(e.strip(r), "Here:\n\nA joke!");
        // the turn's own message holds the same words (a question about the model got the facts): kept
        let e = Echo::new(own, "(Situation: Elisa runs on gemma3:1b · vision: no · tool calling: no · context 8192 tokens) what model?");
        assert_eq!(e.strip("Elisa runs on gemma3:1b · vision: no · tool calling: no · context 8192 tokens."), "Elisa runs on gemma3:1b · vision: no · tool calling: no · context 8192 tokens.");
    }

    #[test]
    fn a_reply_in_another_language_is_caught() {
        assert!(lang_mismatch("xin chào", "Hello!"));
        assert!(lang_mismatch("xin chào, bạn khỏe không?", "Hello! I'm doing well, thank you. How about you?"));
        assert!(lang_mismatch("xin chào", "Okay, what's up? 😊"));
        // a Vietnamese reply with an English sentence in it
        assert!(lang_mismatch("bạn khỏe không?", "Tôi khỏe, cảm ơn bạn! How can I help you today with anything?"));
        assert!(lang_mismatch("Привет, как дела?", "Hello! I am fine, thank you for asking."));
        // another script altogether, however short
        assert!(lang_mismatch("Xin chào, bạn khỏe không?", "元気だよ! 😊"));
        assert!(!lang_mismatch("xin chào", "Chào bạn! Mình có thể giúp gì?"));
        assert!(!lang_mismatch("Привет", "Привет! Как дела?"));
        assert!(!lang_mismatch("hello", "Hello! How can I help?"));
        // an English message with a Vietnamese name, or French with few accents: not judged by accents
        assert!(!lang_mismatch("Tell me about Hồ Chí Minh city", "It is the largest city in Vietnam, in the south."));
        assert!(!lang_mismatch("où est la gare", "La gare est au centre de la ville, tout droit."));
        // code is not prose
        assert!(!lang_mismatch("viết hàm cộng hai số", "Đây là hàm:\n```\nfn add(a: i32, b: i32) -> i32 { a + b }\n```"));
    }

    #[test]
    fn an_unfinished_last_sentence_is_seen() {
        assert!(ends_mid_sentence("Hey, how about a quick story? Let"));
        for t in ["Here is a story.", "Paris", "1. Kyoto 2. Nara 3. Osaka", "- apples\n- pears", "Done. See `main.rs`", "Wow! 😊", "Hi! I am Elisa and I like long walks on the beach"] {
            assert!(!ends_mid_sentence(t), "{t}");
        }
    }
}
