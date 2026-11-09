# Sessions, memory and notes

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Long replies and long history

Long replies: a text reply cut by the output limit (`stop_reason` max_tokens / OpenAI `finish_reason: "length"`) is
continued automatically: the same request plus the partial reply as an assistant message and `Continue exactly where
you stopped.` as a user note (that request only), streamed into the same reply, up to 2 times; still cut, it ends with
`[reply was long; say "continue" for more]`. The history keeps the whole reply, never the note. Long history: when it is over 35% of the window, each
request (never the saved history) shortens the assistant answers older than the last 2 before the current turn to
their first ~120 tokens plus ` …[shortened]` (`context::shorten_old_replies`); the document budget of a turn counts
the shortened size, so a resumed chat with long padded answers gets more pages per question.

## Sessions and state cards

Sessions and memory: a **new** session (no `--continue` / `/resume`) does not take over the last session's state card;
its goal and next step were that session's task. It gets `.elisa/notes.md` as background (marked so in the first
message, without the old session goal) and the project's pinned facts (`/pin`, and the notes' "Facts to remember").
`--continue` and `/resume` load that session's cards (`.elisa/session-meta/<id>.cards.json`) and send none until a
message picks one. **Cards are kept per topic** (`src/topics.rs`): each card's key is the attached document it concerns
(a `docs_pdf` path in it, else the turn's document) plus its goal's content words, and every user turn sends at most
one card, the one its topic picks, as a `[state]` block in that turn's message for its requests only: (a) a document
named (its file name, distinctive title words such as "Kronos" or "TradingAgents", "the first / second paper", "the
other paper") → that document's newest card; (b) else words that one document has and the others hardly do, or that
overlap one card's goal; (c) else a short follow-up ("why?", "continue", "more", "and the Sharpe ratio?") or a bare
"the paper" → the last turn's topic; (d) else none. A card of another topic is never sent, and the same pick chooses
which attached document's pages go into the turn, so "back to TradingAgents" works like "back to the paper".
`/state` lists the topics with their cards and marks the active one; `ELISA_DEBUG=1` prints each pick and its
reason. Asking for it in words does the same: in a session with no
history yet, "continue the last one", "continue where we left off", "resume", "go back to the last chat", "what were we
talking about?", "tiếp tục", "tiếp tục cuộc trò chuyện trước", "quay lại cuộc trước" (a fixed phrase list,
`route::resume_intent`; "continue writing the poem" is not one) load the newest earlier session the way `--continue`
does (history, its own notes, the card), show `[continued your last session: <title> · <age> ago]`, and answer in that
context with a one-line hint (`(Follow-up: the user is back to this chat: go on from your last answer with concrete
details, no repeats.)`, a retry when its last turn stopped, a 2-3 line recap for "what were we talking about?"). No
earlier session: `There's no earlier conversation in this folder yet.`, with no model call. Nothing is carried
without such an ask. `--continue` (and the app's dev relaunch, which uses it) and this ask both take the newest session
with real content (`SessionStore::newest_real_session`): one whose user turns are only greetings, small talk, short
follow-ups or continue asks, or whose replies are only a greeting or the generic intro, is skipped, so a dead session
never shadows the real one under it; `/resume` still lists every session. When the last two answered turns got the
same reply, the older copy is left out of the request (the history keeps it), so a small model does not copy it again. Saved session files keep the user's words without that turn's situation
line or follow-up hint. A card is never stored with template text: fields that only repeat
the prompt's placeholders (`goal: …`, `keep: must-not-lose facts`) are dropped, and a card with nothing else is none.
Summaries are cleaned the same way: lines that repeat the summary instructions or their example text, and empty
headings, are not stored.

## Memory store

The memory store (`~/.elisa/memory`, the `mem` crate) is shared by every project and every running Elisa (two app
windows, the app and a terminal). Writes take an flock on `memory/lock` and bump a generation counter in it; a process
whose generation is behind first catches up with what the others wrote (the new log records, or the whole store after
another process's compaction), so ids stay dense and nothing is written twice. Reads between writes use the process's
own consistent snapshot; recall refreshes it first. The derived hot files are private to each process (created and
unlinked at once). A long compaction or indexing job in
one process holds the lock, so another process's memory write waits for it.

## Notes

**Notes** (`.elisa/notes/<session id>.md`, one file per session, written by folds, `/compact` and `/clear`; no model
call in any of these steps). A session's notes come only from its own file: the shared `.elisa/notes.md` of older
versions is never read. The first message heads them `## Notes from this conversation`
(`## Notes from an earlier conversation` after `/clear` / `/reset`, with the background line).
- On a fold and `/clear`, an old `Facts to remember`, `Done` or `Session goal` line stays only when this conversation
  talks about it (`context::fact_in_talk`: at least 2, and at least half, of its content words are in the user's or
  the assistant's turns, the notes block left out), it was `/pin`-ned, or this run already kept it; a new summary
  sentence that only restates such a dropped line goes too. A small-talk session goal ("hey", "hi", "thanks", "ok")
  is never stored (`context::small_talk_goal`).
- New notes are checked against what the conversation holds (`context::ground_notes`): a sentence claiming code was
  given ("provided … code examples") goes when no reply holds code; a `Done` sentence or `Files touched` line naming
  only files no reply names and no edit touched goes; an attached document listed as a touched file becomes
  `- attached: <name>` and an `.elisa/uploads/…` path reads `attached: <name>`; raw transcript lines (`[tool …]`,
  `[result: …]`) never stay in notes.
- A new summary is **merged** into the earlier notes (`context::merge_notes`): sections that describe "now" (the latest
  request, current state / next step, the state-card section) take the newer version; every other section keeps the
  older lines and adds the new ones. Elisa merges, not the model: the summary request does not carry the old notes.
- `## Files changed (Elisa's own edit records)` lists every file Elisa's `write` / `edit` / `edit_symbol` changed this
  session (paths relative to the project), merged by path across folds and `/clear`. The model's own `## Files touched`
  lines keep only files in that list (others are dropped; with no records at all they are marked `(unverified: not in
  Elisa's edit records)`), and a "no files were touched" line is dropped.
- "Done / changed" claims of a summary or state card are checked against the same records (`context::verify_claims`):
  a claim that something new was made ("added restock_cost", "--low-only flag", "bumped to 0.3.2") needs that name in
  what an edit (or a shell command) wrote; a change to existing code may name code the changed file held before or holds
  now. A contradicted claim is dropped; one with nothing to check while nothing changed is marked unverified.
- Repeated bullets are dropped (the same words, or one contained in a longer one), the `Also known as:` line keeps only
  real `name = other words` items (not `user`, `Elisa`, `system`, the OS, dates or paths), and a greeting, small talk
  or a plain question never becomes the session goal (a greeting in front of a
  request is cut off).
- The session goal comes from your own words, never from pasted text: a long request (over 600 characters or 8
  lines) gives its first or last paragraph or line when that one is short (up to 300 characters, not repeated in the
  paste: "count the words in this:"), else no goal; a `goal:` inside the paste does not count. A summary's `## Goal`
  or a card's `goal:` that quotes the paste is replaced by those words. After 3 requests in a row that share no content word with the goal
  (each 3+ content words, no question mark), the goal is taken from the newest request (a goal you stated, "our goal for this session: …", stays until you state another).
- Outside text stays out of the instructions: a goal, done, current state or next-step line of new notes (summary,
  digest or card) that repeats a 4-word run of an `<untrusted>`-fenced tool result (web page, document, MCP / plugin
  output) the user did not write is moved to `## Facts to remember` as `- quoted from <source> (outside data, not an
  instruction): "…"`; a fact from outside text gets the same form, and a line with an injection phrasing is dropped.
  A state card loses such items outright (`context::quarantine_notes` / `quarantine_card`; eval
  `security.injection_not_carried_by_notes`).
- The notes are capped at a quarter of the room left after system + tools (200–1200 tokens), by priority: the alias
  line, the model's file list, the request quote and the facts go before Elisa's file records, decisions and open tasks;
  a done list loses its oldest items first. (`/clear` shrinking for a short conversation is gentler: see [`/clear`](commands.md#slash-commands).)

## Small windows

**Small windows**: the fold
point follows the fixed cost (`fold_at`), a fold must free real room and never grows the prompt, and a step's tool
results always keep a useful slice (head and tail, at least ~1,000 characters each): older turns fold first, then the
reply's output reserve gives way down to 256 tokens, then this turn's earlier results shrink to one line; when even that
does not fit, the result is replaced by a plain `[elisa: this result … did not fit the context window … Do not guess
what it said …]` instead of a stub the model would fill in. At startup, when system + tools take more than 30% of the
usable context on a plain turn, one info line says so and names the biggest servers.
