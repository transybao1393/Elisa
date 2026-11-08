//! Plugins end to end with the real binary and a temp HOME (never the real ~/.elisa): `elisa plugin …` on a plugin
//! with a host-run fake MCP server (Python 3), then a `--serve-jsonl` session with the scripted model that uses its
//! server, rules and command. The container path (install in Colima, `--network none` run) is the ignored test at the
//! end: it needs Colima running and the node:22-alpine image.
use elisa::eval_fake::{FakeModel, Scripted};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A stdio MCP server: tools `greet` (answers `hello NAME`) and `shout`.
const SERVER: &str = r#"
import json, sys
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth, i = m.get("method"), m["id"]
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "notes", "version": "1"}}})
    elif meth == "tools/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"tools": [{"name": t, "description": t + " someone", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}} for t in ["greet", "shout"]]}})
    elif meth == "tools/call":
        a = m["params"].get("arguments", {})
        send({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": "hello " + str(a.get("name", "?"))}]}})
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "no"}})
"#;

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// root/{home, project, src-plugin}: the plugin folder holds the manifest, the server and one command
fn setup(tag: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("elisa-plugins-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["home/.config/elisa", "project", "src-plugin/commands"] { std::fs::create_dir_all(d.join(sub)).unwrap(); }
    let d = d.canonicalize().unwrap();
    std::fs::write(d.join("home/.config/elisa/config.toml"), "").unwrap();
    std::fs::write(d.join("src-plugin/server.py"), SERVER).unwrap();
    std::fs::write(d.join("src-plugin/plugin.toml"), manifest(&d, "1.0.0")).unwrap();
    std::fs::write(d.join("src-plugin/commands/hello.md"), "---\ndescription: Say hello\n---\nGreet $ARGS with the notes tools.\n").unwrap();
    Dir(d)
}

fn manifest(d: &Path, version: &str) -> String {
    format!(r#"[plugin]
name = "notes"
version = "{version}"
description = "Greets people"

[[server]]
name = "notes"
runs = "host"
command = ["python3", "{}"]
exclude = ["shout"]

[permissions]
allow = ["mcp:notes:greet(world)"]
deny = ["mcp:notes:greet(*secret*)"]
"#, d.join("src-plugin/server.py").display())
}

fn elisa(d: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_elisa")).args(args).current_dir(d.join("project")).env("HOME", d.join("home"))
        .stdin(Stdio::null()).output().unwrap();
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn audit(d: &Path) -> String {
    std::fs::read_dir(d.join("home/.elisa/audit")).into_iter().flatten().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap_or_default()).collect()
}

#[test]
fn the_plugin_cli_adds_lists_pins_switches_and_removes_a_plugin() {
    let d = setup("cli");
    let src = d.0.join("src-plugin").display().to_string();
    // the setup screen first; with no terminal and no --yes nothing is installed
    let (code, out) = elisa(&d.0, &["plugin", "add", &src]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("runs DIRECTLY ON THIS MAC") && out.contains("allow mcp:notes:greet(world)") && out.contains("/hello — Say hello"), "{out}");
    assert!(out.contains("add --yes"), "{out}");
    assert!(!d.0.join("home/.elisa/plugins/notes").exists());
    // approved: copied (manifest + commands only), pinned, its tools learnt
    let (code, out) = elisa(&d.0, &["plugin", "add", &src, "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("notes: 1 tool") && out.contains("installed and on"), "{out}");
    let inst = d.0.join("home/.elisa/plugins/notes");
    assert!(inst.join("plugin.toml").is_file() && inst.join("commands/hello.md").is_file() && !inst.join("server.py").exists());
    let (_, out) = elisa(&d.0, &["plugin", "list"]);
    assert!(out.contains("notes 1.0.0 · user · on") && out.contains("notes (host)") && out.contains("commands: /hello") && out.contains("2 rules"), "{out}");
    let (code, out) = elisa(&d.0, &["plugin", "add", &src, "--yes"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("already installed"), "{out}");
    // a changed installed file: off until trusted
    let m = inst.join("plugin.toml");
    std::fs::write(&m, std::fs::read_to_string(&m).unwrap().replace("Greets people", "Greets everyone")).unwrap();
    let (_, out) = elisa(&d.0, &["plugin", "list"]);
    assert!(out.contains("CHANGED since you approved it"), "{out}");
    let (code, out) = elisa(&d.0, &["plugin", "trust", "notes"]);
    assert_eq!(code, 1, "{out}");
    let (code, out) = elisa(&d.0, &["plugin", "trust", "notes", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let (_, out) = elisa(&d.0, &["plugin", "list"]);
    assert!(out.contains("notes 1.0.0 · user · on"), "{out}");
    // off and on
    assert_eq!(elisa(&d.0, &["plugin", "disable", "notes"]).0, 0);
    assert!(elisa(&d.0, &["plugin", "list"]).1.contains("· off"));
    assert_eq!(elisa(&d.0, &["plugin", "enable", "notes"]).0, 0);
    // an update reads the recorded folder again
    std::fs::write(d.0.join("src-plugin/plugin.toml"), manifest(&d.0, "1.1.0")).unwrap();
    let (code, out) = elisa(&d.0, &["plugin", "update", "notes", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Update plugin notes 1.0.0 → 1.1.0") && out.contains("updated to 1.1.0"), "{out}");
    // removed: folder and state gone
    let (code, out) = elisa(&d.0, &["plugin", "remove", "notes"]);
    assert_eq!(code, 0, "{out}");
    assert!(!inst.exists());
    assert!(elisa(&d.0, &["plugin", "list"]).1.contains("no plugins"));
    // every operation is in the audit log
    let a = audit(&d.0);
    for op in ["add notes", "trust notes", "disable notes", "enable notes", "update notes", "remove notes"] { assert!(a.contains(op), "{op} not audited: {a}"); }
    assert!(a.contains("\"declined\""), "the declined add is audited: {a}");
    // a running server has no network option
    std::fs::write(d.0.join("src-plugin/plugin.toml"), manifest(&d.0, "1.0.0").replace("exclude = [\"shout\"]", "network = true")).unwrap();
    let (code, out) = elisa(&d.0, &["plugin", "add", &src, "--yes"]);
    assert_eq!(code, 1);
    assert!(out.contains("no network option"), "{out}");
}

/// a `--serve-jsonl` session: events in, commands out
struct Sess { child: Child, stdin: ChildStdin, rx: Receiver<Value> }

impl Sess {
    fn start(d: &Path, model: &FakeModel) -> Sess {
        let mut child = Command::new(env!("CARGO_BIN_EXE_elisa"))
            .args(["--serve-jsonl", "--cwd"]).arg(d.join("project"))
            .args(["--base-url", &model.url(), "--type", "streamdense", "--model", "eval-fake"])
            .current_dir(d.join("project")).env("HOME", d.join("home"))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || for l in BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) { if tx.send(v).is_err() { break; } }
        });
        Sess { child, stdin, rx }
    }
    fn cmd(&mut self, v: Value) { writeln!(self.stdin, "{v}").unwrap(); }
    /// events up to and including the first of type `kind`; approvals are answered with `answers` (then "no")
    fn until(&mut self, kind: &str, answers: &mut Vec<&str>) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut evs = Vec::new();
        loop {
            let e = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap_or_else(|_| panic!("no {kind}: {evs:?}"));
            if e["type"] == "approve" {
                let a = if answers.is_empty() { "no" } else { answers.remove(0) };
                self.cmd(json!({"type": "approve", "answer": a}));
            }
            let end = e["type"] == kind;
            evs.push(e);
            if end { return evs; }
        }
    }
    fn turn(&mut self, text: &str, answers: &[&str]) -> Vec<Value> {
        self.cmd(json!({"type": "send", "text": text}));
        let mut a = answers.to_vec();
        let evs = self.until("turn_done", &mut a);
        self.until("ready", &mut Vec::new());
        evs
    }
}

impl Drop for Sess {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

fn tool(name: &str, input: Value) -> Scripted { Scripted { tools: vec![(name.into(), input)], ..Scripted::default() } }
fn text(t: &str) -> Scripted { Scripted { text: Some(t.into()), ..Scripted::default() } }
fn of<'a>(evs: &'a [Value], kind: &str) -> Vec<&'a Value> { evs.iter().filter(|e| e["type"] == kind).collect() }

#[test]
fn a_session_runs_the_plugin_server_under_its_rules_and_offers_its_command() {
    let d = setup("session");
    let (code, out) = elisa(&d.0, &["plugin", "add", &d.0.join("src-plugin").display().to_string(), "--yes"]);
    assert_eq!(code, 0, "{out}");
    let model = FakeModel::start(vec![
        tool("notes_greet", json!({"name": "world"})),
        tool("notes_greet", json!({"name": "top secret"})),
        tool("notes_greet", json!({"name": "bob"})),
        text("greeted"),
        text("hello there"),
        tool("notes_greet", json!({"name": "world"})),
        text("gone"),
    ], 32768).unwrap();
    let mut s = Sess::start(&d.0, &model);
    s.until("ready", &mut Vec::new());
    let evs = s.turn("greet people", &["no"]);
    let done: Vec<String> = of(&evs, "tool_done").iter().map(|e| e["output"].as_str().unwrap_or("").to_string()).collect();
    // allow rule: no prompt; deny rule: refused before any prompt; no rule: asks (answered no)
    assert!(done[0].contains("hello world"), "{done:?}");
    assert!(done[1].contains("denied by the rule deny mcp:notes:greet(*secret*) (plugin notes)"), "{done:?}");
    assert!(done[2].contains("denied by user"), "{done:?}");
    let asked = of(&evs, "approve");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["server"], "notes");
    assert_eq!(asked[0]["rule"], "mcp:notes:greet", "an MCP call is offered its tool's pattern");
    // a plugin server is third-party: its output reaches the model fenced
    let reqs = model.requests();
    assert!(reqs.iter().any(|r| r.to_string().contains("<untrusted source=")), "plugin output not fenced");
    // the filtered-out tool is not offered
    assert!(!reqs[0].to_string().contains("notes_shout"), "excluded tool offered");
    // the plugin's command: its body with $ARGS, as a normal turn, not fenced (the user installed it)
    let n0 = model.requests().len();
    s.turn("/hello there", &[]);
    let last = model.requests()[n0..].iter().map(Value::to_string).collect::<String>();
    assert!(last.contains("Greet there with the notes tools.") && !last.contains("project command"), "{last}");
    // /help lists it with its source; /plugin list sends the plugins event
    let evs = s.turn("/help", &[]);
    assert!(of(&evs, "info").iter().any(|e| e["text"].as_str().unwrap_or("").contains("/hello    Say hello  (plugin notes)")), "{evs:?}");
    let evs = s.turn("/plugin list", &[]);
    let p = of(&evs, "plugins");
    assert_eq!(p.len(), 1, "{evs:?}");
    let pl = &p[0]["plugins"][0];
    assert_eq!((pl["name"].as_str(), pl["source"].as_str(), pl["enabled"].as_bool(), pl["trusted"].as_bool()), (Some("notes"), Some("user"), Some(true), Some(true)));
    assert_eq!((pl["servers"][0]["runs"].as_str(), pl["servers"][0]["tools"].as_u64(), pl["rules"].as_u64()), (Some("host"), Some(1), Some(2)));
    assert!(pl["tokens"].as_u64().unwrap() > 0);
    // the app's request form: disable through a `plugin` command; the server stops and its tool is unknown
    s.cmd(json!({"type": "plugin", "op": "disable", "name": "notes"}));
    let evs = s.until("turn_done", &mut Vec::new());
    s.until("ready", &mut Vec::new());
    let r = of(&evs, "plugin_result");
    assert_eq!((r[0]["op"].as_str(), r[0]["ok"].as_bool()), (Some("disable"), Some(true)), "{evs:?}");
    let evs = s.turn("greet again", &[]);
    let done: Vec<String> = of(&evs, "tool_done").iter().map(|e| e["output"].as_str().unwrap_or("").to_string()).collect();
    assert!(!done[0].contains("hello world"), "a disabled plugin's tool ran: {done:?}");
    assert!(of(&evs, "approve").is_empty());
    let a = audit(&d.0);
    assert!(a.contains("disable notes") && a.contains("\"denial\""), "{a}");
}

#[test]
fn plugin_add_in_a_session_shows_the_setup_asks_and_brings_the_server_in() {
    let d = setup("session-add");
    let src = d.0.join("src-plugin").display().to_string();
    let model = FakeModel::start(vec![tool("notes_greet", json!({"name": "world"})), text("hi")], 32768).unwrap();
    let mut s = Sess::start(&d.0, &model);
    s.until("ready", &mut Vec::new());
    // declined: nothing installed
    let evs = s.turn(&format!("/plugin add {src}"), &["no"]);
    let prev = of(&evs, "plugin_preview");
    assert_eq!(prev.len(), 1, "{evs:?}");
    assert_eq!((prev[0]["preview"]["host"].as_bool(), prev[0]["preview"]["servers"][0]["runs"].as_str()), (Some(true), Some("host")));
    assert_eq!(prev[0]["preview"]["rules"][0]["pattern"], "mcp:notes:greet(world)");
    let q = of(&evs, "approve");
    assert_eq!((q.len(), q[0]["question"].as_bool()), (1, Some(true)), "{q:?}");
    let r = of(&evs, "plugin_result");
    assert_eq!(r[0]["ok"], false);
    assert!(!d.0.join("home/.elisa/plugins/notes").exists());
    // approved through the app's request form: installed, its server started in this session, its tool offered
    s.cmd(json!({"type": "plugin", "op": "add", "path": src}));
    let evs = s.until("turn_done", &mut vec!["yes"]);
    s.until("ready", &mut Vec::new());
    let r = of(&evs, "plugin_result");
    assert_eq!((r[0]["op"].as_str(), r[0]["ok"].as_bool()), (Some("add"), Some(true)), "{evs:?}");
    assert!(of(&evs, "plugins").iter().any(|e| e["plugins"][0]["servers"][0]["running"] == true), "{evs:?}");
    let evs = s.turn("greet the world", &[]);
    assert!(of(&evs, "tool_done")[0]["output"].as_str().unwrap().contains("hello world"), "{evs:?}");
    assert!(of(&evs, "approve").is_empty(), "the plugin's allow rule covers it");
}

/// Real install and run inside Colima: `@modelcontextprotocol/server-sequential-thinking` pinned, node:22-alpine
/// already pulled, then a session calls its tool (an `ask` rule from the plugin: asked even though it is allowed once).
/// Run with Colima up: `DOCKER_HOST=unix://$HOME/.colima/default/docker.sock cargo test --release --test plugins -- --ignored`
/// (a temp HOME has no Colima socket of its own, so DOCKER_HOST is required). No mounts: Colima shares only the home
/// folder, and the temp project lives elsewhere.
#[test]
#[ignore]
fn colima_installs_and_runs_a_pinned_npm_server_without_network() {
    assert!(std::env::var_os("DOCKER_HOST").is_some(), "set DOCKER_HOST to Colima's socket");
    let d = setup("colima");
    let dir = d.0.join("seq");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), r#"[plugin]
name = "seqthink"
version = "1.0.0"
description = "Sequential thinking, in a container"

[[server]]
name = "seqthink"
installer = "npm"
package = "@modelcontextprotocol/server-sequential-thinking"
version = "2025.12.18"
command = ["mcp-server-sequential-thinking"]
env = { DISABLE_THOUGHT_LOGGING = "true" }

[permissions]
ask = ["mcp:seqthink"]
"#).unwrap();
    let (code, out) = elisa(&d.0, &["plugin", "add", &dir.display().to_string(), "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("image node:22-alpine (on this machine)") && out.contains("seqthink: 1 tool"), "{out}");
    let lock: Value = serde_json::from_str(&std::fs::read_to_string(d.0.join("home/.elisa/plugins/seqthink/plugin.lock")).unwrap()).unwrap();
    assert_eq!(lock["servers"][0]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(lock["servers"][0]["tools"], 1);
    let model = FakeModel::start(vec![
        tool("seqthink_sequentialthinking", json!({"thought": "first", "thoughtNumber": 1, "totalThoughts": 1, "nextThoughtNeeded": false})),
        text("thought"),
    ], 32768).unwrap();
    {
        let mut s = Sess::start(&d.0, &model);
        s.until("ready", &mut Vec::new());
        let evs = s.turn("think once", &["yes"]);
        let asked = of(&evs, "approve");
        assert_eq!(asked.len(), 1, "{evs:?}");
        assert!(asked[0]["summary"].as_str().unwrap().contains("asked by the rule ask mcp:seqthink"), "{asked:?}");
        let done = of(&evs, "tool_done");
        assert!(done[0]["output"].as_str().unwrap().contains("thoughtNumber"), "{done:?}");
    }
    // the session is gone: so is its container
    std::thread::sleep(Duration::from_secs(3));
    let ps = Command::new("docker").args(["ps", "-q", "--filter", "label=elisa.plugin=seqthink"]).output().unwrap();
    assert!(String::from_utf8_lossy(&ps.stdout).trim().is_empty(), "a plugin container outlived its session");
    let (code, out) = elisa(&d.0, &["plugin", "remove", "seqthink"]);
    assert_eq!(code, 0, "{out}");
    let tag = elisa::plugins::home_tag(&d.0.join("home"));
    let v = Command::new("docker").args(["volume", "ls", "-q", "--filter", &format!("name=elisa-plugin-seqthink-seqthink-{tag}")]).output().unwrap();
    assert!(String::from_utf8_lossy(&v.stdout).trim().is_empty(), "the package volume was left behind");
}
