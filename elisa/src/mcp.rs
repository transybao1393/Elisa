//! A model behind an MCP server: `tools/call` on one tool per model call, over stdio or streamable HTTP.
//!
//! MCP has no standard "generate" call (its `sampling` runs the other way, server to client), so the tool's own input
//! schema decides the shape:
//! - it has a `messages` argument: the Anthropic-shaped messages, plus `system`, `tools`, `max_tokens`, `model` for those
//!   it declares. A reply may be plain text or an Anthropic message (`{"content": [...], "stop_reason": ...}`), the
//!   latter being how such a tool gives the model Elisa's tools.
//! - else a text argument (`prompt`, `input`, `query`, `text`, `message`, or its only required string): the conversation
//!   as one transcript. Text comes back, so the model **cannot use Elisa's tools** on this connection; `probe` says so.
//!
//! No streaming: the reply arrives whole, and `on_text` gets it once. Token counts are estimates unless it has `usage`.
use crate::api::{parse_input, publish, Phase, PhaseGuard, Request};
use crate::provider::McpTarget;
use crate::types::{Block, Message, Reply, Role, Usage};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-06-18";
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(600);
const TEXT_ARGS: [&str; 5] = ["prompt", "input", "query", "text", "message"];

/// how a tool takes the conversation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape { Messages, Text(String) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool { pub name: String, pub shape: Shape, pub props: Vec<String> }

fn shape_of(schema: &Value) -> Option<(Shape, Vec<String>)> {
    let props: Vec<String> = schema["properties"].as_object().map(|p| p.keys().cloned().collect()).unwrap_or_default();
    if props.iter().any(|p| p == "messages") { return Some((Shape::Messages, props)); }
    if let Some(a) = TEXT_ARGS.iter().find(|a| props.iter().any(|p| p == *a)) { return Some((Shape::Text((*a).to_string()), props)); }
    let req: Vec<&str> = schema["required"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    match req.as_slice() {
        [one] if schema["properties"][*one]["type"] == "string" => Some((Shape::Text((*one).to_string()), props)),
        _ => None,
    }
}

/// The tool to call out of `tools/list`: the one named, or the server's only one.
pub fn pick(tools: &[Value], want: &str) -> Result<Tool, String> {
    let names = || tools.iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>().join(", ");
    let t = if want.is_empty() {
        match tools { [one] => one, [] => return Err("the MCP server has no tools".into()), _ => return Err(format!("the MCP server has several tools, so set tool: {}", names())) }
    } else {
        tools.iter().find(|t| t["name"] == want).ok_or_else(|| format!("the MCP server has no tool {want:?}; it has: {}", names()))?
    };
    let name = t["name"].as_str().unwrap_or("").to_string();
    let (shape, props) = shape_of(&t["inputSchema"]).ok_or_else(|| format!("MCP tool {name}: no messages or text argument to send the conversation in"))?;
    Ok(Tool { name, shape, props })
}

/// The conversation as one text (a text-argument tool).
pub fn transcript(msgs: &[Message]) -> String {
    let mut s = String::new();
    for m in msgs {
        let who = match m.role { Role::User => "User", Role::Assistant => "Assistant" };
        for b in &m.content {
            match b {
                Block::Text { text } => s.push_str(&format!("{who}: {text}\n\n")),
                Block::ToolUse { name, input, .. } => s.push_str(&format!("[tool {name} {input}]\n\n")),
                Block::ToolResult { content, .. } => s.push_str(&format!("[result: {content}]\n\n")),
                Block::Image { .. } => s.push_str(&format!("{who}: [an image, which this tool cannot take]\n\n")),
            }
        }
    }
    s.trim_end().to_string()
}

pub fn arguments(tool: &Tool, model: &str, req: &Request) -> Value {
    let has = |k: &str| tool.props.iter().any(|p| p == k);
    let mut a = Map::new();
    match &tool.shape {
        Shape::Messages => {
            let msgs: Vec<Value> = req.messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
            a.insert("messages".into(), Value::Array(msgs));
            if has("tools") && !req.tools.is_empty() { a.insert("tools".into(), json!(req.tools)); }
        }
        Shape::Text(arg) => {
            let t = transcript(req.messages);
            let text = if has("system") || req.system.is_empty() { t } else { format!("{}\n\n{t}", req.system) };
            a.insert(arg.clone(), json!(text));
        }
    }
    if has("system") && !req.system.is_empty() { a.insert("system".into(), json!(req.system)); }
    if has("max_tokens") { a.insert("max_tokens".into(), json!(req.max_tokens)); }
    if has("model") && !model.is_empty() { a.insert("model".into(), json!(model)); }
    Value::Object(a)
}

/// `tools/call`'s result as a reply: `isError` is an error; an Anthropic-shaped message (in `structuredContent` or as
/// the JSON of the text) gives its blocks; anything else is text.
pub fn reply_of(result: &Value, input_estimate: usize) -> Result<(Vec<Block>, String, Usage), String> {
    let text: String = result["content"].as_array().map_or(String::new(), |c| c.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""));
    if result["isError"].as_bool() == Some(true) {
        return Err(format!("MCP tool error: {}", if text.is_empty() { result.to_string() } else { text }));
    }
    let parsed = serde_json::from_str::<Value>(text.trim()).ok();
    let msg = [Some(&result["structuredContent"]), parsed.as_ref()].into_iter().flatten().find(|v| v["content"].is_array()).cloned();
    let mut usage = Usage { input_tokens: input_estimate, ..Usage::default() };
    let (content, stop) = match msg {
        Some(m) => {
            let mut blocks = Vec::new();
            for b in m["content"].as_array().map_or(&[][..], Vec::as_slice) {
                match b["type"].as_str() {
                    Some("text") => blocks.push(Block::Text { text: b["text"].as_str().unwrap_or("").to_string() }),
                    Some("tool_use") => blocks.push(Block::ToolUse {
                        id: b["id"].as_str().unwrap_or("").to_string(), name: b["name"].as_str().unwrap_or("").to_string(),
                        input: if b["input"].is_object() { b["input"].clone() } else { parse_input(b["input"].as_str().unwrap_or("")) },
                    }),
                    _ => {}
                }
            }
            if let Some(n) = m["usage"]["input_tokens"].as_u64() { usage.input_tokens = n as usize; }
            if let Some(n) = m["usage"]["output_tokens"].as_u64() { usage.output_tokens = n as usize; }
            (blocks, m["stop_reason"].as_str().map(String::from))
        }
        None => (vec![Block::Text { text }], None),
    };
    if usage.output_tokens == 0 {
        usage.output_tokens = content.iter().map(|b| match b {
            Block::Text { text } => crate::context::estimate_tokens(text),
            Block::ToolUse { input, .. } => crate::context::estimate_tokens(&input.to_string()),
            _ => 0,
        }).sum();
    }
    let calls = content.iter().any(|b| matches!(b, Block::ToolUse { .. }));
    let stop = stop.unwrap_or_else(|| if calls { "tool_use".into() } else { "end_turn".into() });
    Ok((content, stop, usage))
}

enum Link {
    Stdio { child: Child, stdin: ChildStdin, rx: Receiver<Value> },
    Http { agent: ureq::Agent, url: String, key: String, session: Option<String> },
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Link::Stdio { child, .. } = self { let _ = child.kill(); let _ = child.wait(); }
    }
}

fn rpc_result(v: &Value) -> Result<Value, String> {
    if !v["error"].is_null() { return Err(format!("MCP error: {}", v["error"]["message"].as_str().unwrap_or("unknown"))); }
    Ok(v["result"].clone())
}

/// One connected server, initialized, with its model tool chosen.
pub struct Conn { link: Link, next: AtomicU64, pub tool: Tool }

impl Conn {
    pub fn open(t: &McpTarget, key: &str) -> Result<Conn, String> {
        let link = if t.url.is_empty() { Self::spawn(&t.command, key)? } else {
            let agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(10))).build().into();
            Link::Http { agent, url: t.url.clone(), key: key.to_string(), session: None }
        };
        let mut c = Conn { link, next: AtomicU64::new(1), tool: Tool { name: String::new(), shape: Shape::Messages, props: Vec::new() } };
        let never = AtomicBool::new(false);
        c.request("initialize", json!({"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "elisa", "version": env!("CARGO_PKG_VERSION")}}), &never, SETUP_TIMEOUT)?;
        c.notify("notifications/initialized", json!({}))?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = cursor.as_ref().map_or_else(|| json!({}), |n| json!({"cursor": n}));
            let r = c.request("tools/list", params, &never, SETUP_TIMEOUT)?;
            tools.extend(r["tools"].as_array().cloned().unwrap_or_default());
            cursor = r["nextCursor"].as_str().map(String::from);
            if cursor.is_none() { break; }
        }
        c.tool = pick(&tools, &t.tool)?;
        Ok(c)
    }

    fn spawn(command: &[String], key: &str) -> Result<Link, String> {
        let (prog, args) = command.split_first().ok_or("mcp: no command")?;
        let mut cmd = Command::new(prog);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if !key.is_empty() { cmd.env("MCP_API_KEY", key); }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the MCP server `{prog}`: {e}"))?;
        let stdin = child.stdin.take().ok_or("mcp: no stdin")?;
        let stdout = child.stdout.take().ok_or("mcp: no stdout")?;
        let (tx, rx) = channel();
        std::thread::Builder::new().name("elisa-mcp".into()).spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) { if tx.send(v).is_err() { return; } }
            }
        }).map_err(|e| e.to_string())?;
        Ok(Link::Stdio { child, stdin, rx })
    }

    fn write_line(stdin: &mut ChildStdin, v: &Value) -> Result<(), String> {
        writeln!(stdin, "{v}").and_then(|()| stdin.flush()).map_err(|e| format!("the MCP server closed its input: {e}"))
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let m = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &mut self.link {
            Link::Stdio { stdin, .. } => Self::write_line(stdin, &m),
            Link::Http { .. } => self.post(&m).map(|_| ()),
        }
    }

    /// POST one JSON-RPC message; the response body as a reader (JSON or SSE) with its content type.
    fn post(&mut self, m: &Value) -> Result<(String, ureq::Body), String> {
        let Link::Http { agent, url, key, session } = &mut self.link else { return Err("not http".into()) };
        let mut rq = agent.post(url.as_str()).header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream").header("mcp-protocol-version", PROTOCOL);
        if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
        if let Some(s) = session.as_deref() { rq = rq.header("mcp-session-id", s); }
        let r = rq.send(m.to_string()).map_err(|e| format!("cannot reach the MCP server: {e}"))?;
        let status = r.status().as_u16();
        if let Some(s) = r.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) { *session = Some(s.to_string()); }
        let ctype = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if !(200..300).contains(&status) {
            let t = r.into_body().read_to_string().unwrap_or_default();
            return Err(format!("MCP server: HTTP {status}: {}", crate::api::err_message(&t)));
        }
        Ok((ctype, r.into_body()))
    }

    /// A JSON-RPC request, polling `cancel`. A cancelled call is told to the server and gives Err(CANCELLED).
    pub fn request(&mut self, method: &str, params: Value, cancel: &AtomicBool, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let m = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if matches!(self.link, Link::Http { .. }) { return self.request_http(id, &m, cancel, timeout); }
        let t0 = Instant::now();
        let Link::Stdio { stdin, rx, .. } = &mut self.link else { unreachable!() };
        Self::write_line(stdin, &m)?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = Self::write_line(stdin, &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}));
                return Err(CANCELLED.into());
            }
            if t0.elapsed() > timeout { return Err(format!("the MCP server did not answer {method} in {} s", timeout.as_secs())); }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(v) if v["id"].as_u64() == Some(id) && v["method"].is_null() => return rpc_result(&v),
                Ok(v) if !v["method"].is_null() && !v["id"].is_null() => {
                    // a request from the server: answer ping, and refuse what this client did not declare
                    let a = if v["method"] == "ping" { json!({"jsonrpc": "2.0", "id": v["id"], "result": {}}) }
                        else { json!({"jsonrpc": "2.0", "id": v["id"], "error": {"code": -32601, "message": "not supported"}}) };
                    let _ = Self::write_line(stdin, &a);
                }
                Ok(_) => {} // a notification, or a late answer to a cancelled call
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("the MCP server exited".into()),
            }
        }
    }

    fn request_http(&mut self, id: u64, m: &Value, cancel: &AtomicBool, timeout: Duration) -> Result<Value, String> {
        let t0 = Instant::now();
        let (ctype, body) = self.post(m)?;
        if !ctype.contains("event-stream") {
            let t = body.into_with_config().limit(64 << 20).read_to_string().map_err(|e| format!("MCP read: {e}"))?;
            return rpc_result(&serde_json::from_str::<Value>(&t).map_err(|e| format!("MCP: bad JSON: {e}"))?);
        }
        for line in BufReader::new(body.into_reader()).lines() {
            if cancel.load(Ordering::Relaxed) { return Err(CANCELLED.into()); }
            if t0.elapsed() > timeout { return Err(format!("the MCP server did not answer in {} s", timeout.as_secs())); }
            let line = line.map_err(|e| format!("MCP read: {e}"))?;
            let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data.trim()) else { continue };
            if v["id"].as_u64() == Some(id) && v["method"].is_null() { return rpc_result(&v); }
        }
        Err("the MCP server ended the stream without an answer".into())
    }

    /// One model call. The Reply's token counts are estimates unless the tool reports `usage`.
    pub fn call(&mut self, model: &str, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
        let t0 = Instant::now();
        let args = arguments(&self.tool, model, req);
        let est = crate::context::estimate_tokens(req.system) + crate::context::estimate_messages(req.messages);
        let params = json!({"name": self.tool.name, "arguments": args});
        match self.request("tools/call", params, cancel, CALL_TIMEOUT) {
            Err(e) if e == CANCELLED => {
                let total = t0.elapsed().as_secs_f64();
                Ok(Reply { stop_reason: "cancelled".into(), usage: Usage { input_tokens: est, ..Usage::default() }, ttft_s: total, total_s: total, ..Reply::default() })
            }
            Err(e) => Err(e),
            Ok(r) => {
                let (content, stop_reason, usage) = reply_of(&r, est)?;
                let ttft_s = t0.elapsed().as_secs_f64();
                publish(Phase::Decode);
                for b in &content { if let Block::Text { text } = b { on_text(text); } }
                Ok(Reply { id: String::new(), content, stop_reason, usage, ttft_s, total_s: t0.elapsed().as_secs_f64() })
            }
        }
    }
}

const CANCELLED: &str = "cancelled";

/// The connection's model call: keeps `conn` open across calls and drops it after a failure, so the next call starts
/// a fresh server.
pub fn send(conn: &mut Option<Conn>, t: &McpTarget, key: &str, model: &str, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
    publish(Phase::Sent);
    let _idle = PhaseGuard;
    if conn.is_none() { *conn = Some(Conn::open(t, key)?); }
    publish(Phase::Started);
    let r = conn.as_mut().ok_or("mcp: no connection")?.call(model, req, cancel, on_text);
    if r.is_err() { *conn = None; }
    r
}

/// `--provider-test` on an MCP connection: start, initialize, list the tools, pick one. No tool is called.
pub fn probe(t: &McpTarget, key: &str) -> Result<String, String> {
    let c = Conn::open(t, key)?;
    Ok(match &c.tool.shape {
        Shape::Messages if c.tool.props.iter().any(|p| p == "tools") => format!("connected · tool {} takes messages and tools", c.tool.name),
        Shape::Messages => format!("connected · tool {} takes messages, but no tools argument: the model cannot use Elisa's tools here", c.tool.name),
        Shape::Text(a) => format!("connected · tool {} takes text ({a}): chat only, the model cannot use Elisa's tools here", c.tool.name),
    })
}
