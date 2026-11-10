//! The topics of one conversation: every attached document and every state card, and which of them a turn is about.
//!
//! A conversation goes back and forth between topics (a paper → another paper → back to the first one), so no card is
//! "the current one" for good. Each card is kept with its topic key (the document it concerns, if any, plus its goal's
//! content words), and every user turn picks at most one topic, by the same rule for the card and the document pages
//! that go into that turn's request (`pick`):
//! (a) the message names a document (its file name, distinctive title words, "the first/second paper", "the other
//!     paper") → that document and its newest card;
//! (b) else its content words point at one document (words that document has and the others hardly do) or overlap one
//!     card's topic → that topic;
//! (c) else a short follow-up ("why?", "continue", "more", "and the Sharpe ratio?") or a bare "the paper" → the last
//!     turn's topic;
//! (d) else nothing: a card or document whose topic does not match is never sent.
//! 2026-11-09: one stale card (#647, "read the first few pages of the PDF file") was carried into every request for
//! hours while the user had moved to another paper, and pulled the model back to the old PDF.

use std::collections::{HashMap, HashSet};

/// words too common, or too generic about documents, to tell topics apart
const COMMON: &[&str] = &["about", "after", "again", "also", "please", "could", "would", "should", "there", "their", "these", "those",
    "this", "that", "with", "from", "into", "what", "when", "where", "which", "while", "make", "write", "show", "tell", "give",
    "need", "want", "just", "like", "some", "then", "than", "have", "does", "your", "them", "they", "will", "each", "more", "only",
    "paper", "document", "file", "page", "read", "first", "second", "third", "last", "other", "content", "text", "section",
    "extract", "provide", "explain", "summary", "summarize", "summarise", "know", "said", "says", "back", "talk", "talking",
    "were", "been", "being", "here", "over", "very", "much", "many", "most", "such", "into", "using", "used", "user", "elisa",
    "next", "done", "none", "goal", "step", "steps", "attached", "attachment", "article", "report", "it's", "let's", "doing",
    "think", "look", "find", "help", "work", "works", "okay", "thank", "thanks", "yes", "sure", "today", "paper's"];

/// Content words of `s` (4+ letters, not common; a plural's `s` dropped), lowercase.
pub fn words(s: &str) -> HashSet<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4 && !COMMON.contains(w))
        .map(|w| w.strip_suffix('s').filter(|r| r.chars().count() >= 4).unwrap_or(w).to_string()).collect()
}

/// Word counts of a document's text (its content words), for telling which document a question is about.
pub fn counts(text: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4 && !COMMON.contains(w)) {
        let w = w.strip_suffix('s').filter(|r| r.chars().count() >= 4).unwrap_or(w);
        *m.entry(w.to_string()).or_insert(0) += 1;
    }
    m
}

/// One attached document as a topic.
#[derive(Clone, Debug, Default)]
pub struct DocTopic {
    /// the file name as uploaded (`2508.02739v1.pdf`)
    pub name: String,
    /// the title's content words ("kronos", "foundation", …)
    pub title: HashSet<String>,
    /// content word counts of its text
    pub counts: HashMap<String, u32>,
    /// its page count (a PDF); 0 for a document not in pages
    pub pages: usize,
}

impl DocTopic {
    pub fn new(name: &str, title: &str, text: &str) -> Self {
        DocTopic { name: name.to_string(), title: words(title), counts: counts(text), pages: 0 }
    }
    fn stem(&self) -> String {
        std::path::Path::new(&self.name).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
    }
}

/// One state card with its topic key.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Card {
    /// its memory id (None: memory off)
    #[serde(default)]
    pub id: Option<u64>,
    /// the attached document it concerns (file name), if any
    #[serde(default)]
    pub doc: Option<String>,
    pub text: String,
}

impl Card {
    /// the goal's content words (the whole card's when it has no goal line)
    pub fn topic_words(&self) -> HashSet<String> {
        let goal = self.text.lines().map(str::trim).find_map(|l| l.strip_prefix("goal:")).map(str::trim).filter(|g| !g.is_empty());
        words(goal.unwrap_or(&self.text))
    }
}

/// Why a topic was picked (ELISA_DEBUG and /state show it).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Why {
    /// a document attached with this message
    Attached,
    /// its file name or title words are in the message
    Named,
    /// "the first / second / last paper"
    Ordinal,
    /// "the other paper"
    Other,
    /// the message's words are that document's (n distinctive words)
    DocWords(usize),
    /// the message's words overlap the card's topic (n words)
    CardWords(usize),
    /// a short follow-up: the last turn's topic
    FollowUp,
    /// "the paper" with nothing more: the last turn's document
    Generic,
    /// nothing matches
    #[default]
    Nothing,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Why::Attached => write!(f, "attached with this message"),
            Why::Named => write!(f, "named in the message"),
            Why::Ordinal => write!(f, "named by its order (\"the first/second paper\")"),
            Why::Other => write!(f, "\"the other one\""),
            Why::DocWords(n) => write!(f, "{n} of the message's words are this document's"),
            Why::CardWords(n) => write!(f, "{n} word(s) overlap the card's goal"),
            Why::FollowUp => write!(f, "a follow-up: the last turn's topic"),
            Why::Generic => write!(f, "\"the paper\": the last turn's document"),
            Why::Nothing => write!(f, "no topic matches"),
        }
    }
}

/// A turn's topic: indexes into the documents and cards it was picked from.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Pick {
    pub doc: Option<usize>,
    pub card: Option<usize>,
    pub why: Why,
}

/// The newest card of document `name`.
fn newest_card_of(cards: &[Card], name: &str) -> Option<usize> {
    cards.iter().rposition(|c| c.doc.as_deref() == Some(name))
}

fn doc_pick(docs: &[DocTopic], cards: &[Card], d: usize, why: Why) -> Pick {
    Pick { doc: Some(d), card: newest_card_of(cards, &docs[d].name), why }
}

/// words that call a document without saying which ("the paper", "this pdf")
const DOC_WORDS: &[&str] = &["paper", "papers", "document", "documents", "doc", "pdf", "file", "article", "attachment", "report", "tài liệu", "bài báo"];

fn has_doc_word(ql: &str) -> bool {
    let w: Vec<&str> = ql.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    DOC_WORDS.iter().any(|k| if k.contains(' ') { ql.contains(k) } else { w.contains(k) })
}

/// "the first paper", "the 2nd document", "the last pdf", "the other one": the document meant by its place.
fn ordinal(ql: &str, docs: &[DocTopic], last_doc: Option<usize>) -> Option<(usize, Why)> {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"\b(first|1st|second|2nd|third|3rd|last|latest|newest|other)\s+(paper|document|doc|pdf|file|article|attachment|one)\b").unwrap());
    let c = r.captures(ql)?;
    let n = docs.len();
    let i = match &c[1] {
        "first" | "1st" => 0,
        "second" | "2nd" => 1,
        "third" | "3rd" => 2,
        "last" | "latest" | "newest" => n.checked_sub(1)?,
        _ => {
            // "the other paper": with two documents, the one the last turn was not about
            let cur = last_doc?;
            if n != 2 { return None; }
            return Some((1 - cur.min(1), Why::Other));
        }
    };
    (i < n).then_some((i, Why::Ordinal))
}

/// The document whose file name or distinctive title words are in the message.
fn named(ql: &str, q_words: &HashSet<String>, docs: &[DocTopic]) -> Option<usize> {
    if let Some(i) = docs.iter().position(|d| { let s = d.stem(); s.chars().count() >= 3 && ql.contains(&s) }) { return Some(i); }
    // title words no other document's title has ("kronos" vs "tradingagent")
    let hits: Vec<usize> = docs.iter().enumerate().map(|(i, d)| {
        d.title.iter().filter(|w| q_words.contains(*w) && !docs.iter().enumerate().any(|(j, o)| j != i && o.title.contains(*w))).count()
    }).collect();
    let best = *hits.iter().max()?;
    (best > 0 && hits.iter().filter(|&&h| h == best).count() == 1).then(|| hits.iter().position(|&h| h == best).unwrap_or(0))
}

/// The document the message's words belong to: words it has at least 4 times as often as any other document (one
/// the others lack entirely counts), with at least half of the message's words found in it. None when two tie or none fits.
fn by_words(q_words: &HashSet<String>, docs: &[DocTopic]) -> Option<(usize, usize)> {
    if q_words.is_empty() { return None; }
    let scores: Vec<(usize, usize)> = docs.iter().enumerate().map(|(i, d)| {
        let found = q_words.iter().filter(|w| d.counts.contains_key(*w)).count();
        let distinct = q_words.iter().filter(|w| {
            let c = d.counts.get(*w).copied().unwrap_or(0);
            c >= 1 && docs.iter().enumerate().all(|(j, o)| j == i || o.counts.get(*w).copied().unwrap_or(0) * 4 <= c)
        }).count();
        (distinct, found)
    }).collect();
    let best = scores.iter().map(|s| s.0).max()?;
    if best == 0 || scores.iter().filter(|s| s.0 == best).count() != 1 { return None; }
    let i = scores.iter().position(|s| s.0 == best)?;
    // most of the message is about it ("what day is today?" with one rare word in a paper is not)
    (scores[i].1 * 2 >= q_words.len()).then_some((i, best))
}

/// The card whose topic (goal words, plus its document's title words) overlaps the message most: at least 2 words, or
/// 1 when the message has at most 2 content words. Ties go to the newer card.
fn by_card(q_words: &HashSet<String>, cards: &[Card], docs: &[DocTopic]) -> Option<(usize, usize)> {
    if q_words.is_empty() { return None; }
    let mut best: Option<(usize, usize)> = None;
    for (i, c) in cards.iter().enumerate() {
        let mut t = c.topic_words();
        if let Some(d) = c.doc.as_deref().and_then(|n| docs.iter().find(|d| d.name == n)) { t.extend(d.title.iter().cloned()); }
        let n = q_words.intersection(&t).count();
        if n >= best.map_or(1, |b| b.1) { best = Some((i, n)); }
    }
    let (i, n) = best?;
    (n >= 2 || q_words.len() <= 2).then_some((i, n))
}

/// A short follow-up that carries the last topic on: "why?", "continue", "more", "and the second table?", "what about
/// GOOGL?" (`route::is_follow_up` / `route::ack` plus a leading "and" / "what about").
pub fn follow_up(q: &str) -> bool {
    let ql = q.trim().to_lowercase();
    crate::route::is_follow_up(q) || crate::route::ack(q)
        || (ql.split_whitespace().count() <= 4 && ["why", "how so", "how come", "really", "what else", "for example", "example", "explain more", "elaborate", "tại sao", "vì sao"].iter().any(|p| ql.starts_with(p)))
        // a question about the work itself ("what was the next step?", "where were we?", "what's left?")
        || ["next step", "the goal", "where were we", "what were we", "what's left", "whats left", "what is left", "status", "progress", "what now", "bước tiếp"].iter().any(|p| ql.contains(p))
        || ["and ", "and?", "what about ", "how about ", "also ", "same for ", "và ", "còn "].iter().any(|p| ql.starts_with(p))
}

/// This turn's topic (see the module doc). `new_doc`: a document attached with this message; `last`: the last turn's.
pub fn pick(q: &str, docs: &[DocTopic], cards: &[Card], last: &Pick, new_doc: Option<usize>) -> Pick {
    pick_for(q, docs, cards, last, new_doc, &HashSet::new())
}

/// `pick`, with `model`: the content words of the model facts (`situation::model_words`). A message whose content
/// words are all among them ("What can this model do for me?") is about the model Elisa runs on: its words pick no
/// document or card; a document named by file name, by order or by "the paper" still counts.
pub fn pick_for(q: &str, docs: &[DocTopic], cards: &[Card], last: &Pick, new_doc: Option<usize>, model: &HashSet<String>) -> Pick {
    if let Some(d) = new_doc.filter(|&d| d < docs.len()) { return doc_pick(docs, cards, d, Why::Attached); }
    let q = q.trim();
    let ql = q.to_lowercase();
    let qw = words(q);
    let qw = if !qw.is_empty() && qw.is_subset(model) { HashSet::new() } else { qw };
    let last_doc = last.doc.filter(|&d| d < docs.len());
    // (a) a document named
    if let Some((d, why)) = ordinal(&ql, docs, last_doc) { return doc_pick(docs, cards, d, why); }
    if let Some(d) = named(&ql, &qw, docs) { return doc_pick(docs, cards, d, Why::Named); }
    // (b) the words
    if let Some((d, n)) = by_words(&qw, docs) {
        // a card of that document whose goal the words name beats its newest card
        let c = by_card(&qw, cards, docs).filter(|(c, _)| cards[*c].doc.as_deref() == Some(docs[d].name.as_str())).map(|c| c.0);
        let mut p = doc_pick(docs, cards, d, Why::DocWords(n));
        if c.is_some() { p.card = c; }
        return p;
    }
    // a page or section named ("what does page 5 say?", "summarize section 3"): the last document (or the only one);
    // its pages are in no other topic (with no text the model makes the page up)
    // a page number past the last document's end ("page 5" after a one-line note) means the newest one that long
    if crate::attach::names_part(q) {
        let want = crate::attach::page_refs(q).into_iter().max().map(|p| p + 1);
        let long = |d: usize| want.is_none_or(|w| docs[d].pages >= w);
        let d = last_doc.filter(|&d| long(d)).or_else(|| (0..docs.len()).rev().find(|&d| long(d) && docs[d].pages > 0))
            .or(last_doc).or_else(|| (docs.len() == 1).then_some(0));
        if let Some(d) = d { return doc_pick(docs, cards, d, Why::FollowUp); }
    }
    if let Some((c, n)) = by_card(&qw, cards, docs) {
        let doc = cards[c].doc.as_deref().and_then(|n| docs.iter().position(|d| d.name == n));
        return Pick { doc, card: Some(c), why: Why::CardWords(n) };
    }
    // (c) a follow-up, or "the paper" alone
    if follow_up(q) && (last.doc.is_some() || last.card.is_some()) {
        return Pick { doc: last_doc, card: last.card.filter(|&c| c < cards.len()), why: Why::FollowUp };
    }
    if has_doc_word(&ql) {
        let d = last_doc.or_else(|| (docs.len() == 1).then_some(0));
        if let Some(d) = d { return doc_pick(docs, cards, d, Why::Generic); }
    }
    Pick::default()
}

/// The document a new card concerns: one whose file name its text says (a `docs_pdf` path), else the turn's topic.
pub fn card_doc(card: &str, docs: &[DocTopic], turn_doc: Option<usize>) -> Option<String> {
    let cl = card.to_lowercase();
    docs.iter().find(|d| cl.contains(&d.name.to_lowercase()))
        .or_else(|| turn_doc.and_then(|i| docs.get(i)))
        .map(|d| d.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<DocTopic> {
        let ta = "TradingAgents: Multi-Agents LLM Financial Trading Framework. Bull and bear researchers debate; the trader agent \
            decides; the risk management team reviews. TradingAgents reaches a Sharpe ratio of 8.21 on AAPL. Researchers debate. \
            The trader agent and the risk team. Bull researcher, bear researcher, fundamental analyst, sentiment analyst.";
        let kr = "Kronos: A Foundation Model for the Language of Financial Markets. Kronos tokenizes K-line candlestick data \
            with a hierarchical tokenizer, then pretrains an autoregressive transformer. The tokenizer quantizes candlestick \
            sequences. Kronos forecasts volatility and synthetic K-line generation. Candlestick tokenizer pretraining.";
        vec![
            DocTopic::new("2412.20138v7.pdf", "TradingAgents: Multi-Agents LLM Financial Trading Framework", ta),
            DocTopic::new("2508.02739v1.pdf", "Kronos: A Foundation Model for the Language of Financial Markets", kr),
        ]
    }

    fn cards() -> Vec<Card> {
        vec![
            Card { id: Some(647), doc: Some("2412.20138v7.pdf".into()), text: "goal: read the first few pages of the PDF file\nnext: extract pages 1-4 via docs_pdf".into() },
            Card { id: Some(700), doc: Some("2508.02739v1.pdf".into()), text: "goal: explain how the Kronos tokenizer works\nnext: compare with other tokenizers".into() },
            Card { id: Some(710), doc: None, text: "goal: fix the failing build in parser.rs\nnext: rerun cargo test".into() },
        ]
    }

    #[test]
    fn a_named_document_brings_its_newest_card() {
        let (d, c) = (docs(), cards());
        let p = pick("tell me about Kronos", &d, &c, &Pick::default(), None);
        assert_eq!((p.doc, p.card, p.why.clone()), (Some(1), Some(1), Why::Named));
        let p = pick("back to TradingAgents: what does the risk team do?", &d, &c, &p, None);
        assert_eq!((p.doc, p.card), (Some(0), Some(0)));
        let p = pick("open 2508.02739v1 again", &d, &c, &p, None);
        assert_eq!(p.doc, Some(1));
    }

    #[test]
    fn order_and_the_other_one() {
        let (d, c) = (docs(), cards());
        assert_eq!(pick("in the first paper, who decides?", &d, &c, &Pick::default(), None).doc, Some(0));
        assert_eq!(pick("and the second paper?", &d, &c, &Pick::default(), None).doc, Some(1));
        let last = Pick { doc: Some(1), card: Some(1), why: Why::Named };
        let p = pick("what does the other paper say about that?", &d, &c, &last, None);
        assert_eq!((p.doc, p.why), (Some(0), Why::Other));
    }

    #[test]
    fn words_pick_the_document_that_has_them() {
        let (d, c) = (docs(), cards());
        let p = pick("how does the candlestick tokenizer quantize sequences?", &d, &c, &Pick::default(), None);
        assert_eq!(p.doc, Some(1));
        assert!(matches!(p.why, Why::DocWords(_)), "{p:?}");
        let p = pick("what do the bull and bear researchers debate?", &d, &c, &Pick::default(), None);
        assert_eq!(p.doc, Some(0));
    }

    #[test]
    fn card_words_pick_a_card_without_a_document() {
        let (d, c) = (docs(), cards());
        let p = pick("is the parser build fixed now?", &d, &c, &Pick::default(), None);
        assert_eq!((p.doc, p.card), (None, Some(2)));
    }

    #[test]
    fn a_follow_up_keeps_the_last_topic_and_other_talk_gets_nothing() {
        let (d, c) = (docs(), cards());
        let last = Pick { doc: Some(1), card: Some(1), why: Why::Named };
        assert_eq!(pick("what was the next step?", &d, &c, &last, None).card, Some(1));
        let p = pick("why?", &d, &c, &last, None);
        assert_eq!((p.doc, p.card, p.why), (Some(1), Some(1), Why::FollowUp));
        let p = pick("what day is today?", &d, &c, &last, None);
        assert_eq!(p, Pick::default());
        let p = pick("hey", &d, &c, &last, None);
        assert_eq!((p.doc, p.card), (None, None));
        // "the paper" alone: the last turn's document
        assert_eq!(pick("summarize the paper", &d, &c, &last, None).doc, Some(1));
    }

    #[test]
    fn a_question_about_the_model_in_use_picks_no_document() {
        // the Kronos paper's title has "model"; the facts' words are the model's, not the paper's
        let one = vec![docs()[1].clone()];
        let c = vec![cards()[1].clone()];
        let last = Pick { doc: Some(0), card: Some(0), why: Why::Attached };
        let m = crate::situation::model_words("qwen3-vl:2b-instruct", true, true, false, 16384);
        assert_eq!(pick("What can this model do for me?", &one, &c, &last, None).doc, Some(0), "without the facts' words: the paper");
        for q in ["What can this model do for me?", "does this model have vision?", "which model is this?"] {
            assert_eq!(pick_for(q, &one, &c, &last, None, &m), Pick::default(), "{q}");
        }
        // the paper's own words, or the paper named, still pick it
        for q in ["what can the Kronos model do?", "how does the model tokenize candlesticks?", "what does the paper say about this model?"] {
            assert_eq!(pick_for(q, &one, &c, &last, None, &m).doc, Some(0), "{q}");
        }
    }

    #[test]
    fn a_page_or_section_follows_the_last_document() {
        let (d, c) = (docs(), cards());
        let last = Pick { doc: Some(1), card: Some(1), why: Why::Named };
        for q in ["what does page 5 say?", "summarize section 3", "pages 3-4 please", "what's in sec. 2.1?"] {
            assert_eq!(pick(q, &d, &c, &last, None).doc, Some(1), "{q}");
        }
        // the last turn was about something else: the only document
        let one = vec![d[1].clone()];
        assert_eq!(pick("what does page 5 say?", &one, &c, &Pick::default(), None).doc, Some(0));
        // two documents and no last one: not a guess
        assert_eq!(pick("what does page 5 say?", &d, &[], &Pick::default(), None).doc, None);
        // page 5 after a one-line note: the PDF that has a page 5
        let mut d2 = docs();
        d2[0].pages = 36;
        d2.push(DocTopic::new("n8.txt", "Note number 8", "Note number 8: the code word is WORD8."));
        let last = Pick { doc: Some(2), card: None, why: Why::Attached };
        assert_eq!(pick("what does page 5 say?", &d2, &[], &last, None).doc, Some(0));
        assert_eq!(pick("summarize section 3", &d2, &[], &last, None).doc, Some(2));
        // a number with no page / section word is not one
        assert_eq!(pick("what is 5 times 3?", &d, &[], &last, None).doc, None);
    }

    #[test]
    fn a_new_attachment_wins() {
        let (d, c) = (docs(), cards());
        let last = Pick { doc: Some(0), card: Some(0), why: Why::Named };
        let p = pick("what is this?", &d, &c, &last, Some(1));
        assert_eq!((p.doc, p.card, p.why), (Some(1), Some(1), Why::Attached));
    }

    #[test]
    fn a_card_never_goes_to_another_topic() {
        let d = docs();
        // only a TradingAgents card: a Kronos question gets the Kronos document and no card
        let c = vec![cards()[0].clone()];
        let p = pick("what is Kronos about?", &d, &c, &Pick::default(), None);
        assert_eq!((p.doc, p.card), (Some(1), None));
    }

    #[test]
    fn a_saved_first_message_loses_its_card() {
        use crate::context::strip_state_card;
        let t = "Working directory: /x\nOS: macos\n\n[state]\nsession goal: learn Kronos\ngoal: read the first few pages of the PDF file\nnext: extract pages 1-4\n[/state]\n\n## Request\nwhat day is today?";
        let s = strip_state_card(t).unwrap();
        assert!(!s.contains("read the first few pages") && s.contains("session goal: learn Kronos") && s.ends_with("## Request\nwhat day is today?"), "{s}");
        let t = "OS: macos\n\n[state]\ngoal: read the first few pages\n[/state]\n\n## Request\nhi";
        assert_eq!(strip_state_card(t).unwrap(), "OS: macos\n\n## Request\nhi");
        assert_eq!(strip_state_card("no state here"), None);
    }

    #[test]
    fn a_card_names_its_document() {
        let d = docs();
        assert_eq!(card_doc("now: read .elisa/uploads/1/2412.20138v7.pdf pages 1-4", &d, Some(1)).as_deref(), Some("2412.20138v7.pdf"));
        assert_eq!(card_doc("goal: answer", &d, Some(1)).as_deref(), Some("2508.02739v1.pdf"));
        assert_eq!(card_doc("goal: answer", &d, None), None);
    }
}
