//! Elisa's own web MCP server: `elisa --mcp-serve web [--engine duckduckgo|searxng|brave] [--searxng-url URL]`, a stdio
//! MCP server with two tools, `web_search` and `web_fetch`, over the search methods Elisa ships (`web.rs`). The
//! session starts it like any other MCP server (`mcp_host.rs`) unless config `web = "off"`, so the model reaches the
//! web the same way it will reach every plugin: through MCP. Each call asks the user first (the host's approval).
//! The Brave Search API key comes in the environment (`BRAVE_API_KEY`), never as an argument (`ps` shows those).
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// the tools this server lists (MCP `tools/list`)
pub fn tools() -> Value {
    json!([
        {"name": "web_search", "description": "Search the web; returns the top results (title, url, snippet). For news, current facts or anything not in the project. Answer with what the results say (for news: what happened), not a list of the sites.",
         "inputSchema": {"type": "object", "properties": {"query": {"type": "string"},
             "recent": {"type": "boolean", "description": "true when the user wants news, the latest or what's new (only pages from the past week); false for lasting facts"}}, "required": ["query", "recent"]}},
        {"name": "web_fetch", "description": "Read a public web page as plain text (http/https), e.g. a url from web_search.",
         "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}},
    ])
}

/// one `tools/call`: (text, is_error)
pub fn call(name: &str, args: &Value, cfg: &crate::web::WebCfg) -> (String, bool) {
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let r = match name {
        "web_search" => match arg("query") {
            Some(q) => crate::web::search(q, recent(&args["recent"]), cfg).map(|t| format!("{t}\n(web_fetch a url to read the page)")),
            None => Err("missing query".into()),
        },
        "web_fetch" => match arg("url") { Some(u) => crate::web::fetch(u), None => Err("missing url".into()) },
        n => Err(format!("unknown tool {n}")),
    };
    match r { Ok(t) => (t, false), Err(e) => (e, true) }
}

/// `recent` as a small model may send it: true, "true" or "yes"
fn recent(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| v.as_str().is_some_and(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")))
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, cfg: &crate::web::WebCfg) -> Option<Value> {
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-web", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], cfg);
            json!({"content": [{"type": "text", "text": text}], "isError": err})
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `--mcp-serve web …`: serve stdin / stdout until stdin closes
pub fn serve(args: &[String]) -> i32 {
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let cfg = crate::web::WebCfg {
        engine: opt("--engine").unwrap_or_else(|| crate::web::DUCKDUCKGO.into()),
        searxng: opt("--searxng-url").unwrap_or_default(),
        brave_key: std::env::var(crate::web::BRAVE_KEY_ENV).unwrap_or_default().trim().to_string(),
    };
    // the session's result cache, when it handed one over (`cache::child_env`)
    crate::cache::from_env();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(a) = answer(&m, &cfg) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_mcp_and_lists_both_tools() {
        let cfg = crate::web::WebCfg { engine: "duckduckgo".into(), ..Default::default() };
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), &cfg).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-web");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &cfg).is_none());
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &cfg).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["web_search", "web_fetch"]);
        // a local address is refused without any network use
        let r = answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "web_fetch", "arguments": {"url": "http://127.0.0.1/x"}}}), &cfg).unwrap();
        assert_eq!(r["result"]["isError"], true);
        let bad = answer(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), &cfg).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    /// bug #6: the model says whether a search is for news (`recent`, required so a small model decides every time)
    #[test]
    fn web_search_asks_whether_the_search_is_for_news() {
        let t = tools();
        assert_eq!(t[0]["inputSchema"]["required"], json!(["query", "recent"]));
        assert_eq!(t[0]["inputSchema"]["properties"]["recent"]["type"], "boolean");
        for (v, want) in [(json!(true), true), (json!("true"), true), (json!(" Yes "), true), (json!(false), false), (json!("no"), false), (Value::Null, false)] {
            assert_eq!(recent(&v), want, "{v}");
        }
    }
}
