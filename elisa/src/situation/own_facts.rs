//! Facts Elisa works out herself for a turn, and a reply checked against them (`ReplyFacts`), so a small model gives
//! the same answer as a big one:
//! - the day and part of the day in the user's language for a question about the time (`clock_words`, from
//!   `data/clock_words.txt`): Elisa's clock line is English, and "22:10 Tuesday evening" came back as "thứ hai
//!   chiều" (Monday afternoon);
//! - exact facts of a table in the turn (an attached or read CSV / spreadsheet) that the question asks for: total,
//!   highest / lowest with its row, average, number of rows (`table_facts`, the asks from `data/table_asks.txt`):
//!   gemma3:1b answered "which region sold the most?" with the wrong row and "the total?" with 1200 for 555;
//! - a reply's numbers against Elisa's computed results ("Elisa computed it exactly: …" in the turn): a wrong one is
//!   put right ("12.5% of 3,480 is 445.0" → 435, and the total 796 added);
//! - a reply that calls the user by a name that belongs to someone or something else in the facts ("my dog is Bo"
//!   → "Nice to meet you, Bo!"): that name goes, or becomes the user's own.

use std::sync::OnceLock;

const CLOCK_DATA: &str = include_str!("../../data/clock_words.txt");
const TABLE_ASKS: &str = include_str!("../../data/table_asks.txt");
const NUMBER_WORDS: &str = include_str!("../../data/number_words.txt");
const COMPARE_ASKS: &str = include_str!("../../data/compare_asks.txt");
const CONFIRM_LINES: &str = include_str!("../../data/confirm_lines.txt");

/// Elisa's own question when she could not work out a number question (`data/confirm_lines.txt`), in the language
/// of the user's words (the file's first one when it is not known).
fn confirm_line(own: &str) -> Option<String> { confirm_line_for(own, "ask") }

/// `confirm_line` by its key in the file ("ask": a number question; "date": a day whose date moves)
fn confirm_line_for(own: &str, key: &str) -> Option<String> {
    let key = format!("{key} =");
    let want = match crate::reply_lang::detect(own) { Some(crate::reply_lang::Lang::Vi) => "[vi]", _ => "[en]" };
    let mut cur = String::new();
    let mut first: Option<String> = None;
    for l in CONFIRM_LINES.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if l.starts_with('[') { cur = l.to_string(); continue; }
        if let Some(v) = l.strip_prefix(key.as_str()).map(str::trim) {
            if cur == want { return Some(v.to_string()); }
            first.get_or_insert_with(|| v.to_string());
        }
    }
    first
}

/// the number words of `data/number_words.txt`: one regex for all, and each (word, value, also an everyday word)
type NumberWords = (regex::Regex, Vec<(String, f64, bool)>);

/// every wording of `data/number_words.txt` as one regex alternation, the longest first
fn number_alternation() -> String {
    let mut words: Vec<String> = NUMBER_WORDS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=')).flat_map(|(_, ws)| ws.split('|').map(|w| w.trim().trim_end_matches('?').to_lowercase()).collect::<Vec<_>>())
        .filter(|w| !w.is_empty()).collect();
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    words.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join("|")
}

/// A number said in words, up to 99: one wording ("mười lăm"), a tens and a unit ("hai mươi chín", "twenty-nine"),
/// or two digits said one after the other ("hai chín" = 29). None for anything else.
fn spoken_number(text: &str) -> Option<u32> {
    let v: Vec<u32> = word_numbers(text).into_iter().map(|(_, _, n, _)| n as u32).collect();
    spoken_value(&v)
}

/// whether `text` is one number word that is also an everyday word ("bay" in "bay gio", "now" without accents)
fn everyday_number(text: &str) -> bool {
    matches!(word_numbers(text).as_slice(), [(_, _, _, true)])
}

fn spoken_value(v: &[u32]) -> Option<u32> {
    match v {
        [a] => Some(*a),
        [t, u] if *t >= 20 && t % 10 == 0 && *u < 10 => Some(t + u),
        [a, b] if *a < 10 && *b < 10 => Some(a * 10 + b),
        _ => None,
    }
}

/// The numbers written as words in a text (`data/number_words.txt`): (start, end, value, also an everyday word).
fn word_numbers(text: &str) -> Vec<(usize, usize, f64, bool)> {
    static W: OnceLock<NumberWords> = OnceLock::new();
    let (re, words) = W.get_or_init(|| {
        let mut words: Vec<(String, f64, bool)> = Vec::new();
        for l in NUMBER_WORDS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Some((n, ws)) = l.split_once('=') else { continue };
            let Ok(v) = n.trim().parse::<f64>() else { continue };
            for w in ws.split('|').map(str::trim).filter(|w| !w.is_empty()) {
                let amb = w.ends_with('?');
                words.push((w.trim_end_matches('?').to_lowercase(), v, amb));
            }
        }
        // the longest wording first ("mười một" before "mười")
        words.sort_by_key(|(w, _, _)| std::cmp::Reverse(w.chars().count()));
        let alt = words.iter().map(|(w, _, _)| regex::escape(w)).collect::<Vec<_>>().join("|");
        (regex::Regex::new(&format!(r"(?i)\b(?:{alt})\b")).unwrap(), words)
    });
    re.find_iter(text).filter_map(|m| {
        let low = m.as_str().to_lowercase();
        words.iter().find(|(w, _, _)| *w == low).map(|(_, v, a)| (m.start(), m.end(), *v, *a))
    }).collect()
}

/// how every computed-facts part starts: the reply check reads Elisa's results back from it
pub const COMPUTED: &str = "Elisa computed it exactly";
/// what a computed-facts part asks of the model (one result, several)
const USE_ONE: &str = "answer with this result";
const USE_ALL: &str = "answer with these results";
/// what the clock words ask of the model
const CLOCK_USE: &str = "say the time, day and part of the day with exactly these words";

// ----- small text helpers -----

/// whether `low` (lower-case) holds the wording `p` (lower-case) as whole words
fn has_phrase(low: &str, p: &str) -> bool {
    let p = p.trim();
    if p.is_empty() { return false; }
    low.match_indices(p).any(|(i, _)| {
        let before = low[..i].chars().next_back();
        let after = low[i + p.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// whether a text is written with accents (any non-ASCII letter)
fn accented(text: &str) -> bool { text.chars().any(|c| c.is_alphabetic() && !c.is_ascii()) }

/// `to` written in the case of `like`: all lower-case when it is ("thứ hai" → "thứ ba"), else with its first
/// letter's case ("Thứ Hai" → "Thứ Ba")
fn same_case(like: &str, to: &str) -> String {
    if !like.chars().any(char::is_uppercase) { return to.to_lowercase(); }
    let up = like.chars().next().is_some_and(char::is_uppercase);
    let mut cs = to.chars();
    match cs.next() {
        Some(f) if up => f.to_uppercase().collect::<String>() + cs.as_str(),
        Some(f) => f.to_lowercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

/// the byte offset of the first whole-word `w` (lower-case) in `low` (lower-case)
fn find_word(low: &str, w: &str) -> Option<usize> {
    if w.is_empty() { return None; }
    low.match_indices(w).map(|(i, _)| i).find(|&i| {
        low[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) && low[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric())
    })
}

/// a number for a person: whole numbers bare, others to two decimals without trailing zeros
pub fn show_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 { return format!("{f:.0}"); }
    let s = format!("{f:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// a number with its thousands grouped: 7006652 → "7,006,652", 1234.5 → "1,234.5"
fn grouped_num(f: f64) -> String {
    let s = show_num(f);
    let (sign, s) = s.strip_prefix('-').map_or(("", s.as_str()), |r| ("-", r));
    let (int, frac) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    format!("{sign}{out}{}", frac.map(|f| format!(".{f}")).unwrap_or_default())
}

/// a sum as a person writes it: "1234*5678" → "1234 × 5678"
fn sum_shown(expr: &str) -> String {
    expr.replace('*', " × ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// a cell or word as a number: "1,250", "$3.50", "12%", "-4" (None for anything else)
fn cell_num(s: &str) -> Option<f64> {
    let t = s.trim().trim_start_matches(['$', '€', '£']).trim_end_matches('%').trim();
    if t.is_empty() || !t.chars().any(|c| c.is_ascii_digit()) { return None; }
    if !t.chars().all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '-')) { return None; }
    t.replace(',', "").parse().ok()
}

fn same(a: f64, b: f64) -> bool { (a - b).abs() < 0.006 }

// ----- the day and part of the day in the user's language -----

/// One language's clock words (`data/clock_words.txt`): each value as (with accents, without).
#[derive(Debug)]
struct ClockLang { asks: Vec<String>, day_asks: Vec<String>, days: Vec<(String, String)>, parts: Vec<(String, (String, String))>, line: String, greet: Vec<String>, time_asks: Vec<String>, say: String,
    /// Elisa's own whole answer to a time ask, here's and a place's (`answer`, `place answer`)
    answer: String, place_answer: String,
    /// the wordings asking today's date or weekday, and Elisa's own answer to one (`date asks`, `date answer`)
    date_asks: Vec<String>, date_answer: String,
    hour_words: Vec<String>, minute_words: Vec<String>,
    /// which hours each part holds, as (part key, from, to), in the data's order (`hours`)
    hours: Vec<(String, u32, u32)>,
    /// the section's name ("en")
    code: String }

/// English's clock words: whose part keys `part_of_day` says, and whose hours a language without its own uses
fn english_clock() -> Option<&'static ClockLang> { clock_langs().iter().find(|l| l.code == "en") }

/// One "part from-to" or "part hour" of `hours`: (part, from, to).
fn hour_range(v: &str) -> Option<(String, u32, u32)> {
    let (part, range) = v.trim().rsplit_once(' ')?;
    let (a, b) = range.split_once('-').unwrap_or((range, range));
    Some((part.trim().to_string(), a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn holds(range: &(String, u32, u32), hour: u32) -> bool {
    let (_, a, b) = range;
    if a <= b { (*a..=*b).contains(&hour) } else { hour >= *a || hour <= *b }
}

impl ClockLang {
    /// this language's hour ranges, or English's when it has none
    fn hour_ranges(&self) -> &[(String, u32, u32)] {
        if !self.hours.is_empty() { return &self.hours; }
        english_clock().map_or(&[], |l| &l.hours)
    }

    /// the part of the day `hour` is, in this language's hours: its key ("noon")
    fn part_at(&self, hour: u32) -> Option<&str> {
        self.hour_ranges().iter().find(|r| holds(r, hour)).map(|r| r.0.as_str())
    }

    /// whether part `key` may be said at `hour` in this language (the hour's own part, or one also right then)
    fn part_holds(&self, key: &str, hour: u32) -> bool {
        self.hour_ranges().iter().any(|r| r.0 == key && holds(r, hour))
    }
}

/// The English part of the day of `hour` (`data/clock_words.txt`): "morning", "noon", "afternoon", "evening" or
/// "late night".
pub fn english_part_of_day(hour: u32) -> &'static str {
    english_clock().and_then(|l| l.part_at(hour)).unwrap_or("late night")
}

/// Whether the English part `key` may be said at `hour` ("afternoon" at 12:30 as well as "noon").
pub fn part_fits_hour(key: &str, hour: u32) -> bool {
    english_clock().is_some_and(|l| l.part_holds(key, hour))
}

fn forms(v: &str) -> (String, String) {
    let mut it = v.split('|').map(str::trim);
    let a = it.next().unwrap_or("").to_string();
    let p = it.next().map(str::to_string).unwrap_or_else(|| a.clone());
    (a, p)
}

fn clock_langs() -> &'static [ClockLang] {
    static L: OnceLock<Vec<ClockLang>> = OnceLock::new();
    L.get_or_init(|| {
        let mut out: Vec<ClockLang> = Vec::new();
        for l in CLOCK_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { out.push(ClockLang { asks: Vec::new(), day_asks: Vec::new(), days: Vec::new(), parts: Vec::new(), line: String::new(), greet: Vec::new(), time_asks: Vec::new(), say: String::new(), answer: String::new(), place_answer: String::new(), date_asks: Vec::new(), date_answer: String::new(), hour_words: Vec::new(), minute_words: Vec::new(), hours: Vec::new(),
                code: l.trim_matches(['[', ']']).trim().to_string() }); continue; }
            let (Some(cur), Some((k, v))) = (out.last_mut(), l.split_once('=')) else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "asks" => cur.asks = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "days" => cur.days = v.split(';').map(forms).collect(),
                "day asks" => cur.day_asks = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "line" => cur.line = v.to_string(),
                "say" => cur.say = v.to_string(),
                "answer" => cur.answer = v.to_string(),
                "place answer" => cur.place_answer = v.to_string(),
                "date asks" => cur.date_asks = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "date answer" => cur.date_answer = v.to_string(),
                "hour words" => cur.hour_words = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "minute words" => cur.minute_words = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "time asks" => cur.time_asks = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                "hours" => cur.hours = v.split('|').filter_map(hour_range).collect(),
                "greet" => cur.greet = v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect(),
                part => cur.parts.push((part.to_string(), forms(v))),
            }
        }
        out.retain(|c| c.days.len() == 7);
        out
    })
}

/// The part of the day a greeting names, in any language of `data/clock_words.txt`: (its English key, as
/// `part_of_day` says it; the greeting as written up to and with the part word; the part word as written). The message
/// opens with the part word, or with one of the language's `greet` words right before it, and the word ends the
/// message or is followed by punctuation ("good morning, Elisa", "chào buổi sáng!", "morning!"). None otherwise
/// ("what did I do this morning?", "this morning I ran 5k", "I sang.").
pub fn greeting_part(own: &str) -> Option<(String, String, String)> {
    let text = own.trim();
    let low = text.to_lowercase();
    for lang in clock_langs() {
        for (key, (a, p)) in &lang.parts {
            for form in [a, p] {
                let form = form.to_lowercase();
                if form.is_empty() { continue; }
                let before = std::iter::once("").chain(lang.greet.iter().map(String::as_str));
                for g in before {
                    let head = if g.is_empty() { form.clone() } else { format!("{g} {form}") };
                    let Some(rest) = low.strip_prefix(&head) else { continue };
                    if !rest.chars().next().is_none_or(|c| !c.is_alphanumeric() && !c.is_whitespace()) { continue; }
                    // the same stretch of the user's own text (lowercasing keeps these letters' lengths)
                    let n = head.chars().count();
                    let said: String = text.chars().take(n).collect();
                    let word: String = said.chars().skip(n - form.chars().count()).collect();
                    return Some((key.clone(), said, word));
                }
            }
        }
    }
    None
}

/// `word` (a part of the day as a greeting wrote it, `greeting_part`) as the part `key` in the same language and the
/// same writing (with or without accents), with the case of `word`'s first letter; None when no language has it.
pub fn part_word_as(word: &str, key: &str) -> Option<String> {
    let w = word.to_lowercase();
    for lang in clock_langs() {
        let Some(at) = lang.parts.iter().find_map(|(_, (a, p))| if a.to_lowercase() == w { Some(0) } else if p.to_lowercase() == w { Some(1) } else { None }) else { continue };
        let (a, p) = &lang.parts.iter().find(|(k, _)| k == key)?.1;
        let to = if at == 0 { a } else { p };
        return Some(same_case(word, to));
    }
    None
}

/// `word` (the part `key` in some language, `part_word_as`) as that language writes it with accents
pub fn accented_part(word: &str, key: &str) -> Option<String> {
    let w = word.to_lowercase();
    clock_langs().iter().find_map(|l| {
        let (a, p) = &l.parts.iter().find(|(k, _)| k == key)?.1;
        (a.to_lowercase() == w || p.to_lowercase() == w).then(|| a.clone())
    })
}

/// Whether the user's words ask the clock time: by the `time asks` wordings of any language of `data/clock_words.txt`
/// ("what time is it in Tokyo?", "bây giờ là mấy giờ?"), or by meaning (`time_ask_by_meaning`: "wats the time rn",
/// "got the time?"). Such a turn always carries Elisa's clock lines (`route::time_lines`), and its reply is held
/// to them (`ReplyFacts`).
pub fn asks_time(own: &str) -> bool {
    // ("what time does it arrive?" after a start time and a duration asks the sum, not the clock: `clock_sum`)
    if is_clock_sum(own) { return false; }
    let low = own.to_lowercase();
    clock_langs().iter().any(|l| l.time_asks.iter().any(|a| has_phrase(&low, a))) || time_ask_by_meaning(own)
}

/// Whether the user's words ask the clock time by a `time asks` wording of `data/clock_words.txt` itself, not by
/// meaning alone (`asks_time`): a message that says so in its own words wants the clock, whatever else it asks.
pub fn asks_time_in_words(own: &str) -> bool {
    let low = own.to_lowercase();
    !is_clock_sum(own) && clock_langs().iter().any(|l| l.time_asks.iter().any(|a| has_phrase(&low, a)))
}

/// Whether a message asks today's date or weekday ("what's today's date?", "hôm nay là thứ mấy?": the `date asks` and
/// `day asks` wordings of `data/clock_words.txt`), and not the time.
pub fn asks_date(own: &str) -> bool {
    if is_clock_sum(own) || asks_time(own) { return false; }
    let low = own.to_lowercase();
    clock_langs().iter().any(|l| l.date_asks.iter().chain(&l.day_asks).any(|a| has_phrase(&low, a)))
}

/// Whether a message asks the clock anything: the time (`asks_time`) or today's date or weekday (`asks_date`). Elisa's
/// clock line answers such a turn, so a reply saying it back is an answer, not an echo.
pub fn asks_clock(own: &str) -> bool { asks_time(own) || asks_date(own) }

/// Example data for `time_ask_by_meaning`: a `[time]` and an `[other]` section, one example per line.
const TIME_ASKS_DATA: &str = include_str!("../../data/time_asks.txt");
/// the similarity to the nearest `[time]` example from which a message asks the time, and how much nearer it must be
/// than the nearest `[other]` example (measured on held-out EN/VI wordings: see the tests)
pub const TIME_ASK_MIN: f32 = 0.6;
pub const TIME_ASK_LEAD: f32 = 0.1;
/// most words of a message read by meaning for a time ask (a longer one asking the time has the words for it)
const TIME_ASK_WORDS: usize = 12;

/// Whether a short message asks what time it is now, by meaning (`data/time_asks.txt`, all-MiniLM-L6-v2): nearest a
/// `[time]` example at `TIME_ASK_MIN` or more and `TIME_ASK_LEAD` nearer than any `[other]` one. False when the
/// embedding model is not installed (the wordings alone decide then).
fn time_ask_by_meaning(own: &str) -> bool {
    type Seen = std::collections::HashMap<String, bool>;
    static SEEN: OnceLock<std::sync::Mutex<Seen>> = OnceLock::new();
    let m = own.trim();
    if m.is_empty() || m.split_whitespace().count() > TIME_ASK_WORDS || !time_ask_gate(m) { return false; }
    let seen = SEEN.get_or_init(Default::default);
    if let Some(b) = seen.lock().ok().and_then(|s| s.get(m).copied()) { return b; }
    let (time, other, _) = time_examples();
    let all: Vec<&str> = time.iter().chain(other.iter()).map(String::as_str).collect();
    let Some(scores) = crate::route::meaning_scores(&super::folded(m), &all) else { return false };
    let best = |r: std::ops::Range<usize>| scores[r].iter().copied().fold(f32::MIN, f32::max);
    let (t, o) = (best(0..time.len()), best(time.len()..all.len()));
    let yes = t >= TIME_ASK_MIN && t - o >= TIME_ASK_LEAD;
    if let Ok(mut s) = seen.lock() { if s.len() < 512 { s.insert(m.to_string(), yes); } }
    yes
}

/// the words of a text for `time_ask_gate`: lowercased, marks folded ("giờ" → "gio"), two letters or more
fn gate_words(s: &str) -> impl Iterator<Item = String> {
    let folded = super::folded(&s.to_lowercase());
    folded.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_string).collect::<Vec<_>>().into_iter()
}

/// how many more `[time]` examples than `[other]` ones a word must be in to be a time word (`time_examples`)
const TIME_WORD_MARGIN: usize = 2;

/// `data/time_asks.txt`: the `[time]` examples, the `[other]` examples (both with their marks folded: the embedding
/// model's vocabulary has no Vietnamese tone marks, a marked word is unknown to it), and the time words: those in two
/// `[time]` examples or more and in `TIME_WORD_MARGIN` more of them than of the `[other]` ones ("time", "clock",
/// "gio"; not "what", "you" or "the", which both sections share about equally)
fn time_examples() -> &'static (Vec<String>, Vec<String>, std::collections::HashSet<String>) {
    static EX: OnceLock<(Vec<String>, Vec<String>, std::collections::HashSet<String>)> = OnceLock::new();
    EX.get_or_init(|| {
        let (mut time, mut other, mut in_time) = (Vec::new(), Vec::new(), false);
        for l in TIME_ASKS_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                Some(name) => in_time = name == "time",
                None => if in_time { time.push(super::folded(l)) } else { other.push(super::folded(l)) },
            }
        }
        let count = |ex: &[String]| {
            let mut n: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for e in ex { for w in gate_words(e).collect::<std::collections::HashSet<_>>() { *n.entry(w).or_default() += 1; } }
            n
        };
        let (in_time, in_other) = (count(&time), count(&other));
        let words = in_time.iter().filter(|(w, n)| **n >= 2 && **n >= in_other.get(*w).copied().unwrap_or(0) + TIME_WORD_MARGIN).map(|(w, _)| w.clone()).collect();
        (time, other, words)
    })
}

/// Whether a message has a time word of the examples at all (`time_examples`): read by meaning only then, so an
/// ordinary message never costs the embedding (every turn's message passes `asks_time`).
fn time_ask_gate(m: &str) -> bool {
    let words = &time_examples().2;
    gate_words(m).any(|w| words.contains(&w))
}

/// the UTC offset in minutes of the first "Time in X now: … (UTC+09:00, …)" line of `lines` (`route::time_lines`)
fn read_line_offset(lines: &str) -> Option<i32> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^Time in .+? now: .*?\(UTC([+\-])(\d\d):(\d\d)").unwrap());
    re.captures(lines).map(|c| { let m = c[2].parse::<i32>().unwrap_or(0) * 60 + c[3].parse::<i32>().unwrap_or(0); if &c[1] == "-" { -m } else { m } })
}

/// The other place (of `places`: name, minute of its day) the clause of `text` around bytes `at..end` is about: one it names,
/// or one the clause before it names when that one holds no time of its own ("In Tokyo, it's 06:10"; never "4:16 AM in
/// Tokyo, 2:16 AM here").
fn place_at(text: &str, at: usize, end: usize, places: &[(String, u32)]) -> Option<u32> {
    let b = ['.', ',', ';', '!', '?', '\n'];
    let st = text[..at].rfind(b).map_or(0, |i| i + 1);
    // (from the end of what was found: "4:16 a.m. in Tokyo")
    let en = text[end..].find(b).map_or(text.len(), |i| end + i);
    let named = |c: &str| { let c = c.to_lowercase(); places.iter().find(|(p, _)| c.contains(&p.to_lowercase())).map(|(_, m)| *m) };
    named(&text[st..en]).or_else(|| {
        // the clause before, when a comma ends it
        if st == 0 || !text[..st].ends_with(',') { return None; }
        let prev_end = st - 1;
        let pst = text[..prev_end].rfind(b).map_or(0, |i| i + 1);
        let prev = &text[pst..prev_end];
        (!prev.chars().any(|c| c.is_ascii_digit())).then(|| named(prev)).flatten()
    })
}

/// The clock words of the language of `own` by `reply_lang` (Vietnamese is the section with a line of its own; any
/// other, the one without: English).
fn lang_of(own: &str) -> Option<&'static ClockLang> {
    let vi = matches!(crate::reply_lang::detect(own), Some(crate::reply_lang::Lang::Vi));
    clock_langs().iter().find(|l| vi == !l.line.is_empty())
}

/// Whether the turn carries Elisa's clock line for another place ("Time in Tokyo now: …", `route::time_lines`): a
/// time turn, whatever its words ("and in Tokyo?").
pub fn has_place_lines(turn: &str) -> bool { turn.starts_with("Time in ") || turn.contains("\nTime in ") }

/// One of Elisa's clock lines, whole ("Local time now: 2026-11-11 23:04 Wednesday late night (UTC+07:00, Ho Chi
/// Minh)", "Time in Tokyo now: 2026-11-12 01:04 Thursday late night (UTC+09:00, Asia/Tokyo)"): the place as named
/// (None for here), its minute of the day, its day (days since 1970), its offset as written and its label.
#[derive(Debug, Clone)]
struct ZoneLine { place: Option<String>, minute: u32, days: i64, offset: String, label: String }

/// the clock lines of a text (`route::time_lines`), whole
fn zone_lines(text: &str) -> Vec<ZoneLine> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^(?:Local time now|Time in (.+?) now): (\d{4})-(\d\d)-(\d\d) (\d\d):(\d\d)[^\n(]*(?:\((UTC[+\-]\d\d:\d\d)(?:, ([^)\n]+))?\))?").unwrap());
    re.captures_iter(text).filter_map(|c| Some(ZoneLine {
        place: c.get(1).map(|p| p.as_str().to_string()),
        minute: c[5].parse::<u32>().ok()? * 60 + c[6].parse::<u32>().ok()?,
        days: days_from_civil(c[2].parse().ok()?, c[3].parse().ok()?, c[4].parse().ok()?),
        offset: c.get(7).map(|x| x.as_str().to_string()).unwrap_or_default(),
        label: c.get(8).map(|x| x.as_str().trim().to_string()).unwrap_or_default(),
    })).collect()
}

/// The unix time of the turn's own "Local time now" line (the last one), so that `local_tm` of it is that line's date
/// and time: a saved or scripted turn is judged by its own clock, not the wall clock. None without such a line.
fn turn_now(turn: &str) -> Option<i64> {
    let line = zone_lines(turn).into_iter().rfind(|l| l.place.is_none())?;
    let civil = line.days * 86_400 + i64::from(line.minute) * 60;
    // the machine's zone offset at that time (taken again across a daylight-saving step)
    let mut t = civil - local_tm(civil)?.tm_gmtoff;
    t = civil - local_tm(t)?.tm_gmtoff;
    Some(t)
}

/// whether two minutes of the day are within a few minutes (the minute may turn while a reply is written)
fn near_minute(a: u32, b: u32) -> bool { let d = a.abs_diff(b); d.min(1440 - d.min(1440)) <= 3 }

/// the minutes of the day each clock time in digits of `text` may mean ("01:04" → 01:04 and 13:04; "1:04 AM" → 01:04)
fn times_in_text(text: &str) -> Vec<u32> {
    static TIME: OnceLock<regex::Regex> = OnceLock::new();
    let re = TIME.get_or_init(|| regex::Regex::new(r"(?i)(?:^|[^\d+\-:])(\d{1,2})[:h](\d{2})(?:\s*([ap])\.?\s*m\b)?").unwrap());
    re.captures_iter(text).flat_map(|c| {
        let (h, m) = (c[1].parse::<u32>().unwrap_or(99), c[2].parse::<u32>().unwrap_or(99));
        if h > 23 || m > 59 { return Vec::new(); }
        match c.get(3).map(|x| x.as_str().to_ascii_lowercase()) {
            Some(x) if x == "p" => vec![(h % 12 + 12) * 60 + m],
            Some(_) => vec![(h % 12) * 60 + m],
            None if h <= 12 => vec![h * 60 + m, (h % 12 + 12) * 60 + m],
            None => vec![h * 60 + m],
        }
    }).collect()
}

/// `text` with every `from` (letters compared without case) made `to`; unchanged when lower-casing shifts its bytes
fn replace_ci(text: &str, from: &str, to: &str) -> String {
    let (low, f) = (text.to_lowercase(), from.to_lowercase());
    if low.len() != text.len() || f.is_empty() { return text.to_string(); }
    let mut out = String::new();
    let mut last = 0;
    for (i, _) in low.match_indices(f.as_str()) {
        if i < last { continue; }
        out.push_str(&text[last..i]);
        out.push_str(to);
        last = i + f.len();
    }
    out.push_str(&text[last..]);
    out
}

/// The clock times of a time ask (`asks_time`), from the turn's clock lines ("Local time now: … 02:16 …", "Time in
/// Tokyo now: … 04:16 …"; worked out again from the clock when the turn lacks them): a time in the reply that is
/// none of them is put right, and a reply with no time gets the time said in front (`say`).
#[derive(Debug, Clone)]
struct TimeFix { local: u32, places: Vec<(String, u32)>, place_asked: bool, say: String,
    /// the local offset from UTC in minutes, from the local clock line ("UTC+07:00")
    offset: Option<i32>,
    /// the clock lines whole (`zone_lines`): the date, offset and label of here and of each place, for the words
    /// around a place's time
    lines: Vec<ZoneLine>,
    /// the ask's language's clock words (`say` is theirs), for Elisa's own answer (`own_answer`); the turn's "now"
    /// for here's day and part when the turn has no local line; and the user's words, lower-cased
    lang: Option<&'static ClockLang>, now: i64, ask: String }

impl TimeFix {
    fn new(turn: &str, own: &str, now: i64) -> Option<TimeFix> {
        if !asks_time(own) && !has_place_lines(turn) { return None; }
        // a date question is no clock question, whatever lines came with it ("Christmas" is also Christmas Island's zone)
        if !asks_time(own) && date_asked(own, now).is_some() { return None; }
        static LINE: OnceLock<regex::Regex> = OnceLock::new();
        let re = LINE.get_or_init(|| regex::Regex::new(r"(?m)^(?:Local time now|Time in (.+?) now): \d{4}-\d\d-\d\d (\d\d):(\d\d)").unwrap());
        let read = |text: &str| -> (Option<u32>, Vec<(String, u32)>) {
            let mut local = None;
            let mut places = Vec::new();
            for c in re.captures_iter(text) {
                let at = c[2].parse::<u32>().unwrap_or(0) * 60 + c[3].parse::<u32>().unwrap_or(0);
                match c.get(1) { Some(p) => places.push((p.as_str().to_string(), at)), None => { local.get_or_insert(at); } }
            }
            (local, places)
        };
        let (mut local, mut places) = read(turn);
        let named = !crate::route::zones_named(own).is_empty();
        if local.is_none() || (named && places.is_empty()) {
            let (l, p) = read(&crate::route::time_lines(own));
            local = local.or(l);
            if places.is_empty() { places = p; }
        }
        let local = local.or_else(|| local_tm(now).map(|t| t.tm_hour as u32 * 60 + t.tm_min as u32))?;
        let low = own.to_lowercase();
        // (a follow-up with no time words, "còn ở Tokyo thì sao?": the language of its words)
        let lang = clock_langs().iter().find(|l| l.time_asks.iter().any(|a| has_phrase(&low, a))).or_else(|| lang_of(own));
        let say = lang.map(|l| l.say.clone()).unwrap_or_default();
        static OFF: OnceLock<regex::Regex> = OnceLock::new();
        let off = OFF.get_or_init(|| regex::Regex::new(r"(?m)^Local time now: .*?\(UTC([+\-])(\d\d):(\d\d)").unwrap());
        let read_off = |text: &str| off.captures(text).map(|c| {
            let m = c[2].parse::<i32>().unwrap_or(0) * 60 + c[3].parse::<i32>().unwrap_or(0);
            if &c[1] == "-" { -m } else { m }
        });
        let offset = read_off(turn).or_else(|| read_off(&crate::route::time_lines("")));
        // (the lines whole, from the clock again when the turn lacks here's or the place's)
        let mut lines = zone_lines(turn);
        if lines.iter().all(|l| l.place.is_some()) || (named && lines.iter().all(|l| l.place.is_none())) {
            for e in zone_lines(&crate::route::time_lines(own)) { if !lines.iter().any(|l| l.place == e.place) { lines.push(e); } }
        }
        Some(TimeFix { local, places, place_asked: named, say, offset, lines, lang, now, ask: low })
    }

    /// Elisa's own whole answer to the time ask, in its language (`answer` / `place answer` of
    /// `data/clock_words.txt`): the place asked's time, day, part of the day and zone from her "Time in … now" line
    /// ("In Paris it's 19:14, Wednesday evening (UTC+01:00, Europe/Paris)."), else here's from her local line, or
    /// the clock ("It's 01:10, Thursday late night.", "Bây giờ là 01:10 khuya Thứ Năm, ngày 12/11/2026."). None
    /// when the ask's language has no such line.
    fn own_answer(&self) -> Option<String> {
        let lang = self.lang?;
        let time = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
        // the day's name and the part of the day at a minute of it, written with accents (Elisa's own line)
        let day_part = |days: i64, minute: u32| -> Option<(String, String)> {
            let day = lang.days.get(weekday_of(days))?.0.clone();
            let key = lang.part_at(minute / 60)?;
            Some((day, lang.parts.iter().find(|(k, _)| k == key)?.1 .0.clone()))
        };
        if self.place_asked && !lang.place_answer.is_empty() {
            if let Some(l) = self.lines.iter().find(|l| l.place.is_some()) {
                let place = l.place.clone()?;
                let (day, part) = day_part(l.days, l.minute)?;
                let zone = if l.label.is_empty() || l.label.eq_ignore_ascii_case(&place) { l.offset.clone() }
                    else if l.offset.is_empty() { l.label.clone() } else { format!("{}, {}", l.offset, l.label) };
                let tpl = if zone.is_empty() { lang.place_answer.replace(" ({zone})", "").replace("({zone})", "") } else { lang.place_answer.clone() };
                return Some(tpl.replace("{place}", &place).replace("{time}", &time(l.minute)).replace("{day}", &day).replace("{part}", &part).replace("{zone}", &zone));
            }
        }
        if lang.answer.is_empty() { return None; }
        let (days, minute) = match self.lines.iter().find(|l| l.place.is_none()) {
            Some(l) => (l.days, l.minute),
            None => { let tm = local_tm(self.now)?; (days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32), self.local) }
        };
        let (day, part) = day_part(days, minute)?;
        let (y, m, d) = civil_from_days(days);
        Some(lang.answer.replace("{time}", &time(minute)).replace("{day}", &day).replace("{part}", &part)
            .replace("{d}", &d.to_string()).replace("{m}", &m.to_string()).replace("{y}", &y.to_string()))
    }

    /// The words a reply to this time ask may repeat without saying anything twice: the user's own, the clock facts
    /// (days, parts of the day, the places and zone labels of the clock lines) and Elisa's own templates' words, in
    /// every language of `data/clock_words.txt`.
    fn fact_words(&self) -> std::collections::HashSet<String> {
        let mut w: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut add = |t: &str| for x in t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()) { w.insert(x.to_string()); };
        add(&self.ask);
        for l in clock_langs() {
            for (a, p) in &l.days { add(a); add(p); }
            for (_, (a, p)) in &l.parts { add(a); add(p); }
            for t in [&l.say, &l.answer, &l.place_answer, &l.line] { add(t); }
            for t in l.hour_words.iter().chain(&l.minute_words) { add(t); }
        }
        for l in &self.lines { if let Some(p) = &l.place { add(p); } add(&l.label); }
        // (a number said in words, "hai mươi hai", says nothing twice either)
        add(&number_alternation().replace(['|', '\\'], " "));
        w
    }

    /// Whether `reply` (holding a right time) answers nothing with the words around it: a sentence with no clock
    /// time that opens with a quoted word (it talks about a word of the message, as moondream:1.8b did: "\"Paris\" is
    /// a placeholder for your location."), or that says one of its own words twice (garbled text going round, as
    /// phi4-mini's "Giờ trông đó còn trông 7 giờ trôi qua, còn trôi qua."). The user's words, the clock facts and
    /// Elisa's own templates (`fact_words`) are no repeats, nor is a word of under three letters.
    fn answers_nothing(&self, reply: &str) -> bool {
        let known = self.fact_words();
        let numbers: std::collections::HashSet<String> = number_alternation().replace(['|', '\\'], " ").split_whitespace().map(str::to_string).collect();
        for sent in reply.split_inclusive(['.', '!', '?', ';', '…', '\n']) {
            let s = sent.trim().trim_end_matches(['.', '!', '?', ';', '…']).trim();
            if s.is_empty() || !times_in_text(s).is_empty() { continue; }
            // a quoted or bracketed sentence beside the time: a prompt line said back, an aside about the date
            if s.starts_with(['"', '“', '\'', '‘', '«', '(', '[']) { return true; }
            let low = s.to_lowercase();
            // (a sentence with a number says a time or a date, spelled or not: "bốn giờ hai mươi hai phút")
            let mut own_word = low.chars().any(|c| c.is_ascii_digit()) || low.split(|c: char| !c.is_alphanumeric()).any(|w| numbers.contains(w));
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for w in low.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().any(char::is_alphabetic)) {
                if known.contains(w) { continue; }
                own_word = true;
                if w.chars().count() >= 3 && !seen.insert(w.to_string()) { return true; }
            }
            // a line of the facts alone ("Thứ Năm", "sáng", "Thursday morning"): fragments, not an answer
            if !own_word { return true; }
        }
        false
    }

    /// The clock lines' places with their day's shift from here's (+1 when it is already tomorrow there), as (name
    /// lowercased, minute of their day, shift), and here's minute (`ClockFix`: the day word of a clause about a place).
    fn place_days(&self) -> (Vec<(String, u32, i64)>, Option<u32>) {
        let here = self.lines.iter().find(|l| l.place.is_none());
        let v = self.lines.iter().filter_map(|l| Some((l.place.as_ref()?.to_lowercase(), l.minute, l.days - here.map_or(l.days, |h| h.days)))).collect();
        (v, Some(here.map_or(self.local, |h| h.minute)))
    }

    /// `text` with the zone words of each clause about the place asked made that place's, from Elisa's own clock
    /// lines: an offset label that is not its ("UTC+07:00" for Tokyo → "UTC+09:00") and here's place name in it ("Ho
    /// Chi Minh" → "Tokyo"). A clause is about the place when it names it or holds its time; one holding here's time
    /// is about here; one with neither is about the place when the text names it nowhere (as `want`). (gemma3:1b
    /// answered "and in Tokyo?" with "It's 01:04 Wednesday, late night (UTC+07:00, Ho Chi Minh)": the clock Tokyo's,
    /// the rest copied from its answer about here; the day is `ClockFix`'s.)
    fn zone_words_fix(&self, text: &str) -> String {
        static OFF: OnceLock<regex::Regex> = OnceLock::new();
        let off_re = OFF.get_or_init(|| regex::Regex::new(r"(?i)\b(?:UTC|GMT)\s*([+\-−])\s*(\d{1,2})(?::?(\d{2}))?\b").unwrap());
        let (Some(here), Some(there)) = (self.lines.iter().find(|l| l.place.is_none()), self.lines.iter().find(|l| l.place.is_some())) else { return text.to_string() };
        let Some(place) = there.place.as_deref() else { return text.to_string() };
        if !self.place_asked || there.offset.is_empty() { return text.to_string(); }
        let place_low = place.to_lowercase();
        let named_anywhere = text.to_lowercase().contains(&place_low);
        let minutes = |c: &regex::Captures| -> i32 {
            let m = c[2].parse::<i32>().unwrap_or(0) * 60 + c.get(3).and_then(|x| x.as_str().parse::<i32>().ok()).unwrap_or(0);
            if &c[1] == "+" { m } else { -m }
        };
        let there_off = off_re.captures(&format!(" {} ", there.offset)).map(|c| minutes(&c));
        let mut out = String::new();
        for clause in text.split_inclusive(['.', '!', '?', ';', '\n']) {
            let mins = times_in_text(clause);
            let about = clause.to_lowercase().contains(&place_low)
                || (mins.iter().any(|m| near_minute(*m, there.minute)) && !mins.iter().any(|m| near_minute(*m, here.minute)))
                || (mins.is_empty() && !named_anywhere);
            if !about { out.push_str(clause); continue; }
            let mut c = off_re.replace_all(clause, |cap: &regex::Captures| if Some(minutes(cap)) == there_off { cap[0].to_string() } else { there.offset.clone() }).into_owned();
            if !here.label.is_empty() && !here.label.eq_ignore_ascii_case(place) { c = replace_ci(&c, &here.label, place); }
            out.push_str(&c);
        }
        out
    }

    /// `reply` with each sentence that claims a time for a zone the turn did not ask about checked against that zone's
    /// real time (`route::zones_named`: UTC, GMT, a city, an abbreviation; or an offset "UTC+9"): a clause about that
    /// zone with another time, or with no time in digits ("in UTC, it is noon"), drops its sentence. An offset label
    /// that is the local one ("(UTC+07:00)") is the local time. Whether a sentence went, and the zone times found right.
    fn other_zones(&self, reply: &str) -> (String, bool, Vec<u32>) {
        static TIME: OnceLock<regex::Regex> = OnceLock::new();
        let tre = TIME.get_or_init(|| regex::Regex::new(r"(?i)(?:^|[^\d+\-:])(\d{1,2})[:h](\d{2})(\s*([ap])\.?\s*m\b)?").unwrap());
        static OFFS: OnceLock<regex::Regex> = OnceLock::new();
        let ore = OFFS.get_or_init(|| regex::Regex::new(r"(?i)\b(?:utc|gmt)\s*([+\-−])\s*(\d{1,2})(?::?(\d{2}))?").unwrap());
        let Some(local_off) = self.offset else { return (reply.to_string(), false, Vec::new()) };
        let mut confirmed: Vec<u32> = Vec::new();
        let utc = (self.local as i32 - local_off).rem_euclid(1440) as u32;
        let asked: Vec<String> = self.places.iter().map(|(p, _)| p.to_lowercase()).collect();
        let near = |a: u32, b: u32| { let d = a.abs_diff(b); d.min(1440 - d.min(1440)) <= 3 };
        let mut out = String::new();
        let mut dropped = false;
        let mut rest = reply;
        while !rest.is_empty() {
            let end = rest.find(['.', '!', '?', '\n']).map_or(rest.len(), |i| i + 1);
            // ("05:06." ends at its dot, "2026-11-11" has none)
            let (sent, next) = rest.split_at(end);
            rest = next;
            // the zones the sentence's clauses are about: an offset label (its own time), or a named zone
            let mut zones: Vec<(usize, usize, u32)> = Vec::new();
            for c in ore.captures_iter(sent) {
                let m = c[2].parse::<i32>().unwrap_or(0) * 60 + c.get(3).and_then(|x| x.as_str().parse::<i32>().ok()).unwrap_or(0);
                let off = if &c[1] == "+" { m } else { -m };
                if off == local_off { continue; }
                let x = c.get(0).unwrap();
                zones.push((x.start(), x.end(), (utc as i32 + off).rem_euclid(1440) as u32));
            }
            // (offset labels blanked, so "UTC+07:00" names no zone)
            let plain: String = { let mut p = sent.to_string(); for c in ore.find_iter(sent) { p.replace_range(c.range(), &" ".repeat(c.len())); } p };
            for z in crate::route::zones_named(&plain) {
                let city = z.rsplit('/').next().unwrap_or(&z).replace('_', " ").to_lowercase();
                if asked.contains(&city) { continue; }
                // (its offset, on the turn's own clock)
                let Some(zoff) = read_line_offset(&crate::route::time_lines(&z.replace('_', " "))) else { continue };
                let t = (utc as i32 + zoff).rem_euclid(1440) as u32;
                if near(t, self.local) { continue; }
                // where it is named
                let low = plain.to_lowercase();
                let names: Vec<String> = crate::route::zone_words(&z);
                // (a short name only in capitals: "UTC", "LA", never Vietnamese "la")
                let at = names.iter().find_map(|n| {
                    let i = find_word(&low, n)?;
                    let written = plain.get(i..i + n.len())?;
                    (n.chars().count() > 3 || written.chars().all(|c| !c.is_lowercase())).then_some((i, i + n.len()))
                });
                if let Some((a, b)) = at { zones.push((a, b, t)); }
            }
            if zones.is_empty() { out.push_str(sent); continue; }
            // each zone's clause, or the clause after it when its own has no time ("In UTC, it is noon")
            let bounds = [',', ';', '(', ')'];
            let clause_of = |at: usize| -> (usize, usize) {
                let st = sent[..at].rfind(bounds).map_or(0, |i| i + 1);
                let en = sent[at..].find(bounds).map_or(sent.len(), |i| at + i);
                (st, en)
            };
            let mut bad = false;
            for (a, _, t) in &zones {
                let (st, en) = clause_of(*a);
                let mut times: Vec<u32> = Vec::new();
                for c in tre.captures_iter(&sent[st..en]) { let (h, m) = (c[1].parse::<u32>().unwrap_or(99), c[2].parse::<u32>().unwrap_or(99)); if h < 24 && m < 60 { times.push(h * 60 + m); times.push((h % 12 + 12) * 60 + m); } }
                if times.is_empty() && en < sent.len() {
                    let (s2, e2) = clause_of((en + 1).min(sent.len()));
                    for c in tre.captures_iter(&sent[s2..e2]) { let (h, m) = (c[1].parse::<u32>().unwrap_or(99), c[2].parse::<u32>().unwrap_or(99)); if h < 24 && m < 60 { times.push(h * 60 + m); times.push((h % 12 + 12) * 60 + m); } }
                }
                // (or the clause before a label in brackets after the time: "01:04 Thursday (UTC+09:00); here …")
                if times.is_empty() && st > 0 {
                    let (s0, e0) = clause_of(st - 1);
                    for c in tre.captures_iter(&sent[s0..e0]) { let (h, m) = (c[1].parse::<u32>().unwrap_or(99), c[2].parse::<u32>().unwrap_or(99)); if h < 24 && m < 60 { times.push(h * 60 + m); times.push((h % 12 + 12) * 60 + m); } }
                }
                // (or the sentence's one time, when commas cut the label off from it: "It's 01:18 Thursday, late
                // night (UTC+09:00), Asia/Tokyo.")
                if times.is_empty() {
                    let all: Vec<(u32, u32)> = tre.captures_iter(sent).filter_map(|c| { let (h, m) = (c[1].parse::<u32>().ok()?, c[2].parse::<u32>().ok()?); (h < 24 && m < 60).then_some((h, m)) }).collect();
                    if let [(h, m)] = all.as_slice() { times.push(h * 60 + m); times.push((h % 12 + 12) * 60 + m); }
                }
                // the same clause's every time: one that is not the zone's (the local one read as UTC) is wrong too
                let ok = !times.is_empty() && times.chunks(2).all(|p| p.iter().any(|x| near(*x, *t)));
                if !ok { bad = true; }
            }
            if bad { dropped = true; } else { out.push_str(sent); confirmed.extend(zones.iter().map(|(_, _, t)| *t)); }
        }
        (out.trim().to_string(), dropped, confirmed)
    }

    /// The real time a time read at `at..end` of `reply` should be (`read`: its possible minutes of the day), None when
    /// it is right: the place its clause is about ("4:16 AM in Tokyo, 2:16 AM here"); none, and the reply names no
    /// place for a place asked: that place; else any real time stands, or here's.
    fn want(&self, reply: &str, at: usize, end: usize, read: &[u32], all: &[u32]) -> Option<u32> {
        let near = |a: u32, b: u32| { let d = a.abs_diff(b); d.min(1440 - d.min(1440)) <= 3 };
        let low = reply.to_lowercase();
        let named = |text: &str| self.places.iter().find(|(p, _)| text.contains(&p.to_lowercase())).map(|(_, t)| *t);
        let want = match place_at(reply, at, end, &self.places) {
            Some(t) => t,
            None if self.place_asked && named(&low).is_none() => self.places.first().map_or(self.local, |(_, t)| *t),
            None if read.iter().any(|r| all.iter().any(|a| near(*r, *a))) => return None,
            None => self.local,
        };
        (!read.iter().any(|r| near(*r, want))).then_some(want)
    }

    /// `reply` with each time written in words (`data/number_words.txt` numbers with the hour and minute words of
    /// `data/clock_words.txt`: "hai giờ hai chín phút", "two o'clock") checked like a digit one, a wrong one written as
    /// the real time in digits; and whether it had one. An hour with no minutes is right when its hour is.
    fn fix_spelled(&self, reply: &str, all: &[u32]) -> (String, bool) {
        static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
        let re = RE.get_or_init(|| {
            let words = |f: fn(&ClockLang) -> &Vec<String>| clock_langs().iter().flat_map(|l| f(l).iter()).map(|w| regex::escape(w)).collect::<Vec<_>>().join("|");
            let (hw, mw) = (words(|l| &l.hour_words), words(|l| &l.minute_words));
            if hw.is_empty() { return None; }
            let num = number_alternation();
            let n = format!(r"(?:{num})(?:[\s-]+(?:{num}))?");
            let mw = if mw.is_empty() { String::new() } else { format!(r"(?:\s+(?:{mw})\b)?") };
            regex::Regex::new(&format!(r"(?i)\b(?P<h>{n})\s+(?:{hw})(?:\s+(?P<m>{n}){mw})?")).ok()
        });
        let Some(re) = re else { return (reply.to_string(), false) };
        let mut found = false;
        let out = re.replace_all(reply, |c: &regex::Captures| {
            let Some(h) = c.name("h").and_then(|x| spoken_number(x.as_str())).filter(|h| *h < 24) else { return c[0].to_string() };
            let m = c.name("m").and_then(|x| spoken_number(x.as_str()));
            if m.is_some_and(|m| m > 59) { return c[0].to_string(); }
            // an everyday word as the hour, with no minutes: no time ("Bay gio la …" is "now is …")
            if m.is_none() && c.name("h").is_some_and(|x| everyday_number(x.as_str())) { return c[0].to_string(); }
            found = true;
            let (at, end) = c.get(0).map_or((0, 0), |x| (x.start(), x.end()));
            let hours = if h <= 12 { vec![h, h % 12 + 12] } else { vec![h] };
            let want = match m {
                Some(m) => { let read: Vec<u32> = hours.iter().map(|h| h * 60 + m).collect(); self.want(reply, at, end, &read, all) }
                // an hour alone: right when the real time's hour is one of its readings
                None => {
                    let read: Vec<u32> = hours.iter().map(|h| h * 60).collect();
                    self.want(reply, at, end, &read, all).filter(|w| !hours.contains(&(w / 60)))
                }
            };
            match want { Some(w) => format!("{:02}:{:02}", w / 60, w % 60), None => c[0].to_string() }
        }).into_owned();
        (out, found)
    }

    /// `reply` with each clock time ("23:16", "11:16 PM", "02h20") made the real one, keeping how it was written; one
    /// within a few minutes of a real one (the minute turned while it was written) stays.
    fn fix(&self, reply: &str) -> String {
        static TIME: OnceLock<regex::Regex> = OnceLock::new();
        let re = TIME.get_or_init(|| regex::Regex::new(r"(?i)(^|[^\d+\-:])(\d{1,2})([:h])(\d{2})(\s*([ap])\.?\s*m\b\.?)?").unwrap());
        // a time claimed for another zone nobody asked about ("… is 05:06 (noon + 1 day) in UTC") first: right, or its
        // sentence goes; a right one is a real time below
        let (reply, dropped, zone_times) = self.other_zones(reply);
        let reply = reply.as_str();
        let all: Vec<u32> = std::iter::once(self.local).chain(self.places.iter().map(|(_, t)| *t)).chain(zone_times).collect();
        let mut found = false;
        let mut part_wrong = false;
        let out = re.replace_all(reply, |c: &regex::Captures| {
            let (h, m) = (c[2].parse::<u32>().unwrap_or(99), c[4].parse::<u32>().unwrap_or(99));
            if h > 23 || m > 59 { return c[0].to_string(); }
            found = true;
            let ampm = c.get(6).map(|x| x.as_str());
            let read: Vec<u32> = match ampm {
                Some(x) if x.eq_ignore_ascii_case("p") => vec![(h % 12 + 12) * 60 + m],
                Some(_) => vec![(h % 12) * 60 + m],
                None if h <= 12 => vec![h * 60 + m, (h % 12 + 12) * 60 + m],
                None => vec![h * 60 + m],
            };
            let (at, end) = c.get(0).map_or((0, 0), |x| (x.start(), x.end()));
            // a twelve-hour figure read by the part of the day right after it ("2:22 khuya" is 02:22, not 14:22): a
            // part no real time's hour has makes the time wrong, whatever the figure (`part_after`)
            let read = match part_after(&reply[end..]) {
                Some(fits) if ampm.is_none() && h <= 12 => {
                    let near = |r: &u32| all.iter().any(|a| r.abs_diff(*a).min(1440 - r.abs_diff(*a).min(1440)) <= 3);
                    let kept: Vec<u32> = read.iter().copied().filter(|r| fits(r / 60)).collect();
                    // (only the figure's other reading is a real time: the part says it is not that one; a part that
                    // fits neither reading is the part-of-day check's, `ClockFix`)
                    if !kept.is_empty() && !kept.iter().any(near) && read.iter().any(near) { part_wrong = true; }
                    if kept.is_empty() { read } else { kept }
                }
                _ => read,
            };
            let Some(want) = self.want(reply, at, end, &read, &all) else { return c[0].to_string() };
            let (wh, wm) = (want / 60, want % 60);
            let time = match ampm {
                Some(x) => {
                    let pm = wh >= 12;
                    let suffix = c[5].replacen(x, if pm { "p" } else { "a" }, 1);
                    let suffix = if x.chars().all(char::is_uppercase) { suffix.to_uppercase() } else { suffix };
                    format!("{}{}{wm:02}{suffix}", if wh % 12 == 0 { 12 } else { wh % 12 }, &c[3])
                }
                None if c[2].len() == 2 || wh >= 10 => format!("{wh:02}{}{wm:02}", &c[3]),
                None => format!("{wh}{}{wm:02}", &c[3]),
            };
            format!("{}{time}", &c[1])
        }).into_owned();
        // a time in words ("hai giờ hai chín phút", "two o'clock"): the same check, put right in digits
        let (out, spelled) = self.fix_spelled(&out, &all);
        // the zone words of a clause about the place asked: its own offset and name
        let out = self.zone_words_fix(&out);
        // (every time gone with its sentence: the local answer is said)
        let found = (found || spelled) && !(dropped && !re.is_match(&out));
        // no clock time at all (a denial, "How can I assist you further?"), or words around it that answer nothing
        // (`answers_nothing`): Elisa's own whole answer
        if !found || part_wrong || self.answers_nothing(&out) {
            if let Some(a) = self.own_answer() { return a; }
        }
        if found || self.say.is_empty() { return out; }
        let want = if self.place_asked { self.places.first().map_or(self.local, |(_, t)| *t) } else { self.local };
        format!("{} {out}", self.say.replace("{time}", &format!("{:02}:{:02}", want / 60, want % 60))).trim_end().to_string()
    }
}

/// The part of the day named right after a clock time (`rest` is the text after it: "khuya Thứ Năm", ", in the
/// afternoon"), in any language of `data/clock_words.txt` (its part words, accented or not, within the next few words):
/// whether an hour may be said with it (that language's `hours`). None when no part word follows.
fn part_after(rest: &str) -> Option<impl Fn(u32) -> bool> {
    const WORDS: usize = 3;
    let head: String = rest.split_whitespace().take(WORDS).collect::<Vec<_>>().join(" ").to_lowercase();
    // (the text up to the next sentence or clause end only)
    let head = head.split(['.', ';', '!', '?', '(']).next().unwrap_or("").to_string();
    let found = clock_langs().iter().flat_map(|l| l.parts.iter().map(move |(k, (a, p))| (l, k, a, p)))
        .filter(|(_, _, a, p)| has_phrase(&head, &a.to_lowercase()) || has_phrase(&head, &p.to_lowercase()))
        .map(|(l, k, _, _)| (l, k.clone())).collect::<Vec<_>>();
    if found.is_empty() { return None; }
    Some(move |hour: u32| found.iter().any(|(l, k)| l.part_holds(k, hour)))
}

/// The `now week` groups of `data/clock_words.txt`, every language: (the wordings, lowercase; the weekdays they fit).
fn now_week_words() -> &'static Vec<(Vec<String>, Vec<i32>)> {
    static W: OnceLock<Vec<(Vec<String>, Vec<i32>)>> = OnceLock::new();
    W.get_or_init(|| include_str!("../../data/clock_words.txt").lines().map(str::trim).filter_map(|l| l.split_once(" = ").filter(|(k, _)| k.trim() == "now week").map(|(_, v)| v))
        .flat_map(|v| v.split(';').filter_map(|g| g.rsplit_once(':').map(|(w, d)| (
            w.split('|').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect::<Vec<_>>(),
            d.split(',').filter_map(|n| n.trim().parse().ok()).collect::<Vec<_>>()))).collect::<Vec<_>>())
        .collect())
}

/// `reply` without its sentences that say where in the week it is now (`now week` of `data/clock_words.txt`: "a great
/// way to start the week", "chúc cuối tuần vui") on a day that wording does not fit, by the local clock at `now`
/// ("start the week" on a Thursday). Not when `ask` names a weekday or such a wording itself (the user's own talk of
/// the week). None: nothing goes, or nothing would be left.
pub fn without_wrong_week_words(reply: &str, ask: &str, now: i64) -> Option<String> {
    let today = local_tm(now)?.tm_wday;
    let low_ask = ask.to_lowercase();
    let all_days: Vec<String> = include_str!("../../data/clock_words.txt").lines().filter_map(|l| l.trim().strip_prefix("days = "))
        .flat_map(|v| v.split([';', '|']).map(|d| d.trim().to_lowercase()).collect::<Vec<_>>()).filter(|d| !d.is_empty()).collect();
    if all_days.iter().any(|d| has_phrase(&low_ask, d)) || now_week_words().iter().flat_map(|(w, _)| w).any(|w| has_phrase(&low_ask, w)) { return None; }
    let wrong = |s: &str| { let low = s.to_lowercase(); now_week_words().iter().any(|(w, days)| !days.contains(&today) && w.iter().any(|x| has_phrase(&low, x))) };
    let mut out = String::new();
    let mut cut = false;
    let mut rest = reply;
    // (sentences end at ".", "!", "?" or a line break, kept with what follows them)
    while !rest.is_empty() {
        let end = rest.find(['.', '!', '?', '\n']).map_or(rest.len(), |i| i + rest[i..].chars().next().map_or(1, char::len_utf8));
        let (s, r) = rest.split_at(end);
        if wrong(s) { cut = true; } else { out.push_str(s); }
        rest = r;
    }
    (cut && out.chars().any(char::is_alphanumeric)).then(|| out.trim().to_string())
}

/// the local time of a unix time
fn local_tm(t: i64) -> Option<libc::tm> {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() { return None; }
        Some(tm)
    }
}

/// The language of a question about the time or the date, by its wordings in `data/clock_words.txt`, with whether it
/// is written without accents.
fn clock_lang(own: &str) -> Option<(&'static ClockLang, bool)> {
    let low = own.to_lowercase();
    // (a clock time plus a duration is no question about now, whatever its asking words: `clock_sum`)
    if is_clock_sum(own) { return None; }
    clock_langs().iter().find(|l| l.asks.iter().any(|a| has_phrase(&low, a))).map(|l| (l, !accented(own)))
}

/// For a question about the time or the date in a language with clock words (`data/clock_words.txt`): the local
/// time, part of the day and day in that language, as a situation part ("the local time in the user's language:
/// 22:10 tối Thứ Ba, ngày 10/11/2026 · …"). Without accents when the question has none. None for any other message.
pub fn clock_words(own: &str, now: i64) -> Option<String> {
    let (lang, plain) = clock_lang(own)?;
    // a language with no line of its own (English: Elisa's clock line is already in it) gets none
    if lang.line.is_empty() { return None; }
    let tm = local_tm(now)?;
    let pick = |f: &(String, String)| if plain { f.1.clone() } else { f.0.clone() };
    let day = pick(lang.days.get(tm.tm_wday as usize)?);
    let part_key = lang.part_at(tm.tm_hour as u32)?;
    let part = pick(&lang.parts.iter().find(|(k, _)| k == part_key)?.1);
    let words = lang.line.replace("{time}", &format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)).replace("{part}", &part).replace("{day}", &day)
        .replace("{d}", &tm.tm_mday.to_string()).replace("{m}", &(tm.tm_mon + 1).to_string()).replace("{y}", &(tm.tm_year + 1900).to_string());
    Some(format!("{CLOCK_HEAD}{words} · {CLOCK_USE}"))
}

/// what `clock_words` starts its words with
const CLOCK_HEAD: &str = "the local time in the user's language: ";

/// The facts Elisa gave in one turn's message (`turn`), as text a reply may hold: the clock words and every
/// computed result (its number, and its row). A reply holding one answers from this turn's facts, so it is no copy of
/// an earlier reply even when it reads the same ("Mấy giờ rồi?" after "bây giờ là mấy giờ?", `context::ReplyGuard`).
pub fn turn_fact_values(turn: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(i) = turn.find(CLOCK_HEAD) {
        let rest = &turn[i + CLOCK_HEAD.len()..];
        // the time and the day words, before the date
        let w = rest.split(" · ").next().unwrap_or("").split(',').next().unwrap_or("").trim();
        if !w.is_empty() { v.push(w.to_lowercase()); }
        // the time alone too (a reply may write the words with or without accents)
        if let Some(t) = w.split_whitespace().next().filter(|t| t.contains(':')) { v.push(t.to_string()); }
    }
    // (a one-digit result is in too many replies to tell anything)
    v.extend(computed_in(turn).into_iter().map(|c| c.label.unwrap_or(c.text).to_lowercase()).filter(|x| x.chars().count() >= 2));
    v
}

/// The day and part-of-day words of a time question's language, and today's ones (`ReplyFacts`).
#[derive(Debug, Clone, Default)]
struct ClockFix { days: Vec<(String, String)>, day: usize, parts: Vec<(String, String)>, part: usize, day_asks: Vec<String>, keys: Vec<String>,
    /// the local hour now, and the language's clock words (whose hours say which part each hour is)
    hour: u32, lang: Option<&'static ClockLang>,
    /// the other places of the turn's clock lines, as (name lowercased, minute of their day) (`TimeFix`)
    places: Vec<(String, u32)>,
    /// those places with their day's shift from today's (+1: tomorrow there already), as (name lowercased, minute,
    /// shift), and here's minute (`TimeFix::place_days`): a day word in a clause about a place is that place's day
    place_days: Vec<(String, u32, i64)>, here_minute: Option<u32> }

impl ClockFix {
    #[cfg(test)]
    fn new(own: &str, now: i64) -> Option<ClockFix> { Self::new_for(own, now, false) }

    /// `new`, or with `clock_lines` (the turn carries Elisa's clock lines for another place: "and in Tokyo?" has no
    /// time words of its own) in the language of the user's words
    fn new_for(own: &str, now: i64, clock_lines: bool) -> Option<ClockFix> {
        let lang = match clock_lang(own) {
            Some((l, _)) => l,
            None if clock_lines => lang_of(own)?,
            None => return None,
        };
        let tm = local_tm(now)?;
        let part_key = lang.part_at(tm.tm_hour as u32)?;
        let part = lang.parts.iter().position(|(k, _)| k == part_key)?;
        // a day or part of the day the user named themself ("thứ Sáu tuần sau là ngày mấy?") is theirs: never changed
        let low = own.to_lowercase();
        let said = |f: &(String, String)| has_phrase(&low, &f.0.to_lowercase()) || has_phrase(&low, &f.1.to_lowercase());
        let days = lang.days.iter().enumerate().map(|(i, f)| if i != tm.tm_wday as usize && said(f) { (String::new(), String::new()) } else { f.clone() }).collect();
        let parts = lang.parts.iter().enumerate().map(|(i, (_, f))| if i != part && said(f) { (String::new(), String::new()) } else { f.clone() }).collect();
        Some(ClockFix { days, day: tm.tm_wday as usize, parts, part, day_asks: lang.day_asks.clone(), keys: lang.parts.iter().map(|(k, _)| k.clone()).collect(),
            hour: tm.tm_hour as u32, lang: Some(lang), places: Vec::new(), place_days: Vec::new(), here_minute: None })
    }

    /// The day shift of the place the clause around `at` is about (`place_days`): the place it names (or the clause
    /// before names, as `place_at`), else the place whose time it holds and not here's. None for here, or when no
    /// place's day is known.
    fn place_shift(&self, text: &str, at: usize) -> Option<i64> {
        if self.place_days.is_empty() { return None; }
        let by_name = |t: &str| { let low = t.to_lowercase(); self.place_days.iter().find(|(p, _, _)| low.contains(p.as_str())).map(|(_, _, sh)| *sh) };
        // (None: no time in it; Some(None): here's time; Some(Some(shift)): a place's)
        let by_time = |t: &str| -> Option<Option<i64>> {
            let mins = times_in_text(t);
            if mins.is_empty() { return None; }
            if self.here_minute.is_some_and(|h| mins.iter().any(|m| near_minute(*m, h))) { return Some(None); }
            Some(self.place_days.iter().find(|(_, m, _)| mins.iter().any(|x| near_minute(*x, *m))).map(|(_, _, sh)| *sh))
        };
        let c = Self::clause(text, at);
        if let Some(sh) = by_name(c) { return Some(sh); }
        if let Some(m) = place_at(text, at, at, &self.places) {
            if let Some((_, _, sh)) = self.place_days.iter().find(|(_, mm, _)| *mm == m) { return Some(*sh); }
        }
        if let Some(r) = by_time(c) { return r; }
        // a clause with neither, cut off by commas: its sentence ("In Tokyo, the time is 01:04 on November 12, 2026
        // (Thursday late night, UTC+09:00)")
        let s = Self::sentence(text, at);
        by_name(s).or_else(|| by_time(s).flatten())
    }

    /// the sentence of `text` around byte `at` (to the nearest . ; ! ? or line end)
    fn sentence(text: &str, at: usize) -> &str {
        let b = ['.', ';', '!', '?', '\n'];
        let st = text[..at].rfind(b).map_or(0, |i| i + 1);
        let en = text[at..].find(b).map_or(text.len(), |i| at + i);
        &text[st..en]
    }

    /// `reply` with another day's name made today's, and another part of the day right next to a time or to the day
    /// made the real one; the form (accents or not, the first letter's case) kept.
    fn fix(&self, reply: &str) -> String {
        let mut s = reply.to_string();
        let (today_a, today_p) = &self.days[self.day];
        // a reply that names today already may name other days for real ("today is Tuesday, tomorrow Wednesday")
        let has = |s: &str, w: &str| !w.is_empty() && find_word(&s.to_lowercase(), &w.to_lowercase()).is_some();
        let names_today = has(&s, today_a) || has(&s, today_p);
        for (i, (a, p)) in self.days.iter().enumerate() {
            for (from, plain) in [(a, false), (p, true)] {
                if from.is_empty() { continue; }
                let re = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(from))).unwrap();
                let src = s.clone();
                s = re.replace_all(&src, |c: &regex::Captures| {
                    let at = c.get(0).map_or(0, |x| x.start());
                    // a clause about another place (named, or holding its time) has that place's day: it may be
                    // tomorrow there ("In Tokyo it's 01:04 Thursday" at 23:04 Wednesday here); else today's
                    let want = match self.place_shift(&src, at) {
                        Some(shift) => (self.day as i64 + shift).rem_euclid(7) as usize,
                        // (a place whose day is not known keeps the day as written)
                        None if self.place_hour(&src, at).is_some() => return c[0].to_string(),
                        None if i == self.day || names_today => return c[0].to_string(),
                        None => self.day,
                    };
                    if want == i { return c[0].to_string(); }
                    let to = if plain { &self.days[want].1 } else { &self.days[want].0 };
                    if to.is_empty() || to.eq_ignore_ascii_case(from) { return c[0].to_string(); }
                    same_case(&c[0], to)
                }).into_owned();
            }
        }
        // a day name followed by another day's own last word that ends the clause ("thứ sáu ba.": "ba" of "thứ ba"):
        // two days run together; the stray word goes ("thứ sáu năm 2026" keeps its year)
        let tails: Vec<String> = self.days.iter().flat_map(|(a, p)| [a, p]).filter_map(|f| f.split_whitespace().nth(1).map(str::to_lowercase)).collect();
        if !tails.is_empty() {
            let names = self.days.iter().flat_map(|(a, p)| [a, p]).filter(|f| !f.is_empty()).map(|f| regex::escape(f)).collect::<Vec<_>>().join("|");
            let tail = tails.iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|");
            let re = regex::Regex::new(&format!(r"(?i)\b(?P<d>{names})[ \t]+(?P<t>{tail})(?P<e>[ \t]*(?:[.!?,;:]|$))")).unwrap();
            s = re.replace_all(&s, |c: &regex::Captures| format!("{}{}", &c["d"], &c["e"])).into_owned();
        }
        // the question's "which day" words written where the day goes ("22:55 tối Thu May"): today's name
        for q in &self.day_asks {
            let to = if accented(q) { today_a } else { today_p };
            let re = regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(q))).unwrap();
            s = re.replace_all(&s, |c: &regex::Captures| same_case(&c[0], to)).into_owned();
        }
        // a part of the day right before or after a time ("22:10 chiều") or today's name ("Thứ Ba chiều", also a
        // named place's day: "01:04 Thursday, late night", a comma allowed after it): the part of that time's own hour
        // ("Tokyo 05:06 late night" read off the local line → morning there), or today's now
        let mut names = vec![today_a.as_str(), today_p.as_str()];
        for (_, _, sh) in &self.place_days {
            let (a, p) = &self.days[(self.day as i64 + sh).rem_euclid(7) as usize];
            names.extend([a.as_str(), p.as_str()]);
        }
        let anchor = std::iter::once(r"\d{1,2}[:h]\d{2}".to_string()).chain(names.iter().filter(|n| !n.is_empty()).map(|n| regex::escape(n))).collect::<Vec<_>>().join("|");
        let hour_re = regex::Regex::new(r"^(\d{1,2})[:h]\d{2}$").unwrap();
        let hour_of = |anchor_text: &str| -> Option<u32> {
            hour_re.captures(anchor_text.trim()).and_then(|c| c[1].parse::<u32>().ok()).filter(|h| *h < 24)
        };
        // the part of `h` in the language's own hours ("12:07" is "trưa" in Vietnamese), or today's now
        let part_for = |h: u32| -> usize {
            self.lang.and_then(|l| l.part_at(h)).and_then(|k| self.keys.iter().position(|x| x == k)).unwrap_or(self.part)
        };
        for (i, (a, p)) in self.parts.clone().into_iter().enumerate() {
            for (plain, from) in [(false, a), (true, p)] {
                if from.is_empty() { continue; }
                let w = regex::escape(&from);
                let re = regex::Regex::new(&format!(r"(?i)(?P<a>(?:{anchor}),?[ \t]+)(?P<w>{w})\b|\b(?P<w2>{w})(?P<b>[ \t]+(?:{anchor}))")).unwrap();
                let src = s.clone();
                s = re.replace_all(&src, |c: &regex::Captures| {
                    let (word, at) = match (c.name("w"), c.name("w2")) { (Some(x), _) => (x.as_str(), c["a"].to_string()), (_, Some(x)) => (x.as_str(), c["b"].to_string()), _ => return c[0].to_string() };
                    // a clause with a time in it, or about another place, follows that time or place ("In Tokyo,
                    // it's 06:10 Wednesday late night" → morning); else the time next to it, or here and now
                    let hour = self.clause_hour(&src, c.get(0).map_or(0, |x| x.start())).or_else(|| hour_of(&at)).unwrap_or(self.hour);
                    // a part also right at that hour ("11:30 trưa", "12:30 afternoon") is left as written, and so is
                    // one right at the twelve-hour figure's afternoon reading ("2:22 chiều" at 14:22)
                    let pm = hour_of(&at).filter(|h| *h < 12).map(|h| h + 12);
                    if self.lang.is_some_and(|l| self.keys.get(i).is_some_and(|k| l.part_holds(k, hour) || pm.is_some_and(|h| l.part_holds(k, h)))) { return c[0].to_string(); }
                    let right = part_for(hour);
                    // the same writing as the word (accents or not)
                    let Some((ra, rp)) = self.parts.get(right) else { return c[0].to_string() };
                    let to = if plain { rp } else { ra };
                    // a part the user named themself is blank there: left as written
                    if to.is_empty() || to.eq_ignore_ascii_case(word) { return c[0].to_string(); }
                    match c.name("w") { Some(_) => format!("{at}{}", same_case(word, to)), None => format!("{}{at}", same_case(word, to)) }
                }).into_owned();
            }
        }
        s
    }

    /// the clause of `text` around byte `at`
    fn clause(text: &str, at: usize) -> &str {
        let b = ['.', ',', ';', '!', '?', '\n'];
        let st = text[..at].rfind(b).map_or(0, |i| i + 1);
        let en = text[at..].find(b).map_or(text.len(), |i| at + i);
        &text[st..en]
    }

    /// The hour of another place the clause around `at` names (`places`), None when it names none.
    fn place_hour(&self, text: &str, at: usize) -> Option<u32> {
        place_at(text, at, at, &self.places).map(|m| m / 60)
    }

    /// The hour the clause around `at` is about: a time written in it ("06:10"), else another place it names; a clause
    /// with neither (cut off by commas: "… 17:04 on November 11, 2026 (Wednesday afternoon, UTC+01:00)") has its
    /// sentence's one time, or the place its sentence names.
    fn clause_hour(&self, text: &str, at: usize) -> Option<u32> {
        static T: OnceLock<regex::Regex> = OnceLock::new();
        let re = T.get_or_init(|| regex::Regex::new(r"(?i)(?:^|[^\d+\-:])(\d{1,2})[:h](\d{2})(\s*([ap])\.?\s*m\b)?").unwrap());
        let hour = |m: &regex::Captures| -> Option<u32> {
            let h = m[1].parse::<u32>().ok()?;
            let h = match m.get(4).map(|x| x.as_str().to_ascii_lowercase()) { Some(x) if x == "p" => h % 12 + 12, Some(_) => h % 12, None => h };
            (h < 24).then_some(h)
        };
        let c = Self::clause(text, at);
        re.captures(c).and_then(|m| hour(&m)).or_else(|| self.place_hour(text, at)).or_else(|| {
            let s = Self::sentence(text, at);
            let hs: Vec<u32> = re.captures_iter(s).filter_map(|m| hour(&m)).collect();
            match hs.as_slice() {
                [h] => Some(*h),
                _ => { let low = s.to_lowercase(); self.places.iter().find(|(p, _)| low.contains(p.as_str())).map(|(_, m)| m / 60) }
            }
        })
    }
}

// ----- a clock time plus or minus a duration -----

const CLOCK_SUM_DATA: &str = include_str!("../../data/clock_sum_words.txt");

/// how the part for a clock sum whose way Elisa cannot tell starts (`clock_sum_part`; `ReplyFacts` asks the user)
pub const CLOCK_UNSURE: &str = "Elisa cannot tell whether the duration comes after or before the start time";

/// every "key: value" line of `data/clock_sum_words.txt`, as written
fn clock_sum_data() -> &'static [(String, String)] {
    static ALL: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ALL.get_or_init(|| CLOCK_SUM_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(_, v)| !v.is_empty()).collect())
}

/// the wordings under `key`, lowercase, the longest first
fn clock_sum_words(key: &str) -> Vec<String> {
    let mut v: Vec<String> = clock_sum_data().iter().filter(|(k, _)| k == key).map(|(_, v)| v.to_lowercase()).collect();
    v.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    v
}

/// the text under "`key`.vi" or "`key`.en" (the first "`key`." line when the language has none)
fn clock_sum_text(key: &str, vi: bool) -> String {
    let want = format!("{key}.{}", if vi { "vi" } else { "en" });
    let any = format!("{key}.");
    clock_sum_data().iter().find(|(k, _)| *k == want).or_else(|| clock_sum_data().iter().find(|(k, _)| k.starts_with(&any))).map(|(_, v)| v.clone()).unwrap_or_default()
}

/// the regexes reading times and durations by the words of `data/clock_sum_words.txt` (case-insensitive, run on
/// the text as written so the places are its own)
struct ClockSumRes { time: regex::Regex, bare: regex::Regex, oclock: regex::Regex, hours: regex::Regex, minutes: regex::Regex }

fn clock_sum_res() -> &'static ClockSumRes {
    static RE: OnceLock<ClockSumRes> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = |k: &str| clock_sum_words(k).iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join("|");
        let marks = format!("{}|{}", alt("pm"), alt("am"));
        let parts = format!("{}|{}", alt("pm part"), alt("am part"));
        let (at, oclock, hours, minutes, join) = (alt("at"), alt("oclock"), alt("hours"), alt("minutes"), alt("join"));
        // (what may follow a word: the end, or anything but a letter or a digit)
        let end = r"(?:$|[^\p{L}\p{N}])";
        ClockSumRes {
            // "3:20 pm", "15:20", "15h20", "3:20 chiều"
            time: regex::Regex::new(&format!(r"(?i)(?:^|[^\d:.])(?P<h>\d{{1,2}})(?P<sep>[:h])(?P<m>\d{{2}})(?:\s*(?P<mark>{marks}){end}|\s+(?P<part>{parts}){end})?")).unwrap(),
            // "3 pm", "9am"
            bare: regex::Regex::new(&format!(r"(?i)(?:^|[^\d:.,])(?P<h>\d{{1,2}})\s*(?P<mark>{marks}){end}")).unwrap(),
            // "lúc 3 giờ 20 chiều", "at 3 o'clock", "từ 8 giờ sáng"
            oclock: regex::Regex::new(&format!(r"(?i)(?:^|[^\p{{L}}])(?:{at})\s+(?P<h>\d{{1,2}})\s*(?:{oclock})(?:\s+(?P<m>\d{{1,2}})(?:\s*(?:{minutes}))?)?(?:\s*(?P<mark>{marks})|\s*(?P<part>{parts}))?{end}")).unwrap(),
            // "2 hours 55 minutes", "2 hours and 55 minutes", "1.5 hours", "2 tiếng", "2h30m"
            hours: regex::Regex::new(&format!(r"(?i)(?:^|[^\d:.,])(?P<h>\d+(?:[.,]\d+)?)\s*(?:{hours})(?:\s*(?:(?:{join})\s+)?(?P<m>\d{{1,2}})\s*(?:{minutes}))?{end}")).unwrap(),
            // "45 minutes", "90 min"
            minutes: regex::Regex::new(&format!(r"(?i)(?:^|[^\d:.,])(?P<m>\d+)\s*(?:{minutes}){end}")).unwrap(),
        }
    })
}

/// A clock time found in a text: its place (from the hour's first digit to the end of its minutes, mark or part
/// word), the minutes of the day it may mean (two for a bare "6:15"), its 12-hour mark as written, whether a mark or
/// a part-of-day word made it a time of day, and whether its hour had two digits.
#[derive(Debug, Clone)]
struct FoundTime { a: usize, b: usize, cands: Vec<u32>, mark: Option<String>, marked: bool, padded: bool }

/// whether `text` up to byte `at` ends with one of `words` as a whole word
fn ends_with_word(text: &str, at: usize, words: &[String]) -> bool {
    let before = text[..at].to_lowercase();
    let before = before.trim_end();
    words.iter().any(|w| before.ends_with(w.as_str()) && before[..before.len() - w.len()].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

/// The clock times of `text` (`clock_sum_res`): a time with a 12-hour mark or a part-of-day word is that time; a bare
/// one up to 12 may be either half of the day. "15h20" counts only right after an `at` word ("2h30" is a duration).
fn clock_times_in(text: &str) -> Vec<FoundTime> {
    let re = clock_sum_res();
    let (at, pm_marks, pm_parts) = (clock_sum_words("at"), clock_sum_words("pm"), clock_sum_words("pm part"));
    let minute = |h: u32, m: u32, pm: Option<bool>, by_part: bool| -> Vec<u32> {
        if m > 59 || h > 23 { return Vec::new(); }
        match pm {
            Some(true) if (1..=12).contains(&h) => vec![(h % 12 + 12) * 60 + m],
            Some(false) if (1..=12).contains(&h) => vec![(h % 12) * 60 + m],
            Some(_) if by_part => vec![h * 60 + m],
            Some(_) => Vec::new(),
            None if (1..=12).contains(&h) => vec![h * 60 + m, (h % 12 + 12) * 60 + m],
            None => vec![h * 60 + m],
        }
    };
    let mut out: Vec<FoundTime> = Vec::new();
    let overlaps = |out: &[FoundTime], a: usize, b: usize| out.iter().any(|f| a < f.b && f.a < b);
    let push = |out: &mut Vec<FoundTime>, c: &regex::Captures, default_m: u32| {
        let (Some(h), Some(m)) = (c.name("h"), c.name("m").map(|x| x.as_str().parse::<u32>().unwrap_or(99)).or(Some(default_m))) else { return };
        let Ok(hour) = h.as_str().parse::<u32>() else { return };
        let (mark, part) = (c.name("mark"), c.name("part"));
        let pm = mark.map(|x| pm_marks.contains(&x.as_str().to_lowercase())).or_else(|| part.map(|p| pm_parts.contains(&p.as_str().to_lowercase())));
        let cands = minute(hour, m, pm, part.is_some());
        let b = mark.or(part).map(|x| x.end()).or_else(|| c.name("m").map(|x| x.end())).unwrap_or(h.end());
        if cands.is_empty() || overlaps(out, h.start(), b) { return; }
        out.push(FoundTime { a: h.start(), b, cands, mark: mark.map(|x| x.as_str().to_string()), marked: mark.is_some() || part.is_some(), padded: h.as_str().len() == 2 });
    };
    for c in re.time.captures_iter(text) {
        if c.name("sep").is_some_and(|s| s.as_str().eq_ignore_ascii_case("h")) && !c.name("h").is_some_and(|h| ends_with_word(text, h.start(), &at)) { continue; }
        push(&mut out, &c, 0);
    }
    for c in re.oclock.captures_iter(text) { push(&mut out, &c, 0); }
    for c in re.bare.captures_iter(text) { push(&mut out, &c, 0); }
    out.sort_by_key(|f| f.a);
    out
}

/// A clock time plus or minus a duration in the user's words ("a train leaves at 3:20 pm and the trip takes 2 hours
/// 55 minutes. when does it arrive?"), worked out by Elisa (`data/clock_sum_words.txt`).
#[derive(Debug, Clone)]
struct ClockSum {
    /// the start as a minute of its day, and the duration in minutes
    start: u32, duration: u32,
    /// the duration comes after the start (true) or before it (false); None when the words do not say (the user is asked)
    later: Option<bool>,
    /// the start's 12-hour mark as the user wrote it ("pm", "PM", "p.m."); None for a 24-hour time
    mark: Option<String>,
    /// a bare start up to 12 with no mark or part word ("3:20"): either half of the day
    ambiguous: bool,
    /// the start's hour written with two digits ("09:30")
    padded: bool,
    vi: bool,
}

/// The clock sum of the user's words: one clock time, one or more durations (added up) and a word asking the time
/// (`asks`); the way from an `earlier` word (before), else a `later`, `later right before` or `span` word (after);
/// both an `earlier` and a `later` word, or none, leave the way open. None when the words hold no such question,
/// or a number follows a duration with no unit of its own ("2 hours 3 times a week": no reading of it).
fn clock_sum(own: &str) -> Option<ClockSum> {
    let low = own.to_lowercase();
    let has = |k: &str| clock_sum_words(k).iter().any(|w| has_phrase(&low, w));
    if !has("asks") { return None; }
    let times = clock_times_in(own);
    let [t] = times.as_slice() else { return None };
    let re = clock_sum_res();
    let mut spans: Vec<(usize, usize)> = vec![(t.a, t.b)];
    let overlaps = |spans: &[(usize, usize)], a: usize, b: usize| spans.iter().any(|(x, y)| a < *y && *x < b);
    let mut total = 0f64;
    for c in re.hours.captures_iter(own) {
        let (Some(h), Some(whole)) = (c.name("h"), c.get(0)) else { continue };
        if overlaps(&spans, h.start(), whole.end()) { continue; }
        let hours: f64 = h.as_str().replace(',', ".").parse().ok()?;
        let mins = c.name("m").map(|x| x.as_str().parse::<f64>().unwrap_or(0.0));
        if mins.is_none() && own[whole.end()..].trim_start().starts_with(|ch: char| ch.is_ascii_digit()) { return None; }
        total += hours * 60.0 + mins.unwrap_or(0.0);
        spans.push((h.start(), whole.end()));
    }
    for c in re.minutes.captures_iter(own) {
        let (Some(m), Some(whole)) = (c.name("m"), c.get(0)) else { continue };
        if overlaps(&spans, m.start(), whole.end()) { continue; }
        total += m.as_str().parse::<f64>().ok()?;
        spans.push((m.start(), whole.end()));
    }
    let duration = total.round() as u32;
    if duration == 0 || duration >= 7 * 24 * 60 { return None; }
    let right_before = clock_sum_words("later right before");
    let before_a_duration = spans[1..].iter().any(|(a, _)| ends_with_word(own, *a, &right_before));
    let later = match (has("earlier"), has("later") || before_a_duration, has("span")) {
        (true, true, _) => None,
        (true, false, _) => Some(false),
        (false, true, _) | (false, false, true) => Some(true),
        (false, false, false) => None,
    };
    let [start] = t.cands.as_slice() else {
        // (a bare "3:20": read as written, either half of the day accepted in a reply)
        let start = *t.cands.first()?;
        return Some(ClockSum { start, duration, later, mark: None, ambiguous: true, padded: t.padded, vi: vi_words(own) });
    };
    Some(ClockSum { start: *start, duration, later, mark: t.mark.clone(), ambiguous: false, padded: t.padded, vi: vi_words(own) })
}

/// whether the words are Vietnamese (`reply_lang`)
fn vi_words(own: &str) -> bool { matches!(crate::reply_lang::detect(own), Some(crate::reply_lang::Lang::Vi)) }

impl ClockSum {
    /// the result in minutes from the start's midnight (past 1440, or below 0, when it crosses midnight)
    fn result(&self) -> Option<i64> {
        self.later.map(|l| if l { self.start as i64 + self.duration as i64 } else { self.start as i64 - self.duration as i64 })
    }

    /// the minutes of the day a reply may give the result as (both halves of the day for an ambiguous start)
    fn accepted(&self) -> Vec<u32> {
        let Some(r) = self.result() else { return Vec::new() };
        let m = r.rem_euclid(1440) as u32;
        if self.ambiguous { vec![m, (m + 720) % 1440] } else { vec![m] }
    }

    /// a clock time written with `mark` as its 12-hour mark ("6:15 pm", the mark's case kept), or as a 24-hour time
    /// with two digits when `padded` or the hour is two digits
    fn clock_with(minute: i64, mark: Option<&str>, padded: bool) -> String {
        let m = minute.rem_euclid(1440) as u32;
        let (h, mm) = (m / 60, m % 60);
        match mark {
            Some(mark) => {
                let low = mark.to_lowercase();
                let s = low.replacen(if low.starts_with('p') { "p" } else { "a" }, if h >= 12 { "p" } else { "a" }, 1);
                let s = if mark.chars().any(char::is_uppercase) { s.to_uppercase() } else { s };
                format!("{}:{mm:02} {s}", if h % 12 == 0 { 12 } else { h % 12 })
            }
            None if padded || h >= 10 => format!("{h:02}:{mm:02}"),
            None => format!("{h}:{mm:02}"),
        }
    }

    /// a clock time as the user wrote the start
    fn clock(&self, minute: i64) -> String { Self::clock_with(minute, self.mark.as_deref(), self.padded) }

    /// the duration as said in Elisa's working ("2 h 55 min", "2 giờ 55 phút")
    fn duration_said(&self) -> String {
        let (h, m) = (self.duration / 60, self.duration % 60);
        let key = match (h, m) { (0, _) => "m", (_, 0) => "h", _ => "hm" };
        clock_sum_text(key, self.vi).replace("{h}", &h.to_string()).replace("{m}", &m.to_string())
    }

    /// Elisa's working: "3:20 pm + 2 h 55 min = 6:15 pm", with the day said when it crosses midnight. None when the
    /// way is not known.
    fn working(&self) -> Option<String> {
        let r = self.result()?;
        let mut s = clock_sum_text("working", self.vi).replace("{start}", &self.clock(self.start as i64)).replace("{sign}", if self.later == Some(true) { "+" } else { "−" })
            .replace("{duration}", &self.duration_said()).replace("{result}", &self.clock(r));
        if r >= 1440 { s = format!("{s} {}", clock_sum_text("next day", self.vi)); } else if r < 0 { s = format!("{s} {}", clock_sum_text("day before", self.vi)); }
        Some(s)
    }

    /// the situation part: the working as a computed fact, or, the way not known, the part that has the reply ask
    fn part(&self) -> String {
        match self.working() {
            Some(w) => format!("{COMPUTED}: {w} · {USE_ONE}"),
            None => format!("{CLOCK_UNSURE} ({} and {}) · give no time; ask the user, in the language of their message, whether the {} comes after or before {}",
                self.clock(self.start as i64), self.duration_said(), self.duration_said(), self.clock(self.start as i64)),
        }
    }

    /// `reply` made true to the sum: one stating the result in any form is left; one time nothing explains (not the
    /// start, the start moved by the whole hours, or the duration written as a clock in a working beside the start,
    /// "3:20 + 2:55") becomes the result; several such times make the reply Elisa's working alone; no time at all gets
    /// the working added. The way not known, a reply that states a time without asking becomes Elisa's own question
    /// (`data/confirm_lines.txt`). None when nothing is off.
    fn fix(&self, reply: &str, ask: &str) -> Option<String> {
        let found = clock_times_in(reply);
        let halves = |m: u32| if self.ambiguous { vec![m, (m + 720) % 1440] } else { vec![m] };
        let says = |f: &FoundTime, m: u32| halves(m).iter().any(|x| f.cands.contains(x));
        let says_start = found.iter().any(|f| says(f, self.start));
        // (a bare time equal to the duration, beside the start: the duration written as a clock; "1:30 am" never is)
        let is_duration = |f: &FoundTime| !f.marked && says_start && says(f, self.duration % 1440);
        let Some(r) = self.result() else {
            // (a reply that only says the start or the duration again guesses nothing)
            let guesses = found.iter().any(|f| !says(f, self.start) && !is_duration(f));
            if !guesses || reply.contains(['?', '？']) { return None; }
            return confirm_line_for(ask, "clock").map(|q| q.replace("{duration}", &self.duration_said()).replace("{start}", &self.clock(self.start as i64)));
        };
        let accepted = self.accepted();
        if found.iter().any(|f| f.cands.iter().any(|c| accepted.contains(c))) { return None; }
        let hours_only = self.duration / 60 * 60;
        let moved = if self.later == Some(true) { self.start as i64 + hours_only as i64 } else { self.start as i64 - hours_only as i64 };
        let unknown: Vec<&FoundTime> = found.iter().filter(|f| !says(f, self.start) && !says(f, moved.rem_euclid(1440) as u32) && !is_duration(f)).collect();
        let working = self.working()?;
        match unknown.as_slice() {
            [] => Some(format!("{}\n\n{working}", reply.trim_end())),
            [one] => {
                let time = match &one.mark { Some(m) => Self::clock_with(r, Some(m), false), None => self.clock(r) };
                Some(format!("{}{time}{}", &reply[..one.a], &reply[one.b..]))
            }
            _ => Some(working),
        }
    }
}

/// Whether the user's words hold a clock time plus or minus a duration (`clock_sum`): no time-of-day ask, and no
/// arithmetic for a model to read.
pub fn is_clock_sum(own: &str) -> bool { clock_sum(own).is_some() }

/// For a clock time plus or minus a duration in the user's words: Elisa's result as a computed-facts part ("Elisa
/// computed it exactly: 3:20 pm + 2 h 55 min = 6:15 pm · answer with this result"), or the part that has the reply
/// ask which way the duration goes. None for any other message.
pub fn clock_sum_part(own: &str) -> Option<String> { clock_sum(own).map(|c| c.part()) }

// ----- date questions -----

const DATE_WORDS: &str = include_str!("../../data/date_words.txt");

/// how the part for a day whose date moves from year to year starts (`date_part`; `ReplyFacts` asks the user)
pub const DATE_UNSURE: &str = "the day asked about falls on a different date each year and Elisa cannot be sure of it";

/// One language's date-question words (`data/date_words.txt`).
#[derive(Debug, Default)]
struct DateLang { until: Vec<String>, since: Vec<String>, weekday: Vec<String>, from_now: Vec<String>, months: Vec<Vec<String>>, day_first: bool, vi: bool,
    date: String, weekday_say: String, relative: Vec<(String, i64)>, on: String,
    /// the words for an event still to come, before its verb; before a verb's -ing form; each verb's forms
    /// (base, -s, -ing, past); the "already" word taking an `ahead` word's place (with accents, without)
    ahead: Vec<String>, ongoing: Vec<String>, verbs: Vec<[String; 4]>, done: (String, String) }

#[derive(Debug, Default)]
struct DateWords { langs: Vec<DateLang>, holidays: Vec<(u32, u32, String)>, moving: Vec<String> }

fn date_words() -> &'static DateWords {
    static D: OnceLock<DateWords> = OnceLock::new();
    D.get_or_init(|| {
        let mut d = DateWords::default();
        let list = |v: &str| v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect::<Vec<_>>();
        let mut sec = String::new();
        for l in DATE_WORDS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') {
                sec = l.to_string();
                if sec != "[holidays]" && sec != "[moving]" { d.langs.push(DateLang { vi: sec == "[vi]", ..Default::default() }); }
                continue;
            }
            match sec.as_str() {
                "[moving]" => d.moving.extend(list(l)),
                "[holidays]" => {
                    let Some((md, names)) = l.split_once('=') else { continue };
                    let Some((m, dd)) = md.trim().split_once('-') else { continue };
                    let (Ok(m), Ok(dd)) = (m.parse::<u32>(), dd.parse::<u32>()) else { continue };
                    d.holidays.extend(list(names).into_iter().map(|n| (m, dd, n)));
                }
                _ => {
                    let (Some(cur), Some((k, v))) = (d.langs.last_mut(), l.split_once('=')) else { continue };
                    match k.trim() {
                        "until" => cur.until = list(v),
                        "since" => cur.since = list(v),
                        "weekday" => cur.weekday = list(v),
                        "from now" => cur.from_now = list(v),
                        "day first" => cur.day_first = v.trim() == "yes",
                        "date" => cur.date = v.trim().to_string(),
                        "weekday say" => cur.weekday_say = v.trim().to_string(),
                        "relative" => cur.relative = list(v).into_iter().filter_map(|x| x.rsplit_once(':').and_then(|(w, n)| Some((w.trim().to_string(), n.trim().parse().ok()?)))).collect(),
                        "on" => cur.on = v.trim().to_string(),
                        "ahead" => cur.ahead = list(v),
                        "ongoing" => cur.ongoing = list(v),
                        "verbs" => cur.verbs = v.split(';').filter_map(|f| {
                            let f: Vec<String> = f.split(',').map(|x| x.trim().to_lowercase()).collect();
                            <[String; 4]>::try_from(f).ok().filter(|f| f.iter().all(|x| !x.is_empty()))
                        }).collect(),
                        "done" => cur.done = forms(v),
                        "months" if v.contains("{n}") => cur.months = (1..=12).map(|n| list(&v.replace("{n}", &n.to_string()))).collect(),
                        "months" => cur.months = v.split(';').map(list).collect(),
                        _ => {}
                    }
                }
            }
        }
        // the longest names first ("christmas eve" before "christmas", "tết dương lịch" before "tết")
        d.holidays.sort_by_key(|(_, _, n)| std::cmp::Reverse(n.chars().count()));
        d.moving.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
        d
    })
}

/// days since 1970-01-01 of a date (proleptic Gregorian)
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// the date of days since 1970-01-01
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// the day of the week (0 = Sunday) of days since 1970-01-01 (a Thursday)
fn weekday_of(days: i64) -> usize { (days + 4).rem_euclid(7) as usize }

/// a date as "25 Dec 2026"
fn show_date(days: i64) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, m, d) = civil_from_days(days);
    format!("{d} {} {y}", M[(m - 1) as usize])
}

/// A day the user's words name: (its name as they wrote it, month, day, year if written), or a day whose date moves.
enum DayNamed { Fixed(String, u32, u32, Option<i64>), Moving(String) }

fn day_named(own: &str) -> Option<DayNamed> {
    let low = own.to_lowercase();
    let w = date_words();
    static ISO: OnceLock<regex::Regex> = OnceLock::new();
    let iso = ISO.get_or_init(|| regex::Regex::new(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b").unwrap());
    if let Some(c) = iso.captures(&low) {
        let (y, m, d) = (c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?);
        return Some(DayNamed::Fixed(c[0].to_string(), m, d, Some(y)));
    }
    // a month by name, with its day before or after it ("25 December", "Dec 25th", "25 tháng 12")
    for lang in &w.langs {
        for (i, names) in lang.months.iter().enumerate() {
            for n in names {
                let e = regex::escape(n);
                let re = regex::Regex::new(&format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?{e}\b|\b{e}\s+(\d{{1,2}})(?:st|nd|rd|th)?\b")).ok()?;
                if let Some(c) = re.captures(&low) {
                    let d: u32 = c.get(1).or(c.get(2))?.as_str().parse().ok()?;
                    if (1..=31).contains(&d) { return Some(DayNamed::Fixed(c[0].to_string(), i as u32 + 1, d, None)); }
                }
            }
        }
    }
    // digits "25/12" (day first in Vietnamese) or "12/25"
    static NUM: OnceLock<regex::Regex> = OnceLock::new();
    let num = NUM.get_or_init(|| regex::Regex::new(r"\b(\d{1,2})/(\d{1,2})(?:/(\d{4}))?\b").unwrap());
    if let Some(c) = num.captures(&low) {
        let (a, b): (u32, u32) = (c[1].parse().ok()?, c[2].parse().ok()?);
        let vi = matches!(crate::reply_lang::detect(own), Some(crate::reply_lang::Lang::Vi));
        let day_first = w.langs.iter().find(|l| l.vi == vi).is_some_and(|l| l.day_first);
        let (m, d) = if a > 12 { (b, a) } else if b > 12 { (a, b) } else if day_first { (b, a) } else { (a, b) };
        if (1..=12).contains(&m) && (1..=31).contains(&d) { return Some(DayNamed::Fixed(c[0].to_string(), m, d, c.get(3).and_then(|y| y.as_str().parse().ok()))); }
    }
    if let Some((m, d, n)) = w.holidays.iter().find(|(_, _, n)| has_phrase(&low, n)) {
        // the name as the user wrote it
        let at = find_word(&low, n).unwrap_or(0);
        let shown = own.get(at..at + n.len()).unwrap_or(n).to_string();
        return Some(DayNamed::Fixed(shown, *m, *d, None));
    }
    w.moving.iter().find(|n| has_phrase(&low, n)).map(|n| DayNamed::Moving(n.clone()))
}

/// What a date question asks, worked out: days until / since a day (its name, the day, the count), the day of the
/// week of a day, the date some days from today; or a day Elisa cannot be sure of.
enum DateAsk { Until(String, i64, i64), Since(String, i64, i64), Weekday(i64), FromNow(i64, i64), Moving(String) }

fn date_asked(own: &str, now: i64) -> Option<DateAsk> {
    let low = own.to_lowercase();
    let w = date_words();
    let said = |f: fn(&DateLang) -> &Vec<String>| w.langs.iter().flat_map(|l| f(l).iter()).find(|p| has_phrase(&low, p)).cloned();
    let tm = local_tm(now)?;
    let today = days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32);
    // "30 days from now", "30 ngày nữa là ngày mấy": the number right before the words
    if let Some(p) = said(|l| &l.from_now) {
        let at = find_word(&low, &p)?;
        let before: Vec<&str> = low[..at].split_whitespace().collect();
        let n = before.last().and_then(|x| x.parse::<i64>().ok())
            .or_else(|| (before.len() >= 2).then(|| spoken_number(&before[before.len() - 2..].join(" "))).flatten().map(i64::from))
            .or_else(|| before.last().and_then(|x| spoken_number(x)).map(i64::from))?;
        // ("days from now" words start with the unit: the number is before them)
        return Some(DateAsk::FromNow(n, today + n));
    }
    // (a day question of the clock words naming a day of its own, "what day is Christmas?", asks that day's weekday;
    // "what day is it?" names none)
    let named_day = || matches!(day_named(own), Some(DayNamed::Fixed(..)));
    let kind = if said(|l| &l.since).is_some() { 's' } else if said(|l| &l.until).is_some() { 'u' }
        else if said(|l| &l.weekday).is_some() || (clock_lang(own).is_some() && named_day()) { 'w' } else { return None };
    let (name, m, d, y) = match day_named(own)? { DayNamed::Moving(n) => return Some(DateAsk::Moving(n)), DayNamed::Fixed(n, m, d, y) => (n, m, d, y) };
    let year = tm.tm_year as i64 + 1900;
    let on = |y: i64| days_from_civil(y, m, d);
    match kind {
        'w' => Some(DateAsk::Weekday(y.map_or_else(|| if on(year) >= today { on(year) } else { on(year + 1) }, on))),
        's' => { let t = y.map_or_else(|| if on(year) <= today { on(year) } else { on(year - 1) }, on); Some(DateAsk::Since(name, t, today - t)) }
        _ => {
            let t = y.map_or_else(|| if on(year) >= today { on(year) } else { on(year + 1) }, on);
            // a written year already gone: the days since it
            if t < today { Some(DateAsk::Since(name, t, today - t)) } else { Some(DateAsk::Until(name, t, t - today)) }
        }
    }
}

/// `reply` with each 24-hour time marked AM or PM ("13:18 PM", "00:05 AM") in its 12-hour form ("1:18 PM", "12:05
/// AM"); an hour past 12 is in the afternoon or evening, so its mark is PM.
fn twelve_hour_fix(reply: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\b(\d{1,2}):(\d{2})(\s*)(?i:(am|pm|a\.m\.|p\.m\.))").unwrap());
    re.replace_all(reply, |c: &regex::Captures| {
        let h: u32 = c[1].parse().unwrap_or(0);
        let mark = &c[4];
        if (13..=23).contains(&h) {
            let pm = if mark.starts_with(['a', 'A']) { if mark.chars().nth(1).is_some_and(|x| x == '.') { "p.m." } else if mark.chars().all(char::is_uppercase) { "PM" } else { "pm" } } else { mark };
            format!("{}:{}{}{pm}", h - 12, &c[2], &c[3])
        } else if h == 0 {
            format!("12:{}{}{mark}", &c[2], &c[3])
        } else { c[0].to_string() }
    }).into_owned()
}

/// `reply` without its clock lines, for an ask that is not the time: each sentence with a time of day in digits
/// ("09:46") and a day name (`data/clock_words.txt`) or a date beside it, and a lone separator line ("---") left
/// behind. Unchanged when it has none; the reply itself when it is nothing else.
fn without_clock_lines(reply: &str) -> String {
    static TIME: OnceLock<regex::Regex> = OnceLock::new();
    let time = TIME.get_or_init(|| regex::Regex::new(r"\b\d{1,2}:\d{2}\b").unwrap());
    if !time.is_match(reply) { return reply.to_string(); }
    let year = local_tm(super::now_secs()).map_or(2000, |tm| tm.tm_year as i64 + 1900);
    let dated = |t: &str| {
        let low = t.to_lowercase();
        clock_langs().iter().flat_map(|l| l.days.iter()).any(|(a, p)| find_word(&low, &a.to_lowercase()).is_some() || find_word(&low, &p.to_lowercase()).is_some())
            || !dates_in(t, year, true).is_empty() || !dates_in(t, year, false).is_empty()
    };
    // (a zone offset is the clock's too: "UTC+7", "GMT-05:00")
    static OFFSET: OnceLock<regex::Regex> = OnceLock::new();
    let offset = OFFSET.get_or_init(|| regex::Regex::new(r"(?i)\b(?:utc|gmt)\s*[+\-−]\s*\d{1,2}(?::\d{2})?").unwrap());
    let mut out = String::new();
    let mut rest = reply;
    let mut dropped = false;
    while !rest.is_empty() {
        let cut = rest.char_indices().find(|(i, ch)| *ch == '\n' || (matches!(ch, '.' | '!' | '?') && rest[i + 1..].chars().next().is_none_or(char::is_whitespace)))
            .map_or(rest.len(), |(i, ch)| i + ch.len_utf8());
        let sent = &rest[..cut];
        rest = &rest[cut..];
        // (only the clock's own numbers in it: "it's 25°C as of 09:00 on Wednesday" says more than the clock)
        let mut bare = offset.replace_all(&time.replace_all(sent, " "), " ").into_owned();
        for (a, b, _) in dates_in(&bare.clone(), year, true).into_iter().rev() { bare.replace_range(a..b, " "); }
        if time.is_match(sent) && dated(sent) && !bare.chars().any(|c| c.is_ascii_digit()) { dropped = true; if sent.ends_with('\n') && !out.is_empty() && !out.ends_with('\n') { out.push('\n'); } continue; }
        out.push_str(sent);
    }
    if !dropped { return reply.to_string(); }
    // a separator line with nothing after it any more, and the blank lines it leaves
    let lines: Vec<&str> = out.lines().collect();
    let keep: Vec<&str> = lines.iter().enumerate().filter(|(i, l)| {
        let t = l.trim();
        let rule = !t.is_empty() && t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '));
        !(rule && lines[i + 1..].iter().all(|x| x.trim().is_empty() || x.trim().chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))))
    }).map(|(_, l)| *l).collect();
    let t = keep.join("\n").trim_end().to_string();
    if t.trim().is_empty() { reply.to_string() } else { t }
}

/// The dates written in `text`: "November 6, 2026", "6 November", "Nov 6th", "2026-11-06", "6/11/2026" (in the
/// order of `day_first`), "6 tháng 11 năm 2026" (month names of `data/date_words.txt`); a missing year is `year`.
/// Each as (start, end, days since 1970), in order, none overlapping.
fn dates_in(text: &str, year: i64, day_first: bool) -> Vec<(usize, usize, i64)> {
    static NUM: OnceLock<regex::Regex> = OnceLock::new();
    let num = NUM.get_or_init(|| regex::Regex::new(r"\b(?:(\d{4})-(\d{1,2})-(\d{1,2})|(\d{1,2})/(\d{1,2})(?:/(\d{4}))?)\b").unwrap());
    // one pattern per month, every language's names of it, the longest first
    static MONTHS: OnceLock<Vec<(regex::Regex, u32)>> = OnceLock::new();
    let months = MONTHS.get_or_init(|| (0..12).filter_map(|i| {
        let mut names: Vec<String> = date_words().langs.iter().filter_map(|l| l.months.get(i)).flatten().cloned().collect();
        if names.is_empty() { return None; }
        names.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
        let e = names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|");
        regex::Regex::new(&format!(r"(?i)\b(?:(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?(?:{e})|(?:{e})\s+(\d{{1,2}})(?:st|nd|rd|th)?)\b(?:,?\s+(?:\p{{L}}+\s+)?(\d{{4}})\b)?")).ok().map(|r| (r, i as u32 + 1))
    }).collect());
    let ok = |m: u32, d: u32| (1..=12).contains(&m) && (1..=31).contains(&d);
    let mut out: Vec<(usize, usize, i64)> = Vec::new();
    for c in num.captures_iter(text) {
        let n = |i: usize| c.get(i).and_then(|x| x.as_str().parse::<i64>().ok());
        let ymd = if c.get(1).is_some() { n(1).zip(n(2)).zip(n(3)).map(|((y, m), d)| (y, m as u32, d as u32)) }
            else { n(4).zip(n(5)).map(|(a, b)| { let (d, m) = if day_first { (a, b) } else { (b, a) }; (n(6).unwrap_or(year), m as u32, d as u32) }) };
        let Some((y, m, d)) = ymd.filter(|(_, m, d)| ok(*m, *d)) else { continue };
        let all = c.get(0).unwrap();
        out.push((all.start(), all.end(), days_from_civil(y, m, d)));
    }
    for (re, m) in months {
        for c in re.captures_iter(text) {
            let Some(d) = c.get(1).or(c.get(2)).and_then(|x| x.as_str().parse::<u32>().ok()).filter(|d| ok(*m, *d)) else { continue };
            let y = c.get(3).and_then(|x| x.as_str().parse().ok()).unwrap_or(year);
            let all = c.get(0).unwrap();
            out.push((all.start(), all.end(), days_from_civil(y, *m, d)));
        }
    }
    out.sort_by_key(|(a, b, _)| (*a, std::cmp::Reverse(*b)));
    let mut kept: Vec<(usize, usize, i64)> = Vec::new();
    for x in out { if kept.last().is_none_or(|k| x.0 >= k.1) { kept.push(x); } }
    kept
}

/// The date written at the start of `text` (after spaces, commas or an opening parenthesis, and in Vietnamese one
/// word before its day, "ngày 6 tháng 11"): its days since 1970 and its length in bytes from the start of `text`.
fn date_at(text: &str, year: i64, day_first: bool) -> Option<(i64, usize)> {
    let lead = text.len() - text.trim_start_matches([' ', ',', '(']).len();
    let t = &text[lead..];
    let word = t.find(|c: char| !c.is_alphabetic()).filter(|&i| i > 0 && t[i..].starts_with(' ')).map(|i| i + 1);
    let (a, b, days) = *dates_in(t, year, day_first).first()?;
    (a == 0 || Some(a) == word).then_some((days, lead + b))
}

/// The day a text names by its distance from today (`relative` of `data/date_words.txt`, any language, the longest
/// wording first, as whole words): "tomorrow" or "ngày mai" 1, "today", "tonight" or "hôm nay" 0, "yesterday" -1.
/// None when it names none.
pub fn day_offset(text: &str) -> Option<i64> {
    let low = text.to_lowercase();
    let whole = |p: &str| low.match_indices(p).any(|(i, _)| low[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) && low[i + p.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric()));
    let mut words: Vec<&(String, i64)> = date_words().langs.iter().flat_map(|l| l.relative.iter()).collect();
    words.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count()));
    words.into_iter().find(|(w, _)| !w.is_empty() && whole(w)).map(|(_, n)| *n)
}

/// `reply` with a day word about an event that the turn's results date (`dates`: each date's days since 1970, as
/// written, and the words of its sentence) put right: when the results give the event's date and it is not the day
/// the word means ("today", the results saying "November 6" on 11 Nov), the date in place of the word ("on
/// November 6"). The event is the result sentence sharing most words with the reply's; left as written when no
/// sentence shares two, or a result's date is the word's day.
fn relative_by_results(reply: &str, today: i64, dates: &[(i64, String, Vec<String>)]) -> String {
    if dates.is_empty() { return reply.to_string(); }
    let year = civil_from_days(today).0;
    let mut out = reply.to_string();
    for lang in &date_words().langs {
        for (w, off) in &lang.relative {
            let Ok(re) = regex::Regex::new(&format!(r"(?i)(^|[^\p{{L}}]){}($|[^\p{{L}}'’])", regex::escape(w))) else { continue };
            let mut from = 0;
            while let Some(c) = re.captures(&out[from..]) {
                let m = c.get(0).unwrap();
                let (a, b) = (from + m.start() + c[1].len(), from + m.end() - c[2].len());
                from = b;
                // a date beside the word: `relative_fix`'s
                let head = out[..a].trim_end_matches([' ', ',', '(']);
                if date_at(&out[b..], year, lang.day_first).is_some() || dates_in(head, year, lang.day_first).last().is_some_and(|d| d.1 == head.len()) { continue; }
                // the reply's sentence around the word, and the result sentence about the same thing
                let s0 = out[..a].rfind(['.', '!', '?', '\n']).map_or(0, |i| i + 1);
                let s1 = out[b..].find(['.', '!', '?', '\n']).map_or(out.len(), |i| b + i);
                let words: Vec<String> = plain_words(&out[s0..s1]).into_iter().filter(|x| x.chars().count() >= 4 && *x != w.to_lowercase()).collect();
                // (the sentences sharing most words with it: when one of them dates it on the word's day, they agree)
                let shared = |ws: &Vec<String>| words.iter().filter(|x| ws.contains(x)).count();
                let most = dates.iter().map(|(_, _, ws)| shared(ws)).max().unwrap_or(0);
                if most < 2 { continue; }
                let best: Vec<&(i64, String, Vec<String>)> = dates.iter().filter(|d| shared(&d.2) == most).collect();
                if best.iter().any(|(t, _, _)| *t == today + off) { continue; }
                let at = &best[0].1;
                let has_on = !lang.on.is_empty() && out[..a].trim_end().to_lowercase().ends_with(&format!(" {}", lang.on.to_lowercase()));
                let on = if has_on || lang.on.is_empty() { String::new() } else { format!("{} ", same_case(&out[a..b], &lang.on)) };
                let to = format!("{on}{at}");
                out = format!("{}{to}{}", &out[..a], &out[b..]);
                from = a + to.len();
            }
        }
    }
    out
}

/// The latest date written in `text` (`dates_in`, in the date order of its language: `vi`), as written ("13/10",
/// "Oct 13"), when it is before today by the clock (`now`); None when `text` dates nothing or dates today or later. A
/// date without its year is this year's, or last year's when this year's would be after today.
pub fn dated_before_today(text: &str, vi: bool, now: i64) -> Option<String> {
    latest_dated(text, vi, now).filter(|d| d.before).map(|d| d.written)
}

/// What a text dates last (`latest_dated`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dated {
    /// the date as written ("13/10", "Oct 13", "2020")
    pub written: String,
    /// before today by the clock
    pub before: bool,
    /// a day (not a bare year)
    pub day: bool,
    /// the days from it to today (0 today, negative for a day to come; a bare year counts from its last day)
    pub days_ago: i64,
}

/// The latest day written in `text` (`dates_in`, in the date order of its language: `vi`), as written; when it writes
/// no day, the latest bare year of 1000 to this year ("in 2020", "(2022 est.)", "năm 1703"). A date without its year
/// is this year's, or last year's when this year's would be after today. None when `text` dates nothing.
pub fn latest_dated(text: &str, vi: bool, now: i64) -> Option<Dated> {
    let tm = local_tm(now)?;
    let (year, today) = (tm.tm_year as i64 + 1900, days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32));
    if let Some((a, b, ago)) = day_spans(text, vi, now).into_iter().min_by_key(|x| x.2) {
        return Some(Dated { written: text[a..b].to_string(), before: ago > 0, day: true, days_ago: ago });
    }
    static YEAR: OnceLock<regex::Regex> = OnceLock::new();
    let re = YEAR.get_or_init(|| regex::Regex::new(r"\b(?:1\d|20)\d\d\b").unwrap());
    let y = re.find_iter(text).filter_map(|m| m.as_str().parse::<i64>().ok()).filter(|y| *y <= year).max()?;
    Some(Dated { written: y.to_string(), before: y < year, day: false, days_ago: today - days_from_civil(y, 12, 31) })
}

/// The days written in `text` (`dates_in`, the day first in Vietnamese), each as its byte span and the days from it
/// to today (0 today, negative for a day to come): a day without a year of its own that would lie ahead is last
/// year's ("Sáng 13/10" in November). Empty without one.
pub fn day_spans(text: &str, vi: bool, now: i64) -> Vec<(usize, usize, i64)> {
    let Some(tm) = local_tm(now) else { return Vec::new() };
    let (year, today) = (tm.tm_year as i64 + 1900, days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32));
    let day_first = date_words().langs.iter().find(|l| l.vi == vi).is_some_and(|l| l.day_first);
    // (the same spans read with last year: a span whose day moves had no year of its own)
    let last = dates_in(text, year - 1, day_first);
    dates_in(text, year, day_first).into_iter().map(|(a, b, t)| {
        let before = last.iter().find(|x| x.0 == a && x.1 == b).map(|x| x.2);
        let t = if t > today { before.filter(|p| *p != t).unwrap_or(t) } else { t };
        (a, b, today - t)
    }).collect()
}

/// The days written in `text` as `day_spans`, a day without its year read as this year's even when it lies ahead:
/// a forecast's "T7 14/11" on 13/11 is tomorrow, not last year's.
pub fn day_spans_ahead(text: &str, vi: bool, now: i64) -> Vec<(usize, usize, i64)> {
    let Some(tm) = local_tm(now) else { return Vec::new() };
    let (year, today) = (tm.tm_year as i64 + 1900, days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32));
    let day_first = date_words().langs.iter().find(|l| l.vi == vi).is_some_and(|l| l.day_first);
    dates_in(text, year, day_first).into_iter().map(|(a, b, t)| (a, b, today - t)).collect()
}

/// The dates in a turn's results, each with how it is written and the words of its sentence
/// (`relative_by_results`).
fn result_dates(results: &str, today: i64) -> Vec<(i64, String, Vec<String>)> {
    let year = civil_from_days(today).0;
    // (a link's path is no date of an event: "…/2026/11/06/…" blanked, same length)
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    let url = URL.get_or_init(|| regex::Regex::new(r"\S+://\S+").unwrap());
    let blank = url.replace_all(results, |c: &regex::Captures| " ".repeat(c[0].len())).into_owned();
    let results = blank.as_str();
    let mut out = Vec::new();
    for lang in &date_words().langs {
        for (a, b, t) in dates_in(results, year, lang.day_first) {
            let s0 = results[..a].rfind(['.', '!', '?', '\n']).map_or(0, |i| i + 1);
            let s1 = results[b..].find(['.', '!', '?', '\n']).map_or(results.len(), |i| b + i);
            let at = results[a..b].to_string();
            if !out.iter().any(|(x, y, _): &(i64, String, Vec<String>)| *x == t && *y == at) { out.push((t, at, plain_words(&results[s0..s1]))); }
        }
    }
    out
}

/// `reply` with an event dated before today told as done (`ahead`, `ongoing`, `verbs` and `done` of
/// `data/date_words.txt`): on 11 Nov, "Apple is launching five products on November 6, 2026" becomes "Apple launched
/// five products on November 6, 2026", "Apple sẽ ra mắt … vào 6/11/2026" becomes "Apple đã ra mắt …". Only the last
/// such verb before the date in its own sentence, at most a few words before it with no comma between, never in a
/// question; a date with no year counts only when it is at
/// most two months back (a bare "January 5" said in November is next year's). `today`: days since 1970.
fn tense_fix(reply: &str, today: i64) -> String {
    const NEAR_DAYS: i64 = 61;
    const NEAR_WORDS: usize = 8;
    let year = civil_from_days(today).0;
    static URL: OnceLock<regex::Regex> = OnceLock::new();
    let url = URL.get_or_init(|| regex::Regex::new(r"\S+://\S+").unwrap());
    static YEAR: OnceLock<regex::Regex> = OnceLock::new();
    let has_year = YEAR.get_or_init(|| regex::Regex::new(r"\d{4}").unwrap());
    // each language's verb pattern: (pattern, verbs) built once
    static RES: OnceLock<Vec<Option<regex::Regex>>> = OnceLock::new();
    let alt = |v: &mut Vec<String>| { v.sort_by_key(|x| std::cmp::Reverse(x.len())); v.iter().map(|x| regex::escape(x).replace(' ', r"\s+")).collect::<Vec<_>>().join("|") };
    let res = RES.get_or_init(|| date_words().langs.iter().map(|l| {
        if l.ahead.is_empty() { return None; }
        let mut ahead = l.ahead.clone();
        if l.verbs.is_empty() { return regex::Regex::new(&format!(r"(?i)\b(?P<m>{})\b", alt(&mut ahead))).ok(); }
        let mut base: Vec<String> = l.verbs.iter().map(|v| v[0].clone()).collect();
        let mut s3: Vec<String> = l.verbs.iter().map(|v| v[1].clone()).collect();
        let mut ing: Vec<String> = l.verbs.iter().map(|v| v[2].clone()).collect();
        let mut ongoing = l.ongoing.clone();
        let be = if ongoing.is_empty() { String::new() } else { format!(r"|\b(?:{})\s+(?P<ing>{})\b", alt(&mut ongoing), alt(&mut ing)) };
        regex::Regex::new(&format!(r"(?i)\b(?:{})\s+(?P<base>{})\b{be}|\b(?P<s>{})\b", alt(&mut ahead), alt(&mut base), alt(&mut s3))).ok()
    }).collect());
    let mut out = reply.to_string();
    for (lang, re) in date_words().langs.iter().zip(res) {
        let Some(re) = re else { continue };
        let text = url.replace_all(&out, |c: &regex::Captures| " ".repeat(c[0].len())).into_owned();
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut prev_end = 0;
        for (a, b, days) in dates_in(&text, year, lang.day_first) {
            let from = prev_end.max(text[..a].rfind(['.', '!', '?', ';', '\n']).map_or(0, |i| i + 1));
            prev_end = b;
            let past = days < today && (has_year.is_match(&text[a..b]) || today - days <= NEAR_DAYS);
            let question = text[b..].find(['.', '!', '?', '\n']).is_some_and(|i| text[b + i..].starts_with('?'));
            if !past || question || from > a { continue; }
            let Some(c) = re.captures_iter(&text[from..a]).last() else { continue };
            let all = c.get(0).unwrap();
            // the verb close before its date, nothing set off between ("will launch in December, a report on
            // November 6 says" keeps its verb)
            let between = &text[from + all.end()..a];
            if between.contains([',', '(', ':']) || between.split_whitespace().count() > NEAR_WORDS { continue; }
            let to = if let Some(m) = c.name("m") {
                let (da, dp) = &lang.done;
                if da.is_empty() { continue; }
                if accented(m.as_str()) { da.clone() } else { dp.clone() }
            } else {
                let (form, i) = match (c.name("base"), c.name("ing"), c.name("s")) {
                    (Some(x), _, _) => (x.as_str(), 0), (_, Some(x), _) => (x.as_str(), 2), (_, _, Some(x)) => (x.as_str(), 1), _ => continue,
                };
                let form = form.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
                let Some(v) = lang.verbs.iter().find(|v| v[i] == form) else { continue };
                v[3].clone()
            };
            edits.push((from + all.start(), from + all.end(), same_case(all.as_str(), &to)));
        }
        for (a, b, to) in edits.into_iter().rev() { out.replace_range(a..b, &to); }
    }
    out
}

/// `reply` without a day word that a date beside it gives the lie to (`relative` of `data/date_words.txt`): on
/// 11 Nov, "launches tomorrow, November 6, 2026" (copied from a 5 Nov news story) becomes "launches on November 6,
/// 2026"; a day word that fits its date stays. `today`: days since 1970.
fn relative_fix(reply: &str, today: i64) -> String {
    let year = civil_from_days(today).0;
    let mut out = reply.to_string();
    for lang in &date_words().langs {
        for (w, off) in &lang.relative {
            let Ok(re) = regex::Regex::new(&format!(r"(?i)(^|[^\p{{L}}]){}($|[^\p{{L}}])", regex::escape(w))) else { continue };
            let mut from = 0;
            while let Some(c) = re.captures(&out[from..]) {
                let m = c.get(0).unwrap();
                let (a, b) = (from + m.start() + c[1].len(), from + m.end() - c[2].len());
                from = b;
                // the date right after the word, or right before it ("November 6 (tomorrow)")
                let after = date_at(&out[b..], year, lang.day_first);
                let before = (after.is_none()).then(|| {
                    let head = out[..a].trim_end_matches([' ', ',', '(']);
                    (0..head.len()).rev().filter(|&i| head.is_char_boundary(i) && (i == 0 || !head[..i].ends_with(|ch: char| ch.is_alphanumeric())))
                        .take(8).find_map(|i| date_at(&head[i..], year, lang.day_first).filter(|(_, l)| i + l == head.len()).map(|(t, _)| (t, head.len())))
                }).flatten();
                let Some((t, _)) = after.or(before) else { continue };
                if t - today == *off { continue; }
                if after.is_some() {
                    // the word gone, "on" before the date (kept once when it is there already)
                    let lead = out[b..].len() - out[b..].trim_start_matches([' ', ',', '(']).len();
                    let has_on = !lang.on.is_empty() && out[..a].trim_end().to_lowercase().ends_with(&format!(" {}", lang.on.to_lowercase()));
                    let on = if has_on || lang.on.is_empty() { String::new() } else { format!("{} ", same_case(&out[a..b], &lang.on)) };
                    let paren = out[b..b + lead].contains('(');
                    out = format!("{}{on}{}", &out[..a], &out[b + lead..]);
                    if paren { if let Some(i) = out[a..].find(')') { out.replace_range(a + i..a + i + 1, ""); } }
                } else {
                    // after the date: the word and what joins it gone ("November 6 (tomorrow)")
                    let end = b + out[b..].len() - out[b..].trim_start_matches(')').len();
                    let head = out[..a].trim_end_matches([' ', ',', '(']).len();
                    out = format!("{}{}", &out[..head], &out[end..]);
                }
                from = 0;
            }
        }
    }
    out
}

/// `reply` to a weekday question about the day `t` (days since 1970) with each other date in digits ("11/11/2026",
/// "2026-11-11") or in Vietnamese words ("ngày 12 tháng 1") made that day, in the same way of writing.
fn date_fix(reply: &str, t: i64) -> String {
    let (y, m, d) = civil_from_days(t);
    static NUM: OnceLock<regex::Regex> = OnceLock::new();
    let num = NUM.get_or_init(|| regex::Regex::new(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b|\b(\d{1,2})/(\d{1,2})(/\d{4})?\b").unwrap());
    let s = num.replace_all(reply, |c: &regex::Captures| {
        if c.get(1).is_some() { return format!("{y}-{m:02}-{d:02}"); }
        let (a, b): (u32, u32) = (c[4].parse().unwrap_or(0), c[5].parse().unwrap_or(0));
        // the order as written: day first when either reading of it says so
        let day_first = a > 12 || (b <= 12 && !(a == m && b == d));
        if (a == d && b == m) || (a == m && b == d) { return c[0].to_string(); }
        let year = c.get(6).map(|_| format!("/{y}")).unwrap_or_default();
        if day_first { format!("{d}/{m}{year}") } else { format!("{m}/{d}{year}") }
    }).into_owned();
    // the month and day in words, from the data file's month names ("ngày 12 tháng 1", "January 12")
    let mut out = s;
    for lang in &date_words().langs {
        for (i, names) in lang.months.iter().enumerate() {
            for n in names {
                let e = regex::escape(n);
                let Ok(re) = regex::Regex::new(&format!(r"(?i)\b(\d{{1,2}})(\s+(?:of\s+)?){e}\b|\b{e}(\s+)(\d{{1,2}})\b")) else { continue };
                let mi = i as u32 + 1;
                out = re.replace_all(&out, |c: &regex::Captures| {
                    let day: u32 = c.get(1).or(c.get(4)).and_then(|x| x.as_str().parse().ok()).unwrap_or(0);
                    if mi == m && day == d { return c[0].to_string(); }
                    // the right month's name in the same language and form
                    let to = lang.months.get((m - 1) as usize).and_then(|ns| ns.get(names.iter().position(|x| x == n).unwrap_or(0)).or(ns.first())).cloned().unwrap_or_default();
                    let to = same_case(c.get(0).map_or("", |x| x.as_str()).trim_start_matches(|ch: char| ch.is_ascii_digit() || ch.is_whitespace()), &to);
                    match c.get(1) { Some(_) => format!("{d}{}{to}", &c[2]), None => format!("{to}{}{d}", &c[3]) }
                }).into_owned();
            }
        }
    }
    out
}

/// `reply` to a weekday question about the day `t` (`name`: as the user named it) that names no real day of the week
/// (`data/clock_words.txt`): a made-up one where a day goes ("Thứ Nhị": the words the language's day names start
/// with, then another word) becomes the real one; with none at all, Elisa's own answer (`weekday say`) stands in for
/// the reply. One day in the reply, never two.
fn weekday_fill(reply: &str, t: i64, name: &str, ask: &str) -> String {
    let low = reply.to_lowercase();
    let i = weekday_of(t);
    let known = clock_langs().iter().flat_map(|l| l.days.iter()).any(|(a, p)| find_word(&low, &a.to_lowercase()).is_some() || find_word(&low, &p.to_lowercase()).is_some());
    if known { return reply.to_string(); }
    let vi = matches!(crate::reply_lang::detect(ask), Some(crate::reply_lang::Lang::Vi));
    let lang = clock_langs().iter().find(|l| vi == !l.line.is_empty());
    // the first word most of the language's day names share ("Thứ"): that word and the next where a day goes
    if let Some(l) = lang {
        let firsts: Vec<(String, String)> = l.days.iter().map(|(a, p)| (a.split_whitespace().next().unwrap_or("").to_string(), p.split_whitespace().next().unwrap_or("").to_string())).collect();
        if let Some((pa, pp)) = firsts.iter().find(|(a, _)| firsts.iter().filter(|(b, _)| b == a).count() * 2 > firsts.len()).cloned() {
            for (pre, plain) in [(pa, false), (pp, true)] {
                let Ok(re) = regex::Regex::new(&format!(r"(?i)\b{}\s+\p{{L}}+", regex::escape(&pre))) else { continue };
                if let Some(m) = re.find(reply) {
                    let to = l.days.get(i).map(|(a, p)| if plain { p.clone() } else { a.clone() }).unwrap_or_default();
                    return format!("{}{}{}", &reply[..m.start()], same_case(m.as_str(), &to), &reply[m.end()..]);
                }
            }
        }
    }
    // no day at all: Elisa's own answer
    let Some(dl) = date_words().langs.iter().find(|l| l.vi == vi) else { return reply.to_string() };
    let day = lang.and_then(|l| l.days.get(i)).map(|d| d.0.clone()).unwrap_or_default();
    let (y, m, d) = civil_from_days(t);
    let month = dl.months.get((m - 1) as usize).and_then(|n| n.first()).map(|n| { let mut c = n.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) }).unwrap_or_default();
    let date = dl.date.replace("{d}", &d.to_string()).replace("{m}", &m.to_string()).replace("{y}", &y.to_string()).replace("{month}", &month);
    let name = { let mut c = name.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) };
    if dl.weekday_say.is_empty() { return reply.to_string(); }
    dl.weekday_say.replace("{name}", &name).replace("{day}", &day).replace("{date}", &date)
}

/// A date question in the user's words, worked out by Elisa (`data/date_words.txt`), as a computed-facts part:
/// "how many days until Christmas?" on 11 Nov 2026 → "Elisa computed it exactly: days until Christmas = from 11 Nov 2026
/// to 25 Dec 2026 = 44 · answer with this result"; the day of the week of a day; the date some days from today. A
/// day whose date moves (Easter, Tết): the user is asked which date it is. None for any other message.
pub fn date_part(own: &str, now: i64) -> Option<String> {
    let tm = local_tm(now)?;
    let today = days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32);
    let vi = matches!(crate::reply_lang::detect(own), Some(crate::reply_lang::Lang::Vi));
    // a day's name in the user's language too ("Friday (Thứ Sáu)")
    let day_name = |days: i64| {
        let i = weekday_of(days);
        let en = clock_langs().iter().find(|l| l.line.is_empty()).and_then(|l| l.days.get(i)).map(|d| d.0.clone()).unwrap_or_default();
        match clock_langs().iter().find(|l| !l.line.is_empty()).and_then(|l| l.days.get(i)).filter(|_| vi) { Some(v) => format!("{en} ({})", v.0), None => en }
    };
    Some(match date_asked(own, now)? {
        DateAsk::Until(n, t, k) => format!("{COMPUTED}: days until {n} = from {} to {} = {k} · {USE_ONE}", show_date(today), show_date(t)),
        DateAsk::Since(n, t, k) => format!("{COMPUTED}: days since {n} = from {} to {} = {k} · {USE_ONE}", show_date(t), show_date(today)),
        DateAsk::Weekday(t) => format!("{COMPUTED}: day of the week of {} = {} · {USE_ONE}", show_date(t), day_name(t)),
        DateAsk::FromNow(k, t) => format!("{COMPUTED}: the date {k} days from today ({}) = {}, {} · {USE_ONE}", show_date(today), day_name(t), show_date(t)),
        DateAsk::Moving(n) => format!("{DATE_UNSURE} ({n}) · give no number or date; ask the user, in the language of their message, which date it is this year"),
    })
}

// ----- facts of a table in the turn -----

/// the facts a table question can ask for, as `data/table_asks.txt` names its sections
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stat { Sum, Max, Min, Average, Count }

fn table_asks() -> &'static [(Stat, Vec<String>)] {
    static A: OnceLock<Vec<(Stat, Vec<String>)>> = OnceLock::new();
    A.get_or_init(|| {
        let mut out: Vec<(Stat, Vec<String>)> = Vec::new();
        for l in TABLE_ASKS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                let s = match name { "sum" => Stat::Sum, "max" => Stat::Max, "min" => Stat::Min, "average" => Stat::Average, _ => Stat::Count };
                out.push((s, Vec::new()));
            } else if let Some((_, v)) = out.last_mut() { v.push(l.to_lowercase()); }
        }
        out
    })
}

/// A table's header and data rows: the first row with two or more cells is the header when one of its cells in a
/// numbers column is not a number. Numbers columns: every non-empty data cell a number, at least one.
#[derive(Debug, Clone, Default)]
pub struct Table { pub name: String, header: Vec<String>, rows: Vec<Vec<String>> }

impl Table {
    /// The table of a sheet's rows (`mcp_docs::sheet_rows`), None when it has no numbers column with a header.
    pub fn new(name: &str, rows: &[Vec<String>]) -> Option<Table> {
        let rows: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|c| c.trim().to_string()).collect::<Vec<_>>())
            .filter(|r| r.iter().any(|c| !c.is_empty())).collect();
        let (head, data) = rows.split_first()?;
        if head.len() < 2 || data.is_empty() { return None; }
        let t = Table { name: name.to_string(), header: head.clone(), rows: data.to_vec() };
        (!t.numeric().is_empty() && t.label_col().is_some()).then_some(t)
    }

    /// the numbers columns whose header is not a number itself
    fn numeric(&self) -> Vec<usize> {
        (0..self.header.len()).filter(|&c| {
            let cells: Vec<&str> = self.rows.iter().filter_map(|r| r.get(c)).map(String::as_str).filter(|x| !x.is_empty()).collect();
            !cells.is_empty() && cells.iter().all(|x| cell_num(x).is_some()) && cell_num(&self.header[c]).is_none() && !self.header[c].is_empty()
        }).collect()
    }

    /// the first column that is not a numbers column: the rows' labels
    fn label_col(&self) -> Option<usize> {
        let n = self.numeric();
        (0..self.header.len()).find(|c| !n.contains(c))
    }

    fn label(&self, r: &[String]) -> String { self.label_col().and_then(|c| r.get(c)).cloned().unwrap_or_default() }

    /// every (row label, number) of a column
    fn column(&self, c: usize) -> Vec<(String, f64)> {
        self.rows.iter().filter_map(|r| Some((self.label(r), cell_num(r.get(c)?)?))).collect()
    }

    /// (row label, numbers) of every numbers column's cells: what a reply may quote as it is
    pub fn cells(&self) -> Vec<(String, f64)> { self.numeric().into_iter().flat_map(|c| self.column(c)).collect() }
}

/// a column name as words to look for in a message: itself, and without a plural "s"
fn names_column(low: &str, header: &str) -> bool {
    let h = header.to_lowercase();
    has_phrase(low, &h) || h.strip_suffix('s').is_some_and(|x| x.len() >= 3 && has_phrase(low, x)) || has_phrase(low, &format!("{h}s"))
}

/// The facts of `table` that the question (`own`, the user's words) asks for, computed exactly, as a computed-facts
/// part (`COMPUTED`): "Elisa computed it exactly from sales.csv: total units = 120 + 340 + 95 = 555; highest units =
/// South (340) · answer with these results". The column is the numbers column the question names, else the only
/// one; the facts are those `data/table_asks.txt` finds wordings of. None when the question asks none.
pub fn table_facts(own: &str, table: &Table) -> Option<String> {
    let low = own.to_lowercase();
    let asked: Vec<Stat> = table_asks().iter().filter(|(_, ws)| ws.iter().any(|w| has_phrase(&low, w))).map(|(s, _)| *s).collect();
    if asked.is_empty() { return None; }
    let numeric = table.numeric();
    let named: Vec<usize> = numeric.iter().copied().filter(|&c| names_column(&low, &table.header[c])).collect();
    let label_named = table.label_col().is_some_and(|c| names_column(&low, &table.header[c]));
    let cols = if !named.is_empty() { named.clone() } else if numeric.len() == 1 { numeric.clone() } else { Vec::new() };
    let mut items: Vec<String> = Vec::new();
    // rows counted: a count ask naming the labels column ("how many regions?"), not a numbers one ("how many units")
    if asked.contains(&Stat::Count) && label_named && named.is_empty() {
        let h = table.label_col().map(|c| table.header[c].clone()).unwrap_or_default();
        items.push(format!("number of {h} = {}", table.rows.len()));
    }
    for c in cols.into_iter().take(3) {
        let col = table.column(c);
        if col.is_empty() { continue; }
        let h = &table.header[c];
        let sum: f64 = col.iter().map(|(_, v)| v).sum();
        for s in &asked {
            match s {
                Stat::Sum => {
                    let parts = if col.len() <= 8 { format!("{} = ", col.iter().map(|(_, v)| show_num(*v)).collect::<Vec<_>>().join(" + ")) } else { String::new() };
                    items.push(format!("total {h} = {parts}{}", show_num(sum)));
                }
                Stat::Max | Stat::Min => {
                    let pick = col.iter().cloned().reduce(|a, b| if (*s == Stat::Max) == (b.1 > a.1) && b.1 != a.1 { b } else { a });
                    if let Some((l, v)) = pick {
                        // a tie is said as all of its rows
                        let ties: Vec<&str> = col.iter().filter(|(_, x)| same(*x, v)).map(|(l, _)| l.as_str()).collect();
                        let l = if ties.len() > 1 { ties.join(" and ") } else { l };
                        items.push(format!("{} {h} = {l} ({})", if *s == Stat::Max { "highest" } else { "lowest" }, show_num(v)));
                    }
                }
                Stat::Average => items.push(format!("average {h} = {} / {} = {}", show_num(sum), col.len(), show_num(sum / col.len() as f64))),
                Stat::Count => {}
            }
        }
    }
    if items.is_empty() { return None; }
    Some(format!("{COMPUTED} from {}: {} · {USE_ALL}", table.name, items.join("; ")))
}

/// `table_facts` for a table file a tool read this turn, as a note after its result ("[Elisa computed it exactly
/// from sales.csv: … · answer with these results]"). None when the file is no table or the question asks no fact.
pub fn table_note(ask: &str, path: &std::path::Path) -> Option<String> {
    if !crate::mcp_docs::is_sheet(path) { return None; }
    let own = crate::context::own_words(ask).unwrap_or(ask);
    let name = path.file_name()?.to_string_lossy().to_string();
    let rows = crate::mcp_docs::sheet_rows(path).ok()?;
    let t = rows.iter().find_map(|(_, r)| Table::new(&name, r))?;
    table_facts(own, &t).map(|f| format!("[{f}]"))
}

// ----- a choice between two numbers -----

/// The two choices of "which is bigger, 3 GB or 5 GB?": each as (as written, its value), when both have the same
/// unit (or none). Empty for any other message.
fn choices(own: &str) -> Vec<(String, f64)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)(\d[\d.,]*)\s*([\p{L}%]+)?\s+(?:or|vs\.?|versus|hay|hoặc|hoac)\s+(\d[\d.,]*)\s*([\p{L}%]+)?").unwrap());
    let Some(c) = re.captures(own) else { return Vec::new() };
    let unit = |i: usize| c.get(i).map(|m| m.as_str().to_string());
    // a unit is a word right after the number, not the next clause ("3 or 5 apples" has one unit for both)
    let (u1, u2) = (unit(2), unit(4));
    let u = match (&u1, &u2) { (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => return Vec::new(), (a, b) => a.clone().or(b.clone()) };
    let (Some(a), Some(b)) = (cell_num(&c[1]), cell_num(&c[3])) else { return Vec::new() };
    if same(a, b) { return Vec::new(); }
    let shown = |n: &str| match &u { Some(u) => format!("{} {u}", n.trim_end_matches(['.', ','])), None => n.trim_end_matches(['.', ',']).to_string() };
    vec![(shown(&c[1]), a), (shown(&c[3]), b)]
}

/// The direction a choice question asks (`data/compare_asks.txt`): Some(true) bigger, Some(false) smaller.
fn compare_way(own: &str) -> Option<bool> {
    static A: OnceLock<Vec<(bool, String)>> = OnceLock::new();
    let a = A.get_or_init(|| {
        let mut out = Vec::new();
        let mut way = true;
        for l in COMPARE_ASKS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { way = l == "[bigger]"; } else { out.push((way, l.to_lowercase())); }
        }
        out
    });
    let low = own.to_lowercase();
    let hits: Vec<bool> = a.iter().filter(|(_, w)| has_phrase(&low, w)).map(|(b, _)| *b).collect();
    // both directions named ("is 7 more or less than 4?"): not a pick
    if hits.is_empty() || hits.iter().any(|b| *b != hits[0]) { return None; }
    Some(hits[0])
}

/// The answer of a choice between two numbers, picked by Elisa, as a computed-facts part: "which is bigger, 3 GB or
/// 5 GB?" → "Elisa computed it exactly: bigger = 5 GB (5) · answer with this result". None for any other message.
pub fn compare_part(own: &str) -> Option<String> {
    let c = choices(own);
    let way = compare_way(own)?;
    let pick = if (c.first()?.1 > c.get(1)?.1) == way { &c[0] } else { &c[1] };
    Some(format!("{COMPUTED}: {} = {} ({}) · {USE_ONE}", if way { "bigger" } else { "smaller" }, pick.0, show_num(pick.1)))
}

// ----- the arithmetic of a message, with its parts -----

/// The arithmetic in the user's words, computed (`mcp_reason::sums_in`), as a computed-facts part: each sum with
/// the values of its terms when a term is itself computed ("(12.5/100*3480) + 19^2 = 435 + 361 = 796"), so a model
/// does not redo a part (gemma3:1b wrote "12.5% of 3,480 is 445.0"). None when the message has none.
pub fn sums_part(own: &str) -> Option<String> {
    let s = crate::mcp_reason::sums_in(own);
    if s.is_empty() { return None; }
    let list = s.iter().map(|(e, r)| match terms_shown(e) { Some(t) if t != *r => format!("{e} = {t} = {r}"), _ => format!("{e} = {r}") }).collect::<Vec<_>>().join("; ");
    Some(format!("{COMPUTED}: {list} · {USE_ONE}"))
}

/// An expression's top-level terms (split at + and - outside parentheses) with each computed one as its value:
/// "(12.5/100*3480) + 19^2" → "435 + 361". None when it has one term, or every term is a plain number.
fn terms_shown(e: &str) -> Option<String> {
    let mut parts: Vec<(char, String)> = Vec::new();
    let (mut depth, mut cur, mut op) = (0i32, String::new(), '+');
    let cs: Vec<char> = e.chars().collect();
    for (k, &c) in cs.iter().enumerate() {
        let binary = || cs[..k].iter().rev().find(|x| **x != ' ').is_some_and(|p| p.is_ascii_digit() || *p == ')');
        if depth == 0 && matches!(c, '+' | '-' | '−') && binary() {
            parts.push((op, std::mem::take(&mut cur)));
            op = if c == '+' { '+' } else { '-' };
            continue;
        }
        if c == '(' { depth += 1; } else if c == ')' { depth -= 1; }
        cur.push(c);
    }
    parts.push((op, cur));
    if parts.len() < 2 || parts.iter().all(|(_, t)| cell_num(t).is_some()) { return None; }
    let mut out = String::new();
    for (i, (o, t)) in parts.iter().enumerate() {
        let v = crate::mcp_reason::calc(&t.replace(',', "")).ok()?;
        if i > 0 { out.push_str(&format!(" {o} ")); }
        out.push_str(&v);
    }
    Some(out)
}

// ----- a reply checked against the facts -----

/// One computed result (`COMPUTED` parts of the turn): its value, the row it names (a table's highest / lowest),
/// the numbers on the way to it, and its working ("435 + 361 = 796") to add when a reply leaves it out.
#[derive(Debug, Clone)]
struct Computed { value: f64, text: String, label: Option<String>, steps: Vec<f64>, shown: String, unit: Option<String>,
    /// the expression as Elisa computed it ("(12.5/100*3480) + 19^2"): its terms are the parts the steps give
    expr: String }

/// the numbers of a text with their byte ranges: "1,200", "445.0", "-3"; not inside a word, a time, a date or a
/// version, nor a list marker at a line start ("1. North")
fn numbers_at(text: &str) -> Vec<(usize, usize, f64)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?").unwrap());
    re.find_iter(text).filter_map(|m| {
        let (b, a) = (text[..m.start()].chars().next_back(), text[m.end()..].chars().next());
        let glued = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || matches!(c, ':' | '/' | '_'));
        if glued(b) || glued(a) || (a == Some('.') && text[m.end() + 1..].starts_with(|c: char| c.is_ascii_digit())) { return None; }
        // a date's parts ("2026-11-10"), a code's ("ORCHID-42")
        if matches!(b, Some('-')) && text[..m.start() - 1].ends_with(|c: char| c.is_alphanumeric()) { return None; }
        if a == Some('-') && text[m.end() + 1..].starts_with(|c: char| c.is_ascii_digit()) { return None; }
        let line_start = text[..m.start()].rsplit('\n').next().is_some_and(|l| l.trim().is_empty());
        // (a list marker has its item after it on the line: "1. North"; a bare "1." is a number)
        let item = text[m.end()..].split('\n').next().is_some_and(|l| l.chars().skip(1).any(char::is_alphanumeric));
        if line_start && matches!(a, Some('.') | Some(')')) && item { return None; }
        Some((m.start(), m.end(), m.as_str().replace(',', "").parse().ok()?))
    }).collect()
}

/// how Elisa's exact count of a file starts (`mcp_reason::count_note`: "[elisa: counted exactly: notes.txt has 8
/// words; use this number]")
const COUNTED: &str = "counted exactly: ";

/// The top-level terms of an expression ("(12.5/100*3480) + 19^2" → "(12.5/100*3480)", "19^2"): split at + and -
/// outside parentheses.
fn terms_of(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for (i, ch) in expr.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '+' | '-' | '−' if depth == 0 && i > 0 && !cur.trim().is_empty() => { out.push(std::mem::take(&mut cur).trim().to_string()); continue; }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}

/// `reply` with each part of a sum (`c`: one term of its expression per step, "435" for "(12.5/100*3480)") that it
/// gives a wrong value put right: a sentence or line naming that part by the question's own numbers for it (`asked`:
/// "12.5% of 3,480", and none of another part's) and a number nothing explains (not the question's, a step, the
/// result or the term's own "100" / "2") has that number wrong; it becomes the step's value everywhere in the reply.
fn parts_fix(reply: &str, c: &Computed, asked: &[f64]) -> String {
    let terms = terms_of(&c.expr);
    if terms.len() != c.steps.len() { return reply.to_string(); }
    let term_nums: Vec<Vec<f64>> = terms.iter().map(|t| numbers_at(t).into_iter().map(|x| x.2).collect()).collect();
    let term_asked: Vec<Vec<f64>> = term_nums.iter().map(|ns| ns.iter().copied().filter(|n| asked.iter().any(|a| same(*a, *n))).collect()).collect();
    let known = |n: f64| asked.iter().chain(c.steps.iter()).chain(std::iter::once(&c.value)).chain(term_nums.iter().flatten()).any(|k| same(*k, n));
    let mut wrong: Vec<(f64, f64)> = Vec::new();
    let mut rest = reply;
    while !rest.is_empty() {
        let cut = rest.char_indices().find(|(i, ch)| matches!(ch, '\n' | ';') || (matches!(ch, '.' | '!' | '?') && rest[i + 1..].chars().next().is_none_or(char::is_whitespace)))
            .map_or(rest.len(), |(i, ch)| i + ch.len_utf8());
        let sent = &rest[..cut];
        rest = &rest[cut..];
        let nums: Vec<f64> = numbers_at(sent).into_iter().map(|x| x.2).collect();
        let names = |i: usize| !term_asked[i].is_empty() && term_asked[i].iter().all(|a| nums.iter().any(|n| same(*n, *a)));
        let named: Vec<usize> = (0..terms.len()).filter(|&i| names(i)).collect();
        let [i] = named.as_slice() else { continue };
        // (one number left that nothing explains, once the wrong ones found already are set aside: that part's value)
        let left: Vec<f64> = nums.iter().copied().filter(|n| !known(*n) && !wrong.iter().any(|(w, _)| same(*w, *n))).collect();
        if let [n] = left.as_slice() { wrong.push((*n, c.steps[*i])); }
    }
    if wrong.is_empty() { return reply.to_string(); }
    let mut out = reply.to_string();
    for (w, right) in wrong {
        let at: Vec<(usize, usize)> = numbers_at(&out).into_iter().filter(|x| same(x.2, w)).map(|x| (x.0, x.1)).collect();
        for (a, b) in at.into_iter().rev() { out.replace_range(a..b, &show_num(right)); }
    }
    out
}

/// `reply` with each sum it states ("42.0 + 361 = 796", "3 × 7 = 22", "21 - 5 = 16") checked: one that does not
/// hold gets its result put right.
fn stated_sums_fix(reply: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let n = r"\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?";
        regex::Regex::new(&format!(r"(?:^|[^\w.,/:-])((?:{n})(?:\s*[+\-−×*÷/]\s*(?:{n}))+|(?:{n})(?:\s+x\s+(?:{n}))+)\s*=\s*({n})")).unwrap()
    });
    let mut out = reply.to_string();
    let found: Vec<(String, std::ops::Range<usize>)> = re.captures_iter(reply).filter_map(|c| {
        let e = c[1].replace(',', "").replace('−', "-").replace('×', "*").replace('÷', "/").replace(" x ", " * ");
        let r: f64 = crate::mcp_reason::calc(&e).ok()?.parse().ok()?;
        let got: f64 = c[2].replace(',', "").parse().ok()?;
        (!same(r, got)).then(|| (show_num(r), c.get(2).unwrap().range()))
    }).collect();
    for (r, at) in found.into_iter().rev() { out.replace_range(at, &r); }
    out
}

/// `reply` with each sentence that gives a sum's result (`c`) beside only some of the question's numbers (`asked`)
/// and none of the working's ("12.5% of 3,480 is 796": 796 is 435 + 361) said in full: the question's sum as the
/// user wrote it, then Elisa's working ("12.5% of 3,480 plus 19 squared = 435 + 361 = 796.").
fn whole_said(reply: &str, c: &Computed, asked: &[f64], ask: &str) -> String {
    let mut distinct: Vec<f64> = Vec::new();
    for a in asked { if !distinct.iter().any(|d| same(*d, *a)) { distinct.push(*a); } }
    if distinct.len() < 2 { return reply.to_string(); }
    // the question's sum: from its first number to the end of its sentence
    let Some(first) = numbers_at(ask).first().map(|x| x.0) else { return reply.to_string() };
    let end = ask[first..].find(['?', '!', '\n']).map_or(ask.len(), |i| first + i);
    let phrase = ask[first..end].trim().trim_end_matches(['.', ',']).trim();
    let shown = if c.shown.contains(" = ") { c.shown.clone() } else { format!("{} = {}", c.shown, c.text) };
    let mut out = String::new();
    let mut rest = reply;
    while !rest.is_empty() {
        // a sentence ends at ". " / "! " / "? " / a line end (never inside "12.5")
        let cut = rest.char_indices().find(|(i, ch)| *ch == '\n' || (matches!(ch, '.' | '!' | '?') && rest[i + 1..].chars().next().is_none_or(char::is_whitespace)))
            .map_or(rest.len(), |(i, ch)| i + ch.len_utf8());
        let sent = &rest[..cut];
        rest = &rest[cut..];
        let nums: Vec<f64> = numbers_at(sent).into_iter().map(|x| x.2).collect();
        let says_result = nums.iter().any(|n| same(*n, c.value));
        let named = distinct.iter().filter(|a| !same(**a, c.value) && nums.iter().any(|n| same(*n, **a))).count();
        let working = c.steps.iter().any(|st| !same(*st, c.value) && nums.iter().any(|n| same(*n, *st)));
        if says_result && named > 0 && named < distinct.len() && !working {
            let lead = &sent[..sent.len() - sent.trim_start().len()];
            let tail = if sent.trim_end().ends_with(['.', '!', '?']) { "." } else { "" };
            let nl = if sent.ends_with('\n') { "\n" } else { "" };
            out.push_str(&format!("{lead}{phrase} = {shown}{tail}{nl}"));
        } else {
            out.push_str(sent);
        }
    }
    out
}

/// Elisa's own resume line (`data/resume_lines.txt`), in Vietnamese (`vi`) or English: the user's earlier short
/// messages (`asks`, oldest first: the last three, each one line), the facts they told (`facts`, said to them), and
/// an offer to go on. None when there is neither.
pub fn resume_line(vi: bool, asks: &[String], facts: &[String]) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for a in asks.iter().filter(|a| a.split_whitespace().count() <= 25) {
        let l = crate::context::one_line(a, 60);
        if !l.is_empty() && !seen.contains(&l) { seen.push(l); }
    }
    if seen.len() > 3 { seen.drain(..seen.len() - 3); }
    if seen.is_empty() && facts.is_empty() { return None; }
    let want = if vi { "[vi]" } else { "[en]" };
    let mut cur = "";
    let mut parts: Vec<String> = Vec::new();
    for l in include_str!("../../data/resume_lines.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if l.starts_with('[') { cur = l; continue; }
        if cur != want { continue; }
        let Some((k, v)) = l.split_once(" = ") else { continue };
        match k.trim() {
            "asks" if !seen.is_empty() => parts.push(v.replace("{asks}", &seen.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(", "))),
            "facts" if !facts.is_empty() => parts.push(v.replace("{facts}", &facts.join(" "))),
            "end" => parts.push(v.to_string()),
            _ => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The words that ask for a number (`data/number_asks.txt`: "how many", "bao nhiêu"), the longest first.
fn count_asks() -> &'static [String] {
    static A: OnceLock<Vec<String>> = OnceLock::new();
    A.get_or_init(|| {
        let mut v: Vec<String> = include_str!("../../data/number_asks.txt").lines().map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect();
        v.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
        v
    })
}

/// a text's words, lower-case, letters and digits only
fn plain_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Elisa's plain answer to a number question (`ask`) when `reply` says its question over again: the asking words of
/// its question part ("how many are left") become the number and the thing counted ("16 apples are left."; "còn lại
/// bao nhiêu quả táo" → "Còn lại 16 quả táo."). The thing counted: the words most of the question's numbers have
/// after them ("7 apples", "5 apples"). None when the reply does not repeat the question part.
fn plain_answer(ask: &str, reply: &str, value: f64) -> Option<String> {
    let low = ask.to_lowercase();
    // the question part: from its asking words to the end of its sentence
    let (at, w) = count_asks().iter().filter_map(|w| find_word(&low, w).map(|i| (i, w.clone()))).min_by_key(|(i, _)| *i)?;
    let start = low[..at].rfind([',', '.', ';', '!', '?', '\n']).map_or(0, |i| i + 1);
    let end = low[at..].find(['.', '?', '!', '\n']).map_or(low.len(), |i| at + i);
    let raw = &ask[start..end];
    let lead = raw.len() - raw.trim_start().len();
    let part = raw.trim();
    let at = at - lead;
    let part_words = plain_words(part);
    if part_words.len() < 2 { return None; }
    let rw = plain_words(reply);
    if !rw.windows(part_words.len()).any(|x| x == part_words.as_slice()) { return None; }
    // the thing counted: the longest run of words after the question's numbers that most of them share
    let before = &low[..start];
    let afters: Vec<Vec<String>> = numbers_at(before).into_iter().map(|(_, b, _)| plain_words(&before[b..]).into_iter().take(3).take_while(|x| x.parse::<f64>().is_err()).collect()).collect();
    let mut unit: Vec<String> = Vec::new();
    for n in (1..=3).rev() {
        let mut best: Option<(usize, Vec<String>)> = None;
        for a in afters.iter().filter(|a| a.len() >= n) {
            let k = afters.iter().filter(|b| b.len() >= n && b[..n] == a[..n]).count();
            if k >= 2 && best.as_ref().is_none_or(|(c, _)| k > *c) { best = Some((k, a[..n].to_vec())); }
        }
        if let Some((_, u)) = best { unit = u; break; }
    }
    if unit.is_empty() { unit = afters.last().and_then(|a| a.first().cloned()).into_iter().collect(); }
    let ask_end = at - start + w.len();
    let rest = part[ask_end.min(part.len())..].trim();
    let rest_low = plain_words(rest);
    let says_unit = !unit.is_empty() && rest_low.len() >= unit.len() && rest_low[..unit.len()] == unit[..];
    let unit_text = if says_unit || unit.is_empty() { String::new() } else { format!(" {}", unit.join(" ")) };
    let line = format!("{}{}{unit_text} {rest}", &part[..at - start], show_num(value));
    let line = line.trim().trim_end_matches([',', ' ']);
    let mut ch = line.chars();
    Some(ch.next().map(|f| f.to_uppercase().collect::<String>() + ch.as_str())? + ".")
}

/// The computed results in a text (`COMPUTED` parts: "… exactly[ from x]: A = 5; B = 3 + 4 = 7 · …"), and Elisa's
/// exact counts of a file (`COUNTED`: each "8 words" a result; gemma3:1b saved 8 and then said "1").
fn computed_in(text: &str) -> Vec<Computed> {
    let mut out: Vec<Computed> = Vec::new();
    for (i, _) in text.match_indices(COUNTED) {
        let rest = &text[i + COUNTED.len()..];
        let body = &rest[..[";", "]", "\n"].iter().filter_map(|e| rest.find(e)).min().unwrap_or(rest.len())];
        let Some((name, list)) = body.split_once(" has ") else { continue };
        for part in list.split(", ") {
            let Some((n, unit)) = part.trim().split_once(' ') else { continue };
            let Some(value) = cell_num(n) else { continue };
            out.push(Computed { value, text: n.to_string(), label: None, steps: Vec::new(), shown: format!("{}: {n} {unit}", name.trim()), unit: Some(unit.trim().to_lowercase()), expr: String::new() });
        }
    }
    for (i, _) in text.match_indices(COMPUTED) {
        let rest = &text[i + COMPUTED.len()..];
        let Some(colon) = rest.find(": ") else { continue };
        let body = &rest[colon + 2..];
        let end = [" · ", "]", "\n"].iter().filter_map(|e| body.find(e)).min().unwrap_or(body.len());
        let mut body = &body[..end];
        // the closing parenthesis of the situation line itself
        if body.matches('(').count() < body.matches(')').count() { body = body.strip_suffix(')').unwrap_or(body); }
        for item in body.split("; ") {
            let parts: Vec<&str> = item.split(" = ").map(str::trim).collect();
            if parts.len() < 2 { continue; }
            let last = parts[parts.len() - 1];
            let (label, num) = match last.rsplit_once(" (") {
                Some((l, n)) if n.ends_with(')') => (Some(l.trim().to_string()), cell_num(n.trim_end_matches(')'))),
                _ => (None, cell_num(last)),
            };
            let Some(value) = num else { continue };
            let steps = parts[1..parts.len() - 1].iter().flat_map(|p| numbers_at(p).into_iter().map(|(_, _, v)| v)).collect();
            // the working as added to a reply: numbers and operators only, or the row with its number ("South (340)"),
            // never Elisa's English names of the facts ("highest units")
            let worded = parts[0].chars().any(char::is_alphabetic) && !parts[0].contains(['+', '*', '/', '^', '(']);
            let shown = if label.is_some() || (worded && parts.len() < 3) { last.to_string() }
                else if parts.len() >= 3 { parts[1..].join(" = ") } else { item.trim().to_string() };
            out.push(Computed { value, text: label.as_ref().map_or_else(|| last.to_string(), |_| show_num(value)), label, steps, shown, unit: None, expr: parts[0].to_string() });
        }
    }
    out
}

/// `reply` without the words Elisa's own fact lines ask of the model, or start with, copied into it ("555 · answer
/// with these results", "Elisa computed it exactly from sales.csv: …"); the facts themselves stay.
pub fn without_note_words(reply: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let tails = [USE_ONE, USE_ALL, CLOCK_USE, "use this number"].iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|");
        regex::Regex::new(&format!(r"(?i)\s*·?\s*(?:{tails})\b\.?|\[?{}(?:\s+from\s+[^:\n]+)?:\s*|{}", regex::escape(COMPUTED), regex::escape(CLOCK_HEAD.trim_end()))).unwrap()
    });
    if !re.is_match(reply) { return reply.to_string(); }
    re.replace_all(reply, "").trim().trim_end_matches(']').trim().to_string()
}

/// The answer of a "which <row>" question said in the user's own words, when a reply names no row: the question's
/// question word and the labels column's name ("which region") become the row, its number follows: "which region sold
/// the most units in sales.csv?" → "South sold the most units in sales.csv (340)." None when the question does not
/// name the labels column after a question word.
fn stated(ask: &str, header: Option<&str>, label: &str, value: &str) -> Option<String> {
    let h = header?.to_lowercase();
    let q = ask.trim().trim_end_matches(['?', '.', '!', ' ']);
    let words: Vec<&str> = q.split_whitespace().collect();
    let low: Vec<String> = words.iter().map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string()).collect();
    let k = low.iter().position(|w| *w == h || w.strip_suffix('s') == Some(h.as_str()) || h.strip_suffix('s') == Some(w.as_str()))?;
    // the question word right before it, the question's first or second word ("which region …", "so which region …")
    if k == 0 || k > 2 { return None; }
    let rest = words[k + 1..].join(" ");
    if rest.is_empty() { return None; }
    Some(format!("{label} {rest} ({value})."))
}

/// What a reply is checked against (`reply_check::Echo`): Elisa's computed results and the clock words of this
/// turn, the cells of the table in it, the user's own numbers, and whose names the facts give.
#[derive(Debug, Clone, Default)]
pub struct ReplyFacts {
    clock: Option<ClockFix>,
    /// the turn's clock lines whole (`zone_lines`): a zone a reply states beside here's time is here's (`zone_label_fix`)
    zones: Vec<ZoneLine>,
    times: Option<TimeFix>,
    computed: Vec<Computed>,
    cells: Vec<(String, f64)>,
    asked: Vec<f64>,
    names: crate::context::NameFix,
    /// the user's words of the turn, and the table's labels column: a reply naming no row gets the answer said in
    /// those words (`stated`)
    ask: String,
    label_header: Option<String>,
    /// the turn says Elisa could not work out its number question (`mcp_reason::UNSURE`): the reply asks, states no number
    unsure: bool,
    /// the turn says the day asked about moves from year to year (`DATE_UNSURE`): the reply asks which date it is
    unsure_date: bool,
    /// the day a weekday question asks about (days since 1970): another date the reply gives is that one
    date_asked: Option<i64>,
    /// a weekday question's day as the user named it: a reply naming no real day gets Elisa's own answer
    weekday_name: Option<String>,
    /// the dates the turn's results give, for a reply's day word about one of their events (`relative_by_results`)
    result_dates: Vec<(i64, String, Vec<String>)>,
    /// a clock time plus or minus a duration in the user's words (`clock_sum`): the reply's time is checked against it
    clock_sum: Option<ClockSum>,
    /// the turn's own clock line as a unix time (`turn_now`): today, for the day words, is that line's day
    turn_now: Option<i64>,
}

impl ReplyFacts {
    /// From one turn's message as sent (`turn`: the user's words and Elisa's lines): its computed parts, and the
    /// clock words when it asks the time.
    pub fn from_turn(turn: &str) -> ReplyFacts {
        let body = crate::context::request_body(turn);
        let own = crate::context::own_words(body).unwrap_or(body);
        // this turn's own facts only: the notes in front of the request ("## Request") are earlier turns', and an
        // earlier computed line copied into them is no answer to this one
        let this_turn = turn.rsplit_once("## Request\n").map_or(turn, |(_, r)| r);
        // the turn's own clock line is its "now" (the day, hour and date the reply is held to), the wall clock without one
        let turn_now = turn_now(turn);
        let now = turn_now.unwrap_or_else(super::now_secs);
        ReplyFacts {
            clock: {
                let times = TimeFix::new(turn, own, now);
                let places = times.as_ref().map(|t| t.places.iter().map(|(p, m)| (p.to_lowercase(), *m)).collect()).unwrap_or_default();
                let (place_days, here_minute) = times.as_ref().map(TimeFix::place_days).unwrap_or_default();
                match date_asked(own, now) {
                    // the day of the week of another day: the reply's day name is that day's, not today's
                    Some(DateAsk::Weekday(days)) | Some(DateAsk::FromNow(_, days)) => ClockFix::new_for(own, now, true).map(|c| ClockFix { day: weekday_of(days), ..c }),
                    _ => ClockFix::new_for(own, now, has_place_lines(turn)).map(|c| ClockFix { places, place_days, here_minute, ..c }),
                }
            },
            times: TimeFix::new(turn, own, now),
            computed: computed_in(this_turn),
            asked: numbers_at(own).into_iter().map(|(_, _, v)| v).collect(),
            // a choice's two sides are the rows a reply may name
            cells: if compare_way(own).is_some() { choices(own) } else { Vec::new() },
            ask: own.trim().to_string(),
            unsure: this_turn.contains(crate::mcp_reason::UNSURE),
            unsure_date: this_turn.contains(DATE_UNSURE),
            date_asked: match date_asked(own, now) { Some(DateAsk::Weekday(t)) | Some(DateAsk::FromNow(_, t)) => Some(t), _ => None },
            weekday_name: match date_asked(own, now) { Some(DateAsk::Weekday(_)) => day_named(own).and_then(|d| match d { DayNamed::Fixed(n, ..) => Some(n), _ => None }), _ => None },
            clock_sum: clock_sum(own),
            turn_now,
            zones: zone_lines(turn),
            ..Default::default()
        }
    }

    /// The session's part: computed notes in this turn's tool results (`results`), the table in the turn, and the
    /// facts saying whose names are whose.
    pub fn session(results: &str, table: Option<&Table>, names: crate::context::NameFix) -> ReplyFacts {
        let today = local_tm(super::now_secs()).map(|tm| days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32));
        ReplyFacts { computed: computed_in(results), result_dates: today.map(|t| result_dates(results, t)).unwrap_or_default(), cells: table.map(Table::cells).unwrap_or_default(), names,
            label_header: table.and_then(|t| t.label_col().map(|c| t.header[c].clone())), ..Default::default() }
    }

    /// both sets of facts
    pub fn merge(mut self, o: ReplyFacts) -> ReplyFacts {
        self.clock = self.clock.or(o.clock);
        self.times = self.times.or(o.times);
        self.computed.extend(o.computed);
        self.cells.extend(o.cells);
        self.asked.extend(o.asked);
        if self.names.is_empty() { self.names = o.names; }
        if self.ask.is_empty() { self.ask = o.ask; }
        self.unsure |= o.unsure;
        self.unsure_date |= o.unsure_date;
        self.date_asked = self.date_asked.or(o.date_asked);
        self.weekday_name = self.weekday_name.or(o.weekday_name);
        self.result_dates.extend(o.result_dates);
        self.label_header = self.label_header.or(o.label_header);
        self.clock_sum = self.clock_sum.or(o.clock_sum);
        self
    }

    /// Elisa's answer from the user's facts to this turn's question about them (`NameFix::answer_from_facts`), or
    /// her own date answer to a question about today (`date_answer`), or her own clock answer on a time turn
    /// (`clock_answer`): for a turn the model gave no usable reply to.
    pub fn facts_answer(&self) -> Option<String> {
        self.names.answer_from_facts(&self.ask).or_else(|| self.date_answer()).or_else(|| self.clock_answer())
    }

    /// `reply` with a zone it states in brackets beside here's time ("23:31 khuya Thứ Năm (UTC+09:00, Asia/Tokyo)" when
    /// the local line says 23:31 at UTC+07:00) made here's own, from the turn's local clock line ("(UTC+07:00, Ho Chi
    /// Minh)"): a stated zone matches the clock line its time is from. A sentence holding a place's time, or naming a
    /// place of the turn's clock lines, keeps its zone (`TimeFix::zone_words_fix` puts that one right).
    fn zone_label_fix(&self, reply: &str) -> String {
        static ZONE: OnceLock<regex::Regex> = OnceLock::new();
        let re = ZONE.get_or_init(|| regex::Regex::new(r"\(\s*(?:UTC|GMT)\s*[+\-−]\s*\d{1,2}(?::?\d{2})?(?:\s*,\s*([^()\n]*?))?\s*\)").unwrap());
        let Some(here) = self.zones.iter().find(|l| l.place.is_none() && !l.offset.is_empty()) else { return reply.to_string() };
        if !re.is_match(reply) { return reply.to_string(); }
        let places: Vec<&ZoneLine> = self.zones.iter().filter(|l| l.place.is_some()).collect();
        let own = if here.label.is_empty() { format!("({})", here.offset) } else { format!("({}, {})", here.offset, here.label) };
        let squash = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        let mut out = String::new();
        for clause in reply.split_inclusive(['.', '!', '?', ';', '\n']) {
            let low = clause.to_lowercase();
            let mins = times_in_text(clause);
            let here_time = mins.iter().any(|m| near_minute(*m, here.minute));
            let place_said = places.iter().any(|p| mins.iter().any(|m| near_minute(*m, p.minute))
                || p.place.as_deref().is_some_and(|n| low.contains(&n.to_lowercase())));
            if !here_time || place_said { out.push_str(clause); continue; }
            // (here's offset alone, or with here's name, stands: "(UTC+07:00)", "(UTC+07:00, Ho Chi Minh)")
            out.push_str(&re.replace_all(clause, |c: &regex::Captures| {
                let off_only = c.get(1).is_none_or(|n| n.as_str().trim().is_empty());
                let same_off = squash(&c[0]).starts_with(&squash(&format!("({}", here.offset)));
                let same_name = c.get(1).is_some_and(|n| here.label.is_empty() || squash(n.as_str()) == squash(&here.label));
                if same_off && (off_only || same_name) { c[0].to_string() } else { own.clone() }
            }));
        }
        out
    }

    /// Whether `reply` gives what a question about today asks (`asks_date`): a weekday ask (`day asks`) a weekday's
    /// name, a date ask the day's number with its month (named or numbered). True when the turn holds no clock line.
    fn says_date(&self, reply: &str) -> bool {
        let low = self.ask.to_lowercase();
        let r = super::folded(reply);
        if clock_langs().iter().any(|l| l.day_asks.iter().any(|a| has_phrase(&low, a))) {
            return clock_langs().iter().flat_map(|l| &l.days).any(|(with, _)| r.contains(&super::folded(with)));
        }
        let Some(tm) = self.turn_now.and_then(local_tm) else { return true };
        let nums: Vec<u32> = r.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
        let month_named = date_words().langs.iter().filter_map(|l| l.months.get(tm.tm_mon as usize)).flatten().any(|n| r.contains(&super::folded(n)));
        // (a year it gives is this one: "12/11/2021" is not today)
        let year = (tm.tm_year + 1900) as u32;
        let wrong_year = nums.iter().any(|n| (1000..10000).contains(n) && *n != year);
        !wrong_year && nums.contains(&(tm.tm_mday as u32)) && (nums.contains(&(tm.tm_mon as u32 + 1)) || month_named)
    }

    /// Elisa's own answer to a question about today's date or weekday (`asks_date`), from the turn's clock line, in the
    /// user's language (`date answer` of `data/clock_words.txt`, the month's name from `data/date_words.txt`): "Today
    /// is Thursday, 12 November 2026." / "Hôm nay là Thứ Năm, ngày 12/11/2026.". None without a clock line or such an ask.
    pub fn date_answer(&self) -> Option<String> {
        if !asks_date(&self.ask) { return None; }
        let low = self.ask.to_lowercase();
        let lang = clock_langs().iter().find(|l| l.date_asks.iter().chain(&l.day_asks).any(|a| has_phrase(&low, a)))
            .or_else(|| lang_of(&self.ask))?;
        if lang.date_answer.is_empty() { return None; }
        let tm = local_tm(self.turn_now?)?;
        let day = lang.days.get(tm.tm_wday as usize).map(|(with, _)| with.clone())?;
        let (d, m, y) = (tm.tm_mday, tm.tm_mon + 1, tm.tm_year + 1900);
        let month = date_words().langs.iter().find(|l| l.vi == (lang.code == "vi"))
            .and_then(|l| l.months.get((m - 1) as usize)).and_then(|names| names.first())
            .map(|n| { let mut c = n.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() })
            .unwrap_or_else(|| m.to_string());
        Some(lang.date_answer.replace("{day}", &day).replace("{month}", &month).replace("{d}", &d.to_string()).replace("{m}", &m.to_string()).replace("{y}", &y.to_string()))
    }

    /// Elisa's own whole answer to this turn's time ask from her clock lines (`TimeFix::own_answer`: "It's 01:10,
    /// Thursday late night."), None on any other turn.
    pub fn clock_answer(&self) -> Option<String> { self.times.as_ref().and_then(TimeFix::own_answer) }

    /// Elisa's answer from the user's facts when `reply` answers this turn's question about them with none of them:
    /// the assistant's own name (`own`), a bare word, or another value (`NameFix::recall_answer`).
    pub fn recall_answer(&self, reply: &str, own: &[String]) -> Option<String> { self.names.recall_answer(&self.ask, reply, own) }

    /// Elisa's own resume line for this turn's "continue our last conversation" (`resume_line`), with the user's
    /// facts from the notes.
    pub fn resume_answer(&self, asks: &[String]) -> Option<String> {
        let vi = matches!(crate::reply_lang::detect(&self.ask), Some(crate::reply_lang::Lang::Vi));
        resume_line(vi, asks, &self.names.all_facts_said(vi))
    }

    pub fn is_empty(&self) -> bool { self.clock.is_none() && self.times.is_none() && self.computed.is_empty() && self.names.is_empty() && self.result_dates.is_empty() && self.clock_sum.is_none() }

    /// Whether `reply`, digits alone, answers this turn by a fact Elisa holds, so it is a whole answer and no reply
    /// without words: a clock time ("22:20", "2026-11-12 00:20") on a time turn (the clock asked, or Elisa's clock
    /// lines for a place: "and in Tokyo?"), or one number, date or time on a turn with a computed result, a date
    /// question or a clock sum. (moondream:1.8b answered "and in Tokyo?" with "22:20", then "00:20", and both were
    /// thrown out as replies with no words; `fix` makes such a time Elisa's own.)
    pub fn answers_in_digits(&self, reply: &str) -> bool {
        let p: Vec<&str> = reply.trim().trim_end_matches('.').split_whitespace().collect();
        let value = !p.is_empty() && p.len() <= 3 && p.iter().all(|x| x.chars().any(|c| c.is_ascii_digit()) && x.chars().all(|c| c.is_ascii_digit() || "-/:.,".contains(c)));
        if !value { return false; }
        let clock = p.iter().any(|x| { let mut it = x.split(':'); matches!((it.next().and_then(|h| h.parse::<u32>().ok()), it.next().and_then(|m| m.parse::<u32>().ok())), (Some(h), Some(m)) if h < 24 && m < 60) });
        ((self.times.is_some() || self.clock.is_some()) && clock) || !self.computed.is_empty() || self.clock_sum.is_some() || self.date_asked.is_some()
    }

    /// `reply` made true to the facts: the time words, the computed numbers and rows, and the user called by no
    /// other's name. Unchanged when nothing is off.
    pub fn fix(&self, reply: &str) -> String {
        if reply.trim().is_empty() { return reply.to_string(); }
        let reply = without_note_words(reply);
        // a day word next to a date it does not fit ("tomorrow, November 6" on 11 Nov), today being the turn's own
        let reply = match local_tm(self.turn_now.unwrap_or_else(super::now_secs)) {
            Some(tm) => {
                let today = days_from_civil(tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32);
                tense_fix(&relative_by_results(&relative_fix(&reply, today), today, &self.result_dates), today)
            }
            None => reply,
        };
        let reply = reply.as_str();
        // unsure of the question, and the reply states a number without asking: Elisa asks herself
        if self.unsure && !reply.contains(['?', '？']) && (!numbers_at(reply).is_empty() || !word_numbers(reply).is_empty()) {
            if let Some(q) = confirm_line(&self.ask) { return q; }
        }
        // (a day whose date moves: any reply that does not ask is no answer, a number or not)
        if self.unsure_date && !reply.contains(['?', '？']) {
            if let Some(q) = confirm_line_for(&self.ask, "date") { return q; }
        }
        // a clock line in a reply to an ask that is not the time (a copy of Elisa's header: "Bây giờ là 09:46 sáng Thứ
        // Tư, ngày …"): gone, before the asked day's date is put in it
        let no_clock;
        let reply = if self.times.is_none() && !asks_time(&self.ask) { no_clock = without_clock_lines(reply); no_clock.as_str() } else { reply };
        // a 24-hour time with AM / PM ("13:18 PM"): the 12-hour form
        let ampm = twelve_hour_fix(reply);
        let reply = ampm.as_str();
        // the times first: the part of the day next to a time follows its hour
        let mut s = match &self.times { Some(t) => t.fix(reply), None => reply.to_string() };
        if let Some(c) = &self.clock { s = c.fix(&s); }
        s = self.zone_label_fix(&s);
        // a question about today's date or weekday answered with neither (gemma3:1b: "Ngày 12 tháng 11, 2026." to "hôm
        // nay là thứ mấy?"): Elisa's own date line, from the clock line
        if let Some(a) = self.date_answer() { if !self.says_date(&s) { return a; } }
        if let Some(t) = self.date_asked { s = date_fix(&s, t); }
        if let (Some(t), Some(n)) = (self.date_asked, &self.weekday_name) { s = weekday_fill(&s, t, n, &self.ask); }
        s = self.names.fix(&s);
        // asked what the user told: "I said your name is Linh" is the user's telling
        s = self.names.told_answer(&self.ask, &s);
        // the user's own facts it asks for, from Elisa's notes (a placeholder "[Your Name]" or one left out)
        s = self.names.answer(&self.ask, &s);
        // a question about the user answered about the assistant ("về mình" read as Elisa): the user's facts
        s = self.names.about_user(&self.ask, &s);
        // a clock time plus or minus a duration, worked out by Elisa: the reply's time is hers (moondream:1.8b made
        // 3:20 pm plus 2 h 55 min "7:55 pm"); the way not known, the reply asks
        if let Some(t) = self.clock_sum.as_ref().and_then(|c| c.fix(&s, &self.ask)) { s = t; }
        // Elisa's own sum called wrong, spoken of by name, or given a second result: her working alone
        if let Some(a) = self.disputed(&s) { return a; }
        // one number worked out from the user's words, and a reply that says the question over again ("…, how many
        // are left? 3 boxes with 16 apples left."): Elisa's plain answer in the question's words
        if let [c] = self.computed.as_slice() {
            if let Some(l) = c.label.is_none().then(|| plain_answer(&self.ask, &s, c.value)).flatten() { return l; }
        }
        // a part of a sum given a wrong value ("12.5% of 3,480 is 42.0" for 435): Elisa's, wherever that number is
        if let [c] = self.computed.as_slice() {
            if c.label.is_none() && c.steps.len() >= 2 { s = parts_fix(&s, c, &self.asked); }
        }
        let mut add: Vec<String> = Vec::new();
        for c in &self.computed {
            let (t, more) = self.fix_one(&s, c);
            s = t;
            // a count put in place of "one word": its unit as Elisa counted it ("8 words")
            if let (Some(u), true) = (&c.unit, c.value != 1.0) {
                let re = regex::Regex::new(&format!(r"\b{} {}\b", regex::escape(&c.text), regex::escape(u.trim_end_matches('s')))).unwrap();
                s = re.replace_all(&s, format!("{} {u}", c.text).as_str()).into_owned();
            }
            add.extend(more.filter(|m| !add.contains(m)));
        }
        // arithmetic the reply states that does not hold ("21 - 5 = 15"): its result put right (after the computed
        // results, whose own working wins when the reply's is wrong all through)
        s = stated_sums_fix(&s);
        if !add.is_empty() { s = format!("{}\n\n{}", s.trim_end(), add.join("\n")); }
        // a sum's result said of a part of it ("12.5% of 3,480 is 796" for "12.5% of 3,480 plus 19 squared"): the
        // whole question and Elisa's working in that sentence
        if let [c] = self.computed.as_slice() {
            if c.label.is_none() && !c.steps.is_empty() && sums_part(&self.ask).is_some() { s = whole_said(&s, c, &self.asked, &self.ask); }
        }
        s
    }

    /// Elisa's answer when a reply disputes the sums she computed herself: it calls one wrong or gives another as
    /// the right one (`data/dispute_words.txt`), or speaks of Elisa by name, next to a result; or it gives the result
    /// and a second, near one that no part of the sum or question holds ("… as 7006652. However, the correct result
    /// … is 7,008,652."). The answer is the working, one line a sum: "1234 × 5678 = 7,006,652". None for rows,
    /// counts, or a reply that keeps to the result.
    fn disputed(&self, reply: &str) -> Option<String> {
        static W: OnceLock<Vec<String>> = OnceLock::new();
        let words = W.get_or_init(|| include_str!("../../data/dispute_words.txt").lines().map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_lowercase).collect());
        let sums = &self.computed;
        let is_sum = |c: &Computed| c.label.is_none() && c.unit.is_none() && c.expr.contains(['+', '-', '*', '/', '^', '×', '÷']);
        if sums.is_empty() || !sums.iter().all(is_sum) { return None; }
        let low = reply.to_lowercase();
        let nums = numbers_at(reply);
        let says = nums.iter().any(|(_, _, n)| sums.iter().any(|c| same(*n, c.value)));
        let known: Vec<f64> = sums.iter().flat_map(|c| numbers_at(&c.expr).into_iter().map(|(_, _, n)| n).chain(c.steps.iter().copied()).chain([c.value]))
            .chain(numbers_at(&self.ask).into_iter().map(|(_, _, n)| n)).collect();
        let other = nums.iter().any(|(_, _, n)| !known.iter().any(|k| same(*k, *n))
            && sums.iter().any(|c| c.value != 0.0 && ((n - c.value) / c.value).abs() < 0.05));
        let name = COMPUTED.split_whitespace().next().unwrap_or_default().to_lowercase();
        let named = find_word(&low, &name).is_some();
        let disputes = words.iter().any(|w| has_phrase(&low, w));
        if !(((disputes || named) && (says || other)) || (says && other)) { return None; }
        Some(sums.iter().map(|c| format!("{} = {}", sum_shown(&c.expr), grouped_num(c.value))).collect::<Vec<_>>().join("\n"))
    }

    /// one result: a wrong row named instead of the computed one made right (and that row's number); a wrong number
    /// made right (one close to a step or the result becomes that, else the last unknown number becomes the
    /// result); the working to add when the reply still does not say the result
    fn fix_one(&self, reply: &str, c: &Computed) -> (String, Option<String>) {
        if let Some(l) = &c.label {
            let low = reply.to_lowercase();
            if l.split(" and ").all(|x| has_phrase(&low, &x.to_lowercase())) { return (reply.to_string(), None); }
            // a choice's side is its number ("5 GB"): the number alone names it ("5"), unless another side is named
            let own_number = l.split_whitespace().next().and_then(cell_num).is_some_and(|n| same(n, c.value));
            let others = self.cells.iter().any(|(x, _)| !x.eq_ignore_ascii_case(l) && find_word(&low, &x.to_lowercase()).is_some());
            if own_number && !others && numbers_at(reply).iter().any(|(_, _, n)| same(*n, c.value)) { return (reply.to_string(), None); }
            // another row's label in its place
            // (offsets in the lower-case text match the reply's only when lower-casing kept every length)
            let other = self.cells.iter().filter(|(x, _)| !x.is_empty() && !l.split(" and ").any(|y| y.eq_ignore_ascii_case(x)))
                .filter_map(|(x, v)| find_word(&low, &x.to_lowercase()).map(|i| (i, x.clone(), *v)))
                .filter(|_| low.len() == reply.len()).min_by_key(|(i, _, _)| *i);
            // no row named at all ("Asia/Ho_Chi_Minh", "not specified"): the answer in the user's own words
            let Some((i, x, v)) = other else { return (stated(&self.ask, self.label_header.as_deref(), l, &c.text).unwrap_or_else(|| c.shown.clone()), None) };
            let mut s = format!("{}{}{}", &reply[..i], l, &reply[i + x.len()..]);
            if let Some((a, b, _)) = numbers_at(&s).into_iter().find(|(_, _, n)| same(*n, v)) { s = format!("{}{}{}", &s[..a], c.text, &s[b..]); }
            return (s, None);
        }
        let mut nums = numbers_at(reply);
        // a number written as a word says it too ("eight words"); a count's wrong one right before its unit ("one
        // word") is one to put right (an everyday word, "không", never is)
        let words = word_numbers(reply);
        if nums.iter().any(|(_, _, n)| same(*n, c.value)) || words.iter().any(|(_, _, n, _)| same(*n, c.value)) { return (reply.to_string(), None); }
        if let Some(u) = &c.unit {
            let one = u.trim_end_matches('s');
            nums.extend(words.iter().filter(|(_, b, _, amb)| !amb && {
                let next: String = reply[*b..].trim_start().chars().take_while(|ch| ch.is_alphabetic()).collect::<String>().to_lowercase();
                next == *u || next == one
            }).map(|(a, b, n, _)| (*a, *b, *n)));
            nums.sort_by_key(|(a, _, _)| *a);
        }
        let known = |n: f64| self.asked.iter().chain(self.cells.iter().map(|(_, v)| v)).chain(self.computed.iter().flat_map(|k| k.steps.iter().chain(std::iter::once(&k.value)))).any(|k| same(*k, n));
        let unknown: Vec<(usize, usize, f64)> = nums.into_iter().filter(|(_, _, n)| !known(*n)).collect();
        let mut s = reply.to_string();
        // close to a step or the result (within a tenth): that one, from the end so the ranges stay right
        let near = |n: f64| c.steps.iter().chain(std::iter::once(&c.value)).copied()
            .filter(|w| *w != 0.0 && ((n - w) / w).abs() <= 0.1).min_by(|a, b| (n - a).abs().total_cmp(&(n - b).abs()));
        let mut left: Vec<(usize, usize, f64)> = Vec::new();
        for (a, b, n) in unknown.iter().rev() {
            match near(*n) {
                Some(w) => s = format!("{}{}{}", &s[..*a], if same(w, c.value) { c.text.clone() } else { show_num(w) }, &s[*b..]),
                None => left.push((*a, *b, *n)),
            }
        }
        if numbers_at(&s).iter().any(|(_, _, n)| same(*n, c.value)) { return (s, None); }
        // several numbers nothing explains ("0.4875 + 0.69 = 0.725" for 435 + 361 = 796): the reply's working is
        // wrong throughout, so Elisa's own working is the answer
        if left.len() >= 2 && !c.shown.is_empty() { return (c.shown.clone(), None); }
        // the last number nothing explains: the result
        if let Some((a, b, _)) = left.first().copied() {
            return (format!("{}{}{}", &s[..a], c.text, &s[b..]), None);
        }
        // a reply that is only one number (one of the question's own: "3" for "… break 3, how many are left?"): the result
        let only = numbers_at(&s);
        if only.len() == 1 && s.trim().trim_end_matches(['.', '!']).trim() == &s[only[0].0..only[0].1] {
            return (format!("{}{}{}", &s[..only[0].0], c.text, &s[only[0].1..]), None);
        }
        (s, Some(c.shown.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_word_the_clock_does_not_fit_goes() {
        // Thursday 12 Nov 2026, noon UTC (Thursday in every zone from UTC-11 to UTC+11)
        let thu = 1_794_484_800;
        let r = "Hi! That's a great way to start the week. What sounds good to eat?";
        assert_eq!(without_wrong_week_words(r, "I went for a run this morning and now I'm starving", thu).as_deref(), Some("Hi! What sounds good to eat?"));
        assert_eq!(without_wrong_week_words("Chúc cuối tuần vui vẻ! Bạn muốn ăn gì?", "mình vừa chạy bộ xong, đói quá", thu - 3 * 86_400).as_deref(), Some("Bạn muốn ăn gì?"));
        // a day it fits, the user's own talk of the week, or nothing left: stands
        assert_eq!(without_wrong_week_words("Happy Friday! Enjoy your lunch.", "lunch time", thu + 86_400), None);
        assert_eq!(without_wrong_week_words("A great way to start the week.", "what a monday, right?", thu), None);
        assert_eq!(without_wrong_week_words("A great way to start the week.", "I went for a run", thu), None);
    }

    #[test]
    fn a_reply_saying_the_question_over_gets_the_plain_answer() {
        let ask = "I have 3 boxes with 7 apples each and give away 5 apples, how many are left?";
        assert_eq!(plain_answer(ask, "3 boxes with 7 apples and give away 5 apples, how many are left? 3 boxes with 16 apples left.", 16.0).as_deref(), Some("16 apples are left."));
        assert_eq!(plain_answer(ask, "You have 21 apples, then 16 are left.", 16.0), None);
        assert_eq!(plain_answer("Có 3 hộp, mỗi hộp 7 quả táo, cho đi 5 quả táo, còn lại bao nhiêu quả táo?", "Còn lại bao nhiêu quả táo? 3 hộp 16 quả táo.", 16.0).as_deref(), Some("Còn lại 16 quả táo."));
        let f = ReplyFacts::from_turn(&format!("(Situation: {COMPUTED}: 3 * 7 - 5 = 21 - 5 = 16 · {USE_ONE})\n\n{ask}"));
        assert_eq!(f.fix("3 boxes with 7 apples and give away 5 apples, how many are left? 3 boxes with 16 apples left."), "16 apples are left.");
    }

    #[test]
    fn a_reply_disputing_elisas_own_sum_gets_her_working() {
        let f = ReplyFacts::from_turn(&format!("(Situation: {COMPUTED}: 1234 * 5678 = 7006652 · {USE_ONE})\n\nwhat is 1234 * 5678?"));
        let said = "Elisa computed the result of 1234 * 5678 as 7006652. However, the correct result of this multiplication is 7,008,652. Elisa seems to have made an error.";
        assert_eq!(f.fix(said), "1234 × 5678 = 7,006,652");
        // a second result alone, with no dispute word; and Elisa spoken of by name
        assert_eq!(f.fix("It is 7006652, or more exactly 7008652."), "1234 × 5678 = 7,006,652");
        assert_eq!(f.fix("Elisa got 7,006,652."), "1234 × 5678 = 7,006,652");
        assert_eq!(f.fix("Kết quả đúng là 7.008.652, Elisa đã tính sai 7006652."), "1234 × 5678 = 7,006,652");
        // a reply that keeps to the result stays
        assert_eq!(f.fix("1234 × 5678 = 7,006,652."), "1234 × 5678 = 7,006,652.");
        assert_eq!(grouped_num(-1234.5), "-1,234.5");
        assert_eq!(grouped_num(999.0), "999");
    }

    #[test]
    fn a_resume_with_no_reply_gets_elisas_own_line() {
        let asks = ["My name is Linh and I live in Hanoi.".to_string(), "what is 15% of 240?".into(), "tell me about Iceland".into(), "tell me about Iceland".into()];
        let facts = ["Your name is Linh.".to_string(), "You live in Hanoi.".into()];
        assert_eq!(resume_line(false, &asks, &facts).as_deref(),
            Some("Last time you asked: \"My name is Linh and I live in Hanoi.\", \"what is 15% of 240?\", \"tell me about Iceland\". Your name is Linh. You live in Hanoi. Want to go on?"));
        assert_eq!(resume_line(true, &[], &["Tên của bạn là Linh.".to_string()]).as_deref(), Some("Tên của bạn là Linh. Bạn muốn tiếp tục không?"));
        assert_eq!(resume_line(false, &[], &[]), None);
    }

    #[test]
    fn an_earlier_computed_line_in_the_notes_is_not_this_turns() {
        let turn = format!("Date: 2026-11-11\n\n## Notes from this conversation\n## Facts to remember\n- {COMPUTED}: (15/100*240) = 36\n\n## Request\nwhat is my name and my meeting code?");
        let f = ReplyFacts::from_turn(&turn);
        assert_eq!(f.fix("Your name is Linh. Your meeting code is ORCHID-42."), "Your name is Linh. Your meeting code is ORCHID-42.");
    }

    /// 2026-11-10 22:10 local (a Tuesday evening), in any zone the tests run in
    fn tuesday_evening() -> i64 {
        let mut t: i64 = 1_794_268_800; // 2026-11-10 00:00 UTC
        loop {
            let tm = local_tm(t).unwrap();
            if tm.tm_wday == 2 && tm.tm_hour == 22 && tm.tm_min == 10 { return t; }
            t += 60;
        }
    }

    #[test]
    fn a_part_of_the_day_next_to_a_time_is_that_times_own() {
        // Tokyo at 05:06 while it is 03:06 here: morning there, not the local late night
        let now = { let mut t = super::super::now_secs() - super::super::now_secs() % 3_600 + 360; while local_tm(t).unwrap().tm_hour != 3 { t += 3_600; } t };
        let turn = "Local time now: 2026-11-11 03:06 Wednesday late night (UTC+07:00)\n\
            Time in Tokyo now: 2026-11-11 05:06 Wednesday morning (UTC+09:00, Asia/Tokyo)\n\nwhat time is it in Tokyo?";
        let mut f = ReplyFacts::from_turn(turn);
        f.clock = ClockFix::new("what time is it in Tokyo?", now);
        assert_eq!(f.fix("It's 05:06 late night in Tokyo."), "It's 05:06 morning in Tokyo.");
        assert_eq!(f.fix("In Tokyo it's 05:06 late night; at home it's 03:06 late night."), "In Tokyo it's 05:06 morning; at home it's 03:06 late night.");
        let vi = "Local time now: 2026-11-11 03:06 Wednesday late night (UTC+07:00)\n\
            Time in Tokyo now: 2026-11-11 05:06 Wednesday morning (UTC+09:00, Asia/Tokyo)\n\nở Tokyo bây giờ là mấy giờ?";
        let mut f = ReplyFacts::from_turn(vi);
        f.clock = ClockFix::new("ở Tokyo bây giờ là mấy giờ?", now);
        assert_eq!(f.fix("Ở Tokyo bây giờ là 05:06 khuya."), "Ở Tokyo bây giờ là 05:06 sáng.");
        assert_eq!(f.fix("Ở Tokyo bây giờ là 03:06 khuya."), "Ở Tokyo bây giờ là 05:06 sáng.", "the time and its part both put right");
    }

    #[test]
    fn a_follow_up_about_another_place_gets_its_part_of_the_day() {
        // "and in Tokyo?" at 04:10 here, 06:10 there: no time words of its own, the turn has Tokyo's clock line
        let now = { let mut t = super::super::now_secs() - super::super::now_secs() % 3_600 + 600; while local_tm(t).unwrap().tm_hour != 4 { t += 3_600; } t };
        let lines = "Local time now: 2026-11-11 04:10 Wednesday late night (UTC+07:00)\n\
            Time in Tokyo now: 2026-11-11 06:10 Wednesday morning (UTC+09:00, Asia/Tokyo)\n\n";
        let fix = |own: &str, reply: &str| {
            let turn = format!("{lines}{own}");
            let mut f = ReplyFacts::from_turn(&turn);
            // (the test's clock: today's name and part at 04:10 here)
            let places = f.clock.as_ref().map(|c| c.places.clone()).unwrap_or_default();
            f.clock = ClockFix::new_for(own, now, true).map(|c| ClockFix { places, ..c });
            f.fix(reply)
        };
        let today = { let tm = local_tm(now).unwrap(); ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][tm.tm_wday as usize] };
        assert_eq!(fix("and in Tokyo?", &format!("In Tokyo, it's 06:10 {today} late night.")), format!("In Tokyo, it's 06:10 {today} morning."));
        // no time: Elisa's own answer for the place (on the turn's own clock, as the app runs it: the lines' day)
        let real = |own: &str, reply: &str| ReplyFacts::from_turn(&format!("{lines}{own}")).fix(reply);
        assert_eq!(real("and in Tokyo?", "In Tokyo, it's Wednesday late night."), "In Tokyo it's 06:10, Wednesday morning (UTC+09:00, Asia/Tokyo).");
        assert_eq!(fix("and in Tokyo?", "In Tokyo, it's 04:10 late night."), "In Tokyo, it's 06:10 morning.", "the time too");
        let vi_today = { let tm = local_tm(now).unwrap(); ["Chủ Nhật", "Thứ Hai", "Thứ Ba", "Thứ Tư", "Thứ Năm", "Thứ Sáu", "Thứ Bảy"][tm.tm_wday as usize] };
        assert_eq!(real("còn ở Tokyo thì sao?", "Còn ở Tokyo thì… đang nghĩ về việc gì?"), "Ở Tokyo bây giờ là 06:10 sáng Thứ Tư (UTC+09:00, Asia/Tokyo).", "no time: her answer in its language");
        assert_eq!(fix("còn ở Tokyo thì sao?", &format!("Ở Tokyo bây giờ là 06:10 {vi_today} khuya.")), format!("Ở Tokyo bây giờ là 06:10 {vi_today} sáng."));
        // here keeps here's
        assert_eq!(fix("and in Tokyo?", &format!("In Tokyo it's 06:10 morning; here it's 04:10 {today} late night.")),
            format!("In Tokyo it's 06:10 morning; here it's 04:10 {today} late night."));
    }

    #[test]
    fn a_bare_clock_time_answers_a_time_turn() {
        // (moondream:1.8b, live: "what time is it?" → " 22:20" stood; "and in Tokyo?" → "  22:20", then "00:20", both
        // thrown out as replies with no words)
        let lines = "Local time now: 2026-11-11 22:20 Wednesday evening (UTC+07:00, Asia/Ho_Chi_Minh)\n\
            Time in Tokyo now: 2026-11-12 00:20 Thursday late night (UTC+09:00, Asia/Tokyo)\n\n";
        let f = ReplyFacts::from_turn(&format!("{lines}and in Tokyo?"));
        assert!(f.answers_in_digits("  22:20") && f.answers_in_digits("00:20") && f.answers_in_digits("2026-11-12 00:20"));
        // (and the time is checked against Elisa's: here's 22:20 is Tokyo's 00:20)
        assert_eq!(f.fix("  22:20").trim(), "00:20");
        assert_eq!(f.fix("00:20"), "00:20");
        let here = ReplyFacts::from_turn("Local time now: 2026-11-11 22:20 Wednesday evening (UTC+07:00, Asia/Ho_Chi_Minh)\n\nwhat time is it?");
        assert!(here.answers_in_digits(" 22:20") && !here.answers_in_digits("22") && !here.answers_in_digits("[0.0, 0.17, 0.99, 0.29]"));
        // no fact of Elisa's behind it: a number to thanks, or a bare number on a time turn, is still no answer
        assert!(!ReplyFacts::from_turn("thanks!").answers_in_digits("4") && !f.answers_in_digits("4"));
        // a bare number on a computed turn, a date on a date turn, a time on a clock sum
        assert!(ReplyFacts::from_turn(&format!("(Situation: {COMPUTED}: 3 * 7 - 5 = 16 · {USE_ONE})\n\n3 boxes with 7 apples, give away 5, how many are left?")).answers_in_digits("16"));
        let q = "what day of the week is Christmas?";
        assert!(ReplyFacts::from_turn(&format!("(Situation: {})\n\n{q}", date_part(q, super::super::now_secs()).unwrap())).answers_in_digits("2026-12-25"));
        let train = "a train leaves at 15:20 and the trip takes 2 hours 55 minutes. when does it arrive?";
        assert!(ReplyFacts::from_turn(&format!("(Situation: {})\n\n{train}", clock_sum_part(train).unwrap())).answers_in_digits("18:15"));
    }

    #[test]
    fn a_places_zone_and_day_are_its_own() {
        // (gemma3:1b, live, 23:04 Wednesday here: "and in Tokyo?" got "It's 01:04 Wednesday, late night (UTC+07:00,
        // Ho Chi Minh)." — the clock Tokyo's, the rest copied from its answer about here)
        let lines = "Local time now: 2026-11-11 23:04 Wednesday late night (UTC+07:00, Ho Chi Minh)\n\
            Time in Tokyo now: 2026-11-12 01:04 Thursday late night (UTC+09:00, Asia/Tokyo)\n\n";
        // (the test's clock: a Wednesday at 23:xx here)
        let now = { let mut t = super::super::now_secs() - super::super::now_secs() % 3_600 + 240; while local_tm(t).map(|x| (x.tm_wday, x.tm_hour)) != Some((3, 23)) { t += 3_600; } t };
        let fix = |lines: &str, own: &str, reply: &str| {
            let mut f = ReplyFacts::from_turn(&format!("{lines}{own}"));
            let places = f.clock.as_ref().map(|c| c.places.clone()).unwrap_or_default();
            let (place_days, here_minute) = f.times.as_ref().map(TimeFix::place_days).unwrap_or_default();
            f.clock = ClockFix::new_for(own, now, true).map(|c| ClockFix { places, place_days, here_minute, ..c });
            f.fix(reply)
        };
        assert_eq!(fix(lines, "and in Tokyo?", "It's 01:04 Wednesday, late night (UTC+07:00, Ho Chi Minh)."), "It's 01:04 Thursday, late night (UTC+09:00, Tokyo).");
        assert_eq!(fix(lines, "and in Tokyo?", "In Tokyo it is Wednesday 1:04 AM (UTC+7)."), "In Tokyo it is Thursday 1:04 AM (UTC+09:00).");
        // a clause about here keeps here's; a right reply, and the answer about here, are left as they are
        assert_eq!(fix(lines, "and in Tokyo?", "In Tokyo it's 01:04 Thursday (UTC+09:00); here it's 23:04 Wednesday (UTC+07:00, Ho Chi Minh)."),
            "In Tokyo it's 01:04 Thursday (UTC+09:00); here it's 23:04 Wednesday (UTC+07:00, Ho Chi Minh).");
        assert_eq!(fix(lines, "what time is it?", "It's 23:04 Wednesday, late night (UTC+07:00, Ho Chi Minh)."), "It's 23:04 Wednesday, late night (UTC+07:00, Ho Chi Minh).");
        // (a right label cut off from its time by commas is no claim for another zone, and the day and part in the
        // brackets are the sentence's: the reply stays as it is)
        assert_eq!(fix(lines, "and in Tokyo?", "It's 01:04 Thursday, late night (UTC+09:00), Asia/Tokyo."), "It's 01:04 Thursday, late night (UTC+09:00), Asia/Tokyo.");
        assert_eq!(fix(lines, "and in Tokyo?", "The local time in Tokyo is 01:04 on November 12, 2026 (Thursday late night, UTC+09:00)."),
            "The local time in Tokyo is 01:04 on November 12, 2026 (Thursday late night, UTC+09:00).");
        let paris = "Local time now: 2026-11-11 23:04 Wednesday late night (UTC+07:00, Ho Chi Minh)\n\
            Time in Paris now: 2026-11-11 17:04 Wednesday afternoon (UTC+01:00, Europe/Paris)\n\n";
        assert_eq!(fix(paris, "and in Paris?", "In Paris, the time is 17:04 on November 11, 2026 (Wednesday afternoon, UTC+01:00)."),
            "In Paris, the time is 17:04 on November 11, 2026 (Wednesday afternoon, UTC+01:00).");
        assert_eq!(fix(paris, "and in Paris?", "It's 17:04 Thursday, late night (UTC+07:00, Ho Chi Minh)."), "It's 17:04 Wednesday, afternoon (UTC+01:00, Paris).");
        // Vietnamese: the day word of its language
        assert_eq!(fix(lines, "còn ở Tokyo thì sao?", "Ở Tokyo bây giờ là 01:04 Thứ Tư khuya (UTC+07:00, Ho Chi Minh)."), "Ở Tokyo bây giờ là 01:04 Thứ Năm khuya (UTC+09:00, Tokyo).");
    }

    /// noon local time on a day
    fn local_noon(y: i64, m: u32, d: u32) -> i64 {
        let mut t = days_from_civil(y, m, d) * 86_400;
        while local_tm(t).map(|x| (x.tm_mday as u32, x.tm_hour)) != Some((d, 12)) { t += 3_600; }
        t
    }

    #[test]
    fn a_date_question_is_worked_out_by_elisa() {
        let now = local_noon(2026, 11, 11);
        assert_eq!(civil_from_days(days_from_civil(2026, 12, 25)), (2026, 12, 25));
        assert_eq!(weekday_of(days_from_civil(2026, 11, 11)), 3, "a Wednesday");
        let p = date_part("how many days until Christmas?", now).unwrap();
        assert_eq!(p, format!("{COMPUTED}: days until Christmas = from 11 Nov 2026 to 25 Dec 2026 = 44 · {USE_ONE}"));
        assert!(date_part("còn bao nhiêu ngày nữa đến Giáng sinh?", now).unwrap().contains("= 44 ·"));
        assert!(date_part("con bao nhieu ngay nua den giang sinh?", now).unwrap().contains("= 44 ·"));
        assert!(date_part("how many days until December 25?", now).unwrap().contains("= 44 ·"));
        assert!(date_part("còn mấy ngày nữa đến 25/12?", now).unwrap().contains("= 44 ·"));
        assert!(date_part("how many days until new year?", now).unwrap().contains("= 51 ·"));
        assert!(date_part("how many days since Halloween?", now).unwrap().contains("= 11 ·"));
        assert!(date_part("what day of the week is Christmas?", now).unwrap().ends_with(&format!("= Friday · {USE_ONE}")));
        assert!(date_part("Giáng sinh năm nay là thứ mấy?", now).unwrap().contains("= Friday (Thứ Sáu)"));
        assert!(date_part("what date is 30 days from now?", now).unwrap().contains("= Friday, 11 Dec 2026"));
        assert!(date_part("how many days until Easter?", now).unwrap().starts_with(DATE_UNSURE));
        for m in ["what time is it?", "how many days are in a week?", "christmas was fun", "what day is it?"] { assert_eq!(date_part(m, now), None, "{m:?}"); }
        // the reply's number put right; a moving day's number becomes Elisa's question
        let turn = |own: &str| format!("(Situation: {})\n\n{own}", date_part(own, now).unwrap());
        let f = ReplyFacts::from_turn(&turn("how many days until Christmas?"));
        assert_eq!(f.fix("30 days"), "44 days");
        assert_eq!(f.fix("There are 15 days until December 25."), "There are 44 days until December 25.");
        assert_eq!(f.fix("44 days to go!"), "44 days to go!");
        let f = ReplyFacts::from_turn(&turn("còn bao nhiêu ngày nữa đến Giáng sinh?"));
        assert_eq!(f.fix("Còn 30 ngày nữa."), "Còn 44 ngày nữa.");
        // the day of the week of another day: a reply naming today's (or any other) day gets that day's
        let real = super::super::now_secs();
        let Some(DateAsk::Weekday(t)) = date_asked("what day of the week is Christmas?", real) else { panic!() };
        let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        let wrong = days[(weekday_of(t) + 2) % 7];
        let f = ReplyFacts::from_turn(&format!("(Situation: {})\n\nwhat day of the week is Christmas?", date_part("what day of the week is Christmas?", real).unwrap()));
        assert_eq!(f.fix(&format!("Christmas is on a {wrong}.")), format!("Christmas is on a {}.", days[weekday_of(t)]));
        // a wrong date beside the day: the asked day's
        let xmas = days_from_civil(2026, 12, 25);
        assert_eq!(date_fix("Thứ Sáu, 11/11/2026", xmas), "Thứ Sáu, 25/12/2026");
        assert_eq!(date_fix("Thứ Sáu, ngày 12 tháng 1.", xmas), "Thứ Sáu, ngày 25 tháng 12.");
        assert_eq!(date_fix("Friday, January 12.", xmas), "Friday, December 25.");
        assert_eq!(date_fix("Friday, December 25, 2026.", xmas), "Friday, December 25, 2026.");
        assert_eq!(date_fix("Friday 2026-11-11", xmas), "Friday 2026-12-25");
        // a day word that a date beside it gives the lie to
        let nov11 = days_from_civil(2026, 11, 11);
        assert_eq!(relative_fix("Apple is preparing to launch five new products tomorrow, November 6, 2026, at its event.", nov11),
            "Apple is preparing to launch five new products on November 6, 2026, at its event.");
        assert_eq!(relative_fix("Tomorrow, November 12, Apple launches.", nov11), "Tomorrow, November 12, Apple launches.");
        assert_eq!(relative_fix("The event was on 6 November 2026 (yesterday).", nov11), "The event was on 6 November 2026.");
        assert_eq!(relative_fix("Apple ra mắt sản phẩm vào ngày mai, 6/11/2026.", nov11), "Apple ra mắt sản phẩm vào 6/11/2026.");
        assert_eq!(relative_fix("Hôm nay, 11/11/2026, trời đẹp.", nov11), "Hôm nay, 11/11/2026, trời đẹp.");
        assert_eq!(relative_fix("See you tomorrow!", nov11), "See you tomorrow!");
        // a day word about an event the results date
        let res = result_dates("Apple to launch five new products tomorrow, November 6, 2026, including a touchscreen MacBook Pro.\nOther news: Apple launches products on November 6.", nov11);
        assert_eq!(relative_by_results("Apple is launching five new products today, including a touchscreen MacBook Pro.", nov11, &res),
            "Apple is launching five new products on November 6, 2026, including a touchscreen MacBook Pro.");
        assert_eq!(relative_by_results("Today's top story: Apple launches new products.", nov11, &res), "Today's top story: Apple launches new products.");
        assert_eq!(relative_by_results("It is sunny today in Hanoi.", nov11, &res), "It is sunny today in Hanoi.", "nothing in common");
        let with_note = result_dates("Apple is preparing to launch five new products tomorrow, November 6, 2026, including its first touchscreen MacBook Pro.\n   https://www.freep.com/story/tech/2026/11/06/apple-new-products/\n(a news search on 2026-11-11: answer now with the newest things these headlines say)", nov11);
        assert!(with_note.iter().all(|d| d.1 != "11/06"), "no date from a link");
        assert_eq!(relative_by_results("Based on the search results, Apple is launching five new products today: the first touchscreen MacBook Pro.", nov11, &with_note),
            "Based on the search results, Apple is launching five new products on November 6, 2026: the first touchscreen MacBook Pro.");
        assert_eq!(relative_by_results("Apple is launching five new products today.", nov11, &result_dates("Apple launches five new products on November 11.", nov11)),
            "Apple is launching five new products today.", "the dates agree");
        assert_eq!(relative_by_results("Apple is launching five new products today.", nov11, &result_dates("Apple launches five new products soon.", nov11)),
            "Apple is launching five new products today.", "no date");
        assert_eq!(relative_by_results("Apple ra mắt năm sản phẩm mới hôm nay.", nov11, &result_dates("Apple ra mắt năm sản phẩm mới vào ngày 6/11/2026.", nov11)),
            "Apple ra mắt năm sản phẩm mới vào 6/11/2026.");
        // a copied clock line (today's time and weekday) in a weekday reply: gone, never given the asked day's date
        let wk = ReplyFacts::from_turn(&format!("(Situation: {})\n\nGiáng sinh năm nay là thứ mấy?", date_part("Giáng sinh năm nay là thứ mấy?", real).unwrap()));
        assert_eq!(wk.fix("Giáng sinh năm nay là **Thứ Sáu**.\n\n---\nBây giờ là **09:46 sáng Thứ Tư, ngày 11/11/2026**."), "Giáng sinh năm nay là **Thứ Sáu**.");
        assert_eq!(without_clock_lines("Christmas is on a Friday. It is now 09:46 on Wednesday, November 11, 2026."), "Christmas is on a Friday.");
        assert_eq!(without_clock_lines("The meeting is at 10:30."), "The meeting is at 10:30.", "a time with no day or date");
        // a 24-hour time with AM / PM: the 12-hour form
        assert_eq!(twelve_hour_fix("13:18 PM."), "1:18 PM.");
        assert_eq!(twelve_hour_fix("It's 21:05 AM in Tokyo, 00:05 am here, 9:30 PM there."), "It's 9:05 PM in Tokyo, 12:05 am here, 9:30 PM there.");
        assert_eq!(without_clock_lines("- **Tên**: Linh\n- **Giờ hiện tại**: 10:13 sáng (UTC+7, Hà Nội), ngày 11/11/2026\n- **Mã cuộc họp**: ORCHID-42"),
            "- **Tên**: Linh\n- **Mã cuộc họp**: ORCHID-42");
        assert_eq!(without_clock_lines("As of 09:00 on Wednesday it is 25°C in Hanoi."), "As of 09:00 on Wednesday it is 25°C in Hanoi.", "more than the clock");
        // no real day in the reply: the made-up one put right, or Elisa's own answer
        assert_eq!(weekday_fill("Thứ Nhị, giáng sinh nay.", xmas, "Giáng sinh", "Giáng sinh năm nay là thứ mấy?"), "Thứ Sáu, giáng sinh nay.");
        assert_eq!(weekday_fill("Giáng sinh nay.", xmas, "Giáng sinh", "Giáng sinh năm nay là thứ mấy?"), "Giáng sinh năm nay là Thứ Sáu, 25/12/2026.");
        assert_eq!(weekday_fill("It's a lovely day.", xmas, "Christmas", "what day of the week is Christmas?"), "Christmas is on a Friday, December 25, 2026.");
        assert_eq!(weekday_fill("Thứ Sáu.", xmas, "Giáng sinh", "Giáng sinh năm nay là thứ mấy?"), "Thứ Sáu.");
        // "what day is Christmas?" names a day; "what day is it?" does not
        assert!(matches!(date_asked("what day is Christmas?", now), Some(DateAsk::Weekday(_))));
        assert!(date_asked("what day is it?", now).is_none());
        // N days from now: the reply's date and day are that day's
        let in30 = ReplyFacts::from_turn(&format!("(Situation: {})\n\nwhat date is 30 days from now?", date_part("what date is 30 days from now?", real).unwrap()));
        let Some(DateAsk::FromNow(_, t30)) = date_asked("what date is 30 days from now?", real) else { panic!() };
        let (_, m30, d30) = civil_from_days(t30);
        const MN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        let wrong_m = MN[(m30 as usize) % 12];
        assert_eq!(in30.fix(&format!("It will be {wrong_m} 3.")), format!("It will be {} {d30}.", MN[(m30 - 1) as usize]));
        let f = ReplyFacts::from_turn(&turn("how many days until Easter?"));
        assert!(f.fix("About 140 days.").starts_with("That day falls on a different date each year"));
        assert!(f.fix("It's currently Wednesday morning.").starts_with("That day falls on a different date each year"));
        assert_eq!(f.fix("Which date is Easter this year?"), "Which date is Easter this year?");
        // a date question with a clock line for a place of the same name: no time said
        let f = ReplyFacts::from_turn(&format!("Time in Christmas now: 2026-11-11 07:00 Wednesday morning (UTC+07:00, Indian/Christmas)\n\n(Situation: {})\n\nwhat day of the week is Christmas?", date_part("what day of the week is Christmas?", real).unwrap()));
        assert!(!f.fix("Christmas falls on a Friday.").starts_with("It's"), "{}", f.fix("Christmas falls on a Friday."));
    }

    #[test]
    fn a_time_in_a_reply_is_the_real_one() {
        let turn = "Date: 2026-11-11\nLocal time now: 2026-11-11 02:16 Wednesday late night (UTC+07:00, Asia/Ho_Chi_Minh)\n\nwhat time is it?";
        let f = ReplyFacts::from_turn(turn);
        assert_eq!(f.fix("It's 11:16 PM (or 23:16) in Ho Chi Minh City."), "It's 2:16 AM (or 02:16) in Ho Chi Minh City.");
        assert_eq!(f.fix("It's 02:16 (UTC+07:00)."), "It's 02:16 (UTC+07:00).", "right, and an offset is no time");
        assert_eq!(f.fix("It's 2:18 am."), "It's 2:18 am.", "the minute turned while it was written");
        assert_eq!(f.fix("It is Wednesday, late night."), "It's 02:16, Wednesday late night.", "no time: Elisa's own answer");
        // the day is the turn's own line's, whatever day the clock on the wall says (a saved or scripted turn)
        let tm = local_tm(turn_now(turn).unwrap()).unwrap();
        assert_eq!((tm.tm_wday, tm.tm_hour, tm.tm_min), (3, 2, 16));
        for (line, day, other) in [("2026-11-13 02:16 Friday", "Friday", "Thursday"), ("2026-11-15 02:16 Sunday", "Sunday", "Monday")] {
            let f = ReplyFacts::from_turn(&format!("Local time now: {line} late night (UTC+07:00, Asia/Ho_Chi_Minh)\n\nwhat time is it?"));
            assert_eq!(f.fix(&format!("It's 02:16, {day} late night.")), format!("It's 02:16, {day} late night."), "{line}");
            assert_eq!(f.fix(&format!("It's 02:16, {other} late night.")), format!("It's 02:16, {day} late night."), "{line}");
        }
        // a place asked: its line's time
        let tokyo = "Local time now: 2026-11-11 02:16 Wednesday late night (UTC+07:00, Asia/Ho_Chi_Minh)\n\
            Time in Tokyo now: 2026-11-11 04:16 Wednesday late night (UTC+09:00, Asia/Tokyo)\n\nwhat time is it in Tokyo?";
        let f = ReplyFacts::from_turn(tokyo);
        assert_eq!(f.fix("It's 2:16 AM in Tokyo."), "It's 4:16 AM in Tokyo.");
        assert_eq!(f.fix("It's 4:16 AM in Tokyo, 2:16 AM here."), "It's 4:16 AM in Tokyo, 2:16 AM here.");
        assert_eq!(f.fix("It's 2:16 AM here. Tokyo is ahead, at 4:16 AM."), "It's 2:16 AM here. Tokyo is ahead, at 4:16 AM.");
        assert_eq!(f.fix("It's 2:16 AM."), "It's 4:16 AM.", "no place named: the one asked");
        // Vietnamese: its own way of writing the time, and its words when there is none
        let vi = "Local time now: 2026-11-11 02:20 Wednesday late night (UTC+07:00)\n\nbây giờ là mấy giờ?";
        let f = ReplyFacts::from_turn(vi);
        assert_eq!(f.fix("Bây giờ là 22:20."), "Bây giờ là 02:20.");
        assert_eq!(f.fix("Bây giờ là 22h20."), "Bây giờ là 02h20.");
        assert_eq!(f.fix("Khuya rồi bạn ơi."), "Bây giờ là 02:20 khuya Thứ Tư, ngày 11/11/2026.", "no time: her own answer");
        // a time in words: read like digits, a wrong one put right in digits; a right one, or the right hour alone, stays
        let vi = "Local time now: 2026-11-11 04:22 Wednesday late night (UTC+07:00)\n\nmấy giờ rồi?";
        let f = ReplyFacts::from_turn(vi);
        assert_eq!(f.fix("Hai giờ hai chín phút đêm thứ Tư."), "04:22 đêm thứ Tư.");
        assert_eq!(f.fix("Bây giờ là bốn giờ hai mươi hai phút."), "Bây giờ là bốn giờ hai mươi hai phút.");
        assert_eq!(f.fix("Khoảng bốn giờ sáng."), "Khoảng bốn giờ sáng.", "the hour alone, right");
        assert_eq!(f.fix("Bay gio la hai gio hai chin phut."), "Bay gio la 04:22.");
        let en = ReplyFacts::from_turn("Local time now: 2026-11-11 04:22 Wednesday late night (UTC+07:00)\n\nwhat time is it?");
        assert_eq!(en.fix("It's about two o'clock."), "It's about 04:22.");
        assert_eq!(en.fix("It's four o'clock-ish."), "It's four o'clock-ish.");
        assert_eq!(spoken_number("hai mươi chín"), Some(29));
        assert_eq!(spoken_number("twenty-nine"), Some(29));
        assert_eq!(spoken_number("hai chín"), Some(29));
        // a time claimed for a zone not asked about: right, or its sentence goes (the local answer stays or is said)
        let t = "Local time now: 2026-11-11 05:06 Wednesday morning (UTC+07:00, Asia/Ho_Chi_Minh)\n\nwhat time is it?";
        let f = ReplyFacts::from_turn(t);
        assert_eq!(f.fix("2026-11-11 05:06 in your time zone is 2026-11-11 05:06 (noon + 1 day) in UTC. So, in UTC, it is noon."), "It's 05:06, Wednesday morning.");
        assert_eq!(f.fix("It's 05:06 here. So, in UTC, it is noon."), "It's 05:06 here.");
        assert_eq!(f.fix("It's 05:06 here, which is 22:06 in UTC."), "It's 05:06 here, which is 22:06 in UTC.");
        assert_eq!(f.fix("It's 05:06 (UTC+07:00)."), "It's 05:06 (UTC+07:00).", "the local offset label");
        assert_eq!(f.fix("It's 05:06 here; that is 23:06 UTC+1."), "It's 05:06 here; that is 23:06 UTC+1.");
        assert_eq!(f.fix("It's 05:06 here. That is 01:06 UTC+1."), "It's 05:06 here.", "a wrong offset time goes");
        // not a time ask: times left alone
        let f = ReplyFacts::from_turn("Local time now: 2026-11-11 02:16 Wednesday\n\nthe train leaves at 9:30, right?");
        assert_eq!(f.fix("Yes, at 9:30."), "Yes, at 9:30.");
        assert!(asks_time("what time is it in Tokyo?") && asks_time("bây giờ là mấy giờ?") && !asks_time("what day is it?"));
    }

    #[test]
    fn a_time_reply_with_no_time_or_junk_around_it_is_elisas_own_clock_answer() {
        // (phi4-mini:3.8b, live, on a time turn: "How can I assist you further?"; on a chat-routed one: "as an AI, I
        // don't have real-time capabilities, so I can't provide the current time")
        let turn = "Local time now: 2026-11-12 01:10 Thursday late night (UTC+07:00, Ho Chi Minh)\n\nwats the time rn";
        let f = ReplyFacts::from_turn(turn);
        // (the slang wordings are in data/clock_words.txt `time asks`; a held-out one is read by meaning, below)
        let own = "It's 01:10, Thursday late night.";
        assert_eq!(f.fix("How can I assist you further?"), own);
        assert_eq!(f.fix("As an AI, I don't have real-time capabilities, so I can't provide the current time."), own);
        assert_eq!(f.clock_answer().as_deref(), Some(own), "her answer for a thrown-out reply too");
        assert_eq!(f.facts_answer().as_deref(), Some(own));
        // a right time with a sensible line beside it stays; a time alone too
        assert_eq!(f.fix("It's 01:10 here. Anything else I can help with?"), "It's 01:10 here. Anything else I can help with?");
        assert_eq!(f.fix("01:10"), "01:10");
        // (minicpm-v:8b: "got the time?" → "Hi! Do you have any more questions…")
        let got = ReplyFacts::from_turn("Local time now: 2026-11-12 01:10 Thursday late night (UTC+07:00, Ho Chi Minh)\n\ngot the time?");
        assert_eq!(got.fix("Hi! Do you have any more questions or tasks for me?"), own);
        // not a time turn: a time in a reply is left alone, and there is no clock answer
        let train = ReplyFacts::from_turn("Local time now: 2026-11-12 01:10 Thursday late night (UTC+07:00, Ho Chi Minh)\n\nthe train leaves at 9:30, right?");
        assert_eq!(train.fix("Yes, at 9:30."), "Yes, at 9:30.");
        assert_eq!(train.clock_answer(), None);
        // (phi4-mini, Vietnamese: the right time inside garbled text going round)
        let vi = ReplyFacts::from_turn("Local time now: 2026-11-12 01:10 Thursday late night (UTC+07:00, Ho Chi Minh)\n\nbây giờ mấy giờ rồi bro");
        let vi_own = "Bây giờ là 01:10 khuya Thứ Năm, ngày 12/11/2026.";
        assert_eq!(vi.fix("Bây giờ trông vào con số, là 01:10 khuya Thứ Năm, ngày 12/11/2026. Giờ trông đó còn trông 7 giờ trôi qua, còn trôi qua."), vi_own);
        assert_eq!(vi.fix("Bây giờ là 01:10 khuya Thứ Năm. Bạn cần mình giúp gì nữa không?"), "Bây giờ là 01:10 khuya Thứ Năm. Bạn cần mình giúp gì nữa không?", "a good reply stays");
        // (cogito:8b: a twelve-hour figure with the wrong part of the day, "2:22 khuya" at 14:22 chiều)
        let pm = ReplyFacts::from_turn("Local time now: 2026-11-12 14:22 Thursday afternoon (UTC+07:00, Ho Chi Minh)\n\nbây giờ mấy giờ rồi bro");
        let pm_own = pm.clock_answer().expect("own answer");
        assert!(pm_own.contains("14:22"), "{pm_own}");
        assert_eq!(pm.fix("Hiện tại là 2:22 khuya Thứ Năm, ngày 12/11/2026"), pm_own);
        assert_eq!(pm.fix("Hiện tại là 2:22 chiều Thứ Năm, ngày 12/11/2026"), "Hiện tại là 2:22 chiều Thứ Năm, ngày 12/11/2026", "the right part stays");
        assert_eq!(pm.fix("Bây giờ là 14:22 chiều Thứ Năm."), "Bây giờ là 14:22 chiều Thứ Năm.");
        let en = ReplyFacts::from_turn("Local time now: 2026-11-12 14:22 Thursday afternoon (UTC+07:00, Ho Chi Minh)\n\nwhat time is it?");
        let en_own = en.clock_answer().expect("own answer");
        assert_eq!(en.fix("It's 2:22 late night."), en_own);
        assert_eq!(en.fix("It's 2:22 AM, Thursday."), "It's 2:22 PM, Thursday.", "an am/pm figure is put right as before");
        assert_eq!(en.fix("It's 2:22 in the afternoon."), "It's 2:22 in the afternoon.");
        // (moondream:1.8b: the right time for Paris, then talk about the word "Paris")
        let paris = ReplyFacts::from_turn("Local time now: 2026-11-12 01:14 Thursday late night (UTC+07:00, Ho Chi Minh)\n\
            Time in Paris now: 2026-11-11 19:14 Wednesday evening (UTC+01:00, Europe/Paris)\n\nwhat time is it in Paris?");
        let paris_own = "In Paris it's 19:14, Wednesday evening (UTC+01:00, Europe/Paris).";
        assert_eq!(paris.fix("It's 19:14. \"Paris\" is a placeholder for your location."), paris_own);
        assert_eq!(paris.fix("I don't have access to real-time data."), paris_own);
        assert_eq!(paris.fix("In Paris it's 19:14 in the evening."), "In Paris it's 19:14 in the evening.");
        // (a clock line with no label: the offset alone in the brackets)
        let bare = ReplyFacts::from_turn("Local time now: 2026-11-12 01:14 Thursday late night (UTC+07:00)\nTime in Paris now: 2026-11-11 19:14 Wednesday evening (UTC+01:00)\n\nwhat time is it in Paris?");
        assert_eq!(bare.fix("Sure!"), "In Paris it's 19:14, Wednesday evening (UTC+01:00).");
    }

    #[test]
    fn a_stray_day_word_after_the_weekday_goes_in_both_languages() {
        let f = ClockFix::new("hôm nay là thứ mấy?", local_noon(2026, 11, 13)).unwrap();
        assert_eq!(f.fix("Hôm nay là thứ năm ba."), "Hôm nay là thứ sáu.");
        assert_eq!(f.fix("Hôm nay là thứ sáu, ngày 13 tháng 11 năm 2026."), "Hôm nay là thứ sáu, ngày 13 tháng 11 năm 2026.");
        let e = ClockFix::new("what day is it today?", local_noon(2026, 11, 13)).unwrap();
        assert_eq!(e.fix("Today is Thursday."), "Today is Friday.");
    }

    #[test]
    fn a_date_or_weekday_ask_is_answered_from_the_clock_line() {
        for m in ["what's today's date?", "hôm nay là thứ mấy?", "what day is it today?", "hom nay la ngay may", "What is the date today?"] {
            assert!(asks_date(m) && asks_clock(m), "{m:?}");
        }
        for m in ["what time is it?", "mấy giờ rồi", "how are you?", "what day of the week is Christmas?"] { assert!(!asks_date(m), "{m:?}"); }
        assert!(asks_clock("what time is it?"));
        let lines = "Local time now: 2026-11-12 03:06 Thursday late night (UTC+07:00)\n\n## Request\n";
        let f = |own: &str| ReplyFacts::from_turn(&format!("{lines}{own}")).facts_answer();
        assert_eq!(f("what's today's date?").as_deref(), Some("Today is Thursday, 12 November 2026."));
        assert_eq!(f("what day is it today?").as_deref(), Some("Today is Thursday, 12 November 2026."));
        assert_eq!(f("hôm nay là thứ mấy?").as_deref(), Some("Hôm nay là Thứ Năm, ngày 12/11/2026."));
        assert_eq!(f("hom nay la ngay may?").as_deref(), Some("Hôm nay là Thứ Năm, ngày 12/11/2026."));
        assert_eq!(f("tell me a joke"), None);
        // without a clock line there is nothing to answer from
        assert_eq!(ReplyFacts::from_turn("## Request\nwhat's today's date?").facts_answer(), None);
        // a reply that gives the weekday or the date asked stands; one that gives neither is Elisa's own line
        let fx = |own: &str, reply: &str| ReplyFacts::from_turn(&format!("{lines}{own}")).fix(reply);
        assert_eq!(fx("hôm nay là thứ mấy?", "Ngày 12 tháng 11, 2026."), "Hôm nay là Thứ Năm, ngày 12/11/2026.");
        assert_eq!(fx("hôm nay là thứ mấy?", "Hôm nay là thứ năm."), "Hôm nay là thứ năm.");
        assert_eq!(fx("what day is it today?", "It's the 12th of November."), "Today is Thursday, 12 November 2026.");
        assert_eq!(fx("what day is it today?", "It's Thursday."), "It's Thursday.");
        assert_eq!(fx("what's today's date?", "November 12, 2026"), "November 12, 2026");
        assert_eq!(fx("what's today's date?", "2026-11-12"), "2026-11-12");
        assert_eq!(fx("what's today's date?", "Today is the 12th."), "Today is Thursday, 12 November 2026.");
        // (another year is not today)
        assert_eq!(fx("what's today's date?", "Today's date is 12/11/2021."), "Today is Thursday, 12 November 2026.");
        assert_eq!(fx("hom nay la ngay bao nhieu?", "Hôm nay là ngày 12/11/2026."), "Hôm nay là ngày 12/11/2026.");
        assert_eq!(f("hom nay la ngay bao nhieu?").as_deref(), Some("Hôm nay là Thứ Năm, ngày 12/11/2026."));
        assert_eq!(fx("tell me a joke", "Why did the chicken cross the road?"), "Why did the chicken cross the road?");
        // (moondream:1.8b, r32: here's time labelled with an earlier turn's Tokyo zone)
        let hcm = "Local time now: 2026-11-12 23:31 Thursday late night (UTC+07:00, Ho Chi Minh)\n\n## Request\n";
        let fz = |own: &str, reply: &str| ReplyFacts::from_turn(&format!("{hcm}{own}")).fix(reply);
        assert_eq!(fz("hôm nay là thứ mấy?", "Bây giờ là 23:31 khuya Thứ Năm (UTC+09:00, Asia/Tokyo)."), "Bây giờ là 23:31 khuya Thứ Năm (UTC+07:00, Ho Chi Minh).");
        assert_eq!(fz("hôm nay là thứ mấy?", "Hôm nay là Thứ Năm, 23:31 (UTC+07:00, Ho Chi Minh)."), "Hôm nay là Thứ Năm, 23:31 (UTC+07:00, Ho Chi Minh).");
        assert_eq!(fz("hôm nay là thứ mấy?", "Hôm nay là Thứ Năm, 23:31 (UTC+07:00)."), "Hôm nay là Thứ Năm, 23:31 (UTC+07:00).");
        assert_eq!(fz("what day is it today?", "It's Thursday, 23:31 (UTC+07:00, Asia/Seoul)."), "It's Thursday, 23:31 (UTC+07:00, Ho Chi Minh).");
        // a place's own line keeps its zone
        let tokyo = format!("{}Time in Tokyo now: 2026-11-13 01:31 Friday late night (UTC+09:00, Asia/Tokyo)\n\n## Request\n", hcm.trim_end_matches("\n## Request\n"));
        assert_eq!(ReplyFacts::from_turn(&format!("{tokyo}and in Tokyo?")).fix("In Tokyo it's 01:31 (UTC+09:00, Asia/Tokyo)."), "In Tokyo it's 01:31 (UTC+09:00, Asia/Tokyo).");
    }

    #[test]
    fn a_message_with_no_time_word_is_never_read_by_meaning() {
        // (no embedding model needed: the gate is words of the examples, so an ordinary message costs nothing)
        let words = &time_examples().2;
        for w in ["time", "clock", "hour", "late", "gio", "may", "roi"] { assert!(words.contains(w), "{w:?} is a time word of the examples"); }
        for w in ["what", "the", "is", "it", "you", "do", "have"] { assert!(!words.contains(w), "{w:?} is shared by both sections"); }
        for m in ["wats the time rn", "got the time?", "mấy giờ rồi", "may gio roi ban oi", "any clue what the clock says?", "Giờ là mấy giờ?",
            "is it late?", "tell me the hour", "mấy h rồi bạn ơi"] {
            assert!(time_ask_gate(m), "{m:?} passes the gate");
        }
        for m in ["thanks a lot", "chạy lệnh echo xin chào", "hello there", "ok cool", "can you search the web?", "what day is it today?",
            "how are you?", "what is the capital of France?", "bạn khỏe không?"] {
            assert!(!time_ask_gate(m), "{m:?} never reaches the embedding");
        }
    }

    #[test]
    fn a_time_ask_is_read_by_meaning_too() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        // held-out wordings (none is an example line nor a `time asks` wording), English, Vietnamese with and without marks
        for m in ["u know what time it is?", "do u have the time", "any clue what the clock says?", "tell me the hour",
            "mấy h rồi bạn ơi", "giờ là mấy giờ rồi ta", "may gio roi ban oi", "bây giờ mấy giờ rồi bro"] {
            assert!(asks_time(m), "{m:?}");
        }
        for m in ["what day is it today?", "what's the date?", "how long does a flight to Tokyo take?", "when does the shop open?",
            "do you have time for a quick question?", "tell me a joke", "hôm nay thứ mấy vậy?", "bao giờ thì xong?", "cảm ơn nhé",
            "how are you?", "what is the capital of France?", "set an alarm for 7", "can you search the web?"] {
            assert!(!asks_time(m), "{m:?}");
        }
        // an ask what Elisa can help with is about her, never the clock ("cho" is a word of the time examples too)
        for m in ["bạn có thể giúp gì cho tôi?", "ban co the giup gi cho toi", "bạn giúp gì được cho tôi?", "what can you help me with today?"] {
            assert!(!asks_time(m), "{m:?}");
        }
        // the turn's facts follow: an ask with no clock line in its turn still gets Elisa's answer from the clock
        let f = ReplyFacts::from_turn("u know what time it is?");
        assert!(f.clock_answer().is_some_and(|a| a.starts_with("It's ") && a.contains(':')), "{:?}", f.clock_answer());
        assert_eq!(f.fix("Nope, no clock here."), f.clock_answer().unwrap());
    }


    #[test]
    fn a_clock_time_plus_a_duration_is_worked_out_by_elisa() {
        let ask = "think it through step by step: a train leaves at 3:20 pm and the trip takes 2 hours 55 minutes. when does it arrive?";
        let part = clock_sum_part(ask).unwrap();
        assert_eq!(part, format!("{COMPUTED}: 3:20 pm + 2 h 55 min = 6:15 pm · {USE_ONE}"));
        // (no question about the clock now, and no arithmetic for a model to read)
        assert!(!asks_time(ask) && !crate::mcp_reason::needs_expression(ask) && sums_part(ask).is_none());
        let f = ReplyFacts::from_turn(&format!("(Situation: {part})\n\n{ask}"));
        // a wrong time becomes Elisa's; the result in any form stays; a working wrong throughout: hers alone; no time: hers added
        assert_eq!(f.fix("The train arrives at 7:55 pm."), "The train arrives at 6:15 pm.");
        assert_eq!(f.fix("It arrives at 6:15 PM."), "It arrives at 6:15 PM.");
        assert_eq!(f.fix("It arrives at 18:15."), "It arrives at 18:15.");
        assert_eq!(f.fix("3:20 pm plus 2 hours is 5:20 pm, plus 55 minutes is 6:15 pm."), "3:20 pm plus 2 hours is 5:20 pm, plus 55 minutes is 6:15 pm.");
        assert_eq!(f.fix("3:20 + 2:55 = 5:75, so 7:55 pm. The train arrives at 7:55 pm."), "3:20 pm + 2 h 55 min = 6:15 pm");
        assert_eq!(f.fix("The train arrives in the evening."), "The train arrives in the evening.\n\n3:20 pm + 2 h 55 min = 6:15 pm");
        // leaving: the duration before the start; a 24-hour start answered the same way; past midnight, the day said
        let leave = "I need to be at the office at 09:00 and the drive takes 45 minutes, when should I leave?";
        assert_eq!(clock_sum_part(leave).unwrap(), format!("{COMPUTED}: 09:00 − 45 min = 08:15 · {USE_ONE}"));
        assert_eq!(ReplyFacts::from_turn(&format!("(Situation: {})\n\n{leave}", clock_sum_part(leave).unwrap())).fix("Leave at 8:45."), "Leave at 08:15.");
        let late = "the show starts at 11:30 pm and lasts 1.5 hours, when does it end?";
        assert_eq!(clock_sum_part(late).unwrap(), format!("{COMPUTED}: 11:30 pm + 1 h 30 min = 1:00 am (the next day) · {USE_ONE}"));
        // (a wrong time that happens to read like the duration is still wrong: gemma3:1b wrote "1:30 am")
        let f = ReplyFacts::from_turn(&format!("(Situation: {})\n\n{late}", clock_sum_part(late).unwrap()));
        assert_eq!(f.fix("1:30 am (the next day)"), "1:00 am (the next day)");
        assert_eq!(f.fix("11:30 + 1:30 = 1:00, so 1:00 am."), "11:30 + 1:30 = 1:00, so 1:00 am.");
        // Vietnamese, by the data file's words: the result as a 24-hour time
        let vi = "tàu chạy lúc 3 giờ 20 chiều, đi mất 2 tiếng 55 phút thì đến nơi lúc mấy giờ?";
        assert_eq!(clock_sum_part(vi).unwrap(), format!("{COMPUTED}: 15:20 + 2 giờ 55 phút = 18:15 · {USE_ONE}"));
        assert!(!asks_time(vi) && clock_words(vi, tuesday_evening()).is_none());
        assert_eq!(ReplyFacts::from_turn(&format!("(Situation: {})\n\n{vi}", clock_sum_part(vi).unwrap())).fix("Tàu đến nơi lúc 19:55."), "Tàu đến nơi lúc 18:15.");
        // the way not said: no result, and a reply that states a time without asking becomes Elisa's question
        let open = "the meeting is at 3:20 pm, 2 hours 55 minutes. what time?";
        assert!(clock_sum_part(open).unwrap().starts_with(CLOCK_UNSURE));
        let f = ReplyFacts::from_turn(&format!("(Situation: {})\n\n{open}", clock_sum_part(open).unwrap()));
        assert_eq!(f.fix("It is at 6:15 pm."), "I'm not sure which way to count: is the 2 h 55 min after 3:20 pm or before it? Tell me and I'll work out the time.");
        assert_eq!(f.fix("Do you mean 2 h 55 min after 3:20 pm?"), "Do you mean 2 h 55 min after 3:20 pm?");
        assert_eq!(f.fix("The meeting is at 3:20 pm."), "The meeting is at 3:20 pm.", "the start said again is no guess");
        // no clock sum: two times, a duration with no start, a number after the duration with no unit, the clock now
        assert!(clock_sum_part("the train leaves at 3:20 pm and arrives at 6:15 pm, when is the trip?").is_none());
        assert!(clock_sum_part("the trip takes 2 hours 55 minutes, when is it?").is_none());
        assert!(clock_sum_part("a train leaves at 3:20 pm and takes 2 hours 3 times a week, when does it arrive?").is_none());
        assert!(clock_sum_part("what time is it in Tokyo?").is_none() && asks_time("what time is it in Tokyo?"));
    }

    #[test]
    fn an_event_before_today_is_told_as_done() {
        let nov11 = days_from_civil(2026, 11, 11);
        let t = |r: &str| tense_fix(r, nov11);
        assert_eq!(t("Apple is launching five new products on November 6, 2026."), "Apple launched five new products on November 6, 2026.");
        assert_eq!(t("Apple will launch five new products on November 6, 2026, at its event."), "Apple launched five new products on November 6, 2026, at its event.");
        assert_eq!(t("The sale starts on November 3."), "The sale started on November 3.");
        assert_eq!(t("Apple is set to unveil the Mac on 2026-11-06."), "Apple unveiled the Mac on 2026-11-06.");
        assert_eq!(t("Will Apple launch it on November 6, 2026?"), "Will Apple launch it on November 6, 2026?");
        // a later date, a bare date far back (next year's), a verb in another sentence or after the date: as written
        assert_eq!(t("Apple will launch five new products on November 20, 2026."), "Apple will launch five new products on November 20, 2026.");
        assert_eq!(t("The fair opens on January 5."), "The fair opens on January 5.");
        assert_eq!(t("Apple will launch more. The event was on November 6, 2026."), "Apple will launch more. The event was on November 6, 2026.");
        assert_eq!(t("On November 6, 2026, Apple will launch it."), "On November 6, 2026, Apple will launch it.");
        assert_eq!(t("Apple plans to launch on November 6, 2026."), "Apple plans to launch on November 6, 2026.");
        assert_eq!(t("Apple will launch in December, a report on November 6, 2026 says."), "Apple will launch in December, a report on November 6, 2026 says.");
        assert_eq!(t("Apple launched on November 6, 2026 and will launch more on December 2, 2026."), "Apple launched on November 6, 2026 and will launch more on December 2, 2026.");
        // Vietnamese: the "still to come" word becomes "already"
        assert_eq!(t("Apple sẽ ra mắt năm sản phẩm mới vào ngày 6/11/2026."), "Apple đã ra mắt năm sản phẩm mới vào ngày 6/11/2026.");
        assert_eq!(t("Apple sap ra mat san pham vao 6/11/2026."), "Apple da ra mat san pham vao 6/11/2026.");
        assert_eq!(t("Apple sẽ ra mắt sản phẩm vào 20/11/2026."), "Apple sẽ ra mắt sản phẩm vào 20/11/2026.");
        assert_eq!(t("See https://x.com/2026/11/06/apple-will-launch for more."), "See https://x.com/2026/11/06/apple-will-launch for more.");
    }

    #[test]
    fn noon_is_its_own_part_of_the_day() {
        // Wednesday 12:07 local
        let mut now: i64 = 1_794_268_800;
        while { let tm = local_tm(now).unwrap(); !(tm.tm_wday == 3 && tm.tm_hour == 12 && tm.tm_min == 7) } { now += 60; }
        assert_eq!(super::super::part_of_day(12), "noon");
        assert_eq!(super::super::part_of_day(11), "morning");
        assert_eq!(super::super::part_of_day(13), "afternoon");
        assert!(part_fits_hour("afternoon", 12) && !part_fits_hour("evening", 12));
        let w = clock_words("Mấy giờ rồi?", now).unwrap();
        assert!(w.contains("12:07 trưa Thứ Tư"), "{w}");
        let f = ReplyFacts { clock: ClockFix::new("Mấy giờ rồi?", now), ..Default::default() };
        assert_eq!(f.fix("Bây giờ là 12:07 chiều Thứ Tư, ngày 11/11/2026."), "Bây giờ là 12:07 trưa Thứ Tư, ngày 11/11/2026.");
        // a part also right at its hour is kept; one that is not is put right by that hour
        assert_eq!(f.fix("Lúc 11:30 trưa Thứ Tư."), "Lúc 11:30 trưa Thứ Tư.");
        assert_eq!(f.fix("Lúc 11:30 sáng Thứ Tư."), "Lúc 11:30 sáng Thứ Tư.");
        assert_eq!(f.fix("Lúc 13:20 trưa Thứ Tư."), "Lúc 13:20 trưa Thứ Tư.");
        assert_eq!(f.fix("Lúc 15:20 trưa Thứ Tư."), "Lúc 15:20 chiều Thứ Tư.");
        let f = ReplyFacts { clock: ClockFix::new("may gio roi", now), ..Default::default() };
        assert_eq!(f.fix("Bay gio la 12:07 chieu Thu Tu."), "Bay gio la 12:07 trua Thu Tu.");
        let en = ReplyFacts { clock: ClockFix::new("what time is it?", now), ..Default::default() };
        assert_eq!(en.fix("It's 12:07 afternoon, Wednesday."), "It's 12:07 afternoon, Wednesday.");
        assert_eq!(en.fix("It's 12:07 evening, Wednesday."), "It's 12:07 noon, Wednesday.");
    }

    #[test]
    fn a_time_question_gets_the_day_and_part_in_its_language() {
        let now = tuesday_evening();
        let w = clock_words("Mấy giờ rồi?", now).unwrap();
        assert!(w.contains("22:10 tối Thứ Ba, ngày 10/11/2026"), "{w}");
        let w = clock_words("bây giờ là mấy giờ?", now).unwrap();
        assert!(w.contains("tối Thứ Ba"), "{w}");
        // written without accents: the words without them too
        let p = clock_words("may gio roi?", now).unwrap();
        assert!(p.contains("22:10 toi Thu Ba"), "{p}");
        assert!(clock_words("hom nay la thu may?", now).is_some());
        for m in ["what time is it?", "hey", "tôi muốn ăn phở", "giờ tôi bận"] { assert_eq!(clock_words(m, now), None, "{m:?}"); }
        // the reply's other day and part next to the time: today's
        let f = ReplyFacts { clock: ClockFix::new("Mấy giờ rồi?", now), ..Default::default() };
        assert_eq!(f.fix("Bây giờ là 22:10 ngày 10/11/2026 (thứ hai chiều, GMT+07:00)."), "Bây giờ là 22:10 ngày 10/11/2026 (thứ ba tối, GMT+07:00).");
        assert_eq!(f.fix("Bây giờ là 22:10 tối Thứ Ba."), "Bây giờ là 22:10 tối Thứ Ba.");
        let f = ReplyFacts { clock: ClockFix::new("may gio roi", now), ..Default::default() };
        assert_eq!(f.fix("Bay gio la 22:10 chieu Thu Hai, toi nghi vay."), "Bay gio la 22:10 toi Thu Ba, toi nghi vay.");
        // English, from the same data file: a wrong day put right, one the user named kept, other days next to
        // today's kept
        let en = ReplyFacts { clock: ClockFix::new("what day is it today?", now), ..Default::default() };
        assert_eq!(en.fix("Today is Thursday, November 10th, 2026."), "Today is Tuesday, November 10th, 2026.");
        assert_eq!(en.fix("Today is Tuesday; tomorrow is Wednesday."), "Today is Tuesday; tomorrow is Wednesday.");
        assert_eq!(en.fix("It is 22:10 in the afternoon, Monday."), "It is 22:10 in the afternoon, Tuesday.");
        assert_eq!(clock_words("what day is it today?", now), None, "English gets no extra line");
        let en = ReplyFacts { clock: ClockFix::new("what day is Friday next week?", now), ..Default::default() };
        assert_eq!(en.fix("Friday next week is November 20."), "Friday next week is November 20.");
        // a day or part the user named themself is never changed; no time question in a language with clock words,
        // no change at all
        let f = ReplyFacts { clock: ClockFix::new("thứ Sáu tuần sau là ngày mấy? sáng hay chiều?", now), ..Default::default() };
        assert_eq!(f.fix("Thứ Sáu tuần sau là ngày 20/11, buổi sáng."), "Thứ Sáu tuần sau là ngày 20/11, buổi sáng.");
        assert_eq!(f.fix("22:10 chiều thứ hai"), "22:10 chiều thứ ba", "a day the user did not name is still put right");
        let f = ReplyFacts::from_turn("what will I do on Friday morning?");
        assert_eq!(f.fix("On Friday morning you have yoga."), "On Friday morning you have yoga.");
        let f = ReplyFacts::from_turn("thứ Hai sáng nay tôi làm gì?");
        assert_eq!(f.fix("Sáng thứ Hai bạn đi họp."), "Sáng thứ Hai bạn đi họp.");
        let f = ReplyFacts { clock: ClockFix::new("hom nay la thu may?", now), ..Default::default() };
        assert_eq!(f.fix("22:55 tối Thu May."), "22:55 tối Thu Ba.");
        let v = turn_fact_values(&format!("(Situation: {})\n\nmay gio roi?", clock_words("may gio roi?", now).unwrap()));
        assert_eq!(v, ["22:10 toi thu ba", "22:10"]);
    }

    fn sales() -> Table {
        let rows: Vec<Vec<String>> = [["region", "units"], ["North", "120"], ["South", "340"], ["East", "95"]].iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect();
        Table::new("sales.csv", &rows).unwrap()
    }

    #[test]
    fn a_table_question_gets_its_exact_facts() {
        let t = sales();
        let most = table_facts("which region sold the most units?", &t).unwrap();
        assert_eq!(most, "Elisa computed it exactly from sales.csv: highest units = South (340) · answer with these results");
        let total = table_facts("what's the total of all units?", &t).unwrap();
        assert!(total.contains("total units = 120 + 340 + 95 = 555"), "{total}");
        assert!(table_facts("which one sold the least?", &t).unwrap().contains("lowest units = East (95)"));
        assert!(table_facts("average units?", &t).unwrap().contains("average units = 555 / 3 = 185"));
        assert!(table_facts("how many regions are there?", &t).unwrap().contains("number of region = 3"));
        assert!(table_facts("vùng nào bán nhiều nhất?", &t).unwrap().contains("South (340)"));
        for m in ["summarise this", "what is in the file?"] { assert_eq!(table_facts(m, &t), None, "{m:?}"); }
        // no numbers column: no table
        let words: Vec<Vec<String>> = vec![vec!["a".into(), "b".into()], vec!["x".into(), "y".into()]];
        assert!(Table::new("w.csv", &words).is_none());
    }

    #[test]
    fn a_reply_with_a_wrong_table_fact_is_put_right() {
        let t = sales();
        let facts = |q: &str| ReplyFacts::from_turn(&format!("(Situation: {})\n\n{q}", table_facts(q, &t).unwrap()))
            .merge(ReplyFacts::session("", Some(&t), Default::default()));
        let f = facts("which region sold the most units?");
        assert_eq!(f.fix("North"), "South");
        assert_eq!(f.fix("không rõ"), "South sold the most units (340).");
        let vi = |q: &str| ReplyFacts::from_turn(&format!("(Situation: {})\n\n{q}", table_facts(q, &t).unwrap())).merge(ReplyFacts::session("", Some(&t), Default::default()));
        assert_eq!(vi("vùng nào bán nhiều nhất?").fix("không rõ"), "South (340)", "no question words to reuse: the row and its number");
        assert_eq!(f.fix("North sold the most, with 120 units."), "South sold the most, with 340 units.");
        assert_eq!(f.fix("South, with 340 units."), "South, with 340 units.");
        // no row named: said in the user's words, nothing of the wrong answer left
        let named = |q: &str| ReplyFacts::from_turn(&format!("(Situation: {})\n\n{q}", table_facts(q, &t).unwrap()))
            .merge(ReplyFacts::session("", Some(&t), Default::default()));
        let f = named("which region sold the most units in sales.csv?");
        assert_eq!(f.fix("Asia/Ho_Chi_Minh"), "South sold the most units in sales.csv (340).");
        assert_eq!(f.fix("not specified\n\nSouth (340)"), "not specified\n\nSouth (340)", "names the row: kept");
        assert_eq!(f.fix("The region is not specified."), "South sold the most units in sales.csv (340).");
        // the note's own words copied into the reply go
        let f = named("what is the total of units in sales.csv?");
        assert_eq!(f.fix("555 · answer with these results"), "555");
        assert_eq!(f.fix("Elisa computed it exactly from sales.csv: total units = 120 + 340 + 95 = 555"), "total units = 120 + 340 + 95 = 555");
        // Elisa's exact count of a file is a fact too
        let c = ReplyFacts::session("1\tmeeting\n[elisa: counted exactly: notes.txt has 8 words; use this number]", None, Default::default());
        assert_eq!(c.fix("The number of words in count.txt is 1."), "The number of words in count.txt is 8.");
        assert_eq!(c.fix("notes.txt has 8 words, saved to count.txt."), "notes.txt has 8 words, saved to count.txt.");
        // Elisa's own plain answer from the results (the file's lines, then what was done): the count added, a code's
        // number never taken for it
        assert_eq!(c.fix("Here's what I found:\n\nRead notes.txt:\nmeeting code ORCHID-42\n\nRewrote count.txt."),
            "Here's what I found:\n\nRead notes.txt:\nmeeting code ORCHID-42\n\nRewrote count.txt.\n\nnotes.txt: 8 words");
        // a count written as a word: right ones stand as they are, a wrong one before its unit is put right
        assert_eq!(c.fix("The notes.txt file contains eight words."), "The notes.txt file contains eight words.");
        assert_eq!(c.fix("Tệp notes.txt có tám từ."), "Tệp notes.txt có tám từ.");
        assert_eq!(c.fix("Tep notes.txt co tam tu."), "Tep notes.txt co tam tu.");
        assert_eq!(c.fix("It has only one word."), "It has only 8 words.");
        // an everyday word that is also a number is never taken for a wrong count
        assert_eq!(c.fix("Không, notes.txt has 8 words."), "Không, notes.txt has 8 words.");
        let f = facts("what's the total of all units?");
        assert_eq!(f.fix("The total is 1200 units."), "The total is 555 units.");
        assert_eq!(f.fix("North 120 + South 340 + East 95 = 1,200."), "North 120 + South 340 + East 95 = 555.");
        assert_eq!(f.fix("The total is 555."), "The total is 555.");
        assert_eq!(f.fix("1. North 120\n2. South 340\n3. East 95"), "1. North 120\n2. South 340\n3. East 95\n\n120 + 340 + 95 = 555");
    }

    #[test]
    fn a_choice_between_two_numbers_is_picked_by_elisa() {
        let q = "which is bigger, 3 GB or 5 GB?";
        let p = compare_part(q).unwrap();
        assert_eq!(p, "Elisa computed it exactly: bigger = 5 GB (5) · answer with this result");
        let f = ReplyFacts::from_turn(&format!("(Situation: {p})\n\n{q}"));
        assert_eq!(f.fix("3 GB"), "5 GB");
        assert_eq!(f.fix("5 GB is bigger."), "5 GB is bigger.");
        assert_eq!(f.fix("5"), "5", "the picked side's number alone");
        assert!(compare_part("which is smaller, 10 or 12?").unwrap().contains("smaller = 10 (10)"));
        assert!(compare_part("cái nào lớn hơn, 7 kg hay 4 kg?").unwrap().contains("= 7 kg (7)"));
        for m in ["1 TB or 900 GB, which is bigger?", "is 7 more or less than 4?", "3 GB or 5 GB?", "what is 2 + 3?"] {
            assert_eq!(compare_part(m), None, "{m:?}");
        }
    }

    #[test]
    fn arithmetic_comes_with_its_parts_and_a_wrong_number_is_put_right() {
        let q = "what is 12.5% of 3,480 plus 19 squared?";
        let p = sums_part(q).unwrap();
        assert!(p.contains("(12.5/100*3480) + 19^2 = 435 + 361 = 796"), "{p}");
        let f = ReplyFacts::from_turn(&format!("(Situation: {p})\n\n{q}"));
        assert_eq!(f.fix("12.5% of 3,480 is 445.0"), "12.5% of 3,480 is 435\n\n435 + 361 = 796");
        assert_eq!(f.fix("12.5% of 3,480 is 435 and 19 squared is 361, so the total is 796."), "12.5% of 3,480 is 435 and 19 squared is 361, so the total is 796.");
        assert_eq!(f.fix("The answer is 806."), "The answer is 796.");
        // a part given a wrong value: Elisa's part everywhere, and the working then holds
        assert_eq!(f.fix("12.5% of 3,480 is 42.0\n19 squared is 361\n12.5% of 3,480 plus 19 squared is 42.0 + 361 = 796"),
            "12.5% of 3,480 is 435\n19 squared is 361\n12.5% of 3,480 plus 19 squared is 435 + 361 = 796");
        assert_eq!(f.fix("19^2 is 361 and 12.5% of 3,480 is 435, so 796."), "19^2 is 361 and 12.5% of 3,480 is 435, so 796.");
        // arithmetic stated wrong: its result put right; a right one, a date or a version left alone
        assert_eq!(stated_sums_fix("So 21 - 5 = 15 apples."), "So 21 - 5 = 16 apples.");
        assert_eq!(stated_sums_fix("3 × 7 = 22, and 2 x 4 = 8"), "3 × 7 = 21, and 2 x 4 = 8");
        assert_eq!(stated_sums_fix("1,200 + 340 = 1,540"), "1,200 + 340 = 1,540");
        assert_eq!(stated_sums_fix("On 2026-11-11 = today"), "On 2026-11-11 = today");
        // the result said of a part of the sum: the whole sum and its working
        assert_eq!(f.fix("12.5% of 3,480 is 796."), "12.5% of 3,480 plus 19 squared = 435 + 361 = 796.");
        assert_eq!(f.fix("Sure! 12.5% of 3,480 is 796. Hope that helps!"), "Sure! 12.5% of 3,480 plus 19 squared = 435 + 361 = 796. Hope that helps!");
        assert_eq!(f.fix("12.5% of 3,480 plus 19 squared is 796."), "12.5% of 3,480 plus 19 squared is 796.");
        assert_eq!(f.fix("The answer is 796."), "The answer is 796.");
        // wrong all through (moondream:1.8b): Elisa's working instead
        assert_eq!(f.fix("12.5% of 3,480 is 0.4875. Adding 19 squared gives us: 0.4875 + 0.69 = 0.725."), "435 + 361 = 796");
        let two = ReplyFacts::from_turn(&format!("(Situation: {})\n\nwhat is 2 + 3?", sums_part("what is 2 + 3?").unwrap()));
        assert_eq!(two.fix("2 + 3 = 4"), "2 + 3 = 5");
        // a word problem's computed result (`mcp_reason::word_problem`) is read the same way
        let wp = ReplyFacts::from_turn("(Situation: Elisa computed it exactly: (3*7) - 5 = 16 · answer with this result)\n\n3 boxes with 7 apples, give away 5?");
        assert_eq!(wp.fix("You have 2 apples left."), "You have 16 apples left.");
        assert_eq!(wp.fix("5"), "16", "a bare number of the question's own");
        assert_eq!(wp.fix("1."), "16.", "a bare number, not a list marker");
        // not worked out: a number stated anyway becomes Elisa's question; a reply that asks stays
        let unsure = ReplyFacts::from_turn(&format!("(Situation: {} · give no number)\n\nif I have 3 boxes with 7 apples and give away 5 apples, how many are left?", crate::mcp_reason::UNSURE));
        assert!(unsure.fix("2").starts_with("I'm not sure how to work this out."), "{}", unsure.fix("2"));
        assert_eq!(unsure.fix("Do you mean 3 times 7, minus 5?"), "Do you mean 3 times 7, minus 5?");
        let vi = ReplyFacts::from_turn(&format!("(Situation: {})\n\ntôi có 3 hộp, mỗi hộp 7 quả táo, cho đi 5 quả, còn mấy quả?", crate::mcp_reason::UNSURE));
        assert!(vi.fix("còn hai quả").starts_with("Mình chưa chắc"), "{}", vi.fix("còn hai quả"));
        // no computed part, no change; times and dates are no numbers to fix
        assert_eq!(ReplyFacts::from_turn("hey").fix("It is 4."), "It is 4.");
        assert_eq!(two.fix("At 20:39 on 2026-11-10: 5."), "At 20:39 on 2026-11-10: 5.");
    }
}
