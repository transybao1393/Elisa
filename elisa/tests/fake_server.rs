//! In-process fake Anthropic Messages server + tests for config, api, repl and ui working together.
use elisa::api::{Client, Request};
use elisa::config::Config;
use elisa::types::{Block, Message};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------- fake server ----------

enum Resp {
    Sse(Vec<String>),
    Json(u16, String),
}

#[derive(Clone, Debug)]
struct Recorded { method: String, path: String, headers: Vec<(String, String)>, body: String }

impl Recorded {
    fn json(&self) -> Value { serde_json::from_str(&self.body).expect("request body is JSON") }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

struct Fake {
    /// /health advertises the prefill-only warm-up (`info.warm`); warm-up calls are answered without the script
    warm: Arc<AtomicBool>,
    port: u16,
    script: Arc<Mutex<VecDeque<Resp>>>,
    log: Arc<Mutex<Vec<Recorded>>>,
}

/// GET /v1/streamdense/watcher answers: None = an older gateway (404 on both watcher endpoints); else the views in
/// order (the last one repeats)
type Views = Option<Arc<Mutex<VecDeque<Value>>>>;

impl Fake {
    fn start(script: Vec<Resp>) -> Fake { Fake::start_watch(script, None) }
    fn start_watch(script: Vec<Resp>, views: Option<Vec<Value>>) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let log = Arc::new(Mutex::new(Vec::new()));
        let views: Views = views.map(|v| Arc::new(Mutex::new(VecDeque::from(v))));
        let (s, g) = (script.clone(), log.clone());
        let warm = Arc::new(AtomicBool::new(false));
        let w = warm.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (s, g, v, w) = (s.clone(), g.clone(), views.clone(), w.clone());
                std::thread::spawn(move || { let _ = handle(c, &s, &g, &v, &w); });
            }
        });
        Fake { warm, port, script, log }
    }
    fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
    fn log(&self) -> Vec<Recorded> { self.log.lock().unwrap().clone() }
    fn messages(&self) -> Vec<Value> {
        self.log().into_iter().filter(|r| r.path == "/v1/messages" && r.header("x-streamdense-warm").is_none()).map(|r| r.json()).collect()
    }
    fn warmups(&self) -> Vec<Value> {
        self.log().into_iter().filter(|r| r.path == "/v1/messages" && r.header("x-streamdense-warm").is_some()).map(|r| r.json()).collect()
    }
    fn left(&self) -> usize { self.script.lock().unwrap().len() }
}

fn handle(mut c: TcpStream, script: &Mutex<VecDeque<Resp>>, log: &Mutex<Vec<Recorded>>, views: &Views, warm: &AtomicBool) -> std::io::Result<()> {
    c.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(c.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    let mut it = first.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 { break; }
        let l = l.trim_end();
        if l.is_empty() { break; }
        if let Some((k, v)) = l.split_once(':') { headers.push((k.trim().to_string(), v.trim().to_string())); }
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();
    let is_warm = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-streamdense-warm"));
    log.lock().unwrap().push(Recorded { method: method.clone(), path: path.clone(), headers, body: body.clone() });

    let resp = match (method.as_str(), path.as_str()) {
        ("GET", "/health") if warm.load(Ordering::SeqCst) => Resp::Json(200, r#"{"status":"ok","info":{"max_ctx":4096,"warm":true}}"#.into()),
        ("GET", "/health") => Resp::Json(200, r#"{"status":"ok","info":{"max_ctx":4096}}"#.into()),
        ("POST", "/v1/messages") if is_warm && warm.load(Ordering::SeqCst) => Resp::Json(200, r#"{"type":"message","content":[],"stop_reason":"end_turn","usage":{"input_tokens":900,"output_tokens":0}}"#.into()),
        ("POST", "/v1/messages/count_tokens") => Resp::Json(200, r#"{"input_tokens":321}"#.into()),
        ("POST", "/v1/streamdense/watcher/note") if views.is_some() => Resp::Json(200, r#"{"ok":true,"decision":null,"domain":"code-rust","turn":3}"#.into()),
        ("GET", "/v1/streamdense/watcher") if views.is_some() => {
            let mut q = views.as_ref().unwrap().lock().unwrap();
            let v = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap_or(json!({})) };
            Resp::Json(200, v.to_string())
        }
        ("POST", "/v1/messages") => script.lock().unwrap().pop_front()
            .unwrap_or_else(|| Resp::Json(500, r#"{"type":"error","error":{"type":"api_error","message":"script exhausted"}}"#.into())),
        _ => Resp::Json(404, r#"{"type":"error","error":{"type":"not_found_error","message":"no route"}}"#.into()),
    };
    match resp {
        Resp::Json(code, b) => {
            write!(c, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len())?;
        }
        Resp::Sse(events) => {
            let total: usize = events.iter().map(String::len).sum();
            write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {total}\r\nconnection: close\r\n\r\n")?;
            c.flush()?;
            for e in events {
                std::thread::sleep(Duration::from_millis(3));
                c.write_all(e.as_bytes())?;
                c.flush()?;
            }
        }
    }
    c.flush()
}

// ---------- SSE script builder ----------

fn ev(kind: &str, data: Value) -> String { format!("event: {kind}\ndata: {data}\n\n") }

struct Sse { ev: Vec<String>, idx: usize }

impl Sse {
    fn new(input: usize, cache_read: usize) -> Sse {
        let start = json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],
            "model":"fake","stop_reason":null,"usage":{"input_tokens":input,"output_tokens":1,"cache_read_input_tokens":cache_read}}});
        Sse { ev: vec![ev("message_start", start), ev("ping", json!({"type":"ping"}))], idx: 0 }
    }
    fn text(mut self, pieces: &[&str]) -> Sse {
        let i = self.idx;
        self.ev.push(ev("content_block_start", json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}})));
        for p in pieces {
            self.ev.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":p}})));
        }
        self.ev.push(ev("content_block_stop", json!({"type":"content_block_stop","index":i})));
        self.idx += 1;
        self
    }
    fn tool(mut self, id: &str, name: &str, pieces: &[&str]) -> Sse {
        let i = self.idx;
        self.ev.push(ev("content_block_start", json!({"type":"content_block_start","index":i,
            "content_block":{"type":"tool_use","id":id,"name":name,"input":{}}})));
        for p in pieces {
            self.ev.push(ev("content_block_delta", json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":p}})));
        }
        self.ev.push(ev("content_block_stop", json!({"type":"content_block_stop","index":i})));
        self.idx += 1;
        self
    }
    /// A complete tool input, split into 3 uneven pieces.
    fn tool_input(self, id: &str, name: &str, input: Value) -> Sse {
        let s = input.to_string();
        let cut: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
        let (a, b) = (cut[cut.len() / 3], cut[2 * cut.len() / 3]);
        self.tool(id, name, &[&s[..a], &s[a..b], &s[b..]])
    }
    /// the gateway message id in message_start (default msg_1)
    fn id(mut self, id: &str) -> Sse { self.ev[0] = self.ev[0].replace("\"msg_1\"", &format!("\"{id}\"")); self }
    fn raw(mut self, e: String) -> Sse { self.ev.push(e); self }
    fn end(mut self, stop: &str, out: usize) -> Resp {
        self.ev.push(ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":out}})));
        self.ev.push(ev("message_stop", json!({"type":"message_stop"})));
        Resp::Sse(self.ev)
    }
    fn into_resp(self) -> Resp { Resp::Sse(self.ev) }
}

fn text_reply(t: &str) -> Resp { Sse::new(100, 0).text(&[t]).end("end_turn", 5) }

// ---------- helpers ----------

fn tmpdir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("elisa-t2-{}-{}-{}", std::process::id(), tag, N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn client(url: &str) -> Client {
    Client::new(&Config { base_url: url.into(), model: "test-m".into(), ..Config::default() })
}

fn send(c: &Client, msgs: &[Message]) -> (Result<elisa::types::Reply, String>, String) {
    let cancel = AtomicBool::new(false);
    let mut streamed = String::new();
    let req = Request { system: "sys", tools: &[], messages: msgs, max_tokens: 100 };
    let r = c.send(&req, &cancel, &mut |t| streamed.push_str(t));
    (r, streamed)
}

struct Out { code: i32, stdout: String, stderr: String }

/// Runs the binary with HOME = cwd/_home. The user config there carries the legacy chat/code mode keys (removed
/// 2026-11-01): old config files must still load, and the keys change nothing.
fn run_elisa(args: &[&str], cwd: &Path, stdin: Option<&str>) -> Out { run_elisa_env(args, cwd, stdin, &[]) }

/// `run_elisa` with extra environment (the demo crate's real cargo needs the real CARGO_HOME / RUSTUP_HOME)
fn run_elisa_env(args: &[&str], cwd: &Path, stdin: Option<&str>, env: &[(&str, String)]) -> Out { run_elisa_cfg(args, cwd, stdin, env, "") }

/// `run_elisa_env` with extra lines in the user config (~/.config/elisa/config.toml)
fn run_elisa_cfg(args: &[&str], cwd: &Path, stdin: Option<&str>, env: &[(&str, String)], user_cfg: &str) -> Out {
    let home = cwd.join("_home");
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), format!("auto_mode = true\nchat_window = 1600\n{user_cfg}")).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
    cmd.args(args).arg("--cwd").arg(cwd).current_dir(cwd).env("HOME", &home)
        .args(if args.contains(&"--model") || user_cfg.contains("model") { &[][..] } else { &["--model", "test-m"][..] })
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, _) in std::env::vars() {
        if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") || k.starts_with("BRAVE_") { cmd.env_remove(&k); }
    }
    for (k, v) in env { cmd.env(k, v); }
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin { child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap(); }
    let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let to = std::thread::spawn(move || { let mut s = String::new(); so.read_to_string(&mut s).ok(); s });
    let te = std::thread::spawn(move || { let mut s = String::new(); se.read_to_string(&mut s).ok(); s });
    let t0 = Instant::now();
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() { break st.code().unwrap_or(-1); }
        if t0.elapsed() > Duration::from_secs(60) { child.kill().ok(); panic!("elisa timed out"); }
        std::thread::sleep(Duration::from_millis(20));
    };
    Out { code, stdout: to.join().unwrap(), stderr: te.join().unwrap() }
}

fn has_tools(req: &Value) -> bool { req.get("tools").is_some() }
fn system(req: &Value) -> &str { req["system"].as_str().unwrap() }

/// The prefix rule: byte-identical system + tools; messages only appended.
fn assert_prefix_rule(reqs: &[Value]) {
    for w in reqs.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert_eq!(a["system"].to_string(), b["system"].to_string(), "system changed within a turn");
        assert_eq!(a["tools"].to_string(), b["tools"].to_string(), "tools changed within a turn");
        let (ma, mb) = (a["messages"].as_array().unwrap(), b["messages"].as_array().unwrap());
        assert!(mb.len() > ma.len(), "messages did not grow");
        for (i, (x, y)) in ma.iter().zip(mb).enumerate() {
            assert_eq!(x.to_string(), y.to_string(), "message {i} was rewritten");
        }
    }
}

fn last_tool_result(req: &Value) -> (String, bool) {
    let m = req["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(m["role"], "user");
    let b = m["content"].as_array().unwrap().iter().find(|b| b["type"] == "tool_result").expect("tool_result");
    (b["content"].as_str().unwrap().to_string(), b["is_error"].as_bool().unwrap_or(false))
}

// ---------- api ----------

#[test]
fn api_text_split_mid_word_with_cache_read() {
    let f = Fake::start(vec![Sse::new(1234, 1100).text(&["Hel", "lo wo", "rld", "!"]).end("end_turn", 7)]);
    let (r, streamed) = send(&client(&f.url()), &[Message::user_text("hi")]);
    let r = r.unwrap();
    assert_eq!(streamed, "Hello world!");
    assert_eq!(r.content, vec![Block::Text { text: "Hello world!".into() }]);
    assert_eq!(r.stop_reason, "end_turn");
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens, r.usage.cache_read_input_tokens), (1234, 7, 1100));
    assert!(r.ttft_s > 0.0 && r.ttft_s <= r.total_s, "ttft {} total {}", r.ttft_s, r.total_s);

    let req = &f.log()[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.header("x-api-key"), Some("dummy"));
    assert_eq!(req.header("authorization"), Some("Bearer dummy"));
    assert_eq!(req.header("anthropic-version"), Some("2023-06-01"));
    // the project id: 16 hex digits of the FNV-1a of the canonical cwd (the client's default: the process's cwd)
    let pid = elisa::api::project_id(&std::env::current_dir().unwrap());
    assert!(pid.len() == 16 && pid.chars().all(|c| c.is_ascii_hexdigit()), "{pid}");
    assert_eq!(req.header(elisa::api::PROJECT_HEADER), Some(pid.as_str()));
    assert!(req.header("content-type").unwrap().starts_with("application/json"));
    let b = req.json();
    let keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["model", "max_tokens", "system", "messages", "stream"], "no tools/thinking when empty/off");
    assert_eq!(b["stream"], true);
    assert_eq!(b["model"], "test-m");
}

#[test]
fn api_text_then_tool_use_json_split() {
    let f = Fake::start(vec![Sse::new(50, 0)
        .text(&["Let me ", "wri", "te it."])
        .tool("toolu_1", "write", &["{\"pa", "th\": \"a/b.t", "xt\", \"content\": \"x\\ny", "\\n\"}"])
        .end("tool_use", 33)]);
    let (r, streamed) = send(&client(&f.url()), &[Message::user_text("go")]);
    let r = r.unwrap();
    assert_eq!(streamed, "Let me write it.");
    assert_eq!(r.content, vec![
        Block::Text { text: "Let me write it.".into() },
        Block::ToolUse { id: "toolu_1".into(), name: "write".into(), input: json!({"path":"a/b.txt","content":"x\ny\n"}) },
    ]);
    assert_eq!(r.stop_reason, "tool_use");
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens, r.usage.cache_read_input_tokens), (50, 33, 0));
    assert!(r.ttft_s <= r.total_s);
}

#[test]
fn api_bad_tool_json_keeps_raw() {
    let f = Fake::start(vec![Sse::new(5, 0).tool("t", "bash", &["{\"command\": \"ls"]).end("tool_use", 3)]);
    let r = send(&client(&f.url()), &[Message::user_text("go")]).0.unwrap();
    match &r.content[0] {
        Block::ToolUse { input, .. } => assert_eq!(input["_raw"], "{\"command\": \"ls"),
        b => panic!("{b:?}"),
    }
}

#[test]
fn api_error_event_is_err() {
    let err = ev("error", json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded right now"}}));
    let f = Fake::start(vec![Sse::new(5, 0).text(&["par"]).raw(err).into_resp()]);
    let (r, _) = send(&client(&f.url()), &[Message::user_text("go")]);
    assert_eq!(r.unwrap_err(), "Overloaded right now");
}

#[test]
fn api_non_2xx_is_err() {
    let f = Fake::start(vec![Resp::Json(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long"}}"#.into())]);
    let e = send(&client(&f.url()), &[Message::user_text("go")]).0.unwrap_err();
    assert!(e.contains("400") && e.contains("prompt is too long"), "{e}");
}

#[test]
fn api_cancel_returns_partial() {
    let f = Fake::start(vec![Sse::new(5, 0).text(&["one ", "two ", "three"]).tool_input("t", "bash", json!({"command":"ls"})).end("tool_use", 3)]);
    let c = client(&f.url());
    let cancel = AtomicBool::new(false);
    let msgs = [Message::user_text("go")];
    let req = Request { system: "s", tools: &[], messages: &msgs, max_tokens: 10 };
    let r = c.send(&req, &cancel, &mut |_| cancel.store(true, Ordering::SeqCst)).unwrap();
    assert_eq!(r.stop_reason, "cancelled");
    assert_eq!(r.content, vec![Block::Text { text: "one ".into() }]);
}

#[test]
fn api_health_and_count_tokens() {
    let f = Fake::start(vec![]);
    let c = client(&f.url());
    assert_eq!(c.server_max_ctx(), Some(4096));
    let msgs = [Message::user_text("hi")];
    let tools = elisa::tools::defs();
    let n = c.count_tokens(&Request { system: "s", tools: &tools, messages: &msgs, max_tokens: 9 }).unwrap();
    assert_eq!(n, 321);
    let b = f.log().into_iter().find(|r| r.path == "/v1/messages/count_tokens").unwrap().json();
    assert!(b.get("stream").is_none() && b.get("max_tokens").is_none() && b.get("tools").is_some());
    // unreachable server -> None, quickly
    let dead = client("http://127.0.0.1:1");
    let t = Instant::now();
    assert_eq!(dead.server_max_ctx(), None);
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[test]
fn config_files_and_env_overrides() {
    // no process-env mutation (it raced with the e2e tests that spawn elisa): home + env are passed in
    let d = tmpdir("cfg");
    let home = d.join("home");
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::create_dir_all(d.join("proj/.elisa")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), "model = \"user-m\"\nmax_output = 111\nauto_approve = [\"read\"]\n").unwrap();
    std::fs::write(d.join("proj/.elisa/config.toml"), "model = \"proj-m\"\nbase_url = \"http://x:1/\"\nmax_ctx = 999\n").unwrap();
    let env = std::cell::RefCell::new(std::collections::HashMap::<&str, &str>::new());
    let load = || Config::load_from(&d.join("proj"), Some(home.clone()), |k| env.borrow().get(k).map(|v| v.to_string()));

    let c = load().unwrap();
    assert_eq!((c.model.as_str(), c.max_output, c.base_url.as_str(), c.max_ctx), ("proj-m", 111, "http://x:1", Some(999)));
    assert_eq!(c.auto_approve, ["read"]);
    assert_eq!(c.api_key, "dummy");

    env.borrow_mut().extend([("ELISA_MODEL", "env-m"), ("ELISA_MAX_CTX", "777"), ("ELISA_BASE_URL", "http://env:2"), ("ANTHROPIC_API_KEY", "ak")]);
    let c = load().unwrap();
    assert_eq!((c.model.as_str(), c.max_ctx, c.base_url.as_str(), c.api_key.as_str()), ("env-m", Some(777), "http://env:2", "ak"));
    env.borrow_mut().insert("ELISA_API_KEY", "ek");
    assert_eq!(load().unwrap().api_key, "ek");
    env.borrow_mut().insert("ELISA_MAX_CTX", "lots");
    assert!(load().is_err());
    env.borrow_mut().clear();

    // legacy chat/code mode keys (removed 2026-11-01): accepted with any value and ignored
    std::fs::write(d.join("proj/.elisa/config.toml"), "auto_mode = false\nchat_window = \"x\"\nmodel = \"legacy-m\"\n").unwrap();
    assert_eq!(load().unwrap().model, "legacy-m");

    std::fs::write(d.join("proj/.elisa/config.toml"), "modle = \"typo\"\n").unwrap();
    let e = load().unwrap_err();
    assert!(e.contains("proj/.elisa/config.toml") && e.contains("modle"), "{e}");

    let c = Config::default();
    assert_eq!((c.base_url.as_str(), c.model.as_str(), c.api_key.as_str(), c.max_ctx), ("http://127.0.0.1:8898", "", "dummy", None), "no guessed model");
    assert_eq!((c.max_output, c.keep_recent_turns, c.tool_output_max_chars, c.bash_timeout_s, c.max_steps), (2048, 3, 6000, 120, 30));
    assert_eq!(c.auto_approve, ["read", "glob", "grep"]);
    assert!(!c.think && (c.compact_at - 0.80).abs() < 1e-6);
}

// ---------- end to end ----------

#[test]
fn e2e_write_bash_text_and_prefix_rule() {
    let d = tmpdir("e2e");
    let f = Fake::start(vec![
        Sse::new(800, 0).text(&["I'll cre", "ate it."])
            .tool_input("toolu_w", "write", json!({"path":"sub/hello.txt","content":"hello from elisa\n"})).end("tool_use", 20),
        Sse::new(900, 790).tool_input("toolu_b", "bash", json!({"command":"cat sub/hello.txt"})).end("tool_use", 10),
        Sse::new(950, 890).text(&["All ", "do", "ne."]).end("end_turn", 4),
    ]);
    let o = run_elisa(&["-p", "make hello", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(std::fs::read_to_string(d.join("sub/hello.txt")).unwrap(), "hello from elisa\n");
    assert!(o.stdout.trim_end().ends_with("All done."), "stdout: {:?}", o.stdout);
    assert!(o.stderr.contains("reused"), "status line shows reuse: {}", o.stderr);
    assert_eq!(f.left(), 0);

    // max_ctx came from /health
    assert!(f.log().iter().any(|r| r.path == "/health"));
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3);
    assert!(reqs.iter().all(has_tools));
    assert_prefix_rule(&reqs);
    assert_eq!(reqs[0]["messages"].as_array().unwrap().len(), 1);
    let (w, err) = last_tool_result(&reqs[1]);
    assert!(!err && w.contains("wrote") && w.contains("sub/hello.txt"), "{w}");
    let (c, err) = last_tool_result(&reqs[2]);
    assert!(!err && c.contains("hello from elisa") && c.contains("[exit 0]"), "{c}");
    // assistant text + tool_use kept together in one assistant message
    let a = &reqs[1]["messages"][1];
    assert_eq!(a["role"], "assistant");
    assert_eq!(a["content"][0]["text"], "I'll create it.");
    assert_eq!(a["content"][1]["type"], "tool_use");
    for r in &reqs {
        let mt = r["max_tokens"].as_u64().unwrap();
        assert!(mt > 0 && mt <= 2048);
    }
    assert!(d.join(".elisa/sessions").read_dir().unwrap().count() == 1, "session saved");
}

#[test]
fn e2e_without_yes_denies_write() {
    let d = tmpdir("deny");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("t1", "write", json!({"path":"x.txt","content":"nope"})).end("tool_use", 9),
        text_reply("Could not write."),
    ]);
    let o = run_elisa(&["-p", "write x", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!d.join("x.txt").exists());
    let reqs = f.messages();
    assert_eq!(reqs.len(), 2);
    let (c, err) = last_tool_result(&reqs[1]);
    assert!(err && c.starts_with("denied (non-interactive"), "{c}");
    assert!(o.stdout.contains("Could not write."));
}

#[test]
fn e2e_api_error_exits_1() {
    let d = tmpdir("err");
    let f = Fake::start(vec![Resp::Json(503, r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#.into())]);
    let o = run_elisa(&["-p", "hi", "--base-url", &f.url(), "--max-ctx", "8000"], &d, None);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("busy"), "{}", o.stderr);
    assert!(!f.log().iter().any(|r| r.path == "/health"), "--max-ctx skips /health");
}

// ---------- interactive REPL over piped stdin ----------

#[test]
fn repl_clear_writes_notes_reset_does_not() {
    let d = tmpdir("repl");
    // a reply longer than the notes' framing: /clear must come out smaller (a tiny chat keeps no notes)
    let hi = format!("Hi there. {}", "Here is some context about the project. ".repeat(12));
    let f = Fake::start(vec![
        text_reply(&hi),
        text_reply("NOTES-CLR: greeted user"),
        text_reply("Second answer."),
        text_reply("Third answer."),
    ]);
    let input = "hello\n/clear\nagain\n/reset\nthird \\\nline\n/context\n/exit\n";
    let o = run_elisa(&["--base-url", &f.url()], &d, Some(input));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("ctx 4096"), "banner: {}", o.stderr);
    assert!(o.stdout.contains("Hi there.") && o.stdout.contains("Second answer.") && o.stdout.contains("Third answer."));
    assert_eq!(f.left(), 0);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 4, "exactly one summary request");
    // the summary is in context: same system + tools, history + one instruction message (a prefix hit)
    assert_prefix_rule(&reqs[..2]);
    assert!(reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"][0]["text"].as_str().unwrap().contains("will be erased"));
    assert!(std::fs::read_to_string(d.join(".elisa/notes.md")).unwrap().contains("NOTES-CLR"));
    // after /clear: same system prompt, notes in the first (only) message
    assert_eq!(system(&reqs[0]), system(&reqs[2]));
    // ... and the same tools: a reset never drops them (2026-11-01: after a reset the model typed tool calls as text)
    assert!(reqs.iter().all(has_tools));
    assert_eq!(reqs[0]["tools"].to_string(), reqs[2]["tools"].to_string());
    let m2 = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(m2.len(), 1);
    assert!(m2[0]["content"][0]["text"].as_str().unwrap().contains("NOTES-CLR"));
    assert!(m2[0]["content"][0]["text"].as_str().unwrap().contains("written by Elisa, not the user"));
    // after /reset: no summary, history empty, notes kept, continuation line joined
    assert_eq!(system(&reqs[3]), system(&reqs[2]));
    let m = reqs[3]["messages"].as_array().unwrap();
    assert_eq!(m.len(), 1);
    let t = m[0]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("NOTES-CLR") && t.ends_with("## Request\nthird \nline"), "{t}");
}

#[test]
fn repl_ui_status_line() {
    use elisa::types::{Reply, Usage};
    let r = Reply { id: String::new(), content: vec![], stop_reason: "end_turn".into(),
        usage: Usage { input_tokens: 3000, output_tokens: 117, cache_read_input_tokens: 2900, ..Usage::default() }, ttft_s: 0.6, total_s: 5.6 };
    assert_eq!(elisa::ui::status_line(&r, 3100, 12288), "23.4 tok/s · ttft 0.6s · ctx 3.1K/12.3K (25%) · reused 2.9K");
    // a tool-call-only reply: no decode window -> no rate (never "0.0 tok/s"): "—" and the request time
    let t = Reply { ttft_s: 2.0, total_s: 2.0, usage: Usage { input_tokens: 100, output_tokens: 53, cache_read_input_tokens: 3900, ..Usage::default() }, ..r.clone() };
    let st = elisa::ui::status(&t, 4053, 8192);
    assert_eq!(st.tok_s, None);
    assert_eq!(elisa::ui::speed(&st), "— tok/s · 2.0s total");
    assert_eq!(elisa::ui::status_text(&st), "53 tok · — tok/s · 2.0s total · ctx 4.1K/8.2K (49%) · reused 3.9K");
    assert_eq!(elisa::ui::speed(&elisa::ui::status(&r, 3100, 12288)), "23.4 tok/s · ttft 0.6s");
    assert_eq!(elisa::ui::k(999), "999");
}

#[test]
fn repl_approval_prompts_and_prefix_across_turns() {
    let d = tmpdir("approve");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path":"no.txt","content":"x"})).end("tool_use", 5),
        text_reply("Skipped."),
        Sse::new(10, 0).tool_input("w2", "write", json!({"path":"yes.txt","content":"y"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("w3", "write", json!({"path":"yes2.txt","content":"z"})).end("tool_use", 5),
        text_reply("Wrote both."),
    ]);
    // turn 1: deny; turn 2: "maybe" (re-asked) then always -> second write is not asked
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("first\nn\nsecond\nmaybe\na\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!d.join("no.txt").exists() && d.join("yes.txt").exists() && d.join("yes2.txt").exists());
    assert_eq!(o.stderr.matches("allow? [y]es").count(), 3, "{}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 5);
    let (c, err) = last_tool_result(&reqs[1]);
    assert!(err && c == format!("denied by user: {NO_RETRY}"), "{c}");
    assert_prefix_rule(&reqs);
}

/// "always allow" is kept per project in .elisa/allow: a restarted Elisa does not ask for that tool again.
#[test]
fn always_allow_survives_a_restart() {
    let d = tmpdir("always");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path":"a.txt","content":"x"})).end("tool_use", 5),
        text_reply("Wrote it."),
    ]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("first\na\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches("allow? [y]es").count(), 1, "{}", o.stderr);
    assert_eq!(std::fs::read_to_string(d.join(".elisa/allow")).unwrap(), "write\n");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w2", "write", json!({"path":"b.txt","content":"y"})).end("tool_use", 5),
        text_reply("Wrote again."),
    ]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("second\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches("allow? [y]es").count(), 0, "{}", o.stderr);
    assert!(d.join("a.txt").exists() && d.join("b.txt").exists());
}

fn tool_results(req: &Value) -> Vec<String> {
    req["messages"].as_array().unwrap().iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "tool_result").map(|b| b["content"].as_str().unwrap().to_string()).collect()
}

/// ~1.7K-token files (budget at --max-ctx 8192: limit 6144, fold_at 4.0K, mid-turn 5.8K, fold_to 1.8K)
fn big_files(d: &Path, names: &[&str]) {
    for n in names {
        let big: String = (1..=100).map(|i| format!("{n} line {i:03} of a fairly long file with padding\n")).collect();
        std::fs::write(d.join(n), &big).unwrap();
    }
}

fn reply_with(input: usize, t: &str) -> Resp { Sse::new(input, 0).text(&[t]).end("end_turn", 5) }
fn read_call(input: usize, id: &str, path: &str) -> Resp {
    Sse::new(input, 0).tool_input(id, "read", json!({"path": path})).end("tool_use", 5)
}

#[test]
fn repl_fold_at_turn_boundary() {
    let d = tmpdir("boundary");
    big_files(&d, &["a.txt"]);
    let f = Fake::start(vec![
        read_call(3000, "r1", "a.txt"),
        reply_with(5000, "Looked."),
        text_reply("Finished after the fold."),
    ]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d, Some("look at a.txt\nnext\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    assert!(o.stdout.contains("Finished after the fold."));
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3, "{reqs:#?}");
    // the fold ran between the turns (over fold_at after the reply): only the last exchange exists, so nothing is
    // summarised; every tool result of it shrinks to one line, the user text and final reply stay verbatim
    assert_eq!(o.stderr.matches("saved summary").count(), 0, "{}", o.stderr);
    assert_eq!(system(&reqs[0]), system(&reqs[2]), "system prompt is stable");
    let r = tool_results(&reqs[2]);
    assert_eq!(r.len(), 1);
    assert!(r[0].starts_with("[trimmed:"), "{}", r[0]);
    let t = reqs[2]["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("look at a.txt"), "{t}");
    assert!(reqs[2].to_string().contains("Looked."));
}

#[test]
fn repl_mid_turn_fold_once_keeps_current_turn() {
    let d = tmpdir("midturn");
    big_files(&d, &["f1.txt", "f2.txt", "f3.txt"]);
    let f = Fake::start(vec![
        // turn 1: one big read, under fold_at afterwards
        read_call(3000, "r1", "f1.txt"),
        reply_with(3800, "ok"),
        // turn 2: the read does not fit whole: the NEW result is cut (append-only, no fold, no older result touched)
        read_call(4300, "r2", "f2.txt"),
        // the window is full: not even a cut-to-minimum result fits -> the turn's one rewrite, a fold of turn 1
        read_call(5900, "r3", "f3.txt"),
        text_reply("NOTES-T1: read f1"),
        text_reply("done"),
    ]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d, Some("read f1\nread f2 then f3\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "{}", o.stderr);
    assert_eq!(o.stderr.matches("saved summary").count(), 1, "exactly one fold: {}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 6, "{reqs:#?}");
    // request 4 extends request 3 exactly (append-only within the turn), with the new f2 result cut
    let (m3, m4) = (reqs[2]["messages"].as_array().unwrap(), reqs[3]["messages"].as_array().unwrap());
    assert!(m4.len() > m3.len() && m3.iter().zip(m4).all(|(a, b)| a == b), "request 4 rewrote request 3's history");
    let r = tool_results(&reqs[3]);
    assert!(r.last().unwrap().contains("[trimmed "), "{r:?}");
    assert!(!has_tools(&reqs[4]), "the fold's summary request");
    // after the fold: the current request and its read pairs are kept, turn 1 is gone
    let last = &reqs[5];
    let first = last["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(first.contains("NOTES-T1"), "{first}");
    // the request is kept verbatim (its own message, or the `## Request` part of the rebuilt first message)
    assert!(user_texts_of(last).iter().any(|t| elisa::context::request_body(t) == "read f2 then f3"), "the current request is kept verbatim: {:?}", user_texts_of(last));
    assert!(!last.to_string().contains("f1.txt line"), "turn 1's read is folded away");
    let r = tool_results(last);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r[0].contains("f2.txt line 001") && r[1].contains("f3.txt line 001"), "{r:?}");
    assert!(last["max_tokens"].as_u64().unwrap() >= 256);
}

fn user_texts_of(req: &Value) -> Vec<String> {
    req["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"][0]["text"].as_str().map(String::from)).collect()
}

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"].as_array().map(|a| a.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// One mode (2026-11-01): every agent request carries the same code system prompt and the full tool list, across a
/// plain question, a code request, a fold and a resume. Only the fold's transcript summary has its own small system.
#[test]
fn one_mode_same_system_and_tools_everywhere() {
    const SUMMARY_SYSTEM: &str = "You write short, concrete session notes for a coding agent.";
    // none of these turns is about documents or security: those tools stay out (gate_tools = "auto")
    let code = ["read", "write", "edit", "bash", "glob", "grep", "outline", "read_symbol", "edit_symbol", "web_search", "web_fetch"];
    let d = tmpdir("one-mode");
    big_files(&d, &["a.txt"]);
    let f = Fake::start(vec![
        text_reply("A monad is a pattern."),           // plain question
        read_call(3000, "r1", "a.txt"),                 // code request: a tool call ...
        reply_with(5000, "Looked."),                    // ... over fold_at: fold at the turn boundary
        text_reply("NOTES-1: explained monads"),        // the fold's summary of turn 1
        text_reply("You're welcome."),                  // after the fold
    ]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d,
        Some("what is a monad?\nlook at a.txt\nthanks\n/chat\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "{}", o.stderr);
    assert_eq!(o.stderr.matches("saved summary").count(), 1, "one fold: {}", o.stderr);
    assert!(o.stderr.contains("modes were removed"), "{}", o.stderr);
    assert!(!o.stderr.contains("mode:") && !o.stderr.contains("[chat]") && !o.stderr.contains("[code]"), "{}", o.stderr);

    // a resume: a second run continues the saved session (dated back: a run never resumes its own run id, and both
    // runs may fall in the same second)
    let sess = std::fs::read_dir(d.join(".elisa/sessions")).unwrap().map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "json")).unwrap();
    let ts: u64 = sess.file_stem().unwrap().to_str().unwrap().parse().unwrap();
    std::fs::rename(&sess, sess.with_file_name(format!("{}.json", ts - 100))).unwrap();
    let g = Fake::start(vec![text_reply("Still here.")]);
    let o2 = run_elisa(&["-p", "and now?", "--continue", "--base-url", &g.url(), "--max-ctx", "8192"], &d, None);
    assert_eq!(o2.code, 0, "stderr: {}", o2.stderr);
    assert!(o2.stderr.contains("resumed last session"), "{}", o2.stderr);
    assert!(o2.stdout.contains("Still here."));

    let all: Vec<Value> = f.messages().into_iter().chain(g.messages()).collect();
    assert_eq!(all.len(), 6);
    let (summary, agent): (Vec<&Value>, Vec<&Value>) = all.iter().partition(|r| system(r) == SUMMARY_SYSTEM);
    assert_eq!(summary.len(), 1, "only the fold's transcript summary differs");
    assert_eq!(agent.len(), 5);
    for r in &agent {
        assert_eq!(system(r), system(agent[0]), "system prompt changed");
        assert_eq!(r["tools"].to_string(), agent[0]["tools"].to_string(), "tools changed");
        assert_eq!(tool_names(r), code);
    }
    assert!(system(agent[0]).contains("answer plain questions directly"), "{}", system(agent[0]));
    // the resumed request carries the earlier history (post-fold) before the new question
    let last = agent[4]["messages"].as_array().unwrap();
    assert!(last.len() > 1 && agent[4].to_string().contains("You're welcome."), "{last:?}");
}

// ---------- domain watcher ----------

fn watch_view(domain: &str, conf: f64, since: u64, gb: f64) -> Value {
    // the shape of engine/crates/streamdense-cli/src/watcher.rs `status()`: per_layer_cold = cold neuron counts
    let layers: Vec<u64> = (0..64).map(|i| (i % 8) * 32).collect();
    json!({"domain": domain, "confidence": conf, "since_turn": since, "turn": since + 2, "pending": {"domain": "chat-en", "wins": 1},
        "candidates": [{"domain": domain, "score": conf}, {"domain": "agent-tools", "score": 0.1}],
        "decisions_recent": [{"turn": since, "domain": domain, "confidence": conf}], "tokens_observed": 12400,
        "unload_plan": {"domain": domain, "tokens": 12400, "ready": true, "cold_ffn_frac": 0.22, "cold_heads": 3, "gb_unload": gb,
            "ms_saved_per_pass": 8.7, "ms_saved_unload": 6.1, "footprint_gb": 15.8, "fits_cap": true, "per_layer_cold": layers,
            "per_layer_cold_heads": vec![1; 64]},
        "profiles": [{"domain": domain, "tokens": 12400, "windows": 3, "gb_unload": gb}], "mode": "dry", 
        "activity": {"source": "W1 recorder", "jobs": 6},
        "config": {"decide_every_turns": 2, "hysteresis": 2, "confidence": 0.6, "keep_frac": 0.9, "granularity": 64, "dir": "/tmp/w"},
        "note": "dry run: nothing unloaded yet"})
}

fn notes(f: &Fake) -> Vec<Value> {
    f.log().into_iter().filter(|r| r.path == "/v1/streamdense/watcher/note").map(|r| r.json()).collect()
}

#[test]
fn watcher_view_parse_and_lines() {
    use elisa::watcher::{change_line, full_lines, line, WatcherView};
    let v = WatcherView::parse(&watch_view("code-rust", 0.86, 12, 2.1));
    assert_eq!(line(&v), "watcher: code-rust 0.86 · since turn 12 · would unload 2.1 GB (FFN 22% cold, 3 heads) · −8.7 ms/pass · dry run");
    assert_eq!(change_line(&v, Some("chat-en")), "watcher → code-rust (was chat-en) · would unload 2.1 GB");
    let full = full_lines(&v).join("\n");
    assert!(full.contains("## candidates") && full.contains("## profiles") && full.contains("turn 12 → code-rust 0.86"), "{full}");
    assert!(full.contains("L 0–31 ▁") && full.contains("L32–63 ") && full.contains("dry run: nothing unloaded yet"), "{full}");
    // a real gateway record (Test 23): no ts / mode / candidates / plan noise; the confidence is the label's
    let rec = json!({"ts": 1793530744.169173, "turn": 69, "trigger": "state_card", "label": "code-rust", "confidence": 0.493,
        "domain": "chat-vi", "switched": false, "since_turn": 51, "pending": null, "candidates": [{"domain": "code-rust", "score": 0.493}],
        "tokens_window": 308, "sessions": ["s1"], "learned": "chat-vi", "mode": "observe", "plan": {"gb_unload": 0.0}});
    assert_eq!(elisa::watcher::decision(&rec), "turn 69 → chat-vi · label code-rust 0.49 · state_card · since 51 · 308 tok · learned chat-vi");
    let rec = json!({"ts": 1.0, "turn": 51, "trigger": "fold", "label": "chat-vi", "confidence": 0.61, "domain": "chat-vi", "switched": true,
        "since_turn": 51, "pending": ["mixed", 1], "tokens_window": 0, "learned": null, "mode": "observe"});
    assert_eq!(elisa::watcher::decision(&rec), "turn 51 → chat-vi 0.61 · fold · switched · pending mixed 1 · since 51 · 0 tok");
    let full = full_lines(&WatcherView::parse(&json!({"decisions_recent": [rec]}))).join("\n");
    assert!(!full.contains("ts ") && !full.contains("mode observe") && !full.contains("since_turn"), "{full}");
    // odd shapes degrade, never panic
    let w = WatcherView::parse(&json!({"domain": null, "unload_plan": {"cold_heads": [[1, 2], [3]], "per_layer_cold": [{"layer": 0, "cold_ffn_frac": 0.5}]}}));
    assert_eq!((w.plan.cold_heads, w.plan.per_layer_cold.clone()), (3, vec![0.5]));
    assert!(line(&w).starts_with("watcher: learning"));
}

#[test]
fn watcher_notes_on_card_and_fold() {
    let d = tmpdir("watch-notes");
    let f = Fake::start_watch(vec![
        text_reply("Hi.<state>goal: greet the user\nnow: waiting</state>"),
        text_reply("NOTES-W: greeted the user"),
    ], Some(vec![watch_view("chat-en", 0.7, 1, 1.2)]));
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("hello\n/compact\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let n = notes(&f);
    let card = n.iter().find(|x| x["kind"] == "state_card").unwrap_or_else(|| panic!("{n:?}"));
    assert!(card["text"].as_str().unwrap().contains("goal: greet the user"));
    assert_eq!((card["project"].as_str().unwrap(), card["mode"].as_str().unwrap()), (d.to_str().unwrap(), "code"));
    assert!(n.iter().any(|x| x["kind"] == "fold" && x["text"].as_str().unwrap().contains("NOTES-W")), "{n:?}");
    assert!(f.log().iter().any(|r| r.method == "GET" && r.path == "/v1/streamdense/watcher"));
    // plain display: the line once, on the first domain
    assert_eq!(o.stderr.matches("watcher: chat-en 0.70").count(), 1, "{}", o.stderr);
}

#[test]
fn watcher_404_is_quiet() {
    let d = tmpdir("watch-old");
    let f = Fake::start(vec![text_reply("Hi.<state>goal: x\nnow: y</state>"), text_reply("Again.")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("hello\nmore\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stdout.contains("Again."));
    assert!(!o.stderr.to_lowercase().contains("watcher") && !o.stderr.contains("404"), "{}", o.stderr);
    // off after the first 404: no further notes or polls
    let hits = f.log().iter().filter(|r| r.path.starts_with("/v1/streamdense/watcher")).count();
    assert!((1..=3).contains(&hits), "{hits} watcher requests");
}

#[test]
fn watcher_domain_change_line() {
    let d = tmpdir("watch-change");
    let f = Fake::start_watch(vec![text_reply("One."), text_reply("Two.")],
        Some(vec![watch_view("code-rust", 0.86, 1, 2.1), watch_view("chat-vi", 0.8, 2, 1.8)]));
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("one\ntwo\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("watcher → chat-vi (was code-rust) · would unload 1.8 GB"), "{}", o.stderr);
    assert_eq!(o.stderr.matches("watcher → chat-vi").count(), 1, "{}", o.stderr);
}

// ---------- audits ----------

fn with_audits(mut v: Value, audits: Value, scorecard: Value) -> Value {
    v["audits_recent"] = audits;
    v["scorecard"] = scorecard;
    v
}

#[test]
fn audit_lines_and_view_tables() {
    use elisa::watcher::{audit_line, full_lines, Audit, WatcherView};
    let a = |result: &str, cause: &str, at: Option<u64>, full: Option<&str>| Audit {
        message_id: "msg_a".into(), result: result.into(), cause: cause.into(), tokens: 312, diverge_at: at, full_answer: full.map(String::from), ..Default::default()
    };
    assert_eq!(audit_line(&a("match", "", None, None)).unwrap(), "audit ✓ full model agrees (312 tokens)");
    assert_eq!(audit_line(&a("mismatch", "mask", Some(87), Some("x"))).unwrap(),
        "audit ✗ masked weights changed this reply at token 87 · profile tightened · /full shows the full-model answer");
    assert_eq!(audit_line(&a("mismatch", "model", Some(3), None)), None);
    assert!(audit_line(&a("pending", "", None, None)).is_none());
    // the gateway's shapes, leniently: nulls, strings for numbers, junk entries
    let mut g = with_audits(watch_view("code-rust", 0.9, 1, 2.0),
        json!([{"ts": 1.0, "message_id": "msg_a", "domain": "code-rust", "active": true, "result": "match", "reason": null, "tokens": "312", "diverge_at": null, "cause": null, "full_answer": null}, 7,
               {"message_id": "msg_b", "result": "mismatch", "cause": "mask", "diverge_at": 87},
               {"message_id": "msg_c", "domain": "chat-vi", "active": false, "result": "skipped", "reason": "dry run: no mask active, reply came from the full model"}]),
        json!([{"domain": "code-rust", "audited_tokens": 5120, "agreement": 0.975, "mismatches": 2, "skipped": 0, "clean_audits": 7, "go_live_at": 20,
                "points": 7, "required": 20, "resets": 1, "tightened": 2, "live": false},
               {"domain": "chat-vi", "audited_tokens": 0, "agreement": null, "mismatches": 0, "skipped": 4, "clean_audits": 0, "go_live_at": 20, "live": false}]));
    g["audit_queue"] = json!({"pending": 2, "max": 8});
    g["heat"] = json!({"now": {"hot_frac_mean": 0.62, "per_layer_hot": [0.5, 0.7], "coldest_layers": [[3, 0.1], [17, 0.2], [40, 0.3], [41, 0.4]]},
        "session": {"hot_frac_mean": 0.58}, "all": {"hot_frac_mean": "0.55"}, "sample_ms": 250, "samples": 1200});
    let v = WatcherView::parse(&g);
    assert_eq!(v.audits.len(), 3);
    assert_eq!((v.audits[0].tokens, v.audits[0].diverge_at, v.audits[1].diverge_at), (312, None, Some(87)));
    assert_eq!((v.scorecard[0].points, v.scorecard[0].required, v.scorecard[1].skipped), (7, 20, 4));
    assert_eq!((v.turn, v.mode.as_str(), v.pending.clone()), (3, "dry", Some(("chat-en".to_string(), 1))));
    assert_eq!((v.plan.fits_cap, v.plan.ms_saved_unload), (Some(true), 6.1));
    let full = full_lines(&v).join("\n");
    assert!(full.contains("## audit scorecard") && full.contains("97.5%") && full.contains("7/20") && full.contains("dry run"), "{full}");
    let chat = full.lines().find(|l| l.trim_start().starts_with("chat-vi")).unwrap();
    assert!(chat.contains("—") && chat.contains("0/20"), "no agreement before real audits: {chat}");
    assert!(full.contains("## last audits") && full.contains("msg_a code-rust · audit ✓ full model agrees (312 tokens)"), "{full}");
    assert!(full.contains("msg_c chat-vi · audit skipped: dry run: no mask active"), "{full}");
    assert!(full.contains("audit queue: 2 pending (max 8)"), "{full}");
    assert!(full.contains("heat now 0.62 · session 0.58 · all 0.55 hot (coldest L3 L17 L40)"), "{full}");
    assert!(full.contains("cold FFN neurons per layer") && full.contains("activity: W1 recorder · 6 jobs"), "{full}");
    // no heat sent: no line
    assert!(WatcherView::parse(&json!({"heat": {"sample_ms": 250}})).heat.is_none());
}

/// Every reply is audited: a match or skipped audit prints nothing (/watcher only), a mismatch of this session's reply prints its line
/// once and /full shows the full-model answer; another session's audit stays out of the transcript. No vote endpoint.
#[test]
fn session_reply_audits_and_full() {
    let d = tmpdir("audit");
    let base = watch_view("code-rust", 0.86, 1, 2.1);
    let audited = with_audits(base.clone(), json!([
        {"message_id": "msg_zzz", "domain": "code-rust", "result": "mismatch", "cause": "mask", "tokens": 50, "diverge_at": 9},
        {"message_id": "msg_c", "domain": "code-rust", "active": true, "result": "match", "tokens": 50},
        {"message_id": "msg_a", "domain": "code-rust", "active": false, "result": "skipped", "reason": "dry run: no mask active, reply came from the full model"},
        {"message_id": "msg_b", "domain": "code-rust", "active": true, "result": "mismatch", "tokens": 120, "diverge_at": 87, "cause": "mask",
         "full_answer": "## FULL-ANSWER\nThe full model says this."}]),
        json!([{"domain": "code-rust", "audited_tokens": 170, "agreement": 0.5, "mismatches": 1, "skipped": 1, "clean_audits": 1, "go_live_at": 20, "live": false}]));
    let f = Fake::start_watch(vec![Sse::new(10, 0).id("msg_a").text(&["Hi."]).end("end_turn", 2), Sse::new(10, 0).id("msg_b").text(&["More."]).end("end_turn", 2),
        Sse::new(10, 0).id("msg_c").text(&["Third."]).end("end_turn", 2)],
        Some(vec![base, audited]));
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("hello\nmore\nthird\n/full\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let mask = "audit ✗ masked weights changed this reply at token 87 · profile tightened · /full shows the full-model answer";
    assert_eq!(o.stderr.matches(mask).count(), 1, "{}", o.stderr);
    assert!(!o.stderr.contains("skipped") && !o.stderr.contains("dry run: no mask"), "skipped: no line: {}", o.stderr);
    assert!(!o.stderr.contains("full model agrees"), "match: no line: {}", o.stderr);
    assert!(!o.stderr.contains("token 9"), "not this session's reply: {}", o.stderr);
    assert_eq!(o.stderr.matches("audit ").count(), 1, "one audit line in all: {}", o.stderr);
    assert!(o.stderr.contains("── full model · code-rust · differs at token 87 ──") && o.stderr.contains("## FULL-ANSWER\nThe full model says this."), "{}", o.stderr);
    assert!(!f.log().iter().any(|r| r.path.contains("vote")), "no vote endpoint");
    // ids stay local: the API sees role + content only
    for r in f.messages() {
        for m in r["messages"].as_array().unwrap() { assert_eq!(m.as_object().unwrap().keys().collect::<Vec<_>>(), ["role", "content"]); }
    }
    let sess = std::fs::read_dir(d.join(".elisa/sessions")).unwrap().next().unwrap().unwrap().path();
    let saved: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(&sess).unwrap()).unwrap();
    assert!(saved.iter().any(|m| m.id.as_deref() == Some("msg_a")) && saved.iter().any(|m| m.id.as_deref() == Some("msg_b")), "reply ids saved");
}

#[test]
fn full_without_a_watcher() {
    let d = tmpdir("audit-old");
    let f = Fake::start(vec![Sse::new(10, 0).id("msg_a").text(&["Hi."]).end("end_turn", 2)]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("hello\n/full\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!o.stderr.contains("404") && !o.stderr.to_lowercase().contains("error"), "{}", o.stderr);
    assert!(o.stderr.contains("watcher: not available on this gateway"), "{}", o.stderr);
}

// ---------- 2026-11-01 test-elisa regressions ----------

/// A reply that types a tool call as text (session 1793541894: `<tool><tool_name>read_file</tool_name>…`) gets one
/// correction; the real call that follows runs. Requests keep the prefix rule and the tools.
#[test]
fn text_tool_call_gets_one_correction() {
    let d = tmpdir("textcall");
    std::fs::write(d.join("main.py"), "print('hi')\n").unwrap();
    let typed = "Mình cần xem file.\n\n<tool>\n<tool_name>read_file</tool_name>\n<path>main.py</path>\n</tool>";
    let f = Fake::start(vec![
        text_reply(typed),
        read_call(200, "r1", "main.py"),
        text_reply("main.py prints hi."),
    ]);
    let o = run_elisa(&["-p", "đọc main.py", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    assert!(o.stderr.contains("wrote a tool call as text"), "{}", o.stderr);
    assert!(o.stdout.trim_end().ends_with("main.py prints hi."), "{}", o.stdout);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3);
    assert!(reqs.iter().all(has_tools));
    assert_prefix_rule(&reqs);
    let m1 = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(m1[1]["content"][0]["text"], typed, "the typed reply stays in history (append-only)");
    let nudge = m1[2]["content"][0]["text"].as_str().unwrap();
    assert!(nudge.starts_with("[elisa] ") && nudge.contains("read, write, edit, bash, glob, grep"), "{nudge}");
    assert!(tool_results(&reqs[2])[0].contains("print('hi')"));

    // only once per turn: a second typed call ends the turn as a plain reply
    let g = Fake::start(vec![text_reply(typed), text_reply(typed)]);
    let o = run_elisa(&["-p", "đọc main.py", "--base-url", &g.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!((g.messages().len(), g.left()), (2, 0));
    assert_eq!(o.stderr.matches("wrote a tool call as text").count(), 1, "{}", o.stderr);
}

/// A write cut by the output limit arrives whole, with stop_reason "tool_use" (the gateway closes the call at the end
/// of the text; mario.html, 2048 tokens): it is not saved, the model gets the reason once, a second cut stops the turn.
#[test]
fn write_cut_by_output_limit_is_not_saved() {
    let d = tmpdir("cutwrite");
    let cut = || Sse::new(100, 0).tool_input("w1", "write", json!({"path":"game.html","content":"<html>"})).end("tool_use", 2048);
    // a context big enough that the output limit is `max_output` (2048) whatever the tool list costs
    let run = |url: &str| run_elisa_cfg(&["-p", "make a game", "--yes", "--base-url", url], &d, None, &[], "max_ctx = 32768\n");
    let f = Fake::start(vec![cut(), text_reply("I'll use smaller pieces.")]);
    let o = run(&f.url());
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    assert!(!d.join("game.html").exists(), "a cut write is not saved");
    assert!(o.stderr.contains("cut off: `write game.html`"), "{}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 2);
    assert!(tool_results(&reqs[1]).is_empty(), "the cut call did not run");
    let m1 = reqs[1]["messages"].as_array().unwrap();
    assert!(m1[1]["content"].as_array().unwrap().iter().all(|b| b["type"] == "text"), "the cut call is not in history: {:?}", m1[1]);
    let nudge = m1[2]["content"][0]["text"].as_str().unwrap();
    assert!(nudge.starts_with("[elisa] ") && nudge.contains("output limit") && nudge.contains("write game.html"), "{nudge}");

    // only once per turn: a second cut stops the turn and says so
    let g = Fake::start(vec![cut(), cut()]);
    let o = run(&g.url());
    assert_eq!((g.messages().len(), g.left()), (2, 0));
    assert!(!d.join("game.html").exists());
    assert!(o.stderr.contains("[stopped: `write game.html` was cut off"), "{}", o.stderr);

    // under the limit the same call runs
    let h = Fake::start(vec![Sse::new(100, 0).tool_input("w1", "write", json!({"path":"game.html","content":"<html>"})).end("tool_use", 2047), text_reply("Done.")]);
    let o = run(&h.url());
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(d.join("game.html").exists());
}

/// The same call a third time in a turn does not run (session 1793540068 ran one failing command ~15 times; chat QA
/// ran one web search 14 times with slightly different results): the model is told to use the earlier result, and a
/// second blocked step stops the turn (`loop_guard.rs`).
#[test]
fn repeated_identical_tool_call_stops_the_turn() {
    let d = tmpdir("repeat");
    let same = || Sse::new(100, 0).tool_input("b", "bash", json!({"command": "echo boom; exit 3"})).end("tool_use", 5);
    let f = Fake::start(vec![same(), same(), same(), same(), text_reply("never sent")]);
    let o = run_elisa(&["-p", "run it", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("[stopped: the model kept repeating calls that already ran"), "{}", o.stderr);
    assert_eq!(f.messages().len(), 4, "no 5th request");
    assert_eq!(f.left(), 1);
    let sess = std::fs::read_dir(d.join(".elisa/sessions")).unwrap().map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "json")).unwrap();
    let h: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(sess).unwrap()).unwrap();
    let results: Vec<&str> = h.iter().flat_map(|m| &m.content).filter_map(|b| match b { Block::ToolResult { content, .. } => Some(content.as_str()), _ => None }).collect();
    assert_eq!(results.len(), 4);
    assert!(results[1].contains("boom") && !results[1].contains("[elisa:"), "{results:?}");
    assert!(results[2].starts_with("[elisa: not run: this exact call already ran 2 times"), "{results:?}");
    let last = h.last().unwrap();
    assert!(matches!(&last.content[0], Block::Text { text } if text.starts_with("[stopped: the model kept repeating calls")), "{last:?}");

    // a tool the request did not offer (security s1: `sec_malware` in a build without it) is refused before any
    // approval, even under --yes: one short error with real names, and only a `denial` in the audit log
    let d = tmpdir("unknown-tool");
    let f = Fake::start(vec![
        Sse::new(100, 0).tool_input("m", "sec_malware", json!({"path": "."})).end("tool_use", 5),
        text_reply("That tool is not available."),
    ]);
    let o = run_elisa(&["-p", "scan for malware", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    let r = tool_results(&f.messages()[1]).remove(0);
    assert!(r.starts_with("unknown tool `sec_malware`") && r.contains("Available: "), "{r}");
    let audit_dir = d.join("_home/.elisa/audit");
    let lines: Vec<Value> = std::fs::read_dir(&audit_dir).unwrap().flat_map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap()
        .lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect::<Vec<_>>()).filter(|v| v["tool"] == "sec_malware").collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((lines[0]["event"].as_str(), lines[0]["reason"].as_str()), (Some("denial"), Some("unknown_tool")));

    // a write in between resets the count (the command may read what it changed)
    let d = tmpdir("repeat2");
    let f = Fake::start(vec![
        Sse::new(100, 0).tool_input("b", "bash", json!({"command": "cat n.txt"})).end("tool_use", 5),
        Sse::new(100, 0).tool_input("w", "write", json!({"path": "n.txt", "content": "2"})).end("tool_use", 5),
        Sse::new(100, 0).tool_input("b", "bash", json!({"command": "cat n.txt"})).end("tool_use", 5),
        Sse::new(100, 0).tool_input("b", "bash", json!({"command": "cat n.txt"})).end("tool_use", 5),
        text_reply("n is 2."),
    ]);
    std::fs::write(d.join("n.txt"), "1").unwrap();
    let o = run_elisa(&["-p", "check n", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "{}", o.stderr);
    assert!(!o.stderr.contains("same result"), "{}", o.stderr);
}

// ---------- --serve-jsonl (the native app's protocol, src/jsonl.rs) ----------

/// `elisa --serve-jsonl` as the app runs it: commands on stdin, events read line by line from stdout
struct Serve { child: std::process::Child, stdin: std::process::ChildStdin, rx: std::sync::mpsc::Receiver<Value>, seen: Vec<Value> }

impl Serve {
    /// memory off: the turn loop under test, not the graph store
    fn start(url: &str, cwd: &Path) -> Serve { Serve::start_cfg(url, cwd, "memory = false\n") }
    fn start_cfg(url: &str, cwd: &Path, config: &str) -> Serve {
        let home = cwd.join("_home");
        std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
        std::fs::write(home.join(".config/elisa/config.toml"), config).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
        cmd.args(["--serve-jsonl", "--model", "test-m", "--base-url", url, "--cwd"]).arg(cwd).current_dir(cwd).env("HOME", &home)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, _) in std::env::vars() { if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") { cmd.env_remove(&k); } }
        let mut child = cmd.spawn().unwrap();
        let (stdin, out) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let j: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON: {l}: {e}"));
                assert_eq!(j["v"], 1, "{l}");
                if tx.send(j).is_err() { return; }
            }
        });
        Serve { child, stdin, rx, seen: vec![] }
    }
    fn cmd(&mut self, c: Value) { writeln!(self.stdin, "{c}").unwrap(); self.stdin.flush().unwrap(); }
    /// events up to and including the first of type `kind` (panics after 20 s)
    fn until(&mut self, kind: &str) -> Vec<Value> {
        let t0 = Instant::now();
        let mut got = vec![];
        loop {
            let left = Duration::from_secs(20).checked_sub(t0.elapsed()).unwrap_or_else(|| panic!("no {kind} event; got {got:?}"));
            let e = self.rx.recv_timeout(left).unwrap_or_else(|_| panic!("no {kind} event; got {got:?}"));
            self.seen.push(e.clone());
            got.push(e.clone());
            if e["type"] == kind { return got; }
        }
    }
    fn quit(mut self) -> Vec<Value> {
        self.cmd(json!({"type": "quit"}));
        self.until("exit");
        let t0 = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(t0.elapsed() < Duration::from_secs(5), "elisa did not exit after quit");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.seen
    }
}

fn kinds(evs: &[Value]) -> Vec<String> {
    evs.iter().map(|e| e["type"].as_str().unwrap().to_string()).filter(|k| k != "shade" && k != "delta" && k != "watch").collect()
}
fn shade_states(evs: &[Value]) -> Vec<String> {
    evs.iter().filter(|e| e["type"] == "shade").map(|e| e["state"].as_str().unwrap().to_string()).collect()
}

#[test]
fn jsonl_scripted_turn_gives_the_expected_events() {
    let dir = tmpdir("jsonl-turn");
    std::fs::write(dir.join("a.txt"), "alpha\nbeta\n").unwrap();
    let f = Fake::start(vec![
        Sse::new(800, 700).text(&["Look", "ing."]).tool_input("t1", "read", json!({"path": "a.txt"})).end("tool_use", 12),
        Sse::new(900, 850).text(&["Done", " <state>goal: x</state>", "!"]).end("end_turn", 6),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    let hello = s.until("ready");
    // max_ctx 4096 (the fake's /health): system + tools are over 30% of it, so one warning follows the banner
    assert_eq!(kinds(&hello), ["hello", "banner", "info", "ready"]);
    let warn = hello.iter().find(|e| e["type"] == "info").unwrap();
    assert!(warn["text"].as_str().unwrap().starts_with("context: system + tools take "), "{warn}");
    assert!(hello[0]["commands"].as_array().unwrap().iter().any(|c| c[0] == "/help"));
    // the launch greeting plays 2.2 s (as in the TUI); let it end so the turn's states show
    while s.until("shade").last().unwrap()["state"] == "greeting" {}
    s.cmd(json!({"type": "send", "text": "read a.txt"}));
    let evs = s.until("turn_done");
    assert_eq!(kinds(&evs), ["user", "waiting", "reply_done", "tool_start", "tool_detail", "tool_done", "waiting", "reply_done", "card", "turn_done"]);
    let text: String = evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(text, "Looking.Done !", "the state card is not shown as text");
    let get = |k: &str| evs.iter().find(|e| e["type"] == k).unwrap().clone();
    assert_eq!(get("tool_start")["name"], "read");
    assert_eq!(get("tool_detail")["detail"], json!({"kind": "read", "path": "a.txt", "a": 1, "b": 2}));
    assert_eq!((get("tool_done")["is_error"].as_bool(), get("tool_done")["note"].as_str()), (Some(false), Some("2 lines")));
    let st = get("reply_done");
    assert_eq!((st["max_ctx"].as_u64(), st["reused"].as_u64()), (Some(4096), Some(700)));
    let shades = shade_states(&s.seen);
    for want in ["thinking", "reading", "streaming"] { assert!(shades.iter().any(|x| x == want), "{want} in {shades:?}"); }
    // the same speed rules as the TUI: one fixed system turn, history only appended
    assert_prefix_rule(&f.messages());
    s.until("ready");
    let all = s.quit();
    assert_eq!(all.last().unwrap()["type"], "exit");
}

#[test]
fn jsonl_approval_round_trip_yes_and_no() {
    let dir = tmpdir("jsonl-approve");
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "write", json!({"path": "new.txt", "content": "hi\n"})).end("tool_use", 9),
        text_reply("wrote it"),
        Sse::new(900, 0).tool_input("t2", "bash", json!({"command": "echo nope > x.txt"})).end("tool_use", 9),
        text_reply("ok, not run"),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "make new.txt"}));
    let evs = s.until("approve");
    assert_eq!((evs.last().unwrap()["tool"].as_str(), evs.last().unwrap()["summary"].as_str()), (Some("write"), Some("write new.txt (1 lines)")));
    assert!(evs.last().unwrap().get("server").is_none(), "Elisa's own tool: no server field");
    let d = evs.iter().find(|e| e["type"] == "tool_detail").unwrap();
    assert_eq!((d["detail"]["kind"].as_str(), d["detail"]["add"].as_u64()), (Some("new file"), Some(1)));
    assert!(!dir.join("new.txt").exists(), "nothing written before the answer");
    // NEEDS YOU goes out before the approval line
    assert_eq!(evs.iter().rev().find(|e| e["type"] == "shade").unwrap()["state"], "permission");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    let evs = s.until("tool_done");
    assert_eq!(evs.last().unwrap()["is_error"], false);
    assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "hi\n");
    s.until("turn_done");
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "now the bash"}));
    let evs = s.until("approve");
    assert!(evs.last().unwrap().get("server").is_none(), "{}", evs.last().unwrap());
    s.cmd(json!({"type": "approve", "answer": "no"}));
    let evs = s.until("tool_done");
    assert_eq!((evs.last().unwrap()["note"].as_str(), evs.last().unwrap()["is_error"].as_bool()), (Some("denied"), Some(true)));
    s.until("turn_done");
    assert!(!dir.join("x.txt").exists());
    let reqs = f.messages();
    assert_eq!(last_tool_result(&reqs[1]), ("wrote 1 lines to new.txt".into(), false));
    assert_eq!(last_tool_result(&reqs[3]), (format!("denied by user: {NO_RETRY}"), true));
    s.quit();
}

/// An edit that cannot change anything (old == new, old not found) and a write to a URL fail with a tool error
/// before any approval prompt (`tools::precheck`).
#[test]
fn useless_edit_fails_without_an_approval_prompt() {
    let dir = tmpdir("jsonl-precheck");
    std::fs::write(dir.join("calc.py"), "x = 1\n").unwrap();
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "edit", json!({"path": "calc.py", "old": "x = 1", "new": "x = 1"})).end("tool_use", 9),
        Sse::new(850, 0).tool_input("t2", "edit", json!({"path": "calc.py", "old": "y = 2", "new": "y = 3"})).end("tool_use", 9),
        Sse::new(900, 0).tool_input("t3", "write", json!({"path": "https://doc.rust-lang.org/std/index.html", "content": "hi\n"})).end("tool_use", 9),
        text_reply("nothing to do"),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "fix calc"}));
    let evs = s.until("turn_done");
    assert!(!evs.iter().any(|e| e["type"] == "approve"), "no approval prompt: {evs:?}");
    let done: Vec<&Value> = evs.iter().filter(|e| e["type"] == "tool_done").collect();
    assert_eq!(done.len(), 3, "{evs:?}");
    assert!(done.iter().all(|e| e["is_error"] == true), "{done:?}");
    let reqs = f.messages();
    assert!(last_tool_result(&reqs[1]).0.contains("no change"), "{:?}", last_tool_result(&reqs[1]));
    assert!(last_tool_result(&reqs[2]).0.contains("old not found"), "{:?}", last_tool_result(&reqs[2]));
    assert!(last_tool_result(&reqs[3]).0.contains("is a URL"), "{:?}", last_tool_result(&reqs[3]));
    assert_eq!(std::fs::read_to_string(dir.join("calc.py")).unwrap(), "x = 1\n");
    assert!(!dir.join("https:").exists());
    s.quit();
}

#[test]
fn jsonl_interrupt_stops_a_streaming_reply() {
    let dir = tmpdir("jsonl-esc");
    // ~3,000 deltas at 3 ms each: ~9 s if nobody stops it
    let words: Vec<String> = (0..3000).map(|i| format!("w{i} ")).collect();
    let pieces: Vec<&str> = words.iter().map(String::as_str).collect();
    let f = Fake::start(vec![Sse::new(500, 0).text(&pieces).end("end_turn", 3000), text_reply("after")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "talk a lot"}));
    s.until("delta");
    let t0 = Instant::now();
    s.cmd(json!({"type": "interrupt"}));
    let evs = s.until("turn_done");
    assert!(t0.elapsed() < Duration::from_secs(3), "interrupt took {:?}", t0.elapsed());
    assert!(evs.iter().any(|e| e["type"] == "info" && e["text"] == "[cancelled]"), "{:?}", kinds(&evs));
    assert!(shade_states(&s.seen).iter().any(|x| x == "interrupted"));
    // the session goes on: the next message is a normal turn
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "again"}));
    let evs = s.until("turn_done");
    let text: String = evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(text, "after");
    s.quit();
}

#[test]
fn jsonl_care_and_queue_while_busy() {
    let dir = tmpdir("jsonl-care");
    let f = Fake::start(vec![text_reply("one"), text_reply("two")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    // /water never reaches the agent: Shade drinks
    s.cmd(json!({"type": "send", "text": "/water"}));
    let evs = s.until("shade");
    let evs = if evs.last().unwrap()["state"] == "drinking" { evs } else { s.until("shade") };
    assert_eq!(evs.last().unwrap()["state"], "drinking");
    // two messages at once: the second is queued and runs after the first
    s.cmd(json!({"type": "send", "text": "first"}));
    s.cmd(json!({"type": "send", "text": "second"}));
    s.until("turn_done");
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "user" && e["text"] == "second"));
    assert_eq!(f.messages().len(), 2);
    assert!(s.seen.iter().any(|e| e["type"] == "queued" && e["n"] == 1));
    s.quit();
}

// ---------- check before done (src/checks.rs) ----------

/// a project whose check is `sh check.sh`: passes when main.txt says GOOD, else prints one error naming the content
fn check_project(tag: &str, toml: &str) -> PathBuf {
    let d = tmpdir(tag);
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/checks.toml"), toml).unwrap();
    std::fs::write(d.join("check.sh"), "grep -q GOOD main.txt && exit 0\necho \"main.txt:1:1: error: main.txt is $(cat main.txt)\"\nexit 1\n").unwrap();
    std::fs::write(d.join("test.sh"), "echo ok\n").unwrap();
    d
}
fn write_call(id: &str, content: &str) -> Resp {
    Sse::new(800, 0).tool_input(id, "write", json!({"path": "main.txt", "content": content})).end("tool_use", 9)
}
fn last_user_text(req: &Value) -> String {
    let m = req["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(m["role"], "user");
    m["content"][0]["text"].as_str().unwrap_or("").to_string()
}

#[test]
fn check_fails_model_fixes_check_passes() {
    let d = check_project("chk-fix", "commands = [\"sh check.sh\"]\ntests = [\"sh test.sh\"]\n");
    let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done"), write_call("w2", "GOOD\n"), text_reply("fixed")]);
    let o = run_elisa(&["-p", "make main.txt", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    assert_eq!(std::fs::read_to_string(d.join("main.txt")).unwrap(), "GOOD\n");
    assert!(o.stdout.trim_end().ends_with("fixed"), "{}", o.stdout);
    assert!(o.stderr.contains("check: sh check.sh ✗ 1 error → fixing (1/2)"), "{}", o.stderr);
    assert!(o.stderr.contains("check: sh check.sh ✓") && o.stderr.contains("check: sh test.sh ✓"), "{}", o.stderr);
    // the test stage ran only after the fast check passed
    assert_eq!(o.stderr.matches("check: sh test.sh").count(), 2, "running + pass once: {}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 4);
    let note = last_user_text(&reqs[2]);
    assert!(note.starts_with("[elisa] check failed after your edits (fix round 1 of 2): `sh check.sh` exited 1 with 1 error:"), "{note}");
    assert!(note.contains("main.txt:1:1: error: main.txt is BAD"), "{note}");
    // the failed reply stays, the note is appended: KV prefix reuse holds
    assert_eq!(reqs[2]["messages"].as_array().unwrap().iter().rev().nth(1).unwrap()["content"][0]["text"], "done");
    assert_prefix_rule(&reqs);
}

#[test]
fn check_same_error_twice_stops() {
    let d = check_project("chk-same", "commands = [\"sh check.sh\"]\n");
    // check.sh names the content: "BAD" both times = the same error
    let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done"), write_call("w2", "BAD\n"), text_reply("fixed?")]);
    let o = run_elisa(&["-p", "make main.txt", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0);
    assert_eq!(f.messages().len(), 4, "no third fix round");
    assert!(o.stderr.contains("check: sh check.sh ✗ 1 error · same errors again → stopped"), "{}", o.stderr);
    assert_prefix_rule(&f.messages());
}

#[test]
fn check_rounds_used_up_stops_and_no_edit_stops() {
    // rounds = 1: a new error after the fix round still ends the turn
    let d = check_project("chk-rounds", "rounds = 1\ncommands = [\"sh check.sh\"]\n");
    let f = Fake::start(vec![write_call("w1", "A\n"), text_reply("done"), write_call("w2", "B\n"), text_reply("fixed?")]);
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!((o.code, f.left(), f.messages().len()), (0, 0, 4), "{}", o.stderr);
    assert!(o.stderr.contains("check: sh check.sh ✗ 1 error → fixing (1/1)"), "{}", o.stderr);
    assert!(o.stderr.contains("check: sh check.sh ✗ 1 error · 1 fix round used → stopped"), "{}", o.stderr);
    // the model answers the note without editing: reported, not re-checked, not silent
    let d = check_project("chk-noedit", "commands = [\"sh check.sh\"]\n");
    let f = Fake::start(vec![write_call("w1", "A\n"), text_reply("done"), text_reply("cannot fix")]);
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!((o.code, f.left(), f.messages().len()), (0, 0, 3), "{}", o.stderr);
    assert!(o.stderr.contains("check: sh check.sh ✗ 1 error · no edit in the fix round → stopped"), "{}", o.stderr);
}

#[test]
fn checks_disabled_run_nothing() {
    for (toml, flag) in [("commands = [\"sh check.sh\"]\n", true), ("enabled = false\ncommands = [\"sh check.sh\"]\n", false)] {
        let d = check_project("chk-off", toml);
        let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done")]);
        let mut args = vec!["-p", "x", "--yes", "--base-url"];
        let url = f.url();
        args.push(&url);
        if flag { args.push("--no-checks"); }
        let o = run_elisa(&args, &d, None);
        assert_eq!((o.code, f.left(), f.messages().len()), (0, 0, 2), "{}", o.stderr);
        assert!(!o.stderr.contains("check:"), "{}", o.stderr);
    }
    // config key `checks = false` (project config) does the same
    let d = check_project("chk-cfg", "commands = [\"sh check.sh\"]\n");
    std::fs::write(d.join(".elisa/config.toml"), "checks = false\n").unwrap();
    let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done")]);
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!((o.code, f.messages().len()), (0, 2), "{}", o.stderr);
    assert!(!o.stderr.contains("check:"), "{}", o.stderr);
    // a turn without edits runs no checks either
    let d = check_project("chk-noedits", "commands = [\"sh check.sh\"]\n");
    let f = Fake::start(vec![text_reply("just an answer")]);
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert!(o.code == 0 && !o.stderr.contains("check:"), "{}", o.stderr);
}

#[test]
fn check_timeout_is_reported_without_a_fix_round() {
    let d = check_project("chk-timeout", "timeout_s = 1\ncommands = [\"sleep 5\", \"sh check.sh\"]\n");
    let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done")]);
    let t0 = Instant::now();
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert!(t0.elapsed() < Duration::from_secs(4), "the sleep was killed: {:?}", t0.elapsed());
    assert_eq!((o.code, f.left(), f.messages().len()), (0, 0, 2), "{}", o.stderr);
    assert!(o.stderr.contains("check: sleep 5 ⏱ timed out after 1s · remaining checks skipped"), "{}", o.stderr);
    assert!(!o.stderr.contains("check: sh check.sh"), "the rest is skipped: {}", o.stderr);
}

#[test]
fn jsonl_check_events() {
    let dir = check_project("jsonl-chk", "commands = [\"sh check.sh\"]\ntests = [\"sh test.sh\"]\n");
    let f = Fake::start(vec![write_call("w1", "BAD\n"), text_reply("done"), write_call("w2", "GOOD\n"), text_reply("fixed")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "make main.txt"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    let evs = s.until("turn_done");
    let checks: Vec<(String, String, u64, String)> = evs.iter().filter(|e| e["type"] == "check").map(|e| (
        e["cmd"].as_str().unwrap().to_string(), e["status"].as_str().unwrap().to_string(), e["round"].as_u64().unwrap(),
        e["summary"].as_str().unwrap().to_string())).collect();
    let want: Vec<(String, String, u64, String)> = [("sh check.sh", "pass", 1, ""), ("sh test.sh", "running", 1, ""), ("sh test.sh", "pass", 1, "")]
        .iter().map(|(a, b, c, d)| (a.to_string(), b.to_string(), *c, d.to_string())).collect();
    let all: Vec<_> = s.seen.iter().filter(|e| e["type"] == "check").cloned().collect();
    assert_eq!(all.len(), 6, "{all:?}");
    assert_eq!((all[0]["status"].as_str(), all[1]["status"].as_str(), all[1]["summary"].as_str()), (Some("running"), Some("fail"), Some("1 error")));
    assert_eq!(all[1]["line"], "check: sh check.sh ✗ 1 error → fixing (1/2)");
    assert_eq!(all[1]["rounds"], 2);
    assert_eq!(checks[checks.len() - 3..], want[..]);
    // the fail flashed error; the passing test stage ends the turn in a celebration ("tests green", a flash after turn_done)
    assert!(shade_states(&s.seen).iter().any(|x| x == "error"), "{:?}", shade_states(&s.seen));
    while s.until("shade").last().unwrap()["state"] != "celebrating" {}
    s.quit();
}

// ---------- syntax-tree tools, parse guard, targeted checks (src/ast.rs) ----------

const LIB_RS: &str = "/// Adds.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub struct Calc { total: i32 }\n\nimpl Calc {\n    pub fn push(&mut self, x: i32) {\n        self.total += x;\n    }\n}\n";

/// a project whose check is `sh check.sh` (passes when src/lib.rs has `a * b`)
fn ast_project(tag: &str) -> PathBuf {
    let d = tmpdir(tag);
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(d.join(".elisa/checks.toml"), "commands = [\"sh check.sh\"]\n").unwrap();
    std::fs::write(d.join("check.sh"), "grep -q 'a \\* b' src/lib.rs && exit 0\necho 'src/lib.rs:3:5: error: add must multiply'\nexit 1\n").unwrap();
    d
}

#[test]
fn outline_read_symbol_edit_symbol_then_check_passes() {
    let d = ast_project("ast-flow");
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("o", "outline", json!({"path": "src/lib.rs"})).end("tool_use", 9),
        Sse::new(900, 0).tool_input("r", "read_symbol", json!({"path": "src/lib.rs", "name": "add"})).end("tool_use", 9),
        Sse::new(950, 0).tool_input("e", "edit_symbol", json!({"path": "src/lib.rs", "name": "add",
            "source": "/// Multiplies.\npub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}"})).end("tool_use", 9),
        text_reply("add multiplies now."),
    ]);
    let o = run_elisa(&["-p", "make add multiply", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!((o.code, f.left()), (0, 0), "{}", o.stderr);
    let reqs = f.messages();
    let (outline, err) = last_tool_result(&reqs[1]);
    assert!(!err);
    assert_eq!(outline, "src/lib.rs · rust · 12 lines · 4 symbols\n1-4 pub fn add(a: i32, b: i32) -> i32\n6 pub struct Calc\n8-12 impl Calc\n  9-11 pub fn push(&mut self, x: i32)");
    let (sym, err) = last_tool_result(&reqs[2]);
    assert!(!err);
    assert_eq!(sym, "src/lib.rs:1-4 fn add\n1\t/// Adds.\n2\tpub fn add(a: i32, b: i32) -> i32 {\n3\t    a + b\n4\t}");
    let (edit, err) = last_tool_result(&reqs[3]);
    assert_eq!((edit.as_str(), err), ("replaced fn add in src/lib.rs (lines 1-4 → 1-4)", false));
    assert_eq!(std::fs::read_to_string(d.join("src/lib.rs")).unwrap(), LIB_RS.replace("/// Adds.", "/// Multiplies.").replace("a + b", "a * b"));
    assert!(o.stderr.contains("check: sh check.sh ✓"), "{}", o.stderr);
    assert!(!o.stderr.contains("✗"), "{}", o.stderr);
    assert_prefix_rule(&reqs);
}

#[test]
fn broken_edit_is_refused_then_fixed() {
    let d = ast_project("ast-guard");
    let f = Fake::start(vec![
        // drops a closing brace: refused, nothing written
        Sse::new(800, 0).tool_input("e1", "edit", json!({"path": "src/lib.rs", "old": "    a + b\n}", "new": "    a * b\n"})).end("tool_use", 9),
        Sse::new(900, 0).tool_input("e2", "edit", json!({"path": "src/lib.rs", "old": "    a + b\n}", "new": "    a * b\n}"})).end("tool_use", 9),
        text_reply("fixed"),
    ]);
    let o = run_elisa(&["-p", "make add multiply", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!((o.code, f.left()), (0, 0), "{}", o.stderr);
    let reqs = f.messages();
    let (msg, err) = last_tool_result(&reqs[1]);
    assert!(err, "{msg}");
    assert!(msg.starts_with("refused: this change would add 1 syntax error to src/lib.rs (rust parser); the file is unchanged.\n"), "{msg}");
    // line:col, the source line and a caret
    assert!(msg.contains("\n12:2: missing `}`\n") || msg.lines().nth(1).is_some_and(|l| l.split(':').nth(1).is_some_and(|c| c.parse::<usize>().is_ok())), "{msg}");
    assert!(msg.contains(" | ") && msg.contains('^'), "{msg}");
    let (ok, err) = last_tool_result(&reqs[2]);
    assert_eq!((ok.as_str(), err), ("edited src/lib.rs (1 replacement)", false));
    assert!(std::fs::read_to_string(d.join("src/lib.rs")).unwrap().contains("    a * b\n}\n"));
    // the refused edit is not an edit: one check, it passes
    assert_eq!(o.stderr.matches("check: sh check.sh ✓").count(), 1, "{}", o.stderr);
    // config ast_guard = false: the same broken edit is written
    let d = ast_project("ast-guard-off");
    std::fs::write(d.join(".elisa/config.toml"), "ast_guard = false\nchecks = false\n").unwrap();
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("e1", "edit", json!({"path": "src/lib.rs", "old": "    a + b\n}", "new": "    a * b\n"})).end("tool_use", 9),
        text_reply("done"),
    ]);
    let o = run_elisa(&["-p", "x", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(last_tool_result(&f.messages()[1]), ("edited src/lib.rs (1 replacement)".into(), false));
}

#[test]
fn ast_off_drops_the_tools_and_the_prompt_line() {
    let d = tmpdir("ast-off");
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "ast = false\n").unwrap();
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa(&["-p", "hello", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = &f.messages()[0];
    // a plain turn: the documents and security tools are left out (gate_tools = "auto")
    assert_eq!(tool_names(r), ["read", "write", "edit", "bash", "glob", "grep", "web_search", "web_fetch"]);
    assert!(!system(r).contains("outline"), "{}", system(r));
    let g = Fake::start(vec![text_reply("hi")]);
    let d2 = tmpdir("ast-on");
    run_elisa(&["-p", "hello", "--base-url", &g.url()], &d2, None);
    assert!(system(&g.messages()[0]).contains("`outline`, then `read_symbol`"));
}

#[test]
fn jsonl_symbol_tools_rows_and_card_number() {
    let dir = ast_project("jsonl-ast");
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("o", "outline", json!({"path": "src/lib.rs"})).end("tool_use", 9),
        Sse::new(800, 0).tool_input("r", "read_symbol", json!({"path": "src/lib.rs", "name": "Calc::push"})).end("tool_use", 9),
        Sse::new(800, 0).tool_input("e", "edit_symbol", json!({"path": "src/lib.rs", "name": "add", "source": "pub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}"})).end("tool_use", 9),
        Sse::new(900, 0).text(&["Done <state>goal: x</state>"]).end("end_turn", 6),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    while s.until("shade").last().unwrap()["state"] == "greeting" {}
    s.cmd(json!({"type": "send", "text": "make add multiply"}));
    let evs = s.until("approve");
    let done: Vec<&Value> = evs.iter().filter(|e| e["type"] == "tool_done").collect();
    assert_eq!((done[0]["note"].as_str(), done[1]["note"].as_str()), (Some("4 symbols"), Some("lines 9–11")));
    let det: Vec<&Value> = evs.iter().filter(|e| e["type"] == "tool_detail").collect();
    assert_eq!(det[1]["detail"], json!({"kind": "read", "path": "src/lib.rs", "a": 9, "b": 11}));
    assert_eq!((det[2]["name"].as_str(), det[2]["detail"]["kind"].as_str(), det[2]["detail"]["add"].as_u64(), det[2]["detail"]["del"].as_u64()),
        (Some("edit_symbol"), Some("edit"), Some(1), Some(2)), "{:?}", det[2]);
    assert_eq!(evs.last().unwrap()["tool"], "edit_symbol");
    let shades = shade_states(&s.seen);
    assert!(shades.iter().any(|x| x == "reading"), "{shades:?}");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    let evs = s.until("turn_done");
    let card = evs.iter().find(|e| e["type"] == "card").unwrap();
    assert_eq!((card["text"].as_str(), card["n"].as_u64()), (Some("goal: x"), Some(1)), "memory off: the count in this run");
    s.quit();
}

/// The scripted demo (`--serve-jsonl --demo-states`) on its real crate: real cargo check / clippy / test
fn demo_run(tag: &str, prompt: &str) -> (Out, PathBuf) {
    let proj = elisa::demo_gateway::project_in(&tmpdir(tag)).unwrap();
    let url = elisa::demo_gateway::start().unwrap();
    let home = std::env::var("HOME").unwrap();
    let env = [("CARGO_HOME", std::env::var("CARGO_HOME").unwrap_or(format!("{home}/.cargo"))),
        ("RUSTUP_HOME", std::env::var("RUSTUP_HOME").unwrap_or(format!("{home}/.rustup")))];
    let o = run_elisa_env(&["-p", prompt, "--yes", "--base-url", &url], &proj, None, &env);
    (o, proj)
}

#[test]
fn demo_symbols_scenario_guard_refuses_then_checks_pass() {
    let (o, proj) = demo_run("demo-sym", "edit split_words by symbol");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("refused"), "the broken edit_symbol was refused: {}", o.stderr);
    assert!(std::fs::read_to_string(proj.join("crates/core/src/split.rs")).unwrap().contains("input.char_indices()"));
    for c in ["check: cargo check ✓", "check: cargo test ✓"] { assert!(o.stderr.contains(c), "{c}: {}", o.stderr); }
    assert!(!o.stderr.contains("✗"), "{}", o.stderr);
    assert!(o.stdout.contains("Replaced `split_words` by name"), "{}", o.stdout);
}

#[test]
fn demo_fix_ends_green_and_check_scenario_fails_then_fixes() {
    // the normal demo: edits, the tests script (asks: --yes), then the checks pass
    let (o, _) = demo_run("demo-fix", "fix the split test");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("check: cargo check ✓") && o.stderr.contains("check: cargo test ✓"), "{}", o.stderr);
    assert!(!o.stderr.contains("✗"), "{}", o.stderr);
    // the check scenario: compiles only after one fix round
    let (o, proj) = demo_run("demo-chk", "make the check fail");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("check: cargo check ✗") && o.stderr.contains("→ fixing (1/2)"), "{}", o.stderr);
    assert!(o.stderr.contains("check: cargo check ✓") && o.stderr.contains("check: cargo test ✓"), "{}", o.stderr);
    assert!(!std::fs::read_to_string(proj.join("crates/core/src/split.rs")).unwrap().contains("as u8"));
}

#[test]
fn metrics_lines_and_stats() {
    let d = tmpdir("metrics");
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/lib.rs"), "pub fn load(name: &str) -> String {\n    name.to_string()\n}\n").unwrap();
    let f = Fake::start(vec![
        Sse::new(300, 0).text(&["Done ", "first."]).end("end_turn", 40),
        Sse::new(120, 880).text(&["Done again."]).end("end_turn", 12),
    ]);
    let input = "fix load so it trims the name\nfix load so it trims the name\n/stats\n/exit\n";
    let o = run_elisa(&["--base-url", &f.url(), "--no-checks"], &d, Some(input));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "stderr: {}", o.stderr);
    let text = std::fs::read_to_string(d.join(".elisa/metrics.jsonl")).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{text}");
    let (a, b) = (&lines[0], &lines[1]);
    assert_eq!(a["outcome"], "done");
    assert_eq!(a["prompt"], "fix load so it trims the name");
    assert_eq!(a["calls"].as_array().unwrap().len(), 1);
    assert_eq!(a["calls"][0]["kind"], "agent");
    assert_eq!(a["calls"][0]["input_tokens"], 300);
    assert_eq!(a["calls"][0]["cached_input_tokens"], 0);
    assert_eq!(a["calls"][0]["output_tokens"], 40);
    assert_eq!(a["by_kind"]["agent"]["calls"], 1);
    for k in ["ttft_s", "total_s"] { assert!(a["calls"][0][k].as_f64().unwrap() >= 0.0); }
    assert!(a["calls"][0].get("decode_tok_s").is_some());
    let (wall, model, other) = (a["wall_s"].as_f64().unwrap(), a["totals"]["model_s"].as_f64().unwrap(), a["other_s"].as_f64().unwrap());
    assert!(wall + 1e-6 >= model && (wall - model - other).abs() < 1e-6, "{a}");
    assert_eq!(b["calls"][0]["cached_input_tokens"], 880);
    assert_eq!(b["totals"]["output_tokens"], 12);
    // /stats: the last request and the session totals
    assert!(o.stderr.contains("last request (done)"), "{}", o.stderr);
    assert!(o.stderr.contains("session: 2 requests"), "{}", o.stderr);

    // metrics = false: no file, /stats says so
    let d2 = tmpdir("metrics-off");
    std::fs::create_dir_all(d2.join(".elisa")).unwrap();
    std::fs::write(d2.join(".elisa/config.toml"), "metrics = false\n").unwrap();
    let f2 = Fake::start(vec![text_reply("Four.")]);
    let o2 = run_elisa(&["--base-url", &f2.url(), "--no-checks"], &d2, Some("what is two plus two\n/stats\n/exit\n"));
    assert_eq!(o2.code, 0, "stderr: {}", o2.stderr);
    assert!(!d2.join(".elisa/metrics.jsonl").exists());
    assert!(o2.stderr.contains("metrics are off"), "{}", o2.stderr);
}

// ---------- ui_run1 fixes: one step's results vs the context; warm-ups ----------

/// Tokens of a request by src/context.rs's estimate (3.3 chars/token, 6 per message, tools + 240 preamble).
fn est_tokens(req: &Value) -> usize {
    let tok = |s: &str| (s.chars().count() * 10).div_ceil(33);
    let block = |b: &Value| match b["type"].as_str().unwrap_or("") {
        "text" => tok(b["text"].as_str().unwrap_or("")),
        "tool_use" => tok(b["id"].as_str().unwrap_or("")) + tok(b["name"].as_str().unwrap_or("")) + tok(&b["input"].to_string()),
        "tool_result" => tok(b["tool_use_id"].as_str().unwrap_or("")) + tok(b["content"].as_str().unwrap_or("")),
        _ => 0,
    };
    let fixed = tok(req["system"].as_str().unwrap_or("")) + req.get("tools").map_or(0, |t| tok(&t.to_string()) + 240);
    fixed + req["messages"].as_array().unwrap().iter().map(|m| 6 + m["content"].as_array().map_or(0, |a| a.iter().map(block).sum())).sum::<usize>()
}

fn four_reads(input: usize, tag: &str) -> Resp {
    let mut s = Sse::new(input, 0);
    for i in 1..=4 { s = s.tool_input(&format!("{tag}{i}"), "read", json!({"path": format!("f{i}.txt")})); }
    s.end("tool_use", 20)
}

/// ui_run1 A: four parallel reads (25 KB) made the next request 10.1K tokens against 8192, and the fold after it
/// kept them and overflowed again. Now one step's results are cut to what the window has left (largest first,
/// `[trimmed N chars]`), and every request (also after the folds) stays under the limit (8192 - 2048 output).
#[test]
fn parallel_tool_results_are_capped_to_the_context_and_folds_stay_under_the_limit() {
    let d = tmpdir("par-cap");
    for i in 1..=4 {
        let big: String = (1..=110).map(|l| format!("{{\"f{i}\": {l:03}, \"pad\": \"a fairly long json-ish line of padding\"}}\n")).collect();
        std::fs::write(d.join(format!("f{i}.txt")), &big).unwrap();
    }
    std::fs::write(d.join("small.txt"), "tiny\n").unwrap();
    let f = Fake::start(vec![
        four_reads(1500, "a"), text_reply("read all four"),
        four_reads(1500, "b"), text_reply("read them again"),
        text_reply("Ok."), text_reply("Ok."), text_reply("Ok."),
    ]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192", "--yes"], &d, Some("read f1..f4 STEP1\nread them all again STEP2\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let reqs = f.messages();
    for (i, r) in reqs.iter().enumerate() {
        assert!(est_tokens(r) <= 6144, "request {i} is {} tokens (limit 6144): {}", est_tokens(r), o.stderr);
    }
    // the step after the four reads: all four results there, the big ones cut with the marker, each a fair share
    let r1 = &reqs[1];
    let res = tool_results(r1);
    assert_eq!(res.len(), 4, "{res:?}");
    assert!(res.iter().all(|c| c.contains("[trimmed ") && c.contains("chars]")), "{res:?}");
    let lens: Vec<usize> = res.iter().map(|c| c.chars().count()).collect();
    assert!(lens.iter().max().unwrap() - lens.iter().min().unwrap() < 200, "water-filled: {lens:?}");
    assert!(o.stderr.contains("trimmed to fit the context"), "{}", o.stderr);
}

#[test]
fn a_fold_sends_the_folded_prompt_as_a_warm_up_when_the_gateway_takes_one() {
    let d = tmpdir("warm");
    big_files(&d, &["a.txt"]);
    let f = Fake::start(vec![read_call(3000, "r1", "a.txt"), reply_with(5000, "Looked."), text_reply("Finished after the fold.")]);
    f.warm.store(true, Ordering::SeqCst);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d, Some("look at a.txt\nnext\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let (reqs, warm) = (f.messages(), f.warmups());
    assert_eq!((reqs.len(), warm.len()), (3, 1), "{warm:#?}");
    // every request carries the session's project id, warm-ups included
    let pid = elisa::api::project_id(&d);
    let posts: Vec<Recorded> = f.log().into_iter().filter(|r| r.path == "/v1/messages").collect();
    assert!(posts.iter().any(|r| r.header("x-streamdense-warm").is_some()));
    assert!(posts.iter().all(|r| r.header(elisa::api::PROJECT_HEADER) == Some(pid.as_str())), "{posts:?}");
    let w = &warm[0];
    assert_eq!(w["max_tokens"], 0);
    assert!(w.get("stream").is_none());
    assert_eq!((&w["system"], &w["tools"]), (&reqs[2]["system"], &reqs[2]["tools"]));
    let (wm, next) = (w["messages"].as_array().unwrap(), reqs[2]["messages"].as_array().unwrap());
    assert_eq!(&next[..wm.len()], &wm[..], "the warm-up is the next request's prefix");
    assert_eq!(next.len(), wm.len() + 1, "+ the new user text");
}

#[test]
fn a_gateway_without_warm_up_gets_no_extra_call() {
    let d = tmpdir("nowarm");
    big_files(&d, &["a.txt"]);
    let f = Fake::start(vec![read_call(3000, "r1", "a.txt"), reply_with(5000, "Looked."), text_reply("Finished after the fold.")]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d, Some("look at a.txt\nnext\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(f.warmups().is_empty());
    assert_eq!(f.left(), 0);
}

#[test]
fn warm_up_answer_without_output_counts_as_taken() {
    use elisa::api::warm_took;
    assert!(warm_took(r#"{"type":"message","content":[],"usage":{"input_tokens":900,"output_tokens":0}}"#));
    assert!(warm_took("event: message_delta\ndata: {\"usage\":{\"output_tokens\":0}}\n\n"));
    // an older gateway generates one token for max_tokens 0
    assert!(!warm_took("data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\ndata: {\"usage\":{\"output_tokens\":1}}"));
    assert!(!warm_took(r#"{"content":[{"type":"text","text":"x"}],"usage":{"output_tokens":1}}"#));
}

// ---------- outside the project folder (src/outside.rs) ----------

/// a project dir `proj` and a sibling `other` with `secret.txt` (outside the project)
fn outside_dirs(tag: &str) -> (PathBuf, PathBuf) {
    let d = tmpdir(tag);
    let (proj, other) = (d.join("proj"), d.join("other"));
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("secret.txt"), "top secret\n").unwrap();
    (proj, other)
}

const ASK_OUT: &str = "outside the project? [y]es once";

#[test]
fn outside_read_asks_even_though_read_never_asks_and_deny_keeps_it_out() {
    let (proj, other) = outside_dirs("out-deny");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "../other/secret.txt"})).end("tool_use", 5),
        text_reply("Not read."),
        Sse::new(10, 0).tool_input("r2", "read", json!({"path": "inside.txt"})).end("tool_use", 5),
        text_reply("Read inside."),
    ]);
    std::fs::write(proj.join("inside.txt"), "in\n").unwrap();
    let o = run_elisa(&["--base-url", &f.url()], &proj, Some("peek\nn\nnow inside\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches(ASK_OUT).count(), 1, "only the outside read asks: {}", o.stderr);
    let want = other.join("secret.txt").display().to_string();
    assert!(o.stderr.contains(&format!("outside the project · read · {want}")), "prompt shows read + the exact path: {}", o.stderr);
    let reqs = f.messages();
    let (c, err) = last_tool_result(&reqs[1]);
    assert!(err && c.starts_with("denied by user (read outside the project folder") && c.contains(&want) && c.ends_with(NO_RETRY), "{c}");
    assert!(!c.contains("top secret"));
    let (c, err) = last_tool_result(&reqs[3]);
    assert!(!err && c.contains("in"), "{c}");
}

#[test]
fn outside_allow_once_asks_again_next_time() {
    let (proj, other) = outside_dirs("out-once");
    let abs = other.join("secret.txt").display().to_string();
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": abs})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("r2", "read", json!({"path": abs})).end("tool_use", 5),
        text_reply("Read twice."),
    ]);
    let o = run_elisa(&["--base-url", &f.url()], &proj, Some("peek\ny\ny\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches(ASK_OUT).count(), 2, "{}", o.stderr);
    let reqs = f.messages();
    for r in &reqs[1..3] {
        let (c, err) = last_tool_result(r);
        assert!(!err && c.contains("top secret"), "{c}");
    }
}

#[test]
fn outside_session_allow_covers_the_path_for_the_session_only() {
    let (proj, other) = outside_dirs("out-session");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path": "../other/out.txt", "content": "a"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("w2", "write", json!({"path": "../other/out.txt", "content": "b"})).end("tool_use", 5),
        // a different outside path still asks
        Sse::new(10, 0).tool_input("w3", "write", json!({"path": "../other/new.txt", "content": "c"})).end("tool_use", 5),
        text_reply("Done."),
    ]);
    let o = run_elisa(&["--base-url", &f.url()], &proj, Some("go\ns\nn\n/allow\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches(ASK_OUT).count(), 2, "w1 + w3 ask, w2 is covered: {}", o.stderr);
    assert!(o.stderr.contains("outside the project · write · "), "{}", o.stderr);
    assert_eq!(o.stderr.matches("allow? [y]es").count(), 0, "no second, normal prompt: {}", o.stderr);
    assert_eq!(std::fs::read_to_string(other.join("out.txt")).unwrap(), "b");
    assert!(!other.join("new.txt").exists());
    assert!(o.stderr.contains("allowed for this session (read + write)"), "{}", o.stderr);
    assert!(o.stderr.contains("outside the project folder: ask (config outside) · allowed for this session:"), "/allow lists it: {}", o.stderr);
    // never saved: .elisa/allow has no outside grant
    assert!(std::fs::read_to_string(proj.join(".elisa/allow")).unwrap_or_default().trim().is_empty());
}

#[test]
fn outside_always_allow_of_a_tool_does_not_cover_outside_writes() {
    let (proj, other) = outside_dirs("out-always");
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    std::fs::write(proj.join(".elisa/allow"), "write\nbash\n").unwrap();
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path": "in.txt", "content": "a"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("w2", "write", json!({"path": "../other/out.txt", "content": "b"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("b1", "bash", json!({"command": "cd .. && rm -f other/secret.txt"})).end("tool_use", 5),
        text_reply("Done."),
    ]);
    let o = run_elisa(&["--base-url", &f.url()], &proj, Some("go\nn\nn\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches(ASK_OUT).count(), 2, "{}", o.stderr);
    assert!(o.stderr.contains("outside the project · run (may write) · "), "bash: {}", o.stderr);
    assert!(proj.join("in.txt").exists(), "{}", o.stderr);
    assert!(!other.join("out.txt").exists() && other.join("secret.txt").exists());
}

#[test]
fn outside_yes_flag_denies_outside_writes_and_bash_but_not_inside() {
    let (proj, other) = outside_dirs("out-yes");
    let script = || vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path": "in.txt", "content": "a"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("w2", "write", json!({"path": "../other/out.txt", "content": "b"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("b1", "bash", json!({"command": format!("rm {}", other.join("secret.txt").display())})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "../other/secret.txt"})).end("tool_use", 5),
        text_reply("Done."),
    ];
    // a project config cannot loosen it
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    std::fs::write(proj.join(".elisa/config.toml"), "outside = \"allow\"\n").unwrap();
    let f = Fake::start(script());
    let o = run_elisa(&["-p", "go", "--yes", "--base-url", &f.url()], &proj, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(proj.join("in.txt").exists(), "inside writes run under --yes");
    assert!(!other.join("out.txt").exists() && other.join("secret.txt").exists(), "outside write + bash denied");
    let reqs = f.messages();
    let (c, err) = last_tool_result(&reqs[2]);
    assert!(err && c.starts_with("denied: write outside the project folder") && c.contains("--yes"), "{c}");
    let (c, err) = last_tool_result(&reqs[3]);
    assert!(err && c.starts_with("denied: run (may write) outside the project folder"), "{c}");
    let (c, err) = last_tool_result(&reqs[4]);
    assert!(err && c.contains("non-interactive") && c.contains("read-ok"), "reads cannot be asked in -p: {c}");

    // outside = "allow" in the user config: --yes allows them
    std::fs::remove_file(proj.join(".elisa/config.toml")).unwrap();
    std::fs::remove_file(proj.join("in.txt")).unwrap();
    let f = Fake::start(script());
    let o = run_elisa_cfg(&["-p", "go", "--yes", "--base-url", &f.url()], &proj, None, &[], "outside = \"allow\"\n");
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(std::fs::read_to_string(other.join("out.txt")).unwrap(), "b");
    assert!(!other.join("secret.txt").exists(), "bash rm ran");

    // read-ok: outside reads run without asking even in -p, writes are still denied under --yes
    std::fs::write(other.join("secret.txt"), "top secret\n").unwrap();
    std::fs::remove_file(other.join("out.txt")).unwrap();
    let f = Fake::start(script());
    let o = run_elisa_cfg(&["-p", "go", "--yes", "--base-url", &f.url()], &proj, None, &[], "outside = \"read-ok\"\n");
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(!other.join("out.txt").exists());
    let (c, err) = last_tool_result(&f.messages()[4]);
    assert!(!err && c.contains("top secret"), "{c}");

    // deny: nothing outside, not even reads, and no prompt
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "../other/secret.txt"})).end("tool_use", 5),
        text_reply("Done."),
    ]);
    let o = run_elisa_cfg(&["--base-url", &f.url()], &proj, Some("go\n"), &[], "outside = \"deny\"\n");
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr.matches(ASK_OUT).count(), 0);
    let (c, err) = last_tool_result(&f.messages()[1]);
    assert!(err && c.contains("config outside = \"deny\""), "{c}");
}

#[test]
fn jsonl_outside_approve_event_carries_paths_and_session_answer() {
    let (proj, other) = outside_dirs("out-jsonl");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "../other/secret.txt"})).end("tool_use", 5),
        Sse::new(10, 0).tool_input("r2", "grep", json!({"pattern": "secret", "path": "../other"})).end("tool_use", 5),
        text_reply("Read it."),
    ]);
    let mut s = Serve::start(&f.url(), &proj);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "peek"}));
    let a = s.until("approve").last().unwrap().clone();
    let want = other.join("secret.txt").display().to_string();
    assert_eq!((a["tool"].as_str(), a["outside"].as_bool(), a["access"].as_str()), (Some("read"), Some(true), Some("read")), "{a}");
    assert_eq!(a["paths"], json!([want]));
    assert!(a["line"].as_str().unwrap().starts_with("outside the project · read · "), "{a}");
    s.cmd(json!({"type": "approve", "answer": "session"}));
    s.until("tool_done");
    // the grep of the parent dir is not covered by a grant for one file: asked again
    let a = s.until("approve").last().unwrap().clone();
    assert_eq!((a["tool"].as_str(), a["paths"].clone()), (Some("grep"), json!([other.display().to_string()])));
    s.cmd(json!({"type": "approve", "answer": "no"}));
    s.until("turn_done");
    let reqs = f.messages();
    assert!(last_tool_result(&reqs[1]).0.contains("top secret"));
    assert!(last_tool_result(&reqs[2]).1, "grep denied");
    s.quit();
}

#[test]
fn web_search_is_denied_with_nobody_to_ask() {
    // web calls leave the machine: like a write, -p without a terminal and without --yes denies them (nothing is sent)
    let d = tmpdir("web-ask");
    let f = Fake::start(vec![
        Sse::new(500, 400).tool_input("w1", "web_search", json!({"query": "rust 2026 edition"})).end("tool_use", 8),
        text_reply("ok"),
    ]);
    let o = run_elisa(&["-p", "look it up", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let m = f.messages();
    assert_eq!(m.len(), 2);
    assert!(tool_names(&m[0]).contains(&"web_search".to_string()), "the built-in web MCP server is on by default");
    assert!(m[1].to_string().contains("denied (non-interactive; use --yes)"), "{}", m[1]);
}

/// a one-shot SearXNG stand-in: answers one JSON search and hands back the request line it got
fn fake_searx() -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut first = String::new();
        let mut r = BufReader::new(c.try_clone().unwrap());
        r.read_line(&mut first).unwrap();
        loop { let mut h = String::new(); if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" { break; } }
        let body = r#"{"results":[{"title":"Ollama v0.99 released","url":"https://ollama.example/blog/v0.99","content":"New engine."}]}"#;
        let _ = write!(c, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        let _ = tx.send(first);
    });
    (url, rx)
}

#[test]
fn model_searches_through_the_builtin_web_mcp_server() {
    // the model asks for web_search; Elisa runs it through its own MCP server (`elisa --mcp-serve web`, here on a
    // SearXNG stand-in) and the hits come back to the model as the tool result
    let d = tmpdir("mcp-web");
    let (searx, got) = fake_searx();
    let f = Fake::start(vec![
        Sse::new(500, 400).tool_input("w1", "web_search", json!({"query": "latest Ollama release"})).end("tool_use", 8),
        text_reply("Ollama v0.99 is out."),
    ]);
    let o = run_elisa_cfg(&["-p", "what is the latest Ollama release?", "--yes", "--base-url", &f.url()], &d, None, &[],
        &format!("web = \"searxng\"\nsearxng_url = \"{searx}\"\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let req = got.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(req.contains("q=latest%20Ollama%20release") || req.contains("q=latest+Ollama+release"), "{req}");
    let m = f.messages();
    assert_eq!(m.len(), 2);
    assert!(tool_names(&m[0]).contains(&"web_search".to_string()) && tool_names(&m[0]).contains(&"web_fetch".to_string()));
    let (res, err) = last_tool_result(&m[1]);
    assert!(!err && res.contains("Ollama v0.99 released"), "{res}");
    assert!(o.stdout.contains("Ollama v0.99 is out."), "{}", o.stdout);
}

/// the request line and headers (lower-cased names) a stand-in server got
type Got = (String, Vec<(String, String)>);

/// a one-shot Brave Search API stand-in: answers one request with `status` and `body` and hands back the request
/// line and its headers (lower-cased names)
fn fake_brave(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<Got>) {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut first = String::new();
        let mut r = BufReader::new(c.try_clone().unwrap());
        r.read_line(&mut first).unwrap();
        let mut heads = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" { break; }
            if let Some((k, v)) = h.trim_end().split_once(':') { heads.push((k.trim().to_ascii_lowercase(), v.trim().to_string())); }
        }
        let _ = write!(c, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        let _ = tx.send((first, heads));
    });
    (url, rx)
}

const BRAVE_OK: &str = r#"{"type":"search","web":{"results":[
  {"title":"Ollama <strong>v0.99</strong> released","url":"https://ollama.example/blog/v0.99","description":"New <strong>engine</strong> &amp; more."},
  {"title":"Second","url":"https://two.example/","description":""}]}}"#;

/// a search through the built-in web server on Brave: (tool result, is_error, stdout + stderr, request line, headers)
fn brave_search(status: u16, body: &'static str, user_cfg: &str, env: &[(&str, &str)]) -> (String, bool, String, String, Vec<(String, String)>) {
    let d = tmpdir("mcp-brave");
    let (brave, got) = fake_brave(status, body);
    let f = Fake::start(vec![
        Sse::new(500, 400).tool_input("w1", "web_search", json!({"query": "latest Ollama release"})).end("tool_use", 8),
        text_reply("done"),
    ]);
    let mut envs: Vec<(&str, String)> = vec![("ELISA_BRAVE_URL", brave)];
    envs.extend(env.iter().map(|(k, v)| (*k, v.to_string())));
    let o = run_elisa_cfg(&["-p", "what is the latest Ollama release?", "--yes", "--base-url", &f.url()], &d, None, &envs, user_cfg);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let (req, heads) = got.recv_timeout(Duration::from_secs(5)).unwrap();
    let m = f.messages();
    assert_eq!(m.len(), 2);
    let (res, err) = last_tool_result(&m[1]);
    (unfence(&res, "web_search latest Ollama release"), err, format!("{}\n{}", o.stdout, o.stderr), req, heads)
}

/// a web result reaches the model inside an `<untrusted>` fence (src/untrusted.rs): the body, checked and unwrapped
fn unfence(res: &str, source: &str) -> String {
    let head = format!("<untrusted source=\"{source}\">\n");
    let body = res.strip_prefix(&head).and_then(|r| r.strip_suffix("\n</untrusted>"));
    body.unwrap_or_else(|| panic!("not fenced as {source:?}: {res}")).to_string()
}

fn header<'a>(heads: &'a [(String, String)], k: &str) -> Option<&'a str> { heads.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()) }

#[test]
fn model_searches_brave_through_the_builtin_web_mcp_server() {
    let (res, err, out, req, heads) = brave_search(200, BRAVE_OK, "web = \"brave\"\nbrave_api_key = \"cfg-key-123\"\n", &[]);
    assert!(req.starts_with("GET /res/v1/web/search?q=latest") && req.contains("Ollama") && req.contains("&count=5"), "{req}");
    assert_eq!(header(&heads, "x-subscription-token"), Some("cfg-key-123"), "the key reached the server through its env: {heads:?}");
    assert_eq!(header(&heads, "accept"), Some("application/json"));
    // the same format as the other engines, markup and entities out
    assert!(!err && res.starts_with("results for \"latest Ollama release\":\n1. Ollama v0.99 released\n   https://ollama.example/blog/v0.99\n   New engine & more.\n2. Second\n   https://two.example/"), "{res}");
    assert!(!out.contains("cfg-key-123"), "{out}");
    // the environment wins over the config file
    let (_, err, out, _, heads) = brave_search(200, BRAVE_OK, "web = \"brave\"\nbrave_api_key = \"cfg-key-123\"\n", &[("BRAVE_API_KEY", "env-key-456")]);
    assert!(!err);
    assert_eq!(header(&heads, "x-subscription-token"), Some("env-key-456"));
    assert!(!out.contains("env-key-456"), "{out}");
}

#[test]
fn brave_refusals_are_one_clear_line_without_the_key() {
    for (status, says) in [(401, "Brave Search refused the API key (HTTP 401): check BRAVE_API_KEY or brave_api_key"),
                           (403, "Brave Search refused the API key (HTTP 403)"),
                           (429, "Brave Search rate limit reached (HTTP 429)")] {
        let (res, err, out, _, _) = brave_search(status, r#"{"type":"ErrorResponse","error":{"detail":"no"}}"#, "web = \"brave\"\n", &[("BRAVE_API_KEY", "bad-key-789")]);
        assert!(err && res.contains(says) && !res.contains('\n'), "{status}: {res}");
        assert!(!res.contains("bad-key-789") && !out.contains("bad-key-789"), "{status}: {res}\n{out}");
    }
}

#[test]
fn brave_without_a_key_turns_web_off_with_one_note_and_a_project_cannot_set_the_key() {
    let web = |names: Vec<String>| names.iter().any(|n| n.starts_with("web_"));
    let note = "web: off — web = \"brave\" needs an API key: set BRAVE_API_KEY or brave_api_key in ~/.config/elisa/config.toml";
    let d = tmpdir("brave-nokey");
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "brave_api_key = \"project-key-000\"\n").unwrap();
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa_cfg(&["-p", "hello", "--base-url", &f.url()], &d, None, &[], "web = \"brave\"\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stderr.matches(note).count(), 1, "{}", o.stderr);
    assert!(o.stderr.contains("brave_api_key ignored: only ~/.config/elisa/config.toml (or BRAVE_API_KEY) holds the key"), "{}", o.stderr);
    assert!(!o.stderr.contains("project-key-000") && !o.stdout.contains("project-key-000"), "{}", o.stderr);
    assert!(!web(tool_names(&f.messages()[0])));
}

/// the advice a denied tool call carries back to the model
const NO_RETRY: &str = "do not call this tool again for this request; answer without it or ask the user";

#[test]
fn a_project_config_cannot_add_mcp_servers_but_elisa_still_runs() {
    let d = tmpdir("mcp-project");
    let marker = d.join("started");
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), format!("[mcp.x]\ncommand = [\"/bin/sh\", \"-c\", \"touch {}\"]\n\n[mcp.web]\nenabled = true\n", marker.display())).unwrap();
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa(&["-p", "hello", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("mcp x: not started — a project config cannot add MCP servers"), "{}", o.stderr);
    assert!(o.stderr.contains("[mcp.web] enabled = true ignored: a project config can only switch servers off"), "{}", o.stderr);
    assert!(!marker.exists(), "the project's server never ran");
    let m = f.messages();
    assert_eq!(m.len(), 1);
    assert!(!tool_names(&m[0]).iter().any(|n| n.starts_with("x_")), "{:?}", tool_names(&m[0]));
}

#[test]
fn mcp_servers_named_like_elisas_tools_are_skipped() {
    // a server called `edit` / `read` would share Elisa's approval keys (and `read` its default auto-approve): refused.
    // Each would be a working server (Elisa's own web server under another name) if it were started.
    let d = tmpdir("mcp-reserved");
    let exe = env!("CARGO_BIN_EXE_elisa");
    let cfg = format!("[mcp.edit]\ncommand = [\"{exe}\", \"--mcp-serve\", \"web\"]\n[mcp.read]\ncommand = [\"{exe}\", \"--mcp-serve\", \"web\"]\n\
        [mcp.\"bad name\"]\ncommand = [\"{exe}\", \"--mcp-serve\", \"web\"]\n");
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa_cfg(&["-p", "hello", "--base-url", &f.url()], &d, None, &[], &cfg);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for n in ["edit", "read"] {
        assert!(o.stderr.contains(&format!("mcp {n}: not started — \"{n}\" is one of Elisa's own tool names")), "{}", o.stderr);
    }
    assert!(o.stderr.contains("mcp bad name: not started — a server name may only use letters, digits, _ and -"), "{}", o.stderr);
    let names = tool_names(&f.messages()[0]);
    assert_eq!(names.iter().filter(|n| n.ends_with("web_search")).collect::<Vec<_>>(), ["web_search"], "only the built-in web server: {names:?}");
}

#[test]
fn malformed_mcp_entries_skip_that_server_and_elisa_still_runs() {
    let d = tmpdir("mcp-malformed");
    let cfg = "[mcp.both]\ncommand = [\"true\"]\nurl = \"http://127.0.0.1:1/mcp\"\n\
        [mcp.neither]\nenv = { A = \"b\" }\n\
        [mcp.empty]\ncommand = [\"\"]\n\
        [mcp.str]\ncommand = \"npx server\"\n\
        [mcp.typo]\ncomand = [\"true\"]\n";
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa_cfg(&["-p", "hello", "--base-url", &f.url()], &d, None, &[], cfg);
    assert_eq!(o.code, 0, "{}", o.stderr);
    for (n, why) in [("both", "not both"), ("neither", "give command"), ("empty", "command is empty"), ("str", "command must be a list"), ("typo", "unknown key \"comand\"")] {
        let line = o.stderr.lines().find(|l| l.contains(&format!("mcp {n}: not started — "))).unwrap_or_else(|| panic!("no note for {n}: {}", o.stderr));
        assert!(line.contains(why), "{line}");
    }
    assert!(o.stdout.contains("hi"));
    assert!(tool_names(&f.messages()[0]).contains(&"web_search".to_string()), "the rest of the config still applies");
}

#[test]
fn bad_web_settings_turn_web_off_with_one_note() {
    let web = |names: Vec<String>| names.iter().any(|n| n.starts_with("web_"));
    for (cfg, note) in [
        ("web = \"google\"\n", "web: off — web = \"google\" is not a search method (use \"duckduckgo\", \"searxng\" or \"brave\")"),
        ("web = \"searxng\"\n", "web: off — web = \"searxng\" needs searxng_url = \"http://…\""),
        ("web = \"searxng\"\nsearxng_url = \"localhost:8080\"\n", "web: off — web = \"searxng\" needs searxng_url"),
    ] {
        let d = tmpdir("web-bad");
        let f = Fake::start(vec![text_reply("hi")]);
        let o = run_elisa_cfg(&["-p", "hello", "--base-url", &f.url()], &d, None, &[], cfg);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stderr.matches(note).count(), 1, "{cfg}: {}", o.stderr);
        assert!(!web(tool_names(&f.messages()[0])), "{cfg}");
    }
    // a project config may only turn web off: picking the search method there is ignored, with a note
    let d = tmpdir("web-project");
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "web = \"searxng\"\n").unwrap();
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa(&["-p", "hello", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("web = \"searxng\" ignored: a project config can only turn web off"), "{}", o.stderr);
    assert!(!o.stderr.contains("web: off"), "{}", o.stderr);
    assert!(web(tool_names(&f.messages()[0])), "the user's setting (duckduckgo) stays");
    // a clean config: no startup notes at all
    let d = tmpdir("web-clean");
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa(&["-p", "hello", "--base-url", &f.url()], &d, None);
    assert!(!o.stderr.contains("web:") && !o.stderr.contains("not started"), "{}", o.stderr);
}

#[test]
fn mcp_always_allow_is_kept_as_mcp_name_and_does_not_cover_elisas_edit() {
    let d = tmpdir("mcp-always");
    std::fs::write(d.join("a.txt"), "old\n").unwrap();
    let (searx, got) = fake_searx();
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "web_search", json!({"query": "elisa"})).end("tool_use", 5),
        text_reply("Searched."),
        Sse::new(10, 0).tool_input("e1", "edit", json!({"path": "a.txt", "old": "old", "new": "new"})).end("tool_use", 5),
        text_reply("Not edited."),
    ]);
    let o = run_elisa_cfg(&["--base-url", &f.url()], &d, Some("search\na\nedit it\nn\n"), &[],
        &format!("memory = false\nweb = \"searxng\"\nsearxng_url = \"{searx}\"\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    got.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(o.stderr.contains("allow? [y]es / [n]o / [a]lways for MCP server web"), "{}", o.stderr);
    assert!(o.stderr.contains("allow? [y]es / [n]o / [a]lways for edit"), "edit still asks: {}", o.stderr);
    assert_eq!(std::fs::read_to_string(d.join(".elisa/allow")).unwrap(), "mcp:web\n");
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "old\n");
    // bare names in .elisa/allow are Elisa's tools only: `web` (as an earlier build saved it) is not `mcp:web`, so it asks
    std::fs::write(d.join(".elisa/allow"), "edit\nweb\n").unwrap();
    let g = Fake::start(vec![
        Sse::new(10, 0).tool_input("w2", "web_fetch", json!({"url": "https://example.invalid/"})).end("tool_use", 5),
        text_reply("Not fetched."),
    ]);
    let o = run_elisa_cfg(&["--base-url", &g.url()], &d, Some("fetch\nn\n"), &[], "memory = false\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stderr.matches("for MCP server web").count(), 1, "a bare `web` entry is not `mcp:web`: {}", o.stderr);
    let (c, err) = last_tool_result(&g.messages()[1]);
    assert!(err && c == format!("denied by user: {NO_RETRY}"), "{c}");
}

#[test]
fn auto_approve_covers_mcp_tools_only_by_mcp_name() {
    // default auto_approve (read, glob, grep) and a bare `web` do not cover MCP tools: denied with nobody to ask
    for cfg in ["", "auto_approve = [\"read\", \"web\", \"web_search\"]\n"] {
        let d = tmpdir("mcp-auto");
        let f = Fake::start(vec![
            Sse::new(10, 0).tool_input("w1", "web_search", json!({"query": "elisa"})).end("tool_use", 5),
            text_reply("No search."),
        ]);
        let o = run_elisa_cfg(&["-p", "search", "--base-url", &f.url()], &d, None, &[], cfg);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let (c, err) = last_tool_result(&f.messages()[1]);
        assert!(err && c.starts_with("denied (non-interactive"), "{cfg}: {c}");
    }
    // `mcp:web` does
    let d = tmpdir("mcp-auto-yes");
    let (searx, got) = fake_searx();
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "web_search", json!({"query": "elisa"})).end("tool_use", 5),
        text_reply("Searched."),
    ]);
    let o = run_elisa_cfg(&["-p", "search", "--base-url", &f.url()], &d, None, &[],
        &format!("auto_approve = [\"read\", \"mcp:web\"]\nweb = \"searxng\"\nsearxng_url = \"{searx}\"\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    got.recv_timeout(Duration::from_secs(5)).unwrap();
    let (c, err) = last_tool_result(&f.messages()[1]);
    assert!(!err && c.contains("Ollama v0.99 released"), "{c}");
}

#[test]
fn jsonl_approve_names_the_mcp_server_and_a_no_says_do_not_retry() {
    let dir = tmpdir("jsonl-mcp");
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("w1", "web_search", json!({"query": "elisa"})).end("tool_use", 9),
        text_reply("ok, no search"),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "search"}));
    let evs = s.until("approve");
    let a = evs.last().unwrap();
    assert_eq!((a["tool"].as_str(), a["server"].as_str(), a["outside"].as_bool()), (Some("web_search"), Some("web"), Some(false)), "{a}");
    s.cmd(json!({"type": "approve", "answer": "no"}));
    let evs = s.until("tool_done");
    assert_eq!(evs.last().unwrap()["note"].as_str(), Some("denied"));
    s.until("turn_done");
    assert_eq!(last_tool_result(&f.messages()[1]), (format!("denied by user: {NO_RETRY}"), true));
    s.quit();
}

#[test]
fn web_tools_go_away_when_the_web_server_is_disabled_or_web_is_off() {
    let web = |names: Vec<String>| names.iter().any(|n| n.starts_with("web_"));
    // [mcp.web] enabled = false, even from a project config
    let d = tmpdir("mcp-web-off");
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "[mcp.web]\nenabled = false\n").unwrap();
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa(&["-p", "hello", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let r = &f.messages()[0];
    assert!(!web(tool_names(r)) && tool_names(r).contains(&"read".to_string()), "{:?}", tool_names(r));
    // web = "off" in the user config
    let d2 = tmpdir("web-off");
    let g = Fake::start(vec![text_reply("hi")]);
    let o2 = run_elisa_cfg(&["-p", "hello", "--base-url", &g.url()], &d2, None, &[], "web = \"off\"\n");
    assert_eq!(o2.code, 0, "{}", o2.stderr);
    assert!(!web(tool_names(&g.messages()[0])), "{:?}", tool_names(&g.messages()[0]));
}

/// QA 2026-11-08 (H1): a model that lists a .env file as `- DB_PASSWORD: value` (or YAML / JSON / `KEY = value`)
/// put the raw value into the saved session and the memory log. Every form is redacted on disk.
#[test]
fn reformatted_secrets_never_reach_sessions_or_memory() {
    let d = tmpdir("secret-forms");
    // assembled at runtime: no scannable literal in this file
    let vals: Vec<String> = (0..7).map(|i| format!("TPcp{i}Xk9q2LmR7vWz4bN")).collect();
    std::fs::write(d.join("deploy.env"), format!("PORT=8080\nDB_PASSWORD={}\n", vals[0])).unwrap();
    let reply = format!(
        "Settings:\n- PORT: 8080\n- DB_PASSWORD: {}\n\nAs YAML:\n```yaml\ndb:\n  password: {}\n```\nAs JSON:\n```json\n{{\"password\": \"{}\"}}\n```\n\
         Shell:\n```sh\nexport DB_PASSWORD={}\nDB_PASSWORD = {}\n```\n1. **API_TOKEN**: `{}`\nDB_PASSWORD:{}\n",
        vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6]
    );
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "deploy.env"})).end("tool_use", 9),
        text_reply(&reply),
    ]);
    let o = run_elisa(&["-p", "read deploy.env and list each setting with its value", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    // walk everything Elisa wrote: .elisa/ (sessions, notes, metrics) and the memory dir under the temp HOME
    fn files(p: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let q = e.path();
            if q.is_dir() { files(&q, out) } else { out.push(q) }
        }
    }
    let mut all = Vec::new();
    files(&d.join(".elisa"), &mut all);
    files(&d.join("_home/.elisa"), &mut all);
    let sessions: Vec<_> = all.iter().filter(|p| p.extension().is_some_and(|e| e == "json") && p.to_string_lossy().contains("sessions")).collect();
    assert_eq!(sessions.len(), 1, "{all:?}");
    assert!(all.iter().any(|p| p.extension().is_some_and(|e| e == "seg")), "memory segments written: {all:?}");
    for p in &all {
        let b = std::fs::read(p).unwrap();
        let s = String::from_utf8_lossy(&b);
        for v in &vals { assert!(!s.contains(v.as_str()), "{} holds {}…", p.display(), &v[..4]); }
    }
    let sess = std::fs::read_to_string(sessions[0]).unwrap();
    assert!(sess.contains("[redacted:env-secret]") && sess.contains("[redacted:generic-secret]"), "{sess}");
    // the memory log holds the (redacted) reply, so the check above really covered it
    assert!(all.iter().filter(|p| p.extension().is_some_and(|e| e == "seg"))
        .any(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap()).contains("[redacted:env-secret]")), "reply not in memory log");
}

/// Every file Elisa wrote for project `d`: `.elisa/` (sessions, notes, metrics) and the memory dir under the temp HOME.
fn written_files(d: &Path) -> Vec<PathBuf> {
    fn files(p: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let q = e.path();
            if q.is_dir() { files(&q, out) } else { out.push(q) }
        }
    }
    let mut all = Vec::new();
    files(&d.join(".elisa"), &mut all);
    files(&d.join("_home/.elisa"), &mut all);
    all
}

/// QA N1: `app.cfg` holds `API_KEY_PROD=<value>`; the tool result was stored redacted, but the model's sentence
/// "…app.cfg with the value <value>" (no key name) reached notes, the session and memory raw. A value the session
/// has seen is now hidden wherever it appears on disk.
#[test]
fn a_secret_restated_in_prose_never_reaches_disk() {
    let d = tmpdir("secret-prose");
    let v = ["ZpQ8", "1kLmN3vB7xR2tY6w"].concat();
    std::fs::write(d.join("app.cfg"), format!("NAME=demo\nAPI_KEY_PROD={v}\n")).unwrap();
    let reply = format!("There is a secret in the project file app.cfg with the value {v}. Rotate {v} soon.\n\
        <state>\ngoal: scan for secrets\ndone: found the key {v} in app.cfg\nkeep: app.cfg value {v}\nnext: none\n</state>");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("s1", "sec_secrets", json!({"path": "."})).end("tool_use", 9),
        Sse::new(10, 0).tool_input("r1", "read", json!({"path": "app.cfg"})).end("tool_use", 9),
        text_reply(&reply),
    ]);
    let o = run_elisa(&["--yes", "--base-url", &f.url()], &d, Some("Use the sec_secrets tool to scan this project for secrets\n/exit\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "{}", o.stderr);
    let all = written_files(&d);
    assert!(all.iter().any(|p| p.to_string_lossy().contains("sessions")), "{all:?}");
    assert!(all.iter().any(|p| p.extension().is_some_and(|e| e == "seg")), "memory segments written: {all:?}");
    let mut saw_reply = false;
    for p in &all {
        let s = String::from_utf8_lossy(&std::fs::read(p).unwrap()).to_string();
        assert!(!s.contains(&v), "{} holds the value", p.display());
        saw_reply |= s.contains("with the value [redacted:env-secret]");
    }
    assert!(saw_reply, "the reply was saved (redacted): {all:?}");
}

/// QA N2: a `KEY=value` / `key: value` the user types inside a sentence was raw in the session, metrics.jsonl and
/// memory (a ghp_ token in the same message was redacted).
#[test]
fn a_secret_typed_mid_sentence_never_reaches_disk() {
    let d = tmpdir("secret-typed");
    let v = ["Zr4W", "q8Lp2Nx6Tb9Vk3Hs"].concat();
    let f = Fake::start(vec![text_reply("OK")]);
    let prompt = format!("Just reply OK, no tools: my db password is DB_PASSWORD={v} and password: {v}");
    let o = run_elisa(&["-p", &prompt, "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let all = written_files(&d);
    assert!(all.iter().any(|p| p.file_name().is_some_and(|n| n == "metrics.jsonl")), "{all:?}");
    assert!(all.iter().any(|p| p.extension().is_some_and(|e| e == "seg")), "{all:?}");
    for p in &all {
        let s = String::from_utf8_lossy(&std::fs::read(p).unwrap()).to_string();
        assert!(!s.contains(&v), "{} holds the value", p.display());
    }
    let sess = all.iter().find(|p| p.to_string_lossy().contains("sessions") && p.extension().is_some_and(|e| e == "json")).expect("session saved");
    let s = std::fs::read_to_string(sess).unwrap();
    assert!(s.contains("DB_PASSWORD=[redacted:env-secret]") && s.contains("password: [redacted:"), "{s}");
}

/// QA N3: a reply with only its state card left the user with nothing to read. Asked once more (the nudge goes with
/// that one request, never into the history), then an info line if it is still empty.
#[test]
fn an_empty_visible_reply_is_retried_once_then_noted() {
    let d = tmpdir("empty-reply");
    let card = "<state>\ngoal: none\ndone: none\nnext: none\n</state>";
    let f = Fake::start(vec![text_reply(card), text_reply(&format!("You're welcome! What next?\n{card}"))]);
    let o = run_elisa(&["-p", "thanks", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stdout.contains("You're welcome!"), "{}", o.stdout);
    assert!(o.stderr.contains("asking the model once more") && !o.stderr.contains("[no answer"), "{}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 2);
    let (m0, m1) = (reqs[0]["messages"].as_array().unwrap(), reqs[1]["messages"].as_array().unwrap());
    // the same history, plus the nudge as the last message
    assert_eq!(m1.len(), m0.len() + 1);
    assert_eq!(m0[..], m1[..m0.len()]);
    assert!(m1.last().unwrap().to_string().contains("one plain sentence"), "{}", m1.last().unwrap());
    let sess = written_files(&d).into_iter().find(|p| p.to_string_lossy().contains("sessions") && p.extension().is_some_and(|e| e == "json")).unwrap();
    let s = std::fs::read_to_string(sess).unwrap();
    assert!(s.contains("You're welcome!") && !s.contains("one plain sentence"), "{s}");

    // empty twice: one retry, then a note instead of silence
    let d = tmpdir("empty-reply2");
    let f = Fake::start(vec![text_reply(card), text_reply(card), text_reply("unused")]);
    let o = run_elisa(&["-p", "2+2=?", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 1, "exactly one retry");
    assert!(o.stderr.contains("[no answer: the model wrote no text for you twice"), "{}", o.stderr);
}

// ---------- session memory: state card carry-over, /clear, ctx after Esc, tok/s null ----------

/// A new session does not take over the last session's state card (uitest 2026-11-08: "continuing from state card
/// #9" with an old goal sent a 2B model on 30 web steps); `--continue` does, cleaned of template text.
#[test]
fn state_card_is_carried_only_into_a_continued_session() {
    let d = tmpdir("card-carry");
    let f = Fake::start(vec![text_reply("ok\n<state>\ngoal: OLD-GOAL-X\ndone: …\nkeep: must-not-lose facts\nnext: OLD-NEXT-X\n</state>")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("first question R1\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    // run 2: a new session in the same project
    let g = Fake::start(vec![text_reply("fresh")]);
    let o = run_elisa(&["--base-url", &g.url()], &d, Some("an unrelated question R2\n/state\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!o.stderr.contains("continuing from state card"), "{}", o.stderr);
    assert!(o.stderr.contains("no state card yet"), "{}", o.stderr);
    let req = g.messages()[0].to_string();
    assert!(req.contains("R2") && !req.contains("OLD-GOAL-X") && !req.contains("OLD-NEXT-X"), "the old card reached a new session: {req}");
    // run 3: --continue carries it, without the template placeholders (a new second: session ids are unix seconds)
    std::thread::sleep(Duration::from_millis(1100));
    let o = run_elisa(&["--base-url", "http://127.0.0.1:9", "--continue"], &d, Some("/state\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("continuing from state card #"), "{}", o.stderr);
    assert!(o.stderr.contains("goal: OLD-GOAL-X") && o.stderr.contains("next: OLD-NEXT-X"), "{}", o.stderr);
    assert!(!o.stderr.contains("must-not-lose") && !o.stderr.contains("done: …"), "template text in the card: {}", o.stderr);
}

/// /clear shrinks the context even when the model's summary is longer than the conversation, stores no
/// instruction / template text, sends `compacted` (the ctx meter updates now), and the next request carries the
/// notes as background.
#[test]
fn jsonl_clear_shrinks_the_context_and_keeps_clean_notes() {
    let dir = tmpdir("jsonl-clear");
    let long = "The answer, at some length. ".repeat(40);
    let pad: String = (0..120).map(|i| format!("- detail line {i:03} PAD\n")).collect();
    let summary = format!("## Goal\nWhat the user said: \"hello\" and \"The conversation above will be erased. Write notes so a fresh session can pick up the work\"\n\n\
        ## Done\nFinished steps and decisions, with their reasons.\n\n## Files touched\n\n\
        ## Current state / next step\nGreeted the user. REAL-NOTE-1\n{pad}\n\
        ## Facts to remember\nCommands that build or test the project, paths, names, and preferences the user stated in their own words.\n\n\
        Also known as: name = other words; …\n<state>\ngoal: x\n</state>");
    let f = Fake::start(vec![text_reply(&long), text_reply(&summary), text_reply("after")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "hello"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    let evs = s.until("turn_done");
    let c = evs.iter().find(|e| e["type"] == "compacted").unwrap_or_else(|| panic!("no compacted event: {:?}", kinds(&evs)));
    let (from, to) = (c["from"].as_u64().unwrap(), c["to"].as_u64().unwrap());
    assert!(to < from, "/clear grew the context: {from} -> {to}");
    let info = evs.iter().filter(|e| e["type"] == "info").map(|e| e["text"].as_str().unwrap()).find(|t| t.starts_with("[cleared:")).unwrap();
    assert!(info.contains("notes saved"), "{info}");
    let notes = std::fs::read_to_string(dir.join(".elisa/notes.md")).unwrap();
    assert!(notes.contains("REAL-NOTE-1"), "{notes}");
    assert!(notes.matches("PAD").count() < 120, "the notes were cut to fit under the conversation: {notes}");
    for bad in ["will be erased", "Finished steps", "name = other words", "Also known as", "Commands that build", "## Files touched", "<state>", "## Goal"] {
        assert!(!notes.contains(bad), "{bad:?} stored in the notes:\n{notes}");
    }
    // the next request: one first message, notes marked as background
    s.cmd(json!({"type": "send", "text": "next one"}));
    s.until("turn_done");
    let reqs = f.messages();
    let m = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(m.len(), 1);
    let t = m[0]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains(elisa::context::NOTES_BACKGROUND) && t.contains("REAL-NOTE-1") && t.ends_with("## Request\nnext one"), "{t}");
    s.quit();
}

/// Esc mid-reply on a server that reports no usage (Ollama sends it only at the end): `reply_done` carries the
/// estimated context, not 0 (uitest: the ctx meter reset to 0/8.2K). A short reply has no decode window: tok_s null.
#[test]
fn jsonl_interrupt_keeps_the_context_and_short_replies_have_no_rate() {
    let dir = tmpdir("jsonl-esc-ctx");
    let words: Vec<String> = (0..3000).map(|i| format!("w{i} ")).collect();
    let pieces: Vec<&str> = words.iter().map(String::as_str).collect();
    let f = Fake::start(vec![Sse::new(0, 0).text(&pieces).end("end_turn", 3000), text_reply("after")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "talk a lot"}));
    s.until("delta");
    std::thread::sleep(Duration::from_millis(100));
    s.cmd(json!({"type": "interrupt"}));
    let evs = s.until("turn_done");
    let rd = evs.iter().find(|e| e["type"] == "reply_done").unwrap();
    let ctx = rd["ctx_used"].as_u64().unwrap();
    // system + tools alone are hundreds of tokens
    assert!(ctx > 300, "ctx_used after Esc: {rd}");
    assert!(rd["output_tokens"].as_u64().unwrap() > 0, "the partial reply counts: {rd}");
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "again"}));
    let evs = s.until("turn_done");
    let rd = evs.iter().find(|e| e["type"] == "reply_done").unwrap();
    assert!(rd.get("tok_s").is_some() && rd["tok_s"].is_null(), "no decode window: tok_s is null, not 0.0: {rd}");
    s.quit();
}

/// soak 2026-11-08: /clear summarised one turn, replaced the notes and said "No files were touched" after 4 edits,
/// and the notes kept a "done" item no edit made. Now: merged with the earlier notes, the file list from Elisa's own
/// edit records, the invented claim dropped.
#[test]
fn jsonl_clear_merges_notes_and_lists_the_files_really_changed() {
    let dir = tmpdir("jsonl-clear-merge");
    std::fs::create_dir_all(dir.join(".elisa")).unwrap();
    std::fs::write(dir.join(".elisa/notes.md"), "## Facts to remember\n- Codename BLUE HERON, owner Mira Tanaka.\n").unwrap();
    let summary = "## Done\n- Wrote hello.txt with a greeting. REAL-DONE\n- Added restock_cost(items) to core.py.\n- Wrote hello.txt with a greeting. REAL-DONE\n\n\
        ## Files touched\nNo files were touched in this session.\n\n## Current state / next step\nnothing open";
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "write", json!({"path": "hello.txt", "content": "hi there\n"})).end("tool_use", 12),
        text_reply("Wrote it."),
        text_reply(summary),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "create hello.txt saying hi there"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    s.until("turn_done");
    assert_eq!(std::fs::read_to_string(dir.join("hello.txt")).unwrap(), "hi there\n");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    s.until("turn_done");
    let notes = std::fs::read_to_string(dir.join(".elisa/notes.md")).unwrap();
    assert!(notes.contains("BLUE HERON"), "earlier facts kept: {notes}");
    assert!(notes.contains(&format!("{}\n- hello.txt: written (new file)", elisa::context::FILES_NOTES)), "{notes}");
    assert_eq!(notes.matches("REAL-DONE").count(), 1, "{notes}");
    assert!(!notes.contains("No files were touched") && !notes.contains("restock_cost"), "{notes}");
    s.quit();
}

/// eval `context.clear_merges_notes` with memory on (the default): the note's facts were also pinned facts, so the
/// first message after /clear carried "Codename BLUE HERON" twice (notes + `[state]`), came out bigger than the
/// two-turn conversation, and the "always shrink" rule cut the fact from the notes, leaving only the file records.
/// Now: the fact stays in the notes, is not repeated as a pinned fact, and the context may simply not shrink.
#[test]
fn jsonl_clear_with_memory_keeps_the_earlier_facts() {
    let dir = tmpdir("jsonl-clear-mem");
    std::fs::create_dir_all(dir.join(".elisa")).unwrap();
    std::fs::write(dir.join(".elisa/notes.md"), "## Facts to remember\n- Codename BLUE HERON, owner Mira Tanaka.\n").unwrap();
    let summary = "## Done\n- Wrote hello.txt with a greeting. REAL-DONE\n\n## Files touched\nNo files were touched in this session.\n\n\
        ## Current state / next step\nnothing open";
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "write", json!({"path": "hello.txt", "content": "hi there\n"})).end("tool_use", 12),
        text_reply("Wrote it."),
        text_reply(summary),
        text_reply("BLUE HERON."),
    ]);
    let mut s = Serve::start_cfg(&f.url(), &dir, "memory = true\n");
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "create hello.txt saying hi there"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    let evs = s.until("turn_done");
    let info = evs.iter().filter(|e| e["type"] == "info").map(|e| e["text"].as_str().unwrap()).find(|t| t.starts_with("[cleared:")).unwrap();
    assert!(info.contains("notes saved"), "{info}");
    let notes = std::fs::read_to_string(dir.join(".elisa/notes.md")).unwrap();
    assert!(notes.contains("Codename BLUE HERON, owner Mira Tanaka."), "earlier facts kept: {notes}");
    assert!(notes.contains(&format!("{}\n- hello.txt: written (new file)", elisa::context::FILES_NOTES)), "{notes}");
    assert!(notes.contains("REAL-DONE") && notes.contains("nothing open"), "{notes}");
    s.cmd(json!({"type": "send", "text": "what is the codename?"}));
    s.until("turn_done");
    let reqs = f.messages();
    let t = reqs[3]["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert_eq!(t.matches("BLUE HERON").count(), 1, "the fact once, in the notes: {t}");
    assert!(!t.contains("Pinned facts"), "{t}");
    s.quit();
}

/// The documents / security tools come only on turns about them, after every stable tool (a flip changes the end of
/// the tool list only); a plain turn has neither.
#[test]
fn docs_and_sec_tools_only_on_turns_about_them_and_last() {
    let d = tmpdir("gate");
    let f = Fake::start(vec![text_reply("plain"), text_reply("doc"), text_reply("sec")]);
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "16384"], &d,
        Some("explain what a mutex is\nwhat does docs/spec.pdf say about reorders?\nscan this project for leaked secrets\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    let reqs = f.messages();
    let names: Vec<Vec<String>> = reqs.iter().map(tool_names).collect();
    assert!(!names[0].iter().any(|n| n.starts_with("docs_") || n.starts_with("sec_")), "{:?}", names[0]);
    let stable = names[0].clone();
    assert_eq!(names[1][..stable.len()], stable[..], "stable tools first, unchanged");
    assert_eq!(names[1][stable.len()..], ["docs_pdf", "docs_sheet"]);
    // the PDF named one turn earlier is still in the recent context: the documents tools stay, the security ones join
    // after them (a fixed order)
    assert_eq!(names[2][..stable.len()], stable[..]);
    assert_eq!(names[2][stable.len()..], ["docs_pdf", "docs_sheet", "sec_secrets", "sec_deps", "sec_binary", "sec_jwt"]);
}

/// No model configured: Elisa guesses none and sends nothing; `-p` says how to choose one and exits 2.
#[test]
fn no_model_sends_nothing_and_says_how_to_choose() {
    let (r, _) = send(&Client::new(&Config { base_url: "http://127.0.0.1:1".into(), ..Config::default() }), &[Message::user_text("hi")]);
    assert_eq!(r.unwrap_err(), elisa::api::NO_MODEL);
    let d = tmpdir("nomodel");
    // the comment keeps run_elisa_cfg from adding --model
    let o = run_elisa_cfg(&["-p", "hi", "--base-url", "http://127.0.0.1:1"], &d, None, &[], "# no model chosen\n");
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("No model chosen") && o.stderr.contains("--model"), "{}", o.stderr);
}
