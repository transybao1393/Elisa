//! Ollama's native `POST {root}/api/chat`, used instead of its OpenAI-style endpoint when an `openai` connection turns
//! out to be Ollama (`provider::ollama_caps`). The OpenAI endpoint cannot set the context size: every request there
//! reloads the model at Ollama's default (4096 on many installs), so a connection's `max_ctx` above that silently cut
//! replies off. Here every request carries `num_ctx` = the session's window. The model's reasoning (`thinking`) is read
//! too: it counts from the first token for the timings, though only the answer is shown.
use crate::api::{Next, publish_thinking, publish, publish_call, Phase, PhaseGuard, Request};
use crate::types::{Block, Message, Reply, Role, Usage};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
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

/// A simple turn's prefill (`no_think`): a closed, short reasoning block the model continues after. qwen3-vl's
/// thinking variant ignores `think: false` (it reasons at length even for a bare "hi", and with
/// Elisa's prompt can reason for minutes and still answer nothing). With this prefill it answers in
/// well under a second and still calls a tool when the request needs one. The wording was picked from several tried on
/// Elisa's real request: without the hint about a plain first sentence a small model often answered with only the
/// state card. "my reply is a plain, natural sentence or two" made a request to write
/// a plan or a story get a one-line promise instead of the content, and the user's
/// facts came back as the model's own. (A version "speaking to them as you" made
/// replies open with "You're the one who…" and repeat across turns.)
pub const NO_THINK_PREFILL: &str = "<think>\nNo long reasoning needed. My reply is plain, natural text for the user: short for chat (a greeting or thanks: one friendly line); if they asked me to write something, the full text now. If it truly needs a tool, I call it instead.\n</think>\n\n";

/// Does `model` take the `<think>…</think>` prefill (the Qwen3 family: its chat format closes reasoning that way)?
pub fn takes_prefill(model: &str) -> bool { let m = model.to_lowercase(); m.contains("qwen3") || m.contains("qwq") }

/// How a reasoning model is asked not to reason on a simple turn (`Client::set_simple`): `think: false`, and for the
/// Qwen3 family the `NO_THINK_PREFILL` (then no reasoning room either: the reasoning is already written).
pub fn no_think(model: &str, thinks: Option<bool>, simple: bool) -> (Option<bool>, Option<&'static str>) {
    match (thinks, simple) {
        (Some(_), true) => (Some(false), takes_prefill(model).then_some(NO_THINK_PREFILL)),
        (t, _) => (t, None),
    }
}

pub fn body(model: &str, req: &Request, num_ctx: usize, thinks: Option<bool>, prefill: Option<&str>) -> Value {
    let mut b = Map::new();
    b.insert("model".into(), json!(model));
    let mut msgs = messages(req.system, req.messages);
    // a trailing assistant message is continued (not part of the history: the prefix before it stays the same)
    if let Some(p) = prefill { msgs.push(json!({"role": "assistant", "content": p})); }
    b.insert("messages".into(), Value::Array(msgs));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req.tools.iter().map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}})).collect();
        b.insert("tools".into(), Value::Array(tools));
    }
    if let Some(t) = thinks { b.insert("think".into(), json!(t)); }
    let predict = req.max_tokens + if thinks.is_some() && prefill.is_none() { REASONING_ROOM } else { 0 };
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
    /// after a `NO_THINK_PREFILL`: the answer is shown only at the end, without any reasoning the model still wrote
    /// into it (it sometimes opens a second block, or reasons and then writes `</think>`)
    pub hold: bool,
}

impl Stream {
    /// The reply has reasoned for `REASONING_ROOM` tokens and written nothing else yet (no answer text, no call). A
    /// small model past that mostly goes round in circles until the whole budget is gone (on a long pasted message it can
    /// spend the whole output budget reasoning and give no answer), so the stream stops here (`cap`) and the
    /// turn asks again without reasoning (`Client::stop_reasoning`).
    pub fn over_reasoning(&self) -> bool { self.thinking >= REASONING_ROOM && self.text.is_empty() && self.calls.is_empty() }

    /// Stop the reply as if it hit its output limit: its reasoning is its whole output (no `done` line will come).
    pub fn cap(&mut self) {
        self.done_reason = "length".into();
        self.usage.output_tokens = self.usage.output_tokens.max(self.thinking);
        self.usage.reasoning_tokens = self.thinking;
    }

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
            if !self.hold { on_text(t); }
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
    pub fn finish(mut self, cancelled: bool) -> (Vec<Block>, String, Usage) {
        if self.hold {
            let (answer, reasoning) = split_reasoning(&self.text);
            if reasoning > 0 { self.usage.reasoning_tokens += (reasoning / 4).max(1).min(self.usage.output_tokens.saturating_sub(self.usage.reasoning_tokens)); }
            self.text = answer;
        }
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

/// A held answer without the reasoning a model wrote into it: everything up to the last `</think>`, or all of it when
/// it opened `<think>` and never closed it (cut at the limit). Returns (answer, chars of reasoning dropped).
pub fn split_reasoning(text: &str) -> (String, usize) {
    if let Some(i) = text.rfind("</think>") {
        return (text[i + "</think>".len()..].trim_start().to_string(), i + "</think>".len());
    }
    if text.trim_start().starts_with("<think>") { return (String::new(), text.len()); }
    (text.to_string(), 0)
}

/// One streamed call to `root` (the server without `/v1`), with the window `num_ctx`. `simple` = a simple turn: a
/// reasoning model is asked not to reason (`no_think`). One attempt: `Client::send` retries a transient failure.
#[allow(clippy::too_many_arguments)]
pub fn send(agent: &ureq::Agent, root: &str, key: &str, model: &str, num_ctx: usize, thinks: Option<bool>, simple: bool, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
    let (thinks, prefill) = no_think(model, thinks, simple);
    let t0 = Instant::now();
    publish(Phase::Sent);
    let _idle = PhaseGuard;
    let mut rq = agent.post(format!("{}/api/chat", root.trim_end_matches('/'))).header("content-type", "application/json");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    // the network reads run on their own thread: Esc stops at once, also before the first byte (`api::post_lines`)
    let mut lines = crate::api::post_lines(rq, body(model, req, num_ctx, thinks, prefill).to_string(), cancel)?;
    if lines.is_some() { publish(Phase::Started); }
    let mut s = Stream { hold: prefill.is_some(), ..Default::default() };
    let mut ttft = None;
    let mut cancelled = false;
    loop {
        let line = match lines.as_mut().map(|l| l.next(cancel)).unwrap_or(Ok(Next::Cancelled))? {
            Next::Line(l) => l,
            Next::End => break,
            Next::Cancelled => { cancelled = true; break; }
        };
        let done = s.feed(line.trim(), on_text)?;
        if s.started { ttft.get_or_insert_with(|| t0.elapsed().as_secs_f64()); }
        if done { break; }
        // closing the stream stops the generation on the server
        if s.over_reasoning() { s.cap(); drop(lines.take()); break; }
    }
    let total = t0.elapsed().as_secs_f64();
    // Ollama's own decode time, when it gave one, makes tok/s exact: total - decode is the wait before the first token
    let ttft = match (s.decode_s, ttft) {
        (Some(d), _) if d < total => total - d,
        (_, Some(t)) => t,
        _ => total,
    };
    let hold = s.hold;
    let (content, stop_reason, usage) = s.finish(cancelled);
    // a held answer shows at once, its reasoning taken out
    if hold { if let Some(Block::Text { text }) = content.first() { on_text(text); } }
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
        let b = body("m", &req, 8192, None, None);
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
        let b = body("m", &req, 8192, Some(false), None);
        assert_eq!((b["think"].clone(), b["options"]["num_predict"].clone()), (json!(false), json!(512 + REASONING_ROOM)));
        let mut s = Stream::default();
        let mut on = |_: &str| {};
        for _ in 0..30 { s.feed(r#"{"message":{"content":"","thinking":"x"},"done":false}"#, &mut on).unwrap(); }
        s.feed(r#"{"message":{"content":"ok"},"done":false}"#, &mut on).unwrap();
        s.feed(r#"{"message":{"content":""},"done":true,"done_reason":"stop","eval_count":31}"#, &mut on).unwrap();
        assert_eq!(s.finish(false).2.reasoning_tokens, 30);
    }

    #[test]
    fn reasoning_past_its_room_with_no_answer_stops_as_the_output_limit() {
        // the stream stops once the reasoning alone fills REASONING_ROOM, and reads as max_tokens
        let mut s = Stream::default();
        let mut on = |_: &str| {};
        let think = r#"{"message":{"content":"","thinking":"x"},"done":false}"#;
        for _ in 0..REASONING_ROOM - 1 { s.feed(think, &mut on).unwrap(); }
        assert!(!s.over_reasoning());
        s.feed(think, &mut on).unwrap();
        assert!(s.over_reasoning());
        s.cap();
        let (content, stop, usage) = s.finish(false);
        assert!(content.is_empty());
        assert_eq!((stop.as_str(), usage.reasoning_tokens, usage.output_tokens), ("max_tokens", REASONING_ROOM, REASONING_ROOM));
        // an answer already started: never cut, however long the reasoning
        let mut s = Stream::default();
        s.feed(r#"{"message":{"content":"Hi"},"done":false}"#, &mut on).unwrap();
        for _ in 0..REASONING_ROOM + 5 { s.feed(think, &mut on).unwrap(); }
        assert!(!s.over_reasoning());
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

    #[test]
    fn a_simple_turn_asks_a_reasoning_model_not_to_reason() {
        // a reasoning Qwen3: think false, the closed-reasoning prefill after the history, no reasoning room
        let (t, p) = no_think("qwen3-vl:2b", Some(true), true);
        assert_eq!((t, p), (Some(false), Some(NO_THINK_PREFILL)));
        let msgs = vec![Message::user_text("hi")];
        let req = Request { system: "s", messages: &msgs, tools: &[], max_tokens: 512 };
        let b = body("qwen3-vl:2b", &req, 8192, t, p);
        assert_eq!(b["think"], json!(false));
        assert_eq!(b["options"]["num_predict"], json!(512));
        assert_eq!(b["messages"][1], json!({"role": "user", "content": "hi"}));
        assert_eq!(b["messages"][2], json!({"role": "assistant", "content": NO_THINK_PREFILL}));
        // another reasoning model: only the flag (it may ignore it, so it keeps its room); a model that does not
        // reason, or a turn that is not simple: as before
        assert_eq!(no_think("deepseek-r1:8b", Some(true), true), (Some(false), None));
        assert_eq!(no_think("qwen3-vl:2b-instruct", None, true), (None, None));
        assert_eq!(no_think("qwen3-vl:2b", Some(true), false), (Some(true), None));
    }

    #[test]
    fn a_held_answer_loses_the_reasoning_written_into_it() {
        assert_eq!(split_reasoning("Paris."), ("Paris.".into(), 0));
        let (a, n) = split_reasoning("Okay, the user said thanks.\n</think>\n\nYou're welcome!");
        assert_eq!(a, "You're welcome!");
        assert!(n > 20);
        assert_eq!(split_reasoning("<think>\nOkay, the user").0, "");
        // streamed: nothing shown until the end, then only the answer
        let mut s = Stream { hold: true, ..Default::default() };
        let mut shown = String::new();
        let mut on = |t: &str| shown.push_str(t);
        s.feed(r#"{"message":{"content":"hmm\n</think>\n\nHi!"},"done":false}"#, &mut on).unwrap();
        s.feed(r#"{"message":{"content":""},"done":true,"done_reason":"stop","eval_count":9}"#, &mut on).unwrap();
        assert!(shown.is_empty());
        let (content, _, usage) = s.finish(false);
        assert_eq!(content, vec![Block::Text { text: "Hi!".into() }]);
        assert!(usage.reasoning_tokens > 0);
    }
}
