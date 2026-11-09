//! Plain ANSI rendering (no TUI crate): `Plain` is the Frontend for `-p` and piped stdin/stdout.
use crate::frontend::{Approval, Banner, Frontend, Progress, SessionInfo, Status, WatchUpdate, WatcherView};
use crate::watcher;
use crate::types::Reply;
use std::io::{self, BufRead, Write};
use std::sync::atomic::Ordering;

pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";
pub const PROMPT: &str = "› ";

pub fn dim(s: &str) { eprintln!("{DIM}{s}{RESET}"); }
pub fn error(s: &str) { eprintln!("{RED}{s}{RESET}"); }
pub fn text(s: &str) { print!("{s}"); let _ = io::stdout().flush(); }

/// A duration for display: under a minute `4.7s` (`places` decimals, `0` gives `5s`); from 60 s whole minutes plus
/// seconds `1m 0s` / `12m 5s`; from an hour `1h 2m`. Never `60s`. Machine outputs keep raw seconds, not this.
pub fn dur(s: f64, places: usize) -> String {
    let s = if s.is_finite() { s.max(0.0) } else { 0.0 };
    let shown = format!("{s:.places$}");
    if shown.parse::<f64>().unwrap_or(s) < 60.0 { return format!("{shown}s"); }
    let t = (s.floor() as u64).max(60);
    if t >= 3600 { format!("{}h {}m", t / 3600, t % 3600 / 60) } else { format!("{}m {}s", t / 60, t % 60) }
}

/// 12345 -> "12.3K"
pub fn k(n: usize) -> String {
    if n < 1000 { n.to_string() } else { format!("{:.1}K", n as f64 / 1000.0) }
}

/// Status of one reply. A tool-only reply arrives in one piece at the end (the gateway hides the call text), so there
/// is no decode window: tok_s is None and ttft_s is the whole request time.
pub fn status(r: &Reply, ctx_used: usize, max_ctx: usize) -> Status {
    let gen = r.total_s - r.ttft_s;
    let (tok_s, ttft_s) = if gen >= crate::api::MIN_DECODE_S && r.usage.output_tokens > 0 { (Some(r.usage.output_tokens as f64 / gen), r.ttft_s) } else { (None, r.total_s) };
    Status { tok_s, ttft_s, ctx_used, max_ctx, reused: r.usage.cache_read_input_tokens, output_tokens: r.usage.output_tokens, estimated: false }
}

/// `23.4 tok/s · ttft 0.6s`; with no decode window (a tool-call-only reply, or under 0.2 s of decode) there is no honest
/// rate: `— tok/s · 2.0s total` (the request time).
pub fn speed(st: &Status) -> String {
    match st.tok_s {
        Some(t) => format!("{t:.1} tok/s · ttft {}", dur(st.ttft_s, 1)),
        None => format!("— tok/s · {} total", dur(st.ttft_s, 1)),
    }
}

/// `23.4 tok/s · ttft 0.6s · ctx 3.1K/12.3K (25%) · reused 2.9K`
pub fn status_text(st: &Status) -> String {
    let pct = st.ctx_used * 100 / st.max_ctx.max(1);
    let sp = if st.tok_s.is_some() { speed(st) } else { format!("{} tok · {}", st.output_tokens, speed(st)) };
    // `~`: Elisa's own count (the server reported none, or a number that means something else)
    let est = if st.estimated { "~" } else { "" };
    let mut s = format!("{sp} · ctx {est}{}/{} ({pct}%{})", k(st.ctx_used), k(st.max_ctx), if st.estimated { ", estimated" } else { "" });
    if st.reused > 0 { s += &format!(" · reused {}", k(st.reused)); }
    s
}

pub fn status_line(r: &Reply, ctx_used: usize, max_ctx: usize) -> String { status_text(&status(r, ctx_used, max_ctx)) }

pub fn tool_line(summary: &str) { dim(&format!("▸ {summary}")); }

pub fn tool_preview(content: &str, is_error: bool) {
    let n = content.lines().count();
    let col = if is_error { RED } else { DIM };
    for l in content.lines().take(3) {
        let l: String = l.chars().take(160).collect();
        eprintln!("{col}  │ {l}{RESET}");
    }
    if n > 3 { dim(&format!("  │ … {} more lines", n - 3)); }
}

fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// Reads one input; a line ending in `\` continues. None on EOF (Ctrl-D).
/// Ctrl-C while waiting is handled by the SIGINT handler (the tty discards the line, a new prompt is drawn).
pub fn read_input() -> Option<String> {
    let (at, cancel) = (&crate::repl::AT_PROMPT, &crate::repl::CANCEL);
    cancel.store(false, Ordering::SeqCst);
    at.store(true, Ordering::SeqCst);
    print!("{BOLD}{PROMPT}{RESET}");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let out = loop {
        let Some(l) = read_line() else { break if buf.is_empty() { None } else { Some(buf) } };
        if cancel.swap(false, Ordering::SeqCst) { buf.clear(); }
        if let Some(head) = l.strip_suffix('\\') {
            buf.push_str(head);
            buf.push('\n');
            print!("  ");
            let _ = io::stdout().flush();
        } else {
            buf.push_str(&l);
            break Some(buf);
        }
    };
    at.store(false, Ordering::SeqCst);
    out
}

/// Asks a one-line question; None on EOF.
pub fn ask(q: &str) -> Option<String> {
    eprint!("{BOLD}{q}{RESET} ");
    let _ = io::stderr().flush();
    crate::repl::ASKING.store(true, Ordering::SeqCst);
    let a = read_line();
    crate::repl::ASKING.store(false, Ordering::SeqCst);
    a
}

pub fn local_time(ts: u64) -> String {
    unsafe {
        let t = ts as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { return ts.to_string(); }
        format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

/// Hides state-card text from streamed text (`context::CardFilter`: tags and lines may be split across deltas).
pub type StateFilter = crate::context::CardFilter;

/// Line-based frontend: stdin input, model text on stdout, everything else dim/red on stderr.
#[derive(Default)]
pub struct Plain { streamed: bool, progress: bool, state: StateFilter }

impl Plain {
    pub fn new() -> Plain { Plain::default() }
}

impl Frontend for Plain {
    fn banner(&mut self, b: &Banner) {
        eprintln!("{BOLD}Elisa{RESET} · {} @ {} · ctx {}", b.model, b.base_url, b.max_ctx);
        dim(&b.cwd);
        if let Some(n) = &b.notes {
            dim(&format!("notes loaded ({} words):", n.split_whitespace().count()));
            let lines: Vec<&str> = n.lines().collect();
            for l in lines.iter().take(8) { dim(&format!("  {l}")); }
            if lines.len() > 8 { dim(&format!("  … {} more lines (/notes)", lines.len() - 8)); }
        }
        if let Some(r) = b.resumed { dim(&format!("resumed last session ({r} messages)")); }
        dim("/help for commands · Ctrl-C cancels · Ctrl-D exits");
    }
    fn user(&mut self, _text: &str) {}
    fn waiting(&mut self) { self.streamed = false; }
    fn text_delta(&mut self, s: &str) {
        let v = self.state.push(s);
        if !v.is_empty() { self.streamed = true; text(&v); }
    }
    fn reply_done(&mut self, st: &Status) {
        let v = self.state.finish();
        if !v.is_empty() { self.streamed = true; text(&v); }
        if std::mem::take(&mut self.streamed) { println!(); }
        dim(&status_text(st));
    }
    fn tool_start(&mut self, summary: &str) { tool_line(summary); }
    fn tool_done(&mut self, output: &str, is_error: bool) { tool_preview(output, is_error); }
    fn approve(&mut self, tool: &str, _summary: &str, server: Option<&str>) -> Approval {
        let what = match server { Some(s) => format!("MCP server {s}"), None => tool.to_string() };
        loop {
            let ans = ask(&format!("allow? [y]es / [n]o / [a]lways for {what}"));
            if crate::repl::CANCEL.load(Ordering::SeqCst) { return Approval::No; }
            match ans.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
                Some("y" | "yes") => return Approval::Yes,
                Some("a" | "always") => return Approval::Always,
                Some("n" | "no") | None => return Approval::No,
                _ => {}
            }
        }
    }
    fn approve_rule(&mut self, tool: &str, summary: &str, server: Option<&str>, rule: &str) -> Approval {
        let what = match server { Some(s) => format!("MCP server {s}"), None => tool.to_string() };
        for l in summary.lines().skip(1) { dim(l); }
        loop {
            let ans = ask(&format!("allow? [y]es / [n]o / [a]lways for {what} / [r] always allow {rule} (saved to ~/.config/elisa/config.toml)"));
            if crate::repl::CANCEL.load(Ordering::SeqCst) { return Approval::No; }
            match ans.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
                Some("y" | "yes") => return Approval::Yes,
                Some("a" | "always") => return Approval::Always,
                Some("r" | "rule") => return Approval::Rule,
                Some("n" | "no") | None => return Approval::No,
                _ => {}
            }
        }
    }
    fn ask_yes_no(&mut self, _what: &str, question: &str) -> bool {
        let ans = ask(&format!("{question} [y/N]"));
        if crate::repl::CANCEL.load(Ordering::SeqCst) { return false; }
        matches!(ans.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("y" | "yes"))
    }
    fn approve_outside(&mut self, _tool: &str, summary: &str, o: &crate::frontend::Outside) -> Approval {
        dim(&format!("{summary}\n{}", o.line()));
        loop {
            let ans = ask(&format!("allow {} outside the project? [y]es once / [s]ession for these paths / [n]o", o.access_word()));
            if crate::repl::CANCEL.load(Ordering::SeqCst) { return Approval::No; }
            match ans.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
                Some("y" | "yes" | "o" | "once") => return Approval::Yes,
                Some("s" | "session" | "a" | "always") => return Approval::Always,
                Some("n" | "no") | None => return Approval::No,
                _ => {}
            }
        }
    }
    fn info(&mut self, s: &str) { dim(s); }
    /// One dim line, redrawn with `\r` on a tty; piped: only the first and the final line.
    fn progress(&mut self, p: Option<&Progress>) {
        let tty = unsafe { libc::isatty(2) == 1 };
        let Some(p) = p else {
            if std::mem::take(&mut self.progress) && tty { eprintln!(); }
            return;
        };
        let line = match &p.done {
            Some(d) => format!("{} · {d} · {}", p.label, dur(p.elapsed_s, 1)),
            None => format!("{}… {}{}", p.label, p.frac.map_or(String::new(), |f| format!("{:.0}% · ", f * 100.0)), dur(p.elapsed_s, 1)),
        };
        if tty { eprint!("\r\x1b[2K{DIM}{line}{RESET}"); } else if !self.progress || p.done.is_some() { eprintln!("{DIM}{line}{RESET}"); }
        self.progress = p.done.is_none();
        if p.done.is_some() && tty { eprintln!(); }
        let _ = io::stderr().flush();
    }
    fn state_card(&mut self, card: &str) { dim(&format!("state · {}", card.lines().map(str::trim).collect::<Vec<_>>().join(" · "))); }
    fn recalled(&mut self, hits: &[String]) { for h in hits { dim(&format!("↺ {h}")); } }
    fn error(&mut self, s: &str) { error(s); }
    fn notes_saved(&mut self, notes: &str) {
        dim("notes saved to .elisa/notes/ (this session's own):");
        for l in notes.lines() { dim(&format!("  {l}")); }
    }
    fn compacted(&mut self, from_tokens: usize, to_tokens: usize) {
        dim(&format!("[compacted: {} → {} tokens]", k(from_tokens), k(to_tokens)));
    }
    fn pick_session(&mut self, list: &[SessionInfo]) -> Option<usize> {
        for (i, s) in list.iter().enumerate() {
            dim(&format!("{:>3}. {} · {} msgs · {}", i + 1, local_time(s.ts), s.n_msgs, s.title));
        }
        let ans = ask(&format!("resume which? [1-{}, Enter cancels]", list.len()))?;
        if crate::repl::CANCEL.swap(false, Ordering::SeqCst) { return None; }
        ans.trim().parse::<usize>().ok().filter(|&n| n >= 1 && n <= list.len()).map(|n| n - 1)
    }
    fn read_input(&mut self) -> Option<String> {
        let r = read_input();
        if r.is_none() { println!(); }
        r
    }
    /// a domain change and finished audits of this session's replies, in the default colour
    fn watcher_update(&mut self, u: &WatchUpdate) {
        for a in &u.audits { if let Some(l) = watcher::audit_line(a) { eprintln!("{l}"); } }
        let Some(v) = u.view.as_ref().filter(|_| u.changed) else { return };
        eprintln!("{}", match &u.was { Some(w) => watcher::change_line(v, Some(w)), None => watcher::line(v) });
    }
    fn full_answer(&mut self, title: &str, text: &str) {
        eprintln!("{BOLD}── {title} ──{RESET}");
        eprintln!("{}", text.trim());
        eprintln!("{BOLD}──{RESET}");
    }
    fn watcher(&mut self, w: Option<&WatcherView>) {
        let Some(v) = w else { return dim("watcher: not available on this gateway") };
        eprintln!("{BOLD}{}{RESET}", watcher::line(v));
        for l in watcher::full_lines(v) {
            match l.strip_prefix("## ") { Some(t) => eprintln!("{BOLD}{t}{RESET}"), None => eprintln!("{l}") }
        }
    }
    fn turn_done(&mut self) {}
}

#[cfg(test)]
mod dur_tests {
    use super::dur;
    #[test]
    fn durations() {
        assert_eq!(dur(0.6, 1), "0.6s");
        assert_eq!(dur(59.4, 1), "59.4s");
        assert_eq!(dur(60.0, 1), "1m 0s");
        assert_eq!(dur(61.0, 1), "1m 1s");
        assert_eq!(dur(725.0, 1), "12m 5s");
        assert_eq!(dur(3725.0, 1), "1h 2m");
        assert_eq!(dur(59.97, 1), "1m 0s");
        assert_eq!(dur(59.0, 0), "59s");
        assert_eq!(dur(4.7, 1), "4.7s");
    }
}
