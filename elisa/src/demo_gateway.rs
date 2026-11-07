//! A scripted stand-in for `streamdense gateway` (no model): `elisa --serve-jsonl --demo-states` starts it on a free
//! 127.0.0.1 port, so the native app can be driven and screenshotted through the REAL turn loop (real tools, real
//! diffs, real approvals) without loading a model. Anthropic Messages SSE, the same shape the gateway streams.
//!
//! The reply depends only on the request (stateless): the last user text decides the scenario and the number of
//! assistant messages after it decides the step.
//! - "fix …" (or "split"): read → grep → edit (crates/core/src/split.rs) → `sh scripts/test.sh` (asks) → summary
//! - "… test …": `sh scripts/test.sh` → summary
//! - "outline …" / "… symbol …": outline → read_symbol → a broken edit_symbol (refused by the parse guard) → the fixed
//!   edit_symbol → summary; the checks then pass
//! - "… check …" / "break …": an edit that parses but does not compile → cargo check fails → fix round → passes
//! - "slow …": a long reply (to try Esc)
//!
//! The demo project is a real (dependency-free) Rust crate (only the demo: Elisa works in any language), so
//! check-before-done runs `cargo check`, `cargo clippy` and the crate's `split_words` test on it, and a normal demo
//! ends green.
//! - anything else: a short markdown answer with a code block
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

const SPLIT_RS: &str = "/// Splits `input` into words at whitespace and ASCII punctuation.
pub fn split_words(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in input.bytes().enumerate() {
        if is_boundary(b as char) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + 1;
        }
    }
    if start < input.len() {
        out.push(&input[start..]);
    }
    out
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_words_multibyte() {
        assert_eq!(split_words(\"voilà tout\"), [\"voilà\", \"tout\"]);
    }
}
";
/// `split_words` after the fix, as one symbol (the `symbols` scenario sends it through edit_symbol)
const FIXED_FN: &str = "/// Splits `input` into words at whitespace and ASCII punctuation.
pub fn split_words(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in input.char_indices() {
        if is_boundary(c) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + c.len_utf8();
        }
    }
    if start < input.len() {
        out.push(&input[start..]);
    }
    out
}";
/// the `check` scenario's first edit: parses, but does not compile (usize + u8)
const NEW_BROKEN: &str = "    for (i, c) in input.char_indices() {
        if is_boundary(c) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + c.len_utf8() as u8;";
const OLD: &str = "    for (i, b) in input.bytes().enumerate() {
        if is_boundary(b as char) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + 1;";
const NEW: &str = "    for (i, c) in input.char_indices() {
        if is_boundary(c) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + c.len_utf8();";
const TEST_SH: &str = "#!/bin/sh
# demo test runner (no compiler needed): passes once split.rs walks char_indices()
echo 'running 51 tests'
if grep -q 'char_indices' crates/core/src/split.rs; then
  echo 'test split::multibyte ... ok'
  echo 'test result: ok. 51 passed; 0 failed; finished in 0.42s'
  exit 0
fi
echo \"test split::multibyte ... FAILED\"
echo \"byte index 3 is not a char boundary; it is inside 'é'\"
echo 'test result: FAILED. 50 passed; 1 failed'
exit 101
";

/// a fresh demo project in the temp dir: crates/core/src/split.rs (the multi-byte bug), scripts/test.sh, and a
/// project config with memory off (demo turns never reach the real memory store)
pub fn project() -> std::io::Result<PathBuf> { project_in(&std::env::temp_dir().join(format!("elisa-demo-{}", std::process::id()))) }

/// `project` under `root` (`root/ferrous`, made fresh): tests give each run its own
pub fn project_in(root: &std::path::Path) -> std::io::Result<PathBuf> {
    let d = root.join("ferrous");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("crates/core/src"))?;
    std::fs::create_dir_all(d.join("scripts"))?;
    std::fs::create_dir_all(d.join(".elisa"))?;
    std::fs::write(d.join("crates/core/src/split.rs"), SPLIT_RS)?;
    std::fs::write(d.join("scripts/test.sh"), TEST_SH)?;
    std::fs::write(d.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/core\"]\nresolver = \"2\"\n")?;
    // a real crate (no dependencies, builds offline): check-before-done runs cargo check / clippy / test on it
    std::fs::write(d.join("crates/core/Cargo.toml"), "[package]\nname = \"ferrous-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")?;
    std::fs::write(d.join("crates/core/src/lib.rs"), "//! ferrous core: text utilities.\npub mod split;\n")?;
    std::fs::write(d.join(".elisa/config.toml"), "memory = false\n")?;
    d.canonicalize()
}

/// starts the scripted gateway on a free local port; returns its base URL
pub fn start() -> std::io::Result<String> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let port = l.local_addr()?.port();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            std::thread::spawn(move || { let _ = handle(c); });
        }
    });
    Ok(format!("http://127.0.0.1:{port}"))
}

fn handle(mut c: TcpStream) -> std::io::Result<()> {
    c.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(c.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    let mut it = first.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let mut len = 0usize;
    loop {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 { break; }
        let l = l.trim_end();
        if l.is_empty() { break; }
        if let Some((k, v)) = l.split_once(':') { if k.trim().eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0); } }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let json = |c: &mut TcpStream, code: u16, b: String| write!(c, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len());
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => json(&mut c, 200, r#"{"status":"ok","info":{"max_ctx":8192,"model":"demo"}}"#.into())?,
        ("POST", "/v1/messages/count_tokens") => json(&mut c, 200, json!({"input_tokens": tokens(&body)}).to_string())?,
        ("POST", "/v1/messages") => {
            let (events, pace) = reply(&body);
            let total: usize = events.iter().map(String::len).sum();
            write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {total}\r\nconnection: close\r\n\r\n")?;
            c.flush()?;
            for (i, e) in events.iter().enumerate() {
                // first token after ~0.45 s (a prefix hit on the real gateway), then ~22 tok/s
                std::thread::sleep(Duration::from_millis(if i == 2 { 450 } else { pace }));
                c.write_all(e.as_bytes())?;
                c.flush()?;
            }
        }
        _ => json(&mut c, 404, r#"{"type":"error","error":{"type":"not_found_error","message":"no route (demo gateway)"}}"#.into())?,
    }
    c.flush()
}

fn tokens(body: &Value) -> usize { (body.to_string().len() as f64 / 3.3).ceil() as usize }

enum Part { Text(String), Tool(&'static str, Value) }

/// the scenario and step for this request
fn script(body: &Value) -> Vec<Part> {
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    // Elisa's own notes (a failed check, a correction) start with `[elisa] `: they continue the scenario, they are
    // not a new request
    let is_text = |m: &Value| m["role"] == "user" && m["content"].as_array().is_some_and(|c| c.iter().any(|b| b["type"] == "text"
        && !b["text"].as_str().unwrap_or("").starts_with(crate::context::NUDGE_PREFIX)));
    let at = msgs.iter().rposition(is_text).unwrap_or(0);
    let user = msgs.get(at).and_then(|m| m["content"].as_array()).and_then(|c| c.iter().rev().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str()).unwrap_or("").to_lowercase();
    // the first message carries Elisa's context header: the user's words are the last paragraph
    let user = user.rsplit("\n\n").next().unwrap_or("").to_string();
    let step = msgs[at..].iter().filter(|m| m["role"] == "assistant").count();
    let last_result = msgs.last().and_then(|m| m["content"].as_array()).and_then(|c| c.iter().find(|b| b["type"] == "tool_result"))
        .map(|b| b["content"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let t = |s: &str| Part::Text(s.to_string());
    let test = || Part::Tool("bash", json!({"command": "sh scripts/test.sh"}));
    let summary = |r: &str| -> Part {
        if r.starts_with("denied") { t("Okay, I did not run the tests. The fix is in place; run `sh scripts/test.sh` when you want to check it.") }
        else if r.contains("test result: ok") { t("Fixed: `split_words` now walks `char_indices()` and advances by `c.len_utf8()`, so multi-byte input like `é` splits cleanly. All 51 tests pass.") }
        else { t("One test still fails: `split::multibyte` slices inside `é`. `split_words` walks `bytes()`; say **fix the split test** and I will switch it to `char_indices()`.") }
    };
    let tool = |n: &'static str, v: Value| Part::Tool(n, v);
    const SPLIT: &str = "crates/core/src/split.rs";
    if user.contains("outline") || user.contains("symbol") {
        // outline → read_symbol → a broken edit_symbol (the parse guard refuses it) → the fixed one → summary
        let broken = FIXED_FN.trim_end_matches('}');
        return match step {
            0 => vec![t("Outlining the crate's splitter first."), tool("outline", json!({"path": SPLIT}))],
            1 => vec![tool("read_symbol", json!({"path": SPLIT, "name": "split_words"}))],
            2 => vec![t("It walks `bytes()`. Replacing the whole function by name:"),
                tool("edit_symbol", json!({"path": SPLIT, "name": "split_words", "source": broken}))],
            3 if last_result.starts_with("refused") => vec![t("That dropped the closing brace; sending the whole function again."),
                tool("edit_symbol", json!({"path": SPLIT, "name": "split_words", "source": FIXED_FN}))],
            _ => vec![t("Replaced `split_words` by name: it walks `char_indices()` now. The broken first try was refused by the parse guard, so the file was never left half-edited.")],
        };
    }
    if user.contains("check") || user.contains("break") {
        // an edit that parses but does not compile → the check fails → the fix note → the fix → the check passes
        return match step {
            0 => vec![t("Switching the loop to `char_indices()`."), tool("edit", json!({"path": SPLIT, "old": OLD, "new": NEW_BROKEN}))],
            1 => vec![t("Done.")],
            2 => vec![t("`len_utf8()` is already a `usize`; dropping the cast."),
                tool("edit", json!({"path": SPLIT, "old": "c.len_utf8() as u8", "new": "c.len_utf8()"}))],
            _ => vec![t("Fixed: the cast is gone and the crate checks clean; `split_words` walks `char_indices()`.")],
        };
    }
    if user.contains("fix") || user.contains("split") {
        return match step {
            0 => vec![t("Reading the splitter and its callers first."), Part::Tool("read", json!({"path": "crates/core/src/split.rs"}))],
            1 => vec![Part::Tool("grep", json!({"pattern": "split_words"}))],
            2 => vec![t("`split_words` walks `bytes()`, so it slices through multi-byte characters like `é`. Switching to `char_indices()` and advancing by the character's UTF-8 length fixes it:"),
                Part::Tool("edit", json!({"path": "crates/core/src/split.rs", "old": OLD, "new": NEW}))],
            3 => vec![t("Running the tests."), test()],
            _ => vec![summary(&last_result)],
        };
    }
    if user.contains("test") {
        return match step { 0 => vec![t("Running the test suite."), test()], _ => vec![summary(&last_result)] };
    }
    if user.contains("slow") {
        let mut s = String::from("This is a long scripted reply so you can try **Esc**. ");
        for i in 1..=60 { s += &format!("Sentence {i} of a reply that keeps streaming while you decide. "); }
        return vec![t(&s)];
    }
    vec![t("This is Elisa's **scripted demo gateway**: no model is loaded, so replies are canned. Try:\n\n- `fix the split test`: reads, greps, edits `split.rs` (see the edits pane) and asks before running the tests\n- `run the tests`\n- `slow`: a long reply, press Esc to interrupt\n\nThe demo project happens to be a small Rust crate; in your own project Elisa works in the project's language.\n\n```rust\nfor (i, c) in input.char_indices() {\n    if is_boundary(c) { /* … */ }\n}\n```")]
}

fn ev(kind: &str, data: Value) -> String { format!("event: {kind}\ndata: {data}\n\n") }

/// the SSE events of one reply and the pause between them (ms)
fn reply(body: &Value) -> (Vec<String>, u64) {
    let parts = script(body);
    let input = tokens(body);
    let cached = input * 9 / 10;
    let mut e = vec![
        ev("message_start", json!({"type":"message_start","message":{"id": format!("msg_demo_{}", input),"type":"message","role":"assistant",
            "content":[],"model":"demo","stop_reason":null,"usage":{"input_tokens": input - cached,"output_tokens":1,"cache_read_input_tokens": cached}}})),
        ev("ping", json!({"type":"ping"})),
        // the streamdense gateway's phase hints (Shade: READING the context, then THINKING)
        ev("ping", json!({"type":"ping","phase":"prefill"})),
        ev("ping", json!({"type":"ping","phase":"decode"})),
    ];
    let (mut out, mut tools) = (0usize, false);
    for (i, p) in parts.iter().enumerate() {
        match p {
            Part::Text(s) => {
                e.push(ev("content_block_start", json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}})));
                for w in s.split_inclusive(' ') {
                    out += 1;
                    e.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":w}})));
                }
                e.push(ev("content_block_stop", json!({"type":"content_block_stop","index":i})));
            }
            Part::Tool(name, input) => {
                tools = true;
                e.push(ev("content_block_start", json!({"type":"content_block_start","index":i,
                    "content_block":{"type":"tool_use","id": format!("toolu_demo_{i}"),"name":name,"input":{}}})));
                let s = input.to_string();
                let cut: Vec<usize> = s.char_indices().map(|(k, _)| k).collect();
                let (a, b) = (cut[cut.len() / 3], cut[2 * cut.len() / 3]);
                for piece in [&s[..a], &s[a..b], &s[b..]] {
                    out += piece.len() / 4;
                    e.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":piece}})));
                }
                e.push(ev("content_block_stop", json!({"type":"content_block_stop","index":i})));
            }
        }
    }
    e.push(ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason": if tools { "tool_use" } else { "end_turn" },"stop_sequence":null},"usage":{"output_tokens":out}})));
    e.push(ev("message_stop", json!({"type":"message_stop"})));
    (e, 45)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(msgs: Value) -> Value { json!({"messages": msgs}) }

    #[test]
    fn script_steps_follow_the_request() {
        let u = |t: &str| json!({"role":"user","content":[{"type":"text","text":t}]});
        let a = json!({"role":"assistant","content":[{"type":"text","text":"x"}]});
        let r = |t: &str| json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":t}]});
        let names = |p: Vec<Part>| p.iter().map(|p| match p { Part::Text(_) => "text".to_string(), Part::Tool(n, _) => n.to_string() }).collect::<Vec<_>>();
        assert_eq!(names(script(&req(json!([u("ctx header\n\nfix the split test")])))), ["text", "read"]);
        assert_eq!(names(script(&req(json!([u("fix it"), a, r("1\tx")])))), ["grep"]);
        let a2 = || json!({"role":"assistant","content":[]});
        assert_eq!(names(script(&req(json!([u("fix it"), a2(), r(""), a2(), r(""), a2(), r(""), a2(), r("test result: ok. 51 passed")])))), ["text"]);
        assert_eq!(names(script(&req(json!([u("hello")])))), ["text"]);
        assert_eq!(names(script(&req(json!([u("run the tests")])))), ["text", "bash"]);
    }

    #[test]
    fn symbol_and_check_scenarios() {
        use crate::ast::{self, Lang};
        let u = |t: &str| json!({"role":"user","content":[{"type":"text","text":t}]});
        let a = || json!({"role":"assistant","content":[]});
        let r = |t: &str| json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":t}]});
        let names = |p: Vec<Part>| p.iter().map(|p| match p { Part::Text(_) => "text".to_string(), Part::Tool(n, _) => n.to_string() }).collect::<Vec<_>>();
        assert_eq!(names(script(&req(json!([u("edit split_words by symbol")])))), ["text", "outline"]);
        assert_eq!(names(script(&req(json!([u("by symbol"), a(), r(""), a(), r(""), a(), r("refused: x")])))), ["text", "edit_symbol"]);
        // a check note continues the scenario (it is not a new request)
        let note = u("[elisa] check failed after your edits (fix round 1 of 2): `cargo check` …");
        assert_eq!(names(script(&req(json!([u("make the check fail"), a(), r("edited"), a(), note])))), ["text", "edit"]);
        // the fixed function by name gives the same file as the scripted fix; the broken one is refused, the
        // check scenario's first edit parses (it fails only in cargo check)
        let o = ast::outline(Lang::Rust, SPLIT_RS).unwrap();
        let y = ast::find(&o, "split_words", None).unwrap();
        assert_eq!(ast::apply(SPLIT_RS, y, ast::Action::Replace, FIXED_FN), SPLIT_RS.replace(OLD, NEW));
        let broken = ast::apply(SPLIT_RS, y, ast::Action::Replace, FIXED_FN.trim_end_matches('}'));
        assert!(!ast::new_errors(Lang::Rust, Some(SPLIT_RS), &broken).is_empty());
        assert!(ast::new_errors(Lang::Rust, Some(SPLIT_RS), &SPLIT_RS.replace(OLD, NEW_BROKEN)).is_empty());
    }

    #[test]
    fn the_scripted_edit_applies_once_to_the_demo_file() {
        assert_eq!(SPLIT_RS.matches(OLD).count(), 1);
        let fixed = SPLIT_RS.replace(OLD, NEW);
        assert!(fixed.contains("char_indices") && !fixed.contains("bytes()"));
    }
}
