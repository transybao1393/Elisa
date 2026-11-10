//! The hosted intent router (optional, paid): one small HTTP call per user turn that names the turn's intents, used
//! instead of the word rules of `route.rs` when it answers. Elisa knows only the endpoint contract below; the model,
//! its prompt and its label descriptions live on the router's server.
//!
//! # Setting
//!
//! A `[router]` table in `~/.config/elisa/providers.toml`, next to the model connections and kept the same way (the
//! file is mode 600; `api_key_env` names an environment variable and wins over `api_key`):
//!
//! ```toml
//! [router]
//! url = "https://router.example.com"
//! api_key_env = "ELISA_ROUTER_KEY"   # or: api_key = "…"
//! elisa_rules = "off"                 # or "fallback"
//! ```
//!
//! No key (neither set) = off: Elisa routes with its own word rules, exactly as without this module.
//!
//! `elisa_rules`: what Elisa's own intent rules (word signals and meaning checks: `route::classify`, the follow-up,
//! greeting, about-Elisa, picture and own-fact checks, the reasoning gate, news recency by meaning) do while the
//! router is on. "off" (the default): only the router's answer decides; on a miss the turn gets every tool and the
//! model decides, with no rule-based notes. "fallback": the router first, Elisa's rules where it gives no answer.
//! A session changes it with `/router rules off|fallback` (saved here).
//!
//! # Contract (version 1)
//!
//! Request: `POST {url}/route`, headers `Authorization: Bearer {key}` and `Content-Type: application/json`, body
//!
//! ```json
//! {"message": "the user's latest message",
//!  "context": {"attachment": "pdf: a.pdf" | null,  // a document attached in THIS turn: "<kind>: <name>"
//!              "doc_in_chat": false,                // a document is in the conversation
//!              "vision": true,                      // the chat model can see pictures
//!              "image_refused": false,              // a picture was refused because the model has no vision
//!              "prev_session": false,               // an earlier session exists to resume (new session only)
//!              "last_user": "…",                    // the previous user message, at most 400 characters
//!              "last_reply": "…"}}                  // Elisa's last reply, at most 400 characters
//! ```
//!
//! Nothing else is sent: no history, no file or document text, no memory, no tool output, no system prompt.
//!
//! Response `200`, JSON: `{"intents": ["web", "web_search_now"], "file"?: "notes.md", "page"?: "3", "item"?: 2,
//! "topic"?: "Osaka"}`. `intents` is a non-empty list of the labels in [`LABELS`]; an unknown label is ignored.
//!
//! Anything else (no answer within [`TIMEOUT`], a non-2xx status, a body that is not this JSON, no known label) is a
//! miss: no error in the chat, at most one quiet info line per session, and the turn routes as `elisa_rules` says.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use crate::route::{Groups, CODE, DOCS, RUN, SEC, TIME, WEB};

/// the whole call, connect included: a turn never waits longer than this for routing
pub const TIMEOUT: Duration = Duration::from_millis(2000);
/// `last_user` / `last_reply` are cut to this many characters
pub const CONTEXT_CHARS: usize = 400;

/// The contract's labels (the endpoint's vocabulary, not its prompt).
pub const LABELS: [&str; 22] = ["chat", "files_code", "web", "documents", "run", "time", "security", "reasoning",
    "web_search_now", "read_file", "save_file", "about_elisa", "greeting", "closing", "resume_last", "recap",
    "followup_why", "followup_more", "pick_item", "what_about", "doc_page", "refused_picture"];

fn empty(s: &str) -> bool { s.is_empty() }

/// `[router]` in providers.toml.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouterConf {
    #[serde(default, skip_serializing_if = "empty")]
    pub url: String,
    /// the name of the environment variable holding the key (preferred; wins over `api_key`)
    #[serde(default, skip_serializing_if = "empty")]
    pub api_key_env: String,
    #[serde(default, skip_serializing_if = "empty")]
    pub api_key: String,
    /// Elisa's own intent rules while the router is on: "off" (the default, also when empty) or "fallback"
    #[serde(default, skip_serializing_if = "empty")]
    pub elisa_rules: String,
}

/// the values of `elisa_rules`
pub const RULES: [&str; 2] = ["off", "fallback"];

impl RouterConf {
    pub fn validate(&self) -> Result<(), String> {
        if !self.url.is_empty() && !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
            return Err(format!("router: url must start with http:// or https:// (got {:?})", self.url));
        }
        if !self.elisa_rules.is_empty() && !RULES.contains(&self.elisa_rules.as_str()) {
            return Err(format!("router: elisa_rules must be \"off\" or \"fallback\" (got {:?})", self.elisa_rules));
        }
        Ok(())
    }

    /// The endpoint to use: None (off) without a URL or without a key.
    pub fn target(&self, getenv: &dyn Fn(&str) -> Option<String>) -> Option<Target> {
        let key = if self.api_key_env.is_empty() { None } else { getenv(&self.api_key_env).filter(|v| !v.trim().is_empty()) }
            .unwrap_or_else(|| self.api_key.clone());
        let (url, key) = (self.url.trim().trim_end_matches('/').to_string(), key.trim().to_string());
        (!url.is_empty() && !key.is_empty()).then_some(Target { url, key, fallback: self.elisa_rules == "fallback" })
    }
}

/// A configured router: where, with which key, and whether Elisa's own rules decide where it gives no answer
/// (`elisa_rules = "fallback"`). `Debug` never prints the key.
#[derive(Clone, PartialEq, Eq)]
pub struct Target { pub url: String, key: String, pub fallback: bool }

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Target {{ url: {:?}, key: <set>, fallback: {} }}", self.url, self.fallback) }
}

impl Target {
    #[cfg(test)]
    pub fn new(url: &str, key: &str) -> Target { Target { url: url.into(), key: key.into(), fallback: false } }
}

/// `/router rules off|fallback`: `elisa_rules` written to `[router]` in providers.toml under `home` (the file's
/// other settings kept; saved as every connection change is, mode 600). Err: a bad value, or no `[router]` table.
pub fn save_rules(home: &std::path::Path, value: &str) -> Result<std::path::PathBuf, String> {
    if !RULES.contains(&value) { return Err(format!("elisa_rules must be \"off\" or \"fallback\" (got {value:?})")); }
    let mut f = crate::provider::Providers::load(home)?;
    let r = f.router.as_mut().ok_or("no [router] table in providers.toml: add its url and key first")?;
    r.elisa_rules = value.to_string();
    f.save(home)
}

/// The turn's facts sent with the message (the contract's `context`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub attachment: Option<String>,
    pub doc_in_chat: bool,
    pub vision: bool,
    pub image_refused: bool,
    pub prev_session: bool,
    pub last_user: String,
    pub last_reply: String,
}

/// at most `n` characters, cut on a character boundary, trimmed
pub fn cut(s: &str, n: usize) -> String { s.trim().chars().take(n).collect::<String>().trim_end().to_string() }

/// `"pdf: a.pdf"` for an attached document's name (its extension, lower case, is the kind; none = "file")
pub fn attachment_label(name: &str) -> String {
    let kind = std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|e| !e.is_empty()).unwrap_or_else(|| "file".into());
    format!("{kind}: {name}")
}

/// The request body: exactly the contract's fields.
pub fn request_body(message: &str, c: &Context) -> Value {
    json!({
        "message": message,
        "context": {
            "attachment": c.attachment,
            "doc_in_chat": c.doc_in_chat,
            "vision": c.vision,
            "image_refused": c.image_refused,
            "prev_session": c.prev_session,
            "last_user": cut(&c.last_user, CONTEXT_CHARS),
            "last_reply": cut(&c.last_reply, CONTEXT_CHARS),
        }
    })
}

/// The router's answer: its known intents (contract order kept) and the filled slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub intents: Vec<String>,
    pub file: Option<String>,
    pub page: Option<String>,
    pub item: Option<u32>,
    pub topic: Option<String>,
}

impl Answer {
    pub fn has(&self, label: &str) -> bool { self.intents.iter().any(|i| i == label) }
    /// "web, web_search_now (file notes.md)": for the debug line and /context
    pub fn describe(&self) -> String {
        let mut s = self.intents.join(", ");
        let slots: Vec<String> = [("file", self.file.clone()), ("page", self.page.clone()), ("item", self.item.map(|i| i.to_string())), ("topic", self.topic.clone())]
            .into_iter().filter_map(|(k, v)| v.map(|v| format!("{k} {v}"))).collect();
        if !slots.is_empty() { s += &format!(" ({})", slots.join(", ")); }
        s
    }
}

/// A response body read as the contract says; Err on anything else (the caller then falls back).
pub fn parse(body: &str) -> Result<Answer, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let list = v["intents"].as_array().ok_or("no `intents` list")?;
    let mut intents: Vec<String> = Vec::new();
    for l in list.iter().filter_map(Value::as_str).map(str::trim) {
        if LABELS.contains(&l) && !intents.iter().any(|x| x == l) { intents.push(l.to_string()); }
    }
    if intents.is_empty() { return Err("no known intent".into()); }
    let s = |k: &str| v[k].as_str().map(str::trim).filter(|x| !x.is_empty()).map(String::from);
    let item = v["item"].as_u64().or_else(|| v["item"].as_str().and_then(|x| x.trim().parse().ok())).filter(|&n| n > 0 && n < 10_000).map(|n| n as u32);
    Ok(Answer { intents, file: s("file"), page: s("page").or_else(|| v["page"].as_u64().map(|n| n.to_string())), item, topic: s("topic") })
}

/// One `/route` call. Err: why it missed (never shown in the chat).
pub fn call(t: &Target, message: &str, c: &Context) -> Result<Answer, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(TIMEOUT)).build().into();
    let mut r = agent.post(format!("{}/route", t.url))
        .header("authorization", format!("Bearer {}", t.key))
        .header("content-type", "application/json")
        .send(request_body(message, c).to_string())
        .map_err(|e| format!("unreachable: {e}"))?;
    let st = r.status().as_u16();
    let body = r.body_mut().with_config().limit(64 * 1024).read_to_string().map_err(|e| format!("read: {e}"))?;
    if !(200..300).contains(&st) { return Err(format!("HTTP {st}")); }
    parse(&body)
}

/// What an answer means for the turn, in Elisa's own terms (`route.rs` groups and the existing handlers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// the tool groups (empty: no tools)
    pub groups: Groups,
    /// the reasoning tools (`mcp_reason`'s gate)
    pub reasoning: bool,
    /// only a companion-less `what_about`: keep the last turn's groups
    pub carry: bool,
}

/// The intents mapped onto the route groups. Shell commands are `files_code` (bash is in CODE).
pub fn plan(a: &Answer) -> Plan {
    let mut g = 0u16;
    for i in &a.intents {
        g |= match i.as_str() {
            "files_code" | "read_file" | "save_file" => CODE,
            "web" | "web_search_now" => WEB,
            "documents" | "doc_page" => DOCS,
            "run" => RUN,
            "time" => TIME,
            "security" => SEC,
            // chat, greeting, closing, about_elisa, recap, followup_*, pick_item, resume_last, refused_picture,
            // reasoning (its own gate), what_about (its companion carries the groups): no tool group
            _ => 0,
        };
    }
    let companion = a.intents.iter().any(|i| i != "what_about" && i != "chat");
    Plan { groups: Groups(g), reasoning: a.has("reasoning"), carry: a.has("what_about") && !companion }
}

/// The session's router: the target, and whether the one quiet miss line was shown.
#[derive(Debug, Default)]
pub struct Router {
    pub target: Option<Target>,
    told: bool,
}

impl Router {
    pub fn new(target: Option<Target>) -> Router { Router { target, told: false } }
    pub fn on(&self) -> bool { self.target.is_some() }
    /// Elisa's own intent rules may decide: no router, or one with `elisa_rules = "fallback"`
    pub fn rules_on(&self) -> bool { self.target.as_ref().is_none_or(|t| t.fallback) }
    /// the router and its `elisa_rules`, in one line (the startup line, `/router`, `/context`)
    pub fn describe(&self) -> String {
        match &self.target {
            None => "intent router off (no [router] url and key in providers.toml): Elisa's own intent rules decide".into(),
            Some(t) if t.fallback => format!("intent router on ({}) · Elisa's own intent rules: fallback (where the router gives no answer)", t.url),
            Some(t) => format!("intent router on ({}) · Elisa's own intent rules: off (only the router decides; a miss offers every tool)", t.url),
        }
    }

    /// The router's answer for this turn, or None (off, or a miss). `info`: the one quiet line of a session's first
    /// miss; later misses say nothing.
    pub fn ask(&mut self, message: &str, c: &Context, info: &mut dyn FnMut(&str)) -> (Option<Answer>, Option<String>) {
        let Some(t) = &self.target else { return (None, None) };
        match call(t, message, c) {
            Ok(a) => (Some(a), None),
            Err(e) => {
                if !std::mem::replace(&mut self.told, true) {
                    info(if t.fallback { "The intent router did not answer; Elisa routes on her own for now." }
                        else { "The intent router did not answer; the model gets every tool for now." });
                }
                (None, Some(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn ans(l: &[&str]) -> Answer { Answer { intents: l.iter().map(|s| s.to_string()).collect(), ..Default::default() } }

    #[test]
    fn intents_map_onto_the_route_groups() {
        let cases: &[(&[&str], u16)] = &[
            (&["files_code"], CODE), (&["files_code", "read_file"], CODE), (&["files_code", "save_file"], CODE),
            (&["web"], WEB), (&["web", "web_search_now"], WEB), (&["documents"], DOCS), (&["doc_page"], DOCS),
            (&["run"], RUN), (&["time"], TIME), (&["security", "files_code"], SEC | CODE),
            (&["web", "web_search_now", "files_code", "save_file"], WEB | CODE),
            (&["what_about", "time"], TIME), (&["what_about", "web"], WEB),
        ];
        for (l, g) in cases { assert_eq!(plan(&ans(l)).groups, Groups(*g), "{l:?}"); }
        for l in ["chat", "greeting", "closing", "about_elisa", "recap", "followup_why", "followup_more", "pick_item", "resume_last", "refused_picture", "reasoning"] {
            let p = plan(&ans(&[l]));
            assert!(p.groups.is_empty() && !p.carry, "{l}: {p:?}");
        }
        assert!(plan(&ans(&["reasoning", "chat"])).reasoning);
        assert!(!plan(&ans(&["web"])).reasoning);
        // a what_about with no companion intent keeps the last turn's groups; with one it does not
        assert!(plan(&ans(&["what_about"])).carry);
        assert!(plan(&ans(&["what_about", "chat"])).carry);
        assert!(!plan(&ans(&["what_about", "web"])).carry);
        // every label is mapped somewhere (no label falls through unnoticed)
        assert_eq!(LABELS.len(), 22);
    }

    #[test]
    fn answers_are_read_strictly() {
        let a = parse(r#"{"intents": ["files_code", "save_file", "bogus", "save_file"], "file": "notes.md", "item": 2, "topic": " Osaka "}"#).unwrap();
        assert_eq!(a.intents, ["files_code", "save_file"]);
        assert_eq!((a.file.as_deref(), a.item, a.topic.as_deref()), (Some("notes.md"), Some(2), Some("Osaka")));
        assert_eq!(parse(r#"{"intents": ["doc_page"], "page": 3}"#).unwrap().page.as_deref(), Some("3"));
        for bad in ["", "not json", "{}", r#"{"intents": []}"#, r#"{"intents": ["bogus"]}"#, r#"{"intents": "web"}"#, "[1,2]"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_request_carries_only_the_contract_fields() {
        let long = "x".repeat(1000);
        let c = Context { attachment: Some(attachment_label("Report.PDF")), doc_in_chat: true, vision: false, image_refused: true,
            prev_session: true, last_user: format!("  {long}"), last_reply: "ồ".repeat(500) };
        let b = request_body("save it to a.md", &c);
        let top: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top, ["message", "context"]);
        let ctx: Vec<&str> = b["context"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(ctx, ["attachment", "doc_in_chat", "vision", "image_refused", "prev_session", "last_user", "last_reply"]);
        assert_eq!(b["context"]["attachment"], "pdf: Report.PDF");
        assert_eq!(b["context"]["last_user"].as_str().unwrap().chars().count(), CONTEXT_CHARS);
        assert_eq!(b["context"]["last_reply"].as_str().unwrap().chars().count(), CONTEXT_CHARS);
        assert_eq!(request_body("hi", &Context::default())["context"]["attachment"], Value::Null);
        assert_eq!(attachment_label("notes"), "file: notes");
    }

    #[test]
    fn no_key_means_off() {
        let env = |k: &str| (k == "RK").then(|| "from-env".to_string());
        let none = |_: &str| None;
        assert!(RouterConf::default().target(&none).is_none());
        assert!(RouterConf { url: "http://r".into(), ..Default::default() }.target(&none).is_none(), "a URL without a key is off");
        assert!(RouterConf { api_key: "k".into(), ..Default::default() }.target(&none).is_none(), "a key without a URL is off");
        assert!(RouterConf { url: "http://r".into(), api_key_env: "UNSET".into(), ..Default::default() }.target(&none).is_none());
        let t = RouterConf { url: "http://r/".into(), api_key_env: "RK".into(), api_key: "file".into(), ..Default::default() }.target(&env).unwrap();
        assert_eq!((t.url.as_str(), t.key.as_str()), ("http://r", "from-env"));
        assert!(!format!("{t:?}").contains("from-env"), "Debug never shows the key");
        assert!(RouterConf { url: "ftp://r".into(), ..Default::default() }.validate().is_err());
        let mut told = 0;
        assert_eq!(Router::new(None).ask("hi", &Context::default(), &mut |_| told += 1), (None, None));
        assert_eq!(told, 0, "off says nothing");
    }

    #[test]
    fn the_setting_lives_in_providers_toml_and_survives_a_window_save() {
        use crate::provider::{from_json, Providers};
        let f = Providers::parse("[router]\nurl = \"http://127.0.0.1:8082\"\napi_key_env = \"RK\"\n", "t").unwrap();
        let r = f.router.clone().unwrap();
        assert_eq!(r.target(&|k: &str| (k == "RK").then(|| "x".to_string())).unwrap().url, "http://127.0.0.1:8082");
        assert!(Providers::parse("[router]\nurl = \"nope\"\napi_key = \"k\"\n", "t").is_err());
        assert!(Providers::parse("[router]\nurl = \"http://r\"\nkey = \"k\"\n", "t").is_err(), "unknown field");
        // the Model Connections window saves connections only: the router setting stays
        let saved = from_json(&json!({"active": "", "providers": []}), &f).unwrap();
        assert_eq!(saved.router, f.router);
        assert!(Providers::parse("active = \"\"\n", "t").unwrap().router.is_none());
    }

    #[test]
    fn elisa_rules_is_off_unless_set_to_fallback() {
        use crate::provider::Providers;
        let key = |_: &str| None;
        let conf = |extra: &str| Providers::parse(&format!("[router]\nurl = \"http://r\"\napi_key = \"k\"\n{extra}"), "t");
        let target = |extra: &str| conf(extra).unwrap().router.unwrap().target(&key).unwrap();
        assert!(!target("").fallback, "the default is off");
        assert!(!target("elisa_rules = \"off\"\n").fallback);
        assert!(target("elisa_rules = \"fallback\"\n").fallback);
        let e = conf("elisa_rules = \"sometimes\"\n").unwrap_err();
        assert!(e.contains("elisa_rules must be \"off\" or \"fallback\""), "{e}");
        // the rules decide with no router, or with one set to fallback; never with one set to off
        assert!(Router::new(None).rules_on());
        assert!(Router::new(Some(target("elisa_rules = \"fallback\"\n"))).rules_on());
        assert!(!Router::new(Some(target(""))).rules_on());
        assert!(Router::new(Some(target(""))).describe().contains("intent rules: off"));
        assert!(Router::new(Some(target("elisa_rules = \"fallback\"\n"))).describe().contains("intent rules: fallback"));
    }

    #[test]
    fn rules_are_saved_to_providers_toml_keeping_the_rest() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("elisa-router-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(save_rules(&home, "fallback").is_err(), "no [router] table: nothing to change");
        let p = crate::provider::path_in(&home);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "active = \"l\"\n[providers.l]\ntype = \"openai\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"m\"\n[router]\nurl = \"http://r\"\napi_key = \"k\"\n").unwrap();
        assert!(save_rules(&home, "maybe").is_err());
        save_rules(&home, "fallback").unwrap();
        let f = crate::provider::Providers::load(&home).unwrap();
        let r = f.router.clone().unwrap();
        assert_eq!((r.url.as_str(), r.api_key.as_str(), r.elisa_rules.as_str()), ("http://r", "k", "fallback"));
        assert_eq!((f.active.as_str(), f.providers["l"].model.as_str()), ("l", "m"));
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        save_rules(&home, "off").unwrap();
        assert!(!crate::provider::Providers::load(&home).unwrap().router.unwrap().target(&|_| None).unwrap().fallback);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// a one-shot HTTP server on 127.0.0.1 that answers with `status` and `body` after `delay`; returns its URL and
    /// the request it got
    fn serve(status: u16, body: &'static str, delay: Duration) -> (String, std::thread::JoinHandle<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 { break; }
                req.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&req).to_string();
                if let Some(i) = t.find("\r\n\r\n") {
                    let len = t.to_lowercase().lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if req.len() >= i + 4 + len { break; }
                }
            }
            std::thread::sleep(delay);
            let _ = write!(s, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            String::from_utf8_lossy(&req).to_string()
        });
        (url, h)
    }

    #[test]
    fn a_good_answer_is_used_and_the_request_is_the_contract() {
        let (url, h) = serve(200, r#"{"intents": ["web", "web_search_now"]}"#, Duration::ZERO);
        let mut r = Router::new(Some(Target::new(&url, "sk-test")));
        let mut lines = Vec::new();
        let (a, e) = r.ask("search the web for rust", &Context { vision: true, ..Default::default() }, &mut |l| lines.push(l.to_string()));
        assert_eq!(a.unwrap().intents, ["web", "web_search_now"], "{e:?}");
        assert!(lines.is_empty());
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /route HTTP/1.1\r\n"), "{req}");
        assert!(req.to_lowercase().contains("authorization: bearer sk-test\r\n"), "{req}");
        let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body, request_body("search the web for rust", &Context { vision: true, ..Default::default() }));
    }

    #[test]
    fn every_miss_falls_back_with_one_quiet_line() {
        let ctx = Context::default();
        let mut lines: Vec<String> = Vec::new();
        // HTTP error, bad JSON, no known label, a timeout, nothing listening: each a miss
        let (u1, h1) = serve(401, r#"{"error": "bad key"}"#, Duration::ZERO);
        let (u2, h2) = serve(200, "<html>oops</html>", Duration::ZERO);
        let (u3, h3) = serve(200, r#"{"intents": ["dance"]}"#, Duration::ZERO);
        let (u4, h4) = serve(200, r#"{"intents": ["chat"]}"#, TIMEOUT + Duration::from_millis(800));
        let dead = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); format!("http://{}", l.local_addr().unwrap()) };
        let mut r = Router::new(None);
        for (u, why) in [(&u1, "HTTP 401"), (&u2, "not JSON"), (&u3, "no known intent"), (&u4, ""), (&dead, "unreachable")] {
            r.target = Some(Target::new(u, "k"));
            let t0 = std::time::Instant::now();
            let (a, e) = r.ask("hi", &ctx, &mut |l| lines.push(l.to_string()));
            assert!(a.is_none(), "{u}");
            assert!(e.as_deref().unwrap_or("").contains(why), "{u}: {e:?}");
            assert!(t0.elapsed() < TIMEOUT + Duration::from_millis(700), "{u}: waited {:?}", t0.elapsed());
        }
        assert_eq!(lines.len(), 1, "one quiet line per session: {lines:?}");
        for h in [h1, h2, h3, h4] { let _ = h.join(); }
    }
}
