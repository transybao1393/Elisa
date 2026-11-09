//! CLI, agent loop, slash commands, approvals. Contract in DESIGN.md. All display and input goes through `Frontend`.
use crate::api::{Client, Request};
use crate::config::Config;
use crate::context::{self, estimate_messages, estimate_tokens, Store};
use crate::checks;
use crate::frontend::{Approval, Args, Banner, CheckEvent, CheckStatus, Frontend, Outside, Progress, COMMANDS};
use crate::outside;
use crate::perms::Level;
use crate::commands::FileCommand;
use crate::plugins;
use crate::images;
use serde_json::json;
use crate::metrics;
use crate::tools::{self, Access, ToolCtx};
use crate::types::{Block, Message, Reply, Role, ToolDef};
use crate::ui;
use crate::watcher::Watcher;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use elisa_mem::{HitKind, Memory, Role as MRole};

mod agent_loop;
mod approve;
mod commands;
mod docs;
mod memory;
mod startup;
mod turn;

pub use startup::{main_entry, parse_args, run_session, ASKING, AT_PROMPT, CANCEL};
use agent_loop::AstStats;

const COMPACT_SYSTEM: &str = "You write short, concrete session notes for an assistant.";
const COMPACT_OUT: usize = 800;
/// the output cap of the isolated picture request (`look`): a description, or the answer to one question about it
const PICTURE_OUT: usize = 320;
const TOOL_PREAMBLE: usize = 240;
/// expected length of a fold summary in chars (progress = chars so far / this, clamped to 0.95)
const FOLD_EXPECT_CHARS: f32 = 1200.0;
/// a step slower than this shows a progress line (next time, or live for elisa-mem's own progress)
const SLOW: Duration = Duration::from_millis(150);
/// before exit (plain display): wait this long for watcher notes and the last view still in flight
const WATCH_EXIT_WAIT: Duration = Duration::from_secs(1);
/// at exit, index pending memories only if that should take less than this; the rest stays on disk for next time
const EXIT_INDEX_BUDGET_S: f64 = 3.0;
/// at exit with no measured speed yet, index only this many memories
const EXIT_INDEX_UNKNOWN: usize = 8;
/// measured embedding speed (memories/s), kept in memory_dir across sessions
const RATE_FILE: &str = "elisa-index-rate";
/// compact the memory store after a fold once its hot log is bigger than this
const COMPACT_HOT_BYTES: u64 = 4 << 20;
/// token cap on the pinned facts carried in the `[state]` block of a first message (newest kept first)
const PINNED_CAP: usize = 150;
/// smallest max_tokens a request is sent with (a near-full window still gets room to answer)
const MIN_OUT: usize = 256;
/// messages the relevance gate looks back over (`recent_text`)
const RECENT_MSGS: usize = 6;
/// system + tools above this share (percent) of the usable context gets a startup warning
const FIXED_WARN_PCT: usize = 30;
/// share of the limit a big step's results leave free for the turn's later steps (small results still append whole
/// there, so a full window does not force a rewrite one step later)
const STEP_TAIL: usize = 8;
const WARM_UNKNOWN: u8 = 0;
const WARM_YES: u8 = 1;
const WARM_NO: u8 = 2;
/// the user message that carries a step's tool results (role + framing)
const MSG_OVERHEAD_TOKENS: usize = 8;

/// Graph memory for one Elisa session (opening Elisa until Ctrl+C / exit; /clear starts a new one).
struct Mem {
    db: Memory,
    session: u64,
    /// cwd's absolute path
    project: String,
}

struct Session<'f> {
    fe: &'f mut dyn Frontend,
    cfg: Config,
    client: Client,
    store: Store,
    tctx: ToolCtx,
    cwd: PathBuf,
    max_ctx: usize,
    /// the model can see pictures (`provider::vision`): the app enables its image button on this
    vision: bool,
    /// pictures that came with the input line just read: the model is asked about them alone (`look`), and its
    /// answer goes into this turn's user message as text
    attach: Vec<Block>,
    /// the pictures of the last turn that had some, and whether the next turn asks about them again (`look`)
    last_pics: Vec<Block>,
    pics_next: bool,
    /// what the last picture turn learned about its pictures (the picture's answer and the ask), for telling whether
    /// the next turn is about them (`context::about_picture`, BUGS #57)
    last_pic_seen: String,
    /// this turn's picture text answer, new or carried (the stand-in for a reply that was only the picture text)
    turn_pic: Option<String>,
    /// a session was just resumed: its first turn carries the user's facts in the third person (`resumed_facts`)
    resumed_facts: bool,
    /// the values of the user's facts that recall brought back this session (`claimed_facts_retry`)
    recalled_values: Vec<String>,
    /// documents that came with the input line just read (`attach.rs`): each one's reference line (the only thing
    /// the history keeps: `[Attached: report.pdf, 38 pages · .elisa/uploads/…]`) and whether it is large (then the
    /// turn is routed to the `docs` tools)
    docs: Vec<(String, bool)>,
    /// this turn's document text (the whole text, an overview or the parts the question needs): added to the turn's
    /// user message in this turn's requests only, never to the history, the saved session or a fold
    doc_req: Option<String>,
    /// its estimated tokens (counted with system + tools while the turn runs)
    doc_req_tokens: usize,
    /// documents waiting for the next turn: the app's `send` {documents} or `-p … --attach FILE`
    doc_paths: Vec<String>,
    /// documents attached in this session (text redacted), newest last: every later turn about one picks its parts
    /// again for that request (`doc_follow_up`)
    kept_docs: Vec<KeptDoc>,
    /// this turn carried document text: (document name, the block to retry with when the reply refuses it)
    doc_retry: Option<(String, String)>,
    /// every state card of this conversation with its topic key (`topics.rs`): a turn sends at most the one its
    /// words pick, never one of another topic
    topic_cards: Vec<crate::topics::Card>,
    /// this turn's topic (between turns: the last turn's), indexes into `kept_docs` and `topic_cards`
    topic: crate::topics::Pick,
    /// the kept document attached with the input line just read (it is this turn's topic)
    doc_new: Option<usize>,
    /// every document attached with the input line just read was refused (password-protected, not a document): the
    /// line that tells the model so, in the turn's own message; the turn gets no earlier document, no recall, no memory
    doc_refused: Option<String>,
    /// this turn's picked card as a `[state]` block: in the turn's user message for its requests only, like `doc_req`
    card_req: Option<String>,
    /// MCP servers plugged into this session (config `[mcp.NAME]` + the built-in `web`): their tools sit next to Elisa's
    mcp: crate::mcp_host::Host,
    tools: Vec<ToolDef>,
    tools_tokens: usize,
    project: Option<String>,
    notes: Option<String>,
    /// the notes are background from an earlier session (a new session, or after /clear or /reset), not this
    /// session's task: the first message marks them so and leaves their old session goal out (`context::background_notes`)
    notes_bg: bool,
    system: String,
    system_tokens: usize,
    history: Vec<Message>,
    /// (history len, real prompt+output tokens) from the last reply; the estimate builds on it until history is rewritten.
    real: Option<(usize, usize)>,
    /// (real prompt tokens, text estimate of that same prompt) from the last reply: after a rewrite (`real` = None)
    /// the text estimate is scaled by this ratio when it ran low, so a fit decision never trusts the estimate alone
    calib: Option<(usize, usize)>,
    /// Raw text of the current turn's user input (re-sent with the new notes after an auto-compact).
    input: String,
    interactive: bool,
    /// sandbox images the user said no to this session: a run that has a present line to fall back to is not asked
    /// about them again
    declined_images: Vec<&'static str>,
    yolo: bool,
    always: HashSet<String>,
    mem: Option<Mem>,
    /// turns recorded in this memory session (a turn = one user text or one assistant reply)
    turn_no: usize,
    /// turns already folded into notes (turn_no - folded = unfolded turns), and the unfolded turns' memory ids
    folded: usize,
    fold_ids: Vec<u64>,
    /// after a failed fold: don't retry below this window size (tokens)
    fold_retry_at: usize,
    /// a summary request ran out on reasoning: later folds use `context::digest` without asking the model again
    digest_only: bool,
    /// inside `turn` (a fold then keeps the current request in the first message)
    in_turn: bool,
    /// a fold already ran in the current user turn: no second fold until the next one
    folded_this_turn: bool,
    /// `[tool args → result]` lines of the current turn, recorded with the assistant reply
    tool_lines: Vec<String>,
    /// the current turn's request failed before anything was answered (nothing to record)
    unsent: bool,
    /// this input line already sent its turn_done (a panic after it must not send a second one)
    done_sent: bool,
    /// the current turn's steps in plain words for the display (src/activity.rs)
    acts: crate::activity::Turn,
    /// the last recall / session save was slow: show a progress line for the next one
    recall_slow: bool,
    save_slow: bool,
    /// this conversation's last turn (unix seconds; `--continue`: the saved session's mtime) and whether it failed:
    /// the situation line of a small-talk turn (`situation.rs`)
    last_turn_at: Option<i64>,
    last_failed: bool,
    /// this turn is small talk routed as chat ("hey", "thanks", "ok"): a tool call is not run or widened to, but
    /// asked again in words (`agent_loop`)
    small_talk_turn: bool,
    /// the note of a one-time words-only request with no tools (`agent_loop` starts with it; `denied_claim_retry`)
    retry_note: Option<String>,
    /// latest state card: text, memory id, and the exchange it came from (fresh = from the last <= 2 exchanges)
    card: Option<String>,
    card_id: Option<u64>,
    card_ex: Option<usize>,
    /// completed exchanges (user turn + final reply) in this run
    exchanges: usize,
    /// card parsed from the current turn's final reply (None + `card_broken` = none or broken)
    new_card: Option<String>,
    card_broken: bool,
    /// this turn's message came with a picture the model cannot see (`take_attached`; `no_picture_line`)
    pic_refused: bool,
    /// measured embedding speed (memories per second), for the exit budget
    index_rate: Option<f64>,
    /// memories added since the last index_pending (this run)
    unindexed: usize,
    /// the gateway's domain watcher: notes after cards and folds; view refreshed after turns and folds
    watch: Watcher,
    /// --no-checks: no check-before-done after edits
    checks_off: bool,
    /// config `outside` (src/outside.rs): what a tool call outside the project folder does
    outside_mode: outside::Mode,
    /// "allow this path for the session" answers: (resolved path, write); never saved
    outside_ok: Vec<(PathBuf, bool)>,
    /// outline / read_symbol tokens vs the whole files they stood in for (/context)
    ast_stats: AstStats,
    /// state cards made in this run (the card number when memory is off)
    cards: u64,
    /// per-request metrics (src/metrics.rs): the request in progress, and the session totals for `/stats`
    req: Option<ReqRun>,
    stats: metrics::Stats,
    /// state cards since the notes were last written: a card fold merges them into the notes (no model call)
    card_log: Vec<String>,
    /// the session goal (`context::next_goal`): set from the first real request, changed only by a stated new goal;
    /// `[state]` shows it apart from the current request and the notes keep it on top
    goal: Option<String>,
    /// requests in a row that had nothing to do with `goal` (`context::goal_drift`)
    goal_misses: u8,
    /// `goal` came from an explicit goal statement ("our goal for this session: …"): it does not drift
    goal_stated: bool,
    /// the goal state before this request (`req_begin`): a cancelled turn puts it back (`drop_cancelled_turn`)
    goal_before: (Option<String>, u8, bool),
    /// notes facts this run already found in its conversation (`context::fact_key`): a later fold keeps them though
    /// their turns are folded away (`merge_kept`)
    facts_ok: std::collections::HashSet<String>,
    /// the current request in the user's words (one line)
    user_req: String,
    /// the gateway's prefill-only warm-up after a fold (config `warm`): WARM_UNKNOWN until /health or the first try
    /// says; a background thread sets it from the answer
    warm: Arc<AtomicU8>,
    /// the reasoning tools (`think` / `steps` / `step_done` / `calc`, `mcp_reason.rs`) are in this turn's request:
    /// decided once at turn start (`reason_turn`), so the tool list never changes mid-turn
    reason_on: bool,
    /// this turn's intent route (`route.rs`, config `route`): the tool groups its requests carry
    route: crate::route::Route,
    /// the hosted intent router (`router.rs`; providers.toml `[router]`): off without a key
    router: crate::router::Router,
    /// the router's answer for this turn; None = no router or a miss, and the word rules decide as before
    routed: Option<crate::router::Answer>,
    /// a picture was refused in this session (the router's `image_refused`)
    pic_refused_seen: bool,
    /// the relevance gate's choice for this turn (`mcp_reason::gate`, config `gate_tools`): (`docs_*`, `sec_*`) offered
    gate: (bool, bool),
    /// what Elisa's own tools changed this session (`context::Ledger`): the notes' file list and the check of a
    /// summary's "done" claims
    ledger: context::Ledger,
    /// the loop guard of the user turn in progress (`loop_guard.rs`): one per turn, shared by the turn's retries
    /// (`denied_claim_retry`, `claimed_facts_retry`), so a retry never runs the same calls afresh (BUGS #86)
    pub(super) turn_guard: crate::loop_guard::Guard,
    /// the audit log (`audit.rs`; config `audit`): off = records nothing
    audit: crate::audit::Log,
    /// how the user answered the approval of the call in progress (None: no one was asked), for the audit log
    answer: Option<&'static str>,
    /// the home folder (plugins, the user config a saved rule goes to); None without HOME
    home: Option<PathBuf>,
    /// the file slash commands in effect (`commands.rs`), merged from the user's, the plugins' and the project's
    file_cmds: Vec<FileCommand>,
    cmd_user: Vec<FileCommand>,
    cmd_plugins: Vec<FileCommand>,
    cmd_project: Vec<FileCommand>,
    /// the installed plugins (as /plugin list shows them) and the (plugin, server) pairs running in this session
    plugin_infos: Vec<plugins::Info>,
    plugin_servers: Vec<(String, String)>,
    /// the plugin state file as last seen (`plugins::state_stamp`): a change made outside this session is synced
    plugin_stamp: Option<(std::time::SystemTime, u64)>,
    /// (tool, plugin): tools of plugins removed or switched off outside this session, for a clear refusal
    plugin_gone_tools: Vec<(String, String)>,
    /// the project's own commands and plugins, waiting for the one-time trust prompt (None: none, or loaded)
    proj_pending: Option<plugins::Project>,
    proj_infos: Vec<plugins::Info>,
    /// the user said no to the project's trust prompt in this session: not asked again until /plugin trust project
    proj_declined: bool,
}

struct ReqRun { ts: u64, t0: Instant, prompt: String }

/// a document attached in this session: its text, the last question about it, and the turns since it was last
/// referred to (`Session::doc_follow_up`)
struct KeptDoc { doc: crate::attach::Doc, query: String, topic: crate::topics::DocTopic }

impl KeptDoc {
    fn new(doc: crate::attach::Doc, query: String) -> Self {
        let mut topic = crate::topics::DocTopic::new(&doc.name, &crate::attach::title(&doc), &doc.text);
        if doc.part == "page" { topic.pages = doc.parts.len(); }
        KeptDoc { doc, query, topic }
    }
}

enum TurnEnd { Done, Stopped, Failed(String) }

fn end_name(e: &TurnEnd) -> &'static str {
    match e { TurnEnd::Done => "done", TurnEnd::Stopped => "stopped", TurnEnd::Failed(_) => "failed" }
}
impl<'f> Session<'f> {

    fn new(fe: &'f mut dyn Frontend, mut cfg: Config, client: Client, cwd: PathBuf, max_ctx: usize, interactive: bool, yes: bool) -> Session<'f> {
        let store = Store::new(&cwd);
        let tctx = ToolCtx { cwd: cwd.clone(), max_chars: cfg.tool_output_max_chars, bash_timeout_s: cfg.bash_timeout_s, ast_guard: cfg.ast_guard };
        crate::sandbox::set_mode(crate::sandbox::Mode::parse(&cfg.shell_sandbox).unwrap_or(crate::sandbox::Mode::platform_default()));
        // what config loading skipped or ignored (a bad [mcp.NAME], a bad `web` value): one line each, nothing when clean
        for n in &cfg.notes { fe.info(n); }
        // the MCP servers: the configured ones, and Elisa's own web server first unless it is off or replaced
        let mut servers = cfg.mcp.clone();
        let web_on = !cfg.web.is_empty() && cfg.web != "off" && !cfg.mcp_off.iter().any(|n| n == "web");
        let web_in = web_on && !servers.iter().any(|c| c.name == "web");
        if web_in { servers.insert(0, crate::mcp_host::builtin_web(&cfg.web, &cfg.searxng_url, &cfg.brave_api_key)); }
        // the documents server (`docs_pdf` / `docs_sheet`), right after the web server, unless `docs = "off"` or replaced
        let docs_on = cfg.docs == "on" && !cfg.mcp_off.iter().any(|n| n == "docs");
        let docs_in = docs_on && !servers.iter().any(|c| c.name == "docs");
        if docs_in { servers.insert(usize::from(web_in), crate::mcp_host::builtin_docs(&cwd, cfg.tool_output_max_chars)); }
        // the security server (`sec_*`), after the documents server, unless `security = "off"` or replaced
        let sec_on = cfg.security == "on" && !cfg.mcp_off.iter().any(|n| n == "sec");
        if sec_on && !servers.iter().any(|c| c.name == "sec") {
            servers.insert(usize::from(web_in) + usize::from(docs_in), crate::mcp_host::builtin_sec(&cwd, cfg.tool_output_max_chars));
        }
        // the code sandbox (`run_code`): only when the user switched it on (`sandbox = "colima"`)
        let run_on = cfg.sandbox == "colima" && !cfg.mcp_off.iter().any(|n| n == "run");
        if run_on && !servers.iter().any(|c| c.name == "run") { servers.push(crate::mcp_host::builtin_run(cfg.sandbox_timeout_s, cfg.sandbox_fetch_timeout_s, &cwd)); }
        // the reasoning tools for small models, last, unless `reason = "off"` or replaced (offered per turn: `reason_turn`)
        let reason_srv = cfg.reason != "off" && !cfg.mcp_off.iter().any(|n| n == crate::mcp_reason::NAME);
        if reason_srv && !servers.iter().any(|c| c.name == crate::mcp_reason::NAME) { servers.push(crate::mcp_host::builtin_reason()); }
        // each server's tool filter (`[mcp.NAME] tools` / `exclude`; the built-in ones by their name too)
        for c in &mut servers { if let Some(f) = cfg.mcp_filter.get(&c.name) { c.filter = f.clone(); } }
        // enabled plugins (`plugins.rs`): their servers next to the configured ones, their rules and commands
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let taken: Vec<String> = servers.iter().map(|c| c.name.clone()).chain(crate::config::BUILTIN_SERVERS.iter().map(|s| s.to_string())).collect();
        let pl = home.as_ref().map(|h| plugins::load_user(h, &cwd, &|n| taken.iter().any(|t| t == n))).unwrap_or_default();
        for n in &pl.notes { fe.info(n); }
        for (lv, p, src) in &pl.rules { if let Err(e) = cfg.perms.add(*lv, p, src) { fe.info(&format!("{src}: rule {p:?} skipped: {e}")); } }
        let plugin_servers: Vec<(String, String)> = pl.servers.iter().map(|(p, c)| (p.clone(), c.name.clone())).collect();
        servers.extend(pl.servers.into_iter().map(|(_, c)| c));
        let own: Vec<String> = tools::defs_for(true).into_iter().map(|d| d.name).collect();
        // no server process starts here: each starts on the first call to one of its tools (`Host::open`)
        let (mut mcp, mcp_notes) = crate::mcp_host::Host::open(&servers, &|n| own.iter().any(|o| o == n), home.as_deref().map(crate::pins::lists_in));
        for n in &mcp_notes { fe.info(n); }
        // filter names a server does not have: known only after its tools/list
        for n in mcp.filter_notes() { fe.info(n); cfg.notes.push(n.clone()); }
        // the audit log (servers started, from here on every tool call) and the tool-list pins of the servers that
        // are not Elisa's own (`pins.rs`): a changed list warns, and that server asks on every call until /trust
        let audit = match &home { Some(h) if cfg.audit == "on" => crate::audit::Log::open(h, &cwd), _ => crate::audit::Log::default() };
        for n in &mcp_notes { audit.note("server_start", &[("outcome", "error"), ("detail", n)]); }
        mcp.audit_to(&audit);
        if let Some(h) = &home {
            for (warn, n) in mcp.check_pins(&crate::pins::path_in(h)) { if warn { fe.error(&n) } else { fe.info(&n) } }
        }
        let (project, notes, always) = (store.load_project(), store.load_notes(), store.load_allowed());
        let watch = Watcher::new(&cfg.base_url, &cwd.display().to_string());
        if let Some(p) = fe.watcher_push() { watch.set_push(p); }
        if cfg.transport != crate::provider::Transport::Gateway { watch.disable(); }
        let plugin_stamp = home.as_deref().and_then(plugins::state_stamp);
        let router = crate::router::Router::new(cfg.router.clone());
        let mut s = Session {
            fe, cfg, client, store, tctx, cwd, max_ctx, vision: false, attach: Vec::new(), last_pics: Vec::new(), pics_next: false, last_pic_seen: String::new(), turn_pic: None, resumed_facts: false, recalled_values: Vec::new(), docs: Vec::new(), doc_req: None, doc_req_tokens: 0, doc_paths: Vec::new(), kept_docs: Vec::new(), doc_retry: None, topic_cards: Vec::new(), topic: Default::default(), doc_new: None, doc_refused: None, card_req: None, mcp, tools: Vec::new(), tools_tokens: 0, project, notes, notes_bg: true,
            system: String::new(), system_tokens: 0, history: Vec::new(), real: None, calib: None, input: String::new(), interactive, declined_images: Vec::new(), yolo: yes, always,
            mem: None, turn_no: 0, folded: 0, fold_ids: Vec::new(), fold_retry_at: 0, digest_only: false, in_turn: false, folded_this_turn: false, tool_lines: Vec::new(), unsent: false, done_sent: false, acts: crate::activity::Turn::new(),
            recall_slow: false, save_slow: false, last_turn_at: None, last_failed: false, small_talk_turn: false, retry_note: None, index_rate: None, unindexed: 0, watch,
            card: None, card_id: None, card_ex: None, exchanges: 0, new_card: None, card_broken: false, pic_refused: false, checks_off: false, outside_mode: outside::Mode::Ask, outside_ok: Vec::new(), ast_stats: AstStats::default(), cards: 0,
            req: None, stats: metrics::Stats::default(),
            card_log: Vec::new(), goal: None, goal_misses: 0, goal_stated: false, goal_before: (None, 0, false), facts_ok: Default::default(), user_req: String::new(), warm: Arc::new(AtomicU8::new(WARM_UNKNOWN)),
            reason_on: false, route: crate::route::Route::default(), router, routed: None, pic_refused_seen: false, gate: (false, false), ledger: context::Ledger::default(), turn_guard: Default::default(), audit, answer: None,
            home, file_cmds: Vec::new(), cmd_user: Vec::new(), cmd_plugins: pl.commands, cmd_project: Vec::new(),
            plugin_stamp, plugin_gone_tools: Vec::new(), plugin_infos: pl.infos, plugin_servers, proj_pending: None, proj_infos: Vec::new(), proj_declined: false,
        };
        s.load_commands();
        // the app's plugin pane: what is installed, from the start (nothing installed: no event; `/plugin list` asks)
        if !s.plugin_infos.is_empty() || !s.proj_infos.is_empty() { s.plugins_event(); }
        s.outside_mode = outside::Mode::parse(&s.cfg.outside).unwrap_or(outside::Mode::Ask);
        if s.cfg.gate_tools == "off" { s.gate = (true, true); }
        if s.cfg.route == "off" { s.route = crate::route::Route::all(); }
        s.rebuild_system();
        s
    }

    /// The only place the system prompt and tools change: startup, compaction, /clear, /compact, /forget, and a turn
    /// whose route (`route.rs`, config `route`) or reasoning / relevance gate differs from the last turn's.
    fn rebuild_system(&mut self) {
        (self.system, self.tools) = (context::system_prompt(&self.cwd, self.cfg.system_prompt_file.as_deref(), self.project.as_deref(), None), tools::defs_for(self.cfg.ast));
        // stable tools first (Elisa's own, then every MCP server that is always offered); the per-turn ones after them,
        // in a fixed order (`docs`, `sec`, then the reasoning tools), so a flip only changes the end of the list
        let defs = self.mcp.defs();
        let m = &self.mcp;
        let gated = |name: &str| m.reason_tool(name) || m.gated_server(name).is_some();
        let routing = self.cfg.route == "auto";
        self.tools.extend(defs.iter().filter(|t| !gated(&t.name)).cloned());
        for (i, srv) in crate::mcp_reason::GATED.iter().enumerate() {
            // routed: the documents / security tools are route groups (`route.rs`), not the old relevance gate
            if routing || [self.gate.0, self.gate.1][i] { self.tools.extend(defs.iter().filter(|t| m.gated_server(&t.name) == Some(*srv)).cloned()); }
        }
        // intent routing: only the tool groups this turn's words call for, in the same fixed order (a chat turn: none)
        if routing {
            let r = &self.route;
            self.tools.retain(|t| {
                let srv = m.server_of(&t.name);
                let g = crate::route::tool_group(&t.name, srv);
                if g == crate::route::OTHER { srv.is_some_and(|s| r.others.iter().any(|o| o == s)) } else { r.groups.has(g) }
            });
            self.system = crate::route::system_for(&self.system, self.route.groups);
        }
        // the reasoning tools only on a turn that wants them (`reason_turn`; the list stays fixed within a turn)
        if self.reason_on { self.tools.extend(defs.iter().filter(|t| m.reason_tool(&t.name)).cloned()); }
        // config `ast = false`: no syntax-tree tools, so no prompt line about them (fixed per session: byte-stable)
        if !self.cfg.ast { self.system = self.system.lines().filter(|l| !l.contains("`outline`")).map(|l| format!("{l}\n")).collect(); }
        // one line on `<untrusted>` fences, only when a tool can return outside text (`untrusted.rs`)
        if self.tools.iter().any(|t| self.untrusted_source(&t.name, &serde_json::Value::Null).is_some()) {
            if !self.system.ends_with('\n') { self.system.push('\n'); }
            self.system.push_str(crate::untrusted::SYSTEM_LINE);
            self.system.push('\n');
        }
        // the model's detected facts, one line (fixed per connection, so the prompt stays byte-stable within a session)
        let (m, v, t, th, ctx) = self.facts();
        let facts = crate::situation::model_facts(&m, v, t, th, ctx);
        if !self.system.ends_with('\n') { self.system.push('\n'); }
        self.system.push_str(&facts);
        self.system.push('\n');
        // a tiny window (`context::tiny_window`, from its size alone): a few lines of prompt and no tools, or the
        // prompt alone fills the window (BUGS #41, #42)
        if self.tiny() { (self.system, self.tools) = (context::TINY_SYSTEM.to_string(), Vec::new()); }
        // + the chat template's tool preamble (Qwen: ~236 tokens, measured with count_tokens)
        self.tools_tokens = if self.tools.is_empty() { 0 } else { estimate_tokens(&serde_json::to_string(&self.tools).unwrap_or_default()) + TOOL_PREAMBLE };
        self.real = None;
        self.system_tokens = estimate_tokens(&self.system);
    }
}
