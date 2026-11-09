# Models and backends

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

Elisa never picks or recommends a model: you choose one, in the app's Model Connections window, in
`~/.config/elisa/providers.toml`, in `config.toml` or with `--model`. With none chosen it sends nothing and says how
to choose one.

## Model connections

`~/.config/elisa/providers.toml` says which model Elisa talks to: on this machine or online, directly or through MCP.
In the app: **Elisa > Model Connections…** (⌘,, `/model`, or the menu-bar icon) adds, tests and picks one; saving
restarts the session on it and resumes the history. In a terminal, edit the file; `/model` lists what is saved.
With no file, or none active, nothing changes: `config.toml` and its environment decide, as below.

```toml
active = "ollama"                     # or: --provider NAME, ELISA_PROVIDER (`none` = ignore this file)

[providers.ollama]                    # any OpenAI-style server
type = "openai"
base_url = "http://127.0.0.1:11434/v1"
model = "your-model"
max_ctx = 8192                        # required for mcp; openai: optional, read from the server (see Backends)

[providers.claude]                    # the Anthropic API
type = "anthropic"
base_url = "https://api.anthropic.com"
model = "your-model"
api_key_env = "ANTHROPIC_API_KEY"     # preferred; `api_key = "..."` also works

[providers.gateway]                   # optional: a streamdense gateway (see below)
type = "streamdense"
base_url = "http://127.0.0.1:8898"
model = "your-model"

[providers.through-mcp]               # a model served by an MCP tool
type = "mcp"
command = ["my-server", "--stdio"]    # or url = "https://..." (streamable HTTP)
tool = "generate"                     # optional when the server has one tool
max_ctx = 8192
```

- **Context size**: the window's buttons are 8K, 16K, 32K, 64K, 128K, 200K, 1M and Custom (`max_ctx` in the file), plus
  Auto for `streamdense`, which asks the gateway. A size does not change the server: on the gateway, a size above its
  `--max-ctx` is lowered to that with a note, and Test says so. On other servers, match what the model was loaded with.
- **Images**: with a model that can see (`vision`), the app's 📎 button, drag and drop and ⌘V attach up to 4 pictures
  to a message (scaled to 1024 px; each costs about 1300 tokens of the window). `vision = true | false` on a connection
  decides; without it, the Anthropic API counts as yes, an OpenAI-type server is asked the Ollama way (`/api/show`
  capabilities) and anything else counts as no. Without vision the button is off and Elisa refuses attached pictures.
- **Types**: only `streamdense` gets the gateway's extras (prompt reuse hints, warm-ups, `/health`, the
  watcher). The others send none of them, and the watcher is off, because its notes hold project text.
- **No prompt cache needed**: Elisa works the same with or without a server-side prompt cache; the cache only changes
  speed. Every request carries the whole conversation (no server state is assumed), the context meter comes from the
  server's total prompt count (`prompt_tokens`, or Elisa's estimate when none is reported), and "reused N" shows only
  when the server reports reused tokens. Intent routing helps most on such a server: a chat turn sends only the tools it needs, often none.
- **MCP**: MCP has no standard "generate" call, so the tool's input schema decides the shape. A tool with a `messages`
  argument gets the messages, plus `system`, `tools`, `max_tokens` and `model` where it declares them; its reply may be
  plain text or an Anthropic-shaped message, which is how it can call Elisa's tools. Otherwise a text argument (`prompt`,
  `input`, `query`, `text`, `message`, or its only required string) gets the conversation as one transcript: **chat
  only, no file tools**. The reply is not streamed. A `command` runs as you, so only use a server you trust. It gets
  the same clean environment and process-group shutdown as the MCP tool servers (see [MCP servers](mcp-and-plugins.md#mcp-servers)), plus the
  connection's key as `MCP_API_KEY` when it has one.
- **Keys**: `api_key_env` wins over `api_key`. The generic `ANTHROPIC_API_KEY` / `ELISA_API_KEY` are ignored while a
  connection is active, so they never reach another provider. The window writes the file with mode 600 and never reads
  a key back. Apps opened from the Dock do not see your shell's variables: start the app from a shell, or save the key.
- **A project cannot choose**: `.elisa/config.toml` has no key for connections, and an active connection beats its
  `base_url` / `model` / `api_key`. `--base-url` or `ELISA_BASE_URL` alone means that server with no connection.
- **Test**: the window's button or `elisa --provider-test NAME` uses free calls only (`/health`, the model list, MCP
  `tools/list`), but on an MCP `command` it does start that server. `--providers-json` and `--providers-save` are what
  the window uses. Code: `src/provider.rs`, `src/openai.rs`, `src/mcp.rs`.

## Hosted intent router (optional, paid)

Elisa routes each turn with its own word rules (config `route`, see [Configuration](config.md#route)). A `[router]`
table in `~/.config/elisa/providers.toml` can hand that choice to a hosted intent router instead: one small HTTP call
per user turn that names the turn's intents. It is a paid service and it is **off without a key**:

```toml
[router]
url = "https://router.example.com"
api_key_env = "ELISA_ROUTER_KEY"   # or: api_key = "…"
```

- No key (neither `api_key_env` nor `api_key` set) or no URL: off, and Elisa routes with its own word rules exactly as
  without it. The router is its own setting, on whichever model connection is active; the Model Connections window
  keeps it as saved.
- What is sent: the latest user message, the previous user message and Elisa's last reply (each cut to 400
  characters), and a few flags (a document attached this turn, a document in the chat, whether the model has vision,
  whether a picture was refused, whether an earlier session exists). No history, file or document text, memory, tool
  output or system prompt.
- Any miss (no answer within 2 s, a non-2xx status, a body that is not the expected JSON, no known label) routes the
  turn as without a router: no error in the chat, at most one quiet info line per session.
- The endpoint contract (version 1): `src/router.rs`.

## Backends

At connect time Elisa works out which server is behind a connection (`src/backend.rs`): the connection's `type`, the
URL's host for the cloud APIs, and for any other server a few cheap GETs sent at once (at most 1.5 s, no tokens). It
fills a `Caps` record (window, vision, tools, usage fields, stream quirks, the backend's own cache) and `/model` shows it.
Nothing depends on a server-side cache: every request carries the whole conversation; a cache is only a speed bonus,
used where Elisa controls it, and the status line says "reused N" only when the server reported N.

| backend | detected how | caps read | cache features Elisa uses | tested |
|---|---|---|---|---|
| Ollama (native `/api/chat`, `/v1`) | `GET /api/version` | window: `/api/ps` loaded `context_length` (else `/api/show` max, capped at 8192); vision/tools/thinking: `/api/show` capabilities | none extra (keep_alive is yours, never sent); `num_ctx` on every request | real + replica + eval |
| llama.cpp `llama-server` | `GET /props` (`default_generation_settings`, `total_slots`) | window `n_ctx`, vision `modalities`, tools `chat_template_caps`; reuse from `timings.cache_n` | `cache_prompt: true`; slot save/restore when started with `--slot-save-path` (probe: a restore of a missing file is not 501) | real + replica + eval |
| LM Studio | `GET /api/v0/models` (`max_context_length`) | window (loaded, else max), vision `type: vlm`, tools `capabilities` | none extra (automatic) | replica + eval |
| MLX `mlx_lm.server` | Python `BaseHTTP` server header on `/v1/models` | usage only with `stream_options.include_usage` (always sent) | none extra (automatic) | replica + eval |
| vLLM | `/v1/models` `owned_by: "vllm"` (+ `/version`) | window `max_model_len`; `cached_tokens` only with `--enable-prompt-tokens-details` | none extra (automatic prefix caching) | replica + eval |
| LocalAI / GPT4All / Jan / other OpenAI-style | LocalAI `GET /version`; GPT4All `owned_by: "humanity"`; Jan port 1337; else generic | usage may be missing: Elisa's estimate, marked `~` | none | replica + eval |
| OpenAI | host `api.openai.com` | `max_completion_tokens`; usage chunk with `prompt_tokens_details.cached_tokens` | none extra (automatic); stable prefix | replica |
| Anthropic | `type = anthropic` + host `api.anthropic.com` | `cache_creation_input_tokens` counted as read, `cache_read_input_tokens` as reused | `cache_control` breakpoints: system (covers tools), the previous user turn, the newest message (3 of 4) | replica (+ eval for Anthropic-compatible servers) |
| Google Gemini | host `generativelanguage.googleapis.com` | tool calls arrive whole, maybe without `index`; error bodies as a list | implicit caching; an explicit `cachedContents` of system + tools (TTL 300 s) only when ≥ the model's minimum and ≥ 4096 tokens and the 3rd call with that prefix within 5 min; then sent as `extra_body.google.cached_content` | replica |
| OpenRouter | host `openrouter.ai` | `: OPENROUTER PROCESSING` comments, mid-stream `error` | none extra | replica |
| Groq | host `api.groq.com` | usage in `x_groq.usage` | none extra | replica |
| Together | host `api.together.xyz` / `api.together.ai` | usage on the last chunk | none extra | replica |
| DeepSeek | host `api.deepseek.com` | reuse `prompt_cache_hit_tokens`, `reasoning_content` | none extra (automatic) | replica |
| Mistral | host `api.mistral.ai` | tool call ids rewritten to 9 letters/digits (stable) | none extra | replica |

- **Context**: `max_ctx` is optional for `type = "openai"`: without it the server's window is used; a window the server
  fixed (llama.cpp `-c`, LM Studio's loaded length, vLLM `max_model_len`) also caps a larger `max_ctx`, with a note.
  Counts that cover only part of the prompt are made whole where they are read (llama.cpp `prompt_n + cache_n`,
  Anthropic input + cache write + cache read). When the server reports no prompt count, the context meter uses Elisa's
  estimate and shows it as `ctx ~3.1K/16.4K (19%, estimated)`.
- **Tools**: native everywhere they exist; streamed argument fragments, whole calls, parallel calls and calls without
  `index` are joined. A model or server without tools (Ollama capabilities without `tools`, llama.cpp without `--jinja`,
  or a server that refuses `tools`) gets them described in the system prompt and `<tool_call>{…}</tool_call>` in the
  reply becomes a real call (only for tools that exist).
- **No stall timeout**: only the connect has a timeout, so a no-cache server reading a long prompt is waited for (Esc
  stops it); the activity line says "Reading the conversation…" on servers that keep the model loaded. A 503 (model
  still loading) is retried once after 3 s; other 5xx at once.
- **`/compact`** uses the in-context form (same prefix + one message) only where the server reuses a prefix; elsewhere
  the smaller plain transcript.
- **llama.cpp slots**: on `/clear`, quit and `/resume` (the session left), slot 0 is saved to `elisa-<project>-<n>.bin`
  in the server's slot dir (4 names per project, reused in turn, since the server cannot delete files);
  `.elisa/slots.json` maps sessions to files; `/resume` and `--continue` restore it.
- Tests: `tests/fake_server.rs` `replica_*` (one per backend, shapes from each backend's docs, URL in a comment) and
  `elisa eval --suite backends` (detection + a tool turn per local protocol).

## streamdense gateway (optional)

`streamdense` is one optional local backend (it lives in its own repository); Elisa works the same on Ollama,
llama.cpp, LM Studio and the other servers above. A connection with `type = "streamdense"` gets the gateway's extras:
prompt reuse hints, warm-ups, `/health` (window size for Auto and `--max-ctx`) and the watcher. The config default
`base_url` (`http://127.0.0.1:8898`) is the gateway's port.

To use it, start the gateway first (check free disk and swap, `df -h /` and `sysctl vm.swapusage`, before loading a
large model). `streamdense up` starts sd-watchdog + nice + gateway with the values in `streamdense.toml` (memory cap,
port 8898, `max_ctx`, MTP):

```sh
~/Development/streamdense/engine/target/release/streamdense up
```

### Speed rules on the gateway

1. **Append-only history**: every step is a gateway prefix hit instead of a full re-read.
2. **One fixed system turn**: system prompt + tools stay byte-identical across projects, days and sessions where the
   route allows (cwd, date and notes go in the first user message), so a new session or a `/clear` restores the
   gateway's stable snapshot instead of prefilling the system turn again.
3. **Summaries in context**: `/clear`, `/compact` and auto-compact send the history plus one instruction message, a
   prefix hit; a separate transcript request is only the fallback.
4. **Rare, big rewrites**: above 80% trim old tool output down to 60%; if trimming cannot get under 90%, compact
   instead. One full re-read follows a trim, then prefix hits again.
5. **Small prefix**: a short system prompt and tool list.
6. No side requests, greedy, no thinking.
