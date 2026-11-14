# Commands and flags

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Running

In a project directory:

```sh
elisa                       # interactive; read/glob/grep/outline/read_symbol run without asking, write/edit/edit_symbol/bash ask
elisa --yes                 # approve everything inside the project (outside calls still ask, see safety.md)
elisa -p "fix the failing test" --yes   # one task, prints the final answer, exit 1 on an API error
elisa --continue            # reload the last saved session of this directory (its history, documents and cards)
elisa -p "summarise this" --attach ~/Downloads/report.pdf   # ask about a document (repeat --attach for more)
```

The mouse wheel scrolls the chat only (Elisa captures the mouse); to select text with the mouse hold fn while dragging
(Terminal.app) or option (iTerm2). `elisa/elisa-window [project dir]` opens Elisa in its own Terminal window titled
"Elisa", with the shell replaced by Elisa and the scrollback cleared first.

Other flags: `--provider NAME`, `--no-checks`, `--base-url U`, `--model M`, `--max-ctx N` (default: the window the server reports, e.g. the gateway's `/health` `info.max_ctx`; see [Backends](models.md#backends)), `--cwd DIR`.
Ctrl-C cancels the request or the running bash command (its whole process group); Ctrl-C twice quits; Ctrl-D exits.
Esc / Ctrl-C stops a model request within about 50 ms, also before the server sent its first byte (a long prefill):
the network reads run on their own thread, so a cancel never waits for the server (`api::post_lines`).
The status line after each reply shows tok/s, first-token time, real context use and `reused N` (KV prefix hit).
A reply with no decode window (a tool-call-only reply, or under 0.2 s of decode) shows `— tok/s` and its total time,
never a false 0.0; after Esc the context shown is Elisa's estimate when the server reported no usage.

Elisa never picks a model. With no model in the active connection, in `config.toml` or from `--model`, it sends
nothing and says how to choose one (app: Model Connections; see [Models and backends](models.md)).

## Slash commands

| Command | Action |
|---|---|
| `/help` | list the commands |
| `/clear` | summarise to `.elisa/notes.md`, then empty the history and drop the state card (the notes become background, as in a new session); later saves go to a new session file, so the pre-clear one stays resumable. The summary is **merged** into the earlier notes (what folds and earlier summaries recorded stays), and its file list comes from Elisa's own edit records (see [Notes](sessions-and-memory.md#notes)), never from the model's claim. The context comes out smaller when it can without losing anything real: notes longer than the conversation lose only low-value lines (new alias items, the model's own file list and the request quote first, then the newest lines of an overlong section); facts, lines of the earlier notes and Elisa's file records are never cut, so after a very short chat the context may not shrink at all. With memory on, a pinned fact the notes already say is not repeated in `[state]` |
| `/reset` | empty the history without a summary (notes kept); the state card is dropped for later sessions too (`.elisa/card-reset`) |
| `/compact` | summarise now and continue with notes + empty history |
| `/context` | estimated tokens of system / tools / history, the fold point, the tools' tokens by owner (Elisa's own, each MCP server offered this turn), the last turn's route (its tool groups and why, config `route`), whether the documents / security / reasoning tools are offered this turn, plus the gateway's real count |
| `/mcp [NAME]` | the MCP servers (see [MCP servers](mcp-and-plugins.md#mcp-servers)): each one's state, transport, tools offered (of how many, when filtered), their token cost, pin state and tool names; `/mcp NAME`: one server with each tool's description and the tools its filter leaves out |
| `/notes` | print the notes |
| `/forget` | forget what Elisa remembers in this project: delete the notes, unpin the pinned facts, stop recalling earlier chats (saved conversation files are kept) |
| `/resume` | load the last saved session (`.elisa/sessions/`) |
| `/stats` | metrics of the last request (each model call: kind agent / fold / summary, input, cached and output tokens, first-token time, total time, decode tok/s; wall time split into model time and the rest) and the session totals. One JSON line per request goes to `.elisa/metrics.jsonl` |
| `/cache [clear [KIND]]` | Elisa's result cache (see [Result cache](documents.md#result-cache)): per kind (`web_fetch`, `web_search`, `docs`) its entries, size and this session's hit rate, plus the other caches Elisa keeps; `/cache clear` empties it, `/cache clear docs` one kind. `/stats` ends with the session's cache hits and misses |
| `/yolo` | toggle approve-all |
| `/trust [NAME]` | accept an MCP server's changed tool list (see [MCP servers](mcp-and-plugins.md#mcp-servers)), or review and accept a plugin whose installed files changed (see [Plugins](mcp-and-plugins.md#plugins)); without a name, list both |
| `/plugin …` | plugins (see [Plugins](mcp-and-plugins.md#plugins)): `list`, `add PATH`, `remove NAME`, `enable NAME`, `disable NAME`, `update NAME`, `trust NAME`, `trust project` |
| `/sandbox [list \| pull X]` | the sandbox images (see [Sandbox images](mcp-and-plugins.md#sandbox-images)): each one's purpose, stable / this-project tags, present or missing and size on disk; `/sandbox pull NAME\|IMAGE\|stable\|project\|all` asks first (with the total download size) and then downloads into Colima's VM |
| `/audit [N]` | the last N (20; at most 500, a bigger N is capped with a note; anything else gets the usage line) entries of this project's audit log, then `chain ok (K entries)` or `chain broken at line L` |
| `/allow` | list always-allowed tools, the outside-the-project mode and this session's path grants; `/allow clear` resets the list |
| `/state` | the state cards per topic (document) and which one this turn uses (see [Sessions and state cards](sessions-and-memory.md#sessions-and-state-cards)) |
| `/pin <fact>` | pin a fact to memory (carried in `[state]`) |
| `/model` | the model connection in use, and the saved ones (see [Models and backends](models.md)) |
| `/copy` | copy the last reply to the clipboard |
| `/watcher` | domain watcher: domain, candidates, unload plan, audit scorecard |
| `/full` | the full model's answer to the last reply its audit found changed |
| `/water` `/feed` `/play` `/poke` | play with Shade, the companion (F2, F3, F4, Ctrl-G) |
| `/exit` | quit |

A line ending in `\` continues on the next line. `/chat`, `/code` and `/mode` are left over from an older mode split;
they only say that there is one mode now.

## Project files

`ELISA.md` (project instructions, added to the system prompt, max 3,000 chars) and `.elisa/`
(notes, sessions, optional `config.toml`).

## File commands

A Markdown file becomes a slash command named after the file (`review.md` → `/review`):

```text
---
description: Review one file
args: "[path]"
---
Review the file $ARGS and list problems by severity.
```

`$ARGS` is what you type after the command (without `$ARGS`, it is added at the end); the text is sent as a normal
turn. They load from `~/.elisa/commands/*.md`, the `commands/` folders of enabled plugins, and — after a one-time
trust prompt — the project's `.elisa/commands/*.md` and its project plugins. Elisa's own commands win a name clash;
between files the first source wins (yours, plugins, the project). `/help` lists them with where each comes from
(`/review [path]  Review one file  (~/.elisa/commands)`; a body that uses `$ARGS` without an `args` line shows
`[args]`, and a description taken from the first line shows `…` for `$ARGS`). A project's command text comes from the
repository and a plugin's from its author, so either reaches the model inside an `<untrusted>` fence, framed as a
task description it may not take as permission to reach outside the project, change settings or reveal secrets; only
your own commands are sent as written. A command file changed since the session loaded it is read again when you use
it: your own runs with its new text; a plugin's only while the plugin's files still match what you approved (else
the plugin is off until `/trust NAME`); a project's shows the trust prompt again with the new text — neither the old
cached text nor the new one runs until you say yes. Front matter keys: `description`, `args` (any other key is an
error, shown as a startup note); a file over 64 KB is skipped.
