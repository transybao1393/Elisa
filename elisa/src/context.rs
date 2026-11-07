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
    let card = card.map(|c| strip_markup(&c)).filter(|c| !c.is_empty()).map(|c| if c.chars().count() > CARD_CAP { format!("{}…", head(&c, CARD_CAP)) } else { c });
    let broken = card.is_none();
    (vis.replace(CARD_ECHO, "").trim_end().to_string(), card, broken)
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

/// a capped tool result keeps at least this many chars (its head and the marker)
const RESULT_MIN_CHARS: usize = 240;

/// Follows the marker of a cut `read`: the cut took its `next: offset=N` footer, and more reads would only be cut too
/// (run 2026-11-05: 2-4 lines per read until the context was full, the offsets guessed).
pub const READ_CUT: &str = "[the context window is too full to read more of this file; answer with what you have]";

/// One result cut to `keep` chars: its head (to a line end when one is near) + `[trimmed N chars]`; a cut `read`
/// (its last line is the `[lines a-b of n]` footer) says why instead of leaving the model to page on.
fn cut_result(content: &str, keep: usize) -> String {
    let n = content.chars().count();
    if n <= keep { return content.to_string(); }
    let read = content.rsplit('\n').next().is_some_and(|l| l.starts_with("[lines "));
    let room = keep.saturating_sub(if read { 24 + READ_CUT.len() } else { 24 }).max(1);
    let mut h = head(content, room);
    if let Some(i) = h.rfind('\n').filter(|&i| i >= h.len() * 3 / 4) { h = &h[..i]; }
    let cut = n - h.chars().count();
    if read { format!("{h}\n[trimmed {cut} chars] {READ_CUT}") } else { format!("{h}\n[trimmed {cut} chars]") }
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
pub fn next_goal(current: Option<&str>, input: &str) -> Option<String> {
    let input = request_body(input).trim();
    if input.is_empty() || input.starts_with('/') || input.starts_with(NUDGE_PREFIX.trim_end()) { return current.map(String::from); }
    match (current, goal_statement(input)) {
        (_, Some(g)) => Some(g),
        (Some(c), None) => Some(c.to_string()),
        (None, None) => Some(one_line(input, 240)),
    }
}

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
        fs::write(&p, format!("{}\n", notes.trim())).map_err(|e| format!("{}: {e}", p.display()))
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
