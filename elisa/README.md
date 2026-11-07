# Elisa

**A coding agent that lives on your Mac and runs on your models.**

Elisa reads your code, edits it, runs your checks, and tells you what it did, with a model running on your own
machine. Nothing leaves your laptop unless you point it at a server yourself. It comes with a native macOS window and
Shade, a small companion who shows you what Elisa is doing while it works.

![Elisa reading a file, then fixing a bug after asking permission, on a 2B model running locally in Ollama](assets/elisa-demo.gif)

<sub>Recorded live on `qwen3-vl:2b-instruct` in Ollama on a laptop; the window is driven by the app's scripted playback.</sub>

- **Local first.** Works with Ollama, llama.cpp, or any OpenAI- or Anthropic-compatible server. Elisa lists the models
  already on your machine so you can pick one.
- **You stay in charge.** Reading is free; writing, editing and running commands ask first. Anything outside the
  project folder always asks, with the exact paths shown.
- **Checks before done.** After a turn that edits files, Elisa runs your project's own checks (compile, lint, tests)
  and gives the model a couple of rounds to fix whatever fails.
- **Small context, long sessions.** Built for 8K windows: a state card carries the goal and next step forward, and old
  turns are compacted while you watch, not behind your back.
- **Sees and searches.** Attach images to vision models, and turn on web search through DuckDuckGo or your own SearXNG.
- **Shows its thinking.** Reasoning models stream their thoughts live in a three-line ticker before the answer.

---

A small terminal coding agent for local models. It is a REPL that talks to any Anthropic Messages API server and is
built for `streamdense gateway` (Qwen3.8-27B, 8K context = `max_ctx = 8192` in `streamdense.toml`; measured prefill,
decode and first-token times: the repo README and `docs/TEST_LOG.md` Tests 19 and 23). Nine tools
(read, write, edit, bash, glob, grep, and the syntax-tree tools outline, read_symbol, edit_symbol), streaming output, notes that survive `/clear`, and auto trim/compact before the
context runs out. Design contract: `DESIGN.md`. One mode: every request carries the code system prompt and all
tools; the model is told to use tools only when the request needs files, commands or this machine and to answer
plain questions directly (the chat/code mode split was removed on 2026-11-01; `/chat`, `/code` and `/mode` just say so,
and old configs with `auto_mode` / `chat_window` still load, the keys ignored).

## Build

```sh
cd elisa && cargo build --release      # binary: elisa/target/release/elisa
cargo test                             # unit + fake-server tests, no model needed
```

## Run

The gateway must be running first (it lives in the streamdense repo; check `df -h /` and `sysctl vm.swapusage` before loading).
`streamdense up` starts sd-watchdog + nice + gateway with the values in `streamdense.toml` (cap 16 GB, port 8898,
max_ctx 8192, MTP):

```sh
~/Development/streamdense/engine/target/release/streamdense up
```

Then, in a project directory:

```sh
elisa                       # interactive; read/glob/grep/outline/read_symbol run without asking, write/edit/edit_symbol/bash ask
elisa --yes                 # approve everything inside the project (never outside writes, see below)
elisa -p "fix the failing test" --yes   # one task, prints the final answer, exit 1 on an API error
elisa --continue            # reload the last saved session of this directory
```

The mouse wheel scrolls the chat only (Elisa captures the mouse); to select text with the mouse hold fn while dragging
(Terminal.app) or option (iTerm2). `elisa/elisa-window [project dir]` opens Elisa in its own Terminal window titled
"Elisa", with the shell replaced by Elisa and the scrollback cleared first.
"always" is saved per project in `.elisa/allow` (`/allow` lists, `/allow clear` resets).

**Outside the project folder** (the session cwd) every tool asks first, `read` / `glob` / `grep` included: a file
tool whose path resolves outside (`..` and symlinks resolved first; the project's own `.elisa/` is inside), or a bash
command that names an absolute path outside, `~` / `$HOME`, an unknown `$VAR/…`, a `..` that escapes, or a `cd` out of
the folder (a best-effort parse that errs toward asking). The prompt shows the tool, read / write / run, and the exact
resolved paths, with **allow once**, **allow these paths for this session** (that path and everything under it, never
saved) or **deny**. The `.elisa/allow` "always" list never covers outside calls, and `--yes` / `/yolo` deny outside
writes and bash (reads still ask; with `-p` and no terminal they are denied). Config `outside` (default `ask`):
`read-ok` lets outside reads run without asking, `deny` refuses everything outside without asking, `allow` drops the
outside prompt (the normal rules apply, so `--yes` then allows outside writes). A project's `.elisa/config.toml` can
only make `outside` stricter than the user config. `/allow` shows the mode and this session's grants.

Companion window: `app/ElisaCompanion/run.sh` (builds with `swift build -c release` on first run) opens Shade in a small
borderless floating window drawn by the brand kit's own renderer (`elisa-shade.js` v2 over `elisa-character.js` v1,
`colors.json`, `layout.json`, read from `design/` at launch). Then `elisa --companion` (or config `companion = true`)
mirrors Shade's state there over the Unix socket `~/.elisa/companion.sock` (`ELISA_COMPANION_SOCK` overrides) and hides
the terminal dock while the window is connected; the status line stays. Clicking Shade or the water / food / toy tray
in the window is the same as Ctrl-G / F2-F4. Without the window running, `--companion` changes nothing. Drag the
window anywhere; the paw icon in the menu bar toggles always-on-top and quits. `elisa-window --companion [dir]` starts
the window (if built) and Elisa together. Protocol: `src/shade/link.rs`.

Other flags: `--provider NAME`, `--no-checks`, `--base-url U`, `--model M`, `--max-ctx N` (default: the gateway's `/health` `info.max_ctx`), `--cwd DIR`.
Ctrl-C cancels the request or the running bash command (its whole process group); Ctrl-C twice quits; Ctrl-D exits.
The status line after each reply shows tok/s, first-token time, real context use and `reused N` (KV prefix hit).

## Code by syntax tree (outline, read_symbol, edit_symbol, parse guard)

Elisa parses code with tree-sitter (`src/ast.rs`) for Rust, Python, Swift, TypeScript / TSX, JavaScript / JSX, SQL and
bash (`.rs .py .pyi .swift .ts .mts .cts .tsx .js .mjs .cjs .jsx .sql .sh .bash`).

- `outline path`: a file's symbols, one line each with its line range and signature (fns, methods, structs, enums,
  traits, impls, classes, protocols, extensions, interfaces, top-level constants), members indented under their type,
  and the imports on one line. A directory gives one line of names per file. If the outline is over the output cap,
  only the top-level symbols show, with a member count.
- `read_symbol path name [line]`: one symbol's source, line-numbered, by `name`, `Type::method` or `Class.method`.
  Either separator works in any language. Doc comments, attributes and decorators belong to the symbol. A name that
  matches several symbols returns the candidates, and `line` picks one. `Foo` means the type, not its impl or
  extension blocks.
- `edit_symbol path name source [action]`: replace the symbol (`action` = `replace`, the default), insert `source`
  before or after it, or `delete` it. `source` may come at any indentation; it takes the symbol's indentation. The
  result goes through the parse guard.
- **Parse guard**: a `write`, `edit` or `edit_symbol` on a supported file is re-parsed first. If the new text has
  ERROR or MISSING nodes that the old text did not (matched by what they are, not by line), Elisa writes nothing and
  returns `refused: …` with `line:col`, the source line and a caret. Files that were already broken stay editable. The
  guard runs before check-before-done, so a refused edit never triggers a check.
- The system prompt says to outline first, read symbols, and read whole files only when they are small. The tool list
  and prompt are fixed per session, so they stay byte-stable for KV reuse; with `ast = false` both leave out the three
  tools and that prompt line. The three tools add about 250 tokens to the cached system turn.
- `/context` shows `ast: outline + read_symbol N calls · X tokens vs Y for reading those files whole`. Each file
  counts once; a directory outline counts only on the outline side.
- Measured (2026-11-03, Elisa's token estimate, real tool outputs at the 6000-char cap, task = outline a file and
  read one function): elisa/src + mem/src (31 files) 254.7k tokens whole vs 23.3k for outline + the median-size
  symbol (10.9x fewer), 50.9k for outline + the largest (5.0x). engine/crates (115 files): 9.5x / 3.7x. app Swift
  (29 files): 7.2x / 3.4x. On the demo crate (31-line split.rs) it is 1.6x / 1.0x: read small files whole.
  To rerun: `cargo test --offline --test ast_measure -- --ignored --nocapture`.
- RAM: one parser and one tree at a time, nothing cached, and files over 1 MiB are not parsed. Peak RSS over all
  177 files was 28.8 MB for the whole debug test process.
- Binary: 7.5 MB → 20.7 MB (release, arm64). Each grammar is a cargo feature (`lang-rust`, `lang-python`,
  `lang-swift`, `lang-typescript`, `lang-javascript`, `lang-sql`, `lang-bash`, all on by default). Rust + Python
  only is 9.4 MB.
- Licences: every added crate (tree-sitter, tree-sitter-language and the seven grammars) is MIT. The build-time
  crates `cc` and `streaming-iterator` are MIT OR Apache-2.0. Nothing copyleft. Listed in `../THIRD_PARTY_NOTICES.md`.

## Check before done

After a turn in which the model wrote or edited files, Elisa runs the project's own checks before it finishes, fast
ones first, then cheap tests. If a check fails, the trimmed errors go back to the model as a `[elisa] check failed …`
note appended to the history, so the KV prefix still matches. The model then gets up to `check_rounds` (2) fix rounds.
Each step shows in the transcript, e.g. `check: cargo clippy ✗ 3 errors → fixing (1/2)`. Elisa stops and reports when
the same errors come back, the rounds are used up, or a fix round ends without an edit. A timed-out check is reported
and skips the rest without a fix round. The idea comes from forheader test G13.

Elisa picks checks from the languages of the edited files. Rust runs `cargo check`, then `cargo clippy -- -D warnings`,
then `cargo test -q` for the edited crate. Python runs `py_compile`, `ruff` if it is installed, then `pytest -x` on
edited test files. With `ast_targeted_checks` (on by default) the tests narrow to the symbols the turn touched. In
Rust, `cargo test -q -- <names>` runs when every touched symbol is a test or has tests named after it, for example
`split_words` runs `split_words_multibyte`. A new file, a change outside any symbol, or a symbol without a named test
runs the crate's whole tests, as before. In Python, pytest also runs the test files that import a touched symbol's
module and name the symbol. `.elisa/checks.toml` commands are never changed. Swift runs `swift build`. JS/TS runs `tsc --noEmit` when the project has a `tsconfig.json`, and `npm test` when the nearest `package.json` has a real test script. Go runs `go vet ./...` and `go test ./...` per `go.mod`. Other languages need `commands` / `tests` in `.elisa/checks.toml`. Checks
run without asking, like a build you started yourself. Turn them off with `--no-checks`, config `checks = false`, or
`enabled = false` in `.elisa/checks.toml`. That file can also set `rounds`, `timeout_s` and `test_timeout_s`, and
`commands = [...]` / `tests = [...]` replace detection. Shade shows building or testing, then error or a "tests green"
celebration. `--serve-jsonl` sends a `check` event (`src/jsonl.rs`).

## Slash commands

| Command | Action |
|---|---|
| `/help` | list the commands |
| `/clear` | summarise to `.elisa/notes.md`, then empty the history (the next session loads the notes); later saves go to a new session file, so the pre-clear one stays resumable |
| `/reset` | empty the history without a summary (notes kept); the state card is dropped for later sessions too (`.elisa/card-reset`) |
| `/compact` | summarise now and continue with notes + empty history |
| `/context` | estimated tokens of system / tools / history, plus the gateway's real count |
| `/notes` | print the notes |
| `/forget` | delete the notes |
| `/resume` | load the last saved session (`.elisa/sessions/`) |
| `/stats` | metrics of the last request (each model call: kind agent / fold / summary, input, cached and output tokens, first-token time, total time, decode tok/s; wall time split into model time and the rest) and the session totals. One JSON line per request goes to `.elisa/metrics.jsonl` |
| `/yolo` | toggle approve-all |
| `/exit` | quit |

Project files: `ELISA.md` (project instructions, added to the system prompt, max 3,000 chars) and `.elisa/`
(notes, sessions, optional `config.toml`).

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
model = "qwen3:8b"
max_ctx = 8192                        # required for openai and mcp

[providers.claude]                    # the Anthropic API
type = "anthropic"
base_url = "https://api.anthropic.com"
model = "claude-sonnet-5-5"
api_key_env = "ANTHROPIC_API_KEY"     # preferred; `api_key = "..."` also works

[providers.gateway]                   # the local gateway
type = "streamdense"
base_url = "http://127.0.0.1:8898"
model = "qwen38-27b"

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
- **MCP**: MCP has no standard "generate" call, so the tool's input schema decides the shape. A tool with a `messages`
  argument gets the messages, plus `system`, `tools`, `max_tokens` and `model` where it declares them; its reply may be
  plain text or an Anthropic-shaped message, which is how it can call Elisa's tools. Otherwise a text argument (`prompt`,
  `input`, `query`, `text`, `message`, or its only required string) gets the conversation as one transcript: **chat
  only, no file tools**. The reply is not streamed. A `command` runs as you, so only use a server you trust.
- **Keys**: `api_key_env` wins over `api_key`. The generic `ANTHROPIC_API_KEY` / `ELISA_API_KEY` are ignored while a
  connection is active, so they never reach another provider. The window writes the file with mode 600 and never reads
  a key back. Apps opened from the Dock do not see your shell's variables: start the app from a shell, or save the key.
- **A project cannot choose**: `.elisa/config.toml` has no key for connections, and an active connection beats its
  `base_url` / `model` / `api_key`. `--base-url` or `ELISA_BASE_URL` alone means that server with no connection.
- **Test**: the window's button or `elisa --provider-test NAME` uses free calls only (`/health`, the model list, MCP
  `tools/list`), but on an MCP `command` it does start that server. `--providers-json` and `--providers-save` are what
  the window uses. Code: `src/provider.rs`, `src/openai.rs`, `src/mcp.rs`.

## Config

`~/.config/elisa/config.toml`, then `<project>/.elisa/config.toml`, then env (`ELISA_BASE_URL`, `ELISA_MODEL`,
`ELISA_MAX_CTX`, `ELISA_API_KEY` or `ANTHROPIC_API_KEY`). Unknown keys are an error.

| Key | Default | Meaning |
|---|---|---|
| `base_url` | `http://127.0.0.1:8898` | server |
| `model` | `qwen38-27b` | model name sent |
| `api_key` | `dummy` | sent as x-api-key and Bearer |
| `max_ctx` | from `/health`, else 200000 | context window |
| `max_output` | 2048 | max tokens per reply (also the reserve kept free) |
| `compact_at` | 0.80 | act when the prompt passes this share of `max_ctx - max_output` |
| `keep_recent_turns` | 3 | user turns that trimming never touches |
| `tool_output_max_chars` | 6000 | tool output cap (head 60% + tail 40%) |
| `bash_timeout_s` | 120 | bash timeout; the process group is killed |
| `auto_approve` | `["read","glob","grep"]` | tools that never ask (`read` covers outline and read_symbol; an "always" for `edit` covers edit_symbol) |
| `max_steps` | 30 | model calls per user turn |
| `system_prompt_file` | none | replaces `prompts/system.md` |
| `think` | false | request thinking |
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
| `outside` | `ask` | tool calls outside the project folder: `ask`, `read-ok` (outside reads run, writes ask), `deny`, `allow` (no outside prompt; `--yes` then allows outside writes). A project config can only tighten it |

## Speed rules (why it is fast on the gateway)

1. **Append-only history**: every step is a gateway prefix hit (~0.5–1 s first token instead of a full re-read).
2. **One fixed system turn**: system prompt + tools are byte-identical across projects, days and sessions (cwd, date
   and notes go in the first user message), so every new session and every `/clear` restores the gateway's stable
   snapshot (~50 ms) instead of prefilling ~860 tokens (~13 s).
3. **Summaries in context**: `/clear`, `/compact` and auto-compact send the history plus one instruction message, a
   prefix hit; a separate transcript request is only the fallback.
4. **Rare, big rewrites**: above 80% trim old tool output down to 60%; if trimming cannot get under 90%, compact
   instead. One full re-read follows a trim, then prefix hits again.
5. **Small prefix**: ~940 tokens real (incl. Qwen's ~236-token tool preamble) vs ~1,235 for Claude Code `--bare`.
6. No side requests, greedy, no thinking.
