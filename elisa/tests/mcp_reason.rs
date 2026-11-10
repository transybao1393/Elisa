//! The built-in reasoning server (`elisa --mcp-serve reason`) through the MCP host, as a session uses it: bare tool
//! names, no approval needed, per-turn caps reset by the host's turn notification, step state across turns; and the
//! host's clean-up of a small model's placeholder arguments against a strict server's schema.
use elisa::mcp_host::{Host, ServerCfg};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

fn reason() -> ServerCfg {
    ServerCfg { name: "reason".into(), command: vec![env!("CARGO_BIN_EXE_elisa").into(), "--mcp-serve".into(), "reason".into()], ..ServerCfg::default() }
}

fn call(h: &mut Host, name: &str, args: Value) -> (String, bool) {
    let o = h.call(name, &args, &AtomicBool::new(false), 6000);
    (o.content, o.is_error)
}

#[test]
fn the_reasoning_tools_keep_their_names_and_caps_reset_per_turn() {
    let elisa_tools = ["read", "bash", "edit"];
    let (mut h, notes) = Host::start(&[reason()], &|n| elisa_tools.contains(&n));
    assert!(notes.is_empty(), "{notes:?}");
    let names: Vec<String> = h.defs().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["think", "steps", "step_done", "calc"]);
    assert!(h.reason_tool("think") && h.reason_tool("calc") && !h.reason_tool("read"));

    // the size the four definitions add to every task turn's request (Elisa's estimate: chars / 3.3)
    let toks = elisa::context::estimate_tokens(&serde_json::to_string(&h.defs()).unwrap());
    assert!(toks <= 250, "{toks} tokens");

    assert_eq!(call(&mut h, "calc", json!({"expr": "(17*23)/4"})), ("97.75".into(), false));
    assert!(call(&mut h, "calc", json!({"expr": "1/0"})).1);
    let (t, e) = call(&mut h, "steps", json!({"list": "read the code; fix it; run the tests"}));
    assert!(!e && t.contains("Next: 1) read the code"), "{t}");
    // n as a string or a number
    assert_eq!(call(&mut h, "step_done", json!({"n": "1"})).0, "Done 1/3. Next: 2) fix it");
    assert_eq!(call(&mut h, "step_done", json!({"n": 2})).0, "Done 2/3. Next: 3) run the tests");
    assert!(call(&mut h, "steps", json!({"list": "a; b"})).0.starts_with("Enough planning"));
    for _ in 0..3 { assert!(call(&mut h, "think", json!({"thought": "x"})).0.starts_with("Noted")); }
    assert_eq!(call(&mut h, "think", json!({"thought": "x"})), ("Enough planning — act now.".into(), false));

    // a new user turn: the caps start again, the step list stays
    h.new_turn();
    assert!(call(&mut h, "think", json!({"thought": "y"})).0.starts_with("Noted"));
    assert_eq!(call(&mut h, "step_done", json!({"n": 3})).0, "Done 3/3. All steps done: check the result and answer.");
}

/// A strict server like @modelcontextprotocol/server-sequential-thinking: refuses `revisesThought` / `branchFromThought`
/// below 1 and an empty `branchId`, else echoes the arguments it got.
const STRICT: &str = r#"
import json, sys
int1 = {"type": "integer", "minimum": 1, "maximum": 9007199254740991}
bs = {"type": ["boolean", "string"]}
schema = {"type": "object", "properties": {"thought": {"type": "string"}, "nextThoughtNeeded": bs, "thoughtNumber": int1,
  "totalThoughts": int1, "isRevision": bs, "revisesThought": int1, "branchFromThought": int1, "branchId": {"type": "string"},
  "needsMoreThoughts": bs}, "required": ["thought", "nextThoughtNeeded", "thoughtNumber", "totalThoughts"]}
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth, i = m.get("method"), m["id"]
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "strict", "version": "1"}}})
    elif meth == "tools/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"tools": [{"name": "sequentialthinking", "description": "think", "inputSchema": schema}]}})
    elif meth == "tools/call":
        a = m["params"]["arguments"]
        bad = [k for k in ("revisesThought", "branchFromThought") if k in a and a[k] < 1] + (["branchId"] if a.get("branchId") == "" else [])
        if bad:
            send({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": "Too small: " + ",".join(bad)}], "isError": True}})
        else:
            send({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": json.dumps(a, sort_keys=True)}]}})
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "no"}})
"#;

#[test]
fn placeholder_arguments_are_dropped_before_a_strict_server_sees_them() {
    let d = std::env::temp_dir().join(format!("elisa-mcp-reason-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let script = d.join("strict.py");
    std::fs::write(&script, STRICT).unwrap();
    let cfg = ServerCfg { name: "think".into(), command: vec!["python3".into(), script.display().to_string()], ..ServerCfg::default() };
    let (mut h, notes) = Host::start(&[cfg], &|_| false);
    assert!(notes.is_empty(), "{notes:?}");
    assert!(!h.reason_tool("think_sequentialthinking"), "a user server is never the built-in one");
    // a small model's call that was rejected before
    let (t, e) = call(&mut h, "think_sequentialthinking", json!({"thought": "list the files", "nextThoughtNeeded": true,
        "thoughtNumber": 1, "totalThoughts": 3, "isRevision": false, "revisesThought": 0, "branchFromThought": 0, "branchId": "", "needsMoreThoughts": false}));
    assert!(!e, "{t}");
    let got: Value = serde_json::from_str(&t).unwrap();
    assert_eq!(got, json!({"thought": "list the files", "nextThoughtNeeded": true, "thoughtNumber": 1, "totalThoughts": 3,
        "isRevision": false, "needsMoreThoughts": false}));
    // a valid optional value goes through untouched
    let (t, e) = call(&mut h, "think_sequentialthinking", json!({"thought": "t", "nextThoughtNeeded": false, "thoughtNumber": 2, "totalThoughts": 2, "revisesThought": 1}));
    assert!(!e && t.contains("\"revisesThought\": 1"), "{t}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_reason_setting_project_may_only_switch_it_off() {
    let d = std::env::temp_dir().join(format!("elisa-reason-cfg-{}", std::process::id()));
    let (home, proj) = (d.join("home"), d.join("proj"));
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    let load = |user: &str, project: &str| {
        std::fs::write(home.join(".config/elisa/config.toml"), user).unwrap();
        std::fs::write(proj.join(".elisa/config.toml"), project).unwrap();
        let c = elisa::config::Config::load_from(&proj, Some(home.clone()), |_| None).unwrap();
        (c.reason, c.notes.into_iter().filter(|n| n.contains("reason")).count())
    };
    assert_eq!(load("", ""), ("auto".into(), 0));
    assert_eq!(load("reason = \"always\"\n", ""), ("always".into(), 0));
    assert_eq!(load("reason = \"sometimes\"\n", ""), ("auto".into(), 1));
    assert_eq!(load("", "reason = \"off\"\n"), ("off".into(), 0));
    assert_eq!(load("", "reason = \"always\"\n"), ("auto".into(), 1));
    assert_eq!(load("reason = \"off\"\n", "reason = \"auto\"\n"), ("off".into(), 1));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn think_simple_defaults_off_and_a_project_may_only_set_it_off() {
    let d = std::env::temp_dir().join(format!("elisa-think-simple-cfg-{}", std::process::id()));
    let (home, proj) = (d.join("home"), d.join("proj"));
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    let load = |user: &str, project: &str| {
        std::fs::write(home.join(".config/elisa/config.toml"), user).unwrap();
        std::fs::write(proj.join(".elisa/config.toml"), project).unwrap();
        let c = elisa::config::Config::load_from(&proj, Some(home.clone()), |_| None).unwrap();
        (c.think_simple, c.notes.into_iter().filter(|n| n.contains("think_simple")).count())
    };
    assert_eq!(load("", ""), ("off".into(), 0));
    assert_eq!(load("think_simple = \"on\"\n", ""), ("on".into(), 0));
    assert_eq!(load("think_simple = \"maybe\"\n", ""), ("off".into(), 1));
    assert_eq!(load("think_simple = \"on\"\n", "think_simple = \"off\"\n"), ("off".into(), 0));
    assert_eq!(load("", "think_simple = \"on\"\n"), ("off".into(), 1));
    let _ = std::fs::remove_dir_all(&d);
}

/// Live check against the real @modelcontextprotocol/server-sequential-thinking (ignored by default; needs node and the
/// server installed): `ELISA_SEQ_NODE=… ELISA_SEQ_JS=… cargo test --test mcp_reason -- --ignored`.
#[test]
#[ignore]
fn the_real_sequential_thinking_server_accepts_the_cleaned_call() {
    let (Ok(node), Ok(js)) = (std::env::var("ELISA_SEQ_NODE"), std::env::var("ELISA_SEQ_JS")) else { return };
    let cfg = ServerCfg { name: "think".into(), command: vec![node, js], ..ServerCfg::default() };
    let (mut h, notes) = Host::start(&[cfg], &|_| false);
    assert!(notes.is_empty(), "{notes:?}");
    let (t, e) = call(&mut h, "think_sequentialthinking", json!({"thought": "list the files", "nextThoughtNeeded": true,
        "thoughtNumber": 1, "totalThoughts": 3, "isRevision": false, "revisesThought": 0, "branchFromThought": 0, "branchId": "", "needsMoreThoughts": false}));
    eprintln!("{t}");
    assert!(!e, "{t}");
}
