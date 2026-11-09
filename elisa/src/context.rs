//! Token budget, trimming, compaction, project store and the system prompt. Contract in DESIGN.md.
use crate::frontend::SessionInfo;
use crate::types::{Block, Message, Role};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SYSTEM_MD: &str = include_str!("../prompts/system.md");
const COMPACT_MD: &str = include_str!("../prompts/compact.md");
const PROJECT_CAP: usize = 3000;
const NOTES_CAP: usize = 4000;
const MSG_OVERHEAD: usize = 6;
/// /resume lists at most this many sessions (newest first)
const LIST_MAX: usize = 50;
/// bytes read from the head of an old session file (no .meta) to find its title
const HEAD_BYTES: u64 = 8192;

fn tokens_for_chars(n: usize) -> usize { (n * 10).div_ceil(33) }
fn chars_for_tokens(t: usize) -> usize { t * 33 / 10 }

pub fn estimate_tokens(s: &str) -> usize { tokens_for_chars(s.chars().count()) }

/// a picture's cost in the window: the app scales pictures to at most 1024 px a side, which vision models read as
/// roughly 1000-1400 tokens (Qwen-VL: w*h/784, Claude: w*h/750). One fixed figure keeps the budget math simple.
pub const IMAGE_TOKENS: usize = 1300;

fn block_tokens(b: &Block) -> usize {
    match b {
        Block::Text { text } => estimate_tokens(text),
        Block::ToolUse { id, name, input } => estimate_tokens(id) + estimate_tokens(name) + estimate_tokens(&input.to_string()),
        Block::ToolResult { tool_use_id, content, .. } => estimate_tokens(tool_use_id) + estimate_tokens(content),
        Block::Image { .. } => IMAGE_TOKENS,
    }
}

fn message_tokens(m: &Message) -> usize { MSG_OVERHEAD + m.content.iter().map(block_tokens).sum::<usize>() }

pub fn estimate_messages(msgs: &[Message]) -> usize { msgs.iter().map(message_tokens).sum() }

/// First `n` chars of `s` (char-boundary safe).
fn head(s: &str, n: usize) -> &str { s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i]) }

/// Last `n` chars of `s` (char-boundary safe).
fn tail(s: &str, n: usize) -> &str {
    let len = s.chars().count();
    if n >= len { return s; }
    s.char_indices().nth(len - n).map_or("", |(i, _)| &s[i..])
}

fn capped(s: &str, cap: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= cap { s.to_string() } else { format!("{}\n[... cut at {cap} chars]", head(s, cap)) }
}

fn local_date() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { return "unknown".into(); }
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

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
        s.push_str("\n\n## Notes from earlier sessions\n");
        s.push_str(&capped(n, NOTES_CAP));
    }
    s.push('\n');
    s
}

/// First user message of a history: cwd, OS, date and notes go here, not in the system prompt, so the system turn
/// (system + tools) is byte-identical across projects, days and sessions and the gateway restores one stable snapshot.
pub fn first_message(cwd: &Path, notes: Option<&str>, input: &str) -> String { first_message_with(cwd, notes, None, input) }

/// Heading of the notes block in a first message: says who wrote them (Test 2026-11-01: after a reset the model
/// treated invented note lines as things the user had said).
pub const NOTES_HEADING: &str = "## Notes from earlier sessions (written by Elisa, not the user; may be wrong: check files before relying on them)";

/// `first_message` plus the latest state card as `[state]\n…\n[/state]` (after the notes, before the request).
pub fn first_message_with(cwd: &Path, notes: Option<&str>, state: Option<&str>, input: &str) -> String {
    first_message_full(cwd, notes, state, &[], input)
}

/// `first_message_with` plus recalled memories, as their own block before `## Request` (never inside it: the
/// request section is the user's words only).
pub fn first_message_full(cwd: &Path, notes: Option<&str>, state: Option<&str>, memory: &[String], input: &str) -> String {
    let mut s = format!("Working directory: {}\nOS: {}\nDate: {}\n\n", cwd.display(), std::env::consts::OS, local_date());
    let notes = notes.map(str::trim).filter(|n| !n.is_empty());
    let state = state.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = notes { s += &format!("{NOTES_HEADING}\n{}\n\n", capped(n, NOTES_CAP)); }
    if let Some(c) = state { s += &format!("[state]\n{c}\n[/state]\n\n"); }
    if !memory.is_empty() { s += &with_memory(memory, ""); }
    if notes.is_some() || state.is_some() || !memory.is_empty() { s += "## Request\n"; }
    s + input
}

/// A reply that writes a tool call as plain text instead of calling the tool (Test 2026-11-01 after a reset:
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
/// only an embedding found never qualifies: Test 2026-11-01 attached a sea-water answer to "how's it going?");
/// `support`: the idf-weighted share of the query's terms the hit has (common words weigh little, so a query made of
/// them alone finds nothing that covers it). Mid-session (the window already holds the conversation) the keyword bar
/// doubles: lex >= 2x min_score. Calibrated 2026-11-01 on a copy of the user's store (no vectors): relevant hits had
/// support 0.45..1.0, the off-topic ones 0.09..0.42.
pub fn recall_passes(lex: f32, support: f32, mid_session: bool, min_score: f32, min_support: f32) -> bool {
    let l = if mid_session { 2.0 * min_score } else { min_score };
    lex >= l && support >= min_support
}

/// Cap on a stored state card (the prompt asks for <= 80 tokens).
const CARD_CAP: usize = 600;

/// Tool-call / think tags the model leaked into a card (`</parameter>`, `<function=x>`, `<tool_call>`, `</think>`, …):
/// removed, then lines left empty dropped. Test 23 had a card ending in a literal `</parameter>`.
pub fn strip_markup(card: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"</?(?:tool_call|function|parameter|think)(?:=[^>\n]*)?>").unwrap());
    let s = re.replace_all(card, "");
    s.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Splits a final reply into (visible text, last complete `<state>` card, broken). Every complete block and a trailing
/// unclosed `<state>` are removed from the visible text; broken = no usable card (none, unclosed or empty).
/// the system prompt's card instruction line, which small models (qwen3-vl:2b-instruct) copy into their reply before
/// the card (or instead of it): removed from what is shown and kept
pub const CARD_ECHO: &str = "<=80-token card of what the next step needs:";

pub fn split_state(text: &str) -> (String, Option<String>, bool) {
    let (mut vis, mut card, mut rest) = (String::new(), None, text);
    while let Some(i) = rest.find("<state>") {
        vis.push_str(&rest[..i]);
        let after = &rest[i + 7..];
        match after.find("</state>") {
            Some(j) => { card = Some(after[..j].trim().to_string()); rest = &after[j + 8..]; }
            None => { rest = ""; break; }
        }
    }
    vis.push_str(rest);
    let card = card.and_then(|c| clean_card(&strip_markup(&c))).map(|c| if c.chars().count() > CARD_CAP { format!("{}…", head(&c, CARD_CAP)) } else { c });
    let broken = card.is_none();
    (vis.replace(CARD_ECHO, "").trim_end().to_string(), card, broken)
}

/// The fields of the system prompt's card template.
const CARD_KEYS: &[&str] = &["goal", "done", "now", "keep", "next", "open"];
/// The template's own values (`goal: …`, `keep: must-not-lose facts`): a small model copies them into its card
/// (uitest 2026-11-08: card #9 kept `keep: must-not-lose facts` and was carried into every later session).
const CARD_PLACEHOLDERS: &[&str] = &["must-not-lose facts", "must not lose facts", "must-not-lose", "n/a", "tbd", "todo", "none yet"];

/// A value that says nothing: empty, only dots / dashes / ellipses, or one of the template's placeholders.
fn is_placeholder(v: &str) -> bool {
    let v = v.trim().trim_matches(|c: char| c == '`' || c == '"' || c == '\'' || c == '*').trim();
    let v = v.trim_end_matches(['.', '…']).trim();
    v.is_empty() || v.chars().all(|c| matches!(c, '.' | '…' | '-' | '_' | '?') || c.is_whitespace())
        || CARD_PLACEHOLDERS.iter().any(|p| v.eq_ignore_ascii_case(p))
}

/// A card as it is kept and stored: template placeholders, empty fields and the copied instruction line dropped
/// (`keep: must-not-lose facts; ./run.sh` keeps `./run.sh`). None when nothing real is left.
pub fn clean_card(card: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for l in card.lines() {
        let t = l.trim();
        if t.is_empty() || t.contains(CARD_ECHO) || t.contains("End final replies") || t == "<state>" || t == "</state>" { continue; }
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().trim_start_matches(['-', '*']).trim().to_ascii_lowercase();
            if CARD_KEYS.contains(&key.as_str()) {
                let items: Vec<&str> = v.split(';').map(str::trim).filter(|x| !is_placeholder(x)).collect();
                if !items.is_empty() { out.push(format!("{key}: {}", items.join("; "))); }
                continue;
            }
        }
        if !is_placeholder(t) { out.push(t.to_string()); }
    }
    let s = out.join("\n");
    (!s.trim().is_empty()).then_some(s)
}

/// lowercase words only (punctuation, quotes and symbols as spaces), for comparing notes with the instructions
fn norm_words(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Summary notes as they are stored: the summarizer's instructions and template text never are (uitest 2026-11-08:
/// a 2B model quoted "The conversation above will be erased. Write notes…" as the user's goal and kept
/// "Finished steps and decisions, with their reasons." and "Also known as: name = other words; …"). Drops lines that
/// repeat (part of) prompts/compact.md, placeholder values, template items of the `Also known as:` line, and
/// headings left with nothing under them.
pub fn clean_notes(notes: &str) -> String {
    let instr = norm_words(COMPACT_MD);
    // whole instruction sentences / clauses: a line quoting one is an echo even with real words around it
    let frags: Vec<String> = COMPACT_MD.split(['.', '\n', ':', '(', ')']).map(norm_words).filter(|f| f.len() >= 24).collect();
    let echo = |t: &str| {
        let n = norm_words(t);
        (n.len() >= 12 && instr.contains(&n)) || frags.iter().any(|f| n.contains(f.as_str()))
    };
    let mut lines: Vec<String> = Vec::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') {
            if !t.trim_start_matches('#').trim().is_empty() { lines.push(t.to_string()); }
            continue;
        }
        if t.is_empty() { lines.push(String::new()); continue; }
        let body = t.trim_start_matches(['-', '*', '`', ' ']).trim_end_matches('`');
        if body.to_ascii_lowercase().starts_with("also known as:") {
            let items: Vec<&str> = body["also known as:".len()..].split(';').map(str::trim)
                .filter(|x| !is_placeholder(x) && norm_words(x) != "name other words" && real_alias(x)).collect();
            if !items.is_empty() { lines.push(format!("Also known as: {}", items.join("; "))); }
            continue;
        }
        if is_placeholder(body) || is_placeholder(body.trim_matches(['[', ']', '(', ')'])) || body.trim_matches(['[', ']', '(', ')']).eq_ignore_ascii_case("none") || echo(t) { continue; }
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

/// names an `Also known as:` item may not have: the parties of every conversation, the OS, dates (retest 2026-11-08:
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

/// The line in front of notes a NEW session carries from an earlier one (not `--continue` / `/resume`, and after
/// `/clear` or `/reset`): they are background, not the task (uitest 2026-11-08: an old goal carried into a new
/// session sent a 2B model on 30 web steps the user never asked for).
pub const NOTES_BACKGROUND: &str = "(Background from an earlier session; the user has not asked to continue it. Do not pursue its goal or next step unless the request asks.)";

/// Notes as a new session's first message carries them: the background line first, and without the old
/// `## Session goal` section (that goal belongs to the earlier session; this one sets its own).
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

fn is_user_text(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }))
}

fn trim_result(content: &str) -> String {
    let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    format!("[trimmed: {} … {} lines]", head(first, 80), content.lines().count())
}

/// Whether a fold's one-line shrink would really make this result shorter: run2 turned "False\n[exit 0]" (14 chars)
/// into "[trimmed: False … 2 lines]" (25), a longer rewrite that still cost a full re-prefill.
fn shrinkable(content: &str) -> bool {
    !content.starts_with("[trimmed:") && trim_result(content).chars().count() < content.chars().count()
}

/// Share (percent) of max_ctx kept free on top of the output reserve: the 3.3 chars/token estimate runs low on code and
/// JSON (run2 call 30: 8,199 real tokens sent against max_ctx 8,192 and 0 tokens came back).
pub const CTX_MARGIN_PCT: usize = 3;
pub fn ctx_margin(max_ctx: usize) -> usize { (max_ctx * CTX_MARGIN_PCT).div_ceil(100) }

/// The next request's size for a fit decision: the largest of
/// - `text_now`: the chars/token estimate of the request as it stands,
/// - `reported`: (the server's last reported prompt tokens, the estimate of what was appended since) — their sum,
/// - `calib`: (real prompt tokens, text estimate of that same prompt) of the last reply — without a report (a rewrite
///   dropped it), `text_now` scaled by that ratio when the estimate ran low. With a report, the margin
///   (`ctx_margin`) covers the appended part.
pub fn safe_request_tokens(text_now: usize, reported: Option<(usize, usize)>, calib: Option<(usize, usize)>) -> usize {
    let by_report = reported.map_or(0, |(real, new)| real + new);
    let by_ratio = match calib {
        Some((real, est)) if reported.is_none() && est > 0 && real > est => (text_now as u128 * real as u128).div_ceil(est as u128) as usize,
        _ => 0,
    };
    text_now.max(by_report).max(by_ratio)
}

/// Whether a request of `safe_tokens` (from `safe_request_tokens`) leaves `out` tokens of output under `max_ctx` after
/// the safety margin.
pub fn request_fits(safe_tokens: usize, out: usize, max_ctx: usize) -> bool {
    safe_tokens + ctx_margin(max_ctx) + out <= max_ctx
}

/// The least room (tokens) a step's tool results need once `cap_step_results` cut each to its minimum.
pub fn min_step_tokens(results: &[Block]) -> usize {
    let mut chars = 0;
    let mut overhead = 0;
    for b in results {
        if let Block::ToolResult { tool_use_id, content, .. } = b {
            chars += content.chars().count().min(RESULT_MIN_CHARS + 24);
            overhead += estimate_tokens(tool_use_id) + 8;
        }
    }
    // cap_step_results budgets 10% under the estimate
    overhead + tokens_for_chars(chars) * 10 / 9 + 1
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
/// folds older turns rather than cut below it, and says so plainly when even that does not fit (`not_shown`). soak:
/// results cut to "0 tokens left" (240 chars of head) were re-requested or answered with invented numbers.
pub const RESULT_MIN_CHARS: usize = 1000;

/// Follows the marker of a cut `read`: the cut took its `next: offset=N` footer, and more reads would only be cut too
/// (run 2026-11-05: 2-4 lines per read until the context was full, the offsets guessed).
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
/// whole (each keeps >= RESULT_MIN_CHARS). ui_run1: four parallel reads of big JSON files (25 KB) made the next request
/// 10.1K tokens against an 8192 limit. Returns how many results were cut.
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
/// requests change it only with an explicit goal statement (the card / notes goal drifted to the newest request in
/// ui_run1, so "what was the goal of this session?" got the last request).
/// A pasted blob gives no goal (only the user's own words around it do; `own_words`).
pub fn next_goal(current: Option<&str>, input: &str) -> Option<String> {
    let input = request_body(input).trim();
    if input.is_empty() || input.starts_with('/') || input.starts_with(NUDGE_PREFIX.trim_end()) { return current.map(String::from); }
    let Some(input) = own_words(input) else { return current.map(String::from) };
    match (current, goal_statement(input)) {
        (_, Some(g)) => Some(g),
        (Some(c), None) => Some(c.to_string()),
        (None, None) => {
            // small talk and a plain question set no goal (soak: "hi Elisa! how are you today?" became the session
            // goal); a greeting in front of a request is not part of it
            let req = without_greeting(input).trim();
            let question = req.ends_with('?') && crate::mcp_reason::wanted(req) == Some(false);
            if req.is_empty() || is_small_talk(req) || question { None } else { Some(one_line(req, 240)) }
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
/// only pasted text (longrun2: the first 240 chars of a 200 KB paste became the session goal and the 2B model kept
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
fn content_words(s: &str) -> HashSet<String> {
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
    let sec = format!("{GOAL_NOTES}\n{}", one_line(goal, 240));
    if rest.is_empty() { sec } else { format!("{sec}\n\n{rest}") }
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

/// Share of the smaller text's distinct words (alphanumeric, lowercased, >= 2 chars) that the other one also has.
pub fn word_overlap(a: &str, b: &str) -> f32 {
    let words = |s: &str| -> HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_lowercase).collect()
    };
    let (a, b) = (words(a), words(b));
    let n = a.len().min(b.len());
    if n == 0 { return 0.0; }
    a.intersection(&b).count() as f32 / n as f32
}

/// Whitespace collapsed to single spaces, cut at `n` chars with `…`.
pub fn one_line(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n { format!("{}…", head(&flat, n)) } else { flat }
}

fn tool_summary(name: &str, input: &Value) -> String {
    let arg = ["path", "command", "pattern"].iter().find_map(|k| input.get(*k).and_then(Value::as_str))
        .or_else(|| input.as_object().and_then(|o| o.values().find_map(Value::as_str)));
    match arg {
        Some(a) => format!("{name} {}", one_line(a, 100)),
        None => name.to_string(),
    }
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
                    if t.is_empty() { continue; }
                    if m.role == Role::User && t.starts_with(NUDGE_PREFIX.trim_end()) { continue; }
                    out.push_str(if m.role == Role::User { "User: " } else { "Elisa: " });
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
        .filter(|l| l.starts_with("User: ") || l.starts_with("Elisa: "))
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

static RUN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn unix_now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) }

/// Per-session id so every save of one session goes to the same session file (set at the first use; /clear starts
/// a new one with `new_session`).
fn run_id() -> u64 {
    use std::sync::atomic::Ordering;
    let id = RUN_ID.load(Ordering::SeqCst);
    if id != 0 { return id; }
    let _ = RUN_ID.compare_exchange(0, unix_now().max(1), Ordering::SeqCst, Ordering::SeqCst);
    RUN_ID.load(Ordering::SeqCst)
}

/// A new session file from now on (/clear): the one saved so far stays as it is, so /resume can go back to it.
pub fn new_session() {
    use std::sync::atomic::Ordering;
    let next = unix_now().max(run_id() + 1);
    RUN_ID.store(next, Ordering::SeqCst);
}

/// This run's session id: the session file is `.elisa/sessions/<id>.json`.
pub fn session_id() -> String { run_id().to_string() }

fn non_empty(s: String) -> Option<String> { if s.trim().is_empty() { None } else { Some(s) } }

/// Project-local state under <cwd>/.elisa/
pub struct Store { pub dir: PathBuf }
impl Store {
    pub fn new(cwd: &Path) -> Store { Store { dir: cwd.join(".elisa") } }

    fn notes_path(&self) -> PathBuf { self.dir.join("notes.md") }
    fn allow_path(&self) -> PathBuf { self.dir.join("allow") }

    /// Tools the user chose "always allow" for in this project (`.elisa/allow`, one name per line).
    pub fn load_allowed(&self) -> HashSet<String> {
        fs::read_to_string(self.allow_path()).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    }
    /// Save the always-allowed tools (empty set removes the file).
    pub fn save_allowed(&self, tools: &HashSet<String>) -> std::io::Result<()> {
        if tools.is_empty() { return match fs::remove_file(self.allow_path()) { Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e), _ => Ok(()) } }
        fs::create_dir_all(&self.dir)?;
        let mut v: Vec<&str> = tools.iter().map(String::as_str).collect();
        v.sort_unstable();
        fs::write(self.allow_path(), v.join("\n") + "\n")
    }
    fn sessions_dir(&self) -> PathBuf { self.dir.join("sessions") }

    pub fn load_project(&self) -> Option<String> {
        let root = self.dir.parent()?;
        fs::read_to_string(root.join("ELISA.md")).ok().and_then(non_empty)
    }

    pub fn load_notes(&self) -> Option<String> { fs::read_to_string(self.notes_path()).ok().and_then(non_empty) }

    pub fn save_notes(&self, notes: &str) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let p = self.notes_path();
        // on disk: never a secret, whatever the `redact` setting
        let (notes, _) = crate::secrets::redact(notes.trim());
        fs::write(&p, format!("{notes}\n")).map_err(|e| format!("{}: {e}", p.display()))
    }

    pub fn clear_notes(&self) -> Result<(), String> {
        let p = self.notes_path();
        match fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
            _ => Ok(()),
        }
    }

    pub fn save_session(&self, msgs: &[Message]) -> Result<PathBuf, String> {
        let dir = self.sessions_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let p = dir.join(format!("{}.json", run_id()));
        // the title below comes from the redacted copy too (QA N2: a password typed in the prompt reached the .meta)
        let msgs = &redact_messages(msgs)[..];
        let json = serde_json::to_string(msgs).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, &p)).map_err(|e| format!("{}: {e}", p.display()))?;
        // tiny sidecar (.elisa/session-meta/<ts>.meta) so /resume never loads whole sessions: "<n_msgs>\t<title>"
        let title = msgs.iter().filter(|m| m.role == Role::User).find_map(|m| m.content.iter().find_map(|b| match b {
            Block::Text { text } => Some(session_title(text)),
            _ => None,
        })).unwrap_or_default();
        let meta = self.dir.join("session-meta");
        if fs::create_dir_all(&meta).is_ok() { let _ = fs::write(meta.join(format!("{}.meta", run_id())), format!("{}\t{title}", msgs.len())); }
        Ok(p)
    }

    /// Newest saved session from an earlier run (this run's own file is skipped).
    pub fn load_last_session(&self) -> Option<Vec<Message>> {
        let own = run_id();
        let newest = fs::read_dir(self.sessions_dir()).ok()?
            .filter_map(|e| {
                let p = e.ok()?.path();
                if p.extension()? != "json" { return None; }
                let ts: u64 = p.file_stem()?.to_str()?.parse().ok()?;
                (ts != own).then_some((ts, p))
            })
            .max_by_key(|(ts, _)| *ts)?;
        serde_json::from_str(&fs::read_to_string(newest.1).ok()?).ok()
    }

    /// Saved sessions from earlier runs, newest first, at most LIST_MAX (this run's own file is skipped). Built from file
    /// names plus the tiny `.meta` sidecars; never loads a whole session (old files without one: title from the
    /// first HEAD_BYTES, n_msgs 0).
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let own = run_id();
        let Ok(rd) = fs::read_dir(self.sessions_dir()) else { return Vec::new() };
        let mut v: Vec<(u64, PathBuf)> = rd.filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension()? != "json" { return None; }
            let ts: u64 = path.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, path))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.truncate(LIST_MAX);
        v.into_iter().map(|(ts, path)| {
            let (n_msgs, title) = match fs::read_to_string(self.dir.join(format!("session-meta/{ts}.meta"))) {
                Ok(m) => {
                    let (n, t) = m.split_once('\t').unwrap_or((&m, ""));
                    (n.trim().parse().unwrap_or(0), t.to_string())
                }
                Err(_) => (0, head_title(&path)),
            };
            SessionInfo { path, ts, n_msgs, title }
        }).collect()
    }

    pub fn load_session(&self, path: &Path) -> Option<Vec<Message>> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    }

    fn card_reset_path(&self) -> PathBuf { self.dir.join("card-reset") }

    /// /reset and /forget dropped the state card: cards up to `id` (elisa-mem ids) are not carried into a new session.
    pub fn save_card_reset(&self, id: u64) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.card_reset_path(), format!("{id}\n")).map_err(|e| format!("{}: {e}", self.card_reset_path().display()))
    }

    /// The newest card id a /reset or /forget dropped (None = never).
    pub fn card_reset(&self) -> Option<u64> { fs::read_to_string(self.card_reset_path()).ok()?.trim().parse().ok() }
}

/// Heading of the notes section a state-card fold writes (no model call): what the folded turns' cards said.
pub const CARD_NOTES: &str = "## From state cards";
/// cap on that section (chars); the oldest done / keep items go first
const CARD_NOTES_CAP: usize = 1500;

/// A card field's items: `a; b` → [a, b] (one item when there is no `;`).
fn items(v: &str) -> Vec<String> { v.split(';').map(|x| x.trim().trim_end_matches('.').to_string()).filter(|x| !x.is_empty() && x != "…" && x != "-").collect() }

/// `notes` with the cards of folded turns merged in, no model call: `goal` = the newest card's, `done` and `keep`
/// accumulate (old section + every card, deduplicated), `open` = the newest card's `now` / `next` minus anything
/// already done. The section replaces the previous one; the rest of the notes is kept as it is.
pub fn merge_card_notes(notes: Option<&str>, cards: &[String]) -> String {
    let notes = notes.unwrap_or("").trim();
    let (rest, old) = match notes.find(CARD_NOTES) {
        Some(i) => {
            let after = &notes[i + CARD_NOTES.len()..];
            let end = after.find("\n## ").map_or(after.len(), |j| j + 1);
            (format!("{}{}", &notes[..i], &after[end..]).trim().to_string(), after[..end].to_string())
        }
        None => (notes.to_string(), String::new()),
    };
    let field = |text: &str, k: &str| -> Vec<String> {
        text.lines().filter_map(|l| l.trim().strip_prefix(k).and_then(|r| r.strip_prefix(':'))).flat_map(items).collect()
    };
    let (mut goal, mut done, mut keep, mut open): (String, Vec<String>, Vec<String>, Vec<String>) =
        (field(&old, "goal").join("; "), field(&old, "done"), field(&old, "keep"), field(&old, "open"));
    let push = |v: &mut Vec<String>, x: String| if !v.iter().any(|y| y.eq_ignore_ascii_case(&x)) { v.push(x) };
    for c in cards {
        let g = field(c, "goal").join("; ");
        if !g.is_empty() { goal = g; }
        for x in field(c, "done") { push(&mut done, x); }
        for x in field(c, "keep") { push(&mut keep, x); }
        let o: Vec<String> = field(c, "now").into_iter().chain(field(c, "next")).collect();
        if !o.is_empty() { open = o; }
    }
    open.retain(|x| !done.iter().any(|d| d.eq_ignore_ascii_case(x)));
    let render = |goal: &str, done: &[String], keep: &[String], open: &[String]| {
        let mut s = format!("{CARD_NOTES}\n");
        if !goal.is_empty() { s += &format!("goal: {goal}\n"); }
        if !done.is_empty() { s += &format!("done: {}\n", done.join("; ")); }
        if !keep.is_empty() { s += &format!("keep: {}\n", keep.join("; ")); }
        if !open.is_empty() { s += &format!("open: {}\n", open.join("; ")); }
        s
    };
    let mut sec = render(&goal, &done, &keep, &open);
    while sec.chars().count() > CARD_NOTES_CAP && (!done.is_empty() || !keep.is_empty()) {
        if done.len() >= keep.len() { done.remove(0); } else { keep.remove(0); }
        sec = render(&goal, &done, &keep, &open);
    }
    let sec = head(&sec, CARD_NOTES_CAP).trim_end().to_string();
    if rest.is_empty() { sec } else { format!("{rest}\n\n{sec}") }
}

/// Title of a session file without a `.meta`: the first `"text":"…"` string in its first HEAD_BYTES.
fn head_title(path: &Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if fs::File::open(path).and_then(|f| f.take(HEAD_BYTES).read_to_end(&mut buf)).is_err() { return String::new(); }
    let s = String::from_utf8_lossy(&buf);
    let Some(i) = s.find("\"text\":\"") else { return String::new() };
    let rest = &s[i + 7..];
    // the JSON string up to its closing quote (or the cut), decoded by serde
    let mut end = rest.len();
    let mut esc = false;
    for (j, c) in rest.char_indices().skip(1) {
        if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { end = j + 1; break; }
    }
    let lit = if end == rest.len() { format!("{}\"", rest.trim_end_matches('\\')) } else { rest[..end].to_string() };
    serde_json::from_str::<String>(&lit).map(|t| session_title(&t)).unwrap_or_default()
}

/// The user's own words of a user message: without the `first_message` header and a leading recalled `[memory]` block.
pub fn request_body(text: &str) -> &str {
    let body = match text.rsplit_once("## Request\n") {
        Some((_, r)) => r,
        None if text.starts_with("Working directory: ") => text.split_once("\n\n").map_or(text, |(_, r)| r),
        None => text,
    };
    strip_memory(body)
}

/// `text` without a leading `[memory…]…[/memory]` block (sessions before 2026-11-01 put it in front of the request).
fn strip_memory(text: &str) -> &str {
    if !text.starts_with("[memory") { return text; }
    match text.find("[/memory]") {
        Some(i) => text[i + "[/memory]".len()..].trim_start_matches('\n'),
        None => text,
    }
}

/// The user's own words of a first message (without the `first_message` header), one line, <= 80 chars.
fn session_title(text: &str) -> String { one_line(request_body(text), 79) }

/// Opening line of a recalled-memory block: says it is background from Elisa, not the user's words.
pub const MEMORY_OPEN: &str = "[memory: recalled by Elisa from earlier sessions, not written by the user; background only, ignore it if unrelated]";

/// Recalled memories as a block that goes in front of the user's text (in the USER message, never the system prompt).
pub fn with_memory(lines: &[String], text: &str) -> String {
    if lines.is_empty() { return text.to_string(); }
    let mut s = format!("{MEMORY_OPEN}\n");
    for l in lines { s += &format!("- {l}\n"); }
    s + "[/memory]\n\n" + text
}

/// The summary instructions (prompts/compact.md), sent as one more user message for an in-context summary.
pub fn compact_instructions() -> &'static str { COMPACT_MD.trim_end() }

// ----- the context budget at small windows (soak 2026-11-08: 8K, 40 folds in 40 turns) -----

/// Prompt tokens where a between-turns fold starts: the fixed cost (system + tools) plus `fold_at` of the room left
/// after it. A share of the whole limit (the old rule) sat barely above the fixed cost at 8K (fold at 3.8K with 2.8K
/// fixed), so nearly every turn folded.
pub fn fold_point(fixed: usize, limit: usize, fold_at: f32) -> usize {
    fixed + (fold_at.clamp(0.0, 1.0) as f64 * limit.saturating_sub(fixed) as f64) as usize
}

/// Room (tokens) for the history a fold keeps: `fold_to` of the working budget (the limit minus the fixed cost) minus
/// the new notes or card (`new_text`), and never less than an eighth of the working budget.
pub fn fold_room(fixed: usize, limit: usize, fold_to: f32, new_text: usize) -> usize {
    let working = limit.saturating_sub(fixed);
    ((fold_to.clamp(0.0, 1.0) as f64 * working as f64) as usize).saturating_sub(new_text).max(working / 8)
}

/// A fold must free at least this much (tokens) to be worth its re-prefill: an eighth of the working budget, >= 64.
pub fn min_fold_gain(fixed: usize, limit: usize) -> usize { (limit.saturating_sub(fixed) / 8).max(64) }

/// The notes' share of the window (tokens): a sixth of the working budget, within 200 ..= 1200. After a fold the
/// prompt is the fixed cost + the notes + the kept exchange; notes near the fold point made every later turn fold again
/// (8K live run: ~1,000-token notes left ~700 tokens of room before the next fold).
pub fn notes_budget(fixed: usize, limit: usize) -> usize { (limit.saturating_sub(fixed) / 6).clamp(200, 1200) }

/// A tool result that did not fit even after a fold: said plainly, so the model does not invent what it held (soak:
/// a grep count trimmed to nothing was answered "2734"; the real count was 378).
pub fn not_shown(content: &str) -> String {
    let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    format!("[elisa: this result ({} chars, {} lines; it starts \"{}\") did not fit the context window even after folding \
        older turns, so it is not shown. Do not guess what it said: tell the user, or run a narrower call (a grep pattern, \
        read with offset and limit, tail).]", content.chars().count(), content.lines().count(), one_line(head(first, 60), 60))
}

// ----- notes: Elisa's edit records, claim checks, merge, dedupe, priority cap (no model call) -----

/// Heading of the notes section Elisa writes from its own edit records (ground truth, never the model's claim).
pub const FILES_NOTES: &str = "## Files changed (Elisa's own edit records)";
const LEDGER_TEXT_CAP: usize = 200_000;

/// What Elisa's own tools changed in this session: the files `write` / `edit` / `edit_symbol` changed (with the text
/// those edits wrote and each file's text before its first change) and the shell commands run. The evidence a
/// summary's "added X" must be backed by.
#[derive(Default, Clone, Debug)]
pub struct Ledger {
    files: Vec<LedgerFile>,
    /// shell commands run (lowercased): a `sed -i` or `echo >>` change is evidence too
    shell: String,
}

#[derive(Clone, Debug)]
struct LedgerFile { path: String, writes: usize, edits: usize, wrote: String, before: String }

/// `path` relative to `cwd` when it lies inside it (the model often writes absolute paths)
fn rel_path(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    p.strip_prefix(cwd).map(|r| r.display().to_string()).unwrap_or_else(|_| path.trim_start_matches("./").to_string())
}

impl Ledger {
    /// Before a write tool runs: the file's text as it was (kept for its first change only).
    pub fn before(&mut self, input: &Value, cwd: &Path) {
        let Some(path) = input.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else { return };
        let rel = rel_path(path, cwd);
        if self.files.iter().any(|f| f.path == rel) { return; }
        let text: String = fs::read_to_string(cwd.join(path)).unwrap_or_default().chars().take(LEDGER_TEXT_CAP).collect();
        self.files.push(LedgerFile { path: rel, writes: 0, edits: 0, wrote: String::new(), before: text.to_lowercase() });
    }

    /// A tool call that succeeded.
    pub fn record(&mut self, tool: &str, input: &Value, cwd: &Path) {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match tool {
            "write" | "edit" | "edit_symbol" => {
                let path = s("path");
                if path.trim().is_empty() { return; }
                let rel = rel_path(&path, cwd);
                let i = match self.files.iter().position(|f| f.path == rel) {
                    Some(i) => i,
                    None => { self.files.push(LedgerFile { path: rel, writes: 0, edits: 0, wrote: String::new(), before: String::new() }); self.files.len() - 1 }
                };
                let f = &mut self.files[i];
                if tool == "write" { f.writes += 1 } else { f.edits += 1 }
                let new = match tool { "write" => s("content"), "edit" => s("new"), _ => format!("{} {}", s("name"), s("source")) };
                if f.wrote.len() + new.len() <= LEDGER_TEXT_CAP { f.wrote.push_str(&new.to_lowercase()); f.wrote.push('\n'); }
            }
            "bash" => {
                let c = s("command").to_lowercase();
                if self.shell.len() + c.len() <= LEDGER_TEXT_CAP { self.shell.push_str(&c); self.shell.push('\n'); }
            }
            _ => {}
        }
    }

    /// Whether any file changed (a `before` with no change after it does not count).
    pub fn is_empty(&self) -> bool { self.files.iter().all(|f| f.writes + f.edits == 0) }

    /// The paths that changed, in first-change order.
    pub fn paths(&self) -> Vec<String> { self.files.iter().filter(|f| f.writes + f.edits > 0).map(|f| f.path.clone()).collect() }

    /// The notes section: one line per changed file. None when nothing changed.
    pub fn section(&self) -> Option<String> {
        if self.is_empty() { return None; }
        let mut s = format!("{FILES_NOTES}\n");
        for f in self.files.iter().filter(|f| f.writes + f.edits > 0) {
            let what = match (f.writes, f.edits) {
                (0, e) => format!("edited {e}×"),
                (w, 0) => format!("{}{}", if f.before.is_empty() { "written (new file)" } else { "rewritten" }, if w > 1 { format!(" {w}×") } else { String::new() }),
                (w, e) => format!("written {w}×, edited {e}×"),
            };
            s += &format!("- {}: {what}\n", f.path);
        }
        Some(s.trim_end().to_string())
    }

    /// The changed files `path` (as a summary wrote it) may mean: the same path, or one ends with the other.
    fn matching(&self, path: &str) -> Vec<&LedgerFile> {
        let p = path.trim_start_matches("./").to_lowercase();
        if p.is_empty() { return Vec::new(); }
        self.files.iter().filter(|f| f.writes + f.edits > 0).filter(|f| {
            let q = f.path.to_lowercase();
            q == p || q.ends_with(&format!("/{p}")) || p.ends_with(&format!("/{q}"))
        }).collect()
    }
}

/// verbs that say something new was made: its name must be in what an edit wrote
const CLAIM_NEW: &[&str] = &[
    "added", "wrote", "written", "created", "implemented", "documented", "appended", "inserted", "introduced", "bumped",
    "add", "write", "create", "implement", "document", "append", "insert", "introduce", "bump",
];
/// past-tense change verbs: a line with one claims something was done to the code
const CLAIM_PAST: &[&str] = &[
    "added", "changed", "edited", "wrote", "written", "created", "fixed", "bumped", "updated", "implemented", "renamed",
    "removed", "deleted", "modified", "refactored", "replaced", "documented", "appended", "inserted", "introduced",
    "rewrote", "rewritten", "patched", "corrected",
];
/// the same verbs as a state card's `done:` items write them ("done: add restock_cost")
const CLAIM_BASE: &[&str] = &[
    "add", "change", "edit", "write", "create", "fix", "bump", "update", "implement", "rename", "remove", "delete",
    "modify", "refactor", "replace", "document", "append", "insert", "introduce", "rewrite", "patch", "correct",
];

fn claim_words(line: &str) -> Vec<String> {
    line.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn is_claim(line: &str, done: bool) -> bool {
    let l = line.to_lowercase();
    if l.contains("(unverified") || l.contains(" not ") || l.contains("never") || l.contains("failed") || l.contains("n't ") { return false; }
    claim_words(line).iter().any(|x| CLAIM_PAST.contains(&x.as_str()) || (done && CLAIM_BASE.contains(&x.as_str())))
}

/// The checkable words of a claim: (paths, identifiers). A path has a `/` or a file extension; an identifier has a
/// `_`, starts with `--`, ends with `()`, is a version (`0.3.2`) or a comparison (`<=`); backticked spans count too.
fn claim_tokens(line: &str) -> (Vec<String>, Vec<String>) {
    let (mut paths, mut ids) = (Vec::new(), Vec::new());
    let ver = |t: &str| { let t = t.trim_start_matches('v'); t.contains('.') && t.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) };
    let ext = |t: &str| t.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && !a.chars().all(|c| c.is_ascii_digit())
        && (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()) && e.chars().any(|c| c.is_ascii_alphabetic()));
    let mut words: Vec<String> = line.split('`').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s.trim().to_string()).collect();
    words.extend(line.split_whitespace().map(String::from));
    for w in words {
        let t = w.trim_matches(|c: char| "\"'`,;:()[]{}!?*".contains(c)).trim_end_matches('.').to_string();
        if t.is_empty() || paths.contains(&t) || ids.contains(&t) { continue; }
        if ["<=", ">=", "==", "!=", "<", ">"].contains(&t.as_str()) { ids.push(t); continue; }
        if t.starts_with("http") { continue; }
        if ver(&t) { ids.push(t); continue; }
        if (t.contains('/') && t.chars().filter(|c| c.is_alphanumeric()).count() >= 2) || ext(&t) { paths.push(t); continue; }
        let id = t.trim_end_matches("()");
        if (id.contains('_') && id.chars().any(char::is_alphanumeric)) || (id.starts_with("--") && id.len() > 3) || t.ends_with("()") {
            ids.push(id.to_string());
        }
    }
    (paths, ids)
}

/// One claim against the edit records: Some(true) backed, Some(false) contradicted, None nothing to check it by.
/// A claim that something new was made ("added restock_cost") needs its name in what an edit wrote (or a shell
/// command); a change to existing code ("fixed low_stock (< to <=)") may name code the changed file held before or
/// holds now. A named file narrows the evidence to that file's edits. With only a path, the path must have changed.
fn check_claim(line: &str, ledger: &Ledger, now: &dyn Fn(&str) -> Option<String>) -> Option<bool> {
    let (paths, ids) = claim_tokens(line);
    let named: Vec<&LedgerFile> = paths.iter().flat_map(|p| ledger.matching(p)).collect();
    if ids.is_empty() {
        return if paths.is_empty() { None } else { Some(!named.is_empty()) };
    }
    let new = claim_words(line).iter().any(|w| CLAIM_NEW.contains(&w.as_str()));
    let files: Vec<&LedgerFile> = if named.is_empty() { ledger.files.iter().collect() } else { named };
    let mut corpus = ledger.shell.clone();
    for f in files {
        corpus.push_str(&f.wrote);
        if !new {
            corpus.push_str(&f.before);
            if let Some(t) = now(&f.path) { corpus.push_str(&t.chars().take(LEDGER_TEXT_CAP).collect::<String>().to_lowercase()); }
        }
    }
    Some(ids.iter().any(|i| corpus.contains(&i.to_lowercase())))
}

/// The marker on a "done" line nothing could confirm.
pub const UNVERIFIED: &str = "(unverified: not in Elisa's edit records)";

/// New notes (a summary, or state cards) with their "done / changed" claims checked against Elisa's own edit records
/// (soak 2026-11-08: notes kept "added restock_cost", "--low-only flag", "bumped the version" though no edit did any
/// of it; `check_claim`). A contradicted claim is dropped; one with nothing to check while nothing changed this
/// session is marked UNVERIFIED. The model's `## Files touched` lines keep only files the records have (others are
/// dropped, or marked when there are no records), and a "no files were touched" line goes. `now`: a changed file's text now (path relative to the project).
pub fn verify_claims(notes: &str, ledger: &Ledger, now: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut heading = String::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') { heading = norm_words(t); out.push(l.to_string()); continue; }
        if t.is_empty() { out.push(String::new()); continue; }
        if heading.starts_with("files touched") {
            let n = norm_words(t);
            if n.starts_with("no files") || n.starts_with("none") || n.starts_with("nothing") || n.contains("no files were") { continue; }
            let (paths, _) = claim_tokens(t);
            // Elisa's own section lists what changed: a line it does not back is noise next to it (dropped); with no
            // records at all (bash may have changed something) it is marked instead
            if paths.iter().any(|p| !ledger.matching(p).is_empty()) {
                // the file did change; what the line says about it is a claim like any other
                if !(is_claim(t, true) && check_claim(t, ledger, now) == Some(false)) { out.push(l.to_string()); }
            } else if ledger.is_empty() { out.push(format!("{} {UNVERIFIED}", l.trim_end())); }
            continue;
        }
        // a card's `done: a; b` line: every item is a claim
        if let Some(rest) = t.strip_prefix("done:") {
            let kept: Vec<String> = rest.split(';').map(str::trim).filter(|x| !x.is_empty()).filter_map(|x| match check_claim(x, ledger, now) {
                Some(false) if is_claim(x, true) => None,
                None if is_claim(x, true) && ledger.is_empty() => Some(format!("{x} {UNVERIFIED}")),
                _ => Some(x.to_string()),
            }).collect();
            if !kept.is_empty() { out.push(format!("done: {}", kept.join("; "))); }
            continue;
        }
        // sentence by sentence: a paragraph that mixes a real change with an invented one keeps only the real one
        // (8K live run: "…created CHANGELOG.md. The model has updated README.md…" with README.md never changed)
        let done = heading.starts_with("done");
        let mut kept: Vec<String> = Vec::new();
        for sent in sentences(l.trim_end()) {
            if !is_claim(&sent, done) { kept.push(sent); continue; }
            match check_claim(&sent, ledger, now) {
                Some(false) => {}
                None if ledger.is_empty() => kept.push(format!("{sent} {UNVERIFIED}")),
                _ => kept.push(sent),
            }
        }
        let line = kept.join(" ");
        if !line.trim().trim_start_matches(['-', '*']).trim().is_empty() { out.push(line); }
    }
    clean_notes(&out.join("\n"))
}

/// `line` split after `. ` / `! ` / `? ` (a version like `0.3.2` or a path like `core.py` has no space after its dot)
fn sentences(line: &str) -> Vec<String> {
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
fn sections(notes: &str) -> Vec<(String, Vec<String>)> {
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
fn heading_key(h: &str) -> String { norm_words(h).split(' ').take(2).collect::<Vec<_>>().join(" ") }

/// sections whose newest version replaces the older one (they describe "now", not history)
fn replaces(key: &str) -> bool { ["goal", "current state", "session goal", "from state"].iter().any(|k| key.starts_with(k)) }

/// Keep-first rank of a section when the notes must shrink (lower = kept longer): Elisa's file records, decisions
/// and open tasks first, the model's own file list and the alias line last.
fn rank(key: &str) -> u8 {
    const ORDER: &[&str] = &["session goal", "files changed", "done", "current state", "facts to", "from state", "goal", "files touched"];
    ORDER.iter().position(|k| key.starts_with(k)).map_or(ORDER.len() as u8, |i| i as u8)
}

/// the words of a bullet line, for duplicate checks (`- `, `* `, numbering removed)
fn bullet_key(l: &str) -> String {
    let t = l.trim().trim_start_matches(['-', '*', '•']).trim();
    let d = t.chars().take_while(char::is_ascii_digit).count();
    let t = if d > 0 && t[d..].starts_with(['.', ')']) { &t[d + 1..] } else { t };
    norm_words(t)
}

/// `lines` without duplicates: the same words again, or a line whose words another line contains (>= 3 words: the
/// longer one stays, where the first one stood). soak: a Done list repeated 8 bullets 3-4 times.
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

/// `new` notes merged into `old` ones, no model call: a section that describes "now" (goal, current state, the state
/// card section) is replaced by its newer version; every other section keeps the old lines and adds the new ones
/// (duplicates dropped); Elisa's own file records merge by path (the newer line for a path wins). Sections only the
/// old notes have stay. soak: /clear replaced the notes with a summary of one turn, and the codename, the owner and
/// the `<=` fix were gone.
pub fn merge_notes(old: Option<&str>, new: &str) -> String {
    let old = old.map(str::trim).unwrap_or("");
    if old.is_empty() { return tidy_notes(new); }
    let mut secs = sections(old);
    for (h, body) in sections(new) {
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

/// `facts` without the ones `notes` already say (all of a fact's words, in order, in the notes): with memory on, a
/// note's `Facts` lines are also pinned facts, and the same fact twice in one first message (in the notes and in
/// `[state]`) made the context after /clear bigger than the conversation it replaced.
pub fn facts_not_in(notes: Option<&str>, facts: Vec<String>) -> Vec<String> {
    let Some(n) = notes.map(norm_words).filter(|n| !n.is_empty()) else { return facts };
    let hay = format!(" {n} ");
    facts.into_iter().filter(|f| { let k = norm_words(f); k.is_empty() || !hay.contains(&format!(" {k} ")) }).collect()
}

/// What the model got from outside sources in a stretch of conversation: the bodies of the `<untrusted source=…>`
/// fences in its tool results (web pages, fetched documents, MCP / plugin output; `untrusted.rs`), as 4-word runs,
/// minus any run the user also wrote. Notes and state cards are checked against it (`quarantine_notes`,
/// `quarantine_card`): longrun2 found a fenced page's instruction copied into the notes' next step, from where it
/// carried across /clear as if the user had asked for it.
#[derive(Default)]
pub struct Outside {
    spans: Vec<(String, HashSet<String>)>,
    user: String,
    /// 4-word runs of text the user pasted (a long request minus its own words, `own_words`), and the own words of the
    /// newest such request: a goal is never a quote of a paste
    pasted: HashSet<String>,
    paste_own: Option<String>,
}

/// 4-word runs of `s` (lowercase words)
fn grams(s: &str) -> HashSet<String> {
    let w: Vec<&str> = s.split(' ').filter(|x| !x.is_empty()).collect();
    w.windows(4).map(|g| g.join(" ")).collect()
}

impl Outside {
    pub fn of(msgs: &[Message]) -> Outside {
        let mut user = String::new();
        let mut spans: Vec<(String, String)> = Vec::new();
        let (mut pasted, mut own_g, mut paste_own) = (String::new(), HashSet::new(), None);
        for m in msgs {
            for b in &m.content {
                match b {
                    Block::Text { text } if m.role == Role::User => {
                        let body = request_body(text).trim();
                        match own_words(body) {
                            Some(o) if o == body => { user.push(' '); user.push_str(&norm_words(body)); }
                            o => {
                                pasted.push(' '); pasted.push_str(&norm_words(body));
                                if let Some(o) = o { own_g.extend(grams(&norm_words(o))); user.push(' '); user.push_str(&norm_words(o)); }
                                paste_own = o.map(String::from);
                            }
                        }
                    }
                    Block::ToolResult { content, .. } => {
                        let mut rest = content.as_str();
                        while let Some(i) = rest.find("<untrusted source=\"") {
                            let after = &rest[i + "<untrusted source=\"".len()..];
                            let Some(q) = after.find("\">") else { break };
                            let src = after[..q].to_string();
                            let body = &after[q + 2..];
                            let end = body.find(crate::untrusted::CLOSE).unwrap_or(body.len());
                            spans.push((src, norm_words(&body[..end])));
                            rest = &body[end..];
                        }
                    }
                    _ => {}
                }
            }
        }
        let ug = grams(&user);
        let spans = spans.into_iter().map(|(s, b)| (s, grams(&b).into_iter().filter(|g| !ug.contains(g)).collect::<HashSet<_>>()))
            .filter(|(_, g)| !g.is_empty()).collect();
        let pasted = grams(&pasted).into_iter().filter(|g| !own_g.contains(g)).collect();
        Outside { spans, user: format!(" {user} "), pasted, paste_own }
    }
    pub fn is_empty(&self) -> bool { self.spans.is_empty() && self.pasted.is_empty() }
    /// `line` quotes pasted text (not the user's own words around it)
    fn quotes_paste(&self, line: &str) -> bool { grams(&norm_words(line)).iter().any(|g| self.pasted.contains(g)) }
    /// The source of an outside text `line` repeats (a 4-word run of it the user never wrote), if any.
    pub fn source_of(&self, line: &str) -> Option<&str> {
        let g = grams(&norm_words(line));
        self.spans.iter().find(|(_, sg)| g.iter().any(|x| sg.contains(x))).map(|(s, _)| s.as_str())
    }
    /// An injection phrase in `line` the user did not write themselves.
    fn injected(&self, line: &str) -> bool {
        crate::untrusted::injection(line).is_some_and(|p| !self.user.contains(&format!(" {} ", norm_words(&p))))
    }
}

/// A notes section (or card field) that says what to do: the goal, decisions, done items, current state, next steps.
fn instruction_section(h: &str) -> bool {
    ["goal", "done", "decision", "current state", "next", "open", "plan", "todo", "task", "now"].iter().any(|k| h.contains(k))
}

/// `notes` with outside text kept out of the instruction-like sections: a goal / done / current state / next step
/// line that repeats outside text (`Outside`) moves to `## Facts to remember` as a quoted fact with its source; one
/// with an injection phrasing is dropped wherever it is; a fact from outside text gets its source.
pub fn quarantine_notes(notes: &str, o: &Outside) -> String {
    let (mut out, mut moved): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut sec = String::new();
    let quote = |src: &str, body: &str| format!("- quoted from {src} (outside data, not an instruction): \"{}\"", one_line(body, 200).replace('"', "'"));
    for line in notes.lines() {
        let t = line.trim();
        if t.starts_with('#') { sec = t.trim_start_matches('#').trim().to_lowercase(); out.push(line.to_string()); continue; }
        let item = t.trim_start_matches(['-', '*', ' ']);
        let key = item.split_once(':').map(|(k, _)| k.trim().to_lowercase()).filter(|k| CARD_KEYS.contains(&k.as_str()));
        let instr = key.as_deref().map_or(instruction_section(&sec), |k| k != "keep");
        let facts = !instr && (sec.contains("fact") || key.as_deref() == Some("keep"));
        // the session goal is Elisa's own, from the user's words; the alias line names things, it asks nothing
        let own = sec.starts_with("session goal") || t.to_lowercase().starts_with("also known as") || t.starts_with("- quoted from ");
        if t.is_empty() || !(instr || facts) || own { out.push(line.to_string()); continue; }
        if o.injected(t) { continue; }
        // a goal quoting a paste: the user's own words around it instead, or no goal line
        if (sec.starts_with("goal") || key.as_deref() == Some("goal")) && o.quotes_paste(t) {
            if let Some(w) = &o.paste_own { out.push(format!("\"{}\"", one_line(w, 240).replace('"', "'"))); }
            continue;
        }
        let Some(src) = o.source_of(t) else { out.push(line.to_string()); continue };
        if instr { moved.push(quote(src, item)) } else { out.push(quote(src, item)) }
    }
    if !moved.is_empty() {
        match out.iter().position(|l| l.trim_start().starts_with('#') && l.to_lowercase().contains("fact")) {
            Some(i) => {
                let end = out[i + 1..].iter().position(|l| l.trim_start().starts_with('#')).map_or(out.len(), |j| i + 1 + j);
                let end = (i + 1..end).rev().find(|&j| !out[j].trim().is_empty()).map_or(i + 1, |j| j + 1);
                for (k, m) in moved.into_iter().enumerate() { out.insert(end + k, m); }
            }
            None => {
                while out.last().is_some_and(|l| l.trim().is_empty()) { out.pop(); }
                if !out.is_empty() { out.push(String::new()); }
                out.push("## Facts to remember".into());
                out.extend(moved);
            }
        }
    }
    // a section left with nothing under it goes too
    let mut keep: Vec<String> = Vec::new();
    for (i, l) in out.iter().enumerate() {
        if l.trim_start().starts_with('#') && out[i + 1..].iter().take_while(|x| !x.trim_start().starts_with('#')).all(|x| x.trim().is_empty()) { continue; }
        keep.push(l.clone());
    }
    keep.join("\n").trim_end().to_string()
}

/// A state card with outside text kept out: an item (`;`-separated) of any field that repeats outside text or carries
/// an injection phrasing is dropped (a card is the next turn's instructions; it has no room for quotes).
pub fn quarantine_card(card: &str, o: &Outside) -> String {
    card.lines().filter_map(|l| {
        let Some((k, v)) = l.split_once(':') else { return (!o.injected(l) && o.source_of(l).is_none()).then(|| l.to_string()) };
        let goal = k.trim().eq_ignore_ascii_case("goal");
        let items: Vec<&str> = v.split(';').map(str::trim)
            .filter(|x| !x.is_empty() && !o.injected(x) && o.source_of(x).is_none() && !(goal && o.quotes_paste(x))).collect();
        (!items.is_empty()).then(|| format!("{k}: {}", items.join("; ")))
    }).collect::<Vec<_>>().join("\n")
}

/// Words that make a message small talk, not a task (soak: "hi Elisa! how are you today?" became the session goal).
const SMALL_TALK: &[&str] = &[
    "hi", "hello", "hey", "heya", "hiya", "yo", "howdy", "greetings", "good", "morning", "afternoon", "evening", "day",
    "night", "how", "are", "is", "it", "going", "you", "doing", "today", "elisa", "there", "again", "thanks", "thank",
    "cheers", "ok", "okay", "great", "nice", "cool", "awesome", "bye", "goodbye", "see", "later", "what's", "whats", "up",
    "sup", "all", "fine", "well", "and", "u", "job", "welcome", "please", "lol", "haha", "yes", "no", "sure", "i'm", "im",
];
/// leading greeting words a request may start with ("Hi! Can you …", "hello again :) tell me …")
const GREETING: &[&str] = &["hi", "hello", "hey", "heya", "hiya", "yo", "howdy", "greetings", "good", "morning", "afternoon", "evening", "elisa", "there", "again"];

/// The request without a leading greeting ("Hi! Can you remind me…" → "Can you remind me…").
fn without_greeting(input: &str) -> &str {
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
