//! The MCP host against small fake MCP servers (Python 3 scripts written into a temp dir): tool names that may not
//! shadow Elisa's own, parallel start with a server that never answers, and restart after a crash.
use elisa::mcp_host::{Host, ServerCfg};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// A stdio MCP server. Args: TOOLS (comma-separated names) MODE DIR.
/// MODE `ok`: answers. `hang`: writes its pid to DIR/hang.pid and never answers. `crash-once`: refuses to start (exits
/// at once) when DIR/crashed exists. Tool `crash` exits without answering (and, with crash-once, leaves DIR/crashed).
/// Tool `sleep` takes 30 s. Any other tool answers `NAME: pid PID`.
const SERVER: &str = r#"
import json, os, sys, time
tools, mode, d = sys.argv[1].split(","), sys.argv[2], sys.argv[3]
if mode == "hang":
    open(os.path.join(d, "hang.pid"), "w").write(str(os.getpid()))
    for line in sys.stdin:
        pass
    sys.exit(0)
if mode == "crash-once" and os.path.exists(os.path.join(d, "crashed")):
    sys.stderr.write("starting\nfatal: cannot open db (token " + "ghp_" + "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5)\n"); sys.stderr.flush()
    sys.exit(3)
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth, i = m.get("method"), m["id"]
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}})
    elif meth == "tools/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"tools": [{"name": t, "description": "fake " + t, "inputSchema": {"type": "object", "properties": {}}} for t in tools]}})
    elif meth == "ping":
        send({"jsonrpc": "2.0", "id": i, "result": {}})
    elif meth == "tools/call":
        name = m["params"]["name"]
        if name == "crash":
            if mode == "crash-once":
                open(os.path.join(d, "crashed"), "w").write("1")
            sys.exit(1)
        if name == "sleep":
            time.sleep(30)
        send({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": name + ": pid " + str(os.getpid())}]}})
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "no"}})
"#;

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn dir(tag: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("elisa-mcp-host-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("server.py"), SERVER).unwrap();
    Dir(d)
}

fn server(d: &Path, name: &str, tools: &str, mode: &str) -> ServerCfg {
    let script = d.join("server.py").display().to_string();
    ServerCfg { name: name.into(), command: vec!["python3".into(), script, tools.into(), mode.into(), d.display().to_string()], ..ServerCfg::default() }
}

const ELISA: [&str; 4] = ["read", "read_symbol", "bash", "edit"];
fn taken(n: &str) -> bool { ELISA.contains(&n) }

fn call(h: &mut Host, name: &str) -> (String, bool) {
    let o = h.call(name, &json!({}), &AtomicBool::new(false), 10_000);
    (o.content, o.is_error)
}

fn names(h: &Host) -> Vec<String> { h.defs().into_iter().map(|d| d.name).collect() }

#[test]
fn mcp_tools_never_take_elisas_names_or_each_others() {
    let d = dir("names");
    let cfgs = [
        server(&d.0, "read", "symbol,other", "ok"),
        server(&d.0, "fs", "read,read", "ok"),
        server(&d.0, "a", "b_c", "ok"),
        server(&d.0, "a_b", "c", "ok"),
    ];
    let (mut h, notes) = Host::start_with_timeout(&cfgs, &taken, Duration::from_secs(5));
    assert!(notes.is_empty(), "{notes:?}");
    let n = names(&h);
    assert_eq!(n, ["mcp_read_symbol", "read_other", "fs_read", "a_b_c", "mcp_a_b_c"]);
    for e in ELISA { assert!(!h.owns(e), "claims Elisa's {e}"); }
    let mut seen = std::collections::HashSet::new();
    assert!(n.iter().all(|x| seen.insert(x.clone())), "duplicate names: {n:?}");
    assert_eq!(h.server_of("mcp_read_symbol"), Some("read"));
    assert_eq!(call(&mut h, "mcp_read_symbol").0.split(':').next(), Some("symbol"));
    assert_eq!(h.server_of("mcp_a_b_c"), Some("a_b"));
    assert!(call(&mut h, "mcp_a_b_c").0.starts_with("c: pid"));
    assert!(call(&mut h, "a_b_c").0.starts_with("b_c: pid"));
    assert!(call(&mut h, "read_symbol").1, "Elisa's read_symbol must not be routed to MCP");
}

#[test]
fn servers_start_in_parallel_and_one_that_never_answers_is_left_out() {
    let d = dir("parallel");
    let cfgs = [server(&d.0, "slow", "x", "hang"), server(&d.0, "slow2", "y", "hang"), server(&d.0, "good", "good_tool", "ok")];
    let limit = Duration::from_millis(1500);
    let t0 = Instant::now();
    let (mut h, notes) = Host::start_with_timeout(&cfgs, &taken, limit);
    let took = t0.elapsed();
    assert!(took < limit * 2, "start took {took:?}");
    assert_eq!(names(&h), ["good_tool"]);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[0].starts_with("mcp slow: not started:"), "{notes:?}");
    assert!(notes[1].starts_with("mcp slow2: not started:"), "{notes:?}");
    assert!(call(&mut h, "good_tool").0.starts_with("good_tool: pid"));
    // the hung server's process was killed
    let pid = std::fs::read_to_string(d.0.join("hang.pid")).unwrap();
    let mut alive = true;
    for _ in 0..20 {
        alive = std::process::Command::new("kill").args(["-0", pid.trim()]).stderr(std::process::Stdio::null()).status().unwrap().success();
        if !alive { break; }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive, "the timed-out server (pid {pid}) is still running");
}

#[test]
fn a_crashed_server_is_restarted_on_the_next_call() {
    let d = dir("restart");
    let (mut h, notes) = Host::start_with_timeout(&[server(&d.0, "s", "echo,crash", "ok")], &taken, Duration::from_secs(5));
    assert!(notes.is_empty(), "{notes:?}");
    let (first, _) = call(&mut h, "s_echo");
    let (msg, err) = call(&mut h, "s_crash");
    assert!(err && msg.starts_with("MCP server s stopped: "), "{msg}");
    let (again, err) = call(&mut h, "s_echo");
    assert!(!err && again.starts_with("echo: pid "), "{again}");
    assert_ne!(first, again, "should be a new process");
    let (msg, _) = call(&mut h, "s_crash");
    assert!(msg.contains("stopped"), "{msg}");
    assert!(call(&mut h, "s_echo").0.starts_with("echo: pid "));
}

#[test]
fn a_server_that_cannot_restart_says_so() {
    let d = dir("norestart");
    let (mut h, _) = Host::start_with_timeout(&[server(&d.0, "s", "echo,crash", "crash-once")], &taken, Duration::from_secs(5));
    assert!(call(&mut h, "s_crash").0.starts_with("MCP server s stopped: "));
    for _ in 0..2 {
        let (msg, err) = call(&mut h, "s_echo");
        assert!(err, "{msg}");
        assert!(msg.starts_with("MCP server s stopped and could not be restarted: "), "{msg}");
        assert!(msg.contains("(exit code 3): fatal: cannot open db"), "{msg}");
        assert!(msg.ends_with("; restart the session to try again"), "{msg}");
    }
}

/// QA 2026-11-08 (B6): "not started: the MCP server exited" said nothing more; now the exit code and the last
/// stderr line (secrets redacted)
#[test]
fn a_server_that_exits_at_start_says_how_and_why() {
    let d = dir("exitnote");
    std::fs::write(d.0.join("crashed"), "1").unwrap();
    let (h, notes) = Host::start_with_timeout(&[server(&d.0, "crashy", "echo", "crash-once")], &taken, Duration::from_secs(5));
    assert!(names(&h).is_empty());
    assert_eq!(notes.len(), 1, "{notes:?}");
    let n = &notes[0];
    assert!(n.starts_with("mcp crashy: not started: the MCP server exited (exit code 3): fatal: cannot open db (token [redacted:github-token])"), "{n}");
    assert!(!n.contains("ghp_"), "{n}");
}

#[test]
fn a_server_still_busy_with_a_cancelled_call_is_restarted() {
    let d = dir("cancel");
    let (mut h, _) = Host::start_with_timeout(&[server(&d.0, "s", "echo,sleep", "ok")], &taken, Duration::from_secs(5));
    let (first, _) = call(&mut h, "s_echo");
    let cancel = AtomicBool::new(false);
    std::thread::scope(|sc| {
        sc.spawn(|| { std::thread::sleep(Duration::from_millis(300)); cancel.store(true, std::sync::atomic::Ordering::Relaxed); });
        let o = h.call("s_sleep", &json!({}), &cancel, 1000);
        assert!(o.is_error && o.content.contains("cancelled"), "{}", o.content);
    });
    let t0 = Instant::now();
    let (again, err) = call(&mut h, "s_echo");
    assert!(!err && again.starts_with("echo: pid "), "{again}");
    assert_ne!(first, again, "should be a new process");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}
