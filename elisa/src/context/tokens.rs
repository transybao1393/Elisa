use super::*;

pub(super) const MSG_OVERHEAD: usize = 6;

fn tokens_for_chars(n: usize) -> usize { (n * 10).div_ceil(33) }
pub(super) fn chars_for_tokens(t: usize) -> usize { t * 33 / 10 }

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

pub(super) fn message_tokens(m: &Message) -> usize { MSG_OVERHEAD + m.content.iter().map(block_tokens).sum::<usize>() }

pub fn estimate_messages(msgs: &[Message]) -> usize { msgs.iter().map(message_tokens).sum() }

/// A context window this small (tokens) is tiny (`tiny_window`).
pub const TINY_CTX: usize = 4096;

/// A window too small for Elisa's usual prompt (in a 2,048-token window the system
/// prompt, tool rules and the first message's header fill most of it, and a model reads them back as its answer). Decided by the
/// window size alone, never by the model's name: such a session gets `TINY_SYSTEM`, no tools, no first-message
/// header and no per-turn hint lines.
pub fn tiny_window(max_ctx: usize) -> bool { max_ctx < TINY_CTX }

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
