use super::*;

const SYSTEM_MD: &str = include_str!("../../prompts/system.md");
pub(super) const COMPACT_MD: &str = include_str!("../../prompts/compact.md");
const PROJECT_CAP: usize = 3000;

fn local_date() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { return "unknown".into(); }
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

/// The local time for the model, from the system clock: `Local time now: 2026-11-09 14:05 Sunday (UTC+07:00,
/// Asia/Ho_Chi_Minh)`. The zone name comes from `TZ`, else the `/etc/localtime` link; left out when neither names one.
/// Added to the first message and to every turn routed to the time group (`route.rs`), so "what time is it in Tokyo"
/// needs no tool.
pub fn local_now() -> String {
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { return "Local time now: unknown".into(); }
        tm
    };
    let off = tm.tm_gmtoff;
    let (sign, a) = if off < 0 { ('-', -off) } else { ('+', off) };
    let zone = std::env::var("TZ").ok().map(|z| z.trim_start_matches(':').to_string()).filter(|z| z.contains('/'))
        .or_else(|| fs::read_link("/etc/localtime").ok().and_then(|p| p.to_str().and_then(|s| s.split_once("zoneinfo/").map(|(_, z)| z.to_string()))));
    let day = DAYS.get(tm.tm_wday as usize).copied().unwrap_or("");
    format!("Local time now: {:04}-{:02}-{:02} {:02}:{:02} {day} (UTC{sign}{:02}:{:02}{})", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
        tm.tm_hour, tm.tm_min, a / 3600, a % 3600 / 60, zone.map(|z| format!(", {z}")).unwrap_or_default())
}

/// The whole system prompt of a tiny window (`tiny_window`): who Elisa is, how to answer, who "I" is.
pub const TINY_SYSTEM: &str = "You are Elisa, a friendly assistant. Answer briefly and naturally. \"I\", \"me\" and \"my\" in the user's messages mean the user.\n";

/// System prompt: prompts/system.md (or override file) + env (cwd, OS, date) + ./ELISA.md + notes.
pub fn system_prompt(cwd: &Path, override_file: Option<&Path>, project: Option<&str>, notes: Option<&str>) -> String {
    let base = override_file.and_then(|p| fs::read_to_string(p).ok()).unwrap_or_else(|| SYSTEM_MD.to_string());
    let mut s = base
        .replace("{cwd}", &cwd.display().to_string())
        .replace("{os}", std::env::consts::OS)
        .replace("{date}", &local_date());
    s.truncate(s.trim_end().len());
    if let Some(p) = project.map(str::trim).filter(|p| !p.is_empty()) {
        s.push_str("\n\n## Project instructions (ELISA.md)\n");
        s.push_str(&capped(p, PROJECT_CAP));
    }
    if let Some(n) = notes.map(str::trim).filter(|n| !n.is_empty()) {
        s.push_str("\n\n## Notes from this conversation\n");
        s.push_str(&capped(n, NOTES_CAP));
    }
    s.push('\n');
    s
}

/// First user message of a history: cwd, OS, date and notes go here, not in the system prompt, so the system turn
/// (system + tools) is byte-identical across projects, days and sessions and the gateway restores one stable snapshot.
pub fn first_message(cwd: &Path, notes: Option<&str>, input: &str) -> String { first_message_with(cwd, notes, None, input) }

/// Heading of the notes block in a first message: says who wrote them (otherwise, after a reset, the model can
/// treat invented note lines as things the user had said). They are this conversation's own notes (a continued
/// session's notes are its own: the old "from earlier sessions" heading went with notes of another task).
pub const NOTES_HEADING: &str = "## Notes from this conversation (written by Elisa, not the user; may be wrong: check files before relying on them)";
/// The heading when the notes are background from before /clear or /reset (`background_notes`)
pub const NOTES_BG_HEADING: &str = "## Notes from an earlier conversation (written by Elisa, not the user; may be wrong: check files before relying on them)";

/// `first_message` plus the latest state card as `[state]\n…\n[/state]` (after the notes, before the request).
pub fn first_message_with(cwd: &Path, notes: Option<&str>, state: Option<&str>, input: &str) -> String {
    first_message_full(cwd, notes, state, &[], input)
}

/// `first_message_with` plus recalled memories, as their own block before `## Request` (never inside it: the
/// request section is the user's words only).
pub fn first_message_full(cwd: &Path, notes: Option<&str>, state: Option<&str>, memory: &[String], input: &str) -> String {
    let s = format!("Working directory: {}\nOS: {}\nDate: {}\n{}\n\n", cwd.display(), std::env::consts::OS, local_date(), local_now());
    s + &first_message_bare(notes, state, memory, input)
}

/// A first message without the Working directory / OS / Date / clock header: notes, `[state]`, recalled memories and
/// the request. A tiny window's first message (`tiny_window`; a small model
/// there reads the header back as its answer).
pub fn first_message_bare(notes: Option<&str>, state: Option<&str>, memory: &[String], input: &str) -> String {
    let mut s = String::new();
    let notes = notes.map(str::trim).filter(|n| !n.is_empty());
    let state = state.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = notes {
        let h = if n.starts_with(NOTES_BACKGROUND) { NOTES_BG_HEADING } else { NOTES_HEADING };
        s += &format!("{h}\n{}\n\n", capped(n, NOTES_CAP));
    }
    if let Some(c) = state { s += &format!("[state]\n{c}\n[/state]\n\n"); }
    if !memory.is_empty() { s += &with_memory(memory, ""); }
    if notes.is_some() || state.is_some() || !memory.is_empty() { s += "## Request\n"; }
    s + input
}

/// A reply that writes a tool call as plain text instead of calling the tool (e.g. after a reset:
/// `<tool><tool_name>read_file</tool_name><path>…</path></tool>`, so nothing ran). Markup only, any language:
/// tool-call-like XML tags (`<tool>`, `<tool_call>`, `<tool_name>`, `<function=…>`, `<invoke …>`, `<parameter=…>`)
/// or a JSON `{"name": …, "arguments"|"input"|"parameters": …}` object, outside fenced code blocks.
pub fn looks_like_text_tool_call(text: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(concat!(
        r"<(?:tool|tool_call|tool_use|tool_name|function_call|invoke)(?:\s[^>\n]*)?>",
        r"|<(?:function|parameter)=[^>\n]*>",
        r#"|\{\s*"(?:name|tool|tool_name)"\s*:\s*"[^"\n]+"\s*,\s*"(?:arguments|input|parameters|args)"\s*:"#,
    )).unwrap());
    let mut fenced = false;
    let mut outside = String::new();
    for l in text.lines() {
        if l.trim_start().starts_with("```") { fenced = !fenced; continue; }
        if !fenced {
            // inline `code` spans are quotes, not calls
            for (i, part) in l.split('`').enumerate() { if i % 2 == 0 { outside.push_str(part); } }
            outside.push('\n');
        }
    }
    re.is_match(&outside)
}

/// Prefix of the user message Elisa sends itself (a correction after a text tool call): never the user's request.
pub const NUDGE_PREFIX: &str = "[elisa] ";

/// The one-time correction after a reply wrote a tool call as text.
pub fn tool_text_nudge(tool_names: &[&str]) -> String {
    format!("{NUDGE_PREFIX}Your last reply wrote a tool call as plain text, so nothing ran. Call the tool through the \
        tool-calling interface instead (tools: {}), or answer without tools.", tool_names.join(", "))
}

/// The one-time correction after a tool call was cut off by the output limit.
pub fn cut_call_nudge(calls: &str, limit: usize) -> String {
    format!("{NUDGE_PREFIX}Your last reply hit the {limit}-token output limit in the middle of `{calls}`, so no tool in it \
        ran and nothing was saved. Do not repeat it at the same size: write a short first part with write, then add \
        the rest in further edit calls, each well under the limit.")
}

/// Whether a recalled hit goes into the request. `lex`: elisa-mem's keyword/graph score without the vector part (a hit
/// only an embedding found never qualifies: it can attach an unrelated answer to small talk);
/// `support`: the idf-weighted share of the query's terms the hit has (common words weigh little, so a query made of
/// them alone finds nothing that covers it). Mid-session (the window already holds the conversation) the keyword bar
/// doubles: lex >= 2x min_score. Calibrated on a copy of the user's store (no vectors): relevant hits had
/// support 0.45..1.0, the off-topic ones 0.09..0.42.
pub fn recall_passes(lex: f32, support: f32, mid_session: bool, min_score: f32, min_support: f32) -> bool {
    let l = if mid_session { 2.0 * min_score } else { min_score };
    lex >= l && support >= min_support
}

/// A remembered exchange that recorded a lookup (the `[read notes.txt]` / `[web_search “…”]` tool line memory keeps
/// after a reply) is an old copy of what a lookup turn should fetch now: on a turn that carries the file tools
/// (`code`) a hit with a file lookup is skipped, on one that carries the web tools (`web`) a hit with a web lookup.
/// Otherwise "read notes.txt and tell me the meeting code" can recall an earlier answer that read the file, and
/// the model answers from it with no read (the file could have
/// changed; the user asked for a read and saw none); "search the web: …" the same with an old search answer.
pub fn stale_lookup(text: &str, code: bool, web: bool) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:^|\s)\[(read|read_symbol|outline|glob|grep|web_search|web_fetch) [^\]\n]+\]").unwrap());
    re.captures_iter(text).any(|c| if c[1].starts_with("web_") { web } else { code })
}

/// What memory must not keep as an answer, nor bring back: a refusal or "I don't know" ("I can't run
/// scripts directly…", "I don't have access…"), the "Okay, I didn't do that" of a denied tool, a cancelled / denied /
/// interrupted / unknown tool, an error placeholder (`[cancelled]`, `[no output]`), or leaked card text.
pub fn unfit_memory(text: &str) -> bool {
    static REFUSAL: OnceLock<regex::Regex> = OnceLock::new();
    let re = REFUSAL.get_or_init(|| regex::Regex::new(concat!(
        r"(?i)^(?:(?:sorry|i'm sorry|i am sorry|unfortunately|okay|ok)[,.!]?\s*(?:but\s*)?)?",
        r"(?:i\s+(?:can't|cannot|can not|am unable|'m unable|am not able|'m not able|won't be able|didn't do that|did not do that",
        r"|don't (?:have|know) (?:access|the ability|the tools?|any (?:way|access|information)|your|that)",
        r"|do not (?:have|know) (?:access|the ability|the tools?|your))|as an ai\b)")).unwrap());
    let t = text.trim().replace('’', "'");
    let first = t.lines().next().unwrap_or("");
    re.is_match(first) || first.starts_with("[cancelled]") || first.starts_with("[no output]") || first.starts_with("[max_tokens]")
        || t.lines().any(failed_tool_line) || t.contains("[state]") || t.contains("<state>")
}

/// Elisa's own answer, with no model call, on a model without vision when no picture is in the conversation (a small
/// model asked about a refused picture makes up an answer, and a note in the request does not stop it): when the message came with the refused picture (`refused_now`), names a
/// picture ("the photo", "this image"), or follows this same answer (`after_line`) and says "it" / "this" / "they".
pub fn no_picture_line(input: &str, model: &str, vi: bool, refused_now: bool, after_line: bool) -> Option<String> {
    (refused_now || picture_words(input, after_line)).then(|| no_picture_text(model, vi))
}

/// The word check of `no_picture_line` (used when the hosted intent router gives no answer; its `refused_picture`
/// decides otherwise, `repl.rs`).
pub fn picture_words(input: &str, after_line: bool) -> bool {
    static PIC: OnceLock<regex::Regex> = OnceLock::new();
    static PRON: OnceLock<regex::Regex> = OnceLock::new();
    let pic = PIC.get_or_init(|| regex::Regex::new(r"(?i)\b(?:the|this|that|my|your|its|in|attached)\s+(?:picture|pic|image|photo|photograph|screenshot|drawing|selfie)s?\b|\b(?:ảnh|hình)\s+(?:này|đó|kia)\b").unwrap());
    let pron = PRON.get_or_init(|| regex::Regex::new(r"(?i)\b(?:it|its|it's|this|that|they|them|their|these|those|he|she|his|her|nó)\b").unwrap());
    pic.is_match(input) || (after_line && pron.is_match(&input.replace('’', "'")))
}

/// Elisa's no-picture answer for `model`, English or Vietnamese.
pub fn no_picture_text(model: &str, vi: bool) -> String {
    if vi { format!("Mình không xem được ảnh với {model}, nên không có ảnh nào đến được mình và mình không biết ảnh có gì. Hãy chọn một mô hình có vision để hỏi về ảnh.") }
    else { format!("I can't see pictures with {model}, so no picture reached me and I can't tell what it shows. Pick a model with vision to ask about images.") }
}

/// Whether a remembered text clearly matches the question (so unrelated old prompts don't come back): it shares at
/// least 2 of the question's content words and half of them, or the question's only content word ("what's my name?"
/// finds "My name is Linh").
pub fn strong_recall(query: &str, hit: &str) -> bool {
    let q = crate::topics::words(query);
    let shared = q.intersection(&crate::topics::words(hit)).count();
    (shared >= 2 && 2 * shared >= q.len()) || (q.len() == 1 && shared == 1)
}

/// The summary instructions (prompts/compact.md), sent as one more user message for an in-context summary.
pub fn compact_instructions() -> &'static str { COMPACT_MD.trim_end() }

/// Words that make a message small talk, not a task (soak: "hi Elisa! how are you today?" became the session goal).
const SMALL_TALK: &[&str] = &[
    "hi", "hello", "hey", "heya", "hiya", "yo", "howdy", "greetings", "good", "morning", "afternoon", "evening", "day",
    "night", "how", "are", "is", "it", "going", "you", "doing", "today", "elisa", "there", "again", "thanks", "thank",
    "cheers", "ok", "okay", "great", "nice", "cool", "awesome", "bye", "goodbye", "see", "later", "what's", "whats", "up",
    "sup", "all", "fine", "well", "and", "u", "job", "welcome", "please", "lol", "haha", "yes", "no", "sure", "i'm", "im", "so", "much", "very", "lot",
];
/// leading greeting words a request may start with ("Hi! Can you …", "hello again :) tell me …")
const GREETING: &[&str] = &["hi", "hello", "hey", "heya", "hiya", "yo", "howdy", "greetings", "good", "morning", "afternoon", "evening", "elisa", "there", "again"];

/// The request without a leading greeting ("Hi! Can you remind me…" → "Can you remind me…").
pub(super) fn without_greeting(input: &str) -> &str {
    let mut rest = input.trim_start();
    loop {
        let w: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '\'').collect();
        if w.is_empty() || !GREETING.contains(&w.to_lowercase().as_str()) { break; }
        rest = rest[w.len()..].trim_start_matches(|c: char| c.is_whitespace() || ",.!:;-–—)(:".contains(c) || !c.is_ascii() && !c.is_alphanumeric());
    }
    rest
}

/// Small talk: a greeting, thanks, "how are you", an acknowledgement — nothing to work on.
pub fn is_small_talk(input: &str) -> bool {
    let w: Vec<String> = input.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|x| !x.is_empty()).map(str::to_lowercase).collect();
    !w.is_empty() && w.len() <= 12 && w.iter().all(|x| SMALL_TALK.contains(&x.as_str()))
}
