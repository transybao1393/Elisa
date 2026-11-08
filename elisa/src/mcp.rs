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
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    /// `stdin`: None once closed (shutdown)
    /// `tail`: the last line the server wrote to stderr (capped) and whether stderr has closed, for the note when it exits
    Stdio { child: Child, stdin: Option<ChildStdin>, rx: Receiver<Value>, tail: Arc<Mutex<(String, bool)>> },
    Http { agent: ureq::Agent, url: String, key: String, session: Option<String> },
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Link::Stdio { child, stdin, .. } = self { stdin.take(); stop(child, STOP_GRACE); }
    }
}

/// Variables a stdio MCP server inherits from Elisa's environment; every other one is left out (API keys, tokens), so
/// a server sees only these, the `env` its config gives it and the names in its `env_pass`.
pub const ENV_ALLOW: [&str; 8] = ["PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR", "TERM"];

/// a variable every stdio server inherits (`ENV_ALLOW`, plus the `LC_*` locale settings)
pub fn inherited(name: &str) -> bool { ENV_ALLOW.contains(&name) || name.starts_with("LC_") }

/// after its input is closed, how long a server may take to exit by itself
pub const STOP_GRACE: Duration = Duration::from_millis(1500);
/// after SIGTERM to its process group, how long before SIGKILL
const TERM_GRACE: Duration = Duration::from_millis(500);
/// soft limit on open files for a server (only ever lowered to this, never raised)
const MAX_FILES: libc::rlim_t = 65_536;

/// In the server's process, between fork and exec (async-signal-safe calls only): no core dumps (a crash must not
/// write the server's memory, which may hold keys, to disk), and a sane cap on open files. Errors are ignored.
fn limits() -> std::io::Result<()> {
    // SAFETY: getrlimit / setrlimit are async-signal-safe and touch only this (child) process
    unsafe {
        let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        let mut files = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut files) == 0 && files.rlim_cur > MAX_FILES {
            files.rlim_cur = MAX_FILES;
            libc::setrlimit(libc::RLIMIT_NOFILE, &files);
        }
    }
    Ok(())
}

/// Stops a server and everything it started (its process group, `spawn`): its input is already closed, so it gets
/// `grace` to exit by itself; then SIGTERM to the group, up to TERM_GRACE, then SIGKILL. The server is reaped. A
/// server that exits at once, with nothing left behind, costs no waiting.
fn stop(child: &mut Child, grace: Duration) {
    let pg = child.id() as libc::pid_t;
    let pause = || std::thread::sleep(Duration::from_millis(10));
    let reaped = |c: &mut Child| !matches!(c.try_wait(), Ok(None));
    let deadline = Instant::now() + grace;
    let mut done = reaped(child);
    while !done && Instant::now() < deadline { pause(); done = reaped(child); }
    // what is left of the group: the server if it did not exit, and anything it started (grandchildren). The group id
    // cannot be reused while any of them is alive, so signalling it never reaches another program.
    // SAFETY: killpg only sends a signal
    if unsafe { libc::killpg(pg, libc::SIGTERM) } == 0 {
        let deadline = Instant::now() + TERM_GRACE;
        loop {
            if !done { done = reaped(child); }
            // signal 0: is any process of the group left (the server itself is reaped by now)
            if done && unsafe { libc::killpg(pg, 0) } != 0 { break; }
            if Instant::now() >= deadline {
                unsafe { libc::killpg(pg, libc::SIGKILL); }
                break;
            }
            pause();
        }
    }
    if !done { let _ = child.wait(); }
}

fn rpc_result(v: &Value) -> Result<Value, String> {
    if !v["error"].is_null() { return Err(format!("MCP error: {}", v["error"]["message"].as_str().unwrap_or("unknown"))); }
    Ok(v["result"].clone())
}

/// One connected server, initialized, with its model tool chosen.
pub struct Conn { link: Link, next: AtomicU64, pub tool: Tool }

impl Conn {
    pub fn open(t: &McpTarget, key: &str) -> Result<Conn, String> {
        let env: Vec<(String, String)> = if key.is_empty() { Vec::new() } else { vec![("MCP_API_KEY".into(), key.into())] };
        let (mut c, tools) = Self::connect_with(&t.command, &t.url, key, &env, None)?;
        c.tool = pick(&tools, &t.tool)?;
        Ok(c)
    }

    /// A server, initialized, and the tools it lists (`mcp_host.rs`: servers that plug tools into a session).
    pub fn connect(command: &[String], url: &str, env: &[(String, String)]) -> Result<(Conn, Vec<Value>), String> {
        Self::connect_with(command, url, "", env, None)
    }

    /// `connect` with one time limit for `initialize` and `tools/list` together (a session's servers start in
    /// parallel and none may hold the session up for long). On failure the server's process is killed.
    pub fn connect_within(command: &[String], url: &str, env: &[(String, String)], limit: Duration) -> Result<(Conn, Vec<Value>), String> {
        Self::connect_with(command, url, "", env, Some(Instant::now() + limit))
    }

    /// `deadline`: for all of setup; without one, each setup request gets SETUP_TIMEOUT
    fn connect_with(command: &[String], url: &str, key: &str, env: &[(String, String)], deadline: Option<Instant>) -> Result<(Conn, Vec<Value>), String> {
        let limit = || deadline.map_or(SETUP_TIMEOUT, |d| d.saturating_duration_since(Instant::now()));
        let link = if url.is_empty() { Self::spawn(command, env)? } else {
            let agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(10))).build().into();
            Link::Http { agent, url: url.to_string(), key: key.to_string(), session: None }
        };
        let mut c = Conn { link, next: AtomicU64::new(1), tool: Tool { name: String::new(), shape: Shape::Messages, props: Vec::new() } };
        let never = AtomicBool::new(false);
        c.request("initialize", json!({"protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "elisa", "version": env!("CARGO_PKG_VERSION")}}), &never, limit())?;
        c.notify("notifications/initialized", json!({}))?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = cursor.as_ref().map_or_else(|| json!({}), |n| json!({"cursor": n}));
            let r = c.request("tools/list", params, &never, limit())?;
            tools.extend(r["tools"].as_array().cloned().unwrap_or_default());
            cursor = r["nextCursor"].as_str().map(String::from);
            if cursor.is_none() { break; }
        }
        Ok((c, tools))
    }

    fn spawn(command: &[String], env: &[(String, String)]) -> Result<Link, String> {
        let (prog, args) = command.split_first().ok_or("mcp: no command")?;
        let mut cmd = Command::new(prog);
        // stderr is read (and dropped) on its own thread; only its last line is kept, for the note when it exits
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // a clean environment: only the allowlist from Elisa's own (`inherited`), then what this server is given
        cmd.env_clear();
        for (k, v) in std::env::vars_os() {
            if k.to_str().is_some_and(inherited) { cmd.env(k, v); }
        }
        for (k, v) in env { cmd.env(k, v); }
        // its own process group, so stopping it stops whatever it started too (`stop`); Ctrl-C at the terminal no
        // longer reaches it either (Elisa cancels a call itself)
        cmd.process_group(0);
        // SAFETY: `limits` makes only async-signal-safe calls
        unsafe { cmd.pre_exec(limits); }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the MCP server `{prog}`: {e}"))?;
        let stdin = child.stdin.take().ok_or("mcp: no stdin")?;
        let stdout = child.stdout.take().ok_or("mcp: no stdout")?;
        let tail = Arc::new(Mutex::new((String::new(), false)));
        if let Some(se) = child.stderr.take() {
            let t = tail.clone();
            std::thread::Builder::new().name("elisa-mcp-err".into()).spawn(move || {
                let mut r = BufReader::new(se);
                let mut buf = Vec::new();
                // bounded per line: a server that writes megabytes without a newline cannot grow this
                while r.by_ref().take(64 * 1024).read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
                    let l = String::from_utf8_lossy(&buf);
                    let l = l.trim();
                    if !l.is_empty() { if let Ok(mut g) = t.lock() { g.0 = crate::context::one_line(l, STDERR_NOTE_CHARS); } }
                    buf.clear();
                }
                if let Ok(mut g) = t.lock() { g.1 = true; }
            }).map_err(|e| e.to_string())?;
        } else if let Ok(mut g) = tail.lock() { g.1 = true; }
        let (tx, rx) = channel();
        std::thread::Builder::new().name("elisa-mcp".into()).spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) { if tx.send(v).is_err() { return; } }
            }
        }).map_err(|e| e.to_string())?;
        Ok(Link::Stdio { child, stdin: Some(stdin), rx, tail })
    }

    fn write_line(stdin: &mut Option<ChildStdin>, v: &Value) -> Result<(), String> {
        let Some(stdin) = stdin.as_mut() else { return Err(format!("{CLOSED_INPUT}: closed")) };
        writeln!(stdin, "{v}").and_then(|()| stdin.flush()).map_err(|e| format!("{CLOSED_INPUT}: {e}"))
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
        let Link::Stdio { stdin, rx, child, tail } = &mut self.link else { unreachable!() };
        if let Err(e) = Self::write_line(stdin, &m) {
            return Err(if e.starts_with(CLOSED_INPUT) { format!("{EXITED}{}", exit_detail(child, tail)) } else { e });
        }
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = Self::write_line(stdin, &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}}));
                return Err(CANCELLED.into());
            }
            if t0.elapsed() > timeout { return Err(format!("the MCP server did not answer {method} in {}", secs(timeout))); }
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
                Err(RecvTimeoutError::Disconnected) => return Err(format!("{EXITED}{}", exit_detail(child, tail))),
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
            if t0.elapsed() > timeout { return Err(format!("the MCP server did not answer in {}", secs(timeout))); }
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

/// the error of a request given up because `cancel` was set
pub const CANCELLED: &str = "cancelled";

/// `3 s`, `1.5 s`: a timeout for a message
pub fn secs(d: Duration) -> String {
    if d.subsec_millis() == 0 { format!("{} s", d.as_secs()) } else { format!("{:.1} s", d.as_secs_f64()) }
}

/// The error says the server is gone (its process exited, its pipe broke, or it cannot be reached), so the
/// connection is of no more use and a new one has to be made.
pub fn gone(err: &str) -> bool {
    err.starts_with(EXITED) || err.starts_with(CLOSED_INPUT) || err.starts_with("cannot reach the MCP server")
}

/// the most of a server's last stderr line a note shows
const STDERR_NOTE_CHARS: usize = 160;

/// After a server's output closed: ` (exit code 3): <its last stderr line>` (or `(signal 9)`, or no status when it has
/// not exited yet), waiting briefly for the process and its stderr. The line goes through `secrets::redact`.
fn exit_detail(child: &mut Child, tail: &Mutex<(String, bool)>) -> String {
    let t0 = Instant::now();
    let mut status = None;
    while t0.elapsed() < Duration::from_millis(500) {
        if status.is_none() { status = child.try_wait().ok().flatten(); }
        if status.is_some() && tail.lock().map_or(true, |g| g.1) { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    let line = tail.lock().map(|g| g.0.clone()).unwrap_or_default();
    exit_text(status.and_then(|s| s.code()), status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s)), &line)
}

fn exit_text(code: Option<i32>, signal: Option<i32>, line: &str) -> String {
    let st = match (code, signal) {
        (Some(c), _) => format!(" (exit code {c})"),
        (None, Some(s)) => format!(" (signal {s})"),
        _ => String::new(),
    };
    let line = crate::secrets::redact(line.trim()).0;
    if line.is_empty() { st } else { format!("{st}: {}", crate::context::one_line(&line, STDERR_NOTE_CHARS)) }
}

const EXITED: &str = "the MCP server exited";
const CLOSED_INPUT: &str = "the MCP server closed its input";

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

#[cfg(test)]
mod exit_tests {
    use super::*;

    #[test]
    fn exit_text_names_the_status_and_the_last_stderr_line() {
        assert_eq!(exit_text(Some(3), None, "fatal: no db"), " (exit code 3): fatal: no db");
        assert_eq!(exit_text(None, Some(9), ""), " (signal 9)");
        assert_eq!(exit_text(None, None, "  "), "");
        let tok = ["ghp_", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5"].concat();
        assert_eq!(exit_text(Some(1), None, &format!("bad key {tok}")), " (exit code 1): bad key [redacted:github-token]");
        let long = "x".repeat(500);
        assert!(exit_text(Some(1), None, &long).chars().count() < STDERR_NOTE_CHARS + 30);
        assert!(gone(&format!("{EXITED} (exit code 3)")) && gone(EXITED));
    }
}
