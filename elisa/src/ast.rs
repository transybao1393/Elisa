//! Light syntax trees (tree-sitter) for code understanding: outlines, symbols by name, edits by name, the parse guard
//! and the symbols an edit touched (for targeted checks). Grammars: Rust, Python, Swift, TypeScript / TSX, JavaScript
//! (JSX), SQL, bash; every crate MIT (see ../THIRD_PARTY_NOTICES.md).
//!
//! RAM: one parser and one tree at a time, files above `MAX_BYTES` are not parsed (a tree is ~10-30x its source), so
//! the peak stays well under the 32 MB helper budget; nothing is cached between calls.
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

/// files above this size are not parsed (outline / guard / symbols skip them)
pub const MAX_BYTES: usize = 1 << 20;
/// a signature line is cut to this many chars
const SIG_MAX: usize = 160;
/// candidates / names listed in an error
const LIST_MAX: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang { Rust, Python, Swift, TypeScript, Tsx, JavaScript, Sql, Bash }

impl Lang {
    /// by file extension; None = not a supported language
    pub fn of(path: &Path) -> Option<Lang> { Lang::by_ext(path).filter(|l| l.language().is_some()) }

    fn by_ext(path: &Path) -> Option<Lang> {
        Some(match path.extension()?.to_str()? {
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "swift" => Lang::Swift,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "sql" => Lang::Sql,
            "sh" | "bash" => Lang::Bash,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust", Lang::Python => "python", Lang::Swift => "swift", Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx", Lang::JavaScript => "javascript", Lang::Sql => "sql", Lang::Bash => "bash",
        }
    }
    /// the grammar; None = built without its `lang-*` feature
    fn language(self) -> Option<Language> {
        Some(match self {
            #[cfg(feature = "lang-rust")]
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "lang-python")]
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "lang-swift")]
            Lang::Swift => tree_sitter_swift::LANGUAGE.into(),
            #[cfg(feature = "lang-typescript")]
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "lang-typescript")]
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            #[cfg(feature = "lang-javascript")]
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "lang-sql")]
            Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
            #[cfg(feature = "lang-bash")]
            Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
            #[allow(unreachable_patterns)]
            _ => return None,
        })
    }
    /// path separator shown in symbol paths (`Type::method` in Rust, `Class.method` elsewhere); lookups accept both
    pub fn sep(self) -> &'static str { if self == Lang::Rust { "::" } else { "." } }
}

pub fn parse(lang: Lang, src: &str) -> Option<Tree> {
    if src.len() > MAX_BYTES { return None; }
    let mut p = Parser::new();
    p.set_language(&lang.language()?).ok()?;
    p.parse(src, None)
}

/// One symbol: a function, method, type, impl, class, constant, … with its byte and line range. The range includes
/// leading doc comments and attributes / decorators (what `read_symbol` shows is what `edit_symbol` replaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sym {
    /// fn, struct, enum, trait, impl, mod, const, static, type, macro, class, def, func, protocol, extension, actor,
    /// interface, function, method, namespace, var, init, table, view, index, …
    pub kind: &'static str,
    /// full path, e.g. `Foo::bar` (Rust) or `Foo.bar`
    pub path: String,
    /// the header on one line, e.g. `pub fn bar(&self, x: u8) -> bool`
    pub sig: String,
    pub depth: usize,
    pub start: usize,
    pub end: usize,
    /// 1-based, inclusive
    pub line_a: usize,
    pub line_b: usize,
    /// impl / trait / class / mod / …: has members listed under it
    pub container: bool,
    /// a test function (`#[test]`, `#[tokio::test]`, Python `test_*`)
    pub test: bool,
}

impl Sym {
    /// the last path segment
    pub fn name(&self) -> &str {
        let p = self.path.as_str();
        let i = p.rfind("::").map(|i| i + 2).into_iter().chain(p.rfind('.').map(|i| i + 1)).max().unwrap_or(0);
        &p[i..]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Outline { pub lines: usize, pub imports: Vec<String>, pub syms: Vec<Sym> }

fn text<'s>(n: Node, src: &'s str) -> &'s str { &src[n.byte_range()] }
fn one_line(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    let mut t: String = s.chars().take(n).collect();
    t.push('…');
    t
}
fn field<'t>(n: Node<'t>, f: &str) -> Option<Node<'t>> { n.child_by_field_name(f) }
fn field_text(n: Node, f: &str, src: &str) -> Option<String> { field(n, f).map(|c| one_line(text(c, src))) }
/// `Foo<T>` → `Foo`, `&mut Foo` → `Foo`, `a::Foo` → `Foo`
fn base_name(t: &str) -> String {
    let t = t.trim().trim_start_matches('&').trim_start_matches("mut ").trim_start_matches("dyn ").trim();
    let t = t.split('<').next().unwrap_or(t);
    t.rsplit("::").next().unwrap_or(t).trim().to_string()
}

struct Item<'t> {
    kind: &'static str,
    /// path segment
    key: String,
    /// the definition node (signature starts here)
    node: Node<'t>,
    /// the node whose range is the symbol (an export / decorated wrapper, else `node`)
    outer: Node<'t>,
    /// where the signature stops (the body); None = the whole node, one line
    body: Option<Node<'t>>,
    /// members are listed (impl / class / …)
    members: Option<Node<'t>>,
    test: bool,
}

fn item<'t>(kind: &'static str, key: String, node: Node<'t>, outer: Node<'t>, body: Option<Node<'t>>) -> Item<'t> {
    Item { kind, key, node, outer, body, members: None, test: false }
}

/// the symbols one node defines (usually 0 or 1; a JS `const a = …, b = …` gives 2)
fn classify<'t>(lang: Lang, n: Node<'t>, src: &str, depth: usize, imports: &mut Vec<String>) -> Vec<Item<'t>> {
    let name = |f: &str| field_text(n, f, src).unwrap_or_default();
    let mut v = Vec::new();
    match lang {
        Lang::Rust => match n.kind() {
            "function_item" | "function_signature_item" => {
                let mut it = item("fn", name("name"), n, n, field(n, "body"));
                it.test = rust_test_attr(n, src);
                v.push(it);
            }
            "struct_item" | "enum_item" | "union_item" => {
                let kind = match n.kind() { "struct_item" => "struct", "enum_item" => "enum", _ => "union" };
                v.push(item(kind, name("name"), n, n, field(n, "body")));
            }
            "trait_item" | "mod_item" => {
                let kind = if n.kind() == "trait_item" { "trait" } else { "mod" };
                let mut it = item(kind, name("name"), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "impl_item" => {
                let ty = field_text(n, "type", src).unwrap_or_default();
                let mut it = item("impl", base_name(&ty), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "const_item" | "static_item" | "type_item" | "macro_definition" => {
                let kind = match n.kind() { "const_item" => "const", "static_item" => "static", "type_item" => "type", _ => "macro" };
                v.push(item(kind, name("name"), n, n, None));
            }
            "use_declaration" if depth == 0 => if let Some(a) = field(n, "argument") { imports.push(one_line(text(a, src))) },
            _ => {}
        },
        Lang::Python => match n.kind() {
            "function_definition" => {
                let key = name("name");
                let mut it = item("def", key.clone(), n, n, field(n, "body"));
                it.test = key.starts_with("test");
                v.push(it);
            }
            "class_definition" => {
                let mut it = item("class", name("name"), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "decorated_definition" => if let Some(d) = field(n, "definition") {
                for mut it in classify(lang, d, src, depth, imports) { it.outer = n; v.push(it); }
            },
            "expression_statement" if depth == 0 => {
                let a = n.named_child(0).filter(|a| a.kind() == "assignment");
                if let Some(l) = a.and_then(|a| field(a, "left")).filter(|l| l.kind() == "identifier") {
                    let key = text(l, src).to_string();
                    if is_const_name(&key) { v.push(item("const", key, n, n, None)); }
                }
            }
            "import_statement" | "import_from_statement" => {
                let t = one_line(text(n, src));
                imports.push(t.strip_prefix("from ").or_else(|| t.strip_prefix("import ")).unwrap_or(&t).to_string());
            }
            _ => {}
        },
        Lang::Swift => match n.kind() {
            "class_declaration" | "protocol_declaration" => {
                let kind = match field(n, "declaration_kind").map(|k| text(k, src)) {
                    Some("struct") => "struct", Some("enum") => "enum", Some("extension") => "extension",
                    Some("actor") => "actor", Some("protocol") => "protocol", _ if n.kind() == "protocol_declaration" => "protocol",
                    _ => "class",
                };
                let mut it = item(kind, base_name(&name("name")), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "function_declaration" | "protocol_function_declaration" => v.push(item("func", name("name"), n, n, field(n, "body"))),
            "init_declaration" => v.push(item("init", "init".into(), n, n, field(n, "body"))),
            "deinit_declaration" => v.push(item("deinit", "deinit".into(), n, n, field(n, "body"))),
            "typealias_declaration" => v.push(item("typealias", name("name"), n, n, None)),
            "property_declaration" => {
                // top-level let / var, and computed properties in types (SwiftUI `var body: some View { … }`)
                let computed = field(n, "computed_value");
                if depth == 0 || computed.is_some() {
                    let key = name("name");
                    let key = key.trim_start_matches("let ").trim_start_matches("var ").to_string();
                    v.push(item("var", key, n, n, computed));
                }
            }
            "import_declaration" => imports.push(one_line(text(n, src).trim_start_matches("import "))),
            _ => {}
        },
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => match n.kind() {
            "function_declaration" | "generator_function_declaration" => v.push(item("function", name("name"), n, n, field(n, "body"))),
            "class_declaration" | "abstract_class_declaration" | "class" => {
                let mut it = item("class", name("name"), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "interface_declaration" => v.push(item("interface", name("name"), n, n, field(n, "body"))),
            "type_alias_declaration" => v.push(item("type", name("name"), n, n, None)),
            "enum_declaration" => v.push(item("enum", name("name"), n, n, field(n, "body"))),
            "method_definition" | "abstract_method_signature" | "method_signature" => {
                v.push(item("method", name("name"), n, n, field(n, "body")))
            }
            "internal_module" | "module" => {
                let mut it = item("namespace", name("name").trim_matches(|c| c == '"' || c == '\'').to_string(), n, n, field(n, "body"));
                it.members = field(n, "body");
                v.push(it);
            }
            "expression_statement" | "ambient_declaration" => if let Some(c) = n.named_child(0) {
                if matches!(c.kind(), "internal_module" | "module" | "function_signature" | "class_declaration" | "lexical_declaration") || n.kind() == "ambient_declaration" {
                    for mut it in classify(lang, c, src, depth, imports) { it.outer = n; v.push(it); }
                }
            },
            "export_statement" => {
                let inner = field(n, "declaration").or_else(|| field(n, "value"));
                if let Some(d) = inner {
                    let mut got = classify(lang, d, src, depth, imports);
                    if got.is_empty() && matches!(d.kind(), "function_expression" | "arrow_function" | "class") {
                        got.push(item("function", "default".into(), d, d, field(d, "body")));
                    }
                    for mut it in got {
                        if it.key.is_empty() { it.key = "default".into(); }
                        it.outer = n;
                        v.push(it);
                    }
                }
            }
            "lexical_declaration" if depth == 0 || n.parent().is_some_and(|p| p.kind() == "export_statement") => {
                let is_const = field(n, "kind").is_some_and(|k| text(k, src) == "const");
                let decls: Vec<Node> = { let mut c = n.walk(); n.named_children(&mut c).filter(|d| d.kind() == "variable_declarator").collect() };
                let single = decls.len() == 1;
                for d in decls {
                    let key = field_text(d, "name", src).unwrap_or_default();
                    let val = field(d, "value");
                    let func = val.filter(|x| matches!(x.kind(), "arrow_function" | "function_expression" | "function" | "generator_function"));
                    let outer = if single { n } else { d };
                    if let Some(f) = func { v.push(item("function", key, outer, outer, field(f, "body"))); }
                    else if is_const { v.push(item("const", key, outer, outer, None)); }
                }
            }
            "import_statement" => {
                let from = field(n, "source").map(|s| text(s, src).trim_matches(|c| c == '"' || c == '\'').to_string()).unwrap_or_default();
                let what = { let mut c = n.walk(); let w = n.named_children(&mut c).find(|c| c.kind() == "import_clause").map(|c| one_line(text(c, src))); w };
                imports.push(match what { Some(w) => format!("{from} {w}"), None => from });
            }
            _ => {}
        },
        Lang::Bash => match n.kind() {
            "function_definition" => v.push(item("function", name("name"), n, n, field(n, "body"))),
            "variable_assignment" if depth == 0 => {
                let key = name("name");
                if is_const_name(&key) { v.push(item("var", key, n, n, None)); }
            }
            _ => {}
        },
        Lang::Sql => if n.kind() == "statement" {
            if let Some(c) = n.named_child(0).filter(|c| c.kind().starts_with("create_")) {
                let kind = sql_kind(&c.kind()["create_".len()..]);
                let key = {
                    let mut cur = c.walk();
                    let kids: Vec<Node> = c.named_children(&mut cur).collect();
                    kids.iter().find(|k| k.kind() == "identifier").or_else(|| kids.iter().find(|k| k.kind() == "object_reference"))
                        .map(|k| one_line(text(*k, src))).unwrap_or_default()
                };
                if !key.is_empty() { v.push(item(kind, key, c, n, None)); }
            }
        },
    }
    v
}

fn sql_kind(k: &str) -> &'static str {
    match k {
        "table" => "table", "view" => "view", "materialized_view" => "view", "index" => "index", "function" => "function",
        "trigger" => "trigger", "type" => "type", "schema" => "schema", "sequence" => "sequence", "database" => "database",
        "extension" => "extension", "role" => "role", _ => "create",
    }
}

fn is_const_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && s.chars().any(|c| c.is_ascii_uppercase())
}

/// a Rust fn with a `#[test]`-like attribute right before it (`#[test]`, `#[tokio::test]`, `#[rstest]`)
fn rust_test_attr(n: Node, src: &str) -> bool {
    let mut p = n.prev_sibling();
    while let Some(s) = p {
        match s.kind() {
            "attribute_item" => {
                let t = text(s, src);
                let inner = t.trim_start_matches("#[").trim_end_matches(']');
                let head = inner.split(['(', ' ']).next().unwrap_or("");
                if head == "test" || head.ends_with("::test") || head == "rstest" || head == "test_case" { return true; }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        p = s.prev_sibling();
    }
    false
}

/// start of the symbol's range: the definition plus the doc comments and attributes right above it (no blank line
/// between); plain `//` comments are not taken
fn leading_start(lang: Lang, n: Node, src: &str) -> usize {
    let mut start = n.start_byte();
    let mut row = n.start_position().row;
    let mut p = n.prev_sibling();
    while let Some(s) = p {
        if s.end_position().row + 1 < row { break; }
        let t = text(s, src);
        let take = match lang {
            Lang::Rust => s.kind() == "attribute_item" || (matches!(s.kind(), "line_comment" | "block_comment") && (t.starts_with("///") || t.starts_with("/**"))),
            Lang::Swift | Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                (s.kind() == "comment" || s.kind() == "multiline_comment") && (t.starts_with("///") || t.starts_with("/**"))
                    || s.kind() == "decorator"
            }
            Lang::Python | Lang::Bash | Lang::Sql => false,
        };
        if !take { break; }
        start = s.start_byte();
        row = s.start_position().row;
        p = s.prev_sibling();
    }
    start
}

/// end of the symbol's range: SQL takes the statement's `;`
fn trailing_end(lang: Lang, n: Node) -> usize {
    if lang == Lang::Sql {
        if let Some(s) = n.next_sibling().filter(|s| s.kind() == ";" && s.start_position().row == n.end_position().row) { return s.end_byte(); }
    }
    n.end_byte()
}

fn sig_of(it: &Item, src: &str) -> String {
    // `export function f` keeps its `export` (not the decorators before it)
    let export = if it.outer.kind() == "export_statement" { let mut c = it.outer.walk(); let e = it.outer.children(&mut c).find(|k| k.kind() == "export").map(|k| k.start_byte()); e } else { None };
    let a = export.unwrap_or(it.node.start_byte());
    let b = it.body.map_or(it.node.end_byte(), |b| b.start_byte()).max(a);
    let mut s = one_line(&src[a..b]);
    // a body-less item: one line is enough (`const X: usize = 400;`)
    if it.body.is_none() { if let Some(i) = s.find(" = ") { if s.len() > 80 { s.truncate(i + 2); s.push('…'); } } }
    let s = s.trim_end_matches(['{', ':']).trim_end().to_string();
    cut(&s, SIG_MAX)
}

fn line_of(src: &str, byte: usize) -> usize { src[..byte.min(src.len())].matches('\n').count() + 1 }

/// All symbols of a source file, in source order (containers before their members).
pub fn outline(lang: Lang, src: &str) -> Option<Outline> {
    let tree = parse(lang, src)?;
    Some(outline_tree(lang, src, &tree))
}

pub fn outline_tree(lang: Lang, src: &str, tree: &Tree) -> Outline {
    let mut o = Outline { lines: src.lines().count(), ..Default::default() };
    walk(lang, tree.root_node(), src, "", 0, &mut o);
    o
}

fn walk(lang: Lang, parent: Node, src: &str, prefix: &str, depth: usize, o: &mut Outline) {
    if depth > 8 { return; }
    let kids: Vec<Node> = { let mut c = parent.walk(); parent.named_children(&mut c).collect() };
    for n in kids {
        for it in classify(lang, n, src, depth, &mut o.imports) {
            let start = leading_start(lang, it.outer, src);
            let end = trailing_end(lang, it.outer);
            let key = if it.key.is_empty() { "?".to_string() } else { it.key.clone() };
            let path = format!("{prefix}{key}");
            o.syms.push(Sym {
                kind: it.kind, path: path.clone(), sig: sig_of(&it, src), depth, start, end,
                line_a: line_of(src, start), line_b: line_of(src, end.saturating_sub(1).max(start)),
                container: it.members.is_some(), test: it.test,
            });
            if let Some(m) = it.members { walk(lang, m, src, &format!("{path}{}", lang.sep()), depth + 1, o); }
        }
    }
}

fn range(a: usize, b: usize) -> String { if a == b { format!("{a}") } else { format!("{a}-{b}") } }

/// The outline of one file as compact text: a header, the imports on one line, then one line per symbol
/// (`lines  signature`, members indented under their impl / class).
pub fn render(rel: &str, lang: Lang, o: &Outline) -> String {
    let mut s = format!("{rel} · {} · {} lines · {} symbols\n", lang.name(), o.lines, o.syms.len());
    if !o.imports.is_empty() {
        let all: Vec<String> = o.imports.iter().map(|i| cut(i, 60)).collect();
        let mut line = String::new();
        let mut shown = 0;
        for i in &all {
            if line.len() + i.len() > 300 { break; }
            if !line.is_empty() { line.push_str(", "); }
            line.push_str(i);
            shown += 1;
        }
        if shown < all.len() { line.push_str(&format!(", … (+{})", all.len() - shown)); }
        s += &format!("imports: {line}\n");
    }
    for y in &o.syms {
        let t = if y.test { " [test]" } else { "" };
        s += &format!("{}{} {}{t}\n", "  ".repeat(y.depth), range(y.line_a, y.line_b), y.sig);
    }
    if o.syms.is_empty() { s += "(no symbols)\n"; }
    s.trim_end().to_string()
}

/// One line per file for a directory outline: `path (N lines): a, B{m1, m2}, c`.
pub fn render_names(rel: &str, o: &Outline) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    let syms = &o.syms;
    while i < syms.len() {
        let y = &syms[i];
        if y.depth > 0 { i += 1; continue; }
        let mut j = i + 1;
        let mut kids = Vec::new();
        while j < syms.len() && syms[j].depth > 0 { if syms[j].depth == 1 { kids.push(syms[j].name().to_string()); } j += 1; }
        let base = y.name();
        parts.push(if kids.is_empty() { base.to_string() } else { format!("{base}{{{}}}", cut(&kids.join(", "), 200)) });
        i = j;
    }
    format!("{rel} ({} lines): {}", o.lines, cut(&parts.join(", "), 600))
}

fn norm(q: &str) -> String { q.trim().trim_end_matches("()").replace("::", ".") }

/// The symbol called `query` (`name`, `Type::method`, `Class.method`, a path suffix). `line` picks among several with
/// the same name. Errors list the candidates (or similar names).
pub fn find<'a>(o: &'a Outline, query: &str, line: Option<usize>) -> Result<&'a Sym, String> {
    let q = norm(query);
    if q.is_empty() { return Err("missing symbol name".into()); }
    let exact: Vec<&Sym> = o.syms.iter().filter(|y| norm(&y.path) == q).collect();
    let mut c: Vec<&Sym> = if exact.is_empty() { o.syms.iter().filter(|y| norm(&y.path).ends_with(&format!(".{q}"))).collect() } else { exact };
    if let Some(l) = line.filter(|&l| l > 0) {
        let at: Vec<&Sym> = c.iter().copied().filter(|y| y.line_a <= l && l <= y.line_b).collect();
        // the innermost one containing the line
        if let Some(y) = at.iter().max_by_key(|y| y.depth) { return Ok(y); }
        if let Some(y) = c.iter().min_by_key(|y| y.line_a.abs_diff(l)) { return Ok(y); }
    }
    if c.len() > 1 {
        // `Foo` = the type, not its impl / extension blocks
        let defs: Vec<&Sym> = c.iter().copied().filter(|y| !matches!(y.kind, "impl" | "extension")).collect();
        if defs.len() == 1 { c = defs; }
    }
    match c.len() {
        1 => Ok(c[0]),
        0 => {
            let low = q.to_lowercase();
            let last = low.rsplit('.').next().unwrap_or(&low).to_string();
            let mut near: Vec<String> = o.syms.iter().filter(|y| y.path.to_lowercase().contains(&last)).map(|y| y.path.clone()).collect();
            if near.is_empty() { near = o.syms.iter().filter(|y| y.depth == 0).map(|y| y.path.clone()).collect(); }
            near.dedup();
            let more = near.len().saturating_sub(LIST_MAX);
            near.truncate(LIST_MAX);
            let tail = if more > 0 { format!(", … (+{more})") } else { String::new() };
            Err(format!("no symbol `{query}`; {}: {}{tail}", if near.iter().any(|n| n.to_lowercase().contains(&last)) { "similar" } else { "top-level symbols" }, near.join(", ")))
        }
        n => {
            let list: Vec<String> = c.iter().take(LIST_MAX).map(|y| format!("  {} {} {}", range(y.line_a, y.line_b), y.kind, y.path)).collect();
            Err(format!("`{query}` matches {n} symbols; pass the full path or `line`:\n{}", list.join("\n")))
        }
    }
}

// ---------- edits by name ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Replace, Before, After, Delete }

impl Action {
    pub fn parse(s: &str) -> Result<Action, String> {
        Ok(match s.trim() {
            "" | "replace" => Action::Replace,
            "before" | "insert_before" => Action::Before,
            "after" | "insert_after" => Action::After,
            "delete" | "remove" => Action::Delete,
            o => return Err(format!("unknown action `{o}` (replace, before, after, delete)")),
        })
    }
}

fn line_start(src: &str, at: usize) -> usize { src[..at].rfind('\n').map_or(0, |i| i + 1) }
fn line_end(src: &str, at: usize) -> usize { src[at..].find('\n').map_or(src.len(), |i| at + i) }

/// `s` with its common indentation removed, then every non-empty line indented by `ind` (the first line too unless
/// `first` is false)
pub fn reindent(s: &str, ind: &str, first: bool) -> String {
    let s = s.trim_matches('\n').trim_end();
    let common = s.lines().filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut out = String::new();
    for (i, l) in s.lines().enumerate() {
        if i > 0 { out.push('\n'); }
        if l.trim().is_empty() { continue; }
        let body = if l.len() >= common && l.is_char_boundary(common) && l[..common].trim().is_empty() { &l[common..] } else { l.trim_start() };
        if i > 0 || first { out.push_str(ind); }
        out.push_str(body);
    }
    out
}

/// The new file text after `action` on `y` with `source` (any indentation: it takes the symbol's).
pub fn apply(src: &str, y: &Sym, action: Action, source: &str) -> String {
    let ls = line_start(src, y.start);
    let prefix = &src[ls..y.start];
    let alone = prefix.trim().is_empty();
    let ind: String = if alone { prefix.to_string() } else { prefix.chars().take_while(|c| c.is_whitespace()).collect() };
    let (a, first) = if alone { (ls, true) } else { (y.start, false) };
    match action {
        Action::Replace => format!("{}{}{}", &src[..a], reindent(source, &ind, first), &src[y.end..]),
        Action::Before => {
            let at = if alone { ls } else { y.start };
            let sep = if alone { "\n\n" } else { " " };
            format!("{}{}{sep}{}", &src[..at], reindent(source, &ind, true), if alone { &src[ls..] } else { &src[y.start..] })
        }
        Action::After => {
            let e = line_end(src, y.end);
            let tail_alone = src[y.end..e].trim().is_empty();
            let at = if tail_alone { e } else { y.end };
            format!("{}\n\n{}{}", &src[..at], reindent(source, &ind, true), &src[at..])
        }
        Action::Delete => {
            let e = line_end(src, y.end);
            let tail_alone = src[y.end..e].trim().is_empty();
            if !(alone && tail_alone) { return format!("{}{}", &src[..y.start], &src[y.end..]); }
            // whole lines, then one blank line fewer when blanks now meet
            let mut b = (e + 1).min(src.len());
            let before_blank = ls == 0 || src[..ls].ends_with("\n\n");
            let after_blank = src[b..].starts_with('\n');
            if after_blank && before_blank { b += 1; }
            format!("{}{}", &src[..ls], &src[b..])
        }
    }
}

// ---------- parse guard ----------

/// One parse problem: 1-based line and column (chars), what it is, and the source line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseErr { pub line: usize, pub col: usize, pub what: String, pub src_line: String }

/// ERROR and MISSING nodes, in source order (an ERROR's insides are not searched again)
pub fn errors(src: &str, tree: &Tree) -> Vec<ParseErr> {
    let mut out = Vec::new();
    if !tree.root_node().has_error() { return out; }
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            let p = n.start_position();
            let ls = line_start(src, n.start_byte().min(src.len()));
            let le = line_end(src, ls);
            let col = src[ls..n.start_byte().min(src.len()).max(ls)].chars().count() + 1;
            let what = if n.is_missing() {
                let k = n.kind();
                format!("missing `{}`", if k.chars().all(|c| c.is_ascii_punctuation()) { k.to_string() } else { k.replace('_', " ") })
            } else {
                format!("unexpected `{}`", cut(&one_line(text(n, src)), 40))
            };
            out.push(ParseErr { line: p.row + 1, col, what, src_line: src[ls..le].to_string() });
            continue;
        }
        if !n.has_error() { continue; }
        let mut c = n.walk();
        let kids: Vec<Node> = n.children(&mut c).collect();
        // the error sits in a hidden node the node API never shows (Python's `MISSING _newline` when two statements
        // are joined on one line): report it here, or the guard lets a broken file through (soak M2)
        if !kids.iter().any(|k| k.has_error() || k.is_error() || k.is_missing()) { out.push(hidden_error(src, n, &kids)); }
        for k in kids.into_iter().rev() { stack.push(k); }
    }
    out
}

/// A parse error inside `n` that no visible child carries: where two statements meet on one line with nothing
/// between them ("missing line break"), else at the start of `n`.
fn hidden_error(src: &str, n: Node, kids: &[Node]) -> ParseErr {
    let at = |b: usize, what: String| {
        let b = b.min(src.len());
        let ls = line_start(src, b);
        let le = line_end(src, ls);
        ParseErr { line: src[..ls].matches('\n').count() + 1, col: src[ls..b].chars().count() + 1, what, src_line: src[ls..le].to_string() }
    };
    let named: Vec<&Node> = kids.iter().filter(|k| k.is_named() && !k.is_extra()).collect();
    for w in named.windows(2) {
        let gap = src.get(w[0].end_byte()..w[1].start_byte()).unwrap_or("");
        if !gap.contains('\n') && !gap.contains(';') {
            return at(w[1].start_byte(), format!("missing line break before `{}`", cut(&one_line(text(*w[1], src)), 40)));
        }
    }
    at(n.start_byte(), format!("syntax error in `{}`", n.kind().replace('_', " ")))
}

/// what identifies an error when lines move: its kind + text, not its position
fn err_key(e: &ParseErr) -> String { e.what.clone() }

/// Parse errors in `new` that `old` did not have (matched by what they are, not where: an edit above them moves
/// them). `old` None = a new file (every error is new). Empty when the language is not supported or too big.
pub fn new_errors(lang: Lang, old: Option<&str>, new: &str) -> Vec<ParseErr> {
    let Some(t) = parse(lang, new) else { return vec![] };
    let errs = errors(new, &t);
    if errs.is_empty() { return errs; }
    drop(t);
    let mut before: Vec<String> = old.and_then(|o| parse(lang, o).map(|t| errors(o, &t))).unwrap_or_default().iter().map(err_key).collect();
    let mut out = Vec::new();
    for e in errs {
        let k = err_key(&e);
        if let Some(i) = before.iter().position(|b| *b == k) { before.swap_remove(i); } else { out.push(e); }
    }
    out
}

/// The error the model gets for a refused change.
pub fn guard_message(rel: &str, lang: Lang, errs: &[ParseErr]) -> String {
    let n = errs.len();
    let mut s = format!("refused: this change would add {n} syntax error{} to {rel} ({} parser); the file is unchanged.\n",
        if n == 1 { "" } else { "s" }, lang.name());
    for e in errs.iter().take(3) {
        let w = e.line.to_string().len();
        let line: String = e.src_line.chars().take(160).collect();
        let pad: String = line.chars().take(e.col.saturating_sub(1)).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
        s += &format!("{}:{}: {}\n  {} | {}\n  {} | {pad}^\n", e.line, e.col, e.what, e.line, line.trim_end(), " ".repeat(w));
    }
    if n > 3 { s += &format!("… and {} more\n", n - 3); }
    s += "Fix the code and send the change again (errors the file already had do not block).";
    s
}

/// Refuse (Err = the message) when `new` adds parse errors to a supported file. `old` None = a new file.
pub fn guard(path: &Path, rel: &str, old: Option<&str>, new: &str) -> Result<(), String> {
    let Some(lang) = Lang::of(path) else { return Ok(()) };
    let errs = new_errors(lang, old, new);
    if errs.is_empty() { Ok(()) } else { Err(guard_message(rel, lang, &errs)) }
}

// ---------- duplicates after an edit (the parse guard's write / edit warning) ----------

/// Everything defined twice in one file, each line named by `rel`: top-level symbols (not `impl` / extension
/// blocks), the same HTTP route registered twice (Python / JS decorators; ui_run2 added `search_items` twice), and
/// class members (Python / JS / TS; Rust and Swift members are skipped: two `impl` / extension blocks may each have
/// their own `fmt`). A Python `@overload` / `@x.setter` / `@x.deleter` member is not a duplicate.
pub fn duplicates(path: &Path, rel: &str) -> Vec<String> {
    static ROUTE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let mut out = Vec::new();
    let Some(lang) = Lang::of(path) else { return out };
    let Ok(src) = std::fs::read_to_string(path) else { return out };
    let Some(o) = outline(lang, &src) else { return out };
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for y in o.syms.iter().filter(|y| y.depth == 0 && !matches!(y.kind, "impl" | "extension")) {
        match seen.iter().find(|(p, _)| *p == y.path) {
            Some((_, l)) => out.push(format!("{rel}: `{}` is defined twice (L{l} and L{})", y.path, y.line_a)),
            None => seen.push((y.path.as_str(), y.line_a)),
        }
    }
    let re = ROUTE.get_or_init(|| regex::Regex::new(r#"^\s*@?\w+\.(get|post|put|delete|patch)\(\s*["']([^"']+)["']"#).unwrap());
    let mut routes: Vec<(String, usize)> = Vec::new();
    for (i, l) in src.lines().enumerate() {
        if let Some(c) = re.captures(l) {
            let key = format!("{} {}", c[1].to_uppercase(), &c[2]);
            match routes.iter().find(|(k, _)| *k == key) {
                Some((_, a)) => out.push(format!("{rel}: route {key} is registered twice (L{a} and L{})", i + 1)),
                None => routes.push((key, i + 1)),
            }
        }
    }
    if matches!(lang, Lang::Rust | Lang::Swift | Lang::Sql | Lang::Bash) { return out; }
    let alt = |y: &Sym| src.get(y.start..y.end).is_some_and(|t| t.lines().take(4).any(|l| {
        let l = l.trim();
        l.starts_with('@') && (l.contains("overload") || l.ends_with(".setter") || l.ends_with(".deleter") || l.contains(".setter(") || l.contains(".deleter("))
    }));
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for y in o.syms.iter().filter(|y| y.depth > 0 && !y.container) {
        if alt(y) { continue; }
        match seen.iter().find(|(q, _)| *q == y.path) {
            Some((_, l)) => out.push(format!("{rel}: `{}` is defined twice (L{l} and L{})", y.path, y.line_a)),
            None => seen.push((y.path.as_str(), y.line_a)),
        }
    }
    out
}

/// The duplicates of `after` that `before` (the same file before the edit) did not have, matched without their line
/// numbers (an edit above them moves them).
pub fn added_duplicates(before: &[String], after: &[String]) -> Vec<String> {
    let key = |s: &str| s.split(" (L").next().unwrap_or(s).to_string();
    let mut old: Vec<String> = before.iter().map(|s| key(s)).collect();
    after.iter().filter(|s| {
        let k = key(s);
        match old.iter().position(|o| *o == k) { Some(i) => { old.swap_remove(i); false } None => true }
    }).cloned().collect()
}

/// The note a write / edit tool result gets when the edit defined something twice (empty = none).
pub fn duplicate_note(added: &[String]) -> String {
    if added.is_empty() { return String::new(); }
    let v: Vec<String> = added.iter().take(4).map(|d| format!("- {d}")).collect();
    format!("\nwarning: this edit defined something twice; remove the extra one (keep one definition):\n{}", v.join("\n"))
}

// ---------- what an edit touched (targeted checks) ----------

/// The symbols one file's change touched: deepest changed symbols (by path), and whether anything outside every
/// symbol changed too (imports do not count). `old` None = a new file: `outside` is true.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touched { pub symbols: Vec<String>, pub outside: bool }

pub fn touched(lang: Lang, old: Option<&str>, new: &str) -> Option<Touched> {
    let Some(old) = old else { return Some(Touched { symbols: vec![], outside: true }) };
    let (a, b) = (outline(lang, old)?, outline(lang, new)?);
    let body = |src: &str, y: &Sym| src[y.start..y.end].replace(char::is_whitespace, "");
    // a symbol's identity: path + kind + which one of that name (Rust has `struct Foo` and several `impl Foo`)
    let ids = |o: &Outline| -> Vec<(String, &'static str, usize)> {
        let mut seen: Vec<(String, &'static str)> = Vec::new();
        o.syms.iter().map(|y| {
            let k = (y.path.clone(), y.kind);
            let n = seen.iter().filter(|s| **s == k).count();
            seen.push(k);
            (y.path.clone(), y.kind, n)
        }).collect()
    };
    let (ia, ib) = (ids(&a), ids(&b));
    let mut changed: Vec<&Sym> = Vec::new();
    for (j, y) in b.syms.iter().enumerate() {
        let same = ia.iter().position(|x| *x == ib[j]).is_some_and(|i| body(old, &a.syms[i]) == body(new, y));
        if !same { changed.push(y); }
    }
    let gone: Vec<&Sym> = a.syms.iter().enumerate().filter(|(i, _)| !ib.contains(&ia[*i])).map(|(_, x)| x).collect();
    // keep the deepest: drop a container when one of its members changed
    let mut names: Vec<String> = Vec::new();
    for y in changed.iter().chain(gone.iter()) {
        let sub = format!("{}{}", y.path, lang.sep());
        if y.container && changed.iter().chain(gone.iter()).any(|z| z.path.starts_with(&sub)) { continue; }
        if !names.contains(&y.path) { names.push(y.path.clone()); }
    }
    let skel = |o: &Outline, src: &str| -> String {
        let mut s = String::new();
        let mut at = 0;
        for y in o.syms.iter().filter(|y| y.depth == 0) {
            if y.start < at { continue; }
            s += &src[at..y.start];
            at = y.end;
        }
        s += &src[at.min(src.len())..];
        // imports may change with an edit without making the check any less targeted
        s.lines().filter(|l| { let t = l.trim_start(); !(t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from ")) })
            .collect::<String>().replace(char::is_whitespace, "")
    };
    Some(Touched { symbols: names, outside: skel(&a, old) != skel(&b, new) })
}

/// Rust test functions of one source file (`#[test]` and friends): their names.
pub fn rust_tests(src: &str) -> Vec<String> {
    outline(Lang::Rust, src).map(|o| o.syms.into_iter().filter(|y| y.test).map(|y| y.name().to_string()).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests;
