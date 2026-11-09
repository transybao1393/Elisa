//! The OpenAI-style `POST {base}/chat/completions` (Ollama, LM Studio, mlx-lm, vLLM, OpenAI, OpenRouter, ...): Elisa's
//! Anthropic-shaped history and tools are translated per request, and the stream comes back as a normal `Reply`.
use crate::api::{parse_input, Next, publish, publish_thinking, publish_call, partial_path, Phase, PhaseGuard, Request};
use crate::types::{Block, Message, Reply, Role, Usage};
use serde_json::{json, Map, Value};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn messages(system: &str, msgs: &[Message]) -> Vec<Value> { messages_ids(system, msgs, &|id: &str| id.to_string()) }

/// The history in OpenAI form; `id` maps Elisa's tool call ids to ones the server accepts (Mistral: `backend::mistral_id`).
fn messages_ids(system: &str, msgs: &[Message], id_of: &dyn Fn(&str) -> String) -> Vec<Value> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    if !system.is_empty() { out.push(json!({"role": "system", "content": system})); }
    for m in msgs {
        let text = m.content.iter().filter_map(|b| if let Block::Text { text } = b { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
        match m.role {
            Role::Assistant => {
                let calls: Vec<Value> = m.content.iter().filter_map(|b| match b {
                    Block::ToolUse { id, name, input } => Some(json!({"id": id_of(id), "type": "function", "function": {"name": name, "arguments": input.to_string()}})),
                    _ => None,
                }).collect();
                let mut a = json!({"role": "assistant", "content": text});
                if !calls.is_empty() { a["tool_calls"] = Value::Array(calls); }
                out.push(a);
            }
            Role::User => {
                // the results answer the assistant's calls, so they come before any text of this turn
                for b in &m.content {
                    if let Block::ToolResult { tool_use_id, content, is_error } = b {
                        let c = if *is_error { format!("[error] {content}") } else { content.clone() };
                        out.push(json!({"role": "tool", "tool_call_id": id_of(tool_use_id), "content": c}));
                    }
                }
                let images: Vec<Value> = m.content.iter().filter_map(|b| match b {
                    Block::Image { source } => Some(json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", source.media_type, source.data)}})),
                    _ => None,
                }).collect();
                if images.is_empty() {
                    if !text.is_empty() { out.push(json!({"role": "user", "content": text})); }
                } else {
                    // with pictures the content is a list of parts: the text first, then each picture
                    let mut parts = Vec::with_capacity(images.len() + 1);
                    if !text.is_empty() { parts.push(json!({"type": "text", "text": text})); }
                    parts.extend(images);
                    out.push(json!({"role": "user", "content": parts}));
                }
            }
        }
    }
    out
}

/// OpenAI's own API refuses `max_tokens` on its newer models; every other server wants it.
fn tokens_field(base: &str) -> &'static str {
    if crate::provider::host(base) == "api.openai.com" { "max_completion_tokens" } else { "max_tokens" }
}

/// What a backend wants on top of the plain request (`backend::Caps`).
#[derive(Clone, Debug, Default)]
pub struct Dialect {
    pub kind: crate::backend::Kind,
    /// llama.cpp: `cache_prompt: true` (reuse the slot's matching prefix; the default on new builds, stated so an
    /// older one does it too)
    pub cache_prompt: bool,
    /// llama.cpp with slot save: the slot every request uses, so the saved file is this session's
    pub id_slot: Option<usize>,
    /// Gemini: an explicit `cachedContents/…` holding the system prompt and tools (then both stay out of the request)
    pub cached_content: Option<String>,
}

pub fn body(base: &str, model: &str, req: &Request) -> Value { body_with(base, model, req, &Dialect::default()) }

pub fn body_with(base: &str, model: &str, req: &Request, d: &Dialect) -> Value {
    use crate::backend::Kind;
    let mut b = Map::new();
    b.insert("model".into(), json!(model));
    b.insert(tokens_field(base).into(), json!(req.max_tokens));
    let cached = d.cached_content.is_some();
    let system = if cached { "" } else { req.system };
    let msgs = if d.kind == Kind::Mistral { messages_ids(system, req.messages, &crate::backend::mistral_id) } else { messages(system, req.messages) };
    b.insert("messages".into(), Value::Array(msgs));
    if let Some(c) = &d.cached_content {
        // Gemini's OpenAI endpoint: Google's own fields go in `extra_body.google`
        b.insert("extra_body".into(), json!({"google": {"cached_content": c}}));
    }
    if d.cache_prompt { b.insert("cache_prompt".into(), json!(true)); }
    if let Some(i) = d.id_slot { b.insert("id_slot".into(), json!(i)); }
    if !req.tools.is_empty() && !cached {
        let tools: Vec<Value> = req.tools.iter().map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}})).collect();
        b.insert("tools".into(), Value::Array(tools));
    }
    b.insert("stream".into(), json!(true));
    b.insert("stream_options".into(), json!({"include_usage": true}));
    Value::Object(b)
}

#[derive(Default)]
struct Call { id: String, name: String, args: String }

/// The state of one streamed reply.
#[derive(Default)]
pub struct Stream {
    text: String,
    calls: Vec<Call>,
    finish: String,
    usage: Usage,
    cache_seen: bool,
    /// llama.cpp `timings`: (prompt_n, cache_n)
    timings: Option<(usize, usize)>,
    /// a first delta came: the seconds to it are the reply's ttft
    pub started: bool,
}

impl Stream {
    /// One `data:` payload. Ok(true) = the stream is over.
    pub fn feed(&mut self, data: &str, on_text: &mut dyn FnMut(&str)) -> Result<bool, String> {
        if data == "[DONE]" { return Ok(true); }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
        if !v["error"].is_null() {
            return Err(v["error"]["message"].as_str().map_or_else(|| v["error"].to_string(), String::from));
        }
        // usage: OpenAI / vLLM / OpenRouter / LM Studio / MLX `usage` (in a last chunk), Groq `x_groq.usage`
        if let Some(u) = v["usage"].as_object().or_else(|| v["x_groq"]["usage"].as_object()) {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
            // the reused part: OpenAI-style `prompt_tokens_details.cached_tokens`, DeepSeek `prompt_cache_hit_tokens`
            let cached = u.get("prompt_tokens_details").and_then(|d| d["cached_tokens"].as_u64())
                .or_else(|| u.get("prompt_cache_hit_tokens").and_then(Value::as_u64));
            self.cache_seen |= cached.is_some();
            // Anthropic's usage (what Elisa counts in) keeps the cached part apart from input_tokens
            let reasoning = u.get("completion_tokens_details").and_then(|d| d["reasoning_tokens"].as_u64()).unwrap_or(0) as usize;
            self.usage = Usage { input_tokens: n("prompt_tokens").saturating_sub(cached.unwrap_or(0) as usize), output_tokens: n("completion_tokens"), cache_read_input_tokens: cached.unwrap_or(0) as usize, reasoning_tokens: reasoning.min(n("completion_tokens")) };
        }
        // llama.cpp's `timings` (last chunk): `prompt_n` tokens read now, `cache_n` reused from the slot
        if let Some(t) = v["timings"].as_object() {
            let n = |k: &str| t.get(k).and_then(Value::as_u64).map(|x| x as usize);
            if let (Some(p), Some(c)) = (n("prompt_n"), n("cache_n")) {
                self.timings = Some((p, c));
            }
        }
        let c = &v["choices"][0];
        if let Some(f) = c["finish_reason"].as_str() { self.finish = f.to_string(); }
        let d = &c["delta"];
        // reasoning (Ollama / OpenRouter `reasoning`, vLLM / LM Studio `reasoning_content`) is not shown, but it is the
        // model's first output: timing from the first answer word made tok/s absurd (or 0 when no answer came)
        if let Some(t) = ["reasoning", "reasoning_content"].iter().find_map(|k| d[k].as_str().filter(|t| !t.is_empty())) {
            if !self.started { self.started = true; publish(Phase::Decode); }
            publish_thinking(t);
        }
        if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
            if !self.started { self.started = true; publish(Phase::Decode); }
            self.text.push_str(t);
            on_text(t);
        }
        for tc in d["tool_calls"].as_array().map_or(&[][..], Vec::as_slice) {
            self.started = true;
            let i = match tc["index"].as_u64() {
                Some(i) => i as usize,
                None => {
                    // no index: a fragment continues the last call unless it starts a new one (an id or a name of its
                    // own, while the last call already has its name)
                    let starts = tc["id"].as_str().is_some_and(|s| !s.is_empty()) || tc["function"]["name"].as_str().is_some_and(|s| !s.is_empty());
                    match self.calls.last() { Some(c) if !(starts && !c.name.is_empty()) => self.calls.len() - 1, _ => self.calls.len() }
                }
            };
            if self.calls.len() <= i { self.calls.resize_with(i + 1, Call::default); }
            let call = &mut self.calls[i];
            if let Some(s) = tc["id"].as_str().filter(|s| !s.is_empty()) { call.id = s.to_string(); }
            if let Some(s) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) { call.name = s.to_string(); publish_call(&call.name, ""); }
            let had_path = partial_path(&call.args).is_some();
            call.args.push_str(tc["function"]["arguments"].as_str().unwrap_or(""));
            if !had_path { if let Some(p) = partial_path(&call.args) { publish_call(&call.name, &p); } }
        }
        Ok(false)
    }

    /// (content, stop_reason, usage, the server reported a cached count)
    pub fn finish(mut self, cancelled: bool) -> (Vec<Block>, String, Usage, bool) {
        // llama.cpp: its timings say what was reused; they win over a usage without `cached_tokens`
        if let Some((p, c)) = self.timings {
            if !self.cache_seen {
                let total = (self.usage.input_tokens + self.usage.cache_read_input_tokens).max(p + c);
                self.usage.cache_read_input_tokens = c;
                self.usage.input_tokens = total.saturating_sub(c);
                self.cache_seen = true;
            }
        }
        let mut content = Vec::new();
        if !self.text.is_empty() { content.push(Block::Text { text: self.text }); }
        if cancelled { return (content, "cancelled".into(), self.usage, self.cache_seen); }
        let any = !self.calls.is_empty();
        for (i, c) in self.calls.into_iter().enumerate() {
            let id = if c.id.is_empty() { format!("call_{i}") } else { c.id };
            content.push(Block::ToolUse { id, name: c.name, input: parse_input(&c.args) });
        }
        // some servers end a tool call with "stop"
        let stop = match self.finish.as_str() {
            "length" => "max_tokens",
            _ if any => "tool_use",
            "tool_calls" | "function_call" => "tool_use",
            _ => "end_turn",
        };
        (content, stop.into(), self.usage, self.cache_seen)
    }
}

/// One streamed call; the bool is `cache_seen` for the metrics.
#[allow(clippy::too_many_arguments)]
pub fn send(agent: &ureq::Agent, base: &str, key: &str, model: &str, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<(Reply, bool), String> {
    send_with(agent, base, key, model, req, &Dialect::default(), cancel, on_text)
}

/// `send` with a backend's extras (`Dialect`).
#[allow(clippy::too_many_arguments)]
pub fn send_with(agent: &ureq::Agent, base: &str, key: &str, model: &str, req: &Request, d: &Dialect, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<(Reply, bool), String> {
    let t0 = Instant::now();
    publish(Phase::Sent);
    let _idle = PhaseGuard;
    let mut rq = agent.post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .header("content-type", "application/json").header("accept", "text/event-stream");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    // the network reads run on their own thread: Esc stops at once, also before the first byte (`api::post_lines`)
    let mut lines = crate::api::post_lines(rq, body_with(base, model, req, d).to_string(), cancel)?;
    if lines.is_some() { publish(Phase::Started); }
    let mut s = Stream::default();
    let mut ttft = None;
    let mut cancelled = false;
    loop {
        let line = match lines.as_mut().map(|l| l.next(cancel)).unwrap_or(Ok(Next::Cancelled))? {
            Next::Line(l) => l,
            Next::End => break,
            Next::Cancelled => { cancelled = true; break; }
        };
        let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else { continue };
        let done = s.feed(data.trim(), on_text)?;
        if s.started { ttft.get_or_insert_with(|| t0.elapsed().as_secs_f64()); }
        if done { break; }
    }
    let total = t0.elapsed().as_secs_f64();
    let (content, stop_reason, usage, seen) = s.finish(cancelled);
    Ok((Reply { id: String::new(), content, stop_reason, usage, ttft_s: ttft.unwrap_or(total), total_s: total }, seen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_becomes_a_data_url_part_after_the_text() {
        let m = Message::new(Role::User, vec![Block::Text { text: "what is this?".into() }, Block::image("image/png", "AAAA")]);
        let out = messages("sys", &[m, Message::user_text("plain")]);
        assert_eq!(out[1]["content"], json!([
            {"type": "text", "text": "what is this?"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
        ]));
        // a message without pictures stays a plain string
        assert_eq!(out[2]["content"], json!("plain"));
    }

    #[test]
    fn reasoning_starts_the_clock() {
        let mut s = Stream::default();
        let mut on = |_: &str| {};
        s.feed(r#"{"choices":[{"delta":{"reasoning":"hmm"}}]}"#, &mut on).unwrap();
        assert!(s.started, "the first reasoning token is the first output");
        let mut v = Stream::default();
        v.feed(r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#, &mut on).unwrap();
        assert!(v.started);
    }
}
