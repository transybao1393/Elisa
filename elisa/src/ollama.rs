//! Ollama's native `POST {root}/api/chat`, used instead of its OpenAI-style endpoint when an `openai` connection turns
//! out to be Ollama (`provider::ollama_caps`). The OpenAI endpoint cannot set the context size: every request there
//! reloads the model at Ollama's default (4096 on many installs), so a connection's `max_ctx` above that silently cut
//! replies off. Here every request carries `num_ctx` = the session's window. The model's reasoning (`thinking`) is read
//! too: it counts from the first token for the timings, though only the answer is shown.
use crate::api::{publish_thinking, publish, publish_call, Phase, PhaseGuard, Request};
use crate::types::{Block, Message, Reply, Role, Usage};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

fn messages(system: &str, msgs: &[Message]) -> Vec<Value> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if !system.is_empty() { out.push(json!({"role": "system", "content": system})); }
    // a tool result names its tool (Ollama matches results by name, not id)
    let mut names: HashMap<&str, &str> = HashMap::new();
    for m in msgs {
        let text = m.content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
        match m.role {
            Role::Assistant => {
                let calls: Vec<Value> = m.content.iter().filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => { names.insert(id, name); Some(json!({"function": {"name": name, "arguments": input}})) }
                    _ => None,
                }).collect();
                let mut a = json!({"role": "assistant", "content": text});
                if !calls.is_empty() { a["tool_calls"] = Value::Array(calls); }
                out.push(a);
            }
            Role::User => {
                for b in &m.content {
                    if let Block::ToolResult { tool_use_id, content, is_error } = b {
                        let c = if *is_error { format!("[error] {content}") } else { content.clone() };
                        let mut r = json!({"role": "tool", "content": c});
                        if let Some(n) = names.get(tool_use_id.as_str()) { r["tool_name"] = json!(n); }
                        out.push(r);
                    }
                }
                let images: Vec<&str> = m.content.iter().filter_map(|b| if let Block::Image { source } = b { Some(source.data.as_str()) } else { None }).collect();
                if !text.is_empty() || !images.is_empty() {
                    let mut u = json!({"role": "user", "content": text});
                    if !images.is_empty() { u["images"] = json!(images); }
                    out.push(u);
                }
            }
        }
    }
    out
}

/// `thinks`: the model reasons (Ollama capability "thinking"). Its reasoning comes out of `num_predict` too, so a
/// reply budget sized for the answer alone was used up before the answer started: such a model gets `REASONING_ROOM`
/// tokens on top of `max_tokens`, and `think` = the session's setting. Not the window's end (-2): a small model's
/// reasoning can run on in circles, and then the whole window went by (185 s on qwen3-vl:2b) before the stop.
/// (qwen3-vl reasons about as long with `think: false` as with true: the flag is a hint, not a switch.)
/// a reasoning model's room for its reasoning, on top of the answer's `max_tokens` (it measured 300–2,500 tokens on
/// most questions, up to 5,700 on a picture)
pub const REASONING_ROOM: usize = 3072;

pub fn body(model: &str, req: &Request, num_ctx: usize, thinks: Option<bool>) -> Value {
    let mut b = Map::new();
    b.insert("model".into(), json!(model));
    b.insert("messages".into(), Value::Array(messages(req.system, req.messages)));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req.tools.iter().map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}})).collect();
        b.insert("tools".into(), Value::Array(tools));
    }
    if let Some(t) = thinks { b.insert("think".into(), json!(t)); }
    let predict = req.max_tokens + if thinks.is_some() { REASONING_ROOM } else { 0 };
    b.insert("options".into(), json!({"num_ctx": num_ctx, "num_predict": predict}));
    b.insert("stream".into(), json!(true));
    Value::Object(b)
}

/// The state of one streamed reply.
#[derive(Default)]
pub struct Stream {
    text: String,
    /// reasoning chunks seen: Ollama streams about one token per line, so this is the reasoning's token count
    thinking: usize,
    calls: Vec<(String, Value)>,
    done_reason: String,
    usage: Usage,
    /// seconds Ollama spent generating (its `eval_duration`): the true decode time, reasoning included
    pub decode_s: Option<f64>,
    /// a first token (reasoning or answer) came: the seconds to it are the reply's ttft
    pub started: bool,
}

impl Stream {
    /// One NDJSON line. Ok(true) = the stream is over.
    pub fn feed(&mut self, line: &str, on_text: &mut dyn FnMut(&str)) -> Result<bool, String> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return Ok(false) };
        if let Some(e) = v["error"].as_str() { return Err(e.to_string()); }
        let m = &v["message"];
        if let Some(t) = m["thinking"].as_str().filter(|t| !t.is_empty()) {
            if !self.started { self.started = true; publish(Phase::Decode); }
            self.thinking += 1;
            publish_thinking(t);
        }
        if let Some(t) = m["content"].as_str().filter(|t| !t.is_empty()) {
            if !self.started { self.started = true; publish(Phase::Decode); }
            self.text.push_str(t);
            on_text(t);
        }
        for tc in m["tool_calls"].as_array().map_or(&[][..], Vec::as_slice) {
            self.started = true;
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            publish_call(&name, tc["function"]["arguments"]["path"].as_str().unwrap_or(""));
            self.calls.push((name, tc["function"]["arguments"].clone()));
        }
        if v["done"].as_bool() == Some(true) {
            let n = |k: &str| v[k].as_u64().unwrap_or(0) as usize;
            let cached = n("prompt_eval_cached_count");
            let out = n("eval_count");
            self.usage = Usage { input_tokens: n("prompt_eval_count").saturating_sub(cached), output_tokens: out, cache_read_input_tokens: cached, reasoning_tokens: self.thinking.min(out) };
            self.decode_s = v["eval_duration"].as_u64().filter(|&d| d > 0).map(|d| d as f64 / 1e9);
            self.done_reason = v["done_reason"].as_str().unwrap_or("").to_string();
            return Ok(true);
        }
        Ok(false)
    }

    /// (content, stop_reason, usage)
    pub fn finish(self, cancelled: bool) -> (Vec<Block>, String, Usage) {
        let mut content = Vec::new();
        if !self.text.is_empty() { content.push(Block::Text { text: self.text }); }
        if cancelled { return (content, "cancelled".into(), self.usage); }
        let any = !self.calls.is_empty();
        for (i, (name, args)) in self.calls.into_iter().enumerate() {
            // arguments come as an object; a model that wrote a string gets it parsed, else an empty input
            let input = match args { Value::Object(_) => args, Value::String(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})), _ => json!({}) };
            content.push(Block::ToolUse { id: format!("call_{i}"), name, input });
        }
        let stop = match self.done_reason.as_str() {
            "length" => "max_tokens",
            _ if any => "tool_use",
            _ => "end_turn",
        };
        (content, stop.into(), self.usage)
    }
}

/// A server-side tool-call parse failure (Ollama: "qwen tool call parsing failed ... unexpected end of JSON input"):
/// the model wrote a broken call, which a second sample usually does not
fn broken_call(e: &str) -> bool { e.contains("JSON input") || e.contains("tool call") }

/// One streamed call to `root` (the server without `/v1`), with the window `num_ctx`.
#[allow(clippy::too_many_arguments)]
pub fn send(agent: &ureq::Agent, root: &str, key: &str, model: &str, num_ctx: usize, thinks: Option<bool>, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
    let mut wrote = false;
    let first = send_once(agent, root, key, model, num_ctx, thinks, req, cancel, &mut |t: &str| { wrote = true; on_text(t) });
    match first {
        // once, and only when nothing was shown yet (a retry after text would show it twice)
        Err(e) if broken_call(&e) && !wrote && !cancel.load(Ordering::Relaxed) => send_once(agent, root, key, model, num_ctx, thinks, req, cancel, on_text),
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
fn send_once(agent: &ureq::Agent, root: &str, key: &str, model: &str, num_ctx: usize, thinks: Option<bool>, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
    let t0 = Instant::now();
    publish(Phase::Sent);
    let _idle = PhaseGuard;
    let mut rq = agent.post(format!("{}/api/chat", root.trim_end_matches('/'))).header("content-type", "application/json");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let r = rq.send(body(model, req, num_ctx, thinks).to_string()).map_err(|e| format!("request failed: {e}"))?;
    let status = r.status().as_u16();
    if !(200..300).contains(&status) {
        let text = r.into_body().read_to_string().unwrap_or_default();
        return Err(format!("HTTP {status}: {}", crate::api::err_message(&text)));
    }
    publish(Phase::Started);
    let mut s = Stream::default();
    let mut ttft = None;
    let mut cancelled = false;
    for line in BufReader::new(r.into_body().into_reader()).lines() {
        if cancel.load(Ordering::Relaxed) { cancelled = true; break; }
        let line = line.map_err(|e| format!("stream read: {e}"))?;
        let done = s.feed(line.trim(), on_text)?;
        if s.started { ttft.get_or_insert_with(|| t0.elapsed().as_secs_f64()); }
        if done { break; }
    }
    let total = t0.elapsed().as_secs_f64();
    // Ollama's own decode time, when it gave one, makes tok/s exact: total - decode is the wait before the first token
    let ttft = match (s.decode_s, ttft) {
        (Some(d), _) if d < total => total - d,
        (_, Some(t)) => t,
        _ => total,
    };
    let (content, stop_reason, usage) = s.finish(cancelled);
    Ok(Reply { id: String::new(), content, stop_reason, usage, ttft_s: ttft, total_s: total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDef;

    #[test]
    fn the_body_carries_the_window_images_and_tool_names() {
        let msgs = vec![
            Message::new(Role::User, vec![Block::Text { text: "look".into() }, Block::image("image/png", "AAAA")]),
            Message::new(Role::Assistant, vec![Block::ToolUse { id: "t1".into(), name: "read".into(), input: json!({"path": "a"}) }]),
            Message::new(Role::User, vec![Block::ToolResult { tool_use_id: "t1".into(), content: "x".into(), is_error: false }]),
        ];
        let tools = [ToolDef { name: "read".into(), description: "d".into(), input_schema: json!({"type": "object"}) }];
        let req = Request { system: "sys", messages: &msgs, tools: &tools, max_tokens: 512 };
        let b = body("m", &req, 8192, None);
        assert_eq!(b["options"], json!({"num_ctx": 8192, "num_predict": 512}));
        assert_eq!(b["messages"][1], json!({"role": "user", "content": "look", "images": ["AAAA"]}));
        assert_eq!(b["messages"][2]["tool_calls"][0]["function"], json!({"name": "read", "arguments": {"path": "a"}}));
        assert_eq!(b["messages"][3], json!({"role": "tool", "content": "x", "tool_name": "read"}));
        assert_eq!(b["tools"][0]["function"]["name"], "read");
    }

    #[test]
    fn reasoning_counts_for_timing_but_only_the_answer_is_shown() {
        let mut s = Stream::default();
        let mut shown = String::new();
        let mut on = |t: &str| shown.push_str(t);
        for l in [
            r#"{"message":{"role":"assistant","content":"","thinking":"hmm"},"done":false}"#,
            r#"{"message":{"role":"assistant","content":"Hi"},"done":false}"#,
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":100,"prompt_eval_cached_count":60,"eval_count":20,"eval_duration":500000000}"#,
        ] {
            let done = s.feed(l, &mut on).unwrap();
            assert!(s.started);
            if done { break; }
        }
        assert_eq!(s.decode_s, Some(0.5));
        let (content, stop, usage) = s.finish(false);
        assert_eq!(shown, "Hi");
        assert_eq!(content, vec![Block::Text { text: "Hi".into() }]);
        assert_eq!(stop, "end_turn");
        assert_eq!((usage.input_tokens, usage.cache_read_input_tokens, usage.output_tokens), (40, 60, 20));
    }

    #[test]
    fn a_thinking_model_gets_room_to_reason_and_its_reasoning_is_counted() {
        let msgs = vec![Message::user_text("hi")];
        let req = Request { system: "", messages: &msgs, tools: &[], max_tokens: 512 };
        let b = body("m", &req, 8192, Some(false));
        assert_eq!((b["think"].clone(), b["options"]["num_predict"].clone()), (json!(false), json!(512 + REASONING_ROOM)));
        let mut s = Stream::default();
        let mut on = |_: &str| {};
        for _ in 0..30 { s.feed(r#"{"message":{"content":"","thinking":"x"},"done":false}"#, &mut on).unwrap(); }
        s.feed(r#"{"message":{"content":"ok"},"done":false}"#, &mut on).unwrap();
        s.feed(r#"{"message":{"content":""},"done":true,"done_reason":"stop","eval_count":31}"#, &mut on).unwrap();
        assert_eq!(s.finish(false).2.reasoning_tokens, 30);
    }

    #[test]
    fn a_tool_call_and_an_error_line() {
        let mut s = Stream::default();
        let mut on = |_: &str| {};
        s.feed(r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read","arguments":{"path":"a.rs"}}}]},"done":false}"#, &mut on).unwrap();
        s.feed(r#"{"message":{"content":""},"done":true,"done_reason":"stop"}"#, &mut on).unwrap();
        let (content, stop, _) = s.finish(false);
        assert_eq!(stop, "tool_use");
        assert_eq!(content, vec![Block::ToolUse { id: "call_0".into(), name: "read".into(), input: json!({"path": "a.rs"}) }]);
        assert_eq!(Stream::default().feed(r#"{"error":"model not found"}"#, &mut on).unwrap_err(), "model not found");
    }
}
