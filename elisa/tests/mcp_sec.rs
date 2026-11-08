//! The built-in security MCP server (`elisa --mcp-serve sec`) as a session starts it: through the MCP host, against
//! tiny fixtures written here. Paths outside `--root` are refused; `sec_deps` talks to a fake OSV server on 127.0.0.1
//! (`ELISA_OSV_URL`, honoured only for this machine), so no test sends anything to osv.dev.
use elisa::config::Config;
use elisa::mcp_host::{Host, ServerCfg};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("elisa-mcp-sec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn start(root: &Path, env: Vec<(String, String)>) -> Host {
    let cfg = ServerCfg { name: "sec".into(), command: vec![env!("CARGO_BIN_EXE_elisa").into(), "--mcp-serve".into(), "sec".into(),
        "--root".into(), root.display().to_string(), "--max-chars".into(), "6000".into()], env, ..ServerCfg::default() };
    let (host, notes) = Host::start(&[cfg], &|n| ["read", "write", "edit", "bash"].contains(&n));
    assert!(notes.is_empty(), "{notes:?}");
    host
}

fn call(h: &mut Host, name: &str, args: Value) -> (String, bool) {
    let o = h.call(name, &args, &AtomicBool::new(false), 6000);
    (o.content, o.is_error)
}

#[test]
fn sec_server_lists_short_tools() {
    let d = tmp("list");
    let h = start(&d, Vec::new());
    let defs = h.defs();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["sec_secrets", "sec_deps", "sec_binary", "sec_jwt"]);
    // prompt cost as sent (the serialized ToolDefs), with Elisa's estimate of 3.3 characters a token
    let chars = serde_json::to_string(&defs).unwrap().chars().count();
    eprintln!("sec tool defs: {chars} chars (~{} tokens)", (chars * 10).div_ceil(33));
    assert!(chars < 800, "{chars}");
    assert!(defs.iter().any(|d| d.description.contains("osv.dev")));
}

#[test]
fn secrets_are_masked_gitignore_respected_and_outside_refused() {
    let base = tmp("secrets");
    let proj = base.join("proj");
    let key = format!("AKIA{}", "Q3ZB7XK2MN4PL6RT");
    write(&proj, "src/config.py", &format!("# settings\nregion = 'eu'\naws_key = \"{key}\"\n"));
    write(&proj, "ignored/dump.txt", &format!("{key}\n"));
    write(&proj, "node_modules/x/index.js", &format!("const k = '{key}';\n"));
    write(&proj, ".gitignore", "ignored/\n");
    write(&proj, "clean.md", "nothing to see\n");
    write(&base, "outside.txt", &format!("{key}\n"));
    let mut h = start(&proj, Vec::new());
    let (out, err) = call(&mut h, "sec_secrets", json!({}));
    assert!(!err, "{out}");
    assert!(out.contains("src/config.py:3 aws-access-key AKIA"), "{out}");
    assert!(!out.contains(&key), "a full secret leaked: {out}");
    assert!(!out.contains("ignored/") && !out.contains("node_modules"), "{out}");
    assert!(out.contains("1 finding in 1 file"), "{out}");
    let (out, err) = call(&mut h, "sec_secrets", json!({"path": "../outside.txt"}));
    assert!(err && out.contains("outside the project"), "{out}");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.join("outside.txt"), proj.join("link.txt")).unwrap();
        let (out, err) = call(&mut h, "sec_secrets", json!({"path": "link.txt"}));
        assert!(err && out.contains("outside the project"), "{out}");
        // and the walk does not follow it
        let (out, _) = call(&mut h, "sec_secrets", json!({}));
        assert!(!out.contains("link.txt"), "{out}");
    }
    let (out, err) = call(&mut h, "sec_secrets", json!({"path": "clean.md"}));
    assert!(!err && out.contains("no likely secrets"), "{out}");
}

/// a fake OSV API on 127.0.0.1: answers querybatch (lodash and time vulnerable) and vulns/ID; records each request
fn fake_osv() -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            if r.read_line(&mut first).is_err() { continue; }
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() { break; }
                if let Some((k, v)) = h.split_once(':') { if k.eq_ignore_ascii_case("content-length") { len = v.trim().parse().unwrap_or(0); } }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).into_owned();
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            log2.lock().unwrap().push(format!("{path} {body}"));
            let answer = if path == "/v1/querybatch" {
                let q: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let results: Vec<Value> = q["queries"].as_array().map(|a| a.iter().map(|q| match q["package"]["name"].as_str() {
                    Some("lodash") => json!({"vulns": [{"id": "GHSA-35jh-r3h4-6jhm", "modified": "2024-01-01T00:00:00Z"}]}),
                    Some("time") => json!({"vulns": [{"id": "RUSTSEC-2020-0071"}]}),
                    _ => json!({}),
                }).collect()).unwrap_or_default();
                json!({"results": results})
            } else if path == "/v1/vulns/GHSA-35jh-r3h4-6jhm" {
                json!({"id": "GHSA-35jh-r3h4-6jhm", "summary": "Command Injection in lodash", "aliases": ["CVE-2021-23337"],
                    "database_specific": {"severity": "HIGH"},
                    "affected": [{"package": {"ecosystem": "npm", "name": "lodash"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "4.17.21"}]}]}]})
            } else if path == "/v1/vulns/RUSTSEC-2020-0071" {
                json!({"id": "RUSTSEC-2020-0071", "summary": "Potential segfault in the time crate", "aliases": ["CVE-2020-26235"],
                    "affected": [{"package": {"ecosystem": "crates.io", "name": "time"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0.0.0-0"}, {"fixed": "0.2.23"}]}]}]})
            } else { json!({"code": 5, "message": "not found"}) };
            let b = answer.to_string();
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}", b.len());
        }
    });
    (url, log)
}

#[test]
fn deps_are_checked_against_a_local_fake_osv() {
    let d = tmp("deps");
    write(&d, "Cargo.lock", "version = 4\n[[package]]\nname = \"time\"\nversion = \"0.1.45\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
        [[package]]\nname = \"secret-inhouse\"\nversion = \"0.1.0\"\n");
    write(&d, "web/package-lock.json", r#"{"lockfileVersion":3,"packages":{"":{"name":"app"},
        "node_modules/lodash":{"version":"4.17.20","resolved":"https://registry.npmjs.org/lodash/-/lodash-4.17.20.tgz"},
        "node_modules/left-pad":{"version":"1.3.0","resolved":"https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz"}}}"#);
    write(&d, "requirements.txt", "requests==2.31.0\nflask>=2\n");
    let (url, log) = fake_osv();
    let mut h = start(&d, vec![("ELISA_OSV_URL".into(), url)]);
    let (out, err) = call(&mut h, "sec_deps", json!({}));
    assert!(!err, "{out}");
    assert!(out.contains("4 packages checked") && out.contains("2 with known advisories"), "{out}");
    assert!(out.contains("lodash 4.17.20 → GHSA-35jh-r3h4-6jhm (CVE-2021-23337) [HIGH] fixed: 4.17.21 — Command Injection in lodash"), "{out}");
    assert!(out.contains("time 0.1.45 → RUSTSEC-2020-0071 (CVE-2020-26235) fixed: 0.2.23"), "{out}");
    let log = log.lock().unwrap().clone();
    assert_eq!(log.len(), 3, "{log:?}");
    // only public names + versions left this machine: no path crate, no file contents
    assert!(!log.iter().any(|l| l.contains("secret-inhouse")), "{log:?}");
    assert!(log[0].contains("\"ecosystem\":\"PyPI\"") && log[0].contains("\"version\":\"2.31.0\""), "{log:?}");
}

#[test]
fn deps_without_lockfiles_send_nothing() {
    let d = tmp("nolock");
    write(&d, "main.rs", "fn main() {}\n");
    // an override that is not on this machine is ignored, but there is nothing to send anyway
    let mut h = start(&d, vec![("ELISA_OSV_URL".into(), "http://example.com".into())]);
    let (out, err) = call(&mut h, "sec_deps", json!({}));
    assert!(!err && out.contains("no lockfiles found") && out.contains("nothing sent"), "{out}");
}

#[test]
fn binary_and_jwt() {
    let d = tmp("bin");
    std::fs::copy(env!("CARGO_BIN_EXE_elisa"), d.join("elisa-copy")).unwrap();
    let mut h = start(&d, Vec::new());
    let (out, err) = call(&mut h, "sec_binary", json!({"path": "elisa-copy"}));
    assert!(!err, "{out}");
    #[cfg(target_os = "macos")]
    assert!(out.contains("Mach-O") && out.contains("code signature:") && out.contains("libSystem"), "{out}");
    #[cfg(target_os = "linux")]
    assert!(out.contains("ELF") && out.contains("libc"), "{out}");
    let (out, err) = call(&mut h, "sec_binary", json!({"path": "/bin/ls"}));
    assert!(err && out.contains("outside the project"), "{out}");
    // {"alg":"none"} . {"sub":"42","exp":1} . (empty)
    let (out, err) = call(&mut h, "sec_jwt", json!({"token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiI0MiIsImV4cCI6MX0."}));
    assert!(!err && out.contains("NOT verified") && out.contains("WARNING") && out.contains("EXPIRED") && out.contains("sub: 42"), "{out}");
}

#[test]
fn malware_tool_is_gone() {
    // YARA scanning was removed (it returns once yara-x >= 1.19 fits the dependency graph)
    let d = tmp("nomalware");
    let h = start(&d, Vec::new());
    assert!(!h.owns("sec_malware"));
    let cfg = elisa::mcp_sec::SecCfg { root: d.clone(), max_chars: 6000 };
    let (out, err) = elisa::mcp_sec::call("sec_malware", &json!({}), &cfg);
    assert!(err && out.contains("unknown tool"), "{out}");
}

#[test]
fn config_security_switch() {
    let home = tmp("cfg-home");
    let proj = tmp("cfg-proj");
    let none = |_: &str| None;
    assert_eq!(Config::load_from(&proj, Some(home.clone()), none).unwrap().security, "on");
    write(&home, ".config/elisa/config.toml", "security = \"maybe\"\n");
    let c = Config::load_from(&proj, Some(home.clone()), none).unwrap();
    assert_eq!(c.security, "off");
    assert_eq!(c.notes.iter().filter(|n| n.contains("security")).count(), 1, "{:?}", c.notes);
    // a project may switch it off, never on
    write(&home, ".config/elisa/config.toml", "security = \"off\"\n");
    write(&proj, ".elisa/config.toml", "security = \"on\"\n");
    let c = Config::load_from(&proj, Some(home.clone()), none).unwrap();
    assert_eq!(c.security, "off");
    assert!(c.notes.iter().any(|n| n.contains("only switch the security server off")), "{:?}", c.notes);
    write(&home, ".config/elisa/config.toml", "");
    write(&proj, ".elisa/config.toml", "security = \"off\"\n");
    assert_eq!(Config::load_from(&proj, Some(home), none).unwrap().security, "off");
}
