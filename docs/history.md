# Elisa development history

Dated notes, test-run findings (longrun, soak) and measurements that used to sit in the terminal README, grouped by
topic. They explain why things are the way they are; the current behaviour is in the
[reference](reference/README.md). Model names here are the models a run was measured on, never a recommendation:
Elisa never picks a model.

## Modes and the original gateway setup

- The terminal app started as "a small terminal assistant for local models, with coding tools": a REPL that talked
  to any Anthropic Messages API server and was built for `streamdense gateway` (Qwen3.8-27B, 8K context =
  `max_ctx = 8192` in `streamdense.toml`; measured prefill, decode and first-token times were in the streamdense
  repo README and its `docs/TEST_LOG.md` Tests 19 and 23). Nine tools (read, write, edit, bash, glob, grep, and the
  syntax-tree tools outline, read_symbol, edit_symbol), plus the tools of any MCP servers (the built-in web server's
  `web_search` and `web_fetch` and the documents server's `docs_pdf` and `docs_sheet` by default), streaming output,
  notes that survive `/clear`, and auto trim/compact before the context runs out.
- 2026-11-01: the chat/code mode split was removed. From then on there was one mode: every request carried the code
  system prompt and all tools, and the model was told to use tools only when the request needed files, commands or
  this machine. `/chat`, `/code` and `/mode` just say so, and old configs with `auto_mode` / `chat_window` still load
  with those keys ignored. Intent routing (`route`) later made each turn carry only the tool groups it needs.
- The README's Run section used to say the gateway must be running first (it lives in the streamdense repo; check
  `df -h /` and `sysctl vm.swapusage` before loading), started with
  `~/Development/streamdense/engine/target/release/streamdense up` (sd-watchdog + nice + gateway with the values in
  `streamdense.toml`: cap 16 GB, port 8898, max_ctx 8192, MTP). The gateway is now one optional backend.
- Speed rules as first written for the gateway, with their numbers: append-only history made every step a gateway
  prefix hit (~0.5–1 s first token instead of a full re-read); one fixed system turn, byte-identical across projects,
  days and sessions, let every new session and every `/clear` restore the gateway's stable snapshot (~50 ms) instead
  of prefilling ~860 tokens (~13 s); the prefix was ~940 tokens real (including Qwen's ~236-token tool preamble) vs
  ~1,235 for Claude Code `--bare`.

- The demo GIF in the terminal README was recorded live on `qwen3-vl:2b-instruct` in Ollama on a laptop; the window
  is driven by the app's scripted playback.

## Turn loop, replies and errors

- Before 2026-11-09, Esc / Ctrl-C waited for the server's first byte: it waited 3.6 s for qwen3-vl:2b's first token.
  The network reads then moved to their own thread (`api::post_lines`), so a cancel takes about 50 ms.
- 2026-11-09: three "continue" turns in a row stopped mid-sentence at 1024 tokens. That led to the automatic
  continuation of a reply cut by the output limit.

## Sessions, state cards and notes

- 2026-11-09: one card, "read the first few pages of the PDF file", was carried into every request for hours after
  the user had moved to another paper. Cards are now kept per topic.
- When the last two answered turns got the same reply, the 2B model copied it again (measured: the intro three times
  running). The older copy is now left out of the request.
- Before 2026-11-09 the memory store's derived hot files were shared between processes: a second session's turns and
  `/clear` rewrote them under an idle first session, whose next turn panicked in elisa-mem and hung (longrun2 F1).
  They are now private to each process.
- 2026-11-09: `--continue` brought back the shared `.elisa/notes.md` of older versions, and an old Python-async
  task's facts and a "hey" session goal were merged forward through a PDF chat. Notes are now one file per session,
  and the shared file is never read.
- "hi Elisa! how are you today?" once became a session goal; a greeting, small talk or a plain question no longer
  does.
- longrun2: the first 240 characters of a 200 KB paste were the session goal and survived `/clear`. The goal now
  comes from the user's own words, never from pasted text.
- Before 2026-11-09 an instruction inside a fenced web page could become the notes' next step and carry across
  `/clear`. Outside text in notes is now quarantined.
- The summary request used to carry the old notes; Elisa now merges new summaries into the notes itself.

## Small context windows

- Soak 2026-11-08 at 8K: 40 folds in 40 turns, 47 tool results "trimmed to fit (0 tokens left)".
- After the fold-point and result-slice changes, the same 16-turn script on qwen3-vl:2b-instruct at 8K, before →
  after: compactions 15 → 4–12 over three runs (the 2B model's tool loops vary), tool results cut 17 (4 of them to
  "0 tokens") → 0, no fold grew the prompt; at 16K: 4 → 4 compactions, cuts 3 → 0.
- Config `fold_at`: at 8K with ~1.4K fixed cost the fold point is ~4.3K, up from 3.8K under the old "share of the
  whole limit" rule, which sat barely above the fixed cost and folded before almost every turn.

## Attached documents

- The docs tools are not offered with an attached document by default: on the measured 2B model the tool
  definitions made the reply a state card.
- Measured with qwen3-vl:2b-instruct, a 38-page arXiv paper of ~32K tokens, 16K window, six turns ("read this one for
  me", "what are the agent roles?", "and what about the risk team?", "continue", "what were the results on AAPL?
  give the exact numbers", "and the Sharpe ratio for GOOGL and AMZN?"): prompts of 3.1K, 5.6K, 5.2K, 6.1K, 7.2K and
  7.7K tokens, no fold (before: the text stayed in the history and a fold at 10.1K → 3.1K dropped it); turn 5
  answered CR 26.62%, ARR 30.5%, SR 8.21, MDD 0.91% (all right; before the table labels it read the AMZN column),
  turn 6 GOOGL SR 6.39 (right; AMZN left out). 4–14 s a turn.

## Coding tools

- Adding tree-sitter grew the release binary (arm64) from 7.5 MB to 20.7 MB.
- Measured 2026-11-03 (Elisa's token estimate, real tool outputs at the 6000-char cap, task = outline a file and read
  one function): elisa/src + mem/src (31 files) 254.7k tokens whole vs 23.3k for outline + the median-size symbol
  (10.9x fewer), 50.9k for outline + the largest (5.0x). engine/crates (115 files): 9.5x / 3.7x. app Swift (29
  files): 7.2x / 3.4x. On a small crate (31-line split.rs) it was 1.6x / 1.0x: read small files whole. To rerun:
  `cargo test --offline --test ast_measure -- --ignored --nocapture`.
- Check before done: the idea came from forheader test G13.

## Model connections and backends

- No prompt cache needed, measured 2026-11-09 with the same 2B GGUF (qwen3-vl:2b-instruct's text weights), one 8-turn
  session (greeting, "what can you do?", Tokyo time, read and summarise README.md, thanks, a pasted traceback, "ok,
  and why?", `/compact`, a question about the crash): `llama-server --no-cache-prompt --cache-reuse 0 --cache-ram 0`
  answered every turn correctly, with the same prompt sizes as with caching on (163 to 2.3K tokens on a turn's first
  request) and reused 0 throughout. First-token time grew with the prompt instead (0.15 s for "hey", 1.1 s for a
  1.6K-token tool turn, 1.7 s at 2.3K, about 1400 prompt tokens/s), against 0.03–1.0 s with llama.cpp's cache on and
  0.02–0.9 s on Ollama. With a 4K window (`max_ctx = 4096`, server `-c 4096`) the folds and the "would not fit" stop
  kept every request under the server's limit (no context errors). A chat turn sent about 0.2–0.8K tokens instead of
  1.6K+ with every tool.
- llama.cpp slot restore, measured 2026-11-09 (qwen3-vl 2B GGUF, `-c 16384 --cache-ram 0 --slot-save-path`, a
  2.4–2.6K-token session, the slot first filled by another prompt): first token after `--continue` 1.9–2.0 s without
  the restore, 0.0–0.1 s with it (3 runs each); save 0.1–0.6 s.

## Intent routing, follow-ups and greetings

- Intent routing, measured on qwen3-vl:2b: "hey" went from 1.9K to 0.27K prompt tokens.
- Small talk never runs a tool: on qwen3-vl:2b, "hey" after a turn the loop guard stopped called tools, the widen
  path ran them, and the turn looped to 11-42K tokens.
- Follow-up hints, measured on qwen3-vl:2b: without them "more", "the second one" and "tiếp đi" got the last answer
  again word for word, and "why?" got "I can't provide the answer".
- 2026-11-09: "add new tips" in a document chat got "details that are not in the original paper" and an unrequested
  code example, so follow-up hints now stay with the document.
- A bare "yes" after an answer offering several options: the 2B model otherwise picked the first, a code example.
- Config `gate_tools`: a plain coding turn at 8K sends 1.0K tokens of tools instead of 1.4K (2.4K before, with the
  soak config's run_code and time server).
- Config `think_simple`: a flat 1024-token cap cut plain chat answers mid-sentence, so the cap became 1/8 of the
  window. qwen3-vl:2b on "hi": 150 s of reasoning with no answer before, under 1.5 s after.
- Empty replies: qwen3-vl:2b sometimes answered "thanks" with only the state card.
- Config `greet_context`, measured on qwen3-vl:2b: "hey" and "chào" 9 of 9 answered with the one situation line in
  one request, ~1.7K prompt tokens with the history, where before "hey" looped to 11-42K. Prompt sizes on
  qwen3-vl:2b-instruct: "hey" with no context 163 tokens; returning user 204; mid-conversation ~285 (history
  included).

## MCP servers

- `@playwright/mcp` 0.0.84 lists about 5.6K tokens of tool definitions (measured with `/mcp`); an older version was
  ~4.1K. The recommended eight-tool set took a session's tools from 7.3K to 3.0K with qwen3-vl:2b at 16K.
- Malware scanning (YARA-X) was removed from the security server; it will return after the ratatui upgrade allows
  yara-x ≥ 1.19. The unknown-tool example in the README used to be `sec_malware` in a build without YARA.
- The reasoning tools cost about 250 tokens as sent to Qwen (measured with Ollama: 64 + 63 + 58 + 65).
- Cost of a reasoning-gate flip, measured with qwen3-vl:2b on Ollama: the tools follow the system prompt in Qwen's
  template, so the shared prefix (about 1.1K tokens: system prompt + Elisa's own tools) stays cached and only the rest
  is read again: about 500 tokens, 0.3–0.4 s, once per flip (a normal step reads 20–70 new tokens). That is why
  follow-ups keep the previous choice.
- Process-group shutdown made a wrapper like
  `command = ["/bin/sh", "-c", "\"$0\" \"$@\"; exit $?", "npx", "@playwright/mcp"]` unnecessary, and Ctrl-C at the
  terminal stopped reaching the servers.
