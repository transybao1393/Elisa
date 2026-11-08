//! Model connections: `~/.config/elisa/providers.toml`, the one place that says which model Elisa talks to and how.
//!
//! ```toml
//! active = "ollama"
//!
//! [providers.ollama]            # type: streamdense | anthropic | openai | mcp
//! type = "openai"
//! base_url = "http://127.0.0.1:11434/v1"
//! model = "qwen3:8b"
//! max_ctx = 8192
//! ```
//!
//! Only the user's home holds this file, and `.elisa/config.toml` has no key for it: a repository (or the model, which
//! can write `.elisa/`) can neither add a connection nor choose one, so it cannot send the project or a key elsewhere.
//! The app's window (`app/ElisaApp/.../Connections.swift`) reads and writes it through `--providers-json`,
//! `--providers-save` and `--provider-test`, and a saved file is mode 600. The key itself never leaves this module on
//! those commands: the JSON says only `has_key`.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STREAMDENSE: &str = "streamdense";
pub const ANTHROPIC: &str = "anthropic";
pub const OPENAI: &str = "openai";
pub const MCP: &str = "mcp";

/// `--provider none`: ignore `active`, use `config.toml` and its environment as if this file did not exist
pub const NONE: &str = "none";

/// How requests reach the model. `Gateway` (the default, also with no providers file) keeps every streamdense extra:
/// `/health`, warm-ups, the watcher, the project header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Gateway,
    Anthropic,
    OpenAi,
    Mcp(McpTarget),
}

/// An MCP server serving a model: a local command (stdio) or a URL (streamable HTTP).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpTarget { pub command: Vec<String>, pub url: String, pub tool: String }

fn empty(s: &str) -> bool { s.is_empty() }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "empty")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "empty")]
    pub model: String,
    /// the name of the environment variable holding the key (preferred; wins over `api_key`)
    #[serde(default, skip_serializing_if = "empty")]
    pub api_key_env: String,
    #[serde(default, skip_serializing_if = "empty")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ctx: Option<usize>,
    /// mcp: the server as a command line, program first (stdio)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// mcp: the server's URL (streamable HTTP)
    #[serde(default, skip_serializing_if = "empty")]
    pub url: String,
    /// mcp: the tool that runs the model; "" = the server's only tool
    #[serde(default, skip_serializing_if = "empty")]
    pub tool: String,
    /// the model can see pictures: true / false, or absent = work it out (Anthropic yes, Ollama asks the server, else no)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// old connection-level web setting: still read and saved so files load, but ignored since 2026-11-08 (web access
    /// is config.toml `web`, and each call asks in the chat): "" = off, "duckduckgo" (no key) or "searxng"
    #[serde(default, skip_serializing_if = "empty")]
    pub web: String,
    /// the SearXNG server for `web = "searxng"`
    #[serde(default, skip_serializing_if = "empty")]
    pub searxng_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    #[serde(default, skip_serializing_if = "empty")]
    pub active: String,
    #[serde(default)]
    pub providers: BTreeMap<String, Profile>,
}

pub fn path_in(home: &Path) -> PathBuf { home.join(".config/elisa/providers.toml") }

fn home() -> Option<PathBuf> { std::env::var_os("HOME").map(PathBuf::from) }

/// host of an http(s) URL, without port or user
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |x| x.1);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    if let Some(v6) = auth.strip_prefix('[') { return v6.split(']').next().unwrap_or(v6); }
    auth.split(':').next().unwrap_or(auth)
}

/// the URL's host is this machine
pub fn is_local(url: &str) -> bool {
    let h = host(url);
    h == "localhost" || h == "::1" || h == "127.0.0.1" || h.starts_with("127.")
}

impl Profile {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        let bad = |m: String| Err(format!("provider {name}: {m}"));
        let http = |u: &str| u.starts_with("http://") || u.starts_with("https://");
        match self.kind.as_str() {
            STREAMDENSE | ANTHROPIC | OPENAI => {
                if !http(&self.base_url) { return bad(format!("base_url must start with http:// or https:// (got {:?})", self.base_url)); }
                if self.model.is_empty() { return bad("model is required".into()); }
                if !self.command.is_empty() || !self.url.is_empty() || !self.tool.is_empty() { return bad(format!("command, url and tool are only for type = \"{MCP}\"")); }
                if self.kind == OPENAI && self.max_ctx.unwrap_or(0) == 0 { return bad("max_ctx is required for type = \"openai\" (the API does not report the model's window)".into()); }
            }
            MCP => {
                if self.command.is_empty() == self.url.is_empty() { return bad("set exactly one of command (stdio) or url (HTTP)".into()); }
                if !self.url.is_empty() && !http(&self.url) { return bad(format!("url must start with http:// or https:// (got {:?})", self.url)); }
                if self.command.first().is_some_and(|c| c.is_empty()) { return bad("command is empty".into()); }
                if !self.base_url.is_empty() { return bad("base_url is not for type = \"mcp\": use url or command".into()); }
                if self.max_ctx.unwrap_or(0) == 0 { return bad("max_ctx is required for type = \"mcp\"".into()); }
            }
            k => return bad(format!("type = {k:?}: use \"{STREAMDENSE}\", \"{ANTHROPIC}\", \"{OPENAI}\" or \"{MCP}\"")),
        }
        match self.web.as_str() {
            "" | crate::web::DUCKDUCKGO => {}
            crate::web::SEARXNG => if !http(&self.searxng_url) { return bad(format!("web = \"searxng\" needs searxng_url starting with http:// or https:// (got {:?})", self.searxng_url)); },
            w => return bad(format!("web = {w:?}: use \"{}\" or \"{}\", or leave it out", crate::web::DUCKDUCKGO, crate::web::SEARXNG)),
        }
        Ok(())
    }

    /// The key to send: `api_key_env`'s value, else `api_key`, else "". A named variable that is unset is an error, and
    /// so is no key at all for the Anthropic API off this machine. Generic `ANTHROPIC_API_KEY` is never picked up by
    /// itself: it must not reach another provider.
    pub fn key(&self, name: &str, getenv: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
        let k = if self.api_key_env.is_empty() { self.api_key.clone() } else {
            match getenv(&self.api_key_env).filter(|v| !v.is_empty()) {
                Some(v) => v,
                None if !self.api_key.is_empty() => self.api_key.clone(),
                None => return Err(format!("provider {name}: the environment variable {} is not set", self.api_key_env)),
            }
        };
        if k.is_empty() && self.kind == ANTHROPIC && !is_local(&self.base_url) {
            return Err(format!("provider {name}: {} needs an API key (api_key_env or api_key)", host(&self.base_url)));
        }
        Ok(k)
    }

    pub fn transport(&self) -> Transport {
        match self.kind.as_str() {
            ANTHROPIC => Transport::Anthropic,
            OPENAI => Transport::OpenAi,
            MCP => Transport::Mcp(McpTarget { command: self.command.clone(), url: self.url.clone(), tool: self.tool.clone() }),
            _ => Transport::Gateway,
        }
    }

    /// what the banner shows as the model's address
    pub fn address(&self) -> String {
        if self.kind != MCP { return self.base_url.trim_end_matches('/').to_string(); }
        if self.url.is_empty() { format!("mcp:{}", self.command.first().map_or("", String::as_str)) } else { format!("mcp:{}", self.url) }
    }

    /// Where the conversation goes: None = stays on this machine; Some(host) = it leaves it. A stdio MCP server runs
    /// here, but what it does with the text is its own business, so it counts as leaving unless stated otherwise.
    pub fn leaves_machine(&self) -> Option<String> {
        match self.kind.as_str() {
            MCP if self.url.is_empty() => Some(format!("the MCP server `{}`", self.command.first().map_or("", String::as_str))),
            MCP if is_local(&self.url) => Some(format!("the MCP server at {}", self.url)),
            MCP => Some(host(&self.url).to_string()),
            _ if is_local(&self.base_url) => None,
            _ => Some(host(&self.base_url).to_string()),
        }
    }
}

impl Providers {
    pub fn parse(text: &str, origin: &str) -> Result<Providers, String> {
        let f: Providers = toml::from_str(text).map_err(|e| format!("{origin}: {e}"))?;
        f.validate().map_err(|e| format!("{origin}: {e}"))?;
        Ok(f)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (n, p) in &self.providers {
            let ok = !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !ok { return Err(format!("provider name {n:?}: use letters, digits, - and _")); }
            if n == NONE { return Err(format!("provider name {NONE:?} is reserved")); }
            p.validate(n)?;
        }
        if !self.active.is_empty() && !self.providers.contains_key(&self.active) {
            return Err(format!("active = {:?}: no such provider", self.active));
        }
        Ok(())
    }

    /// The file under `home`, or the empty set when there is none.
    pub fn load(home: &Path) -> Result<Providers, String> {
        let p = path_in(home);
        if !p.is_file() { return Ok(Providers::default()); }
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Providers::parse(&s, &p.display().to_string())
    }

    /// Atomic write, mode 600 (the file may hold keys).
    pub fn save(&self, home: &Path) -> Result<PathBuf, String> {
        self.validate()?;
        let p = path_in(home);
        let dir = p.parent().ok_or("no parent dir")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let body = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let text = format!("# Elisa's model connections. The app's Model Connections window rewrites this file (comments are not kept).\n# type: streamdense | anthropic | openai | mcp. Prefer api_key_env to api_key.\n\n{body}");
        let tmp = dir.join(format!(".providers.toml.{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &p)
        };
        write().map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("{}: {e}", p.display()) })?;
        Ok(p)
    }
}

fn entry(name: &str, p: &Profile, getenv: &dyn Fn(&str) -> Option<String>) -> Value {
    json!({
        "name": name, "type": p.kind, "base_url": p.base_url, "model": p.model, "api_key_env": p.api_key_env,
        "has_key": !p.api_key.is_empty(), "env_set": !p.api_key_env.is_empty() && getenv(&p.api_key_env).is_some_and(|v| !v.is_empty()),
        "max_ctx": p.max_ctx, "command": p.command, "url": p.url, "tool": p.tool, "vision": p.vision, "web": p.web, "searxng_url": p.searxng_url, "leaves": p.leaves_machine(),
    })
}

/// `--providers-json`: everything but the keys.
pub fn to_json(f: &Providers, path: &Path, getenv: &dyn Fn(&str) -> Option<String>) -> Value {
    json!({ "path": path.display().to_string(), "active": f.active,
        "providers": f.providers.iter().map(|(n, p)| entry(n, p, getenv)).collect::<Vec<_>>() })
}

fn str_of(v: &Value, k: &str) -> String { v[k].as_str().unwrap_or("").trim().to_string() }

/// One provider from the window's JSON. `api_key` absent = keep the saved key of `was` (or `name`); "" = remove it.
fn profile_from(v: &Value, old: &Providers) -> (String, Profile) {
    let name = str_of(v, "name");
    let was = Some(str_of(v, "was")).filter(|w| !w.is_empty()).unwrap_or_else(|| name.clone());
    let api_key = match v["api_key"].as_str() {
        Some(k) => k.trim().to_string(),
        None => old.providers.get(&was).map(|p| p.api_key.clone()).unwrap_or_default(),
    };
    let command = v["command"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    (name, Profile {
        kind: str_of(v, "type"), base_url: str_of(v, "base_url"), model: str_of(v, "model"), api_key_env: str_of(v, "api_key_env"), api_key,
        max_ctx: v["max_ctx"].as_u64().map(|x| x as usize).filter(|&x| x > 0), command, url: str_of(v, "url"), tool: str_of(v, "tool"), vision: v["vision"].as_bool(),
        web: str_of(v, "web"), searxng_url: str_of(v, "searxng_url"),
    })
}

/// `--providers-save`: the window's whole list replaces the saved one.
pub fn from_json(v: &Value, old: &Providers) -> Result<Providers, String> {
    let mut f = Providers { active: str_of(v, "active"), providers: BTreeMap::new() };
    for e in v["providers"].as_array().ok_or("providers: not a list")? {
        let (n, p) = profile_from(e, old);
        if f.providers.insert(n.clone(), p).is_some() { return Err(format!("two connections are named {n:?}")); }
    }
    f.validate()?;
    Ok(f)
}

fn agent(timeout_s: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(timeout_s))).build().into()
}

/// Whether the model can see pictures. The connection's `vision` setting wins; without one: the Anthropic API always
/// can, an OpenAI-style server is asked the Ollama way (`/api/show` lists "vision" among the model's capabilities; any
/// other server answers nothing and counts as no), and the gateway and MCP count as no.
/// `ollama`: the model's capabilities when the server is Ollama (`ollama_caps`), asked once at startup.
pub fn vision(transport: &Transport, ollama: Option<&[String]>, set: Option<bool>) -> bool {
    if let Some(v) = set { return v; }
    match transport {
        Transport::Anthropic => true,
        Transport::OpenAi => ollama.is_some_and(|c| c.iter().any(|x| x == "vision")),
        Transport::Gateway | Transport::Mcp(_) => false,
    }
}

/// the server without the OpenAI `/v1` path: where Ollama's native API lives
pub fn ollama_root(base_url: &str) -> &str { base_url.trim_end_matches('/').trim_end_matches("/v1") }

/// When an OpenAI-style server is Ollama: the model's capabilities from `/api/show` ("completion", "vision", "tools",
/// "thinking", …). None: not Ollama (any other server has no `/api/show`), or no answer within 3 s.
pub fn ollama_caps(base_url: &str, model: &str) -> Option<Vec<String>> {
    let mut r = agent(3).post(format!("{}/api/show", ollama_root(base_url))).header("content-type", "application/json")
        .send(json!({"model": model}).to_string()).ok()?;
    if !r.status().is_success() { return None; }
    let v: Value = serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()?;
    Some(v["capabilities"].as_array()?.iter().filter_map(|c| c.as_str().map(String::from)).collect())
}

/// One model a server has (`--provider-models`): Ollama gives size, parameter count and capabilities, other
/// OpenAI-style servers (llama.cpp, LM Studio, vLLM, mlx-lm) only the id.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerModel { pub name: String, pub bytes: u64, pub params: String, pub vision: Option<bool> }

/// Ollama's `/api/tags` answer: its models, newest name order as Ollama lists them
pub fn parse_ollama_tags(v: &Value) -> Option<Vec<ServerModel>> {
    Some(v["models"].as_array()?.iter().filter_map(|m| Some(ServerModel {
        name: m["name"].as_str().or_else(|| m["model"].as_str())?.to_string(),
        bytes: m["size"].as_u64().unwrap_or(0),
        params: m["details"]["parameter_size"].as_str().unwrap_or("").to_string(),
        vision: None,
    })).collect())
}

/// An OpenAI-style `/models` answer: `data[].id`
pub fn parse_openai_models(v: &Value) -> Option<Vec<ServerModel>> {
    Some(v["data"].as_array()?.iter().filter_map(|m| m["id"].as_str())
        .map(|id| ServerModel { name: id.to_string(), bytes: 0, params: String::new(), vision: None }).collect())
}

/// The models an OpenAI-style connection's server has: Ollama's own list first (with each model's vision, asked in
/// parallel), else the server's `/models`. Ok((server, models)): server is "ollama" or "openai".
pub fn list_models(p: &Profile, getenv: &dyn Fn(&str) -> Option<String>) -> Result<(String, Vec<ServerModel>), String> {
    if p.kind != OPENAI { return Err("only an OpenAI API connection lists its server's models".into()); }
    let base = p.base_url.trim_end_matches('/');
    if base.is_empty() || !base.contains("://") { return Err("set the server URL first".into()); }
    if let Ok(mut r) = agent(3).get(format!("{}/api/tags", ollama_root(base))).call() {
        let tags = r.body_mut().read_to_string().ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| parse_ollama_tags(&v));
        if let Some(mut ms) = tags {
            let names: Vec<String> = ms.iter().map(|m| m.name.clone()).collect();
            let vis: Vec<Option<bool>> = std::thread::scope(|sc| {
                let hs: Vec<_> = names.iter().map(|n| sc.spawn(move || ollama_caps(base, n))).collect();
                hs.into_iter().map(|h| h.join().ok().flatten().map(|c| c.iter().any(|x| x == "vision"))).collect()
            });
            for (m, v) in ms.iter_mut().zip(vis) { m.vision = v; }
            return Ok(("ollama".into(), ms));
        }
    }
    let key = p.key("draft", getenv).unwrap_or_default();
    let mut rq = agent(8).get(format!("{base}/models"));
    if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
    let mut r = rq.call().map_err(|e| format!("no server answers at {base} ({e}): is it running?"))?;
    let (st, t) = (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default());
    if !(200..300).contains(&st) { return Err(http_err(st, &t)); }
    let v: Value = serde_json::from_str(&t).map_err(|_| format!("{base}/models did not answer with JSON"))?;
    parse_openai_models(&v).map(|m| ("openai".into(), m)).ok_or_else(|| format!("{base}/models has no data list"))
}

fn http_err(status: u16, body: &str) -> String {
    let m = serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from));
    format!("HTTP {status}: {}", m.unwrap_or_else(|| body.chars().take(200).collect()))
}

/// Can this connection be reached? Uses only free calls (`/health`, the model list, MCP `tools/list`): no tokens.
pub fn test(name: &str, p: &Profile, getenv: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    p.validate(name)?;
    let key = p.key(name, getenv)?;
    let base = p.base_url.trim_end_matches('/');
    let a = agent(8);
    match p.kind.as_str() {
        STREAMDENSE => {
            let mut r = a.get(format!("{base}/health")).call().map_err(|e| format!("cannot reach {base}: {e}"))?;
            let (st, t) = (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default());
            if !(200..300).contains(&st) { return Err(http_err(st, &t)); }
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            let server = v["info"]["max_ctx"].as_u64().map(|n| n as usize);
            if let (Some(want), Some(has)) = (p.max_ctx, server) {
                if want > has { return Err(format!("connected, but the gateway runs with context {has} and this connection asks for {want}. Restart the gateway with --max-ctx {want}, or choose a smaller size. Until then Elisa uses {has}.")); }
            }
            Ok(format!("connected · gateway model {} · context {}", v["model"].as_str().unwrap_or("?"), server.map_or("?".into(), |n| n.to_string())))
        }
        ANTHROPIC | OPENAI => {
            let url = if p.kind == ANTHROPIC { format!("{base}/v1/models") } else { format!("{base}/models") };
            let mut rq = a.get(&url);
            if p.kind == ANTHROPIC { rq = rq.header("x-api-key", &key).header("anthropic-version", "2023-06-01"); }
            else if !key.is_empty() { rq = rq.header("authorization", format!("Bearer {key}")); }
            let mut r = rq.call().map_err(|e| format!("cannot reach {base}: {e}"))?;
            let (st, t) = (r.status().as_u16(), r.body_mut().read_to_string().unwrap_or_default());
            if !(200..300).contains(&st) { return Err(http_err(st, &t)); }
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            let ids: Vec<&str> = v["data"].as_array().map(|d| d.iter().filter_map(|m| m["id"].as_str()).collect()).unwrap_or_default();
            if ids.contains(&p.model.as_str()) { return Ok(format!("connected · {} is available", p.model)); }
            if ids.is_empty() { return Ok("connected · the server lists no models".into()); }
            let some = ids.iter().take(5).copied().collect::<Vec<_>>().join(", ");
            Err(format!("connected, but {} is not listed. The server has: {some}{}", p.model, if ids.len() > 5 { ", …" } else { "" }))
        }
        _ => crate::mcp::probe(&McpTarget { command: p.command.clone(), url: p.url.clone(), tool: p.tool.clone() }, &key),
    }
}

/// The lines of `/model`.
pub fn summary(active: &str, f: &Providers, path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if f.providers.is_empty() { out.push(format!("no connections saved ({} does not list any)", path.display())); }
    else { out.push(format!("connections in {}:", path.display())); }
    for (n, p) in &f.providers {
        let mark = if n == active { "●" } else { "○" };
        let place = p.leaves_machine().map_or("this machine".to_string(), |h| format!("sends to {h}"));
        out.push(format!("{mark} {n} · {} · {} · {place}", p.kind, if p.model.is_empty() { p.address() } else { p.model.clone() }));
    }
    out.push("change: elisa --provider NAME, ELISA_PROVIDER, or Elisa > Model Connections… in the app (restarts the session)".into());
    out
}

/// `--providers-json`, `--providers-save`, `--provider-test`, `--provider-models` (the draft on stdin): None = not one of these, Some(exit code) = done.
pub fn cli(args: &[String]) -> Option<i32> {
    let (cmd, val) = (args.first()?.as_str(), args.get(1).map(String::as_str));
    if !matches!(cmd, "--providers-json" | "--providers-save" | "--provider-test" | "--provider-models") { return None; }
    let fail = |e: String| { println!("{}", json!({"ok": false, "detail": e})); Some(1) };
    let Some(home) = home() else { return fail("HOME is not set, so there is no ~/.config/elisa/providers.toml".into()) };
    let env = |k: &str| std::env::var(k).ok();
    let old = match Providers::load(&home) { Ok(f) => f, Err(e) => return fail(e) };
    let stdin = || -> Result<Value, String> {
        serde_json::from_reader(std::io::stdin()).map_err(|e| format!("stdin: {e}"))
    };
    match cmd {
        "--providers-json" => { println!("{}", to_json(&old, &path_in(&home), &env)); Some(0) }
        "--providers-save" => match stdin().and_then(|v| from_json(&v, &old)).and_then(|f| f.save(&home)) {
            Ok(p) => { println!("{}", json!({"ok": true, "detail": format!("saved {}", p.display())})); Some(0) }
            Err(e) => fail(e),
        },
        "--provider-models" => {
            let (_, p) = match stdin() { Ok(v) => profile_from(&v, &old), Err(e) => return fail(e) };
            match list_models(&p, &env) {
                Ok((server, ms)) => {
                    let models: Vec<Value> = ms.iter().map(|m| json!({"name": m.name, "bytes": m.bytes, "params": m.params, "vision": m.vision})).collect();
                    println!("{}", json!({"ok": true, "server": server, "models": models}));
                    Some(0)
                }
                Err(e) => fail(e),
            }
        }
        _ => {
            let picked = match val {
                None | Some("-") => stdin().map(|v| profile_from(&v, &old)),
                Some(n) => old.providers.get(n).cloned().map(|p| (n.to_string(), p)).ok_or_else(|| format!("no provider named {n}")),
            };
            match picked.and_then(|(n, p)| test(if n.is_empty() { "draft" } else { &n }, &p, &env)) {
                Ok(d) => { println!("{}", json!({"ok": true, "detail": d})); Some(0) }
                Err(e) => fail(e),
            }
        }
    }
}
