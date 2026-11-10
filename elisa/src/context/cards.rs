use super::*;

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
/// the system prompt's card instruction line, which small models copy into their reply before
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
/// (a small model may echo the first message's `[state]` block as its reply)
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
/// (`CARD_ECHO`), and a run of bare card lines (3+, or 2 with a `now:` / `keep:` one, as in a reply that is
/// only "done: … / now: … / keep: …"). A single `next: …` line stays: it may be prose.
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
            // a stray closing tag with no block open (e.g. "```tool_code\noutline\n</state>\n```") is dropped
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

/// The fields of the system prompt's card template.
pub(super) const CARD_KEYS: &[&str] = &["goal", "done", "now", "keep", "next", "open"];
/// The template's own values (`goal: …`, `keep: must-not-lose facts`): a small model copies them into its card
/// (otherwise a card's placeholder lines get carried into later sessions).
const CARD_PLACEHOLDERS: &[&str] = &["must-not-lose facts", "must not lose facts", "must-not-lose", "n/a", "tbd", "todo", "none yet"];

/// A value that says nothing: empty, only dots / dashes / ellipses, or one of the template's placeholders.
pub(super) fn is_placeholder(v: &str) -> bool {
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
