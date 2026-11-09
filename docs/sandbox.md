# The shell sandbox

Elisa runs shell commands in two places: the `bash` tool (after you approve a command) and the check commands it runs
after edits. Both go through one function (`tools::shell` in `elisa/src/tools.rs`), and the setting `shell_sandbox`
(in `src/sandbox.rs`) decides how they are fenced in. This is separate from the code sandbox (`sandbox = "colima"`,
the `run_code` tool), which runs short snippets in a throwaway container.

Set it in `~/.config/elisa/config.toml`:

```toml
shell_sandbox = "write"   # the default on macOS
```

A project's `.elisa/config.toml` can only make it stricter, in this order: `off` < `write` < `strict` < `colima`.

## The modes

| Mode | How commands run | Files | Network |
|---|---|---|---|
| `write` (default on macOS) | `zsh` under Seatbelt (`/usr/bin/sandbox-exec`) | write only in the project, the temp folders and build tool caches; secret stores (`~/.ssh`, `~/.aws`, keychains, browser profiles, …) unreadable | as usual |
| `strict` | the same | the same | localhost only, no DNS |
| `colima` | `bash` in a throwaway container on Colima's docker | only the project folder is visible | none |
| `off` | plain `zsh` | everything you can reach | as usual |

In every Seatbelt mode a command also cannot write Elisa's permission files (`.elisa/allow`, `.elisa/config.toml`,
`.elisa/checks.toml`), plant git hooks or change `.git/config`, connect to a container engine's socket (Docker,
Colima, Lima, OrbStack), or hand work to processes outside the sandbox (`open`, `osascript`, `launchctl`).

When the output of a command looks like the sandbox stopped it, Elisa adds a one-line note for the model, so it can ask
you to run the command yourself instead of trying workarounds.

## sandbox-exec is deprecated

`write` and `strict` use Seatbelt through `/usr/bin/sandbox-exec`. Apple has marked `sandbox-exec` as deprecated. It
still ships with macOS and works today, but a future macOS could remove it. It is also best effort: it limits what a
mistaken or careless command can do, but it is not a security boundary against a determined attacker.

If `sandbox-exec` is missing, Elisa does not fall back to running commands unsandboxed. Every command fails with a
message that says what to do. When Colima is running, the message suggests `shell_sandbox = "colima"` first; `off`
comes last.

## The colima option

```toml
shell_sandbox = "colima"
```

Each command runs with `bash -c` in a new container on Colima's docker daemon, the same daemon and helpers as the code
sandbox (`src/mcp_run.rs`). The container:

- sees only the project folder, mounted at its own path (so paths read the same inside and out), and starts there
- gets `.elisa/`, `.git/config` and `.git/hooks/` read-only (a missing `.elisa` or `.git/hooks` folder is created empty
  for the run and removed again afterwards, so a command cannot create it either)
- has no network (`--network none`), a read-only root filesystem and a 512 MB `/tmp` (also `$HOME`)
- runs as your own user id, with no capabilities, `no-new-privileges`, 1 GB memory, one CPU and at most 256 processes
- is killed by name when the command times out or you press Ctrl-C, and removed when it ends

What you need:

- Colima running (`brew install colima docker`, then `colima start`). Elisa never starts Colima itself.
- The `bash` image from Elisa's pinned list already on the machine. Elisa never downloads images on its own
  (`--pull never`); get it with `/sandbox pull bash` or `docker pull bash:5.3`.
- The project inside your home folder. By default Colima shares only your home folder with its VM; for a project
  elsewhere, add it to `mounts` in `~/.colima/default/colima.yaml`. (If you set `DOCKER_HOST` yourself, Elisa leaves
  the mounts to you.)

What to expect: the container has only the tools in its image (bash and the basic Unix tools), not your Mac's
toolchains, so `cargo`, `swift`, `npm` or `git` will not be found. That makes `colima` a good fit when you mostly want
Elisa to look at and change files, and a poor fit for running your project's build. For builds, `write` or `strict`
is the better choice while `sandbox-exec` is around.

## off

```toml
shell_sandbox = "off"
```

Commands run as plain `zsh` with everything your user account can reach: your home folder, secret stores, the network
and the Docker socket. Elisa still asks before each command (unless you allowed it), but nothing limits what an
approved command does. Use this only if you understand that, for example on a throwaway machine. On systems other than
macOS the shell sandbox does not exist and every setting acts like `off` (Elisa says so once at startup).
