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
pub mod brand;
pub mod shade;
pub mod tools;
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
pub mod repl;
pub mod tui;
pub mod ui;
pub mod watcher;
/// `--serve-jsonl`: the headless JSON-lines frontend for the native app (app/ElisaApp)
pub mod jsonl;
/// the scripted no-model gateway behind `--serve-jsonl --demo-states`
pub mod demo_gateway;
