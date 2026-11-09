# elisa eval

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Regression checks and model facts

`elisa eval` runs a fixed set of scenarios against Elisa itself: each case starts `elisa --serve-jsonl` (the protocol
the app speaks) in a fresh temp project with a temp HOME, sends user turns, answers approvals and checks what
happened. It catches regressions before a commit, and it measures a model with Elisa on small tasks so a user can
compare models with facts. Elisa never picks or recommends a model: with no model chosen, model cases do not run and
the usual "No model chosen" message is printed.

```sh
elisa eval                         # every case (needs a model for the model suite)
elisa eval --suite fake            # only the scripted cases: no model, ~5 s (also `cargo test --test eval`)
elisa eval --suite model --model MODEL --json run.json
elisa eval --case core.loop_guard_repeat --keep   # keep the temp project, home and the event log
elisa eval --list
```

Flags: `--suite NAME|all|fake|model`, `--case ID`, `--model M` (overrides the connection's model), `--provider NAME`
(model cases use that saved connection instead of the active one; an unknown name is a usage error), `--base-url URL
--type T` (a server with no saved connection, e.g. Ollama: `--base-url http://127.0.0.1:11434/v1 --type openai
--max-ctx 8192 --model M`), `--max-ctx N`, `--json FILE`, `--save` (also keep the whole report as
`~/.elisa/evals/reports/<started>-<model>.json`; the history line names it in `report`), `--keep`, `--list`, `--dir DIR`
(read cases from another folder), `--jobs N` (scripted cases at a time, default 4), `--no-history`. Exit 0 = all
passed, 1 = a case failed, 2 = usage error or no model. SIGTERM / SIGINT / SIGHUP (the app's Cancel, Ctrl-C) kill the
running sessions' process groups, remove the temp folders and exit 128 + signal; nothing is written to the history.

**Two kinds of case.** `fake` cases (suites `core`, `tools`, `security`, `context`) talk to a scripted model
(`src/eval_fake.rs`), so they test Elisa's own behaviour whatever the model: loop guard, unknown tools, empty-reply
retry, reasoning gate, edit prechecks, approvals, parse guard, check before done, secrets on disk, redaction, the shell
sandbox, untrusted-text fences, the audit chain, `/clear` notes, state cards across sessions, Esc. `model` cases use
the configured connection (or `--model`): tool use when asked, no tools on chat, right answers to small tasks, counts
taken from tool output rather than made up, edits that land, a "no" that is respected, secrets kept off disk, speed.

**Isolation.** Per case: `$TMPDIR/elisa-eval-…/{project,home,outside}`. Only `~/.config/elisa/providers.toml` is copied
into the temp home (model cases); the user's `config.toml`, MCP servers, memory and `~/.elisa` stay out. Each case has
a timeout (60 s scripted, 300 s model unless `timeout_s`), the child runs in its own process group and is killed at
the end, and the temp folders are removed unless `--keep`. After a run the command appends a one-line summary to
`~/.elisa/evals/history.jsonl` (not with `--no-history`; the test suite never writes there).

**Case files** live in `evals/*.toml` (built into the binary). One file per suite:

```toml
suite = "tools"            # default for the cases below
kind = "fake"              # fake | model

[[case]]
id = "tools.approve_yes_no"
description = "yes runs the write; no denies the command"
config = "redact = \"always\"\n"   # optional: the temp ~/.config/elisa/config.toml
project_config = ""                # optional: the project's .elisa/config.toml
approve = "no"                     # answer to approvals a turn does not list (default "no")
timeout_s = 60
platform = "macos"                 # optional: skipped elsewhere
[[case.files]]                     # fixture files (content repeated `repeat` times)
path = "a.txt"
content = "DB_PASSWORD={{secret1}}\n"
[[case.home_files]]                # optional: files in the temp HOME (e.g. .elisa/commands/review.md)
path = ".elisa/commands/review.md"
content = "Review $ARGS."
[[case.script]]                    # fake cases: the model's replies, in order
tool = { name = "write", input = { path = "new.txt", content = "hi\n" } }
[[case.script]]
text = "wrote it"                  # also: tools = [..], stop, out_tokens, repeat, repeat_text, slow
[[case.turns]]
send = "make new.txt"
approvals = ["yes"]                # yes | always | rule | no, in order (rule = yes + save the offered pattern)
# restart = true / continue = true: a new session (with --continue) before this turn; interrupt = true: Esc at the first text
[[case.checks]]
type = "file_contains"
path = "new.txt"
text = "hi"
```

Placeholders: `{{secret1}}`…`{{secret9}}` (fresh random values each run, so no secret sits in the repo), `{{project}}`,
`{{home}}`, `{{outside}}`. Checks (`turn = N` limits one to turn N; reply checks default to the last turn):
`reply_contains` (`text` or `any = [..]`, case-insensitive), `reply_not_contains`, `reply_matches` (`regex`),
`reply_not_empty`, `tool_called` (`name`, optional `args` substring of the call, `min`/`max`), `tool_not_called`
(`name`; `*` any, `a|b`, `x*` prefix), `tool_result_contains`, `no_tools_offered` / `tools_offered` (`prefix`; fake
cases read the requests, model cases ask `/context`, which covers the reasoning, docs and sec tools), `file_contains`,
`file_not_contains`, `file_unchanged` (vs the fixture), `file_absent`, `max_steps` (`n` model calls in a turn),
`max_seconds` (`n`, slowest turn), `no_secret_on_disk` (`values`: grep of the project's `.elisa/` and the temp home),
`audit_has` (`event`, `reason`, `tool`, `min`/`max`), `approval_for` / `no_approval_for` (`tool`), `approvals`
(`min`/`max`), `stopped_by_loop_guard`, `info_contains` / `info_not_contains` (info and error lines),
`request_contains` / `request_not_contains` (fake cases: what Elisa sent the model), `event` (`event` type, optional
`text` in its JSON).

**JSON report** (`--json FILE`, schema `elisa-eval/1`; the model scorecard reads it):

```json
{
  "schema": "elisa-eval/1",
  "elisa_version": "0.1.0",
  "started_at": 1794150000, "finished_at": 1794150420,        // unix seconds
  "suite": "model", "case": null,                              // the selection
  "model": {"name": "MODEL", "connection": "ollama", "base_url": "http://127.0.0.1:11434/v1"},  // null without model cases
  "summary": {"cases": 11, "passed": 9, "failed": 2, "skipped": 0},
  "aggregates": [{                                             // one per (kind, model); scripted cases are model "eval-fake"
    "model": "MODEL", "kind": "model", "base_url": "…",
    "cases": 11, "passed": 9, "failed": 2, "skipped": 0, "pass_rate": 0.818,      // rates are 0..1 or null
    "suites": {"model": {"cases": 11, "passed": 9, "pass_rate": 0.818}},
    "turns": 11, "median_turn_s": 2.1, "max_turn_s": 41.0, "median_ttft_s": 0.4,  // user turns, send → turn done
    "tool_call_accuracy": {"checks": 12, "passed": 11, "rate": 0.917},           // tool_called / tool_not_called checks
    "empty_reply_turns": 0, "empty_reply_rate": 0.0, "empty_retries": 1,          // visible reply empty after Elisa's retry
    "loop_guard_stops": 0, "model_calls": 30, "tool_calls": 14, "tool_errors": 1,
    "output_tokens": 2400, "max_ctx_used": 5200, "thinking_chars": 0
  }],
  "cases": [{
    "id": "model.read_when_asked", "suite": "model", "kind": "model", "model": "MODEL",
    "description": "…", "status": "pass",                    // pass | fail | skip
    "seconds": 6.2, "notes": ["…"],                           // failed checks' details first
    "checks": [{"type": "tool_called", "pass": true, "detail": "read ~\"notes.txt\": 1 call(s) — calls: [read]"}],
    "metrics": {"turns": 1, "turn_s": [5.9], "model_calls": 2, "max_calls_per_turn": 2, "output_tokens": 61,
      "max_ctx_used": 3100, "tool_calls": 1, "tool_errors": 0, "approvals": 0, "empty_reply_turns": 0,
      "empty_retries": 0, "loop_guard_stops": 0, "thinking_chars": 0, "median_ttft_s": 0.5},
    "dir": null                                               // the kept temp folder with --keep
  }]
}
```

`~/.elisa/evals/history.jsonl` holds one line per run with `schema`, `elisa_version`, `started_at`, `finished_at`,
`suite`, `case`, `model`, `summary`, `aggregates`, `cases` as `[{id, status, seconds}]` and `report` (the saved report's
path with `--save`, else null). The app's Model Scorecard (Elisa > Model Scorecard…) reads it; its "Test this model"
runs `elisa eval --suite model --provider NAME --model M --json TMP --save`. Fields may be added within
`elisa-eval/1`; a change of meaning gets a new schema name.

Code: `src/eval.rs` (runner, checks, report), `src/eval_fake.rs` (scripted model), `evals/*.toml` (cases),
`tests/eval.rs` (the scripted cases under `cargo test`).
