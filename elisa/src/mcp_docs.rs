//! Elisa's own documents MCP server: `elisa --mcp-serve docs --root DIR [--max-chars N]`, a stdio MCP server with two
//! tools, `docs_pdf` and `docs_sheet` (prefixed with the server name, so `mcp_host` offers them unchanged), for files a
//! plain text `read` cannot show: PDF text (pdf-extract), spreadsheets (calamine: xlsx / xlsm / xlsb / xls / ods) and
//! CSV / TSV (read here). The session starts it like any other MCP
//! server (`mcp_host.rs`) unless config `docs = "off"`. MCP calls are approved per server, not per path, so the server
//! itself keeps to the project folder (`--root`, the session cwd): every path is resolved (`..` and symlinks, as
//! `outside.rs` does) and one outside the folder is refused. No network. Output stays under `--max-chars` (the
//! session's tool output cap), cut at a page / row boundary with a `next:` hint so a big file is read in parts.
use crate::outside;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// the output cap when the session does not pass one (= the default `tool_output_max_chars`)
pub const DOC_CHARS: usize = 6000;
/// a file larger than this is not loaded (a PDF / workbook is parsed in memory)
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// one spreadsheet cell shows at most this many characters
const CELL_CHARS: usize = 200;
const SHEET_EXTS: [&str; 7] = ["xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv"];

#[derive(Clone, Debug)]
pub struct DocsCfg {
    /// the canonical project root: nothing outside it is read
    pub root: PathBuf,
    /// the most text one call returns
    pub max_chars: usize,
}

/// the tools this server lists (MCP `tools/list`)
pub fn tools() -> Value {
    json!([
        {"name": "docs_pdf", "description": "Read the text of a PDF file in the project.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"},
            "pages": {"type": "string", "description": "e.g. \"3\" or \"2-5\"; default all"}}, "required": ["path"]}},
        {"name": "docs_sheet", "description": "Read a spreadsheet in the project (xlsx, xls, ods, csv) as rows.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"},
            "sheet": {"type": "string", "description": "sheet name; default first"},
            "range": {"type": "string", "description": "e.g. \"A1:D50\" or rows \"10-60\""}}, "required": ["path"]}},
    ])
}

/// one `tools/call`: (text, is_error)
pub fn call(name: &str, args: &Value, cfg: &DocsCfg) -> (String, bool) {
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let r = match name {
        "docs_pdf" => match arg("path") { Some(p) => read_pdf(p, arg("pages"), cfg), None => Err("missing path".into()) },
        "docs_sheet" => match arg("path") { Some(p) => read_sheet(p, arg("sheet"), arg("range"), cfg), None => Err("missing path".into()) },
        n => Err(format!("unknown tool {n}")),
    };
    match r { Ok(t) => (t, false), Err(e) => (e, true) }
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, cfg: &DocsCfg) -> Option<Value> {
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-docs", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], cfg);
            json!({"content": [{"type": "text", "text": text}], "isError": err})
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `--mcp-serve docs …`: serve stdin / stdout until stdin closes
pub fn serve(args: &[String]) -> i32 {
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let root = opt("--root").map(PathBuf::from).or_else(|| std::env::current_dir().ok()).unwrap_or_else(|| PathBuf::from("."));
    let max_chars = opt("--max-chars").and_then(|v| v.parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(DOC_CHARS);
    let cfg = DocsCfg { root: outside::root(&root), max_chars };
    // a parser panic on a malformed file is caught per call (`guard`); keep its message off stderr
    std::panic::set_hook(Box::new(|_| {}));
    // the session's result cache, when it handed one over (`cache::child_env`)
    crate::cache::from_env();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(a) = answer(&m, &cfg) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
    0
}

/// `p` resolved inside the project, an existing file of a loadable size; else the error for the model
fn project_file(p: &str, cfg: &DocsCfg) -> Result<PathBuf, String> {
    let r = outside::resolve(&cfg.root, Path::new(p));
    if !outside::inside(&cfg.root, &r) {
        return Err(format!("{p} is outside the project folder; only files inside {} can be read", cfg.root.display()));
    }
    let md = std::fs::metadata(&r).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { format!("no such file: {p}") } else { format!("cannot read {p}: {e}") })?;
    if md.is_dir() { return Err(format!("{p} is a directory")); }
    if md.len() > MAX_FILE_BYTES { return Err(format!("{p} is too large ({} MB; the limit is {} MB)", md.len() >> 20, MAX_FILE_BYTES >> 20)); }
    Ok(r)
}

fn ext(path: &Path) -> String { path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default() }

/// `f` with a parser panic turned into an error (the parsers trust malformed files in places)
fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(format!("{what}: the file looks damaged (the parser failed)")))
}

/// room for the text itself: the cap less the header and the `next:` line
fn room(cfg: &DocsCfg) -> usize { cfg.max_chars.saturating_sub(200).max(200) }

/// `s` cut to at most `n` characters
fn take(s: &str, n: usize) -> &str { s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i]) }

// ---------- PDF ----------

/// a page selection: "3", "2-5", "4-" (to the end), "1,3,7-9"; 1-based, within 1..=total
fn parse_pages(spec: &str, total: u32) -> Result<Vec<u32>, String> {
    let bad = || format!("pages {spec:?}: use e.g. \"3\" or \"2-5\" (the PDF has {total} pages)");
    let num = |s: &str| s.trim().parse::<u32>().map_err(|_| bad());
    let mut out: Vec<u32> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (num(a)?, if b.trim().is_empty() { total } else { num(b)? }),
            None => { let n = num(part)?; (n, n) }
        };
        if a == 0 || a > b { return Err(bad()); }
        if a > total { return Err(format!("page {a} is past the end (the PDF has {total} pages)")); }
        for n in a..=b.min(total) { if !out.contains(&n) { out.push(n); } }
    }
    if out.is_empty() { return Err(bad()); }
    Ok(out)
}

/// "1-3,5" for a list of pages
fn page_list(pages: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let mut j = i;
        while j + 1 < pages.len() && pages[j + 1] == pages[j] + 1 { j += 1; }
        parts.push(if i == j { pages[i].to_string() } else { format!("{}-{}", pages[i], pages[j]) });
        i = j + 1;
    }
    parts.join(",")
}

/// page text tidied for a small context: trailing spaces off, at most one blank line in a row
fn tidy(t: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for l in t.lines().map(str::trim_end) {
        if l.is_empty() { blank += 1; if blank > 1 { continue; } } else { blank = 0; }
        out.push_str(l);
        out.push('\n');
    }
    out.trim().to_string()
}

fn pdf_error(p: &str, e: pdf_extract::Error) -> String {
    use pdf_extract::Error as E;
    match e {
        E::Decryption(_) | E::InvalidPassword | E::AlreadyEncrypted => format!("{p} is a password-protected PDF; it cannot be read"),
        E::Unimplemented(m) if m.contains("encrypt") => format!("{p} is a password-protected PDF; it cannot be read"),
        e => format!("cannot read {p} as a PDF: {e}"),
    }
}

fn is_pdf(bytes: &[u8]) -> bool { bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-") }

/// the PDF loaded and opened (an empty user password is opened on load; anything else stays encrypted)
fn load_pdf(p: &str, bytes: &[u8]) -> Result<pdf_extract::Document, String> {
    guard(p, || {
        let mut doc = pdf_extract::Document::load_mem(bytes).map_err(|e| pdf_error(p, e))?;
        if doc.is_encrypted() && doc.decrypt("").is_err() { return Err(format!("{p} is a password-protected PDF; it cannot be read")); }
        if doc.get_pages().is_empty() { return Err(format!("{p}: the PDF has no pages")); }
        Ok(doc)
    })
}

/// one page's text, tidied; None when the parser fails on it
fn page_text(doc: &pdf_extract::Document, n: u32) -> Option<String> {
    let mut s = String::new();
    match catch_unwind(AssertUnwindSafe(|| pdf_extract::output_doc_page(doc, &mut pdf_extract::PlainTextOutput::new(&mut s), n))) {
        Ok(Ok(())) => Some(tidy(&s)),
        _ => None,
    }
}

/// Every page's text (None: that page could not be read), cached by the file's content (`cache::docs_json`, kind
/// `docs`): a second read of the same PDF, by `docs_pdf` or an attachment, does not parse it again.
fn pdf_texts(p: &str, path: &Path) -> Result<Vec<Option<String>>, String> {
    crate::cache::docs_json(path, "pdf_pages", || {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {p}: {e}"))?;
        if !is_pdf(&bytes) { return Err(format!("{p} is not a PDF")); }
        let doc = load_pdf(p, &bytes)?;
        let total = doc.get_pages().len() as u32;
        Ok((1..=total).map(|n| page_text(&doc, n)).collect())
    })
}

fn read_pdf(p: &str, pages: Option<&str>, cfg: &DocsCfg) -> Result<String, String> {
    let path = project_file(p, cfg)?;
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(&path).and_then(|f| f.take(1024).read_to_end(&mut head)).map_err(|e| format!("cannot read {p}: {e}"))?;
    if !is_pdf(&head) {
        return Err(format!("{p} is not a PDF{}", if SHEET_EXTS.contains(&ext(&path).as_str()) { " (use docs_sheet)" } else { "" }));
    }
    // all pages (or a PDF already in the cache): the cached page texts; a few pages of an uncached PDF: only those
    // pages are parsed (a big PDF's first `pages="3"` stays quick)
    enum Src { All(Vec<Option<String>>), Doc(Box<pdf_extract::Document>) }
    let src = if pages.is_none() || crate::cache::docs_has(&path, "pdf_pages") { Src::All(pdf_texts(p, &path)?) } else {
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
        Src::Doc(Box::new(load_pdf(p, &bytes)?))
    };
    let total = match &src { Src::All(v) => v.len() as u32, Src::Doc(d) => d.get_pages().len() as u32 };
    if total == 0 { return Err(format!("{p}: the PDF has no pages")); }
    let want = match pages { Some(s) => parse_pages(s, total)?, None => (1..=total).collect() };
    let room = room(cfg);
    let mut body = String::new();
    let (mut shown, mut texts) = (Vec::new(), 0);
    let mut cut_page = None;
    for &n in &want {
        let text = match &src { Src::All(v) => v.get(n as usize - 1).cloned().flatten(), Src::Doc(d) => page_text(d, n) };
        let text = match text {
            Some(t) if t.is_empty() => "[no text on this page]".to_string(),
            Some(t) => { texts += 1; t }
            None => "[the text of this page could not be read]".to_string(),
        };
        let block = format!("--- page {n} ---\n{text}\n");
        let used = body.chars().count();
        if used + block.chars().count() > room {
            if shown.is_empty() {
                // one page alone is over the cap: its start
                body.push_str(take(&block, room));
                body.push_str("\n[… page cut]\n");
                shown.push(n);
                cut_page = Some(n);
            }
            break;
        }
        body.push_str(&block);
        shown.push(n);
    }
    if texts == 0 && cut_page.is_none() {
        return Err(format!("{p}: no text on page{} {} (a scanned PDF of images? Elisa has no OCR)", if shown.len() == 1 { "" } else { "s" }, page_list(&shown)));
    }
    let rest: Vec<u32> = want.iter().copied().filter(|n| !shown.contains(n)).collect();
    let mut out = format!("[{p}: {total} page{} · showing {}]\n{}", if total == 1 { "" } else { "s" }, page_list(&shown), body.trim_end());
    if cut_page.is_some() || !rest.is_empty() {
        out.push_str("\n[output cut at the size limit");
        if !rest.is_empty() { out.push_str(&format!("; next: pages=\"{}\"", page_list(&rest))); }
        out.push(']');
    }
    Ok(out)
}

/// The text of every page of a PDF, one string per page ("" for a page without text): for a document attached to a
/// message (`attach.rs`), which the caller has already copied into the project. The same parser and errors as
/// `docs_pdf`.
pub fn pdf_pages(path: &Path) -> Result<Vec<String>, String> {
    let p = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(pdf_texts(&p, path)?.into_iter().map(Option::unwrap_or_default).collect())
}

/// one sheet's name and its rows of cell text
pub type SheetRows = (String, Vec<Vec<String>>);

/// Every sheet of a spreadsheet (or the one table of a CSV / TSV) as (sheet name, rows of cell text); the name is ""
/// for CSV / TSV. For an attached document (`attach.rs`), with `docs_sheet`'s parser.
pub fn sheet_rows(path: &Path) -> Result<Vec<SheetRows>, String> {
    let p = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let e = ext(path);
    if e == "csv" || e == "tsv" || !SHEET_EXTS.contains(&e.as_str()) {
        let (name, _, table, _) = load_sheet(&p, path, None)?;
        return Ok(vec![(name, table.rows)]);
    }
    // one parse of the whole workbook (cached), every sheet from it
    let book = workbook(&p, path)?;
    if book.is_empty() { return Err(format!("{p} has no sheets")); }
    book.into_iter().map(|(n, r)| r.map(|(_, rows)| (n.clone(), rows)).map_err(|e| format!("cannot read sheet {n:?} of {p}: {e}"))).collect()
}

/// the spreadsheet extensions `docs_sheet` reads
pub fn is_sheet(path: &Path) -> bool { SHEET_EXTS.contains(&ext(path).as_str()) }

// ---------- spreadsheets ----------

/// a cell area, 0-based and inclusive; None = to the end of the data
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Area { r0: u32, r1: Option<u32>, c0: u32, c1: Option<u32>, cols: bool }

/// "A" → 0, "AB" → 27
fn col_index(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 3 { return None; }
    s.bytes().try_fold(0u32, |n, b| b.is_ascii_alphabetic().then(|| n * 26 + (b.to_ascii_uppercase() - b'A') as u32 + 1)).map(|n| n - 1)
}

fn col_name(mut c: u32) -> String {
    let mut s = Vec::new();
    loop { s.push(b'A' + (c % 26) as u8); if c < 26 { break; } c = c / 26 - 1; }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// "B7" → (row 6, col 1)
fn cell_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let i = s.find(|c: char| c.is_ascii_digit())?;
    let row: u32 = s[i..].parse().ok()?;
    if row == 0 { return None; }
    Some((row - 1, col_index(&s[..i])?))
}

/// "A1:D50", "B3", rows "10-60" / "10-" / "10"
fn parse_range(spec: &str) -> Result<Area, String> {
    let bad = || format!("range {spec:?}: use cells like \"A1:D50\" or rows like \"10-60\"");
    let spec = spec.trim();
    if spec.starts_with(|c: char| c.is_ascii_digit()) {
        let num = |s: &str| s.trim().parse::<u32>().ok().filter(|&n| n > 0).ok_or_else(bad);
        let (a, b) = match spec.split_once('-') {
            Some((a, b)) => (num(a)?, if b.trim().is_empty() { None } else { Some(num(b)?) }),
            None => { let n = num(spec)?; (n, Some(n)) }
        };
        if b.is_some_and(|b| b < a) { return Err(bad()); }
        return Ok(Area { r0: a - 1, r1: b.map(|b| b - 1), ..Area::default() });
    }
    let (a, b) = spec.split_once(':').unwrap_or((spec, spec));
    let ((r0, c0), (r1, c1)) = (cell_ref(a).ok_or_else(bad)?, cell_ref(b).ok_or_else(bad)?);
    Ok(Area { r0: r0.min(r1), r1: Some(r0.max(r1)), c0: c0.min(c1), c1: Some(c0.max(c1)), cols: true })
}

/// a whole table in memory: rows of cell text, plus the absolute position of its first cell
struct Table { start: (u32, u32), rows: Vec<Vec<String>> }

impl Table {
    fn end(&self) -> (u32, u32) {
        let w = self.rows.iter().map(Vec::len).max().unwrap_or(0) as u32;
        (self.start.0 + (self.rows.len() as u32).saturating_sub(1), self.start.1 + w.saturating_sub(1))
    }
    fn cell(&self, r: u32, c: u32) -> &str {
        if r < self.start.0 || c < self.start.1 { return ""; }
        self.rows.get((r - self.start.0) as usize).and_then(|row| row.get((c - self.start.1) as usize)).map_or("", String::as_str)
    }
}

/// a spreadsheet serial date (1900 system) as ISO text
fn serial_date(v: f64, duration: bool) -> String {
    if duration || !(0.0..2_958_466.0).contains(&v) { return format!("{v}"); }
    let secs = (v.fract() * 86400.0).round() as i64;
    // 1899-12-30 is serial 0 (it absorbs Lotus' 1900 leap-year quirk for dates after February 1900)
    let days = v.trunc() as i64 + secs / 86400 - 25569;
    let (y, m, d) = civil(days);
    let t = secs % 86400;
    if t == 0 { format!("{y:04}-{m:02}-{d:02}") } else { format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) }
}

/// days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days)
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn cell_text(d: &calamine::Data) -> String {
    match d {
        calamine::Data::DateTime(t) => serial_date(t.as_f64(), t.is_duration()),
        d => d.to_string(),
    }
}

/// a CSV / TSV file (quoted fields, `""` escapes, newlines inside quotes)
fn parse_delimited(text: &str, sep: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut chars) = (false, text.trim_start_matches('\u{feff}').chars().peekable());
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' { if chars.peek() == Some(&'"') { chars.next(); field.push('"'); } else { quoted = false; } } else { field.push(c); }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == sep {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') { chars.next(); }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() { row.push(field); rows.push(row); }
    rows
}

/// the table of one sheet, the names of all sheets, and a note when the asked-for sheet was not the one read.
/// Small models invent `sheet: "Sheet1"` (on every call, then loop on the error): a CSV / TSV ignores
/// `sheet`, and a workbook with one sheet reads that sheet whatever the name; with several, a wrong name is an error
/// that lists them.
fn load_sheet(p: &str, path: &Path, sheet: Option<&str>) -> Result<(String, Vec<String>, Table, Option<String>), String> {
    let e = ext(path);
    if e == "csv" || e == "tsv" {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {p}: {e}"))?;
        if bytes[..bytes.len().min(8192)].contains(&0) { return Err(format!("{p} is not a text file")); }
        let rows = parse_delimited(&String::from_utf8_lossy(&bytes), if e == "tsv" { '\t' } else { ',' });
        return Ok((String::new(), Vec::new(), Table { start: (0, 0), rows }, None));
    }
    if !SHEET_EXTS.contains(&e.as_str()) {
        let hint = if e == "pdf" { " (use docs_pdf)" } else { "" };
        return Err(format!("{p}: unknown spreadsheet format{}{hint}; docs_sheet reads xlsx, xlsm, xlsb, xls, ods, csv and tsv",
            if e.is_empty() { String::new() } else { format!(" .{e}") }));
    }
    let book = workbook(p, path)?;
    let names: Vec<String> = book.iter().map(|(n, _)| n.clone()).collect();
    let mut note = None;
    let name = match sheet {
        None => names.first().cloned().ok_or_else(|| format!("{p} has no sheets"))?,
        Some(s) => match names.iter().find(|n| n.as_str() == s).or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case(s))) {
            Some(n) => n.clone(),
            None if names.len() == 1 => {
                note = Some(format!("[no sheet {s:?}; read the only sheet, {:?}]", names[0]));
                names[0].clone()
            }
            None => return Err(format!("{p} has no sheet {s:?} (sheets: {})", names.join(", "))),
        },
    };
    let (start, rows) = match book.into_iter().find(|(n, _)| *n == name).map(|(_, r)| r) {
        Some(Ok(t)) => t,
        Some(Err(e)) => return Err(format!("cannot read sheet {name:?} of {p}: {e}")),
        None => return Err(format!("{p} has no sheet {name:?}")),
    };
    Ok((name, names, Table { start, rows }, note))
}

/// a workbook's sheets in order: (name, (first cell, rows of cell text) or why the sheet could not be read)
type Book = Vec<(String, Result<((u32, u32), Vec<Vec<String>>), String>)>;

/// Every sheet of a workbook, parsed once and cached by the file's content (`cache::docs_json`, kind `docs`).
fn workbook(p: &str, path: &Path) -> Result<Book, String> {
    crate::cache::docs_json(path, "workbook", || guard(p, || {
        use calamine::Reader;
        let mut wb = calamine::open_workbook_auto(path).map_err(|e| {
            let m = e.to_string();
            if m.to_lowercase().contains("password") || m.to_lowercase().contains("encrypt") { format!("{p} is password-protected; it cannot be read") } else { format!("cannot read {p} as a spreadsheet: {m}") }
        })?;
        let names = wb.sheet_names();
        Ok(names.into_iter().map(|name| {
            let r = wb.worksheet_range(&name).map(|range| (range.start().unwrap_or((0, 0)), range.rows().map(|r| r.iter().map(cell_text).collect()).collect()))
                .map_err(|e| e.to_string());
            (name, r)
        }).collect())
    }))
}

fn read_sheet(p: &str, sheet: Option<&str>, range: Option<&str>, cfg: &DocsCfg) -> Result<String, String> {
    let path = project_file(p, cfg)?;
    let (name, names, table, note) = load_sheet(p, &path, sheet)?;
    let note = note.map(|n| format!("{n}\n")).unwrap_or_default();
    // no range: from the first cell with data
    let area = match range { Some(r) => parse_range(r)?, None => Area { r0: table.start.0, c0: table.start.1, ..Area::default() } };
    let what = if name.is_empty() { p.to_string() } else { format!("{p} sheet {name:?}") };
    let others = if names.len() > 1 { format!(" (sheets: {})", names.join(", ")) } else { String::new() };
    if table.rows.iter().all(|r| r.iter().all(|c| c.trim().is_empty())) { return Ok(format!("{note}[{what} is empty{others}]")); }
    let (last_r, last_c) = table.end();
    let r1 = area.r1.unwrap_or(last_r).min(last_r);
    let c1 = area.c1.unwrap_or(last_c).min(last_c);
    let c0 = area.c0;
    if area.r0 > last_r { return Err(format!("row {} is past the end of {what} (rows 1-{})", area.r0 + 1, last_r + 1)); }
    if c0 > last_c { return Err(format!("column {} is past the end of {what} (columns A-{})", col_name(c0), col_name(last_c))); }
    let room = room(cfg);
    let (mut body, mut used, mut next) = (String::new(), 0usize, None);
    for r in area.r0..=r1 {
        let mut cells: Vec<String> = (c0..=c1).map(|c| {
            let t = table.cell(r, c).replace(['\t', '\n', '\r'], " ");
            if t.chars().count() > CELL_CHARS { format!("{}…", take(&t, CELL_CHARS)) } else { t }
        }).collect();
        while cells.last().is_some_and(|c| c.is_empty()) { cells.pop(); }
        let line = format!("{}\t{}\n", r + 1, cells.join("\t"));
        let n = line.chars().count();
        if used + n > room && r > area.r0 { next = Some(r); break; }
        body.push_str(if n > room { take(&line, room) } else { &line });
        used += n;
    }
    let shown_end = next.map_or(r1, |n| n - 1);
    let mut out = format!("{note}[{what}{others} · rows {}-{} of {} · columns {}-{} · tab-separated, first field = row number]\n{}",
        area.r0 + 1, shown_end + 1, last_r + 1, col_name(c0), col_name(c1), body.trim_end_matches('\n'));
    if let Some(n) = next {
        let hint = if area.cols { format!("{}{}:{}{}", col_name(c0), n + 1, col_name(c1), r1 + 1) } else { format!("{}-{}", n + 1, r1 + 1) };
        out.push_str(&format!("\n[output cut at the size limit; next: range=\"{hint}\"]"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-docs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(root: &Path, max_chars: usize) -> DocsCfg { DocsCfg { root: outside::root(root), max_chars } }

    /// a minimal valid PDF with one text line per page (Helvetica, WinAnsi), xref offsets computed
    pub(crate) fn pdf(pages: &[&str]) -> Vec<u8> { pdf_enc(pages, false) }

    /// `encrypt`: with a standard security handler whose user password is not empty (made-up /O and /U)
    fn pdf_enc(pages: &[&str], encrypt: bool) -> Vec<u8> {
        let n = pages.len();
        let font = 3 + 2 * n;
        let mut objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect::<Vec<_>>().join(" ")),
        ];
        for (i, t) in pages.iter().enumerate() {
            let content = if t.is_empty() { String::new() } else { format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET") };
            objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + 2 * i));
            objs.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
        objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
        let trailer_extra = if encrypt {
            objs.push(format!("<< /Filter /Standard /V 1 /R 2 /P -4 /O <{}> /U <{}> >>", "ab".repeat(32), "cd".repeat(32)));
            format!(" /Encrypt {} 0 R /ID [<{}> <{}>]", objs.len(), "01".repeat(16), "01".repeat(16))
        } else { String::new() };
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offs { out.extend(format!("{o:010} 00000 n \n").bytes()); }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        out
    }

    #[test]
    fn speaks_mcp_and_lists_both_tools() {
        let d = tmp("mcp");
        let c = cfg(&d, DOC_CHARS);
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), &c).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-docs");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &c).is_none());
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &c).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["docs_pdf", "docs_sheet"]);
        // small enough for an 8K context: the whole tools list well under ~400 tokens (~4 chars a token)
        assert!(tools().to_string().len() < 800, "{}", tools().to_string().len());
        let r = answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "docs_pdf", "arguments": {"path": "nope.pdf"}}}), &c).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("no such file"));
        assert_eq!(answer(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), &c).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn reads_pdf_pages_and_cuts_with_a_next_hint() {
        let d = tmp("pdf");
        std::fs::write(d.join("a.pdf"), pdf(&["Hello first page", "Second page here", "Third one"])).unwrap();
        let c = cfg(&d, DOC_CHARS);
        let all = read_pdf("a.pdf", None, &c).unwrap();
        assert!(all.starts_with("[a.pdf: 3 pages · showing 1-3]"), "{all}");
        assert!(all.contains("--- page 2 ---\nSecond page here"), "{all}");
        let two = read_pdf("a.pdf", Some("2-3"), &c).unwrap();
        assert!(!two.contains("Hello") && two.contains("Third one"), "{two}");
        assert!(read_pdf("a.pdf", Some("9"), &c).unwrap_err().contains("past the end"));
        assert!(read_pdf("a.pdf", Some("x"), &c).unwrap_err().contains("pages"));
        // a small cap: whole pages, then where to go on
        let pages: Vec<String> = (1..=12).map(|i| format!("page {i} {}", "word ".repeat(60))).collect();
        std::fs::write(d.join("big.pdf"), pdf(&pages.iter().map(String::as_str).collect::<Vec<_>>())).unwrap();
        let small = cfg(&d, 1000);
        let cut = read_pdf("big.pdf", None, &small).unwrap();
        assert!(cut.chars().count() <= 1000, "{}", cut.chars().count());
        assert!(cut.contains("next: pages=\""), "{cut}");
        // a page with no text, alone: scanned
        std::fs::write(d.join("scan.pdf"), pdf(&["", ""])).unwrap();
        assert!(read_pdf("scan.pdf", None, &c).unwrap_err().contains("no text"));
        // not a PDF / damaged
        std::fs::write(d.join("fake.pdf"), "just text").unwrap();
        assert!(read_pdf("fake.pdf", None, &c).unwrap_err().contains("not a PDF"));
        std::fs::write(d.join("broken.pdf"), b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 9 0 R >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF").unwrap();
        assert!(read_pdf("broken.pdf", None, &c).is_err());
        std::fs::write(d.join("locked.pdf"), pdf_enc(&["secret"], true)).unwrap();
        assert!(read_pdf("locked.pdf", None, &c).unwrap_err().contains("password-protected"), "{:?}", read_pdf("locked.pdf", None, &c));
    }

    #[test]
    fn refuses_paths_outside_the_project() {
        let base = tmp("outside");
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(base.join("secret.csv"), "a,b\n").unwrap();
        let c = cfg(&proj, DOC_CHARS);
        assert!(read_sheet("../secret.csv", None, None, &c).unwrap_err().contains("outside the project"));
        let abs = base.join("secret.csv").display().to_string();
        assert!(read_sheet(&abs, None, None, &c).unwrap_err().contains("outside the project"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("secret.csv"), proj.join("link.csv")).unwrap();
            assert!(read_sheet("link.csv", None, None, &c).unwrap_err().contains("outside the project"));
            std::os::unix::fs::symlink(&base, proj.join("up")).unwrap();
            assert!(read_pdf("up/secret.csv", None, &c).unwrap_err().contains("outside the project"));
        }
        std::fs::write(proj.join("in.csv"), "x,y\n1,2\n").unwrap();
        assert!(read_sheet("./sub/../in.csv", None, None, &c).is_ok());
    }

    #[test]
    fn reads_csv_with_ranges_and_cuts() {
        let d = tmp("csv");
        std::fs::write(d.join("t.csv"), "name,note,n\nann,\"hi, there\",1\nbob,\"two\nlines\",2\n\"q\"\"x\",,3\n").unwrap();
        let c = cfg(&d, DOC_CHARS);
        let all = read_sheet("t.csv", None, None, &c).unwrap();
        assert!(all.contains("rows 1-4 of 4 · columns A-C"), "{all}");
        assert!(all.contains("2\tann\thi, there\t1"), "{all}");
        assert!(all.contains("3\tbob\ttwo lines\t2"), "{all}");
        assert!(all.contains("4\tq\"x\t\t3"), "{all}");
        let part = read_sheet("t.csv", None, Some("B2:C3"), &c).unwrap();
        assert!(part.contains("rows 2-3 of 4 · columns B-C") && part.contains("2\thi, there\t1") && !part.contains("ann"), "{part}");
        assert!(read_sheet("t.csv", None, Some("3-"), &c).unwrap().contains("rows 3-4"));
        assert!(read_sheet("t.csv", None, Some("9-10"), &c).unwrap_err().contains("past the end"));
        assert!(read_sheet("t.csv", None, Some("zz"), &c).unwrap_err().contains("range"));
        // a made-up sheet name on a CSV is ignored (small models always send "Sheet1")
        assert_eq!(read_sheet("t.csv", Some("Sheet1"), None, &c).unwrap(), all);
        let big: String = (1..=500).map(|i| format!("{i},value number {i},{}\n", i * 2)).collect();
        std::fs::write(d.join("big.csv"), big).unwrap();
        let cut = read_sheet("big.csv", None, None, &cfg(&d, 1000)).unwrap();
        assert!(cut.chars().count() <= 1000 && cut.contains("next: range=\""), "{cut}");
        let cols = read_sheet("big.csv", None, Some("A1:B500"), &cfg(&d, 1000)).unwrap();
        assert!(cols.contains("next: range=\"A"), "{cols}");
        // unknown formats and missing files
        std::fs::write(d.join("x.docx"), "PK").unwrap();
        assert!(read_sheet("x.docx", None, None, &c).unwrap_err().contains("unknown spreadsheet format .docx"));
        assert!(read_sheet("gone.xlsx", None, None, &c).unwrap_err().contains("no such file"));
        std::fs::write(d.join("bad.xlsx"), "not a zip").unwrap();
        assert!(read_sheet("bad.xlsx", None, None, &c).unwrap_err().contains("cannot read bad.xlsx"));
    }

    #[test]
    fn ranges_columns_and_dates() {
        assert_eq!(parse_range("A1:D50").unwrap(), Area { r0: 0, r1: Some(49), c0: 0, c1: Some(3), cols: true });
        assert_eq!(parse_range("$b$7").unwrap(), Area { r0: 6, r1: Some(6), c0: 1, c1: Some(1), cols: true });
        assert_eq!(parse_range("10-60").unwrap(), Area { r0: 9, r1: Some(59), ..Area::default() });
        assert!(parse_range("0-3").is_err() && parse_range("5-2").is_err() && parse_range("A0").is_err());
        assert_eq!((col_index("A"), col_index("Z"), col_index("AA"), col_index("ab")), (Some(0), Some(25), Some(26), Some(27)));
        assert_eq!((col_name(0), col_name(25), col_name(26), col_name(701), col_name(702)), ("A".into(), "Z".into(), "AA".into(), "ZZ".into(), "AAA".into()));
        assert_eq!(serial_date(45000.0, false), "2023-03-15");
        assert_eq!(serial_date(45000.5, false), "2023-03-15 12:00:00");
        assert_eq!(serial_date(1.0, false), "1899-12-31");
        assert_eq!(parse_pages("1,3-4,3", 5).unwrap(), [1, 3, 4]);
        assert_eq!(parse_pages("4-", 5).unwrap(), [4, 5]);
        assert_eq!(page_list(&[1, 2, 3, 5, 7, 8]), "1-3,5,7-8");
    }
}
