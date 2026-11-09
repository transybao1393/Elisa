# Internals

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## One mode

There is one mode. Each turn's requests carry the tool groups the user's words call for (intent routing, config
[`route`](config.md#route), or the optional [hosted router](models.md#hosted-intent-router-optional-paid)); with
`route = "off"` every request carries every tool. The model is told to use tools only when the request needs files,
commands, the web or this machine, and to answer plain questions directly. `/chat`, `/code` and `/mode` only say so;
old configs with `auto_mode` / `chat_window` still load, those keys ignored. Design contract:
[DESIGN.md](../../elisa/DESIGN.md).

Reasoning models stream their thoughts live in a three-line ticker before the answer.

## Tool calls the model cannot make

**Tool calls the model cannot make.** Only a tool the request offered runs (Elisa's own and the MCP servers', as
the reasoning gate filtered them for this turn). A call to any other name (a name the model made up, or
`think` on a turn the gate left it out) gets an immediate error with a few names that do exist, those that look like
it first; there is no approval box and nothing runs.

## Loop guard

**Loop guard** (`src/loop_guard.rs`, per turn, no model call). It looks at the calls, not their results (a web search
or a listing can differ a little every time):

- the same tool with the same input (keys sorted, whitespace collapsed, a search query lowercased) a third time does
  not run; the model gets `[elisa: not run: this exact call already ran 2 times …]`. A real change resets it: an edit
  or write to a file resets the count of that file's reads and of `bash` / `grep` / `glob`, and a new shell command
  resets the reads, so read → edit → read and edit → test → fix → test never trip it (an edit whose old and new text
  are the same is no change). A second step with such a call stops the turn.
- a window of 2–4 calls repeated back to back (outline → glob → read → read, grep → read → think) gets one note to use
  what it has; if the cycle comes round again the turn stops.
- after 10 tool steps without a change, one soft note: answer with what you have, or say what blocks you.

Each note shows as a `[loop guard: …]` line; a stop shows `[stopped: …; say how to proceed]`.

## Server errors

**Server errors.** A model call that fails with a server error (HTTP 5xx) or a tool call the server could not parse
(Ollama's `invalid character '}' after object key`) is sent once more when nothing was shown yet; 4xx errors are not
retried. A second failure ends the turn with one line saying what happened (`the model wrote a malformed tool call
… ask again, or more simply`, `the model server failed, twice …`, or for 401 / 403 `check the connection's API key`).

## Internal errors

An internal error (a panic) in a turn or command never hangs the session: the line ends with an `internal error: … at
file:line` error (and `turn_done` in the app), the turn's request gets a `[stopped: internal error]` reply, and the next
message works as usual; a panic inside the memory store turns memory off for the rest of that session.

## Companion window

Companion window: `app/ElisaCompanion/run.sh` (builds with `swift build -c release` on first run) opens Shade in a small
borderless floating window drawn by the brand kit's own renderer (`elisa-shade.js` v2 over `elisa-character.js` v1,
`colors.json`, `layout.json`, read from `design/` at launch). Then `elisa --companion` (or config `companion = true`)
mirrors Shade's state there over the Unix socket `~/.elisa/companion.sock` (`ELISA_COMPANION_SOCK` overrides) and hides
the terminal dock while the window is connected; the status line stays. Clicking Shade or the water / food / toy tray
in the window is the same as Ctrl-G / F2-F4. Without the window running, `--companion` changes nothing. Drag the
window anywhere; the paw icon in the menu bar toggles always-on-top and quits. `elisa-window --companion [dir]` starts
the window (if built) and Elisa together. Protocol: `src/shade/link.rs`.

## App protocol (`--serve-jsonl`)

**In the app (`--serve-jsonl`, full protocol in `src/jsonl.rs`; protocol v2: every line Elisa writes has `"v":2` and
`hello.protocol` is 2. v2 = v1 plus `send.documents: [{path}]`, absolute paths of attached files, at most 8, see
[Attached documents](documents.md#attached-documents); the `prefetch` {documents: [{path}]} command, which checks files attached in the composer before
Enter; and the `activity` event below).** Events:

- `doc_info` {path, name, kind, bytes, pages, unit, sheets, words, tokens, lang, text_layer, category, category_word,
  sections: [{title, pages: [from, to]}], tables, outline, parts: [{title, pages: [from, to]}], fit: {plan:
  "whole"|"overview"|"selection", doc_tokens, budget}, warnings, line} — an attached file's intake, after `prefetch`
  and again when the message is read (`line` = the chip's one-line result). A file that cannot be attached: {path,
  name, refused: true, warnings: [why], line: "⚠ why"}
- `reply_retracted` {} — the reply streamed in this turn said it could not read the attached document: the app removes
  it, and the reply to the retry follows
- `history` {items: [{role: "user"|"assistant", text}]} — a resumed conversation's earlier turns, right after the
  `banner` (`--continue`) or the resume line (`/resume`, "continue the last one"); additive, no protocol bump. The
  user's own words only (no Working directory / OS / Date / clock header, notes, `[state]`, recalled memories,
  situation or follow-up hints; an attached document as its `[Attached: …]` line, never its text), replies as written
  with each tool call as one line (`used read on README.md`), tool results left out; the last 40 user turns
  (`context::earlier_turns`). The app shows them as normal user and reply blocks between `— earlier in this
  conversation —` and `— continued —`; the TUI shows the last 3 turns the same way

- `activity` {phase, id, kind, text, …} — what Elisa is doing, in plain words for the person, never tool names
  (`src/activity.rs` holds the one mapping from tool + arguments to a phrase; arguments are redacted and shortened,
  paths show only the file name). `phase`: `start` (`text` = the live line: `Searching the web for “Apple news”…`,
  `Reading report.pdf…`, `Looking through your project files…`, `Running the tests…`, `Using gh tool list issues…`),
  `update` (same `id`; `waiting_ok: true` while its approval is open: `Editing app.py (waiting for your OK)`; a lazy
  MCP server start: `Starting the web reader…`), `done` (`text` = `Read apple.com` / `… didn't work` / `Skipped: …
  (you said no)`, `summary` = `5 results` | `12 pages` | `exit 0`, `ms`, `error`) and `summary` (once per turn that had
  steps, before `turn_done`: `Searched the web · read 2 pages · 3.1 s`; a pure chat turn has none). Model requests
  are `kind: "model"` starts (`Waiting for the model…`, then `Thinking about the next step…`) updated to `Writing the
  answer…` / `Thinking it over…`; a local model that is likely cold (first request, or idle for minutes) carries
  `slow_after_ms` / `slow_text` and is updated to `Loading the model (first message)…` when no token came by then.
  The app shows the newest line under the last message (a step under 300 ms never takes it) and folds a finished
  reply's steps into the summary line, which opens to the steps and the detailed tool rows; the TUI shows the same
  live line in its status bar

- `plugins` {plugins: [{name, version, description, source: "user"|"project", enabled, trusted, servers: [{name,
  runs: "container"|"host", installed, tools, tokens, running?, state: "ok"|"failed"|"not installed", error}], tools,
  tokens, rules, commands, notes}]} (`error`: why the server did not start — this session's start, else the last
  probe after install / update; null when it runs) — at
  startup (when any plugin is installed or the project has one), after every change and on `/plugin list`.
  `elisa plugin list --json` prints the same payload without a session (the app's Plugins window uses it when no
  folder is open)
- `plugin_preview` {op: "add"|"update"|"trust", name, text, preview: {name, version, old_version, description,
  source, host, servers: [{name, runs, installer, package, version, image, image_present, command, mounts, env,
  tools}], rules: [{level, pattern, broad}], commands: [{name, description, args, text}], notes, problems,
  missing_images}} (`text`: the command's full prompt text) — the setup screen; a yes /
  no `approve` with `question: true` follows unless `problems` is non-empty. With `missing_images`, Elisa first asks
  whether to download them (tool `sandbox pull IMAGE…`) and then sends the preview again
- `sandbox_images` {images: [{image, lang, version, label, purpose, stable, project, default, present, size_mb,
  disk_mb, pinned}], error?} (`default`: the line used with no project version, and by plugin installers) — the pinned image list and what is on this machine (on `/sandbox`, after a download, and on a
  `sandbox` list request); `elisa sandbox list --json` prints the same without a session
- `image_pull` {image, state: "pulling"|"done"|"kept"|"cancelled"|"failed", text, done?, layers?, elapsed_s?} — a
  download's progress and end; `interrupt` cancels it
- `plugin_result` {op, name, ok, working, text} — after add / remove / enable / disable / update / trust; `working`
  false with `ok` true: installed / updated, but a server did not start (the text says which and why)
- `project_trust` {path, hash, text, commands, rules, plugins} — before the project's trust question
- `approve` gains `rule` (the pattern "always allow this pattern" would save; answer `"rule"`) and `question`

Request: `{"type":"plugin","op":"list"|"add"|"remove"|"enable"|"disable"|"update"|"trust", "path": "…"}` (add) or
`"name": "…"` (the rest; `trust` with `"project"` trusts the project's commands) — the same as sending `/plugin OP ARG`.
`{"type":"sandbox","op":"list"|"pull","name":"…"}` — the same as sending `/sandbox OP NAME` (a pull asks first).
