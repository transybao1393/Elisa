//! The tools: read, write, edit, bash, glob, grep (contract in DESIGN.md), and the syntax-tree tools outline,
//! read_symbol, edit_symbol (src/ast.rs). write / edit / edit_symbol pass the parse guard first (`ast::guard`).
use crate::ast::{self, Lang};
use crate::types::ToolDef;
use regex::Regex;
use serde_json::{json, Value};
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access { ReadOnly, Write, Exec }

#[derive(Clone, Debug)]
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub max_chars: usize,
    pub bash_timeout_s: u64,
    /// config `ast_guard`: refuse a write / edit that adds parse errors to a supported code file
    pub ast_guard: bool,
}

impl Default for ToolCtx {
    fn default() -> Self { ToolCtx { cwd: PathBuf::from("."), max_chars: 6000, bash_timeout_s: 120, ast_guard: true } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput { pub content: String, pub is_error: bool }

const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".elisa"];
const READ_LIMIT: usize = 400;
/// this many padding lines in a row collapse to one marker line in `read` (a blank line or two is ordinary code)
const PADDING_RUN_MIN: usize = 3;
const GLOB_MAX: usize = 200;
/// a glob walk stops after this long, this many files looked at, or this many matches
const GLOB_SECS: u64 = 5;
const GLOB_SCAN_MAX: usize = 100_000;
const GLOB_HITS_MAX: usize = 10_000;
const GREP_MAX: usize = 100;
const BASH_BUF_MAX: usize = 16 << 20;
/// code files in a directory outline
const OUTLINE_FILES_MAX: usize = 300;

fn def(name: &str, description: &str, props: Value, required: &[&str]) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type": "object", "properties": props, "required": required}),
    }
}

fn s() -> Value { json!({"type": "string"}) }
fn sd(d: &str) -> Value { json!({"type": "string", "description": d}) }

/// All tools (config `ast` on). Byte-stable: fixed order, fixed key order.
pub fn defs() -> Vec<ToolDef> { defs_for(true) }

/// read, write, edit, bash, glob, grep, and with `ast` outline, read_symbol, edit_symbol. Byte-stable: fixed order,
/// fixed key order, nothing per turn.
pub fn defs_for(ast: bool) -> Vec<ToolDef> {
    let mut v = vec![
        def("read", "Read a file, line-numbered.",
            json!({"path": s(), "offset": {"type": "integer", "description": "1-based line"},
                   "limit": {"type": "integer", "description": "default 400"}}), &["path"]),
        def("write", "Create or overwrite a whole file.", json!({"path": s(), "content": s()}), &["path", "content"]),
        def("edit", "Replace exact old text by new; unique unless all=true.",
            json!({"path": s(), "old": s(), "new": s(), "all": {"type": "boolean"}}), &["path", "old", "new"]),
        def("bash", "Run a zsh command in cwd. Never use it to write files.",
            json!({"command": s()}), &["command"]),
        def("glob", "Find files by glob, e.g. src/**/*.py or **/*_test.go.",
            json!({"pattern": s(), "path": sd("default cwd")}), &["pattern"]),
        def("grep", "Search file contents by regex; returns file:line: text.",
            json!({"pattern": s(), "path": s(), "glob": sd("e.g. *.ts")}), &["pattern"]),
    ];
    if ast {
        let name = s;
        let line = || json!({"type": "integer"});
        v.extend([
            def("outline", "Signatures and line ranges of a code file (dir: names per file).",
                json!({"path": s()}), &["path"]),
            def("read_symbol", "A symbol's source by name, e.g. Type::method; line picks among same names.",
                json!({"path": s(), "name": name(), "line": line()}), &["path", "name"]),
            def("edit_symbol", "Replace (or insert before/after, delete) a symbol by name with source.",
                json!({"path": s(), "name": name(), "source": s(),
                       "action": {"enum": ["replace", "before", "after", "delete"]}}), &["path", "name"]),
        ]);
    }
    v
}


/// The name an approval rule is kept under: outline / read_symbol are read-only like `read`, edit_symbol asks like
/// `edit` (an "always" for one covers the other; `auto_approve = ["read"]` covers outline and read_symbol);
/// an MCP tool goes under `mcp:SERVER` instead (`mcp_key`; `repl.rs` approve_inside).
pub fn approval_name(name: &str) -> &str {
    match name { "outline" | "read_symbol" => "read", "edit_symbol" => "edit", n => n }
}

/// the prefix of an MCP server's approval key in `.elisa/allow` and `auto_approve`
pub const MCP_KEY: &str = "mcp:";

/// An MCP server's approval key (`mcp:web`): its own namespace, so it never equals one of Elisa's tool keys.
pub fn mcp_key(server: &str) -> String { format!("{MCP_KEY}{server}") }

/// Names an MCP server may not take: Elisa's own tools and approval names (a server called `edit` must not look
/// like Elisa's `edit` anywhere: approvals, notes, tool lists).
pub fn reserved_server_name(name: &str) -> bool {
    defs_for(true).iter().any(|d| d.name == name || approval_name(&d.name) == name)
}

pub fn access(name: &str) -> Access {
    match name {
        "read" | "glob" | "grep" | "outline" | "read_symbol" => Access::ReadOnly,
        "write" | "edit" | "edit_symbol" => Access::Write,
        _ => Access::Exec,
    }
}

fn sv<'a>(input: &'a Value, k: &str) -> &'a str { input.get(k).and_then(Value::as_str).unwrap_or("") }

/// One line for the permission prompt / transcript, e.g. "bash cargo build".
pub fn summary(name: &str, input: &Value) -> String {
    match name {
        "read" | "edit" | "outline" => format!("{name} {}", sv(input, "path")),
        "read_symbol" => format!("{name} {} {}", sv(input, "path"), sv(input, "name")),
        "edit_symbol" => {
            let act = sv(input, "action");
            let act = if act.is_empty() || act == "replace" { String::new() } else { format!(" ({act})") };
            format!("{name} {} {}{act}", sv(input, "path"), sv(input, "name"))
        }
        "write" => format!("{name} {} ({} lines)", sv(input, "path"), sv(input, "content").lines().count()),
        "bash" => {
            let one: String = sv(input, "command").split_whitespace().collect::<Vec<_>>().join(" ");
            let mut t: String = one.chars().take(100).collect();
            if one.chars().count() > 100 { t.push('…'); }
            format!("bash {t}")
        }
        "glob" | "grep" => format!("{name} {}", sv(input, "pattern")),
        "web_search" => format!("{name} \u{201c}{}\u{201d}", sv(input, "query")),
        "web_fetch" => format!("{name} {}", sv(input, "url")),
        // the sandbox: the language and the code's first non-empty line (and how many more there are)
        "run_code" => {
            let code = sv(input, "code");
            let first = code.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let mut t: String = first.chars().take(80).collect();
            if first.chars().count() > 80 { t.push('…'); }
            let more = code.lines().filter(|l| !l.trim().is_empty()).count().saturating_sub(1);
            let more = if more > 0 { format!(" (+{more} lines)") } else { String::new() };
            format!("{name} {}: {t}{more}{}", sv(input, "language"), crate::mcp_run::deps_summary(input))
        }
        // an MCP tool: its name and its first text argument
        _ => match input.as_object().and_then(|o| o.values().find_map(|v| v.as_str())) {
            Some(v) => { let v: String = v.chars().take(100).collect(); format!("{name} {v}") }
            None => name.to_string(),
        },
    }
}

pub fn run(name: &str, input: &Value, ctx: &ToolCtx) -> ToolOutput {
    let r = match name {
        "read" => read(input, ctx),
        "write" => write(input, ctx),
        "edit" => edit(input, ctx),
        "bash" => bash(input, ctx),
        "glob" => glob(input, ctx),
        "grep" => grep(input, ctx),
        "outline" => outline(input, ctx),
        "read_symbol" => read_symbol(input, ctx),
        "edit_symbol" => edit_symbol(input, ctx),
        _ => Err(format!("unknown tool: {name}")),
    };
    match r {
        Ok(c) => ToolOutput { content: cap(&c, ctx.max_chars), is_error: false },
        Err(e) => ToolOutput { content: cap(&e, ctx.max_chars), is_error: true },
    }
}

pub(crate) fn cap(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if max == 0 || n <= max { return s.to_string(); }
    let head = max * 6 / 10;
    let tail = max - head;
    let hb = s.char_indices().nth(head).map_or(s.len(), |(i, _)| i);
    let tb = s.char_indices().nth(n - tail).map_or(s.len(), |(i, _)| i);
    let mid = &s[hb..tb];
    format!("{}\n[... {} chars / {} lines cut ...]\n{}", &s[..hb], n - head - tail, mid.matches('\n').count(), &s[tb..])
}

fn arg<'a>(input: &'a Value, k: &str) -> Result<&'a str, String> {
    input.get(k).and_then(Value::as_str).ok_or_else(|| format!("missing string field '{k}'"))
}

fn opt_str<'a>(input: &'a Value, k: &str) -> Option<&'a str> {
    input.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn num(input: &Value, k: &str) -> Option<usize> {
    match input.get(k)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)).map(|v| v as usize),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn flag(input: &Value, k: &str) -> bool {
    match input.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn resolve(ctx: &ToolCtx, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() { p.to_path_buf() } else { ctx.cwd.join(p) }
}

fn rel(ctx: &ToolCtx, p: &Path) -> String {
    p.strip_prefix(&ctx.cwd).unwrap_or(p).to_string_lossy().into_owned()
}

fn is_binary(b: &[u8]) -> bool { b[..b.len().min(8192)].contains(&0) }

/// A line with nothing in it: blank, spaces, or a quoted string of spaces (`"    ",`, the padding of a cut-off write).
fn is_padding(l: &str) -> bool { l.chars().all(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',')) }

fn read(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = file_path_arg(input)?;
    let path = resolve(ctx, p);
    if path.is_dir() { return Err(dir_error(p)); }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    if is_binary(&bytes) { return Err(binary_read_error(p)); }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 { return Ok(format!("[{p} is empty]")); }
    let start = num(input, "offset").unwrap_or(1).max(1);
    let limit = num(input, "limit").filter(|&l| l > 0).unwrap_or(READ_LIMIT);
    if start > total { return Err(format!("offset {start} is past the end ({total} lines)")); }
    let mut out = String::new();
    // stop at a whole line before the output cap (a cut middle made the model miscount lines)
    let room = if ctx.max_chars == 0 { usize::MAX } else { ctx.max_chars.saturating_sub(80) };
    let (mut next, mut shown) = (start - 1, 0);
    while next < total && shown < limit {
        // a run of padding costs one marker line and none of the limit, so one read moves past it
        let run = lines[next..].iter().take_while(|l| is_padding(l)).count();
        let padded = run >= PADDING_RUN_MIN;
        let line = if padded { format!("{}-{}\t[{run} lines of only spaces or quotes skipped]\n", next + 1, next + run) } else { format!("{}\t{}\n", next + 1, lines[next]) };
        if !out.is_empty() && out.chars().count() + line.chars().count() > room { break; }
        out.push_str(&line);
        if padded { next += run; } else { next += 1; shown += 1; }
    }
    let end = next;
    if start > 1 || end < total {
        out.push_str(&format!("[lines {start}-{end} of {total}]"));
        if end < total { out.push_str(&format!(" next: offset={}", end + 1)); }
    }
    // a PDF that happens to be text: its raw objects, not its words (the session fences it as a document either way)
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        out.push_str(&format!("\n[{p} is a PDF read as raw text; docs_pdf extracts its words]"));
    }
    Ok(out)
}

/// `read` on a directory: what to use instead
fn dir_error(p: &str) -> String {
    let base = p.trim_end_matches('/');
    format!("{p} is a directory, not a file: use glob to list it (pattern `{}/*`, or `**/*` with path `{p}`) or outline for its code", if base.is_empty() { "." } else { base })
}

/// `read` on a binary file: the same whatever offset / limit, so the loop guard treats every later read of the path
/// as a repeat (`loop_guard.rs`)
pub const BINARY_READ: &str = "is a binary file: read shows text only, and no offset / limit changes that.";

fn binary_read_error(p: &str) -> String {
    format!("{p} {BINARY_READ} Use docs_sheet for a spreadsheet (xlsx, xls, ods), docs_pdf for a PDF, or bash `file {p}` to see what it is.")
}

fn write(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = file_path_arg(input)?;
    let content = arg(input, "content")?;
    let path = resolve(ctx, p);
    not_binary(&path, p, "write")?;
    if ctx.ast_guard {
        let old = std::fs::read_to_string(&path).ok();
        ast::guard(&path, p, old.as_deref(), content)?;
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let before = dups_of(ctx, &path, p);
    std::fs::write(&path, content).map_err(|e| format!("cannot write {p}: {e}"))?;
    Ok(format!("wrote {} lines to {p}{}", content.lines().count(), dup_note(ctx, &path, p, &before)))
}

/// An edit worked out without writing anything: (path, old text, new text, replacements). Every reason it cannot
/// change the file is an error here, so `precheck` refuses it before anyone is asked to approve it.
fn edit_plan(input: &Value, ctx: &ToolCtx) -> Result<(PathBuf, String, String, usize), String> {
    let p = file_path_arg(input)?;
    let old = arg(input, "old")?;
    let new = arg(input, "new")?;
    if old.is_empty() { return Err("old must not be empty".into()); }
    if old == new { return Err(format!("no change: old and new are the same text; {p} was not edited")); }
    let path = resolve(ctx, p);
    if path.is_dir() { return Err(format!("{p} is a directory: edit changes one file")); }
    let text = text_file(&path, p, "edit")?;
    let n = text.matches(old).count();
    let all = flag(input, "all");
    if n == 0 { return Err(format!("old not found in {p}; read the file and copy the exact text (whitespace included)")); }
    if n > 1 && !all { return Err(format!("old matches {n} times in {p}; add context or set all=true")); }
    let out = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    Ok((path, text, out, n))
}

fn edit(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let (path, text, out, n) = edit_plan(input, ctx)?;
    let p = arg(input, "path")?;
    if ctx.ast_guard { ast::guard(&path, p, Some(&text), &out)?; }
    let before = dups_of(ctx, &path, p);
    std::fs::write(&path, out).map_err(|e| format!("cannot write {p}: {e}"))?;
    Ok(format!("edited {p} ({n} replacement{}){}", if n == 1 { "" } else { "s" }, dup_note(ctx, &path, p, &before)))
}

/// `scheme://…` (https://, file://, ssh://): a URL, not a file path. `write https://x/y` would otherwise create
/// `https:/x/y` inside the project.
pub fn looks_like_url(p: &str) -> bool {
    let Some(i) = p.find("://") else { return false };
    let scheme = &p[..i];
    !scheme.is_empty() && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The `path` argument of a file tool; a URL is refused (with a pointer to web_fetch).
fn file_path_arg(input: &Value) -> Result<&str, String> {
    let p = arg(input, "path")?;
    if looks_like_url(p) {
        return Err(format!("{p} is a URL, not a file path: file tools take a path inside the project (use web_fetch to read a web page)"));
    }
    Ok(p)
}

/// A file write / edit may change: missing (write creates it) or UTF-8 text without NUL bytes.
fn not_binary(path: &Path, p: &str, tool: &str) -> Result<(), String> {
    if !path.is_file() { return Ok(()); }
    text_file(path, p, tool).map(|_| ())
}

/// The file's text; a binary file (NUL bytes) or one that is not UTF-8 is an error naming `tool`.
fn text_file(path: &Path, p: &str, tool: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {p}: {e}"))?;
    if is_binary(&bytes) { return Err(format!("{p} is a binary file: {tool} only changes text files")); }
    String::from_utf8(bytes).map_err(|_| format!("{p} is not UTF-8 text: {tool} only changes text files"))
}

/// Before approval: a call of one of Elisa's file tools that cannot do anything useful fails here, so nobody is asked
/// to approve it (a URL as a path; an edit whose old text is missing or the same as new; an edit / write on a binary
/// file; an edit_symbol that changes nothing or names no symbol). Ok for every other tool and call: the run itself
/// still checks everything. A path outside the project gets only the URL check: its contents are not looked at
/// before the outside-the-project approval (an "old not found" would otherwise tell what an unapproved file holds).
pub fn precheck(name: &str, input: &Value, ctx: &ToolCtx) -> Result<(), String> {
    if matches!(name, "read" | "outline" | "read_symbol" | "write" | "edit" | "edit_symbol") {
        file_path_arg(input)?;
        if crate::outside::check(name, input, &ctx.cwd).is_some() { return Ok(()); }
    }
    match name {
        "read" | "outline" | "read_symbol" => file_path_arg(input).map(|_| ()),
        "write" => {
            let p = file_path_arg(input)?;
            not_binary(&resolve(ctx, p), p, "write")
        }
        "edit" => edit_plan(input, ctx).map(|_| ()),
        "edit_symbol" => edit_symbol_plan(input, ctx).map(|_| ()),
        _ => Ok(()),
    }
}

/// With the parse guard on: what a supported code file defines twice now (`ast::duplicates`), named by `p`.
fn dups_of(ctx: &ToolCtx, path: &Path, p: &str) -> Vec<String> {
    if !ctx.ast_guard || Lang::of(path).is_none() || !path.is_file() { return vec![]; }
    ast::duplicates(path, p)
}

/// After a write / edit: a warning for each duplicate the edit added (a second route / function / method), so the
/// model removes it in the same turn. Empty when there is none.
fn dup_note(ctx: &ToolCtx, path: &Path, p: &str, before: &[String]) -> String {
    ast::duplicate_note(&ast::added_duplicates(before, &dups_of(ctx, path, p)))
}

// ---------- syntax-tree tools (src/ast.rs) ----------

/// a supported code file's language and text (errors name what is wrong)
fn code_file(ctx: &ToolCtx, p: &str) -> Result<(PathBuf, Lang, String), String> {
    let path = resolve(ctx, p);
    let Some(lang) = Lang::of(&path) else {
        return Err(format!("{p}: no syntax tree for this file type (Rust, Python, Swift, TS/TSX, JS, SQL, bash); use read / grep"));
    };
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    if meta.len() as usize > ast::MAX_BYTES { return Err(format!("{p} is too large to parse ({} bytes); use grep / read with offset", meta.len())); }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    Ok((path, lang, text))
}

/// The whole file as `read` numbers it (no line limit, no output cap): what a whole-file read costs. None = not a
/// readable text file. Used for the outline / read_symbol savings in /context.
pub fn whole_read_text(ctx: &ToolCtx, p: &str) -> Option<String> {
    let path = resolve(ctx, p);
    if !path.is_file() { return None; }
    let text = std::fs::read_to_string(&path).ok()?;
    Some(text.lines().enumerate().map(|(i, l)| format!("{}\t{l}\n", i + 1)).collect())
}

fn outline(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = arg(input, "path")?;
    let root = resolve(ctx, p);
    let room = if ctx.max_chars == 0 { usize::MAX } else { ctx.max_chars.saturating_sub(100) };
    if root.is_dir() {
        let files: Vec<PathBuf> = walk(&root).map(|e| e.into_path()).filter(|f| Lang::of(f).is_some()).collect();
        if files.is_empty() { return Ok(format!("no code files under {p}")); }
        let mut out = String::new();
        let mut shown = 0;
        for f in files.iter().take(OUTLINE_FILES_MAX) {
            let lang = Lang::of(f).expect("filtered");
            let line = match std::fs::read_to_string(f).ok().filter(|t| t.len() <= ast::MAX_BYTES).and_then(|t| ast::outline(lang, &t)) {
                Some(o) => ast::render_names(&rel(ctx, f), &o),
                None => format!("{} (not parsed: too large or not UTF-8)", rel(ctx, f)),
            };
            if shown > 0 && out.chars().count() + line.chars().count() + 1 > room { break; }
            out += &line;
            out.push('\n');
            shown += 1;
        }
        if shown < files.len() { out += &format!("[{shown} of {} code files; outline a subdirectory or a file]\n", files.len()); }
        out += "outline a file for signatures and line ranges";
        return Ok(out);
    }
    let (_, lang, text) = code_file(ctx, p)?;
    let o = ast::outline(lang, &text).ok_or_else(|| format!("{p}: could not parse"))?;
    let full = ast::render(p, lang, &o);
    if full.chars().count() <= room { return Ok(full); }
    // too long: top-level symbols only, with a member count
    let top = ast::Outline { syms: o.syms.iter().filter(|y| y.depth == 0).cloned().collect(), ..o.clone() };
    let mut s = ast::render(p, lang, &top);
    s += &format!("\n[{} members not shown: outline is over the output cap; read_symbol a type to see its members]", o.syms.len() - top.syms.len());
    Ok(s)
}

fn line_arg(input: &Value) -> Option<usize> { num(input, "line").filter(|&l| l > 0) }

fn read_symbol(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = arg(input, "path")?;
    let name = arg(input, "name")?;
    let (_, lang, text) = code_file(ctx, p)?;
    let o = ast::outline(lang, &text).ok_or_else(|| format!("{p}: could not parse"))?;
    let y = ast::find(&o, name, line_arg(input)).map_err(|e| format!("{p}: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!("{p}:{}-{} {} {}\n", y.line_a, y.line_b, y.kind, y.path);
    let room = if ctx.max_chars == 0 { usize::MAX } else { ctx.max_chars.saturating_sub(120) };
    let mut end = y.line_b;
    for i in y.line_a..=y.line_b.min(lines.len()) {
        let line = format!("{i}\t{}\n", lines[i - 1]);
        if i > y.line_a && out.chars().count() + line.chars().count() > room { end = i - 1; break; }
        out.push_str(&line);
    }
    if end < y.line_b { out += &format!("[lines {}-{end} of the symbol's {}-{}] next: read offset={}", y.line_a, y.line_a, y.line_b, end + 1); }
    Ok(out.trim_end().to_string())
}

/// An edit_symbol worked out without writing: (path, old text, new text, the symbol, the action). Errors for every
/// call that cannot change the file (used by `precheck` too).
fn edit_symbol_plan(input: &Value, ctx: &ToolCtx) -> Result<(PathBuf, String, String, ast::Sym, ast::Action), String> {
    let p = file_path_arg(input)?;
    let name = arg(input, "name")?;
    let action = ast::Action::parse(opt_str(input, "action").unwrap_or(""))?;
    let source = input.get("source").and_then(Value::as_str).unwrap_or("");
    if action != ast::Action::Delete && source.trim().is_empty() { return Err("source must not be empty (or use action=delete)".into()); }
    let (path, lang, text) = code_file(ctx, p)?;
    let o = ast::outline(lang, &text).ok_or_else(|| format!("{p}: could not parse"))?;
    let y = ast::find(&o, name, line_arg(input)).map_err(|e| format!("{p}: {e}"))?;
    let out = ast::apply(&text, y, action, source);
    if out == text { return Err(format!("no change: the new source of {} is the same", y.path)); }
    let y = y.clone();
    Ok((path, text, out, y, action))
}

fn edit_symbol(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let (path, text, out, y, action) = edit_symbol_plan(input, ctx)?;
    let p = arg(input, "path")?;
    if ctx.ast_guard { ast::guard(&path, p, Some(&text), &out)?; }
    let before = dups_of(ctx, &path, p);
    std::fs::write(&path, &out).map_err(|e| format!("cannot write {p}: {e}"))?;
    let note = dup_note(ctx, &path, p, &before);
    let (a, b) = (y.line_a, y.line_b);
    let delta = out.lines().count() as i64 - text.lines().count() as i64;
    let msg = match action {
        ast::Action::Replace => format!("replaced {} {} in {p} (lines {a}-{b} → {a}-{})", y.kind, y.path, (b as i64 + delta).max(a as i64)),
        ast::Action::Before => format!("inserted {} lines before {} {} in {p} (at line {a})", delta, y.kind, y.path),
        ast::Action::After => format!("inserted {} lines after {} {} in {p} (after line {b})", delta, y.kind, y.path),
        ast::Action::Delete => format!("deleted {} {} from {p} (lines {a}-{b})", y.kind, y.path),
    };
    Ok(msg + &note)
}

/// Preview of an edit_symbol call for the edit panel: (old text, new text). None = it would fail.
pub fn edit_symbol_preview(input: &Value, ctx: &ToolCtx) -> Option<(String, String)> {
    let (_, lang, text) = code_file(ctx, opt_str(input, "path")?).ok()?;
    let o = ast::outline(lang, &text)?;
    let y = ast::find(&o, opt_str(input, "name")?, line_arg(input)).ok()?;
    let action = ast::Action::parse(opt_str(input, "action").unwrap_or("")).ok()?;
    let new = ast::apply(&text, y, action, input.get("source").and_then(Value::as_str).unwrap_or(""));
    Some((text, new))
}

/// The line range read_symbol would show (for the read panel). None = it would fail.
pub fn symbol_lines(input: &Value, ctx: &ToolCtx) -> Option<(usize, usize)> {
    let (_, lang, text) = code_file(ctx, opt_str(input, "path")?).ok()?;
    let o = ast::outline(lang, &text)?;
    let y = ast::find(&o, opt_str(input, "name")?, line_arg(input)).ok()?;
    Some((y.line_a, y.line_b))
}

fn bash(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let cmd = arg(input, "command")?;
    if cmd.trim().is_empty() { return Err("empty command".into()); }
    let r = shell(cmd, &ctx.cwd, ctx.bash_timeout_s, BASH_BUF_MAX)?;
    let mut out = r.out;
    if r.timed_out {
        out.push_str(&format!("[timeout after {}s, killed]", ctx.bash_timeout_s.max(1)));
        return Err(out);
    }
    if r.cancelled { out.push_str("[cancelled by user, killed]"); return Err(out); }
    out.push_str(&match (r.code, r.signal) {
        (Some(code), _) => format!("[exit {code}]"),
        (None, Some(sig)) => format!("[killed by signal {sig}]"),
        _ => "[exit ?]".into(),
    });
    // `make test` that runs no pytest in a project with pytest files: the real tests too
    if let Some(py) = crate::checks::make_test_gap(&ctx.cwd, cmd) {
        if let Ok(t) = shell(&py, &ctx.cwd, ctx.bash_timeout_s, BASH_BUF_MAX) {
            out.push_str(&format!("\n[Elisa: `{}` runs no pytest; the project's tests: {py}]\n{}[exit {}]", cmd.trim(), t.out, t.code.map_or("?".into(), |c| c.to_string())));
        }
    }
    Ok(out)
}

/// One finished `zsh -c` run.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shell {
    /// stdout + stderr as they arrived (lossy UTF-8), ends with a newline when not empty; at most `buf_max` bytes kept
    pub out: String,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
}

/// Runs `cmd` with zsh in its own process group; the whole group is killed on timeout or Ctrl-C (`repl::CANCEL`).
/// Output beyond `buf_max` bytes is dropped (the run continues). Shared by the bash tool and the edit checks.
pub(crate) fn shell(cmd: &str, cwd: &Path, timeout_s: u64, buf_max: usize) -> Result<Shell, String> {
    // under the shell sandbox (src/sandbox.rs, config `shell_sandbox`) unless it is off
    let mode = crate::sandbox::mode();
    let mut c = crate::sandbox::Policy::new(mode, cwd).command(cmd)?;
    c.current_dir(cwd)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    unsafe { c.pre_exec(|| if libc::setpgid(0, 0) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }); }
    let mut child = c.spawn().map_err(|e| format!("cannot start zsh: {e}"))?;
    let pid = child.id() as libc::pid_t;
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (tx, rx) = mpsc::channel::<()>();
    let readers: Vec<Box<dyn Read + Send>> = [
        child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>),
        child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>),
    ].into_iter().flatten().collect();
    let nreaders = readers.len();
    for mut r in readers {
        let (buf, tx) = (buf.clone(), tx.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(k) = r.read(&mut chunk) {
                if k == 0 { break; }
                if let Ok(mut b) = buf.lock() {
                    let room = buf_max.saturating_sub(b.len());
                    b.extend_from_slice(&chunk[..k.min(room)]);
                }
            }
            let _ = tx.send(());
        });
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(timeout_s.max(1));
    let (mut timed_out, mut cancelled) = (false, false);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if crate::repl::CANCEL.load(std::sync::atomic::Ordering::SeqCst) => {
                cancelled = true;
                unsafe { libc::kill(-pid, libc::SIGKILL); }
                break child.wait().ok();
            }
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                unsafe { libc::kill(-pid, libc::SIGKILL); }
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let grace = Instant::now() + Duration::from_secs(if timed_out || cancelled { 1 } else { 2 });
    for _ in 0..nreaders {
        let left = grace.saturating_duration_since(Instant::now());
        if rx.recv_timeout(left).is_err() { break; }
    }
    let bytes = buf.lock().map(|b| b.clone()).unwrap_or_default();
    let mut out = String::from_utf8_lossy(&bytes).into_owned();
    if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); }
    let code = status.and_then(|s| s.code());
    // a quiet strict-mode failure of a network tool (`curl -s`: exit 6, no output) gets the hint too
    let h = crate::sandbox::hint(mode, &out).or_else(|| if timed_out || cancelled { None } else { crate::sandbox::quiet_network_hint(mode, cmd, &out, code) });
    if let Some(h) = h { out.push_str(h); out.push('\n'); }
    Ok(Shell { out, code, signal: status.and_then(|s| s.signal()), timed_out, cancelled })
}

/// Glob → anchored regex. `**/` matches zero or more dirs; a pattern without `/` matches at any depth.
fn glob_re(pat: &str) -> Result<Regex, String> {
    let pat = pat.trim_start_matches("./");
    let pat = if pat.contains('/') { pat.to_string() } else { format!("**/{pat}") };
    let cs: Vec<char> = pat.chars().collect();
    let mut re = String::from("^");
    let (mut i, mut depth) = (0, 0);
    while i < cs.len() {
        match cs[i] {
            '*' if cs.get(i + 1) == Some(&'*') => {
                if cs.get(i + 2) == Some(&'/') { re.push_str("(?:.*/)?"); i += 3; } else { re.push_str(".*"); i += 2; }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => { depth += 1; re.push_str("(?:"); }
            '}' if depth > 0 => { depth -= 1; re.push(')'); }
            ',' if depth > 0 => re.push('|'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    for _ in 0..depth { re.push(')'); }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("bad glob: {e}"))
}

fn walk(root: &Path) -> impl Iterator<Item = walkdir::DirEntry> {
    walkdir::WalkDir::new(root).follow_links(false).sort_by_file_name().into_iter()
        .filter_entry(|e| e.depth() == 0 || !(e.file_type().is_dir() && SKIP_DIRS.contains(&e.file_name().to_str().unwrap_or(""))))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
}

fn rel_slash(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// A glob call's (directory, pattern). A pattern that starts at `/` or `~/` names its own directory: its literal
/// leading part becomes the directory (`/etc/ssh/*.conf` → `/etc/ssh`, `*.conf`; a literal path with no wildcard
/// lists that directory, or matches that one file), so the outside-the-project rules see it (`outside::check`) and an
/// absolute pattern inside the project works. Otherwise `path` (default ".") and the pattern as given.
/// A leading `/` that names no real directory (`/`, `/*`, `/**/*.py`, `/src/*.py` with no `/src`) is the project
/// root as a small model writes it ("list the files in this folder" → `/*` searched the whole disk, BUGS #25): it is
/// relative to `path`, so only an explicit outside path reaches the outside-the-project rules.
pub fn glob_args(input: &Value) -> (String, String) {
    let pat = sv(input, "pattern").trim();
    let home = || std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let relative = |p: &str| (opt_str(input, "path").unwrap_or(".").to_string(), if p.is_empty() { "*".to_string() } else { p.to_string() });
    let abs = if let Some(r) = pat.strip_prefix("~/") { format!("{}/{r}", home().trim_end_matches('/')) }
        else if pat == "~" { home() } else if pat.starts_with('/') { pat.to_string() } else {
            return relative(pat);
        };
    let parts: Vec<&str> = abs.split('/').collect();
    let wild = parts.iter().position(|c| c.contains(['*', '?', '{', '['])).unwrap_or(parts.len());
    let base = parts[..wild].join("/");
    if pat.starts_with('/') && (base.trim_matches('/').is_empty() || !Path::new(&base).exists()) {
        return relative(pat.trim_start_matches('/'));
    }
    if wild < parts.len() { return (base, parts[wild..].join("/")); }
    let bp = Path::new(&base);
    match (bp.is_dir(), bp.parent(), bp.file_name()) {
        (false, Some(dir), Some(f)) => (dir.to_string_lossy().into_owned(), f.to_string_lossy().into_owned()),
        _ => (base, "*".into()),
    }
}

fn glob(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    arg(input, "pattern")?;
    let (dir, pat) = glob_args(input);
    let re = glob_re(&pat)?;
    let root = resolve(ctx, &dir);
    if !root.is_dir() { return Err(format!("not a directory: {}", root.display())); }
    // a huge tree (a whole disk, a home folder) stops at a time / file cap instead of running for minutes
    let deadline = Instant::now() + Duration::from_secs(GLOB_SECS);
    let (mut hits, mut seen, mut cut) = (Vec::new(), 0usize, false);
    for e in walk(&root) {
        seen += 1;
        if seen > GLOB_SCAN_MAX || hits.len() >= GLOB_HITS_MAX || (seen % 256 == 0 && Instant::now() >= deadline) { cut = true; break; }
        if re.is_match(&rel_slash(&root, e.path())) { hits.push(rel(ctx, e.path())); }
    }
    let stop = format!("[search stopped early (over {GLOB_SECS} s or too many files under {}): narrow the pattern or the path]", root.display());
    if hits.is_empty() { return Ok(if cut { format!("no files found\n{stop}") } else { "no files found".into() }); }
    hits.sort();
    let n = hits.len();
    hits.truncate(GLOB_MAX);
    let mut out = hits.join("\n");
    if n > GLOB_MAX { out.push_str(&format!("\n[{GLOB_MAX} of {}{n} files shown]", if cut { "at least " } else { "" })); }
    if cut { out.push('\n'); out.push_str(&stop); }
    Ok(out)
}

fn grep(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let re = Regex::new(arg(input, "pattern")?).map_err(|e| format!("bad regex: {e}"))?;
    let filt = opt_str(input, "glob").map(glob_re).transpose()?;
    let root = resolve(ctx, opt_str(input, "path").unwrap_or("."));
    if !root.exists() { return Err(format!("no such path: {}", root.display())); }
    let files: Box<dyn Iterator<Item = PathBuf>> = if root.is_file() {
        Box::new(std::iter::once(root.clone()))
    } else {
        Box::new(walk(&root).map(|e| e.into_path()))
    };
    let mut out = String::new();
    let mut n = 0;
    'files: for f in files {
        if let Some(g) = &filt {
            if root.is_dir() && !g.is_match(&rel_slash(&root, &f)) { continue; }
        }
        let Ok(bytes) = std::fs::read(&f) else { continue };
        if is_binary(&bytes) { continue; }
        let Ok(text) = std::str::from_utf8(&bytes) else { continue };
        let name = rel(ctx, &f);
        for (i, line) in text.lines().enumerate() {
            if !re.is_match(line) { continue; }
            if n == GREP_MAX { out.push_str(&format!("[stopped at {GREP_MAX} matches: there are more; narrow the pattern, path or glob]")); break 'files; }
            let t = line.trim();
            let t: String = if t.chars().count() > 200 { t.chars().take(200).chain("…".chars()).collect() } else { t.into() };
            out.push_str(&format!("{name}:{}: {t}\n", i + 1));
            n += 1;
        }
    }
    if n == 0 { return Ok("no matches".into()); }
    Ok(out.trim_end().to_string())
}
