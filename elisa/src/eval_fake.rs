//! The scripted model behind `elisa eval`'s deterministic cases: a tiny Anthropic Messages server on 127.0.0.1 that
//! answers each `POST /v1/messages` with the next scripted reply (streamed as SSE, like a gateway) and records every
//! request body, so a case can check what Elisa sent (the tools offered, the history) without any real model.
//! Same shape as the test helper in `tests/fake_server.rs`.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One scripted model reply.
#[derive(Clone, Debug, Default)]
pub struct Scripted {
    pub text: Option<String>,
    /// (name, input)
    pub tools: Vec<(String, Value)>,
    /// stop_reason; default `tool_use` when there are tool calls, else `end_turn`
    pub stop: Option<String>,
    pub out_tokens: usize,
    pub input_tokens: usize,
    /// stream the text word by word with a pause (an interrupt test needs time to press Esc)
    pub slow: bool,
}

/// The text the server answers with once the script is used up (the case then usually fails on its own checks).
pub const EXHAUSTED: &str = "[eval fake model: script exhausted]";

pub struct FakeModel {
    port: u16,
    script: Arc<Mutex<VecDeque<Scripted>>>,
    log: Arc<Mutex<Vec<Value>>>,
    exhausted: Arc<Mutex<usize>>,
    stop: Arc<AtomicBool>,
}

impl FakeModel {
    /// Starts the server; `max_ctx` is what its /health reports (Elisa sizes its window from it).
    pub fn start(script: Vec<Scripted>, max_ctx: usize) -> std::io::Result<FakeModel> {
        let l = TcpListener::bind("127.0.0.1:0")?;
        let port = l.local_addr()?.port();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let log = Arc::new(Mutex::new(Vec::new()));
        let exhausted = Arc::new(Mutex::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, g, x, st) = (script.clone(), log.clone(), exhausted.clone(), stop.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                if st.load(Ordering::SeqCst) { return; }
                let Ok(c) = c else { continue };
                let (s, g, x) = (s.clone(), g.clone(), x.clone());
                std::thread::spawn(move || { let _ = handle(c, &s, &g, &x, max_ctx); });
            }
        });
        Ok(FakeModel { port, script, log, exhausted, stop })
    }
    pub fn url(&self) -> String { format!("http://127.0.0.1:{}", self.port) }
    /// the bodies of the model requests so far (warm-ups and token counts left out), oldest first
    pub fn requests(&self) -> Vec<Value> { self.log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
    /// scripted replies not used
    pub fn left(&self) -> usize { self.script.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len() }
    /// requests answered after the script ran out
    pub fn exhausted(&self) -> usize { *self.exhausted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
}

impl Drop for FakeModel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop so its thread ends
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn handle(mut c: TcpStream, script: &Mutex<VecDeque<Scripted>>, log: &Mutex<Vec<Value>>, exhausted: &Mutex<usize>, max_ctx: usize) -> std::io::Result<()> {
    c.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut r = BufReader::new(c.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    let mut it = first.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let (mut len, mut warm) = (0usize, false);
    loop {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 { break; }
        let l = l.trim_end();
        if l.is_empty() { break; }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0); }
            if k.trim().eq_ignore_ascii_case("x-streamdense-warm") { warm = true; }
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let json_reply = |c: &mut TcpStream, code: u16, b: &str| -> std::io::Result<()> {
        write!(c, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len())?;
        c.flush()
    };
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => json_reply(&mut c, 200, &json!({"status": "ok", "info": {"max_ctx": max_ctx}}).to_string()),
        ("POST", "/v1/messages/count_tokens") => {
            let n = body.len() / 4;
            json_reply(&mut c, 200, &json!({"input_tokens": n}).to_string())
        }
        ("POST", "/v1/messages") if warm => json_reply(&mut c, 200, r#"{"type":"message","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":0}}"#),
        ("POST", "/v1/messages") => {
            let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(req);
            let next = script.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front();
            let s = next.unwrap_or_else(|| {
                *exhausted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
                Scripted { text: Some(EXHAUSTED.into()), ..Default::default() }
            });
            let (events, slow) = (sse(&s), s.slow);
            let total: usize = events.iter().map(String::len).sum();
            write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {total}\r\nconnection: close\r\n\r\n")?;
            c.flush()?;
            for e in events {
                std::thread::sleep(Duration::from_millis(if slow { 6 } else { 2 }));
                c.write_all(e.as_bytes())?;
                c.flush()?;
            }
            Ok(())
        }
        _ => json_reply(&mut c, 404, r#"{"type":"error","error":{"type":"not_found_error","message":"no route"}}"#),
    }
}

fn ev(kind: &str, data: Value) -> String { format!("event: {kind}\ndata: {data}\n\n") }

/// pieces of `s` cut at char boundaries: words when slow, else about 3 pieces (a delta split mid-word is normal)
fn pieces(s: &str, slow: bool) -> Vec<String> {
    if slow { return s.split_inclusive(' ').map(String::from).collect(); }
    let cut: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    if cut.len() < 3 { return vec![s.to_string()]; }
    let (a, b) = (cut[cut.len() / 3], cut[2 * cut.len() / 3]);
    vec![s[..a].to_string(), s[a..b].to_string(), s[b..].to_string()]
}

/// the SSE events of one scripted reply
pub fn sse(s: &Scripted) -> Vec<String> {
    let input = if s.input_tokens == 0 { 200 } else { s.input_tokens };
    let mut out = vec![
        ev("message_start", json!({"type": "message_start", "message": {"id": "msg_eval", "type": "message", "role": "assistant", "content": [],
            "model": "eval-fake", "stop_reason": null, "usage": {"input_tokens": input, "output_tokens": 1, "cache_read_input_tokens": 0}}})),
        ev("ping", json!({"type": "ping"})),
    ];
    let mut idx = 0;
    if let Some(t) = s.text.as_deref().filter(|t| !t.is_empty()) {
        out.push(ev("content_block_start", json!({"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}})));
        for p in pieces(t, s.slow) {
            out.push(ev("content_block_delta", json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": p}})));
        }
        out.push(ev("content_block_stop", json!({"type": "content_block_stop", "index": idx})));
        idx += 1;
    }
    for (i, (name, input)) in s.tools.iter().enumerate() {
        out.push(ev("content_block_start", json!({"type": "content_block_start", "index": idx,
            "content_block": {"type": "tool_use", "id": format!("toolu_eval_{i}"), "name": name, "input": {}}})));
        for p in pieces(&input.to_string(), false) {
            out.push(ev("content_block_delta", json!({"type": "content_block_delta", "index": idx, "delta": {"type": "input_json_delta", "partial_json": p}})));
        }
        out.push(ev("content_block_stop", json!({"type": "content_block_stop", "index": idx})));
        idx += 1;
    }
    let stop = s.stop.clone().unwrap_or_else(|| if s.tools.is_empty() { "end_turn".into() } else { "tool_use".into() });
    let outn = if s.out_tokens == 0 { 5 + s.text.as_deref().map_or(0, |t| t.len() / 4) } else { s.out_tokens };
    out.push(ev("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": outn}})));
    out.push(ev("message_stop", json!({"type": "message_stop"})));
    out
}
