//! Model connections (`src/provider.rs`): which config wins, where keys may go, and each transport end to end against
//! a throwaway HTTP server or a real stdio MCP server (a small sh script).
use elisa::api::{Client, Request};
use elisa::config::Config;
use elisa::provider::{find_model, from_json, path_in, test as probe, use_model, Profile, Providers, Transport};
use elisa::types::{Block, Message, Role, ToolDef};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

static N: AtomicUsize = AtomicUsize::new(0);

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("elisa-providers-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_providers(home: &Path, text: &str) {
    let p = path_in(home);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const TWO: &str = r#"
active = "cloud"
[providers.cloud]
type = "openai"
base_url = "https://api.example.com/v1/"
model = "big"
api_key_env = "CLOUD_KEY"
max_ctx = 100000
[providers.box]
type = "streamdense"
base_url = "http://127.0.0.1:8898"
model = "qwen38-27b"
"#;

fn load(cwd: &Path, home: &Path, env: &[(&str, &str)], forced: Option<&str>) -> Result<Config, String> {
    let env: Vec<(String, String)> = env.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    Config::load_from_as(cwd, Some(home.to_path_buf()), move |k| env.iter().find(|e| e.0 == k).map(|e| e.1.clone()), forced)
}

#[test]
fn no_file_keeps_the_old_behaviour() {
    let d = tmp();
    let c = load(&d, &d, &[("ANTHROPIC_API_KEY", "k1")], None).unwrap();
    assert_eq!((c.base_url.as_str(), c.api_key.as_str(), c.provider.as_str()), ("http://127.0.0.1:8898", "k1", ""));
    assert_eq!(c.transport, Transport::Gateway);
    assert_eq!(c.warm, "auto");
}

#[test]
fn legacy_planning_keys_still_load() {
    let d = tmp();
    std::fs::create_dir_all(d.join(".elisa")).unwrap();
    std::fs::write(d.join(".elisa/config.toml"), "plan = \"auto\"\ntest_plan = \"off\"\nplan_exec = \"agent\"\nembed_url = \"http://x\"\ntools_env = [\"ruff\"]\nmax_steps = 7\n").unwrap();
    let c = load(&d, &d, &[], None).unwrap();
    assert_eq!(c.max_steps, 7);
}

#[test]
fn active_connection_replaces_the_address_and_turns_the_gateway_extras_off() {
    let d = tmp();
    write_providers(&d, TWO);
    let c = load(&d, &d, &[("CLOUD_KEY", "secret")], None).unwrap();
    assert_eq!((c.provider.as_str(), c.model.as_str(), c.api_key.as_str()), ("cloud", "big", "secret"));
    assert_eq!(c.base_url, "https://api.example.com/v1");
    assert_eq!(c.transport, Transport::OpenAi);
    assert_eq!(c.warm, "off");
    assert_eq!(c.max_ctx, Some(100000));
    assert_eq!(c.leaves.as_deref(), Some("api.example.com"));
}

#[test]
fn a_generic_anthropic_key_never_reaches_another_provider() {
    let d = tmp();
    write_providers(&d, "active = \"o\"\n[providers.o]\ntype = \"openai\"\nbase_url = \"https://x.example/v1\"\nmodel = \"m\"\nmax_ctx = 1000\n");
    let c = load(&d, &d, &[("ANTHROPIC_API_KEY", "anthropic-secret"), ("ELISA_API_KEY", "elisa-secret")], None).unwrap();
    assert_eq!(c.api_key, "");
}

#[test]
fn choosing_a_connection() {
    let d = tmp();
    write_providers(&d, TWO);
    let env = [("CLOUD_KEY", "s")];
    assert_eq!(load(&d, &d, &env, Some("box")).unwrap().provider, "box");
    assert_eq!(load(&d, &d, &[("ELISA_PROVIDER", "box")], None).unwrap().provider, "box");
    assert_eq!(load(&d, &d, &[("CLOUD_KEY", "s"), ("ELISA_PROVIDER", "box")], Some("cloud")).unwrap().provider, "cloud", "the flag wins over the env");
    // none, or a server named directly: no connection and none of its key
    let c = load(&d, &d, &env, Some("none")).unwrap();
    assert_eq!((c.provider.as_str(), c.transport.clone()), ("", Transport::Gateway));
    let c = load(&d, &d, &[("ELISA_BASE_URL", "http://127.0.0.1:1")], None).unwrap();
    assert_eq!((c.provider.as_str(), c.base_url.as_str()), ("", "http://127.0.0.1:1"));
    assert!(load(&d, &d, &env, Some("nope")).unwrap_err().contains("nope"));
    assert!(load(&d, &d, &[], None).unwrap_err().contains("CLOUD_KEY"), "the named key variable is unset");
}

#[test]
fn a_project_cannot_choose_or_redirect_a_connection() {
    let d = tmp();
    write_providers(&d, TWO);
    let proj = d.join("proj");
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    // its base_url loses to the active connection, so the key does not go to it
    std::fs::write(proj.join(".elisa/config.toml"), "base_url = \"https://evil.example\"\nmodel = \"x\"\n").unwrap();
    let c = load(&proj, &d, &[("CLOUD_KEY", "s")], None).unwrap();
    assert_eq!(c.base_url, "https://api.example.com/v1");
    assert_eq!(c.model, "big");
    // the keys do not exist in a project file
    std::fs::write(proj.join(".elisa/config.toml"), "provider = \"cloud\"\n").unwrap();
    assert!(load(&proj, &d, &[("CLOUD_KEY", "s")], None).is_err());
    std::fs::write(proj.join(".elisa/config.toml"), "[providers.x]\ntype = \"openai\"\n").unwrap();
    assert!(load(&proj, &d, &[("CLOUD_KEY", "s")], None).is_err());
}

#[test]
fn bad_files_say_what_is_wrong() {
    let e = |t: &str| Providers::parse(t, "f").unwrap_err();
    assert!(e("active = \"x\"\n").contains("no such provider"));
    assert!(e("[providers.a]\ntype = \"grpc\"\nbase_url = \"http://x\"\nmodel = \"m\"\n").contains("grpc"));
    // openai without max_ctx is fine: the window comes from the server (`backend::detect`)
    assert!(Providers::parse("[providers.a]\ntype = \"openai\"\nbase_url = \"http://x\"\nmodel = \"m\"\n", "f").is_ok());
    assert!(e("[providers.a]\ntype = \"mcp\"\ncommand = [\"x\"]\n").contains("max_ctx"));
    assert!(e("[providers.a]\ntype = \"openai\"\nbase_url = \"x\"\nmodel = \"m\"\nmax_ctx = 1\n").contains("http"));
    assert!(e("[providers.a]\ntype = \"mcp\"\nmax_ctx = 1\n").contains("exactly one"));
    assert!(e("[providers.a]\ntype = \"mcp\"\ncommand = [\"x\"]\nurl = \"http://y\"\nmax_ctx = 1\n").contains("exactly one"));
    assert!(e("[providers.a]\ntype = \"anthropic\"\nbase_url = \"http://x\"\nmodel = \"m\"\nfoo = 1\n").contains("foo"));
    assert!(e("[providers.none]\ntype = \"anthropic\"\nbase_url = \"http://x\"\nmodel = \"m\"\n").contains("reserved"));
    let remote = Profile { kind: "anthropic".into(), base_url: "https://api.anthropic.com".into(), model: "m".into(), ..Profile::default() };
    assert!(remote.key("c", &|_| None).unwrap_err().contains("API key"));
    let local = Profile { base_url: "http://127.0.0.1:9".into(), ..remote };
    assert_eq!(local.key("c", &|_| None).unwrap(), "");
}

#[test]
fn saving_keeps_keys_out_of_the_json_and_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmp();
    let draft = |key: Value| json!({"active": "a", "providers": [{"name": "a", "type": "openai", "base_url": "http://127.0.0.1:1/v1", "model": "m", "max_ctx": 100, "api_key": key}]});
    let f = from_json(&draft(json!("sk-one")), &Providers::default()).unwrap();
    let p = f.save(&d).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    let back = Providers::load(&d).unwrap();
    assert_eq!(back, f);
    let shown = elisa::provider::to_json(&back, &p, &|_| None).to_string();
    assert!(!shown.contains("sk-one"), "{shown}");
    assert!(shown.contains("\"has_key\":true"));
    // absent = keep the saved key (also across a rename); "" = remove it
    let keep = from_json(&json!({"providers": [{"name": "b", "was": "a", "type": "openai", "base_url": "http://127.0.0.1:1/v1", "model": "m", "max_ctx": 100}]}), &back).unwrap();
    assert_eq!(keep.providers["b"].api_key, "sk-one");
    assert_eq!(from_json(&draft(json!("")), &back).unwrap().providers["a"].api_key, "");
    assert!(from_json(&json!({"active": "z", "providers": []}), &back).is_err());
}

// ---------- a throwaway HTTP server: records each request, answers from `reply(path, body)` ----------

#[derive(Clone, Debug)]
struct Seen { path: String, headers: Vec<(String, String)>, body: Value }

impl Seen {
    fn header(&self, n: &str) -> Option<&str> { self.headers.iter().find(|h| h.0 == n).map(|h| h.1.as_str()) }
}

fn serve(reply: impl Fn(&str, &Value) -> (u16, &'static str, String) + Send + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { return };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() { continue; }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (mut headers, mut len) = (Vec::new(), 0usize);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() { break; }
                if let Some((k, v)) = h.trim().split_once(':') {
                    let k = k.to_lowercase();
                    if k == "content-length" { len = v.trim().parse().unwrap_or(0); }
                    headers.push((k, v.trim().to_string()));
                }
            }
            let mut b = vec![0; len];
            let _ = r.read_exact(&mut b);
            let body: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
            let (st, ct, out) = reply(&path, &body);
            s2.lock().unwrap().push(Seen { path, headers, body });
            let _ = write!(c, "HTTP/1.1 {st} X\r\ncontent-type: {ct}\r\nmcp-session-id: sess-1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len());
        }
    });
    (url, seen)
}

fn tool_def() -> ToolDef { ToolDef { name: "read".into(), description: "read a file".into(), input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}) } }

fn history() -> Vec<Message> {
    vec![
        Message::user_text("read a.rs"),
        Message::new(Role::Assistant, vec![Block::Text { text: "ok".into() }, Block::ToolUse { id: "t1".into(), name: "read".into(), input: json!({"path": "a.rs"}) }]),
        Message::new(Role::User, vec![Block::ToolResult { tool_use_id: "t1".into(), content: "bad".into(), is_error: true }, Block::Text { text: "and?".into() }]),
    ]
}

fn run(client: &Client, msgs: &[Message], tools: &[ToolDef]) -> Result<(elisa::types::Reply, String), String> {
    let req = Request { system: "sys", tools, messages: msgs, max_tokens: 50 };
    let mut text = String::new();
    let r = client.send(&req, &AtomicBool::new(false), &mut |t| text.push_str(t))?;
    Ok((r, text))
}

#[test]
fn openai_translates_the_request_and_the_stream() {
    let sse = [
        r#"{"choices":[{"delta":{"content":"hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"b.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":30,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":10}}}"#,
    ].iter().map(|d| format!("data: {d}\n\n")).collect::<String>() + "data: [DONE]\n\n";
    let (url, seen) = serve(move |_, _| (200, "text/event-stream", sse.clone()));
    let cfg = Config { base_url: format!("{url}/v1"), model: "m".into(), api_key: "sk".into(), transport: Transport::OpenAi, ..Config::default() };
    let client = Client::new(&cfg);
    let (r, text) = run(&client, &history(), &[tool_def()]).unwrap();

    assert_eq!(text, "hello");
    assert_eq!(r.stop_reason, "tool_use");
    assert_eq!(r.content, vec![Block::Text { text: "hello".into() }, Block::ToolUse { id: "c1".into(), name: "read".into(), input: json!({"path": "b.rs"}) }]);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_input_tokens, r.usage.output_tokens), (20, 10, 7));
    assert_eq!(client.take_calls()[0].cached_input_tokens, Some(10));

    let s = seen.lock().unwrap()[0].clone();
    assert_eq!(s.path, "/v1/chat/completions");
    assert_eq!(s.header("authorization"), Some("Bearer sk"));
    assert_eq!(s.header("x-api-key"), None);
    assert_eq!(s.body["max_tokens"], 50);
    assert_eq!(s.body["tools"][0]["function"]["name"], "read");
    let m = s.body["messages"].as_array().unwrap();
    assert_eq!(m.iter().map(|x| x["role"].as_str().unwrap()).collect::<Vec<_>>(), ["system", "user", "assistant", "tool", "user"]);
    assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
    assert_eq!((m[3]["tool_call_id"].as_str(), m[3]["content"].as_str()), (Some("t1"), Some("[error] bad")));
    assert_eq!(m[4]["content"], "and?");
}

#[test]
fn openai_errors_and_length_stop() {
    let (url, _) = serve(|_, _| (401, "application/json", r#"{"error":{"message":"bad key"}}"#.into()));
    let cfg = Config { base_url: url, model: "m".into(), transport: Transport::OpenAi, ..Config::default() };
    assert!(run(&Client::new(&cfg), &history(), &[]).unwrap_err().contains("bad key"));

    let (url, _) = serve(|_, _| (200, "text/event-stream", "data: {\"choices\":[{\"delta\":{\"content\":\"cut\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n".into()));
    let cfg = Config { base_url: url, model: "m".into(), transport: Transport::OpenAi, ..Config::default() };
    assert_eq!(run(&Client::new(&cfg), &history(), &[]).unwrap().0.stop_reason, "max_tokens");

    let (url, _) = serve(|_, _| (200, "text/event-stream", "data: {\"error\":{\"message\":\"overloaded\"}}\n\n".into()));
    let cfg = Config { base_url: url, model: "m".into(), transport: Transport::OpenAi, ..Config::default() };
    assert!(run(&Client::new(&cfg), &history(), &[]).unwrap_err().contains("overloaded"));
}

#[test]
fn openai_own_api_gets_its_token_field() {
    let req = Request { system: "", tools: &[], messages: &[], max_tokens: 9 };
    let b = elisa::openai::body("https://api.openai.com/v1", "m", &req);
    assert_eq!(b["max_completion_tokens"], 9);
    assert!(b.get("max_tokens").is_none());
}

const ANTHROPIC_SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":3}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

#[test]
fn anthropic_gets_no_gateway_extras_and_the_gateway_keeps_them() {
    let (url, seen) = serve(|_, _| (200, "text/event-stream", ANTHROPIC_SSE.into()));
    let cfg = Config { base_url: url, model: "m".into(), api_key: "sk-ant".into(), transport: Transport::Anthropic, ..Config::default() };
    let client = Client::new(&cfg);
    assert_eq!(run(&client, &history(), &[]).unwrap().1, "hi");
    let s = seen.lock().unwrap()[0].clone();
    assert_eq!(s.path, "/v1/messages");
    assert_eq!(s.header("x-api-key"), Some("sk-ant"));
    assert_eq!(s.header("authorization"), None, "a Bearer header would be read as an OAuth token");
    assert_eq!(s.header("x-streamdense-project"), None);
    assert_eq!(client.server_max_ctx(), None);
    assert!(!client.server_warm());

    let (url, seen) = serve(|_, _| (200, "text/event-stream", ANTHROPIC_SSE.into()));
    let cfg = Config { base_url: url, model: "m".into(), ..Config::default() };
    run(&Client::new(&cfg), &history(), &[]).unwrap();
    let s = seen.lock().unwrap()[0].clone();
    assert!(s.header("x-streamdense-project").is_some());
}

// ---------- MCP ----------

/// A real MCP server over stdio: answers by matching the method, echoes `ARG_CHECK` when the call has it.
fn mcp_script(dir: &Path, schema: &str, answer: &str) -> Vec<String> {
    let body = format!(r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{{"jsonrpc":"2.0","id":\([0-9]*\).*/\1/p')
  printf '%s\n' "$line" >> "{log}"
  case "$line" in
    *notifications/*) ;;
    *'"tools/list"'*) printf '%s\n' '{{"jsonrpc":"2.0","method":"notifications/message","params":{{}}}}'; printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"generate","inputSchema":{schema}}}]}}}}\n' "$id" ;;
    *'"tools/call"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{answer}}}\n' "$id" ;;
    *'"initialize"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fake","version":"1"}}}}}}\n' "$id" ;;
  esac
done
"#, log = dir.join("log").display());
    let p = dir.join("server.sh");
    std::fs::write(&p, body).unwrap();
    vec!["/bin/sh".into(), p.display().to_string()]
}

fn mcp_cfg(command: Vec<String>) -> Config {
    Config { base_url: "mcp:x".into(), model: "m".into(), transport: Transport::Mcp(elisa::provider::McpTarget { command, ..Default::default() }), ..Config::default() }
}

#[test]
fn mcp_stdio_messages_tool_can_use_elisas_tools() {
    let d = tmp();
    let answer = r#"{"content":[{"type":"text","text":"{\"content\":[{\"type\":\"text\",\"text\":\"reading\"},{\"type\":\"tool_use\",\"id\":\"u1\",\"name\":\"read\",\"input\":{\"path\":\"a.rs\"}}],\"stop_reason\":\"tool_use\",\"usage\":{\"input_tokens\":11,\"output_tokens\":4}}"}]}"#.replace('\\', "\\\\");
    let cmd = mcp_script(&d, r#"{"type":"object","properties":{"messages":{},"system":{},"tools":{},"max_tokens":{}}}"#, &answer);
    let client = Client::new(&mcp_cfg(cmd));
    let (r, text) = run(&client, &history(), &[tool_def()]).unwrap();
    assert_eq!(text, "reading");
    assert_eq!(r.stop_reason, "tool_use");
    assert_eq!(r.content[1], Block::ToolUse { id: "u1".into(), name: "read".into(), input: json!({"path": "a.rs"}) });
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (11, 4));
    // the second call reuses the running server: one initialize in the log
    run(&client, &history(), &[tool_def()]).unwrap();
    let log = std::fs::read_to_string(d.join("log")).unwrap();
    assert_eq!(log.matches("\"initialize\"").count(), 1);
    let call: Value = serde_json::from_str(log.lines().find(|l| l.contains("tools/call")).unwrap()).unwrap();
    let a = &call["params"]["arguments"];
    assert_eq!(call["params"]["name"], "generate");
    assert_eq!((a["system"].as_str(), a["max_tokens"].as_u64()), (Some("sys"), Some(50)));
    assert_eq!(a["tools"][0]["name"], "read");
    assert_eq!(a["messages"][0]["role"], "user");
    assert!(a.get("model").is_none(), "only what the schema declares");
}

#[test]
fn mcp_stdio_text_tool_gets_a_transcript_and_says_it_is_chat_only() {
    let d = tmp();
    let cmd = mcp_script(&d, r#"{"type":"object","properties":{"prompt":{"type":"string"}},"required":["prompt"]}"#, r#"{"content":[{"type":"text","text":"plain answer"}]}"#);
    let t = Profile { kind: "mcp".into(), command: cmd.clone(), max_ctx: Some(1000), ..Profile::default() };
    assert!(probe("p", &t, &|_| None).unwrap().contains("chat only"));
    let client = Client::new(&mcp_cfg(cmd));
    let (r, text) = run(&client, &history(), &[tool_def()]).unwrap();
    assert_eq!((text.as_str(), r.stop_reason.as_str()), ("plain answer", "end_turn"));
    assert!(r.usage.output_tokens > 0, "estimated");
    let log = std::fs::read_to_string(d.join("log")).unwrap();
    let call: Value = serde_json::from_str(log.lines().find(|l| l.contains("tools/call")).unwrap()).unwrap();
    let prompt = call["params"]["arguments"]["prompt"].as_str().unwrap();
    assert!(prompt.starts_with("sys\n\nUser: read a.rs"), "{prompt}");
    assert!(prompt.contains("[tool read"));
}

#[test]
fn mcp_failures_are_errors_and_the_next_call_starts_over() {
    let d = tmp();
    let cmd = mcp_script(&d, r#"{"properties":{"prompt":{}}}"#, r#"{"isError":true,"content":[{"type":"text","text":"model is down"}]}"#);
    let client = Client::new(&mcp_cfg(cmd));
    assert!(run(&client, &history(), &[]).unwrap_err().contains("model is down"));
    let _ = run(&client, &history(), &[]);
    assert!(std::fs::read_to_string(d.join("log")).unwrap().matches("\"initialize\"").count() >= 1);

    let client = Client::new(&mcp_cfg(vec!["/no/such/server".into()]));
    assert!(run(&client, &history(), &[]).unwrap_err().contains("cannot start"));
}

#[test]
fn mcp_cancel_returns_at_once() {
    let d = tmp();
    let p = d.join("silent.sh");
    std::fs::write(&p, "#!/bin/sh\nwhile IFS= read -r line; do\n  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\n  case \"$line\" in\n    *'\"initialize\"'*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{}}\\n' \"$id\" ;;\n    *'\"tools/list\"'*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"tools\":[{\"name\":\"g\",\"inputSchema\":{\"properties\":{\"prompt\":{}}}}]}}\\n' \"$id\" ;;\n  esac\ndone\n").unwrap();
    let client = Client::new(&mcp_cfg(vec!["/bin/sh".into(), p.display().to_string()]));
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(300)); c2.store(true, Ordering::SeqCst); });
    let msgs = history();
    let req = Request { system: "s", tools: &[], messages: &msgs, max_tokens: 5 };
    let t0 = std::time::Instant::now();
    let r = client.send(&req, &cancel, &mut |_| {}).unwrap();
    assert_eq!(r.stop_reason, "cancelled");
    assert!(t0.elapsed().as_secs() < 5);
}

#[test]
fn mcp_http_json_and_sse_with_session_and_bearer() {
    let reply = |_: &str, b: &Value| -> (u16, &'static str, String) {
        let id = b["id"].clone();
        match b["method"].as_str().unwrap_or("") {
            "initialize" => (200, "application/json", json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18", "capabilities": {}}}).to_string()),
            "notifications/initialized" => (202, "application/json", String::new()),
            "tools/list" => (200, "application/json", json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "one", "inputSchema": {"properties": {"messages": {}}}}, {"name": "two", "inputSchema": {"properties": {"prompt": {}}}}]}}).to_string()),
            _ => (200, "text/event-stream", format!("data: {}\n\ndata: {}\n\n", json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}), json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "over http"}]}}))),
        }
    };
    let (url, seen) = serve(reply);
    let target = |tool: &str| elisa::provider::McpTarget { url: format!("{url}/mcp"), tool: tool.into(), ..Default::default() };
    let cfg = Config { model: "m".into(), api_key: "tok".into(), transport: Transport::Mcp(target("two")), ..Config::default() };
    let (_, text) = run(&Client::new(&cfg), &history(), &[]).unwrap();
    assert_eq!(text, "over http");
    let s = seen.lock().unwrap().clone();
    assert_eq!(s[0].header("authorization"), Some("Bearer tok"));
    assert_eq!(s[0].header("mcp-session-id"), None);
    assert_eq!(s.last().unwrap().header("mcp-session-id"), Some("sess-1"));
    assert_eq!(s.last().unwrap().body["params"]["name"], "two");

    // two tools and none named: say which exist; a wrong name too
    let t = target("");
    let e = elisa::mcp::probe(&t, "").unwrap_err();
    assert!(e.contains("one, two"), "{e}");
    assert!(elisa::mcp::probe(&target("three"), "").unwrap_err().contains("no tool"));
}

#[test]
fn connection_test_uses_free_calls_and_names_a_missing_model() {
    let (url, seen) = serve(|p, _| match p {
        "/v1/models" => (200, "application/json", r#"{"data":[{"id":"a"},{"id":"b"}]}"#.into()),
        "/health" => (200, "application/json", r#"{"model":"q","info":{"max_ctx":8192}}"#.into()),
        _ => (404, "text/plain", "no".into()),
    });
    let p = |kind: &str, base: String, model: &str| Profile { kind: kind.into(), base_url: base, model: model.into(), max_ctx: Some(10), api_key: "k".into(), ..Profile::default() };
    assert!(probe("x", &p("openai", format!("{url}/v1"), "a"), &|_| None).unwrap().contains("a is available"));
    let e = probe("x", &p("openai", format!("{url}/v1"), "z"), &|_| None).unwrap_err();
    assert!(e.contains("z is not listed") && e.contains("a, b"), "{e}");
    assert!(probe("x", &p("anthropic", url.clone(), "b"), &|_| None).unwrap().contains("b is available"));
    // `p` asks for max_ctx 10: below the gateway's 8192 it is fine, above it the test says so
    assert!(probe("x", &p("streamdense", url.clone(), "q"), &|_| None).unwrap().contains("8192"));
    let big = Profile { max_ctx: Some(16384), ..p("streamdense", url.clone(), "q") };
    let e = probe("x", &big, &|_| None).unwrap_err();
    assert!(e.contains("runs with context 8192") && e.contains("--max-ctx 16384"), "{e}");
    let auto = Profile { max_ctx: None, ..big };
    assert!(probe("x", &auto, &|_| None).is_ok());
    assert!(seen.lock().unwrap().iter().all(|s| s.body.is_null()), "no request carries a prompt");
    assert!(probe("x", &p("openai", "http://127.0.0.1:1/v1".into(), "a"), &|_| None).unwrap_err().contains("cannot reach"));
}

#[test]
fn vision_follows_the_setting_else_the_transport() {
    use elisa::provider::{ollama_caps, vision, Transport};
    let caps = vec!["completion".to_string(), "vision".to_string()];
    assert!(vision(&Transport::Anthropic, None, None));
    assert!(!vision(&Transport::Anthropic, None, Some(false)));
    assert!(!vision(&Transport::Gateway, None, None));
    assert!(vision(&Transport::Gateway, None, Some(true)));
    assert!(vision(&Transport::OpenAi, Some(&caps), None));
    assert!(!vision(&Transport::OpenAi, Some(&caps[..1]), None));
    assert!(!vision(&Transport::OpenAi, None, None));
    // a server that is not Ollama (nothing listens on port 9) answers nothing, quickly
    let t0 = std::time::Instant::now();
    assert!(ollama_caps("http://127.0.0.1:9/v1", "m").is_none());
    assert!(t0.elapsed().as_secs() < 4);
}

#[test]
fn server_model_lists_are_read_from_ollama_and_openai_shapes() {
    use elisa::provider::{parse_ollama_tags, parse_openai_models};
    let o = serde_json::json!({"models": [
        {"name": "qwen3-vl:2b-instruct", "size": 1900000000u64, "details": {"parameter_size": "2.1B"}},
        {"model": "llama3.2:1b"},
    ]});
    let ms = parse_ollama_tags(&o).unwrap();
    assert_eq!((ms[0].name.as_str(), ms[0].bytes, ms[0].params.as_str()), ("qwen3-vl:2b-instruct", 1_900_000_000, "2.1B"));
    assert_eq!(ms[1].name, "llama3.2:1b");
    // llama.cpp's server lists the model it was started with
    let l = serde_json::json!({"object": "list", "data": [{"id": "gemma-3-4b-it-Q4_K_M.gguf", "object": "model"}]});
    assert_eq!(parse_openai_models(&l).unwrap()[0].name, "gemma-3-4b-it-Q4_K_M.gguf");
    assert!(parse_openai_models(&serde_json::json!({"error": "x"})).is_none());
}

/// A server with no prompt cache (llama.cpp `--no-cache-prompt`, a plain GGUF server, Ollama's native API with a cold
/// prompt): the usage without any cache field is the whole prompt as input, 0 reused, and not "reported" for metrics.
#[test]
fn usage_without_cache_fields_is_the_whole_prompt() {
    let mut on = |_: &str| {};
    let mut s = elisa::openai::Stream::default();
    s.feed(r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#, &mut on).unwrap();
    s.feed(r#"{"choices":[],"usage":{"prompt_tokens":2280,"completion_tokens":84,"total_tokens":2364}}"#, &mut on).unwrap();
    let (_, stop, u, seen) = s.finish(false);
    assert_eq!((stop.as_str(), u.input_tokens, u.cache_read_input_tokens, u.output_tokens, seen), ("end_turn", 2280, 0, 84, false));

    // llama.cpp with prompt caching off still sends the field, as 0
    let mut s = elisa::openai::Stream::default();
    s.feed(r#"{"choices":[],"usage":{"prompt_tokens":2280,"completion_tokens":84,"prompt_tokens_details":{"cached_tokens":0}}}"#, &mut on).unwrap();
    let (_, _, u, seen) = s.finish(false);
    assert_eq!((u.input_tokens, u.cache_read_input_tokens, seen), (2280, 0, true));

    let mut s = elisa::ollama::Stream::default();
    assert!(s.feed(r#"{"message":{"role":"assistant","content":"hi"},"done":true,"done_reason":"stop","prompt_eval_count":2280,"eval_count":84}"#, &mut on).unwrap());
    let (_, _, u) = s.finish(false);
    assert_eq!((u.input_tokens, u.cache_read_input_tokens, u.output_tokens), (2280, 0, 84));
}

// `/model NAME` switches the active connection to that installed model; only the typed name counts
#[test]
fn model_name_switches_the_active_connection_to_that_installed_model() {
    let names: Vec<String> = ["granite4.2:3b", "gemma3:1b", "llama3.2:latest"].iter().map(|s| s.to_string()).collect();
    assert_eq!(find_model("granite4.2:3b", &names), Some("granite4.2:3b"));
    assert_eq!(find_model(" gemma3:1b ", &names), Some("gemma3:1b"));
    assert_eq!(find_model("llama3.2", &names), Some("llama3.2:latest"), "a bare name is its :latest tag");
    assert_eq!(find_model("granite4.2", &names), None, "no closest match: Elisa never picks");
    assert_eq!(find_model("gemma3:4b", &names), None);
    assert_eq!(find_model("", &names), None);

    let (url, _) = serve(|p, _| match p {
        "/api/tags" => (200, "application/json", r#"{"models":[{"name":"qwen3-vl:2b-instruct"},{"name":"granite4.2:3b"}]}"#.into()),
        _ => (404, "text/plain", "no".into()),
    });
    let home = tmp();
    write_providers(&home, &format!("active = \"ollama\"\n[providers.ollama]\ntype = \"openai\"\nbase_url = \"{url}/v1\"\nmodel = \"qwen3-vl:2b-instruct\"\nmax_ctx = 8192\n[providers.other]\ntype = \"openai\"\nbase_url = \"{url}/v1\"\nmodel = \"x\"\n"));
    assert_eq!(use_model(&home, "granite4.2:3b", &|_| None).unwrap(), ("ollama".into(), "granite4.2:3b".into(), true));
    let f = Providers::load(&home).unwrap();
    assert_eq!(f.active, "ollama");
    assert_eq!(f.providers["ollama"].model, "granite4.2:3b");
    assert_eq!(f.providers["ollama"].max_ctx, Some(8192), "the rest of the connection is kept");
    assert_eq!(f.providers["other"].model, "x", "other connections are untouched");
    assert!(!use_model(&home, "granite4.2:3b", &|_| None).unwrap().2, "already on it: nothing to change");
    let (why, installed) = use_model(&home, "gemma3:1b", &|_| None).unwrap_err();
    assert!(why.contains("no model named \"gemma3:1b\""), "{why}");
    assert_eq!(installed, vec!["qwen3-vl:2b-instruct".to_string(), "granite4.2:3b".to_string()]);
    assert_eq!(Providers::load(&home).unwrap().providers["ollama"].model, "granite4.2:3b", "an unknown name changes nothing");

    let none = tmp();
    assert!(use_model(&none, "granite4.2:3b", &|_| None).unwrap_err().0.contains("no saved connection"));
}
