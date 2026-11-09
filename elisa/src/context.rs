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

/// A context window this small (tokens) is tiny (`tiny_window`).
pub const TINY_CTX: usize = 4096;

/// A window too small for Elisa's usual prompt (BUGS #41, #42: moondream:1.8b's 2,048 tokens were mostly the system
/// prompt, tool rules and the first message's header, and the model read them back as its answer). Decided by the
/// window size alone, never by the model's name: such a session gets `TINY_SYSTEM`, no tools, no first-message
/// header and no per-turn hint lines.
pub fn tiny_window(max_ctx: usize) -> bool { max_ctx < TINY_CTX }

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

/// Heading of the notes block in a first message: says who wrote them (Test 2026-11-01: after a reset the model
/// treated invented note lines as things the user had said). They are this conversation's own notes (a continued
/// session's notes are its own: 2026-11-09 the old "from earlier sessions" heading went with notes of another task).
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
/// the request. A tiny window's first message (`tiny_window`; BUGS #42: moondream:1.8b with a 2K window answered
/// "hi" with "Date: OS: macos xterm …", the header read back).
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

/// A remembered exchange that recorded a lookup (the `[read notes.txt]` / `[web_search “…”]` tool line memory keeps
/// after a reply) is an old copy of what a lookup turn should fetch now: on a turn that carries the file tools
/// (`code`) a hit with a file lookup is skipped, on one that carries the web tools (`web`) a hit with a web lookup.
/// App test 2026-11-09: "read notes.txt and tell me the meeting code" recalled an earlier run's "I found the meeting
/// code in the file: ORCHID-42 … [read notes.txt]" and the model answered from it with no read (the file could have
/// changed; the user asked for a read and saw none); "search the web: …" the same with an old search answer.
pub fn stale_lookup(text: &str, code: bool, web: bool) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:^|\s)\[(read|read_symbol|outline|glob|grep|web_search|web_fetch) [^\]\n]+\]").unwrap());
    re.captures_iter(text).any(|c| if c[1].starts_with("web_") { web } else { code })
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
    // what the user sees and the history keeps: the same filter the stream display runs (`CardFilter`), so a
    // copied `[state]…[/state]` block or bare `done: / now: / keep:` lines never stay in the reply either
    let mut f = CardFilter::default();
    let vis = f.push(text) + &f.finish();
    (vis.trim_end().to_string(), card, broken)
}

/// the card block's tags: the reply's own `<state>`, and the first message's `[state]` a small model copies
/// (BUGS #22: "echo first-run [state] goal: … keep: … [/state]" as the reply, 3 turns in a row)
const CARD_OPEN: [&str; 2] = ["<state>", "[state]"];
const CARD_CLOSE: [&str; 2] = ["</state>", "[/state]"];

/// `line`'s card key (`goal`, `done`, `now`, `keep`, `next`, `open`) when it is a card line (`done: wrote x`).
fn card_key(line: &str) -> Option<&'static str> {
    let t = line.trim().trim_start_matches(['-', '*']).trim_start();
    let k = t.split_once(':')?.0.trim().to_ascii_lowercase();
    CARD_KEYS.iter().copied().find(|c| *c == k)
}

/// A line start that may still become a card line (`do`, `keep`, `- now:`): held back until it is known.
fn card_prefix(p: &str) -> bool {
    let t = p.trim_start().trim_start_matches(['-', '*']).trim_start().to_ascii_lowercase();
    if t.contains(':') { return card_key(&t).is_some(); }
    CARD_KEYS.iter().any(|k| k.starts_with(t.as_str()))
}

/// Hides state-card text from a reply, as it streams (tags and lines may be split across deltas) and in the final
/// text (`split_state`): `<state>…</state>` and `[state]…[/state]` blocks, the copied card instruction line
/// (`CARD_ECHO`), and a run of bare card lines (3+, or 2 with a `now:` / `keep:` one; BUGS #12: an approved write's
/// reply was "done: wrote "hello" to deny.txt / now: … / keep: …"). A single `next: …` line stays: it may be prose.
#[derive(Default)]
pub struct CardFilter { buf: String, inside: bool, mid: bool, cards: String, n: usize, strong: bool }

impl CardFilter {
    /// The visible part of `delta` that is safe to show now.
    pub fn push(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        let mut out = String::new();
        loop {
            if self.inside {
                match first_tag(&self.buf, &CARD_CLOSE) {
                    Some((i, n)) => { self.buf.drain(..i + n); self.inside = false; continue; }
                    None => { let p = tag_partial(&self.buf, &CARD_CLOSE); self.buf.drain(..self.buf.len() - p); return out; }
                }
            }
            if let Some(i) = self.buf.find(CARD_ECHO) { self.buf.replace_range(i..i + CARD_ECHO.len(), ""); continue; }
            // a stray closing tag with no block open (gemma3:1b: "```tool_code\noutline\n</state>\n```") is dropped
            if let Some((i, n)) = first_tag(&self.buf, &CARD_CLOSE).filter(|c| first_tag(&self.buf, &CARD_OPEN).is_none_or(|o| c.0 < o.0)) {
                self.buf.replace_range(i..i + n, "");
                continue;
            }
            let open = first_tag(&self.buf, &CARD_OPEN);
            let end = open.map_or_else(|| self.buf.len() - tag_partial(&self.buf, &[CARD_OPEN[0], CARD_OPEN[1], CARD_CLOSE[0], CARD_CLOSE[1], CARD_ECHO]), |(i, _)| i);
            let seg = self.buf[..end].to_string();
            let used = self.feed(&seg, &mut out, open.is_some());
            match open {
                Some((i, n)) => { self.buf.drain(..i + n); self.inside = true; }
                None => { self.buf.drain(..used); return out; }
            }
        }
    }

    /// End of a reply: what is still held back (nothing inside an unclosed block, no card-line run).
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        if !self.inside {
            let mut rest = std::mem::take(&mut self.buf);
            // a reply that stopped inside the copied instruction line ("…\n\n<=80-token card"): not text for the user
            let p = tag_partial(&rest, &[CARD_ECHO]);
            if p >= 6 { rest.truncate(rest.len() - p); }
            self.feed(&rest, &mut out, true);
        }
        self.flush(&mut out);
        *self = CardFilter::default();
        out
    }

    /// `seg` line by line: card lines at a line start are held, other text shown. `last`: nothing more comes before a
    /// tag (or the end), so a partial line is decided now. Returns the bytes used (the rest is a held line start).
    fn feed(&mut self, seg: &str, out: &mut String, last: bool) -> usize {
        let mut i = 0;
        while i < seg.len() {
            let rest = &seg[i..];
            let nl = rest.find('\n');
            if self.mid {
                let j = nl.map_or(rest.len(), |j| j + 1);
                out.push_str(&rest[..j]);
                i += j;
                self.mid = nl.is_none();
                continue;
            }
            let line = &rest[..nl.map_or(rest.len(), |j| j + 1)];
            if nl.is_none() && !last && card_prefix(line) { return i; }
            match card_key(line) {
                Some(k) => { self.cards.push_str(line); self.n += 1; self.strong |= k == "now" || k == "keep"; }
                None => { self.flush(out); out.push_str(line); self.mid = nl.is_none(); }
            }
            i += line.len();
        }
        i
    }

    /// The held card lines: dropped when they are a card (3+, or 2 with `now:` / `keep:`), else shown.
    fn flush(&mut self, out: &mut String) {
        if !(self.n >= 3 || (self.n >= 2 && self.strong)) { out.push_str(&self.cards); }
        (self.cards, self.n, self.strong) = (String::new(), 0, false);
    }
}

/// The first of `tags` in `s`: (byte index, tag length).
fn first_tag(s: &str, tags: &[&str]) -> Option<(usize, usize)> {
    tags.iter().filter_map(|t| s.find(t).map(|i| (i, t.len()))).min()
}

/// The longest end of `s` that starts one of `tags` (held until it resolves).
fn tag_partial(s: &str, tags: &[&str]) -> usize {
    tags.iter().map(|t| (1..t.len()).rev().find(|&n| t.is_char_boundary(n) && s.ends_with(&t[..n])).unwrap_or(0)).max().unwrap_or(0)
}

/// What memory must not keep as an answer, nor bring back (BUGS #16, #27): a refusal or "I don't know" ("I can't run
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

/// A turn's tool line (`[bash x → exit 1]`, `[web_fetch u → cancelled by user]`) for a call that did not succeed:
/// denied, cancelled, interrupted, an unknown tool, an error, or a command with a non-zero exit.
pub fn failed_tool_line(line: &str) -> bool {
    let l = line.trim();
    l.starts_with('[') && l.ends_with(']') && l.contains(" → ") && !l.ends_with(" → exit 0]")
}

/// Elisa's own answer, with no model call, on a model without vision when no picture is in the conversation (BUGS
/// #36: gemma3:1b, the image refused at attach, then "what colour are the insides of its ears?" got "Pink!"; a note
/// in the request did not stop it, 0/3): when the message came with the refused picture (`refused_now`), names a
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

/// Whether a remembered answer records a tool that ran (`[read notes.txt]`, `[bash ls → exit 0]`): memory keeps an
/// answer only from such a turn (`repl` `turn`); an older store's plain-chat answers ("I'll check how many lines
/// fizz.py prints") are not recalled either.
pub fn has_tool_line(text: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^\[[a-z_]+ [^\n]*\]$").unwrap());
    re.is_match(text)
}

/// Whether a remembered text clearly matches the question (BUGS #34: unrelated old prompts came back): it shares at
/// least 2 of the question's content words and half of them, or the question's only content word ("what's my name?"
/// finds "My name is Linh").
pub fn strong_recall(query: &str, hit: &str) -> bool {
    let q = crate::topics::words(query);
    let shared = q.intersection(&crate::topics::words(hit)).count();
    (shared >= 2 && 2 * shared >= q.len()) || (q.len() == 1 && shared == 1)
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
        // a raw tool call or result copied from a transcript is no note (2026-11-09: "[tool glob …]" and "[result: no
        // files found]" sat under "## Files touched" and were carried forward)
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

/// The line in front of the notes after `/clear` or `/reset` (a new session carries no earlier notes at all): they
/// are background, not the task (uitest 2026-11-08: an old goal carried into a new
/// session sent a 2B model on 30 web steps the user never asked for).
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

fn is_user_text(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { .. }))
}

/// a user message that is a request (the user's words, not tool results or Elisa's own correction note)
fn is_request_msg(m: &Message) -> bool {
    m.role == Role::User && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.starts_with(NUDGE_PREFIX)))
}

/// `msgs[..end]` with every attached document's text cut to its reference line (`[Attached: report.pdf, 12 pages ·
/// …]`): the first room-maker when a request is over the window (BUGS #41). A later question about the document
/// gets its parts again (`attach::follow_up`). None when there is nothing to cut.
pub fn drop_doc_text(msgs: &[Message], end: usize) -> Option<Vec<Message>> {
    let mut out = msgs.to_vec();
    let mut changed = false;
    for m in out[..end.min(msgs.len())].iter_mut().filter(|m| m.role == Role::User) {
        for b in &mut m.content {
            if let Block::Text { text } = b {
                if crate::attach::is_block(text) && text.trim_end().contains('\n') {
                    *text = crate::attach::first_line(text).to_string();
                    changed = true;
                }
            }
        }
    }
    changed.then_some(out)
}

/// `msgs` without its oldest whole turns (a turn: a request and everything up to the next one), dropped until at
/// least `free` tokens are gone, never the turn at `end` (the current request) or after it: the second room-maker
/// (BUGS #41: a 2K window stopped with "say continue" instead). The first message's notes / `[state]` / recalled
/// lines (the part before `## Request`) move to the new first message, so the notes stay in the request. None when
/// nothing can go.
pub fn drop_old_turns(msgs: &[Message], end: usize, free: usize) -> Option<Vec<Message>> {
    let end = end.min(msgs.len());
    let starts: Vec<usize> = (0..end).filter(|&i| is_request_msg(&msgs[i])).chain([end]).collect();
    let mut cut = 0;
    for &s in starts.iter().skip(1) {
        cut = s;
        if estimate_messages(&msgs[..cut]) >= free { break; }
    }
    if cut == 0 { return None; }
    let head = msgs.first().and_then(|m| m.content.iter().find_map(|b| match b { Block::Text { text } => text.rsplit_once("## Request\n").map(|(h, _)| h.to_string()), _ => None }));
    let mut out = msgs[cut..].to_vec();
    if let (Some(h), Some(Block::Text { text })) = (head, out.first_mut().and_then(|m| m.content.first_mut())) {
        if !text.contains("## Request\n") { *text = format!("{h}## Request\n{text}"); }
    }
    Some(out)
}

/// the start of a picture's text block in a user message: what the model saw when asked about the picture alone
/// (`picture_request`); the turn itself then runs as text, and the picture is never sent again with the history
pub const PICTURE_MARK: &str = "[picture: ";
/// the question asked of a picture sent with no words
pub const PICTURE_QUESTION: &str = "Describe this picture.";
pub const PICTURES_QUESTION: &str = "Describe these pictures.";

/// the picture text when no answer told anything about the picture (`picture_non_answer`, asked twice)
pub const PICTURE_UNSEEN: &str = "the model could not make out the picture";

/// A picture's text block for the turn's user message: `[picture: ANSWER]`.
pub fn picture_block(answer: &str) -> String { format!("{PICTURE_MARK}{}]", answer.trim()) }

/// a user message block that is a picture's text (`picture_block`), not the user's words
pub fn is_picture_block(text: &str) -> bool { text.starts_with(PICTURE_MARK) }

/// A picture answer that tells nothing about the picture (BUGS #57, #58): a refusal (`unfit_memory`: "I'm sorry, but
/// I can't assist with that." from granite3.2-vision:2b asked "Use your read tool on …" with the picture) or a
/// non-answer ("I'm sorry, I'm not sure what you're asking", "I didn't understand your question"). Carried into the
/// turn, the main model took it as the answer and repeated it on later turns.
pub fn picture_non_answer(answer: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)not sure what you(?:'re| are) asking|(?:didn't|did not|don't|do not) understand (?:your|the) question|\bunanswerable\b").unwrap());
    let t = answer.trim().replace('’', "'");
    // a refusal anywhere, not only first (qwen3-vl:2b-instruct asked the read ask with the picture: "That image
    // appears to be a digital illustration… I can't extract a secret word from the image you shared.")
    t.is_empty() || re.is_match(&t) || t.split_inclusive(['.', '!', '?', '\n']).any(|s| unfit_memory(s.trim()))
}

/// A reply without any `[picture: …]` text a model copied from the turn's message (BUGS #57: moondream:1.8b began
/// replies with "[picture: The image shows a black cat face…"): each such span removed through its `]` (an unclosed
/// one through the end of its line). Picture text is Elisa's request-only context, never reply text.
pub fn strip_picture_echo(reply: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?im)\[picture:[^\]\n]*(?:\]|$)").unwrap());
    if !re.is_match(reply) { return reply.to_string(); }
    let out = re.replace_all(reply, "");
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let mut s = lines.join("\n");
    while s.contains("\n\n\n") { s = s.replace("\n\n\n", "\n\n"); }
    s.trim().to_string()
}

/// The history without the picture text of earlier turns (BUGS #57): a picture's text belongs to the turn that looked
/// at it (`picture_request`); later turns have the reply that answered it. Run before a new turn's message is added,
/// never within a turn (the prefix rule). Whether anything was removed.
pub fn drop_picture_blocks(history: &mut [Message]) -> bool {
    let mut any = false;
    for m in history.iter_mut().filter(|m| m.role == Role::User) {
        // never the first block: that is the user's own words, even when they look like picture text
        let n = m.content.len();
        let mut i = 0;
        m.content.retain(|b| { i += 1; i == 1 || !matches!(b, Block::Text { text } if is_picture_block(text)) });
        any |= m.content.len() != n;
    }
    any
}

/// what an image part becomes in a request to a model without vision (`without_images`)
pub const IMAGE_STUB: &str = "[a picture was shared here; this model can't see pictures]";

/// The messages with every image part replaced by `IMAGE_STUB`, for a model without vision (BUGS #62: a session saved
/// before pictures went in their own request still held image parts, and after a switch to granite4.2:3b / gemma3:1b
/// every turn failed with "image input is not supported" / "Multimodal data provided"). None when there is none. The
/// same messages always give the same request, so the prefix stays stable.
pub fn without_images(msgs: &[Message]) -> Option<Vec<Message>> {
    if !msgs.iter().any(|m| m.content.iter().any(|b| matches!(b, Block::Image { .. }))) { return None; }
    Some(msgs.iter().map(|m| {
        let mut m = m.clone();
        for b in m.content.iter_mut() {
            if matches!(b, Block::Image { .. }) { *b = Block::Text { text: IMAGE_STUB.into() }; }
        }
        m
    }).collect())
}

/// The line after a resumed conversation's restored turns (BUGS #65: after a model switch, the capabilities and
/// "resumed N messages" were drawn above the history, so only the model chip showed the change): which model now
/// answers and what it can do, e.g. "Now on granite4.2:3b · no vision · tools · thinking".
pub fn now_on(model: &str, vision: bool, native_tools: bool, thinking: bool) -> String {
    let mut s = format!("Now on {model} · {}", if vision { "vision" } else { "no vision" });
    s.push_str(if native_tools { " · tools" } else { " · tools via the prompt" });
    if thinking { s.push_str(" · thinking"); }
    s
}

/// The isolated picture request (BUGS #37, #17): no system prompt, no history, the pictures alone in one message and
/// then the question (the user's words, or `PICTURE_QUESTION` when there are none). Measured on Ollama 2026-11-09:
/// granite3.2-vision:2b answered "unanswerable" or "I can't … corrupted image" with any system prompt, with earlier
/// history, and even with the question in the pictures' own message, and read the picture with this shape;
/// qwen3-vl:2b-instruct and moondream:1.8b read it the same way. The question ends with a line break: moondream:1.8b
/// ended its answer at once (empty reply) on a question ending in "?" without one.
pub fn picture_request(question: &str, pics: &[Block]) -> Vec<Message> {
    let q = question.trim();
    let q = if !q.is_empty() { q } else if pics.len() > 1 { PICTURES_QUESTION } else { PICTURE_QUESTION };
    vec![Message::new(Role::User, pics.to_vec()), Message::user_text(format!("{q}\n"))]
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
    // a small-talk goal is no goal: the section goes (2026-11-09: "hey" was stored and carried forward)
    if small_talk_goal(goal) { return rest; }
    let sec = format!("{GOAL_NOTES}\n{}", users_words(&one_line(goal, 240)));
    if rest.is_empty() { sec } else { format!("{sec}\n\n{rest}") }
}

/// The clauses of the user's own words (a statement sentence's parts split at `,` `;` and before "and my / and I"),
/// questions left out.
fn user_clauses(text: &str) -> Vec<String> {
    static AND: OnceLock<regex::Regex> = OnceLock::new();
    let and = AND.get_or_init(|| regex::Regex::new(r"(?i)\s+(?:and|but|also)\s+((?:my|i|i'm|i am)\b)").unwrap());
    let body = request_body(text).trim();
    if body.starts_with('/') || body.starts_with(NUDGE_PREFIX.trim_end()) { return Vec::new(); }
    let Some(own) = own_words(body) else { return Vec::new() };
    let own = without_greeting(own).replace(['’', '‘'], "'");
    // sentences end at . ! ? before a space / the end, or at a line break (not inside "v1.2" or "example.com")
    let mut sentences: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let cs: Vec<char> = own.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        let end = c == '\n' || (matches!(c, '.' | '!' | '?') && cs.get(i + 1).is_none_or(|n| n.is_whitespace()));
        if !end { cur.push(c); continue; }
        sentences.push((std::mem::take(&mut cur), c == '?'));
    }
    sentences.push((cur, false));
    sentences.into_iter().filter(|(s, q)| !q && !s.trim().is_empty())
        .flat_map(|(s, _)| and.replace_all(&s, ", $1").split([',', ';']).map(|c| c.trim().to_string()).collect::<Vec<_>>())
        .filter(|c| !c.is_empty()).collect()
}

/// A fact the user stated about themself in one clause, said in the third person ("my name is Linh" → "The user's
/// name is Linh"). 2026-11-09 (qwen3-vl:2b): after /compact the notes held "My name is Linh, …" as the session goal,
/// written by Elisa, and "What's my name?" got "My name is Elisa": the user's facts must never read as Elisa's.
fn user_fact(clause: &str) -> Option<String> { user_fact_kv(clause).map(|(f, _)| f) }

/// `user_fact` and the fact's value ("Linh", "Hanoi", "ORCHID-42").
fn user_fact_kv(clause: &str) -> Option<(String, String)> {
    static RES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| [
        (r"^my name(?:'s|\s+is)\s+(?P<v>.+)$", "The user's name is $v"),
        (r"^(?:i'm|i\s+am)\s+called\s+(?P<v>.+)$", "The user's name is $v"),
        // "I'm Linh": one capitalised word (checked below), never "I'm tired"
        (r"^(?:i'm|i\s+am)\s+(?P<n>[\p{L}][\p{L}'-]*)$", "The user's name is $n"),
        (r"^(?:please\s+)?call\s+me\s+(?P<v>.+)$", "The user wants to be called $v"),
        (r"^i\s+(?:live|stay)\s+in\s+(?P<v>.+)$", "The user lives in $v"),
        (r"^(?:i'm|i\s+am)\s+from\s+(?P<v>.+)$", "The user is from $v"),
        (r"^i\s+work\s+(?P<p>as|at|for|in)\s+(?P<v>.+)$", "The user works $p $v"),
        (r"^(?:i'm|i\s+am)\s+(?P<v>\d{1,3})\s+years?\s+old$", "The user is $v years old"),
        // any other "my X is Y" (X up to 4 words): "my favourite colour is green", "my meeting code's ORCHID-42"
        (r"^my\s+(?P<k>[a-z][a-z'-]*(?:\s+[a-z][a-z'-]*){0,3}?)(?:'s|\s+(?P<b>is|are))\s+(?P<v>.+)$", ""),
    ].into_iter().map(|(r, t)| (regex::Regex::new(&format!("(?i){r}")).unwrap(), t)).collect());
    let c = clause.trim().trim_end_matches(['.', '!', ';']).trim();
    for (re, tmpl) in res {
        let Some(m) = re.captures(c) else { continue };
        if let Some(n) = m.name("n") {
            if !n.as_str().starts_with(char::is_uppercase) { continue; }
            return Some((format!("The user's name is {}", n.as_str()), n.as_str().to_string()));
        }
        let v = m.name("v").map_or("", |v| v.as_str()).trim().trim_matches(['"', '\'', '`']).trim();
        let words = v.split_whitespace().count();
        if words == 0 || words > 6 || v.contains('?') { return None; }
        let out = match m.name("k") {
            Some(k) => format!("The user's {} {} {v}", k.as_str(), m.name("b").map_or("is", |b| b.as_str())),
            // the value without its quotes: expanded from the trimmed text
            None => { let mut o = String::new(); m.expand(tmpl, &mut o); o.replacen(m.name("v").map_or("", |x| x.as_str()), v, 1) }
        };
        return Some((out.split_whitespace().collect::<Vec<_>>().join(" "), v.to_string()));
    }
    None
}

/// The values of the facts the user stated about themself in `text` ("Linh", "Hanoi", "ORCHID-42"; `user_facts`).
pub fn user_fact_values(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (_, x) in user_clauses(text).iter().filter_map(|c| user_fact_kv(c)) { if !v.contains(&x) { v.push(x); } }
    v
}

/// The user's fact values (`user_fact_values`) a reply says as Elisa's own (bug #69: "I'm Elisa, and I live in
/// Hanoi. My meeting code is ORCHID-42."; "I'm from Hanoi too"): a clause of the reply with a first-person word and,
/// after it, a value, and no "you" / "your" (that clause speaks to the user: "I'm glad you're from Hanoi"). Only
/// the session's own fact values are looked for, never fixed words of a reply. Empty when none is claimed.
// a few first- and second-person words, English and Vietnamese, kept short on purpose (user 2026-11-09: "can be
// hardcoded but not too much"); never grow it per failure case
pub fn claimed_user_facts(reply: &str, values: &[String]) -> Vec<String> {
    static FIRST: OnceLock<regex::Regex> = OnceLock::new();
    static YOU: OnceLock<regex::Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| regex::Regex::new(r"(?i)\b(?:i|me|my|mine|myself|i'm|i've|i'd|i'll|tôi|mình|tớ|em)\b").unwrap());
    let you = YOU.get_or_init(|| regex::Regex::new(r"(?i)\b(?:you|your|yours|you're|you've|you'll|you'd|yourself|bạn|cậu|anh|chị)\b").unwrap());
    let (text, _, _) = split_state(reply);
    let text = text.replace(['’', '‘'], "'");
    let mut out: Vec<String> = Vec::new();
    for clause in text.split(['.', ',', ';', '!', '?', '\n', ':', '—']) {
        if you.is_match(clause) { continue; }
        let Some(at) = first.find(clause).map(|m| m.end()) else { continue };
        let after = clause[at..].to_lowercase();
        for v in values {
            let lv = v.to_lowercase();
            let hit = after.match_indices(&lv).any(|(i, _)| {
                let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                edge(after[..i].chars().next_back()) && edge(after[i + lv.len()..].chars().next())
            });
            if !lv.is_empty() && hit && !out.contains(v) { out.push(v.clone()); }
        }
    }
    out
}

/// The facts the user stated about themself in `text` (their own words: `request_body`, `own_words`), in the third
/// person (`user_fact`), for the notes' `Facts to remember`: a summary may drop them or write them so they read as
/// Elisa's ("Name: Linh", a quoted "My name is Linh").
pub fn user_facts(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for f in user_clauses(text).iter().filter_map(|c| user_fact(c)) { if !v.contains(&f) { v.push(f); } }
    v
}

/// A fact in the third person (`user_fact`'s "The user's name is Linh") as said to the user ("your name is Linh"),
/// None for any other line. Only Elisa's own templates are read, never the user's or a model's wording.
pub fn fact_to_you(fact: &str) -> Option<String> {
    let f = fact.trim();
    let rest = f.strip_prefix("The user")?;
    let you = if let Some(r) = rest.strip_prefix("'s ") { format!("your {r}") }
        else if let Some(r) = rest.strip_prefix(" is ") { format!("you are {r}") }
        else if let Some(r) = rest.strip_prefix(" wants ") { format!("you want {r}") }
        else if let Some(r) = rest.strip_prefix(" lives ") { format!("you live {r}") }
        else if let Some(r) = rest.strip_prefix(" works ") { format!("you work {r}") }
        else { return None };
    Some(you)
}

/// Whether a request only tells facts about the user ("My name is Linh, I live in Hanoi"): no session goal.
pub fn only_user_facts(text: &str) -> bool {
    let cl = user_clauses(text);
    !cl.is_empty() && cl.iter().all(|c| user_fact(c).is_some())
}

/// `notes` with `facts` (`user_facts`) in `## Facts to remember` (merged, duplicates dropped).
pub fn with_user_facts(notes: &str, facts: &[String]) -> String {
    if facts.is_empty() { return notes.to_string(); }
    let add = format!("## Facts to remember\n{}", facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n"));
    merge_notes(Some(notes), &add)
}

/// The user's words as Elisa shows them in notes or `[state]`: a stated fact in the third person (`user_facts`),
/// first-person words quoted as the user's (`the user said: "fix my build"`), anything else as it is. "I", "me" and
/// "my" outside the user's own messages would read as Elisa's (2026-11-09: "My name is Elisa").
pub fn users_words(text: &str) -> String {
    let t = text.trim();
    if only_user_facts(t) { return user_facts(t).join("; "); }
    static FIRST: OnceLock<regex::Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| regex::Regex::new(r"(?i)\b(?:i|me|my|mine|myself|i'm|i've|i'd|i'll)\b").unwrap());
    if first.is_match(&t.replace('’', "'")) && !t.starts_with("the user said:") { format!("the user said: \"{}\"", t.trim_matches('"')) } else { t.to_string() }
}

/// what an old assistant answer keeps in a request when the history is long (`shorten_old_replies`), in tokens
pub const SHORT_REPLY_TOKENS: usize = 120;
/// the marker after a shortened answer
pub const SHORTENED: &str = " …[shortened]";
/// the history share of the window (percent) above which old answers are shortened in requests
pub const SHORTEN_AT_PCT: usize = 35;

/// The history as a request sends it when it is long (over SHORTEN_AT_PCT of `max_ctx`): every assistant answer
/// before `end` (the current turn's request) except the last 2 keeps only its first SHORT_REPLY_TOKENS tokens plus
/// SHORTENED. For requests only, never the saved history; None when nothing changes. 2026-11-09: a resumed PDF chat
/// carried ~7K tokens of a 2B model's long padded answers, which squeezed each question's document budget to its 2K
/// floor (1-2 pages), and answers came out partly made up.
pub fn shorten_old_replies(msgs: &[Message], end: usize, max_ctx: usize) -> Option<Vec<Message>> {
    if estimate_messages(msgs) * 100 <= max_ctx * SHORTEN_AT_PCT { return None; }
    let answers: Vec<usize> = msgs[..end.min(msgs.len())].iter().enumerate()
        .filter(|(_, m)| m.role == Role::Assistant && m.content.iter().any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty())))
        .map(|(i, _)| i).collect();
    let old = &answers[..answers.len().saturating_sub(2)];
    let max_chars = SHORT_REPLY_TOKENS * 33 / 10;
    let mut out: Option<Vec<Message>> = None;
    for &i in old {
        let m = &msgs[i];
        if !m.content.iter().any(|b| matches!(b, Block::Text { text } if text.chars().count() > max_chars + SHORTENED.len())) { continue; }
        let v = out.get_or_insert_with(|| msgs.to_vec());
        for b in v[i].content.iter_mut() {
            if let Block::Text { text } = b {
                if text.chars().count() <= max_chars + SHORTENED.len() { continue; }
                let cut: String = text.chars().take(max_chars).collect();
                // at a word boundary when there is one near the end
                let cut = match cut.rfind(char::is_whitespace) { Some(j) if j > max_chars * 3 / 4 => cut[..j].to_string(), _ => cut };
                *text = format!("{}{SHORTENED}", cut.trim_end());
            }
        }
    }
    out
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
                    // an attached document (`attach.rs`): its name line only, never its text, reaches the notes
                    let t = if m.role == Role::User && crate::attach::is_block(t) { crate::attach::first_line(t) } else { t };
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
pub fn run_id() -> u64 {
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

    /// This session's notes: `.elisa/notes/<session id>.md`, one file per session, so a new session never overwrites
    /// or loses an earlier one's (2026-11-09: one shared `notes.md` was carried into every new session and a 2B model
    /// answered "hey" with the earlier session's task).
    fn notes_path(&self) -> PathBuf { self.notes_path_of(run_id()) }
    fn notes_path_of(&self, id: u64) -> PathBuf { self.dir.join("notes").join(format!("{id}.md")) }
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

    /// This session's own notes (None for a new session: it starts clean).
    pub fn load_notes(&self) -> Option<String> { fs::read_to_string(self.notes_path()).ok().and_then(non_empty) }

    /// The notes of saved session `id` (for `--continue` / `/resume`): only that session's own
    /// `.elisa/notes/<id>.md`. The shared `.elisa/notes.md` of Elisa before per-session notes is never read
    /// (2026-11-09: it came back with `--continue`, and an old Python-async task's facts and a "hey" session goal were
    /// merged forward into a PDF conversation's notes by every fold).
    pub fn session_notes(&self, id: u64) -> Option<String> {
        fs::read_to_string(self.notes_path_of(id)).ok().and_then(non_empty)
    }

    /// Saves this session's notes (`.elisa/notes/<session id>.md`); earlier sessions' notes are not touched.
    pub fn save_notes(&self, notes: &str) -> Result<(), String> {
        let p = self.notes_path();
        let dir = p.parent().unwrap_or(&self.dir);
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
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
        let mut msgs = redact_messages(msgs);
        // a turn's situation line / follow-up hint is that request's only: the saved user words go without it
        for m in msgs.iter_mut().filter(|m| m.role == Role::User) {
            for b in m.content.iter_mut() { if let Block::Text { text } = b { if let Some(t) = strip_turn_lines(text) { *text = t; } } }
        }
        let msgs = &msgs[..];
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

    /// The newest earlier session with real content (`real_content`), up to 20 back; a file over 2 MB counts as
    /// real without being read. A terminal `--continue` and the explicit "continue the last one" use it, so a dead
    /// session ("continue the last one" → the generic intro) never shadows the real one under it. `/resume` lists
    /// everything.
    pub fn newest_real_session(&self) -> Option<(u64, PathBuf)> {
        let own = run_id();
        let mut v: Vec<(u64, PathBuf)> = fs::read_dir(self.sessions_dir()).ok()?.filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "json" { return None; }
            let ts: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, p))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.into_iter().take(20).find(|(_, p)| {
            if fs::metadata(p).is_ok_and(|m| m.len() > 2_000_000) { return true; }
            self.load_session(p).is_some_and(|h| real_content(&h))
        })
    }

    /// Newest saved session from an earlier run (this run's own file is skipped).
    pub fn load_last_session(&self) -> Option<Vec<Message>> { self.load_last_session_with_notes().map(|(h, _)| h) }

    /// Newest saved session from an earlier run with its own notes (`--continue`).
    pub fn load_last_session_with_notes(&self) -> Option<(Vec<Message>, Option<String>)> {
        let (id, p) = self.newest_real_session()?;
        let h = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
        Some((h, self.session_notes(id)))
    }

    /// The newest earlier session with any message, up to 20 back: the conversation that was open last, whatever was
    /// said in it. `--continue-open` takes it: the app's restart of the open conversation (a model switch, a
    /// reconnect, a dev reload) (BUGS #63: a one-turn "hi" chat was skipped as not
    /// `real_content`, and the switch said "no saved session to continue", 6 of 10 quick switches).
    pub fn newest_session(&self) -> Option<(u64, PathBuf)> {
        let own = run_id();
        let mut v: Vec<(u64, PathBuf)> = fs::read_dir(self.sessions_dir()).ok()?.filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "json" { return None; }
            let ts: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, p))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.into_iter().take(20).find(|(_, p)| fs::metadata(p).is_ok_and(|m| m.len() > 2_000_000) || self.load_session(p).is_some_and(|h| !h.is_empty()))
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

    /// The notes of the saved session at `path` (`/resume`).
    pub fn load_session_notes(&self, path: &Path) -> Option<String> {
        self.session_notes(path.file_stem()?.to_str()?.parse().ok()?)
    }

    fn card_reset_path(&self) -> PathBuf { self.dir.join("card-reset") }

    /// /reset and /forget dropped the state card: cards up to `id` (elisa-mem ids) are not carried into a new session.
    pub fn save_card_reset(&self, id: u64) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.card_reset_path(), format!("{id}\n")).map_err(|e| format!("{}: {e}", self.card_reset_path().display()))
    }

    /// The newest card id a /reset or /forget dropped (None = never).
    pub fn card_reset(&self) -> Option<u64> { fs::read_to_string(self.card_reset_path()).ok()?.trim().parse().ok() }

    fn forgot_path(&self) -> PathBuf { self.dir.join("memory-forgotten") }

    /// /forget: this project's memories (elisa-mem ids) below `next` are never recalled again (the store is
    /// append-only; the saved conversation files are kept).
    pub fn save_forgot(&self, next: u64) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.forgot_path(), format!("{next}\n")).map_err(|e| format!("{}: {e}", self.forgot_path().display()))
    }

    /// The memory id the last /forget stopped at: this project's memories below it are forgotten (None = never).
    pub fn forgot(&self) -> Option<u64> { fs::read_to_string(self.forgot_path()).ok()?.trim().parse().ok() }

    fn cards_path_of(&self, id: u64) -> PathBuf { self.dir.join("session-meta").join(format!("{id}.cards.json")) }

    /// This session's state cards with their topics (`topics.rs`), beside its `.meta`: what `--continue` / `/resume`
    /// load (nothing is sent until a message picks one). None written while there is none.
    pub fn save_cards(&self, cards: &[crate::topics::Card]) {
        if cards.is_empty() { return; }
        let p = self.cards_path_of(run_id());
        let redacted: Vec<crate::topics::Card> = cards.iter().map(|c| crate::topics::Card { text: crate::secrets::redact(&c.text).0, ..c.clone() }).collect();
        if let (Some(dir), Ok(json)) = (p.parent(), serde_json::to_string(&redacted)) {
            if fs::create_dir_all(dir).is_ok() { let _ = fs::write(&p, json); }
        }
    }

    /// The state cards saved with session `id` (`save_cards`); None for an older session without them.
    pub fn load_cards(&self, id: u64) -> Option<Vec<crate::topics::Card>> {
        serde_json::from_str(&fs::read_to_string(self.cards_path_of(id)).ok()?).ok()
    }
}

/// `text` (a first message) without the state card lines in its `[state]` block, the block gone when nothing else is
/// left in it (the session goal and pinned facts stay). None when there is no card in it. An older Elisa put the
/// latest card there; a resumed session sends a card only when the turn's topic picks it (`topics.rs`).
pub fn strip_state_card(text: &str) -> Option<String> {
    let i = text.find("[state]\n")?;
    let body_at = i + "[state]\n".len();
    let j = body_at + text[body_at..].find("[/state]")?;
    let body = &text[body_at..j];
    let is_card = |l: &str| l.split_once(':').is_some_and(|(k, _)| CARD_KEYS.contains(&k.trim().to_ascii_lowercase().as_str()));
    if !body.lines().any(is_card) { return None; }
    let keep: Vec<&str> = body.lines().filter(|l| !is_card(l)).filter(|l| !l.trim().is_empty()).collect();
    let end = j + "[/state]".len();
    let after = text[end..].strip_prefix("\n\n").or_else(|| text[end..].strip_prefix('\n')).unwrap_or(&text[end..]);
    Some(if keep.is_empty() { format!("{}{after}", &text[..i]) } else { format!("{}[state]\n{}\n[/state]\n\n{after}", &text[..i], keep.join("\n")) })
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
    // "OS: …": a routed chat first turn's header, without the working directory (`repl.rs` `route_cwd`);
    // "Local time now: …": the clock lines in front of a routed time turn (`repl.rs`): both may lead, in either order
    let header = |t: &str| t.starts_with("Working directory: ") || t.starts_with("Local time now: ") || (t.starts_with("OS: ") && t.contains("\nDate: "));
    let body = match text.rsplit_once("## Request\n") {
        Some((_, r)) => r,
        None => {
            let mut t = text;
            for _ in 0..2 { if header(t) { t = t.split_once("\n\n").map_or(t, |(_, r)| r); } }
            t
        }
    };
    // a small-talk turn's situation line, or a short follow-up's hint, sits right before the user's words
    // (`situation.rs`, `followup.rs`)
    crate::followup::strip(crate::situation::strip(strip_memory(crate::route::strip_ask(crate::reply_lang::strip_hint(body)))))
}

/// turns of a resumed conversation shown again (`earlier_turns`)
pub const EARLIER_TURNS: usize = 40;

/// A resumed conversation's earlier turns as the user saw them, for the display (the app's `history` event, the
/// TUI): the user's own words (no Working directory / OS / Date / clock header, notes, `[state]`, recalled memories,
/// situation or follow-up hints; an attached document as its reference line only) and the replies as written, a
/// tool call as one line ("used read on README.md"), tool results left out. The last `max_turns` user turns.
pub fn earlier_turns(history: &[Message], cwd: &Path, max_turns: usize) -> Vec<crate::frontend::Earlier> {
    use crate::frontend::Earlier;
    let mut out: Vec<Earlier> = Vec::new();
    for m in history {
        match m.role {
            Role::User => {
                let mut words = String::new();
                let mut refs: Vec<String> = Vec::new();
                for b in &m.content {
                    let Block::Text { text } = b else { continue };
                    if text.starts_with(NUDGE_PREFIX) || is_picture_block(text) { continue; }
                    if crate::attach::is_block(text) {
                        if text.starts_with(crate::attach::MARK) { refs.push(crate::attach::first_line(text).to_string()); }
                        continue;
                    }
                    let t = request_body(text);
                    let t = strip_turn_lines(t).unwrap_or_else(|| t.to_string());
                    if !t.trim().is_empty() { if !words.is_empty() { words.push('\n'); } words += t.trim(); }
                }
                for r in refs { if !words.is_empty() { words.push('\n'); } words += &r; }
                if !words.is_empty() { out.push(Earlier { role: "user".into(), text: words }); }
            }
            Role::Assistant => {
                let mut t = String::new();
                for b in &m.content {
                    let line = match b {
                        Block::Text { text } => split_state(text).0.trim().to_string(),
                        Block::ToolUse { name, input, .. } => match crate::tui::act_target(name, input, cwd).filter(|x| !x.is_empty()) {
                            Some(x) => format!("used {name} on {}", one_line(&x, 80)),
                            None => format!("used {name}"),
                        },
                        _ => String::new(),
                    };
                    if line.is_empty() { continue; }
                    if !t.is_empty() { t.push_str(if matches!(b, Block::ToolUse { .. }) { "\n" } else { "\n\n" }); }
                    t += &line;
                }
                if t.is_empty() { continue; }
                // the steps of one turn (tool calls, then the answer) are one reply
                match out.last_mut() {
                    Some(l) if l.role == "assistant" => { l.text.push_str("\n\n"); l.text += &t; }
                    _ => out.push(Earlier { role: "assistant".into(), text: t }),
                }
            }
        }
    }
    let users: Vec<usize> = out.iter().enumerate().filter(|(_, e)| e.role == "user").map(|(i, _)| i).collect();
    if users.len() > max_turns { out.drain(..users[users.len() - max_turns]); }
    out
}

/// `text` without its situation line / follow-up hint paragraph (`situation.rs`, `followup.rs`), None when it has
/// neither.
pub fn strip_turn_lines(text: &str) -> Option<String> {
    let line = |p: &str| (p.starts_with(crate::situation::PREFIX) || p.starts_with(crate::followup::PREFIX)) && p.ends_with(')') && !p.contains('\n');
    if !text.split("\n\n").any(line) { return None; }
    Some(text.split("\n\n").filter(|p| !line(p)).collect::<Vec<_>>().join("\n\n"))
}

/// A reply that is only a greeting or Elisa's generic intro ("Hello! How can I help you today?", "I'm Elisa, a
/// personal assistant on your Mac…"), or Elisa's own "no earlier conversation" line: nothing to follow up on or
/// continue.
pub fn is_intro(text: &str) -> bool {
    let l = text.trim().to_lowercase();
    if l.is_empty() { return true; }
    let intro = ["i'm elisa", "i am elisa", "a personal assistant on your mac", "your personal assistant on mac", "your personal assistant on your mac", "there's no earlier conversation", "chưa có cuộc trò chuyện nào trước đó", "tôi là elisa"];
    let hello = ["how can i help", "what can i help", "how can i assist", "what would you like to do next", "giúp gì", "bạn muốn làm gì tiếp theo"];
    intro.iter().any(|p| l.contains(p)) || (l.chars().count() <= 60 && hello.iter().any(|p| l.contains(p)))
}

/// A saved session worth continuing: some user turn that is more than a greeting, small talk, a short follow-up or
/// a continue ask (or carries an attached document), and some reply that is more than a greeting / the intro.
pub fn real_content(h: &[Message]) -> bool {
    let user = h.iter().filter(|m| m.role == Role::User).any(|m| {
        let t: Vec<&str> = m.content.iter().filter_map(|b| match b { Block::Text { text } => Some(text.as_str()), _ => None }).collect();
        t.len() > 1 || t.first().is_some_and(|t| {
            let w = request_body(t).trim();
            !w.is_empty() && !crate::route::small_talk(w) && !crate::route::ack(w) && !crate::route::is_follow_up(w) && crate::route::resume_intent(w).is_none() && crate::followup::kind(w).is_none()
        })
    });
    let reply = h.iter().filter(|m| m.role == Role::Assistant).any(|m| m.content.iter().any(|b| match b {
        Block::ToolUse { .. } => true,
        Block::Text { text } => !text.trim_start().starts_with('[') && !is_intro(text),
        _ => false,
    }));
    user && reply
}

/// The request's messages with a repeated reply dropped: when the last two answered turns got the same reply (the
/// 2B model then copies it a third time: measured, the intro 3 times running), the older turn (its user message and
/// reply) goes from the request, until the last two differ. None when nothing repeats. The history is unchanged.
pub fn drop_repeated_replies(msgs: &[Message]) -> Option<Vec<Message>> {
    let user_text = |m: &Message| m.role == Role::User && m.content.iter().all(|b| matches!(b, Block::Text { .. }));
    let reply = |m: &Message| (m.role == Role::Assistant && m.content.iter().all(|b| matches!(b, Block::Text { .. })))
        .then(|| m.content.iter().filter_map(|b| match b { Block::Text { text } => Some(text.trim()), _ => None }).collect::<Vec<_>>().join("\n"));
    let mut v = msgs.to_vec();
    let mut changed = false;
    loop {
        // answered turns: (index of the user message, its reply text)
        let pairs: Vec<(usize, String)> = (0..v.len().saturating_sub(1)).filter(|&i| user_text(&v[i])).filter_map(|i| reply(&v[i + 1]).map(|r| (i, r))).collect();
        let n = pairs.len();
        if n < 2 { break; }
        let ((i1, r1), (i2, r2)) = (&pairs[n - 2], &pairs[n - 1]);
        // adjacent turns with the same non-empty reply; never the first message (it carries the session's header)
        if *i1 == 0 || i1 + 2 != *i2 || r1.is_empty() || r1 != r2 { break; }
        v.drain(*i1..i1 + 2);
        changed = true;
    }
    changed.then_some(v)
}

/// A loaded session: an assistant message with no recorded model gets "" (unknown: saved before models were
/// recorded), so stamping on the next save (`stamp_model`) never claims it for this run's model.
pub fn loaded(mut h: Vec<Message>) -> Vec<Message> {
    for m in h.iter_mut().filter(|m| m.role == Role::Assistant && m.model.is_none()) { m.model = Some(String::new()); }
    h
}

/// The assistant messages written in this run (no model recorded yet) get `model` (bug #66).
pub fn stamp_model(h: &mut [Message], model: &str) {
    for m in h.iter_mut().filter(|m| m.role == Role::Assistant && m.model.is_none()) { m.model = Some(model.to_string()); }
}

/// how the block of another model's turns starts in a request (`other_model_view`)
pub const EARLIER_OPEN: &str = "[earlier in this chat";
/// an earlier ask in that block, at most this many chars
pub const EARLIER_ASK_CHARS: usize = 200;

/// The request's messages when the history holds turns another model answered (bug #66: after a model switch,
/// small models took the old model's replies in the assistant role as their own and copied them, the old model's
/// name included: granite, gemma and granite3.2-vision each said "I'm Qwen3-VL"). Each such turn leaves the
/// assistant role and its reply leaves the request: the user's words (as Elisa writes them, `users_words`: a fact in
/// the third person, other first-person words quoted) become one line in an `[earlier in this chat …]` block at the
/// front of the next turn this model answers (the first message's header stays first). Measured: the old replies
/// kept in that block, labelled "Elisa on granite4.2:3b: …", were continued as a transcript by gemma3:1b ("Elisa on
/// granite4.2:3b: No, I cannot see pictures."); with the replies left out every model answered as itself. A turn
/// whose ask the user repeats later is left out too. `now` is the current model's facts (`situation::model_now`).
/// A tiny window gets the user's lines only. Decided from the recorded models alone, never from reply wording; for
/// requests only (the history is unchanged). None when no turn was answered by another model.
pub fn other_model_view(msgs: &[Message], model: &str, now: &str, tiny: bool) -> Option<Vec<Message>> {
    let other = |m: &Message| m.role == Role::Assistant && m.model.as_deref().is_some_and(|x| !x.is_empty() && x != model);
    if !msgs.iter().any(other) { return None; }
    let starts: Vec<usize> = (0..msgs.len()).filter(|&i| is_request_msg(&msgs[i])).collect();
    if starts.first() != Some(&0) { return None; }
    // the user's own words of a request message (no header, hints, memory; a document as its reference line)
    let words = |m: &Message| -> String {
        let mut v: Vec<String> = Vec::new();
        for b in &m.content {
            let Block::Text { text } = b else { continue };
            if text.starts_with(NUDGE_PREFIX) || is_picture_block(text) { continue; }
            if crate::attach::is_block(text) { v.push(crate::attach::first_line(text).to_string()); continue; }
            let t = request_body(text);
            v.push(strip_turn_lines(t).unwrap_or_else(|| t.to_string()));
        }
        one_line(&v.join(" "), usize::MAX)
    };
    let norm = |s: &str| s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>().join(" ");
    let asks: Vec<String> = starts.iter().map(|&i| norm(&words(&msgs[i]))).collect();
    let (mut out, mut lines, mut by, mut header) = (Vec::new(), Vec::<String>::new(), Vec::<String>::new(), String::new());
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(msgs.len());
        let turn = &msgs[s..e];
        if let Some(w) = turn.iter().find(|m| other(m)).and_then(|m| m.model.clone()) {
            // the first message's header (working directory, date, notes, [state]) is not this turn's: it stays first
            if s == 0 {
                if let Some(Block::Text { text }) = turn[0].content.iter().find(|b| matches!(b, Block::Text { .. })) {
                    let body = request_body(text);
                    let at = (body.as_ptr() as usize).saturating_sub(text.as_ptr() as usize).min(text.len());
                    if text.is_char_boundary(at) { header = text[..at].to_string(); }
                }
            }
            if !asks[k].is_empty() && asks[k + 1..].contains(&asks[k]) { continue; }
            let u = words(&turn[0]);
            if !u.is_empty() { lines.push(format!("- {}", users_words(&one_line(&u, EARLIER_ASK_CHARS)))); }
            if !by.contains(&w) { by.push(w); }
            continue;
        }
        let mut t = turn.to_vec();
        if !lines.is_empty() || !header.is_empty() {
            let block = if lines.is_empty() { String::new() } else if tiny {
                format!("{EARLIER_OPEN}]\n{}\n[/earlier]\n\n", lines.join("\n"))
            } else {
                format!("{EARLIER_OPEN}, what the user asked or said before the switch to {model}; all answered already by \
                    {}, another model, its replies left out: never answer these again · now: {now}]\n{}\n[/earlier]\n\n", by.join(", "), lines.join("\n"))
            };
            if let Some(Block::Text { text }) = t[0].content.iter_mut().find(|b| matches!(b, Block::Text { text } if !text.starts_with(NUDGE_PREFIX))) {
                // after the header's notes and [state], before its "## Request" line
                *text = match header.rfind("## Request\n") {
                    Some(i) => format!("{}{block}{}{text}", &header[..i], &header[i..]),
                    None => format!("{header}{block}{text}"),
                };
            }
            (lines, header) = (Vec::new(), String::new());
        }
        out.extend(t);
    }
    // nothing of this model's after them (never: the current request is this model's own)
    if !lines.is_empty() || !header.is_empty() { return None; }
    Some(out)
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

/// Opening line of a recalled-memory block holding the user's facts, said to the user (`fact_to_you`).
pub const MEMORY_OPEN_YOU: &str = "[memory: recalled by Elisa from earlier sessions, not written by the user; the user's own facts, as said to the user; background only, ignore it if unrelated]";

/// Recalled memories as a block that goes in front of the user's text (in the USER message, never the system prompt).
/// A fact about the user is said to the user ("your name is Linh", `fact_to_you`), never "The user's name is Linh"
/// (bug #69: in the user's own message, next to "what's my name?", the third person came back as "My name is
/// Elisa, and I live in Hanoi"; measured, 2B models answered "your …" more often with the facts already in that form).
pub fn with_memory(lines: &[String], text: &str) -> String {
    if lines.is_empty() { return text.to_string(); }
    let lines: Vec<(String, bool)> = lines.iter().map(|l| fact_to_you(l).map_or((l.clone(), false), |y| (y, true))).collect();
    let open = if lines.iter().any(|(_, y)| *y) { MEMORY_OPEN_YOU } else { MEMORY_OPEN };
    let mut s = format!("{open}\n");
    for (l, _) in &lines { s += &format!("- {l}\n"); }
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
            // an attached document (`ground_notes`): read, not changed, and not a claim
            if n.starts_with("attached ") { out.push(l.to_string()); continue; }
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

/// Words of a claim that the assistant gave code ("provided code examples", "wrote a script").
const CODE_CLAIM: &[&str] = &["code example", "code examples", "example code", "sample code", "code sample", "code samples", "code snippet",
    "code snippets", "python script", "a script", "scripts", "python code", "the code"];
const GAVE: &[&str] = &["provided", "generated", "wrote", "written", "shared", "gave", "given", "included", "includes", "including", "created", "showed", "shown", "offered"];

/// New notes checked against what the conversation actually holds (2026-11-09, a 2B fold of a PDF chat: "Elisa has
/// also provided … code examples" with no code in any reply; "Files touched: /Users/…/2412.20138v7.pdf – analyzed"
/// for a PDF the user attached; "[tool glob …]" under Files touched). `said`: every assistant text of the
/// conversation; `docs`: the attached documents (file names); `ledger`: Elisa's edit records.
/// - a sentence claiming code was given goes when no reply holds code (a ``` fence or an indented block);
/// - a `## Done` sentence claiming a file was made or changed goes when no reply names that file and no edit
///   touched it;
/// - a `## Files touched` line naming only files no reply names and no edit touched goes;
/// - a `## Files touched` line naming an attached document (by name, or an `.elisa/uploads/…` path) becomes
///   `- attached: <name>`; an upload path anywhere else becomes `attached: <name>`;
/// - raw tool lines go (`clean_notes`).
pub fn ground_notes(notes: &str, said: &str, docs: &[String], ledger: &Ledger) -> String {
    let has_code = said.contains("```") || said.lines().filter(|l| l.starts_with("    ") && !l.trim().is_empty()).count() >= 3;
    let said_l = said.to_lowercase();
    let base = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string();
    let upload_name = |tok: &str| -> Option<String> {
        let t = tok.trim_matches(|c: char| "\"'`,;:()[]{}!?*".contains(c)).trim_end_matches('.');
        if t.contains(".elisa/uploads/") { return Some(base(t)); }
        let b = base(t);
        docs.iter().find(|d| !d.is_empty() && **d == b && (t.contains('/') || t == d.as_str())).cloned()
    };
    let mut out: Vec<String> = Vec::new();
    let mut heading = String::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') { heading = norm_words(t); out.push(l.to_string()); continue; }
        if t.is_empty() { out.push(String::new()); continue; }
        // Elisa's own reference lines of attached documents stay exactly as they are
        if heading.starts_with("attached documents") || t.contains(crate::attach::MARK) { out.push(l.to_string()); continue; }
        if heading.starts_with("files touched") {
            if let Some(name) = t.split_whitespace().find_map(upload_name) { out.push(format!("- attached: {name}")); continue; }
            // a file no reply names and no edit touched (a 2B fold listed "TradingAgents_code_example.py (likely
            // modified or created)" in a chat with no files)
            let (paths, _) = claim_tokens(t);
            if !paths.is_empty() && paths.iter().all(|p| !said_l.contains(&base(p).to_lowercase()) && ledger.matching(p).is_empty()) { continue; }
            out.push(l.to_string());
            continue;
        }
        // upload paths read as what they are
        let line: String = l.split(' ').map(|w| match w.contains(".elisa/uploads/").then(|| upload_name(w)).flatten() {
            Some(n) => format!("attached: {n}"),
            None => w.to_string(),
        }).collect::<Vec<_>>().join(" ");
        let done = heading.starts_with("done");
        let mut kept: Vec<String> = Vec::new();
        for sent in sentences(line.trim_end()) {
            let low = sent.to_lowercase();
            let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            let gave = words.iter().any(|w| GAVE.contains(w));
            if !has_code && gave && CODE_CLAIM.iter().any(|c| low.contains(c)) { continue; }
            if done && is_claim(&sent, true) {
                let (paths, _) = claim_tokens(&sent);
                let real: Vec<&String> = paths.iter().filter(|p| upload_name(p).is_none() && !p.contains("attached:")).collect();
                if !real.is_empty() && real.iter().all(|p| !said_l.contains(&base(p).to_lowercase()) && ledger.matching(p).is_empty()) { continue; }
            }
            kept.push(sent);
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
    // a session goal that is only small talk is no goal (2026-11-09: "## Session goal\nhey" was carried forward)
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

/// Words too common in notes to say what a fact is about ("The user asked Elisa to …").
const FACT_COMMON: &[&str] = &["user", "elisa", "asked", "asking", "ask", "using", "use", "used", "provide", "provided", "providing"];

/// Whether a notes fact (or session goal) is something `talk` (the user's and the assistant's words of the
/// conversation being summarised) is about: at least 2 of its content words, and at least half of them, are in the
/// talk (a fact with one content word: that word). A fact with no content words never is.
pub fn fact_in_talk(fact: &str, talk: &str) -> bool {
    let cw: Vec<String> = content_words(fact).into_iter().filter(|w| !FACT_COMMON.contains(&w.as_str())).collect();
    if cw.is_empty() { return false; }
    let said = content_words(talk);
    let found = cw.iter().filter(|w| said.contains(*w)).count();
    found >= cw.len().min(2) && found * 2 >= cw.len()
}

/// The `Facts to remember`, `Session goal` and `Done` lines of `notes`, as keys (`fact_key`).
pub fn fact_keys(notes: &str) -> Vec<String> {
    sections(notes).into_iter().filter(|(h, _)| { let k = heading_key(h); k.starts_with("facts") || k.starts_with("session goal") || k.starts_with("done") })
        .flat_map(|(_, b)| b.into_iter().map(|l| fact_key(&l)).filter(|k| !k.is_empty()).collect::<Vec<_>>()).collect()
}

/// A notes line's words, for telling the same fact again (bullets and case ignored).
pub fn fact_key(line: &str) -> String { bullet_key(line) }

/// Whether a notes fact says what a pinned fact says (all the pinned fact's words in it, or the fact's words half
/// in the pinned fact).
pub fn fact_pinned(fact: &str, pinned: &[String]) -> bool {
    let f = format!(" {} ", norm_words(fact));
    pinned.iter().any(|p| { let k = norm_words(p); (!k.is_empty() && f.contains(&format!(" {k} "))) || fact_in_talk(fact, p) })
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
/// old notes have stay. soak: /clear replaced the notes with a summary of one turn, and the codename, the owner and
/// the `<=` fix were gone.
pub fn merge_notes(old: Option<&str>, new: &str) -> String { merge_notes_kept(old, new, &|_| true) }

/// `merge_notes`, where an old `Facts to remember`, `Session goal` or `Done` line stays only when `keep` says so (a
/// line of the new notes always does). The fold and /clear keep an old fact only when the conversation being summarised talks
/// about it, or it was pinned (`fact_in_talk`): 2026-11-09 an old Python-async task's facts and a "hey" goal, read
/// from a stale notes file, were carried forward by every fold of a PDF conversation that never mentioned them.
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
    "sup", "all", "fine", "well", "and", "u", "job", "welcome", "please", "lol", "haha", "yes", "no", "sure", "i'm", "im", "so", "much", "very", "lot",
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
