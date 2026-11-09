# Permissions and safety

Part of the Elisa reference. Start at the [terminal README](../../elisa/README.md); all topics: [index](README.md).

## Approvals

Reading is free: `read`, `glob`, `grep`, `outline` and `read_symbol` run without asking inside the project.
`write`, `edit`, `edit_symbol`, `bash` and every MCP tool ask first; `--yes` / `/yolo` approve everything inside the
project (never outside writes).
"always" is saved per project in `.elisa/allow` (`/allow` lists, `/allow clear` resets). A "no" goes back to the
model as `denied by user: do not call this tool again for this request; answer without it or ask the user`.

## Outside the project folder

**Outside the project folder** (the session cwd) every tool asks first, `read` / `glob` / `grep` included: a file
tool whose path resolves outside (`..` and symlinks resolved first; the project's own `.elisa/` is inside), or a bash
command that names an absolute path outside, `~` / `$HOME`, an unknown `$VAR/…`, a `..` that escapes, or a `cd` out of
the folder (a best-effort parse that errs toward asking). The prompt shows the tool, read / write / run, and the exact
resolved paths, with **allow once**, **allow these paths for this session** (that path and everything under it, never
saved) or **deny**. The `.elisa/allow` "always" list never covers outside calls, and `--yes` / `/yolo` deny outside
writes and bash (reads still ask; with `-p` and no terminal they are denied). Config `outside` (default `ask`):
`read-ok` lets outside reads run without asking, `deny` refuses everything outside without asking, `allow` drops the
outside prompt (the normal rules apply, so `--yes` then allows outside writes). A project's `.elisa/config.toml` can
only make `outside` stricter than the user config (a looser value there is ignored with a startup note). `/allow` shows the mode and this session's grants.

## Shell sandbox

**Shell sandbox (macOS).** An approved `bash` command, and every check command (check before done), runs under
`/usr/bin/sandbox-exec` with a Seatbelt profile built for the project folder (`src/sandbox.rs`), so even a command you
said yes to cannot do damage outside the project. Config `shell_sandbox`:

- `write` (default on macOS): file **writes** only inside the project, the temp folders (`$TMPDIR`, your
  `/var/folders/…` folder, `/private/tmp`, `/private/var/tmp`), `/dev` and the build caches (`~/.cargo/registry`,
  `~/.cargo/git`, cargo's lock files in `~/.cargo`, `~/.npm`, `~/.cache`, `~/Library/Caches`, `~/.gradle/caches`,
  `~/Library/org.swift.swiftpm`). Inside the project, `.elisa/allow`, `.elisa/config.toml`, `.elisa/checks.toml`
  (and the `.elisa` folder itself) cannot be written, nor `.git/hooks`, `.git/config` (and the `.git` folder itself,
  once it exists), so a command cannot grant itself permissions, plant check commands or git hooks
  (`core.hooksPath` / `core.fsmonitor`). Reads are open except the secret stores, which are neither readable nor
  writable: `~/.ssh` (but `known_hosts` and `config`, so ssh with an agent still works), `~/.aws`, `~/.gnupg`,
  `~/.netrc`, `~/.git-credentials`, `~/.password-store`, `~/.kube`, `~/.azure`, `~/.pypirc`, `~/.docker/config.json`,
  `~/.cargo/credentials(.toml)`, `~/.config/elisa` (API keys in `providers.toml` / `config.toml`), `~/.config/gh`,
  `~/.config/gcloud`, `~/Library/Keychains`, `~/Library/Cookies`, `~/Library/Safari`, the Chrome / Firefox /
  Brave / Edge / Arc / Chromium profiles, and the Colima / Lima VM ssh keys (`~/.colima/_lima/_config`,
  `~/.lima/_config`). "`~`" is your real home folder from the password database, not `$HOME`: a command (or a
  parent process) that changes `$HOME` does not move the deny list; when `$HOME` points elsewhere, the same stores
  under it are denied as well. `open`, `osascript`, `launchctl` and Apple events are blocked (they would hand work to
  a process outside the sandbox). Container engine sockets cannot be connected to, in every mode: every unix socket
  under `~/.colima`, `~/.lima`, `~/.docker/run`, `~/.orbstack/run`, `~/.rd`, Docker Desktop's container folder, and
  `/var/run/docker.sock` (with its symlink target), since the Docker API can start a container that mounts any host
  folder; run `docker` yourself (the built-in `run_code` server talks to Docker from Elisa itself, so it is not
  affected). Network as usual, so cargo / npm / pip can fetch.
- `strict`: the same, and **no network** except localhost: outbound TCP/UDP only to `localhost` / `127.0.0.1` (a
  local model server such as Ollama keeps working), local servers may bind and accept, unix sockets only inside the
  project and temp folders. DNS lookups fail too. Fetching dependencies needs `write` (or the user's own terminal).
- `off`: commands run unsandboxed.

A project's `.elisa/config.toml` can only make it stricter (`off` there is ignored with one note); a value Elisa does
not know keeps the current mode with one note. On other systems the sandbox does not exist (setting it says so once).
When a command's output says `Operation not permitted` (or, under `strict`, a lookup / connect failed, or a network
tool such as `curl -s`, `wget`, `git clone/fetch/pull/push`, `npm/pip/cargo install|add`, `ssh`, `nc` exited non-zero
with little or no output), the tool result gets one line: `[Elisa: probably blocked by the shell sandbox: …]`, so the
model asks you instead of retrying. A check whose output carries that line still passes or fails by its exit code,
but its check line says the sandbox blocked some of its writes, so a ✓ is not taken at face value.
To opt out: `shell_sandbox = "off"` in `~/.config/elisa/config.toml`. Cost: about 10 ms per command
(sandbox-exec start-up). Limits: Apple marks `sandbox-exec` deprecated (it still ships and works on macOS 26); it is
best effort, not a container: the environment (including any API key variables) is inherited, a Docker daemon
reachable over TCP (`DOCKER_HOST=tcp://…`) under `write` is as powerful as you, `ssh` / `git` over ssh cannot read your keys from `~/.ssh` (an ssh agent
works), and `git push -u` / `git remote add` cannot change `.git/config` (run those yourself).

## Permission rules

`[permissions]` in `~/.config/elisa/config.toml` decides tool calls by pattern before anyone is asked:

```toml
[permissions]
allow = ["shell(git status*)", "shell(cargo test *)", "write(src/**)", "mcp:browser:navigate(https://github.com/*)"]
ask   = ["shell(git push *)", "mcp:fs"]
deny  = ["shell(rm *)", "read(**/*.env)", "mcp:browser:evaluate"]
```

- **Kinds.** `shell(…)` (or `bash`) matches the `bash` command; `read(…)` matches read, grep, glob, outline and
  read_symbol paths; `write(…)` (or `edit`) matches write, edit and edit_symbol; `mcp:SERVER`, `mcp:SERVER:TOOL` and
  `mcp:SERVER:TOOL(…)` match an MCP server's tools (the argument checks the call's `url`, `path`, `file`, `query` or
  `command`, else its first text value). A kind without `(…)` matches every call of that kind.
- **Globs.** In a path, `*` stays inside one folder and `**` crosses folders (`src/**`, `**/*.env`); paths are
  relative to the project (`..` worked out, a symlink checked by its real path too), `~/…` and `/…` are absolute. In a
  command or an MCP argument `*` matches anything, and a trailing ` *` also matches nothing (`shell(cargo test *)`
  covers `cargo test`).
- **Precedence.** deny > ask > allow > the normal approval. A deny refuses the call before any prompt (the model is
  told which rule). An ask always asks — even under `--yes`, `/yolo`, an "always" answer or a matching allow; with
  nobody to ask the call is denied. An allow runs the call without asking.
- **Safety.** An allow never covers a compound shell command (`;`, `&&`, `|`, redirects, `$(…)`, backticks, a second
  line): `shell(git status*)` does not allow `git status; rm -rf ~`. A deny or ask checks every part of one
  (`echo hi; rm -rf build` is denied by `shell(rm *)`). A relative path allow never covers a path outside the
  project. The other hard limits still win: unknown tools are refused before any approval, the shell sandbox still
  applies, an MCP server whose tool list changed asks on every call whatever the rules say, and a call outside the
  project still gets its own prompt.
- **Who may add what.** Your config: allow, ask and deny. A project `.elisa/config.toml`: deny and ask only (an allow
  there is ignored with a note). A plugin you install: all three, listed on its setup screen first. A project plugin:
  deny and ask only. A malformed pattern is skipped with a startup note.
- **"Always allow this pattern".** When nothing forces the prompt, the approval box offers a fourth choice with a
  narrow pattern Elisa picks for that call — `shell(cargo test *)` (program and subcommand), `write(src/**)` (the
  file's folder), `mcp:browser:navigate(https://github.com/*)` (the URL's host) — shown exactly before you choose it.
  It is saved to `[permissions] allow` in `~/.config/elisa/config.toml` (your other lines are kept) and applies at
  once. No pattern is offered for compound commands, commands that run arbitrary code or delete (`rm`, `sudo`, `sh`,
  `curl`, `python`, …), paths outside the project or in `.elisa` / `.git`, or a server whose tool list changed. Saved
  rules and every denial by rule go into the audit log.

Code: `src/perms.rs`.
