use elisa::tools::{self, ToolCtx};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("elisa-t1-tools-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
    fn ctx(&self) -> ToolCtx { ToolCtx { cwd: self.0.clone(), max_chars: 100_000, bash_timeout_s: 10, ..ToolCtx::default() } }
    fn put(&self, rel: &str, content: &[u8]) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}
impl Drop for Tmp {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn run(name: &str, input: Value, ctx: &ToolCtx) -> tools::ToolOutput { tools::run(name, &input, ctx) }

#[test]
fn read_numbers_and_offset_limit() {
    let t = Tmp::new();
    let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    t.put("a.txt", body.as_bytes());
    let ctx = t.ctx();
    let full = run("read", json!({"path": "a.txt"}), &ctx);
    assert!(!full.is_error);
    assert!(full.content.starts_with("1\tline1\n2\tline2\n"));
    assert!(full.content.contains("10\tline10"));
    assert!(!full.content.contains("[lines"), "full read must not say partial: {}", full.content);

    let part = run("read", json!({"path": "a.txt", "offset": 3, "limit": 2}), &ctx);
    assert!(!part.is_error);
    assert_eq!(part.content, "3\tline3\n4\tline4\n[lines 3-4 of 10] next: offset=5");

    let tailr = run("read", json!({"path": "a.txt", "offset": 9, "limit": 50}), &ctx);
    assert_eq!(tailr.content, "9\tline9\n10\tline10\n[lines 9-10 of 10]");

    let abs = run("read", json!({"path": t.0.join("a.txt").to_str().unwrap(), "limit": 1}), &ctx);
    assert_eq!(abs.content, "1\tline1\n[lines 1-1 of 10] next: offset=2");

    let past = run("read", json!({"path": "a.txt", "offset": 99}), &ctx);
    assert!(past.is_error);
    let missing = run("read", json!({"path": "nope.txt"}), &ctx);
    assert!(missing.is_error);
    let dir = run("read", json!({"path": "."}), &ctx);
    assert!(dir.is_error);
}

#[test]
fn read_default_limit_400() {
    let t = Tmp::new();
    let body: String = (1..=500).map(|i| format!("{i}\n")).collect();
    t.put("big.txt", body.as_bytes());
    let o = run("read", json!({"path": "big.txt"}), &t.ctx());
    assert!(o.content.contains("400\t400\n"));
    assert!(!o.content.contains("401\t401"));
    assert!(o.content.ends_with("[lines 1-400 of 500] next: offset=401"));
}

#[test]
fn read_refuses_binary() {
    let t = Tmp::new();
    t.put("b.bin", &[0x7f, b'E', b'L', b'F', 0, 0, 1, 2, 3]);
    let o = run("read", json!({"path": "b.bin"}), &t.ctx());
    assert!(o.is_error);
    assert!(o.content.contains("binary"), "{}", o.content);
}

#[test]
fn write_creates_dirs() {
    let t = Tmp::new();
    let ctx = t.ctx();
    let o = run("write", json!({"path": "x/y/z.rs", "content": "fn a() {}\nfn b() {}\n"}), &ctx);
    assert!(!o.is_error, "{}", o.content);
    assert_eq!(o.content, "wrote 2 lines to x/y/z.rs");
    assert_eq!(std::fs::read_to_string(t.0.join("x/y/z.rs")).unwrap(), "fn a() {}\nfn b() {}\n");
    let o = run("write", json!({"path": "x/y/z.rs", "content": "fn new() {}\n"}), &ctx);
    assert!(!o.is_error);
    assert_eq!(std::fs::read_to_string(t.0.join("x/y/z.rs")).unwrap(), "fn new() {}\n");
    // the parse guard: a write that breaks a code file is refused (the file keeps its text)
    let o = run("write", json!({"path": "x/y/z.rs", "content": "new"}), &ctx);
    assert!(o.is_error && o.content.starts_with("refused:"), "{}", o.content);
    assert_eq!(std::fs::read_to_string(t.0.join("x/y/z.rs")).unwrap(), "fn new() {}\n");
}

#[test]
fn edit_unique_and_all() {
    let t = Tmp::new();
    let ctx = t.ctx();
    t.put("e.txt", b"foo bar foo baz foo\nunique\n");
    let o = run("edit", json!({"path": "e.txt", "old": "foo", "new": "X"}), &ctx);
    assert!(o.is_error);
    assert!(o.content.contains('3'), "count missing: {}", o.content);
    assert_eq!(std::fs::read_to_string(t.0.join("e.txt")).unwrap(), "foo bar foo baz foo\nunique\n");

    let o = run("edit", json!({"path": "e.txt", "old": "unique", "new": "one"}), &ctx);
    assert_eq!(o.content, "edited e.txt (1 replacement)");
    let o = run("edit", json!({"path": "e.txt", "old": "foo", "new": "X", "all": true}), &ctx);
    assert!(!o.is_error);
    assert_eq!(o.content, "edited e.txt (3 replacements)");
    assert_eq!(std::fs::read_to_string(t.0.join("e.txt")).unwrap(), "X bar X baz X\none\n");

    let o = run("edit", json!({"path": "e.txt", "old": "absent", "new": "y"}), &ctx);
    assert!(o.is_error);
    assert!(o.content.contains("not found"));
    let o = run("edit", json!({"path": "nofile", "old": "a", "new": "b"}), &ctx);
    assert!(o.is_error);
}

#[test]
fn bash_output_exit_stderr() {
    let t = Tmp::new();
    let ctx = t.ctx();
    let o = run("bash", json!({"command": "echo hi; pwd"}), &ctx);
    assert!(!o.is_error);
    assert!(o.content.starts_with("hi\n"), "{}", o.content);
    assert!(o.content.contains(t.0.to_str().unwrap()), "not run in cwd: {}", o.content);
    assert!(o.content.ends_with("[exit 0]"));

    let o = run("bash", json!({"command": "echo out; echo err 1>&2; exit 3"}), &ctx);
    assert!(o.content.contains("out\n") && o.content.contains("err\n"), "{}", o.content);
    assert!(o.content.ends_with("[exit 3]"), "{}", o.content);

    let o = run("bash", json!({"command": "true"}), &ctx);
    assert_eq!(o.content, "[exit 0]");
}

#[test]
fn bash_timeout_kills_quickly() {
    let t = Tmp::new();
    let ctx = ToolCtx { bash_timeout_s: 1, ..t.ctx() };
    let start = Instant::now();
    let o = run("bash", json!({"command": "echo before; sleep 30; echo after"}), &ctx);
    let el = start.elapsed();
    println!("timeout run took {el:?}");
    assert!(el.as_secs_f64() < 5.0, "took {el:?}");
    assert!(o.is_error);
    assert!(o.content.contains("before"), "{}", o.content);
    assert!(!o.content.contains("\nafter\n") && !o.content.starts_with("after"), "{}", o.content);
    assert!(o.content.contains("timeout"), "{}", o.content);

    // background grandchild holding the pipe must not hang the call either
    let start = Instant::now();
    let o = run("bash", json!({"command": "sleep 30 & echo bg"}), &ctx);
    let el = start.elapsed();
    println!("bg run took {el:?}");
    assert!(el.as_secs_f64() < 5.0, "took {el:?}");
    assert!(o.content.contains("bg"), "{}", o.content);
}

#[test]
fn glob_recursive_and_skips() {
    let t = Tmp::new();
    for f in ["src/main.rs", "src/a/b.rs", "lib.rs", "readme.md", "target/x.rs", ".git/y.rs", "node_modules/z.rs", ".elisa/w.rs", "sub/target/q.rs"] {
        t.put(f, b"x\n");
    }
    let ctx = t.ctx();
    let o = run("glob", json!({"pattern": "**/*.rs"}), &ctx);
    assert!(!o.is_error, "{}", o.content);
    assert_eq!(o.content, "lib.rs\nsrc/a/b.rs\nsrc/main.rs");
    let o = run("glob", json!({"pattern": "src/**/*.rs"}), &ctx);
    assert_eq!(o.content, "src/a/b.rs\nsrc/main.rs");
    let o = run("glob", json!({"pattern": "*.rs"}), &ctx);
    assert_eq!(o.content, "lib.rs\nsrc/a/b.rs\nsrc/main.rs");
    let o = run("glob", json!({"pattern": "*.rs", "path": "src"}), &ctx);
    assert_eq!(o.content, "src/a/b.rs\nsrc/main.rs");
    let o = run("glob", json!({"pattern": "*.{md,rs}", "path": "."}), &ctx);
    assert!(o.content.contains("readme.md") && o.content.contains("lib.rs"));
    let o = run("glob", json!({"pattern": "*.zzz"}), &ctx);
    assert!(!o.is_error);
    assert_eq!(o.content, "no files found");
}

#[test]
fn glob_caps_at_200() {
    let t = Tmp::new();
    for i in 0..250 { t.put(&format!("f{i:03}.txt"), b""); }
    let o = run("glob", json!({"pattern": "*.txt"}), &t.ctx());
    assert_eq!(o.content.lines().filter(|l| l.ends_with(".txt")).count(), 200);
    assert!(o.content.ends_with("[200 of 250 files shown]"));
}

#[test]
fn grep_with_glob_filter() {
    let t = Tmp::new();
    t.put("src/a.rs", b"fn main() {}\nlet needle = 1;\n");
    t.put("src/b.txt", b"needle in txt\n");
    t.put("target/c.rs", b"needle skipped\n");
    t.put("bin.dat", b"needle\0\0binary");
    let ctx = t.ctx();
    let o = run("grep", json!({"pattern": "needle"}), &ctx);
    assert!(!o.is_error);
    assert_eq!(o.content, "src/a.rs:2: let needle = 1;\nsrc/b.txt:1: needle in txt");
    let o = run("grep", json!({"pattern": "needle", "glob": "*.rs"}), &ctx);
    assert_eq!(o.content, "src/a.rs:2: let needle = 1;");
    let o = run("grep", json!({"pattern": "need?le", "path": "src", "glob": "*.txt"}), &ctx);
    assert_eq!(o.content, "src/b.txt:1: needle in txt");
    let o = run("grep", json!({"pattern": "needle", "path": "src/a.rs"}), &ctx);
    assert_eq!(o.content, "src/a.rs:2: let needle = 1;");
    let o = run("grep", json!({"pattern": "zzz"}), &ctx);
    assert_eq!(o.content, "no matches");
    let o = run("grep", json!({"pattern": "(unclosed"}), &ctx);
    assert!(o.is_error);
}

#[test]
fn grep_caps_at_100() {
    let t = Tmp::new();
    let body: String = (0..150).map(|i| format!("hit {i}\n")).collect();
    t.put("h.txt", body.as_bytes());
    let o = run("grep", json!({"pattern": "hit"}), &t.ctx());
    assert_eq!(o.content.lines().filter(|l| l.starts_with("h.txt:")).count(), 100);
    assert!(o.content.contains("[stopped at 100 matches]"));
}

#[test]
fn output_cap_head_tail_utf8() {
    let t = Tmp::new();
    // multi-byte chars everywhere so a byte-based cut would panic or split a char
    let body: String = (0..400).map(|i| format!("ä€😀 line {i:03}\n")).collect();
    t.put("u.txt", body.as_bytes());
    let ctx = ToolCtx { max_chars: 1000, ..t.ctx() };
    // read stops at a whole line under the cap and says where to go on
    let r = run("read", json!({"path": "u.txt"}), &ctx).content;
    assert!(r.chars().count() <= 1000 && r.contains("\t") && r.ends_with(" next: offset=59"), "{r}");
    let o = run("bash", json!({"command": "cat u.txt"}), &ctx);
    assert!(!o.is_error);
    let c = &o.content;
    assert!(c.starts_with("ä€😀 line 000"), "{}", &c[..40]);
    assert!(c.ends_with("line 399\n[exit 0]"), "tail: {}", &c[c.len() - 60..]);
    let marker = c.lines().find(|l| l.starts_with("[... ") && l.ends_with(" lines cut ...]")).expect("cut marker");
    let parts: Vec<&str> = c.split(&format!("\n{marker}\n")).collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].chars().count(), 600);
    assert_eq!(parts[1].chars().count(), 400);
    let full = tools::run("bash", &json!({"command": "cat u.txt"}), &t.ctx()).content;
    assert!(full.starts_with(parts[0]) && full.ends_with(parts[1]));
    let cut = full.chars().count() - 1000;
    assert!(marker.starts_with(&format!("[... {cut} chars / ")), "{marker} vs {cut}");
    println!("marker: {marker}");
}

#[test]
fn bad_input_is_error_no_panic() {
    let t = Tmp::new();
    let ctx = t.ctx();
    let cases: Vec<(&str, Value)> = vec![
        ("read", json!({})),
        ("read", json!({"path": 5})),
        ("read", json!(null)),
        ("read", json!("string")),
        ("write", json!({"path": "a"})),
        ("edit", json!({"path": "a", "old": "", "new": "x"})),
        ("edit", json!({"path": "a"})),
        ("bash", json!({})),
        ("bash", json!({"command": "   "})),
        ("glob", json!({})),
        ("glob", json!({"pattern": "*", "path": "nope"})),
        ("grep", json!({"pattern": "x", "path": "nope"})),
        ("nosuch", json!({})),
        ("write", json!({"path": "", "content": "x"})),
    ];
    for (name, input) in cases {
        let o = tools::run(name, &input, &ctx);
        assert!(o.is_error, "{name} {input} should error, got {}", o.content);
        assert!(!o.content.is_empty());
    }
    // lenient numeric forms are fine
    t.put("n.txt", b"a\nb\nc\n");
    let o = tools::run("read", &json!({"path": "n.txt", "offset": "2", "limit": -1}), &ctx);
    assert!(!o.is_error, "{}", o.content);
    assert!(o.content.starts_with("2\tb"));
}

#[test]
fn defs_stable_and_small() {
    let a = serde_json::to_string(&tools::defs()).unwrap();
    let b = serde_json::to_string(&tools::defs()).unwrap();
    assert_eq!(a, b);
    let names: Vec<String> = tools::defs().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["read", "write", "edit", "bash", "glob", "grep", "outline", "read_symbol", "edit_symbol"]);
    let plain: Vec<String> = tools::defs_for(false).into_iter().map(|d| d.name).collect();
    assert_eq!(plain, ["read", "write", "edit", "bash", "glob", "grep"], "config ast = false");
    let est = elisa::context::estimate_tokens(&a);
    println!("tool defs JSON: {} bytes, ~{} est tokens", a.len(), est);
    for d in tools::defs() {
        let one = serde_json::to_string(&d).unwrap();
        let e = elisa::context::estimate_tokens(&one);
        println!("  {}: {} bytes, ~{} tokens", d.name, one.len(), e);
        // edit_symbol carries 4 fields (path, name, source, action): its own cap
        let max = if d.name == "edit_symbol" { 100 } else { 80 };
        assert!(e <= max, "{} def too large: {e}", d.name);
        let req = d.input_schema["required"].as_array().unwrap();
        assert!(!req.is_empty());
    }
    assert_eq!(tools::access("read"), tools::Access::ReadOnly);
    assert_eq!(tools::access("glob"), tools::Access::ReadOnly);
    assert_eq!(tools::access("grep"), tools::Access::ReadOnly);
    assert_eq!(tools::access("write"), tools::Access::Write);
    assert_eq!(tools::access("edit"), tools::Access::Write);
    assert_eq!(tools::access("bash"), tools::Access::Exec);
}

#[test]
fn summaries() {
    assert_eq!(tools::summary("read", &json!({"path": "src/main.rs"})), "read src/main.rs");
    assert_eq!(tools::summary("write", &json!({"path": "src/a.rs", "content": "a\nb\n"})), "write src/a.rs (2 lines)");
    assert_eq!(tools::summary("edit", &json!({"path": "src/a.rs"})), "edit src/a.rs");
    assert_eq!(tools::summary("bash", &json!({"command": "cargo\n  build"})), "bash cargo build");
    let long = tools::summary("bash", &json!({"command": "é".repeat(300)}));
    assert!(!long.contains('\n'));
    assert!(long.chars().count() <= "bash ".len() + 101);
    assert_eq!(tools::summary("glob", &json!({"pattern": "**/*.rs"})), "glob **/*.rs");
    assert_eq!(tools::summary("grep", &json!({"pattern": "foo"})), "grep foo");
    assert_eq!(tools::summary("bash", &json!(null)), "bash ");
}

// ---------- check-before-done runner (src/checks.rs); here, not in the lib tests: those set repl::CANCEL ----------

#[test]
fn check_run_counts_trims_and_times_out() {
    let d = std::env::temp_dir();
    let mk = |cmd: &str, t| elisa::checks::Check { name: "c".into(), cmd: cmd.into(), stage: elisa::checks::Stage::Fast, timeout_s: t };
    let o = elisa::checks::run(&mk("echo '   Compiling x'; echo 'a.rs:1:1: error: one'; echo 'a.rs:2:1: error: two'; exit 101", 10), &d, 3000);
    assert_eq!((o.status, o.summary.as_str(), o.code), (elisa::frontend::CheckStatus::Fail, "2 errors", Some(101)));
    assert!(!o.output.contains("Compiling"), "{}", o.output);
    assert_eq!(elisa::checks::run(&mk("true", 10), &d, 3000).status, elisa::frontend::CheckStatus::Pass);
    assert_eq!(elisa::checks::run(&mk("exit 3", 10), &d, 3000).summary, "exit 3");
    // a long log keeps the error lines up front within the cap
    let o = elisa::checks::run(&mk("for i in $(seq 1 2000); do echo noise line $i; done; echo 'z.rs:1:1: error: the one'; exit 1", 10), &d, 600);
    assert!(o.output.starts_with("errors:\nz.rs:1:1: error: the one") && o.output.chars().count() < 900, "{}", o.output);
    let t0 = std::time::Instant::now();
    let o = elisa::checks::run(&mk("sleep 5", 1), &d, 3000);
    assert_eq!((o.status, o.summary.as_str()), (elisa::frontend::CheckStatus::Timeout, "timed out after 1s"));
    assert!(t0.elapsed().as_secs_f64() < 4.5, "the sleep was killed: {:?}", t0.elapsed());
}

#[test]
fn an_edit_that_defines_a_route_twice_gets_a_warning() {
    // partial_run2: the model added `search_items` a second time; the tool result tells it in the same turn
    let t = Tmp::new();
    let src = "from fastapi import FastAPI\n\napp = FastAPI()\n\n\n@app.get(\"/items/search\")\ndef search_items(q: str):\n    return []\n";
    t.put("main.py", src.as_bytes());
    let ctx = t.ctx();
    let ok = run("edit", json!({"path": "main.py", "old": "return []", "new": "return [q]"}), &ctx);
    assert!(!ok.is_error && !ok.content.contains("warning"), "{}", ok.content);
    let add = "    return [q]\n\n\n@app.get(\"/items/search\")\ndef search_items(q: str, limit: int = 20):\n    return []\n";
    let dup = run("edit", json!({"path": "main.py", "old": "    return [q]\n", "new": add}), &ctx);
    assert!(!dup.is_error, "{}", dup.content);
    assert!(dup.content.contains("warning: this edit defined something twice") && dup.content.contains("main.py: `search_items` is defined twice")
        && dup.content.contains("main.py: route GET /items/search is registered twice"), "{}", dup.content);
    // an unrelated later edit does not repeat the old duplicate
    let later = run("edit", json!({"path": "main.py", "old": "app = FastAPI()", "new": "app = FastAPI(title=\"x\")"}), &ctx);
    assert!(!later.content.contains("warning"), "{}", later.content);
}

/// The shape of a cut-off write (2026-11-05 mario.html): code, then a table of rows that are quoted strings of
/// spaces, a stray quote as the last line, no final newline.
fn padded_html() -> String {
    let mut s: String = (1..=98).map(|i| if i % 30 == 0 { "\n".to_string() } else { format!("const code{i} = {i};\n") }).collect();
    for _ in 0..214 { s += &format!("\"{}\",\n", " ".repeat(100)); }
    s.push('"');
    s
}

#[test]
fn read_collapses_padding_in_one_call() {
    let t = Tmp::new();
    t.put("mario.html", padded_html().as_bytes());
    let ctx = t.ctx();
    // past the padding in one read, to the end: nothing left to page through
    let o = run("read", json!({"path": "mario.html"}), &ctx);
    assert!(o.content.contains("98\tconst code98 = 98;\n"), "{}", o.content);
    assert!(o.content.ends_with("99-313\t[215 lines of only spaces or quotes skipped]\n"), "{}", o.content);
    assert!(!o.content.contains("next: offset"), "{}", o.content);
    // a single blank line in the code stays, numbered
    assert!(o.content.contains("30\t\n31\tconst code31"), "{}", o.content);
    // from inside the run: one marker and the footer
    let o = run("read", json!({"path": "mario.html", "offset": 200}), &ctx);
    assert_eq!(o.content, "200-313\t[114 lines of only spaces or quotes skipped]\n[lines 200-313 of 313]");
}

#[test]
fn read_padding_does_not_eat_the_limit() {
    let t = Tmp::new();
    let pad = format!("{}\n", " ".repeat(50)).repeat(500);
    t.put("a.txt", format!("one\n{pad}two\nthree\nfour\n").as_bytes());
    let ctx = t.ctx();
    let o = run("read", json!({"path": "a.txt", "limit": 2}), &ctx);
    assert_eq!(o.content, "1\tone\n2-501\t[500 lines of only spaces or quotes skipped]\n502\ttwo\n[lines 1-502 of 504] next: offset=503");
    // short runs are ordinary lines, and count
    t.put("b.txt", b"a\n\n\nb\n");
    assert_eq!(run("read", json!({"path": "b.txt"}), &ctx).content, "1\ta\n2\t\n3\t\n4\tb\n");
    // code with content is never padding
    t.put("c.txt", b"[\n],\n};\n[\n],\n");
    assert_eq!(run("read", json!({"path": "c.txt"}), &ctx).content, "1\t[\n2\t],\n3\t};\n4\t[\n5\t],\n");
}
