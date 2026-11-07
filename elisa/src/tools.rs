//! The tools: read, write, edit, bash, glob, grep (contract in DESIGN.md), and the syntax-tree tools outline,
//! read_symbol, edit_symbol (src/ast.rs). write / edit / edit_symbol pass the parse guard first (`ast::guard`).
use crate::ast::{self, Lang};
use crate::types::ToolDef;
use regex::Regex;
use serde_json::{json, Value};
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    /// web access (`web_search`, `web_fetch`); None = off
    pub web: Option<crate::web::WebCfg>,
}

impl Default for ToolCtx {
    fn default() -> Self { ToolCtx { cwd: PathBuf::from("."), max_chars: 6000, bash_timeout_s: 120, ast_guard: true, web: None } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput { pub content: String, pub is_error: bool }

const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".elisa"];
const READ_LIMIT: usize = 400;
/// this many padding lines in a row collapse to one marker line in `read` (a blank line or two is ordinary code)
const PADDING_RUN_MIN: usize = 3;
const GLOB_MAX: usize = 200;
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

/// `web_search` and `web_fetch`, added after the others when the connection turns web access on (fixed for the session)
pub fn web_defs() -> Vec<ToolDef> {
    vec![
        def("web_search", "Search the web; returns the top results (title, url, snippet). For facts not in the project.",
            json!({"query": s()}), &["query"]),
        def("web_fetch", "Read a public web page as plain text (http/https), e.g. a url from web_search.",
            json!({"url": s()}), &["url"]),
    ]
}

/// The name an approval rule is kept under: outline / read_symbol are read-only like `read`, edit_symbol asks like
/// `edit` (an "always" for one covers the other; `auto_approve = ["read"]` covers outline and read_symbol).
pub fn approval_name(name: &str) -> &str {
    match name { "outline" | "read_symbol" => "read", "edit_symbol" => "edit", n => n }
}

pub fn access(name: &str) -> Access {
    match name {
        "read" | "glob" | "grep" | "outline" | "read_symbol" | "web_search" | "web_fetch" => Access::ReadOnly,
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
        "web_search" => format!("{name} {}", sv(input, "query")),
        "web_fetch" => format!("{name} {}", sv(input, "url")),
        _ => name.to_string(),
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
        "web_search" | "web_fetch" => match &ctx.web {
            None => Err("web access is off for this connection".into()),
            Some(w) if name == "web_search" => arg(input, "query").and_then(|q| crate::web::search(q, w)),
            Some(_) => arg(input, "url").and_then(crate::web::fetch),
        },
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
    let p = arg(input, "path")?;
    let path = resolve(ctx, p);
    if path.is_dir() { return Err(format!("{p} is a directory")); }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    if is_binary(&bytes) { return Err(format!("{p} is a binary file")); }
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
    Ok(out)
}

fn write(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = arg(input, "path")?;
    let content = arg(input, "content")?;
    let path = resolve(ctx, p);
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

fn edit(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = arg(input, "path")?;
    let old = arg(input, "old")?;
    let new = arg(input, "new")?;
    if old.is_empty() { return Err("old must not be empty".into()); }
    let path = resolve(ctx, p);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {p}: {e}"))?;
    let n = text.matches(old).count();
    let all = flag(input, "all");
    if n == 0 { return Err(format!("old not found in {p}")); }
    if n > 1 && !all { return Err(format!("old matches {n} times in {p}; add context or set all=true")); }
    let out = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    if ctx.ast_guard { ast::guard(&path, p, Some(&text), &out)?; }
    let before = dups_of(ctx, &path, p);
    std::fs::write(&path, out).map_err(|e| format!("cannot write {p}: {e}"))?;
    Ok(format!("edited {p} ({n} replacement{}){}", if n == 1 { "" } else { "s" }, dup_note(ctx, &path, p, &before)))
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

fn edit_symbol(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let p = arg(input, "path")?;
    let name = arg(input, "name")?;
    let action = ast::Action::parse(opt_str(input, "action").unwrap_or(""))?;
    let source = input.get("source").and_then(Value::as_str).unwrap_or("");
    if action != ast::Action::Delete && source.trim().is_empty() { return Err("source must not be empty (or use action=delete)".into()); }
    let (path, lang, text) = code_file(ctx, p)?;
    let o = ast::outline(lang, &text).ok_or_else(|| format!("{p}: could not parse"))?;
    let y = ast::find(&o, name, line_arg(input)).map_err(|e| format!("{p}: {e}"))?;
    let out = ast::apply(&text, y, action, source);
    if out == text { return Err(format!("no change: the new source of {} is the same", y.path)); }
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
    let mut c = Command::new("/bin/zsh");
    c.arg("-c").arg(cmd).current_dir(cwd)
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
    Ok(Shell { out, code: status.and_then(|s| s.code()), signal: status.and_then(|s| s.signal()), timed_out, cancelled })
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

fn glob(input: &Value, ctx: &ToolCtx) -> Result<String, String> {
    let re = glob_re(arg(input, "pattern")?)?;
    let root = resolve(ctx, opt_str(input, "path").unwrap_or("."));
    if !root.is_dir() { return Err(format!("not a directory: {}", root.display())); }
    let mut hits: Vec<String> = walk(&root)
        .filter(|e| re.is_match(&rel_slash(&root, e.path())))
        .map(|e| rel(ctx, e.path()))
        .collect();
    if hits.is_empty() { return Ok("no files found".into()); }
    hits.sort();
    let n = hits.len();
    hits.truncate(GLOB_MAX);
    let mut out = hits.join("\n");
    if n > GLOB_MAX { out.push_str(&format!("\n[{GLOB_MAX} of {n} files shown]")); }
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
            if n == GREP_MAX { out.push_str(&format!("[stopped at {GREP_MAX} matches]")); break 'files; }
            let t = line.trim();
            let t: String = if t.chars().count() > 200 { t.chars().take(200).chain("…".chars()).collect() } else { t.into() };
            out.push_str(&format!("{name}:{}: {t}\n", i + 1));
            n += 1;
        }
    }
    if n == 0 { return Ok("no matches".into()); }
    Ok(out.trim_end().to_string())
}
