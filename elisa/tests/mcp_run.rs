//! The built-in code sandbox (`elisa --mcp-serve run`, src/mcp_run.rs), spoken to over stdio like the session does.
//! The tests marked `#[ignore]` need Colima running and the images pulled (golang optional: its cases are skipped
//! when the image is not there); `colima_fetches_deps_*` also need the internet (PyPI, npm, crates.io, the Go proxy)
//! and use their own cache volumes (`elisa-test-PID-…`), removed at the end:
//!   colima start && docker pull python:3.13-alpine node:22-alpine bash:5.2 rust:1.91-alpine golang:1.25-alpine
//!   DOCKER_HOST=unix://$HOME/.colima/default/docker.sock cargo test --release --test mcp_run -- --ignored
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

struct Server { child: Child, stdin: ChildStdin, rx: Receiver<Value>, next: u64 }

impl Server {
    fn start(timeout_s: u64, env: &[(&str, String)]) -> Server { Server::start_with(timeout_s, 80, env) }

    fn start_with(timeout_s: u64, fetch_s: u64, env: &[(&str, String)]) -> Server {
        let mut c = Command::new(env!("CARGO_BIN_EXE_elisa"));
        c.args(["--mcp-serve", "run", "--timeout", &timeout_s.to_string(), "--fetch-timeout", &fetch_s.to_string()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env { c.env(k, v); }
        let mut child = c.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for l in BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) { if tx.send(v).is_err() { return; } }
        });
        let mut s = Server { child, stdin, rx, next: 1 };
        let init = s.request("initialize", json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-run");
        s
    }

    fn send(&mut self, v: &Value) { writeln!(self.stdin, "{v}").unwrap(); self.stdin.flush().unwrap(); }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.wait(id)
    }

    fn wait(&mut self, id: u64) -> Value {
        loop {
            let v = self.rx.recv_timeout(Duration::from_secs(60)).expect("no answer");
            if v["id"] == id { return v; }
        }
    }

    /// (text, isError)
    fn run(&mut self, args: Value) -> (String, bool) {
        let r = self.request("tools/call", json!({"name": "run_code", "arguments": args}));
        (r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string(), r["result"]["isError"].as_bool().unwrap_or(false))
    }
}

impl Drop for Server {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("elisa-mcp-run-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn no_daemon_says_to_start_colima_and_never_starts_it() {
    let home = scratch("nodaemon");
    let mut s = Server::start(5, &[("HOME", home.display().to_string()), ("DOCKER_HOST", "unix:///nonexistent/elisa/docker.sock".into())]);
    let (t, err) = s.run(json!({"language": "python", "code": "print(1)"}));
    assert!(err && t.contains("Colima isn't running — start it with `colima start`"), "{t}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A stand-in `docker` (first on PATH) that logs its arguments and environment and, for `run`, hangs: the wall-clock
/// limit must `docker kill` the container by its name, and the code must come through the environment.
fn fake_docker(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = d.join("log");
    std::fs::write(bin.join("docker"), format!(
        "#!/bin/sh\necho \"ARGS $* HOST=$DOCKER_HOST\" >> {log}\n\
         case \"$*\" in *\"--name elisa-fetch-\"*) echo \"DEPS $ELISA_DEPS CODE=$ELISA_CODE\" >> {log}; [ -n \"$FAKE_FETCH_HANG\" ] && exec sleep 30; echo ELISA-FETCHED; exit 0;; esac\n\
         if [ \"$1\" = run ]; then echo \"CODE $ELISA_CODE\" >> {log}; echo started; exec sleep 30; fi\n",
        log = log.display())).unwrap();
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(bin.join("docker"), std::fs::Permissions::from_mode(0o755)).unwrap(); }
    std::fs::write(d.join("sock"), "").unwrap();
    log
}

#[test]
fn the_wall_clock_limit_kills_the_container_by_name() {
    let d = scratch("timeout");
    let log = fake_docker(&d);
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let host = format!("unix://{}", d.join("sock").display());
    let mut s = Server::start(1, &[("PATH", path), ("DOCKER_HOST", host.clone()), ("HOME", d.display().to_string())]);
    let t0 = Instant::now();
    let (t, err) = s.run(json!({"language": "python", "code": "while True: pass  # secret-marker"}));
    assert!(t0.elapsed() < Duration::from_secs(8), "{:?}", t0.elapsed());
    assert!(err && t.contains("timed out after 1 s") && t.contains("started"), "{t}");
    let l = std::fs::read_to_string(&log).unwrap();
    let run = l.lines().find(|x| x.starts_with("ARGS run")).unwrap();
    let name = run.split("--name ").nth(1).unwrap().split(' ').next().unwrap();
    assert!(name.starts_with("elisa-run-"), "{run}");
    assert!(l.lines().any(|x| x.starts_with(&format!("ARGS kill {name}")) && x.ends_with(&format!("HOST={host}"))), "{l}");
    // the code is in the environment, never the arguments (`ps` shows those)
    assert!(!run.contains("secret-marker") && l.contains("CODE while True: pass  # secret-marker"), "{l}");
    for f in ["--network none", "--read-only", "--user 65534:65534", "--cap-drop ALL", "--pids-limit 64", "--memory 256m"] { assert!(run.contains(f), "{f}: {run}"); }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_cancelled_call_kills_its_container_and_the_server_stays_up() {
    let d = scratch("cancel");
    let log = fake_docker(&d);
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let mut s = Server::start(30, &[("PATH", path), ("DOCKER_HOST", format!("unix://{}", d.join("sock").display())), ("HOME", d.display().to_string())]);
    s.send(&json!({"jsonrpc": "2.0", "id": 50, "method": "tools/call", "params": {"name": "run_code", "arguments": {"language": "bash", "code": "sleep 100"}}}));
    std::thread::sleep(Duration::from_millis(500));
    // a ping is answered while the run is going
    s.send(&json!({"jsonrpc": "2.0", "id": 51, "method": "ping"}));
    assert_eq!(s.wait(51)["result"], json!({}));
    let t0 = Instant::now();
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 50}}));
    let r = s.wait(50);
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("cancelled"), "{r}");
    assert!(std::fs::read_to_string(&log).unwrap().contains("ARGS kill elisa-run-"));
    assert_eq!(s.request("ping", json!({}))["result"], json!({}));
    let _ = std::fs::remove_dir_all(&d);
}

/// the fake docker's environment: its bin first on PATH, its socket, a scratch HOME, a test volume prefix
fn fake_env(d: &Path) -> Vec<(&'static str, String)> {
    vec![("PATH", format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default())),
        ("DOCKER_HOST", format!("unix://{}", d.join("sock").display())), ("HOME", d.display().to_string()),
        ("ELISA_SANDBOX_VOLUME_PREFIX", "elisa-test-fake-".into())]
}

#[test]
fn deps_are_fetched_online_first_then_the_code_runs_offline() {
    let d = scratch("deps");
    let log = fake_docker(&d);
    let mut s = Server::start_with(1, 80, &fake_env(&d));
    let (t, err) = s.run(json!({"language": "python", "code": "import six  # code-marker", "deps": ["six==1.16.0", "attrs==24.2.0"]}));
    assert!(err && t.contains("timed out after 1 s") && t.contains("deps fetched in"), "{t}");
    let l = std::fs::read_to_string(&log).unwrap();
    let runs: Vec<&str> = l.lines().filter(|x| x.starts_with("ARGS run")).collect();
    assert_eq!(runs.len(), 2, "{l}");
    let (fetch, run) = (runs[0], runs[1]);
    // the fetch: network, only the cache volume, no code; the list (sorted) in its environment, not its arguments
    assert!(fetch.contains("--name elisa-fetch-") && fetch.contains("--network bridge") && !fetch.contains("--network none"), "{fetch}");
    assert_eq!(fetch.matches("--mount").count(), 1, "{fetch}");
    assert!(fetch.contains("--mount type=volume,src=elisa-test-fake-python,dst=/deps ") && !fetch.contains(" -v "), "{fetch}");
    assert!(!fetch.contains("six==") && l.contains("DEPS attrs==24.2.0 six==1.16.0 CODE=\n"), "{l}");
    // the run: no network, the same volume read-only, the code in its environment
    assert!(run.contains("--name elisa-run-") && run.contains("--network none") && !run.contains("--network bridge"), "{run}");
    assert!(run.contains("--mount type=volume,src=elisa-test-fake-python,dst=/deps,readonly") && run.contains("PYTHONPATH=/deps/"), "{run}");
    assert!(l.contains("CODE import six  # code-marker"), "{l}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn bad_deps_are_refused_before_docker_is_touched() {
    let d = scratch("baddeps");
    let log = fake_docker(&d);
    let mut s = Server::start(5, &fake_env(&d));
    for (lang, deps, why) in [("python", json!(["six"]), "not pinned"), ("javascript", json!(["left-pad@^1.3.0"]), "not pinned"),
        ("rust", json!(["itoa = \"1\""]), "not pinned"), ("go", json!(["github.com/a/b@master"]), "no branches"),
        ("python", json!(["https://evil.example/six.whl"]), "URL, path or git ref"), ("javascript", json!(["--registry=http://x"]), "flag"),
        ("python", json!(["six==1.16.0; curl x"]), "contains"), ("bash", json!(["jq@1.7.1"]), "takes no deps"),
        ("python", json!((0..11).map(|i| format!("p{i}==1.0.0")).collect::<Vec<_>>()), "at most 10")] {
        let (t, err) = s.run(json!({"language": lang, "code": "x", "deps": deps}));
        assert!(err && t.contains(why) && t.contains("nothing was fetched or run"), "{lang} {deps}: {t}");
    }
    assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap_or_default());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_slow_fetch_is_killed_at_its_own_limit_and_the_code_never_runs() {
    let d = scratch("slowfetch");
    let log = fake_docker(&d);
    let mut env = fake_env(&d);
    env.push(("FAKE_FETCH_HANG", "1".into()));
    let mut s = Server::start_with(30, 5, &env);
    let t0 = Instant::now();
    let (t, err) = s.run(json!({"language": "javascript", "code": "console.log(1)", "deps": ["left-pad@1.3.0"]}));
    assert!(t0.elapsed() < Duration::from_secs(12), "{:?}", t0.elapsed());
    assert!(err && t.contains("deps: fetching left-pad@1.3.0 timed out after 5 s (config sandbox_fetch_timeout); the code was not run"), "{t}");
    let l = std::fs::read_to_string(&log).unwrap();
    assert!(l.lines().any(|x| x.starts_with("ARGS kill elisa-fetch-")), "{l}");
    assert!(!l.contains("--name elisa-run-"), "{l}");
    let _ = std::fs::remove_dir_all(&d);
}

// ---- with Colima (ignored by default) ----

fn colima() -> Server { colima_with(10) }

fn exit_line(t: &str) -> &str { t.lines().next().unwrap_or("") }

#[test]
#[ignore = "needs Colima and the images"]
fn colima_runs_each_language() {
    let mut s = colima();
    for (lang, code) in [("python", "print(6*7)"), ("javascript", "console.log(6*7)"), ("bash", "echo $((6*7))"),
        ("rust", "fn main() { println!(\"{}\", 6 * 7); }")] {
        let (t, err) = s.run(json!({"language": lang, "code": code}));
        assert!(!err && t.starts_with("exit 0 ·") && t.contains("stdout:\n42"), "{lang}: {t}");
        eprintln!("{lang}: {}", exit_line(&t));
    }
    // stdin reaches the snippet; a failing snippet is an error result (red), its exit code in the text
    let (t, _) = s.run(json!({"language": "python", "code": "import sys; print(sys.stdin.read().upper())", "stdin": "hello"}));
    assert!(t.contains("HELLO"), "{t}");
    let (t, err) = s.run(json!({"language": "python", "code": "raise SystemExit('bad')"}));
    assert!(err && t.starts_with("exit 1") && t.contains("stderr:\nbad"), "{t}");
    let (t, _) = s.run(json!({"language": "rust", "code": "fn main() { let x: i32 = \"no\"; }"}));
    assert!(t.starts_with("exit 1") && t.contains("mismatched types"), "{t}");
}

#[test]
#[ignore = "needs Colima and the images"]
fn colima_contains_an_infinite_loop() {
    let mut s = colima_with(3);
    let t0 = Instant::now();
    let (t, err) = s.run(json!({"language": "python", "code": "print('before', flush=True)\nwhile True: pass"}));
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    assert!(err && t.contains("timed out after 3 s") && t.contains("before"), "{t}");
    // nothing left running
    assert!(!docker_ps().contains("elisa-run-"), "{}", docker_ps());
}

#[test]
#[ignore = "needs Colima and the images"]
fn colima_contains_a_fork_bomb() {
    let mut s = colima_with(5);
    // the classic bomb, kept alive for a moment: the process limit stops it, the run still ends, the host is fine
    let t0 = Instant::now();
    let (t, _) = s.run(json!({"language": "bash", "code": ":(){ :|:& };:\nsleep 2; echo still-here"}));
    eprintln!("fork bomb ({:?}): {}", t0.elapsed(), &t[..t.len().min(400)]);
    assert!(t0.elapsed() < Duration::from_secs(12), "{:?}", t0.elapsed());
    assert!(t.contains("retry") || t.contains("Resource temporarily unavailable") || t.contains("timed out"), "{t}");
    let (t, _) = s.run(json!({"language": "python", "code": "import os\nn=0\ntry:\n    while True:\n        if os.fork()==0:\n            import time; time.sleep(60)\n        n+=1\nexcept OSError as e:\n    print('stopped after', n, e)"}));
    assert!(t.contains("stopped after") && t.contains("Resource temporarily unavailable"), "{t}");
    let n: usize = t.split("stopped after ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
    assert!(n < 64, "{t}");
    assert!(!docker_ps().contains("elisa-run-"), "{}", docker_ps());
}

#[test]
#[ignore = "needs Colima and the images"]
fn colima_cuts_big_output() {
    let mut s = colima();
    let (t, _) = s.run(json!({"language": "python", "code": "import sys\nwhile True: sys.stdout.write('x'*1000+'\\n')"}));
    eprintln!("big output: {}", exit_line(&t));
    assert!(t.contains("more than 1 MB of output") && t.contains("[… stdout cut at 4000 characters"), "{}", &t[..t.len().min(300)]);
    assert!(t.len() < 6000, "{}", t.len());
    let (t, _) = s.run(json!({"language": "bash", "code": "head -c 200000 /dev/zero | tr '\\0' y"}));
    assert!(t.starts_with("exit 0") && t.contains("200000 bytes in all"), "{}", &t[..t.len().min(300)]);
}

#[test]
#[ignore = "needs Colima and the images"]
fn colima_has_no_network_no_host_files_and_no_root() {
    let mut s = colima();
    let (t, _) = s.run(json!({"language": "python", "code": "import socket\ntry:\n    socket.create_connection(('1.1.1.1', 80), timeout=3); print('CONNECTED')\nexcept OSError as e: print('blocked', e)\ntry:\n    socket.getaddrinfo('example.com', 80); print('RESOLVED')\nexcept OSError as e: print('no dns', e)"}));
    assert!(t.contains("blocked") && t.contains("no dns") && !t.contains("CONNECTED") && !t.contains("RESOLVED"), "{t}");
    let (t, _) = s.run(json!({"language": "bash", "code": "id -u; ip -o link 2>/dev/null | wc -l; touch /x 2>&1 || echo ro-root; ls /Users /home 2>&1; cat /proc/self/status | grep CapEff; grep -c . /proc/mounts"}));
    eprintln!("isolation: {t}");
    assert!(t.contains("stdout:\n65534") && t.contains("ro-root") && t.contains("CapEff:\t0000000000000000") && !t.contains("Development"), "{t}");
    let (t, _) = s.run(json!({"language": "javascript", "code": "require('https').get('https://example.com', r => console.log('GOT', r.statusCode)).on('error', e => console.log('net error', e.code))"}));
    assert!(t.contains("net error") && !t.contains("GOT"), "{t}");
    // memory is capped at 256 MB
    let (t, _) = s.run(json!({"language": "python", "code": "b = bytearray(400 * 1024 * 1024); print('ALLOCATED')"}));
    assert!(!t.contains("ALLOCATED"), "{t}");
    // /tmp is 64 MB
    let (t, _) = s.run(json!({"language": "bash", "code": "head -c 100000000 /dev/zero > /tmp/big; echo rc=$?; du -m /tmp/big"}));
    assert!(t.contains("rc=1") && t.contains("64\t/tmp/big"), "{t}");
}

fn colima_with(timeout_s: u64) -> Server {
    let home = std::env::var("HOME").unwrap_or_default();
    let host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| format!("unix://{home}/.colima/default/docker.sock"));
    Server::start(timeout_s, &[("DOCKER_HOST", host)])
}

fn docker_ps() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| format!("unix://{home}/.colima/default/docker.sock"));
    let o = Command::new("docker").args(["ps", "-a", "--format", "{{.Names}}"]).env("DOCKER_HOST", host).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn the_sandbox_is_off_by_default_and_a_project_can_only_switch_it_off() {
    use elisa::config::Config;
    let d = scratch("config");
    let (home, proj) = (d.join("home"), d.join("proj"));
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    let load = || Config::load_from(&proj, Some(home.clone()), |_| None).unwrap();
    let c = load();
    assert_eq!((c.sandbox.as_str(), c.sandbox_timeout_s), ("off", 10));
    // a project cannot switch it on, nor lengthen the limit
    std::fs::write(proj.join(".elisa/config.toml"), "sandbox = \"colima\"\nsandbox_timeout = 30\n").unwrap();
    let c = load();
    assert_eq!(c.sandbox, "off");
    assert_eq!(c.sandbox_timeout_s, 10);
    assert!(c.notes.iter().any(|n| n.contains("a project config can only switch the sandbox off")), "{:?}", c.notes);
    assert!(c.notes.iter().any(|n| n.contains("sandbox_timeout = 30 ignored")), "{:?}", c.notes);
    // the user's config switches it on; a project may switch it off or shorten the limit
    std::fs::write(home.join(".config/elisa/config.toml"), "sandbox = \"colima\"\nsandbox_timeout = 20\n").unwrap();
    std::fs::write(proj.join(".elisa/config.toml"), "sandbox_timeout = 5\n").unwrap();
    let c = load();
    assert_eq!((c.sandbox.as_str(), c.sandbox_timeout_s), ("colima", 5));
    std::fs::write(proj.join(".elisa/config.toml"), "sandbox = \"off\"\n").unwrap();
    assert_eq!(load().sandbox, "off");
    // a bad value is off with a note; the limit stays within 1 ..= 30
    std::fs::remove_file(proj.join(".elisa/config.toml")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), "sandbox = \"docker\"\n").unwrap();
    let c = load();
    assert!(c.sandbox == "off" && c.notes.iter().any(|n| n.contains("sandbox = \"docker\" is not a sandbox")), "{:?}", c.notes);
    std::fs::write(home.join(".config/elisa/config.toml"), "sandbox = \"colima\"\nsandbox_timeout = 300\n").unwrap();
    assert_eq!(load().sandbox_timeout_s, 30);
    // the fetch limit for deps: 60 s by default, 5 ..= 80, a project may only lower it
    assert_eq!(load().sandbox_fetch_timeout_s, 60);
    std::fs::write(home.join(".config/elisa/config.toml"), "sandbox = \"colima\"\nsandbox_fetch_timeout = 500\n").unwrap();
    let c = load();
    assert!(c.sandbox_fetch_timeout_s == 80 && c.notes.iter().any(|n| n.contains("sandbox_fetch_timeout = 500 out of range")), "{:?}", c.notes);
    std::fs::write(home.join(".config/elisa/config.toml"), "sandbox = \"colima\"\nsandbox_fetch_timeout = 40\n").unwrap();
    std::fs::write(proj.join(".elisa/config.toml"), "sandbox_fetch_timeout = 70\n").unwrap();
    let c = load();
    assert!(c.sandbox_fetch_timeout_s == 40 && c.notes.iter().any(|n| n.contains("sandbox_fetch_timeout = 70 ignored")), "{:?}", c.notes);
    std::fs::write(proj.join(".elisa/config.toml"), "sandbox_fetch_timeout = 20\n").unwrap();
    assert_eq!(load().sandbox_fetch_timeout_s, 20);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_approval_line_shows_the_language_and_first_line() {
    let s = elisa::tools::summary("run_code", &json!({"language": "python", "code": "\nimport math\nprint(math.pi)\n"}));
    assert_eq!(s, "run_code python: import math (+1 lines)");
    let c = elisa::mcp_host::builtin_run(7, 45);
    assert_eq!((c.name.as_str(), &c.command[1..]), ("run", &["--mcp-serve".to_string(), "run".into(), "--timeout".into(), "7".into(), "--fetch-timeout".into(), "45".into()][..]));
    // deps are named on the approval line, with the fact that they are fetched online
    let s = elisa::tools::summary("run_code", &json!({"language": "python", "code": "import six\nprint(six.__version__)", "deps": ["six==1.16.0"]}));
    assert_eq!(s, "run_code python: import six (+1 lines) · fetches online: six==1.16.0");
}

/// removes the test's cache volumes when dropped (also when an assertion failed)
struct Volumes(String);

impl Drop for Volumes {
    fn drop(&mut self) {
        for l in ["python", "javascript", "rust", "go"] {
            let _ = docker(&["volume", "rm", "-f", &format!("{}{l}", self.0)]);
        }
    }
}

fn docker(args: &[&str]) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| format!("unix://{home}/.colima/default/docker.sock"));
    let o = Command::new("docker").args(args).env("DOCKER_HOST", host).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
#[ignore = "needs Colima, the images and the internet"]
fn colima_fetches_deps_then_runs_offline() {
    let prefix = format!("elisa-test-{}-", std::process::id());
    let _cleanup = Volumes(prefix.clone());
    let home = std::env::var("HOME").unwrap_or_default();
    let host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| format!("unix://{home}/.colima/default/docker.sock"));
    let mut s = Server::start_with(30, 80, &[("DOCKER_HOST", host), ("ELISA_SANDBOX_VOLUME_PREFIX", prefix.clone())]);
    let mut cases = vec![
        ("python", "import six\nprint(six.__version__)", json!(["six==1.16.0"]), "1.16.0"),
        ("javascript", "console.log(require('left-pad')('7', 3, '0'))", json!(["left-pad@1.3.0"]), "007"),
        ("rust", "fn main() { let mut b = itoa::Buffer::new(); println!(\"{}\", b.format(42)); }", json!(["itoa@1.0.11"]), "42"),
    ];
    if docker(&["images", "-q", "golang:1.25-alpine"]).trim().is_empty() {
        eprintln!("golang:1.25-alpine is not pulled: go deps skipped");
    } else {
        cases.push(("go", "package main\nimport (\"fmt\"; \"golang.org/x/text/language\")\nfunc main() { fmt.Println(language.English) }", json!(["golang.org/x/text@v0.14.0"]), "en"));
    }
    for (lang, code, deps, want) in cases {
        let (t, err) = s.run(json!({"language": lang, "code": code, "deps": deps}));
        eprintln!("{lang} first: {}", exit_line(&t));
        assert!(!err && t.contains("deps fetched in") && t.contains(&format!("stdout:\n{want}")), "{lang}: {t}");
        // the second time the set comes from the cache
        let (t, err) = s.run(json!({"language": lang, "code": code, "deps": deps}));
        eprintln!("{lang} again: {}", exit_line(&t));
        assert!(!err && t.contains("deps cached") && t.contains(&format!("stdout:\n{want}")), "{lang}: {t}");
    }
    // with deps the code still has no network, and the cache is read-only
    let (t, _) = s.run(json!({"language": "python", "deps": ["six==1.16.0"], "code": "import socket\ntry:\n    socket.create_connection(('1.1.1.1', 80), timeout=3); print('CONNECTED')\nexcept OSError as e: print('blocked', e)\ntry:\n    open('/deps/x', 'w'); print('WROTE')\nexcept OSError as e: print('ro', e)"}));
    assert!(t.contains("blocked") && t.contains("ro") && !t.contains("CONNECTED") && !t.contains("WROTE"), "{t}");
    // a version that does not exist: the registry's own words, and the code never ran
    let (t, err) = s.run(json!({"language": "python", "code": "print('RAN')", "deps": ["six==99.0.0"]}));
    assert!(err && t.contains("failed") && t.contains("No matching distribution") && !t.contains("RAN"), "{t}");
    assert!(!docker_ps().contains("elisa-fetch-") && !docker_ps().contains("elisa-run-"), "{}", docker_ps());
}

#[test]
#[ignore = "needs Colima and the golang image"]
fn colima_runs_go() {
    if docker(&["images", "-q", "golang:1.25-alpine"]).trim().is_empty() { eprintln!("golang:1.25-alpine is not pulled: skipped"); return; }
    let mut s = colima_with(30);
    let (t, err) = s.run(json!({"language": "go", "code": "package main\nimport \"fmt\"\nfunc main() { fmt.Println(6 * 7) }"}));
    assert!(!err && t.contains("stdout:\n42"), "{t}");
}
