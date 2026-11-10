//! Long-session harness (no model): a scripted 30+ turn plain-REPL session against an in-process fake gateway that
//! computes usage from what it is sent, so folds trigger the way they do on the real 8K gateway. After each turn it
//! checks what the model is SENT: goal / files / decisions / open items (via the first message's notes and
//! `[state]`, or kept history), no leaks from cleared history, a byte-stable
//! system + tools prefix, and token estimates under the limit. Across runs: /compact, /clear, /reset, a brand-new
//! session in the same project, /resume of an older session, and elisa-mem.
//!
//! The fake model is deliberately plain:
//! - agent replies are scripted; some end with a `<state>` card that names only the goal and the next step
//!   (an 80-token card cannot carry everything), some carry no card (so folds must summarise);
//! - summaries are *extractive*: they keep exactly the marker tokens (GOAL-/DEC-/OPEN-/DONE-/STEP-)
//!   and `FILE:` paths found in what Elisa sent. So whatever survives a summary is what Elisa put in front of it.
//!
//! Run: `cargo test --test long_session -- --nocapture` (prints the survival table).
//! Known issues are `#[ignore]`d tests named `known_*`: `cargo test --test long_session -- --ignored`.
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_CTX: usize = 8192;
const COMPACT_SYSTEM: &str = "You write short, concrete session notes for an assistant.";

// ---------- token formula (the same as src/context.rs) ----------

fn tok(s: &str) -> usize { (s.chars().count() * 10).div_ceil(33) }

fn block_tok(b: &Value) -> usize {
    match b["type"].as_str().unwrap_or("") {
        "text" => tok(b["text"].as_str().unwrap_or("")),
        "tool_use" => tok(b["id"].as_str().unwrap_or("")) + tok(b["name"].as_str().unwrap_or("")) + tok(&b["input"].to_string()),
        "tool_result" => tok(b["tool_use_id"].as_str().unwrap_or("")) + tok(b["content"].as_str().unwrap_or("")),
        _ => 0,
    }
}
fn msg_tok(m: &Value) -> usize { 6 + m["content"].as_array().map_or(0, |a| a.iter().map(block_tok).sum()) }
fn fixed_tok(req: &Value) -> usize {
    tok(req["system"].as_str().unwrap_or("")) + req.get("tools").map_or(0, |t| tok(&t.to_string()) + 240)
}
fn req_tok(req: &Value) -> usize { fixed_tok(req) + msgs(req).iter().map(msg_tok).sum::<usize>() }
fn msgs(req: &Value) -> Vec<Value> { req["messages"].as_array().cloned().unwrap_or_default() }

// ---------- fake gateway ----------

#[derive(Clone, Debug)]
enum Step { Tool(&'static str, Value), Text(String) }

#[derive(Clone, Debug, PartialEq)]
enum Kind { Agent, SummaryInContext, SummaryTranscript }

#[derive(Clone, Debug)]
struct Rec { kind: Kind, body: Value, cached: usize, total: usize }

#[derive(Default)]
struct Script { agent: VecDeque<Step>, scale_pct: usize }

struct Fake { port: u16, script: Arc<Mutex<Script>>, log: Arc<Mutex<Vec<Rec>>> }

impl Fake {
    fn start(agent: Vec<Step>) -> Fake { Fake::start_scaled(agent, 100) }
    /// The server's tokenizer counts `scale_pct`% of the chars/token formula (code and JSON tokenize denser than the
    /// estimate), and like the real gateway it answers nothing for a request over MAX_CTX.
    fn start_scaled(agent: Vec<Step>, scale_pct: usize) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let script = Arc::new(Mutex::new(Script { agent: agent.into(), scale_pct }));
        let log: Arc<Mutex<Vec<Rec>>> = Arc::new(Mutex::new(Vec::new()));
        let (s, g) = (script.clone(), log.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (s, g) = (s.clone(), g.clone());
                std::thread::spawn(move || { let _ = handle(c, &s, &g); });
            }
        });
        Fake { port, script, log }
    }
    fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
    fn log(&self) -> Vec<Rec> { self.log.lock().unwrap().clone() }
    fn left(&self) -> usize { self.script.lock().unwrap().agent.len() }
}

fn ev(kind: &str, data: Value) -> String { format!("event: {kind}\ndata: {data}\n\n") }

fn sse(content: &[Step], input: usize, cached: usize) -> String {
    let mut e = vec![ev("message_start", json!({"type":"message_start","message":{"id":"msg_x","type":"message","role":"assistant","content":[],
        "model":"fake","stop_reason":null,"usage":{"input_tokens":input,"output_tokens":1,"cache_read_input_tokens":cached}}}))];
    let mut tool = false;
    for (i, s) in content.iter().enumerate() {
        match s {
            Step::Text(t) => {
                e.push(ev("content_block_start", json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}})));
                e.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":t}})));
            }
            Step::Tool(name, inp) => {
                tool = true;
                e.push(ev("content_block_start", json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":format!("t{}", rand_id()),"name":name,"input":{}}})));
                e.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":inp.to_string()}})));
            }
        }
        e.push(ev("content_block_stop", json!({"type":"content_block_stop","index":i})));
    }
    e.push(ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason": if tool {"tool_use"} else {"end_turn"},"stop_sequence":null},"usage":{"output_tokens":20}})));
    e.push(ev("message_stop", json!({"type":"message_stop"})));
    e.concat()
}

fn rand_id() -> usize { static N: AtomicUsize = AtomicUsize::new(1); N.fetch_add(1, Ordering::SeqCst) }

/// Tokens of the request prefix shared with `prev` (same system + tools, then identical leading messages).
fn shared_prefix(prev: Option<&Value>, req: &Value) -> usize {
    let Some(p) = prev else { return 0 };
    if p["system"] != req["system"] || p.get("tools") != req.get("tools") { return 0; }
    let (a, b) = (msgs(p), msgs(req));
    fixed_tok(req) + a.iter().zip(&b).take_while(|(x, y)| x == y).map(|(x, _)| msg_tok(x)).sum::<usize>()
}

fn handle(mut c: TcpStream, script: &Mutex<Script>, log: &Mutex<Vec<Rec>>) -> std::io::Result<()> {
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
    let json_resp = |c: &mut TcpStream, code: u16, b: &str| write!(c, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len());
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => json_resp(&mut c, 200, &format!(r#"{{"status":"ok","info":{{"max_ctx":{MAX_CTX}}}}}"#))?,
        ("POST", "/v1/messages/count_tokens") => {
            let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
            json_resp(&mut c, 200, &format!(r#"{{"input_tokens":{}}}"#, req_tok(&req)))?
        }
        ("POST", "/v1/messages") => {
            let req: Value = serde_json::from_slice(&body).unwrap();
            let sys = req["system"].as_str().unwrap_or("").to_string();
            let last_text = msgs(&req).last().and_then(|m| m["content"][0]["text"].as_str().map(String::from)).unwrap_or_default();
            let kind = if sys == COMPACT_SYSTEM { Kind::SummaryTranscript }
                else if last_text == elisa::context::compact_instructions() { Kind::SummaryInContext }
                else { Kind::Agent };
            let scale = script.lock().unwrap().scale_pct;
            let total = req_tok(&req) * scale / 100;
            let reply: Option<Vec<Step>> = {
                let mut s = script.lock().unwrap();
                match kind {
                    // as the real gateway does: "[streamdense] Context full …", 0 tokens generated
                    _ if total > MAX_CTX => Some(vec![Step::Text(format!("[streamdense] Context full: this conversation is {total} tokens and the engine limit is {MAX_CTX}"))]),
                    Kind::SummaryTranscript => Some(vec![Step::Text(summarise(&last_text))]),
                    Kind::SummaryInContext => Some(vec![Step::Text(summarise(&render(&req)))]),
                    Kind::Agent => s.agent.pop_front().map(|x| vec![x]),
                }
            };
            let cached = {
                let mut g = log.lock().unwrap();
                let prev = g.iter().rev().find(|x| x.body["system"] == req["system"]).map(|x| x.body.clone());
                let cached = shared_prefix(prev.as_ref(), &req) * scale / 100;
                g.push(Rec { kind: kind.clone(), body: req.clone(), cached, total });
                cached
            };
            match reply {
                None => json_resp(&mut c, 500, r#"{"type":"error","error":{"type":"api_error","message":"script exhausted"}}"#)?,
                Some(steps) => {
                    let s = sse(&steps, total.saturating_sub(cached), cached);
                    write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{s}", s.len())?;
                }
            }
        }
        _ => json_resp(&mut c, 404, r#"{"type":"error","error":{"type":"not_found_error","message":"no route"}}"#)?,
    }
    c.flush()
}

/// The whole request as the model would read it (for the in-context summary).
fn render(req: &Value) -> String {
    let mut out = String::new();
    for m in msgs(req) {
        for b in m["content"].as_array().cloned().unwrap_or_default() {
            match b["type"].as_str().unwrap_or("") {
                "text" => out += &format!("{}: {}\n", m["role"], b["text"].as_str().unwrap_or("")),
                "tool_use" => out += &format!("[tool {} {}]\n", b["name"].as_str().unwrap_or(""), b["input"]["path"].as_str().unwrap_or("")),
                "tool_result" => out += &format!("[result: {}]\n", b["content"].as_str().unwrap_or("")),
                _ => {}
            }
        }
    }
    out
}

fn markers(text: &str) -> BTreeSet<String> {
    let re = Regex::new(r"\b(?:GOAL|DEC|OPEN|DONE|STEP)-[A-Z0-9]+\b").unwrap();
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}
fn edited_paths(text: &str) -> BTreeSet<String> {
    let re = Regex::new(r"\[tool (?:write|edit|edit_symbol) ([\w./-]+)|FILE: ([\w./-]+)").unwrap();
    re.captures_iter(text).filter_map(|c| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string())).collect()
}

/// Extractive notes in prompts/compact.md's headings: only markers and edited files that were in `text`.
fn summarise(text: &str) -> String {
    let m = markers(text);
    let pick = |p: &[&str]| m.iter().filter(|x| p.iter().any(|q| x.starts_with(q))).cloned().collect::<Vec<_>>().join(" ");
    let done: BTreeSet<String> = m.iter().filter_map(|x| x.strip_prefix("DONE-").map(|o| format!("OPEN-{o}"))).collect();
    let open: Vec<String> = m.iter().filter(|x| x.starts_with("OPEN-") && !done.contains(*x)).cloned().collect();
    let files: Vec<String> = edited_paths(text).into_iter().map(|f| format!("FILE: {f}")).collect();
    format!("## Goal\n{}\n\n## Done\n{}\n\n## Files touched\n{}\n\n## Current state / next step\n{}\n\n## Facts to remember\nAlso known as: wc = word count",
        pick(&["GOAL-"]), pick(&["DEC-", "STEP-", "DONE-"]), files.join("\n"), open.join(" "))
}

// ---------- the project and the binary ----------

fn tmpdir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("elisa-long-{}-{}-{}", std::process::id(), tag, N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

const WC0: &str = "import sys\nfrom collections import Counter\n\n\ndef count_words(text):\n    return Counter(text.split())\n\n\ndef main(argv):\n    text = open(argv[1]).read()\n    for w, n in count_words(text).items():\n        print(w, n)\n\n\nif __name__ == \"__main__\":\n    main(sys.argv)\n";

fn project(tag: &str) -> PathBuf {
    let d = tmpdir(tag);
    for p in ["src", "tests", "data", ".elisa"] { std::fs::create_dir_all(d.join(p)).unwrap(); }
    std::fs::write(d.join("src/wc.py"), WC0).unwrap();
    std::fs::write(d.join("src/report.py"), "def header():\n    return \"word count\"\n").unwrap();
    std::fs::write(d.join("tests/test_wc.py"), "from src.wc import count_words\n\n\ndef test_count():\n    assert count_words(\"a a\")[\"a\"] == 2\n").unwrap();
    for i in 1..=4 {
        let big: String = (1..=100).map(|l| format!("big{i} sample line {l:03} with some padding words to fill the window\n")).collect();
        std::fs::write(d.join(format!("data/big{i}.txt")), big).unwrap();
    }
    std::fs::write(d.join(".elisa/checks.toml"), "commands = [\"sh check.sh\"]\n").unwrap();
    std::fs::write(d.join("check.sh"), "if grep -q FIXME src/wc.py; then echo \"src/wc.py:1:1: error: FIXME left in src/wc.py\"; exit 1; fi\nexit 0\n").unwrap();
    d
}

struct Out { code: i32, stderr: String }

fn run(d: &Path, url: &str, stdin: &str) -> Out {
    let home = d.join("_home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
    cmd.args(["--yes", "--model", "test-m", "--base-url", url, "--max-ctx", &MAX_CTX.to_string(), "--cwd"]).arg(d).current_dir(d).env("HOME", &home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, _) in std::env::vars() { if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") { cmd.env_remove(&k); } }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let to = std::thread::spawn(move || { let mut s = String::new(); so.read_to_string(&mut s).ok(); s });
    let te = std::thread::spawn(move || { let mut s = String::new(); se.read_to_string(&mut s).ok(); s });
    let t0 = Instant::now();
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() { break st.code().unwrap_or(-1); }
        if t0.elapsed() > Duration::from_secs(120) { child.kill().ok(); panic!("elisa timed out"); }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = to.join();
    Out { code, stderr: te.join().unwrap() }
}

// ---------- scripted model ----------

fn read(p: &str) -> Step { Step::Tool("read", json!({"path": p})) }
fn write(p: &str, c: &str) -> Step { Step::Tool("write", json!({"path": p, "content": c})) }
fn say(t: &str) -> Step { Step::Text(t.to_string()) }
/// A final reply with a card that names only the goal and the next step.
fn say_card(t: &str, next: &str) -> Step { Step::Text(format!("{t}\n<state>\ngoal: GOAL-G1 word-count CLI\nnext: {next}\n</state>")) }

const WC1: &str = "import sys\nfrom collections import Counter\n\n\ndef count_words(text):\n    return Counter(text.lower().split())\n\n\ndef main(argv):\n    text = open(argv[1]).read()\n    for w, n in count_words(text).items():\n        print(w, n)\n";
const WC2: &str = "import sys\nfrom collections import Counter\n\n\ndef count_words(text):\n    return Counter(text.lower().split())\n\n\ndef top_n(text, n):\n    return count_words(text).most_common(n)\n\n\ndef main(argv):\n    text = open(argv[1]).read()\n    for w, n in count_words(text).items():\n        print(w, n)\n";

/// One user turn: the stdin lines and the agent steps it consumes.
struct Turn { q: String, lines: Vec<String>, steps: Vec<Step>, edits: Vec<&'static str>, adds: Vec<&'static str> }

fn turn(q: usize, prompt: &str, steps: Vec<Step>) -> Turn {
    let q = format!("Q{q:02}");
    Turn { lines: vec![format!("{prompt} {q}")], q, steps, edits: vec![], adds: vec![] }
}
impl Turn {
    fn edits(mut self, f: &[&'static str]) -> Turn { self.edits.extend(f); self }
    /// markers this turn introduces (decisions, open items) — for the survival table
    fn adds(mut self, m: &[&'static str]) -> Turn { self.adds.extend(m); self }
}

fn filler(q: usize, k: usize, card: bool) -> Turn {
    let f = format!("data/big{}.txt", 1 + k % 4);
    let t = turn(q, &format!("what does {f} say around line 050?"), vec![read(&f), if card { say_card(&format!("Line 050 is sample text. Q{q:02}-R"), "keep going") } else { say(&format!("Line 050 is sample text. Q{q:02}-R")) }]);
    t
}

/// Run 1: 31 user turns, edits over two files, a failed check + fix, /compact.
fn run1_turns() -> Vec<Turn> {
    let mut v = vec![
        turn(1, "read src/wc.py. GOAL-G1: a word-count CLI with top-N output and a report table.", vec![read("src/wc.py"),
            say_card("It counts words with Counter. DEC-D1: stdlib only. OPEN-O1: no top-N yet.", "top_n")]).adds(&["GOAL-G1", "DEC-D1", "OPEN-O1"]),
        turn(2, "what is in data/big1.txt?", vec![read("data/big1.txt"), say("Sample lines. Q02-R")]),
        // first half now, the rest next turn (after the fold this turn causes)
        turn(3, "make count_words lowercase words, then add a top_n function to src/wc.py", vec![read("data/big2.txt"), write("src/wc.py", WC1),
            say_card("STEP-S1 done: count_words lowercases. DEC-D2: lowercase all words.", "add top_n")])
            .edits(&["src/wc.py"]).adds(&["DEC-D2"]),
        turn(4, "now add top_n", vec![write("src/wc.py", WC2), say("STEP-S2 done: top_n added. DONE-O1.")]),
        turn(5, "add a format_table function to src/report.py", vec![read("data/big3.txt"), write("src/report.py", "def header():\n    return \"word count\"\n\n\ndef format_table(rows):\n    return \"\\n\".join(f\"{w:<20}{n:>6}\" for w, n in rows)\n"),
            say("DEC-D3: right-align counts. OPEN-O2: wire the table into main.")])
            .edits(&["src/report.py"]).adds(&["DEC-D3", "OPEN-O2"]),
        turn(6, "should count_words be renamed to tally?", vec![say("No: callers use count_words. Q06-R")]),
        turn(7, "what does data/big4.txt contain?", vec![read("data/big4.txt"), read("data/big1.txt"), say("More samples. Q07-R")]),
        turn(8, "how many lines does data/big2.txt have?", vec![read("data/big2.txt"), say_card("100 lines. Q08-R", "wire the table into main")]),
    ];
    for (i, q) in (9..=16).enumerate() { v.push(filler(q, i, i % 3 == 0)); }
    v.push(turn(17, "update main in src/wc.py to print a table", vec![read("data/big3.txt"), read("data/big4.txt"),
        write("src/wc.py", &format!("{WC2}# FIXME\n")), say("done Q17-A"),
        write("src/wc.py", WC2), say("DEC-D4: main prints via format_table. DONE-O2. Q17-R")]).edits(&["src/wc.py"]).adds(&["DEC-D4"]));
    v.push(turn(18, "add a --top flag to main using top_n in src/wc.py", vec![read("data/big1.txt"), write("src/wc.py", WC2),
        say_card("STEP-S3 done. OPEN-O3: tests for --top.", "tests for --top")])
        .edits(&["src/wc.py"]).adds(&["OPEN-O3"]));
    v.push(turn(19, "what is left to do?", vec![say("OPEN-O3 remains. Q19-R")]));
    for (i, q) in (20..=24).enumerate() { v.push(filler(q, i + 1, i % 2 == 1)); }
    v.push(Turn { q: "/compact".into(), lines: vec!["/compact".into()], steps: vec![], edits: vec![], adds: vec![] });
    v.push(turn(25, "what was the goal and what did we decide?", vec![say_card("Q25-R", "tests for --top")]));
    for (i, q) in (26..=31).enumerate() { v.push(filler(q, i + 2, i % 3 == 2)); }
    v
}

// ---------- analysis ----------

fn agent(log: &[Rec]) -> Vec<&Rec> { log.iter().filter(|r| r.kind == Kind::Agent).collect() }

fn user_texts(req: &Value) -> Vec<String> {
    msgs(req).iter().filter(|m| m["role"] == "user").filter_map(|m| m["content"][0]["text"].as_str().map(String::from)).collect()
}

/// The first agent request of turn `q` (its last message is the user text carrying the tag).
fn first_req<'a>(log: &'a [Rec], q: &str) -> Option<&'a Rec> {
    agent(log).into_iter().find(|r| msgs(&r.body).last().and_then(|m| m["content"][0]["text"].as_str()).is_some_and(|t| t.contains(q)))
}

fn whole(r: &Rec) -> String { format!("{}\n{}", r.body["system"].as_str().unwrap_or(""), render(&r.body)) }

/// Files the model is shown as edited: a write/edit tool_use still in history, or a `FILE:` line in notes/[state].
fn files_seen(r: &Rec) -> BTreeSet<String> {
    let mut s = edited_paths(&whole(r));
    for m in msgs(&r.body) {
        for b in m["content"].as_array().cloned().unwrap_or_default() {
            if b["type"] == "tool_use" && ["write", "edit", "edit_symbol"].contains(&b["name"].as_str().unwrap_or("")) {
                if let Some(p) = b["input"]["path"].as_str() { s.insert(p.to_string()); }
            }
        }
    }
    s
}

struct Row { step: String, goal: bool, files: (usize, usize), dec: (usize, usize), open: Vec<String>, est: usize, cached: usize, where_: String }

fn row(step: &str, r: &Rec, edited: &BTreeSet<String>, made: &BTreeSet<String>) -> Row {
    let w = whole(r);
    let m = markers(&w);
    let fs = files_seen(r);
    let decs: Vec<&String> = made.iter().filter(|x| x.starts_with("DEC-")).collect();
    let first = user_texts(&r.body).first().cloned().unwrap_or_default();
    let mut where_ = Vec::new();
    if first.contains("## Notes from") { where_.push("notes"); }
    if first.contains("[state]") { where_.push("state"); }
    if first.contains("[memory") || user_texts(&r.body).iter().any(|t| t.contains("[memory")) { where_.push("memory"); }
    Row {
        step: step.into(), goal: m.contains("GOAL-G1"),
        files: (edited.iter().filter(|f| fs.contains(*f)).count(), edited.len()),
        dec: (decs.iter().filter(|d| m.contains(**d)).count(), decs.len()),
        open: m.iter().filter(|x| x.starts_with("OPEN-")).cloned().collect(),
        est: r.total, cached: r.cached, where_: where_.join("+"),
    }
}

fn print_rows(title: &str, rows: &[Row]) {
    eprintln!("\n=== {title} ===");
    eprintln!("{:<14}| goal | files | dec | open        | est/limit   | reused | carried by", "step");
    for r in rows {
        eprintln!("{:<14}| {:<4} | {}/{}   | {}/{} | {:<11} | {:>4}/6144 | {:>5} | {}",
            r.step, if r.goal { "yes" } else { "NO" }, r.files.0, r.files.1, r.dec.0, r.dec.1, r.open.join(","), r.est, r.cached, r.where_);
    }
}

/// System + tools stable within a user turn (all runs). Intent routing (`route.rs`, config `route = "auto"`) and the
/// reasoning gate (`mcp_reason.rs`) pick the tool groups once per user turn, so system and tools may change only on
/// the first request of a turn (last message a user text, not tool results). Mid-turn the one allowed change is a
/// widen: a call to a session tool outside the route adds that group, so the tool list only grows.
fn assert_stable_prefix(reqs: &[&Rec]) {
    let names = |r: &Rec| -> Vec<String> { r.body["tools"].as_array().cloned().unwrap_or_default().iter().filter_map(|t| t["name"].as_str().map(String::from)).collect() };
    let turn_start = |r: &Rec| msgs(&r.body).last().is_some_and(|m| m["role"] == "user" && m["content"][0]["type"] == "text");
    for (i, w) in reqs.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        if turn_start(b) { continue; }
        let (na, nb) = (names(a), names(b));
        if na == nb {
            assert_eq!(b.body["system"].to_string(), a.body["system"].to_string(), "agent request {}: system changed mid-turn", i + 1);
            assert_eq!(b.body["tools"].to_string(), a.body["tools"].to_string(), "agent request {}: tools changed mid-turn", i + 1);
        } else {
            assert!(na.iter().all(|n| nb.contains(n)), "agent request {}: tools went away mid-turn: {na:?} -> {nb:?}", i + 1);
        }
    }
}

/// History rewrites (message i of the previous agent request changed): returns their count.
fn rewrites(reqs: &[&Rec]) -> usize {
    reqs.windows(2).filter(|w| {
        let (a, b) = (msgs(&w[0].body), msgs(&w[1].body));
        b.len() < a.len() || a.iter().zip(&b).any(|(x, y)| x != y)
    }).count()
}

fn assert_budget(log: &[Rec]) {
    for (i, r) in log.iter().enumerate() {
        let mt = r.body["max_tokens"].as_u64().unwrap_or(0) as usize;
        assert!(r.total + 256 <= MAX_CTX, "request {i} ({:?}): {} tokens leaves < 256 for output", r.kind, r.total);
        if r.kind == Kind::Agent { assert!(r.total + mt <= MAX_CTX + 64, "request {i}: {} + max_tokens {mt} > {MAX_CTX}", r.total); }
    }
}

fn session_files(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d.join(".elisa/sessions")).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()).unwrap_or_default();
    v.sort();
    v
}

/// a later run must get a different run id (unix seconds): move the earlier run's files one minute back
fn age_sessions(d: &Path) {
    for p in session_files(d) {
        let ts: u64 = p.file_stem().unwrap().to_str().unwrap().parse().unwrap();
        let back = ts - 60;
        std::fs::rename(&p, p.with_file_name(format!("{back}.json"))).unwrap();
        let meta = d.join(format!(".elisa/session-meta/{ts}.meta"));
        if meta.exists() { std::fs::rename(&meta, d.join(format!(".elisa/session-meta/{back}.meta"))).unwrap(); }
    }
}

/// Tokens the real gateway reuses (docs/GATEWAY.md "KV reuse"): the engine state is DeltaNet + KV, so it cannot rewind
/// to an interior token. Reuse = the previous agent request's whole message list is a prefix of this one (live state:
/// `prefix` / `followup`), else only the saved system + tools snapshot (stable prefix): after
/// any history rewrite (fold, shrunk old result) the next call reuses only system + tools.
fn state_reuse(prev: Option<&Rec>, req: &Rec) -> usize {
    let fixed = fixed_tok(&req.body);
    let Some(p) = prev else { return 0 };
    if p.body["system"] != req.body["system"] || p.body.get("tools") != req.body.get("tools") { return 0; }
    let (a, b) = (msgs(&p.body), msgs(&req.body));
    if a.len() <= b.len() && a.iter().zip(&b).all(|(x, y)| x == y) { fixed + a.iter().map(msg_tok).sum::<usize>() } else { fixed }
}

/// (re-prefilled tokens after a history rewrite, number of rewrites) over the agent requests, by `state_reuse`.
fn rewrite_prefill(reqs: &[&Rec]) -> (usize, usize) { let (t, n, _) = rewrite_split(reqs); (t, n) }

/// … plus the re-prefilled tokens of the rewrites that open a user turn (between-turns folds: a warm-up can take
/// them off the critical path); the rest happened mid-turn.
fn rewrite_split(reqs: &[&Rec]) -> (usize, usize, usize) {
    let (mut tok, mut n, mut boundary) = (0, 0, 0);
    for w in reqs.windows(2) {
        let (a, b) = (msgs(&w[0].body), msgs(&w[1].body));
        let rewrite = b.len() < a.len() || a.iter().zip(&b).any(|(x, y)| x != y);
        if rewrite {
            let t = w[1].total - state_reuse(Some(w[0]), w[1]);
            n += 1; tok += t;
            if b.last().is_some_and(|m| m["content"][0]["type"] == "text") { boundary += t; }
        }
    }
    (tok, n, boundary)
}

// ---------- the long session ----------

#[test]
fn long_session_context_and_memory() {
    let d = project("long");

    // ===== run 1 =====
    let turns = run1_turns();
    let user_turns = turns.iter().filter(|t| t.q.starts_with('Q')).count();
    assert!(user_turns >= 30, "{user_turns} turns");
    let steps: Vec<Step> = turns.iter().flat_map(|t| t.steps.clone()).collect();
    let input: String = turns.iter().flat_map(|t| t.lines.iter().map(|l| format!("{l}\n"))).collect::<String>() + "/exit\n";
    let f = Fake::start(steps);
    let o = run(&d, &f.url(), &input);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(f.left(), 0, "every scripted reply used: {}", o.stderr);
    let log1 = f.log();
    let folds = o.stderr.matches("[compacted:").count();
    let summaries = log1.iter().filter(|r| r.kind == Kind::SummaryTranscript).count();
    let card_folds = o.stderr.matches("folded · kept").count();
    eprintln!("run 1: {} requests · {} agent · folds/compactions {folds} ({card_folds} by state card, {summaries} transcript summaries, {} in-context)",
        log1.len(), agent(&log1).len(), log1.iter().filter(|r| r.kind == Kind::SummaryInContext).count());
    assert!(folds >= 4, "several automatic folds: {folds}\n{}", o.stderr);
    assert!(summaries >= 1 && card_folds >= 1, "both fold kinds ran: summaries {summaries}, card folds {card_folds}");
    assert!(o.stderr.contains("check: sh check.sh ✗") && o.stderr.contains("check: sh check.sh ✓"), "the failed check and its fix ran");

    // survival table, per turn
    let (mut edited, mut made, mut rows) = (BTreeSet::new(), BTreeSet::new(), Vec::new());
    for t in &turns {
        if t.q.starts_with('Q') {
            if let Some(r) = first_req(&log1, &t.q) { rows.push(row(&t.q, r, &edited, &made)); }
        }
        edited.extend(t.edits.iter().map(|s| s.to_string()));
        made.extend(t.adds.iter().map(|s| s.to_string()));
    }
    print_rows("run 1 (first request of each turn; markers made before the turn)", &rows);
    let a1 = agent(&log1);
    eprintln!("run 1: history rewrites between agent requests: {} (folds + /compact) · KV reused share {:.0}%",
        rewrites(&a1), 100.0 * a1.iter().map(|r| r.cached).sum::<usize>() as f64 / a1.iter().map(|r| r.total).sum::<usize>() as f64);
    let reused: usize = a1.windows(2).map(|w| state_reuse(Some(w[0]), w[1])).sum();
    let (rp, rn, rb) = rewrite_split(&a1);
    eprintln!("run 1: rewrites at a turn start (warm-up can prefill them while the user reads) {rb} of {rp} tokens");
    eprintln!("run 1 (gateway rule, DeltaNet state): reused share {:.0}% · {rn} rewrites re-prefill {rp} tokens ({} per rewrite; ~{:.0} s at 75 tok/s)",
        100.0 * reused as f64 / a1.iter().skip(1).map(|r| r.total).sum::<usize>() as f64, rp / rn.max(1), rp as f64 / 75.0);

    // --- hard checks, run 1 ---
    assert_stable_prefix(&a1);
    assert_budget(&log1);
    // the goal is in every agent request (user text, then notes and/or the [state] card)
    for r in &rows { assert!(r.goal, "{}: goal lost", r.step); }
    // the /compact summary was in context (same system + tools, a prefix hit)
    let ic: Vec<&Rec> = log1.iter().filter(|r| r.kind == Kind::SummaryInContext).collect();
    assert_eq!(ic.len(), 1);
    assert_eq!(ic[0].body["system"], a1[0].body["system"]);
    assert!(ic[0].cached > 0);
    // after /compact: only the first message (notes + [state] + request)
    let q25 = first_req(&log1, "Q25").unwrap();
    assert_eq!(msgs(&q25.body).len(), 1);
    let (notes1_path, notes1) = newest_notes_file(&d);
    eprintln!("run 1 notes ({}):\n{notes1}", notes1_path.display());

    // ===== run 2: a brand-new session in the same project, then /clear and /reset =====
    age_sessions(&d);
    let g = Fake::start(vec![
        say_card("R1 answer.", "tests for --top"), // R01 (no tool)
        read("data/big1.txt"), say("R02 answer."),
        say("R03 answer."),
        say("R04 answer."),
    ]);
    let input2 = "what was GOAL-G1 about, and what did we decide? R01\nwhat does data/big1.txt say? R02\n/clear\nwhat is still open? R03\n/reset\nanything else? R04\n/exit\n";
    let o2 = run(&d, &g.url(), input2);
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    let log2 = g.log();
    let a2 = agent(&log2);
    let r01 = first_req(&log2, "R01").unwrap();
    let r03 = first_req(&log2, "R03").unwrap();
    let r04 = first_req(&log2, "R04").unwrap();
    let all_edited: BTreeSet<String> = ["src/wc.py", "src/report.py"].map(String::from).into();
    let all_made: BTreeSet<String> = turns.iter().flat_map(|t| t.adds.iter().map(|s| s.to_string())).collect();
    let rows2 = vec![row("new session", r01, &all_edited, &all_made), row("after /clear", r03, &all_edited, &all_made), row("after /reset", r04, &all_edited, &all_made)];
    print_rows("run 2 (new session, /clear, /reset)", &rows2);
    let mut both = a1.clone();
    both.extend(a2.iter());
    assert_stable_prefix(&both);
    assert_budget(&log2);
    // the new session starts clean (a small model can answer "hey" with the earlier session's task): none of
    // run 1's notes, nor run 1's last card (its goal / next step were that session's task), nor
    // run 1's raw history. Run 1's notes stay in their own file, unchanged.
    let w01 = whole(r01);
    assert!(!w01.contains(elisa::context::NOTES_HEADING) && !w01.contains(elisa::context::NOTES_BACKGROUND), "run 1's notes reached a new session: {w01}");
    assert_eq!(std::fs::read_to_string(&notes1_path).unwrap(), notes1, "run 2 changed run 1's notes");
    assert!(!w01.contains("next: tests for --top"), "run 1's state card reached a new session: {w01}");
    assert!(!w01.contains("Q31-R") && !w01.contains("big3 sample line"), "raw run-1 history leaked into a new session");
    // after /clear: notes (with the /clear summary), no raw pre-clear history (R02's tool output and reply)
    let w03 = whole(r03);
    assert_eq!(msgs(&r03.body).len(), 1);
    assert!(!w03.contains("big1 sample line") && !w03.contains("R02 answer."), "pre-/clear history leaked: {w03}");
    // after /reset: the state card is gone, notes stay
    let w04 = whole(r04);
    assert!(!w04.contains("[state]") && w04.contains("## Notes from"), "{w04}");

    // ===== run 3: restart, then /resume run 1 =====
    age_sessions(&d);
    let h = Fake::start(vec![say("S01 answer."), say("S02 answer.")]);
    let list_before = session_files(&d);
    // newest first: run 2's post-/clear session, run 2's pre-/clear one (its own file since /clear), then run 1's
    let o3 = run(&d, &h.url(), "status? S01\n/resume\n3\ncontinue with the --top tests S02\n/exit\n");
    assert_eq!(o3.code, 0, "{}", o3.stderr);
    let log3 = h.log();
    let s01 = first_req(&log3, "S01").unwrap();
    let s02 = first_req(&log3, "S02").unwrap();
    let rows3 = vec![row("restart", s01, &all_edited, &all_made), row("/resume run 1", s02, &all_edited, &all_made)];
    print_rows("run 3 (restart after /reset, /resume of run 1)", &rows3);
    eprintln!("sessions before run 3: {list_before:?}\nrun 3 stderr (resume):\n{}", o3.stderr.lines().filter(|l| l.contains("resum") || l.contains(". ")).collect::<Vec<_>>().join("\n"));
    both.extend(agent(&log3).iter());
    assert_stable_prefix(&both);
    assert_budget(&log3);
    assert!(o3.stderr.contains("resumed \""), "{}", o3.stderr);
    // the resumed request carries run 1's saved (post-fold) history, then the new question
    let w02 = whole(s02);
    assert!(w02.contains("Q31") && msgs(&s02.body).len() > 1, "{w02}");

    // elisa-mem across sessions: what was recalled into any request ([memory] block)
    let mem_lines: Vec<String> = log1.iter().chain(&log2).chain(&log3).filter(|r| r.kind == Kind::Agent)
        .flat_map(|r| user_texts(&r.body)).filter(|t| t.contains("[memory")).map(|t| t.lines().filter(|l| l.starts_with("- ")).collect::<Vec<_>>().join(" | ")).collect();
    eprintln!("\nrecalled [memory] blocks: {}\n{}", mem_lines.len(), mem_lines.join("\n"));
    let store = d.join("_home/.elisa/memory");
    eprintln!("memory store: {:?}", std::fs::read_dir(&store).map(|r| r.flatten().map(|e| e.file_name()).collect::<Vec<_>>()).ok());
}

// ---------- focused probes ----------

/// A turn whose last user text is Elisa's check-fix note: the boundary fold keeps "the last exchange" from that note
/// on, so the user's actual request of that turn leaves the window.
#[test]
fn known_fold_after_check_fix_drops_the_request() {
    let d = project("fixnote");
    let f = Fake::start(vec![
        read("data/big1.txt"), read("data/big2.txt"),
        write("src/wc.py", &format!("{WC0}# FIXME\n")), say("done"),
        write("src/wc.py", WC0), say("fixed it"),
        say("next answer"),
    ]);
    let o = run(&d, &f.url(), "update src/wc.py so main prints a table REQ-UNIQUE-1\nand now? N2\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("[compacted:"), "a fold ran: {}", o.stderr);
    let log = f.log();
    let n2 = first_req(&log, "N2").unwrap();
    let w = whole(n2);
    eprintln!("N2 first message:\n{}", user_texts(&n2.body)[0]);
    assert!(w.contains("REQ-UNIQUE-1"), "the request of the folded-but-kept exchange is gone");
}

/// /reset drops the state card for this run only: a restart reloads it from elisa-mem (latest_card).
#[test]
fn known_reset_card_comes_back_after_restart() {
    let d = project("reset");
    let f = Fake::start(vec![say_card("ok", "OLD-TASK-NEXT")]);
    let o = run(&d, &f.url(), "first question here R1\n/reset\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    age_sessions(&d);
    let g = Fake::start(vec![say("fresh")]);
    let o = run(&d, &g.url(), "a new unrelated question R2\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let w = whole(first_req(&g.log(), "R2").unwrap());
    assert!(!w.contains("OLD-TASK-NEXT"), "the card dropped by /reset came back: {w}");
}

/// /clear keeps writing the same run's session file: the pre-/clear conversation is
/// overwritten by the post-clear history, so /resume can never get back to it.
#[test]
fn known_clear_overwrites_the_resumable_session() {
    let d = project("clear-resume");
    let f = Fake::start(vec![write("src/report.py", "x = 1\n"), say("STEP-X done"), say("after clear")]);
    let o = run(&d, &f.url(), "add a format_table function for the report PRE-CLEAR-1\n/clear\nsomething new POST-1\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let files = session_files(&d);
    let all: String = files.iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    assert!(all.contains("PRE-CLEAR-1") && all.contains("STEP-X done"), "{} session file(s); none holds the pre-/clear conversation", files.len());
}

/// Two folds by state card, nothing summarised: the agent still sees the card's next step in `[state]`, and the
/// card folds merged the folded turns' cards into the notes.
#[test]
fn card_folds_keep_the_next_step_and_write_notes() {
    let d = project("card-folds");
    let f = Fake::start(vec![
        read("data/big1.txt"), write("src/wc.py", WC1), say_card("STEP-S1 done", "step s2"),
        read("data/big2.txt"), read("data/big3.txt"), say_card("looked Q2", "step s2"),
        say("what next Q3 answer"),
    ]);
    let o = run(&d, &f.url(), "make count_words lowercase words, then add a top_n function to src/wc.py\nlook at big2 and big3 Q2\nwhat was the next step? Q3\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let log = f.log();
    assert_eq!(log.iter().filter(|r| r.kind == Kind::SummaryTranscript).count(), 0, "card folds only");
    let q3 = whole(first_req(&log, "Q3").unwrap());
    assert!(q3.contains("[state]") && q3.contains("next: step s2"), "{q3}");
    let notes = newest_notes(&d);
    assert!(notes.contains("## From state cards") && notes.contains("goal: GOAL-G1 word-count CLI") && notes.contains("open: step s2"), "{notes}");
}

/// Recall after folds: the session goal stays the first request's (not the newest request's), and every
/// request stays under the limit.
#[test]
fn recall_after_folds_keeps_the_session_goal() {
    let d = project("recall");
    let report = "def header():\n    return \"word count\"\n\n\ndef format_table(rows):\n    return rows\n";
    let mut steps = vec![
        read("data/big1.txt"), write("src/wc.py", WC1), say_card("STEP-S1 done", "step s2"),
        write("src/report.py", report), say("table added"),
    ];
    // fillers to fold several times
    for k in 0..6 { steps.push(read(&format!("data/big{}.txt", 1 + k % 4))); steps.push(say(&format!("sample text F{k}"))); }
    steps.push(say("Q9 answer"));
    steps.push(say("Q10 answer"));
    let f = Fake::start(steps);
    let mut input = String::from("add a top_n function to src/wc.py. Our goal for this session: GOAL-S1 add top_n and a report table to the wc tool\n");
    input += "add a format_table function for the report\n";
    for k in 0..6 { input += &format!("what does data/big{}.txt say around line 050? F{k}\n", 1 + k % 4); }
    input += "list what we changed Q9\nwhat was the goal of this session? Q10\n/exit\n";
    let o = run(&d, &f.url(), &input);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(f.left(), 0, "every scripted reply used: {}", o.stderr);
    let log = f.log();
    assert_budget(&log);
    assert!(o.stderr.contains("[compacted:") || o.stderr.contains("folded"), "folds ran: {}", o.stderr);
    let q10 = first_req(&log, "Q10").unwrap();
    let first = user_texts(&q10.body)[0].clone();
    eprintln!("Q10 first message:\n{first}");
    // the goal from the first request, not "what was the goal" / the newest request
    assert!(first.contains("session goal: GOAL-S1 add top_n and a report table to the wc tool"), "{first}");
    assert!(first.contains("## Session goal") && !first.contains("session goal: list what we changed"), "{first}");
}

/// Many reads in one turn: an in-turn shrink of older results would rewrite the history, and the next call
/// re-prefill everything after system + tools. One request with
/// ten big reads: the new results are cut to fit while the window has room (append-only), so the history is rewritten
/// rarely; every request stays under the limit. Prints reuse by the gateway rule (`state_reuse`).
#[test]
fn many_reads_in_one_request_keep_the_prompt_reusable() {
    let d = project("reads");
    let mut steps: Vec<Step> = (0..10).map(|k| read(&format!("data/big{}.txt", 1 + k % 4))).collect();
    steps.push(say("read them all R1"));
    steps.push(say("R2 answer"));
    let f = Fake::start(steps);
    let o = run(&d, &f.url(), "read data/big1..4 again and again R1\nand now? R2\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let log = f.log();
    assert_budget(&log);
    let a = agent(&log);
    let (rp, rn) = rewrite_prefill(&a);
    let reused: usize = a.windows(2).map(|w| state_reuse(Some(w[0]), w[1])).sum();
    let total: usize = a.iter().skip(1).map(|r| r.total).sum();
    eprintln!("ten reads in one request: {} agent calls · {rn} rewrites re-prefill {rp} tokens · reused {:.0}% · trimmed lines {}",
        a.len(), 100.0 * reused as f64 / total as f64, o.stderr.matches("trimmed to fit the context").count());
    // append-only cuts rewrite less than shrinking older results whenever a step did not fit, and re-prefill about
    // half the tokens (the rest are the folds a full window needs)
    assert!(rn <= 4 && rp < 9000, "{rn} rewrites, {rp} tokens re-prefilled in one request: {}", o.stderr);
}

/// Rewrites of an earlier message between two agent calls of the same user turn (the second call's last message is a
/// tool result, i.e. it continues the turn): each one re-prefills everything after system + tools on the gateway.
fn mid_turn_rewrites(reqs: &[&Rec]) -> Vec<(usize, usize)> {
    reqs.windows(2).enumerate().filter_map(|(i, w)| {
        let (a, b) = (msgs(&w[0].body), msgs(&w[1].body));
        let mid = b.last().is_some_and(|m| m["content"][0]["type"] == "tool_result");
        let rewrite = b.len() < a.len() || a.iter().zip(&b).any(|(x, y)| x != y);
        (mid && rewrite).then(|| (i + 1, w[1].total - state_reuse(Some(w[0]), w[1])))
    }).collect()
}

/// max_ctx 8192: two big reads, then many small steps (`False\n[exit 0]`, 3-line reads) with
/// the window in its last 12.5%. Shrinking ONE earlier result per step to `[trimmed: … N lines]` (often longer than
/// the original) would make each call reuse only system + tools and re-prefill everything after them.
/// The server's tokenizer counts 25% more than the chars/token formula (code: the text estimate runs that low once a
/// rewrite drops the reported usage). Append-only: the new results are cut instead (a big one leaves the window's
/// tail for the small steps after it), and no call within the turn rewrites the history.
#[test]
fn small_results_late_in_a_turn_keep_the_history_append_only() {
    let d = project("append");
    let mut steps: Vec<Step> = vec![read("data/big1.txt"), read("data/big2.txt")];
    for k in 0..8 {
        steps.push(if k % 2 == 0 { Step::Tool("bash", json!({"command": format!("echo False # check {k}")})) }
            else { Step::Tool("read", json!({"path": "src/wc.py", "offset": 1 + k % 10, "limit": 3})) });
    }
    steps.push(say("routes listed R1"));
    let f = Fake::start_scaled(steps, 125);
    let o = run(&d, &f.url(), "read the files and check a few things R1\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(f.left(), 0, "every scripted reply used: {}", o.stderr);
    let log = f.log();
    assert_budget(&log);
    let a = agent(&log);
    let limit = MAX_CTX - 2048 - MAX_CTX.div_ceil(100) * 3;
    let peak = a.iter().map(|r| r.total).max().unwrap_or(0);
    let late = a.iter().filter(|r| r.total * 8 >= limit * 7).count();
    let bad = mid_turn_rewrites(&a);
    eprintln!("run2 replay: {} agent calls · peak {peak}/{limit} · {late} calls in the last 12.5% · mid-turn rewrites {bad:?}", a.len());
    // the pattern really ran: several calls in the zone where the old code shrank one older result per step
    assert!(late >= 4, "only {late} calls in the last 12.5% of the window (peak {peak}): {}", o.stderr);
    assert!(bad.is_empty(), "history rewritten within the turn at calls {bad:?} (call, re-prefilled tokens): {}", o.stderr);
}

/// After an in-turn rewrite the estimate falls back to chars/3.3, which runs low on code, so a request can go a few
/// tokens over max_ctx and get nothing back. A server whose tokenizer counts 25% more than the formula, a turn
/// that fills the window, a second turn: no request goes over (the server never answers "Context full").
#[test]
fn a_denser_tokenizer_never_overflows_the_context() {
    let d = project("dense");
    let mut steps: Vec<Step> = (0..6).map(|k| read(&format!("data/big{}.txt", 1 + k % 4))).collect();
    for k in 0..10 { steps.push(Step::Tool("read", json!({"path": "src/wc.py", "offset": 1 + k, "limit": 4}))); }
    steps.push(say("done R1"));
    steps.push(read("data/big3.txt"));
    steps.push(say("done R2"));
    let f = Fake::start_scaled(steps, 125);
    let o = run(&d, &f.url(), "read them all R1\nonce more R2\n/exit\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let log = f.log();
    let over: Vec<(usize, usize)> = log.iter().enumerate().filter(|(_, r)| r.total + 256 > MAX_CTX).map(|(i, r)| (i, r.total)).collect();
    eprintln!("dense tokenizer: {} requests · peak {} real tokens · over {over:?}", log.len(), log.iter().map(|r| r.total).max().unwrap_or(0));
    assert!(over.is_empty(), "requests over the context (index, real tokens): {over:?}: {}", o.stderr);
    assert!(!o.stderr.contains("Context full"), "{}", o.stderr);
    assert_budget(&log);
}

/// The newest per-session notes file (`.elisa/notes/<session id>.md`) of project `d`: (path, text).
fn newest_notes_file(d: &std::path::Path) -> (std::path::PathBuf, String) {
    let p = std::fs::read_dir(d.join(".elisa/notes")).expect("no .elisa/notes")
        .filter_map(|e| { let p = e.ok()?.path(); let id: u64 = p.file_stem()?.to_str()?.parse().ok()?; Some((id, p)) })
        .max_by_key(|(id, _)| *id).expect("no notes file").1;
    let t = std::fs::read_to_string(&p).unwrap();
    (p, t)
}
fn newest_notes(d: &std::path::Path) -> String { newest_notes_file(d).1 }
