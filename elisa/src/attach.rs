//! Documents attached to a user message: the app's 📎 / drag and drop (`send` {documents: [{path}]}, src/jsonl.rs)
//! and `elisa -p "…" --attach FILE`. Elisa reads them itself, before the model is asked:
//!
//! 1. The file is copied into the session's upload area, `<project>/.elisa/uploads/<session-id>/<name>` (at most
//!    `MAX_BYTES`). That is inside the project root, so the documents server (`docs_pdf` / `docs_sheet`, which read
//!    only project files) and `read` reach it, and nothing outside the project is opened again later.
//! 2. Its text is extracted here: PDF and spreadsheets with the documents server's own parsers (`mcp_docs::pdf_pages`,
//!    `mcp_docs::sheet_rows`), Word .docx from its `word/document.xml`, anything else that is text as it is.
//! 3. Elisa does the document work itself, never relying on a small model choosing to call a tool (2026-11-09: a 2B
//!    model given a header and "use docs_pdf" answered "I can't process the PDF"). The budget comes from the context
//!    window (`budget`: ~45% of it less the conversation so far). A document that fits goes into the turn whole, as
//!    `[Attached: name.pdf, 2 pages]` + its text. A larger one: for a vague request ("read this", "summarise", no
//!    text) an `overview` (title, abstract, every section heading with its first sentences, the conclusion;
//!    references dropped); for a specific question the pages that answer it (`select`: page / section references
//!    first, then BM25 word overlap, blended with MiniLM embeddings when they are cached). Either way the model answers
//!    in one call with no tool; the docs tools stay offered as an extra (`route::with_documents`). A follow-up about
//!    the same document picks pages again (`repl.rs`), and a refusal-like reply (`refusal`) gets one retry.
//!
//! The text is outside content: secrets are hidden as in tool output (config `redact`) and it is fenced as
//! `<untrusted>` (src/untrusted.rs). Only the document's name reaches memory and fold notes (`context::transcript`
//! keeps the `[Attached: …]` line of such a block and drops the rest).
use crate::context::estimate_tokens;
use std::io::Read;
use std::path::{Path, PathBuf};

/// the largest file Elisa takes as an attachment
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// a document whose extracted text is at most this many tokens goes into the turn whole
pub const INLINE_TOKENS: usize = 2500;
/// the beginning of a larger document shown in its header
pub const HEAD_TOKENS: usize = 300;
/// at most this many documents per message
pub const DOCS_MAX: usize = 8;
/// the first line of every attached-document block in a user message (`context::transcript` keys on it)
pub const MARK: &str = "[Attached: ";

/// one attached document, read and ready for the user turn
#[derive(Clone, Debug)]
pub struct Doc {
    /// the file name as uploaded (`report.pdf`)
    pub name: String,
    /// the copy's path relative to the project root (`.elisa/uploads/<id>/report.pdf`)
    pub rel: String,
    pub bytes: u64,
    /// "2 pages", "3 sheets", "120 rows", "85 lines"
    pub units: String,
    /// the text as extracted
    pub text: String,
    /// how its text is read when it is not inline: (tool, how to ask for a part)
    pub how: (&'static str, &'static str),
    /// a sidecar with the extracted text (a .docx), relative to the project: what `read` opens for a large one
    pub text_rel: Option<String>,
    pub tokens: usize,
    /// the text in parts Elisa picks from: a PDF's pages, else chunks of rows / paragraphs
    pub parts: Vec<String>,
    /// what one part is called: "page" (PDF) or "part"
    pub part: &'static str,
    /// the copy's absolute path (the embeddings cache keys on its content)
    pub path: PathBuf,
    /// what the intake found (category, sections, readable parts, warnings)
    pub info: Option<Intake>,
}

impl Doc {
    /// the whole text fits the old fixed size (kept for callers without a window size)
    pub fn inline(&self) -> bool { self.tokens <= INLINE_TOKENS }
}

/// The upload area of a session: `<root>/.elisa/uploads/<session>`.
pub fn upload_dir(root: &Path, session: u64) -> PathBuf { root.join(".elisa").join("uploads").join(session.to_string()) }

/// A file name safe to create: the last path part, no leading dots, no control or path characters, at most 120 chars.
fn safe_name(src: &Path) -> String {
    let raw = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let s: String = raw.chars().map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '*' | '?') { '_' } else { c }).collect();
    let s = s.trim().trim_start_matches('.').to_string();
    let s = if s.chars().count() > 120 {
        let ext = Path::new(&s).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        format!("{}{ext}", s.chars().take(120 - ext.chars().count().min(20)).collect::<String>())
    } else { s };
    if s.is_empty() { "document".into() } else { s }
}

fn mb(b: u64) -> String {
    if b >= 1 << 20 { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) } else if b >= 1024 { format!("{} KB", b.div_ceil(1024)) } else { format!("{b} bytes") }
}

/// Copies `src` into the session's upload area under the project `root` (canonical) and returns the copy's path.
/// Refused: not a file, larger than `MAX_BYTES`, or an upload area that resolves outside the project (a symlinked
/// `.elisa` planted by a cloned repository).
pub fn stage(root: &Path, session: u64, src: &Path) -> Result<PathBuf, String> {
    let name = safe_name(src);
    let md = std::fs::metadata(src).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { format!("{name}: no such file") } else { format!("{name}: cannot read it ({e})") })?;
    if md.is_dir() { return Err(format!("{name} is a folder; attach the files in it instead")); }
    if md.len() == 0 { return Err(format!("{name} is empty")); }
    if md.len() > MAX_BYTES {
        return Err(format!("{name} is too large to attach ({}; the limit is {} MB per file)", mb(md.len()), MAX_BYTES >> 20));
    }
    let dir = upload_dir(root, session);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let real = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    if !real.starts_with(root) { return Err(format!("{name}: the upload folder {} leads outside the project; not copied", dir.display())); }
    // the same file attached again: the copy that is there; another file of the same name: name-2.ext, …
    let src_real = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    let (stem, ext) = match name.rsplit_once('.') { Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")), _ => (name.clone(), String::new()) };
    for i in 1..1000 {
        let n = if i == 1 { name.clone() } else { format!("{stem}-{i}{ext}") };
        let dst = real.join(&n);
        if dst == src_real { return Ok(dst); }
        if let Ok(m) = std::fs::symlink_metadata(&dst) {
            if m.is_file() && m.len() == md.len() && std::fs::read(&dst).ok() == std::fs::read(src).ok() { return Ok(dst); }
            continue;
        }
        std::fs::copy(src, &dst).map_err(|e| format!("{name}: cannot copy it into the project ({e})"))?;
        return Ok(dst);
    }
    Err(format!("{name}: too many files of that name in {}", dir.display()))
}

/// The text of a Word .docx: the paragraphs of `word/document.xml`, a tab for `<w:tab/>`, a line break for
/// `<w:br/>` and each paragraph end; XML entities decoded.
/// Cached by the file's content (`cache::docs_json`, kind `docs`).
pub fn docx_text(path: &Path) -> Result<String, String> {
    crate::cache::docs_json(path, "docx", || docx_text_uncached(path))
}

fn docx_text_uncached(path: &Path) -> Result<String, String> {
    let name = safe_name(path);
    let f = std::fs::File::open(path).map_err(|e| format!("cannot read {name}: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|_| format!("{name} is not a Word document (not a .docx zip)"))?;
    let mut xml = String::new();
    {
        let mut e = z.by_name("word/document.xml").map_err(|_| format!("{name} is not a Word document (no word/document.xml)"))?;
        // a zip bomb guard: the text part of a 50 MB document is far smaller than this
        e.by_ref().take(200 << 20).read_to_string(&mut xml).map_err(|e| format!("cannot read {name}: {e}"))?;
    }
    Ok(docx_xml_text(&xml))
}

fn docx_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    let mut in_text = false;
    while let Some(i) = rest.find('<') {
        if in_text { out.push_str(&unescape(&rest[..i])); }
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let closing = tag.starts_with('/');
        let empty = tag.ends_with('/');
        match name {
            "w:t" => in_text = !closing && !empty,
            "w:tab" if !closing => out.push('\t'),
            "w:br" | "w:cr" if !closing => out.push('\n'),
            "w:p" if closing || empty => out.push('\n'),
            _ => {}
        }
        rest = &rest[i + j + 1..];
    }
    // at most one blank line in a row
    let mut tidy = String::new();
    let mut blank = 0;
    for l in out.lines().map(str::trim_end) {
        if l.is_empty() { blank += 1; if blank > 1 { continue; } } else { blank = 0; }
        tidy.push_str(l);
        tidy.push('\n');
    }
    tidy.trim().to_string()
}

fn unescape(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(j) = tail.find(';').filter(|&j| j <= 10) else { out.push('&'); rest = &tail[1..]; continue };
        let ent = &tail[1..j];
        let c = match ent {
            "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c { Some(c) => { out.push(c); rest = &tail[j + 1..]; } None => { out.push('&'); rest = &tail[1..]; } }
    }
    out.push_str(rest);
    out
}

fn plural(n: usize, w: &str) -> String { format!("{n} {w}{}", if n == 1 { "" } else { "s" }) }

/// Copies `src` into the upload area and extracts its text: the attached document, or why it cannot be attached.
pub fn read(root: &Path, session: u64, src: &Path) -> Result<Doc, String> {
    let dst = stage(root, session, src)?;
    read_staged(root, &dst)
}

/// A document already in this project's upload area (a resumed session's `[Attached: … · .elisa/uploads/…]`),
/// read where it is: no copy. A path outside `<root>/.elisa/uploads` is refused.
pub fn open(root: &Path, rel: &str) -> Result<Doc, String> {
    let p = root.join(rel);
    let area = root.join(".elisa").join("uploads").canonicalize().map_err(|_| "no uploads in this folder".to_string())?;
    let real = p.canonicalize().map_err(|_| "no longer in this folder's uploads".to_string())?;
    if !real.starts_with(&area) || !real.is_file() { return Err("not in this folder's uploads".into()); }
    read_staged(root, &p)
}

fn read_staged(root: &Path, dst: &Path) -> Result<Doc, String> {
    let dst = dst.to_path_buf();
    let name = dst.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let rel = dst.strip_prefix(root).map(|r| r.display().to_string()).unwrap_or_else(|_| dst.display().to_string());
    let bytes = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
    let ext = dst.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    // the intake first: a password-protected file, a zip bomb or a format Elisa does not read is refused before any
    // parser sees it (a cache hit when the app already checked it at attach time)
    let info = Some(intake(&dst)?);
    let mut text_rel = None;
    let mut parts: Vec<String> = Vec::new();
    let mut part = "part";
    let (units, text, how) = if ext == "pdf" {
        let pages = label_tables(crate::mcp_docs::pdf_pages(&dst)?);
        let n = pages.len();
        let with_text = pages.iter().filter(|p| !p.is_empty()).count();
        if with_text == 0 { return Err(format!("{name}: no text in it (a scanned PDF of images? Elisa has no OCR)")); }
        let text = if n == 1 { pages[0].clone() } else {
            pages.iter().enumerate().map(|(i, p)| format!("--- page {} ---\n{}", i + 1, if p.is_empty() { "[no text on this page]" } else { p })).collect::<Vec<_>>().join("\n")
        };
        parts = pages;
        part = "page";
        (plural(n, "page"), text, ("docs_pdf", "pages=\"2-4\""))
    } else if crate::mcp_docs::is_sheet(&dst) {
        let sheets = crate::mcp_docs::sheet_rows(&dst)?;
        let one = sheets.len() == 1;
        let sep = if ext == "csv" { "," } else { "\t" };
        let mut text = String::new();
        let mut rows = 0;
        for (sname, rs) in &sheets {
            if !one || !sname.is_empty() { text.push_str(&format!("--- sheet {sname} ---\n")); }
            for r in rs {
                let mut cells: Vec<String> = r.iter().map(|c| { let c = c.replace(['\n', '\r'], " "); if sep == "," && c.contains([',', '"']) { format!("\"{}\"", c.replace('"', "\"\"")) } else { c } }).collect();
                while cells.last().is_some_and(|c| c.is_empty()) { cells.pop(); }
                if cells.is_empty() { continue; }
                rows += 1;
                text.push_str(&cells.join(sep));
                text.push('\n');
            }
        }
        let units = if one { plural(rows, "row") } else { format!("{}, {}", plural(sheets.len(), "sheet"), plural(rows, "row")) };
        (units, text.trim_end().to_string(), ("docs_sheet", "range=\"1-50\""))
    } else if ext == "docx" {
        let text = docx_text(&dst)?;
        if text.is_empty() { return Err(format!("{name}: no text in it")); }
        let side = dst.with_file_name(format!("{name}.txt"));
        if std::fs::write(&side, &text).is_ok() { text_rel = side.strip_prefix(root).ok().map(|r| r.display().to_string()); }
        (plural(text.lines().count(), "line"), text, ("read", "offset=1 limit=200"))
    } else {
        let raw = std::fs::read(&dst).map_err(|e| format!("cannot read {name}: {e}"))?;
        if raw[..raw.len().min(8192)].contains(&0) {
            return Err(not_readable(&name, &ext));
        }
        let text = String::from_utf8_lossy(&raw).trim_start_matches('\u{feff}').replace("\r\n", "\n");
        let text = text.trim_end().to_string();
        if text.trim().is_empty() { return Err(format!("{name} is empty")); }
        (plural(text.lines().count(), "line"), text, ("read", "offset=1 limit=200"))
    };
    let tokens = estimate_tokens(&text);
    if parts.is_empty() { parts = chunk(&text, if crate::mcp_docs::is_sheet(&dst) { 600 } else { 450 }); }
    Ok(Doc { name, rel, bytes, units, text, how, text_rel, tokens, parts, part, path: dst, info })
}

/// `text` in parts of about `tokens` each, cut at a blank line (else a line end) when one is near; a table's first
/// line (the header row of a CSV) starts every part
fn chunk(text: &str, tokens: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let table = lines.len() > 3 && lines.iter().take(5).all(|l| l.contains(',') || l.contains('\t'));
    let head = if table { lines.first().copied() } else { None };
    let (mut out, mut cur, mut n) = (Vec::new(), String::new(), 0usize);
    for (i, l) in lines.iter().enumerate() {
        if table && i == 0 { continue; }
        let t = estimate_tokens(l) + 1;
        if n + t > tokens && n > tokens / 2 && (l.trim().is_empty() || table || n + t > tokens * 3 / 2) {
            out.push(std::mem::take(&mut cur).trim_end().to_string());
            n = 0;
        }
        if n == 0 { if let Some(h) = head { cur.push_str(h); cur.push('\n'); n += estimate_tokens(h); } }
        cur.push_str(l);
        cur.push('\n');
        n += t;
    }
    if !cur.trim().is_empty() { out.push(cur.trim_end().to_string()); }
    if out.is_empty() { out.push(text.to_string()); }
    out
}

/// The first `n` tokens' worth of `s` (by Elisa's estimate), cut at a line end when one is near.
fn head(s: &str, n: usize) -> &str {
    let chars = n * 4;
    let Some((i, _)) = s.char_indices().nth(chars) else { return s };
    match s[..i].rfind('\n') { Some(j) if j > i / 2 => &s[..j], _ => &s[..i] }
}

/// The document budget of a turn: about 45% of the context window less the conversation so far (`history`
/// tokens), never below a small floor so a long session still gets something.
pub fn budget(max_ctx: usize, history: usize) -> usize {
    // the floor: a quarter of what is still free, so a long session's follow-up still gets a page or two
    let floor = (max_ctx.saturating_sub(history) / 4).clamp(600, 2000);
    (max_ctx * 45 / 100).saturating_sub(history).max(floor)
}

/// What of a document goes into a turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// all of it (it fits)
    Whole,
    /// the overview (a vague request: "read this", "summarise")
    Overview,
    /// these parts (0-based), for a specific question
    Parts(Vec<usize>),
}

/// The user-turn block of an attached document (its text already redacted as config `redact` says), for the user's
/// words `question`, within `budget` tokens: the whole text when it fits, else an overview or the parts that answer
/// the question. `sent`: parts already in the visible conversation (left out of a selection). Starts with `MARK`.
pub fn block_for(d: &Doc, question: &str, budget: usize, sent: &[usize]) -> (String, Pick) {
    let named = !part_refs(d, question).is_empty();
    let pick = if d.tokens <= budget { Pick::Whole } else if is_vague(question) && !named { Pick::Overview } else {
        match select(d, question, budget, sent) { v if v.is_empty() => Pick::Overview, v => Pick::Parts(v) }
    };
    (render(d, &pick, budget), pick)
}

fn fence_src(d: &Doc) -> String { format!("attached {}", d.name.replace(['"', '<', '>', '\n', '\r'], "")) }

/// The block for `pick`. The whole text: `[Attached: name.pdf, 2 pages]`; an overview: `[Attached: name.pdf, 38
/// pages; overview below, ask about any part for detail]`; parts: `[Attached: name.pdf, 38 pages; pages 4, 9 below,
/// picked for this question; ask about any other part]`.
pub fn render(d: &Doc, pick: &Pick, budget: usize) -> String {
    let src = fence_src(d);
    let path = d.text_rel.as_deref().unwrap_or(&d.rel);
    let tool = d.how.0;
    let tail = format!("\n(The full file is saved at {path}; {tool} reads any other {}.)", d.part);
    match pick {
        Pick::Whole => {
            let (body, _) = crate::untrusted::fence(&src, &d.text);
            format!("{MARK}{}, {}]\n{body}", d.name, d.units)
        }
        Pick::Overview => {
            let (body, _) = crate::untrusted::fence(&src, &overview(d, budget));
            // a vague request ("read this for me") answered with "Sure, I'll read it" on the 2B model: say what to do
            format!("{MARK}{}, {}; overview below, ask about any part for detail]\n{body}{tail}\n(Elisa: unless asked for something else, \
                summarise this document for the person in a few short points.)", d.name, d.units)
        }
        Pick::Parts(v) => {
            let (body, _) = crate::untrusted::fence(&src, &parts_text(d, v, budget));
            format!("{MARK}{}, {}; {} below, picked for this question; ask about any other part]\n{body}{tail}", d.name, d.units, part_list(d, v))
        }
    }
}

/// A follow-up turn about a document attached earlier: the parts that answer `question` (none already `sent`), or
/// None when the question does not match the document (a "thanks", a new topic). Starts with `[From name.pdf: …]`.
pub fn follow_up(d: &Doc, question: &str, budget: usize, sent: &[usize]) -> Option<(String, Vec<usize>)> {
    let named = !part_refs(d, question).is_empty() || !refs(question).is_empty() || !section_refs(question).is_empty();
    if !named && (is_vague(question) || content_words(question).is_empty()) { return None; }
    let v = select(d, question, budget, sent);
    if v.is_empty() { return None; }
    let (body, _) = crate::untrusted::fence(&fence_src(d), &parts_text(d, &v, budget));
    Some((format!("[From {}, {}: {} below, picked for this question]\n{body}", d.name, d.units, part_list(d, &v)), v))
}

/// "pages 4, 9" / "page 3" / "parts 2, 5" (1-based)
fn part_list(d: &Doc, v: &[usize]) -> String {
    format!("{}{} {}", d.part, if v.len() == 1 { "" } else { "s" }, v.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(", "))
}

/// the title line(s), then each picked part under `--- page N ---`, the last one cut to the budget
fn parts_text(d: &Doc, v: &[usize], budget: usize) -> String {
    let mut out = String::new();
    if d.part == "page" && !v.contains(&0) {
        let t = title(d);
        if !t.is_empty() { out.push_str(&format!("Title: {t}\n")); }
    }
    for &i in v {
        let room = budget.saturating_sub(estimate_tokens(&out) + 20);
        if room < 40 { break; }
        let p = d.parts.get(i).map(String::as_str).unwrap_or("");
        out.push_str(&format!("--- {} {} ---\n{}\n", d.part, i + 1, head(p, room)));
    }
    out.trim_end().to_string()
}

// ---------- vague or specific ----------

const STOP: &[&str] = &["the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "with", "by", "at", "from", "is", "are", "was", "were", "be",
    "been", "it", "its", "this", "that", "these", "those", "there", "here", "what", "which", "who", "whom", "how", "why", "when", "where", "do", "does",
    "did", "can", "could", "would", "should", "will", "you", "your", "me", "my", "i", "we", "our", "they", "their", "them", "he", "she", "his", "her",
    "about", "into", "than", "then", "so", "if", "as", "not", "no", "yes", "any", "some", "all", "also", "please", "pls", "tell", "say", "says", "said",
    "give", "show", "find", "get", "use", "used", "uses", "using", "one", "ones", "more", "most", "much", "many", "just", "like", "now", "again", "well",
    "ok", "okay", "thanks", "thank", "hey", "hi", "hello", "let", "lets", "let's", "want", "need", "know", "think", "make", "explain", "describe",
    "mean", "means", "us", "very", "really", "but", "too", "have", "has", "had", "being", "doing", "between", "other", "same", "such", "only", "own"];
/// words of a vague request about the attachment as a whole
const VAGUE: &[&str] = &["read", "summarise", "summarize", "summary", "summaries", "overview", "document", "doc", "pdf", "file", "paper", "article",
    "report", "attached", "attachment", "main", "points", "point", "key", "gist", "tldr", "tl", "dr", "review", "look", "check", "go", "through", "quick",
    "quickly", "brief", "briefly", "short", "takeaways", "takeaway", "ideas", "idea", "topic", "topics", "content", "contents", "whole", "entire",
    "help", "understand", "simple", "simply", "terms", "words", "bullet", "bullets", "abstract", "outline", "highlights", "for", "something", "anything",
    "everything", "important", "stuff", "thing", "things", "kind", "sort", "skim", "analyse", "analyze", "analysis", "spreadsheet", "sheet", "csv",
    "data", "table", "part", "parts", "page", "pages", "section", "sections", "chapter", "show"];

fn stem(w: &str) -> String {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    if w.len() > 4 && w.ends_with("ies") { return format!("{}y", &w[..w.len() - 3]); }
    if w.len() > 4 && w.ends_with('s') && !w.ends_with("ss") { return w[..w.len() - 1].to_string(); }
    w.to_string()
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// the question's content words, stemmed (no stop words, no vague words)
fn content_words(q: &str) -> Vec<String> {
    let mut v: Vec<String> = words(q).into_iter().filter(|w| !STOP.contains(&w.as_str()) && !VAGUE.contains(&w.as_str()) && w.len() > 1).map(|w| stem(&w)).collect();
    v.dedup();
    v
}

/// A request about the attachment as a whole: no words beyond "read / summarise / what is this about / the main
/// points" (or no text at all), and no page or section named.
pub fn is_vague(q: &str) -> bool { refs(q).is_empty() && section_refs(q).is_empty() && content_words(q).is_empty() }

/// The line for a turn whose every attachment was refused (`errs`: the reasons, each naming its file), after the
/// user's words in the turn's own message: an explicit-ask line (`route::ASK_PREFIX`), so views strip it
/// (`context::request_body`).
pub fn refused_note(errs: &[String]) -> String {
    let n = if errs.len() == 1 { "file" } else { "files" };
    format!("{}about the {n} attached with this message, but Elisa could not read it: {}. There is no text from it: tell the user that plainly, \
        with the reason, in one or two sentences; never guess what it says or answer from another file.)", crate::route::ASK_PREFIX, errs.join("; "))
}

/// The question names a page or a section ("what does page 5 say?", "summarize section 3").
pub fn names_part(q: &str) -> bool { !refs(q).is_empty() || !section_refs(q).is_empty() }

/// The pages the question names, 0-based ("page 5" is 4).
pub fn page_refs(q: &str) -> Vec<usize> { refs(q) }

/// pages named in the question ("page 5", "pages 3-4", "p. 7"), 0-based
fn refs(q: &str) -> Vec<usize> {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"(?i)\b(?:pages?|pp?\.)\s*(\d{1,4})(?:\s*(?:-|–|to|and)\s*(\d{1,4}))?").unwrap());
    let mut v = Vec::new();
    for c in r.captures_iter(q) {
        let a: usize = c[1].parse().unwrap_or(0);
        let b: usize = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(a);
        if a == 0 { continue; }
        for n in a..=b.min(a + 20) { if !v.contains(&(n - 1)) { v.push(n - 1); } }
    }
    v
}

/// sections named in the question ("section 4", "sec. 3.2")
fn section_refs(q: &str) -> Vec<String> {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"(?i)\b(?:section|sec\.?|chapter|§)\s*(\d+(?:\.\d+)*)").unwrap());
    r.captures_iter(q).map(|c| c[1].to_string()).collect()
}

/// A reply that says it cannot read the attachment ("I can't process the PDF", "unable to access the file").
pub fn refusal(reply: &str) -> bool {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(concat!(r"(?i)\b(?:can(?:no|')?t|cannot|can not|unable to|not able to|don't have (?:the )?(?:ability|access)|do not have (?:the )?(?:ability|access)|no access)",
        r"\b.{0,40}\b(?:process|read|open|access|view|see|handle|analy[sz]e|attach|pdf|file|document)")).unwrap());
    let head: String = reply.chars().take(300).collect();
    r.is_match(&head)
}

// ---------- structure: headings, title, overview ----------

/// A section heading line: (its number if any, the heading as written). Numbered ("2.1. Related Work", "4 Results"),
/// or a common unnumbered name ("Abstract", "Conclusion", "References").
fn heading(l: &str) -> Option<(Option<String>, String)> {
    static NUM: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let t = l.trim();
    if t.len() < 3 || t.len() > 90 || t.ends_with([',', ';']) { return None; }
    let low = t.trim_end_matches([':', '.']).to_lowercase();
    const NAMED: &[&str] = &["abstract", "introduction", "conclusion", "conclusions", "references", "bibliography", "acknowledgements",
        "acknowledgments", "acknowledgement", "appendix", "related work", "discussion", "results", "methods", "methodology", "experiments",
        "summary", "background", "limitations", "future work", "conclusion and future work", "conclusions and future work", "executive summary",
        "overview", "evaluation", "approach", "method"];
    if NAMED.contains(&low.as_str()) { return Some((None, t.trim_end_matches(':').to_string())); }
    // a Markdown heading ("## Build")
    if let Some(h) = t.strip_prefix('#') {
        let h = h.trim_start_matches('#');
        if h.starts_with(' ') && h.trim().len() >= 2 { return Some((None, h.trim().to_string())); }
    }
    let re = NUM.get_or_init(|| regex::Regex::new(r"^((?:\d{1,2})(?:\.\d{1,2}){0,3})\.?\s+([A-Z][A-Za-z][^\n]*)$").unwrap());
    let c = re.captures(t)?;
    let rest = c[2].trim();
    let nw = rest.split_whitespace().count();
    if nw > 12 || rest.ends_with('.') && nw > 6 { return None; }
    // a table row or a list of numbers is not a heading
    if rest.chars().filter(|c| c.is_ascii_digit()).count() * 3 > rest.len() { return None; }
    let first: u32 = c[1].split('.').next()?.parse().ok()?;
    if first == 0 || first > 30 { return None; }
    Some((Some(c[1].to_string()), t.to_string()))
}

/// The headings of one pass over a document, in order: a heading seen before is not one again, and a numbered one
/// must follow the numbering so far (the same top number one level down, or the next one or two; the first at most 3).
/// A figure's or table's axis labels ("10 Kronossmall", "15 TimeVAE" twenty times over in a 36-page paper) and a
/// stray "18. The table" are left out; so is a heading-like line found 3 or more times in the document (a label; a
/// real heading is there once, or twice with a contents list).
struct Outline { seen: std::collections::HashSet<String>, noise: std::collections::HashSet<String>, top: u32 }

impl Outline {
    fn new(d: &Doc) -> Self {
        let mut count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for p in &d.parts { for l in p.lines() { if let Some((_, h)) = heading(l) { *count.entry(h).or_default() += 1; } } }
        Outline { seen: Default::default(), noise: count.into_iter().filter(|(_, c)| *c >= 3).map(|(h, _)| h).collect(), top: 0 }
    }

    fn heading(&mut self, l: &str) -> Option<(Option<String>, String)> {
        let (num, h) = heading(l)?;
        if self.seen.contains(&h) || self.noise.contains(&h) { return None; }
        if let Some(n) = &num {
            let first: u32 = n.split('.').next()?.parse().ok()?;
            let sub = n.trim_end_matches('.').contains('.');
            let ok = if self.top == 0 { first <= 3 } else if sub { first == self.top || first == self.top + 1 } else { first > self.top && first <= self.top + 2 };
            if !ok { return None; }
            self.top = first;
        }
        self.seen.insert(h.clone());
        Some((num, h))
    }
}

/// lines that repeat on many pages (a running header, a page footer) and bare page numbers: left out of the overview
fn boilerplate(d: &Doc) -> std::collections::HashSet<String> {
    let mut count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in &d.parts {
        let mut seen = std::collections::HashSet::new();
        for l in p.lines().map(str::trim).filter(|l| !l.is_empty()) { if seen.insert(l) { *count.entry(l.to_string()).or_default() += 1; } }
    }
    let n = d.parts.len();
    count.into_iter().filter(|(_, c)| n >= 4 && *c * 10 >= n * 4).map(|(l, _)| l).collect()
}

/// the document's title: the first short lines of its first part, before the first paragraph (an arXiv id left out)
pub fn title(d: &Doc) -> String {
    let Some(p) = d.parts.first() else { return String::new() };
    let bp = boilerplate(d);
    let mut t: Vec<&str> = Vec::new();
    for l in p.lines().map(str::trim) {
        if l.is_empty() { if !t.is_empty() { break; } continue; }
        if l.starts_with("arXiv:") || bp.contains(l) || l.chars().filter(|c| c.is_alphabetic()).count() < 3 { continue; }
        // a spaced-out logo ("TAU RIC RE SEA RCH"): words of 3 letters or fewer on average
        let ws: Vec<&str> = l.split_whitespace().collect();
        if ws.len() >= 3 && ws.iter().map(|w| w.len()).sum::<usize>() < ws.len() * 4 && t.is_empty() { continue; }
        if l.len() > 120 || heading(l).is_some() { break; }
        t.push(l);
        if t.len() >= 3 { break; }
    }
    t.join(" ")
}

/// the first `n` tokens of `s`, cut at a sentence end when one is in the second half
fn sentences(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let h = head(&flat, n);
    if h.len() == flat.len() { return h.to_string(); }
    match h.rfind(". ") { Some(i) if i > h.len() / 2 => h[..=i].to_string(), _ => format!("{h}…") }
}

/// An overview that fits `budget` tokens: the title, the abstract (or the opening text), every section heading with
/// its first sentences (page numbers for a PDF), and the conclusion; the references are left out. A document with no
/// headings: the start of every part, evenly.
pub fn overview(d: &Doc, budget: usize) -> String {
    let bp = boilerplate(d);
    // (part, line) of the whole document, boilerplate and bare numbers out
    let mut lines: Vec<(usize, &str)> = Vec::new();
    for (i, p) in d.parts.iter().enumerate() {
        for l in p.lines() {
            let t = l.trim();
            if bp.contains(t) || (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c.is_whitespace())) { continue; }
            lines.push((i, l));
        }
    }
    // sections: heading, its part, its body
    struct Sec { name: String, part: usize, body: String, named: String }
    let mut secs: Vec<Sec> = Vec::new();
    let mut lead = String::new();
    let mut outline = Outline::new(d);
    for (i, l) in &lines {
        if let Some((_, h)) = outline.heading(l) {
            let named = h.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ').to_lowercase();
            secs.push(Sec { name: h, part: *i, body: String::new(), named });
            continue;
        }
        let dst = match secs.last_mut() { Some(s) => &mut s.body, None => &mut lead };
        dst.push_str(l);
        dst.push('\n');
    }
    // the references and what follows them (appendix bodies) are not overview material
    if let Some(k) = secs.iter().position(|s| matches!(s.named.as_str(), "references" | "bibliography")) {
        for s in secs.iter_mut().skip(k) { s.body.clear(); }
        secs.remove(k);
    }
    let t = title(d);
    let mut out = String::new();
    if !t.is_empty() { out.push_str(&format!("Title: {t}\n")); }
    let pg = |i: usize| if d.part == "page" { format!(" (p. {})", i + 1) } else { String::new() };
    if secs.len() < 2 {
        // no structure found: the start of every part, evenly
        let per = (budget.saturating_sub(estimate_tokens(&out) + 20) / d.parts.len().max(1)).max(30);
        for (i, p) in d.parts.iter().enumerate() {
            if estimate_tokens(&out) + per > budget { out.push_str("[… the rest is left out]\n"); break; }
            out.push_str(&format!("--- {} {} ---\n{}\n", d.part, i + 1, sentences(p, per)));
        }
        return out.trim_end().to_string();
    }
    // the abstract: its section, else the opening text after the title
    let abs_sec = secs.iter().position(|s| s.named == "abstract");
    let abstract_text = match abs_sec { Some(k) => secs[k].body.clone(), None => {
        let tl: Vec<&str> = t.split(' ').collect();
        lead.lines().filter(|l| l.trim().len() > 60 && !tl.iter().all(|w| l.contains(w))).collect::<Vec<_>>().join("\n")
    } };
    let concl = secs.iter().rposition(|s| s.named.starts_with("conclusion") || s.named == "summary" || s.named == "discussion and conclusion");
    let abs_n = (budget / 5).clamp(120, 500);
    let concl_n = (budget / 6).clamp(100, 450);
    if !abstract_text.trim().is_empty() { out.push_str(&format!("Abstract: {}\n", sentences(&abstract_text, abs_n))); }
    let n = secs.iter().enumerate().filter(|(k, _)| Some(*k) != abs_sec && Some(*k) != concl).count().max(1);
    let fixed = estimate_tokens(&out) + if concl.is_some() { concl_n + 20 } else { 0 } + secs.len() * 12 + 40;
    let per = (budget.saturating_sub(fixed) / n).clamp(0, 350);
    for (k, s) in secs.iter().enumerate() {
        if Some(k) == abs_sec { continue; }
        let take = if Some(k) == concl { concl_n } else { per };
        out.push_str(&format!("## {}{}\n", s.name, pg(s.part)));
        if take >= 15 && !s.body.trim().is_empty() { out.push_str(&sentences(&s.body, take)); out.push('\n'); }
        if estimate_tokens(&out) > budget { break; }
    }
    out.trim_end().to_string()
}

// ---------- picking parts for a question ----------

/// The parts (0-based, in document order) that best answer `question`, within `budget` tokens, none of `sent`:
/// pages / sections the question names first, then the best scored (BM25 over the parts' words, blended with MiniLM
/// embeddings when they are cached for this file). Empty: nothing matches.
pub fn select(d: &Doc, question: &str, budget: usize, sent: &[usize]) -> Vec<usize> {
    let n = d.parts.len();
    let mut want: Vec<usize> = refs(question).into_iter().chain(part_refs(d, question)).filter(|i| *i < n).collect();
    want.dedup();
    for sref in section_refs(question) {
        // the section's heading part, and the parts up to the next heading of the same or a higher level
        let level = sref.matches('.').count();
        let mut start = None;
        let mut outline = Outline::new(d);
        'outer: for (i, p) in d.parts.iter().enumerate() {
            for l in p.lines() {
                if let Some((Some(num), _)) = outline.heading(l) {
                    if start.is_none() && num.trim_end_matches('.') == sref { start = Some(i); }
                    else if let Some(s0) = start { if num.matches('.').count() <= level && i > s0 { if !want.contains(&i) { want.push(i); } break 'outer; } }
                }
            }
            if let Some(s0) = start {
                if i - s0 > 4 { break; }
                if !want.contains(&i) { want.push(i); }
            }
        }
    }
    let scores = scores(d, question);
    let mut ranked: Vec<usize> = (0..n).filter(|i| scores[*i] > 0.0).collect();
    ranked.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    // a part far below the best adds little
    let best = ranked.first().map_or(0.0, |&i| scores[i]);
    ranked.retain(|&i| scores[i] >= best * 0.35);
    let mut pick: Vec<usize> = Vec::new();
    let mut used = estimate_tokens(&title(d)) + 30;
    let named = want.clone();
    for i in want.into_iter().chain(ranked) {
        // a page the person named goes in even when it was sent before
        if pick.contains(&i) || (sent.contains(&i) && !named.contains(&i)) { continue; }
        let t = estimate_tokens(&d.parts[i]) + 12;
        if used + t > budget {
            // the first pick alone is over: it goes in cut (`parts_text`)
            if pick.is_empty() { pick.push(i); }
            break;
        }
        used += t;
        pick.push(i);
        if pick.len() >= 12 { break; }
    }
    pick.sort();
    pick
}

/// BM25 over the parts for the question's content words (k1 1.2, b 0.75), blended 50 / 50 with MiniLM cosine
/// similarity when the parts' embeddings are cached (`embed_cached`); 0 = no match.
fn scores(d: &Doc, question: &str) -> Vec<f32> {
    let q = content_words(question);
    let n = d.parts.len();
    let docs: Vec<Vec<String>> = d.parts.iter().map(|p| words(p).into_iter().map(|w| stem(&w)).collect()).collect();
    let avg = docs.iter().map(Vec::len).sum::<usize>() as f32 / n.max(1) as f32;
    let mut s = vec![0f32; n];
    for w in &q {
        let df = docs.iter().filter(|d| d.contains(w)).count() as f32;
        if df == 0.0 { continue; }
        let idf = ((n as f32 - df + 0.5) / (df + 0.5) + 1.0).ln();
        for (i, dw) in docs.iter().enumerate() {
            let tf = dw.iter().filter(|x| *x == w).count() as f32;
            if tf == 0.0 { continue; }
            // b = 0.5: a short part (a chart's axis labels) is not boosted much over a page of prose
            s[i] += idf * tf * 2.2 / (tf + 1.2 * (0.5 + 0.5 * dw.len() as f32 / avg.max(1.0)));
        }
    }
    // a part that has more of the question's words ranks above one that repeats one of them
    if q.len() > 1 {
        for (i, dw) in docs.iter().enumerate() {
            let hit = q.iter().filter(|w| dw.contains(w)).count() as f32;
            s[i] *= 1.0 + hit / q.len() as f32;
        }
    }
    let max = s.iter().cloned().fold(0f32, f32::max);
    if max <= 0.0 { return s; }
    if let Some(cos) = embed_scores(d, question) {
        let (lo, hi) = cos.iter().fold((f32::MAX, f32::MIN), |(a, b), &c| (a.min(c), b.max(c)));
        for i in 0..n {
            let c = if hi > lo { (cos[i] - lo) / (hi - lo) } else { 0.0 };
            // only a part with some word match is a candidate; the embedding reorders them
            if s[i] > 0.0 { s[i] = 0.5 * s[i] / max + 0.5 * c; }
        }
    }
    s
}

/// chunks of a part for the embedder (MiniLM reads ~256 word pieces): (part index, text)
fn embed_chunks(d: &Doc) -> Vec<(usize, String)> {
    let mut v = Vec::new();
    for (i, p) in d.parts.iter().enumerate() {
        let w: Vec<&str> = p.split_whitespace().collect();
        for c in w.chunks(160) { v.push((i, c.join(" "))); }
    }
    v
}

/// The parts' MiniLM embeddings (per chunk), computed and cached by file content (`cache::docs_json`, kind docs).
/// For a background thread: `prefetch_embeddings`.
fn embed_parts(d: &Doc) -> Result<Vec<(usize, Vec<f32>)>, String> {
    let dir = elisa_mem::embed::model_dir().ok_or("no embedding model")?;
    crate::cache::docs_json(&d.path, "minilm_parts", || {
        let ch = embed_chunks(d);
        let refs: Vec<&str> = ch.iter().map(|c| c.1.as_str()).collect();
        let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
        let vs = elisa_mem::embed::embed(&dir, &vocab, &refs, &mut |_| {}).map_err(|e| e.to_string())?;
        Ok(ch.iter().zip(vs).map(|((i, _), v)| (*i, v.to_vec())).collect())
    })
}

/// Fills the embeddings cache for `d` on a background thread (only with the cache on and the model present), so a
/// question a moment later, or a follow-up, ranks with them.
pub fn prefetch_embeddings(d: &Doc) -> Option<std::thread::JoinHandle<()>> {
    if crate::cache::global().is_none() || elisa_mem::embed::model_dir().is_none() || d.parts.len() < 3 { return None; }
    let d = d.clone();
    Some(std::thread::spawn(move || { let _ = embed_parts(&d); }))
}

/// cosine similarity of each part (its best chunk) to the question, when the parts' embeddings are already cached
fn embed_scores(d: &Doc, question: &str) -> Option<Vec<f32>> {
    if !crate::cache::docs_has(&d.path, "minilm_parts") { return None; }
    let parts = embed_parts(d).ok()?;
    let dir = elisa_mem::embed::model_dir()?;
    let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
    let q = elisa_mem::embed::embed(&dir, &vocab, &[question], &mut |_| {}).ok()?.into_iter().next()?;
    let mut best = vec![f32::MIN; d.parts.len()];
    for (i, v) in &parts {
        let c: f32 = v.iter().zip(q.iter()).map(|(a, b)| a * b).sum();
        if let Some(b) = best.get_mut(*i) { *b = b.max(c); }
    }
    Some(best.into_iter().map(|b| if b == f32::MIN { 0.0 } else { b }).collect())
}

// ---------- intake: inspect, check, categorise, structure, plan (in the background, the moment a file is attached) ----------

/// bumped when the intake's findings change, so a cached intake from an older Elisa is not reused
const INTAKE_VERSION: u32 = 3;

/// the longest Elisa waits for a document's text
pub const EXTRACT_SECS: u64 = 30;
/// a .docx / .xlsx / .ods whose entries declare more than this, or more entries than `ZIP_ENTRIES`, is refused
const ZIP_BYTES: u64 = 400 << 20;
const ZIP_ENTRIES: usize = 20_000;

/// a section or a reading part: its title and its 1-based page (or part) range
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Span { pub title: String, pub from: usize, pub to: usize }

/// one sheet of a workbook / CSV
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SheetInfo { pub name: String, pub rows: usize, pub columns: Vec<String> }

/// What Elisa found out about an attached file, before the model sees any of it (`intake`). Cached by content.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Intake {
    pub name: String,
    /// by content: "pdf" | "docx" | "xlsx" | "ods" | "xls" | "csv" | "text" | "code"
    pub kind: String,
    pub bytes: u64,
    pub pages: Option<usize>,
    pub sheets: Vec<SheetInfo>,
    pub words: usize,
    pub tokens: usize,
    /// "en" | "vi" | ""
    pub lang: String,
    /// the text could be read (false: a scanned PDF)
    pub text_layer: bool,
    /// paper | report | invoice | contract | cv | statement | slides | book | data | code | notes | other
    pub category: String,
    pub sections: Vec<Span>,
    pub tables: usize,
    /// the PDF has its own outline (table of contents), used for `sections`
    pub outline: bool,
    /// readable parts for the person ("part 2 (pages 9–14)")
    pub parts: Vec<Span>,
    pub warnings: Vec<String>,
    /// what one page / part is called in `sections` and `parts`
    pub unit: String,
}

/// the words for a category
pub fn category_word(c: &str) -> &'static str {
    match c {
        "paper" => "research paper", "report" => "report", "invoice" => "invoice or receipt", "contract" => "contract or legal text",
        "cv" => "CV / résumé", "statement" => "bank or financial statement", "slides" => "slides", "book" => "book or manual",
        "data" => "spreadsheet / data", "code" => "code", "notes" => "notes", _ => "document",
    }
}

/// What the type of a file is by its first bytes (and, for a zip, its entries): (kind, None) or the reason it
/// cannot be opened.
fn sniff(path: &Path, head: &[u8]) -> Result<String, String> {
    let name = safe_name(path);
    if head.windows(5).take(1024).any(|w| w == b"%PDF-") { return Ok("pdf".into()); }
    if head.starts_with(b"PK\x03\x04") {
        let f = std::fs::File::open(path).map_err(|e| format!("cannot read {name}: {e}"))?;
        let mut z = zip::ZipArchive::new(f).map_err(|_| format!("{name} looks like a zip file but cannot be opened"))?;
        if z.len() > ZIP_ENTRIES { return Err(format!("{name} has {} entries inside: refused as a possible zip bomb", z.len())); }
        let mut total = 0u64;
        let mut names = Vec::new();
        for i in 0..z.len() {
            let Ok(e) = z.by_index_raw(i) else { continue };
            total = total.saturating_add(e.size());
            if e.size() > 50 << 20 && e.compressed_size() > 0 && e.size() / e.compressed_size().max(1) > 200 {
                return Err(format!("{name}: an entry expands {}x (to {}): refused as a possible zip bomb", e.size() / e.compressed_size().max(1), mb(e.size())));
            }
            names.push(e.name().map(|n| n.to_string()).unwrap_or_default());
        }
        if total > ZIP_BYTES { return Err(format!("{name} expands to {} when unpacked: refused as a possible zip bomb (the limit is {})", mb(total), mb(ZIP_BYTES))); }
        let has = |n: &str| names.iter().any(|x| x == n);
        return Ok(if has("word/document.xml") { "docx" } else if has("xl/workbook.xml") { "xlsx" }
            else if has("content.xml") && names.iter().any(|x| x == "mimetype") { "ods" } else { "zip" }.into());
    }
    if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        // an OLE container: a legacy .xls / .doc, or an encrypted .docx / .xlsx (stream "EncryptionInfo")
        let enc: Vec<u8> = "EncryptionInfo".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let all = std::fs::read(path).unwrap_or_default();
        if all.windows(enc.len()).any(|w| w == enc.as_slice()) { return Err(format!("{name} is a password-protected Office file; it cannot be read")); }
        return Ok("ole".into());
    }
    if head[..head.len().min(8192)].contains(&0) { return Ok("binary".into()); }
    Ok("text".into())
}

const CODE_EXTS: &[&str] = &["swift", "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "kt", "go", "rb", "php", "c", "h", "cc", "cpp",
    "hpp", "m", "mm", "cs", "sh", "bash", "zsh", "sql", "r", "lua", "dart", "scala", "pl", "ex", "exs", "hs", "vue", "svelte"];

/// the kind the extension promises (for the mismatch warning)
/// why a binary file is refused: a text name with binary bytes says so (measured on qwen3-vl:2b: "it reads … text
/// files" for a .txt made the reply "it is a text file but I don't have access to its content")
fn not_readable(name: &str, ext: &str) -> String {
    if ext_kind(ext) == Some("text") { return format!("{name} is named .{ext} but holds binary data, not text, so Elisa cannot read it"); }
    let hint = if matches!(ext, "doc" | "ppt" | "pptx" | "pages" | "key" | "numbers" | "rtfd") { " (save it as PDF or .docx and attach that)" } else { "" };
    format!("{name} is not a document Elisa can read{hint}; it reads PDF, Word .docx, spreadsheets (xlsx, xls, ods, csv) and text files")
}

fn ext_kind(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "pdf", "docx" | "docm" => "docx", "xlsx" | "xlsm" | "xlsb" => "xlsx", "ods" => "ods", "xls" | "doc" => "ole",
        "csv" | "tsv" | "txt" | "md" | "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "htm" | "log" => "text",
        e if CODE_EXTS.contains(&e) => "text",
        _ => return None,
    })
}

fn kind_word(k: &str) -> &'static str {
    match k { "pdf" => "a PDF", "docx" => "a Word document", "xlsx" => "an Excel workbook", "ods" => "an OpenDocument sheet", "ole" => "an old Office file",
        "zip" => "a zip archive", "binary" => "a binary file", _ => "a text file" }
}

/// `f` on a thread, given at most `secs`: a parser stuck on a hostile file does not hold the turn
fn timed<T: Send + 'static>(what: &str, secs: u64, f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("elisa-extract".into()).spawn(move || { let _ = tx.send(f()); }).map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(secs)).unwrap_or_else(|_| Err(format!("{what} took more than {secs} s to read; not attached")))
}

/// The safety checks of a file Elisa is about to read (nothing in it is ever run): its kind by content, and the
/// warnings (active PDF content, macros, a type that does not match the extension). Err: it must not be opened
/// (password-protected, a zip bomb, a format Elisa does not read).
pub fn check(path: &Path) -> Result<(String, Vec<String>), String> {
    let name = safe_name(path);
    let mut head = vec![0u8; 8192];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).map_err(|e| format!("cannot read {name}: {e}"))?;
    head.truncate(n);
    if n == 0 { return Err(format!("{name} is empty")); }
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len > MAX_BYTES { return Err(format!("{name} is too large to attach ({}; the limit is {} MB per file)", mb(len), MAX_BYTES >> 20)); }
    let kind = sniff(path, &head)?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut warn = Vec::new();
    if let Some(k) = ext_kind(&ext) {
        if k != kind && !(k == "xlsx" && kind == "xlsx") {
            warn.push(format!("named .{ext} but it is {}; read as what it is", kind_word(&kind)));
        }
    }
    match kind.as_str() {
        "pdf" => {
            let all = std::fs::read(path).map_err(|e| format!("cannot read {name}: {e}"))?;
            let has = |k: &[u8]| all.windows(k.len()).any(|w| w == k);
            if has(b"/JavaScript") || has(b"/JS ") || has(b"/JS(") || has(b"/JS<") { warn.push("has JavaScript (not run; text only)".into()); }
            if has(b"/Launch") { warn.push("has a launch action (not run; text only)".into()); }
            if has(b"/EmbeddedFile") { warn.push("has embedded files (not opened; text only)".into()); }
        }
        "docx" | "xlsx" | "ods" => {
            let f = std::fs::File::open(path).map_err(|e| format!("cannot read {name}: {e}"))?;
            if let Ok(mut z) = zip::ZipArchive::new(f) {
                let macros = (0..z.len()).any(|i| z.by_index_raw(i).is_ok_and(|e| e.name().is_ok_and(|n| n.to_ascii_lowercase().contains("vbaproject.bin") || n.starts_with("Basic/"))));
                if macros || ext == "docm" || ext == "xlsm" { warn.push("has macros (not run; text only)".into()); }
            }
        }
        "ole" => {} // calamine reads a legacy .xls; a .doc is refused below
        "zip" => return Err(format!("{name} is a zip archive; unpack it and attach the files inside")),
        "binary" => {
            return Err(not_readable(&name, &ext));
        }
        _ => {}
    }
    if kind == "ole" && ext != "xls" { return Err(format!("{name} is an old Office file Elisa cannot read; save it as PDF, .docx or .xlsx")); }
    Ok((kind, warn))
}

/// The intake of a file (cached by content, `cache::docs_json` part "intake"): what it is, whether it is safe, its
/// category, structure and readable parts. Err: it cannot be attached, with the reason. The fit against the window
/// is `fit` (it depends on the session, so it is not cached).
pub fn intake(path: &Path) -> Result<Intake, String> {
    let (kind, warnings) = check(path)?;
    let p = path.to_path_buf();
    crate::cache::docs_json(path, &format!("intake-{INTAKE_VERSION}"), move || {
        let name = safe_name(&p);
        let k2 = kind.clone();
        let p2 = p.clone();
        let doc = timed(&name, EXTRACT_SECS, move || extract(&p2, &k2))?;
        Ok(describe(&doc, &kind, warnings))
    })
}

/// the text in parts, for the intake (pages of a PDF, else chunks); sheets as (name, rows)
struct Extracted { name: String, bytes: u64, parts: Vec<String>, unit: &'static str, sheets: Vec<crate::mcp_docs::SheetRows>, toc: Vec<(usize, String, usize)> }

fn extract(path: &Path, kind: &str) -> Result<Extracted, String> {
    let name = safe_name(path);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut x = Extracted { name: name.clone(), bytes, parts: Vec::new(), unit: "part", sheets: Vec::new(), toc: Vec::new() };
    match kind {
        "pdf" => {
            x.parts = crate::mcp_docs::pdf_pages(path)?;
            x.unit = "page";
            x.toc = pdf_toc(path);
        }
        "xlsx" | "ods" | "ole" => { x.sheets = crate::mcp_docs::sheet_rows(path)?; }
        "docx" => { x.parts = chunk(&docx_text(path)?, 450); }
        _ => {
            let raw = std::fs::read(path).map_err(|e| format!("cannot read {name}: {e}"))?;
            let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
            let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if ext == "csv" || ext == "tsv" { x.sheets = crate::mcp_docs::sheet_rows(path).unwrap_or_default(); }
            x.parts = chunk(text.trim(), 450);
        }
    }
    Ok(x)
}

/// a PDF's own outline: (level, title, 1-based page); empty when it has none (or it cannot be read)
fn pdf_toc(path: &Path) -> Vec<(usize, String, usize)> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    std::panic::catch_unwind(|| {
        let Ok(mut d) = pdf_extract::Document::load_mem(&bytes) else { return Vec::new() };
        if d.is_encrypted() && d.decrypt("").is_err() { return Vec::new(); }
        d.get_toc().map(|t| t.toc.into_iter().map(|e| (e.level, e.title.trim().to_string(), e.page)).filter(|e| !e.1.is_empty()).collect()).unwrap_or_default()
    }).unwrap_or_default()
}

fn describe(x: &Extracted, kind: &str, warnings: Vec<String>) -> Intake {
    let mut it = Intake { name: x.name.clone(), kind: kind.to_string(), bytes: x.bytes, warnings, unit: x.unit.to_string(), ..Default::default() };
    let ext = Path::new(&x.name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if kind == "text" && CODE_EXTS.contains(&ext.as_str()) { it.kind = "code".into(); }
    if (kind == "text" && (ext == "csv" || ext == "tsv")) || !x.sheets.is_empty() && kind != "text" { it.kind = if kind == "text" { "csv".into() } else { kind.into() }; }
    let text: String = if x.sheets.is_empty() || kind == "text" { x.parts.join("\n") } else {
        x.sheets.iter().flat_map(|(_, rows)| rows.iter().map(|r| r.join(" "))).collect::<Vec<_>>().join("\n")
    };
    it.words = text.split_whitespace().count();
    it.tokens = estimate_tokens(&text);
    it.lang = match crate::reply_lang::detect(&text.chars().take(4000).collect::<String>()) { Some(crate::reply_lang::Lang::Vi) => "vi", Some(crate::reply_lang::Lang::En) => "en", None => "" }.into();
    it.sheets = x.sheets.iter().map(|(n, rows)| SheetInfo {
        name: n.clone(), rows: rows.iter().filter(|r| r.iter().any(|c| !c.trim().is_empty())).count(),
        columns: rows.first().map(|r| r.iter().filter(|c| !c.trim().is_empty()).take(30).cloned().collect()).unwrap_or_default(),
    }).collect();
    if x.unit == "page" {
        it.pages = Some(x.parts.len());
        let with_text = x.parts.iter().filter(|p| p.split_whitespace().count() > 5).count();
        it.text_layer = with_text * 2 >= x.parts.len().max(1) || with_text > 0 && it.words > 50;
        if with_text == 0 { it.text_layer = false; it.warnings.push("no text layer (a scanned PDF of images? Elisa has no OCR)".into()); }
    } else { it.text_layer = it.words > 0; }
    // structure: the outline when the PDF has one, else headings found in the text
    let doc = Doc { name: x.name.clone(), rel: String::new(), bytes: x.bytes, units: String::new(), text: String::new(), how: ("", ""), text_rel: None,
        tokens: it.tokens, parts: x.parts.clone(), part: x.unit, path: PathBuf::new(), info: None };
    let n = x.parts.len().max(1);
    if !x.toc.is_empty() {
        it.outline = true;
        let top = x.toc.iter().map(|e| e.0).min().unwrap_or(0);
        // entries in page order only (one whose page could not be resolved comes back as 0 or out of order)
        let mut last = 0;
        let tops: Vec<&(usize, String, usize)> = x.toc.iter().filter(|e| e.0 <= top + 1).filter(|e| { let ok = e.2 >= 1 && e.2 >= last; if ok { last = e.2; } ok }).collect();
        for (i, e) in tops.iter().enumerate() {
            let to = tops.get(i + 1).map_or(n, |nx| nx.2.max(e.2 + 1) - 1).max(e.2.max(1));
            it.sections.push(Span { title: e.1.clone(), from: e.2.max(1), to: to.min(n) });
        }
    } else { it.sections = sections(&doc); }
    it.tables = x.parts.iter().map(|p| p.lines().filter(|l| { let t = l.trim_start(); (t.starts_with("Table ") || t.starts_with("TABLE ")) && t.chars().nth(6).is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase()) }).count()).sum::<usize>()
        + it.sheets.len();
    it.category = categorise(&it, &text);
    it.parts = reading_parts(&it.sections, n, x.unit == "page");
    it
}

/// sections from the headings in the text, with their 1-based page / part ranges (top level only when there are
/// many; references kept as a section so a question can name them)
fn sections(d: &Doc) -> Vec<Span> {
    let mut all: Vec<(usize, Option<String>, String)> = Vec::new();
    let bp = boilerplate(d);
    let mut outline = Outline::new(d);
    for (i, p) in d.parts.iter().enumerate() {
        for l in p.lines() {
            if bp.contains(l.trim()) { continue; }
            if let Some((num, h)) = outline.heading(l) { all.push((i + 1, num, h)); }
        }
    }
    let top: Vec<&(usize, Option<String>, String)> = if all.len() > 14 { all.iter().filter(|a| a.1.as_deref().is_none_or(|n| !n.contains('.') || n.matches('.').count() == 1 && n.ends_with('.'))).collect() } else { all.iter().collect() };
    let n = d.parts.len();
    top.iter().enumerate().map(|(i, a)| {
        let next = top.get(i + 1).map_or(n, |b| if b.0 > a.0 { b.0 - 1 } else { a.0 });
        Span { title: a.2.clone(), from: a.0, to: next.max(a.0) }
    }).collect()
}

/// readable parts for the person: along the sections (small ones merged, at most ~6 pages each), else about 5 pages
/// (or parts) at a time. A short document is one part.
fn reading_parts(secs: &[Span], n: usize, pages: bool) -> Vec<Span> {
    let max = if pages { 6 } else { 4 };
    if n <= max { return Vec::new(); }
    let mut out: Vec<Span> = Vec::new();
    let mut good: Vec<&Span> = secs.iter().filter(|s| !s.title.to_lowercase().contains("reference") && !s.title.to_lowercase().contains("bibliograph")).collect();
    good.dedup_by(|b, a| b.from == a.from);
    if good.len() >= 2 {
        let mut start = 1;
        for (i, s) in good.iter().enumerate() {
            let end = good.get(i + 1).map_or(n, |nx| nx.from.saturating_sub(1).max(s.from));
            if s.from > start && out.is_empty() { out.push(Span { title: "Opening".into(), from: start, to: s.from - 1 }); }
            match out.last_mut() {
                // merge a small section into the part before it while that stays small
                Some(l) if l.to + 1 >= s.from && end + 1 - l.from <= max && (l.to + 1 - l.from) < 3 => { l.to = end; l.title = format!("{} · {}", l.title, short_title(&s.title)); }
                _ => out.push(Span { title: short_title(&s.title), from: s.from.max(out.last().map_or(1, |l| l.to + 1)), to: end }),
            }
            start = end + 1;
        }
        // a part longer than `max` is split
        let mut split = Vec::new();
        for p in out {
            if p.to + 1 - p.from <= max + 1 { split.push(p); continue; }
            let mut a = p.from;
            let mut k = 1;
            while a <= p.to { let b = (a + max - 1).min(p.to); split.push(Span { title: format!("{} ({k})", p.title), from: a, to: b }); a = b + 1; k += 1; }
        }
        split.retain(|p| p.from <= p.to);
        return split;
    }
    let step = if pages { 5 } else { 3 };
    (0..n.div_ceil(step)).map(|k| { let a = k * step + 1; let b = ((k + 1) * step).min(n); Span { title: format!("{} {a}–{b}", if pages { "Pages" } else { "Parts" }), from: a, to: b } }).collect()
}

fn short_title(t: &str) -> String {
    let t = t.trim();
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').trim();
    if t.chars().count() > 48 { format!("{}…", t.chars().take(47).collect::<String>()) } else { t.to_string() }
}

/// The category from cheap signals in the text and the structure (no model call).
pub fn categorise(it: &Intake, text: &str) -> String {
    match it.kind.as_str() {
        "xlsx" | "ods" | "ole" | "csv" => return "data".into(),
        "code" => return "code".into(),
        _ => {}
    }
    let low: String = text.chars().take(60_000).collect::<String>().to_lowercase();
    // whole words only ("vat" is not in "private")
    let word_at = |i: usize, k: &str| {
        let before = low[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = low[i + k.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric() || k.ends_with(['.', ':']) || k == "indemnif");
        before && after
    };
    let count = |ks: &[&str]| ks.iter().map(|k| low.match_indices(k).filter(|(i, _)| word_at(*i, k)).count().min(3)).sum::<usize>();
    let secs: Vec<String> = it.sections.iter().map(|s| s.title.to_lowercase()).collect();
    let sec = |k: &str| secs.iter().any(|s| s.contains(k));
    let mut sc: Vec<(&str, usize)> = vec![
        ("paper", count(&["abstract", "et al.", "arxiv", "doi:", "proceedings", "preprint", "we propose", "in this paper", "related work"])
            + 4 * usize::from(sec("references") || sec("bibliography") || low.contains("\nreferences\n") || low.contains("\nbibliography\n"))
            + 3 * usize::from(sec("abstract") || low.starts_with("abstract") || low.contains("\nabstract\n"))
            + 3 * usize::from(low.chars().take(400).collect::<String>().contains("arxiv:"))),
        ("invoice", 3 * count(&["invoice", "total due", "amount due", "bill to", "invoice no", "tax invoice", "receipt", "invoice number"]) + count(&["subtotal", "vat", "qty", "payment terms", "due date", "unit price"])),
        ("contract", 2 * count(&["agreement", "hereinafter", "whereas", "governing law", "the parties", "witness whereof", "party of the first part"]) + count(&["termination", "liability", "indemnif", "confidentiality", "shall not", "obligations"])),
        ("cv", 3 * count(&["curriculum vitae", "résumé", "work experience", "employment history", "professional experience"]) + count(&["education", "skills", "languages", "linkedin.com", "references available"])),
        ("statement", 3 * count(&["statement period", "opening balance", "closing balance", "account number", "iban", "available balance"]) + count(&["transaction", "debit", "credit", "balance"])),
        ("report", 3 * count(&["executive summary"]) + count(&["report", "findings", "recommendation", "recommendations", "quarter", "annual", "kpi", "revenue", "fiscal"]) + 3 * usize::from(sec("executive summary"))),
        ("book", count(&["chapter", "table of contents", "user guide", "manual", "installation", "preface", "isbn", "getting started", "usage", "install"]) + 4 * usize::from(it.pages.unwrap_or(0) >= 80)),
        ("notes", 0),
    ];
    if let Some(p) = it.pages {
        // slides as PDF: many pages with few words each
        if p >= 5 && it.words / p.max(1) < 70 { sc.push(("slides", 9)); }
    }
    sc.sort_by_key(|x| std::cmp::Reverse(x.1));
    match sc[0] {
        (c, s) if s >= 6 => c.into(),
        _ if it.words < 1500 && it.pages.unwrap_or(1) <= 3 => "notes".into(),
        _ => "other".into(),
    }
}

/// How the document fits the turn: (plan, document tokens, budget). The plan is "whole" (it all goes in), "overview"
/// or "selection" (a vague request gets the overview, a question the pages that answer it).
pub fn fit(it: &Intake, max_ctx: usize, history: usize) -> (&'static str, usize, usize) {
    let b = budget(max_ctx, history);
    (if it.tokens <= b { "whole" } else { "overview" }, it.tokens, b)
}

/// The one-line check result: "38 pages · research paper · 7 sections · fits as overview" or "⚠ has macros (not run)".
pub fn check_line(it: &Intake, plan: &str) -> String {
    let mut v: Vec<String> = Vec::new();
    for w in &it.warnings { v.push(format!("⚠ {w}")); }
    match it.pages {
        Some(p) => v.push(plural(p, "page")),
        None if !it.sheets.is_empty() => v.push(if it.sheets.len() == 1 { plural(it.sheets[0].rows, "row") } else { format!("{} · {}", plural(it.sheets.len(), "sheet"), plural(it.sheets.iter().map(|s| s.rows).sum(), "row")) }),
        None => v.push(plural(it.words, "word")),
    }
    v.push(category_word(&it.category).into());
    if !it.sections.is_empty() { v.push(plural(it.sections.len(), "section")); }
    v.push(match plan { "whole" => "fits whole".into(), p => format!("fits as {p}") });
    v.join(" · ")
}

/// The `doc_info` event's fields for `path` (`--serve-jsonl`; README "Attached documents").
pub fn info_json(path: &str, it: &Intake, max_ctx: usize, history: usize) -> serde_json::Value {
    let (plan, tokens, budget) = fit(it, max_ctx, history);
    let span = |s: &Span| serde_json::json!({"title": s.title, "pages": [s.from, s.to]});
    serde_json::json!({"path": path, "name": it.name, "kind": it.kind, "bytes": it.bytes, "pages": it.pages, "unit": it.unit,
        "sheets": it.sheets.iter().map(|s| serde_json::json!({"name": s.name, "rows": s.rows, "columns": s.columns})).collect::<Vec<_>>(),
        "words": it.words, "tokens": tokens, "lang": it.lang, "text_layer": it.text_layer, "category": it.category,
        "category_word": category_word(&it.category), "sections": it.sections.iter().map(span).collect::<Vec<_>>(), "tables": it.tables,
        "outline": it.outline, "parts": it.parts.iter().map(span).collect::<Vec<_>>(),
        "fit": {"plan": plan, "doc_tokens": tokens, "budget": budget}, "warnings": it.warnings, "line": check_line(it, plan)})
}

/// The `doc_info` fields of a file that cannot be attached: its name and the reason as the line.
pub fn refused_json(path: &str, why: &str) -> serde_json::Value {
    serde_json::json!({"path": path, "name": Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "refused": true, "warnings": [why], "line": format!("⚠ {why}")})
}

/// "part 2" / "parts 2-3" named in a question: the page range of those reading parts (0-based pages)
fn part_refs(d: &Doc, q: &str) -> Vec<usize> {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"(?i)\bparts?\s+(\d{1,3})(?:\s*(?:-|–|to|and)\s*(\d{1,3}))?").unwrap());
    let Some(it) = &d.info else { return Vec::new() };
    let mut v = Vec::new();
    for c in r.captures_iter(q) {
        let a: usize = c[1].parse().unwrap_or(0);
        let b: usize = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(a);
        for k in a..=b.min(a + 5) {
            if let Some(p) = it.parts.get(k.wrapping_sub(1)) { for pg in p.from..=p.to { if pg >= 1 && !v.contains(&(pg - 1)) { v.push(pg - 1); } } }
        }
    }
    v
}

/// Result tables that PDF extraction flattens into rows of bare numbers ("Ours TradingAgents 26.62 30.5 8.21 0.91
/// 24.36 …" under "CR%↑ ARR%↑ SR↑ MDD%↓ CR%↑ …" and "… AAPL GOOGL AMZN"): each row is written out with its group and
/// column names ("TradingAgents — AAPL: CR% 26.62, ARR% 30.5, SR 8.21, MDD% 0.91; GOOGL: …"), the abbreviations
/// the document itself defines named once above them ("SR = Sharpe Ratio"). Measured on qwen3-vl:2b: from the bare
/// row it quoted the right line and read the wrong column. Pages without such a table are unchanged.
fn label_tables(pages: Vec<String>) -> Vec<String> {
    let abbr = abbreviations(&pages.join("\n"));
    pages.into_iter().map(|p| label_page(&p, &abbr)).collect()
}

/// "Sharpe Ratio (SR)", "Annualized Rate of Return (ARR)": abbreviation → phrase, when the capitals match
fn abbreviations(t: &str) -> std::collections::HashMap<String, String> {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let r = R.get_or_init(|| regex::Regex::new(r"((?:[A-Z][a-z]+(?:\s+(?:of|and|the|to|for|in|on))?\s+){0,4}[A-Z][a-z]+)\s*\(([A-Z]{2,6})s?\)").unwrap());
    let mut m = std::collections::HashMap::new();
    for c in r.captures_iter(t) {
        let ab = c[2].to_string();
        let caps: Vec<&str> = c[1].split_whitespace().filter(|w| w.starts_with(|ch: char| ch.is_ascii_uppercase())).collect();
        // the last words whose initials spell it ("We use the Sharpe Ratio (SR)" → "Sharpe Ratio")
        if caps.len() < ab.len() { continue; }
        let tail = &caps[caps.len() - ab.len()..];
        if tail.iter().map(|w| w.chars().next().unwrap_or(' ')).collect::<String>() != ab { continue; }
        let start = c[1].find(tail[0]).unwrap_or(0);
        m.entry(ab).or_insert_with(|| c[1][start..].split_whitespace().collect::<Vec<_>>().join(" "));
    }
    m
}

fn label_page(page: &str, abbr: &std::collections::HashMap<String, String>) -> String {
    let lines: Vec<&str> = page.lines().collect();
    let num = |t: &str| t == "-" || t == "–" || t.trim_end_matches('%').trim_start_matches(['+', '-']).parse::<f64>().is_ok();
    let key = |t: &str| t.trim_end_matches(['↑', '↓']).to_string();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    let mut changed = false;
    while i < lines.len() {
        let toks: Vec<&str> = lines[i].split_whitespace().collect();
        // a column header repeated k times: m metric names, each short and mostly capitals
        let metric = |t: &str| { let k = key(t); let l = k.trim_end_matches('%'); (1..=8).contains(&l.len()) && l.chars().all(|c| c.is_ascii_uppercase() || c == '@' || c.is_ascii_digit()) };
        let mut shape = None;
        if toks.len() >= 4 && toks.iter().all(|t| metric(t)) {
            for m in 1..=toks.len() / 2 {
                if toks.len().is_multiple_of(m) && (m..toks.len()).all(|j| key(toks[j]) == key(toks[j % m])) { shape = Some((m, toks.len() / m)); break; }
            }
        }
        let Some((m, k)) = shape.filter(|&(m, k)| k >= 2 && m >= 2) else { out.push(lines[i].to_string()); i += 1; continue; };
        // the group names: the last k words of a line just above ("Categories Models AAPL GOOGL AMZN")
        let groups: Vec<String> = (i.saturating_sub(3)..i).rev().find_map(|j| {
            let w: Vec<&str> = lines[j].split_whitespace().collect();
            (w.len() >= k && !w[w.len() - k..].iter().any(|t| num(t))).then(|| w[w.len() - k..].iter().map(|s| s.to_string()).collect())
        }).unwrap_or_else(|| (1..=k).map(|g| format!("group {g}")).collect());
        let names: Vec<String> = toks[..m].iter().map(|t| key(t)).collect();
        let defs: Vec<String> = names.iter().filter_map(|n| abbr.get(n.trim_end_matches('%')).map(|p| format!("{n} = {p}"))).collect();
        let dirs: Vec<String> = toks[..m].iter().filter_map(|t| if t.ends_with('↑') { Some(format!("{} higher is better", key(t))) } else if t.ends_with('↓') { Some(format!("{} lower is better", key(t))) } else { None }).collect();
        let mut note = format!("[table, one row per line: {} for each of {}", names.join(", "), groups.join(", "));
        if !defs.is_empty() { note.push_str(&format!("; {}", defs.join(", "))); }
        if !dirs.is_empty() { note.push_str(&format!("; {}", dirs.join(", "))); }
        note.push(']');
        let mut rows = Vec::new();
        let mut j = i + 1;
        while j < lines.len() && rows.len() < 40 {
            let w: Vec<&str> = lines[j].split_whitespace().collect();
            if w.is_empty() { j += 1; continue; }
            let n = w.iter().rev().take_while(|t| num(t)).count();
            if n != m * k || w.len() == n { break; }
            let vals = &w[w.len() - n..];
            let label = w[..w.len() - n].join(" ");
            let cells: Vec<String> = (0..k).map(|g| format!("{}: {}", groups[g], (0..m).map(|c| format!("{} {}", names[c], vals[g * m + c])).collect::<Vec<_>>().join(", "))).collect();
            rows.push(format!("{label} — {}", cells.join("; ")));
            j += 1;
        }
        if rows.is_empty() { out.push(lines[i].to_string()); i += 1; continue; }
        changed = true;
        out.push(note);
        out.extend(rows);
        i = j;
    }
    if changed { out.join("\n") } else { page.to_string() }
}

/// The line a document leaves in the conversation (history, saved session, fold notes): `[Attached: report.pdf, 38
/// pages · .elisa/uploads/<session>/report.pdf]`. Its text never stays there: each request gets the parts it needs
/// (`block_for` / `follow_up`), for that request only.
pub fn reference(d: &Doc) -> String { ref_line(&d.name, &d.units, &d.rel) }

fn ref_line(name: &str, units: &str, rel: &str) -> String { format!("{MARK}{name}, {units} · {rel}]") }

/// A document named in a saved conversation: a reference line, or an older session's inline block
/// (`[Attached: ta.pdf, 38 pages; overview below…` with `The full file is saved at <path>`, or a header with
/// `Saved at: <path>`, or a small document's whole text with no path).
#[derive(Debug, Clone, PartialEq)]
pub struct DocRef { pub name: String, pub units: String, pub rel: Option<String>, pub old: bool }

impl DocRef {
    /// the line the history keeps from now on (without a path: the header line only)
    pub fn line(&self) -> String {
        match &self.rel { Some(r) => ref_line(&self.name, &self.units, r), None => format!("{MARK}{}, {}]", self.name, self.units) }
    }
}

/// `block` (a user-message text block, or a line of the notes from `[Attached: ` on) as a document reference
pub fn parse_ref(block: &str) -> Option<DocRef> {
    let head = block.lines().next()?.strip_prefix(MARK)?;
    let (name, rest) = head.split_once(", ")?;
    let one_line = !block.trim_end().contains('\n');
    if one_line {
        if let Some((units, rel)) = rest.rsplit_once(" · ") {
            if let Some(rel) = rel.strip_suffix(']') {
                if rel.contains("/") { return Some(DocRef { name: name.into(), units: units.into(), rel: Some(rel.into()), old: false }); }
            }
        }
    }
    let units = rest.split([';', ']', ',']).next().unwrap_or("").trim().to_string();
    let path_after = |key: &str| block.find(key).map(|i| {
        let t = &block[i + key.len()..];
        t.split([';', '\n', ' ', ')']).next().unwrap_or("").trim_end_matches(['.', ',']).to_string()
    }).filter(|p| !p.is_empty());
    let rel = path_after("The full file is saved at ").or_else(|| path_after("Saved at: "));
    Some(DocRef { name: name.into(), units, rel, old: true })
}

/// The files in this folder's upload area, newest first, one per name and size (the same file attached in two
/// sessions is one), at most DOCS_MAX: the candidates when a resumed conversation names no document by reference
/// (a session folded before references were kept).
pub fn uploads(root: &Path) -> Vec<String> {
    let area = root.join(".elisa").join("uploads");
    let mut v: Vec<(std::time::SystemTime, String, u64, String)> = Vec::new();
    for s in std::fs::read_dir(&area).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(s.path()).into_iter().flatten().flatten() {
            let Ok(m) = f.metadata() else { continue };
            if !m.is_file() || f.file_name().to_string_lossy().ends_with(".txt") && f.path().with_extension("").is_file() { continue; }
            let Ok(rel) = f.path().strip_prefix(root).map(|r| r.display().to_string()) else { continue };
            v.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), f.file_name().to_string_lossy().to_string(), m.len(), rel));
        }
    }
    v.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out: Vec<(String, u64, String)> = Vec::new();
    for (_, n, l, r) in v { if !out.iter().any(|(n2, l2, _)| *n2 == n && *l2 == l) { out.push((n, l, r)); } }
    out.into_iter().take(DOCS_MAX).map(|(_, _, r)| r).collect()
}

/// Is `d` the document a conversation (`text`: its words and notes) was about, though it never names the file by
/// reference: its extracted title from the start ("TradingAgents: Multi-Agents LLM Financial Trading Framework", at
/// least 4 words and 20 letters), or its file name, appears in the text. A few shared words ("financial", "market") never count: a newer
/// paper on a related topic must not be taken for the one the conversation discussed.
pub fn named_in(d: &Doc, text: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    let t = norm(text);
    let stem = norm(&Path::new(&d.name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
    if stem.len() >= 8 && t.contains(&stem) { return true; }
    // the title's leading words (`title` may run on into the author line): at least 4 words and 20 letters of it
    let title = norm(&title(d));
    let w: Vec<&str> = title.split(' ').filter(|x| !x.is_empty()).collect();
    (4..=w.len()).rev().map(|n| w[..n].join(" ")).any(|p| p.len() >= 20 && t.contains(&p))
}

/// The files in `<root>/.elisa/uploads/<session>/`: what that session attached itself.
pub fn uploads_of(root: &Path, session: u64) -> Vec<String> {
    let dir = upload_dir(root, session);
    let mut v: Vec<String> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))
        .filter(|f| !(f.file_name().to_string_lossy().ends_with(".txt") && f.path().with_extension("").is_file()))
        .filter_map(|f| f.path().strip_prefix(root).ok().map(|r| r.display().to_string())).collect();
    v.sort();
    v
}

/// the newest `<root>/.elisa/uploads/<session>/<name>` (an older session's small document carried no path)
pub fn find_upload(root: &Path, name: &str) -> Option<String> {
    let area = root.join(".elisa").join("uploads");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&area).ok()?.flatten() {
        let p = e.path().join(name);
        if let Ok(m) = std::fs::metadata(&p) {
            let t = m.modified().unwrap_or(std::time::UNIX_EPOCH);
            if m.is_file() && best.as_ref().is_none_or(|(b, _)| t > *b) { best = Some((t, p)); }
        }
    }
    best.and_then(|(_, p)| p.strip_prefix(root).ok().map(|r| r.display().to_string()))
}

/// Does `q` refer to the attached document: a page / section / part named, its file name, a word for a document
/// ("the paper", "this pdf", "back to the report"), or most of its content words found in the text ("what about the
/// risk team?" after a paper on trading teams). False for a new topic ("what's the weather in Hanoi?").
pub fn mentions(d: &Doc, q: &str) -> bool {
    if !part_refs(d, q).is_empty() || !refs(q).is_empty() || !section_refs(q).is_empty() { return true; }
    let ql = q.to_lowercase();
    let stem = Path::new(&d.name).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    if stem.len() >= 3 && ql.contains(&stem) { return true; }
    const DOC_WORDS: &[&str] = &["paper", "document", "doc", "pdf", "file", "report", "article", "attachment", "attached", "spreadsheet",
        "sheet", "csv", "table", "chapter", "author", "authors", "tài liệu", "bài báo", "file"];
    let w = words(&ql);
    if DOC_WORDS.iter().any(|k| if k.contains(' ') { ql.contains(k) } else { w.iter().any(|x| x == k) }) { return true; }
    let cw = content_words(q);
    if cw.is_empty() { return false; }
    let text = d.text.to_lowercase();
    let found = cw.iter().filter(|c| text.contains(c.as_str())).count();
    found * 2 > cw.len() && found >= 1 && (cw.len() > 1 || cw[0].len() >= 4)
}

/// an attached-document block of a user message (`[Attached: …` or a follow-up's `[From …`)
pub fn is_block(t: &str) -> bool { t.starts_with(MARK) || (t.starts_with("[From ") && t.contains("picked for this question]")) }

/// `block`'s first line, for memory and notes: `[Attached: report.pdf, 2 pages]`.
pub fn first_line(block: &str) -> &str { block.lines().next().unwrap_or(block) }

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-attach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn docx_xml_to_text() {
        let x = r#"<w:document><w:body><w:p><w:r><w:t>Hello &amp; welcome</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve"> there</w:t></w:r></w:p><w:p/><w:p><w:r><w:t>Line&#33;</w:t><w:br/><w:t>two</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(docx_xml_text(x), "Hello & welcome\t there\n\nLine!\ntwo");
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name(Path::new("/a/b/..hidden.pdf")), "hidden.pdf");
        assert_eq!(safe_name(Path::new("/a/we:ird|name?.txt")), "we_ird_name_.txt");
        assert_eq!(safe_name(Path::new("/")), "document");
    }

    #[test]
    fn head_cuts_near_a_line_end() {
        let s = "a".repeat(10) + "\n" + &"b".repeat(2000);
        assert_eq!(head(&s, 1000), s.as_str());
        assert!(head(&s, 100).len() <= 400);
    }

    #[test]
    fn stage_copies_and_reuses() {
        let root = tmp("stage");
        let out = tmp("stage-out");
        let f = out.join("n.txt");
        std::fs::write(&f, "hello").unwrap();
        let a = stage(&root, 7, &f).unwrap();
        assert_eq!(a, root.join(".elisa/uploads/7/n.txt"));
        assert_eq!(stage(&root, 7, &f).unwrap(), a, "same file: same copy");
        std::fs::write(&f, "other").unwrap();
        assert_eq!(stage(&root, 7, &f).unwrap(), root.join(".elisa/uploads/7/n-2.txt"));
        assert!(stage(&root, 7, &out.join("missing.pdf")).unwrap_err().contains("no such file"));
        assert!(stage(&root, 7, &out).unwrap_err().contains("folder"));
    }

    #[test]
    fn a_symlinked_upload_area_is_refused() {
        let root = tmp("sym");
        let elsewhere = tmp("sym-else");
        std::os::unix::fs::symlink(&elsewhere, root.join(".elisa")).unwrap();
        let f = elsewhere.join("x.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(stage(&root, 1, &f).unwrap_err().contains("outside the project"));
    }

    /// a minimal PDF, one text line per page; `extra` goes into the catalog (an /OpenAction, /Names …) and `encrypt`
    /// adds a standard security handler with a non-empty user password
    fn pdf(pages: &[&str], extra: &str, encrypt: bool) -> Vec<u8> {
        let n = pages.len();
        let font = 3 + 2 * n;
        let mut objs: Vec<String> = vec![format!("<< /Type /Catalog /Pages 2 0 R {extra} >>"),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect::<Vec<_>>().join(" "))];
        for (i, t) in pages.iter().enumerate() {
            let c = format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET");
            objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + 2 * i));
            objs.push(format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()));
        }
        objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
        let trailer = if encrypt {
            objs.push(format!("<< /Filter /Standard /V 1 /R 2 /P -4 /O <{}> /U <{}> >>", "ab".repeat(32), "cd".repeat(32)));
            format!(" /Encrypt {} 0 R /ID [<{}> <{}>]", objs.len(), "01".repeat(16), "01".repeat(16))
        } else { String::new() };
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() { offs.push(out.len()); out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes()); }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offs { out.extend(format!("{o:010} 00000 n \n").bytes()); }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R{trailer} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        out
    }

    fn zip_of(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default().compression_method(if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored });
        for (n, b) in entries { w.start_file(*n, o).unwrap(); w.write_all(b).unwrap(); }
        w.finish().unwrap().into_inner()
    }

    const DOC_XML: &[u8] = b"<w:document><w:body><w:p><w:r><w:t>Quarterly memo</w:t></w:r></w:p></w:body></w:document>";

    #[test]
    fn check_refuses_encrypted_pdfs_and_warns_on_active_content() {
        let d = tmp("check-pdf");
        let enc = d.join("locked.pdf");
        std::fs::write(&enc, pdf(&["secret"], "", true)).unwrap();
        let e = intake(&enc).unwrap_err();
        assert!(e.contains("password-protected"), "{e}");
        let js = d.join("js.pdf");
        std::fs::write(&js, pdf(&["hello there reader"], "/OpenAction << /S /JavaScript /JS (app.alert(1)) >> /Names << /EmbeddedFiles 9 0 R >>", false)).unwrap();
        let (_, w) = check(&js).unwrap();
        assert!(w.iter().any(|x| x.contains("JavaScript (not run")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("embedded files")), "{w:?}");
        let it = intake(&js).unwrap();
        assert!(check_line(&it, "whole").starts_with("⚠ has JavaScript"), "{}", check_line(&it, "whole"));
    }

    #[test]
    fn check_warns_on_macros_and_refuses_zip_bombs() {
        let d = tmp("check-zip");
        let m = d.join("memo.docx");
        std::fs::write(&m, zip_of(&[("word/document.xml", DOC_XML), ("word/vbaProject.bin", b"\x00macro")], false)).unwrap();
        let (k, w) = check(&m).unwrap();
        assert_eq!(k, "docx");
        assert!(w.iter().any(|x| x.contains("macros (not run")), "{w:?}");
        let it = intake(&m).unwrap();
        assert_eq!(it.kind, "docx");
        assert!(it.words >= 2);
        // 60 MB of zeros deflates to a few dozen KB: refused before any parser reads it
        let zeros = vec![0u8; 60 << 20];
        let b = d.join("bomb.docx");
        std::fs::write(&b, zip_of(&[("word/document.xml", DOC_XML), ("word/media/x.bin", &zeros)], true)).unwrap();
        assert!(std::fs::metadata(&b).unwrap().len() < 1 << 20);
        let e = check(&b).unwrap_err();
        assert!(e.contains("zip bomb"), "{e}");
    }

    #[test]
    fn check_warns_on_a_type_that_does_not_match_the_extension() {
        let d = tmp("check-ext");
        let f = d.join("report.pdf");
        std::fs::write(&f, "just some plain text notes\nsecond line\n").unwrap();
        let (k, w) = check(&f).unwrap();
        assert_eq!(k, "text");
        assert!(w.iter().any(|x| x.contains("named .pdf but it is a text file")), "{w:?}");
        let x = d.join("data.xlsx");
        std::fs::write(&x, pdf(&["a page"], "", false)).unwrap();
        assert!(check(&x).unwrap().1.iter().any(|w| w.contains("named .xlsx but it is a PDF")));
        let z = d.join("bundle.docx");
        std::fs::write(&z, zip_of(&[("readme.txt", b"hi")], false)).unwrap();
        assert!(check(&z).unwrap_err().contains("zip archive"));
        let big = d.join("big.txt");
        std::fs::File::create(&big).unwrap().set_len(MAX_BYTES + 1).unwrap();
        assert!(check(&big).unwrap_err().contains("too large"));
    }

    fn cat(text: &str, pages: Option<usize>) -> String {
        let it = Intake { kind: "pdf".into(), pages, words: text.split_whitespace().count(), ..Default::default() };
        categorise(&it, text)
    }

    #[test]
    fn categories_from_cheap_signals() {
        assert_eq!(cat("Abstract\nWe propose a method. In this paper we show ... et al. (2023)\nReferences\n[1] Smith et al.", Some(12)), "paper");
        assert_eq!(cat("TAX INVOICE\nInvoice No: 4471\nBill to: ACME\nQty Unit price\nSubtotal 300\nVAT 30\nTotal due 330", Some(1)), "invoice");
        assert_eq!(cat("SERVICE AGREEMENT\nThis Agreement is made between the parties (hereinafter the Client). Whereas ... Governing law: England. Termination. Liability.", Some(6)), "contract");
        assert_eq!(cat("Jane Doe — Curriculum Vitae\nWork experience\n2019-2024 Engineer\nEducation\nSkills: Rust", Some(2)), "cv");
        assert_eq!(cat("Statement period 1-31 Oct\nAccount number 1234\nOpening balance 100\nClosing balance 80\nTransaction Debit Credit", Some(2)), "statement");
        assert_eq!(cat("Executive summary\nThe annual report shows revenue up. Findings and recommendations follow.", Some(2)), "report");
        let it = Intake { kind: "xlsx".into(), ..Default::default() };
        assert_eq!(categorise(&it, "a b c"), "data");
        let slides = (0..10).map(|i| format!("Slide {i}\nGrowth")).collect::<Vec<_>>().join("\n");
        assert_eq!(cat(&slides, Some(10)), "slides");
        assert_eq!(cat("buy milk\ncall mum", Some(1)), "notes");
    }

    fn paper() -> Doc {
        let mut parts = vec!["Great Paper Title Here\nJane Doe\n\nAbstract\nWe study widgets at scale and show they work well in practice for everyone.\n\n1. Introduction\nWidgets matter. They are everywhere.".to_string()];
        for i in 2..=9 { parts.push(format!("Running Header\n{}. Section {i} Name\nThe body of section {i} talks about topic{i} in depth. More text follows here.\nfiller line {i}\n{i}", i)); }
        parts.push("Running Header\n10. Conclusion\nWidgets win. Future work remains.\nReferences\n[1] A. Author. A paper. 2020.".into());
        let text = parts.join("\n");
        Doc { name: "p.pdf".into(), rel: ".elisa/uploads/1/p.pdf".into(), bytes: 1000, units: "10 pages".into(), tokens: 50_000, text, how: ("docs_pdf", ""),
            text_rel: None, parts, part: "page", path: PathBuf::from("/nonexistent/p.pdf"), info: None }
    }

    #[test]
    fn vague_requests_get_an_overview_with_every_heading() {
        for q in ["read this one for me", "summarise", "", "what is this about?", "Summarise this document.", "give me the main points of the paper"] {
            assert!(is_vague(q), "{q:?}");
        }
        for q in ["what are the agent roles?", "explain section 4", "what is on page 3?", "and what about the risk team?"] { assert!(!is_vague(q), "{q:?}"); }
        let d = paper();
        let (b, pick) = block_for(&d, "read this one for me", 2000, &[]);
        assert_eq!(pick, Pick::Overview);
        assert!(b.starts_with("[Attached: p.pdf, 10 pages; overview below, ask about any part for detail]"), "{b}");
        assert!(b.contains("Title: Great Paper Title Here") && b.contains("Abstract: We study widgets"), "{b}");
        assert!(b.contains("## 5. Section 5 Name (p. 5)") && b.contains("## 10. Conclusion (p. 10)") && b.contains("Widgets win"), "{b}");
        assert!(!b.contains("A. Author") && !b.contains("Running Header"), "references and running headers left out: {b}");
        assert!(estimate_tokens(&b) <= 2000 + 120);
    }

    #[test]
    fn a_refused_attachment_is_told_in_a_line_views_strip() {
        let n = refused_note(&["locked.pdf is a password-protected PDF; it cannot be read".to_string()]);
        assert!(n.contains("locked.pdf is a password-protected PDF") && n.contains("never guess"), "{n}");
        let msg = format!("what does this PDF say?{n}");
        assert_eq!(crate::context::request_body(&msg), "what does this PDF say?");
        assert!(not_readable("garbage.txt", "txt").contains("holds binary data, not text"));
        assert!(not_readable("slides.pptx", "pptx").contains("save it as PDF"));
    }

    #[test]
    fn figure_labels_are_not_headings_in_the_overview() {
        let mut d = paper();
        // a figure's axis labels on several pages ("10 Kronossmall" over and over), and a stray "18. The table"
        for p in d.parts.iter_mut().skip(5) { p.push_str("\n10 Kronossmall\n15 TimeVAE\n10 Kronossmall\n18. The table"); }
        d.text = d.parts.join("\n");
        let o = overview(&d, 4000);
        assert!(!o.contains("## 10 Kronossmall"), "{o}");
        assert!(!o.contains("## 15 TimeVAE") && !o.contains("## 18. The table"), "{o}");
        for k in 2..=9 { assert_eq!(o.matches(&format!("## {k}. Section {k} Name")).count(), 1, "{o}"); }
        assert!(o.contains("## 10. Conclusion"), "{o}");
        // the same heading twice is listed once
        let mut d = paper();
        d.parts[3].push_str("\n3. Section 3 Name");
        assert_eq!(overview(&d, 4000).matches("## 3. Section 3 Name").count(), 1);
    }

    #[test]
    fn specific_questions_get_the_pages_that_answer_them() {
        let d = paper();
        let (b, pick) = block_for(&d, "what does it say about topic7?", 2000, &[]);
        assert_eq!(pick, Pick::Parts(vec![6]), "{b}");
        assert!(b.contains("page 7 below, picked for this question") && b.contains("topic7 in depth"), "{b}");
        // explicit pages and sections win; already sent pages are left out of a follow-up, unless named
        assert_eq!(select(&d, "what is on page 3?", 2000, &[]), vec![2]);
        assert!(select(&d, "explain section 4", 2000, &[]).contains(&3));
        assert_eq!(follow_up(&d, "and topic7 again?", 2000, &[6]), None);
        assert!(follow_up(&d, "what about topic8?", 2000, &[6]).unwrap().0.starts_with("[From p.pdf, 10 pages: page 8 below"));
        assert_eq!(follow_up(&d, "thanks!", 2000, &[]), None);
    }

    #[test]
    fn refusals_are_recognised() {
        for r in ["I'm sorry, but I can't process the PDF file you've attached.", "I am unable to access the document.", "I cannot open attachments.",
            "Unfortunately I don't have the ability to read files."] { assert!(refusal(r), "{r}"); }
        for r in ["The paper proposes TradingAgents.", "You can't go wrong with option 2."] { assert!(!refusal(r), "{r}"); }
    }

    #[test]
    fn budget_follows_the_window() {
        assert_eq!(budget(16384, 0), 7372);
        assert_eq!(budget(16384, 7000), 2000, "a long session keeps a floor");
        assert_eq!(budget(4096, 4000), 600);
    }

    #[test]
    fn a_question_refers_to_the_document_or_not() {
        let d = paper();
        assert!(reference(&d).starts_with("[Attached: p.pdf, 10 pages · .elisa/uploads/1/p.pdf]"));
        assert!(is_block(&reference(&d)));
        for q in ["back to the paper", "what does topic7 say?", "explain section 4", "what is on page 3?", "and widgets?"] { assert!(mentions(&d, q), "{q}"); }
        for q in ["what's the weather in Hanoi?", "write me a haiku about cats", "thanks"] { assert!(!mentions(&d, q), "{q}"); }
    }

    #[test]
    fn flattened_tables_get_their_column_names() {
        let abbr = abbreviations("We report the Cumulative Returns (CR), the Annualized Rate of Return (ARR), the Sharpe Ratio (SR) and Maximum Drawdown (MDD).");
        assert_eq!(abbr.get("SR").map(String::as_str), Some("Sharpe Ratio"));
        assert_eq!(abbr.get("ARR").map(String::as_str), Some("Annualized Rate of Return"));
        let page = "Table 1: Performance.\n\nCategories Models AAPL GOOGL AMZN\n\nCR%↑ ARR%↑ SR↑ MDD%↓ CR%↑ ARR%↑ SR↑ MDD%↓ CR%↑ ARR%↑ SR↑ MDD%↓\n\n\
            Market B&H -5.23 -5.09 -1.29 11.90 7.78 8.09 1.35 13.04 17.1 17.6 3.53 3.80\n\nRule-based MACD -1.49 -1.48 -0.81 4.53 6.20 6.26 2.31 1.22 - - - -\n\
            Ours TradingAgents 26.62 30.5 8.21 0.91 24.36 27.58 6.39 1.69 23.21 24.90 5.60 2.11\n\nSome text after.";
        let out = label_page(page, &abbr);
        assert!(out.contains("Ours TradingAgents — AAPL: CR% 26.62, ARR% 30.5, SR 8.21, MDD% 0.91; GOOGL: CR% 24.36, ARR% 27.58, SR 6.39, MDD% 1.69; AMZN:"), "{out}");
        assert!(out.contains("SR = Sharpe Ratio") && out.contains("MDD% lower is better"), "{out}");
        assert!(out.contains("Rule-based MACD — AAPL: CR% -1.49") && out.contains("Some text after."), "{out}");
        assert_eq!(label_page("plain text\nno table", &abbr), "plain text\nno table");
    }

    #[test]
    fn saved_references_old_and_new_are_parsed() {
        let r = parse_ref("[Attached: ta.pdf, 38 pages · .elisa/uploads/17/ta.pdf]").unwrap();
        assert_eq!(r, DocRef { name: "ta.pdf".into(), units: "38 pages".into(), rel: Some(".elisa/uploads/17/ta.pdf".into()), old: false });
        assert_eq!(r.line(), "[Attached: ta.pdf, 38 pages · .elisa/uploads/17/ta.pdf]");
        let old = "[Attached: ta.pdf, 38 pages; overview below, ask about any part for detail]\n<untrusted>Title: X</untrusted>\n(The full file is saved at .elisa/uploads/9/ta.pdf; docs_pdf reads any other page.)";
        let r = parse_ref(old).unwrap();
        assert!(r.old && r.rel.as_deref() == Some(".elisa/uploads/9/ta.pdf") && r.units == "38 pages", "{r:?}");
        let older = "[Attached: h.pdf, 40 pages, 1.2 MB]\nSaved at: .elisa/uploads/3/h.pdf\nUse docs_pdf.";
        assert_eq!(parse_ref(older).unwrap().rel.as_deref(), Some(".elisa/uploads/3/h.pdf"));
        let small = parse_ref("[Attached: memo.pdf, 1 page]\n<untrusted>hello</untrusted>").unwrap();
        assert!(small.old && small.rel.is_none() && small.line() == "[Attached: memo.pdf, 1 page]");
        assert!(parse_ref("hello").is_none());
    }

    #[test]
    fn open_reads_only_inside_the_upload_area() {
        let root = tmp("open");
        let out = tmp("open-out");
        std::fs::write(out.join("n.txt"), "some notes here").unwrap();
        let dst = stage(&root, 5, &out.join("n.txt")).unwrap();
        let rel = dst.strip_prefix(&root).unwrap().display().to_string();
        let d = open(&root, &rel).unwrap();
        assert_eq!(d.rel, rel);
        assert!(d.text.contains("some notes"));
        assert_eq!(find_upload(&root, "n.txt").as_deref(), Some(rel.as_str()));
        assert!(open(&root, "../open-out/n.txt").is_err());
        assert!(open(&root, ".elisa/uploads/5/gone.txt").unwrap_err().contains("no longer"));
    }

    #[test]
    fn a_document_is_known_by_its_title() {
        let d = paper();
        assert!(named_in(&d, "Elisa analysed the Great Paper Title Here and its results"));
        assert!(named_in(&d, "the \"great paper title here\" paper"), "case and punctuation do not matter");
        assert!(!named_in(&d, "a great paper with a title"), "some of its words are not its title");
        assert!(!named_in(&d, "great paper title"), "too short to tell");
        assert!(!named_in(&d, "the user asked about the weather"));
    }
}
