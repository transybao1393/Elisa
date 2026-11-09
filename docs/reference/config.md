# Configuration

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Config

`~/.config/elisa/config.toml`, then `<project>/.elisa/config.toml`, then env (`ELISA_BASE_URL`, `ELISA_MODEL`,
`ELISA_MAX_CTX`, `ELISA_API_KEY` or `ANTHROPIC_API_KEY`). Unknown keys are an error.

Model connections and the optional hosted intent router (`[router]`) live in `~/.config/elisa/providers.toml`, not
here: see [Models and backends](models.md).

| Key | Default | Meaning |
|---|---|---|
| `base_url` | `http://127.0.0.1:8898` | server (the default is the optional streamdense gateway's port; a connection in `providers.toml` overrides it) |
| `model` | none: you choose it | model name sent. Elisa never picks one: with no model here, in the active connection or from `--model`, it sends nothing and says how to choose one (app: Model Connections) |
| `api_key` | `dummy` | sent as x-api-key and Bearer |
| `max_ctx` | from `/health`, else 200000 | context window |
| `max_output` | 2048 | max tokens per reply (also the reserve kept free) |
| `compact_at` | 0.80 | act when the prompt passes this share of `max_ctx - max_output` |
| `fold_at` | 0.65 | when a between-turns fold starts (share of the room left after the fixed cost) ([details](#fold_at)) |
| `fold_to` | 0.30 | the history a fold keeps: this share of the room left after the fixed cost, minus the new notes or card (at least an eighth of that room) |
| `keep_recent_turns` | 3 | user turns that trimming never touches |
| `tool_output_max_chars` | 6000 | tool output cap (head 60% + tail 40%) |
| `bash_timeout_s` | 120 | bash timeout; the process group is killed |
| `auto_approve` | `["read","glob","grep"]` | tools that never ask (`read` covers outline and read_symbol; an "always" for `edit` covers edit_symbol; MCP tools only by `mcp:SERVER`, so `mcp:web` covers web_search and web_fetch; a bare name never covers an MCP tool) |
| `web` | `"duckduckgo"` | search engine of the built-in web server: `duckduckgo`, `searxng`, `brave` or `off` ([details](#web)) |
| `brave_api_key` | none | Brave Search API key for `web = "brave"` (user config only) ([details](#brave_api_key)) |
| `sandbox` | `"off"` | `colima` turns on the built-in code sandbox (`run_code`); user config only ([details](#sandbox)) |
| `route` | `"auto"` | intent routing: each turn carries only the tool groups it needs (`auto` / `off`) ([details](#route)) |
| `gate_tools` | `"auto"` | offer the documents and security tools only on turns about them (`auto` / `off`) ([details](#gate_tools)) |
| `reason` | `"auto"` | the built-in reasoning tools: `auto`, `always`, `off` ([details](#reason)) |
| `docs` | `"on"` | the built-in documents server (`docs_pdf`, `docs_sheet`): `on` / `off` ([details](#docs)) |
| `security` | `"on"` | the built-in security server (`sec_*`): `on` / `off` ([details](#security)) |
| `sandbox_timeout` | 10 | wall-clock limit per `run_code` call in seconds (1–30); on timeout the container is killed and what it printed so far comes back. A project config may only lower it |
| `sandbox_fetch_timeout` | 60 | limit in seconds (5–80) for fetching a `run_code` call's `deps` (the online phase, before the offline run); on timeout the fetch container is killed and the code is not run. A project config may only lower it |
| `max_steps` | 30 | model calls per user turn (the loop guard usually stops a stuck turn much earlier: see [Loop guard](internals.md#loop-guard)) |
| `system_prompt_file` | none | replaces `prompts/system.md` |
| `think` | false | request thinking |
| `think_simple` | `"off"` | reasoning on simple turns (`off` / `on`) ([details](#think_simple)) |
| `companion` | false | link to the companion window (app/ElisaCompanion) when it runs; same as `--companion` |
| `checks` | true | check before done after edits (`--no-checks` turns it off) |
| `check_rounds` | 2 | most fix rounds after a failed check |
| `check_timeout_s` | 120 | timeout per build / type / lint check; the process group is killed |
| `check_test_timeout_s` | 60 | timeout per test check (a timeout skips the tests, it is not a failure) |
| `check_output_max_chars` | 3000 | failing output sent back (error lines first) |
| `ast` | true | the outline / read_symbol / edit_symbol tools and their prompt line |
| `ast_guard` | true | refuse a write / edit / edit_symbol that adds syntax errors |
| `ast_targeted_checks` | true | narrow the test check to the touched symbols |
| `metrics` | true | per-request metrics: `.elisa/metrics.jsonl` and `/stats` (`src/metrics.rs`) |
| `greet_context` | true | answer a greeting from the situation, not with a stock hello ([details](#greet_context)) |
| `cache` | true | Elisa's own result cache, `~/.elisa/cache` (see [Result cache](documents.md#result-cache)): web pages, web searches and document text. A project config can only switch it off (`cache = true` there is ignored with a note when the user config has it off) |
| `cache_max_mb` | 200 | the result cache's size cap in MB (1 ..= 100000); the least recently used entries go first. Only the user config sets it |
| `outside` | `ask` | tool calls outside the project folder: `ask`, `read-ok` (outside reads run, writes ask), `deny`, `allow` (no outside prompt; `--yes` then allows outside writes). A project config can only tighten it |
| `shell_sandbox` | `write` (macOS) | Seatbelt sandbox around `bash` and the checks (see [Shell sandbox](safety.md#shell-sandbox)): `write` (writes only in the project, temp folders, build caches; secret stores unreadable), `strict` (also no network but localhost), `off`. A project config can only tighten it; a no-op off macOS |
| `[permissions]` | none | `allow` / `ask` / `deny` lists of tool-call patterns (see [Permission rules](safety.md#permission-rules)). A project config may add only `deny` and `ask` |
| `audit` | `"on"` | the audit log of tool calls, approvals and MCP server starts / stops (`~/.elisa/audit/`, `/audit`; see [MCP servers](mcp-and-plugins.md#mcp-servers)). A project config can only switch it off; any other value keeps it on with a note |
| `redact` | `"remote"` | hide secrets in tool output from the model: `remote`, `always`, `off` ([details](#redact)) |

## Details

### `fold_at`

Default: 0.65.

a between-turns fold starts when the prompt reaches the fixed cost (system + tools) plus this share of the room left after it (the prompt budget minus the fixed cost). A fold that could not free at least an eighth of that room is skipped, and a fold whose result would be bigger than the prompt it replaces is not kept (the notes are cut by priority first; failing that, everything stays as it was).

### `web`

Default: `"duckduckgo"`.

the search engine of Elisa's built-in web MCP server: `"duckduckgo"`, `"searxng"` (with `searxng_url`), `"brave"` (the Brave Search API, with a key: `brave_api_key`), or `"off"` (no web tools). The model decides when to search or open a page; each call is shown in the chat and asks first. Any other value, `searxng` without an http(s) `searxng_url`, or `brave` without a key, turns web off with a startup note. A project config can only turn it off (another value there is ignored, with a note). DuckDuckGo rate-limits automated searches (it may answer with a bot check); SearXNG (its settings.yml must allow `json` in `search.formats`) and Brave are the reliable choices

### `brave_api_key`

Default: none.

the Brave Search API key for `web = "brave"`. The environment variable `BRAVE_API_KEY` wins over it when set; the setting is for when there is no shell environment (the macOS app started from Finder). Only `~/.config/elisa/config.toml` may hold it: a project config's `brave_api_key` is ignored with a note. Elisa hands it to the web server in that process's environment, never on its command line, and never prints it. A refused key (HTTP 401/403) or the rate limit (429) comes back to the model as one line

### `sandbox`

Default: `"off"`.

`"colima"` turns on the built-in code sandbox (`run_code`: python, javascript, bash, rust, go in a locked-down container on Colima, with optional pinned `deps` fetched online first; see [MCP servers](mcp-and-plugins.md#mcp-servers)). Off by default; only `~/.config/elisa/config.toml` can switch it on, a project config can only switch it off (`sandbox = "off"`). Any other value is off with a startup note

### `route`

Default: `"auto"`.

Intent routing (`route.rs`): each turn's requests carry only the tool groups the user's words call for, judged by cheap signals (no model call), once per turn. Groups: **chat** (no tools at all: greetings, small talk, jokes, creative writing, short questions with no file / URL / code marker), **web** (`web_search`, `web_fetch`: a URL, "search", "latest", "news", "look up", weather, prices), **docs** (`docs_pdf`, `docs_sheet`, `read`: a PDF / spreadsheet / CSV), **files/code** (`read`, `glob`, `grep`, `outline`, `read_symbol`, `write`, `edit`, `edit_symbol`, `bash`: a file name or path, a code block, a traceback, "fix" / "build" / "refactor", "this repo / project / file", "write a python script"), **run** (`run_code`: "run this snippet", "sandbox"), **security** (`sec_*`, same words as `gate_tools`), **time** (no tools: the turn gets the local clock line, `Local time now: 2026-11-09 14:05 Monday (UTC+07:00, Asia/Ho_Chi_Minh)`, plus one line per place it names, `Time in Tokyo now: …`, from the system clock and zone database; a clock time to convert, "3pm PST to London", also offers the time server's tools), **other** servers (yours and plugins': offered when the turn names the server). The reasoning tools keep their own gate (`reason`). A follow-up ("continue", "more", "the second one", "tiếp đi", "now also add X", "it still fails") keeps the last turn's groups; a clear chat turn ("thanks!", "tell me a joke") drops them. A long message with no signal gets the small default set (web, or the last turn's groups). A call to a session tool outside the routed set widens the route to its group and runs after the usual approval (a tool the session does not have is still refused), so a wrong guess never fails the turn. Small talk is the exception: on a greeting, thanks, bye or bare "ok" routed as chat (an "ok" that answers an offer, a last answer ending in "?", is not small talk), a tool call never runs and never widens the route; the turn is asked once more with no tools and `(Reply in words; no tools for this message.)` after the user's words in that request only, and a reply that still calls a tool is replaced by a plain line (`Hi! What would you like to do next?`, or `Chào bạn! Bạn muốn làm gì tiếp theo?` for a Vietnamese message) instead of a loop. A short follow-up after an answer ("why?", "more", "go on", "tiếp đi", "the second one", "number 2", "cái thứ hai", "số 2", "and in Osaka?", "còn Osaka thì sao?") gets one hint line of at most ~25 tokens right before the user's words in that turn's message (`src/followup.rs`): `(Follow-up: explain the reason behind your last answer. Don't repeat it.)`, `(Follow-up: go deeper on your last answer with concrete details; no repeats; say if you're unsure.)`, `(Follow-up: the user picks item 2 from your last answer; tell more about that item only.)`, `(Follow-up: same question as before, now about Osaka.)`; when the conversation is about an attached document (an `[Attached: …]` reference in the history, a kept document, or the docs route) the hints stay with it: `(Follow-up: continue from the document: the next relevant part, only what it says, no repeats.)`, `(Follow-up: explain the reason behind your last answer using the document.)`. A bare "yes" / "ok" / "sure" after an answer that ends by offering several options ("Would you like me to: 1. … 2. …") gets `(Follow-up: you offered several options; ask which one the user wants. Don't start one.)`; a yes to a single offer goes ahead; none in a new session or after a turn with no answer (no invented context), and none for "more" / "continue" on a turn that carries tools (there it means resume the task). Titles and history views strip it. A chat turn's system prompt leaves out the file / code rules and the state-card paragraph, and a chat first turn leaves the working directory out of its header (the first tool turn carries it). `/context` shows the last turn's route; `ELISA_DEBUG=1` prints a `[route: …]` line every turn. `"off"`: every tool on every turn (the `gate_tools` / `reason` gates still apply). A project config can only set it `"auto"`

### `gate_tools`

Default: `"auto"`.

Relevance gating for the built-in documents and security servers (`mcp_reason::gate`, a check of your words, no model call, decided once per turn like `reason`): `"auto"` offers `docs_pdf` / `docs_sheet` only on a turn that mentions a document, PDF, spreadsheet, sheet, CSV / XLSX (or such a file is in the last few messages), and the `sec_*` tools only on a turn about security, secrets, leaks, credentials, vulnerabilities / CVEs, dependencies, JWTs or binaries; a follow-up ("continue", "yes") keeps the last turn's choice. Web, run_code, your own servers and Elisa's tools are always offered. The gated tools sit after every stable tool (documents, then security, then the reasoning tools), so a flip changes only the end of the tool list. A call to a gated tool on a turn without it is refused once and the tool is offered from the next request on. `"off"`: every tool on every turn. A project config can only set it `"auto"`

### `reason`

Default: `"auto"`.

The built-in reasoning tools for small models (`think`, `steps`, `step_done`, `calc`; see [MCP servers](mcp-and-plugins.md#mcp-servers)): `"auto"` offers them only on turns whose request looks like a multi-step task (a check of your words, no model call), `"always"` on every turn, `"off"` never (the server is not started). A project config can only switch it off; any other value is `"auto"` with a startup note

### `docs`

Default: `"on"`.

Elisa's built-in documents MCP server (`docs_pdf`, `docs_sheet`: PDF text and spreadsheets inside the project folder; see [MCP servers](mcp-and-plugins.md#mcp-servers)). `"off"` removes its two tools (about 180 tokens of tool definitions). Any other value is off with a startup note; a project config can only switch it off (`docs = "on"` there is ignored, with a note, when the user config has it off)

### `security`

Default: `"on"`.

Elisa's built-in security MCP server (`sec_secrets`, `sec_deps`, `sec_binary`, `sec_jwt`; see [MCP servers](mcp-and-plugins.md#mcp-servers)). `sec_deps` sends package names and versions from your lockfiles to osv.dev. `"off"` removes its four tools (about 210 tokens of tool definitions). Any other value is off with a startup note; a project config can only switch it off

### `think_simple`

Default: `"off"`.

Reasoning on a turn the reasoning gate (`mcp_reason::wanted`, the same check as `reason = "auto"`, whatever `reason` is) judges simple: a greeting, a short question ("2+2=?", "what is the capital of France?"). `"off"`: the model is asked not to reason and the reply gets a smaller cap (1/8 of the window, 1024..4096: 2048 on a 16K window), where the connection allows it: Ollama's native API sends `think: false` to a reasoning model, and a Qwen3-family reasoning model (which ignores that flag) also gets a closed one-line reasoning block as a trailing assistant message, so it answers at once; the answer is shown when complete, with any reasoning it still wrote taken out. Anthropic's API without `think` gets the cap only; other servers are unchanged. `"on"`: simple turns reason like any other. A reply with no visible text once its state card is taken out (any turn, any connection) is asked for once more with a one-off nudge that never enters the history; if that is empty too, an info line says so instead of silence. A simple turn whose reply uses its whole cap and shows no text (a 2B model that opened a second reasoning block, which the held answer drops) is sent once more without the simple-turn limits, with an info line; a reply that hits the limit with no text again says so plainly instead of only `[stopped: reply hit max_tokens]`. A follow-up ("ok", "continue") keeps the last turn's choice; the choice is made once per turn, so a turn's requests keep one shape. A project config can only set it `"off"`

### `greet_context`

Default: true.

A greeting ("hey", "hi Elisa", "good morning", "chào") routed as chat is answered from the situation, not with a stock hello (`src/situation.rs`). With no context (a first session, or a new session whose last earlier session is over 3 days old) nothing is added: the user's words alone, the cheapest request. With context, ONE line of ~30-40 tokens goes right before the user's words in that turn's message only (after the cached system / tools / history prefix), built from data Elisa already keeps: mid-conversation, how long since the last turn, the topic (the latest request that was not small talk) or, after a task, the state card's `next:` step, and the part of the day after a gap of an hour or more, e.g. `(Situation: same chat, 3 min later · topic: "plan a 3-day trip to Tokyo…" · one line, no hello, e.g. "Yes? More on <topic>?")`; in a new session, the newest earlier session's title (from `.elisa/session-meta`, redacted; its last card's goal when the title is itself small talk) to offer, never continue: `(Situation: returning user · last chat 2 days ago: "…" · one line: greet, ask: continue it or something new?)`. Never notes or any other earlier text. A thanks / bye / "cool" gets no line (the history answers it well). After a stopped task (the loop guard, a step limit, an error) the line says so briefly, with its goal and an example reply in the greeting's language: `(Situation: last task stopped: "run ./test.sh and tell me what…" · reply like "Hi! Last task stopped. Try again or something new?")` (a tool call on such a turn is not run anyway, see `route`). Titles and history views never show the line.

### `redact`

Default: `"remote"`.

secrets in tool output (`src/secrets.rs`: AWS, GitHub, OpenAI/Anthropic, Slack, Google, Stripe keys, private key blocks, JWTs, `password = "…"` assignments and `.env` lines with random-looking values) are replaced by `[redacted:<rule>]` before the model sees them: `"remote"` when the connection leaves this machine (a cloud provider, or a non-loopback `base_url`), `"always"` also for local models, `"off"` never. One info line says how many were hidden (`[2 secrets hidden from the model]`), never the value; your own view of the output is unchanged. Whatever Elisa writes to disk — `.elisa/notes.md`, saved sessions, memory turns / cards / notes / pinned facts, `.elisa/metrics.jsonl`, session titles — is redacted in every mode. `KEY=value` / `key: value` count inside a sentence too ("my db password is DB_PASSWORD=…"; mid-line the value must also hold a digit and a letter). Every secret value the session has seen (in tool output, a file read, your own message) is remembered in memory only — never written anywhere — and hidden wherever it appears again, so a model restating it in prose ("…app.cfg with the value Zp…") is redacted on disk and, when redaction applies, in tool output for the model (values under 8 characters or not random-looking are not remembered). A project config can only make it stricter (off < remote < always); any other value is an error
