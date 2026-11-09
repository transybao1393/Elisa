<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-dark.svg">
    <img alt="Elisa" src="designs/Logo/SVG%20(web)/elisa-lockup-horizontal-on-light.svg" width="360">
  </picture>
</p>

<p align="center"><b>A personal assistant that lives on your Mac and runs on your models.</b></p>

<p align="center"><b>English</b> · <a href="README.vi.md">Tiếng Việt</a> · <a href="README.ja.md">日本語</a></p>

<p align="center">
  <a href="https://github.com/transybao1393/Elisa/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/transybao1393/Elisa?style=flat&logo=github"></a>
  <a href="https://github.com/transybao1393/Elisa/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/transybao1393/Elisa?include_prereleases&sort=semver"></a>
  <a href="https://github.com/transybao1393/Elisa/commits/main"><img alt="Last commit" src="https://img.shields.io/github/last-commit/transybao1393/Elisa"></a>
  <a href="https://github.com/transybao1393/Elisa/issues"><img alt="Open issues" src="https://img.shields.io/github/issues/transybao1393/Elisa"></a>
  <img alt="Platform: macOS 14+" src="https://img.shields.io/badge/platform-macOS%2014%2B-black?logo=apple">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-core-orange?logo=rust">
  <img alt="SwiftUI" src="https://img.shields.io/badge/SwiftUI-app-F05138?logo=swift&logoColor=white">
  <img alt="MCP" src="https://img.shields.io/badge/MCP-plugins-6E56CF">
  <img alt="Local first" src="https://img.shields.io/badge/local--first-private-2EA44F">
  <a href="LICENSE"><img alt="License: proprietary" src="https://img.shields.io/badge/license-proprietary-lightgrey"></a>
</p>


Elisa is a small chat assistant for your Mac. It runs on a model you choose, in a native macOS app or the terminal,
with Shade, a small companion who shows what Elisa is doing. With a local model nothing leaves your Mac.

## Features

- **Chat** in any language: questions, writing, plans, jokes. Plain questions get plain answers, with no tools.
- **Fits the model you pick.** At start Elisa detects what the model can do (vision, tools, thinking, context window)
  and adapts. Models without native tool calling get tools through the prompt. Small windows are handled for you.
- **Switch models mid-chat.** The conversation carries over, and the new model answers as itself.
- **Web.** Searches and reads pages when a question needs fresh facts. Results are cached for an hour.
- **Files and documents.** Reads and saves files in the project folder. Understands attached PDFs, spreadsheets and
  Word files.
- **Pictures** on vision models, with follow-up questions. Models without vision say so plainly instead of guessing.
- **Memory.** Remembers what you tell it. `/pin` keeps a fact, `/notes` shows what's kept, `/forget` clears it. Long
  chats are compacted automatically (or with `/compact`). Continue your last conversation when you choose to.
- **Time and dates** answered from your clock.
- **Step-by-step reasoning** tools (think, steps, calc), used only when a question needs them.
- **Coding help**, as one feature among the rest: read and edit a project with your approval, run commands, and try
  snippets in an offline sandbox (Python, JavaScript, Rust, Go, Bash).
- **Optional intent router.** A hosted model (paid) that decides which tools each message needs. Without it, Elisa
  routes on her own.

## Private and in your control

- **Your model, your choice.** Ollama, LM Studio, llama.cpp, MLX, or any OpenAI- or Anthropic-compatible API. Elisa
  never picks a model for you. The Model Scorecard shows measured results to help you choose.
- **Local first.** With a local model, your conversations stay on your Mac.
- **Nothing happens behind your back.** Reading is free. Writing files, running commands and anything outside the
  current folder ask first. Allow / ask / deny rules let you decide once.
- **Safe by default.** Secrets you paste are hidden from online models and from anything saved to disk, web and
  document content is treated as untrusted, and every action is recorded in a tamper-evident audit log.
- **Each conversation starts fresh.** A new chat follows what you ask in it. Continue an earlier one only when you
  choose to.

## Extend it

Everything Elisa can use plugs in through MCP. Built-in servers cover web search, documents, reasoning and the code
sandbox. Plugins bundle more MCP servers, slash commands and permission rules from a local folder, and run in an
offline container unless they need your Mac.

## Quick start

Requirements: macOS 14 or later, Rust and Swift toolchains. Colima is optional, for the code sandbox and container
plugins.

```sh
# macOS app (builds everything it needs)
app/ElisaApp/run.sh                      # choose a model, then start chatting

# terminal app
cd elisa && cargo build --release        # binary: elisa/target/release/elisa
elisa/target/release/elisa               # chat in the terminal
```

## Repository

| Path | What |
|---|---|
| [`elisa/`](elisa/) | Elisa itself (Rust): chat, tools, MCP host and built-in servers, plugins, eval. Full documentation in [`elisa/README.md`](elisa/README.md). |
| [`app/ElisaApp/`](app/ElisaApp/) | The native macOS app (SwiftUI). |
| [`app/ElisaCompanion/`](app/ElisaCompanion/) | The small floating Shade companion window. |
| [`designs/`](designs/) | App icon and logo artwork. |

## License

Proprietary. Copyright (c) 2026 Johnny. All rights reserved. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
