//! In-process fake Anthropic Messages server + tests for config, api, repl and ui working together.
use elisa::api::{Client, Request};
use elisa::config::Config;
use elisa::types::{Block, Message, Role};
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
        ("POST", "/v1/messages") | ("POST", "/v1/chat/completions") => script.lock().unwrap().pop_front()
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

/// An OpenAI-compatible stream (llama.cpp, LM Studio, vLLM, Ollama's /v1): the deltas, a finish reason, then the
/// usage chunk exactly as given (a server with no prompt cache may leave `prompt_tokens_details` out entirely).
fn oa_reply(deltas: &[Value], finish: &str, usage: Value) -> Resp {
    let mut ev: Vec<String> = deltas.iter().map(|d| format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": d}]}))).collect();
    ev.push(format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]})));
    ev.push(format!("data: {}\n\n", json!({"choices": [], "usage": usage})));
    ev.push("data: [DONE]\n\n".into());
    Resp::Sse(ev)
}

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

/// `run_elisa` with extra environment (a cargo project's real cargo needs the real CARGO_HOME / RUSTUP_HOME)
fn run_elisa_env(args: &[&str], cwd: &Path, stdin: Option<&str>, env: &[(&str, String)]) -> Out { run_elisa_cfg(args, cwd, stdin, env, "") }

/// `run_elisa_env` with extra lines in the user config (~/.config/elisa/config.toml)
fn run_elisa_cfg(args: &[&str], cwd: &Path, stdin: Option<&str>, env: &[(&str, String)], user_cfg: &str) -> Out {
    let home = cwd.join("_home");
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    // `route = "off"`: these tests check the full tool list; intent routing has its own cases (evals/tools.toml route_*)
    std::fs::write(home.join(".config/elisa/config.toml"), format!("auto_mode = true\nchat_window = 1600\n{}{user_cfg}", if user_cfg.contains("route =") { "" } else { "route = \"off\"\n" })).unwrap();
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
fn a_model_without_vision_never_gets_image_parts() {
    // an old saved session's image part, resumed on a model without vision
    let msgs = vec![Message::new(Role::User, vec![Block::image("image/png", "iVBORw0KGgo="), Block::Text { text: "what is this?".into() }]),
        Message::assistant_text("A cat."), Message::user_text("tell me a short joke")];
    let f = Fake::start(vec![text_reply("ok"), text_reply("ok"), text_reply("ok")]);
    let c = client(&f.url());
    c.set_vision(false);
    send(&c, &msgs).0.unwrap();
    send(&c, &msgs).0.unwrap();
    c.set_vision(true);
    send(&c, &msgs).0.unwrap();
    let reqs: Vec<Value> = f.log().iter().map(|r| r.json()).collect();
    let body = |i: usize| reqs[i]["messages"].to_string();
    assert!(!body(0).contains("\"image\"") && body(0).contains(elisa::context::IMAGE_STUB) && body(0).contains("what is this?"), "{}", body(0));
    assert_eq!(body(0), body(1), "the same request every time: the prefix stays");
    assert!(body(2).contains("\"image\""), "a model with vision gets the picture");
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
    std::fs::write(home.join(".config/elisa/config.toml"), "route = \"off\"\nmodel = \"user-m\"\nmax_output = 111\nauto_approve = [\"read\"]\n").unwrap();
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
    assert!(newest_notes(&d).contains("NOTES-CLR"));
    // after /clear: same system prompt, notes in the first (only) message
    assert_eq!(system(&reqs[0]), system(&reqs[2]));
    // ... and the same tools: a reset never drops them (without them, after a reset the model types tool calls as text)
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
    assert!(err && c == format!("denied by user: {SAID_NO}"), "{c}");
    assert_prefix_rule(&reqs);
}

/// A reply that says the file was saved after the user said no to the write is asked once more in words (no tools,
/// a note that nothing was saved); still claiming it: a plain line instead. A reply that says it was not saved stands.
#[test]
fn a_saved_claim_after_a_denied_write_is_not_shown() {
    let write = || Sse::new(10, 0).tool_input("w1", "write", json!({"path":"x.txt","content":"hello"})).end("tool_use", 5);
    // the retry still claims it: the plain line
    let d = tmpdir("deny-claim");
    let f = Fake::start(vec![write(), text_reply("I've saved \"hello\" to x.txt for you."), text_reply("Saved it to x.txt.")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("save hello to x.txt\nn\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!d.join("x.txt").exists());
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3, "{}", o.stderr);
    assert!(!has_tools(&reqs[2]), "the retry offers no tools");
    // the note is Elisa's line inside the last tool result, never a user text of its own after it (there it
    // reads as the user speaking)
    let (c, _) = last_tool_result(&reqs[2]);
    assert!(c.contains("[elisa: For this reply: nothing was saved in this message (the user said no)"), "{c}");
    let last = reqs[2]["messages"].as_array().unwrap().last().unwrap();
    assert!(last["content"].as_array().unwrap().iter().all(|b| b["type"] == "tool_result"), "{last}");
    assert!(o.stdout.contains("Okay, I did not save it: you said no, so nothing was written."), "{}", o.stdout);
    // the retry says it was not saved: that reply stands
    let d = tmpdir("deny-claim2");
    let f = Fake::start(vec![write(), text_reply("Done, x.txt has been created."), text_reply("Okay, I didn't save it, since you said no.")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("save hello to x.txt\nn\n"));
    assert_eq!((o.code, f.messages().len()), (0, 3), "{}", o.stderr);
    assert!(o.stdout.contains("since you said no") && !o.stdout.contains("did not save it: you said no"), "{}", o.stdout);
    // a plain "not saved" first reply: no retry
    let d = tmpdir("deny-claim3");
    let f = Fake::start(vec![write(), text_reply("Okay, I didn't do that, since you said no.")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("save hello to x.txt\nn\n"));
    assert_eq!((o.code, f.messages().len()), (0, 2), "{}", o.stderr);
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
    assert_eq!(o.stderr.matches("saved summary").count(), 1, "exactly one fold: {}", o.stderr);    let reqs = f.messages();
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

/// A message too big for what is left of the window: the fold before its request is reported once (one
/// `[compacted: …]` line), not once by the fold and again for the same shrink.
#[test]
fn a_fold_before_a_request_is_reported_once() {
    let d = tmpdir("fold-once");
    big_files(&d, &["f1.txt"]);
    let f = Fake::start(vec![
        read_call(3000, "r1", "f1.txt"),
        reply_with(3800, "ok"),
        text_reply("NOTES-T1: read f1"),
        text_reply("done"),
    ]);
    let long: String = (0..700).map(|i| format!("word{i} ")).collect::<String>() + "summarise all of that";
    let o = run_elisa(&["--base-url", &f.url(), "--max-ctx", "8192"], &d, Some(&format!("read f1\n{long}\n")));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "{}", o.stderr);
    assert_eq!(o.stderr.matches("saved summary").count(), 1, "one fold: {}", o.stderr);
    assert_eq!(o.stderr.matches("[compacted:").count(), 1, "{}", o.stderr);
}

fn user_texts_of(req: &Value) -> Vec<String> {
    req["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"][0]["text"].as_str().map(String::from)).collect()
}

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"].as_array().map(|a| a.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// One mode: every agent request carries the same code system prompt and the full tool list, across a
/// plain question, a code request, a fold and a resume. Only the fold's transcript summary has its own small system.
#[test]
fn one_mode_same_system_and_tools_everywhere() {
    const SUMMARY_SYSTEM: &str = "You write short, concrete session notes for an assistant.";
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
    assert!(system(agent[0]).contains("Default to normal chat: answer directly"), "{}", system(agent[0]));
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
    // (a card comes only from a turn that ran a tool: plain chat carries none)
    let f = Fake::start_watch(vec![
        Sse::new(5, 0).tool_input("t", "glob", json!({"pattern": "*.md"})).end("tool_use", 3),
        text_reply("Hi.<state>goal: greet the user\nnow: waiting</state>"),
        text_reply("NOTES-W: greeted the user"),
    ], Some(vec![watch_view("chat-en", 0.7, 1, 1.2)]));
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("glob the markdown files, then greet me\n/compact\n"));
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

// ---------- reset and tool-call regressions ----------

/// A reply that types a tool call as text (e.g. `<tool><tool_name>read_file</tool_name>…`) gets one
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

/// The same call a third time in a turn does not run (a model can run one failing command, or one web search,
/// over and over): the model is told to use the earlier result, and a
/// second blocked step ends the tool loop (`loop_guard.rs`). The model is then asked once, with no tools, for an
/// answer from what it has (so the turn never shows only the stop line and no reply); calling a tool anyway stops.
#[test]
fn repeated_identical_tool_call_stops_the_turn() {
    let d = tmpdir("repeat");
    let same = || Sse::new(100, 0).tool_input("b", "bash", json!({"command": "echo boom; exit 3"})).end("tool_use", 5);
    let f = Fake::start(vec![same(), same(), same(), same(), text_reply("The command fails with boom (exit 3)."), text_reply("never sent")]);
    let o = run_elisa(&["-p", "run it", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("[loop guard: the model kept repeating calls that already ran; asking the model to answer"), "{}", o.stderr);
    assert!(o.stdout.contains("The command fails with boom"), "the final answer is shown: {}", o.stdout);
    assert_eq!(f.messages().len(), 5, "one more request, then none");
    assert_eq!(f.left(), 1);
    let fin = &f.messages()[4];
    assert!(fin["tools"].as_array().is_none_or(|t| t.is_empty()), "the final request offers no tools");
    assert!(fin.to_string().contains("No more tool calls"), "and carries the note");
    let sess = std::fs::read_dir(d.join(".elisa/sessions")).unwrap().map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "json")).unwrap();
    let h: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(sess).unwrap()).unwrap();
    let results: Vec<&str> = h.iter().flat_map(|m| &m.content).filter_map(|b| match b { Block::ToolResult { content, .. } => Some(content.as_str()), _ => None }).collect();
    assert_eq!(results.len(), 4);
    assert!(results[1].contains("boom") && !results[1].contains("[elisa:"), "{results:?}");
    assert!(results[2].starts_with("[elisa: not run: this exact call already ran 2 times"), "{results:?}");
    let last = h.last().unwrap();
    assert!(matches!(&last.content[0], Block::Text { text } if text.starts_with("The command fails with boom")), "{last:?}");
    assert!(!h.iter().flat_map(|m| &m.content).any(|b| matches!(b, Block::Text { text } if text.contains("No more tool calls"))), "the note is request-only");

    // asked for the final answer, it calls a tool anyway: the turn stops as before, nothing runs
    let d = tmpdir("repeat-final");
    let f = Fake::start(vec![same(), same(), same(), same(), same(), text_reply("never sent")]);
    let o = run_elisa(&["-p", "run it", "--yes", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("[stopped: the model kept repeating calls that already ran"), "{}", o.stderr);
    assert_eq!((f.messages().len(), f.left()), (5, 1));

    // the step limit after a tool step: the same one no-tools answer, never an empty reply
    let d = tmpdir("step-limit");
    let step = |i: usize| Sse::new(100, 0).tool_input("r", "bash", json!({"command": format!("echo step{i}")})).end("tool_use", 5);
    let f = Fake::start(vec![step(1), step(2), text_reply("Ran two steps: step1 and step2."), text_reply("never sent")]);
    let o = run_elisa_cfg(&["-p", "run things", "--yes", "--base-url", &f.url()], &d, None, &[], "max_steps = 2\n");
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("[step limit: the 2-step limit was reached; asking the model to answer"), "{}", o.stderr);
    assert!(o.stdout.contains("Ran two steps"), "{}", o.stdout);
    assert_eq!((f.messages().len(), f.left()), (3, 1));
    assert!(f.messages()[2]["tools"].as_array().is_none_or(|t| t.is_empty()), "the final request offers no tools");
    // it calls a tool anyway: stopped, nothing more runs
    let d = tmpdir("step-limit-tool");
    let f = Fake::start(vec![step(1), step(2), step(3), text_reply("never sent")]);
    let o = run_elisa_cfg(&["-p", "run things", "--yes", "--base-url", &f.url()], &d, None, &[], "max_steps = 2\n");
    assert!(o.stderr.contains("[stopped: the 2-step limit was reached"), "{}", o.stderr);
    assert_eq!((f.messages().len(), f.left()), (3, 1));

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
    fn start_cfg(url: &str, cwd: &Path, config: &str) -> Serve { Serve::start_full(url, cwd, config, &[], &[]) }
    /// with extra arguments (e.g. `--type anthropic`) and environment
    fn start_full(url: &str, cwd: &Path, config: &str, args: &[&str], env: &[(&str, &str)]) -> Serve {
        let home = cwd.join("_home");
        std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
        // `route = "off"` unless the test sets it: the full tool list (intent routing: evals/tools.toml route_*)
        let config = if config.contains("route =") { config.to_string() } else { format!("route = \"off\"\n{config}") };
        std::fs::write(home.join(".config/elisa/config.toml"), config).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
        cmd.args(["--serve-jsonl", "--model", "test-m", "--base-url", url]).args(args).arg("--cwd").arg(cwd).current_dir(cwd).env("HOME", &home)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, _) in std::env::vars() { if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") { cmd.env_remove(&k); } }
        for (k, v) in env { cmd.env(k, v); }
        let mut child = cmd.spawn().unwrap();
        let (stdin, out) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let j: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON: {l}: {e}"));
                assert_eq!(j["v"], elisa::jsonl::VERSION, "{l}");
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
    let no_act: Vec<Value> = evs.iter().filter(|e| e["type"] != "activity").cloned().collect();
    assert_eq!(kinds(&no_act), ["user", "waiting", "reply_done", "tool_start", "tool_detail", "tool_done", "waiting", "reply_done", "card", "turn_done"]);
    // the same turn in plain words (src/activity.rs): waiting, writing, the read step, its summary line
    let acts: Vec<String> = evs.iter().filter(|e| e["type"] == "activity").map(|e| format!("{} {}", e["phase"].as_str().unwrap(), e["text"].as_str().unwrap())).collect();
    assert_eq!(acts[..5], ["start Waiting for the model…", "update Writing the answer…", "start Reading a.txt…", "done Read a.txt", "start Thinking about the next step…"], "{acts:?}");
    assert!(acts.last().unwrap().starts_with("summary Read a.txt · "), "{acts:?}");
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
    assert_eq!(last_tool_result(&reqs[3]), (format!("denied by user: {SAID_NO}"), true));
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

/// a real dependency-free Cargo workspace (builds offline) whose `split_words` slices through multi-byte characters:
/// check-before-done runs the real cargo check / clippy / test on it
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
/// `split_words` after the fix, as one symbol
const SPLIT_FIXED_FN: &str = "/// Splits `input` into words at whitespace and ASCII punctuation.
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
const SPLIT_OLD: &str = "    for (i, b) in input.bytes().enumerate() {
        if is_boundary(b as char) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + 1;";
/// parses, but does not compile (usize + u8)
const SPLIT_NEW_BROKEN: &str = "    for (i, c) in input.char_indices() {
        if is_boundary(c) {
            if start < i {
                out.push(&input[start..i]);
            }
            start = i + c.len_utf8() as u8;";

fn cargo_project(tag: &str) -> PathBuf {
    let d = tmpdir(tag);
    std::fs::create_dir_all(d.join("crates/core/src")).unwrap();
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join("crates/core/src/split.rs"), SPLIT_RS).unwrap();
    std::fs::write(d.join("Cargo.toml"), "[workspace]\nmembers = [\"crates/core\"]\nresolver = \"2\"\n").unwrap();
    std::fs::write(d.join("crates/core/Cargo.toml"), "[package]\nname = \"ferrous-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(d.join("crates/core/src/lib.rs"), "//! ferrous core: text utilities.\npub mod split;\n").unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "memory = false\n").unwrap();
    d.canonicalize().unwrap()
}

/// `run_elisa` on a cargo project: its real cargo needs the real CARGO_HOME / RUSTUP_HOME
fn run_cargo(args: &[&str], d: &Path) -> Out {
    let home = std::env::var("HOME").unwrap();
    let env = [("CARGO_HOME", std::env::var("CARGO_HOME").unwrap_or(format!("{home}/.cargo"))),
        ("RUSTUP_HOME", std::env::var("RUSTUP_HOME").unwrap_or(format!("{home}/.rustup")))];
    run_elisa_env(args, d, None, &env)
}

#[test]
fn cargo_edit_symbol_guard_refuses_then_checks_pass() {
    let d = cargo_project("cargo-sym");
    const SPLIT: &str = "crates/core/src/split.rs";
    let f = Fake::start(vec![
        // drops the closing brace: refused by the parse guard
        Sse::new(800, 0).tool_input("e1", "edit_symbol", json!({"path": SPLIT, "name": "split_words", "source": SPLIT_FIXED_FN.trim_end_matches('}')})).end("tool_use", 9),
        Sse::new(900, 0).tool_input("e2", "edit_symbol", json!({"path": SPLIT, "name": "split_words", "source": SPLIT_FIXED_FN})).end("tool_use", 9),
        text_reply("Replaced `split_words` by name"),
    ]);
    let o = run_cargo(&["-p", "edit split_words by symbol", "--yes", "--base-url", &f.url()], &d);
    assert_eq!((o.code, f.left()), (0, 0), "{}", o.stderr);
    let (msg, err) = last_tool_result(&f.messages()[1]);
    assert!(err && msg.starts_with("refused: this change would add"), "{msg}");
    assert!(std::fs::read_to_string(d.join(SPLIT)).unwrap().contains("input.char_indices()"));
    for c in ["check: cargo check ✓", "check: cargo test ✓"] { assert!(o.stderr.contains(c), "{c}: {}", o.stderr); }
    assert!(!o.stderr.contains("✗"), "{}", o.stderr);
    assert!(o.stdout.contains("Replaced `split_words` by name"), "{}", o.stdout);
}

#[test]
fn cargo_check_fails_then_fix_round_passes() {
    let d = cargo_project("cargo-chk");
    const SPLIT: &str = "crates/core/src/split.rs";
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("e1", "edit", json!({"path": SPLIT, "old": SPLIT_OLD, "new": SPLIT_NEW_BROKEN})).end("tool_use", 9),
        text_reply("Done."),
        Sse::new(900, 0).tool_input("e2", "edit", json!({"path": SPLIT, "old": "c.len_utf8() as u8", "new": "c.len_utf8()"})).end("tool_use", 9),
        text_reply("Fixed."),
    ]);
    let o = run_cargo(&["-p", "make split_words handle multi-byte input", "--yes", "--base-url", &f.url()], &d);
    assert_eq!((o.code, f.left()), (0, 0), "{}", o.stderr);
    assert!(o.stderr.contains("check: cargo check ✗") && o.stderr.contains("→ fixing (1/2)"), "{}", o.stderr);
    assert!(o.stderr.contains("check: cargo check ✓") && o.stderr.contains("check: cargo test ✓"), "{}", o.stderr);
    assert!(!std::fs::read_to_string(d.join(SPLIT)).unwrap().contains("as u8"));
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

// ---------- one step's results vs the context; warm-ups ----------

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

/// Four parallel big reads must not push the next request over the limit, nor the fold after it
/// keep them and overflow again. Now one step's results are cut to what the window has left (largest first,
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
    assert!(err && c.starts_with("denied by user (read outside the project folder") && c.contains(&want) && c.ends_with(SAID_NO), "{c}");
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

/// the text a call the user said no to carries back to the model
const SAID_NO: &str = "the user said no, so NOTHING was done: no file was written or changed and no command ran. Reply in one \
    short sentence like \"Okay, I didn't do that, since you said no.\"; never say it was saved or done. Do not call this tool again for this request";

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
    assert!(err && c == format!("denied by user: {SAID_NO}"), "{c}");
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
    assert_eq!(last_tool_result(&f.messages()[1]), (format!("denied by user: {SAID_NO}"), true));
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

/// A model that lists a .env file as `- DB_PASSWORD: value` (or YAML / JSON / `KEY = value`)
/// must not put the raw value into the saved session and the memory log. Every form is redacted on disk.
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

/// `app.cfg` holds `API_KEY_PROD=<value>`; the tool result is stored redacted, and the model's sentence
/// "…app.cfg with the value <value>" (no key name) must not reach notes, the session or memory raw. A value the session
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

/// A `KEY=value` / `key: value` the user types inside a sentence must not stay raw in the session, metrics.jsonl or
/// memory (a ghp_ token in the same message is redacted too).
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

/// A reply with only its state card leaves the user with nothing to read. Asked once more (the nudge goes with
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
    // the retry's line is in plain words: Elisa's own term for the hidden card never reaches the person
    assert!(!o.stderr.contains("state card"), "{}", o.stderr);
    let reqs = f.messages();
    assert_eq!(reqs.len(), 2);
    let (m0, m1) = (reqs[0]["messages"].as_array().unwrap(), reqs[1]["messages"].as_array().unwrap());
    // the same history, the nudge after the user's words in that one request (never a user message of its own)
    assert_eq!(m1.len(), m0.len());
    assert_eq!(m0[..m0.len() - 1], m1[..m0.len() - 1]);
    let last = m1.last().unwrap().to_string();
    assert!(last.contains("thanks") && last.contains("one plain sentence") && !last.contains("the user"), "{last}");
    let sess = written_files(&d).into_iter().find(|p| p.to_string_lossy().contains("sessions") && p.extension().is_some_and(|e| e == "json")).unwrap();
    let s = std::fs::read_to_string(sess).unwrap();
    assert!(s.contains("You're welcome!") && !s.contains("one plain sentence"), "{s}");

    // empty twice: one retry, the user's words once alone, then a note instead of silence
    let d = tmpdir("empty-reply2");
    let f = Fake::start(vec![text_reply(card), text_reply(card), text_reply(card), text_reply("unused")]);
    let o = run_elisa(&["-p", "2+2=?", "--base-url", &f.url()], &d, None);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 1, "exactly one retry and one request alone");
    assert_eq!(f.messages()[2]["messages"].as_array().unwrap().len(), 1, "asked alone");
    assert!(o.stderr.contains("[no answer: the model wrote no text for you twice"), "{}", o.stderr);
}

// ---------- session memory: state card carry-over, /clear, ctx after Esc, tok/s null ----------

/// A new session does not take over the last session's state card (its old goal can send a small model on work
/// nobody asked for); `--continue` loads the continued session's cards (cleaned of
/// template text), and sends one only when a message's topic picks it (`topics.rs`).
#[test]
fn state_card_is_carried_only_into_a_continued_session() {
    let d = tmpdir("card-carry");
    // (a card comes only from a turn that ran a tool: plain chat carries none)
    let f = Fake::start(vec![Sse::new(5, 0).tool_input("t", "glob", json!({"pattern": "*.md"})).end("tool_use", 3),
        text_reply("ok\n<state>\ngoal: OLD-GOAL-X\ndone: …\nkeep: must-not-lose facts\nnext: OLD-NEXT-X\n</state>")]);
    let o = run_elisa(&["--base-url", &f.url()], &d, Some("glob the markdown files R1\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    // run 2: --continue has its cards, without the template placeholders (a new second: session ids are unix seconds)
    std::thread::sleep(Duration::from_millis(1100));
    let o = run_elisa(&["--base-url", "http://127.0.0.1:9", "--continue"], &d, Some("/state\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("continuing your last conversation"), "{}", o.stderr);
    assert!(o.stderr.contains("goal: OLD-GOAL-X") && o.stderr.contains("next: OLD-NEXT-X"), "{}", o.stderr);
    assert!(!o.stderr.contains("must-not-lose") && !o.stderr.contains("done: …"), "template text in the card: {}", o.stderr);
    // run 3: a new session in the same project
    std::thread::sleep(Duration::from_millis(1100));
    let g = Fake::start(vec![text_reply("fresh")]);
    let o = run_elisa(&["--base-url", &g.url()], &d, Some("an unrelated question R2\n/state\n/exit\n"));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!o.stderr.contains("continuing your last conversation"), "{}", o.stderr);
    assert!(o.stderr.contains("no state card yet"), "{}", o.stderr);
    let req = g.messages()[0].to_string();
    assert!(req.contains("R2") && !req.contains("OLD-GOAL-X") && !req.contains("OLD-NEXT-X"), "the old card reached a new session: {req}");
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
    let notes = newest_notes(&dir);
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

/// Esc at a tool's approval ends the turn with nothing kept: no "[cancelled]" stand-in reply, no cancelled
/// call, so the next request is that next message alone (a model copies "[cancelled]" into the next replies).
#[test]
fn jsonl_a_cancelled_tool_turn_leaves_no_trace_in_the_next_request() {
    let dir = tmpdir("jsonl-esc-tool");
    let f = Fake::start(vec![
        Sse::new(100, 0).tool_input("t1", "write", json!({"path": "out.txt", "content": "hi"})).end("tool_use", 9),
        text_reply("4"),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "save hi to out.txt"}));
    s.until("approve");
    s.cmd(json!({"type": "interrupt"}));
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "info" && e["text"] == "[cancelled]"), "{:?}", kinds(&evs));
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "what is 2+2?"}));
    s.until("turn_done");
    let reqs = f.messages();
    let m = reqs[1]["messages"].as_array().unwrap();
    let all = serde_json::to_string(m).unwrap();
    assert_eq!(m.len(), 1, "only the new message: {all}");
    assert!(!all.contains("[cancelled]") && !all.contains("out.txt"), "{all}");
    s.quit();
}

/// Esc mid-reply on a server that reports no usage (Ollama sends it only at the end): `reply_done` carries the
/// estimated context, not 0 (that would reset the app's ctx meter). A short reply has no decode window: tok_s null.
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

/// /clear must not replace the notes with one turn's summary, say "No files were touched" after edits, or keep a
/// "done" item no edit made. Now: merged with the earlier notes, the file list from Elisa's own
/// edit records, the invented claim dropped. Notes are per session: the earlier facts are this session's
/// own (it continues a saved one with `--continue`), and the continued session's notes file stays as it was.
#[test]
fn jsonl_clear_merges_notes_and_lists_the_files_really_changed() {
    let dir = tmpdir("jsonl-clear-merge");
    let seeded = seed_session(&dir, 100, "## Facts to remember\n- Codename BLUE HERON, owner Mira Tanaka.\n");
    let summary = "## Done\n- Wrote hello.txt with a greeting. REAL-DONE\n- Added restock_cost(items) to core.py.\n- Wrote hello.txt with a greeting. REAL-DONE\n\n\
        ## Files touched\nNo files were touched in this session.\n\n## Current state / next step\nnothing open";
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "write", json!({"path": "hello.txt", "content": "hi there\n"})).end("tool_use", 12),
        text_reply("Wrote it."),
        text_reply(summary),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\n", &["--continue"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "create hello.txt saying hi there"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    s.until("turn_done");
    assert_eq!(std::fs::read_to_string(dir.join("hello.txt")).unwrap(), "hi there\n");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    s.until("turn_done");
    let notes = newest_notes(&dir);
    assert!(notes.contains("BLUE HERON"), "earlier facts kept: {notes}");
    assert!(notes.contains(&format!("{}\n- hello.txt: written (new file)", elisa::context::FILES_NOTES)), "{notes}");
    assert_eq!(notes.matches("REAL-DONE").count(), 1, "{notes}");
    assert!(!notes.contains("No files were touched") && !notes.contains("restock_cost"), "{notes}");
    assert_eq!(std::fs::read_to_string(&seeded).unwrap(), "## Facts to remember\n- Codename BLUE HERON, owner Mira Tanaka.\n", "the notes of the continued session changed");
    s.quit();
}

/// eval `context.clear_merges_notes` with memory on (the default): the note's facts were also pinned facts, so the
/// first message after /clear carried "Codename BLUE HERON" twice (notes + `[state]`), came out bigger than the
/// two-turn conversation, and the "always shrink" rule cut the fact from the notes, leaving only the file records.
/// Now: the fact stays in the notes, is not repeated as a pinned fact, and the context may simply not shrink.
/// (Notes are per session: the facts are this session's own, from the session it continues.)
#[test]
fn jsonl_clear_with_memory_keeps_the_earlier_facts() {
    let dir = tmpdir("jsonl-clear-mem");
    let seeded = seed_session(&dir, 100, "## Facts to remember\n- Codename BLUE HERON, owner Mira Tanaka.\n");
    let summary = "## Done\n- Wrote hello.txt with a greeting. REAL-DONE\n\n## Files touched\nNo files were touched in this session.\n\n\
        ## Current state / next step\nnothing open";
    let f = Fake::start(vec![
        Sse::new(800, 0).tool_input("t1", "write", json!({"path": "hello.txt", "content": "hi there\n"})).end("tool_use", 12),
        text_reply("Wrote it."),
        text_reply(summary),
        text_reply("BLUE HERON."),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = true\n", &["--continue"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "create hello.txt saying hi there"}));
    s.until("approve");
    s.cmd(json!({"type": "approve", "answer": "yes"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    let evs = s.until("turn_done");
    let info = evs.iter().filter(|e| e["type"] == "info").map(|e| e["text"].as_str().unwrap()).find(|t| t.starts_with("[cleared:")).unwrap();
    assert!(info.contains("notes saved"), "{info}");
    let notes = newest_notes(&dir);
    assert!(notes.contains("Codename BLUE HERON, owner Mira Tanaka."), "earlier facts kept: {notes}");
    assert!(notes.contains(&format!("{}\n- hello.txt: written (new file)", elisa::context::FILES_NOTES)), "{notes}");
    assert!(notes.contains("REAL-DONE") && notes.contains("nothing open"), "{notes}");
    s.cmd(json!({"type": "send", "text": "what is the codename?"}));
    s.until("turn_done");
    let reqs = f.messages();
    let t = reqs[3]["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert_eq!(t.matches("BLUE HERON").count(), 1, "the fact once, in the notes: {t}");
    assert!(!t.contains("Pinned facts"), "{t}");
    assert!(std::fs::read_to_string(&seeded).unwrap().contains("BLUE HERON"), "the notes of the continued session were lost");
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

// ---------- a panic in a turn, Esc before the first token, max_tokens with nothing shown ----------

/// A panic inside a turn (e.g. an index out of bounds in elisa-mem) reaches the user as an error event plus
/// `turn_done`, and the next message is an ordinary turn (before, the agent thread died and the app waited forever).
#[test]
fn jsonl_a_panic_in_a_turn_is_an_error_and_the_session_goes_on() {
    let dir = tmpdir("jsonl-panic");
    let f = Fake::start(vec![text_reply("still here")]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = true\n", &[], &[("ELISA_TEST_PANIC", "PANIC-ME")]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "please PANIC-ME now"}));
    let evs = s.until("turn_done");
    let err = evs.iter().find(|e| e["type"] == "error").unwrap_or_else(|| panic!("no error event: {evs:?}"));
    let t = err["text"].as_str().unwrap();
    assert!(t.starts_with("internal error: injected test panic at src/repl/turn.rs:"), "{t}");
    assert!(t.contains("the session goes on"), "{t}");
    assert_eq!(evs.iter().filter(|e| e["type"] == "turn_done").count(), 1);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "are you still there?"}));
    let evs = s.until("turn_done");
    let text: String = evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(text, "still here", "{evs:?}");
    // the history the model got: the panicked request closed by a reply, roles alternating
    let msgs = f.messages();
    let m = msgs.last().unwrap()["messages"].as_array().unwrap().clone();
    let roles: Vec<&str> = m.iter().map(|x| x["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user"], "{m:?}");
    assert_eq!(m[1]["content"][0]["text"], "[stopped: internal error]");
    s.quit();
}

/// Esc while the server has not sent a byte yet (prefill of a long prompt): the call stops within ~300 ms instead of
/// waiting for the first bytes (seconds on a long prefill).
#[test]
fn cancel_before_the_first_byte_stops_at_once() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                let _ = c.read(&mut buf);
                // the first byte comes after 5 s
                std::thread::sleep(Duration::from_secs(5));
                let _ = write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
            });
        }
    });
    for kind in ["gateway", "ollama"] {
        let c = client(&format!("http://127.0.0.1:{port}"));
        let c = if kind == "ollama" {
            let c = Client::new(&Config { base_url: format!("http://127.0.0.1:{port}/v1"), model: "test-m".into(), transport: elisa::provider::Transport::OpenAi, ..Config::default() });
            c.use_ollama(4096, false);
            c
        } else { c };
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(200)); c2.store(true, Ordering::SeqCst); });
        let t0 = Instant::now();
        let msgs = [Message::user_text("hi")];
        let r = c.send(&Request { system: "sys", tools: &[], messages: &msgs, max_tokens: 100 }, &cancel, &mut |_| {}).unwrap();
        let dt = t0.elapsed();
        assert_eq!(r.stop_reason, "cancelled", "{kind}");
        assert!(dt < Duration::from_millis(500), "{kind}: Esc took {dt:?} (cancel at 200 ms)");
    }
}

/// A simple turn (small output cap) whose reply spends the whole cap with no text for the user is sent once more
/// without the simple-turn cap; the user gets the answer, not just a stop line.
#[test]
fn simple_turn_that_hits_max_tokens_with_no_text_is_retried_uncapped() {
    let dir = tmpdir("jsonl-maxtok");
    let f = Fake::start(vec![
        // only a tool call cut at the limit would be shown as cut; this one wrote nothing visible at all
        Sse::new(500, 0).end("max_tokens", 1024),
        text_reply("Both answers: 4 and 9."),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "hi"}));
    let evs = s.until("turn_done");
    let text: String = evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(text, "Both answers: 4 and 9.", "{evs:?}");
    assert!(evs.iter().any(|e| e["type"] == "info" && e["text"].as_str().unwrap().contains("asking once more without that limit")), "{evs:?}");
    let reqs = f.messages();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0]["max_tokens"], 1024, "the simple turn's cap (1/8 of the 8K window)");
    assert!(reqs[1]["max_tokens"].as_u64().unwrap() > 1024, "{}", reqs[1]["max_tokens"]);
    assert_eq!(reqs[0]["messages"], reqs[1]["messages"], "the same request, only the cap differs");
    s.quit();

    // twice nothing: a clear line instead of the bare stop line
    let dir = tmpdir("jsonl-maxtok2");
    let f = Fake::start(vec![Sse::new(500, 0).end("max_tokens", 1024), Sse::new(500, 0).end("max_tokens", 4096)]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "hi"}));
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "info" && e["text"].as_str().unwrap().contains("wrote no text for you; ask again")), "{evs:?}");
    s.quit();
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

/// A saved earlier session `id` in project `d` (one exchange) with its own notes file `notes`: returns the notes path.
fn seed_session(d: &std::path::Path, id: u64, notes: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(d.join(".elisa/sessions")).unwrap();
    std::fs::create_dir_all(d.join(".elisa/notes")).unwrap();
    let h = json!([{"role": "user", "content": [{"type": "text", "text": "remember the codename BLUE HERON, the owner is Mira Tanaka"}]},
        {"role": "assistant", "content": [{"type": "text", "text": "Noted."}]}]);
    std::fs::write(d.join(format!(".elisa/sessions/{id}.json")), h.to_string()).unwrap();
    let p = d.join(format!(".elisa/notes/{id}.md"));
    std::fs::write(&p, notes).unwrap();
    p
}

/// The old Python-async task of a problem report: notes of an earlier session.
const OLD_TASK_NOTES: &str = "## Goal\nFix a Python async example that raises RuntimeError with asyncio.run. OLD-TASK-GOAL\n\n\
    ## Current state / next step\nCheck the directory for the async file. OLD-TASK-NEXT\n\n## Facts to remember\n- OLD-TASK-FACT\n";

/// In a folder whose earlier session left notes (the old shared `.elisa/notes.md` and a per-session
/// notes file), a new session must not answer "hey" and "what can you do for me?" with that old task. A new session starts
/// clean: no earlier notes in any request, and the earlier notes stay on disk as they were.
#[test]
fn jsonl_new_session_with_old_notes_answers_hey_without_the_old_task() {
    let dir = tmpdir("jsonl-new-clean");
    let seeded = seed_session(&dir, 100, OLD_TASK_NOTES);
    std::fs::write(dir.join(".elisa/notes.md"), OLD_TASK_NOTES).unwrap();
    let f = Fake::start(vec![text_reply("Hi! How can I help?"), text_reply("I can chat, answer questions and help with files.")]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "hey"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "what can you do for me?"}));
    s.until("turn_done");
    s.quit();
    for r in f.messages() {
        let body = r.to_string();
        for old in ["OLD-TASK", "asyncio", "Notes from earlier sessions", "Background from an earlier session"] {
            assert!(!body.contains(old), "{old:?} reached a new session's request: {body}");
        }
    }
    let first = f.messages()[0]["messages"][0]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(first.ends_with("hey"), "{first}");
    assert_eq!(std::fs::read_to_string(&seeded).unwrap(), OLD_TASK_NOTES, "the earlier session's notes changed");
    assert_eq!(std::fs::read_to_string(dir.join(".elisa/notes.md")).unwrap(), OLD_TASK_NOTES, "the legacy notes.md changed");
    // the new session's own notes (if any) are a file of their own
    let own: Vec<_> = std::fs::read_dir(dir.join(".elisa/notes")).unwrap().flatten().map(|e| e.path()).filter(|p| *p != seeded).collect();
    for p in own { assert!(!std::fs::read_to_string(&p).unwrap().contains("OLD-TASK"), "{}", p.display()); }
}

/// `--continue` after a new session in between: the continued (newest) session comes back with its history and its
/// own notes; `/clear` in it then carries those notes, and the notes of the session it continued are untouched.
/// An old fact the conversation never mentions (another task's, carried in from a stale notes file) is
/// not merged forward; one it talks about is.
#[test]
fn jsonl_continue_restores_history_and_its_own_notes() {
    let dir = tmpdir("jsonl-continue-notes");
    let own = "## Facts to remember\n- The codename to remember is HERON-FACT\n- Fix the Python asyncio RuntimeError example OLD-TASK-FACT\n";
    let seeded = seed_session(&dir, 100, own);
    let f = Fake::start(vec![text_reply("Back on it."), text_reply("## Done\n- talked\n\n## Current state / next step\nnothing open"), text_reply("ok")]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\n", &["--continue"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "/notes"}));
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "info" && e["text"].as_str().unwrap_or("").contains("OLD-TASK-FACT")), "continued notes: {evs:?}");
    s.cmd(json!({"type": "send", "text": "go on"}));
    s.until("turn_done");
    let body = f.messages()[0].to_string();
    assert!(body.contains("remember the codename") && body.contains("Noted."), "the continued history: {body}");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "next"}));
    s.until("turn_done");
    s.quit();
    let t = f.messages()[2]["messages"][0]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(t.contains("HERON-FACT"), "after /clear the continued session's notes carry: {t}");
    assert!(!t.contains("OLD-TASK-FACT"), "a fact the conversation never mentions is not merged forward: {t}");
    assert!(newest_notes(&dir).contains("HERON-FACT") && !newest_notes(&dir).contains("OLD-TASK-FACT"));
    assert_eq!(std::fs::read_to_string(&seeded).unwrap(), own, "the continued session's own file changed");
}

/// A continued conversation must not look empty in the app (a "resumed N messages" line only). Right after the banner
/// a `history` event lists its earlier turns: the user's own words (no Working directory / OS / Date header, notes,
/// `[state]`, situation line; an attached document as its reference line), the replies as written, a tool call as one
/// line, no tool results.
#[test]
fn jsonl_continue_sends_the_earlier_turns() {
    let dir = tmpdir("jsonl-continue-history");
    std::fs::create_dir_all(dir.join(".elisa/sessions")).unwrap();
    let first = "Working directory: /x\nOS: macos\nDate: 2026-11-09\nLocal time now: 2026-11-09 15:15 Monday\n\n## Notes from this conversation (written by Elisa)\n## Done\nNOTES-BODY\n\n[state]\ngoal: read the first few pages\n[/state]\n\n## Request\nwhat day is today?";
    let h = json!([
        {"role": "user", "content": [{"type": "text", "text": first}]},
        {"role": "assistant", "content": [{"type": "text", "text": "Today is **Monday**."}]},
        {"role": "user", "content": [{"type": "text", "text": "what is in README.md?"}, {"type": "text", "text": "[Attached: paper.pdf, 3 pages · .elisa/uploads/1/paper.pdf]\nDOC-TEXT-HERE"}]},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "read", "input": {"path": "README.md"}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "TOOL-RESULT-BODY"}]},
        {"role": "assistant", "content": [{"type": "text", "text": "It describes the project."}]}
    ]);
    std::fs::write(dir.join(".elisa/sessions/100.json"), h.to_string()).unwrap();
    let f = Fake::start(vec![]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\n", &["--continue"], &[]);
    let evs = s.until("ready");
    s.quit();
    let hi = evs.iter().position(|e| e["type"] == "history").unwrap_or_else(|| panic!("no history event: {evs:?}"));
    let bi = evs.iter().position(|e| e["type"] == "banner").unwrap();
    assert!(bi < hi, "history comes after the banner");
    let items = evs[hi]["items"].as_array().unwrap();
    let pairs: Vec<(String, String)> = items.iter().map(|i| (i["role"].as_str().unwrap().to_string(), i["text"].as_str().unwrap().to_string())).collect();
    assert_eq!(pairs, vec![
        ("user".to_string(), "what day is today?".to_string()),
        ("assistant".to_string(), "Today is **Monday**.".to_string()),
        ("user".to_string(), "what is in README.md?\n[Attached: paper.pdf, 3 pages · .elisa/uploads/1/paper.pdf]".to_string()),
        ("assistant".to_string(), "used read on README.md\n\nIt describes the project.".to_string()),
    ]);
    let all = evs[hi].to_string();
    for bad in ["Working directory", "NOTES-BODY", "[state]", "DOC-TEXT-HERE", "TOOL-RESULT-BODY", "Local time"] { assert!(!all.contains(bad), "{bad} in {all}"); }
}

/// With memory on, a session's notes' "Facts to remember" become facts in memory; they used to come back as
/// "Pinned facts" in every new session's `[state]`. Now only facts the user pinned (/pin) do: the note facts stay
/// with their session.
#[test]
fn jsonl_new_session_with_memory_gets_only_user_pinned_facts() {
    let dir = tmpdir("jsonl-new-mem");
    let summary = "## Done\n- talked\n\n## Facts to remember\n- Old task: fix the asyncio example OLD-TASK-FACT\n";
    let long = format!("Sure. {}", "Some context about the old task. ".repeat(20));
    let f = Fake::start(vec![text_reply(&long), text_reply(summary)]);
    let mut s = Serve::start_cfg(&f.url(), &dir, "memory = true\n");
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "/pin the release branch is trunk USER-PIN"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "fix my asyncio example"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "/clear"}));
    s.until("turn_done");
    s.quit();
    assert!(newest_notes(&dir).contains("OLD-TASK-FACT"), "the first session's notes keep its facts");
    std::thread::sleep(Duration::from_millis(1100));
    let g = Fake::start(vec![text_reply("Hi!")]);
    let mut s = Serve::start_cfg(&g.url(), &dir, "memory = true\n");
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "hey"}));
    s.until("turn_done");
    s.quit();
    let body = g.messages()[0].to_string();
    assert!(!body.contains("OLD-TASK-FACT"), "a note fact of an earlier session reached a new one: {body}");
    assert!(body.contains("USER-PIN"), "a fact the user pinned carries: {body}");
}

/// Lazy MCP start, end to end: a user server whose tool list is cached starts no process for a plain chat turn; the
/// first call to one of its tools starts it and a second call reuses it (its start script logs each start).
#[test]
fn a_plain_chat_turn_starts_no_mcp_server_and_the_first_call_starts_it_once() {
    let d = tmpdir("mcp-lazy");
    let marker = d.join("started");
    let exe = env!("CARGO_BIN_EXE_elisa");
    let script = format!("echo $$ >> '{}'; exec '{exe}' --mcp-serve reason", marker.display());
    let command = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    let home = d.join("_home");
    elisa::pins::cache_list(&elisa::pins::lists_in(&home), &elisa::pins::key("m", &command, ""), elisa::mcp_reason::tools().as_array().unwrap()).unwrap();
    // route = "off": every turn carries every tool, so the plain turn below is offered m_calc and still starts nothing
    let cfg = format!("route = \"off\"\nweb = \"off\"\ndocs = \"off\"\nreason = \"off\"\nsecurity = \"off\"\nmemory = false\nchecks = false\n\n[mcp.m]\ncommand = {}\n",
        serde_json::to_string(&command).unwrap());
    let starts = || std::fs::read_to_string(&marker).map(|t| t.lines().count()).unwrap_or(0);
    // a plain chat turn: the tools are offered, no server runs
    let f = Fake::start(vec![text_reply("hi")]);
    let o = run_elisa_cfg(&["-p", "hello", "--base-url", &f.url()], &d, None, &[], &cfg);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(tool_names(&f.messages()[0]).iter().any(|n| n == "m_calc"), "{:?}", tool_names(&f.messages()[0]));
    assert_eq!(starts(), 0, "a plain chat turn started the server: {}", o.stderr);
    // two calls: the first starts it, the second reuses it
    let f = Fake::start(vec![
        Sse::new(5, 0).tool_input("t1", "m_calc", json!({"expr": "2+3"})).end("tool_use", 3),
        Sse::new(5, 0).tool_input("t2", "m_calc", json!({"expr": "4*5"})).end("tool_use", 3),
        text_reply("done"),
    ]);
    let o = run_elisa_cfg(&["-p", "sums", "--yes", "--base-url", &f.url()], &d, None, &[], &cfg);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let m = f.messages();
    assert!(last_tool_result(&m[1]).0.contains('5') && !last_tool_result(&m[1]).1, "{:?}", last_tool_result(&m[1]));
    assert!(last_tool_result(&m[2]).0.contains("20") && !last_tool_result(&m[2]).1, "{:?}", last_tool_result(&m[2]));
    assert_eq!(starts(), 1, "one start for both calls: {}", o.stderr);
}

// ---------- a backend with no prompt cache (llama.cpp --no-cache-prompt, a plain GGUF server) ----------

/// Elisa must work the same without a server-side prompt cache: every request carries the whole conversation (no
/// server state assumed), the context meter comes from `prompt_tokens` alone, the status line never claims reuse, and
/// the metrics say "not reported" (null) or 0, never a guess. Turn 1: no cache fields at all; turn 2: llama.cpp's
/// shape with prompt caching off (`cached_tokens: 0`).
#[test]
fn no_prompt_cache_backend_works_the_same() {
    let d = tmpdir("nocache");
    std::fs::write(d.join("a.txt"), "the answer is 42\n").unwrap();
    let f = Fake::start(vec![
        oa_reply(&[json!({"content": "Hello"}), json!({"content": " there."})], "stop", json!({"prompt_tokens": 400, "completion_tokens": 6, "total_tokens": 406})),
        oa_reply(&[json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a.txt\"}"}}]})], "tool_calls",
            json!({"prompt_tokens": 900, "completion_tokens": 12, "prompt_tokens_details": {"cached_tokens": 0}})),
        oa_reply(&[json!({"content": "It says the answer is 42."})], "stop",
            json!({"prompt_tokens": 1000, "completion_tokens": 9, "prompt_tokens_details": {"cached_tokens": 0}})),
    ]);
    let home = d.join("_home/.config/elisa");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("providers.toml"), format!("active = \"l\"\n[providers.l]\ntype = \"openai\"\nbase_url = \"{}/v1\"\nmodel = \"local\"\nmax_ctx = 4096\n", f.url())).unwrap();
    let o = run_elisa(&["--no-checks"], &d, Some("hey\nread a.txt and tell me what it says\n/exit\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "stderr: {}", o.stderr);
    assert!(o.stdout.contains("Hello there.") && o.stdout.contains("It says the answer is 42."), "stdout: {}", o.stdout);
    assert!(!o.stderr.contains("reused"), "no reuse claimed without a cache: {}", o.stderr);
    // ctx = the server's prompt_tokens + the reply, not a delta and not the estimate
    assert!(o.stderr.contains("ctx 406/4.1K"), "{}", o.stderr);
    assert!(o.stderr.contains("ctx 1.0K/4.1K"), "{}", o.stderr);

    // stateless: each request resends the whole conversation, the last one holds the first turn and the tool result
    let reqs: Vec<Value> = f.log().iter().filter(|r| r.path == "/v1/chat/completions").map(|r| r.json()).collect();
    assert_eq!(reqs.len(), 3);
    let msgs = |r: &Value| r["messages"].as_array().unwrap().clone();
    let (m0, m2) = (msgs(&reqs[0]), msgs(&reqs[2]));
    assert_eq!(m0[0]["role"], "system");
    assert_eq!(m2[0], m0[0], "the same system turn every time");
    let all = serde_json::to_string(&m2).unwrap();
    assert!(all.contains("Hello there.") && all.contains("the answer is 42"), "{all}");
    assert!(m2.iter().any(|m| m["role"] == "tool"), "{all}");

    // metrics: not reported = null, reported 0 = 0; input_tokens is the whole prompt
    let text = std::fs::read_to_string(d.join(".elisa/metrics.jsonl")).unwrap();
    let calls: Vec<Value> = text.lines().flat_map(|l| serde_json::from_str::<Value>(l).unwrap()["calls"].as_array().unwrap().clone()).collect();
    let agent: Vec<&Value> = calls.iter().filter(|c| c["kind"] == "agent").collect();
    assert_eq!(agent.iter().map(|c| c["input_tokens"].as_u64().unwrap()).collect::<Vec<_>>(), [400, 900, 1000], "{text}");
    assert_eq!(agent.iter().map(|c| c["cached_input_tokens"].clone()).collect::<Vec<_>>(), [Value::Null, json!(0), json!(0)], "{text}");
}

// ---------- backend protocol replicas (src/backend.rs) ----------
//
// One small server per backend, answering with the shapes of that backend's public docs (URL above each replica):
// its model / props endpoints for detection, and a streamed chat reply with a tool call, its usage fields and its
// quirks. Each test checks detection (or the host table, for cloud APIs that cannot be pointed at 127.0.0.1), the
// request Elisa sends (the backend's own cache controls) and that the reply comes back as a correct tool turn.

mod replicas {
    use super::Recorded;
    use elisa::api::{Client, Request};
    use elisa::backend;
    use elisa::config::Config;
    use elisa::provider::Transport;
    use elisa::types::{Block, Message, Reply, Role, ToolDef};
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// (status, content type, body)
    pub type Answer = (u16, &'static str, String);

    pub struct Replica { pub port: u16, pub log: Arc<Mutex<Vec<Recorded>>> }

    impl Replica {
        /// `routes`: (method, path with query, answer) for the info endpoints; any POST to a chat path
        /// (`/chat/completions`, `/api/chat`, `/v1/messages`) takes the next of `chats`. `server`: the Server header.
        pub fn start(server: &'static str, routes: Vec<(&'static str, &'static str, Answer)>, chats: Vec<Answer>) -> Replica {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let log = Arc::new(Mutex::new(Vec::new()));
            let chats = Arc::new(Mutex::new(VecDeque::from(chats)));
            let routes = Arc::new(routes);
            let (g, c) = (log.clone(), chats);
            std::thread::spawn(move || for s in l.incoming() {
                let Ok(s) = s else { continue };
                let (g, c, r) = (g.clone(), c.clone(), routes.clone());
                std::thread::spawn(move || { let _ = serve(s, server, &r, &c, &g); });
            });
            Replica { port, log }
        }
        pub fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
        pub fn reqs(&self, path_end: &str) -> Vec<Value> {
            self.log.lock().unwrap().iter().filter(|r| r.method == "POST" && r.path.ends_with(path_end)).map(|r| r.json()).collect()
        }
        pub fn hits(&self, path: &str) -> usize { self.log.lock().unwrap().iter().filter(|r| r.path == path).count() }
    }

    fn serve(mut c: TcpStream, server: &str, routes: &[(&str, &str, Answer)], chats: &Mutex<VecDeque<Answer>>, log: &Mutex<Vec<Recorded>>) -> std::io::Result<()> {
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
        let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        log.lock().unwrap().push(Recorded { method: method.clone(), path: path.clone(), headers, body: String::from_utf8_lossy(&body).into_owned() });
        let chat = method == "POST" && (path.ends_with("/chat/completions") || path == "/api/chat" || path == "/v1/messages");
        let (code, ctype, b) = match routes.iter().find(|(m, p, _)| *m == method && *p == path) {
            Some((_, _, a)) => a.clone(),
            None if chat => chats.lock().unwrap().pop_front().unwrap_or((500, "application/json", r#"{"error":{"message":"script exhausted"}}"#.into())),
            None => (404, "application/json", r#"{"error":{"code":404,"message":"File Not Found","type":"not_found_error"}}"#.into()),
        };
        write!(c, "HTTP/1.1 {code} X\r\nserver: {server}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len())?;
        c.flush()
    }

    pub fn json_ok(v: Value) -> Answer { (200, "application/json", v.to_string()) }
    /// an OpenAI-style SSE stream: one `data:` line per chunk, then `[DONE]`
    pub fn sse(chunks: &[Value]) -> Answer {
        let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        s += "data: [DONE]\n\n";
        (200, "text/event-stream", s)
    }
    pub fn chunk(delta: Value, finish: Option<&str>) -> Value {
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "m", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    pub fn tools() -> Vec<ToolDef> {
        vec![ToolDef { name: "read".into(), description: "Read a file".into(), input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}) }]
    }

    /// A tool turn's second request: the user asked, the model called `read`, the result came back.
    pub fn tool_history(id: &str) -> Vec<Message> {
        vec![
            Message::user_text("read README.md"),
            Message::new(Role::Assistant, vec![Block::ToolUse { id: id.into(), name: "read".into(), input: json!({"path": "README.md"}) }]),
            Message::new(Role::User, vec![Block::ToolResult { tool_use_id: id.into(), content: "# Elisa".into(), is_error: false }]),
        ]
    }

    pub fn rclient(url: &str, transport: Transport, caps: backend::Caps, key: &str) -> Client {
        let cfg = Config { base_url: url.into(), model: "m".into(), api_key: key.into(), transport, max_ctx: Some(8192), ..Config::default() };
        let mut c = Client::new(&cfg);
        c.set_caps(caps, 8192);
        c
    }

    pub fn rsend(c: &Client, system: &str, msgs: &[Message]) -> Result<Reply, String> {
        let t = tools();
        c.send(&Request { system, tools: &t, messages: msgs, max_tokens: 256 }, &AtomicBool::new(false), &mut |_| {})
    }

    pub fn call_of(r: &Reply) -> (String, Value) {
        r.content.iter().find_map(|b| if let Block::ToolUse { name, input, .. } = b { Some((name.clone(), input.clone())) } else { None }).expect("a tool call")
    }

    pub fn detect(url: &str) -> backend::Caps { backend::detect_with(&Transport::OpenAi, &format!("{url}/v1"), "m", "", 3000) }

    /// the usual OpenAI-shaped tool call: id/name in the first fragment, the arguments in pieces
    pub fn fragmented_call() -> Vec<Value> {
        vec![
            chunk(json!({"role": "assistant", "content": null, "tool_calls": [{"index": 0, "id": "call_abc", "type": "function", "function": {"name": "read", "arguments": ""}}]}), None),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"pa"}}]}), None),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "th\": \"README.md\"}"}}]}), None),
            chunk(json!({}), Some("tool_calls")),
        ]
    }

    pub fn assert_read_call(r: &Reply) {
        assert_eq!(r.stop_reason, "tool_use", "{r:?}");
        assert_eq!(call_of(r), ("read".to_string(), json!({"path": "README.md"})));
    }

    pub use elisa::backend::{Cache as C, Kind as K, Tools as T};
}

use replicas::*;
use elisa::backend;
use elisa::provider::Transport;

// Ollama: https://github.com/ollama/ollama/blob/main/docs/api.md (GET /api/version, POST /api/show, POST /api/chat NDJSON)
#[test]
fn replica_ollama_detects_native_api_window_and_tool_turn() {
    let show = json!({"modelfile": "", "details": {"family": "qwen3vl"}, "model_info": {"general.architecture": "qwen3vl", "qwen3vl.context_length": 262144},
        "capabilities": ["completion", "vision", "tools", "thinking"]});
    let ndjson = [
        json!({"model": "m", "created_at": "2026-11-09T00:00:00Z", "message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "read", "arguments": {"path": "README.md"}}}]}, "done": false}),
        json!({"model": "m", "created_at": "2026-11-09T00:00:01Z", "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop",
            "total_duration": 1, "load_duration": 1, "prompt_eval_count": 120, "prompt_eval_cached_count": 100, "prompt_eval_duration": 1, "eval_count": 12, "eval_duration": 400000000}),
    ].iter().map(|v| format!("{v}\n")).collect::<String>();
    // GET /api/ps: the loaded model's context (https://github.com/ollama/ollama/blob/main/docs/api.md#list-running-models)
    let ps = json!({"models": [{"name": "m", "model": "m", "size": 3732783758u64, "details": {"format": "gguf"}, "expires_at": "2026-11-09T13:50:39Z", "size_vram": 3732783758u64, "context_length": 16384}]});
    let f = Replica::start("", vec![("GET", "/api/version", json_ok(json!({"version": "0.5.1"}))), ("POST", "/api/show", json_ok(show.clone())), ("GET", "/api/ps", json_ok(ps))],
        vec![(200, "application/x-ndjson", ndjson)]);
    let caps = detect(&f.url());
    assert_eq!((caps.kind, caps.ctx, caps.vision, caps.thinking, caps.tools, caps.cache), (K::Ollama, Some(16384), Some(true), true, T::Native, C::OllamaKeepAlive));
    // not loaded: the model's maximum, capped
    let idle = Replica::start("", vec![("GET", "/api/version", json_ok(json!({"version": "0.5.1"}))), ("POST", "/api/show", json_ok(show)), ("GET", "/api/ps", json_ok(json!({"models": []})))], vec![]);
    assert_eq!(detect(&idle.url()).ctx, Some(backend::OLLAMA_CAP));
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (20, 100));
    let b = &f.reqs("/api/chat")[0];
    assert_eq!(b["options"]["num_ctx"], 8192, "native API with the session's window");
    assert!(b.get("keep_alive").is_none(), "the user's keep_alive is left alone");
}

// Ollama, a model without "tools" in its capabilities: https://github.com/ollama/ollama/blob/main/docs/api.md#show-model-information
#[test]
fn replica_ollama_model_without_tools_gets_them_as_text() {
    let show = json!({"model_info": {"gemma3.context_length": 8192}, "capabilities": ["completion"]});
    let text = "I will read it.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"README.md\"}}</tool_call>";
    let ndjson = [json!({"message": {"role": "assistant", "content": text}, "done": false}), json!({"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 50, "eval_count": 20})]
        .iter().map(|v| format!("{v}\n")).collect::<String>();
    let f = Replica::start("", vec![("GET", "/api/version", json_ok(json!({"version": "0.5.1"}))), ("POST", "/api/show", json_ok(show))], vec![(200, "application/x-ndjson", ndjson)]);
    let caps = detect(&f.url());
    assert_eq!(caps.tools, T::Text);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &tool_history("t1")).unwrap();
    assert_read_call(&r);
    assert_eq!(r.content[0], Block::Text { text: "I will read it.".into() });
    let b = &f.reqs("/api/chat")[0];
    assert!(b.get("tools").is_none(), "no native tools sent");
    assert!(b["messages"][0]["content"].as_str().unwrap().contains("<tool_call>"), "described in the system prompt");
    assert!(b.to_string().contains("tool_result"), "the earlier result went as text");
}

// llama.cpp llama-server: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
// (GET /props, POST /slots/{id}?action=save|restore, streamed chunks with `timings`, error {"error":{code,message,type}})
#[test]
fn replica_llamacpp_props_slots_cache_prompt_and_fragmented_tool_call() {
    let props = json!({"default_generation_settings": {"id": 0, "id_task": -1, "n_ctx": 16384, "speculative": false, "is_processing": false, "params": {}},
        "total_slots": 1, "model_path": "../models/x.gguf", "chat_template": "...", "chat_template_caps": {"supports_tools": true}, "modalities": {"vision": false}, "build_info": "b11429-d81235049"});
    let mut chunks = fragmented_call();
    chunks.push(json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [], "usage": {"completion_tokens": 9, "prompt_tokens": 2524, "total_tokens": 2533},
        "timings": {"cache_n": 2400, "prompt_n": 124, "prompt_ms": 30.9, "predicted_n": 9, "predicted_ms": 120.0}}));
    let save = json_ok(json!({"id_slot": 0, "filename": "f.bin", "n_saved": 2533, "n_written": 14309796, "timings": {"save_ms": 49.8}}));
    let f = Replica::start("llama.cpp", vec![
        ("GET", "/props", json_ok(props)),
        // a probe of a missing file: fails, but not "not supported" (the server has --slot-save-path)
        ("POST", "/slots/0?action=restore", (400, "application/json", r#"{"error":{"code":400,"message":"failed to restore slot","type":"invalid_request_error"}}"#.into())),
        ("POST", "/slots/0?action=save", save),
    ], vec![sse(&chunks)]);
    let caps = detect(&f.url());
    assert_eq!((caps.kind, caps.ctx, caps.ctx_hard, caps.slots, caps.slot_save, caps.cache), (K::LlamaCpp, Some(16384), true, 1, true, C::LlamaCpp));
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (124, 2400), "reuse from timings.cache_n");
    let b = &f.reqs("/chat/completions")[0];
    assert_eq!(b["cache_prompt"], true);
    assert!(b.get("id_slot").is_none(), "one slot: no pinning needed");
    assert_eq!(c.slot_save("elisa-p-0.bin").unwrap(), (2533, 49.8));
    assert_eq!(f.reqs("action=save")[0], json!({"filename": "elisa-p-0.bin"}));
}

// llama.cpp without --slot-save-path (501 not_supported_error) and without --jinja (tools refused):
// https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
#[test]
fn replica_llamacpp_without_slot_path_or_jinja() {
    let props = json!({"default_generation_settings": {"n_ctx": 4096}, "total_slots": 1, "modalities": {"vision": false}});
    let refused = (500, "application/json", r#"{"error":{"code":500,"message":"tools param requires --jinja flag","type":"server_error"}}"#.to_string());
    let text = sse(&[chunk(json!({"role": "assistant", "content": "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"README.md\"}}</tool_call>"}), None), chunk(json!({}), Some("stop"))]);
    let f = Replica::start("llama.cpp", vec![("GET", "/props", json_ok(props)),
        ("POST", "/slots/0?action=restore", (501, "application/json", r#"{"error":{"code":501,"message":"This server does not support slots action. Start it with `--slot-save-path`","type":"not_supported_error"}}"#.into()))],
        vec![refused, text]);
    let caps = detect(&f.url());
    assert!(!caps.slot_save);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    assert!(c.slot_save("x.bin").is_err(), "no slot calls without the save path");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert!(c.text_tools(), "after the refusal the tools stay text");
    let reqs = f.reqs("/chat/completions");
    assert!(reqs[0].get("tools").is_some() && reqs[1].get("tools").is_none());
    assert_eq!(f.hits("/slots/0?action=save"), 0);
}

// LM Studio: https://lmstudio.ai/docs/developer/rest/endpoints (GET /api/v0/models) and
// https://lmstudio.ai/docs/app/api/endpoints/openai (OpenAI-compatible /v1/chat/completions)
#[test]
fn replica_lmstudio_window_vision_and_reasoning_content() {
    let models = json!({"object": "list", "data": [
        {"id": "qwen2-vl-7b-instruct", "object": "model", "type": "vlm", "publisher": "mlx-community", "arch": "qwen2_vl", "compatibility_type": "mlx", "quantization": "4bit", "state": "loaded", "max_context_length": 32768},
        {"id": "meta-llama-3.1-8b-instruct", "object": "model", "type": "llm", "publisher": "lmstudio-community", "arch": "llama", "compatibility_type": "gguf", "quantization": "Q4_K_M", "state": "not-loaded", "max_context_length": 131072}]});
    let mut chunks = vec![chunk(json!({"role": "assistant", "reasoning_content": "The user wants the file."}), None)];
    chunks.extend(fragmented_call());
    chunks.push(json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [], "usage": {"prompt_tokens": 300, "completion_tokens": 20, "total_tokens": 320}}));
    let f = Replica::start("", vec![("GET", "/api/v0/models", json_ok(models))], vec![sse(&chunks)]);
    let caps = backend::detect_with(&Transport::OpenAi, &format!("{}/v1", f.url()), "qwen2-vl-7b-instruct", "", 3000);
    assert_eq!((caps.kind, caps.ctx, caps.vision, caps.cache), (K::LmStudio, Some(32768), Some(true), C::Automatic));
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (300, 0), "no reuse claimed: none reported");
}

// MLX mlx_lm.server: https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md (Python BaseHTTP server;
// usage in a last `chat.completion` object with stream_options.include_usage; tool calls arrive whole with index)
#[test]
fn replica_mlx_whole_tool_call_and_final_usage_object() {
    let models = json!({"object": "list", "data": [{"id": "mlx-community/Qwen3-4B-4bit", "object": "model", "created": 1700000000}]});
    let chunks = [
        json!({"id": "chatcmpl-u", "system_fingerprint": "x", "object": "chat.completion.chunk", "model": "m", "created": 1, "choices": [{"index": 0, "finish_reason": null,
            "delta": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "read", "arguments": "{\"path\": \"README.md\"}"}, "type": "function", "id": "u-1", "index": 0}]}}]}),
        json!({"id": "chatcmpl-u", "object": "chat.completion.chunk", "model": "m", "created": 1, "choices": [{"index": 0, "finish_reason": "tool_calls", "delta": {"role": "assistant", "content": ""}}]}),
        json!({"id": "chatcmpl-u", "system_fingerprint": "x", "object": "chat.completion", "model": "m", "created": 0, "choices": [],
            "usage": {"prompt_tokens": 500, "completion_tokens": 15, "total_tokens": 515, "prompt_tokens_details": {"cached_tokens": 450}}}),
    ];
    let f = Replica::start("BaseHTTP/0.6 Python/3.12.7", vec![("GET", "/v1/models", json_ok(models))], vec![sse(&chunks)]);
    let caps = detect(&f.url());
    assert_eq!(caps.kind, K::Mlx);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (50, 450));
    assert_eq!(f.reqs("/chat/completions")[0]["stream_options"], json!({"include_usage": true}), "mlx reads include_usage directly");
}

// vLLM: https://docs.vllm.ai/en/latest/serving/openai_compatible_server.html (/v1/models owned_by vllm with
// max_model_len, /version, usage chunk with stream_options, cached_tokens with --enable-prompt-tokens-details)
#[test]
fn replica_vllm_max_model_len_and_parallel_tool_calls() {
    let models = json!({"object": "list", "data": [{"id": "m", "object": "model", "created": 1700000000, "owned_by": "vllm", "root": "m", "parent": null, "max_model_len": 32768, "permission": []}]});
    let chunks = [
        chunk(json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "chatcmpl-tool-a", "type": "function", "function": {"name": "read", "arguments": ""}}]}), None),
        chunk(json!({"tool_calls": [{"index": 1, "id": "chatcmpl-tool-b", "type": "function", "function": {"name": "read", "arguments": ""}}]}), None),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\": \"README.md\"}"}}]}), None),
        chunk(json!({"tool_calls": [{"index": 1, "function": {"arguments": "{\"path\": \"a.txt\"}"}}]}), None),
        chunk(json!({}), Some("tool_calls")),
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [], "usage": {"prompt_tokens": 800, "total_tokens": 830, "completion_tokens": 30, "prompt_tokens_details": {"cached_tokens": 768}}}),
    ];
    let f = Replica::start("uvicorn", vec![("GET", "/v1/models", json_ok(models)), ("GET", "/version", json_ok(json!({"version": "0.11.0"})))], vec![sse(&chunks)]);
    let caps = detect(&f.url());
    assert_eq!((caps.kind, caps.ctx, caps.version.as_deref()), (K::Vllm, Some(32768), Some("0.11.0")));
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("read both")]).unwrap();
    let calls: Vec<Value> = r.content.iter().filter_map(|b| if let Block::ToolUse { input, .. } = b { Some(input["path"].clone()) } else { None }).collect();
    assert_eq!(calls, [json!("README.md"), json!("a.txt")]);
    assert_eq!(r.usage.cache_read_input_tokens, 768);
}

// LocalAI (GET /version), GPT4All (owned_by "humanity"), Jan (port 1337) and an unknown server:
// https://localai.io/basics/getting_started/ · https://docs.gpt4all.io/gpt4all_api_server/home.html · https://jan.ai/docs/api-server
#[test]
fn replica_generic_servers_are_told_apart_and_missing_usage_is_estimated() {
    let local_ai = Replica::start("", vec![("GET", "/version", json_ok(json!({"version": "v2.24.2"}))), ("GET", "/v1/models", json_ok(json!({"object": "list", "data": [{"id": "m", "object": "model"}]})))], vec![]);
    assert_eq!(detect(&local_ai.url()).kind, K::LocalAi);
    let gpt4all = Replica::start("", vec![("GET", "/v1/models", json_ok(json!({"object": "list", "data": [{"id": "m", "object": "model", "created": 0, "owned_by": "humanity", "root": "m", "parent": null}]})))], vec![]);
    assert_eq!(detect(&gpt4all.url()).kind, K::Gpt4All);
    // a server that sends no usage at all: the reply has none, and the context comes from Elisa's estimate
    let plain = Replica::start("", vec![("GET", "/v1/models", json_ok(json!({"object": "list", "data": [{"id": "m", "object": "model"}]})))],
        vec![sse(&[chunk(json!({"role": "assistant", "content": "Hi!"}), None), chunk(json!({}), Some("stop"))])]);
    let caps = detect(&plain.url());
    assert_eq!(caps.kind, K::OpenAiCompat);
    let c = rclient(&format!("{}/v1", plain.url()), Transport::OpenAi, caps, "");
    let r = rsend(&c, "sys", &[Message::user_text("hey")]).unwrap();
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (0, 0));
    assert_eq!(backend::ctx_from(r.usage.input_tokens, 1234), (1234, true));
}

// OpenAI: https://platform.openai.com/docs/api-reference/chat/streaming and
// https://platform.openai.com/docs/guides/prompt-caching (usage chunk with choices [], prompt_tokens_details.cached_tokens)
#[test]
fn replica_openai_usage_chunk_and_cached_tokens() {
    assert_eq!(backend::host_kind("https://api.openai.com/v1"), Some(K::OpenAi));
    let mut chunks: Vec<Value> = fragmented_call().into_iter().map(|mut c| { c["usage"] = Value::Null; c }).collect();
    chunks.push(json!({"id": "chatcmpl-123", "object": "chat.completion.chunk", "created": 1694268190, "model": "gpt-4o-mini", "choices": [],
        "usage": {"prompt_tokens": 2006, "completion_tokens": 300, "total_tokens": 2306, "prompt_tokens_details": {"cached_tokens": 1920, "audio_tokens": 0},
            "completion_tokens_details": {"reasoning_tokens": 0, "audio_tokens": 0, "accepted_prediction_tokens": 0, "rejected_prediction_tokens": 0}}}));
    let f = Replica::start("", vec![], vec![sse(&chunks), (401, "application/json", r#"{"error":{"message":"Incorrect API key provided: sk-...","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#.into())]);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, backend::defaults(K::OpenAi), "sk-x");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (86, 1920));
    assert!(rsend(&c, "sys", &[Message::user_text("x")]).unwrap_err().contains("Incorrect API key"));
}

// Anthropic: https://docs.claude.com/en/docs/build-with-claude/streaming and
// https://docs.claude.com/en/docs/build-with-claude/prompt-caching (cache_control, cache_creation/read_input_tokens)
#[test]
fn replica_anthropic_breakpoints_and_cache_usage() {
    let ev = |k: &str, d: Value| format!("event: {k}\ndata: {d}\n\n");
    let s = [
        ev("message_start", json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "claude", "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 48, "cache_read_input_tokens": 1800, "cache_creation_input_tokens": 248, "output_tokens": 1}}})),
        ev("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}})),
        ev("ping", json!({"type": "ping"})),
        ev("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"READ"}})),
        ev("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "ME.md\"}"}})),
        ev("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        ev("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null}, "usage": {"output_tokens": 15}})),
        ev("message_stop", json!({"type": "message_stop"})),
    ].concat();
    let overloaded = (529, "application/json", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_1"}"#.to_string());
    let f = Replica::start("", vec![], vec![(200, "text/event-stream", s), overloaded.clone(), overloaded]);
    let caps = backend::detect(&Transport::Anthropic, "https://api.anthropic.com", "m", "k");
    assert_eq!((caps.kind, caps.cache), (K::Anthropic, C::AnthropicBreakpoints));
    let c = rclient(&f.url(), Transport::Anthropic, caps, "sk-ant");
    let r = rsend(&c, "a long stable system prompt", &tool_history("toolu_0")).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens), (296, 1800), "a cache write is prefilled too");
    let b = &f.reqs("/v1/messages")[0];
    assert_eq!(b["system"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(b["messages"][2]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert!(b.to_string().matches("cache_control").count() <= 4);
    let e = rsend(&c, "s", &[Message::user_text("x")]).unwrap_err();
    assert!(e.contains("Overloaded") && e.contains("tried twice"), "529 is retried once: {e}");
    // another Anthropic-style server gets no breakpoints
    assert_eq!(backend::detect(&Transport::Anthropic, "http://127.0.0.1:9", "m", "").cache, C::None);
}

// Gemini: https://ai.google.dev/gemini-api/docs/openai (OpenAI endpoint, extra_body.google.cached_content) and
// https://ai.google.dev/api/caching (POST /v1beta/cachedContents)
#[test]
fn replica_gemini_explicit_cache_for_a_long_hot_prefix_and_unindexed_calls() {
    let whole = |id: &str, path: &str| chunk(json!({"role": "assistant", "tool_calls": [{"id": id, "type": "function", "function": {"name": "read", "arguments": format!("{{\"path\":\"{path}\"}}")}}]}), None);
    let reply = || sse(&[whole("", "README.md"), whole("", "a.txt"), chunk(json!({}), Some("tool_calls")),
        json!({"object": "chat.completion.chunk", "choices": [], "usage": {"prompt_tokens": 9000, "completion_tokens": 20, "total_tokens": 9020}})]);
    let cache = json_ok(json!({"name": "cachedContents/abc123", "model": "models/gemini-2.5-pro", "createTime": "2026-11-09T00:00:00Z", "expireTime": "2026-11-09T00:05:00Z", "usageMetadata": {"totalTokenCount": 8200}}));
    let f = Replica::start("", vec![("POST", "/v1beta/cachedContents", cache)], vec![reply(), reply(), reply(), reply()]);
    assert_eq!(backend::host_kind("https://generativelanguage.googleapis.com/v1beta/openai/"), Some(K::Gemini));
    let cfg = Config { base_url: format!("{}/v1beta/openai", f.url()), model: "gemini-2.5-pro".into(), api_key: "g-key".into(), transport: Transport::OpenAi, ..Config::default() };
    let mut c = Client::new(&cfg);
    c.set_caps(backend::defaults(K::Gemini), 1_000_000);
    let system = "Stable instructions. ".repeat(1200);
    for _ in 0..4 { let r = rsend(&c, &system, &[Message::user_text("read both")]).unwrap(); assert_eq!(r.content.len(), 2, "two unindexed calls stay two: {r:?}"); }
    let reqs = f.reqs("/chat/completions");
    assert!(reqs[0]["messages"][0]["role"] == "system" && reqs[1].get("extra_body").is_none(), "first calls: the prefix as usual");
    assert_eq!(reqs[2]["extra_body"], json!({"google": {"cached_content": "cachedContents/abc123"}}), "the third call in the window uses the cache");
    assert!(reqs[2].get("tools").is_none() && reqs[2]["messages"][0]["role"] == "user", "system and tools live in the cache");
    assert_eq!(reqs[3]["extra_body"]["google"]["cached_content"], "cachedContents/abc123", "reused, not created again");
    assert_eq!(f.reqs("/v1beta/cachedContents").len(), 1);
    let created = &f.reqs("/v1beta/cachedContents")[0];
    assert_eq!((created["model"].as_str(), created["ttl"].as_str()), (Some("models/gemini-2.5-pro"), Some("300s")));
    assert_eq!(f.log.lock().unwrap().iter().find(|r| r.path == "/v1beta/cachedContents").unwrap().header("x-goog-api-key"), Some("g-key"));
    // an error list body
    assert_eq!(elisa::api::err_message(r#"[{"error":{"code":400,"message":"API key not valid","status":"INVALID_ARGUMENT"}}]"#), "API key not valid");
}

// OpenRouter: https://openrouter.ai/docs/api/reference/streaming (": OPENROUTER PROCESSING" comments, usage in the last
// chunk with finish_reason, mid-stream error with finish_reason "error")
#[test]
fn replica_openrouter_comments_usage_and_midstream_error() {
    let (_, _, body) = sse(&[chunk(json!({"role": "assistant", "content": "Hel"}), None), chunk(json!({"content": "lo"}), None),
        json!({"id": "gen-1", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": "", "role": "assistant"}, "finish_reason": "stop", "native_finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 2, "total_tokens": 1002, "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 0}}})]);
    let ok = (200, "text/event-stream", format!(": OPENROUTER PROCESSING\n\n: OPENROUTER PROCESSING\n\n{body}"));
    let err = sse(&[chunk(json!({"content": "partial"}), None), json!({"id": "gen-2", "object": "chat.completion.chunk", "provider": "x",
        "error": {"code": "server_error", "message": "Provider disconnected unexpectedly"}, "choices": [{"index": 0, "delta": {"content": ""}, "finish_reason": "error"}]})]);
    let f = Replica::start("", vec![], vec![ok, err]);
    let c = rclient(&format!("{}/api/v1", f.url()), Transport::OpenAi, backend::defaults(K::OpenRouter), "or-key");
    let r = rsend(&c, "sys", &[Message::user_text("hi")]).unwrap();
    assert_eq!((r.content.clone(), r.usage.cache_read_input_tokens, r.usage.input_tokens), (vec![Block::Text { text: "Hello".into() }], 900, 100));
    assert!(rsend(&c, "sys", &[Message::user_text("hi")]).unwrap_err().contains("Provider disconnected"));
}

// Groq: https://console.groq.com/docs/api-reference (usage in x_groq.usage of the last chunk)
#[test]
fn replica_groq_usage_in_x_groq() {
    let mut chunks = fragmented_call();
    chunks.push(json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
        "x_groq": {"id": "req_1", "usage": {"queue_time": 0.03, "prompt_tokens": 418, "prompt_time": 0.0006, "completion_tokens": 20, "completion_time": 0.04, "total_tokens": 438, "total_time": 0.05}}}));
    let f = Replica::start("", vec![], vec![sse(&chunks)]);
    let c = rclient(&format!("{}/openai/v1", f.url()), Transport::OpenAi, backend::defaults(K::Groq), "gsk");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (418, 20));
}

// Together AI: https://docs.together.ai/docs/chat-overview (OpenAI-compatible; usage on the final chunk)
#[test]
fn replica_together_usage_on_the_finish_chunk() {
    let mut chunks = fragmented_call();
    chunks.pop();
    chunks.push(json!({"id": "x", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 700, "completion_tokens": 12, "total_tokens": 712}}));
    let f = Replica::start("", vec![], vec![sse(&chunks)]);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, backend::defaults(K::Together), "tg");
    let r = rsend(&c, "sys", &[Message::user_text("read README.md")]).unwrap();
    assert_read_call(&r);
    assert_eq!(r.usage.input_tokens, 700);
}

// DeepSeek: https://api-docs.deepseek.com/api/create-chat-completion (prompt_cache_hit_tokens / miss, reasoning_content)
#[test]
fn replica_deepseek_cache_hit_tokens_and_reasoning() {
    let chunks = [chunk(json!({"role": "assistant", "reasoning_content": "Think."}), None), chunk(json!({"content": "Done."}), None),
        json!({"id": "x", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": ""}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1200, "completion_tokens": 11, "total_tokens": 1211, "prompt_cache_hit_tokens": 1152, "prompt_cache_miss_tokens": 48, "completion_tokens_details": {"reasoning_tokens": 5}}})];
    let f = Replica::start("", vec![], vec![sse(&chunks)]);
    let c = rclient(&f.url(), Transport::OpenAi, backend::defaults(K::DeepSeek), "ds");
    let r = rsend(&c, "sys", &[Message::user_text("hi")]).unwrap();
    assert_eq!(r.content, vec![Block::Text { text: "Done.".into() }], "reasoning is not shown");
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens, r.usage.reasoning_tokens), (48, 1152, 5));
}

// Mistral: https://docs.mistral.ai/api/endpoint/chat and https://docs.mistral.ai/openapi.yaml (tool call ids
// ^[a-zA-Z0-9]{9}$, tool calls arrive whole)
#[test]
fn replica_mistral_tool_ids_are_nine_alphanumerics() {
    let chunks = [chunk(json!({"role": "assistant", "content": "", "tool_calls": [{"id": "D681PevKs", "function": {"name": "read", "arguments": "{\"path\": \"README.md\"}"}, "index": 0}]}), Some("tool_calls")),
        json!({"id": "x", "object": "chat.completion.chunk", "choices": [], "usage": {"prompt_tokens": 90, "completion_tokens": 10, "total_tokens": 100}})];
    let f = Replica::start("", vec![], vec![sse(&chunks)]);
    let c = rclient(&format!("{}/v1", f.url()), Transport::OpenAi, backend::defaults(K::Mistral), "ms");
    let r = rsend(&c, "sys", &tool_history("toolu_01XYZ")).unwrap();
    assert_read_call(&r);
    let b = &f.reqs("/chat/completions")[0];
    let id = b["messages"][2]["tool_calls"][0]["id"].as_str().unwrap().to_string();
    assert!(id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric()), "{id}");
    assert_eq!(b["messages"][3]["tool_call_id"], id.as_str(), "the result names the same id");
}

#[test]
fn replica_lines_for_model_command() {
    let mut caps = backend::defaults(K::LlamaCpp);
    (caps.ctx, caps.ctx_from, caps.slot_save, caps.slots, caps.detected_by) = (Some(16384), "n_ctx (/props)", true, 1, "GET /props".into());
    let l = backend::lines(&caps, 16384).join("\n");
    assert!(l.contains("llama.cpp") && l.contains("server window 16384") && l.contains("slot save/restore: on"), "{l}");
    let l = backend::lines(&backend::defaults(K::OpenAiCompat), 8192).join("\n");
    assert!(l.contains("reports no window") && l.contains("usage may be missing"), "{l}");
}

// ---------- a reply cut by the output limit is continued ----------

/// Plain answers can stop mid-sentence at the output limit (stop_reason max_tokens), leaving the user to type
/// "continue". A text reply cut by the limit (OpenAI `finish_reason: "length"`) is
/// continued: the same history plus the partial reply and "Continue exactly where you stopped." (that request only),
/// streamed into the same reply, up to 2 times; still cut: a quiet line to say "continue".
#[test]
fn reply_cut_at_output_limit_is_continued() {
    let usage = || json!({"prompt_tokens": 400, "completion_tokens": 6});
    let provider = |d: &Path, url: &str| {
        let home = d.join("_home/.config/elisa");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("providers.toml"), format!("active = \"l\"\n[providers.l]\ntype = \"openai\"\nbase_url = \"{url}/v1\"\nmodel = \"local\"\nmax_ctx = 16384\n")).unwrap();
    };
    let chat = |f: &Fake| -> Vec<Value> { f.log().iter().filter(|r| r.path == "/v1/chat/completions").map(|r| r.json()).collect() };

    // cut once, then finished: one reply, and the next turn's history holds it whole, without the note
    let d = tmpdir("cont-ok");
    let f = Fake::start(vec![
        oa_reply(&[json!({"content": "The paper has three uses: trading"})], "length", usage()),
        oa_reply(&[json!({"content": ", risk control and research."})], "stop", usage()),
        oa_reply(&[json!({"content": "You're welcome."})], "stop", usage()),
    ]);
    provider(&d, &f.url());
    let o = run_elisa(&["--no-checks"], &d, Some("what are the uses of the trading paper you know about?\nthanks\n/exit\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(f.left(), 0, "stderr: {}", o.stderr);
    assert!(o.stdout.contains("The paper has three uses: trading, risk control and research."), "stdout: {}", o.stdout);
    assert!(!o.stderr.contains("hit max_tokens") && !o.stderr.contains("reply was long"), "{}", o.stderr);
    let reqs = chat(&f);
    assert_eq!(reqs.len(), 3);
    let m1 = reqs[1]["messages"].as_array().unwrap();
    let n = m1.len();
    assert_eq!(m1[n - 2]["role"], "assistant");
    assert!(serde_json::to_string(&m1[n - 2]).unwrap().contains("The paper has three uses: trading"), "{:?}", m1[n - 2]);
    assert_eq!(m1[n - 1]["role"], "user");
    assert!(serde_json::to_string(&m1[n - 1]).unwrap().contains("Continue exactly where you stopped."), "{:?}", m1[n - 1]);
    // the continuation is the same request plus those two messages
    assert_eq!(&reqs[0]["messages"].as_array().unwrap()[..], &m1[..n - 2]);
    let next = serde_json::to_string(&reqs[2]["messages"]).unwrap();
    assert!(next.contains("The paper has three uses: trading, risk control and research.") && !next.contains("Continue exactly where you stopped."), "{next}");
    let text = std::fs::read_to_string(d.join(".elisa/metrics.jsonl")).unwrap();
    let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["outcome"], "done", "{first}");

    // still cut after 2 continuations: 3 requests, the reply kept, and the line to say "continue"
    let d = tmpdir("cont-cut");
    let f = Fake::start(vec![
        oa_reply(&[json!({"content": "Part one,"})], "length", usage()),
        oa_reply(&[json!({"content": " part two,"})], "length", usage()),
        oa_reply(&[json!({"content": " part three,"})], "length", usage()),
    ]);
    provider(&d, &f.url());
    let o = run_elisa(&["--no-checks"], &d, Some("write a long essay about the sea\n/exit\n"));
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!((chat(&f).len(), f.left()), (3, 0), "stderr: {}", o.stderr);
    assert!(o.stdout.contains("Part one, part two, part three,"), "stdout: {}", o.stdout);
    assert!(o.stderr.contains("[reply was long; say \"continue\" for more]"), "{}", o.stderr);
}

/// The picture flow through `--serve-jsonl`. Picture text a model copies into its reply is never shown;
/// on the turn after (about the picture), a picture answer that is a refusal is not carried (the read ask keeps its tools and runs), the
/// earlier turn's picture text has left the history, and the turn after that asks no picture request at all.
#[test]
fn picture_text_is_never_a_reply_and_only_its_own_turn_carries_it() {
    let dir = tmpdir("jsonl-picture");
    std::fs::write(dir.join("a.txt"), "the word is MANGO\n").unwrap();
    let f = Fake::start(vec![
        text_reply("A black cat with blue ears."),
        text_reply("[picture: A black cat with blue ears.] It's a black cat."),
        // the turn after: the picture request answers the read ask with a refusal, asked both ways
        text_reply("I'm sorry, but I can't assist with that."),
        text_reply("I'm sorry, but I can't assist with that."),
        Sse::new(800, 0).tool_input("t1", "read", json!({"path": "a.txt"})).end("tool_use", 9),
        text_reply("The word is MANGO."),
        text_reply("You're welcome!"),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    let shown = |evs: &[Value]| -> String { evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect::<String>() };
    s.cmd(json!({"type": "send", "text": "what is this?", "images": [{"media_type": "image/png", "data": "iVBORw0KGgo="}]}));
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "reply_retracted"), "{evs:?}");
    let after = evs.iter().rposition(|e| e["type"] == "reply_retracted").unwrap();
    assert_eq!(shown(&evs[after..]), "It's a black cat.");
    // (about the picture: "this cat", so the picture is looked at again)
    s.cmd(json!({"type": "send", "text": "read a.txt and tell me the word for this cat"}));
    let evs = s.until("turn_done");
    assert!(evs.iter().any(|e| e["type"] == "tool_start" && e["name"] == "read"), "{evs:?}");
    assert!(shown(&evs).ends_with("The word is MANGO."), "{}", shown(&evs));
    s.cmd(json!({"type": "send", "text": "thanks"}));
    s.until("turn_done");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 7, "three picture requests (the turn after asks twice), four turn requests");
    let has_image = |r: &Value| r["messages"].to_string().contains("\"image\"");
    assert!(has_image(&reqs[0]) && has_image(&reqs[2]) && has_image(&reqs[3]), "the picture turn and the one after look");
    assert!(!reqs[1..].iter().enumerate().any(|(i, r)| i != 1 && i != 2 && has_image(r)), "no picture in the turn requests");
    let body = |r: &Value| r["messages"].to_string();
    assert!(body(&reqs[1]).contains("[picture: A black cat with blue ears.]"), "the picture turn carries its text");
    for r in &reqs[4..] {
        assert!(!body(r).contains("[picture:"), "no picture text after its turn: {}", body(r));
        assert!(!body(r).contains("can't assist"), "the refusal is not carried");
        assert!(r.get("tools").is_some_and(|t| t.as_array().is_some_and(|a| !a.is_empty())), "tools offered");
    }
    assert_prefix_rule(&reqs[4..6]);
}

/// The turn after a picture looks at it again only when it is about it. "Tell me a joke." gets no picture
/// request and no picture text (the model answers the ask); "what color are its eyes?" after the next picture does.
#[test]
fn only_a_turn_about_the_picture_sends_it_again() {
    let dir = tmpdir("jsonl-picture-about");
    let f = Fake::start(vec![
        text_reply("A black cat with blue eyes."),
        text_reply("It's a black cat."),
        text_reply("Why did the chicken cross the road? To get to the other side."),
        text_reply("A black cat with blue eyes."),
        text_reply("A black cat."),
        text_reply("Blue."),
        text_reply("Its eyes are blue."),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    let pic = json!([{"media_type": "image/png", "data": "iVBORw0KGgo="}]);
    let shown = |evs: &[Value]| -> String { evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect::<String>() };
    s.cmd(json!({"type": "send", "text": "what is this?", "images": pic}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "Tell me a joke."}));
    let evs = s.until("turn_done");
    assert_eq!(shown(&evs), "Why did the chicken cross the road? To get to the other side.");
    s.cmd(json!({"type": "send", "text": "and now this one?", "images": pic}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "what color are its eyes?"}));
    let evs = s.until("turn_done");
    assert_eq!(shown(&evs), "Its eyes are blue.");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 7, "look, turn, joke turn, look, turn, look again, turn");
    let has_image = |r: &Value| r["messages"].to_string().contains("\"image\"");
    let body = |r: &Value| r["messages"].to_string();
    assert_eq!((0..7).filter(|&i| has_image(&reqs[i])).collect::<Vec<_>>(), vec![0, 3, 5]);
    assert!(!body(&reqs[2]).contains("[picture:"), "the joke turn has no picture text: {}", body(&reqs[2]));
    assert!(body(&reqs[6]).contains("[picture: Blue.]"), "the eyes turn carries its picture text");
}

/// A question about the picture after other talk still looks at the picture again (it names the picture or shares
/// what the picture turn learned), so it is answered from the picture; /reset forgets the picture.
#[test]
fn a_later_question_about_the_picture_looks_at_it_again() {
    let dir = tmpdir("jsonl-picture-later");
    let f = Fake::start(vec![
        text_reply("A black cat with light blue ears and a small yellow star."),
        text_reply("It's a black cat with light blue ears."),
        text_reply("Why did the chicken cross the road? To get to the other side."),
        text_reply("The star is yellow."),
        text_reply("The star in the picture is yellow."),
        text_reply("There is no picture in our chat now."),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    let pic = json!([{"media_type": "image/png", "data": "iVBORw0KGgo="}]);
    let shown = |evs: &[Value]| -> String { evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect::<String>() };
    s.cmd(json!({"type": "send", "text": "what is this?", "images": pic}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "Tell me a joke."}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "what colour is the star next to the cat?"}));
    let evs = s.until("turn_done");
    assert_eq!(shown(&evs), "The star in the picture is yellow.");
    s.cmd(json!({"type": "send", "text": "/reset"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "what colour is the star next to the cat?"}));
    s.until("turn_done");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 6, "look, turn, joke turn, look again, turn, turn after /reset");
    let has_image = |r: &Value| r["messages"].to_string().contains("\"image\"");
    let body = |r: &Value| r["messages"].to_string();
    assert_eq!((0..6).filter(|&i| has_image(&reqs[i])).collect::<Vec<_>>(), vec![0, 3]);
    assert!(body(&reqs[3]).contains("what colour is the star next to the cat?"), "the later question is asked of the picture");
    assert!(body(&reqs[4]).contains("[picture: The star is yellow.]"), "its turn carries the picture's answer");
    assert!(!body(&reqs[5]).contains("[picture:"), "nothing of the picture after /reset");
}

/// Two pictures in one message: each is looked at alone and described (a small vision model may read only one
/// picture of a request), and the turn answers from both answers, numbered; its bare count stands.
#[test]
fn several_pictures_are_looked_at_one_by_one() {
    let dir = tmpdir("jsonl-two-pictures");
    let f = Fake::start(vec![
        text_reply("A black cat with blue ears."),
        text_reply("A black cat looking up."),
        text_reply("2"),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    let pics = json!([{"media_type": "image/png", "data": "iVBORw0KGgo="}, {"media_type": "image/png", "data": "R0lGODlh"}]);
    s.cmd(json!({"type": "send", "text": "How many images are in this message? Answer with just the number.", "images": pics}));
    let evs = s.until("turn_done");
    let shown: String = evs.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(shown, "2", "{evs:?}");
    assert!(!evs.iter().any(|e| e["type"] == "reply_retracted"), "{evs:?}");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3, "a look per picture, then the turn");
    let body = |r: &Value| r["messages"].to_string();
    for (r, data) in [(&reqs[0], "iVBORw0KGgo="), (&reqs[1], "R0lGODlh")] {
        assert_eq!(body(r).matches("\"image\"").count(), 1, "one picture per look: {}", body(r));
        assert!(body(r).contains(data) && body(r).contains("Describe this picture."), "{}", body(r));
    }
    assert!(body(&reqs[2]).contains("[picture: 2 pictures, each looked at on its own:\\n1. A black cat with blue ears.\\n2. A black cat looking up.]"), "{}", body(&reqs[2]));
}

/// A reply that copies an earlier reply to another ask, or has no words, is asked once more with the user's
/// words alone (no history), and that answer is shown and kept.
#[test]
fn a_copied_or_wordless_reply_is_asked_again_alone() {
    let dir = tmpdir("jsonl-copied-reply");
    let long = "I hope this email finds you well. I wanted to share some interesting information about lifestyles.";
    let f = Fake::start(vec![
        text_reply(long),
        text_reply(&format!("{long} More of the same.")),
        text_reply("Why did the tomato turn red? It saw the salad dressing!"),
        text_reply("[0.0, 0.17, 0.99, 0.29]"),
        text_reply("Hello there!"),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    let shown = |evs: &[Value]| -> String {
        let at = evs.iter().rposition(|e| e["type"] == "reply_retracted").map_or(0, |i| i + 1);
        evs[at..].iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect::<String>()
    };
    s.cmd(json!({"type": "send", "text": "write me something"}));
    s.until("turn_done");
    s.cmd(json!({"type": "send", "text": "Tell me a joke."}));
    let evs = s.until("turn_done");
    assert_eq!(shown(&evs), "Why did the tomato turn red? It saw the salad dressing!");
    s.cmd(json!({"type": "send", "text": "hello"}));
    let evs = s.until("turn_done");
    assert_eq!(shown(&evs), "Hello there!");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 5);
    for i in [2, 4] {
        let m = reqs[i]["messages"].as_array().unwrap();
        assert_eq!(m.len(), 1, "asked alone: {}", reqs[i]["messages"]);
    }
    assert!(reqs[2]["messages"].to_string().contains("Tell me a joke."));
    // the history keeps the answer given alone, not the copy
    let later = reqs[3]["messages"].to_string();
    assert!(later.contains("salad dressing") && !later.contains("More of the same"), "{later}");
}

/// A reply not in the message's language is asked once more with the language note; still in another language,
/// the message is asked alone with the note, and that answer is shown and kept.
#[test]
fn a_reply_still_in_another_language_is_asked_alone_with_the_note() {
    let dir = tmpdir("jsonl-lang-alone");
    let f = Fake::start(vec![
        text_reply("Hi! Yes, I'm doing well, thanks!"),
        text_reply("Hi! Yes, I'm doing well, thank you!"),
        text_reply("Chào bạn! Mình khỏe, cảm ơn bạn!"),
    ]);
    let mut s = Serve::start_full(&f.url(), &dir, "memory = false\nmax_ctx = 8192\n", &["--type", "anthropic"], &[]);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "Xin chào, bạn khỏe không?"}));
    let evs = s.until("turn_done");
    let at = evs.iter().rposition(|e| e["type"] == "reply_retracted").map_or(0, |i| i + 1);
    let shown: String = evs[at..].iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(shown, "Chào bạn! Mình khỏe, cảm ơn bạn!", "{evs:?}");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3);
    let note = elisa::reply_check::lang_note("Xin chào, bạn khỏe không?").to_string();
    assert!(reqs[1]["messages"].to_string().contains(&note), "the second try has the note");
    let alone = reqs[2]["messages"].as_array().unwrap();
    assert_eq!(alone.len(), 1, "asked alone: {alone:?}");
    assert!(alone[0].to_string().contains("Xin chào") && alone[0].to_string().contains(&note));
}

/// A reply that names a saved file on a turn with no write at all is corrected: asked once more in words
/// with no tools, and a reply that still claims it gets the plain line. A plain reply is left alone.
#[test]
fn a_saved_claim_with_no_write_is_corrected() {
    let dir = tmpdir("jsonl-noclaim");
    let f = Fake::start(vec![
        text_reply("Okay, the plan is saved to `marketing-plan.md`."),
        text_reply("Done, I've saved it to marketing-plan.md."),
        text_reply("Keep your menu updated and post daily."),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    s.cmd(json!({"type": "send", "text": "save that plan to marketing-plan.md"}));
    let evs = s.until("turn_done");
    let after = evs.iter().rposition(|e| e["type"] == "reply_retracted").expect("retracted");
    let text: String = evs[after..].iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(text, "I have not saved it: no file was written.");
    s.cmd(json!({"type": "send", "text": "one more tip?"}));
    let evs = s.until("turn_done");
    assert!(!evs.iter().any(|e| e["type"] == "reply_retracted"), "a plain reply stays: {evs:?}");
    s.quit();
    let reqs = f.messages();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[1].get("tools").is_none_or(|t| t.as_array().is_some_and(|a| a.is_empty())), "the retry has no tools");
    assert!(reqs[1]["messages"].to_string().contains("nothing was saved in this message (no write was made)"), "{}", reqs[1]["messages"]);
    assert!(!dir.join("marketing-plan.md").exists());
}

/// A later turn's reply that recalls a save Elisa's tools really made this session ("I also saved the poem
/// to poem.md") is no false claim: it stays, with no retry and no "nothing was saved" note.
#[test]
fn a_recalled_save_from_an_earlier_turn_stands() {
    let dir = tmpdir("jsonl-recall-save");
    let f = Fake::start(vec![
        Sse::new(10, 0).tool_input("w1", "write", json!({"path": "poem.md", "content": "rain on the lake\n"})).end("tool_use", 5),
        text_reply("The poem is saved to poem.md."),
        text_reply("You're Linh. I also saved the poem to `poem.md` for you."),
        text_reply("I saved the poem to the file."),
        text_reply("Okay, the plan is saved to `plan.md`."),
        text_reply("Still saved to plan.md."),
    ]);
    let mut s = Serve::start(&f.url(), &dir);
    s.until("ready");
    for (msg, retracted) in [("write a poem to poem.md", false), ("what do you remember about me?", false),
        ("and the poem?", false), ("and the plan?", true)] {
        s.cmd(json!({"type": "send", "text": msg}));
        if msg.starts_with("write") { s.until("approve"); s.cmd(json!({"type": "approve", "answer": "yes"})); }
        let evs = s.until("turn_done");
        assert_eq!(evs.iter().any(|e| e["type"] == "reply_retracted"), retracted, "{msg}: {evs:?}");
    }
    s.quit();
    assert!(dir.join("poem.md").exists());
    let reqs = f.messages();
    assert_eq!(reqs.len(), 6, "a retry only for the file never written");
    assert!(!reqs[..5].iter().any(|r| r["messages"].to_string().contains("nothing was saved")));
    assert!(reqs[5]["messages"].to_string().contains("nothing was saved"));
}
