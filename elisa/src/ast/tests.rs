use super::*;

fn paths(o: &Outline) -> Vec<String> { o.syms.iter().map(|y| y.path.clone()).collect() }
fn src_of<'a>(src: &'a str, y: &Sym) -> &'a str { &src[y.start..y.end] }
fn ol(lang: Lang, src: &str) -> Outline { outline(lang, src).expect("parses") }

const RS: &str = "use std::io::{self, Read};
use crate::types::ToolDef;

/// A thing.
#[derive(Debug)]
pub struct Foo<T> { x: T }

pub enum E { A, B }

pub trait Tr {
    fn m(&self) -> u8;
}

impl<T> Tr for Foo<T> {
    fn m(&self) -> u8 { 1 }
}

impl<T> Foo<T> {
    /// Makes one.
    pub fn new(x: T) -> Self {
        Foo { x }
    }

    pub fn get(&self) -> &T { &self.x }
}

const N: usize = 3;

pub fn split_words(s: &str) -> Vec<&str> {
    s.split(' ').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_words_basic() { assert_eq!(split_words(\"a b\").len(), 2); }
}
";

#[test]
fn rust_outline_symbols_signatures_lines() {
    let o = ol(Lang::Rust, RS);
    assert_eq!(paths(&o), ["Foo", "E", "Tr", "Tr::m", "Foo", "Foo::m", "Foo", "Foo::new", "Foo::get", "N", "split_words", "tests", "tests::split_words_basic"]);
    assert_eq!(o.imports, ["std::io::{self, Read}", "crate::types::ToolDef"]);
    let foo = &o.syms[0];
    // doc comment + attribute are part of the symbol
    assert_eq!((foo.kind, foo.line_a, foo.line_b, foo.sig.as_str()), ("struct", 4, 6, "pub struct Foo<T>"));
    let new = o.syms.iter().find(|y| y.path == "Foo::new").unwrap();
    assert_eq!((new.line_a, new.line_b, new.sig.as_str(), new.depth), (19, 22, "pub fn new(x: T) -> Self", 1));
    assert_eq!(o.syms.iter().find(|y| y.path == "N").unwrap().sig, "const N: usize = 3;");
    assert!(o.syms.iter().find(|y| y.path == "tests::split_words_basic").unwrap().test);
    assert!(!o.syms.iter().find(|y| y.path == "split_words").unwrap().test);
    let r = render("src/lib.rs", Lang::Rust, &o);
    assert!(r.starts_with("src/lib.rs · rust · 39 lines · 13 symbols\nimports: std::io::{self, Read}, crate::types::ToolDef\n4-6 pub struct Foo<T>\n"), "{r}");
    assert!(r.contains("\n14-16 impl<T> Tr for Foo<T>\n  15 fn m(&self) -> u8\n"), "{r}");
    assert!(r.contains("  37-38 fn split_words_basic() [test]"), "{r}");
    assert_eq!(rust_tests(RS), ["split_words_basic"]);
    let names = render_names("src/lib.rs", &o);
    assert_eq!(names, "src/lib.rs (39 lines): Foo, E, Tr{m}, Foo{m}, Foo{new, get}, N, split_words, tests{split_words_basic}");
}

#[test]
fn rust_find_by_path_name_and_line() {
    let o = ol(Lang::Rust, RS);
    assert_eq!(find(&o, "Foo::new", None).unwrap().line_a, 19);
    assert_eq!(find(&o, "Foo.new", None).unwrap().line_a, 19, "either separator");
    assert_eq!(find(&o, "new()", None).unwrap().path, "Foo::new", "suffix + call parens");
    // `Foo` = the struct, not its impl blocks
    assert_eq!(find(&o, "Foo", None).unwrap().kind, "struct");
    // `m` is in the trait and in the impl: ambiguous, listed; `line` picks
    let e = find(&o, "m", None).unwrap_err();
    assert!(e.starts_with("`m` matches 2 symbols") && e.contains("11 fn Tr::m") && e.contains("15 fn Foo::m"), "{e}");
    assert_eq!(find(&o, "m", Some(15)).unwrap().path, "Foo::m");
    let e = find(&o, "split", None).unwrap_err();
    assert!(e.contains("similar: split_words, tests::split_words_basic"), "{e}");
    let y = find(&o, "split_words", None).unwrap();
    assert_eq!(src_of(RS, y), "pub fn split_words(s: &str) -> Vec<&str> {\n    s.split(' ').collect()\n}");
}

#[test]
fn rust_edit_symbol_replace_before_after_delete_keep_indent() {
    let o = ol(Lang::Rust, RS);
    let get = find(&o, "Foo::get", None).unwrap();
    // the model sends it unindented: it takes the method's indentation
    let new = apply(RS, get, Action::Replace, "pub fn get(&self) -> &T {\n    &self.x\n}");
    assert!(new.contains("\n    pub fn get(&self) -> &T {\n        &self.x\n    }\n}\n"), "{new}");
    assert!(new_errors(Lang::Rust, Some(RS), &new).is_empty());
    let before = apply(RS, get, Action::Before, "pub fn len(&self) -> usize { 0 }");
    assert!(before.contains("    }\n\n    pub fn len(&self) -> usize { 0 }\n\n    pub fn get"), "{before}");
    let after = apply(RS, find(&o, "N", None).unwrap(), Action::After, "const M: usize = 4;");
    assert!(after.contains("const N: usize = 3;\n\nconst M: usize = 4;\n\npub fn split_words"), "{after}");
    let del = apply(RS, find(&o, "E", None).unwrap(), Action::Delete, "");
    assert!(del.contains("pub struct Foo<T> { x: T }\n\npub trait Tr"), "one blank line left: {del}");
    // deleting a documented item takes its docs and attributes
    let del = apply(RS, find(&o, "Foo", None).unwrap(), Action::Delete, "");
    assert!(!del.contains("A thing") && !del.contains("derive"), "{del}");
    assert!(new_errors(Lang::Rust, Some(RS), &del).is_empty());
}

#[test]
fn rust_guard_new_errors_only() {
    let broken = RS.replace("s.split(' ').collect()", "s.split(' ').collect(");
    let e = new_errors(Lang::Rust, Some(RS), &broken);
    assert!(!e.is_empty());
    let m = guard_message("src/lib.rs", Lang::Rust, &e);
    assert!(m.starts_with("refused: this change would add") && m.contains("src/lib.rs (rust parser); the file is unchanged."), "{m}");
    assert!(m.contains(&format!("{}:", e[0].line)) && m.contains(" | ") && m.contains('^'), "{m}");
    // an already-broken file can still be edited elsewhere: its old error is not new
    let fixed_elsewhere = broken.replace("const N: usize = 3;", "const N: usize = 4;");
    assert!(new_errors(Lang::Rust, Some(&broken), &fixed_elsewhere).is_empty());
    // a new file: every error is new; unsupported languages never block
    assert!(!new_errors(Lang::Rust, None, "fn a( {").is_empty());
    assert!(guard(Path::new("x.txt"), "x.txt", None, "fn a( {").is_ok());
    assert!(guard(Path::new("x.rs"), "x.rs", None, "fn a() {}\n").is_ok());
}

#[test]
fn rust_touched_symbols() {
    let new = RS.replace("s.split(' ').collect()", "s.split_whitespace().collect()");
    let t = touched(Lang::Rust, Some(RS), &new).unwrap();
    assert_eq!(t, Touched { symbols: vec!["split_words".into()], outside: false });
    let new = RS.replace("&self.x }", "&(self.x) }").replace("use crate::types::ToolDef;", "use crate::types::{ToolDef, Block};");
    let t = touched(Lang::Rust, Some(RS), &new).unwrap();
    assert_eq!(t, Touched { symbols: vec!["Foo::get".into()], outside: false }, "imports do not count; the impl is not listed");
    let new = format!("{RS}\nstatic X: u8 = 1;\n");
    assert_eq!(touched(Lang::Rust, Some(RS), &new).unwrap().symbols, ["X"]);
    let new = RS.replace("const N", "// note\nconst N");
    assert!(touched(Lang::Rust, Some(RS), &new).unwrap().outside, "a plain comment outside every symbol changed");
    assert!(touched(Lang::Rust, None, RS).unwrap().outside, "a new file is not targeted");
}

const PY: &str = "import os
from pkg.mod import (a, b)

LIMIT = 3
lower = 1

@decorator
def top(x: int) -> int:
    return x

class Shape(Base):
    \"\"\"A shape.\"\"\"

    def area(self) -> float:
        return 0.0

    @property
    def name(self):
        return 'shape'

def test_top():
    assert top(1) == 1
";

#[test]
fn python_outline_read_edit_guard() {
    let o = ol(Lang::Python, PY);
    assert_eq!(paths(&o), ["LIMIT", "top", "Shape", "Shape.area", "Shape.name", "test_top"]);
    assert_eq!(o.imports, ["os", "pkg.mod import (a, b)"]);
    let top = find(&o, "top", None).unwrap();
    assert_eq!((top.line_a, top.line_b, top.sig.as_str()), (7, 9, "def top(x: int) -> int"));
    assert!(src_of(PY, top).starts_with("@decorator\ndef top"), "decorators are part of it");
    assert!(find(&o, "test_top", None).unwrap().test);
    let name = find(&o, "Shape.name", None).unwrap();
    assert_eq!((name.line_a, name.line_b), (17, 19));
    let new = apply(PY, name, Action::Replace, "@property\ndef name(self):\n    return 'circle'");
    assert!(new.contains("\n    @property\n    def name(self):\n        return 'circle'\n"), "{new}");
    assert!(new_errors(Lang::Python, Some(PY), &new).is_empty());
    let after = apply(PY, find(&o, "Shape.area", None).unwrap(), Action::After, "def perimeter(self):\n    return 1.0\n");
    assert!(after.contains("return 0.0\n\n    def perimeter(self):\n        return 1.0\n\n    @property"), "{after}");
    let broken = PY.replace("def area(self) -> float:", "def area(self -> float:");
    let e = new_errors(Lang::Python, Some(PY), &broken);
    assert!(!e.is_empty(), "missing paren is new");
    assert_eq!(e[0].line, 14);
    let t = touched(Lang::Python, Some(PY), &new).unwrap();
    assert_eq!(t, Touched { symbols: vec!["Shape.name".into()], outside: false });
}

const SWIFT: &str = "import SwiftUI
import Foundation

let maxItems = 10

/// The main view.
@MainActor
struct ContentView: View {
    @State var count = 0

    var body: some View {
        Text(\"\\(count)\")
    }

    func bump() {
        count += 1
    }
}

protocol Shape {
    func area() -> Double
}

extension ContentView {
    func reset() { count = 0 }
}

enum Mode { case a, b }

func helper(_ x: Int) -> Int {
    x * 2
}
";

#[test]
fn swift_outline_read_edit_guard() {
    let o = ol(Lang::Swift, SWIFT);
    assert_eq!(paths(&o), ["maxItems", "ContentView", "ContentView.body", "ContentView.bump", "Shape", "Shape.area", "ContentView", "ContentView.reset", "Mode", "helper"]);
    assert_eq!(o.imports, ["SwiftUI", "Foundation"]);
    let cv = find(&o, "ContentView", None).unwrap();
    assert_eq!((cv.kind, cv.line_a, cv.line_b), ("struct", 6, 18), "doc comment included; the extension is not the type");
    assert_eq!(find(&o, "ContentView.body", None).unwrap().sig, "var body: some View");
    let bump = find(&o, "bump", None).unwrap();
    let new = apply(SWIFT, bump, Action::Replace, "func bump() {\n    count += 2\n}");
    assert!(new.contains("\n    func bump() {\n        count += 2\n    }\n"), "{new}");
    assert!(new_errors(Lang::Swift, Some(SWIFT), &new).is_empty());
    let broken = SWIFT.replace("x * 2\n}", "x * (2\n}");
    assert!(!new_errors(Lang::Swift, Some(SWIFT), &broken).is_empty());
    assert_eq!(find(&o, "reset", None).unwrap().path, "ContentView.reset");
}

const TS: &str = "import { a, b } from './x';
import React from 'react';

export const LIMIT = 3;

/** Adds. */
export function add(x: number, y: number): number {
  return x + y;
}

export class Store extends Base {
  private n = 0;
  inc(by: number): void {
    this.n += by;
  }
  get value(): number { return this.n; }
}

interface Props { name: string }
type Id = string;
enum Color { Red, Green }

const double = (x: number) => x * 2;

namespace Util {
  export function clamp(x: number): number { return x; }
}
";

#[test]
fn typescript_outline_read_edit_guard() {
    let o = ol(Lang::TypeScript, TS);
    assert_eq!(paths(&o), ["LIMIT", "add", "Store", "Store.inc", "Store.value", "Props", "Id", "Color", "double", "Util", "Util.clamp"]);
    assert_eq!(o.imports, ["./x { a, b }", "react React"]);
    let add = find(&o, "add", None).unwrap();
    assert_eq!((add.line_a, add.line_b, add.sig.as_str()), (6, 9, "export function add(x: number, y: number): number"));
    let inc = find(&o, "Store.inc", None).unwrap();
    let new = apply(TS, inc, Action::Replace, "inc(by: number): void {\n  this.n += by * 2;\n}");
    assert!(new.contains("\n  inc(by: number): void {\n    this.n += by * 2;\n  }\n"), "{new}");
    assert!(new_errors(Lang::TypeScript, Some(TS), &new).is_empty());
    let broken = TS.replace("return x + y;", "return x + ;");
    assert!(!new_errors(Lang::TypeScript, Some(TS), &broken).is_empty());
    // TSX parses JSX
    let tsx = "export function App(): JSX.Element {\n  return <div className=\"a\">{1}</div>;\n}\n";
    let o = ol(Lang::Tsx, tsx);
    assert_eq!(paths(&o), ["App"]);
    assert!(new_errors(Lang::Tsx, None, tsx).is_empty());
    assert!(!new_errors(Lang::Tsx, None, &tsx.replace("</div>", "</dv")).is_empty());
}

#[test]
fn javascript_outline_read_edit_guard() {
    let js = "const fs = require('fs');\nimport x from './x.js';\n\nexport default function main() {\n  return 1;\n}\n\nclass A {\n  run() { return 2; }\n}\n\nexport const handler = async (e) => {\n  return e;\n};\n";
    let o = ol(Lang::JavaScript, js);
    assert_eq!(paths(&o), ["fs", "main", "A", "A.run", "handler"]);
    let run = find(&o, "run", None).unwrap();
    let new = apply(js, run, Action::Delete, "");
    assert!(new.contains("class A {\n}"), "{new}");
    assert!(new_errors(Lang::JavaScript, Some(js), &new).is_empty());
    assert!(!new_errors(Lang::JavaScript, Some(js), &js.replace("return 1;", "return 1;;}")).is_empty());
}

#[test]
fn sql_outline_read_edit_guard() {
    let sql = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);\n\nCREATE INDEX users_name ON users (name);\n\nCREATE VIEW active AS SELECT * FROM users;\n";
    let o = ol(Lang::Sql, sql);
    assert_eq!(paths(&o), ["users", "users_name", "active"]);
    assert_eq!(o.syms.iter().map(|y| y.kind).collect::<Vec<_>>(), ["table", "index", "view"]);
    let t = find(&o, "users", None).unwrap();
    assert!(src_of(sql, t).ends_with(");"), "the statement's ; is part of it: {}", src_of(sql, t));
    let new = apply(sql, t, Action::Replace, "CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);");
    assert!(new.starts_with("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, age INT);\n\nCREATE INDEX"), "{new}");
    assert!(new_errors(Lang::Sql, Some(sql), &new).is_empty());
    assert!(!new_errors(Lang::Sql, Some(sql), &sql.replace("name TEXT);", "name TEXT;")).is_empty());
    let del = apply(sql, find(&o, "users_name", None).unwrap(), Action::Delete, "");
    assert_eq!(del, "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);\n\nCREATE VIEW active AS SELECT * FROM users;\n");
}

#[test]
fn bash_outline_read_edit_guard() {
    let sh = "#!/bin/bash\nset -e\nOUT=build\n\nbuild() {\n  mkdir -p \"$OUT\"\n}\n\nfunction clean {\n  rm -rf \"$OUT\"\n}\n\nbuild\n";
    let o = ol(Lang::Bash, sh);
    assert_eq!(paths(&o), ["OUT", "build", "clean"]);
    let b = find(&o, "build", None).unwrap();
    assert_eq!((b.line_a, b.line_b), (5, 7));
    let new = apply(sh, b, Action::Replace, "build() {\n  mkdir -p \"$OUT/bin\"\n}");
    assert!(new_errors(Lang::Bash, Some(sh), &new).is_empty());
    assert!(new.contains("mkdir -p \"$OUT/bin\""));
    assert!(!new_errors(Lang::Bash, Some(sh), &sh.replace("rm -rf \"$OUT\"\n}", "rm -rf \"$OUT\n}")).is_empty());
}

#[test]
fn languages_by_extension_and_size_cap() {
    for (f, l) in [("a.rs", Lang::Rust), ("a.py", Lang::Python), ("A.swift", Lang::Swift), ("a.ts", Lang::TypeScript),
        ("a.tsx", Lang::Tsx), ("a.jsx", Lang::JavaScript), ("a.mjs", Lang::JavaScript), ("q.sql", Lang::Sql), ("b.sh", Lang::Bash)] {
        assert_eq!(Lang::of(Path::new(f)), Some(l), "{f}");
    }
    for f in ["a.md", "a.zsh", "Makefile", "a.c"] { assert_eq!(Lang::of(Path::new(f)), None, "{f}"); }
    let big = "fn a() {}\n".repeat(MAX_BYTES / 10 + 1);
    assert!(outline(Lang::Rust, &big).is_none(), "above MAX_BYTES nothing is parsed");
    assert!(new_errors(Lang::Rust, None, &format!("{big}fn (")).is_empty(), "and the guard does not block it");
}

#[test]
fn reindent_keeps_relative_indentation() {
    assert_eq!(reindent("    fn a() {\n        b();\n    }\n", "  ", true), "  fn a() {\n      b();\n  }");
    assert_eq!(reindent("fn a() {\n\n    b();\n}", "\t", true), "\tfn a() {\n\n\t    b();\n\t}");
}

/// Two Python statements joined on one line leave only a hidden `MISSING _newline` node; the guard must
/// still see it (it let the file through before)
#[test]
fn python_guard_catches_joined_statements() {
    let old = "def t():\n    skus = sorted(x)\n    assert skus == ['A']\n    skus = sorted(x)\n";
    let new = "def t():\n    skus = sorted(x)\n    assert skus == ['A']    skus = sorted(x)\n";
    let e = new_errors(Lang::Python, Some(old), new);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!((e[0].line, e[0].col), (3, 29));
    assert!(e[0].what.contains("missing line break before `skus = sorted(x)`"), "{e:?}");
    assert!(guard(Path::new("t.py"), "t.py", Some(old), new).unwrap_err().starts_with("refused:"));
    // the same break already in the file does not block a later edit
    assert!(new_errors(Lang::Python, Some(new), &format!("{new}\n\ndef u():\n    pass\n")).is_empty());
}
