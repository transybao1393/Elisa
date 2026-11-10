//! Elisa's own code sandbox: `elisa --mcp-serve run [--timeout SECS] [--fetch-timeout SECS]`, a stdio MCP server
//! with one tool, `run_code`, that runs a short snippet (python, javascript, bash, rust, go) in a throwaway container
//! and returns stdout, stderr, the exit code and the time. Off unless config `sandbox = "colima"`; every call asks
//! first (approval `mcp:run`).
//!
//! The container (`docker run`, flags in `run_args`): no network, read-only root, a tmpfs at /tmp (64 MB; 256 MB for
//! go, and for rust with deps), 256 MB memory (512 MB for go; no swap), one CPU, at most 64 processes, user nobody,
//! no capabilities, no new privileges, no host mounts. The code goes in through the docker client's environment
//! (`-e ELISA_CODE`, not its arguments, which `ps` shows) and is written to /tmp inside; the snippet's own stdin is
//! the container's stdin. Elisa enforces the wall-clock limit (`docker kill`), and a `timeout` inside the container is
//! the backstop if this server itself dies.
//!
//! Dependencies (`deps`, two phases). A call with pinned `deps` (pip `name==1.2.3`, npm `name@1.2.3`, cargo
//! `name@1.2.3` / `name = "=1.2.3"`, go `module@v1.2.3`; `parse_deps` refuses ranges, URLs, paths, git refs, flags and
//! shell characters) first runs a **fetch** container (`fetch_args`): the same image and limits, *with* network, root
//! without capabilities, no stdin, no code and no project files; its only mount is Elisa's cache, one named docker
//! volume per language (`elisa-deps-python`, …; it lives inside the Colima VM, never on the Mac). It installs the set
//! into `/deps/KEY` (KEY: a hash of the image and the sorted deps), without running package code (pip wheels only,
//! npm `--ignore-scripts`, `cargo fetch`, `go get` through the module proxy), and a set already there is reused. Then
//! the **run** container runs the snippet exactly as without deps (`--network none`), with that volume mounted
//! read-only at /deps and the language pointed at `/deps/KEY` (PYTHONPATH, NODE_PATH, a Cargo project built
//! `--offline --locked`, GOMODCACHE with GOPROXY=off). The fetch has its own limit (config `sandbox_fetch_timeout`).
//!
//! The image (`images.rs`, the one pinned list): the `version` the model passed (checked against the list), else what
//! the project under `--root DIR` declares (`.python-version`, `package.json` engines, `go.mod`, …), else the stable
//! line of the language. The result's status line names the image; a project version that is not on the list runs on
//! stable with a one-line note.
//!
//! The daemon: `DOCKER_HOST` if set, else Colima's `~/.colima/default/docker.sock`, else `/var/run/docker.sock`,
//! handed to the docker CLI as `DOCKER_HOST` (the user's docker context is never read or changed). This server never
//! starts Colima and never pulls an image (`--pull never`). A missing image comes back as an error result whose
//! `_meta` names it (`images::META_KEY`): the session (not the model) then asks the user whether to download it.
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// default wall-clock limit per run, seconds (config `sandbox_timeout`)
pub const DEFAULT_TIMEOUT_S: u64 = 10;
/// the most `sandbox_timeout` may be
pub const MAX_TIMEOUT_S: u64 = 30;
/// default limit for fetching `deps`, seconds (config `sandbox_fetch_timeout`)
pub const DEFAULT_FETCH_TIMEOUT_S: u64 = 60;
/// the most `sandbox_fetch_timeout` may be: a fetch plus the longest run stays under the host's 120 s per tool call
pub const MAX_FETCH_TIMEOUT_S: u64 = 80;
/// the most `deps` one call may list
pub const MAX_DEPS: usize = 10;
const MAX_DEP_LEN: usize = 200;
/// the dependency cache: one docker volume per language, `PREFIX + language` (tests set their own prefix here)
pub const VOLUME_PREFIX_ENV: &str = "ELISA_SANDBOX_VOLUME_PREFIX";
pub const VOLUME_PREFIX: &str = "elisa-deps-";
/// largest snippet / stdin accepted, bytes (the code travels in one environment variable)
const MAX_CODE: usize = 64 * 1024;
const MAX_STDIN: usize = 64 * 1024;
/// output kept per stream, bytes; a run that writes more than OUTPUT_STOP in all is stopped early
const KEEP_BYTES: usize = 32 * 1024;
const OUTPUT_STOP: usize = 1 << 20;
/// characters of stdout / stderr given back to the model
const STDOUT_CHARS: usize = 4000;
const STDERR_CHARS: usize = 2000;

/// (language, file name, command run in /tmp); the images are in `images::IMAGES`
pub const LANGS: [(&str, &str, &str); 5] = [
    ("python", "main.py", "python3 main.py"),
    ("javascript", "main.js", "node main.js"),
    ("bash", "main.sh", "bash main.sh"),
    ("rust", "main.rs", "rustc --edition 2021 -o main main.rs && ./main"),
    ("go", "main.go", "go run main.go"),
];

/// the tools this server lists (MCP `tools/list`); kept short for small context windows
pub fn tools() -> Value {
    json!([
        {"name": "run_code", "description": "Run a short snippet in an offline sandbox (no network, no project files); returns stdout, stderr, exit code, time. deps: exact pins fetched online first (six==1.16.0, left-pad@1.3.0, itoa@1.0.11, golang.org/x/text@v0.14.0). version: optional (default: the project's, else the installed one).",
         "inputSchema": {"type": "object", "properties": {
             "language": {"type": "string", "enum": ["python", "javascript", "bash", "rust", "go"]},
             "code": {"type": "string"},
             "stdin": {"type": "string"},
             "deps": {"type": "array", "items": {"type": "string"}},
             "version": {"type": "string"}}, "required": ["language", "code"]}},
    ])
}

/// the language a model asked for, with the usual aliases
fn lang(l: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    let l = crate::images::lang_name(l)?;
    LANGS.iter().find(|x| x.0 == l)
}

const UNKNOWN_LANG: &str = "use python, javascript, bash, rust or go";

// ---- deps ----

/// how one language writes a pin (in messages)
fn pin_example(lname: &str) -> &'static str {
    match lname {
        "python" => "name==1.2.3 (e.g. six==1.16.0)",
        "javascript" => "name@1.2.3 (e.g. left-pad@1.3.0)",
        "rust" => "name@1.2.3 or name = \"=1.2.3\" (e.g. itoa@1.0.11; features: serde[derive]@1.0.210)",
        _ => "module@v1.2.3 (e.g. golang.org/x/text@v0.14.0)",
    }
}

const SEMVER: &str = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?";

fn re(cell: &'static OnceLock<Regex>, src: impl FnOnce() -> String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&src()).expect("dep regex"))
}

/// One `deps` entry checked and written the one way the fetch and the cache key use: python `name[extras]==1.2.3`
/// (name lower-cased), javascript `[@scope/]name@1.2.3`, rust `name[features]@1.2.3`, go `module@v1.2.3`.
pub fn check_dep(lname: &str, raw: &str) -> Result<String, String> {
    static PY: OnceLock<Regex> = OnceLock::new();
    static JS: OnceLock<Regex> = OnceLock::new();
    static RS: OnceLock<Regex> = OnceLock::new();
    static RS_TOML: OnceLock<Regex> = OnceLock::new();
    static GO: OnceLock<Regex> = OnceLock::new();
    let d = raw.trim();
    let bad = |why: &str| Err(format!("deps: {d:?} {why}; write {}", pin_example(lname)));
    if d.len() > MAX_DEP_LEN { return Err(format!("deps: an entry is longer than {MAX_DEP_LEN} characters")); }
    if d.starts_with('-') { return bad("looks like a flag (flags are not allowed)"); }
    let lower = d.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("git+") || lower.starts_with("git@") || lower.starts_with("file:")
        || lower.starts_with("npm:") || lower.contains("github:") || lower.ends_with(".git") || lower.contains(".git@")
        || d.starts_with(['/', '.', '~']) || d.contains('\\') || d.contains("..") {
        return bad("is a URL, path or git ref (only registry packages with an exact version)");
    }
    let range_chars: &[char] = if lname == "go" { &['>', '<', '^', '*', '|'] } else { &['>', '<', '~', '^', '*', '|'] };
    if d.contains(range_chars) || d.contains("!=") || d.contains("~=") || lower.ends_with("@latest") || lower.ends_with("@*") {
        return bad("is not pinned to one exact version");
    }
    let quote_ok = lname == "rust";
    if let Some(c) = d.chars().find(|&c| !(c.is_ascii_alphanumeric() || "._-@=/[],+~".contains(c) || (quote_ok && (c == '"' || c == ' ')))) {
        return bad(&format!("contains {c:?} (no spaces, quotes or shell characters)"));
    }
    let caps = match lname {
        "python" => re(&PY, || r"^([A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?)(\[[A-Za-z0-9._-]+(?:,[A-Za-z0-9._-]+)*\])?==([0-9][0-9A-Za-z.+]*[0-9A-Za-z])$".into())
            .captures(d).map(|c| format!("{}{}=={}", c[1].to_ascii_lowercase(), c.get(2).map_or("", |m| m.as_str()), &c[3])),
        "javascript" => re(&JS, || format!(r"^((?:@[a-z0-9][a-z0-9._-]*/)?[a-z0-9][a-z0-9._-]*)@({SEMVER})$"))
            .captures(d).map(|c| format!("{}@{}", &c[1], &c[2])),
        "rust" => re(&RS, || format!(r"^([A-Za-z0-9][A-Za-z0-9_-]*)(\[[A-Za-z0-9_-]+(?:,[A-Za-z0-9_-]+)*\])?@=?({SEMVER})$"))
            .captures(d).map(|c| format!("{}{}@{}", &c[1], c.get(2).map_or("", |m| m.as_str()), &c[3]))
            .or_else(|| re(&RS_TOML, || format!(r#"^([A-Za-z0-9][A-Za-z0-9_-]*) *= *"=({SEMVER})"$"#)).captures(d).map(|c| format!("{}@{}", &c[1], &c[2]))),
        "go" => re(&GO, || format!(r"^([a-z0-9][a-z0-9-]*(?:\.[a-z0-9-]+)+(?:/[A-Za-z0-9._~-]+)*)@(v{SEMVER})$"))
            .captures(d).map(|c| format!("{}@{}", &c[1], &c[2])),
        _ => return Err(format!("deps: {lname} takes no deps (deps are for python, javascript, rust and go)")),
    };
    let sep = match lname { "python" => "==", "rust" if d.contains('=') => "=", _ => "@" };
    match caps {
        Some(n) => Ok(n),
        None if !d.contains(sep) || d.ends_with(sep) => bad("is not pinned to an exact version"),
        None if lname == "rust" && d.contains('=') && !d.contains("\"=") => bad("is not pinned to an exact version (\"=1.2.3\")"),
        None if lname == "go" => bad("is not module@vX.Y.Z (a release or pseudo-version; no branches, commits or @latest)"),
        None => bad("is not a pinned package"),
    }
}

/// The `deps` input (an array of strings; a string is split at commas and new lines): checked, de-duplicated and
/// sorted. Empty: no fetch.
pub fn parse_deps(lname: &str, v: &Value) -> Result<Vec<String>, String> {
    let raw: Vec<String> = match v {
        Value::Null => return Ok(Vec::new()),
        Value::String(s) => s.split([',', '\n']).map(str::to_string).collect(),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or("deps: every entry must be a string"))
            .collect::<Result<_, _>>()?,
        _ => return Err("deps: give a list of pinned packages, e.g. [\"six==1.16.0\"]".into()),
    };
    let raw: Vec<&str> = raw.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if raw.is_empty() { return Ok(Vec::new()); }
    if raw.len() > MAX_DEPS { return Err(format!("deps: {} entries; at most {MAX_DEPS} per call", raw.len())); }
    let mut out = raw.iter().map(|d| check_dep(lname, d)).collect::<Result<Vec<_>, _>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// the deps as a call's input names them (for the approval line and the untrusted fence), unchecked
pub fn deps_of(input: &Value) -> Vec<String> {
    match &input["deps"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        Value::String(s) => s.split([',', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// the approval line's tail for a call with deps: ` · fetches online: six==1.16.0, …` (empty without deps)
pub fn deps_summary(input: &Value) -> String {
    let d = deps_of(input);
    if d.is_empty() { return String::new(); }
    let all = d.join(", ");
    let mut t: String = all.chars().take(120).collect();
    if all.chars().count() > 120 { t.push('…'); }
    format!(" · fetches online: {t}")
}

/// the cache folder of one set inside the volume: 16 hex digits of sha256(image, deps)
pub fn cache_key(image: &str, deps: &[String]) -> String {
    let mut h = Sha256::new();
    h.update(image.as_bytes());
    for d in deps { h.update(b"\n"); h.update(d.as_bytes()); }
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// the cache volume of a language: `elisa-deps-python`, or the prefix in ELISA_SANDBOX_VOLUME_PREFIX (tests)
pub fn volume(lname: &str, prefix: Option<&str>) -> String {
    static OK: OnceLock<Regex> = OnceLock::new();
    let p = prefix.filter(|p| re(&OK, || r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,40}$".into()).is_match(p)).unwrap_or(VOLUME_PREFIX);
    format!("{p}{lname}")
}

/// rust deps as a Cargo manifest: exact versions, features from `name[a,b]@1.2.3`, the image's rust as rust-version
/// (so the resolver prefers releases this compiler can build)
fn cargo_manifest(image: &str, deps: &[String]) -> String {
    let rv = image.split(':').nth(1).and_then(|t| t.split('-').next()).unwrap_or(crate::images::stable("rust").version);
    let mut m = format!("[package]\nname = \"snippet\"\nversion = \"0.1.0\"\nedition = \"2021\"\nrust-version = \"{rv}\"\n\n[dependencies]\n");
    for d in deps {
        let (name, ver) = d.rsplit_once('@').unwrap_or((d, ""));
        match name.split_once('[') {
            Some((n, f)) => {
                let feats: Vec<String> = f.trim_end_matches(']').split(',').map(|x| format!("\"{x}\"")).collect();
                m.push_str(&format!("{n} = {{ version = \"={ver}\", features = [{}] }}\n", feats.join(", ")));
            }
            None => m.push_str(&format!("{name} = \"={ver}\"\n")),
        }
    }
    m
}

/// (tmpfs MB at /tmp, memory MB): go builds its standard library into a cache in /tmp; a rust build with crates
/// needs room for its target folder (33 MB for serde + serde_json with debug info off)
fn limits(lname: &str, deps: bool) -> (u32, u32) {
    match (lname, deps) { ("go", _) => (256, 512), ("rust", true) => (256, 256), _ => (64, 256) }
}

/// the fetch container's install step per language, into "$S" (the staging folder); `$ELISA_DEPS` is the checked
/// list (word-split with globbing off), `$ELISA_MANIFEST` the Cargo manifest. No step runs package code.
fn fetch_step(lname: &str) -> &'static str {
    match lname {
        "python" => "pip install --no-cache-dir --only-binary=:all: --root-user-action=ignore --disable-pip-version-check --no-input --no-warn-script-location --target \"$S\" $ELISA_DEPS",
        "javascript" => "cd \"$S\" && npm install --ignore-scripts --no-audit --no-fund --no-update-notifier --save-exact --loglevel=error --cache \"$S/.npm-cache\" --prefix \"$S\" $ELISA_DEPS && rm -rf \"$S/.npm-cache\"",
        "rust" => "mkdir -p /tmp/p/src && cd /tmp/p && printf '%s' \"$ELISA_MANIFEST\" > Cargo.toml && echo 'fn main() {}' > src/main.rs && export CARGO_HOME=\"$S/home\" CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback && cargo generate-lockfile -q && cargo fetch -q && cp Cargo.toml Cargo.lock \"$S/\"",
        _ => "mkdir -p /tmp/m && cd /tmp/m && export GOMODCACHE=\"$S/mod\" GOFLAGS=-modcacherw GOPROXY=https://proxy.golang.org && go mod init sandbox 2>/dev/null && go get $ELISA_DEPS && cp go.mod go.sum \"$S/\"",
    }
}

/// The fetch container: the language's image with network (the default bridge) and the same limits as a run, root
/// without capabilities (the volume is root's), no stdin, no code, no host files; its one mount is the cache volume
/// at /deps. A set already in the cache is reused (prints ELISA-CACHED); a new one is installed into a staging folder
/// and renamed into place when complete (prints ELISA-FETCHED); a failed install removes its staging folder, and one
/// left by a killed fetch is removed by the next fetch after an hour.
pub fn fetch_args(name: &str, lname: &str, image: &str, vol: &str, key: &str, timeout: Duration) -> Vec<String> {
    let (tmp, mem) = limits(lname, true);
    let script = format!("set -ef; D=/deps/{key}; if [ -d \"$D\" ]; then echo ELISA-CACHED; exit 0; fi; \
        find /deps -maxdepth 1 -name '*.tmp-*' -mmin +60 -exec rm -rf {{}} \\; 2>/dev/null || true; \
        export S=\"$D.tmp-{name}\"; mkdir -p \"$S\"; if ! timeout -s KILL {backstop} sh -c 'set -ef; {step}'; then rm -rf \"$S\"; exit 1; fi; \
        touch \"$S/.elisa-ok\"; if [ ! -d \"$D\" ]; then mv \"$S\" \"$D\"; fi; rm -rf \"$S\"; echo ELISA-FETCHED",
        backstop = timeout.as_secs() + 5, step = fetch_step(lname).replace('\'', "'\\''"));
    let mut a: Vec<String> = [
        "run", "--rm", "--pull", "never", "--name", name, "--init", "--network", "bridge", "--read-only",
        "--tmpfs", &format!("/tmp:rw,noexec,nosuid,nodev,size={tmp}m"), "--workdir", "/tmp", "--memory", &format!("{mem}m"),
        "--memory-swap", &format!("{mem}m"), "--cpus", "1", "--pids-limit", "64", "--user", "0:0", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--log-driver", "none",
        "--mount", &format!("type=volume,src={vol},dst=/deps"),
        "-e", "ELISA_DEPS", "-e", "HOME=/tmp", "-e", "PIP_NO_INPUT=1",
    ].iter().map(|s| s.to_string()).collect();
    if lname == "rust" { a.extend(["-e".into(), "ELISA_MANIFEST".into()]); }
    if lname == "go" { a.extend(["GOCACHE=/tmp/.cache/go", "GOPATH=/tmp/.go", "GOTOOLCHAIN=local", "GOTELEMETRY=off", "CGO_ENABLED=0"].iter().flat_map(|e| ["-e".to_string(), e.to_string()])); }
    a.extend([image.to_string(), "sh".into(), "-c".into(), script]);
    a
}

/// how to reach the docker daemon
#[derive(Clone, Debug, PartialEq)]
pub struct Daemon {
    /// the `DOCKER_HOST` value handed to the docker CLI
    pub host: String,
    /// the user set `DOCKER_HOST` themselves (a pull hint then needs no `-H`)
    pub from_env: bool,
}

pub const NOT_RUNNING: &str = "Colima isn't running — start it with `colima start` (Elisa never starts it itself)";

/// `DOCKER_HOST` if set, else `~/.colima/default/docker.sock`, else `/var/run/docker.sock`; Err when no socket is there
pub fn find_daemon(docker_host: Option<&str>, home: Option<&Path>) -> Result<Daemon, String> {
    if let Some(h) = docker_host.map(str::trim).filter(|h| !h.is_empty()) {
        if let Some(p) = h.strip_prefix("unix://") {
            if !Path::new(p).exists() { return Err(format!("{NOT_RUNNING} (DOCKER_HOST={h}: no socket there)")); }
        }
        return Ok(Daemon { host: h.to_string(), from_env: true });
    }
    let colima = home.map(|h| h.join(".colima/default/docker.sock"));
    for p in colima.into_iter().chain([PathBuf::from("/var/run/docker.sock")]) {
        if p.exists() { return Ok(Daemon { host: format!("unix://{}", p.display()), from_env: false }); }
    }
    Err(NOT_RUNNING.into())
}

/// the docker CLI: on PATH, else where Homebrew puts it (shared with the shell sandbox's "colima" mode)
pub fn docker_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join("docker"))
        .chain(["/opt/homebrew/bin/docker", "/usr/local/bin/docker"].map(PathBuf::from))
        .find(|p| p.is_file())
}

/// a name no other container has: `elisa-run-PID-N-NANOS` (or `elisa-fetch-…`, `elisa-shell-…`)
pub fn unique_name(kind: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("elisa-{kind}-{}-{}-{nanos}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// The `docker run` arguments: the isolation flags, the image, and a shell line that writes the code (from
/// `$ELISA_CODE`) to /tmp, drops the variable, and runs it under a backstop `timeout`. With `deps` (the cache
/// volume and the set's key) the volume is mounted read-only at /deps and the language is pointed at /deps/KEY;
/// everything else, `--network none` first of all, is the same.
pub fn run_args(name: &str, lname: &str, image: &str, timeout: Duration, deps: Option<(&str, &str)>) -> Vec<String> {
    let (_, file, plain) = *lang(lname).unwrap_or(&LANGS[0]);
    let (tmp, mem) = limits(lname, deps.is_some());
    // rust and go run the binary they compiled into /tmp; everything else only reads its file there
    let exec = if matches!(lname, "rust" | "go") { "exec" } else { "noexec" };
    let mut env: Vec<String> = vec!["HOME=/tmp".into(), "PYTHONUNBUFFERED=1".into(), "PYTHONDONTWRITEBYTECODE=1".into()];
    if lname == "go" {
        env.extend(["GOCACHE=/tmp/.cache/go", "GOPATH=/tmp/.go", "GOTOOLCHAIN=local", "GOPROXY=off", "GOTELEMETRY=off", "CGO_ENABLED=0"].map(String::from));
    }
    let mut cmd = plain.to_string();
    let mut mount = None;
    if let Some((vol, key)) = deps {
        let d = format!("/deps/{key}");
        mount = Some(format!("type=volume,src={vol},dst=/deps,readonly"));
        match lname {
            "python" => env.push(format!("PYTHONPATH={d}")),
            "javascript" => env.push(format!("NODE_PATH={d}/node_modules")),
            "rust" => {
                // a Cargo project in /tmp: the fetched manifest and lock, the registry from the cache (read-only),
                // CARGO_HOME itself writable for cargo's lock file; the build stays offline and locked
                env.extend(["CARGO_HOME=/tmp/ch", "CARGO_TARGET_DIR=/tmp/p/target", "CARGO_INCREMENTAL=0", "CARGO_PROFILE_DEV_DEBUG=0", "CARGO_TERM_COLOR=never"].map(String::from));
                cmd = format!("mkdir -p p/src ch && mv main.rs p/src/ && cp {d}/Cargo.toml {d}/Cargo.lock p/ && ln -s {d}/home/registry ch/registry && cd p && cargo run -q --offline --locked");
            }
            "go" => {
                env.extend([format!("GOMODCACHE={d}/mod"), "GOFLAGS=-mod=mod".into(), "GOSUMDB=off".into()]);
                cmd = format!("mkdir m && mv main.go m/ && cp {d}/go.mod {d}/go.sum m/ && cd m && go run .");
            }
            _ => {}
        }
    }
    let backstop = timeout.as_secs() + 5;
    let script = format!("printf '%s' \"$ELISA_CODE\" > {file} && unset ELISA_CODE && timeout -s KILL {backstop} sh -c '{cmd}'");
    let mut a: Vec<String> = [
        "run", "--rm", "-i", "--pull", "never", "--name", name, "--init", "--network", "none", "--read-only",
        "--tmpfs", &format!("/tmp:rw,{exec},nosuid,nodev,size={tmp}m"), "--workdir", "/tmp", "--memory", &format!("{mem}m"),
        "--memory-swap", &format!("{mem}m"), "--cpus", "1", "--pids-limit", "64", "--user", "65534:65534", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--log-driver", "none", "-e", "ELISA_CODE",
    ].iter().map(|s| s.to_string()).collect();
    if let Some(m) = mount { a.extend(["--mount".into(), m]); }
    for e in env { a.extend(["-e".into(), e]); }
    a.extend([image.to_string(), "sh".into(), "-c".into(), script]);
    a
}

/// what a run gave back
#[derive(Debug, Default)]
pub struct Outcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// bytes written in all (stdout, stderr)
    pub total: (usize, usize),
    pub code: Option<i32>,
    pub secs: f64,
    /// why the run was stopped early: "timed out after 10 s", "cancelled", "output over 1 MB"
    pub stopped: Option<String>,
    /// with deps: "deps cached" or "deps fetched in 3.1 s" (on the status line)
    pub deps: Option<String>,
    /// the image it ran in (on the status line)
    pub image: String,
    /// the project's version is not on the list: what ran instead (a line after the status line)
    pub note: Option<String>,
}

/// Why a call could not run at all: words for the model, and (an image that is not on this machine) the image, for
/// the session to offer a download.
#[derive(Debug, PartialEq)]
pub struct Fail {
    pub text: String,
    /// the listed image that is not on this machine (offered for download by the session)
    pub missing: Option<String>,
    /// with `missing`: the version of the newest present line that still fits, which the session runs on when the user
    /// says no to the download
    pub fallback: Option<String>,
}

impl From<String> for Fail { fn from(text: String) -> Fail { Fail { text, missing: None, fallback: None } } }
impl From<&str> for Fail { fn from(text: &str) -> Fail { Fail { text: text.into(), missing: None, fallback: None } } }

/// Reads a stream to its end: (kept bytes, bytes in all). Keeps the first KEEP_BYTES; with `tail` (stderr, where a
/// traceback ends with the line that matters) a longer stream keeps its first and last KEEP_BYTES / 2 instead.
fn drain(mut r: impl Read + Send + 'static, seen: std::sync::Arc<AtomicU64>, tail: bool) -> std::thread::JoinHandle<(Vec<u8>, usize)> {
    std::thread::spawn(move || {
        let (mut keep, mut last, mut n, mut buf) = (Vec::new(), Vec::new(), 0usize, [0u8; 8192]);
        while let Ok(k) = r.read(&mut buf) {
            if k == 0 { break; }
            if keep.len() < KEEP_BYTES { keep.extend_from_slice(&buf[..k.min(KEEP_BYTES - keep.len())]); }
            if tail {
                last.extend_from_slice(&buf[..k]);
                if last.len() > 2 * KEEP_BYTES { last.drain(..last.len() - KEEP_BYTES); }
            }
            n += k;
            seen.fetch_add(k as u64, Ordering::Relaxed);
        }
        (head_tail(keep, &last, n, tail), n)
    })
}

/// `drain`'s kept bytes: the head alone, or (a stream over KEEP_BYTES, with `tail`) its first and last KEEP_BYTES / 2
fn head_tail(mut head: Vec<u8>, last: &[u8], n: usize, tail: bool) -> Vec<u8> {
    if !tail || n <= KEEP_BYTES { return head; }
    head.truncate(KEEP_BYTES / 2);
    head.extend_from_slice(&last[last.len().saturating_sub(KEEP_BYTES / 2)..]);
    head
}

/// `docker kill NAME`, given a few seconds
pub fn kill(docker: &Path, host: &str, name: &str) {
    let Ok(mut c) = Command::new(docker).args(["kill", name]).env("DOCKER_HOST", host).env_remove("DOCKER_CONTEXT")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return };
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(_)) = c.try_wait() { return; }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = c.kill();
    let _ = c.wait();
}

/// One `docker run` with the wall-clock limit, the output cap and `cancel`; the docker client's own failures (no
/// image, no daemon) come back as Err, in words for the model.
#[allow(clippy::too_many_arguments)]
fn exec(docker: &Path, daemon: &Daemon, name: &str, args: Vec<String>, env: &[(&str, &str)], stdin: Option<&str>,
    timeout: Duration, cancel: &mut dyn FnMut() -> bool, lname: &str, image: &str) -> Result<Outcome, Fail> {
    let t0 = Instant::now();
    let mut c = Command::new(docker);
    c.args(args).env("DOCKER_HOST", &daemon.host).env_remove("DOCKER_CONTEXT")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env { c.env(k, v); }
    let mut child = c.spawn().map_err(|e| format!("cannot run {}: {e}", docker.display()))?;
    let seen = std::sync::Arc::new(AtomicU64::new(0));
    let out = drain(child.stdout.take().ok_or("no stdout")?, seen.clone(), false);
    let err = drain(child.stderr.take().ok_or("no stderr")?, seen.clone(), true);
    if let (Some(mut w), Some(s)) = (child.stdin.take(), stdin) {
        let input = s.as_bytes().to_vec();
        std::thread::spawn(move || { let _ = w.write_all(&input); });
    }
    let mut stopped = None;
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() { break Some(s); }
        let why = if t0.elapsed() >= timeout { Some(format!("timed out after {} s", timeout.as_secs())) }
            else if seen.load(Ordering::Relaxed) as usize > OUTPUT_STOP { Some(format!("stopped: more than {} MB of output", OUTPUT_STOP >> 20)) }
            else if cancel() { Some("cancelled".to_string()) } else { None };
        if let Some(w) = why {
            stopped = Some(w);
            kill(docker, &daemon.host, name);
            // the docker client normally ends with its container; if not, end it
            let t1 = Instant::now();
            break loop {
                if let Ok(Some(s)) = child.try_wait() { break Some(s); }
                if t1.elapsed() > Duration::from_secs(3) { let _ = child.kill(); break child.wait().ok(); }
                std::thread::sleep(Duration::from_millis(20));
            };
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let secs = t0.elapsed().as_secs_f64();
    let (stdout, n_out) = out.join().unwrap_or_default();
    let (stderr, n_err) = err.join().unwrap_or_default();
    let code = status.and_then(|s| s.code());
    // the docker client's own failures (exit 125, or 1 when it cannot reach the daemon) with nothing from the snippet
    if stdout.is_empty() && (code == Some(125) || (code == Some(1) && stopped.is_none())) {
        let e = String::from_utf8_lossy(&stderr);
        if code == Some(125) && (e.contains("No such image") || e.contains("Unable to find image") || e.contains("pull access denied")) {
            let _ = daemon;
            return Err(Fail { text: format!("the {lname} image {image} is not on this machine, so nothing was run (the user can download it with /sandbox pull {image})"),
                missing: Some(image.to_string()), fallback: None });
        }
        if e.contains("Cannot connect to the Docker daemon") || e.contains("failed to connect to the docker API") {
            return Err(NOT_RUNNING.into());
        }
        if code == Some(125) { return Err(format!("docker could not start the sandbox: {}", e.trim()).into()); }
    }
    Ok(Outcome { stdout, stderr, total: (n_out, n_err), code, secs, stopped, ..Outcome::default() })
}

/// the time limits of one call: the run, and fetching its deps
#[derive(Clone, Copy, Debug)]
pub struct Limits { pub run: Duration, pub fetch: Duration }

/// What one call asks for.
pub struct Call<'a> {
    pub language: &'a str,
    pub code: &'a str,
    pub stdin: &'a str,
    pub deps: &'a Value,
    /// the model's `version` (checked against `images::IMAGES`)
    pub version: Option<&'a str>,
    /// the project folder whose version files pick the image (`--root`)
    pub root: Option<&'a Path>,
}

/// Run one snippet, after fetching its `deps` (checked, sorted; empty: no fetch). `cancel` is polled while it runs
/// (true: the call was cancelled). Err: the sandbox could not run it at all (no daemon, no image, bad input, a failed
/// fetch), as a message for the model.
pub fn run(c: &Call, lim: Limits, cancel: &mut dyn FnMut() -> bool) -> Result<Outcome, Fail> {
    let (language, code, stdin) = (c.language, c.code, c.stdin);
    let (lname, _, _) = *lang(language).ok_or_else(|| format!("unknown language {language:?} ({UNKNOWN_LANG})"))?;
    if code.trim().is_empty() { return Err("missing code".into()); }
    if code.len() > MAX_CODE { return Err(format!("code is too long ({} bytes; at most {MAX_CODE})", code.len()).into()); }
    if code.contains('\0') { return Err("code contains a NUL byte".into()); }
    if stdin.len() > MAX_STDIN { return Err(format!("stdin is too long ({} bytes; at most {MAX_STDIN})", stdin.len()).into()); }
    // bad input is refused before docker is touched (the image list included)
    let deps = parse_deps(lname, c.deps).map_err(|e| format!("{e} (nothing was fetched or run)"))?;
    if c.version.is_some_and(|v| !v.trim().is_empty()) { crate::images::resolve(lname, c.version, c.root, &|_| false)?; }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let daemon = find_daemon(std::env::var("DOCKER_HOST").ok().as_deref(), home.as_deref());
    let docker = docker_bin();
    let choice = match (&daemon, &docker) {
        (Ok(d), Some(b)) => choose(lname, c, b, d)?,
        // no docker: the error below says so; the line is still checked first
        _ => crate::images::resolve(lname, c.version, c.root, &|_| false)?,
    };
    let image = choice.img.image;
    let daemon = daemon?;
    let docker = docker.ok_or("the docker CLI is not installed (brew install docker colima)")?;
    let mut cached = None;
    let mut note = None;
    // a missing image: the list is read again next time, and the newest present line that fits is named for the
    // session to run on after a no (the deps' cache key is per image, so the fallback fetches its own set)
    let fallback = choice.fallback;
    let with_fallback = |mut f: Fail| {
        if f.missing.is_some() {
            forget_present();
            f.fallback = fallback.map(|i| i.version.to_string());
        }
        f
    };
    if !deps.is_empty() {
        let vol = volume(lname, std::env::var(VOLUME_PREFIX_ENV).ok().as_deref());
        let key = cache_key(image, &deps);
        let name = unique_name("fetch");
        let list = deps.join(" ");
        let manifest = if lname == "rust" { cargo_manifest(image, &deps) } else { String::new() };
        // the fetch runs in the same image as the code, so a missing image is offered (with its fallback) here too
        let f = exec(&docker, &daemon, &name, fetch_args(&name, lname, image, &vol, &key, lim.fetch),
            &[("ELISA_DEPS", &list), ("ELISA_MANIFEST", &manifest)], None, lim.fetch, cancel, lname, image).map_err(with_fallback)?;
        let said = String::from_utf8_lossy(&f.stdout);
        if let Some(w) = &f.stopped {
            let w = if w.starts_with("timed out") { format!("{w} (config sandbox_fetch_timeout)") } else { w.clone() };
            return Err(format!("deps: fetching {list} {w}; the code was not run").into());
        }
        if f.code != Some(0) || !(said.contains("ELISA-FETCHED") || said.contains("ELISA-CACHED")) {
            let why = if f.stderr.is_empty() { trimmed_ends(&f.stdout, f.total.0, 1500) } else { trimmed_ends(&f.stderr, f.total.1, 1500) };
            return Err(format!("deps: fetching {list} failed (exit {}); the code was not run:\n{why}", f.code.map_or("killed".into(), |c| c.to_string())).into());
        }
        note = Some(if said.contains("ELISA-CACHED") { "deps cached".to_string() } else { format!("deps fetched in {:.1} s", f.secs) });
        cached = Some((vol, key));
    }
    let name = unique_name("run");
    let args = run_args(&name, lname, image, lim.run, cached.as_ref().map(|(v, k)| (v.as_str(), k.as_str())));
    let mut o = exec(&docker, &daemon, &name, args, &[("ELISA_CODE", code)], Some(stdin), lim.run, cancel, lname, image).map_err(with_fallback)?;
    o.deps = note;
    o.image = image.to_string();
    o.note = choice.note.or_else(|| choice.hint.filter(|_| first_hint(lname)));
    Ok(o)
}

/// the listed images on the daemon, as last seen (this server lives as long as the session)
static PRESENT: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

fn forget_present() { if let Ok(mut p) = PRESENT.lock() { *p = None; } }

/// The listed images on the daemon: the last list read in this process, read again (`docker image ls`) when there is
/// none yet or `want` is not in it (so an image downloaded meanwhile is seen at once). Shared with the shell sandbox.
pub fn present_images(docker: &Path, d: &Daemon, want: &str) -> Vec<String> {
    let mut seen = PRESENT.lock().map(|p| p.clone()).unwrap_or(None);
    if !seen.as_ref().is_some_and(|s| s.iter().any(|i| i == want)) {
        let now = present_now(docker, d);
        if let Ok(mut p) = PRESENT.lock() { *p = Some(now.clone()); }
        seen = Some(now);
    }
    seen.unwrap_or_default()
}

/// `docker image ls`: REPO:TAG of every image (empty when it cannot be read: the run then says what is wrong)
fn present_now(docker: &Path, d: &Daemon) -> Vec<String> {
    let mut c = std::process::Command::new(docker);
    c.args(["image", "ls", "--format", "{{.Repository}}:{{.Tag}}|{{.Size}}"]).env("DOCKER_HOST", &d.host).env_remove("DOCKER_CONTEXT");
    match crate::plugins::run_cmd(c, Duration::from_secs(10)) {
        Ok((Some(0), out, _)) => out.lines().filter_map(|l| l.split('|').next()).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// The line for a call, from what is present. The image list is read once and read again only while the line the
/// call would take with everything present is not in it (so an image downloaded meanwhile is seen at once).
fn choose(lname: &str, c: &Call, docker: &Path, d: &Daemon) -> Result<crate::images::Choice, String> {
    let ideal = crate::images::resolve(lname, c.version, c.root, &|_| true)?.img.image;
    let have = present_images(docker, d, ideal);
    crate::images::resolve(lname, c.version, c.root, &|i| have.iter().any(|h| h == i.image))
}

/// true the first time a language's "older line present" hint is due in this server (once per session)
fn first_hint(lname: &str) -> bool {
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut s) = SAID.lock() else { return false };
    if s.iter().any(|l| l == lname) { return false; }
    s.push(lname.to_string());
    true
}

/// at most `max` characters of a stream, with a note when it was cut (web.rs style)
fn trimmed(b: &[u8], total: usize, max: usize, what: &str) -> String {
    let s = String::from_utf8_lossy(b);
    let s = s.trim_end_matches('\n');
    let n = s.chars().count();
    if n <= max && total <= b.len() { return s.to_string(); }
    format!("{}\n[… {what} cut at {} characters; {total} bytes in all]", s.chars().take(max).collect::<String>(), n.min(max))
}

/// stderr in at most `max` characters: when cut, its first quarter and its end (a traceback's last line says what
/// went wrong; a head-only cut would drop e.g. `urllib.error.URLError: …`)
fn trimmed_ends(b: &[u8], total: usize, max: usize) -> String {
    let s = String::from_utf8_lossy(b);
    let s = s.trim_end_matches('\n');
    let n = s.chars().count();
    if n <= max && total <= b.len() { return s.to_string(); }
    let (h, t) = (max / 4, max - max / 4);
    let head: String = s.chars().take(h).collect();
    let tail: String = s.chars().skip(n.saturating_sub(t).max(h)).collect();
    format!("{head}\n[… stderr cut: {total} bytes in all; the first {} and last {} characters shown …]\n{tail}", head.chars().count(), tail.chars().count())
}

/// the text the model gets: a status line, then stdout and stderr (each only when there is some)
pub fn format(o: &Outcome) -> String {
    let code = o.code.map_or("killed".to_string(), |c| c.to_string());
    let mut t = format!("exit {code} · {:.2} s", o.secs);
    if !o.image.is_empty() { t.push_str(&format!(" · {}", o.image)); }
    if let Some(d) = &o.deps { t.push_str(&format!(" · {d}")); }
    if let Some(w) = &o.stopped { t.push_str(&format!(" · {w}")); }
    if let Some(n) = &o.note { t.push_str(&format!("\nnote: {n}")); }
    if !o.stdout.is_empty() { t.push_str(&format!("\nstdout:\n{}", trimmed(&o.stdout, o.total.0, STDOUT_CHARS, "stdout"))); }
    if !o.stderr.is_empty() { t.push_str(&format!("\nstderr:\n{}", trimmed_ends(&o.stderr, o.total.1, STDERR_CHARS))); }
    if o.stdout.is_empty() && o.stderr.is_empty() { t.push_str("\n(no output)"); }
    t
}

/// an error result (a red row; the exit code is in the text): a non-zero exit, a kill, a timeout or another stop
fn failed(o: &Outcome) -> bool { o.stopped.is_some() || o.code != Some(0) }

/// one `tools/call`: (text, is_error, the image that is missing and the present version to fall back to)
pub fn call(name: &str, args: &Value, root: Option<&Path>, lim: Limits, cancel: &mut dyn FnMut() -> bool) -> (String, bool, Option<(String, Option<String>)>) {
    if name != "run_code" { return (format!("unknown tool {name}"), true, None); }
    let s = |k: &str| args[k].as_str().unwrap_or("");
    if s("language").trim().is_empty() { return (format!("missing language ({UNKNOWN_LANG})"), true, None); }
    let c = Call { language: s("language"), code: s("code"), stdin: s("stdin"), deps: &args["deps"], version: args["version"].as_str(), root };
    match run(&c, lim, cancel) {
        // a non-zero exit is an error result too (shown red; the exit code is in the text), as is a timeout
        Ok(o) => (format(&o), failed(&o), None),
        Err(e) => (e.text, true, e.missing.map(|m| (m, e.fallback))),
    }
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, root: Option<&Path>, lim: Limits, cancel: &mut dyn FnMut() -> bool) -> Option<Value> {
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-run", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err, missing) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], root, lim, cancel);
            let mut r = json!({"content": [{"type": "text", "text": text}], "isError": err});
            // structured, so the session never has to read the words: the image to offer for download
            if let Some((i, f)) = missing {
                r["_meta"] = json!({crate::images::META_KEY: i});
                if let Some(v) = f { r["_meta"][crate::images::FALLBACK_KEY] = json!(v); }
            }
            r
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// the `--timeout` value, within 1 ..= MAX_TIMEOUT_S
pub fn clamp_timeout(s: u64) -> Duration { Duration::from_secs(s.clamp(1, MAX_TIMEOUT_S)) }

/// the `--fetch-timeout` value, within 5 ..= MAX_FETCH_TIMEOUT_S
pub fn clamp_fetch_timeout(s: u64) -> Duration { Duration::from_secs(s.clamp(5, MAX_FETCH_TIMEOUT_S)) }

/// `--mcp-serve run [--root DIR] …`: serve stdin / stdout until stdin closes. Messages are read on their own thread, so a run in
/// progress sees `notifications/cancelled` (or stdin closing) and kills its container; a ping is answered meanwhile.
pub fn serve(args: &[String]) -> i32 {
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let lim = Limits {
        run: clamp_timeout(opt("--timeout").and_then(|t| t.parse().ok()).unwrap_or(DEFAULT_TIMEOUT_S)),
        fetch: clamp_fetch_timeout(opt("--fetch-timeout").and_then(|t| t.parse().ok()).unwrap_or(DEFAULT_FETCH_TIMEOUT_S)),
    };
    let root = opt("--root").map(PathBuf::from);
    let (tx, rx) = std::sync::mpsc::channel::<Option<Value>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(m) = serde_json::from_str::<Value>(&line) { if tx.send(Some(m)).is_err() { return; } }
        }
        let _ = tx.send(None);
    });
    let mut out = std::io::stdout();
    let mut backlog: VecDeque<Value> = VecDeque::new();
    let mut eof = false;
    loop {
        let m = match backlog.pop_front() {
            Some(m) => m,
            None if eof => return 0,
            None => match rx.recv() { Ok(Some(m)) => m, _ => return 0 },
        };
        let id = m.get("id").cloned();
        let mut cancel = || {
            while let Ok(x) = rx.try_recv() {
                let Some(v) = x else { eof = true; return true };
                if v["method"] == "notifications/cancelled" && Some(&v["params"]["requestId"]) == id.as_ref() { return true; }
                if v["method"] == "ping" && v.get("id").is_some() {
                    let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": v["id"], "result": {}})).and_then(|()| out.flush());
                } else { backlog.push_back(v); }
            }
            eof
        };
        if let Some(a) = answer(&m, root.as_deref(), lim, &mut cancel) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_mcp_and_lists_run_code() {
        let t = Limits { run: Duration::from_secs(1), fetch: Duration::from_secs(5) };
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), None, t, &mut || false).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-run");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), None, t, &mut || false).is_none());
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), None, t, &mut || false).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["run_code"]);
        // bad input is refused before docker is touched
        for args in [json!({"language": "cobol", "code": "x"}), json!({"language": "python", "code": "  "}), json!({"code": "print(1)"}),
            json!({"language": "python", "code": "import six", "deps": ["six"]}), json!({"language": "bash", "code": "x", "deps": ["jq@1.0.0"]})] {
            let r = answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "run_code", "arguments": args}}), None, t, &mut || false).unwrap();
            assert_eq!(r["result"]["isError"], true, "{args}");
        }
        let bad = answer(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), None, t, &mut || false).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[test]
    fn the_tool_definition_stays_small() {
        // small local models have ~8K context: the whole definition well under ~250 tokens (~4 chars a token)
        let n = tools().to_string().len();
        assert!(n < 1000, "{n} chars");
    }

    #[test]
    fn languages_and_aliases() {
        assert_eq!(lang("Python3").unwrap().0, "python");
        assert_eq!(lang("node").unwrap().0, "javascript");
        assert_eq!(lang("sh").unwrap().0, "bash");
        assert_eq!(lang("rs").unwrap().0, "rust");
        assert_eq!(lang("golang").unwrap().0, "go");
        assert!(lang("ruby").is_none());
    }

    #[test]
    fn the_daemon_is_docker_host_then_colima_then_the_system_socket() {
        let d = std::env::temp_dir().join(format!("elisa-run-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join(".colima/default")).unwrap();
        let sock = d.join(".colima/default/docker.sock");
        // no Colima socket and (on a dev Mac) no /var/run/docker.sock: the "start Colima" message
        if !Path::new("/var/run/docker.sock").exists() {
            assert_eq!(find_daemon(None, Some(&d)).unwrap_err(), NOT_RUNNING);
        }
        std::fs::write(&sock, "").unwrap();
        assert_eq!(find_daemon(None, Some(&d)).unwrap(), Daemon { host: format!("unix://{}", sock.display()), from_env: false });
        assert!(!find_daemon(Some(""), Some(&d)).unwrap().from_env);
        // DOCKER_HOST wins; a unix socket that is not there is the same clear message
        assert_eq!(find_daemon(Some("tcp://10.0.0.1:2375"), Some(&d)).unwrap(), Daemon { host: "tcp://10.0.0.1:2375".into(), from_env: true });
        let e = find_daemon(Some("unix:///nonexistent/docker.sock"), Some(&d)).unwrap_err();
        assert!(e.starts_with(NOT_RUNNING), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_container_is_locked_down() {
        let a = run_args("elisa-run-1", "python", crate::images::stable("python").image, Duration::from_secs(10), None).join(" ");
        for f in ["--rm", "-i", "--pull never", "--name elisa-run-1", "--network none", "--read-only", "--tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m",
            "--memory 256m", "--memory-swap 256m", "--cpus 1", "--pids-limit 64", "--user 65534:65534", "--cap-drop ALL",
            "--security-opt no-new-privileges", "-e ELISA_CODE ", "timeout -s KILL 15"] {
            assert!(a.contains(f), "missing {f}: {a}");
        }
        assert!(!a.contains("-v ") && !a.contains("--volume") && !a.contains("--mount") && !a.contains("--privileged"), "{a}");
        // only rust may execute from /tmp (its compiled binary)
        let r = run_args("n", "rust", crate::images::stable("rust").image, Duration::from_secs(10), None).join(" ");
        assert!(r.contains("/tmp:rw,exec,") && r.contains("rust:1.99-alpine"), "{r}");
        let g = run_args("n", "go", crate::images::stable("go").image, Duration::from_secs(10), None).join(" ");
        assert!(g.contains("/tmp:rw,exec,nosuid,nodev,size=256m") && g.contains("--memory 512m") && g.contains("GOPROXY=off") && g.contains("golang:1.27-alpine"), "{g}");
    }

    #[test]
    fn output_is_cut_with_a_note() {
        let o = Outcome { stdout: "x".repeat(KEEP_BYTES).into_bytes(), total: (5_000_000, 0), code: Some(0), secs: 0.5, ..Outcome::default() };
        let t = format(&o);
        assert!(t.starts_with("exit 0 · 0.50 s\nstdout:\n"), "{t}");
        assert!(t.contains(&format!("[… stdout cut at {STDOUT_CHARS} characters; 5000000 bytes in all]")), "{t}");
        assert!(t.len() < STDOUT_CHARS + 200);
        let k = Outcome { code: None, stopped: Some("timed out after 10 s".into()), ..Outcome::default() };
        assert_eq!(format(&k), "exit killed · 0.00 s · timed out after 10 s\n(no output)");
        // the image it ran in, and a note when the project's version is not on the list
        let n = Outcome { code: Some(0), image: "python:3.15-alpine".into(), note: Some("project asks for Python 3.9 (.python-version); using 3.15".into()), ..Outcome::default() };
        assert_eq!(format(&n), "exit 0 · 0.00 s · python:3.15-alpine\nnote: project asks for Python 3.9 (.python-version); using 3.15\n(no output)");
        assert_eq!(clamp_timeout(0), Duration::from_secs(1));
        assert_eq!(clamp_timeout(999), Duration::from_secs(MAX_TIMEOUT_S));
    }

    #[test]
    fn stderr_keeps_its_end() {
        // a long traceback: the last line (the one that says what happened) survives the cut
        let mut tb = String::from("Traceback (most recent call last):\n");
        for i in 0..80 { tb.push_str(&format!("  File \"/usr/lib/python3.13/urllib/request.py\", line {i}, in do_open\n")); }
        tb.push_str("urllib.error.URLError: <urlopen error [Errno -3] Try again>\n");
        let o = Outcome { stderr: tb.clone().into_bytes(), total: (0, tb.len()), code: Some(1), secs: 0.2, ..Outcome::default() };
        let t = format(&o);
        assert!(t.starts_with("exit 1 · 0.20 s\nstderr:\nTraceback (most recent call last):"), "{t}");
        assert!(t.ends_with("urllib.error.URLError: <urlopen error [Errno -3] Try again>"), "{t}");
        assert!(t.contains(&format!("[… stderr cut: {} bytes in all;", tb.len())), "{t}");
        assert!(t.chars().count() < STDERR_CHARS + 200, "{}", t.len());
        // short stderr is untouched
        let o = Outcome { stderr: b"bad\n".to_vec(), total: (0, 4), code: Some(1), ..Outcome::default() };
        assert_eq!(format(&o), "exit 1 · 0.00 s\nstderr:\nbad");
        assert!(failed(&o));
        assert!(!failed(&Outcome { code: Some(0), ..Outcome::default() }));
        assert!(failed(&Outcome { code: None, stopped: Some("timed out after 10 s".into()), ..Outcome::default() }));
        assert!(failed(&Outcome { code: Some(0), stopped: Some("cancelled".into()), ..Outcome::default() }));
        // drain's kept bytes for a stream over KEEP_BYTES: its first and last halves
        let all: Vec<u8> = (0..3 * KEEP_BYTES).map(|i| (i % 251) as u8).collect();
        let kept = head_tail(all[..KEEP_BYTES].to_vec(), &all[all.len() - KEEP_BYTES..], all.len(), true);
        assert_eq!(kept.len(), KEEP_BYTES);
        assert_eq!(&kept[..KEEP_BYTES / 2], &all[..KEEP_BYTES / 2]);
        assert_eq!(&kept[KEEP_BYTES / 2..], &all[all.len() - KEEP_BYTES / 2..]);
        assert_eq!(head_tail(b"short".to_vec(), b"short", 5, true), b"short");
    }

    #[test]
    fn deps_must_be_exact_registry_pins() {
        let ok = |l: &str, d: &str| check_dep(l, d).unwrap_or_else(|e| panic!("{l} {d}: {e}"));
        assert_eq!(ok("python", "Six==1.16.0"), "six==1.16.0");
        assert_eq!(ok("python", "requests[socks]==2.32.3"), "requests[socks]==2.32.3");
        assert_eq!(ok("javascript", "left-pad@1.3.0"), "left-pad@1.3.0");
        assert_eq!(ok("javascript", "@types/node@22.7.4"), "@types/node@22.7.4");
        assert_eq!(ok("rust", "itoa@1.0.11"), "itoa@1.0.11");
        assert_eq!(ok("rust", "itoa = \"=1.0.11\""), "itoa@1.0.11");
        assert_eq!(ok("rust", "serde[derive]@1.0.210"), "serde[derive]@1.0.210");
        assert_eq!(ok("go", "golang.org/x/text@v0.14.0"), "golang.org/x/text@v0.14.0");
        assert_eq!(ok("go", "github.com/google/uuid@v1.6.0"), "github.com/google/uuid@v1.6.0");
        let no = |l: &str, d: &str, why: &str| {
            let e = check_dep(l, d).expect_err(d);
            assert!(e.contains(why) && e.starts_with("deps: "), "{l} {d}: {e}");
        };
        for (l, d) in [("python", "six"), ("python", "six>=1.16"), ("python", "six~=1.16"), ("python", "six==1.*"), ("javascript", "left-pad"),
            ("javascript", "left-pad@^1.3.0"), ("javascript", "left-pad@latest"), ("rust", "itoa"), ("rust", "itoa = \"1.0\""), ("rust", "itoa@1"),
            ("go", "golang.org/x/text"), ("go", "golang.org/x/text@latest"), ("javascript", "left-pad@~1.3.0")] {
            no(l, d, "pinned");
        }
        no("go", "golang.org/x/text@master", "no branches");
        for (l, d) in [("python", "https://evil.example/x.whl"), ("python", "git+https://github.com/a/b"), ("javascript", "github:a/b"),
            ("javascript", "./local"), ("rust", "/etc/passwd"), ("go", "../x@v1.0.0"), ("javascript", "npm:left-pad@1.3.0"),
            ("python", "file:/x"), ("go", "github.com/a/b.git@v1.0.0")] {
            no(l, d, "URL, path or git ref");
        }
        for (l, d) in [("python", "--index-url=x"), ("javascript", "-g"), ("rust", "--locked")] { no(l, d, "flag"); }
        for (l, d) in [("python", "six==1.16.0;rm -rf /"), ("javascript", "a@1.0.0 && curl x"), ("python", "six==1.16.0 six==1.15.0"),
            ("go", "a.com/b@v1.0.0$(id)"), ("rust", "itoa@1.0.11`id`"), ("javascript", "x@1.0.0\nb@1.0.0")] {
            no(l, d, "contains");
        }
        assert!(check_dep("bash", "jq@1.7.1").unwrap_err().contains("bash takes no deps"));
        // the list: a string splits at commas, entries are sorted and de-duplicated, at most MAX_DEPS
        assert_eq!(parse_deps("python", &json!("six==1.16.0, attrs==24.2.0,six==1.16.0")).unwrap(), ["attrs==24.2.0", "six==1.16.0"]);
        assert!(parse_deps("python", &json!(null)).unwrap().is_empty() && parse_deps("python", &json!([])).unwrap().is_empty());
        let many: Vec<String> = (0..=MAX_DEPS).map(|i| format!("p{i}==1.0.0")).collect();
        assert!(parse_deps("python", &json!(many)).unwrap_err().contains("at most 10"));
        assert!(parse_deps("python", &json!([1])).is_err() && parse_deps("python", &json!({"a": 1})).is_err());
        // the cache key follows the image and the set, not the order it was given in
        let a = cache_key("python:3.13-alpine", &parse_deps("python", &json!(["b==1.0.0", "a==1.0.0"])).unwrap());
        assert_eq!(a, cache_key("python:3.13-alpine", &parse_deps("python", &json!(["a==1.0.0", "b==1.0.0"])).unwrap()));
        assert_ne!(a, cache_key("python:3.14-alpine", &parse_deps("python", &json!(["a==1.0.0", "b==1.0.0"])).unwrap()));
        assert!(a.len() == 16 && a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(volume("python", None), "elisa-deps-python");
        assert_eq!(volume("go", Some("elisa-test-1-")), "elisa-test-1-go");
        assert_eq!(volume("go", Some("bad name;")), "elisa-deps-go");
    }

    #[test]
    fn the_approval_line_names_the_deps_and_the_network() {
        assert_eq!(deps_summary(&json!({"deps": ["six==1.16.0", "attrs==24.2.0"]})), " · fetches online: six==1.16.0, attrs==24.2.0");
        assert_eq!(deps_summary(&json!({"deps": []})), "");
        assert_eq!(deps_summary(&json!({})), "");
        assert!(deps_summary(&json!({"deps": vec!["x".repeat(50); 5]})).ends_with('…'));
    }

    #[test]
    fn a_run_with_deps_is_still_offline_and_mounts_only_the_cache_read_only() {
        for l in ["python", "javascript", "rust", "go"] {
            let a = run_args("elisa-run-1", l, crate::images::stable(l).image, Duration::from_secs(10), Some(("elisa-deps-x", "0123456789abcdef")));
            let j = a.join(" ");
            let net: Vec<&str> = a.windows(2).filter(|w| w[0] == "--network").map(|w| w[1].as_str()).collect();
            assert_eq!(net, ["none"], "{j}");
            let mounts: Vec<&str> = a.windows(2).filter(|w| w[0] == "--mount").map(|w| w[1].as_str()).collect();
            assert_eq!(mounts, ["type=volume,src=elisa-deps-x,dst=/deps,readonly"], "{j}");
            for f in ["--read-only", "--user 65534:65534", "--cap-drop ALL", "--pids-limit 64", "--security-opt no-new-privileges"] { assert!(j.contains(f), "{f}: {j}"); }
            assert!(!a.iter().any(|x| x == "-v" || x == "--volume" || x == "--privileged" || x.starts_with("--network=")), "{j}");
        }
        let p = run_args("n", "python", crate::images::stable("python").image, Duration::from_secs(10), Some(("v", "k"))).join(" ");
        assert!(p.contains("PYTHONPATH=/deps/k") && p.contains("size=64m"), "{p}");
        let j = run_args("n", "javascript", crate::images::stable("javascript").image, Duration::from_secs(10), Some(("v", "k"))).join(" ");
        assert!(j.contains("NODE_PATH=/deps/k/node_modules"), "{j}");
        let r = run_args("n", "rust", crate::images::stable("rust").image, Duration::from_secs(10), Some(("v", "k"))).join(" ");
        assert!(r.contains("cargo run -q --offline --locked") && r.contains("ln -s /deps/k/home/registry ch/registry") && r.contains("size=256m") && r.contains("--memory 256m"), "{r}");
        let g = run_args("n", "go", crate::images::stable("go").image, Duration::from_secs(10), Some(("v", "k"))).join(" ");
        assert!(g.contains("GOMODCACHE=/deps/k/mod") && g.contains("GOPROXY=off") && g.contains("GOSUMDB=off") && g.contains("go run ."), "{g}");
    }

    #[test]
    fn the_fetch_container_has_network_but_no_host_files_and_the_same_limits() {
        for l in ["python", "javascript", "rust", "go"] {
            let image = crate::images::stable(l).image;
            let a = fetch_args("elisa-fetch-1", l, image, "elisa-deps-x", "0123456789abcdef", Duration::from_secs(60));
            let j = a.join(" ");
            let mounts: Vec<&str> = a.windows(2).filter(|w| w[0] == "--mount").map(|w| w[1].as_str()).collect();
            assert_eq!(mounts, ["type=volume,src=elisa-deps-x,dst=/deps"], "{j}");
            assert!(!a.iter().any(|x| x == "-v" || x == "--volume" || x == "-i" || x == "--privileged" || x.contains("/Users") || x.starts_with("--network=")), "{j}");
            for f in ["--rm", "--pull never", "--network bridge", "--read-only", "--cap-drop ALL", "--security-opt no-new-privileges", "--pids-limit 64",
                "--cpus 1", "--user 0:0", "-e ELISA_DEPS ", "timeout -s KILL 65", "D=/deps/0123456789abcdef", "ELISA-CACHED", "ELISA-FETCHED"] {
                assert!(j.contains(f), "{l} {f}: {j}");
            }
            // the same memory as the run with deps; no code goes in
            let (_, mem) = limits(l, true);
            assert!(j.contains(&format!("--memory {mem}m")) && !j.contains("ELISA_CODE"), "{j}");
            assert_eq!(a[a.len() - 4], image);
        }
        let p = fetch_args("n", "python", "python:3.13-alpine", "v", "k", Duration::from_secs(60)).join(" ");
        assert!(p.contains("--only-binary=:all:") && p.contains("--target"), "{p}");
        let j = fetch_args("n", "javascript", "node:22-alpine", "v", "k", Duration::from_secs(60)).join(" ");
        assert!(j.contains("--ignore-scripts"), "{j}");
        let g = fetch_args("n", "go", "golang:1.25-alpine", "v", "k", Duration::from_secs(60)).join(" ");
        assert!(g.contains("GOPROXY=https://proxy.golang.org &&") && g.contains("GOTOOLCHAIN=local"), "{g}");
        let m = cargo_manifest("rust:1.91-alpine", &["itoa@1.0.11".into(), "serde[derive,rc]@1.0.210".into()]);
        assert!(m.contains("rust-version = \"1.91\"") && m.contains("itoa = \"=1.0.11\"\n") && m.contains("serde = { version = \"=1.0.210\", features = [\"derive\", \"rc\"] }"), "{m}");
        assert_eq!(clamp_fetch_timeout(1), Duration::from_secs(5));
        assert_eq!(clamp_fetch_timeout(999), Duration::from_secs(MAX_FETCH_TIMEOUT_S));
    }

    #[test]
    fn the_status_line_says_how_the_deps_came() {
        let o = Outcome { stdout: b"1.16.0\n".to_vec(), total: (7, 0), code: Some(0), secs: 0.3, deps: Some("deps fetched in 2.1 s".into()), ..Outcome::default() };
        assert_eq!(format(&o), "exit 0 · 0.30 s · deps fetched in 2.1 s\nstdout:\n1.16.0");
    }
}
