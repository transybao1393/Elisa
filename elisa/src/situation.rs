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

mod own_facts;
pub use own_facts::*;

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

// ----- a question about one thing Elisa can do -----

/// The frames and example data of `ability_answer`: `data/ability_answers.txt`.
const ABILITY_DATA: &str = include_str!("../data/ability_answers.txt");
/// the similarity to a kind's nearest example from which an ability is of that kind, and how much nearer it must be
/// than the nearest `[other]` example (all-MiniLM-L6-v2; see the tests)
pub const ABILITY_MIN: f32 = 0.55;
pub const ABILITY_LEAD: f32 = 0.08;
/// the similarity to a tool's own description from which an ability is that tool's
pub const ABILITY_TOOL_MIN: f32 = 0.6;
/// most words an ability has ("search the web", "read files on my computer"): more is a task with its details
const ABILITY_WORDS: usize = 7;

/// One language's frames of `data/ability_answers.txt`: each (the frame with `{x}`, the answer when Elisa can, when she
/// cannot), the pronoun swaps and the politeness tail words.
struct AbilityLang { frames: Vec<(String, String, String)>, swap: Vec<(String, String)>, tail: Vec<String> }
/// One kind of ability: its name, the tools that give it, its example wordings.
struct AbilityKind { name: String, tools: Vec<String>, examples: Vec<String> }

/// lower-case with Vietnamese marks folded away, one char per char (`reply_lang::fold_mark`)
fn folded(s: &str) -> String { s.chars().map(crate::reply_lang::fold_mark).collect() }

/// the words of a text, folded, of three letters or more
fn ability_words(s: &str) -> Vec<String> {
    folded(s).split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(str::to_string).collect()
}

fn ability_data() -> &'static (Vec<AbilityLang>, Vec<AbilityKind>) {
    static D: std::sync::OnceLock<(Vec<AbilityLang>, Vec<AbilityKind>)> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let mut langs: Vec<AbilityLang> = Vec::new();
        let mut kinds: Vec<AbilityKind> = Vec::new();
        let mut in_lang = false;
        let list = |v: &str| v.split('|').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
        for l in ABILITY_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(h) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                let mut it = h.split_whitespace();
                let name = it.next().unwrap_or("").to_string();
                // (a two-letter header is a language's frames; any other, a kind of ability with its tools)
                in_lang = name.chars().count() == 2 && name.chars().all(|c| c.is_ascii_lowercase());
                if in_lang { langs.push(AbilityLang { frames: Vec::new(), swap: Vec::new(), tail: Vec::new() }); }
                else { kinds.push(AbilityKind { name, tools: it.map(str::to_string).collect(), examples: Vec::new() }); }
                continue;
            }
            // (examples folded: the embedding model's vocabulary has no Vietnamese tone marks, a marked word is unknown to it)
            if !in_lang { if let Some(k) = kinds.last_mut() { k.examples.push(folded(l)); } continue; }
            let Some(lang) = langs.last_mut() else { continue };
            let Some((k, v)) = l.split_once('=') else { continue };
            match k.trim() {
                "swap" => lang.swap = list(v).iter().filter_map(|p| p.split_once('>')).map(|(a, b)| (folded(a.trim()), b.trim().to_string())).collect(),
                "tail" => lang.tail = list(v).iter().map(|t| folded(t)).collect(),
                frame if frame.contains("{x}") => {
                    let (yes, no) = v.split_once('|').unwrap_or((v, v));
                    lang.frames.push((frame.to_string(), yes.trim().to_string(), no.trim().to_string()));
                }
                _ => {}
            }
        }
        (langs, kinds)
    })
}

/// The ability a message asks about by the frames of `data/ability_answers.txt` ("can you search the web?" → "search
/// the web"), as the user wrote it, with the frame that fits it (its language and index). The frame's opening stands
/// at a word's start (a "hey, " before it is fine) and its end closes the message; the longest frame wins. None for
/// a longer ask that is a task with its details ("can you search the web for who won the 2022 world cup"), one
/// naming a file, a number or a quote, or one asking the time (`asks_time`).
fn ability_asked(ask: &str) -> Option<(&'static AbilityLang, usize, String)> {
    let mut text = ask.trim().trim_end_matches(['?', '!', '.', '…', ' ', '？']).trim().to_string();
    if text.is_empty() || asks_time(&text) { return None; }
    // the politeness tail off the end ("… được không nhé", "… please"), in any language
    loop {
        let lx = folded(&text);
        let Some(t) = ability_data().0.iter().flat_map(|l| &l.tail).find(|t| lx.ends_with(t.as_str()) && lx[..lx.len() - t.len()].ends_with(' ')) else { break };
        text = text.chars().take(lx.chars().count() - t.chars().count()).collect::<String>().trim_end().to_string();
    }
    let text = text.as_str();
    let low = folded(text);
    let chars: Vec<char> = text.chars().collect();
    // (the fold keeps one char per char: byte offsets in `low` map to char indexes)
    if low.chars().count() != chars.len() { return None; }
    let mut best: Option<(&'static AbilityLang, usize, String, usize)> = None;
    for lang in &ability_data().0 {
        for (i, (frame, _, _)) in lang.frames.iter().enumerate() {
            let f = folded(frame);
            let Some((pre, suf)) = f.split_once("{x}") else { continue };
            let suf = suf.trim_end();
            if pre.trim().is_empty() { continue; }
            // (the frame opens the message, or follows a mark: "hey, can you …"; never a word, "what can you do?")
            let Some(at) = low.match_indices(pre).map(|(i, _)| i).find(|&i| low[..i].trim_end().chars().next_back().is_none_or(|c| !c.is_alphanumeric())) else { continue };
            let end = if suf.is_empty() { low.len() } else if low.ends_with(suf) { low.len() - suf.len() } else { continue };
            if end <= at + pre.len() { continue; }
            let (a, b) = (low[..at + pre.len()].chars().count(), low[..end].chars().count());
            let x: String = chars[a..b].iter().collect();
            let fit = pre.len() + suf.len();
            if best.as_ref().is_none_or(|(_, _, _, n)| fit > *n) { best = Some((lang, i, x, fit)); }
        }
    }
    let (lang, i, x, _) = best?;
    // the politeness tail off ("… please", "… nhé")
    let mut x = x.trim().trim_end_matches([',', ' ']).to_string();
    loop {
        let lx = folded(&x);
        let Some(t) = lang.tail.iter().find(|t| lx.ends_with(t.as_str()) && lx[..lx.len() - t.len()].ends_with(' ')) else { break };
        x = x.chars().take(lx.chars().count() - t.chars().count()).collect::<String>().trim_end().to_string();
    }
    let words = x.split_whitespace().count();
    if x.is_empty() || words > ABILITY_WORDS || x.chars().any(|c| c.is_ascii_digit() || matches!(c, ':' | '/' | '"' | '“' | '”' | '`' | '\\')) { return None; }
    // (a file name: "read notes.txt")
    if x.split_whitespace().any(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).split_once('.').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())) { return None; }
    Some((lang, i, x))
}

/// The kind of ability `x` is (`data/ability_answers.txt`, by meaning): nearest a kind's example at `ABILITY_MIN` or more,
/// `ABILITY_LEAD` nearer than any `[other]` example, and sharing a word with that example that no `[other]` example
/// has (MiniLM reads Vietnamese poorly). None otherwise, or without the embedding model.
fn ability_kind(x: &str) -> Option<&'static AbilityKind> {
    let kinds = &ability_data().1;
    let texts: Vec<&str> = kinds.iter().flat_map(|k| k.examples.iter().map(String::as_str)).collect();
    let scores = crate::route::meaning_scores(&folded(x), &texts)?;
    let mut at = 0;
    let mut per: Vec<(&AbilityKind, f32, &str)> = Vec::new();
    for k in kinds {
        let n = k.examples.len();
        let best = scores[at..at + n].iter().enumerate().fold((f32::MIN, ""), |b, (i, s)| if *s > b.0 { (*s, k.examples[i].as_str()) } else { b });
        per.push((k, best.0, best.1));
        at += n;
    }
    let other = per.iter().find(|(k, _, _)| k.name == "other").map_or(f32::MIN, |(_, s, _)| *s);
    let other_words: std::collections::HashSet<String> = kinds.iter().filter(|k| k.name == "other").flat_map(|k| k.examples.iter().flat_map(|e| ability_words(e))).collect();
    let (k, t, ex) = per.into_iter().filter(|(k, _, _)| k.name != "other").max_by(|a, b| a.1.total_cmp(&b.1))?;
    let mine: Vec<String> = ability_words(x).into_iter().filter(|w| !other_words.contains(w)).collect();
    let shared = ability_words(ex).iter().any(|w| mine.contains(w));
    (t >= ABILITY_MIN && t - other >= ABILITY_LEAD && shared).then_some(k)
}

/// Whether `x` is a chat ability by meaning (an `[other]` example of `data/ability_answers.txt` at ABILITY_MIN or
/// nearer: writing, explaining, translating), or there is no embedding model to tell: the model's to answer.
fn chat_ability(x: &str) -> bool {
    let others: Vec<&str> = ability_data().1.iter().filter(|k| k.name == "other").flat_map(|k| k.examples.iter().map(String::as_str)).collect();
    if others.is_empty() { return true; }
    crate::route::meaning_scores(&folded(x), &others).is_none_or(|s| s.iter().copied().fold(f32::MIN, f32::max) >= ABILITY_MIN)
}

/// Whether `x` is what one of `tools` (name, description) does, by its description's gist's meaning
/// (`ABILITY_TOOL_MIN`) and a shared word: an ability a plugin's tool gives that no kind of `data/ability_answers.txt`
/// names.
fn ability_of_a_tool(x: &str, tools: &[(String, String)]) -> bool {
    let pairs: Vec<(String, &str)> = tools.iter().filter(|(_, d)| !d.trim().is_empty()).map(|(_, d)| (tool_gist(d), d.as_str())).collect();
    let texts: Vec<&str> = pairs.iter().map(|(g, _)| g.as_str()).collect();
    if texts.is_empty() { return false; }
    let Some(scores) = crate::route::meaning_scores(&folded(x), &texts) else { return false };
    let mine = ability_words(x);
    scores.iter().zip(&pairs).any(|(s, (_, d))| *s >= ABILITY_TOOL_MIN && ability_words(d).iter().any(|w| mine.contains(w)))
}

/// A tool description's gist for the meaning check: what it does in a few words (`route::tool_action`: "Search the
/// web; returns …" → "search the web"), else its first clause. Never the whole description: one is many lines of
/// parameters and notes, which say more than the one ability asked about and cost a long embedding each.
fn tool_gist(description: &str) -> String {
    crate::route::tool_action(description).unwrap_or_else(|| {
        let first = description.split(". ").next().unwrap_or(description);
        let end = first.find([';', '(', ':', ',', '\n']).unwrap_or(first.len());
        first[..end].trim().trim_end_matches('.').to_lowercase()
    })
}

/// `x` with the pronouns swapped between the user and Elisa (`swap` of its language: "my" → "your", "mình" → "bạn")
fn swap_pronouns(x: &str, swap: &[(String, String)]) -> String {
    x.split_whitespace().map(|w| {
        let core = w.trim_matches(|c: char| !c.is_alphanumeric());
        if core.is_empty() { return w.to_string(); }
        let f = folded(core);
        match swap.iter().find(|(a, _)| *a == f) {
            // (a word typed without its marks gets its swap without them: "may minh" → "may ban")
            Some((_, to)) => w.replacen(core, &if core.is_ascii() { folded(to) } else { to.clone() }, 1),
            None => w.to_string(),
        }
    }).collect::<Vec<_>>().join(" ")
}

/// Whether `reply` answers the question about the ability `x` as Elisa would: it speaks of it (shares a word of `x`
/// that is none of the question frames' own: "file", not "được"), is not the question said back with the pronouns
/// turned ("Tôi có thể chạy lệnh được không nhé."), its first sentence about it says yes when she `can` and no when
/// she cannot (`reply_check::says_not`), and when she can, no sentence about it denies it ("I can read files you
/// attach, but I cannot read files directly on your computer").
fn ability_reply_fits(reply: &str, x: &str, can: bool, lang: &AbilityLang) -> bool {
    let frame_words: std::collections::HashSet<String> = lang.frames.iter().flat_map(|(f, _, _)| ability_words(f))
        .chain(lang.tail.iter().flat_map(|t| ability_words(t))).collect();
    let mut content: Vec<String> = ability_words(x).into_iter().filter(|w| !frame_words.contains(w)).collect();
    if content.is_empty() { content = ability_words(x); }
    let words = ability_words(reply);
    if words.len() < 2 || !content.iter().any(|w| words.contains(w)) { return false; }
    if ability_asked(&swap_pronouns(reply, &lang.swap)).is_some_and(|(_, _, back)| folded(&back) == folded(x)) { return false; }
    let mut about = reply.split_inclusive(['.', '!', '?', ';', '\n']).filter(|s| { let w = ability_words(s); content.iter().any(|c| w.contains(c)) });
    let Some(first) = about.next() else { return false };
    crate::reply_check::says_not(first) != can && (!can || !about.any(crate::reply_check::says_not))
}

/// Elisa's own answer to a question about one thing she can do ("can you search the web?", "bạn có đọc được file
/// trên máy mình không?": `ability_asked`), when the model's `reply` does not answer it as she would (`ability_reply_fits`:
/// gemma3:1b said "Okay."). Whether she can comes from what she holds: the tools of the ability's kind among `tools`
/// (name, description), a tool whose description says it (`ability_of_a_tool`), or the model's detected `vision`; the
/// words are the user's own ("Yes, I can search the web.", "Có, mình đọc được file trên máy bạn."). None when the
/// message asks no such question, the ability is none Elisa can tell (chat: writing, explaining), or the reply fits.
pub fn ability_answer(ask: &str, reply: &str, tools: &[(String, String)], vision: bool) -> Option<String> {
    let body = crate::context::request_body(ask);
    let own = crate::context::own_words(body).unwrap_or(body);
    let (lang, i, x) = ability_asked(own)?;
    // a tool she has says it does that: yes; else by the kind of ability, from the tools of that kind she has
    let can = if ability_of_a_tool(&x, tools) { true } else {
        match ability_kind(&x) {
            Some(k) if k.name == "vision" => vision,
            Some(k) => k.tools.iter().any(|t| tools.iter().any(|(n, _)| n == t)),
            // (an ability of no kind and of no tool that is no chat ability either, "read minds": nothing she has
            // gives it, so a reply claiming it is unbacked and gets her no; any other reply, or none, is left alone)
            None if !chat_ability(&x) && ability_reply_fits(reply, &x, true, lang) => false,
            None => return None,
        }
    };
    if ability_reply_fits(reply, &x, can, lang) { return None; }
    let (_, yes, no) = &lang.frames[i];
    Some((if can { yes } else { no }).replace("{x}", &swap_pronouns(&x, &lang.swap)))
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

/// the part of the day for a local hour, in English (the hours in `data/clock_words.txt`)
pub fn part_of_day(hour: u32) -> &'static str { english_part_of_day(hour) }

/// The thing a question asks about the user themself, when the whole question is "what is my X" (any question word,
/// "is / are / was / 's") or "do you know / remember my X": X ("favourite colour"). None for anything else
/// ("how do I fix my code?", a statement, a longer ask).
pub fn own_fact_asked(msg: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)^(?:(?:what|which|who|where|when|how)(?:'s|\s+(?:is|are|was|were))|(?:do|did)\s+you\s+(?:know|remember))\s+my\s+(?P<k>[\p{L}][\p{L}'-]*(?:\s+[\p{L}][\p{L}'-]*){0,3})$").unwrap());
    let body = crate::context::request_body(msg);
    let own = crate::context::own_words(body).unwrap_or(body).replace(['’', '‘'], "'");
    let q = own.trim().trim_end_matches(['?', '.', '!', ' ']).trim();
    re.captures(q).map(|c| c["k"].to_lowercase())
}

/// Whether `known` (what this turn's request carries: recalled memories, notes, pinned facts, the conversation)
/// says anything about `subject`: one of its words there (words of 4+ letters by their first four, so "favorite
/// color" finds "favourite colour").
pub fn subject_known(subject: &str, known: &str) -> bool {
    let key = |w: &str| -> String { if w.chars().count() >= 4 { w.chars().take(4).collect() } else { w.to_string() } };
    let have: std::collections::HashSet<String> = known.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(key).collect();
    subject.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).any(|w| have.contains(&key(w)))
}

/// The line after a question about the user's own X when memory was searched and nothing Elisa has says it
/// (`own_fact_asked`, `subject_known`): the answer is that the user has not said it (a small model otherwise names
/// one: after /forget, "what's my favourite colour?" got "Blue" and "You like purple.").
pub fn unknown_fact_note(subject: &str) -> String {
    format!("{}about their {subject}: Elisa searched her memory and this chat and found nothing about it, so they have \
        not told you. Say you don't know their {subject} yet and ask them; never guess one.)", crate::route::ASK_PREFIX)
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
            // a Vietnamese greeting gets the ask in words, never an English example to copy ("Xin chào, bạn khỏe
            // không?" got "… hi! what's next?")
            parts.push(if f.vi {
                match (long, next.is_some(), has_topic) {
                    (true, true, _) => "welcome them back in Vietnamese and offer to <next>",
                    (false, true, _) => "greet back briefly in Vietnamese and offer to <next>",
                    (true, false, true) => "welcome them back in Vietnamese and offer more on <topic>",
                    (true, false, false) => "welcome them back briefly in Vietnamese",
                    (false, false, true) => "greet back briefly in Vietnamese and offer more on <topic>",
                    (false, false, false) => "greet back briefly in Vietnamese, then ask what's next",
                }
            } else {
                match (long, next.is_some(), has_topic) {
                    (true, true, _) => "reply like \"Welcome back! Want me to <next>?\"",
                    // a greeting mid-chat is greeted back, briefly (not with a topic or an old answer)
                    (false, true, _) => "greet back briefly, e.g. \"Hi! Want me to <next>?\"",
                    (true, false, true) => "reply like \"Welcome back! More on <topic>?\"",
                    (true, false, false) => "reply like \"Welcome back!\"",
                    (false, false, true) => "greet back briefly, e.g. \"Hi! More on <topic>?\"",
                    (false, false, false) => "greet back briefly, e.g. \"Hi! What's next?\"",
                }
            }.into());
        }
        None => {
            let (t, topic) = f.past.as_ref()?;
            let gap = f.now - t;
            if !(0..=RECENT_S).contains(&gap) || topic.trim().is_empty() { return None; }
            // only a request is offered back: not a question (it got its answer) and not facts the user told
            // ("hi! remember that my dog is Bo" offered back made "hello again" get "our dog Bo is doing great")
            if !offer_topic(topic) || crate::context::only_user_facts(topic) { return None; }
            parts.push("returning user".into());
            parts.push(format!("last chat {} ago: \"{}\"", ago(gap), clip(topic, 40)));
            parts.push("one line: greet, ask: continue it or something new?".into());
        }
    }
    Some(format!("{PREFIX}{})", parts.join(" · ")))
}

/// Whether a conversation topic is worth offering back on a mid-chat greeting ("Hi! More on <topic>?"): a request
/// with something to go on with, not a question (it got its answer already; offering "more on" it reads oddly).
/// Nor small talk (`route::small_talk`), nor an ask done once answered ("tell me something fun"), by meaning
/// (`one_off`): offered back, a greeting got an unasked fun fact instead of a greeting.
pub fn offer_topic(topic: &str) -> bool {
    let t = topic.trim();
    !t.is_empty() && !t.trim_end_matches(['"', '\'', ')', ' ']).ends_with(['?', '？'])
        && !crate::route::small_talk(t) && !one_off(t)
}

/// Example data for `one_off`: a `[one_off]` and an `[ongoing]` section, one example per line.
const TOPICS_DATA: &str = include_str!("../data/topics.txt");

/// Whether an ask is nearer, by meaning, to a `[one_off]` example of `data/topics.txt` than to any `[ongoing]` one.
/// False when the embedding model is not installed (the topic is offered as before).
fn one_off(ask: &str) -> bool {
    let (mut sec, mut one, mut on) = ("", Vec::new(), Vec::new());
    for l in TOPICS_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(n) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) { sec = n; }
        else if sec == "one_off" { one.push(l); } else { on.push(l); }
    }
    let best = |ex: &[&str]| crate::route::meaning_scores(ask, ex).map(|v| v.into_iter().fold(f32::MIN, f32::max));
    matches!((best(&one), best(&on)), (Some(a), Some(b)) if a > b)
}

/// A mid-chat greeting's topic comes from the last this-many user turns only: an older request is stale.
pub const TOPIC_TURNS: usize = 3;

/// The facts Elisa adds to any turn from the user's own words (`own`: `context::request_body`, no Elisa lines), as
/// situation parts, at `now` (unix seconds; local clock):
/// - the arithmetic in it, computed (`mcp_reason::sums_note`): a small model answered "what is 2 + 3?" with "4";
/// - the part of the day, when the words name another one (`part_of_day`'s own words, not a closing such as "good
///   night"): "morning! busy day ahead" at 20:47 got "Good to hear—morning!";
/// - a message that only tells facts about the user (`context::only_user_facts`): acknowledged briefly, nothing
///   added ("My name is Linh and I live in Hanoi." got an unasked street-food tip).
pub fn turn_facts(own: &str, now: i64) -> Vec<String> {
    let mut v = exact_facts(own, now);
    if let Some(d) = day_note(own, now) { v.push(d); }
    if !own.contains('?') && crate::context::only_user_facts(own) {
        // (a bare "acknowledge them" came back as "I acknowledge your details.")
        // the facts as Elisa's own lines, with whose they are: "using their name if they gave it" after "my dog is
        // Bo" got "nice to meet you, Bo"
        let facts = crate::context::user_facts(own);
        let user = facts.iter().find_map(|f| f.strip_prefix("The user's name is ").or_else(|| f.strip_prefix("The user wants to be called ")));
        let call = match user { Some(n) => format!("call them {n}"), None => "they gave no name of their own: call them by no name".into() };
        // (no goodbye either: at 02:30, minicpm-v ended it with "Have a good night")
        v.push(format!("the user is telling you about themself: {} · reply with one short, warm thank-you for telling you; {call}; \
            no tips, offers, questions or goodbye", facts.join("; ")));
    }
    v
}

/// What Elisa computed exactly from the user's words, on any window (a few tokens; a tiny window's small model needs
/// them most): the arithmetic with its parts (`own_facts::sums_part`), a choice between two numbers
/// (`own_facts::compare_part`), a clock time plus or minus a duration (`own_facts::clock_sum_part`), and the day and
/// part of the day in the language of a time question (`clock_words`).
pub fn exact_facts(own: &str, now: i64) -> Vec<String> {
    [sums_part(own), compare_part(own), clock_sum_part(own), date_part(own, now), clock_words(own, now)].into_iter().flatten().collect()
}

/// A tiny window's line: `exact_facts` and `extra` (a word problem's result, a table's facts) as one situation line;
/// None when there are none.
pub fn with_exact_facts(own: &str, now: i64, extra: Option<String>) -> Option<String> {
    let mut f = exact_facts(own, now);
    f.extend(extra);
    (!f.is_empty()).then(|| format!("{PREFIX}{})", f.join(" · ")))
}

/// `text` without its "Local time now: …" line (a first message's header on a greeting or small talk: nobody asked
/// the time); everything else as it is.
pub fn without_clock_line(text: &str) -> String {
    // the first such line (the header's), wherever the minute turned since it was written
    let at = if text.starts_with("Local time now: ") { Some(0) } else { text.find("\nLocal time now: ").map(|i| i + 1) };
    let Some(i) = at else { return text.to_string() };
    let end = text[i..].find('\n').map_or(text.len(), |j| i + j + 1);
    format!("{}{}", &text[..i], &text[end..])
}

/// `text` without its header's "Date: …" line (a greeting opening a chat: phi4-mini answered "hey" with talk about
/// the date); the date comes with the first turn that is not small talk (`repl::turn`).
pub fn without_date_line(text: &str) -> String {
    let at = if text.starts_with("Date: ") { Some(0) } else { text.find("
Date: ").map(|i| i + 1) };
    let Some(i) = at else { return text.to_string() };
    let end = text[i..].find('\n').map_or(text.len(), |j| i + j + 1);
    // a header left empty: its blank separator line goes too
    let rest = if i == 0 || text[..i].ends_with("\n\n") { text[end..].trim_start_matches('\n') } else { &text[end..] };
    format!("{}{}", &text[..i], rest)
}

/// The part of the day said right when the message names another one: "it is evening here now (20:47), not morning".
pub fn day_note(own: &str, now: i64) -> Option<String> {
    if crate::route::closing(own) { return None; }
    let (h, m) = local_hm(now);
    let part = part_of_day(h);
    // a greeting's part of the day, in any language of `data/clock_words.txt` ("good morning", "chào buổi sáng")
    let (said, greeting, word) = greeting_part(own)?;
    // (a part also right at this hour, "good afternoon" at 12:30, is no other part)
    if said == part || part_fits_hour(&said, h) { return None; }
    // the hour with no greeting of its own (late night): a hello back, the hour at most noted ("good morning" at 01:21
    // got "Good night, sleep well!" from ministral-3:8b when told only that it was late night)
    if part.contains(' ') {
        return Some(format!("it is {h:02}:{m:02} here, {part} · the user is saying hello: say hello back in one short line, \
            noting lightly that it is late if you like; never say good night or goodbye, never tell them to sleep"));
    }
    // said as what to do ("never call it morning" got "good afternoon"; a quoted "good evening" came back bare); in
    // another language than English, the greeting itself put right in it ("chào buổi tối")
    let right = part_word_as(&word, part).unwrap_or_else(|| part.to_string());
    let own_way = if right.eq_ignore_ascii_case(part) { String::new() }
        else { format!(" (in their words: \"{}{right}\")", &greeting[..greeting.len() - word.len()]) };
    Some(format!("it is {part} here ({h:02}:{m:02}), not {said} · greet back warmly for the {part}{own_way}, in one short line"))
}

/// The part-of-day word a greeting reply should have said, when the user's greeting named another part
/// (`day_note`) and the reply's opening repeats the user's: ("morning", "evening"), or in the greeting's language
/// ("sáng", "tối"; with accents, as replies write it). None otherwise, or late at night (no greeting word for it).
pub fn day_fix(own: &str, now: i64) -> Option<(String, String)> {
    day_note(own, now)?;
    let part = part_of_day(local_hm(now).0);
    if part.contains(' ') { return None; }
    let (said, _, word) = greeting_part(own)?;
    // the accented form when the greeting had none: a reply to "chao buoi sang" writes "Chào buổi sáng"
    let accented = |key: &str| part_word_as(&word, key).and_then(|w| accented_part(&w, key)).unwrap_or_else(|| key.to_string());
    Some((accented(&said).to_lowercase(), accented(part).to_lowercase()))
}

/// `hint` (the turn's line before the user's words, if any) with `turn_facts` in it: added to a situation line, or a
/// situation line of their own when there is none; another kind of hint is left as it is.
pub fn with_turn_facts(hint: Option<String>, own: &str, now: i64) -> Option<String> { with_turn_facts_plus(hint, own, now, None) }

/// `with_turn_facts` plus one part Elisa worked out with a model call this turn (`mcp_reason::word_problem`).
pub fn with_turn_facts_plus(hint: Option<String>, own: &str, now: i64, extra: Option<String>) -> Option<String> {
    let mut f = turn_facts(own, now);
    f.extend(extra);
    if f.is_empty() { return hint; }
    match hint {
        None => Some(format!("{PREFIX}{})", f.join(" · "))),
        Some(h) if h.starts_with(PREFIX) && h.ends_with(')') && !h.contains('\n') => Some(format!("{} · {})", &h[..h.len() - 1], f.join(" · "))),
        // a follow-up hint: the facts as their own line before it (views strip both, `context::request_body`), so a
        // short follow-up ("and the total?") still gets them
        Some(h) if h.starts_with(crate::followup::PREFIX) => Some(format!("{PREFIX}{})\n\n{h}", f.join(" · "))),
        other => other,
    }
}

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
pub fn local_hour(t: i64) -> u32 { local_hm(t).0 }

/// the local (hour, minute) of a unix time
pub fn local_hm(t: i64) -> (u32, u32) {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() { return (12, 0); }
        (tm.tm_hour as u32, tm.tm_min as u32)
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
    fn a_question_about_one_ability_is_answered_from_elisas_own_tools() {
        let tools: Vec<(String, String)> = [("read", "Read a file, line-numbered."), ("bash", "Run a zsh command in cwd. Never use it to write files."),
            ("web_search", "Search the web; returns the top results (title, url, snippet). For news, current facts or anything not in the project.")]
            .iter().map(|(n, d)| (n.to_string(), d.to_string())).collect();
        // the frames, and the ability as the user wrote it (no embedding model needed)
        let x = |m: &str| ability_asked(m).map(|(_, _, x)| x);
        assert_eq!(x("can you search the web?").as_deref(), Some("search the web"));
        assert_eq!(x("hey, could you read files on my computer please?").as_deref(), Some("read files on my computer"));
        assert_eq!(x("bạn có đọc được file trên máy mình không?").as_deref(), Some("đọc được file trên máy mình"));
        assert_eq!(x("ban co the tim tren mang khong").as_deref(), Some("tim tren mang"));
        assert_eq!(x("bạn có thể chạy lệnh được không nhé?").as_deref(), Some("chạy lệnh"));
        // a task with its details, a file named, the general question, a time ask: none
        for m in ["can you search the web for who won the 2022 fifa world cup", "can you read notes.txt?", "what can you do?", "can you tell me what time it is?", "hey"] {
            assert_eq!(x(m), None, "{m:?}");
        }
        assert_eq!(swap_pronouns("read files on my computer for me", &ability_data().0[0].swap), "read files on your computer for you");
        if elisa_mem::embed::model_dir().is_none() { return; }
        // (gemma3:1b, live: "can you search the web?" → "Okay.")
        assert_eq!(ability_answer("can you search the web?", "Okay.", &tools, false).as_deref(), Some("Yes, I can search the web."));
        assert_eq!(ability_answer("can you search the web?", "I'm sorry, I cannot browse the internet.", &tools, false).as_deref(), Some("Yes, I can search the web."));
        assert_eq!(ability_answer("can you search the web?", "Yes, I can search the web for you. What should I look up?", &tools, false), None, "a fitting reply stands");
        // (granite3.2-vision:2b, live, in Vietnamese: looping text)
        assert_eq!(ability_answer("bạn có đọc được file trên máy mình không?", "Bạn có thể cung cấp tôi một cách chính xác để tôi có thể giúp bạn.", &tools, false).as_deref(),
            Some("Có, mình đọc được file trên máy bạn."));
        assert_eq!(ability_answer("ban co the chay lenh tren may minh khong?", "Được.", &tools, false).as_deref(), Some("Có, mình có thể chay lenh tren may ban."));
        // vision from the model's detected facts; a tool she lacks
        assert_eq!(ability_answer("can you see images?", "Yes, send me a picture!", &tools, false).as_deref(), Some("No, I can't see images here."));
        assert_eq!(ability_answer("can you look at pictures I send you?", "Sure.", &tools, true).as_deref(), Some("Yes, I can look at pictures you send me."));
        let no_web: Vec<(String, String)> = tools.iter().filter(|(n, _)| n != "web_search").cloned().collect();
        assert_eq!(ability_answer("can you search the web?", "Okay.", &no_web, false).as_deref(), Some("No, I can't search the web here."));
        assert_eq!(ability_answer("are you able to run shell commands?", "No.", &tools, false).as_deref(), Some("Yes, I'm able to run shell commands."));
        // an ability nothing she has gives, and no chat ability: a reply claiming it gets her no; any other reply, or none, is left alone
        assert_eq!(ability_answer("can you read minds?", "Yes, I can read minds.", &tools, false).as_deref(), Some("No, I can't read minds here."));
        assert_eq!(ability_answer("can you read minds?", "No, I can't read minds.", &tools, false), None);
        assert_eq!(ability_answer("can you read minds?", "", &tools, false), None);
        assert_eq!(ability_answer("can you write a poem?", "Yes, I can write a poem.", &tools, false), None, "a chat ability is the model's to answer");
        assert_eq!(ability_answer("bạn bay được không?", "Có, mình bay được.", &tools, false).as_deref(), Some("Không, mình không bay được."));
        assert_eq!(ability_answer("bạn bay được không?", "Không, mình không bay được đâu.", &tools, false), None);
        // (seen live) a reply sharing only a frame word with the ability, the question said back, a denial after a yes
        assert_eq!(ability_answer("bạn có đọc được file trên máy mình không?", "Được, bạn có thể trải lời câu hỏi của bạn. Tôi có thể trải lời câu hỏi của bạn.", &tools, false).as_deref(),
            Some("Có, mình đọc được file trên máy bạn."));
        assert_eq!(ability_answer("bạn có thể chạy lệnh được không nhé?", "Tôi có thể chạy lệnh được không nhé.", &tools, false).as_deref(), Some("Có, mình có thể chạy lệnh."));
        assert_eq!(ability_answer("can you read files on my computer?", "I can read files on my computer? I don't have access to your local files. I can read files you attach or provide, but I cannot read files directly on your computer.", &tools, false).as_deref(),
            Some("Yes, I can read files on your computer."));
        assert_eq!(ability_answer("can you see images?", "No, I cannot see images. I can describe pictures you describe to me.", &tools, false), None, "a right no with a nuance stands");
        // a chat ability is the model's to answer
        for (m, r) in [("can you write a poem?", "Sure, what about?"), ("can you help me with something?", "Of course."), ("do you speak Vietnamese?", "Yes")] {
            assert_eq!(ability_answer(m, r, &tools, false), None, "{m:?}");
        }
        // a plugin's tool, by its own description
        let pdf = vec![("make_pdf".to_string(), "Make a PDF file from plain text.".to_string())];
        assert_eq!(ability_answer("can you make a pdf file?", "Hmm.", &pdf, false).as_deref(), Some("Yes, I can make a pdf file."));
    }


    #[test]
    fn no_context_no_line() {
        assert_eq!(line(&Facts { now: NOW, hour: 21, ..Default::default() }), None, "first session ever");
        // an earlier session too old, or with no topic: still nothing
        assert_eq!(line(&Facts { now: NOW, past: Some((NOW - 5 * 86_400, "README summary".into())), ..Default::default() }), None);
        assert_eq!(line(&Facts { now: NOW, past: Some((NOW - 3_600, " ".into())), ..Default::default() }), None);
        // facts the user told, or a question already answered, are not offered back
        for t in ["hi! remember that my dog is Bo", "My name is Linh and I live in Hanoi.", "What's my favourite colour?"] {
            assert_eq!(line(&Facts { now: NOW, past: Some((NOW - 60, t.into())), ..Default::default() }), None, "{t}");
        }
        // a Vietnamese greeting mid-chat: the ask in words, no English example
        let vi = line(&Facts { now: NOW, hour: 21, last_turn: Some(NOW - 60), vi: true, ..Default::default() }).unwrap();
        assert!(vi.contains("in Vietnamese") && !vi.contains("Hi!"), "{vi}");
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
        assert!(!offer_topic("thủ đô của Pháp là gì？") && !offer_topic("  ") && offer_topic("read notes.txt and summarise it"));
        if elisa_mem::embed::model_dir().is_some() {
            assert!(!offer_topic("tell me something fun please") && offer_topic("plan a weekend in Hanoi"));
        }
        // a past session is never mentioned mid-conversation
        let mid = line(&Facts { now: NOW, last_turn: Some(NOW - 60), past: Some((NOW - 60, "old topic".into())), ..Default::default() }).unwrap();
        assert!(!mid.contains("old topic"));
        for l in [&quick, &back, &failed, &task, &stopped] { assert!(short(l), "{l}"); }
    }

    #[test]
    fn a_question_about_the_users_own_fact_and_whether_anything_says_it() {
        assert_eq!(own_fact_asked("What's my favourite colour?").as_deref(), Some("favourite colour"));
        // such a question is a no-tool turn (the note goes only on those)
        assert!(crate::route::classify("What's my favourite colour?", &Default::default(), &[]).groups.tool_free());
        assert_eq!(own_fact_asked("what is my name").as_deref(), Some("name"));
        assert_eq!(own_fact_asked("Do you remember my meeting code?").as_deref(), Some("meeting code"));
        assert_eq!(own_fact_asked("Local time now: x\n\nwhere’s my hometown?").as_deref(), Some("hometown"));
        for m in ["how do I fix my code?", "my favourite colour is green", "what is the capital of France?", "what's my name and where do I live?", "what is your name?"] {
            assert_eq!(own_fact_asked(m), None, "{m:?}");
        }
        // nothing at all (after /forget): unknown; the user said it, in either spelling: known
        assert!(!subject_known("favourite colour", ""));
        assert!(!subject_known("favourite colour", "- your name is Linh\nThe user lives in Hanoi"));
        assert!(subject_known("favourite colour", "- your favorite color is green"));
        assert!(subject_known("name", "The user's name is Linh"));
        let n = unknown_fact_note("favourite colour");
        assert!(n.starts_with(crate::route::ASK_PREFIX) && n.contains("favourite colour") && n.ends_with(')'), "{n}");
        // views strip it like any ask line
        assert_eq!(crate::context::request_body(&format!("what's my favourite colour?{n}")), "what's my favourite colour?");
    }

    /// a time at `hour`:30 local (any zone the tests run in)
    fn at_local(hour: u32) -> i64 {
        let mut t = NOW - NOW % 3_600 + 1_800;
        while local_hour(t) != hour { t += 3_600; }
        t
    }

    #[test]
    fn a_greeting_naming_another_part_of_the_day_gets_the_real_one() {
        let eve = at_local(20);
        let n = day_note("morning! busy day ahead", eve).unwrap();
        assert_eq!(n, "it is evening here (20:30), not morning · greet back warmly for the evening, in one short line");
        assert_eq!(day_fix("good morning", eve), Some(("morning".into(), "evening".into())));
        assert_eq!(day_fix("good evening", eve), None);
        assert!(day_note("Good morning", eve).is_some());
        // the right part, a part talked about, a closing, no part: nothing
        for m in ["good evening!", "what did I do this morning?", "good night", "hey", "this morning I ran 5k"] {
            assert_eq!(day_note(m, eve), None, "{m:?}");
        }
        assert_eq!(day_note("good morning!", at_local(8)), None);
        // late at night: a hello back, never a farewell; and no word swapped into the reply
        let late = at_local(1);
        let n = day_note("good morning", late).unwrap();
        assert!(n.contains("say hello back") && n.contains("never say good night or goodbye") && !n.contains("greet back warmly"), "{n}");
        assert_eq!(day_fix("good morning", late), None);
        // Vietnamese, from the [vi] parts of data/clock_words.txt, with or without accents
        let n = day_note("chào buổi sáng", eve).unwrap();
        assert_eq!(n, "it is evening here (20:30), not morning · greet back warmly for the evening (in their words: \"chào buổi tối\"), in one short line");
        assert!(day_note("chao buoi sang!", eve).unwrap().contains("\"chao buoi toi\""));
        assert_eq!(day_fix("chao buoi sang", eve), Some(("sáng".into(), "tối".into())));
        assert_eq!(day_fix("Chào buổi sáng", eve), Some(("sáng".into(), "tối".into())));
        assert_eq!(day_note("chào buổi tối", eve), None, "the right part");
        assert!(day_note("chào buổi tối", at_local(8)).unwrap().contains("\"chào buổi sáng\""));
        assert!(day_note("chào buổi sáng", late).unwrap().contains("say hello back"));
        // no greeting: a part talked about, a word that only looks like one
        for m in ["sáng nay tôi chạy bộ", "hôm qua buổi sáng mưa", "I sang.", "toi la Linh"] {
            assert_eq!(day_note(m, eve), None, "{m:?}");
        }
    }

    #[test]
    fn turn_facts_join_the_situation_line_or_make_one() {
        let eve = at_local(20);
        // arithmetic: computed by Elisa
        let l = with_turn_facts(None, "Reply in one short sentence: what is 2 + 3?", eve).unwrap();
        assert!(l.starts_with(PREFIX) && l.contains("2 + 3 = 5") && l.ends_with(')'), "{l}");
        assert_eq!(strip(&format!("{l}\n\nwhat is 2 + 3?")), "what is 2 + 3?", "views strip it");
        // facts only: acknowledged, nothing added
        let f = with_turn_facts(None, "My name is Linh and I live in Hanoi.", eve).unwrap();
        assert!(f.contains("warm thank-you") && f.contains("no tips") && !f.contains("acknowledge"), "{f}");
        // into a greeting's situation line
        let g = with_turn_facts(Some("(Situation: same chat, 3 min later · greet back briefly, e.g. \"Hi! What's next?\")".into()), "morning!", eve).unwrap();
        assert!(g.starts_with("(Situation: same chat, 3 min later") && g.contains("not morning") && g.ends_with(')') && !g.contains(")) ·"), "{g}");
        // nothing to add, or another kind of hint: unchanged
        assert_eq!(with_turn_facts(None, "hey", eve), None);
        assert_eq!(with_turn_facts(Some("x".into()), "what is 2 + 3?", eve).as_deref(), Some("x"));
        // a follow-up hint: the facts line before it, both stripped from views
        let fu = format!("{}why?)", crate::followup::PREFIX);
        let both = with_turn_facts(Some(fu.clone()), "and 2 + 3?", eve).unwrap();
        assert!(both.starts_with(PREFIX) && both.ends_with(&fu) && both.contains("2 + 3 = 5"), "{both}");
        assert_eq!(crate::context::request_body(&format!("{both}\n\nand 2 + 3?")), "and 2 + 3?");
        // a pet's name is said as the pet's, and the user is called by no name
        let pet = with_turn_facts(None, "hi! remember that my dog is Bo", eve).unwrap();
        assert!(pet.contains("The user's dog is called Bo") && pet.contains("call them by no name"), "{pet}");
        let named = with_turn_facts(None, "My name is Linh and my dog is Bo.", eve).unwrap();
        assert!(named.contains("call them Linh") && named.contains("dog is called Bo"), "{named}");
    }

    #[test]
    fn a_greeting_header_loses_only_its_clock_line() {
        let h = "Working directory: /p\nOS: macos\nDate: 2026-11-10\nLocal time now: 2026-11-10 23:43 Tuesday late night (UTC+07:00)\n\nchào bạn";
        assert_eq!(without_clock_line(h), "Working directory: /p\nOS: macos\nDate: 2026-11-10\n\nchào bạn");
        assert_eq!(crate::context::request_body(&without_clock_line(h)), "chào bạn");
        assert_eq!(without_clock_line("hey"), "hey");
        assert_eq!(without_date_line(&without_clock_line(h)), "Working directory: /p\nOS: macos\n\nchào bạn");
        assert_eq!(without_date_line("Date: 2026-11-10\n\nhey"), "hey");
        assert_eq!(without_date_line("hey"), "hey");
        // a chat turn's header: no working directory or OS, and on a greeting no clock line either
        assert_eq!(crate::context::request_body("Date: 2026-11-10\n\nhello"), "hello");
        assert_eq!(crate::context::request_body("Date: 2026-11-10\nLocal time now: x\n\nhello"), "hello");
        assert_eq!(crate::context::request_body("Date: tomorrow, I said\nhello"), "Date: tomorrow, I said\nhello");
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
