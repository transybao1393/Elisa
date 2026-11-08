//! MCP servers that plug into Elisa: each configured server (config `[mcp.NAME]`, plus Elisa's built-in `web`
//! server, `mcp_web.rs`) is started once per session, its tools are listed and offered to the model next to Elisa's
//! own, and a call the model makes is sent to the server it came from (`tools/call`). This is the base the plugin
//! system will build on. Every MCP tool asks the user first (approval key `mcp:NAME`, `tools::mcp_key`, so "always" covers
//! a whole server and never one of Elisa's own tools), and with no one to ask (`-p` without `--yes`) it is denied.
use crate::mcp::{self, Conn};
use crate::types::ToolDef;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

/// one server from config: a command (stdio) or a URL (streamable HTTP)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerCfg {
    pub name: String,
    pub command: Vec<String>,
    pub url: String,
    /// extra environment for a command server (e.g. an API key the server reads)
    pub env: Vec<(String, String)>,
    /// names of variables a command server inherits from Elisa's environment on top of the allowlist
    /// (`mcp::ENV_ALLOW`); config `env_pass = ["NAME", …]`. A name that is not set is skipped
    pub env_pass: Vec<String>,
    /// one of Elisa's own servers (`builtin_*`): exempt from tool-list pinning (`pins.rs`)
    pub builtin: bool,
}

/// A command server's environment on top of the allowlist: its `env_pass` names with their values from Elisa's
/// environment (those that are set), then its own `env` (which wins).
pub fn env_of(c: &ServerCfg) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = c.env_pass.iter().filter(|k| !c.env.iter().any(|(e, _)| e == *k))
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.clone(), v))).collect();
    env.extend(c.env.iter().cloned());
    env
}

/// proxy settings the built-in web server passes on (a proxy is how some networks reach the web at all)
const PROXY_ENV: [&str; 8] = ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY", "https_proxy", "http_proxy", "all_proxy", "no_proxy"];

/// Elisa's own web server (`elisa --mcp-serve web`), run from this same binary. The Brave Search key goes in the
/// server's environment (`BRAVE_API_KEY`), never on its command line, which other users can see in `ps`.
pub fn builtin_web(engine: &str, searxng_url: &str, brave_key: &str) -> ServerCfg {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    let mut command = vec![exe, "--mcp-serve".into(), "web".into(), "--engine".into(), engine.into()];
    if !searxng_url.is_empty() { command.extend(["--searxng-url".into(), searxng_url.into()]); }
    let env = if engine == crate::web::BRAVE && !brave_key.is_empty() { vec![(crate::web::BRAVE_KEY_ENV.into(), brave_key.into())] } else { Vec::new() };
    // its environment is clean (`mcp::inherited`): only the proxy settings and the test override of the Brave URL
    let env_pass = PROXY_ENV.iter().chain([&crate::web::BRAVE_URL_ENV]).map(|k| k.to_string()).collect();
    ServerCfg { name: "web".into(), command, env, env_pass, builtin: true, ..ServerCfg::default() }
}

/// Elisa's code sandbox (`elisa --mcp-serve run --timeout SECS`, `mcp_run.rs`), on when config `sandbox = "colima"`.
/// Named `run` so its one tool keeps the name `run_code`; its approval key is `mcp:run`.
pub fn builtin_run(timeout_s: u64) -> ServerCfg {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    // the docker daemon the user chose (`mcp_run::find_daemon`); HOME and PATH come with every server
    ServerCfg { name: "run".into(), command: vec![exe, "--mcp-serve".into(), "run".into(), "--timeout".into(), timeout_s.to_string()],
        env_pass: vec!["DOCKER_HOST".into()], builtin: true, ..ServerCfg::default() }
}

/// Elisa's documents server (`elisa --mcp-serve docs --root DIR --max-chars N`, `mcp_docs.rs`), on unless config
/// `docs = "off"`: tools `docs_pdf` / `docs_sheet`, approval key `mcp:docs`. It reads only inside `root` (the session
/// cwd) and keeps its output under `max_chars` (the session's tool output cap, so the host never cuts its `next:` hint).
pub fn builtin_docs(root: &std::path::Path, max_chars: usize) -> ServerCfg {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    let command = vec![exe, "--mcp-serve".into(), "docs".into(), "--root".into(), root.display().to_string(), "--max-chars".into(), max_chars.to_string()];
    ServerCfg { name: "docs".into(), command, builtin: true, ..ServerCfg::default() }
}

/// Elisa's security server (`elisa --mcp-serve sec --root DIR --max-chars N`, `mcp_sec.rs`), on unless config
/// `security = "off"`: tools `sec_secrets` / `sec_deps` / `sec_binary` / `sec_jwt`, approval key
/// `mcp:sec`. It reads only inside `root` (the session cwd); `sec_deps` sends package names + versions to osv.dev.
pub fn builtin_sec(root: &std::path::Path, max_chars: usize) -> ServerCfg {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    let command = vec![exe, "--mcp-serve".into(), "sec".into(), "--root".into(), root.display().to_string(), "--max-chars".into(), max_chars.to_string()];
    ServerCfg { name: "sec".into(), command, builtin: true, ..ServerCfg::default() }
}

/// Elisa's reasoning server for small models (`elisa --mcp-serve reason`, `mcp_reason.rs`), on unless config
/// `reason = "off"`: tools `think`, `steps`, `step_done`, `calc` keep their own names (`is_builtin_reason`) and run
/// without asking (no side effects); the session offers them only on multi-step turns (`mcp_reason::wanted`).
pub fn builtin_reason() -> ServerCfg {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    ServerCfg { name: crate::mcp_reason::NAME.into(), command: vec![exe, "--mcp-serve".into(), "reason".into()], builtin: true, ..ServerCfg::default() }
}

/// the built-in reasoning server itself (not a user server that happens to be called `reason`)
pub fn is_builtin_reason(c: &ServerCfg) -> bool {
    c.name == crate::mcp_reason::NAME && c.url.is_empty() && c.command.get(1..3).is_some_and(|a| a == ["--mcp-serve", "reason"])
}

const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// how long a session's server may take to start (`initialize` and `tools/list` together); servers start in parallel
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// after a cancelled call, how long a stdio server may take to answer a ping before it counts as stuck in the
/// cancelled work and is restarted
const PING_TIMEOUT: Duration = Duration::from_secs(3);
/// on top of the setup timeout: the start thread's own deadline normally ends it first
const GRACE: Duration = Duration::from_millis(500);

/// `live`: the server (as last started) still has the tool; a restarted server that dropped a tool keeps its name
/// (the model was offered it) but a call to it is refused
struct Tool { exposed: String, remote: String, def: ToolDef, live: bool }

struct Server {
    cfg: ServerCfg,
    /// None: the server stopped; the next call tries to restart it once
    conn: Option<Conn>,
    tools: Vec<Tool>,
    /// why the restart failed: no more tries this session
    failed: Option<String>,
    /// the last call was cancelled: the server may still be busy with it
    cancelled: bool,
    /// its tool list as last listed (`pins.rs` hashes it; `/trust` pins it)
    list: Vec<Value>,
    /// the tool list pinned or accepted this session (None: not pinned: a built-in, or pinning is off)
    pinned: Option<Vec<Value>>,
    /// its tool list differs from its pin: every call asks (no always-allow, no --yes) until `/trust`
    untrusted: bool,
    /// a warning for the user that came up inside a call (a restarted server listed other tools): `Host::take_notes`
    note: Option<String>,
    audit: crate::audit::Log,
}

pub struct Host {
    servers: Vec<Server>,
    setup: Duration,
    /// the pins file, once `check_pins` ran (None: no pinning)
    pins: Option<PathBuf>,
}

impl Default for Host {
    fn default() -> Self { Host { servers: Vec::new(), setup: SETUP_TIMEOUT, pins: None } }
}

/// Stops every server, in parallel (each may take up to `mcp::STOP_GRACE` + 0.5 s to stop by itself), and logs it.
impl Drop for Host {
    fn drop(&mut self) {
        let stops: Vec<(Conn, crate::audit::Log, String)> = self.servers.iter_mut()
            .filter_map(|s| s.conn.take().map(|c| (c, s.audit.clone(), s.cfg.name.clone()))).collect();
        std::thread::scope(|sc| for (conn, audit, name) in stops {
            sc.spawn(move || { drop(conn); audit.note("server_stop", &[("server", &name), ("outcome", "ok")]); });
        });
    }
}

fn sanitize(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect()
}

/// The model-facing name: the server's own tool name when it already says which server (`web_search` on `web`), else
/// `server_tool`. It is never one of Elisa's own tool names (`taken`) nor one already given out (`used`): then it is
/// `mcp_server_tool`, and if even that is taken, `mcp_server_tool_2`, `_3`, …
fn exposed_name(server: &str, tool: &str, taken: &dyn Fn(&str) -> bool, used: &HashSet<String>) -> String {
    let free = |n: &str| !taken(n) && !used.contains(n);
    let own = tool.starts_with(&format!("{server}_"));
    let n = sanitize(&if own { tool.to_string() } else { format!("{server}_{tool}") });
    if free(&n) { return n; }
    let base = sanitize(&format!("mcp_{server}_{tool}"));
    if free(&base) { return base; }
    (2..).map(|i: usize| {
        let suffix = format!("_{i}");
        format!("{}{suffix}", base.chars().take(64 - suffix.len()).collect::<String>())
    }).find(|n| free(n)).unwrap_or(base)
}

/// The tools of one server's `tools/list`, named (see `exposed_name`; `bare`: a tool keeps its own name while that is
/// free, for the built-in reasoning server's `think` / `calc`), the names added to `used`.
fn tools_of(server: &str, list: &[Value], taken: &dyn Fn(&str) -> bool, used: &mut HashSet<String>, bare: bool) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    for t in list {
        let Some(remote) = t["name"].as_str() else { continue };
        if tools.iter().any(|x| x.remote == remote) { continue; }
        let own = sanitize(remote);
        let exposed = if bare && own == remote && !taken(&own) && !used.contains(&own) { own } else { exposed_name(server, remote, taken, used) };
        used.insert(exposed.clone());
        let schema = if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({"type": "object", "properties": {}}) };
        let desc = t["description"].as_str().unwrap_or("").trim();
        tools.push(Tool { exposed: exposed.clone(), remote: remote.to_string(), live: true,
            def: ToolDef { name: exposed, description: if desc.is_empty() { format!("{remote} (MCP server {server})") } else { desc.to_string() }, input_schema: schema } });
    }
    tools
}

fn remote_names(list: &[Value]) -> HashSet<String> {
    list.iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()
}

impl Server {
    /// A new process / session for a stopped server, its tools listed again. The names stay; a tool it no longer
    /// has is not routed any more. Gives the reason it could not start.
    fn restart(&mut self, setup: Duration) -> Result<(), String> {
        // the old process (stuck in cancelled work, if it is still there) is stopped in the background: its grace
        // time never holds up the call
        if let Some(old) = self.conn.take() {
            let _ = std::thread::Builder::new().name("elisa-mcp-stop".into()).spawn(move || drop(old));
        }
        self.audit.note("server_stop", &[("server", &self.cfg.name), ("outcome", "restart")]);
        self.cancelled = false;
        let r = Conn::connect_within(&self.cfg.command, &self.cfg.url, &env_of(&self.cfg), setup);
        self.audit.note("server_start", &[("server", &self.cfg.name), ("outcome", if r.is_ok() { "ok" } else { "error" })]);
        let (conn, list) = r?;
        let have = remote_names(&list);
        for t in &mut self.tools { t.live = have.contains(&t.remote); }
        // a pinned server that came back with other tools: it asks on every call from now on (`/trust` accepts)
        if let Some(d) = self.pinned.as_ref().and_then(|old| crate::pins::changed(old, &list)) {
            if !self.untrusted { self.note = Some(changed_note(&self.cfg.name, &d)); }
            self.untrusted = true;
        }
        self.list = list;
        self.conn = Some(conn);
        Ok(())
    }

    fn restart_failed(&self, e: &str) -> String {
        format!("MCP server {} stopped and could not be restarted: {e}; restart the session to try again", self.cfg.name)
    }

    /// Ready for a call: restarted if it stopped (once), or if a cancelled call still holds it (it does not answer a
    /// ping). Err: the message for the model.
    fn ready(&mut self, setup: Duration, cancel: &AtomicBool) -> Result<(), String> {
        if let Some(e) = &self.failed { return Err(self.restart_failed(e)); }
        if self.conn.is_some() && self.cancelled && self.cfg.url.is_empty() {
            let conn = self.conn.as_mut().ok_or("no connection")?;
            match conn.request("ping", json!({}), cancel, PING_TIMEOUT) {
                Err(e) if e == mcp::CANCELLED => return Err(format!("MCP server {}: {e}", self.cfg.name)),
                // answered (even "no such method"): it is free again
                Ok(_) => self.cancelled = false,
                Err(e) if e.starts_with("MCP error") => self.cancelled = false,
                Err(_) => self.conn = None, // stuck in the cancelled work, or gone: restart it below
            }
        }
        if self.conn.is_none() {
            if let Err(e) = self.restart(setup) {
                let msg = self.restart_failed(&e);
                self.failed = Some(e);
                return Err(msg);
            }
        }
        Ok(())
    }
}

impl Host {
    /// Starts every server, in parallel, each given SETUP_TIMEOUT; one that fails is left out with a note (the
    /// session goes on without it). `taken`: Elisa's own tool names, which an MCP tool may not shadow.
    pub fn start(cfgs: &[ServerCfg], taken: &dyn Fn(&str) -> bool) -> (Host, Vec<String>) {
        Self::start_with_timeout(cfgs, taken, SETUP_TIMEOUT)
    }

    /// `start` with another setup time limit per server (also used when a server is restarted).
    pub fn start_with_timeout(cfgs: &[ServerCfg], taken: &dyn Fn(&str) -> bool, setup: Duration) -> (Host, Vec<String>) {
        // each server starts on its own thread; one that is too slow is given up (its thread's own deadline kills its
        // process, and a result that comes later is dropped, which does too)
        let pending: Vec<_> = cfgs.iter().map(|c| {
            let (tx, rx) = channel();
            let c = c.clone();
            let spawned = std::thread::Builder::new().name("elisa-mcp-start".into())
                .spawn(move || { let _ = tx.send(Conn::connect_within(&c.command, &c.url, &env_of(&c), setup)); });
            (rx, spawned.err())
        }).collect();
        let deadline = Instant::now() + setup + GRACE;
        let mut host = Host { servers: Vec::new(), setup, pins: None };
        let mut notes = Vec::new();
        let mut used = HashSet::new();
        for (c, (rx, spawn_err)) in cfgs.iter().zip(pending) {
            let r = match spawn_err {
                Some(e) => Err(format!("cannot start a thread: {e}")),
                None => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(r) => r,
                    Err(RecvTimeoutError::Timeout) => Err(format!("the MCP server did not start in {}", mcp::secs(setup))),
                    Err(RecvTimeoutError::Disconnected) => Err("its start failed".into()),
                },
            };
            match r {
                Err(e) => notes.push(format!("mcp {}: not started: {e}", c.name)),
                Ok((conn, list)) => {
                    let tools = tools_of(&c.name, &list, taken, &mut used, is_builtin_reason(c));
                    host.servers.push(Server { cfg: c.clone(), conn: Some(conn), tools, failed: None, cancelled: false, list,
                        pinned: None, untrusted: false, note: None, audit: crate::audit::Log::default() });
                }
            }
        }
        (host, notes)
    }

    /// Logs to `audit` from now on (restarts, stops), starting with a `server_start` line for each running server.
    pub fn audit_to(&mut self, audit: &crate::audit::Log) {
        for s in &mut self.servers {
            s.audit = audit.clone();
            audit.note("server_start", &[("server", &s.cfg.name), ("outcome", "ok")]);
        }
    }

    /// Tool-list pinning (`pins.rs`) against the pins file at `path`, for every server that is not built in: a new
    /// one is pinned (an info note), one whose list changed is untrusted for the session (a warning naming the
    /// changes). Notes: (warning, text).
    pub fn check_pins(&mut self, path: &Path) -> Vec<(bool, String)> {
        use crate::pins::{Pins, Seen};
        let mut pins = Pins::load(path);
        let (mut notes, mut dirty) = (Vec::new(), false);
        for s in self.servers.iter_mut().filter(|s| !s.cfg.builtin) {
            match pins.check(&crate::pins::key(&s.cfg.name, &s.cfg.command, &s.cfg.url), &s.list) {
                Seen::New => {
                    dirty = true;
                    s.pinned = Some(s.list.clone());
                    notes.push((false, format!("mcp {}: tool list pinned ({} tools, {})", s.cfg.name, s.list.len(), path.display())));
                }
                Seen::Same => s.pinned = Some(s.list.clone()),
                Seen::Changed(d) => { s.untrusted = true; notes.push((true, changed_note(&s.cfg.name, &d))); }
            }
        }
        if dirty { if let Err(e) = pins.save() { notes.push((true, format!("mcp tool-list pins not saved: {e}"))); } }
        self.pins = Some(path.to_path_buf());
        notes
    }

    /// the server's tool list differs from its pin: its tools ask on every call (`untrusted`)
    pub fn untrusted(&self, server: &str) -> bool { self.servers.iter().any(|s| s.cfg.name == server && s.untrusted) }

    /// the servers whose tool list differs from its pin
    pub fn untrusted_servers(&self) -> Vec<String> { self.servers.iter().filter(|s| s.untrusted).map(|s| s.cfg.name.clone()).collect() }

    /// `/trust NAME`: the server's current tool list is accepted and pinned; its tools follow the normal rules again.
    pub fn trust(&mut self, server: &str) -> Result<String, String> {
        let path = self.pins.clone().ok_or("tool-list pinning is not on in this session")?;
        let s = self.servers.iter_mut().find(|s| s.cfg.name == server && !s.cfg.builtin)
            .ok_or_else(|| format!("no MCP server {server:?} with a pinned tool list (Elisa's built-in servers are not pinned)"))?;
        let mut pins = crate::pins::Pins::load(&path);
        pins.accept(&crate::pins::key(&s.cfg.name, &s.cfg.command, &s.cfg.url), &s.list);
        pins.save()?;
        let was = std::mem::replace(&mut s.untrusted, false);
        s.pinned = Some(s.list.clone());
        Ok(format!("mcp {server}: {} ({} tools) pinned in {}", if was { "new tool list accepted and" } else { "tool list" }, s.list.len(), path.display()))
    }

    /// warnings that came up during calls (a restarted server listed other tools), each once
    pub fn take_notes(&mut self) -> Vec<String> { self.servers.iter_mut().filter_map(|s| s.note.take()).collect() }

    /// the tools offered to the model, server by server, in config order
    pub fn defs(&self) -> Vec<ToolDef> { self.servers.iter().flat_map(|s| s.tools.iter().map(|t| t.def.clone())).collect() }

    pub fn owns(&self, name: &str) -> bool { self.server_of(name).is_some() }

    /// the server a tool belongs to (its approval key is `tools::mcp_key` of it)
    pub fn server_of(&self, name: &str) -> Option<&str> {
        self.servers.iter().find(|s| s.tools.iter().any(|t| t.exposed == name)).map(|s| s.cfg.name.as_str())
    }

    /// one line per server for the startup info: `mcp web: web_search, web_fetch`
    pub fn lines(&self) -> Vec<String> {
        self.servers.iter().map(|s| format!("mcp {}: {}", s.cfg.name, s.tools.iter().map(|t| t.exposed.as_str()).collect::<Vec<_>>().join(", "))).collect()
    }

    /// a tool of the built-in reasoning server: no side effects, so it runs without asking, and the session offers
    /// it only on multi-step turns
    pub fn reason_tool(&self, name: &str) -> bool {
        self.servers.iter().any(|s| is_builtin_reason(&s.cfg) && s.tools.iter().any(|t| t.exposed == name))
    }

    /// The built-in gated server (`mcp_reason::GATED`: `docs`, `sec`) a tool belongs to: offered per turn by the
    /// relevance gate (config `gate_tools`). None for every other tool (a user's own server named `docs` is never gated).
    pub fn gated_server(&self, name: &str) -> Option<&'static str> {
        let s = self.servers.iter().find(|s| s.tools.iter().any(|t| t.exposed == name))?;
        if !s.cfg.builtin { return None; }
        crate::mcp_reason::GATED.iter().copied().find(|g| *g == s.cfg.name)
    }

    /// Estimated prompt tokens of each server's tool definitions (as the request carries them), largest first.
    pub fn costs(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self.servers.iter().map(|s| {
            let defs: Vec<&ToolDef> = s.tools.iter().map(|t| &t.def).collect();
            (s.cfg.name.clone(), crate::context::estimate_tokens(&serde_json::to_string(&defs).unwrap_or_default()))
        }).collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v
    }

    /// A new user turn: the reasoning server's per-turn caps start again (`mcp_reason::TURN`). Best effort: a server
    /// that stopped starts fresh anyway.
    pub fn new_turn(&mut self) {
        for s in self.servers.iter_mut().filter(|s| is_builtin_reason(&s.cfg)) {
            if let Some(c) = s.conn.as_mut() { let _ = c.notify(crate::mcp_reason::TURN, json!({})); }
        }
    }

    /// The model's call, sent to its server: the text parts of the result, at most `max_chars`. A server that stopped
    /// is restarted on the next call (once per session).
    pub fn call(&mut self, name: &str, input: &Value, cancel: &AtomicBool, max_chars: usize) -> crate::tools::ToolOutput {
        let out = |content: String, is_error: bool| crate::tools::ToolOutput { content, is_error };
        let setup = self.setup;
        let Some(s) = self.servers.iter_mut().find(|s| s.tools.iter().any(|t| t.exposed == name)) else { return out(format!("no MCP tool {name}"), true) };
        if let Err(e) = s.ready(setup, cancel) { return out(e, true); }
        let Some(tool) = s.tools.iter().find(|t| t.exposed == name) else { return out(format!("no MCP tool {name}"), true) };
        if !tool.live { return out(format!("MCP server {} restarted without its tool {}, so {name} is not available any more", s.cfg.name, tool.remote), true); }
        let remote = tool.remote.clone();
        let args = if input.is_object() { input.clone() } else { json!({}) };
        // placeholders a small model put in optional fields ("revisesThought": 0, "branchId": "") are left out
        // rather than sent for a strict server to reject (`mcp_args::clean`, against the tool's inputSchema)
        let (args, _dropped) = crate::mcp_args::clean(&tool.def.input_schema, &args);
        let Some(conn) = s.conn.as_mut() else { return out(format!("MCP server {}: not connected", s.cfg.name), true) };
        match conn.request("tools/call", json!({"name": remote, "arguments": args}), cancel, CALL_TIMEOUT) {
            Err(e) if e == mcp::CANCELLED => { s.cancelled = true; out(format!("MCP server {}: {e}", s.cfg.name), true) }
            Err(e) if mcp::gone(&e) => { s.conn = None; out(format!("MCP server {} stopped: {e}", s.cfg.name), true) }
            Err(e) => out(format!("MCP server {}: {e}", s.cfg.name), true),
            Ok(r) => {
                let mut text: String = r["content"].as_array().map(|a| a.iter().filter_map(|b| match b["type"].as_str() {
                    Some("text") => b["text"].as_str().map(String::from),
                    Some(t) => Some(format!("[{t} content not shown]")),
                    None => None,
                }).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                if text.is_empty() { text = r["structuredContent"].to_string(); }
                if text.chars().count() > max_chars {
                    text = format!("{}\n[… cut at {max_chars} characters]", text.chars().take(max_chars).collect::<String>());
                }
                out(text, r["isError"].as_bool().unwrap_or(false))
            }
        }
    }
}

fn changed_note(server: &str, what: &str) -> String {
    format!("mcp {server}: its tool list changed since it was pinned ({what}); its tools ask on every call this session \
        (no always-allow, no --yes). Check the server, then /trust {server} to accept the new list")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(server: &str, tool: &str, taken: &dyn Fn(&str) -> bool, used: &mut HashSet<String>) -> String {
        let n = exposed_name(server, tool, taken, used);
        used.insert(n.clone());
        n
    }

    #[test]
    fn tool_names_keep_their_server_and_never_shadow_elisas_own() {
        let taken = |n: &str| ["read", "bash", "read_symbol", "web_search", "mcp_x_y"].contains(&n);
        let used = &mut HashSet::new();
        assert_eq!(name("web", "web_fetch", &taken, used), "web_fetch");
        assert_eq!(name("github", "create_issue", &taken, used), "github_create_issue");
        assert_eq!(name("fs", "read", &taken, used), "fs_read");
        assert_eq!(name("my srv", "a.b", &taken, used), "my_srv_a_b");
        // a built name that is one of Elisa's
        assert_eq!(name("read", "symbol", &taken, used), "mcp_read_symbol");
        assert_eq!(name("web", "web_search", &taken, used), "mcp_web_web_search");
        // already given out
        assert_eq!(name("fs", "read", &taken, used), "mcp_fs_read");
        assert_eq!(name("fs", "read", &taken, used), "mcp_fs_read_2");
        assert_eq!(name("fs", "read", &taken, used), "mcp_fs_read_3");
        assert_eq!(name("x", "y", &taken, used), "x_y");
        assert_eq!(name("x", "y", &taken, used), "mcp_x_y_2");
        // long names stay within 64 characters and unique
        let long = "t".repeat(80);
        let a = name("s", &long, &taken, used);
        let b = name("s", &long, &taken, used);
        let c = name("s", &long, &taken, used);
        assert!(a.len() <= 64 && b.len() <= 64 && c.len() <= 64);
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn the_brave_key_goes_to_the_web_server_in_its_env_not_its_arguments() {
        let c = builtin_web("brave", "", "sekret-123");
        assert!(!c.command.iter().any(|a| a.contains("sekret")), "{:?}", c.command);
        assert_eq!(c.command[c.command.len() - 2..], ["--engine", "brave"]);
        assert_eq!(c.env, [("BRAVE_API_KEY".to_string(), "sekret-123".to_string())]);
        // other engines get no key
        assert!(builtin_web("duckduckgo", "", "sekret-123").env.is_empty());
    }

    #[test]
    fn conn_can_move_to_a_start_thread() {
        fn send<T: Send>() {}
        send::<Conn>();
    }
}
