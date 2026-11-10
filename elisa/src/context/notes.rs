use super::*;

pub(super) const NOTES_CAP: usize = 4000;

/// A turn's tool line (`[bash x → exit 1]`, `[web_fetch u → cancelled by user]`) for a call that did not succeed:
/// denied, cancelled, interrupted, an unknown tool, an error, or a command with a non-zero exit.
pub fn failed_tool_line(line: &str) -> bool {
    let l = line.trim();
    l.starts_with('[') && l.ends_with(']') && l.contains(" → ") && !l.ends_with(" → exit 0]")
}

/// Whether a remembered answer records a tool that ran (`[read notes.txt]`, `[bash ls → exit 0]`): memory keeps an
/// answer only from such a turn (`repl` `turn`); an older store's plain-chat answers ("I'll check how many lines
/// fizz.py prints") are not recalled either.
pub fn has_tool_line(text: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^\[[a-z_]+ [^\n]*\]$").unwrap());
    re.is_match(text)
}

/// Summary notes as they are stored: the summarizer's instructions and template text never are (a small model can
/// copy the summary instructions in as the user's goal and keep the template's own lines, such as
/// "Finished steps and decisions, with their reasons." and "Also known as: name = other words; …"). Drops lines that
/// repeat (part of) prompts/compact.md, placeholder values, template items of the `Also known as:` line, and
/// headings left with nothing under them.
/// A transcript's tool line: `[tool NAME …]`, `[result: …]`, `[result …]`, `[error: …]` of a tool.
fn raw_tool_line(body: &str) -> bool {
    let b = body.trim_start_matches(['-', '*', ' ']).to_ascii_lowercase();
    b.starts_with("[tool ") || b.starts_with("[tool:") || b.starts_with("[result:") || b.starts_with("[result ") || b.starts_with("[tool_result")
}

pub fn clean_notes(notes: &str) -> String {
    let instr = norm_words(COMPACT_MD);
    // whole instruction sentences / clauses: a line quoting one is an echo even with real words around it
    let frags: Vec<String> = COMPACT_MD.split(['.', '\n', ':', '(', ')']).map(norm_words).filter(|f| f.len() >= 24).collect();
    let echo = |t: &str| {
        let n = norm_words(t);
        (n.len() >= 12 && instr.contains(&n)) || frags.iter().any(|f| n.contains(f.as_str()))
    };
    // the template's own headings; another heading that repeats the instructions ("## Last line: `Also known as:`")
    // goes with its lines (names listed under it read as one group: the user's name next to Elisa's)
    let headings: HashSet<String> = COMPACT_MD.lines().filter(|l| l.starts_with('#')).map(norm_words).collect();
    let mut skip = false;
    let mut lines: Vec<String> = Vec::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') {
            skip = echo(t) && !headings.contains(&norm_words(t));
            if !skip && !t.trim_start_matches('#').trim().is_empty() { lines.push(t.to_string()); }
            continue;
        }
        if skip { continue; }
        if t.is_empty() { lines.push(String::new()); continue; }
        let body = t.trim_start_matches(['-', '*', '`', ' ']).trim_end_matches('`');
        if body.to_ascii_lowercase().starts_with("also known as:") {
            let items: Vec<&str> = body["also known as:".len()..].split(';').map(str::trim)
                .filter(|x| !is_placeholder(x) && norm_words(x) != "name other words" && real_alias(x)).collect();
            if !items.is_empty() { lines.push(format!("Also known as: {}", items.join("; "))); }
            continue;
        }
        if is_placeholder(body) || is_placeholder(body.trim_matches(['[', ']', '(', ')'])) || body.trim_matches(['[', ']', '(', ')']).eq_ignore_ascii_case("none") || echo(t) { continue; }
        // a raw tool call or result copied from a transcript is no note (lines like "[tool glob …]" would sit under
        // "## Files touched" and be carried forward)
        if raw_tool_line(body) { continue; }
        lines.push(l.trim_end().to_string());
    }
    // headings with nothing under them go; blank runs become one blank line
    let mut out: Vec<String> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with('#') {
            let has_body = lines[i + 1..].iter().take_while(|x| !x.starts_with('#')).any(|x| !x.trim().is_empty());
            if !has_body { continue; }
        }
        if l.is_empty() && out.last().is_none_or(|p: &String| p.is_empty()) { continue; }
        out.push(l.clone());
    }
    out.join("\n").trim().to_string()
}

/// names an `Also known as:` item may not have: the parties of every conversation, the OS, dates (e.g.
/// `Also known as: user, Elisa, system, macos, 2026-11-08, <path>`)
const TRIVIAL_ALIAS: &[&str] = &["user", "the user", "elisa", "system", "assistant", "macos", "os", "linux", "model", "project", "the project", "today", "date", "me", "you", "i"];

/// An alias worth keeping: `name = other words`, both sides real, the name not one of TRIVIAL_ALIAS, a date or a path.
fn real_alias(item: &str) -> bool {
    let Some((name, other)) = item.split_once('=') else { return false };
    let (n, o) = (norm_words(name), norm_words(other));
    !n.is_empty() && !o.is_empty() && n != o && !TRIVIAL_ALIAS.contains(&n.as_str()) && !name.contains('/')
        && !n.chars().all(|c| c.is_ascii_digit() || c == ' ')
}

/// `notes` cut to whole lines within `max_tokens` (the top first: the goal and done sections lead).
pub fn cap_notes(notes: &str, max_tokens: usize) -> String {
    let mut out = String::new();
    for l in notes.lines() {
        if estimate_tokens(&out) + estimate_tokens(l) + 1 > max_tokens { break; }
        out.push_str(l);
        out.push('\n');
    }
    // a heading cut off from its body is noise
    let mut s = out.trim_end().to_string();
    while let Some(last) = s.lines().last() {
        if !last.starts_with('#') { break; }
        s = s[..s.len() - last.len()].trim_end().to_string();
    }
    s
}

/// The line in front of the notes after `/clear` or `/reset` (a new session carries no earlier notes at all): they
/// are background, not the task (otherwise an old goal carried into a new
/// session sends a small model on work the user never asked for).
pub const NOTES_BACKGROUND: &str = "(Background from an earlier session; the user has not asked to continue it. Do not pursue its goal or next step unless the request asks.)";

/// Notes as the first message carries them after `/clear` or `/reset` (a new session carries none: repl starts it
/// with no notes): the background line first, and without the `## Session goal` section, so the next request sets
/// the goal from what the user asks.
pub fn background_notes(notes: &str) -> String {
    let notes = notes.trim();
    let rest = match notes.find(GOAL_NOTES) {
        Some(i) => {
            let after = &notes[i + GOAL_NOTES.len()..];
            let end = after.find("\n## ").map_or(after.len(), |j| j + 1);
            format!("{}{}", &notes[..i], &after[end..]).trim().to_string()
        }
        None => notes.to_string(),
    };
    if rest.is_empty() { String::new() } else { format!("{NOTES_BACKGROUND}\n{rest}") }
}

/// An explicit goal statement in a request: the words after `our goal for this session:`, `the goal is`,
/// `new goal:`, `goal:` (one line, <= 240 chars). None for a question about the goal ("what was the goal?").
pub fn goal_statement(input: &str) -> Option<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(
        r"(?i)\b(?:(?:our|my|the|new|session)\s+)?(?:new\s+)?goal(?:\s+(?:for|of)\s+(?:this|the)\s+session)?(?:\s+(?:is\s+now|now\s+is|is|will\s+be))?\s*(?::|=|-|–|\bis\b)?\s+").unwrap());
    let body = request_body(input);
    for m in re.find_iter(body) {
        let marker = m.as_str();
        // a statement: `goal:` / `goal is` / `goal for this session:` (not "what was the goal of this session?")
        let stated = marker.contains(':') || marker.contains('=') || marker.to_lowercase().split_whitespace().any(|w| w == "is" || w == "be");
        if !stated { continue; }
        let rest = body[m.end()..].trim();
        let rest = rest.split("\n\n").next().unwrap_or("").trim();
        if rest.is_empty() || rest.starts_with('?') || rest.ends_with('?') || rest.split_whitespace().count() < 2 { continue; }
        return Some(one_line(rest, 240));
    }
    None
}

/// The session goal after `input`: the first real request sets it (its goal statement, else the request); later
/// requests change it only with an explicit goal statement (otherwise the card / notes goal drifts to the newest
/// request, and "what was the goal of this session?" gets the last request).
/// A pasted blob gives no goal (only the user's own words around it do; `own_words`).
pub fn next_goal(current: Option<&str>, input: &str) -> Option<String> {
    let input = request_body(input).trim();
    if input.is_empty() || input.starts_with('/') || input.starts_with(NUDGE_PREFIX.trim_end()) { return current.map(String::from); }
    let Some(input) = own_words(input) else { return current.map(String::from) };
    match (current, goal_statement(input)) {
        (_, Some(g)) => Some(g),
        (Some(c), None) => Some(c.to_string()),
        (None, None) => {
            // small talk and a plain question set no goal (otherwise "hi Elisa! how are you today?" becomes the session
            // goal); a greeting in front of a request is not part of it
            let req = without_greeting(input).trim();
            let question = req.ends_with('?') && crate::mcp_reason::wanted(req) == Some(false);
            // telling facts about themself ("My name is Linh, I live in Hanoi") is no goal: the facts go to the notes'
            // facts in the third person (`user_facts`)
            if req.is_empty() || small_talk_goal(req) || question || only_user_facts(req) { None } else { Some(one_line(req, 240)) }
        }
    }
}

/// A request longer than this (chars) or than GOAL_PASTE_LINES lines carries pasted text.
const GOAL_PASTE_CHARS: usize = 600;
const GOAL_PASTE_LINES: usize = 8;
/// the longest line / paragraph around a paste that still counts as the user's own words
const GOAL_OWN_CHARS: usize = 300;

/// The part of a request that is the user's own words, for the session goal. A short request is all of it. A long
/// one (a paste) gives its first or last paragraph, else its first or last line, when that is short and not repeated
/// inside the paste: the user's line around what they pasted ("count the words in this:"). None when the request is
/// only pasted text (otherwise the start of a long paste becomes the session goal and a small model keeps
/// steering back to it, also after /clear).
pub fn own_words(input: &str) -> Option<&str> {
    let input = input.trim();
    if input.chars().count() <= GOAL_PASTE_CHARS && input.lines().count() <= GOAL_PASTE_LINES { return Some(input); }
    let paras: Vec<&str> = input.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
    let lines: Vec<&str> = input.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let cands: [Option<&str>; 4] = [paras.first().copied(), paras.last().copied(), lines.first().copied(), lines.last().copied()];
    cands.into_iter().flatten().find(|c| c.chars().count() <= GOAL_OWN_CHARS && c.split_whitespace().count() >= 2
        && input.matches(*c).count() == 1 && !is_small_talk(c))
}

/// Content words (4+ letters, not a common word) of `s`, for telling whether a request is about the session goal.
pub(super) fn content_words(s: &str) -> HashSet<String> {
    const COMMON: &[&str] = &["about", "after", "again", "also", "please", "could", "would", "should", "there", "their", "these", "those",
        "this", "that", "with", "from", "into", "what", "when", "where", "which", "while", "make", "write", "show", "tell", "give",
        "need", "want", "just", "like", "some", "then", "than", "have", "does", "your", "them", "they", "will", "each", "more", "only"];
    norm_words(s).split(' ').filter(|w| w.chars().count() >= 4 && !COMMON.contains(w)).map(|w| w.trim_end_matches('s').to_string()).collect()
}

/// Whether a request moves away from the session goal: Some(true) = a real request (3+ content words, no question
/// mark in the user's own words) sharing none of the goal's content words; Some(false) = it shares some; None = too short or a question
/// to tell (a follow-up like "now add tests" or "what was the goal?"). The session re-derives its goal after
/// GOAL_DRIFT unrelated requests in a row (`repl`).
pub fn goal_drift(goal: &str, input: &str) -> Option<bool> {
    let body = request_body(input).trim();
    if body.starts_with('/') { return None; }
    let own = own_words(body)?;
    if own.contains('?') { return None; }
    let w = content_words(own);
    if w.len() < 3 { return None; }
    let g = content_words(goal);
    Some(w.is_disjoint(&g))
}

/// unrelated requests in a row after which the session goal is taken from the newest request (`goal_drift`)
pub const GOAL_DRIFT: u8 = 3;

/// Heading of the notes section Elisa writes for the session goal (kept on top, replaced on every save).
pub const GOAL_NOTES: &str = "## Session goal (set from the user's first request or a stated new goal)";

/// `notes` with the session goal section first (an older one replaced).
pub fn with_session_goal(notes: &str, goal: &str) -> String {
    let notes = notes.trim();
    let rest = match notes.find(GOAL_NOTES) {
        Some(i) => {
            let after = &notes[i + GOAL_NOTES.len()..];
            let end = after.find("\n## ").map_or(after.len(), |j| j + 1);
            format!("{}{}", &notes[..i], &after[end..]).trim().to_string()
        }
        None => notes.to_string(),
    };
    // a small-talk goal is no goal: the section goes (otherwise a greeting like "hey" is stored and carried forward)
    if small_talk_goal(goal) { return rest; }
    let sec = format!("{GOAL_NOTES}\n{}", users_words(&one_line(goal, 240)));
    if rest.is_empty() { sec } else { format!("{sec}\n\n{rest}") }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let arg = ["path", "command", "pattern"].iter().find_map(|k| input.get(*k).and_then(Value::as_str))
        .or_else(|| input.as_object().and_then(|o| o.values().find_map(Value::as_str)));
    match arg {
        Some(a) => format!("{name} {}", one_line(a, 100)),
        None => name.to_string(),
    }
}

/// the assistant's own name, as the system prompt gives it
pub(super) const ASSISTANT: &str = "Elisa";
/// the speaker labels of a summary request's transcript (`transcript`)
const USER_LABEL: &str = "User: ";
const ELISA_LABEL: &str = "Elisa: ";

/// A summary's notes (`clean_notes`) without the lines that copy the transcript's dialogue: a line led by one of
/// its speaker labels (`USER_LABEL`, `ELISA_LABEL`), such as `Elisa: “What's your name and your meeting code?”` (the
/// user's question, labelled as Elisa's) or `Elisa: Assistant, Goal: …`. Out of their turns they no longer say who
/// is who, and a small model reading them back took the user for Elisa.
pub fn summary_notes(raw: &str) -> String {
    let labels = [USER_LABEL, ELISA_LABEL].map(|l| l.trim_end().to_lowercase());
    let kept: Vec<&str> = raw.lines().filter(|l| {
        // (after any bullet a model writes: "-", "*", "–", "•", a quote mark)
        let body = l.trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let body = body.replace("**", "");
        !labels.iter().any(|lab| body.starts_with(lab.as_str()))
    }).collect();
    clean_notes(&kept.join("\n"))
}

fn transcript(msgs: &[Message]) -> String {
    let mut out = String::new();
    for m in msgs {
        for b in &m.content {
            match b {
                Block::Text { text } if !text.trim().is_empty() => {
                    // the user's own words only: the first message's header (cwd, old notes, [state]) and recalled
                    // [memory] were written by Elisa; shown as "User:" they became facts about the user in the notes
                    let t = if m.role == Role::User { request_body(text).trim() } else { text.trim() };
                    // an attached document (`attach.rs`): its name line only, never its text, reaches the notes
                    let t = if m.role == Role::User && crate::attach::is_block(t) { crate::attach::first_line(t) } else { t };
                    if t.is_empty() { continue; }
                    if m.role == Role::User && t.starts_with(NUDGE_PREFIX.trim_end()) { continue; }
                    out.push_str(if m.role == Role::User { USER_LABEL } else { ELISA_LABEL });
                    out.push_str(t);
                }
                Block::ToolUse { name, input, .. } => out.push_str(&format!("[tool {}]", tool_summary(name, input))),
                Block::ToolResult { content, is_error, .. } => {
                    let c = content.trim();
                    let c = if c.chars().count() > 400 { format!("{}…", head(c, 400)) } else { c.to_string() };
                    out.push_str(&format!("[{}: {c}]", if *is_error { "error" } else { "result" }));
                }
                _ => continue,
            }
            out.push('\n');
        }
    }
    out
}

/// One user message asking for a summary of `msgs`, rendered as a plain transcript whose middle is
/// cut so the whole request stays <= budget_tokens.
/// Notes for folded turns without a model call, when the summary request failed (a small reasoning model can reason
/// past its whole budget on it): the old notes, then one shortened line per user request and reply, the newest kept
/// when it is too long.
pub fn digest(msgs: &[Message], old_notes: Option<&str>, max_chars: usize) -> String {
    let lines: Vec<String> = transcript(msgs).lines()
        .filter(|l| l.starts_with(USER_LABEL) || l.starts_with(ELISA_LABEL))
        .map(|l| if l.chars().count() > 160 { format!("{}…", head(l, 160)) } else { l.to_string() })
        .collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for l in lines.iter().rev() {
        if used + l.len() + 3 > max_chars { break; }
        used += l.len() + 3;
        kept.push(l);
    }
    kept.reverse();
    let mut out = old_notes.map(str::trim).filter(|n| !n.is_empty()).map(|n| format!("{}\n\n", capped(n, NOTES_CAP))).unwrap_or_default();
    out.push_str("Earlier turns (shortened, no summary):\n");
    for l in kept { out.push_str("- "); out.push_str(l); out.push('\n'); }
    out.trim_end().to_string()
}

pub fn compact_request(msgs: &[Message], old_notes: Option<&str>, budget_tokens: usize) -> Message {
    let notes = old_notes.map(str::trim).filter(|n| !n.is_empty()).map(|n| capped(n, NOTES_CAP));
    let mut out = String::new();
    if let Some(n) = &notes {
        out.push_str("Earlier notes:\n");
        out.push_str(n);
        out.push_str("\n\n");
    }
    let fixed = estimate_tokens(&out) + estimate_tokens(COMPACT_MD) + MSG_OVERHEAD + 32;
    let room = budget_tokens.saturating_sub(fixed);
    let t = transcript(msgs);
    out.push_str("Transcript:\n");
    if estimate_tokens(&t) <= room {
        out.push_str(&t);
    } else {
        let room_chars = chars_for_tokens(room);
        let first = head(&t, room_chars * 3 / 10);
        let last = tail(&t, room_chars - room_chars * 3 / 10);
        out.push_str(first);
        out.push_str("\n[... earlier part cut ...]\n");
        out.push_str(last);
    }
    out.push_str("\n\n");
    out.push_str(COMPACT_MD.trim_end());
    Message::user_text(out)
}

/// `line` split after `. ` / `! ` / `? ` (a version like `0.3.2` or a path like `core.py` has no space after its dot)
pub(super) fn sentences(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && it.peek().is_some_and(|n| n.is_whitespace()) {
            out.push(std::mem::take(&mut cur).trim().to_string());
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    // a bullet marker alone is no sentence
    out.retain(|x| !x.is_empty());
    out
}

/// notes as (heading line, body lines); the lines before the first heading have heading ""
pub(super) fn sections(notes: &str) -> Vec<(String, Vec<String>)> {
    let mut v: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    for l in notes.lines() {
        if l.trim_start().starts_with("## ") { v.push((l.trim().to_string(), Vec::new())); }
        else if let Some(s) = v.last_mut() { s.1.push(l.trim_end().to_string()); }
    }
    for s in &mut v {
        while s.1.last().is_some_and(|l| l.trim().is_empty()) { s.1.pop(); }
        while s.1.first().is_some_and(|l| l.trim().is_empty()) { s.1.remove(0); }
    }
    v.retain(|s| !s.0.is_empty() || !s.1.is_empty());
    v
}

/// a heading's first two words, lowercased: "## Current state / next step" → "current state"
pub(super) fn heading_key(h: &str) -> String { norm_words(h).split(' ').take(2).collect::<Vec<_>>().join(" ") }

/// sections whose newest version replaces the older one (they describe "now", not history)
fn replaces(key: &str) -> bool { ["goal", "current state", "session goal", "from state"].iter().any(|k| key.starts_with(k)) }

/// Keep-first rank of a section when the notes must shrink (lower = kept longer): Elisa's file records, decisions
/// and open tasks first, the model's own file list and the alias line last.
fn rank(key: &str) -> u8 {
    const ORDER: &[&str] = &["session goal", "files changed", "done", "current state", "facts to", "from state", "goal", "files touched"];
    ORDER.iter().position(|k| key.starts_with(k)).map_or(ORDER.len() as u8, |i| i as u8)
}

/// the words of a bullet line, for duplicate checks (`- `, `* `, numbering removed)
pub(super) fn bullet_key(l: &str) -> String {
    let t = l.trim().trim_start_matches(['-', '*', '•']).trim();
    let d = t.chars().take_while(char::is_ascii_digit).count();
    let t = if d > 0 && t[d..].starts_with(['.', ')']) { &t[d + 1..] } else { t };
    norm_words(t)
}

/// `lines` without duplicates: the same words again, or a line whose words another line contains (>= 3 words: the
/// longer one stays, where the first one stood): a Done list can otherwise repeat the same bullets several times.
fn dedupe(lines: &[String]) -> Vec<String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in lines {
        let k = bullet_key(l);
        if k.is_empty() { if !l.trim().is_empty() { out.push((l.clone(), k)); } continue; }
        if let Some(i) = out.iter().position(|(_, o)| !o.is_empty() && (*o == k || (o.split(' ').count() >= 3 && k.contains(o.as_str())) || (k.split(' ').count() >= 3 && o.contains(k.as_str())))) {
            if k.len() > out[i].1.len() { out[i] = (l.clone(), k); }
            continue;
        }
        out.push((l.clone(), k));
    }
    out.into_iter().map(|(l, _)| l).collect()
}

/// the `Also known as:` items of `lines` (taken out of them)
fn take_aliases(lines: &mut Vec<String>) -> Vec<String> {
    let mut items = Vec::new();
    lines.retain(|l| {
        let b = l.trim().trim_start_matches(['-', '*', '`', ' ']).trim_end_matches('`');
        match b.to_ascii_lowercase().starts_with("also known as:") {
            true => { items.extend(b["also known as:".len()..].split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty())); false }
            false => true,
        }
    });
    items
}

/// Notes as they are kept: duplicate bullets dropped in every section, one `Also known as:` line at the end (merged,
/// trivial items dropped), sections in a fixed order (session goal, Elisa's file records, done, current state, facts, …).
pub fn tidy_notes(notes: &str) -> String {
    let mut secs = sections(&clean_notes(notes));
    let mut aliases: Vec<String> = Vec::new();
    for s in &mut secs {
        aliases.extend(take_aliases(&mut s.1));
        s.1 = dedupe(&s.1);
    }
    // a session goal that is only small talk is no goal (otherwise "## Session goal\nhey" is carried forward)
    for s in &mut secs {
        if heading_key(&s.0).starts_with("session goal") { s.1.retain(|l| !small_talk_goal(l)); }
    }
    secs.retain(|s| s.1.iter().any(|l| !l.trim().is_empty()));
    secs.sort_by_key(|s| if s.0.is_empty() { 254 } else { rank(&heading_key(&s.0)) });
    let mut out: Vec<String> = Vec::new();
    for (h, body) in &secs {
        if !h.is_empty() { out.push(h.clone()); }
        out.extend(body.iter().cloned());
        out.push(String::new());
    }
    let mut seen: Vec<String> = Vec::new();
    aliases.retain(|a| { let k = norm_words(a); let new = !seen.contains(&k); seen.push(k); new });
    if !aliases.is_empty() { out.push(format!("Also known as: {}", aliases.join("; "))); }
    // clean_notes drops trivial alias items and squeezes blank lines
    clean_notes(&out.join("\n"))
}

/// A session goal that is only small talk ("hey", "hi", "thanks", "ok"): never stored as the goal.
pub fn small_talk_goal(goal: &str) -> bool {
    let g = goal.trim();
    let rest = without_greeting(g).trim();
    // a thanks / acknowledgement in a few words ("thanks a lot", "ok got it"); not "great, now summarise the paper"
    let short = g.split_whitespace().count() <= 3;
    rest.is_empty() || is_small_talk(g) || is_small_talk(rest) || (short && (crate::route::closing(g) || crate::route::ack(g) || crate::route::small_talk(g)))
}

/// `new` notes merged into `old` ones, no model call: a section that describes "now" (goal, current state, the state
/// card section) is replaced by its newer version; every other section keeps the old lines and adds the new ones
/// (duplicates dropped); Elisa's own file records merge by path (the newer line for a path wins). Sections only the
/// old notes have stay, so /clear never replaces the notes with a summary of one turn and loses the earlier facts.
pub fn merge_notes(old: Option<&str>, new: &str) -> String { merge_notes_kept(old, new, &|_| true) }

/// `merge_notes`, where an old `Facts to remember`, `Session goal` or `Done` line stays only when `keep` says so (a
/// line of the new notes always does). The fold and /clear keep an old fact only when the conversation being summarised talks
/// about it, or it was pinned (`fact_in_talk`): otherwise an unrelated old task's facts, read from a stale notes
/// file, are carried forward by every fold of a conversation that never mentions them.
pub fn merge_notes_kept(old: Option<&str>, new: &str, keep: &dyn Fn(&str) -> bool) -> String {
    let old = old.map(str::trim).unwrap_or("");
    if old.is_empty() { return tidy_notes(new); }
    let mut secs = sections(old);
    let filtered = |key: &str| key.starts_with("facts") || key.starts_with("session goal") || key.starts_with("done");
    let mut dropped: Vec<String> = Vec::new();
    for (h, body) in secs.iter_mut() {
        if !filtered(&heading_key(h)) { continue; }
        body.retain(|l| {
            let t = l.trim();
            let k = t.is_empty() || t.to_ascii_lowercase().trim_start_matches(['-', '*', '`', ' ']).starts_with("also known as:") || keep(t);
            if !k { dropped.push(t.to_string()); }
            k
        });
    }
    // a new summary restating a dropped line (it read the old notes in the conversation's first message): that
    // sentence goes too, unless the conversation is about it
    let mut new_secs = sections(new);
    if !dropped.is_empty() {
        let gone = dropped.join("\n");
        for (h, body) in new_secs.iter_mut() {
            if !filtered(&heading_key(h)) { continue; }
            for l in body.iter_mut() {
                if l.trim().is_empty() { continue; }
                let kept: Vec<String> = sentences(l).into_iter().filter(|x| !(fact_in_talk(x, &gone) && !keep(x))).collect();
                *l = kept.join(" ");
            }
            body.retain(|l| !l.trim().trim_start_matches(['-', '*']).trim().is_empty());
        }
    }
    for (h, body) in new_secs {
        let key = heading_key(&h);
        let Some(i) = secs.iter().position(|s| heading_key(&s.0) == key && (key.is_empty() == s.0.is_empty())) else { secs.push((h, body)); continue };
        if replaces(&key) { secs[i] = (h, body); continue; }
        if key.starts_with("files changed") {
            let path = |l: &str| l.trim().trim_start_matches("- ").split(": ").next().unwrap_or("").to_string();
            let mut lines = secs[i].1.clone();
            for l in body {
                match lines.iter().position(|o| path(o) == path(&l)) { Some(j) => lines[j] = l, None => lines.push(l) }
            }
            secs[i].1 = lines;
            continue;
        }
        let mut lines = secs[i].1.clone();
        lines.extend(body);
        secs[i].1 = lines;
    }
    let text: Vec<String> = secs.into_iter().flat_map(|(h, b)| {
        let mut v = Vec::new();
        if !h.is_empty() { v.push(h); }
        v.extend(b);
        v.push(String::new());
        v
    }).collect();
    tidy_notes(&text.join("\n"))
}

/// lines every section keeps while another one still has more (`cap_notes_priority`)
const CAP_FLOOR: usize = 2;

/// `notes` (tidied) cut to `max_tokens` by priority, not position: lines go from the lowest-ranked section first
/// (the alias line, the model's own file list, the request quote, …), the oldest first in a `done` list, so Elisa's
/// file records, decisions and open tasks are the last to go. A section left empty loses its heading.
pub fn cap_notes_priority(notes: &str, max_tokens: usize) -> String {
    let notes = tidy_notes(notes);
    if estimate_tokens(&notes) <= max_tokens { return notes; }
    let mut lines: Vec<String> = notes.lines().map(String::from).collect();
    let mut aliases = take_aliases(&mut lines);
    let mut secs = sections(&lines.join("\n"));
    let render = |secs: &[(String, Vec<String>)], aliases: &[String]| {
        let mut out: Vec<String> = Vec::new();
        for (h, b) in secs {
            if b.iter().all(|l| l.trim().is_empty()) { continue; }
            if !h.is_empty() { out.push(h.clone()); }
            out.extend(b.iter().cloned());
            out.push(String::new());
        }
        if !aliases.is_empty() { out.push(format!("Also known as: {}", aliases.join("; "))); }
        out.join("\n").trim().to_string()
    };
    let lines_of = |b: &[String]| b.iter().filter(|l| !l.trim().is_empty()).count();
    while estimate_tokens(&render(&secs, &aliases)) > max_tokens {
        if aliases.pop().is_some() { continue; }
        // the lowest-ranked section that still has more than CAP_FLOOR lines, so one long section (a done list) never
        // starves the facts and the next step; once every section is down to that, strictly by rank (the session goal
        // goes last of all)
        let rank_of = |i: usize| if secs[i].0.is_empty() { 253 } else { rank(&heading_key(&secs[i].0)) };
        let pick = |floor: usize| (0..secs.len()).filter(|&i| lines_of(&secs[i].1) > floor).max_by_key(|&i| (rank_of(i), i));
        let Some(i) = pick(CAP_FLOOR).or_else(|| pick(0)) else { break };
        let key = heading_key(&secs[i].0);
        let b = &mut secs[i].1;
        let oldest_first = key.starts_with("done") || key.starts_with("from state");
        let pos = if oldest_first { b.iter().position(|l| !l.trim().is_empty()) } else { b.iter().rposition(|l| !l.trim().is_empty()) };
        match pos { Some(p) => { b.remove(p); } None => break }
    }
    render(&secs, &aliases)
}

/// lines a long section of the new summary keeps when /clear shrinks the notes (`shrink_for_clear`)
const CLEAR_FLOOR: usize = 8;

/// `notes` (tidied) made smaller for /clear, dropping only low-value lines, never facts (eval
/// `context.clear_merges_notes` with memory on: "Codename BLUE HERON" was cut so a two-turn session's context
/// came out smaller). Never dropped: a line the earlier notes `old` had, Elisa's file records, any facts section.
/// Dropped first, until the notes fit `max_tokens`: new `Also known as:` items, the model's own file list and the
/// request quote (`## Files touched`, `## Goal`); then new lines of an overlong section (more than CLEAR_FLOOR lines:
/// verbose prose), the lowest-ranked section first. When that is not enough the notes stay bigger than `max_tokens`:
/// losing a fact costs more than a context that did not shrink.
pub fn shrink_for_clear(notes: &str, old: Option<&str>, max_tokens: usize) -> String {
    let notes = tidy_notes(notes);
    if estimate_tokens(&notes) <= max_tokens { return notes; }
    let old_keys: HashSet<String> = old.map(|o| o.lines().map(bullet_key).filter(|k| !k.is_empty()).collect()).unwrap_or_default();
    let old_aliases: HashSet<String> = old.map(|o| {
        let mut l: Vec<String> = o.lines().map(String::from).collect();
        take_aliases(&mut l).iter().map(|a| norm_words(a)).collect()
    }).unwrap_or_default();
    let mut lines: Vec<String> = notes.lines().map(String::from).collect();
    let mut aliases = take_aliases(&mut lines);
    let mut secs = sections(&lines.join("\n"));
    let render = |secs: &[(String, Vec<String>)], aliases: &[String]| {
        let mut out: Vec<String> = Vec::new();
        for (h, b) in secs {
            if b.iter().all(|l| l.trim().is_empty()) { continue; }
            if !h.is_empty() { out.push(h.clone()); }
            out.extend(b.iter().cloned());
            out.push(String::new());
        }
        if !aliases.is_empty() { out.push(format!("Also known as: {}", aliases.join("; "))); }
        out.join("\n").trim().to_string()
    };
    // 0 = never cut, 1 = only its excess over CLEAR_FLOOR, 2 = low value
    let class = |h: &str| {
        let key = heading_key(h);
        if key.starts_with("facts") || key.starts_with("files changed") { 0 }
        else if key.starts_with("goal") || key.starts_with("files touched") { 2 }
        else { 1 }
    };
    let rank_of = |h: &str| if h.is_empty() { 253 } else { rank(&heading_key(h)) };
    while estimate_tokens(&render(&secs, &aliases)) > max_tokens {
        if let Some(i) = aliases.iter().rposition(|a| !old_aliases.contains(&norm_words(a))) { aliases.remove(i); continue; }
        let new_lines = |b: &[String]| b.iter().enumerate().filter(|(_, l)| { let k = bullet_key(l); !k.is_empty() && !old_keys.contains(&k) }).map(|(j, _)| j).collect::<Vec<_>>();
        let pick = (0..secs.len()).filter(|&i| {
            let (c, n) = (class(&secs[i].0), secs[i].1.iter().filter(|l| !l.trim().is_empty()).count());
            !new_lines(&secs[i].1).is_empty() && (c == 2 || (c == 1 && n > CLEAR_FLOOR))
        }).max_by_key(|&i| (class(&secs[i].0), rank_of(&secs[i].0), i));
        let Some(i) = pick else { break };
        let cand = new_lines(&secs[i].1);
        let key = heading_key(&secs[i].0);
        let oldest_first = key.starts_with("done") || key.starts_with("from state");
        let p = if oldest_first { cand[0] } else { cand[cand.len() - 1] };
        secs[i].1.remove(p);
    }
    render(&secs, &aliases)
}
