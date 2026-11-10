# Elisa (terminal app and core)

`elisa` is the chat assistant behind the Elisa macOS app, usable on its own in a terminal. It is a chat assistant
first: ask questions, search the web, read attached documents and pictures, keep a light memory. Coding help (read,
edit and run code in a project, with your approval) is one feature among the rest. It runs on a model you choose:
local (Ollama, llama.cpp, LM Studio, MLX…) or any OpenAI- or Anthropic-compatible API.

![Elisa reading a file, then fixing a function after asking permission, on a small model running locally](assets/elisa-demo.gif)

- **You choose the model.** Elisa never picks or recommends one. With none chosen it sends nothing and tells you how
  to choose.
- **You stay in charge.** Reading is free; writing, editing, running commands and every MCP tool ask first. Anything
  outside the project folder always asks, with the exact paths shown.
- **Small windows, long sessions.** Built for 8K contexts: each turn carries only the tools it needs, and old turns are
  folded into notes while you watch.
- **Extensible through MCP.** Built-in servers for the web, documents, security checks, reasoning and an opt-in
  offline code sandbox; add your own servers or plugins.

## Build and run

```sh
cd elisa && cargo build --release      # binary: elisa/target/release/elisa
cargo test                             # unit + fake-server tests, no model needed
```

In a project directory:

```sh
elisa                                  # interactive chat
elisa --continue                       # reload the last saved session of this folder
elisa -p "summarise this" --attach ~/Downloads/report.pdf   # one question about a document
elisa -p "fix the failing test" --yes  # one task; approves everything inside the project
```

`elisa/elisa-window [dir]` opens Elisa in its own Terminal window. Esc / Ctrl-C stops a request, Ctrl-C twice
quits, Ctrl-D exits. The macOS app (`app/ElisaApp/run.sh`) runs this same core. All flags:
[commands](../docs/reference/commands.md).

## Connect a model

Model connections live in `~/.config/elisa/providers.toml`. In the app, **Elisa > Model Connections…** (⌘,) adds,
tests and picks one; in a terminal, edit the file (`/model` shows what is saved), or pass `--base-url` and `--model`.

```toml
active = "local"

[providers.local]                      # any OpenAI-style server, e.g. Ollama
type = "openai"
base_url = "http://127.0.0.1:11434/v1"
model = "your-model"                   # a model you have pulled; Elisa does not choose one

[providers.cloud]                      # the Anthropic API
type = "anthropic"
base_url = "https://api.anthropic.com"
model = "your-model"
api_key_env = "ANTHROPIC_API_KEY"
```

Types: `openai`, `anthropic`, `mcp` (a model served by an MCP tool) and `streamdense` (an optional local gateway, one
backend among many). At connect time Elisa detects what the server and model can do (window, vision, tools) and
adapts; nothing depends on a server-side prompt cache. `elisa --provider-test NAME` checks a connection with free
calls only.

**Optional hosted intent router.** A `[router]` table in the same file (`url` and `api_key_env` or `api_key`) hands
the choice of each turn's tools to a hosted router, a paid service. Without a key it is off and Elisa routes with its
own word rules. Details: [models and backends](../docs/reference/models.md).

## Main slash commands

| Command | Action |
|---|---|
| `/help` | list the commands (and your own file commands) |
| `/model` | the connection in use and the saved ones |
| `/clear` | save a summary to notes, then start fresh |
| `/compact` | summarise now and continue from the notes |
| `/resume` | pick a past session to continue |
| `/notes` · `/pin <fact>` · `/forget` | show the notes · pin a fact · forget this project's memory |
| `/context` · `/stats` | token use · the last request's metrics |
| `/mcp [NAME]` | MCP servers: state, tools, token cost |
| `/plugin …` | list, add, remove, enable, disable, update plugins |
| `/allow` · `/yolo` | always-allowed tools · toggle approve-all |
| `/cache [clear]` | Elisa's result cache (web pages, searches, document text) |
| `/exit` | quit |

Every command, file commands (`~/.elisa/commands/*.md`) and project files (`ELISA.md`, `.elisa/`):
[commands](../docs/reference/commands.md).

## Configuration

`~/.config/elisa/config.toml`, then the project's `.elisa/config.toml` (where safety settings can only be tightened), then
environment variables (`ELISA_BASE_URL`, `ELISA_MODEL`, `ELISA_MAX_CTX`, `ELISA_API_KEY`). Unknown keys are an
error. The main keys:

| Key | Default | Meaning |
|---|---|---|
| `model` | none | model name sent; you choose it |
| `base_url` | `http://127.0.0.1:8898` | server when no connection is active |
| `max_ctx` | from the server | context window |
| `max_output` | 2048 | max tokens per reply |
| `web` | `"duckduckgo"` | web search: `duckduckgo`, `searxng` (+ `searxng_url`), `brave` (+ key), `off` |
| `route` | `"auto"` | send each turn only the tool groups it needs; `off` sends every tool |
| `docs` | `"on"` | built-in documents server (PDF, spreadsheets) |
| `security` | `"on"` | built-in security server (secrets, dependency advisories) |
| `reason` | `"auto"` | built-in think / steps / calc tools for multi-step tasks |
| `sandbox` | `"off"` | `"colima"` turns on the offline code sandbox (`run_code`) |
| `auto_approve` | `["read","glob","grep"]` | tools that never ask |
| `outside` | `ask` | calls outside the project: `ask`, `read-ok`, `deny`, `allow` |
| `shell_sandbox` | `write` (macOS) | Seatbelt sandbox around shell commands: `write`, `strict`, `off` |
| `checks` | true | run the project's checks after a turn that edits files |
| `redact` | `"remote"` | hide secrets from the model: `remote`, `always`, `off` |
| `cache` | true | Elisa's own result cache in `~/.elisa/cache` |
| `[mcp.NAME]` | none | add an MCP server (`command` or `url`, optional `tools` / `exclude`) |
| `[permissions]` | none | `allow` / `ask` / `deny` patterns for tool calls |

Every key with its full rules: [configuration](../docs/reference/config.md).

## Documentation

- [Reference index](../docs/reference/README.md)
- [Commands and flags](../docs/reference/commands.md) · [Configuration](../docs/reference/config.md) ·
  [Models and backends](../docs/reference/models.md)
- [Permissions and safety](../docs/reference/safety.md) ·
  [Sessions, memory and notes](../docs/reference/sessions-and-memory.md)
- [Documents and the result cache](../docs/reference/documents.md) · [Coding tools](../docs/reference/coding.md)
- [MCP servers, plugins and sandbox images](../docs/reference/mcp-and-plugins.md) ·
  [elisa eval](../docs/reference/eval.md) · [Internals](../docs/reference/internals.md)
- [Design contract](DESIGN.md) · [Development history](../docs/history.md) ·
  [Third-party notices](../THIRD_PARTY_NOTICES.md)
