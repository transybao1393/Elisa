---
name: elisa-chat-test
description: Elisa's real-use test runner, a real conversation with the user's real model in a visible window. Use with the general real-use-test method whenever testing Elisa.
---

# Elisa real-model chat test

The method in short: run the real app against a real model in a visible window, read every reply yourself (a passed pattern check is not proof), and report each step as passed, failed or could not be tested, never "partly".

Unit tests passing does not mean Elisa works. "Working" means a real conversation with the user's real model behaves the way a person expects. This skill runs that conversation where the user can watch it, then reports what actually happened.

## Run

```zsh
.claude/skills/elisa-chat-test/scripts/run.sh chat       # everyday conversation (the default bar)
.claude/skills/elisa-chat-test/scripts/run.sh features   # images, tool read, interrupt
```

The script:
- builds the core (release) and the app (debug, a few seconds);
- copies only the user's active connection into a temp HOME (model, address, window size; never keys, never ~/.elisa);
- opens the test window in front on the main display (`--frame 320,130,1280,820`);
- drives the real send path (the app's `--selftest` / `--selftest-chat`, SelfTest.swift);
- prints the results;
- leaves the window open for the user;
- closes the previous run's window first, so only one test window is ever open.

Run it in the background, and don't block on it.

When there are several independent failures, send each one to its own agent in parallel. Brief each agent with the cause to find, how to run a real turn over `--serve-jsonl`, and these rules. Then rerun the scenario yourself once both are done.

## What the chat scenario checks

| Step | Sends | Must |
|---|---|---|
| greeting | hey | short hello, no tools |
| tell-a-fact | My name is Linh and I live in Hanoi. | acknowledge, no tools |
| recall-in-chat | What's my name? | say Linh (context) |
| time / time-follow-up | what time is it? · and in Tokyo? | a clock time, no tools (the clock line) |
| joke / why-follow-up | tell me a short joke · why is that funny? | answer from context, no tools |
| image / image-follow-up | picture + what is this? · ear colour | describe it, then "blue" (only if the model has vision) |
| file-intent | read notes.txt and tell me the meeting code | runs a read tool, says ORCHID-42 |
| web-intent | search the web: capital of Australia | runs a web tool, says Canberra |
| resume | (core restarted with --continue) What's my name and where do I live? | Linh and Hanoi |
| resume-context | what was the meeting code you found? | ORCHID |

Every reply is also checked for leaked internal text (`<state>`, `<=80`, "state card").

## Report

1. Read `transcript.txt` yourself: every reply, as the user would. A step can pass its pattern and still read badly. Flag these:
   - pitching ("I can help with coding, chat…") nobody asked for;
   - the wrong person ("my" answered about Elisa);
   - stale context;
   - wrong language;
   - rambling.
2. Look at one or two of the PNGs (window pictures per step).
3. Report a table, Step | Result | What Elisa said, with every FAIL first and its cause. No "all passed" unless the table shows it.
4. Say plainly what the run does not cover.

## Fixing

- Fix the cause in Elisa (prompt, router, context), not the test's patterns.
- After a fix, run the scenario again and report the new table.
- A failure that comes from the test setup (for example, a missing connection setting) is the test's fault; say so.

## Rules

- Never pull container images, never touch ~/.elisa or ~/.config/elisa, and never send synthetic mouse or keyboard input.
- Never close or restart the user's own Elisa window. The test window is separate.
- The model is whatever the user's connection uses. Elisa never picks or recommends one.
