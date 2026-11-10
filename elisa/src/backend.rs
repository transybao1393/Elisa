//! Which server is behind a connection, and what it can do (`Caps`): found once at connect time from the connection's
//! `type`, the URL's host (the cloud APIs) and a few cheap GETs (local and self-hosted servers). Elisa works the same
//! on every backend: nothing here is a premise. What a backend offers on top (its own prompt / KV cache, a saved slot)
//! is used only where Elisa controls it, and the status line says "reused N" only when the server reported N.
//!
//! | backend | how it is told apart |
//! |---|---|
//! | Ollama | `GET /api/version` answers `{"version"}` (llama.cpp and the rest 404) |
//! | llama.cpp `llama-server` | `GET /props` has `default_generation_settings` / `total_slots` |
//! | LM Studio | `GET /api/v0/models` lists models with `max_context_length` |
//! | vLLM | `GET /v1/models` entries are `owned_by: "vllm"` (with `max_model_len`) |
//! | MLX `mlx_lm.server` | Python's `BaseHTTP` server header on `GET /v1/models` |
//! | LocalAI / GPT4All / Jan | LocalAI `GET /version` without vLLM's models; GPT4All `owned_by: "humanity"`; Jan port 1337 |
//! | cloud APIs | the host: api.openai.com, api.anthropic.com, generativelanguage.googleapis.com, openrouter.ai, … |
use crate::provider::Transport;
use crate::types::{Block, Message, ToolDef};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Unknown,
    /// the streamdense gateway (`type = "streamdense"`)
    Gateway,
    Ollama,
    LlamaCpp,
    LmStudio,
    Mlx,
    Vllm,
    LocalAi,
    Gpt4All,
    Jan,
    /// another OpenAI-style server: nothing told it apart
    OpenAiCompat,
    OpenAi,
    Anthropic,
    /// an Anthropic Messages server that is not api.anthropic.com
    AnthropicCompat,
    Gemini,
    OpenRouter,
    Groq,
    Together,
    DeepSeek,
    Mistral,
    Mcp,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Unknown => "unknown",
            Kind::Gateway => "streamdense gateway",
            Kind::Ollama => "Ollama",
            Kind::LlamaCpp => "llama.cpp llama-server",
            Kind::LmStudio => "LM Studio",
            Kind::Mlx => "MLX mlx_lm.server",
            Kind::Vllm => "vLLM",
            Kind::LocalAi => "LocalAI",
            Kind::Gpt4All => "GPT4All",
            Kind::Jan => "Jan",
            Kind::OpenAiCompat => "OpenAI-compatible server",
            Kind::OpenAi => "OpenAI",
            Kind::Anthropic => "Anthropic",
            Kind::AnthropicCompat => "Anthropic-compatible server",
            Kind::Gemini => "Google Gemini",
            Kind::OpenRouter => "OpenRouter",
            Kind::Groq => "Groq",
            Kind::Together => "Together AI",
            Kind::DeepSeek => "DeepSeek",
            Kind::Mistral => "Mistral",
            Kind::Mcp => "MCP model server",
        }
    }
}

/// How the model calls tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tools {
    /// native tool calling (`tools` in the request, structured calls back)
    #[default]
    Native,
    /// the server or model has none: the tools are described in the system prompt and a call written as
    /// `<tool_call>{…}</tool_call>` is read back from the text (`text_calls`)
    Text,
}

/// The backend's own reuse of earlier work, and what Elisa does with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cache {
    #[default]
    None,
    /// the server reuses a matching prefix by itself (OpenAI, vLLM's automatic prefix caching, LM Studio, MLX,
    /// DeepSeek, Groq, …): Elisa keeps the request prefix stable, nothing else
    Automatic,
    /// Ollama: the runner keeps the last prompt's KV while the model stays loaded (`keep_alive`, the user's setting:
    /// Elisa never changes it)
    OllamaKeepAlive,
    /// llama.cpp: `cache_prompt: true` on every request; slot save/restore when the server has `--slot-save-path`
    LlamaCpp,
    /// Anthropic: explicit `cache_control` breakpoints Elisa places (`anthropic_breakpoints`)
    AnthropicBreakpoints,
    /// Gemini: implicit caching, plus an explicit `cachedContents` entry for a long stable prefix when it is clearly
    /// cheaper (`GeminiCache`)
    GeminiExplicit,
    /// the streamdense gateway's own snapshots and warm-ups
    Gateway,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Caps {
    pub kind: Kind,
    /// how the kind was found ("GET /props", "host api.openai.com", "type = streamdense")
    pub detected_by: String,
    pub version: Option<String>,
    /// the server's window for this model, and where that number came from; None = it reports none
    pub ctx: Option<usize>,
    pub ctx_from: &'static str,
    /// the window is fixed by the server (llama.cpp `-c`, LM Studio's loaded length, vLLM `max_model_len`): a larger
    /// request overflows. Not so for Ollama (Elisa sets `num_ctx` per request) or the cloud.
    pub ctx_hard: bool,
    /// the most the model itself was trained for (Ollama `/api/show` context_length): a larger window, even one the
    /// connection sets, only feeds it text it can't read (moondream's 2048 asked for 16384 answered with garbage)
    pub model_max: Option<usize>,
    pub vision: Option<bool>,
    pub tools: Tools,
    /// the model reasons (Ollama capability "thinking")
    pub thinking: bool,
    /// the usage the server is known to report: the prompt's tokens / how many of them it reused
    pub usage_prompt: bool,
    pub usage_cached: bool,
    /// stream quirks Elisa handles for this backend (shown in /model)
    pub quirks: Vec<&'static str>,
    pub cache: Cache,
    /// llama.cpp: the server was started with `--slot-save-path` (a restore probe did not answer "not supported")
    pub slot_save: bool,
    /// llama.cpp `total_slots`
    pub slots: usize,
    /// Ollama's capability list (`/api/show`)
    pub ollama: Option<Vec<String>>,
    /// Ollama: the model's template when Elisa renders the prompt herself (`ollama::raw_template`: the template puts
    /// whitespace right before the reply)
    pub raw_template: Option<String>,
}

impl Caps {
    /// The server reuses a matching request prefix (its own cache, KV or snapshot): then a request that repeats the
    /// conversation and adds one message is cheap. Without it every request is read in full.
    pub fn reuses_prefix(&self) -> bool { self.cache != Cache::None }

    /// The server keeps its model loaded between requests (llama.cpp, LM Studio's loaded model, vLLM, MLX, the cloud):
    /// a long wait before the first token is the prompt being read, not a model load. Ollama and LocalAI load on demand.
    pub fn always_loaded(&self) -> bool { !matches!(self.kind, Kind::Ollama | Kind::LocalAi | Kind::Unknown | Kind::Gateway | Kind::Mcp) }
}

/// The server without the OpenAI `/v1` path (where Ollama's, llama.cpp's and LM Studio's own endpoints live).
pub fn root(base_url: &str) -> &str { crate::provider::ollama_root(base_url) }

/// A cloud API from its host.
pub fn host_kind(base_url: &str) -> Option<Kind> {
    let h = crate::provider::host(base_url).to_ascii_lowercase();
    let is = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    Some(if is("api.openai.com") { Kind::OpenAi }
    else if is("api.anthropic.com") { Kind::Anthropic }
    else if is("generativelanguage.googleapis.com") { Kind::Gemini }
    else if is("openrouter.ai") { Kind::OpenRouter }
    else if is("api.groq.com") { Kind::Groq }
    else if is("api.together.xyz") || is("api.together.ai") { Kind::Together }
    else if is("api.deepseek.com") { Kind::DeepSeek }
    else if is("api.mistral.ai") { Kind::Mistral }
    else { return None })
}

/// What a kind offers by itself (before any probe filled in the window, vision or tools).
pub fn defaults(kind: Kind) -> Caps {
    let mut c = Caps { kind, usage_prompt: true, ..Default::default() };
    match kind {
        Kind::Gateway => { c.cache = Cache::Gateway; c.usage_cached = true; }
        Kind::Ollama => { c.cache = Cache::OllamaKeepAlive; c.quirks = vec!["native /api/chat (NDJSON) so num_ctx holds", "tool calls arrive whole"]; }
        Kind::LlamaCpp => {
            c.cache = Cache::LlamaCpp;
            c.usage_cached = true;
            c.quirks = vec!["reuse read from timings.cache_n", "tool-call arguments stream in fragments"];
        }
        Kind::LmStudio => { c.cache = Cache::Automatic; c.quirks = vec!["reasoning in delta.reasoning_content"]; }
        Kind::Mlx => { c.cache = Cache::Automatic; c.quirks = vec!["usage only with stream_options.include_usage"]; }
        Kind::Vllm => { c.cache = Cache::Automatic; c.usage_cached = false; c.quirks = vec!["cached_tokens only with --enable-prompt-tokens-details", "reasoning in delta.reasoning_content"]; }
        Kind::LocalAi | Kind::Gpt4All | Kind::Jan | Kind::OpenAiCompat => { c.quirks = vec!["usage may be missing: Elisa's estimate stands in"]; }
        Kind::OpenAi => { c.cache = Cache::Automatic; c.usage_cached = true; c.quirks = vec!["max_completion_tokens", "usage in a last chunk with empty choices"]; }
        Kind::Anthropic => { c.cache = Cache::AnthropicBreakpoints; c.usage_cached = true; c.vision = Some(true); }
        Kind::AnthropicCompat => {}
        Kind::Gemini => { c.cache = Cache::GeminiExplicit; c.usage_cached = true; c.quirks = vec!["tool calls arrive whole, possibly without index", "errors may come as a JSON list"]; }
        Kind::OpenRouter => { c.cache = Cache::Automatic; c.usage_cached = true; c.quirks = vec![": OPENROUTER PROCESSING keep-alive comments", "mid-stream error with finish_reason error"]; }
        Kind::Groq => { c.cache = Cache::Automatic; c.quirks = vec!["usage in x_groq.usage"]; }
        Kind::Together => { c.cache = Cache::Automatic; c.quirks = vec!["usage in the last chunk"]; }
        Kind::DeepSeek => { c.cache = Cache::Automatic; c.usage_cached = true; c.quirks = vec!["reuse in usage.prompt_cache_hit_tokens", "reasoning in delta.reasoning_content"]; }
        Kind::Mistral => { c.cache = Cache::Automatic; c.quirks = vec!["tool call ids must be 9 letters/digits"]; }
        Kind::Mcp | Kind::Unknown => { c.usage_prompt = false; }
    }
    c
}

fn agent(ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_millis(ms))).build().into()
}

/// One probe GET: (status, the `server` header, the body as JSON). None = no answer.
fn get(a: &ureq::Agent, url: &str, key: &str) -> Option<(u16, String, Value)> {
    let mut rq = a.get(url);
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let mut r = rq.call().ok()?;
    let st = r.status().as_u16();
    let server = r.headers().get("server").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let body = r.body_mut().read_to_string().unwrap_or_default();
    Some((st, server, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

fn ok_json(r: &Option<(u16, String, Value)>) -> Option<&Value> {
    r.as_ref().filter(|(st, _, v)| (200..300).contains(st) && !v.is_null()).map(|(_, _, v)| v)
}

/// Ollama `/api/show`: (capabilities, the model's context length from `model_info["<arch>.context_length"]`, its
/// template).
pub fn parse_ollama_show(v: &Value) -> Option<(Vec<String>, Option<usize>, Option<String>)> {
    let caps: Vec<String> = v["capabilities"].as_array()?.iter().filter_map(|c| c.as_str().map(String::from)).collect();
    let ctx = v["model_info"].as_object().and_then(|m| m.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, x)| x.as_u64())).map(|x| x as usize);
    Some((caps, ctx, v["template"].as_str().map(String::from)))
}

/// llama.cpp `/props`: (n_ctx, total_slots, vision, tools in the chat template). None = not llama.cpp.
/// (n_ctx, total_slots, vision, tools)
pub type Props = (Option<usize>, usize, Option<bool>, Option<bool>);
/// (window, vision, tools, state)
pub type LmModel = (Option<usize>, bool, Option<bool>, Option<String>);

pub fn parse_llamacpp_props(v: &Value) -> Option<Props> {
    let g = &v["default_generation_settings"];
    if g.is_null() && v["total_slots"].is_null() { return None; }
    let n_ctx = g["n_ctx"].as_u64().or_else(|| g["params"]["n_ctx"].as_u64()).or_else(|| v["n_ctx"].as_u64()).map(|x| x as usize);
    let slots = v["total_slots"].as_u64().unwrap_or(1) as usize;
    let vision = v["modalities"]["vision"].as_bool();
    let tools = v["chat_template_caps"]["supports_tools"].as_bool().or_else(|| v["chat_template_caps"]["supports_tool_calls"].as_bool());
    Some((n_ctx, slots, vision, tools))
}

/// LM Studio `/api/v0/models`: the entry of `model` (else the first loaded one): (window, vision, tools). The window is
/// the loaded length when LM Studio reports it, else the model's maximum.
pub fn parse_lmstudio(v: &Value, model: &str) -> Option<LmModel> {
    let data = v["data"].as_array()?;
    if !data.iter().any(|m| m.get("max_context_length").is_some() || m.get("compatibility_type").is_some()) { return None; }
    let m = data.iter().find(|m| m["id"] == model).or_else(|| data.iter().find(|m| m["state"] == "loaded")).or(data.first())?;
    let ctx = m["loaded_context_length"].as_u64().or_else(|| m["max_context_length"].as_u64()).map(|x| x as usize);
    let vision = m["type"] == "vlm" || m["capabilities"].as_array().is_some_and(|c| c.iter().any(|x| x == "vision"));
    let tools = m["capabilities"].as_array().map(|c| c.iter().any(|x| x == "tool_use"));
    Some((ctx, vision, tools, m["state"].as_str().map(String::from)))
}

/// A `/v1/models` answer's owner of `model` (or of the first entry) and its `max_model_len` (vLLM).
pub fn parse_models_owner(v: &Value, model: &str) -> Option<(String, Option<usize>)> {
    let data = v["data"].as_array()?;
    let m = data.iter().find(|m| m["id"] == model).or(data.first())?;
    Some((m["owned_by"].as_str().unwrap_or("").to_string(), m["max_model_len"].as_u64().map(|x| x as usize)))
}

/// Find the backend and its caps. Cloud hosts are known by name (no request); any other server gets its probes at once
/// in parallel (each at most `ms`), and the most specific answer wins. Never fails: an unknown server is a generic
/// OpenAI-compatible one.
pub fn detect(transport: &Transport, base_url: &str, model: &str, key: &str) -> Caps {
    detect_with(transport, base_url, model, key, 1500)
}

pub fn detect_with(transport: &Transport, base_url: &str, model: &str, key: &str, ms: u64) -> Caps {
    match transport {
        Transport::Gateway => { let mut c = defaults(Kind::Gateway); c.detected_by = "type = streamdense".into(); return c; }
        Transport::Mcp(_) => { let mut c = defaults(Kind::Mcp); c.detected_by = "type = mcp".into(); return c; }
        Transport::Anthropic => {
            let k = if host_kind(base_url) == Some(Kind::Anthropic) { Kind::Anthropic } else { Kind::AnthropicCompat };
            let mut c = defaults(k);
            c.detected_by = format!("type = anthropic, host {}", crate::provider::host(base_url));
            return c;
        }
        Transport::OpenAi => {}
    }
    if let Some(k) = host_kind(base_url) {
        let mut c = defaults(k);
        c.detected_by = format!("host {}", crate::provider::host(base_url));
        return c;
    }
    let r = root(base_url).trim_end_matches('/').to_string();
    let base = base_url.trim_end_matches('/').to_string();
    let a = agent(ms);
    let (ver, props, lms, models, version) = std::thread::scope(|s| {
        let g = |u: String| { let a = a.clone(); let key = key.to_string(); s.spawn(move || get(&a, &u, &key)) };
        let h = (g(format!("{r}/api/version")), g(format!("{r}/props")), g(format!("{r}/api/v0/models")), g(format!("{base}/models")), g(format!("{r}/version")));
        (h.0.join().ok().flatten(), h.1.join().ok().flatten(), h.2.join().ok().flatten(), h.3.join().ok().flatten(), h.4.join().ok().flatten())
    });
    let server_hdr = |r: &Option<(u16, String, Value)>| r.as_ref().map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    let owner = ok_json(&models).and_then(|v| parse_models_owner(v, model));
    // Ollama: /api/version, and not a llama.cpp that serves Ollama-style routes
    if let Some(v) = ok_json(&ver).filter(|v| v["version"].is_string()).filter(|_| !server_hdr(&ver).contains("llama.cpp")) {
        let mut c = defaults(Kind::Ollama);
        c.version = v["version"].as_str().map(String::from);
        c.detected_by = "GET /api/version".into();
        if let Some((caps, ctx, tmpl)) = ollama_show(&r, model, key, ms) {
            c.raw_template = tmpl.as_deref().and_then(crate::ollama::raw_template);
            if c.raw_template.is_some() { c.quirks.push("the template writes a space before the reply: prompt rendered by Elisa (raw /api/generate)"); }
            c.thinking = caps.iter().any(|x| x == "thinking");
            c.vision = Some(caps.iter().any(|x| x == "vision"));
            if !caps.iter().any(|x| x == "tools") { c.tools = Tools::Text; }
            c.ollama = Some(caps);
            c.model_max = ctx;
            // the window Ollama runs this model with (its loaded context, the user's setting), else the model's
            // maximum capped: Elisa sets num_ctx per request, and a 256K window would not fit in memory
            (c.ctx, c.ctx_from) = match ollama_loaded_ctx(&r, model, key, ms) {
                Some(n) => (Some(n), "loaded context_length (/api/ps)"),
                None => (ctx.map(|n| n.min(OLLAMA_CAP)), "model maximum (/api/show), capped at 8192 until set"),
            };
        }
        return c;
    }
    if let Some((n_ctx, slots, vision, tools)) = ok_json(&props).and_then(parse_llamacpp_props) {
        let mut c = defaults(Kind::LlamaCpp);
        c.detected_by = "GET /props".into();
        c.version = ok_json(&props).and_then(|v| v["build_info"].as_str()).map(String::from);
        (c.ctx, c.ctx_from, c.ctx_hard) = (n_ctx, "n_ctx (/props)", true);
        c.slots = slots;
        c.vision = vision;
        if tools == Some(false) { c.tools = Tools::Text; }
        c.slot_save = slot_save_supported(&a, &r, key);
        return c;
    }
    if let Some((ctx, vision, tools, state)) = ok_json(&lms).and_then(|v| parse_lmstudio(v, model)) {
        let mut c = defaults(Kind::LmStudio);
        c.detected_by = "GET /api/v0/models".into();
        (c.ctx, c.ctx_from, c.ctx_hard) = (ctx, "loaded/max_context_length (/api/v0/models)", true);
        c.vision = Some(vision);
        if tools == Some(false) { c.tools = Tools::Text; }
        c.version = state.map(|s| format!("model {s}"));
        return c;
    }
    if let Some((o, len)) = &owner {
        if o == "vllm" {
            let mut c = defaults(Kind::Vllm);
            c.detected_by = "GET /v1/models (owned_by vllm)".into();
            c.version = ok_json(&version).and_then(|v| v["version"].as_str()).map(String::from);
            (c.ctx, c.ctx_from, c.ctx_hard) = (*len, "max_model_len (/v1/models)", true);
            return c;
        }
        if o == "llamacpp" {
            let mut c = defaults(Kind::LlamaCpp);
            c.detected_by = "GET /v1/models (owned_by llamacpp)".into();
            return c;
        }
        if o == "humanity" {
            let mut c = defaults(Kind::Gpt4All);
            c.detected_by = "GET /v1/models (owned_by humanity)".into();
            return c;
        }
    }
    if server_hdr(&models).starts_with("basehttp") {
        let mut c = defaults(Kind::Mlx);
        c.detected_by = "GET /v1/models (Python BaseHTTP server)".into();
        return c;
    }
    if let Some(v) = ok_json(&version).filter(|v| v["version"].is_string()) {
        let mut c = defaults(Kind::LocalAi);
        c.detected_by = "GET /version".into();
        c.version = v["version"].as_str().map(String::from);
        return c;
    }
    let port = base_url.split("://").nth(1).and_then(|h| h.split('/').next()).and_then(|h| h.rsplit(':').next()).unwrap_or("");
    if port == "1337" && owner.is_some() {
        let mut c = defaults(Kind::Jan);
        c.detected_by = "port 1337 with an OpenAI model list".into();
        return c;
    }
    let mut c = defaults(Kind::OpenAiCompat);
    c.detected_by = if owner.is_some() { "GET /v1/models (no known server)".into() } else { "nothing answered the probes".into() };
    c
}

/// Without a configured `max_ctx` and with the model not loaded: at most this window on Ollama.
pub const OLLAMA_CAP: usize = 8192;

/// Ollama `/api/ps`: the context the loaded `model` runs with.
fn ollama_loaded_ctx(root: &str, model: &str, key: &str, ms: u64) -> Option<usize> {
    let v = ok_json(&get(&agent(ms), &format!("{root}/api/ps"), key))?.clone();
    v["models"].as_array()?.iter().find(|m| m["name"] == model || m["model"] == model)?["context_length"].as_u64().map(|x| x as usize)
}

/// Ollama `/api/show` of `model`.
pub fn ollama_show(root: &str, model: &str, key: &str, ms: u64) -> Option<(Vec<String>, Option<usize>, Option<String>)> {
    let mut rq = agent(ms.max(3000)).post(format!("{}/api/show", root.trim_end_matches('/'))).header("content-type", "application/json");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let mut r = rq.send(json!({"model": model}).to_string()).ok()?;
    if !r.status().is_success() { return None; }
    parse_ollama_show(&serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()?)
}

/// The restore of a file that does not exist: a server without `--slot-save-path` answers 501 "not supported"; one
/// with it fails on the missing file (and touches no slot).
fn slot_save_supported(a: &ureq::Agent, root: &str, key: &str) -> bool {
    let mut rq = a.post(format!("{root}/slots/0?action=restore")).header("content-type", "application/json");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let Ok(mut r) = rq.send(json!({"filename": "elisa-probe-none.bin"}).to_string()) else { return false };
    let st = r.status().as_u16();
    let body = r.body_mut().read_to_string().unwrap_or_default();
    !(st == 501 || body.contains("not_supported") || st == 404)
}

/// The `/model` lines about the backend.
pub fn lines(c: &Caps, max_ctx: usize) -> Vec<String> {
    let mut out = vec![format!("backend: {}{} · detected by {}", c.kind.label(), c.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default(), c.detected_by)];
    let ctx = match c.ctx {
        Some(n) => format!("server window {n} ({}){}", c.ctx_from, if n != max_ctx { format!(" · Elisa uses {max_ctx}") } else { String::new() }),
        None => format!("the server reports no window · Elisa uses {max_ctx}"),
    };
    out.push(format!("  context: {ctx}"));
    let vis = match c.vision { Some(true) => "yes", Some(false) => "no", None => "not reported" };
    let tools = match c.tools { Tools::Native => "native", Tools::Text => "described in the prompt, calls read from the text (no native tools)" };
    out.push(format!("  vision: {vis} · tools: {tools}{}", if c.thinking { " · reasons (thinking)" } else { "" }));
    let usage = match (c.usage_prompt, c.usage_cached) {
        (true, true) => "prompt and reused tokens",
        (true, false) => "prompt tokens (reuse not reported)",
        _ => "none known: Elisa's own estimate, marked ~",
    };
    out.push(format!("  usage reported: {usage}"));
    let cache = match c.cache {
        Cache::None => "none: every request is read in full (no stall timeout: a long prefill is waited for)".to_string(),
        Cache::Automatic => "the server's automatic prefix reuse; Elisa keeps the request prefix stable".to_string(),
        Cache::OllamaKeepAlive => "Ollama keeps the last prompt while the model is loaded (keep_alive: your setting, unchanged)".to_string(),
        Cache::LlamaCpp => format!("cache_prompt on every request · slot save/restore: {}", if c.slot_save { format!("on (slot 0 of {}; saved on /clear, quit and session switch, restored on /resume and --continue)", c.slots.max(1)) } else { "off (start llama-server with --slot-save-path to keep a session's KV across restarts)".into() }),
        Cache::AnthropicBreakpoints => "cache_control breakpoints Elisa places: system+tools, the previous turn, the newest message".to_string(),
        Cache::GeminiExplicit => "implicit caching; an explicit cachedContents entry for the system prompt and tools when long and reused enough to be cheaper".to_string(),
        Cache::Gateway => "the gateway's snapshots and warm-ups".to_string(),
    };
    out.push(format!("  cache: {cache}"));
    if !c.quirks.is_empty() { out.push(format!("  handled: {}", c.quirks.join(" · "))); }
    out
}

// ---------- context accounting ----------

/// The context a reply leaves, and whether it is Elisa's estimate. `reported` = the prompt tokens the server said it
/// read, prefilled + reused (0 = none), `estimate` = Elisa's own count of the request just sent. Counts that mean
/// something else are made whole where they are parsed (llama.cpp `timings`: prompt_n + cache_n; Anthropic: input +
/// cache write + cache read; Ollama: `prompt_eval_count` is the whole prompt), so a nonzero report is the server's
/// real count and wins: Elisa's text estimate is coarser than any tokenizer.
pub fn ctx_from(reported: usize, estimate: usize) -> (usize, bool) {
    if reported == 0 { (estimate, true) } else { (reported, false) }
}

// ---------- tools without native tool calling ----------

/// The system prompt's addition when the tools are described in text (`Tools::Text`).
pub fn text_tools_prompt(tools: &[ToolDef]) -> String {
    let mut s = String::from("\n\n## Tools\nThis server has no native tool calling. To call a tool, write exactly one line per call and nothing \
        else on it:\n<tool_call>{\"name\": \"TOOL\", \"arguments\": {…}}</tool_call>\nThe result comes back in the next message. Tools:\n");
    for t in tools { s += &format!("- {}: {} · arguments schema: {}\n", t.name, t.description.lines().next().unwrap_or(""), t.input_schema); }
    s
}

/// Tool calls written as text (`<tool_call>{…}</tool_call>`, or a bare `{"name": …, "arguments": …}` line) for tools
/// that exist: the text without them, and the calls. A name that is not a tool stays text.
pub fn text_calls(text: &str, tools: &[ToolDef]) -> (String, Vec<(String, Value)>) {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?ms)<tool_call>\s*(\{.*?\})\s*</tool_call>|^[ \t]*(\{[ \t]*\x22name\x22[ \t]*:[^\n]*\})[ \t]*$").unwrap());
    let known = |n: &str| tools.iter().any(|t| t.name == n);
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut last = 0;
    for m in re.captures_iter(text) {
        let whole = m.get(0).unwrap();
        let js = m.get(1).or(m.get(2)).map_or("", |x| x.as_str());
        let Ok(v) = serde_json::from_str::<Value>(js) else { continue };
        let Some(name) = v["name"].as_str().filter(|n| known(n)) else { continue };
        let args = match &v["arguments"] { Value::Object(_) => v["arguments"].clone(), Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})), _ => v.get("input").cloned().unwrap_or(json!({})) };
        rest.push_str(&text[last..whole.start()]);
        last = whole.end();
        calls.push((name.to_string(), args));
    }
    rest.push_str(&text[last..]);
    (rest.trim().to_string(), calls)
}

/// History for a server without native tools: a call becomes its `<tool_call>` text, a result a user message.
pub fn textify(msgs: &[Message]) -> Vec<Message> {
    msgs.iter().map(|m| {
        let content = m.content.iter().map(|b| match b {
            Block::ToolUse { name, input, .. } => Block::Text { text: format!("<tool_call>{}</tool_call>", json!({"name": name, "arguments": input})) },
            Block::ToolResult { content, is_error, .. } => Block::Text { text: format!("<tool_result{}>\n{content}\n</tool_result>", if *is_error { " error" } else { "" }) },
            b => b.clone(),
        }).collect();
        Message { role: m.role, content, id: m.id.clone(), model: m.model.clone() }
    }).collect()
}

// ---------- Mistral ----------

/// Mistral accepts only tool call ids of 9 letters and digits: any other id maps to one, the same every time.
pub fn mistral_id(id: &str) -> String {
    if id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric()) { return id.to_string(); }
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in id.as_bytes() { h ^= x as u64; h = h.wrapping_mul(0x100000001b3); }
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..9).map(|_| { let c = A[(h % 62) as usize] as char; h /= 62; if h == 0 { h = 0x9e3779b97f4a7c15; } c }).collect()
}

// ---------- Anthropic cache_control ----------

/// Elisa's breakpoints on an Anthropic Messages body (at most 4 allowed; 3 used): the system prompt (the cache covers
/// tools → system, both fixed for the session), the last block of the newest message (the whole conversation so far,
/// read back by the next step) and the last block of the user message before it (still a hit when the newest
/// message changed, e.g. after a retry). Below the model's minimum (1024 / 2048 tokens) Anthropic ignores them at no cost.
pub fn anthropic_breakpoints(body: &mut Value) {
    let cc = json!({"type": "ephemeral"});
    if let Some(s) = body["system"].as_str().filter(|s| !s.is_empty()).map(String::from) {
        body["system"] = json!([{"type": "text", "text": s, "cache_control": cc}]);
    } else if body["system"].as_str() == Some("") {
        if let Some(t) = body["tools"].as_array_mut().and_then(|t| t.last_mut()) { t["cache_control"] = cc.clone(); }
    }
    let Some(msgs) = body["messages"].as_array_mut() else { return };
    let n = msgs.len();
    let mut marks = Vec::new();
    if n > 0 { marks.push(n - 1); }
    if let Some(i) = (0..n.saturating_sub(1)).rev().find(|&i| msgs[i]["role"] == "user") { marks.push(i); }
    for i in marks {
        let m = &mut msgs[i];
        if let Some(t) = m["content"].as_str().map(String::from) { m["content"] = json!([{"type": "text", "text": t}]); }
        // a thinking block cannot carry cache_control: the last other block does
        if let Some(b) = m["content"].as_array_mut().and_then(|c| c.iter_mut().rev().find(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking")) {
            b["cache_control"] = cc.clone();
        }
    }
}

// ---------- llama.cpp slot save / restore ----------

/// How many slot files Elisa keeps per project on a llama.cpp server: the names are reused in turn, so the server's
/// slot dir never grows past this many of Elisa's files (it has no delete action).
pub const SLOT_FILES: usize = 4;
pub const SLOT_MAP: &str = "slots.json";

/// POST `{root}/slots/{id}?action=save|restore` with `filename`: Ok(tokens saved / restored, ms).
pub fn slot_action(root: &str, key: &str, slot: usize, action: &str, filename: &str) -> Result<(usize, f64), String> {
    let a: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(120))).build().into();
    let mut rq = a.post(format!("{}/slots/{slot}?action={action}", root.trim_end_matches('/'))).header("content-type", "application/json");
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let mut r = rq.send(json!({"filename": filename}).to_string()).map_err(|e| format!("slot {action}: {e}"))?;
    let st = r.status().as_u16();
    let t = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&st) { return Err(format!("slot {action}: HTTP {st}: {}", crate::api::err_message(&t))); }
    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
    let n = v["n_saved"].as_u64().or_else(|| v["n_restored"].as_u64()).unwrap_or(0) as usize;
    let ms = v["timings"]["save_ms"].as_f64().or_else(|| v["timings"]["restore_ms"].as_f64()).unwrap_or(0.0);
    Ok((n, ms))
}

/// Which session's KV is in which slot file (`.elisa/slots.json`): session id → {file, server, model, tokens, at}.
#[derive(Default, Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlotMap { pub entries: std::collections::BTreeMap<String, SlotEntry>, #[serde(default)] pub next: usize }

#[derive(Default, Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlotEntry { pub file: String, pub server: String, pub model: String, pub tokens: usize, pub at: u64 }

impl SlotMap {
    pub fn load(dir: &Path) -> SlotMap {
        std::fs::read_to_string(dir.join(SLOT_MAP)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let p: PathBuf = dir.join(SLOT_MAP);
        let tmp = dir.join(format!(".{SLOT_MAP}.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }
    /// The file for `session` on this server: its own if it has one, else the next name in the ring (the session that
    /// had it loses its entry).
    pub fn file_for(&mut self, session: &str, server: &str, project: &str) -> String {
        if let Some(e) = self.entries.get(session).filter(|e| e.server == server) { return e.file.clone(); }
        let file = format!("elisa-{project}-{}.bin", self.next % SLOT_FILES);
        self.next = (self.next + 1) % SLOT_FILES;
        self.entries.retain(|_, e| !(e.file == file && e.server == server));
        file
    }
    /// The saved entry for `session` on this server and model.
    pub fn find(&self, session: &str, server: &str, model: &str) -> Option<&SlotEntry> {
        self.entries.get(session).filter(|e| e.server == server && e.model == model)
    }
}

// ---------- Gemini explicit context caching ----------

/// Gemini's minimum for a cache (tokens): 2.5 Flash / Pro 2,048, newer models 4,096
/// (https://ai.google.dev/gemini-api/docs/caching); unknown models get the larger one. Elisa also wants at least `GEMINI_MIN_ELISA` so the storage it pays for is worth it.
pub fn gemini_min_tokens(model: &str) -> usize {
    let m = model.to_ascii_lowercase();
    if m.contains("2.5") { 2048 } else { 4096 }
}
pub const GEMINI_MIN_ELISA: usize = 4096;
/// the explicit cache's lifetime
pub const GEMINI_TTL_S: u64 = 300;
/// calls within `GEMINI_WINDOW_S` with the same prefix before one is created (a burst: an agent turn's steps)
pub const GEMINI_REUSES: usize = 3;
pub const GEMINI_WINDOW_S: u64 = 300;

/// The explicit cache of one session's system prompt + tools.
#[derive(Default, Debug, Clone)]
pub struct GeminiCache {
    /// the prefix hash and `cachedContents/…` name in use, and when it expires
    pub active: Option<(u64, String, std::time::Instant)>,
    /// recent calls' (prefix hash, when)
    pub recent: Vec<(u64, std::time::Instant)>,
    /// creation failed for this prefix: not tried again
    pub failed: Option<u64>,
}

pub fn prefix_hash(system: &str, tools: &[ToolDef]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let s = format!("{system}\u{0}{}", serde_json::to_string(tools).unwrap_or_default());
    for &x in s.as_bytes() { h ^= x as u64; h = h.wrapping_mul(0x100000001b3); }
    h
}

impl GeminiCache {
    /// Should this call use (or create) the explicit cache? Some(name) = use it; Err(hash) = create one for this hash
    /// first; Ok(None) = send the prefix as usual. Cheaper only when the prefix is long (`prefix_tokens` ≥ the model's
    /// minimum and `GEMINI_MIN_ELISA`) and read again often within its lifetime (`GEMINI_REUSES` calls in
    /// `GEMINI_WINDOW_S`): then the cached reads (a quarter of the price or less) outweigh one hour's storage of it
    /// pro rata. The implicit cache of Gemini 2.5+ needs nothing from Elisa and stays the default.
    pub fn decide(&mut self, hash: u64, prefix_tokens: usize, model: &str, now: std::time::Instant) -> Result<Option<String>, u64> {
        self.recent.retain(|(_, t)| now.duration_since(*t).as_secs() < GEMINI_WINDOW_S);
        self.recent.push((hash, now));
        if let Some((h, name, until)) = &self.active {
            if *h == hash && now < *until { return Ok(Some(name.clone())); }
        }
        self.active = None;
        let long = prefix_tokens >= gemini_min_tokens(model).max(GEMINI_MIN_ELISA);
        let hot = self.recent.iter().filter(|(h, _)| *h == hash).count() >= GEMINI_REUSES;
        if long && hot && self.failed != Some(hash) { Err(hash) } else { Ok(None) }
    }
}

/// The native API root for a Gemini base URL (`…/v1beta/openai` → `…/v1beta`).
pub fn gemini_native(base_url: &str) -> String {
    let b = base_url.trim_end_matches('/');
    b.strip_suffix("/openai").unwrap_or(b).to_string()
}

/// POST `{native}/cachedContents`: the system prompt and tools, `GEMINI_TTL_S`. Ok(name).
pub fn gemini_create_cache(base_url: &str, key: &str, model: &str, system: &str, tools: &[ToolDef]) -> Result<String, String> {
    let decls: Vec<Value> = tools.iter().map(|t| json!({"name": t.name, "description": t.description, "parametersJsonSchema": t.input_schema})).collect();
    let mut body = json!({
        "model": format!("models/{}", model.trim_start_matches("models/")),
        "systemInstruction": {"parts": [{"text": system}]},
        "ttl": format!("{GEMINI_TTL_S}s"),
    });
    if !decls.is_empty() { body["tools"] = json!([{"functionDeclarations": decls}]); }
    let a: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut r = a.post(format!("{}/cachedContents", gemini_native(base_url))).header("content-type", "application/json")
        .header("x-goog-api-key", key).send(body.to_string()).map_err(|e| format!("cachedContents: {e}"))?;
    let st = r.status().as_u16();
    let t = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&st) { return Err(format!("cachedContents: HTTP {st}: {}", crate::api::err_message(&t))); }
    let v: Value = serde_json::from_str(&t).map_err(|e| format!("cachedContents: {e}"))?;
    v["name"].as_str().map(String::from).ok_or_else(|| "cachedContents: no name".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;

    #[test]
    fn cloud_hosts_are_known_by_name() {
        assert_eq!(host_kind("https://api.openai.com/v1"), Some(Kind::OpenAi));
        assert_eq!(host_kind("https://generativelanguage.googleapis.com/v1beta/openai/"), Some(Kind::Gemini));
        assert_eq!(host_kind("https://openrouter.ai/api/v1"), Some(Kind::OpenRouter));
        assert_eq!(host_kind("https://api.groq.com/openai/v1"), Some(Kind::Groq));
        assert_eq!(host_kind("https://api.together.xyz/v1"), Some(Kind::Together));
        assert_eq!(host_kind("https://api.deepseek.com"), Some(Kind::DeepSeek));
        assert_eq!(host_kind("https://api.mistral.ai/v1"), Some(Kind::Mistral));
        assert_eq!(host_kind("http://127.0.0.1:8080/v1"), None);
        assert_eq!(host_kind("https://evil-api.openai.com.example/v1"), None);
    }

    #[test]
    fn server_answers_parse() {
        let p = json!({"default_generation_settings": {"n_ctx": 16384, "params": {}}, "total_slots": 2, "modalities": {"vision": false}, "chat_template_caps": {"supports_tools": true}});
        assert_eq!(parse_llamacpp_props(&p), Some((Some(16384), 2, Some(false), Some(true))));
        assert_eq!(parse_llamacpp_props(&json!({"version": "x"})), None);
        let s = json!({"capabilities": ["completion", "vision"], "model_info": {"qwen3vl.context_length": 262144}});
        assert_eq!(parse_ollama_show(&s), Some((vec!["completion".into(), "vision".into()], Some(262144), None)));
        let l = json!({"object": "list", "data": [{"id": "a", "type": "llm", "state": "not-loaded", "max_context_length": 8192},
            {"id": "qwen2-vl", "type": "vlm", "state": "loaded", "max_context_length": 32768, "loaded_context_length": 4096, "capabilities": ["tool_use"]}]});
        assert_eq!(parse_lmstudio(&l, "qwen2-vl"), Some((Some(4096), true, Some(true), Some("loaded".into()))));
        assert_eq!(parse_lmstudio(&l, "nope").unwrap().0, Some(4096), "the loaded model when the name is not listed");
        assert_eq!(parse_lmstudio(&json!({"data": [{"id": "x", "owned_by": "vllm"}]}), "x"), None);
        let v = json!({"object": "list", "data": [{"id": "m", "object": "model", "owned_by": "vllm", "max_model_len": 32768}]});
        assert_eq!(parse_models_owner(&v, "m"), Some(("vllm".into(), Some(32768))));
    }

    #[test]
    fn a_report_that_means_something_else_gives_way_to_the_estimate() {
        assert_eq!(ctx_from(0, 3000), (3000, true));
        assert_eq!(ctx_from(2900, 3000), (2900, false));
        assert_eq!(ctx_from(300, 3000), (300, false), "the server's tokenizer wins over the text estimate");
    }

    #[test]
    fn text_tool_calls_are_read_back_only_for_real_tools() {
        let tools = [ToolDef { name: "read".into(), description: "Read a file".into(), input_schema: json!({"type": "object"}) }];
        let (rest, calls) = text_calls("Let me look.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"README.md\"}}</tool_call>", &tools);
        assert_eq!(rest, "Let me look.");
        assert_eq!(calls, vec![("read".to_string(), json!({"path": "README.md"}))]);
        let (_, calls) = text_calls("{\"name\": \"read\", \"arguments\": \"{\\\"path\\\": \\\"a\\\"}\"}", &tools);
        assert_eq!(calls[0].1, json!({"path": "a"}));
        let (rest, calls) = text_calls("<tool_call>{\"name\": \"rm\", \"arguments\": {}}</tool_call>", &tools);
        assert!(calls.is_empty() && rest.contains("rm"));
        assert!(text_tools_prompt(&tools).contains("- read: Read a file"));
        let h = textify(&[Message::new(Role::Assistant, vec![Block::ToolUse { id: "1".into(), name: "read".into(), input: json!({"path": "a"}) }])]);
        assert_eq!(h[0].content, vec![Block::Text { text: "<tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"a\"}}</tool_call>".into() }]);
    }

    #[test]
    fn mistral_ids_are_nine_alphanumerics_and_stable() {
        let a = mistral_id("toolu_01AbCdEf");
        assert_eq!(a.len(), 9);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(a, mistral_id("toolu_01AbCdEf"));
        assert_ne!(a, mistral_id("call_0"));
        assert_eq!(mistral_id("Ab3dE6gH9"), "Ab3dE6gH9");
    }

    #[test]
    fn anthropic_breakpoints_go_on_system_previous_turn_and_newest_message() {
        let mut b = json!({"system": "sys", "tools": [{"name": "a"}], "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "text", "text": "x"}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "r"}]},
        ]});
        anthropic_breakpoints(&mut b);
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["messages"][2]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["messages"][0]["content"][0], json!({"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}));
        assert!(b["messages"][1]["content"][0].get("cache_control").is_none());
        let n = b.to_string().matches("cache_control").count();
        assert!(n <= 4, "Anthropic allows at most 4 breakpoints: {n}");
    }

    #[test]
    fn slot_files_are_a_ring_per_server() {
        let mut m = SlotMap::default();
        let srv = "http://127.0.0.1:1";
        let f: Vec<String> = (0..SLOT_FILES + 1).map(|i| {
            let f = m.file_for(&format!("s{i}"), srv, "p");
            m.entries.insert(format!("s{i}"), SlotEntry { file: f.clone(), server: srv.into(), model: "m".into(), tokens: 1, at: 0 });
            f
        }).collect();
        assert_eq!(f[0], f[SLOT_FILES], "the oldest name is reused");
        assert!(m.find("s0", srv, "m").is_none(), "its old session lost the entry");
        assert!(m.find("s1", srv, "m").is_some());
        assert_eq!(m.file_for("s1", srv, "p"), f[1], "a session keeps its own file");
    }

    #[test]
    fn gemini_caches_only_a_long_hot_prefix() {
        let mut g = GeminiCache::default();
        let t = std::time::Instant::now();
        assert_eq!(g.decide(1, 10_000, "gemini-2.5-pro", t), Ok(None));
        assert_eq!(g.decide(1, 10_000, "gemini-2.5-pro", t), Ok(None));
        assert_eq!(g.decide(1, 10_000, "gemini-2.5-pro", t), Err(1), "third call in the window: create");
        g.active = Some((1, "cachedContents/x".into(), t + Duration::from_secs(GEMINI_TTL_S)));
        assert_eq!(g.decide(1, 10_000, "gemini-2.5-pro", t), Ok(Some("cachedContents/x".into())));
        assert_eq!(g.decide(2, 10_000, "gemini-2.5-pro", t), Ok(None), "a new prefix drops the old cache");
        let mut s = GeminiCache::default();
        for _ in 0..5 { assert_eq!(s.decide(3, 900, "gemini-2.5-flash", t), Ok(None), "short: never"); }
        assert_eq!(gemini_native("https://generativelanguage.googleapis.com/v1beta/openai/"), "https://generativelanguage.googleapis.com/v1beta");
    }
}
