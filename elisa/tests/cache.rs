//! Elisa's own result cache (src/cache.rs) against local stand-ins: a fake HTTP server that counts requests, real
//! PDF files, two real processes at once, the docs MCP server, and `/cache` in the REPL. Nothing touches the real
//! home: every cache lives in a temp folder.
use elisa::cache::{self, Cache, Kind};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;
use common::TempDir;

/// the process-wide cache is one per process: tests that configure it take turns
static GLOBAL: Mutex<()> = Mutex::new(());

fn tmp(tag: &str) -> TempDir { TempDir::real(&format!("cache-it-{tag}")) }

/// A tiny HTTP server: every request counted; `/slow` answers after 600 ms, `/nostore` with Cache-Control:
/// no-store, `/err` with HTTP 500, anything else with a small page.
struct Http { port: u16, hits: Arc<AtomicUsize> }
impl Http {
    fn start() -> Http {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || for c in l.incoming().flatten() {
            let h = h.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() { return; }
                loop { let mut l = String::new(); if r.read_line(&mut l).is_err() || l == "\r\n" || l.is_empty() { break; } }
                h.fetch_add(1, Ordering::SeqCst);
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                if path == "/slow" { std::thread::sleep(Duration::from_millis(600)); }
                let (status, extra, body) = match path.as_str() {
                    "/err" => ("500 Internal Server Error", "", "boom".to_string()),
                    "/nostore" => ("200 OK", "Cache-Control: no-store\r\n", "<html><body><p>private page</p></body></html>".to_string()),
                    p => ("200 OK", "", format!("<html><head><title>T</title></head><body><p>page {p}</p></body></html>")),
                };
                let mut c = c;
                let _ = write!(c, "HTTP/1.1 {status}\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        });
        Http { port, hits }
    }
    fn url(&self, p: &str) -> String { format!("http://127.0.0.1:{}{p}", self.port) }
    fn count(&self) -> usize { self.hits.load(Ordering::SeqCst) }
}

/// a minimal PDF, one Helvetica text line per page (as tests/mcp_docs.rs)
fn pdf(pages: &[&str]) -> Vec<u8> {
    let n = pages.len();
    let font = 3 + 2 * n;
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect::<Vec<_>>().join(" ")),
    ];
    for (i, t) in pages.iter().enumerate() {
        let c = format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET");
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + 2 * i));
        objs.push(format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()));
    }
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offs { out.extend(format!("{o:010} 00000 n \n").bytes()); }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    out
}

fn counts(kind: Kind) -> (u64, u64) {
    cache::session_counts().into_iter().find(|c| c.0 == kind).map(|c| (c.1, c.2)).unwrap()
}

#[test]
fn second_identical_web_fetch_does_not_hit_the_network() {
    let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let d = tmp("fetch");
    cache::configure_at(Some(&d.join("cache")), 50, "fetch-test");
    let s = Http::start();
    let a = elisa::web::fetch_local_for_tests(&s.url("/a"), 8000).unwrap();
    assert!(a.contains("page /a"), "{a}");
    assert_eq!(s.count(), 1);
    // the same page again, spelled with an upper-case scheme and a fragment: from the cache
    let b = elisa::web::fetch_local_for_tests(&s.url("/a#top").replace("http://", "HTTP://"), 8000).unwrap();
    assert_eq!(a, b);
    assert_eq!(s.count(), 1, "the second fetch was served from the cache");
    assert_eq!(counts(Kind::WebFetch), (1, 1));
    // no-store is respected, errors are not cached
    for _ in 0..2 { elisa::web::fetch_local_for_tests(&s.url("/nostore"), 8000).unwrap(); }
    assert_eq!(s.count(), 3);
    for _ in 0..2 { assert!(elisa::web::fetch_local_for_tests(&s.url("/err"), 8000).is_err()); }
    assert_eq!(s.count(), 5);
    // a model's web_fetch of a local address is still refused, even though that address is in the cache
    assert!(elisa::web::fetch(&s.url("/a")).is_err());
    assert_eq!(s.count(), 5);
    // `/cache` shows the entry and this session's hit rate; `/cache clear web_fetch` empties it
    let r = cache::command("").unwrap();
    assert!(r.contains("web_fetch") && r.contains("1 entries") && r.contains("hits 1 / misses 5"), "{r}");
    assert!(r.contains("MCP tool lists") && r.contains("sandbox deps"), "{r}");
    assert!(cache::stats_line().contains("1 hits"), "{}", cache::stats_line());
    assert!(cache::command("clear web_fetch").unwrap().contains("1 entries"));
    elisa::web::fetch_local_for_tests(&s.url("/a"), 8000).unwrap();
    assert_eq!(s.count(), 6, "cleared: fetched again");
    assert!(cache::command("clear nope").is_err());
    cache::configure_at(None, 0, "");
    // off: every call goes out
    elisa::web::fetch_local_for_tests(&s.url("/a"), 8000).unwrap();
    assert_eq!(s.count(), 7);
}

#[test]
fn docs_hit_on_the_second_read_and_miss_after_the_file_changes() {
    let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let d = tmp("docs");
    cache::configure_at(Some(&d.join("cache")), 50, "docs-test");
    let f = d.join("report.pdf");
    std::fs::write(&f, pdf(&["Alpha page one", "Beta page two"])).unwrap();
    let t1 = cache::docs_text(&f).unwrap();
    assert!(t1.contains("Alpha") && t1.contains("--- page 2 ---"), "{t1}");
    assert_eq!(counts(Kind::Docs), (0, 1));
    let t2 = cache::docs_text(&f).unwrap();
    assert_eq!(t1, t2);
    assert_eq!(counts(Kind::Docs), (1, 1), "second read from the cache");
    // the attach path and the docs tool share the entry (a copy of the same bytes elsewhere: still a hit)
    let copy = d.join("copy.pdf");
    std::fs::copy(&f, &copy).unwrap();
    assert_eq!(elisa::mcp_docs::pdf_pages(&copy).unwrap().len(), 2);
    assert_eq!(counts(Kind::Docs), (2, 1));
    // the file changes: its hash changes, a miss, new text
    std::fs::write(&f, pdf(&["Gamma changed"])).unwrap();
    let t3 = cache::docs_text(&f).unwrap();
    assert!(t3.contains("Gamma") && !t3.contains("Alpha"), "{t3}");
    assert_eq!(counts(Kind::Docs), (2, 2));
    // prefetch fills the cache in the background; the read after it is a hit
    let g = d.join("later.pdf");
    std::fs::write(&g, pdf(&["Prefetched text"])).unwrap();
    cache::prefetch_docs(g.clone()).unwrap().join().unwrap();
    assert_eq!(counts(Kind::Docs), (2, 3));
    assert!(cache::docs_text(&g).unwrap().contains("Prefetched"));
    assert_eq!(counts(Kind::Docs), (3, 3));
    // entries are private
    use std::os::unix::fs::PermissionsExt;
    for e in std::fs::read_dir(d.join("cache/entries/docs")).unwrap().flatten() {
        assert_eq!(e.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(std::fs::metadata(d.join("cache")).unwrap().permissions().mode() & 0o777, 0o700);
    cache::configure_at(None, 0, "");
}

#[test]
fn docs_server_shares_the_session_cache() {
    use elisa::mcp_host::{Host, ServerCfg};
    let d = tmp("server");
    std::fs::write(d.join("r.pdf"), pdf(&["Server text"])).unwrap();
    let cdir = d.join("cache");
    let env = vec![(cache::ENV_SESSION.to_string(), "srv".to_string()), (cache::ENV_DIR.to_string(), cdir.display().to_string())];
    let cfg = ServerCfg { name: "docs".into(), command: vec![env!("CARGO_BIN_EXE_elisa").into(), "--mcp-serve".into(), "docs".into(),
        "--root".into(), d.display().to_string()], env, ..ServerCfg::default() };
    let (mut h, notes) = Host::start(&[cfg], &|n| ["read", "write", "edit", "bash"].contains(&n));
    assert!(notes.is_empty(), "{notes:?}");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for _ in 0..2 {
        let o = h.call("docs_pdf", &serde_json::json!({"path": "r.pdf"}), &cancel, 6000);
        assert!(!o.is_error && o.content.contains("Server text"), "{}", o.content);
    }
    drop(h);
    // the server's own stats file: one miss, one hit
    let stats: Vec<_> = std::fs::read_dir(cdir.join("stats")).unwrap().flatten().collect();
    assert_eq!(stats.len(), 1);
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(stats[0].path()).unwrap()).unwrap();
    assert_eq!(v["docs"], serde_json::json!([1, 1]), "{v}");
}

/// The child side of the two-process tests: runs only when ELISA_CACHE_CHILD is set (see `two_processes_*`).
#[test]
fn child_worker() {
    let Ok(role) = std::env::var("ELISA_CACHE_CHILD") else { return };
    let dir = PathBuf::from(std::env::var("ELISA_CACHE_CHILD_DIR").unwrap());
    // start together: wait for the go file
    let go = dir.join("go");
    while !go.exists() { std::thread::sleep(Duration::from_millis(5)); }
    match role.as_str() {
        "fetch" => {
            cache::configure_at(Some(&dir.join("cache")), 50, "procs");
            let t = elisa::web::fetch_local_for_tests(&std::env::var("ELISA_CACHE_CHILD_URL").unwrap(), 8000).unwrap();
            assert!(t.contains("page /slow"));
        }
        id => {
            let c = Cache::open(&dir.join("cache"), 200, "procs").unwrap();
            for i in 0..150 {
                let body = format!("{id}:{}", "x".repeat(2000 + i));
                c.put(Kind::Docs, &format!("same-{i}"), &body).unwrap();
                c.put(Kind::Docs, &format!("own-{id}-{i}"), &body).unwrap();
                // read back while the other writes the same keys: always a whole entry of one of the two writers
                let got = c.lookup(Kind::Docs, &format!("same-{i}")).expect("an entry is there");
                assert!(got.starts_with("A:") || got.starts_with("B:"), "torn: {}", &got[..20.min(got.len())]);
                assert_eq!(got.len(), 2 + 2000 + i);
            }
        }
    }
}

fn spawn_child(role: &str, dir: &Path, url: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_worker", "--nocapture", "--test-threads=1"])
        .env("ELISA_CACHE_CHILD", role).env("ELISA_CACHE_CHILD_DIR", dir).env("ELISA_CACHE_CHILD_URL", url)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
}

fn wait_ok(c: std::process::Child) {
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "child failed: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
}

#[test]
fn two_processes_write_the_same_and_different_keys_at_once() {
    let d = tmp("procs-write");
    let (a, b) = (spawn_child("A", &d, ""), spawn_child("B", &d, ""));
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(d.join("go"), "").unwrap();
    wait_ok(a);
    wait_ok(b);
    let c = Cache::open(&d.join("cache"), 200, "check").unwrap();
    for i in 0..150 {
        let s = c.lookup(Kind::Docs, &format!("same-{i}")).unwrap();
        assert!(s.starts_with("A:") || s.starts_with("B:"));
        for id in ["A", "B"] { assert!(c.lookup(Kind::Docs, &format!("own-{id}-{i}")).unwrap().starts_with(&format!("{id}:"))); }
    }
    let (_, n, _) = c.usage().into_iter().find(|u| u.0 == Kind::Docs).unwrap();
    assert_eq!(n, 450, "150 shared + 2 × 150 own");
    let tmps = std::fs::read_dir(d.join("cache/entries/docs")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
    assert_eq!(tmps, 0);
}

#[test]
fn two_processes_asking_for_the_same_slow_url_fetch_it_once() {
    let d = tmp("procs-fetch");
    let s = Http::start();
    let url = s.url("/slow");
    let (a, b) = (spawn_child("fetch", &d, &url), spawn_child("fetch", &d, &url));
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    std::fs::write(d.join("go"), "").unwrap();
    wait_ok(a);
    wait_ok(b);
    assert_eq!(s.count(), 1, "in-flight dedup: one request for two processes");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[test]
fn cache_command_in_the_repl() {
    let d = tmp("repl");
    let home = d.join("_home");
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), "cache_max_mb = 64\n").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
    cmd.args(["--cwd"]).arg(&d).args(["--model", "test-m", "--base-url", "http://127.0.0.1:9", "--max-ctx", "8000"])
        .current_dir(&d).env("HOME", &home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, _) in std::env::vars() { if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") { cmd.env_remove(&k); } }
    let mut ch = cmd.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(b"/cache\n/stats\n/cache clear docs\n/exit\n").unwrap();
    let mut out = String::new();
    ch.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    let mut err = String::new();
    ch.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    ch.wait().unwrap();
    let all = format!("{out}{err}");
    assert!(all.contains("cache ~/.elisa/cache") && all.contains("of 64.0 MB"), "{all}");
    assert!(all.contains("web_fetch") && all.contains("web_search") && all.contains("docs"), "{all}");
    assert!(all.contains("cache: 0 hits · 0 misses"), "{all}");
    assert!(all.contains("cache cleared (docs): 0 entries"), "{all}");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(home.join(".elisa/cache")).unwrap().permissions().mode() & 0o777, 0o700);
}
