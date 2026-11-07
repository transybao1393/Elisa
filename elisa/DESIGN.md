# Elisa — design contract

Elisa is a small terminal coding agent. It is a REPL that talks to any Anthropic Messages API server. The
main target is `streamdense gateway` (Qwen3.8-27B on a Mac mini, 8K context: `max_ctx = 8192` in `streamdense.toml`). Everything is built around
two constraints: a **small context window** and a **slow full prefill** (~75 tok/s).

## Goals (what we learned from running Claude Code on the 27B)

| Problem seen with Claude Code | Elisa's answer |
|---|---|
| ~1.2K tokens of system prompt + tools, even in `--bare` | System prompt + 6 tool defs **≤ 750 tokens** in total |
| Assumes a 200K window, so it never auto-compacts and hits a hard "Context full" | Reads the real `max_ctx` from the gateway's `GET /health` (`info.max_ctx`); auto trim and compact before the limit |
| `/compact` fails once the context is full | Builds the compact request from a plain transcript whose middle is cut to fit the budget |
| `/clear` loses everything | `/clear` writes a summary to `.elisa/notes.md`, which the next session loads. `/reset` wipes without a summary |
| The model wrote files through huge bash heredocs (1.3K-token commands) | A `write` tool exists, and the system prompt says to use write/edit for files, never bash |
| Side requests of ~32K tokens | None: one model, one request at a time |
| Breaking the prefix forces a re-prefill | The system prompt is byte-stable within a session (the date only, no time); history is only appended between compactions |

## Speed rules: use the engine's architecture

The gateway reuses its KV state when the new prompt starts with the tokens it already holds (prefix mode, ~0.5 s),
and it restores saved snapshots of a stable prefix in ~30 ms. A prompt it can't reuse is read in full at ~75 tok/s,
which takes ~2 min for 9K tokens. So:

1. **Append-only history.** Between compactions and trims, never rewrite earlier messages; only append. Every turn
   is then a prefix hit.
2. **A byte-stable system prompt and tool list** for the whole session. Build them once. The date has day
   precision, there are no timestamps or counters in it, and tool order and JSON key order are fixed.
3. **Trim and compact rarely, in big steps (hysteresis).** Trigger at 80% of the limit and cut down to 60%, so
   a prefix-breaking rewrite happens once every several turns, not every turn. After a rewrite, the next request
   re-reads the prompt once and is then fast again.
4. **Small fixed prefix.** The system prompt plus tools (≤ 700 tokens) is what the gateway snapshots, so every new
   session and every `/clear` starts from a ~30 ms restore. The gateway snapshots the whole system turn (through its
   first `<|im_end|>`), so cwd, OS, date and notes go in the FIRST USER MESSAGE (`context::first_message`), not in the
   system prompt: the system turn is then identical across projects, days and sessions (changed at integration).
   Summaries are asked in context (history + one instruction message: a prefix hit); the transcript request is the
   fallback.
5. **Streaming everywhere.** Text appears token by token. The status line shows tok/s, ttft, context use and
   `reused N` (from `usage.cache_read_input_tokens` when the server reports it) so you can see prefix hits.
6. **No side requests.** Only the main loop and compaction call the model. The domain watcher (`src/watcher.rs`) is not a model
   call: after cards and folds it POSTs a note (<= 2 KB) to `/v1/streamdense/watcher/note`, and after turns
   and folds it GETs `/v1/streamdense/watcher`, both on background threads with a 1.5 s timeout; a 404 turns it off for
   the session. The TUI shows one line under the status bar and `/watcher`; plain mode prints only domain changes.
   No `kind:"turn"` notes: the gateway counts every `/v1/messages` call as a turn itself. The note schema keeps a
   `mode` field; Elisa always sends `"code"` (one mode, rule 7). Domain detection lives in the gateway watcher.
7. **One mode (2026-11-01).** Every request carries the code system prompt (`prompts/system.md`) and the full tool
   set (read, write, edit, bash, glob, grep), for plain questions too. The prompt says: use tools only when the
   request needs files, commands or this machine; answer plain questions directly. There is no chat mode, no
   per-message classifier, no `need_tools` escalation and no re-send: an earlier chat/code split stalled (chat mode had
   no tools after a fold or idle switch, so the model wrote fake tool markup), every flip changed system + tools and
   forced a full re-read, and the phrase-list heuristics kept missing cases. Cost (estimates from
   `estimate_tokens`): code system ≈ 250 + tools ≈ 425 + template tool preamble 240 vs chat system ≈ 126, so
   ≈ +0.8K tokens per request (~10% of an 8.2K window), paid once per session from the gateway snapshot. Old configs
   with `auto_mode` / `chat_window` still load (keys ignored); `/chat`, `/code` and `/mode` print "modes were removed".
   Saved sessions never stored a mode; chat-stripped histories are plain text and resume as-is.
8. **Audits.** Every assistant message keeps the gateway's `message_start.message.id` (`Message.id`, also on tool
   steps); it lives in the session file and is never sent to the API (`api.rs` sends role + content only; an old
   session's `vote` key is ignored on load). The gateway audits every reply against the full model in idle time
   (dry run: skipped until masks exist); Elisa sends nothing for it and polls nothing extra, the normal turn-cadence
   GET picks up finished audits. The view's `audits_recent` (lenient) gives one transcript line per
   `mismatch` audit of a final reply of this session (the newest 512 ids), shown once: `audit ✗ masked weights changed
   this reply at token N · profile tightened · /full shows the full-model answer`, or `audit ✓ same answer from the
   full model — not caused by unloading`. Match and skipped audits print nothing (no noise on every reply); they
   count in `/watcher`, which adds the `scorecard` table (agreement, audited tokens, mismatches, skipped,
   clean/go-live, points/required, LIVE | dry run; no vote columns; old and new gateway shapes parse), the last 5
   audits, the audit queue and a one-line `heat` summary. `/full` shows the newest mismatched audit with a full-model
   answer of this session's replies, else any in the last view. No votes: Elisa never calls the old vote endpoint.
   - **TUI keys:** mouse capture on (user decision 2026-11-01: the wheel must never scroll the terminal's own
     scrollback, which showed the shell). The wheel scrolls the transcript 3 lines; text selection with the mouse
     needs fn-drag (Terminal.app) / option-drag (iTerm2). With an empty input box ↑↓ also scroll the transcript
     (3 lines); input history and queued-message recall are ctrl-p / ctrl-n. `/copy` (last reply → `pbcopy`) stays
     as a convenience. PgUp/PgDn or shift-↑↓ also scroll the transcript, alt-↑↓ the edit panel, ctrl-←→ resizes the split, ctrl-s toggles the last
     state card.
7. **Greedy, no thinking** by default (think=false), which keeps outputs short. MTP speeds up decode on the server.

## Layout

```
elisa/
  Cargo.toml          (deps: serde, serde_json, toml, ureq 3, regex, walkdir, libc)
  prompts/system.md   base system prompt (include_str!)
  prompts/compact.md  summary instructions (include_str!)
  src/types.rs        shared types (coordinator owns; do not change)
  src/config.rs       Config + loading                 [agent A]
  src/api.rs          HTTP client, SSE streaming       [agent A]
  src/tools.rs        6 tools                          [agent B]
  src/context.rs      budget, trim, compact, store, system prompt, prompts/*.md   [agent C]
  src/repl.rs         CLI args, agent loop, slash commands, permissions           [agent D]
  src/ui.rs           terminal rendering (ANSI), status line, input               [agent D]
  src/main.rs         calls repl::main_entry()
```

The public signatures in the stub files are the contract. Keep them exactly; you may add private helpers and
extra public items.

## config.rs + api.rs (agent A)

- **`Config::default()`**:
  - base_url `http://127.0.0.1:8898`, model `qwen38-27b`, api_key `dummy`, max_ctx `None`
  - max_output 2048, compact_at 0.80, keep_recent_turns 3, tool_output_max_chars 6000
  - bash_timeout_s 120, auto_approve `["read","glob","grep"]`, max_steps 30
  - system_prompt_file None, think false
- **`Config::load`** reads, in order (later overrides earlier):
  1. the defaults
  2. `~/.config/elisa/config.toml`
  3. `<cwd>/.elisa/config.toml`
  4. env `ELISA_BASE_URL`, `ELISA_MODEL`, `ELISA_MAX_CTX`, and `ANTHROPIC_API_KEY` (only if `ELISA_API_KEY` is unset; `ELISA_API_KEY` wins)

  Each TOML key is the field name. Unknown keys produce an error that names the file.
- **The Client:**
  - **Headers:** `x-api-key`, `authorization: Bearer <key>`, `anthropic-version: 2023-06-01`, `content-type: application/json`.
  - **`send`:** POST `/v1/messages` with `{model, max_tokens, system, messages, tools, stream:true}`. It omits `tools` when empty, and passes `thinking` only if think.
    - It parses SSE events: `message_start` (usage.input_tokens), `content_block_start` (text / tool_use with id+name), `content_block_delta` (`text_delta` → on_text, `input_json_delta` → accumulate partial JSON per block), `content_block_stop` (parse the tool input JSON; on bad JSON keep `{}` and the raw text in a `_raw` field), `message_delta` (stop_reason, usage.output_tokens), `message_stop`, `error` (→ Err with the message), `ping` (ignore).
    - Non-2xx: Err with the status and the body's `error.message`.
    - It measures ttft_s (to the first delta) and total_s.
    - Check `cancel` between events; on cancel, drop the connection and return what has been received so far with stop_reason `cancelled`.
    - Read the body line by line from `into_body().into_reader()`; no async.
  - **`count_tokens`:** POST `/v1/messages/count_tokens`, same body without stream/max_tokens.
  - **`server_max_ctx`:** GET `/health`, then `info.max_ctx`. Use a 2 s timeout.

## tools.rs (agent B)

Six tools. Descriptions are terse (each tool def ≤ 80 tokens; one sentence plus the parameters). Schemas use JSON Schema with
`required`. Paths are relative to ctx.cwd unless absolute.

| Tool | Input | Behaviour | Access |
|---|---|---|---|
| `read` | path, offset? (1-based line), limit? (default 400) | Numbered lines `N\t...`; says "[lines a-b of N]" when partial; error if missing or binary | ReadOnly |
| `write` | path, content | Creates parent dirs, writes the whole file; returns "wrote N lines to P" | Write |
| `edit` | path, old, new, all? (bool) | `old` must match exactly once unless all=true; error with the match count otherwise; returns "edited P (N replacement[s])" | Write |
| `bash` | command | `/bin/zsh -c` in cwd, stdout+stderr merged, timeout (kill the process group), returns output + "[exit N]" | Exec |
| `glob` | pattern (e.g. `src/**/*.rs`), path? | walkdir + glob→regex; skips `.git`, `target`, `node_modules`, `.elisa`; at most 200 results, sorted | ReadOnly |
| `grep` | pattern (regex), path?, glob? | Same skips, text files only; `file:line: text`; at most 100 matches | ReadOnly |

- **Output cap:** every output longer than `max_chars` keeps the head (60%) and the tail (40%), joined by `\n[... N chars / M lines cut ...]\n`.
- **Errors:** unknown tool or bad input gives `is_error=true` with a short message, and the tool never panics.
- **`summary`:**
  - `read src/main.rs`
  - `write src/a.rs (42 lines)`
  - `edit src/a.rs`
  - `bash cargo build`, truncated to 100 chars and kept on one line
  - `glob **/*.rs`
  - `grep foo`

## context.rs + prompts (agent C)

- **`estimate_tokens`:** `ceil(chars / 3.3)` plus a per-message overhead of 6 in `estimate_messages`. Tool inputs count via their JSON text.
- **`system_prompt`:** fills `prompts/system.md` with `{cwd}`, `{os}` and `{date}` (YYYY-MM-DD, from libc localtime). If ELISA.md exists, append `## Project instructions (ELISA.md)` capped at 3,000 chars. If notes exist, append `## Notes from earlier sessions` capped at 4,000 chars. The output must be byte-identical for the same inputs.
- **`prompts/system.md`:** ≤ 300 tokens, written in our own words. It says:
  - You are Elisa, a coding agent on a local model with a small context.
  - Be brief.
  - Use tools only when the request needs files, commands or this machine; answer plain questions directly.
  - Use tools to inspect before changing.
  - Create or replace files with `write` and change them with `edit`; NEVER write files through bash (no heredoc / echo >).
  - Read only the lines you need (offset/limit).
  - Run the tests or the build after a change.
  - Stop and report when done.
- **`trim_old`:** process messages before the last `keep_recent` user text turns, oldest first:
  1. Replace each long tool_result content (> 300 chars) with `[trimmed: <first line, at most 80 chars> … N lines]`.
  2. Replace long string fields (> 300 chars) inside tool_use inputs with `[trimmed N chars]`.
  3. Shorten long assistant text (> 600 chars) to its first 300 chars + `…`.

  Stop as soon as the estimate is ≤ target. Never remove messages: tool_use ↔ tool_result pairing must stay valid.
- **`compact_request`:** renders msgs as a transcript:
  - `User: …`, `Elisa: …`, `[tool read src/a.rs]`, `[result: …]` (each result at most 400 chars)
  - old notes first, if any

  If it is over the budget, it keeps the first 25% and the last 75% of the budget and cuts the middle with `[... earlier part cut ...]`. Then it appends `prompts/compact.md`.
- **`prompts/compact.md`:** asks for notes of at most 250 words under the headings Goal, Done, Files touched, Current state / next step, and Facts to remember. The model must output only the notes.
- **2026-11-04 (ui_run1 fixes):**
  - `cap_step_results`: one step's tool results are fitted into what the window has left before the output
    reserve (largest cut first, water-filling, each `head + [trimmed N chars]`). While ≥ 12.5% of the limit is left
    the new results are cut (append-only, so the gateway keeps reusing the prompt); below it the history is folded /
    older results shrunk first. A fold ends with the newest results cut to fit too, so no request goes over.
  - Session goal (`next_goal`): set from the first real request (its `goal:` / `our goal for this session:` words,
    else the request), changed only by a stated new goal; `[state]` shows `session goal:` (and `current request:`
    mid-turn), the notes keep it on top as `## Session goal …`. The card's `goal:` and the summary's `## Goal`
    follow the newest request.
  - Folds and prompt reuse: the gateway's engine state (DeltaNet + KV) cannot rewind, so a rewritten history is
    re-prefilled after system + tools. Defaults fold_at 0.65 / fold_to 0.30 (was 0.75 / 0.35): same fold count in
    long_session, more of them at turn starts; after a between-turns fold the folded prompt goes out as a
    prefill-only warm-up in the background (config `warm` = auto: only when /health says the gateway takes one).
- **`Store`:**
  - dir `<cwd>/.elisa/`, created on the first save
  - `notes.md`; `sessions/<unix-ts>.json` (a Vec<Message>); `load_last_session` takes the newest file
  - `load_project` reads `<cwd>/ELISA.md`

## repl.rs + ui.rs (agent D)

- **CLI:**
  - `elisa [-p PROMPT] [--yes] [--continue] [--base-url U] [--model M] [--max-ctx N] [--cwd DIR] [--version] [-h]`
  - `-p` runs one task non-interactively: prints the final text, exits 0, or 1 on an API error. It implies auto-approval only if `--yes` is given; otherwise it denies Write/Exec tools with the result "denied (non-interactive; use --yes)".
- **Startup:**
  1. Load the config.
  2. Set max_ctx = cli or config or `client.server_max_ctx()` or 200000.
  3. Load ELISA.md and the notes, and build the system prompt once.
  4. Print a banner: `Elisa · <model> @ <base_url> · ctx <max_ctx>`, plus `notes loaded (N words)` when notes exist.
- **The agent loop** (per user input, at most `max_steps` model calls):
  1. **Budget:** `limit = max_ctx - max_output`. If `est(system + tools + history)` is greater than `compact_at * limit`:
     - first `trim_old(keep_recent_turns, target = 0.6*limit)`;
     - if it is still greater than `0.9*limit`, auto-compact: run the compact request with no tools, save the notes, rebuild the system prompt with the notes, keep only the last user message, and print `[auto-compacted: X → Y tokens]`.
  2. **Request:** max_tokens = `min(max_output, max_ctx - est - 64)`. The usage.input_tokens of the latest reply replaces the estimate for status display.
  3. **Streaming:** stream text to the terminal as it arrives.
  4. **On `tool_use`:**
     - For each call, print a dim `● <summary>` line.
     - Approval: a call outside the project folder goes through the outside rules first (below); otherwise auto_approve list or `--yes` or session `always` approvals; otherwise prompt `allow? [y]es / [n]o / [a]lways for <tool>`.
     - Run the tool and print the first 3 lines of its output in dim.
     - Append one user message with all the tool_results, in order, and loop.
  5. **`max_tokens` stop:** print a note and stop the loop.
- **Ctrl-C** during a request sets `cancel` (a libc SIGINT handler sets a static AtomicBool; do not exit). At the prompt, Ctrl-C clears the line and Ctrl-D exits.
- **Status line** after each model reply (dim): `23.4 tok/s · ttft 0.6s · ctx 3.1K/12.3K (25%)`. tok/s = output_tokens / (total_s - ttft_s).
- **Slash commands:**

  | Command | Action |
  |---|---|
  | `/help` | list the commands |
  | `/clear` | summarise to notes (compact request), save them, empty the history, rebuild the system prompt |
  | `/reset` | empty the history without a summary (notes kept) |
  | `/compact` | summarise now and continue with notes plus an empty history |
  | `/context` | show the tokens of the system prompt / tools / history and the limit |
  | `/notes` | print the notes |
  | `/forget` | delete the notes |
  | `/resume` | load the last saved session |
  | `/watcher` | domain watcher view: domain, unload plan, audit scorecard, last audits |
  | `/full` | the full model's answer to the last reply its audit found changed (boxed, markdown) |
  | `/copy` | copy the last reply to the clipboard |
  | `/yolo` | toggle approve-all |
  | `/exit` | quit |

- **Outside the project** (`src/outside.rs`, `Session::approve_outside`, 2026-11-04):
  - Root = the canonical session cwd. A path is resolved with `outside::resolve`: made absolute, `.` / `..` applied, and
    every existing prefix canonicalized (symlinks followed the kernel way: `link/..` is the target's parent); a tail
    that does not exist yet is applied lexically. Inside = `starts_with(root)` by components (`proj2` is not inside
    `proj`); the project's `.elisa/` is inside.
  - File tools (read, write, edit, edit_symbol, outline, read_symbol, glob, grep; glob / grep default `.`): the
    `path` argument. bash (`outside::bash_outside`, best effort, errs toward asking): zsh-ish words (quotes,
    backslashes; `>` `<` end a word; `; & | ( ) newline backquote` start a new command). A word counts when it is
    absolute, has a `..` component, or starts with `~` / `$HOME` / `${HOME}` / `$PWD`; `NAME=value`, `--flag=value` and
    `-I/path` take the value. `$VAR/…` (unknown) and `~user` count as outside as written. `cd` / `pushd` at a command
    start move the base (bare `cd` = home, `cd -` = outside); after a `cd` out, relative words with a `/` count too.
    `/dev/null`, `/dev/std*`, `/dev/tty`, `/dev/zero`, `/dev/urandom`, `/dev/fd/*` never count.
  - The request (`frontend::Outside`): access "read" (read-only tools) | "write" (write tools) | "bash" (treated as a
    write), and the resolved paths. `Frontend::approve_outside` asks; `Approval::Yes` = once, `Always` = these paths
    for the session (a write grant covers reads; a grant covers its subtree; unknown words are never covered),
    `No` = deny (`denied by user (<access> outside the project folder: <paths>)`).
  - Order: config `outside` = deny → denied, no prompt; allow → the normal rules; read-ok and a read → the normal
    rules; then a session grant → run; then `--yes` / `/yolo` and a write or bash → denied (message names
    `outside = "allow"`); then non-interactive (-p) → denied; else ask. The `.elisa/allow` list and `auto_approve`
    are never consulted before the outside prompt.
  - Config `outside`: the user file sets it; the project `.elisa/config.toml` may only tighten it (Allow < ReadOk <
    Ask < Deny), so a repo (or the model, which can write `.elisa/`) cannot open it up.
  - Displays: plain (`allow <access> outside the project? [y]es once / [s]ession for these paths / [n]o`, after a
    dim line with the summary and `outside the project · <access> · <paths>`); TUI (same permission box, a yellow ⚠
    line with the paths, options "Yes, once" / "Yes, allow these paths for this session" / "No"); `--serve-jsonl`
    (`approve` with `outside: true, access, access_word, paths, line`; answers `once`/`session` alias `yes`/`always`);
    the app's permission box (warning-coloured path block and border). App playback answers outside boxes only from
    `@outside once|session|deny` / `{"outside": …}` lines of the prompt; with none left it denies and logs
    `FINDING: outside-folder request …`.
- **Sessions:** the session is saved after every completed turn (`Store::save_session`, one file per run).
- **ui.rs:** ANSI only (no TUI crate). Elisa's text is in the default colour; tool lines, status and notes are dim; errors are red; the prompt is `› `. Input is read with `stdin().read_line`. A line ending in `\` continues onto the next line.

## Rules for every agent

- Only edit your own files (listed above). Do not edit types.rs, lib.rs, main.rs or Cargo.toml. If you need a
  dependency, say so in your report.
- `cargo check` only; **do not write or run tests**. Self-tests happen in a later phase.
- Use your own target dir: `CARGO_TARGET_DIR=/tmp/elisa-<letter> cargo check`. Other agents' files may be
  mid-edit; if an error is in a file that isn't yours, re-run after a short wait, or report it.
- No Python. No comments that restate the code; match a terse Rust style.
