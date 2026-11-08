//! The audit log: one JSON line per tool call, approval answer, denial and MCP server start / stop, appended to
//! `~/.elisa/audit/<project-key>.jsonl` (`project-key`: a short sha256 of the canonical project path; each line also
//! names the project). Lines are hash-chained: `prev` is the sha256 of the line before (empty on the first), so an
//! edited or removed line breaks the chain at the line after it (`verify`, `/audit`). Only one-line summaries go in
//! (`tools::summary`, through `secrets::redact`), never a tool's full input or output. Config `audit = "on"` (default)
//! | `"off"`; a project config may only switch it off. Writing is best effort: a failure never stops a tool call.
use serde_json::{json, Map, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// the audit directory under the home directory
pub const DIR: &str = ".elisa/audit";
/// a summary longer than this is cut (the log is for what happened, not for content)
const MAX_FIELD: usize = 300;
/// how much of the file's end is read to find the last line (a longer line: the whole file is read)
const TAIL: u64 = 64 << 10;

/// 16 hex digits of the sha256 of the canonical project path
pub fn project_key(project: &Path) -> String {
    let p = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    crate::pins::sha256_hex(p.to_string_lossy().as_bytes())[..16].to_string()
}

pub fn path_for(home: &Path, project: &Path) -> PathBuf { home.join(DIR).join(format!("{}.jsonl", project_key(project))) }

struct Inner { path: PathBuf, project: String }

/// The session's audit log; `Log::default()` is off (records nothing). Cheap to clone (the MCP host keeps one).
#[derive(Clone, Default)]
pub struct Log(Option<Arc<Inner>>);

/// the hash a line's successor keeps in `prev`
pub fn line_hash(line: &str) -> String { crate::pins::sha256_hex(line.as_bytes()) }

/// the last non-empty line of the open file (empty when there is none)
fn last_line(f: &mut std::fs::File) -> std::io::Result<String> {
    let len = f.seek(SeekFrom::End(0))?;
    let mut from = len.saturating_sub(TAIL);
    loop {
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf);
        let trimmed = text.trim_end_matches('\n');
        match trimmed.rfind('\n') {
            Some(i) => return Ok(trimmed[i + 1..].to_string()),
            None if from == 0 => return Ok(trimmed.to_string()),
            None => from = 0, // one line longer than TAIL: read it all
        }
    }
}

fn cut(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= MAX_FIELD { one } else { format!("{}…", one.chars().take(MAX_FIELD).collect::<String>()) }
}

impl Log {
    /// the log of `project` under `home`
    pub fn open(home: &Path, project: &Path) -> Log {
        let project = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
        Log(Some(Arc::new(Inner { path: path_for(home, &project), project: project.display().to_string() })))
    }

    pub fn on(&self) -> bool { self.0.is_some() }
    pub fn path(&self) -> Option<&Path> { self.0.as_ref().map(|i| i.path.as_path()) }

    /// Appends one line: `event` and its `fields` (tool, server, summary, outcome, answer, …; empty values left out).
    /// Every value goes through `secrets::redact` and is cut to one short line. Best effort; Err says why it failed.
    pub fn record(&self, event: &str, fields: &[(&str, &str)]) -> Result<(), String> {
        let Some(i) = &self.0 else { return Ok(()) };
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut m = Map::new();
        m.insert("ts".into(), json!(ts));
        m.insert("session".into(), json!(crate::context::session_id()));
        m.insert("project".into(), json!(i.project));
        m.insert("event".into(), json!(event));
        for (k, v) in fields.iter().filter(|(_, v)| !v.is_empty()) {
            let (clean, _) = crate::secrets::redact(v);
            m.insert((*k).to_string(), json!(cut(&clean)));
        }
        if let Some(d) = i.path.parent() { std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?; }
        let mut f = std::fs::OpenOptions::new().read(true).append(true).create(true).open(&i.path)
            .map_err(|e| format!("{}: {e}", i.path.display()))?;
        // one writer at a time (two sessions in one project): the last line is read under the lock
        // SAFETY: flock on our own open file descriptor
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX); }
        let r = (|| {
            let last = last_line(&mut f)?;
            m.insert("prev".into(), json!(if last.is_empty() { String::new() } else { line_hash(&last) }));
            f.write_all(format!("{}\n", Value::Object(m)).as_bytes())
        })();
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN); }
        r.map_err(|e| format!("{}: {e}", i.path.display()))
    }

    /// `record`, errors dropped (an audit failure never gets in the way of the work)
    pub fn note(&self, event: &str, fields: &[(&str, &str)]) { let _ = self.record(event, fields); }
}

/// Checks the chain of the log at `path`: Ok(number of lines), or Err((1-based line, why)) at the first line whose
/// `prev` does not match the line before it (that line, or the one before, was changed, added or removed).
pub fn verify(path: &Path) -> Result<usize, (usize, String)> {
    let text = std::fs::read_to_string(path).map_err(|e| (0, e.to_string()))?;
    let mut prev = String::new();
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() { continue; }
        let v: Value = serde_json::from_str(line).map_err(|_| (i + 1, "not a JSON line".to_string()))?;
        let want = if n == 0 { String::new() } else { line_hash(&prev) };
        if v["prev"].as_str() != Some(want.as_str()) { return Err((i + 1, "prev does not match the line before".into())); }
        prev = line.to_string();
        n += 1;
    }
    Ok(n)
}

/// `2026-11-08 14:03:07` (UTC) from unix milliseconds
fn when(ms: u64) -> String {
    let s = ms / 1000;
    let (days, rem) = (s / 86_400, s % 86_400);
    // civil from days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `2026-11-08 15:03:07 CET`: local time with its zone name (the log stores UTC milliseconds); `… UTC` when the
/// local time cannot be worked out
fn when_local(ms: u64) -> String {
    let t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() { return format!("{} UTC", when(ms)); }
    let zone = if tm.tm_zone.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(tm.tm_zone) }.to_string_lossy().into_owned() };
    // a numeric abbreviation (`+07`) reads as an offset from UTC
    let zone = if zone.starts_with(['+', '-']) { format!("UTC{zone}") } else { zone };
    let zone = if zone.is_empty() {
        let off = tm.tm_gmtoff / 60;
        format!("UTC{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, off.abs() / 60, off.abs() % 60)
    } else { zone };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} {zone}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The last `n` lines of the log at `path`, one readable line each: `time  event  outcome  server/tool  summary`.
pub fn last(path: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| {
        let Ok(v) = serde_json::from_str::<Value>(l) else { return format!("(not JSON) {l}") };
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let what = [s("server"), s("tool")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("/");
        let outcome = [s("outcome"), s("answer")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
        let parts = [when_local(v["ts"].as_u64().unwrap_or(0)), s("event"), outcome, what, s("summary"), s("detail")];
        parts.into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ")
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(when(0), "1970-01-01 00:00:00");
        assert_eq!(when(1_762_610_587_000), "2025-11-08 14:03:07");
        assert_eq!(when(951_782_400_000), "2000-02-29 00:00:00");
        // local time always names its zone (TZ is not changed here: tests run in parallel)
        let l = when_local(1_762_610_587_000);
        let zone = l.splitn(3, ' ').nth(2).unwrap_or("");
        assert!(l.starts_with("2025-11-0") && !zone.is_empty() && zone.starts_with(|c: char| c.is_ascii_alphabetic()), "{l}");
    }
}
