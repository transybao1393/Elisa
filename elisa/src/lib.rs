//! Elisa: a small terminal coding agent. See DESIGN.md for the module contract.
pub mod types;
pub mod config;
pub mod api;
/// model connections: `~/.config/elisa/providers.toml` (local or cloud, directly or through MCP)
pub mod provider;
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
/// Elisa's code sandbox MCP server (`run_code` in a locked-down container on Colima; config `sandbox`)
pub mod mcp_run;
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
/// `<untrusted source=…>` fences around web / third-party MCP results, with injection-phrase warnings
pub mod untrusted;
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
/// the tool-loop guard of one turn: repeated identical calls, short cycles, steps without progress
pub mod loop_guard;
pub mod repl;
pub mod tui;
pub mod ui;
pub mod watcher;
/// `--serve-jsonl`: the headless JSON-lines frontend for the native app (app/ElisaApp)
pub mod jsonl;
/// the scripted no-model gateway behind `--serve-jsonl --demo-states`
pub mod demo_gateway;
