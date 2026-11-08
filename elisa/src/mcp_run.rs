//! Elisa's own code sandbox: `elisa --mcp-serve run [--timeout SECS]`, a stdio MCP server with one tool, `run_code`,
//! that runs a short snippet (python, javascript, bash, rust) in a throwaway container and returns stdout, stderr, the
//! exit code and the time. Off unless config `sandbox = "colima"`; every call asks first (approval `mcp:run`).
//!
//! The container (`docker run`, flags in `run_args`): no network, read-only root, a 64 MB tmpfs at /tmp, 256 MB memory
//! (no swap), one CPU, at most 64 processes, user nobody, no capabilities, no new privileges, no host mounts. The code
//! goes in through the docker client's environment (`-e ELISA_CODE`, not its arguments, which `ps` shows) and is
//! written to /tmp inside; the snippet's own stdin is the container's stdin. Elisa enforces the wall-clock limit
//! (`docker kill`), and a `timeout` inside the container is the backstop if this server itself dies.
//!
//! The daemon: `DOCKER_HOST` if set, else Colima's `~/.colima/default/docker.sock`, else `/var/run/docker.sock`,
//! handed to the docker CLI as `DOCKER_HOST` (the user's docker context is never read or changed). This server never
//! starts Colima and never pulls an image (`--pull never`): it says what to run instead.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// default wall-clock limit per run, seconds (config `sandbox_timeout`)
pub const DEFAULT_TIMEOUT_S: u64 = 10;
/// the most `sandbox_timeout` may be
pub const MAX_TIMEOUT_S: u64 = 30;
/// largest snippet / stdin accepted, bytes (the code travels in one environment variable)
const MAX_CODE: usize = 64 * 1024;
const MAX_STDIN: usize = 64 * 1024;
/// output kept per stream, bytes; a run that writes more than OUTPUT_STOP in all is stopped early
const KEEP_BYTES: usize = 32 * 1024;
const OUTPUT_STOP: usize = 1 << 20;
/// characters of stdout / stderr given back to the model
const STDOUT_CHARS: usize = 4000;
const STDERR_CHARS: usize = 2000;

/// (language, image, file name, command run in /tmp). Images are pinned to a minor release; pull each once with
/// `docker pull IMAGE` (README "MCP servers").
pub const LANGS: [(&str, &str, &str, &str); 4] = [
    ("python", "python:3.13-alpine", "main.py", "python3 main.py"),
    ("javascript", "node:22-alpine", "main.js", "node main.js"),
    ("bash", "bash:5.2", "main.sh", "bash main.sh"),
    ("rust", "rust:1.91-alpine", "main.rs", "rustc --edition 2021 -o main main.rs && ./main"),
];

/// the tools this server lists (MCP `tools/list`); kept short for small context windows
pub fn tools() -> Value {
    json!([
        {"name": "run_code", "description": "Run a short snippet in an offline sandbox (no network, no project files); returns stdout, stderr, exit code, time.",
         "inputSchema": {"type": "object", "properties": {
             "language": {"type": "string", "enum": ["python", "javascript", "bash", "rust"]},
             "code": {"type": "string"},
             "stdin": {"type": "string"}}, "required": ["language", "code"]}},
    ])
}

/// the language a model asked for, with the usual aliases
fn lang(l: &str) -> Option<&'static (&'static str, &'static str, &'static str, &'static str)> {
    let l = l.trim().to_ascii_lowercase();
    let l = match l.as_str() { "py" | "python3" => "python", "js" | "node" | "nodejs" => "javascript", "sh" | "shell" => "bash", "rs" => "rust", o => o };
    LANGS.iter().find(|x| x.0 == l)
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

/// the docker CLI: on PATH, else where Homebrew puts it
fn docker_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join("docker"))
        .chain(["/opt/homebrew/bin/docker", "/usr/local/bin/docker"].map(PathBuf::from))
        .find(|p| p.is_file())
}

/// a name no other run has: `elisa-run-PID-N-NANOS`
fn unique_name() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("elisa-run-{}-{}-{nanos}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// The `docker run` arguments: the isolation flags, the image, and a shell line that writes the code (from
/// `$ELISA_CODE`) to /tmp, drops the variable, and runs it under a backstop `timeout`.
pub fn run_args(name: &str, image: &str, file: &str, cmd: &str, timeout: Duration) -> Vec<String> {
    // rust runs the binary it compiled into /tmp; everything else only reads its file there
    let tmpfs = if file.ends_with(".rs") { "/tmp:rw,exec,nosuid,nodev,size=64m" } else { "/tmp:rw,noexec,nosuid,nodev,size=64m" };
    let backstop = timeout.as_secs() + 5;
    let script = format!("printf '%s' \"$ELISA_CODE\" > {file} && unset ELISA_CODE && timeout -s KILL {backstop} sh -c '{cmd}'");
    [
        "run", "--rm", "-i", "--pull", "never", "--name", name, "--init", "--network", "none", "--read-only",
        "--tmpfs", tmpfs, "--workdir", "/tmp", "--memory", "256m", "--memory-swap", "256m", "--cpus", "1",
        "--pids-limit", "64", "--user", "65534:65534", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--log-driver", "none", "-e", "ELISA_CODE", "-e", "HOME=/tmp", "-e", "PYTHONUNBUFFERED=1", "-e", "PYTHONDONTWRITEBYTECODE=1",
        image, "sh", "-c", &script,
    ].iter().map(|s| s.to_string()).collect()
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
}

/// read a stream to the end, keeping the first KEEP_BYTES; the shared counter lets the runner stop a flood
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
fn kill(docker: &Path, host: &str, name: &str) {
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

/// Run one snippet. `cancel` is polled while it runs (true: the call was cancelled). Err: the sandbox could not run
/// it at all (no daemon, no image, bad input), as a message for the model.
pub fn run(language: &str, code: &str, stdin: &str, timeout: Duration, cancel: &mut dyn FnMut() -> bool) -> Result<Outcome, String> {
    let (lname, image, file, cmd) = *lang(language).ok_or_else(|| format!("unknown language {language:?} (use python, javascript, bash or rust)"))?;
    if code.trim().is_empty() { return Err("missing code".into()); }
    if code.len() > MAX_CODE { return Err(format!("code is too long ({} bytes; at most {MAX_CODE})", code.len())); }
    if code.contains('\0') { return Err("code contains a NUL byte".into()); }
    if stdin.len() > MAX_STDIN { return Err(format!("stdin is too long ({} bytes; at most {MAX_STDIN})", stdin.len())); }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let daemon = find_daemon(std::env::var("DOCKER_HOST").ok().as_deref(), home.as_deref())?;
    let docker = docker_bin().ok_or("the docker CLI is not installed (brew install docker colima)")?;
    let name = unique_name();
    let t0 = Instant::now();
    let mut child = Command::new(&docker).args(run_args(&name, image, file, cmd, timeout))
        .env("DOCKER_HOST", &daemon.host).env_remove("DOCKER_CONTEXT").env("ELISA_CODE", code)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("cannot run {}: {e}", docker.display()))?;
    let seen = std::sync::Arc::new(AtomicU64::new(0));
    let out = drain(child.stdout.take().ok_or("no stdout")?, seen.clone(), false);
    let err = drain(child.stderr.take().ok_or("no stderr")?, seen.clone(), true);
    if let Some(mut w) = child.stdin.take() {
        let input = stdin.as_bytes().to_vec();
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
            kill(&docker, &daemon.host, &name);
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
            let h = if daemon.from_env { String::new() } else { format!(" -H {}", daemon.host) };
            return Err(format!("the {lname} image {image} is not on this machine; pull it once (outside Elisa) with: docker{h} pull {image}"));
        }
        if e.contains("Cannot connect to the Docker daemon") || e.contains("failed to connect to the docker API") {
            return Err(NOT_RUNNING.into());
        }
        if code == Some(125) { return Err(format!("docker could not start the sandbox: {}", e.trim())); }
    }
    Ok(Outcome { stdout, stderr, total: (n_out, n_err), code, secs, stopped })
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
/// went wrong; QA 2026-11-08: a head-only cut dropped `urllib.error.URLError: …`)
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
    if let Some(w) = &o.stopped { t.push_str(&format!(" · {w}")); }
    if !o.stdout.is_empty() { t.push_str(&format!("\nstdout:\n{}", trimmed(&o.stdout, o.total.0, STDOUT_CHARS, "stdout"))); }
    if !o.stderr.is_empty() { t.push_str(&format!("\nstderr:\n{}", trimmed_ends(&o.stderr, o.total.1, STDERR_CHARS))); }
    if o.stdout.is_empty() && o.stderr.is_empty() { t.push_str("\n(no output)"); }
    t
}

/// an error result (a red row; the exit code is in the text): a non-zero exit, a kill, a timeout or another stop
fn failed(o: &Outcome) -> bool { o.stopped.is_some() || o.code != Some(0) }

/// one `tools/call`: (text, is_error)
pub fn call(name: &str, args: &Value, timeout: Duration, cancel: &mut dyn FnMut() -> bool) -> (String, bool) {
    if name != "run_code" { return (format!("unknown tool {name}"), true); }
    let s = |k: &str| args[k].as_str().unwrap_or("");
    if s("language").trim().is_empty() { return ("missing language (python, javascript, bash or rust)".into(), true); }
    match run(s("language"), s("code"), s("stdin"), timeout, cancel) {
        // a non-zero exit is an error result too (shown red; the exit code is in the text), as is a timeout
        Ok(o) => (format(&o), failed(&o)),
        Err(e) => (e, true),
    }
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, timeout: Duration, cancel: &mut dyn FnMut() -> bool) -> Option<Value> {
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-run", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], timeout, cancel);
            json!({"content": [{"type": "text", "text": text}], "isError": err})
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// the `--timeout` value, within 1 ..= MAX_TIMEOUT_S
pub fn clamp_timeout(s: u64) -> Duration { Duration::from_secs(s.clamp(1, MAX_TIMEOUT_S)) }

/// `--mcp-serve run …`: serve stdin / stdout until stdin closes. Messages are read on their own thread, so a run in
/// progress sees `notifications/cancelled` (or stdin closing) and kills its container; a ping is answered meanwhile.
pub fn serve(args: &[String]) -> i32 {
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let timeout = clamp_timeout(opt("--timeout").and_then(|t| t.parse().ok()).unwrap_or(DEFAULT_TIMEOUT_S));
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
        if let Some(a) = answer(&m, timeout, &mut cancel) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_mcp_and_lists_run_code() {
        let t = Duration::from_secs(1);
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), t, &mut || false).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-run");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), t, &mut || false).is_none());
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), t, &mut || false).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["run_code"]);
        // bad input is refused before docker is touched
        for args in [json!({"language": "cobol", "code": "x"}), json!({"language": "python", "code": "  "}), json!({"code": "print(1)"})] {
            let r = answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "run_code", "arguments": args}}), t, &mut || false).unwrap();
            assert_eq!(r["result"]["isError"], true, "{args}");
        }
        let bad = answer(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), t, &mut || false).unwrap();
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
        assert_eq!(lang("Python3").unwrap().1, "python:3.13-alpine");
        assert_eq!(lang("node").unwrap().0, "javascript");
        assert_eq!(lang("sh").unwrap().0, "bash");
        assert_eq!(lang("rs").unwrap().0, "rust");
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
        let a = run_args("elisa-run-1", "python:3.13-alpine", "main.py", "python3 main.py", Duration::from_secs(10)).join(" ");
        for f in ["--rm", "-i", "--pull never", "--name elisa-run-1", "--network none", "--read-only", "--tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m",
            "--memory 256m", "--memory-swap 256m", "--cpus 1", "--pids-limit 64", "--user 65534:65534", "--cap-drop ALL",
            "--security-opt no-new-privileges", "-e ELISA_CODE ", "timeout -s KILL 15"] {
            assert!(a.contains(f), "missing {f}: {a}");
        }
        assert!(!a.contains("-v ") && !a.contains("--volume") && !a.contains("--mount") && !a.contains("--privileged"), "{a}");
        // only rust may execute from /tmp (its compiled binary)
        let r = run_args("n", "rust:1.91-alpine", "main.rs", "rustc main.rs", Duration::from_secs(10)).join(" ");
        assert!(r.contains("/tmp:rw,exec,"), "{r}");
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
}
