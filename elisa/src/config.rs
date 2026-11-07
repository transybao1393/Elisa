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
    /// the active connection of `providers.toml`; "" = none: `base_url` / `model` / `api_key` above as configured
    pub provider: String,
    /// how requests reach the model (`Gateway` unless a connection says otherwise)
    pub transport: crate::provider::Transport,
    /// the active connection sends the conversation here (None: it stays on this machine)
    pub leaves: Option<String>,
    /// the connection's `vision` setting (None: work it out at startup, `provider::vision`)
    pub vision: Option<bool>,
    /// web access for the model: "" = off, "duckduckgo" or "searxng" (from the connection, or config.toml `web`)
    pub web: String,
    /// the SearXNG server for `web = "searxng"`
    pub searxng_url: String,
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
            model: "qwen38-27b".into(),
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
            ast_targeted_checks: true,
            metrics: true,
            warm: "auto".into(),
            outside: "ask".into(),
            provider: String::new(),
            transport: crate::provider::Transport::Gateway,
            leaves: None,
            vision: None,
            web: String::new(),
            searxng_url: String::new(),
        }
    }
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
    ast_targeted_checks: Option<bool>,
    warm: Option<String>,
    metrics: Option<bool>,
    outside: Option<String>,
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
        c.base_url = c.base_url.trim_end_matches('/').to_string();
        Ok(c)
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
        self.web = p.web.clone();
        self.searxng_url = p.searxng_url.clone();
        if self.transport != Transport::Gateway { self.warm = "off".into(); }
        self.provider = name;
        Ok(())
    }

    /// `project`: the project's `.elisa/config.toml` (its `outside` may only tighten the setting so far)
    fn apply_file(&mut self, p: &Path, project: bool) -> Result<(), String> {
        let s = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut f: FileConfig = toml::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(o) = f.outside.take() {
            let m = crate::outside::Mode::parse(&o)
                .ok_or_else(|| format!("{}: outside = {o:?}: use \"ask\", \"read-ok\", \"deny\" or \"allow\"", p.display()))?;
            let cur = crate::outside::Mode::parse(&self.outside).unwrap_or(crate::outside::Mode::Ask);
            self.outside = if project { cur.stricter(m) } else { m }.word().to_string();
        }
        // web access runs without asking, so only the user's own config (or connection) may turn it on: a project's
        // .elisa/config.toml can only turn it off
        if let Some(w) = f.web.take() {
            if !project || w.is_empty() { self.web = w; }
        }
        if let Some(u) = f.searxng_url.take() { if !project { self.searxng_url = u; } }
        if f.max_ctx.is_some() { self.max_ctx = f.max_ctx; }
        if f.system_prompt_file.is_some() { self.system_prompt_file = f.system_prompt_file; }
        if let Some(d) = f.memory_dir { self.memory_dir = expand(d); }
        overlay!(self, f; base_url, model, api_key, max_output, compact_at, keep_recent_turns,
            tool_output_max_chars, bash_timeout_s, auto_approve, max_steps, think, memory, fold_at, fold_to, fold_max_turns, recall_k, recall_min_score, recall_min_support, recall_max_tokens, character, theme, sleep_after_s, companion,
            checks, check_rounds, check_timeout_s, check_test_timeout_s, check_output_max_chars, ast, ast_guard, ast_targeted_checks, metrics, warm);
        if !matches!(self.warm.as_str(), "auto" | "on" | "off") { return Err(format!("{}: warm = {:?}: use \"auto\", \"on\" or \"off\"", p.display(), self.warm)); }
        Ok(())
    }
}
