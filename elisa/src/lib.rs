//! Elisa: a small terminal assistant for local models, with coding tools. See DESIGN.md for the module contract.
pub mod types;
pub mod config;
pub mod api;
/// model connections: `~/.config/elisa/providers.toml` (local or cloud, directly or through MCP)
pub mod provider;
/// the optional hosted intent router (`[router]` in providers.toml): its endpoint contract and the intent → group map
pub mod router;
/// which server is behind a connection and what it can do (`Caps`): detection, cache features Elisa controls
pub mod backend;
/// the OpenAI-style chat completions transport
pub mod ollama;
pub mod web;
pub mod openai;
/// a model behind an MCP server
pub mod mcp;
pub mod mcp_host;
pub mod mcp_web;
/// `elisa --mcp-serve docs`: PDF and spreadsheet reading for the model, inside the project folder only
pub mod mcp_docs;
// documents attached to a message: copied into the project's upload area, read, inline or header (src/attach.rs)
pub mod attach;
/// Elisa's code sandbox MCP server (`run_code` in a locked-down container on Colima; config `sandbox`)
pub mod mcp_run;
/// the one pinned list of container images Elisa uses (sandbox languages, plugin installers), project version
/// detection, user-approved downloads (`/sandbox pull`) and their digest pins (`~/.elisa/images.lock`)
pub mod images;
/// `elisa --mcp-serve reason`: think / steps / step_done / calc for small models, offered only on multi-step turns
pub mod mcp_reason;
/// `elisa --mcp-serve sec`: secrets, dependency advisories (osv.dev), binary and JWT inspection, project folder only
pub mod mcp_sec;
/// clean-up of a model's MCP tool arguments against the tool's inputSchema before they are sent
pub mod mcp_args;
/// tool-list pins for MCP servers (`~/.elisa/mcp-pins.json`): a changed tool list loses "always allow" until `/trust`
pub mod pins;
/// the audit log (`~/.elisa/audit/<project>.jsonl`): tool calls, approvals, server start/stop, hash-chained; `/audit`
pub mod audit;
pub mod brand;
pub mod shade;
pub mod tools;
/// the shell sandbox around `bash` and the checks: a Seatbelt profile per project (config `shell_sandbox`, macOS)
pub mod sandbox;
/// secret detection and redaction (`find`, `redact`; config `redact`)
pub mod secrets;
/// Elisa's own result cache (`~/.elisa/cache`): web pages, searches, document text; cross-process safe; `/cache`
pub mod cache;
/// `<untrusted source=…>` fences around web / third-party MCP results, with injection-phrase warnings
pub mod untrusted;
/// pattern permissions (`[permissions] allow / ask / deny`: `shell(git status*)`, `write(src/**)`, `mcp:S:T(url)`)
pub mod perms;
/// file slash commands (`~/.elisa/commands/*.md`, plugins, a trusted project's `.elisa/commands`)
pub mod commands;
/// plugins: local folders with a plugin.toml bundling MCP servers (installed and run inside Colima), commands, rules
pub mod plugins;
/// work outside the project folder asks first (path resolution, bash parse, config `outside`)
pub mod outside;
/// tree-sitter outlines, symbols by name, the parse guard, touched symbols (tools outline / read_symbol / edit_symbol)
pub mod ast;
/// check before done: run the project's checks after edits, feed failures back (bounded rounds)
pub mod checks;
/// per-request metrics: model calls, review time, `.elisa/metrics.jsonl`, `/stats`
pub mod metrics;
pub mod context;
pub mod frontend;
/// what Elisa is doing in the background, in plain words: tool call → friendly phrase, result summary, reply summary
pub mod activity;
/// the tool-loop guard of one turn: repeated identical calls, short cycles, steps without progress
pub mod loop_guard;
/// intent routing (config `route`): which tool groups a turn carries, from cheap signals in the user's words
pub mod route;
/// reply in the user's language: a hint on an English / Vietnamese switch
pub mod reply_lang;
/// reply checks for any model: garbled scripts, a copied prompt, another language, an unfinished last sentence
pub mod reply_check;
/// the situation line of a small-talk turn: what context there is, so "hey" is answered from it
pub mod situation;
pub mod topics;
/// the follow-up hint of a short follow-up ("why?", "more", "the second one"): what it asks for
pub mod followup;
pub mod repl;
pub mod tui;
pub mod ui;
pub mod watcher;
/// `--serve-jsonl`: the headless JSON-lines frontend for the native app (app/ElisaApp)
pub mod jsonl;
/// `elisa eval`: the evaluation suite (cases in evals/*.toml, run through `--serve-jsonl`)
pub mod eval;
/// the scripted model behind `elisa eval`'s deterministic cases
pub mod eval_fake;
