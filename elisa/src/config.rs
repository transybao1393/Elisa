//! Config: defaults <- user file <- project file <- active model connection (`src/provider.rs`) <- env.
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub max_ctx: Option<usize>,
    pub max_output: usize,
    /// legacy: no longer used (the fold replaced trim + auto-compact); kept so old config files still load
    pub compact_at: f32,
    pub keep_recent_turns: usize,
    pub tool_output_max_chars: usize,
    pub bash_timeout_s: u64,
    pub auto_approve: Vec<String>,
    pub max_steps: usize,
    pub system_prompt_file: Option<PathBuf>,
    pub think: bool,
    /// graph memory (elisa-mem): record turns, recall, every-N-turns fold
    pub memory: bool,
    /// shared across projects; each session is tagged with the cwd
    pub memory_dir: PathBuf,
    /// fold (summarise the oldest messages) when the window reaches this share of the usable budget (max_ctx - reserve)
    pub fold_at: f32,
    /// ... down to a kept history of at most this share
    pub fold_to: f32,
    /// also fold after this many turns (user text or assistant reply); 0 = off
    pub fold_max_turns: usize,
    pub recall_k: usize,
    /// recalled hits below this elisa-mem score are not injected
    pub recall_min_score: f32,
    /// recalled hits whose idf-weighted share of the query's terms is below this are not injected
    pub recall_min_support: f32,
    pub recall_max_tokens: usize,
    /// the companion Shade in the TUI (also off with --no-character, and automatically when the terminal is too small)
    pub character: bool,
    /// "dark" (brand default) or "light"
    pub theme: String,
    /// Shade falls asleep after this many idle seconds; 0 = never (the kit's prototype default: 45 s)
    pub sleep_after_s: u64,
    /// link to the companion window (app/ElisaCompanion) when it runs; the terminal dock hides while it is connected
    pub companion: bool,
    /// check-before-done: after a turn that edited files, run the project's detected checks (src/checks.rs) and let
    /// the model fix failures; `.elisa/checks.toml` and `--no-checks` override
    pub checks: bool,
    /// most fix rounds after a failed check
    pub check_rounds: usize,
    /// timeout per fast check (build / type / lint), seconds; the process group is killed
    pub check_timeout_s: u64,
    /// timeout per test check, seconds (tests run only when cheap: a timeout skips them, it is not a failure)
    pub check_test_timeout_s: u64,
    /// failing check output sent back to the model, chars (error lines first, then head + tail)
    pub check_output_max_chars: usize,
    /// syntax-tree tools outline / read_symbol / edit_symbol (src/ast.rs) and the prompt line that steers to them
    pub ast: bool,
    /// refuse a write / edit / edit_symbol that adds parse errors to a Rust / Python / Swift / TS / JS / SQL / bash file
    pub ast_guard: bool,
    /// MCP servers that plug tools into the session (`[mcp.NAME]` in the user's config.toml; `mcp_host.rs`). Elisa's
    /// own `web` server comes on top unless `web = "off"` or a server of that name is configured
    pub mcp: Vec<crate::mcp_host::ServerCfg>,
    /// servers switched off by name (`[mcp.NAME] enabled = false`, also from a project config; "web" = the built-in)
    pub mcp_off: Vec<String>,
    /// per-server tool filters by server name (`[mcp.NAME] tools = […]` / `exclude = […]`), for user servers and the
    /// built-in ones alike; a project config can only narrow them (`mcp_host::ToolFilter`)
    pub mcp_filter: std::collections::BTreeMap<String, crate::mcp_host::ToolFilter>,
    /// narrow the test check to the edited symbols (Rust: a test-name filter; Python: the test files that import them)
    pub ast_targeted_checks: bool,
    /// per-request metrics (src/metrics.rs): one JSON line per request in `.elisa/metrics.jsonl`, and `/stats`
    pub metrics: bool,
    /// after a fold, send the folded prompt as a prefill-only warm-up in the background (gateway `x-streamdense-warm`,
    /// docs/GATEWAY.md), so the next real call reuses it: "auto" (only when the gateway's /health says it can), "on"
    /// (try; an error or a generated token turns it off for the session), "off"
    pub warm: String,
    /// tool calls outside the project folder (src/outside.rs): "ask" (default) | "read-ok" (outside reads run without
    /// asking, writes ask) | "deny" | "allow" (no outside prompt; the normal tool rules apply, so `--yes` then allows
    /// outside writes). A project's `.elisa/config.toml` may only make it stricter than the user config.
    pub outside: String,
    /// the shell sandbox around `bash` and the checks (src/sandbox.rs, macOS Seatbelt): "write" (default on macOS:
    /// writes only inside the project, temp folders and build caches; secret stores unreadable), "strict" (also no
    /// network but localhost), "off". A project's `.elisa/config.toml` may only make it stricter; elsewhere a no-op
    pub shell_sandbox: String,
    /// the active connection of `providers.toml`; "" = none: `base_url` / `model` / `api_key` above as configured
    pub provider: String,
    /// how requests reach the model (`Gateway` unless a connection says otherwise)
    pub transport: crate::provider::Transport,
    /// the active connection sends the conversation here (None: it stays on this machine)
    pub leaves: Option<String>,
    /// the connection's `vision` setting (None: work it out at startup, `provider::vision`)
    pub vision: Option<bool>,
    /// web access for the model (config.toml `web`): "duckduckgo" (default), "searxng" or "brave"; "" or "off" = off.
    /// Every web_search / web_fetch call asks first, like the other tools (not a connection setting any more)
    pub web: String,
    /// the SearXNG server for `web = "searxng"`
    pub searxng_url: String,
    /// the Brave Search API key for `web = "brave"`: env `BRAVE_API_KEY`, else the user config's `brave_api_key` (a
    /// project config cannot set it). Handed to the web server in its environment only; never printed
    pub brave_api_key: String,
    /// the code sandbox (`run_code`, `mcp_run.rs`): "colima" runs snippets in a locked-down container on Colima's
    /// docker; "off" (default). A project config can only switch it off
    pub sandbox: String,
    /// wall-clock limit per `run_code` call, seconds (config `sandbox_timeout`, 1 ..= 30; a project config may only lower it)
    pub sandbox_timeout_s: u64,
    /// the documents server (`docs_pdf` / `docs_sheet`, `mcp_docs.rs`): "on" (default) reads PDFs and spreadsheets
    /// inside the project folder; "off" (or "") leaves it out. A project config can only switch it off
    pub docs: String,
    /// the security server (`sec_secrets` / `sec_deps` / `sec_binary` / `sec_jwt`, `mcp_sec.rs`): "on"
    /// (default) or "off" (or ""). `sec_deps` sends package names + versions to osv.dev. A project config can only
    /// switch it off
    pub security: String,
    /// the reasoning tools for small models (`think` / `steps` / `step_done` / `calc`, `mcp_reason.rs`): "auto"
    /// (default: offered only on turns whose request looks like a multi-step task), "always", "off" (server not
    /// started). A project config can only switch it off
    pub reason: String,
    /// reasoning on a turn the reasoning gate (`mcp_reason::wanted`) judges simple ("hi", "2+2=?"): "off" (default:
    /// the model is asked not to reason where the connection allows it, and the reply gets a smaller output cap) or
    /// "on" (as on any other turn). A project config can only set it "off"
    pub think_simple: String,
    /// relevance gating for the built-in tool servers (`mcp_reason::gate`): "auto" (default: `docs_*` only on turns about
    /// a document / PDF / spreadsheet, `sec_*` only on turns about security, secrets, vulnerabilities, dependencies,
    /// JWTs or binaries; decided once per turn) or "off" (every tool on every turn). A project config can only set it
    /// "auto" (fewer tools is the stricter side)
    pub gate_tools: String,
    /// the audit log (`audit.rs`, `~/.elisa/audit/<project>.jsonl`, `/audit`): "on" (default) | "off". A project
    /// config may only switch it off
    pub audit: String,
    /// secrets in tool output (`secrets.rs`): "remote" (default: hidden from the model when the connection leaves this
    /// machine), "always" (also for local models), "off". Disk writes (notes, memory, sessions) are redacted always.
    /// A project config may only make it stricter (off < remote < always)
    pub redact: String,
    /// problems that did not stop loading (a skipped `[mcp.NAME]`, a bad `web` value, a project setting that was
    /// ignored): shown once as info lines when the session starts. Empty on a clean config
    pub notes: Vec<String>,
}

fn home() -> PathBuf { std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from) }

/// `~/x` -> `$HOME/x`
fn expand(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix("~/")) { Some(r) => home().join(r), None => p }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base_url: "http://127.0.0.1:8898".into(),
            // no guessed model: the user chooses one (app: Model Connections; terminal: --model or providers.toml)
            model: String::new(),
            api_key: "dummy".into(),
            max_ctx: None,
            max_output: 2048,
            compact_at: 0.80,
            keep_recent_turns: 3,
            tool_output_max_chars: 6000,
            bash_timeout_s: 120,
            auto_approve: ["read", "glob", "grep"].map(String::from).to_vec(),
            max_steps: 30,
            system_prompt_file: None,
            think: false,
            memory: true,
            memory_dir: home().join(".elisa/memory"),
            fold_at: 0.65,
            fold_to: 0.30,
            fold_max_turns: 0,
            recall_k: 3,
            recall_min_score: 10.0,
            recall_min_support: 0.45,
            recall_max_tokens: 300,
            character: true,
            theme: "dark".into(),
            sleep_after_s: 45,
            companion: false,
            checks: true,
            check_rounds: 2,
            check_timeout_s: 120,
            check_test_timeout_s: 60,
            check_output_max_chars: 3000,
            ast: true,
            ast_guard: true,
            mcp: Vec::new(),
            mcp_off: Vec::new(),
            mcp_filter: Default::default(),
            ast_targeted_checks: true,
            metrics: true,
            warm: "auto".into(),
            outside: "ask".into(),
            shell_sandbox: crate::sandbox::Mode::platform_default().word().into(),
            provider: String::new(),
            transport: crate::provider::Transport::Gateway,
            leaves: None,
            vision: None,
            web: crate::web::DUCKDUCKGO.into(),
            searxng_url: String::new(),
            brave_api_key: String::new(),
            sandbox: "off".into(),
            sandbox_timeout_s: crate::mcp_run::DEFAULT_TIMEOUT_S,
            docs: "on".into(),
            security: "on".into(),
            reason: "auto".into(),
            think_simple: "off".into(),
            gate_tools: "auto".into(),
            audit: "on".into(),
            redact: "remote".into(),
            notes: Vec::new(),
        }
    }
}

/// `[mcp.NAME]`: `command = ["prog", "arg"]` (stdio) or `url = "https://…"` (streamable HTTP), optional `env` table,
/// `enabled = false` to switch a server (or the built-in `web`) off, `tools = […]` / `exclude = […]` to offer only
/// some of its tools. Checked by hand (`parse_mcp`), not by serde, so one bad entry skips that server with a note
/// instead of failing the whole config file.
#[derive(Default)]
struct McpFile {
    command: Vec<String>,
    url: String,
    env: Vec<(String, String)>,
    env_pass: Vec<String>,
    enabled: Option<bool>,
    tools: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

/// Elisa's own servers, which `[mcp.NAME]` can give a tool filter without a command
const BUILTIN_SERVERS: [&str; 5] = ["web", "docs", "sec", "run", crate::mcp_reason::NAME];

fn names_list(v: &toml::Value, key: &str) -> Result<Vec<String>, String> {
    let a = v.as_array().ok_or_else(|| format!("{key} must be a list of tool names: {key} = [\"name\", …]"))?;
    a.iter().map(|x| x.as_str().map(|s| s.trim().to_string())).collect::<Option<_>>().ok_or_else(|| format!("{key} must be a list of strings"))
}

fn parse_mcp(v: &toml::Value) -> Result<McpFile, String> {
    let t = v.as_table().ok_or("not a table: use [mcp.NAME] with command = [\"program\", \"args\"…] or url = \"https://…\"")?;
    let mut m = McpFile::default();
    for (k, v) in t {
        match k.as_str() {
            "command" => {
                let a = v.as_array().ok_or("command must be a list: command = [\"program\", \"args\"…]")?;
                m.command = a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<_>>().ok_or("command must be a list of strings")?;
                if m.command.first().is_some_and(|c| c.trim().is_empty()) { return Err("command is empty: command = [\"program\", \"args\"…]".into()); }
            }
            "url" => m.url = v.as_str().ok_or("url must be a string: url = \"https://…\"")?.to_string(),
            "env" => {
                let e = v.as_table().ok_or("env must be a table of strings: env = { KEY = \"value\" }")?;
                m.env = e.iter().map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect::<Option<_>>()
                    .ok_or("env values must be strings")?;
            }
            "env_pass" => {
                let a = v.as_array().ok_or("env_pass must be a list of variable names: env_pass = [\"NAME\", …]")?;
                m.env_pass = a.iter().map(|x| x.as_str().map(String::from)).collect::<Option<_>>().ok_or("env_pass must be a list of strings")?;
            }
            "enabled" => m.enabled = Some(v.as_bool().ok_or("enabled must be true or false")?),
            "tools" => m.tools = Some(names_list(v, "tools")?),
            "exclude" => m.exclude = Some(names_list(v, "exclude")?),
            k => return Err(format!("unknown key {k:?} (use command, url, env, env_pass, enabled, tools, exclude)")),
        }
    }
    if !m.url.is_empty() && !(m.url.starts_with("http://") || m.url.starts_with("https://")) {
        return Err(format!("url must start with http:// or https:// (got {:?})", m.url));
    }
    Ok(m)
}

/// A server name that can be added: `[A-Za-z0-9_-]+` and not one of Elisa's own tool / approval names (`web` is
/// fine: it replaces the built-in web server).
fn check_server_name(name: &str) -> Result<(), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("a server name may only use letters, digits, _ and -".into());
    }
    if crate::tools::reserved_server_name(name) {
        return Err(format!("{name:?} is one of Elisa's own tool names; give the server another name"));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    max_ctx: Option<usize>,
    max_output: Option<usize>,
    compact_at: Option<f32>,
    keep_recent_turns: Option<usize>,
    tool_output_max_chars: Option<usize>,
    bash_timeout_s: Option<u64>,
    auto_approve: Option<Vec<String>>,
    web: Option<String>,
    searxng_url: Option<String>,
    brave_api_key: Option<String>,
    sandbox: Option<String>,
    sandbox_timeout: Option<u64>,
    docs: Option<String>,
    security: Option<String>,
    reason: Option<String>,
    think_simple: Option<String>,
    gate_tools: Option<String>,
    audit: Option<String>,
    redact: Option<String>,
    max_steps: Option<usize>,
    system_prompt_file: Option<PathBuf>,
    think: Option<bool>,
    memory: Option<bool>,
    memory_dir: Option<PathBuf>,
    fold_at: Option<f32>,
    fold_to: Option<f32>,
    fold_max_turns: Option<usize>,
    recall_k: Option<usize>,
    recall_min_score: Option<f32>,
    recall_min_support: Option<f32>,
    recall_max_tokens: Option<usize>,
    character: Option<bool>,
    theme: Option<String>,
    sleep_after_s: Option<u64>,
    companion: Option<bool>,
    checks: Option<bool>,
    check_rounds: Option<usize>,
    check_timeout_s: Option<u64>,
    check_test_timeout_s: Option<u64>,
    check_output_max_chars: Option<usize>,
    ast: Option<bool>,
    ast_guard: Option<bool>,
    /// checked by hand (`parse_mcp`): a mistake there skips one server, it does not fail the file
    mcp: Option<toml::Value>,
    ast_targeted_checks: Option<bool>,
    warm: Option<String>,
    metrics: Option<bool>,
    outside: Option<String>,
    shell_sandbox: Option<String>,
    /// legacy (chat/code modes removed 2026-11-01): accepted with any value and ignored, so old config files load
    #[serde(rename = "auto_mode")]
    _auto_mode: Option<toml::Value>,
    #[serde(rename = "chat_window")]
    _chat_window: Option<toml::Value>,
    /// legacy (planning removed 2026-11-08, with its embedding rerank and external dev tools): accepted with any
    /// value and ignored, so old config files load
    #[serde(rename = "plan")]
    _plan: Option<toml::Value>,
    #[serde(rename = "test_plan")]
    _test_plan: Option<toml::Value>,
    #[serde(rename = "plan_exec")]
    _plan_exec: Option<toml::Value>,
    #[serde(rename = "embed_url")]
    _embed_url: Option<toml::Value>,
    #[serde(rename = "embed_model")]
    _embed_model: Option<toml::Value>,
    #[serde(rename = "embed_key")]
    _embed_key: Option<toml::Value>,
    #[serde(rename = "embed_timeout_ms")]
    _embed_timeout_ms: Option<toml::Value>,
    #[serde(rename = "tools_env")]
    _tools_env: Option<toml::Value>,
}

macro_rules! overlay {
    ($c:ident, $f:ident; $($k:ident),*) => { $(if let Some(v) = $f.$k { $c.$k = v; })* };
}

impl Config {
    /// Defaults <- ~/.config/elisa/config.toml <- ./.elisa/config.toml <- env (ELISA_BASE_URL, ELISA_MODEL, ELISA_MAX_CTX, ANTHROPIC_API_KEY).
    pub fn load(cwd: &std::path::Path) -> Result<Config, String> { Config::load_as(cwd, None) }

    /// `load` with the connection named on the command line (`provider::NONE` = no connection).
    pub fn load_as(cwd: &std::path::Path, provider: Option<&str>) -> Result<Config, String> {
        Config::load_from_as(cwd, std::env::var_os("HOME").map(PathBuf::from), |k| std::env::var(k).ok(), provider)
    }

    /// `load` with an explicit home dir and environment lookup (tests pass their own: no process-env mutation, no races).
    pub fn load_from(cwd: &std::path::Path, home: Option<PathBuf>, getenv: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        Config::load_from_as(cwd, home, getenv, None)
    }

    pub fn load_from_as(cwd: &std::path::Path, home: Option<PathBuf>, getenv: impl Fn(&str) -> Option<String>, provider: Option<&str>) -> Result<Config, String> {
        let mut c = Config::default();
        let conn_home = home.clone();
        let files = home.map(|h| h.join(".config/elisa/config.toml")).into_iter().chain([cwd.join(".elisa/config.toml")]);
        let project = cwd.join(".elisa/config.toml");
        for p in files {
            if p.is_file() { c.apply_file(&p, p == project)?; }
        }
        let env = |k: &str| getenv(k).filter(|v| !v.is_empty());
        c.apply_connection(provider, conn_home.as_deref(), &getenv)?;
        if let Some(v) = env("ELISA_BASE_URL") { c.base_url = v; }
        if let Some(v) = env("ELISA_MODEL") { c.model = v; }
        if let Some(v) = env("ELISA_MAX_CTX") {
            c.max_ctx = Some(v.trim().parse().map_err(|_| format!("ELISA_MAX_CTX: not a number: {v}"))?);
        }
        if let Some(v) = env("ELISA_THEME") { c.theme = v; }
        // a connection has its own key: a generic key in the environment must not reach another provider
        if c.provider.is_empty() { if let Some(v) = env("ELISA_API_KEY").or_else(|| env("ANTHROPIC_API_KEY")) { c.api_key = v; } }
        // the Brave Search key: the environment wins over the user config (like a connection's api_key_env)
        if let Some(v) = env(crate::web::BRAVE_KEY_ENV) { c.brave_api_key = v; }
        c.brave_api_key = c.brave_api_key.trim().to_string();
        c.base_url = c.base_url.trim_end_matches('/').to_string();
        c.check_web();
        c.check_sandbox();
        c.check_docs();
        c.check_security();
        c.check_reason();
        c.check_gate_tools();
        if !matches!(c.think_simple.as_str(), "off" | "on") {
            c.notes.push(format!("think_simple: off — think_simple = {:?} is not a setting (use \"off\" or \"on\")", c.think_simple));
            c.think_simple = "off".into();
        }
        c.check_audit();
        c.check_shell_sandbox();
        Ok(c)
    }

    /// The shell sandbox exists only on macOS: anywhere else a setting other than "off" is a no-op, said once.
    fn check_shell_sandbox(&mut self) {
        if !cfg!(target_os = "macos") && self.shell_sandbox != "off" {
            self.notes.push(format!("shell_sandbox = {:?} has no effect here: the shell sandbox needs macOS (sandbox-exec); commands run unsandboxed", self.shell_sandbox));
            self.shell_sandbox = "off".into();
        }
    }

    /// A `web` value Elisa cannot search with turns web access off for the session, with one note (instead of an
    /// error at the first search). Quiet when the built-in web server is not used anyway.
    fn check_web(&mut self) {
        use crate::web::{BRAVE, BRAVE_KEY_ENV, DUCKDUCKGO, SEARXNG};
        let http = |u: &str| u.starts_with("http://") || u.starts_with("https://");
        let why = match self.web.as_str() {
            "" | "off" | DUCKDUCKGO => return,
            SEARXNG if http(&self.searxng_url) => return,
            SEARXNG => format!("web = \"{SEARXNG}\" needs searxng_url = \"http://…\""),
            BRAVE if !self.brave_api_key.is_empty() => return,
            BRAVE => format!("web = \"{BRAVE}\" needs an API key: set {BRAVE_KEY_ENV} or brave_api_key in ~/.config/elisa/config.toml"),
            w => format!("web = {w:?} is not a search method (use \"{DUCKDUCKGO}\", \"{SEARXNG}\" or \"{BRAVE}\")"),
        };
        let builtin = !self.mcp_off.iter().any(|n| n == "web") && !self.mcp.iter().any(|c| c.name == "web");
        if builtin { self.notes.push(format!("web: off — {why}")); }
        self.web = "off".into();
    }

    /// A `sandbox` value other than "colima" / "off" turns the sandbox off with one note; the timeout is kept in range.
    fn check_sandbox(&mut self) {
        use crate::mcp_run::MAX_TIMEOUT_S;
        match self.sandbox.as_str() {
            "colima" | "off" => {}
            "" => self.sandbox = "off".into(),
            w => {
                self.notes.push(format!("sandbox: off — sandbox = {w:?} is not a sandbox (use \"colima\" or \"off\")"));
                self.sandbox = "off".into();
            }
        }
        if !(1..=MAX_TIMEOUT_S).contains(&self.sandbox_timeout_s) {
            let t = self.sandbox_timeout_s.clamp(1, MAX_TIMEOUT_S);
            if self.sandbox == "colima" { self.notes.push(format!("sandbox_timeout = {} out of range: using {t} s (1 ..= {MAX_TIMEOUT_S})", self.sandbox_timeout_s)); }
            self.sandbox_timeout_s = t;
        }
    }

    /// A `security` value other than "on" / "off" switches the security server off with one note ("" = off, quietly).
    fn check_security(&mut self) {
        match self.security.as_str() {
            "on" | "off" => {}
            "" => self.security = "off".into(),
            w => {
                self.notes.push(format!("security: off — security = {w:?} is not a setting (use \"on\" or \"off\")"));
                self.security = "off".into();
            }
        }
    }

    /// A `docs` value other than "on" / "off" switches the documents server off with one note ("" = off, quietly).
    fn check_docs(&mut self) {
        match self.docs.as_str() {
            "on" | "off" => {}
            "" => self.docs = "off".into(),
            w => {
                self.notes.push(format!("docs: off — docs = {w:?} is not a setting (use \"on\" or \"off\")"));
                self.docs = "off".into();
            }
        }
    }

    /// An `audit` value other than "on" / "off" keeps the log on, with one note.
    fn check_audit(&mut self) {
        if self.audit != "on" && self.audit != "off" {
            self.notes.push(format!("audit: on — audit = {:?} is not a setting (use \"on\" or \"off\")", self.audit));
            self.audit = "on".into();
        }
    }

    /// A `gate_tools` value other than "auto" / "off" falls back to "auto" with one note.
    fn check_gate_tools(&mut self) {
        if self.gate_tools != "auto" && self.gate_tools != "off" {
            self.notes.push(format!("gate_tools: auto — gate_tools = {:?} is not a setting (use \"auto\" or \"off\")", self.gate_tools));
            self.gate_tools = "auto".into();
        }
    }

    /// A `reason` value other than "auto" / "always" / "off" falls back to "auto" with one note ("" = off, quietly).
    fn check_reason(&mut self) {
        match self.reason.as_str() {
            "auto" | "always" | "off" => {}
            "" => self.reason = "off".into(),
            w => {
                self.notes.push(format!("reason: auto — reason = {w:?} is not a setting (use \"auto\", \"always\" or \"off\")"));
                self.reason = "auto".into();
            }
        }
    }

    /// The active connection replaces `base_url`, `model`, `api_key` and `max_ctx`, wherever they came from. Chosen by
    /// `--provider`, else `ELISA_PROVIDER`, else the file's `active`; `ELISA_BASE_URL` alone means that server, no connection.
    fn apply_connection(&mut self, forced: Option<&str>, home: Option<&Path>, getenv: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        use crate::provider::{Providers, Transport, ANTHROPIC, NONE, STREAMDENSE};
        let set = |k: &str| getenv(k).filter(|v| !v.is_empty());
        let chosen = forced.map(String::from).or_else(|| set("ELISA_PROVIDER"))
            .or_else(|| set("ELISA_BASE_URL").map(|_| NONE.to_string()));
        if chosen.as_deref() == Some(NONE) { return Ok(()); }
        let file = match home { Some(h) => Providers::load(h)?, None => Providers::default() };
        let name = chosen.unwrap_or_else(|| file.active.clone());
        if name.is_empty() { return Ok(()); }
        let p = file.providers.get(&name).ok_or_else(|| format!("no provider named {name:?} in ~/.config/elisa/providers.toml"))?;
        let key = p.key(&name, getenv)?;
        self.api_key = if key.is_empty() && (p.kind == STREAMDENSE || p.kind == ANTHROPIC) { "dummy".into() } else { key };
        self.base_url = p.address();
        self.model = p.model.clone();
        self.max_ctx = p.max_ctx;
        self.transport = p.transport();
        self.leaves = p.leaves_machine();
        self.vision = p.vision;
        if self.transport != Transport::Gateway { self.warm = "off".into(); }
        self.provider = name;
        Ok(())
    }

    /// `--base-url U [--type T]`. With `kind` the connection was already left out (`--provider none`): U is that kind of
    /// server. Without it the active connection's kind, model and size stay with U as the address; its key goes only
    /// to the same host (another host gets none); an MCP connection has no URL to replace, so U is the gateway.
    pub fn point_at(&mut self, u: &str, kind: Option<&str>) {
        use crate::provider::{host, is_local, Transport, ANTHROPIC, OPENAI};
        let u = u.trim().trim_end_matches('/').to_string();
        if let Some(k) = kind {
            self.transport = match k { ANTHROPIC => Transport::Anthropic, OPENAI => Transport::OpenAi, _ => Transport::Gateway };
            if self.transport != Transport::Gateway { self.warm = "off".into(); }
        } else if !self.provider.is_empty() {
            if matches!(self.transport, Transport::Mcp(_)) {
                self.transport = Transport::Gateway;
                self.provider.clear();
                self.api_key = "dummy".into();
            } else if host(&u) != host(&self.base_url) {
                self.api_key = if self.transport == Transport::OpenAi { String::new() } else { "dummy".into() };
            }
            self.leaves = if is_local(&u) { None } else { Some(host(&u).to_string()) };
        }
        // the gateway / Anthropic paths start with /v1 themselves (the watcher too): an OpenAI-style …/v1 loses it
        self.base_url = crate::api::request_base(&u, &self.transport);
    }

    /// `project`: the project's `.elisa/config.toml` (its `outside` may only tighten the setting so far)
    fn apply_file(&mut self, p: &Path, project: bool) -> Result<(), String> {
        let s = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut f: FileConfig = toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
        // secrets in tool output: a project config may only make it stricter (off < remote < always)
        if let Some(r) = f.redact.take() {
            let m = crate::secrets::Mode::parse(&r)
                .ok_or_else(|| format!("{}: redact = {r:?}: use \"remote\", \"always\" or \"off\"", p.display()))?;
            let cur = crate::secrets::Mode::parse(&self.redact).unwrap_or(crate::secrets::Mode::Remote);
            let new = if project { cur.stricter(m) } else { m };
            if project && new != m {
                self.notes.push(format!("{}: redact = {r:?} ignored: a project config can only make it stricter (now {:?})", p.display(), new.word()));
            }
            self.redact = new.word().to_string();
        }
        if let Some(o) = f.outside.take() {
            let m = crate::outside::Mode::parse(&o)
                .ok_or_else(|| format!("{}: outside = {o:?}: use \"ask\", \"read-ok\", \"deny\" or \"allow\"", p.display()))?;
            let cur = crate::outside::Mode::parse(&self.outside).unwrap_or(crate::outside::Mode::Ask);
            self.outside = if project { cur.stricter(m) } else { m }.word().to_string();
        }
        // the shell sandbox: a bad value keeps the current one (one note); a project may only make it stricter
        if let Some(w) = f.shell_sandbox.take() {
            use crate::sandbox::Mode;
            let cur = Mode::parse(&self.shell_sandbox).unwrap_or(Mode::platform_default());
            match Mode::parse(&w) {
                None => self.notes.push(format!("{}: shell_sandbox = {w:?} is not a setting (use \"write\", \"strict\" or \"off\"): keeping \"{}\"", p.display(), cur.word())),
                Some(m) if project && cur.stricter(m) != m => self.notes.push(format!("{}: shell_sandbox = {w:?} ignored: a project config can only make the shell sandbox stricter (now \"{}\")", p.display(), cur.word())),
                Some(m) => self.shell_sandbox = m.word().to_string(),
            }
        }
        // a project's .elisa/config.toml can turn web access off but not pick the search server: only the user's own
        // config decides where queries go
        if let Some(w) = f.web.take() {
            if !project || w.is_empty() || w == "off" { self.web = w; } else {
                self.notes.push(format!("{}: web = {w:?} ignored: a project config can only turn web off (web = \"off\"); \
                    the search method comes from ~/.config/elisa/config.toml", p.display()));
            }
        }
        if let Some(u) = f.searxng_url.take() {
            if !project { self.searxng_url = u; } else if !u.is_empty() {
                self.notes.push(format!("{}: searxng_url ignored: only ~/.config/elisa/config.toml picks the search server", p.display()));
            }
        }
        // a secret: only the user's own config holds it (a project's file may be shared or checked in); the note
        // never repeats the value
        if let Some(k) = f.brave_api_key.take() {
            if !project { self.brave_api_key = k; } else if !k.is_empty() {
                self.notes.push(format!("{}: brave_api_key ignored: only ~/.config/elisa/config.toml (or {}) holds the key", p.display(), crate::web::BRAVE_KEY_ENV));
            }
        }
        // the sandbox runs model-written code: only the user's own config switches it on; a project may switch it off
        // or shorten its time limit
        if let Some(sb) = f.sandbox.take() {
            if !project || sb.is_empty() || sb == "off" { self.sandbox = sb; } else {
                self.notes.push(format!("{}: sandbox = {sb:?} ignored: a project config can only switch the sandbox off (sandbox = \"off\"); \
                    switch it on in ~/.config/elisa/config.toml", p.display()));
            }
        }
        if let Some(t) = f.sandbox_timeout.take() {
            if !project || t <= self.sandbox_timeout_s { self.sandbox_timeout_s = t; } else {
                self.notes.push(format!("{}: sandbox_timeout = {t} ignored: a project config can only lower it (now {} s)", p.display(), self.sandbox_timeout_s));
            }
        }
        // the documents server reads project files: a project may switch it off, never on (only the user's config)
        if let Some(d) = f.docs.take() {
            if !project || d.is_empty() || d == "off" || d == self.docs { self.docs = d; } else {
                self.notes.push(format!("{}: docs = {d:?} ignored: a project config can only switch the documents server off (docs = \"off\"); \
                    switch it on in ~/.config/elisa/config.toml", p.display()));
            }
        }
        // the security server reads project files and can reach osv.dev: a project may switch it off, never on
        if let Some(s) = f.security.take() {
            if !project || s.is_empty() || s == "off" || s == self.security { self.security = s; } else {
                self.notes.push(format!("{}: security = {s:?} ignored: a project config can only switch the security server off (security = \"off\"); \
                    switch it on in ~/.config/elisa/config.toml", p.display()));
            }
        }
        // the reasoning tools: a project may switch them off, never on or to "always" (only the user's config)
        // reasoning on simple turns: a project may switch it off (less waste), never on (only the user's config)
        if let Some(t) = f.think_simple.take() {
            if !project || t == "off" || t == self.think_simple { self.think_simple = t; } else {
                self.notes.push(format!("{}: think_simple = {t:?} ignored: a project config can only set think_simple = \"off\"; \
                    change it in ~/.config/elisa/config.toml", p.display()));
            }
        }
        // tool gating: a project may switch it on ("auto": fewer tools per turn), never off (only the user's config)
        if let Some(g) = f.gate_tools.take() {
            if !project || g == "auto" || g == self.gate_tools { self.gate_tools = g; } else {
                self.notes.push(format!("{}: gate_tools = {g:?} ignored: a project config can only set gate_tools = \"auto\"; \
                    change it in ~/.config/elisa/config.toml", p.display()));
            }
        }
        if let Some(r) = f.reason.take() {
            if !project || r.is_empty() || r == "off" || r == self.reason { self.reason = r; } else {
                self.notes.push(format!("{}: reason = {r:?} ignored: a project config can only switch the reasoning tools off (reason = \"off\"); \
                    change it in ~/.config/elisa/config.toml", p.display()));
            }
        }
        // the audit log: a project may switch it off, never on (only the user's config)
        if let Some(a) = f.audit.take() {
            if !project || a == "off" || a == self.audit { self.audit = a; } else {
                self.notes.push(format!("{}: audit = {a:?} ignored: a project config can only switch the audit log off (audit = \"off\")", p.display()));
            }
        }
        self.apply_mcp(p, project, f.mcp.take());
        if f.max_ctx.is_some() { self.max_ctx = f.max_ctx; }
        if f.system_prompt_file.is_some() { self.system_prompt_file = f.system_prompt_file; }
        if let Some(d) = f.memory_dir { self.memory_dir = expand(d); }
        overlay!(self, f; base_url, model, api_key, max_output, compact_at, keep_recent_turns,
            tool_output_max_chars, bash_timeout_s, auto_approve, max_steps, think, memory, fold_at, fold_to, fold_max_turns, recall_k, recall_min_score, recall_min_support, recall_max_tokens, character, theme, sleep_after_s, companion,
            checks, check_rounds, check_timeout_s, check_test_timeout_s, check_output_max_chars, ast, ast_guard, ast_targeted_checks, metrics, warm);
        if !matches!(self.warm.as_str(), "auto" | "on" | "off") { return Err(format!("{}: warm = {:?}: use \"auto\", \"on\" or \"off\"", p.display(), self.warm)); }
        Ok(())
    }

    /// `[mcp.NAME]` tables. MCP servers run commands: only the user's own config adds them; a project config may only
    /// switch one off. A mistake skips that one server with a note (`notes`); it never stops Elisa from starting, and
    /// a skipped server never starts.
    fn apply_mcp(&mut self, p: &Path, project: bool, mcp: Option<toml::Value>) {
        let Some(mcp) = mcp else { return };
        let Some(t) = mcp.as_table() else {
            self.notes.push(format!("{}: mcp ignored: use [mcp.NAME] tables", p.display()));
            return;
        };
        for (name, v) in t {
            let skip = |why: &str| format!("mcp {name}: not started — {why} ({} [mcp.{name}])", p.display());
            let m = match parse_mcp(v) { Ok(m) => m, Err(e) => { self.notes.push(skip(&e)); continue; } };
            if m.enabled == Some(false) {
                self.mcp.retain(|c| &c.name != name);
                if !self.mcp_off.contains(name) { self.mcp_off.push(name.clone()); }
                continue;
            }
            let adds = !m.command.is_empty() || !m.url.is_empty();
            let filtered = m.tools.is_some() || m.exclude.is_some();
            if project {
                if adds {
                    self.notes.push(skip("a project config cannot add MCP servers (they run programs); add it to ~/.config/elisa/config.toml"));
                    continue;
                }
                // a project's tool filter only narrows: its allowlist is one more that every tool must pass, its
                // denylist adds to the user's; neither can bring back a tool the user config left out
                if filtered {
                    let f = self.mcp_filter.entry(name.clone()).or_default();
                    if let Some(t) = m.tools { f.allow.push(t); }
                    f.deny.extend(m.exclude.unwrap_or_default());
                }
                if !filtered || m.enabled == Some(true) {
                    self.notes.push(format!("{}: [mcp.{name}] enabled = true ignored: a project config can only switch servers off", p.display()));
                }
                continue;
            }
            // the user config sets the server's filter (a user server's, or a built-in's: `[mcp.docs] exclude = […]`)
            let builtin = !adds && BUILTIN_SERVERS.contains(&name.as_str());
            if filtered && (adds || builtin) {
                self.mcp_filter.insert(name.clone(), crate::mcp_host::ToolFilter { allow: m.tools.into_iter().collect(), deny: m.exclude.unwrap_or_default() });
            }
            if builtin && (filtered || name == "web") {
                // `[mcp.web] enabled = true` in the user config: the built-in web server back on
                if name == "web" || m.enabled == Some(true) { self.mcp_off.retain(|n| n != name); }
                continue;
            }
            if let Err(e) = check_server_name(name) { self.notes.push(skip(&e)); continue; }
            if !m.command.is_empty() && !m.url.is_empty() {
                self.notes.push(skip("give either command or url, not both"));
                continue;
            }
            if !adds {
                self.notes.push(skip("give command = [\"program\", \"args\"…] (stdio) or url = \"https://…\" (HTTP)"));
                continue;
            }
            self.mcp_off.retain(|n| n != name);
            self.mcp.retain(|c| &c.name != name);
            self.mcp.push(crate::mcp_host::ServerCfg { name: name.clone(), command: m.command, url: m.url, env: m.env, env_pass: m.env_pass, builtin: false, filter: Default::default() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a temp home with `user` as its config, and a project with `project` as its `.elisa/config.toml`
    fn load(tag: &str, user: &str, project: &str) -> Config {
        let d = std::env::temp_dir().join(format!("elisa-config-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (home, proj) = (d.join("home"), d.join("proj"));
        std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
        std::fs::create_dir_all(proj.join(".elisa")).unwrap();
        std::fs::write(home.join(".config/elisa/config.toml"), user).unwrap();
        if !project.is_empty() { std::fs::write(proj.join(".elisa/config.toml"), project).unwrap(); }
        let c = Config::load_from(&proj, Some(home), |_| None).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        c
    }

    #[test]
    fn tool_filters_for_user_and_builtin_servers() {
        let c = load("filters", "[mcp.browser]\ncommand = [\"npx\", \"pw\"]\ntools = [\"navigate\", \"browser_click\"]\nexclude = [\"click\"]\n\
            [mcp.sec]\nexclude = [\"deps\"]\n[mcp.web]\ntools = [\"search\"]\n[mcp.nocmd]\ntools = [\"x\"]\n", "");
        let b = &c.mcp_filter["browser"];
        assert_eq!(b.allow, [vec!["navigate".to_string(), "browser_click".into()]]);
        assert!(b.allows("browser", "browser_navigate") && !b.allows("browser", "browser_click"));
        assert_eq!(c.mcp_filter["sec"].deny, ["deps"]);
        assert!(!c.mcp_filter["sec"].allows("sec", "sec_deps") && c.mcp_filter["sec"].allows("sec", "sec_jwt"));
        assert!(c.mcp_filter["web"].allows("web", "web_search") && !c.mcp_filter["web"].allows("web", "web_fetch"));
        // built-ins take a filter without a command; the server list holds only the user server
        assert_eq!(c.mcp.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["browser"]);
        assert_eq!(c.notes.len(), 1, "{:?}", c.notes);
        assert!(c.notes[0].contains("mcp nocmd: not started — give command"), "{:?}", c.notes);
        // a bad value skips that server with a note
        let c = load("filters-bad", "[mcp.x]\ncommand = [\"x\"]\ntools = \"navigate\"\n", "");
        assert!(c.mcp.is_empty() && c.notes[0].contains("tools must be a list of tool names"), "{:?}", c.notes);
    }

    #[test]
    fn a_project_tool_filter_only_narrows() {
        let user = "[mcp.browser]\ncommand = [\"npx\", \"pw\"]\ntools = [\"navigate\", \"click\", \"snapshot\"]\n";
        let project = "[mcp.browser]\ntools = [\"navigate\", \"snapshot\", \"hover\"]\nexclude = [\"snapshot\"]\n[mcp.docs]\nexclude = [\"sheet\"]\n";
        let c = load("narrow", user, project);
        assert!(c.notes.is_empty(), "{:?}", c.notes);
        let b = &c.mcp_filter["browser"];
        assert!(b.allows("browser", "browser_navigate"));
        assert!(!b.allows("browser", "browser_click"), "the project's allowlist narrows");
        assert!(!b.allows("browser", "browser_snapshot"), "the project's exclude adds");
        assert!(!b.allows("browser", "browser_hover"), "a project cannot add back a tool the user left out");
        assert!(!c.mcp_filter["docs"].allows("docs", "docs_sheet") && c.mcp_filter["docs"].allows("docs", "docs_pdf"));
        // the server itself still comes from the user config only
        assert_eq!(c.mcp.len(), 1);
        // a project cannot widen by replacing: with no user filter its allowlist is the only one; enabled = true is noted
        let c = load("narrow2", "[mcp.browser]\ncommand = [\"npx\"]\nexclude = [\"evaluate\"]\n", "[mcp.browser]\nenabled = true\ntools = [\"evaluate\", \"click\"]\n");
        let b = &c.mcp_filter["browser"];
        assert!(!b.allows("browser", "browser_evaluate") && b.allows("browser", "browser_click") && !b.allows("browser", "browser_hover"));
        assert!(c.notes.iter().any(|n| n.contains("enabled = true ignored")), "{:?}", c.notes);
    }
}
