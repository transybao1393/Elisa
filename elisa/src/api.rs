//! Anthropic Messages API client: blocking ureq 3, SSE read line by line.
use crate::config::Config;
use crate::provider::Transport;
use crate::types::{Block, Message, Reply, ToolDef, Usage};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One finished model call, for the per-request metrics (`src/metrics.rs`). `kind` is what the caller set with
/// `Client::set_kind` before the call (agent, fold, summary).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CallMetric {
    pub kind: String,
    /// prompt tokens the server prefilled (Anthropic usage: excludes the cached part)
    pub input_tokens: usize,
    /// prompt tokens the server reused from its KV / snapshot (`cache_read_input_tokens`); None = not reported
    pub cached_input_tokens: Option<usize>,
    pub output_tokens: usize,
    /// seconds from request send to the first text / tool delta
    pub ttft_s: f64,
    pub total_s: f64,
    /// output_tokens / (total - ttft); None when there is no decode window (< 0.2 s, e.g. a tool-call-only reply)
    pub decode_tok_s: Option<f64>,
    pub stop_reason: String,
}

/// the smallest simple-turn output cap (`simple_out`)
pub const SIMPLE_OUT: usize = 1024;

/// A simple turn's output cap (`Client::simple_cap`) for a `max_ctx` window: 1/8 of it, 1024..=4096 (2048 on a 16K
/// window). 2026-11-09: a flat 1024 cut plain chat answers about a paper mid-sentence on a 16K window, three times
/// in a row.
pub fn simple_out(max_ctx: usize) -> usize { (max_ctx / 8).clamp(SIMPLE_OUT, 4 * SIMPLE_OUT) }

/// How often a streaming call that waits on the server (for the response head or the next line) looks at the
/// cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// What `LineStream::next` got.
pub(crate) enum Next { Line(String), End, Cancelled }

/// The body of a streaming response, line by line, read on its own thread (`post_lines`).
pub(crate) struct LineStream { rx: std::sync::mpsc::Receiver<Result<String, String>> }
impl LineStream {
    /// The next line, or Cancelled as soon as `cancel` is set (within CANCEL_POLL, even while the server sends
    /// nothing), or End.
    pub(crate) fn next(&mut self, cancel: &AtomicBool) -> Result<Next, String> {
        use std::sync::mpsc::RecvTimeoutError;
        loop {
            if cancel.load(Ordering::Relaxed) { return Ok(Next::Cancelled); }
            match self.rx.recv_timeout(CANCEL_POLL) {
                Ok(Ok(l)) => return Ok(Next::Line(l)),
                Ok(Err(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(Next::End),
            }
        }
    }
}

/// Send a streaming POST. The blocking network reads (the wait for the response head, then each line) run on a
/// thread of their own, so the caller can stop the moment `cancel` is set: Esc before the first token used to wait
/// for the server's first bytes (longrun2: 3.6 s). A cancelled call's thread ends with its connection, the next time
/// the server sends or closes. Ok(None) = cancelled before the response head; an error status is Err("HTTP n: …").
pub(crate) fn post_lines(rq: ureq::RequestBuilder<ureq::typestate::WithBody>, body: String, cancel: &AtomicBool) -> Result<Option<LineStream>, String> {
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let (htx, hrx) = channel::<Result<(), String>>();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let r = match rq.send(body) { Ok(r) => r, Err(e) => { let _ = htx.send(Err(format!("request failed: {e}"))); return; } };
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            let text = r.into_body().read_to_string().unwrap_or_default();
            let _ = htx.send(Err(format!("HTTP {status}: {}", err_message(&text))));
            return;
        }
        if htx.send(Ok(())).is_err() { return; }
        for l in BufReader::new(r.into_body().into_reader()).lines() {
            let failed = l.is_err();
            if tx.send(l.map_err(|e| format!("stream read: {e}"))).is_err() || failed { return; }
        }
    });
    loop {
        if cancel.load(Ordering::Relaxed) { return Ok(None); }
        match hrx.recv_timeout(CANCEL_POLL) {
            Ok(Ok(())) => return Ok(Some(LineStream { rx })),
            Ok(Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("request failed: the connection ended".into()),
        }
    }
}

/// Shown instead of sending a request when no model is configured: Elisa never picks one for the user.
pub const NO_MODEL: &str = "No model chosen, so nothing was sent. Elisa doesn't pick a model for you: \
    in the app, choose one in Model Connections (or on the welcome screen); \
    in the terminal, pass --model NAME or set an active connection with a model in ~/.config/elisa/providers.toml.";

/// A failure worth one more try: the server's own error (5xx), or a model's broken tool call the server could not
/// parse (Ollama: "invalid character '}' after object key", "unexpected end of JSON input"). Never a 4xx (a bad key
/// or a refused request fails the same way again).
pub fn transient(e: &str) -> bool {
    if let Some(code) = e.strip_prefix("HTTP ").and_then(|r| r.get(..3)).and_then(|c| c.parse::<u16>().ok()) { return (500..600).contains(&code); }
    e.contains("JSON input") || e.contains("tool call") || e.contains("invalid character")
}

/// How long a retry waits on a server that is still loading its model (HTTP 503).
const RETRY_WAIT: Duration = Duration::from_secs(3);

/// The decode window `ui::status` uses too: below this there is no honest tok/s.
pub const MIN_DECODE_S: f64 = 0.2;

/// Where the running model call is, from the gateway's own stream (for Shade, `shade::Companion::gateway`):
/// `Sent` = the request is out and the gateway has not started it (its queue; ends at `message_start`); `Started` =
/// the gateway took it (no finer hint from this gateway: the model is working); `Prefill` = the gateway announced it is
/// reading the prompt (streamdense ping `phase: prefill`); `Decode` = the first token came (ping `phase: decode`, or
/// any content delta: reasoning or text); `Call` = the model is writing a tool call (`call_target()`: its name and
/// path; streamdense ping `phase: tool`, or an Anthropic `tool_use` block streaming its input). `Idle` = no call running. Published process-wide (one agent client per process) and polled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Idle, Sent, Started, Prefill, Decode, Call }

/// the published phase: bits 0..3 the phase, the rest a counter that changes with every publish
static PHASE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn publish(p: Phase) {
    let code = match p { Phase::Idle => 0, Phase::Sent => 1, Phase::Started => 2, Phase::Prefill => 3, Phase::Decode => 4, Phase::Call => 5 };
    let seq = (PHASE.load(Ordering::Relaxed) >> 3).wrapping_add(1);
    PHASE.store((seq << 3) | code, Ordering::Relaxed);
}

/// the running call's phase now, and a counter that moves with each change (so a poller sees every new call)
pub fn phase() -> (Phase, u64) {
    let v = PHASE.load(Ordering::Relaxed);
    let p = [Phase::Idle, Phase::Sent, Phase::Started, Phase::Prefill, Phase::Decode, Phase::Call];
    (p[((v & 7) as usize).min(5)], v >> 3)
}

/// the running reply's reasoning text, as it streams (Ollama `thinking`, OpenAI-style `reasoning`): the JSONL server
/// drains it into `thinking` events. A frontend that never drains it (the TUI) stops it at `THINKING_CAP`.
static THINKING: Mutex<String> = Mutex::new(String::new());
const THINKING_CAP: usize = 64 * 1024;

pub(crate) fn publish_thinking(t: &str) {
    if let Ok(mut b) = THINKING.lock() { if b.len() + t.len() <= THINKING_CAP { b.push_str(t); } }
}

/// the reasoning streamed since the last call (empty when none)
pub fn take_thinking() -> String { THINKING.lock().map(|mut b| std::mem::take(&mut *b)).unwrap_or_default() }

/// the tool call being written (`Phase::Call`): its tool name and `path` argument (empty until known)
static CALL: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

pub fn call_target() -> (String, String) { CALL.lock().map(|c| c.clone()).unwrap_or_default() }

pub(crate) fn publish_call(name: &str, path: &str) {
    if let Ok(mut c) = CALL.lock() { *c = (name.to_string(), path.to_string()); }
    publish(Phase::Call);
}

/// the `"path"` string value in a partial JSON object (`{"path": "src/a.rs", "old_string": "…`), once it is complete
pub fn partial_path(json: &str) -> Option<String> {
    let i = json.find("\"path\"")?;
    let rest = json[i + 6..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// a gateway `ping` event's phase hint (streamdense gateway: `{"type":"ping","phase":"prefill"|"decode"}`)
pub fn ping_phase(d: &Value) -> Option<Phase> {
    match d["phase"].as_str()? { "prefill" => Some(Phase::Prefill), "decode" => Some(Phase::Decode), _ => None }
}

/// sets the phase back to Idle when a call ends, however it ends (error, cancel, early return)
pub(crate) struct PhaseGuard;
impl Drop for PhaseGuard { fn drop(&mut self) { publish(Phase::Idle); } }

pub struct Request<'a> {
    pub system: &'a str,
    pub tools: &'a [ToolDef],
    pub messages: &'a [Message],
    pub max_tokens: usize,
}

/// The request body (Anthropic Messages).
fn body_of(model: &str, think: bool, req: &Request, stream: bool) -> Value {
    let mut b = Map::new();
    b.insert("model".into(), json!(model));
    if stream { b.insert("max_tokens".into(), json!(req.max_tokens)); }
    b.insert("system".into(), json!(req.system));
    // role + content only: `id` is local bookkeeping
    let msgs: Vec<Value> = req.messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    b.insert("messages".into(), Value::Array(msgs));
    if !req.tools.is_empty() { b.insert("tools".into(), json!(req.tools)); }
    if think {
        b.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": req.max_tokens.saturating_sub(1).max(1024)}));
    }
    if stream { b.insert("stream".into(), json!(true)); }
    Value::Object(b)
}

/// the project-id header every request carries (`project_id`): the gateway keys per-project state by it
pub const PROJECT_HEADER: &str = "x-streamdense-project";

/// The project id: 16 lowercase hex digits of the FNV-1a 64 hash of the canonical cwd (the path as given when it
/// cannot be canonicalized).
pub fn project_id(cwd: &std::path::Path) -> String {
    let p = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in p.to_string_lossy().as_bytes() { h ^= x as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}")
}

pub struct Client {
    base: String,
    /// `project_id` of the session's cwd (`PROJECT_HEADER`)
    project: String,
    model: String,
    key: String,
    think: bool,
    agent: ureq::Agent,
    /// label for the next calls' metrics (`set_kind`)
    kind: Mutex<String>,
    /// finished calls since the last `take_calls` (drained per user request, so it stays small)
    calls: Mutex<Vec<CallMetric>>,
    transport: Transport,
    /// `Transport::Mcp`: the open server, started on the first call
    mcp: Mutex<Option<crate::mcp::Conn>>,
    /// an `openai` connection that is Ollama: its native API with this window (`use_ollama`); 0 = the OpenAI endpoint
    ollama_ctx: std::sync::atomic::AtomicUsize,
    /// that Ollama model reasons (capability "thinking"): it gets `think` = the session's setting and room to reason
    ollama_thinks: AtomicBool,
    /// the turn in progress is a simple one (`set_simple`): no reasoning where the connection can switch it off
    simple: AtomicBool,
    /// the rest of this turn's agent calls ask the model not to reason, with the full output budget (`stop_reasoning`,
    /// after a reply that only reasoned); cleared by the next `set_simple`
    think_off: AtomicBool,
    /// what the backend is and can do (`backend::detect`, `set_caps`)
    caps: crate::backend::Caps,
    /// the tools go in the system prompt as text (`backend::Tools::Text`): set from the caps, or after a server
    /// refused `tools`
    text_tools: AtomicBool,
    /// Gemini's explicit cache of the system prompt and tools (`backend::GeminiCache`)
    gemini: Mutex<crate::backend::GeminiCache>,
    /// the model has no vision (`set_vision`): image parts never go out (`context::without_images`)
    blind: AtomicBool,
}

/// An error that says the server or model takes no `tools` (Ollama "does not support tools", llama.cpp without
/// `--jinja`, an OpenAI-style server's "tool_choice"/"tools" not supported): then the tools go in the prompt as text.
pub fn no_tools_error(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("does not support tools") || l.contains("requires --jinja") || (l.contains("tool") && (l.contains("not supported") || l.contains("unsupported")))
}

enum Part {
    Text(String),
    Tool { id: String, name: String, json: String },
    Skip,
}

impl Part {
    fn finish(self) -> Option<Block> {
        match self {
            Part::Text(t) => Some(Block::Text { text: t }),
            Part::Tool { id, name, json } => Some(Block::ToolUse { id, name, input: parse_input(&json) }),
            Part::Skip => None,
        }
    }
}

pub(crate) fn parse_input(s: &str) -> Value {
    if s.trim().is_empty() { return json!({}); }
    match serde_json::from_str::<Value>(s) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({ "_raw": s }),
    }
}

/// A warm-up answer (JSON or SSE) that generated nothing: no text delta / text block and every `output_tokens` 0.
pub fn warm_took(body: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#""output_tokens"\s*:\s*(\d+)"#).unwrap());
    !body.contains("text_delta") && !body.contains("\"tool_use\"") && re.captures_iter(body).all(|c| &c[1] == "0")
}

pub fn err_message(body: &str) -> String {
    serde_json::from_str::<Value>(body).ok()
        // Gemini's OpenAI endpoint may wrap it in a list: `[{"error": {…}}]`
        .map(|v| if v.is_array() { v[0].clone() } else { v })
        // Anthropic / OpenAI `{"error": {"message": …}}`, Ollama `{"error": "…"}`
        .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// Returns whether the server reported `cache_read_input_tokens` at all (0 counts as reported).
fn read_usage(u: &Value, usage: &mut Usage) -> bool {
    let n = |k: &str| u[k].as_u64().map(|x| x as usize);
    // Anthropic: `input_tokens` excludes both the cache read and the cache write; a write is prefilled too
    let read_now = n("input_tokens").unwrap_or(0) + n("cache_creation_input_tokens").unwrap_or(0);
    if read_now > 0 { usage.input_tokens = read_now; }
    if let Some(x) = n("output_tokens") { usage.output_tokens = x; }
    let cached = n("cache_read_input_tokens");
    if let Some(x) = cached.filter(|&x| x > 0) { usage.cache_read_input_tokens = x; }
    cached.is_some()
}

/// The base the request paths are joined to. The gateway and the Anthropic API take `/v1/messages`, so a base that
/// already ends in `/v1` (as an OpenAI-style URL does) loses it rather than asking for `/v1/v1/messages`.
pub fn request_base(url: &str, transport: &Transport) -> String {
    let b = url.trim_end_matches('/');
    match transport {
        Transport::Gateway | Transport::Anthropic => b.strip_suffix("/v1").unwrap_or(b).to_string(),
        _ => b.to_string(),
    }
}

impl Client {
    pub fn new(cfg: &Config) -> Client {
        // only a connect timeout: no read timeout, so a server without a prompt cache that prefills a long prompt for
        // minutes before its first byte is waited for, not taken for a stall (Esc stops it: `post_lines`)
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .build()
            .into();
        Client {
            base: request_base(&cfg.base_url, &cfg.transport),
            project: project_id(&std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))),
            model: cfg.model.clone(),
            key: cfg.api_key.clone(),
            think: cfg.think,
            agent,
            kind: Mutex::new("agent".into()),
            calls: Mutex::new(Vec::new()),
            transport: cfg.transport.clone(),
            mcp: Mutex::new(None),
            ollama_ctx: std::sync::atomic::AtomicUsize::new(0),
            ollama_thinks: AtomicBool::new(false),
            simple: AtomicBool::new(false),
            think_off: AtomicBool::new(false),
            caps: crate::backend::Caps::default(),
            text_tools: AtomicBool::new(false),
            gemini: Mutex::new(crate::backend::GeminiCache::default()),
            blind: AtomicBool::new(false),
        }
    }

    /// Whether the model can see pictures (the session's detected caps / config). Without vision, every request has
    /// its image parts replaced by a short text stub, wherever they came from (an old saved session, a resumed one).
    pub fn set_vision(&self, vision: bool) { self.blind.store(!vision, Ordering::Relaxed); }

    /// `req`'s messages as this model may get them: None = as they are (`set_vision`)
    fn seen(&self, req: &Request) -> Option<Vec<Message>> {
        if self.blind.load(Ordering::Relaxed) { crate::context::without_images(req.messages) } else { None }
    }

    /// The backend found at connect time (`backend::detect`): an Ollama server gets its native API with the window
    /// `num_ctx`, a server without native tools gets them as text.
    pub fn set_caps(&mut self, caps: crate::backend::Caps, num_ctx: usize) {
        if caps.kind == crate::backend::Kind::Ollama && self.transport == Transport::OpenAi { self.use_ollama(num_ctx, caps.thinking); }
        self.text_tools.store(caps.tools == crate::backend::Tools::Text, Ordering::Relaxed);
        self.caps = caps;
    }

    /// what `set_caps` was given (the default, `Kind::Unknown`, before)
    pub fn caps(&self) -> &crate::backend::Caps { &self.caps }

    /// the tools currently go in the prompt as text
    pub fn text_tools(&self) -> bool { self.text_tools.load(Ordering::Relaxed) }

    /// llama.cpp with `--slot-save-path`: save slot 0's processed prompt to `file` (in the server's slot dir).
    /// Ok((tokens, ms)).
    pub fn slot_save(&self, file: &str) -> Result<(usize, f64), String> {
        if !self.caps.slot_save { return Err("the server has no slot save".into()); }
        crate::backend::slot_action(crate::backend::root(&self.base), &self.key, 0, "save", file)
    }

    /// llama.cpp with `--slot-save-path`: load `file` into slot 0. Ok((tokens, ms)).
    pub fn slot_restore(&self, file: &str) -> Result<(usize, f64), String> {
        if !self.caps.slot_save { return Err("the server has no slot save".into()); }
        crate::backend::slot_action(crate::backend::root(&self.base), &self.key, 0, "restore", file)
    }

    /// the server's address as the slot map records it
    pub fn base(&self) -> &str { &self.base }

    /// the model the requests name
    pub fn model(&self) -> &str { &self.model }

    /// Talk to this `openai` connection through Ollama's native API, with the window `num_ctx` on every request.
    pub fn use_ollama(&self, num_ctx: usize, thinks: bool) {
        self.ollama_ctx.store(num_ctx, Ordering::Relaxed);
        self.ollama_thinks.store(thinks, Ordering::Relaxed);
    }

    /// Output tokens a reply may spend on reasoning on top of its `max_tokens` (an Ollama model that reasons), so a
    /// request sized to the window leaves room for them; 0 elsewhere.
    pub fn reasoning_room(&self) -> usize {
        if self.ollama_ctx.load(Ordering::Relaxed) > 0 && self.ollama_thinks.load(Ordering::Relaxed) { crate::ollama::REASONING_ROOM } else { 0 }
    }

    /// The turn starting now is simple (the reasoning gate's `mcp_reason::wanted` said so) and config `think_simple`
    /// is "off": its agent calls ask the model not to reason where the connection allows it (Ollama's native API:
    /// `ollama::no_think`) and get the smaller `SIMPLE_OUT` cap (`simple_cap`). Set once per turn, so a turn's
    /// requests all have the same shape.
    pub fn set_simple(&self, on: bool) {
        self.simple.store(on, Ordering::Relaxed);
        self.think_off.store(false, Ordering::Relaxed);
    }

    /// A reply spent its whole budget reasoning and wrote nothing: the rest of this turn's agent calls ask
    /// the model not to reason (as a simple turn does, `ollama::no_think`) but keep the full output budget. True when
    /// that changes the next request: a connection that can switch reasoning off (Ollama's native API, a model with
    /// the "thinking" capability) that is not already asked not to reason.
    pub fn stop_reasoning(&self) -> bool {
        let can = self.reasoning_room() > 0 && !self.simple.load(Ordering::Relaxed) && !self.think_off.load(Ordering::Relaxed);
        if can { self.think_off.store(true, Ordering::Relaxed); }
        can
    }

    /// the last `set_simple`
    pub fn simple(&self) -> bool { self.simple.load(Ordering::Relaxed) }

    /// The output cap of a simple turn's agent calls, where the model then does not reason (Ollama's native API, or
    /// Anthropic's API without `think`); None elsewhere (a reasoning model behind another server could spend a small
    /// cap on reasoning and never answer).
    pub fn simple_cap(&self, max_ctx: usize) -> Option<usize> {
        let quiet = self.ollama_ctx.load(Ordering::Relaxed) > 0 || (self.transport == Transport::Anthropic && !self.think);
        (self.simple.load(Ordering::Relaxed) && quiet).then(|| simple_out(max_ctx))
    }

    /// The project the requests are for (`PROJECT_HEADER`; default: the process's current dir at `new`).
    pub fn set_project(&mut self, cwd: &std::path::Path) { self.project = project_id(cwd); }

    /// the project id the requests carry
    pub fn project(&self) -> &str { &self.project }

    /// Label the following calls' metrics (stays until changed).
    pub fn set_kind(&self, kind: &str) {
        if let Ok(mut k) = self.kind.lock() { k.clear(); k.push_str(kind); }
    }

    /// The calls finished since the last take, oldest first (empties the list).
    pub fn take_calls(&self) -> Vec<CallMetric> {
        self.calls.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default()
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        if self.transport != Transport::Gateway {
            // a real Anthropic API: its own key header only, and nothing of the gateway's
            return self.agent.post(format!("{}{}", self.base, path))
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json");
        }
        self.agent.post(format!("{}{}", self.base, path))
            .header("x-api-key", &self.key)
            .header("authorization", format!("Bearer {}", self.key))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header(PROJECT_HEADER, &self.project)
    }

    /// the label the next calls' metrics get (`set_kind`)
    pub fn kind(&self) -> String { self.kind.lock().map(|k| k.clone()).unwrap_or_default() }

    fn body(&self, req: &Request, stream: bool) -> Value {
        let mut b = body_of(&self.model, self.think, req, stream);
        // Anthropic's own API: Elisa's cache breakpoints (not on a count: it takes none)
        if stream && self.caps.cache == crate::backend::Cache::AnthropicBreakpoints { crate::backend::anthropic_breakpoints(&mut b); }
        b
    }

    /// GET {base}/health -> info.max_ctx (streamdense gateway). None if absent/unreachable.
    pub fn server_max_ctx(&self) -> Option<usize> {
        if self.transport != Transport::Gateway { return None; }
        let mut r = self.agent.get(format!("{}/health", self.base)).header(PROJECT_HEADER, &self.project)
            .config().timeout_global(Some(Duration::from_secs(2))).build()
            .call().ok()?;
        if !r.status().is_success() { return None; }
        let v: Value = serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()?;
        v["info"]["max_ctx"].as_u64().map(|x| x as usize)
    }

    /// GET {base}/health: does the gateway take a prefill-only warm-up call (`info.warm` / `warm` true, or "warm" in
    /// `info.features` / `features`)? false when absent or unreachable.
    pub fn server_warm(&self) -> bool {
        if self.transport != Transport::Gateway { return false; }
        let Ok(mut r) = self.agent.get(format!("{}/health", self.base)).header(PROJECT_HEADER, &self.project).config().timeout_global(Some(Duration::from_secs(2))).build().call() else { return false };
        if !r.status().is_success() { return false; }
        let Some(v) = r.body_mut().read_to_string().ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return false };
        let has = |f: &Value| f.as_array().is_some_and(|a| a.iter().any(|x| x.as_str().is_some_and(|s| s.starts_with("warm"))));
        v["info"]["warm"].as_bool() == Some(true) || v["warm"].as_bool() == Some(true) || has(&v["info"]["features"]) || has(&v["features"])
    }

    /// A prefill-only warm-up of `req` (no output: `max_tokens: 0` + `x-streamdense-warm: 1`), to run on another
    /// thread: the gateway reads the prompt and keeps its state / snapshot, so the next call with this prefix reuses
    /// it. The job returns Ok(true) when the gateway took it as a warm-up (2xx, no token generated), Ok(false) when it
    /// answered but generated (no warm-up support), Err on an error status / no connection.
    pub fn warm_job(&self, req: &Request) -> impl FnOnce() -> Result<bool, String> + Send + 'static {
        let no_model = self.model.trim().is_empty();
        let msgs = self.seen(req);
        let req = &Request { messages: msgs.as_deref().unwrap_or(req.messages), ..*req };
        let mut body = self.body(req, false);
        body["max_tokens"] = json!(0);
        let post = self.post("/v1/messages").header("x-streamdense-warm", "1");
        let body = body.to_string();
        move || {
            if no_model { return Err(NO_MODEL.into()); }
            let mut r = post.send(body).map_err(|e| format!("warm-up: {e}"))?;
            let status = r.status().as_u16();
            let text = r.body_mut().read_to_string().unwrap_or_default();
            if !(200..300).contains(&status) { return Err(format!("warm-up: HTTP {status}: {}", err_message(&text))); }
            Ok(warm_took(&text))
        }
    }

    /// POST /v1/messages/count_tokens -> input_tokens.
    pub fn count_tokens(&self, req: &Request) -> Result<usize, String> {
        if !matches!(self.transport, Transport::Gateway | Transport::Anthropic) { return Err("this connection has no token count".into()); }
        let body = self.body(req, false).to_string();
        let mut r = self.post("/v1/messages/count_tokens").send(body).map_err(|e| format!("count_tokens: {e}"))?;
        let status = r.status().as_u16();
        let text = r.body_mut().read_to_string().map_err(|e| format!("count_tokens: {e}"))?;
        if !(200..300).contains(&status) { return Err(format!("HTTP {status}: {}", err_message(&text))); }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("count_tokens: bad JSON: {e}"))?;
        v["input_tokens"].as_u64().map(|x| x as usize).ok_or_else(|| "count_tokens: no input_tokens".into())
    }

    /// POST /v1/messages with stream:true. Calls on_text for each text delta as it arrives.
    /// Stops early (stop_reason "cancelled") when `cancel` becomes true.
    /// A `transient` failure before anything was shown is tried once more (a second sample of a broken tool call is
    /// usually whole); the second failure is the error.
    pub fn send(&self, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
        if self.model.trim().is_empty() { return Err(NO_MODEL.into()); }
        let msgs = self.seen(req);
        let req = &Request { messages: msgs.as_deref().unwrap_or(req.messages), ..*req };
        let mut wrote = false;
        let first = self.send_tools(req, cancel, &mut |t: &str| { wrote = true; on_text(t) });
        match first {
            // the server refused `tools`: from now on they go in the prompt as text
            Err(e) if !req.tools.is_empty() && !self.text_tools() && no_tools_error(&e) && !wrote && !cancel.load(Ordering::Relaxed) => {
                self.text_tools.store(true, Ordering::Relaxed);
                self.send_tools(req, cancel, on_text).map_err(|e2| format!("{e2} (tools sent as text after: {e})"))
            }
            Err(e) if transient(&e) && !wrote && !cancel.load(Ordering::Relaxed) => {
                // a server still loading its model (503) gets a moment first; a broken tool call is resampled at once
                if e.starts_with("HTTP 503") || e.to_ascii_lowercase().contains("loading model") {
                    let t = Instant::now();
                    while t.elapsed() < RETRY_WAIT && !cancel.load(Ordering::Relaxed) { std::thread::sleep(CANCEL_POLL); }
                }
                self.send_tools(req, cancel, on_text).map_err(|e2| if e2 == e { format!("{e} (tried twice)") } else { format!("{e2} (tried twice; first: {e})") })
            }
            r => r,
        }
    }

    /// One attempt, with the tools as text when the backend has no native tool calling (`backend::Tools::Text`): they
    /// are described in the system prompt, earlier calls and results become text, and a call the reply writes as
    /// `<tool_call>{…}</tool_call>` becomes a real one.
    fn send_tools(&self, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
        if req.tools.is_empty() || !self.text_tools() { return self.send_once(req, cancel, on_text); }
        let system = format!("{}{}", req.system, crate::backend::text_tools_prompt(req.tools));
        let msgs = crate::backend::textify(req.messages);
        let r2 = Request { system: &system, tools: &[], messages: &msgs, max_tokens: req.max_tokens };
        let mut reply = self.send_once(&r2, cancel, on_text)?;
        if reply.stop_reason == "cancelled" { return Ok(reply); }
        let mut calls = Vec::new();
        let mut content = Vec::new();
        for b in std::mem::take(&mut reply.content) {
            match b {
                Block::Text { text } => {
                    let (rest, c) = crate::backend::text_calls(&text, req.tools);
                    if !rest.is_empty() { content.push(Block::Text { text: rest }); }
                    calls.extend(c);
                }
                b => content.push(b),
            }
        }
        let any = !calls.is_empty();
        for (i, (name, input)) in calls.into_iter().enumerate() { content.push(Block::ToolUse { id: format!("call_text_{i}"), name, input }); }
        if any && reply.stop_reason == "end_turn" { reply.stop_reason = "tool_use".into(); }
        reply.content = content;
        Ok(reply)
    }

    /// llama.cpp / OpenAI-style extras for this backend; for Gemini, the explicit cache when it pays (created here).
    fn dialect(&self, req: &Request) -> crate::openai::Dialect {
        use crate::backend::{Cache, Kind};
        let mut d = crate::openai::Dialect { kind: self.caps.kind, ..Default::default() };
        if self.caps.cache == Cache::LlamaCpp {
            d.cache_prompt = true;
            if self.caps.slot_save && self.caps.slots > 1 { d.id_slot = Some(0); }
        }
        if self.caps.kind == Kind::Gemini && !self.key.is_empty() {
            let hash = crate::backend::prefix_hash(req.system, req.tools);
            let tokens = crate::context::estimate_tokens(req.system) + crate::context::estimate_tokens(&serde_json::to_string(req.tools).unwrap_or_default());
            let Ok(mut g) = self.gemini.lock() else { return d };
            match g.decide(hash, tokens, &self.model, Instant::now()) {
                Ok(name) => d.cached_content = name,
                Err(h) => match crate::backend::gemini_create_cache(&self.base, &self.key, &self.model, req.system, req.tools) {
                    Ok(name) => {
                        let until = Instant::now() + Duration::from_secs(crate::backend::GEMINI_TTL_S.saturating_sub(15));
                        g.active = Some((h, name.clone(), until));
                        d.cached_content = Some(name);
                    }
                    // not cheaper after all (too small for this model, no caching on it): the prefix goes as usual
                    Err(_) => g.failed = Some(h),
                },
            }
        }
        d
    }

    fn send_once(&self, req: &Request, cancel: &AtomicBool, on_text: &mut dyn FnMut(&str)) -> Result<Reply, String> {
        match &self.transport {
            Transport::OpenAi if self.ollama_ctx.load(Ordering::Relaxed) > 0 => {
                let ctx = self.ollama_ctx.load(Ordering::Relaxed);
                let thinks = self.ollama_thinks.load(Ordering::Relaxed).then_some(self.think);
                let simple = (self.simple.load(Ordering::Relaxed) || self.think_off.load(Ordering::Relaxed)) && self.kind() == "agent";
                let r = crate::ollama::send(&self.agent, crate::provider::ollama_root(&self.base), &self.key, &self.model, ctx, thinks, simple, req, cancel, on_text)?;
                self.record(&r, true);
                return Ok(r);
            }
            Transport::OpenAi => {
                let d = self.dialect(req);
                let (r, seen) = crate::openai::send_with(&self.agent, &self.base, &self.key, &self.model, req, &d, cancel, on_text)?;
                self.record(&r, seen);
                return Ok(r);
            }
            Transport::Mcp(t) => {
                let mut conn = self.mcp.lock().map_err(|_| "mcp: lock".to_string())?;
                let r = crate::mcp::send(&mut conn, t, &self.key, &self.model, req, cancel, on_text)?;
                self.record(&r, false);
                return Ok(r);
            }
            Transport::Gateway | Transport::Anthropic => {}
        }
        let t0 = Instant::now();
        let body = self.body(req, true).to_string();
        publish(Phase::Sent);
        let _idle = PhaseGuard;
        let mut decoding = false;
        let mut lines = post_lines(self.post("/v1/messages").header("accept", "text/event-stream"), body, cancel)?;

        let mut parts: Vec<Option<Part>> = Vec::new();
        let mut done: Vec<(usize, Block)> = Vec::new();
        let mut usage = Usage::default();
        let mut stop_reason = String::new();
        let mut ttft: Option<f64> = None;
        let mut event = String::new();
        let mut cancelled = false;
        let mut id = String::new();
        let mut cache_seen = false;

        loop {
            let line = match lines.as_mut().map(|l| l.next(cancel)).unwrap_or(Ok(Next::Cancelled))? {
                Next::Line(l) => l,
                Next::End => break,
                Next::Cancelled => { cancelled = true; break; }
            };
            let line = line.trim_end_matches('\r');
            if let Some(e) = line.strip_prefix("event:") { event = e.trim().to_string(); continue; }
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(d) = serde_json::from_str::<Value>(data.trim()) else { continue };
            let kind = d["type"].as_str().map(String::from).unwrap_or_else(|| event.clone());
            let idx = d["index"].as_u64().unwrap_or(0) as usize;
            match kind.as_str() {
                "message_start" => {
                    if !decoding { publish(Phase::Started); }
                    cache_seen |= read_usage(&d["message"]["usage"], &mut usage);
                    if let Some(i) = d["message"]["id"].as_str() { id = i.to_string(); }
                }
                "content_block_start" => {
                    let cb = &d["content_block"];
                    let p = match cb["type"].as_str() {
                        Some("text") => Part::Text(cb["text"].as_str().unwrap_or("").to_string()),
                        Some("tool_use") => { decoding = true; publish_call(cb["name"].as_str().unwrap_or(""), ""); Part::Tool {
                            id: cb["id"].as_str().unwrap_or("").to_string(),
                            name: cb["name"].as_str().unwrap_or("").to_string(),
                            json: String::new(),
                        } },
                        _ => Part::Skip,
                    };
                    if parts.len() <= idx { parts.resize_with(idx + 1, || None); }
                    parts[idx] = Some(p);
                }
                "ping" => match ping_phase(&d) {
                    Some(p) if !decoding => { decoding = p == Phase::Decode; publish(p); }
                    _ if d["phase"] == "tool" => {
                        decoding = true;
                        publish_call(d["name"].as_str().unwrap_or(""), d["path"].as_str().unwrap_or(""));
                    }
                    _ => {}
                },
                "content_block_delta" => {
                    if !decoding { decoding = true; publish(Phase::Decode); }
                    ttft.get_or_insert_with(|| t0.elapsed().as_secs_f64());
                    let dl = &d["delta"];
                    match (parts.get_mut(idx).and_then(Option::as_mut), dl["type"].as_str()) {
                        (Some(Part::Text(t)), Some("text_delta")) => {
                            let s = dl["text"].as_str().unwrap_or("");
                            t.push_str(s);
                            on_text(s);
                        }
                        (Some(Part::Tool { json, name, .. }), Some("input_json_delta")) => {
                            let known = partial_path(json).is_some();
                            json.push_str(dl["partial_json"].as_str().unwrap_or(""));
                            if !known { if let Some(p) = partial_path(json) { publish_call(name, &p); } }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    if let Some(b) = parts.get_mut(idx).and_then(Option::take).and_then(Part::finish) { done.push((idx, b)); }
                }
                "message_delta" => {
                    if let Some(s) = d["delta"]["stop_reason"].as_str() { stop_reason = s.to_string(); }
                    cache_seen |= read_usage(&d["usage"], &mut usage);
                }
                "message_stop" => break,
                "error" => return Err(d["error"]["message"].as_str().unwrap_or("stream error").to_string()),
                _ => {}
            }
        }
        drop(lines);

        let total = t0.elapsed().as_secs_f64();
        if cancelled {
            stop_reason = "cancelled".into();
            done.retain(|(_, b)| !matches!(b, Block::ToolUse { .. }));
            for (i, p) in parts.into_iter().enumerate() {
                if let Some(Part::Text(t)) = p { if !t.is_empty() { done.push((i, Block::Text { text: t })); } }
            }
        } else {
            for (i, p) in parts.into_iter().enumerate() {
                if let Some(b) = p.and_then(Part::finish) { done.push((i, b)); }
            }
            if stop_reason.is_empty() { stop_reason = "end_turn".into(); }
            // the streamdense gateway says "tool_use" for a call cut at the limit (its parser closes an unfinished
            // call at the end of the text, so the JSON is whole): the full budget spent counts as cut
            if stop_reason == "tool_use" && usage.output_tokens >= req.max_tokens { stop_reason = "max_tokens".into(); }
        }
        done.sort_by_key(|(i, _)| *i);
        let ttft_s = ttft.unwrap_or(total);
        let reply = Reply {
            id,
            content: done.into_iter().map(|(_, b)| b).collect(),
            stop_reason,
            usage,
            ttft_s,
            total_s: total,
        };
        self.record(&reply, cache_seen);
        Ok(reply)
    }

    /// The finished call's metrics.
    fn record(&self, r: &Reply, cache_seen: bool) {
        let decode = r.total_s - r.ttft_s;
        let m = CallMetric {
            kind: self.kind.lock().map(|k| k.clone()).unwrap_or_default(),
            input_tokens: r.usage.input_tokens,
            cached_input_tokens: cache_seen.then_some(r.usage.cache_read_input_tokens),
            output_tokens: r.usage.output_tokens,
            ttft_s: r.ttft_s, total_s: r.total_s,
            decode_tok_s: (decode >= MIN_DECODE_S).then(|| r.usage.output_tokens as f64 / decode),
            stop_reason: r.stop_reason.clone(),
        };
        if let Ok(mut c) = self.calls.lock() { c.push(m); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reasoning_stop_asks_once_more_without_reasoning() {
        // only where reasoning can be switched off (an Ollama model with the "thinking" capability), once
        // per turn, and not on a simple turn (already asked not to reason)
        let c = Client::new(&Config::default());
        assert!(!c.stop_reasoning());
        c.use_ollama(8192, true);
        c.set_simple(false);
        assert!(c.stop_reasoning());
        assert!(!c.stop_reasoning());
        // the full budget stays: no simple-turn cap
        assert_eq!(c.simple_cap(8192), None);
        c.set_simple(false);
        assert!(c.stop_reasoning());
        c.set_simple(true);
        assert!(!c.stop_reasoning());
        let plain = Client::new(&Config::default());
        plain.use_ollama(8192, false);
        assert!(!plain.stop_reasoning());
    }

    #[test]
    fn only_server_errors_and_broken_calls_are_tried_again() {
        assert!(transient("HTTP 500: invalid character '}' after object key"));
        assert!(transient("HTTP 503: overloaded"));
        assert!(transient("qwen tool call parsing failed: unexpected end of JSON input"));
        assert!(!transient("HTTP 401: invalid x-api-key"));
        assert!(!transient("HTTP 429: rate limited"));
        assert!(!transient("request failed: connection refused"));
        assert_eq!(err_message(r#"{"error":"invalid character '}' after object key"}"#), "invalid character '}' after object key");
        assert_eq!(err_message(r#"{"error":{"message":"bad key"}}"#), "bad key");
    }

    #[test]
    fn phase_hints_parse() {
        assert_eq!(partial_path(r#"{"path": "src/main.py", "old_str"#), Some("src/main.py".into()));
        assert_eq!(partial_path(r#"{"path": "src/ma"#), None);
        assert_eq!(ping_phase(&json!({"type": "ping", "phase": "prefill"})), Some(Phase::Prefill));
        assert_eq!(ping_phase(&json!({"type": "ping"})), None);
        let (_, a) = phase();
        publish_call("edit", "a.rs");
        let (p, b) = phase();
        assert!(b > a);
        if p == Phase::Call { assert_eq!(call_target(), ("edit".into(), "a.rs".into())); }
    }
}
