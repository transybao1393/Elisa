//! Elisa's own result cache, `~/.elisa/cache`: backend-independent (it never relies on a model server's KV / prefix
//! cache, which llama.cpp, a raw GGUF and most other backends do not have) and safe for several Elisa processes at
//! once (two app windows, the app plus a terminal, and each session's own MCP server processes).
//!
//! What it holds (`Kind`), each content-addressed by sha256(kind, version, key):
//! - `web_fetch`: a fetched page's text per normalised URL (and size cap), 1 h. Not cached: errors, a page sent with
//!   `Cache-Control: no-store`.
//! - `web_search`: the result list per (engine, query), 15 min.
//! - `docs`: the extracted text of PDF, spreadsheet and Word .docx files, keyed by the file's content (sha256) and
//!   the extractor version. No TTL: a changed file has another hash, so an entry is never stale.
//!
//! Never cached: model answers and chat replies, local file reads by `read`, and any result that holds a secret
//! (`secrets::redact` finds one: the result is not stored at all).
//!
//! Layout (dirs 0700, files 0600):
//! - `lock`: an flock taken (exclusive) by writers only, around rename-into-place, eviction and clearing (the mem
//!   crate's approach; its lock type is private to that crate, so the few lines are repeated here)
//! - `entries/<kind>/<hash>.json`: one entry, `{v, kind, key, ver, created, ttl, body}`. Written to a temp file in
//!   the same directory and renamed into place, so a reader (who takes no lock) never sees half an entry. The file's
//!   mtime is its last use (a hit touches it); its length is its size. A corrupt entry is ignored and removed.
//! - `inflight/<hash>.lock`: in-flight dedup. The one process (or thread) that holds this key's flock does the work;
//!   the others wait (polling, up to a timeout) and then read its result, or do the work themselves when it left none.
//! - `stats/<session>-<pid>.json`: hits and misses of each process of a session (the session's MCP servers count
//!   theirs), summed by `/cache` and `/stats`.
//!
//! Limits: `cache_max_mb` (default 200): past it the least recently used entries go, down to 90 %. `cache = false`
//! switches it off (a project config can only switch it off).
//!
//! The cache is off in a process until it is configured: the session does it from config (`configure`), Elisa's MCP
//! servers from the environment the session gives them (`child_env` → `from_env`). So tests and a bare
//! `--mcp-serve` never write to the user's home.
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// environment for Elisa's MCP servers: the session id (its presence switches the cache on in the server)
pub const ENV_SESSION: &str = "ELISA_CACHE_SESSION";
/// environment: the size cap in MB
pub const ENV_MAX_MB: &str = "ELISA_CACHE_MAX_MB";
/// environment: the cache directory (the session passes its own, so a server under another HOME agrees)
pub const ENV_DIR: &str = "ELISA_CACHE_DIR";
/// default size cap (config `cache_max_mb`)
pub const DEFAULT_MAX_MB: u64 = 200;
/// the docs extractor version: bump when the PDF / spreadsheet / docx text changes for the same file
pub const DOCS_VERSION: &str = "pdf-extract 0.12.1 · calamine 0.38.0 · docx 1";
/// how long a request waits for another process doing the same work before doing it itself
const WAIT_WEB: Duration = Duration::from_secs(30);
const WAIT_DOCS: Duration = Duration::from_secs(120);
/// a single entry larger than this share of the cap is not stored
const ENTRY_SHARE: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind { WebFetch, WebSearch, Docs }

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::WebFetch, Kind::WebSearch, Kind::Docs];
    pub fn name(self) -> &'static str {
        match self { Kind::WebFetch => "web_fetch", Kind::WebSearch => "web_search", Kind::Docs => "docs" }
    }
    pub fn parse(s: &str) -> Option<Kind> { Kind::ALL.into_iter().find(|k| k.name() == s) }
    /// seconds an entry lives; None = until its content changes (the key is the content's hash)
    pub fn ttl(self) -> Option<u64> {
        match self { Kind::WebFetch => Some(3600), Kind::WebSearch => Some(900), Kind::Docs => None }
    }
    fn version(self) -> &'static str {
        match self { Kind::WebFetch => "1", Kind::WebSearch => "1", Kind::Docs => DOCS_VERSION }
    }
    fn wait(self) -> Duration { if self == Kind::Docs { WAIT_DOCS } else { WAIT_WEB } }
    fn policy(self) -> &'static str {
        match self { Kind::WebFetch => "1 h", Kind::WebSearch => "15 min", Kind::Docs => "until the file changes" }
    }
}

fn now_s() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) }

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

/// the sha256 of a file's content, hex
pub fn file_hash(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// an exclusive flock on an open file, released on drop (closing the file releases it too)
struct Flock(File);
impl Flock {
    fn wait(f: File) -> io::Result<Flock> {
        // SAFETY: flock on our own open descriptor
        while unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted { return Err(e); }
        }
        Ok(Flock(f))
    }
    fn try_take(f: &File) -> bool {
        // SAFETY: as above, non-blocking
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }
}
impl Drop for Flock {
    fn drop(&mut self) { unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN); } }
}

fn mkdir(p: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(p)?;
    // an existing directory from before: private too
    let m = fs::metadata(p)?;
    if m.permissions().mode() & 0o777 != 0o700 { fs::set_permissions(p, fs::Permissions::from_mode(0o700))?; }
    Ok(())
}

fn open_private(p: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(p)
}

/// one cache directory
#[derive(Debug)]
pub struct Cache { dir: PathBuf, max_bytes: u64, session: String }

/// what `get` found
enum Found { Hit(String), Miss }

impl Cache {
    /// a cache at `dir` (created 0700) with a size cap
    pub fn open(dir: &Path, max_mb: u64, session: &str) -> io::Result<Cache> {
        for d in ["", "entries", "inflight", "stats"] { mkdir(&dir.join(d))?; }
        for k in Kind::ALL { mkdir(&dir.join("entries").join(k.name()))?; }
        Ok(Cache { dir: dir.to_path_buf(), max_bytes: max_mb.max(1) << 20, session: session.to_string() })
    }

    pub fn dir(&self) -> &Path { &self.dir }
    pub fn max_bytes(&self) -> u64 { self.max_bytes }

    fn hash(kind: Kind, key: &str) -> String {
        let mut h = Sha256::new();
        for part in [kind.name(), kind.version(), key] { h.update(part.as_bytes()); h.update([0u8]); }
        hex(&h.finalize())
    }

    pub fn entry_path(&self, kind: Kind, key: &str) -> PathBuf {
        self.dir.join("entries").join(kind.name()).join(format!("{}.json", Cache::hash(kind, key)))
    }

    fn write_lock(&self) -> io::Result<Flock> { Flock::wait(open_private(&self.dir.join("lock"))?) }

    /// a stored entry still valid for (kind, key); touches its last use. No lock (rename keeps entries whole).
    fn get(&self, kind: Kind, key: &str) -> Found {
        let p = self.entry_path(kind, key);
        let Ok(raw) = fs::read(&p) else { return Found::Miss };
        let v: Option<Value> = serde_json::from_slice(&raw).ok();
        let ok = v.as_ref().is_some_and(|v| v["v"] == 1 && v["kind"] == kind.name() && v["key"] == key && v["ver"] == kind.version()
            && v["body"].is_string() && v["created"].is_u64());
        if !ok {
            // corrupt (or a hash collision, or a foreign file): ignored, and removed if it is still the same file
            self.remove_if_same(&p, &raw);
            return Found::Miss;
        }
        let v = v.unwrap();
        if let Some(ttl) = v["ttl"].as_u64() {
            if now_s() >= v["created"].as_u64().unwrap_or(0).saturating_add(ttl) {
                self.remove_if_same(&p, &raw);
                return Found::Miss;
            }
        }
        // last use = mtime; another process may have renamed a fresh entry over it meanwhile: touching either is fine
        if let Ok(f) = OpenOptions::new().write(true).open(&p) { let _ = f.set_modified(SystemTime::now()); }
        Found::Hit(v["body"].as_str().unwrap_or("").to_string())
    }

    /// removes `p` under the write lock when it still holds `raw` (a writer may have replaced it since)
    fn remove_if_same(&self, p: &Path, raw: &[u8]) {
        if let Ok(_l) = self.write_lock() {
            if fs::read(p).ok().as_deref() == Some(raw) { let _ = fs::remove_file(p); }
        }
    }

    /// The text for (kind, key), or None. Records a hit or nothing (a miss is counted where the work is done).
    pub fn lookup(&self, kind: Kind, key: &str) -> Option<String> {
        match self.get(kind, key) { Found::Hit(b) => { note(self, kind, true); Some(b) } Found::Miss => None }
    }

    /// Stores `body` for (kind, key): a temp file, then (under the write lock) renamed into place, then eviction.
    pub fn put(&self, kind: Kind, key: &str, body: &str) -> io::Result<()> {
        let e = json!({"v": 1, "kind": kind.name(), "key": key, "ver": kind.version(), "created": now_s(), "ttl": kind.ttl(), "body": body});
        let data = serde_json::to_vec(&e).map_err(io::Error::other)?;
        if data.len() as u64 > self.max_bytes / ENTRY_SHARE { return Ok(()); }
        let p = self.entry_path(kind, key);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let tmp = p.with_file_name(format!(".{}.{}.{nanos}.{:?}.tmp", p.file_stem().and_then(|s| s.to_str()).unwrap_or("e"),
            std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        if let Err(e) = f.write_all(&data) { let _ = fs::remove_file(&tmp); return Err(e); }
        drop(f);
        let _l = self.write_lock()?;
        if let Err(e) = fs::rename(&tmp, &p) { let _ = fs::remove_file(&tmp); return Err(e); }
        self.evict_locked();
        Ok(())
    }

    /// every entry: (kind, path, size, last use)
    fn entries(&self) -> Vec<(Kind, PathBuf, u64, SystemTime)> {
        let mut out = Vec::new();
        for k in Kind::ALL {
            let Ok(rd) = fs::read_dir(self.dir.join("entries").join(k.name())) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") { continue; }
                if let Ok(m) = e.metadata() { out.push((k, p, m.len(), m.modified().unwrap_or(UNIX_EPOCH))); }
            }
        }
        out
    }

    /// With the write lock held: the least recently used entries go until the total is at most 90 % of the cap;
    /// temp files left by a crashed writer (older than 10 min) go too.
    fn evict_locked(&self) {
        let mut all = self.entries();
        let mut total: u64 = all.iter().map(|e| e.2).sum();
        if total > self.max_bytes {
            all.sort_by_key(|e| e.3);
            let goal = self.max_bytes / 10 * 9;
            for (_, p, n, _) in &all {
                if total <= goal { break; }
                if fs::remove_file(p).is_ok() { total = total.saturating_sub(*n); }
            }
        }
        let old = SystemTime::now() - Duration::from_secs(600);
        for k in Kind::ALL {
            let Ok(rd) = fs::read_dir(self.dir.join("entries").join(k.name())) else { continue };
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".tmp") && e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// Removes every entry (of one kind): (entries, bytes) removed.
    pub fn clear(&self, kind: Option<Kind>) -> io::Result<(usize, u64)> {
        let _l = self.write_lock()?;
        let (mut n, mut b) = (0, 0);
        for (k, p, len, _) in self.entries() {
            if kind.is_some_and(|w| w != k) { continue; }
            if fs::remove_file(&p).is_ok() { n += 1; b += len; }
        }
        Ok((n, b))
    }

    /// per kind: (entries, bytes)
    pub fn usage(&self) -> Vec<(Kind, usize, u64)> {
        let all = self.entries();
        Kind::ALL.into_iter().map(|k| {
            let es: Vec<_> = all.iter().filter(|e| e.0 == k).collect();
            (k, es.len(), es.iter().map(|e| e.2).sum())
        }).collect()
    }

    /// The cached text for (kind, key), or `work`'s. `work` returns (text, cacheable); an error, an uncacheable text
    /// or one holding a secret is not stored. In-flight dedup: while one caller (any process or thread) does the work
    /// for a key, the others wait up to `wait` and take its result; when it left none (an error, not cacheable) or
    /// the wait ran out, they do the work themselves.
    pub fn get_or_compute(&self, kind: Kind, key: &str, wait: Duration, work: impl FnOnce() -> Result<(String, bool), String>)
        -> Result<String, String> {
        if let Some(b) = self.lookup(kind, key) { return Ok(b); }
        let lock_path = self.dir.join("inflight").join(format!("{}.lock", Cache::hash(kind, key)));
        let held = open_private(&lock_path).ok().and_then(|f| {
            let until = std::time::Instant::now() + wait;
            loop {
                if Flock::try_take(&f) { return Some(Flock(f)); }
                if std::time::Instant::now() >= until { return None; }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        // the one who held the key before us may have stored it
        if held.is_some() { if let Some(b) = self.lookup(kind, key) { return Ok(b); } }
        note(self, kind, false);
        let r = work();
        if let Ok((body, true)) = &r {
            if crate::secrets::redact(body).1 == 0 { let _ = self.put(kind, key, body); }
        }
        drop(held);
        r.map(|(b, _)| b)
    }
}

// ---------- the process's cache ----------

static GLOBAL: RwLock<Option<Arc<Cache>>> = RwLock::new(None);

/// the configured cache of this process, if any
pub fn global() -> Option<Arc<Cache>> { GLOBAL.read().ok()?.clone() }

/// the default cache directory, `~/.elisa/cache`
pub fn default_dir() -> Option<PathBuf> { std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elisa/cache")) }

/// The session switches its cache on (config `cache`, `cache_max_mb`); off when `enabled` is false or the
/// directory cannot be created. Stats files older than two days (sessions long gone) are removed.
pub fn configure(enabled: bool, max_mb: u64) {
    let c = if enabled { default_dir().and_then(|d| Cache::open(&d, max_mb, &std::process::id().to_string()).ok()) } else { None };
    if let Some(c) = &c { prune_stats(c); }
    set(c);
}

/// tests: a cache at `dir` (or none)
pub fn configure_at(dir: Option<&Path>, max_mb: u64, session: &str) {
    set(dir.and_then(|d| Cache::open(d, max_mb, session).ok()));
}

fn set(c: Option<Cache>) {
    // a new session's counts start at zero
    if let Ok(mut s) = STATS.lock() { *s = None; }
    if let Ok(mut g) = GLOBAL.write() { *g = c.map(Arc::new); }
}

/// In one of Elisa's MCP servers: the cache the session handed over in the environment, else off.
pub fn from_env() {
    let (Some(session), Some(dir)) = (std::env::var(ENV_SESSION).ok().filter(|s| !s.is_empty()), std::env::var_os(ENV_DIR)) else { return };
    let mb = std::env::var(ENV_MAX_MB).ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_MB);
    configure_at(Some(Path::new(&dir)), mb, &session);
}

/// what the session gives its own MCP servers (web, docs) so they share its cache: nothing when the cache is off
pub fn child_env() -> Vec<(String, String)> {
    let Some(c) = global() else { return Vec::new() };
    vec![(ENV_SESSION.into(), c.session.clone()), (ENV_DIR.into(), c.dir.display().to_string()), (ENV_MAX_MB.into(), (c.max_bytes >> 20).to_string())]
}

/// `work`'s text through the cache when it is on (see `Cache::get_or_compute`), else `work` alone
pub fn cached(kind: Kind, key: &str, work: impl FnOnce() -> Result<(String, bool), String>) -> Result<String, String> {
    match global() { Some(c) => c.get_or_compute(kind, key, kind.wait(), work), None => work().map(|(t, _)| t) }
}

/// whether any string in a JSON value holds a secret
fn has_secret(v: &Value) -> bool {
    match v {
        Value::String(s) => crate::secrets::redact(s).1 > 0,
        Value::Array(a) => a.iter().any(has_secret),
        Value::Object(o) => o.values().any(has_secret),
        _ => false,
    }
}

/// A document extractor's result (`part`: which extractor, e.g. "pdf_pages") cached by the file's content hash and
/// `DOCS_VERSION`. Without a cache, or when the file cannot be hashed, `extract` runs as it is.
pub fn docs_json<T: Serialize + DeserializeOwned>(path: &Path, part: &str, extract: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let Some(c) = global() else { return extract() };
    let Ok(h) = file_hash(path) else { return extract() };
    let key = format!("{part}:{h}");
    let mut fresh: Option<T> = None;
    let mut extract = Some(extract);
    let body = c.get_or_compute(Kind::Docs, &key, Kind::Docs.wait(), || {
        let v = extract.take().ok_or("docs cache: extractor already used")?()?;
        let j = serde_json::to_value(&v).map_err(|e| e.to_string())?;
        let ok = !has_secret(&j);
        let s = j.to_string();
        fresh = Some(v);
        Ok((s, ok))
    })?;
    match fresh {
        Some(v) => Ok(v),
        None => match serde_json::from_str(&body) {
            Ok(v) => Ok(v),
            // a body that no longer fits the type (should not happen: DOCS_VERSION): drop it and extract
            Err(_) => { let _ = c.clear_one(Kind::Docs, &key); extract.take().ok_or("docs cache: extractor already used")?() }
        },
    }
}

/// whether the docs cache holds `part` of this file's current content (no hit is counted)
pub fn docs_has(path: &Path, part: &str) -> bool {
    let Some(c) = global() else { return false };
    let Ok(h) = file_hash(path) else { return false };
    matches!(c.get(Kind::Docs, &format!("{part}:{h}")), Found::Hit(_))
}

impl Cache {
    fn clear_one(&self, kind: Kind, key: &str) -> io::Result<()> {
        let _l = self.write_lock()?;
        fs::remove_file(self.entry_path(kind, key))
    }
}

/// The extracted text of a PDF, spreadsheet (xlsx / xlsm / xlsb / xls / ods / csv / tsv) or Word .docx, cached by
/// content: the API for document upload. PDF pages are separated by `--- page N ---` lines, sheets by
/// `--- sheet NAME ---` with tab-separated cells.
pub fn docs_text(path: &Path) -> Result<String, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "pdf" {
        let pages = crate::mcp_docs::pdf_pages(path)?;
        if pages.len() == 1 { return Ok(pages[0].clone()); }
        return Ok(pages.iter().enumerate().map(|(i, p)| format!("--- page {} ---\n{}", i + 1, if p.is_empty() { "[no text on this page]" } else { p }))
            .collect::<Vec<_>>().join("\n"));
    }
    if crate::mcp_docs::is_sheet(path) {
        let sheets = crate::mcp_docs::sheet_rows(path)?;
        let one = sheets.len() == 1;
        let mut out = String::new();
        for (name, rows) in sheets {
            if !one || !name.is_empty() { out.push_str(&format!("--- sheet {name} ---\n")); }
            for r in rows { out.push_str(&r.join("\t")); out.push('\n'); }
        }
        return Ok(out.trim_end().to_string());
    }
    if ext == "docx" { return crate::attach::docx_text(path); }
    Err(format!("{}: not a PDF, spreadsheet or .docx", path.display()))
}

/// Fills the docs cache for `path` on a background thread (the same extractors `attach::read` and the docs tools
/// use, so their later call is a hit, or waits for this one through the in-flight lock). For a file the user just
/// attached, before they press Enter. Does nothing when the cache is off.
pub fn prefetch_docs(path: PathBuf) -> Option<std::thread::JoinHandle<()>> {
    global()?;
    std::thread::Builder::new().name("elisa-prefetch".into()).spawn(move || {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= crate::attach::MAX_BYTES) { return; }
        if ext == "pdf" { let _ = crate::mcp_docs::pdf_pages(&path); }
        else if crate::mcp_docs::is_sheet(&path) && ext != "csv" && ext != "tsv" { let _ = crate::mcp_docs::sheet_rows(&path); }
        else if ext == "docx" { let _ = crate::attach::docx_text(&path); }
    }).ok()
}

/// Fills the web_fetch cache for `url` on a background thread. The caller decides that this fetch is allowed (web
/// on, the user's own link; a model's `web_fetch` still asks through MCP): this only reads public pages
/// (`web::fetch`'s checks) and does nothing when the cache is off.
pub fn prefetch_url(url: String) -> Option<std::thread::JoinHandle<()>> {
    global()?;
    std::thread::Builder::new().name("elisa-prefetch".into()).spawn(move || { let _ = crate::web::fetch(&url); }).ok()
}

// ---------- hits and misses ----------

static STATS: Mutex<Option<HashMap<&'static str, (u64, u64)>>> = Mutex::new(None);

/// counts a hit or a miss in this process and writes the process's stats file
fn note(c: &Cache, kind: Kind, hit: bool) {
    let snapshot = {
        let Ok(mut g) = STATS.lock() else { return };
        let m = g.get_or_insert_with(HashMap::new);
        let e = m.entry(kind.name()).or_default();
        if hit { e.0 += 1 } else { e.1 += 1 }
        m.iter().map(|(k, (h, mi))| (k.to_string(), json!([h, mi]))).collect::<serde_json::Map<_, _>>()
    };
    let p = c.dir.join("stats").join(format!("{}-{}.json", c.session, std::process::id()));
    let tmp = p.with_extension(format!("{:?}.tmp", std::thread::current().id()).replace(['(', ')'], ""));
    if OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)
        .and_then(|mut f| f.write_all(Value::Object(snapshot).to_string().as_bytes())).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

fn prune_stats(c: &Cache) {
    let old = SystemTime::now() - Duration::from_secs(2 * 86400);
    let Ok(rd) = fs::read_dir(c.dir.join("stats")) else { return };
    for e in rd.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) { let _ = fs::remove_file(e.path()); }
    }
}

/// hits and misses per kind this session (the session's own process and its MCP servers)
pub fn session_counts() -> Vec<(Kind, u64, u64)> {
    let Some(c) = global() else { return Kind::ALL.into_iter().map(|k| (k, 0, 0)).collect() };
    let mut sum: HashMap<&str, (u64, u64)> = HashMap::new();
    let prefix = format!("{}-", c.session);
    if let Ok(rd) = fs::read_dir(c.dir.join("stats")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !n.starts_with(&prefix) || !n.ends_with(".json") { continue; }
            let Ok(v) = fs::read(e.path()).map(|b| serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null)) else { continue };
            for k in Kind::ALL {
                let s = sum.entry(k.name()).or_default();
                s.0 += v[k.name()][0].as_u64().unwrap_or(0);
                s.1 += v[k.name()][1].as_u64().unwrap_or(0);
            }
        }
    }
    Kind::ALL.into_iter().map(|k| { let (h, m) = sum.get(k.name()).copied().unwrap_or_default(); (k, h, m) }).collect()
}

/// the total hits this session (to tell, around a tool call, that it was served from the cache)
pub fn session_hits() -> Vec<(Kind, u64)> { session_counts().into_iter().map(|(k, h, _)| (k, h)).collect() }

/// the kinds whose hit count grew between two `session_hits` snapshots
pub fn new_hits(before: &[(Kind, u64)], after: &[(Kind, u64)]) -> Vec<&'static str> {
    after.iter().filter(|(k, h)| *h > before.iter().find(|(b, _)| b == k).map_or(0, |x| x.1)).map(|(k, _)| k.name()).collect()
}

fn size(b: u64) -> String {
    if b >= 1 << 20 { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) } else if b >= 1024 { format!("{} KB", b.div_ceil(1024)) } else { format!("{b} B") }
}

/// `/stats`'s cache line
pub fn stats_line() -> String {
    if global().is_none() { return "cache: off (config cache = false)".into(); }
    let c = session_counts();
    let (h, m): (u64, u64) = c.iter().fold((0, 0), |a, x| (a.0 + x.1, a.1 + x.2));
    let per: Vec<String> = c.iter().filter(|x| x.1 + x.2 > 0).map(|x| format!("{} {}/{}", x.0.name(), x.1, x.2)).collect();
    format!("cache: {h} hits · {m} misses{}", if per.is_empty() { String::new() } else { format!(" ({})", per.join(", ")) })
}

/// `/cache` and `/cache clear [kind]`
pub fn command(arg: &str) -> Result<String, String> {
    let mut words = arg.split_whitespace();
    match words.next() {
        None => Ok(report()),
        Some("clear") => {
            let kind = match words.next() {
                None | Some("all") => None,
                Some(w) => Some(Kind::parse(w).ok_or_else(|| format!("no cache kind {w:?} (web_fetch, web_search, docs)"))?),
            };
            let c = global().ok_or("the cache is off (config cache = false)")?;
            let (n, b) = c.clear(kind).map_err(|e| format!("cache: {e}"))?;
            Ok(format!("cache cleared{}: {n} entries, {}", kind.map_or(String::new(), |k| format!(" ({})", k.name())), size(b)))
        }
        Some(w) => Err(format!("usage: /cache · /cache clear [web_fetch|web_search|docs] (not {w:?})")),
    }
}

/// `/cache`: per kind entries, size, this session's hit rate; and the other caches, kept as they are
pub fn report() -> String {
    let mut out = String::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let tilde = |p: &Path| p.strip_prefix(&home).map_or_else(|_| p.display().to_string(), |r| format!("~/{}", r.display()));
    match global() {
        None => out.push_str("cache: off (config cache = false)\n"),
        Some(c) => {
            let usage = c.usage();
            let total: u64 = usage.iter().map(|u| u.2).sum();
            out.push_str(&format!("cache {} · {} of {} · results only, never model replies\n", tilde(&c.dir), size(total), size(c.max_bytes)));
            let counts = session_counts();
            for (k, n, b) in usage {
                let (_, h, m) = counts.iter().find(|x| x.0 == k).copied().unwrap_or((k, 0, 0));
                let rate = (h * 100).checked_div(h + m).map_or("-".to_string(), |r| format!("{r}%"));
                out.push_str(&format!("  {:<11} {:>4} entries {:>9}  hits {h} / misses {m} ({rate} this session) · keeps {}\n", k.name(), n, size(b), k.policy()));
            }
        }
    }
    out.push_str("other caches (kept as they are):\n");
    let lists = home.join(crate::pins::LISTS_FILE);
    out.push_str(&format!("  MCP tool lists    {} ({})\n", tilde(&lists), fs::metadata(&lists).map_or("none yet".into(), |m| size(m.len()))));
    out.push_str(&format!("  sandbox deps      docker volumes {}<language> inside the Colima VM (/sandbox)\n", crate::mcp_run::VOLUME_PREFIX));
    out.push_str("  memory            hot vectors and the embedding vocab in the memory store (config memory_dir)\n");
    out.push_str("/cache clear [web_fetch|web_search|docs]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-cache-{tag}-{}-{}", std::process::id(), now_s()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn put_get_and_private_modes() {
        let d = tmp("modes");
        let c = Cache::open(&d, 10, "t").unwrap();
        assert!(c.lookup(Kind::Docs, "k").is_none());
        c.put(Kind::Docs, "k", "hello").unwrap();
        assert_eq!(c.lookup(Kind::Docs, "k").as_deref(), Some("hello"));
        assert_eq!(fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(c.entry_path(Kind::Docs, "k")).unwrap().permissions().mode() & 0o777, 0o600);
        // another kind, another key: separate entries
        assert!(c.lookup(Kind::WebFetch, "k").is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn ttl_expires() {
        let d = tmp("ttl");
        let c = Cache::open(&d, 10, "t").unwrap();
        c.put(Kind::WebSearch, "q", "hits").unwrap();
        assert!(c.lookup(Kind::WebSearch, "q").is_some());
        // age it past 15 min by rewriting `created`
        let p = c.entry_path(Kind::WebSearch, "q");
        let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        v["created"] = json!(now_s() - 901);
        fs::write(&p, v.to_string()).unwrap();
        assert!(c.lookup(Kind::WebSearch, "q").is_none());
        assert!(!p.exists(), "an expired entry is removed");
        // docs never expire
        c.put(Kind::Docs, "d", "x").unwrap();
        let p = c.entry_path(Kind::Docs, "d");
        let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        v["created"] = json!(1);
        fs::write(&p, v.to_string()).unwrap();
        assert!(c.lookup(Kind::Docs, "d").is_some());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_entry_is_ignored_and_removed() {
        let d = tmp("corrupt");
        let c = Cache::open(&d, 10, "t").unwrap();
        c.put(Kind::Docs, "k", "good").unwrap();
        let p = c.entry_path(Kind::Docs, "k");
        fs::write(&p, b"{\"v\":1,\"kind\":\"docs\",\"bo").unwrap();
        assert!(c.lookup(Kind::Docs, "k").is_none());
        assert!(!p.exists());
        // an entry of another key at this path (a collision or a foreign file) is not served
        c.put(Kind::Docs, "k", "good").unwrap();
        let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        v["key"] = json!("other");
        fs::write(&p, v.to_string()).unwrap();
        assert!(c.lookup(Kind::Docs, "k").is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn least_recently_used_goes_first_past_the_cap() {
        let d = tmp("evict");
        let c = Cache::open(&d, 1, "t").unwrap(); // 1 MB
        let body = "x".repeat(200 * 1024);
        for i in 0..4 { c.put(Kind::Docs, &format!("k{i}"), &body).unwrap(); }
        // k0 is the oldest; touch it so k1 becomes the least recently used
        let t0 = SystemTime::now() - Duration::from_secs(100);
        for i in 0..4 { File::options().write(true).open(c.entry_path(Kind::Docs, &format!("k{i}"))).unwrap().set_modified(t0 + Duration::from_secs(i * 10)).unwrap(); }
        assert!(c.lookup(Kind::Docs, "k0").is_some()); // now the most recent
        c.put(Kind::Docs, "k4", &body).unwrap();
        c.put(Kind::Docs, "k5", &body).unwrap();
        let total: u64 = c.usage().iter().map(|u| u.2).sum();
        assert!(total <= c.max_bytes(), "{total}");
        assert!(c.lookup(Kind::Docs, "k1").is_none(), "least recently used evicted");
        assert!(c.lookup(Kind::Docs, "k0").is_some(), "recently used kept");
        assert!(c.lookup(Kind::Docs, "k5").is_some());
        // an entry larger than a quarter of the cap is not stored
        c.put(Kind::Docs, "big", &"y".repeat(300 * 1024)).unwrap();
        assert!(c.lookup(Kind::Docs, "big").is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_readers_never_see_half_an_entry() {
        let d = tmp("atomic");
        let c = Arc::new(Cache::open(&d, 50, "t").unwrap());
        let a = "a".repeat(1 << 20);
        let b = "b".repeat(1 << 20);
        let w = { let (c, a, b) = (c.clone(), a.clone(), b.clone()); std::thread::spawn(move || for i in 0..40 { c.put(Kind::Docs, "k", if i % 2 == 0 { &a } else { &b }).unwrap(); }) };
        let mut seen = 0;
        while !w.is_finished() {
            if let Found::Hit(t) = c.get(Kind::Docs, "k") { assert!(t == a || t == b, "a torn entry"); seen += 1; }
        }
        w.join().unwrap();
        assert!(seen > 0);
        // no temp files left
        assert!(fs::read_dir(d.join("entries/docs")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn in_flight_dedup_across_threads_and_no_secrets_stored() {
        let d = tmp("dedup");
        let c = Arc::new(Cache::open(&d, 10, "t").unwrap());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hs: Vec<_> = (0..6).map(|_| { let (c, runs) = (c.clone(), runs.clone()); std::thread::spawn(move || {
            c.get_or_compute(Kind::WebFetch, "slow", Duration::from_secs(10), || {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                Ok(("page".into(), true))
            }).unwrap()
        }) }).collect();
        for h in hs { assert_eq!(h.join().unwrap(), "page"); }
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        // an error is not cached; uncacheable text is not stored; a secret is not stored
        assert!(c.get_or_compute(Kind::WebFetch, "err", Duration::from_secs(1), || Err("HTTP 500".into())).is_err());
        assert!(!c.entry_path(Kind::WebFetch, "err").exists());
        c.get_or_compute(Kind::WebFetch, "nostore", Duration::from_secs(1), || Ok(("x".into(), false))).unwrap();
        assert!(!c.entry_path(Kind::WebFetch, "nostore").exists());
        let key = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
        c.get_or_compute(Kind::WebFetch, "secret", Duration::from_secs(1), || Ok((format!("aws key {key} here"), true))).unwrap();
        assert!(!c.entry_path(Kind::WebFetch, "secret").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn clear_by_kind_and_command_words() {
        let d = tmp("clear");
        let c = Cache::open(&d, 10, "t").unwrap();
        c.put(Kind::Docs, "a", "1").unwrap();
        c.put(Kind::WebFetch, "b", "2").unwrap();
        assert_eq!(c.clear(Some(Kind::Docs)).unwrap().0, 1);
        assert!(c.lookup(Kind::WebFetch, "b").is_some());
        assert_eq!(c.clear(None).unwrap().0, 1);
        assert!(new_hits(&[(Kind::Docs, 1)], &[(Kind::Docs, 2), (Kind::WebFetch, 0)]) == ["docs"]);
        let _ = fs::remove_dir_all(&d);
    }
}
