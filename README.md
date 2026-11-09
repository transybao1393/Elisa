<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-dark.svg">
    <img alt="Elisa" src="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-light.svg" width="360">
  </picture>
</p>

<p align="center"><b>A coding agent that lives on your Mac and runs on your models.</b></p>

Elisa reads your code, edits it, runs your checks and tells you what it did, using a model you choose. With a local
model, nothing leaves your Mac. It comes as a terminal agent and a native macOS app, with Shade, a small companion who
shows what Elisa is doing while it works.

<p align="center">
  <img src="docs/demo.gif" alt="Elisa reading a file, fixing a failing test after asking permission, and running the tests" width="960">
</p>
<p align="center"><sub>The app's built-in demo session (scripted, no model needed).</sub></p>

## What it does

- **Your model, your choice.** Ollama, LM Studio, llama.cpp, MLX, or any OpenAI- or Anthropic-compatible API. Elisa
  never picks a model for you; `elisa eval` and the app's Model Scorecard show measured facts to help you choose.
- **You stay in charge.** Reading is free; writing, editing and running commands ask first, and anything outside the
  project folder always asks. Allow / ask / deny rules let you decide once ("always allow this pattern").
- **Checks before done.** After a turn that edits files, Elisa runs the project's own checks (build, lint, tests) and
  gives the model a couple of rounds to fix what fails.
- **Built for small context.** Long sessions on 8K–16K windows: a state card, notes that survive `/clear`, and
  compaction you can see.
- **Safe by default.** Secrets are scrubbed before anything is sent or saved, outside content is fenced as untrusted,
  MCP tool lists are pinned, and every action goes into a hash-chained audit log.
- **Everything plugs in through MCP.** Built-in servers for web search, documents (PDF, spreadsheets), reasoning and a
  code sandbox, plus plugins that bundle MCP servers, slash commands and permission rules.
- **Code sandbox in Colima.** Snippets in Python, JavaScript, Rust, Go and Bash run in an offline container; pinned
  dependencies are fetched in a separate step, and images are only downloaded after you approve.

## Quick start

Requirements: macOS 14 or later, Rust and Swift toolchains. Colima is optional (for the code sandbox and container
plugins).

```sh
# terminal agent
cd elisa && cargo build --release        # binary: elisa/target/release/elisa
elisa/target/release/elisa               # run it inside a project folder

# macOS app (builds the agent too when needed)
app/ElisaApp/run.sh                      # pick a folder and a model in the window
app/ElisaApp/run.sh --demo               # try it without a model
```

## Repository

| Path | What |
|---|---|
| [`elisa/`](elisa/) | The agent (Rust): REPL, tools, MCP host and built-in servers, plugins, eval. Full documentation in [`elisa/README.md`](elisa/README.md). |
| [`app/ElisaApp/`](app/ElisaApp/) | The native macOS app (SwiftUI). |
| [`app/ElisaCompanion/`](app/ElisaCompanion/) | The small floating Shade companion window. |
| [`designs/`](designs/) | App icon and logo artwork. |

## License

Proprietary. Copyright (c) 2026 Johnny. All rights reserved. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
