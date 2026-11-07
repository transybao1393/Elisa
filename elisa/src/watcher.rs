//! Client for the gateway's domain watcher (`/v1/streamdense/watcher`): notes go out on detached threads, the view
//! is fetched in the background after turns and folds. An older gateway (404) turns it off quietly for the session.
//! Bounded: at most MAX_INFLIGHT note threads, one fetch at a time, note text <= NOTE_MAX bytes.
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// the watcher state stays usable after a panic on another thread (a poisoned lock), instead of panicking every caller
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(PoisonError::into_inner) }
use std::time::{Duration, Instant};

const NOTE_MAX: usize = 2048;
const MAX_INFLIGHT: usize = 4;
/// `mode` field of a note (the gateway schema keeps it; Elisa has a single mode)
const NOTE_MODE: &str = "code";
const TIMEOUT: Duration = Duration::from_millis(1500);
const STACK: usize = 256 << 10;
/// reply ids of this session kept for matching audits (oldest dropped first)
const MINE_MAX: usize = 512;
const BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub cold_ffn_frac: f64,
    pub cold_heads: usize,
    pub gb_unload: f64,
    pub ms_saved_per_pass: f64,
    pub footprint_gb: f64,
    /// per layer: a 0..1 fraction, or a count of cold FFN neurons (the gateway sends counts)
    pub per_layer_cold: Vec<f64>,
    pub ms_saved_unload: f64,
    /// None = not sent
    pub fits_cap: Option<bool>,
    pub ready: Option<bool>,
}

/// One audit of a reply against the full model (`audits_recent`; the gateway audits every reply in idle time).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audit {
    pub message_id: String,
    pub domain: String,
    /// match | mismatch | skipped (pending on older gateways)
    pub result: String,
    pub tokens: u64,
    pub diverge_at: Option<u64>,
    /// mask | model | "" (unknown)
    pub cause: String,
    pub full_answer: Option<String>,
    /// why an audit was skipped (e.g. "dry run: no mask active, reply came from the full model"); "" = none sent
    pub reason: String,
}

/// Per-domain audit scorecard row (`scorecard`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    pub domain: String,
    pub audited_tokens: u64,
    pub agreement: f64,
    pub mismatches: u64,
    pub clean_audits: u64,
    pub go_live_at: u64,
    pub live: bool,
    /// audits skipped by the gateway (no mask active: nothing to compare)
    pub skipped: u64,
    /// go-live points so far and needed; 0 / 0 = not sent
    pub points: u64,
    pub required: u64,
}

/// GET /v1/streamdense/watcher, parsed leniently (missing or odd fields become 0 / empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WatcherView {
    /// empty = no domain decided yet
    pub domain: String,
    pub confidence: f64,
    pub since_turn: u64,
    pub candidates: Vec<(String, f64)>,
    /// one line each: `turn N → domain (0.86) …`
    pub decisions: Vec<String>,
    pub tokens_observed: u64,
    pub plan: Plan,
    /// (domain, tokens, gb_unload)
    pub profiles: Vec<(String, u64, f64)>,
    pub note: String,
    /// gateway turn counter, watcher mode, pending switch (domain, wins), activity source line
    pub turn: u64,
    pub mode: String,
    pub pending: Option<(String, u64)>,
    pub activity: String,
    /// oldest first
    pub audits: Vec<Audit>,
    pub scorecard: Vec<Score>,
    /// audits waiting (pending, max); None = not sent
    pub queue: Option<(u64, u64)>,
    /// hot fraction (now, session, all) and the coldest layers now; None = no heat map sent
    pub heat: Option<Heat>,
}

/// The gateway's heat map summary (`heat`): mean hot fraction per window, coldest layers of the newest window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heat { pub now: Option<f64>, pub session: Option<f64>, pub all: Option<f64>, pub coldest: Vec<u64> }

/// A new view for the display. `view` None = the watcher is gone (older gateway): hide it. `changed` = the domain
/// differs from the last one seen (`was` = that one; None on the first domain).
#[derive(Clone, Debug, Default)]
/// `audits` = mismatch audits of this session's replies that finished since the last update (shown once each).
pub struct WatchUpdate { pub view: Option<WatcherView>, pub changed: bool, pub was: Option<String>, pub audits: Vec<Audit> }

pub type Push = Box<dyn Fn(WatchUpdate) + Send>;

fn num(v: &Value) -> f64 { v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0) }
fn text(v: &Value) -> String { v.as_str().map(String::from).unwrap_or_default() }
fn flag(v: &Value) -> Option<bool> { v.as_bool() }
fn opt_num(v: &Value) -> Option<u64> { (v.is_number() || v.is_string()).then(|| num(v).max(0.0) as u64) }
fn count(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.iter().map(|x| if x.is_array() { count(x) } else { 1 }).sum(),
        _ => num(v).max(0.0) as usize,
    }
}
fn layer_frac(v: &Value) -> f64 {
    if v.is_object() {
        ["cold_ffn_frac", "ffn_frac", "frac", "cold"].iter().map(|k| &v[*k]).find(|x| x.is_number()).map_or(0.0, num)
    } else { num(v) }
}
/// `heat`: {"now"|"session"|"all": {"hot_frac_mean", "coldest_layers": [[layer, frac], ..]}, ..}; None when no window has a mean
fn heat(v: &Value) -> Option<Heat> {
    let m = |k: &str| { let x = &v[k]["hot_frac_mean"]; (x.is_number() || x.is_string()).then(|| num(x)) };
    let h = Heat { now: m("now"), session: m("session"), all: m("all"), coldest: vec![] };
    if h.now.is_none() && h.session.is_none() && h.all.is_none() { return None; }
    let w = if h.now.is_some() { "now" } else if h.session.is_some() { "session" } else { "all" };
    let coldest = v[w]["coldest_layers"].as_array().map_or(vec![], |a| a.iter().filter_map(|x| x.get(0).or(Some(x)).filter(|l| l.is_number()).map(|l| num(l) as u64)).take(3).collect());
    Some(Heat { coldest, ..h })
}

/// `heat now 0.62 · session 0.58 · all 0.55 hot (coldest L3 L17 L40)`
pub fn heat_line(h: &Heat) -> String {
    let parts: Vec<String> = [("now", h.now), ("session", h.session), ("all", h.all)].iter()
        .filter_map(|(k, x)| x.map(|x| format!("{k} {x:.2}"))).collect();
    let mut s = format!("heat {} hot", parts.join(" · "));
    if !h.coldest.is_empty() { s += &format!(" (coldest {})", h.coldest.iter().map(|l| format!("L{l}")).collect::<Vec<_>>().join(" ")); }
    s
}

/// One decision line, fixed order: `turn 69 → chat-vi · label code-rust 0.49 · state_card · since 51 · 308 tok ·
/// learned chat-vi`. The gateway's confidence is the label's, so it sits beside the label (beside the domain when they
/// match). `switched` shows only when true; ts / mode / candidates / plan / sessions and unknown keys are left out.
pub fn decision(v: &Value) -> String {
    let Some(o) = v.as_object() else { return v.as_str().map_or_else(|| v.to_string(), String::from) };
    let st = |k: &str| o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let mut s = String::new();
    if let Some(t) = o.get("turn").filter(|t| t.is_number()) { s += &format!("turn {} ", num(t) as u64); }
    let to = st("domain").or_else(|| st("to"));
    if let Some(d) = to { s += &format!("→ {d}"); }
    if let Some(f) = st("from") { s += &format!(" (was {f})"); }
    let conf = ["confidence", "score"].iter().find_map(|k| o.get(*k).filter(|x| x.is_number())).map(|c| format!(" {:.2}", num(c)));
    match st("label") {
        Some(l) if Some(l) != to => s += &format!(" · label {l}{}", conf.unwrap_or_default()),
        _ => s += &conf.unwrap_or_default(),
    }
    if let Some(t) = st("trigger") { s += &format!(" · {t}"); }
    if o.get("switched").and_then(Value::as_bool) == Some(true) { s += " · switched"; }
    if let Some(p) = o.get("pending").and_then(Value::as_array).filter(|p| !p.is_empty()) {
        s += &format!(" · pending {} {}", p[0].as_str().unwrap_or("?"), p.get(1).map_or(0, |n| num(n) as u64));
    }
    if let Some(n) = o.get("since_turn").filter(|x| x.is_number()) { s += &format!(" · since {}", num(n) as u64); }
    if let Some(n) = o.get("tokens_window").filter(|x| x.is_number()) { s += &format!(" · {} tok", num(n) as u64); }
    if let Some(l) = st("learned") { s += &format!(" · learned {l}"); }
    s.trim().to_string()
}

impl WatcherView {
    pub fn parse(v: &Value) -> WatcherView {
        let arr = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        let p = &v["unload_plan"];
        WatcherView {
            domain: text(&v["domain"]),
            confidence: num(&v["confidence"]),
            since_turn: num(&v["since_turn"]) as u64,
            candidates: arr("candidates").iter().map(|c| (text(&c["domain"]), num(&c["score"]))).collect(),
            decisions: arr("decisions_recent").iter().map(decision).filter(|s| !s.is_empty()).collect(),
            tokens_observed: num(&v["tokens_observed"]) as u64,
            plan: Plan {
                cold_ffn_frac: num(&p["cold_ffn_frac"]),
                cold_heads: count(&p["cold_heads"]),
                gb_unload: num(&p["gb_unload"]),
                ms_saved_per_pass: num(&p["ms_saved_per_pass"]),
                footprint_gb: num(&p["footprint_gb"]),
                per_layer_cold: p["per_layer_cold"].as_array().map_or(vec![], |a| a.iter().map(layer_frac).collect()),
                ms_saved_unload: num(&p["ms_saved_unload"]),
                fits_cap: flag(&p["fits_cap"]),
                ready: flag(&p["ready"]),
            },
            profiles: arr("profiles").iter().map(|r| (text(&r["domain"]), num(&r["tokens"]) as u64, num(&r["gb_unload"]))).collect(),
            note: text(&v["note"]),
            turn: num(&v["turn"]) as u64,
            mode: text(&v["mode"]),
            pending: match &v["pending"] {
                Value::Object(o) => o.get("domain").and_then(Value::as_str).map(|d| (d.to_string(), o.get("wins").map_or(0, |w| num(w) as u64))),
                Value::Array(a) => a.first().and_then(Value::as_str).map(|d| (d.to_string(), a.get(1).map_or(0, |w| num(w) as u64))),
                _ => None,
            },
            activity: match &v["activity"] {
                Value::Object(o) => format!("{} · {} jobs", o.get("source").and_then(Value::as_str).unwrap_or("?"), o.get("jobs").map_or(0, |j| num(j) as u64)),
                x => text(x),
            },
            audits: arr("audits_recent").iter().filter(|a| a.is_object()).map(|a| Audit {
                message_id: text(&a["message_id"]), domain: text(&a["domain"]), result: text(&a["result"]), tokens: num(&a["tokens"]) as u64,
                diverge_at: opt_num(&a["diverge_at"]), cause: text(&a["cause"]), full_answer: a["full_answer"].as_str().map(String::from),
                reason: text(&a["reason"]),
            }).collect(),
            scorecard: arr("scorecard").iter().filter(|r| r.is_object()).map(|r| Score {
                domain: text(&r["domain"]), audited_tokens: num(&r["audited_tokens"]) as u64, agreement: num(&r["agreement"]),
                mismatches: num(&r["mismatches"]) as u64, clean_audits: num(&r["clean_audits"]) as u64,
                go_live_at: num(&r["go_live_at"]) as u64,
                live: r["live"].as_bool().unwrap_or_else(|| text(&r["status"]).eq_ignore_ascii_case("live")),
                skipped: num(&r["skipped"]) as u64, points: num(&r["points"]) as u64, required: num(&r["required"]) as u64,
            }).collect(),
            queue: v["audit_queue"].as_object().map(|q| (q.get("pending").map_or(0, |n| num(n) as u64), q.get("max").map_or(0, |n| num(n) as u64))),
            heat: heat(&v["heat"]),
        }
    }
    /// nothing has been unloaded (the server says dry run, or says nothing)
    pub fn dry(&self) -> bool { self.note.is_empty() || self.note.to_lowercase().contains("dry") }
}

fn tok(n: u64) -> String { crate::ui::k(n as usize) }

/// `watcher: code-rust 0.86 · since turn 12 · would unload 2.1 GB (FFN 22% cold, 3 heads) · −8.7 ms/pass · dry run`
pub fn line(v: &WatcherView) -> String { lines(v).join(" · ") }

/// The same text in up to three parts (the TUI shows one per row): `watcher: chat-vi 0.86` /
/// `since turn 12 · would unload 1.8 GB (FFN 19% cold, 40 heads)` / `−11.2 ms/pass · dry run`
pub fn lines(v: &WatcherView) -> Vec<String> {
    if v.domain.is_empty() { return vec![format!("watcher: learning · {} tokens observed", tok(v.tokens_observed))]; }
    let p = &v.plan;
    let mut out = vec![
        format!("watcher: {} {:.2}", v.domain, v.confidence),
        format!("since turn {} · would unload {:.1} GB (FFN {:.0}% cold, {} head{})",
            v.since_turn, p.gb_unload, p.cold_ffn_frac * 100.0, p.cold_heads, if p.cold_heads == 1 { "" } else { "s" }),
    ];
    let mut third = vec![];
    if p.ms_saved_per_pass > 0.0 { third.push(format!("−{:.1} ms/pass", p.ms_saved_per_pass)); }
    if v.dry() { third.push("dry run".into()); }
    if !third.is_empty() { out.push(third.join(" · ")); }
    out
}

/// `watcher → chat-vi (was code-rust) · would unload 1.8 GB`
pub fn change_line(v: &WatcherView, was: Option<&str>) -> String {
    let was = was.map_or(String::new(), |w| format!(" (was {w})"));
    format!("watcher → {}{was} · would unload {:.1} GB", v.domain, v.plan.gb_unload)
}

/// `msg_01abcdef…` -> `…abcdef` (6 trailing chars)
fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 10 { id.to_string() } else { format!("…{}", id.chars().skip(n - 6).collect::<String>()) }
}

/// The transcript line for a finished audit (None while pending). Good news (agrees / same answer from the full model)
/// is `audit ✓`; a mask-caused change is `audit ✗`.
pub fn audit_line(a: &Audit) -> Option<String> {
    let at = a.diverge_at.map_or(String::new(), |t| format!(" at token {t}"));
    let full = if a.full_answer.is_some() { " · /full shows the full-model answer" } else { "" };
    Some(match (a.result.as_str(), a.cause.as_str()) {
        ("pending", _) | ("", _) => return None,
        ("match", _) => format!("audit ✓ full model agrees ({} tokens)", a.tokens),
        ("mismatch", "mask") => format!("audit ✗ masked weights changed this reply{at} · profile tightened{full}"),
        ("mismatch", "model") => return None, // a vote-era cause: nothing to tell the user
        ("mismatch", _) => format!("audit ✗ the full model answers differently{at}{full}"),
        ("skipped", _) if !a.reason.is_empty() => format!("audit skipped: {}", a.reason),
        ("skipped", _) => "audit skipped by the gateway".into(),
        (r, _) => format!("audit {r}"),
    })
}

/// a good audit line (✓) vs a bad one (✗)
pub fn audit_ok(a: &Audit) -> bool { a.result == "match" || (a.result == "mismatch" && a.cause == "model") }

/// one block char per value in 0..=1
pub fn sparkline(xs: &[f64]) -> String {
    xs.iter().map(|&x| BARS[((x.clamp(0.0, 1.0) * (BARS.len() - 1) as f64).round() as usize).min(BARS.len() - 1)]).collect()
}

/// The full /watcher view as plain lines; section titles start with `## ` (a display may style and strip that).
pub fn full_lines(v: &WatcherView) -> Vec<String> {
    let mut o = vec![];
    if v.domain.is_empty() {
        o.push(format!("domain: none yet · {} tokens observed", tok(v.tokens_observed)));
    } else {
        o.push(format!("domain: {} · confidence {:.2} · since turn {} · {} tokens observed", v.domain, v.confidence, v.since_turn, tok(v.tokens_observed)));
    }
    if !v.candidates.is_empty() {
        o.push("## candidates".into());
        let dw = v.candidates.iter().map(|c| c.0.chars().count()).max().unwrap_or(0);
        for (d, s) in &v.candidates {
            let n = (s.clamp(0.0, 1.0) * 20.0).round() as usize;
            o.push(format!("  {d:<dw$}  {s:.2}  {}", "█".repeat(n)));
        }
    }
    if !v.decisions.is_empty() {
        o.push("## recent decisions".into());
        for d in v.decisions.iter().rev().take(8).collect::<Vec<_>>().into_iter().rev() { o.push(format!("  {d}")); }
    }
    let p = &v.plan;
    o.push("## unload plan".into());
    let fp = if p.footprint_gb > 0.0 { format!(" of {:.1} GB footprint", p.footprint_gb) } else { String::new() };
    o.push(format!("  would unload {:.1} GB{fp} · FFN {:.0}% cold · {} cold head{} · −{:.1} ms/pass",
        p.gb_unload, p.cold_ffn_frac * 100.0, p.cold_heads, if p.cold_heads == 1 { "" } else { "s" }, p.ms_saved_per_pass));
    let mut extra = vec![];
    if p.ms_saved_unload > 0.0 { extra.push(format!("−{:.1} ms after unload", p.ms_saved_unload)); }
    if let Some(f) = p.fits_cap { extra.push(if f { "fits the cap".into() } else { "over the cap".into() }); }
    if p.ready == Some(false) { extra.push("few tokens yet".into()); }
    if !extra.is_empty() { o.push(format!("  {}", extra.join(" · "))); }
    let l = &p.per_layer_cold;
    if !l.is_empty() {
        let (lo, hi) = l.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        let bars: Vec<f64>;
        if hi > 1.0 {
            // counts of cold neurons: scaled to the largest layer
            bars = l.iter().map(|x| x / hi).collect();
            o.push(format!("  cold FFN neurons per layer ({} layers, █ = max; min {lo:.0} · max {hi:.0})", l.len()));
        } else {
            bars = l.clone();
            o.push(format!("  cold FFN per layer ({} layers, ▁ 0% … █ 100%; min {:.0}% · max {:.0}%)", l.len(), lo * 100.0, hi * 100.0));
        }
        let nw = (l.len() - 1).to_string().len();
        for (i, ch) in bars.chunks(32).enumerate() {
            let a = i * 32;
            o.push(format!("  L{a:>nw$}–{:>nw$} {}", a + ch.len() - 1, sparkline(ch)));
        }
    }
    if !v.profiles.is_empty() {
        o.push("## profiles".into());
        let dw = v.profiles.iter().map(|r| r.0.chars().count()).max().unwrap_or(0).max(6);
        o.push(format!("  {:<dw$}  {:>8}  {:>9}", "domain", "tokens", "unload"));
        for (d, t, g) in &v.profiles { o.push(format!("  {d:<dw$}  {:>8}  {:>6.1} GB", tok(*t), g)); }
    }
    if !v.scorecard.is_empty() {
        o.push("## audit scorecard".into());
        let dw = v.scorecard.iter().map(|r| r.domain.chars().count()).max().unwrap_or(0).max(6);
        o.push(format!("  {:<dw$}  {:>6}  {:>7}  {:>4}  {:>4}  {:>7}  {:>7}  {}", "domain", "agree", "tokens", "miss", "skip", "clean", "points", "status"));
        for r in &v.scorecard {
            // no compared tokens yet (every audit skipped): no agreement figure
            let agree = if r.audited_tokens == 0 { "—".to_string() } else { format!("{:.1}%", r.agreement * 100.0) };
            let pts = if r.required == 0 { "—".to_string() } else { format!("{}/{}", r.points, r.required) };
            o.push(format!("  {:<dw$}  {:>6}  {:>7}  {:>4}  {:>4}  {:>7}  {:>7}  {}", r.domain, agree, tok(r.audited_tokens),
                r.mismatches, r.skipped, format!("{}/{}", r.clean_audits, r.go_live_at), pts, if r.live { "LIVE" } else { "dry run" }));
        }
    }
    if !v.audits.is_empty() {
        o.push("## last audits".into());
        for a in v.audits.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
            let what = audit_line(a).unwrap_or_else(|| "audit pending…".into());
            o.push(format!("  {} {} · {what}", short_id(&a.message_id), if a.domain.is_empty() { "?" } else { &a.domain }));
        }
    }
    if let Some((p, m)) = v.queue { o.push(format!("audit queue: {p} pending (max {m})")); }
    if let Some(h) = &v.heat { o.push(heat_line(h)); }
    if v.turn > 0 || !v.mode.is_empty() || v.pending.is_some() || !v.activity.is_empty() {
        let mut s = vec![];
        if v.turn > 0 { s.push(format!("turn {}", v.turn)); }
        if !v.mode.is_empty() { s.push(format!("mode {}", v.mode)); }
        if let Some((d, n)) = &v.pending { s.push(format!("pending → {d} ({n} wins)")); }
        if !v.activity.is_empty() { s.push(format!("activity: {}", v.activity)); }
        o.push(s.join(" · "));
    }
    if v.dry() { o.push("dry run: nothing unloaded yet".into()); }
    if !v.note.is_empty() { o.push(format!("note: {}", v.note)); }
    o
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max { return s; }
    let mut i = max;
    while !s.is_char_boundary(i) { i -= 1; }
    &s[..i]
}

/// This session's replies (their audits get a transcript line), which audits were already shown, and the newest
/// mismatch with a full-model answer (for /full). `mine` keeps the newest MINE_MAX ids.
#[derive(Default)]
struct Mine { ids: VecDeque<String>, shown: HashSet<String>, full_mine: Option<Audit>, full_any: Option<Audit> }

#[derive(Default)]
struct St { seen: Option<String>, visible: bool, pending: Option<WatchUpdate>, mine: Mine }

struct Shared {
    off: AtomicBool, inflight: AtomicUsize, fetching: AtomicBool, again: AtomicBool,
    st: Mutex<St>, push: Mutex<Option<Push>>,
}

impl Shared {
    /// a fetch result: Some(view) = 200, None = 404 (gone). Delivered to `push`, else kept for `take`.
    fn deliver(&self, view: Option<WatcherView>) {
        let mut st = lock(&self.st);
        let u = match &view {
            None => {
                self.off.store(true, Ordering::SeqCst);
                if !std::mem::take(&mut st.visible) { return; }
                WatchUpdate::default()
            }
            Some(v) => {
                st.visible = true;
                let changed = !v.domain.is_empty() && st.seen.as_deref() != Some(v.domain.as_str());
                let was = if changed { st.seen.replace(v.domain.clone()) } else { None };
                let ms = &mut st.mine;
                let mut audits = vec![];
                for a in &v.audits {
                    let mismatch = a.result == "mismatch" && a.full_answer.is_some();
                    if mismatch { ms.full_any = Some(a.clone()); }
                    // every reply is audited: a transcript line only for a mismatch; match / skipped count in /watcher only
                    if a.result != "mismatch" || !ms.ids.contains(&a.message_id) { continue; }
                    if mismatch { ms.full_mine = Some(a.clone()); }
                    if ms.shown.insert(a.message_id.clone()) { audits.push(a.clone()); }
                }
                WatchUpdate { view, changed, was, audits }
            }
        };
        match &*lock(&self.push) {
            Some(p) => p(u),
            None => {
                // keep an undelivered domain change and undelivered audits when a newer view replaces them
                let u = match st.pending.take() {
                    Some(old) => {
                        let mut audits = old.audits;
                        audits.extend(u.audits);
                        if old.changed && !u.changed { WatchUpdate { view: u.view, changed: true, was: old.was, audits } } else { WatchUpdate { audits, ..u } }
                    }
                    None => u,
                };
                st.pending = Some(u);
            }
        }
    }
}

pub struct Watcher { base: String, project: String, agent: ureq::Agent, sh: Arc<Shared> }

impl Watcher {
    pub fn new(base_url: &str, project: &str) -> Watcher {
        let agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(TIMEOUT)).build().into();
        let sh = Shared {
            off: AtomicBool::new(false), inflight: AtomicUsize::new(0), fetching: AtomicBool::new(false),
            again: AtomicBool::new(false), st: Mutex::default(), push: Mutex::new(None),
        };
        Watcher { base: base_url.trim_end_matches('/').to_string(), project: project.to_string(), agent, sh: Arc::new(sh) }
    }

    /// Updates go straight to `p` (from a background thread) instead of waiting for `take`.
    pub fn set_push(&self, p: Push) { *lock(&self.sh.push) = Some(p); }
    pub fn pushes(&self) -> bool { lock(&self.sh.push).is_some() }
    pub fn off(&self) -> bool { self.sh.off.load(Ordering::SeqCst) }
    /// No notes and no polls for this session: the server is not a streamdense gateway, and notes hold project text.
    pub fn disable(&self) { self.sh.off.store(true, Ordering::SeqCst); }

    /// POST a note in the background; dropped when off or MAX_INFLIGHT notes are still out.
    pub fn note(&self, kind: &str, text: &str) {
        if self.off() || self.sh.inflight.load(Ordering::SeqCst) >= MAX_INFLIGHT { return; }
        let body = json!({"kind": kind, "text": cut(text.trim(), NOTE_MAX), "project": self.project, "mode": NOTE_MODE}).to_string();
        let (agent, url, sh) = (self.agent.clone(), format!("{}/v1/streamdense/watcher/note", self.base), self.sh.clone());
        sh.inflight.fetch_add(1, Ordering::SeqCst);
        let sh2 = sh.clone();
        let spawned = std::thread::Builder::new().stack_size(STACK).spawn(move || {
            if let Ok(r) = agent.post(&url).header("content-type", "application/json").send(body) {
                if matches!(r.status().as_u16(), 404 | 405) { sh.deliver(None); }
            }
            sh.inflight.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() { sh2.inflight.fetch_sub(1, Ordering::SeqCst); }
    }

    /// A final reply of this session (its gateway message id): its audit is shown once it finishes. The normal
    /// turn-cadence fetch picks it up; nothing is polled.
    pub fn mine(&self, message_id: &str) {
        let ms = &mut lock(&self.sh.st).mine;
        if ms.ids.iter().any(|i| i == message_id) { return; }
        if ms.ids.len() >= MINE_MAX { if let Some(old) = ms.ids.pop_front() { ms.shown.remove(&old); } }
        ms.ids.push_back(message_id.to_string());
    }

    /// The newest mismatched audit with a full-model answer: of this session's replies, else any in the last view.
    pub fn last_full(&self) -> Option<Audit> {
        let ms = &lock(&self.sh.st).mine;
        ms.full_mine.clone().or_else(|| ms.full_any.clone())
    }

    /// GET the view in the background (one fetch at a time; a refresh during a fetch runs once more after it).
    pub fn refresh(&self) {
        if self.off() { return; }
        if self.sh.fetching.swap(true, Ordering::SeqCst) { self.sh.again.store(true, Ordering::SeqCst); return; }
        let (agent, url, sh) = (self.agent.clone(), self.url(), self.sh.clone());
        let sh2 = sh.clone();
        let spawned = std::thread::Builder::new().stack_size(STACK).spawn(move || fetch_loop(&agent, &url, &sh));
        if spawned.is_err() { sh2.fetching.store(false, Ordering::SeqCst); }
    }

    /// GET now (blocking, short timeout): the view, or None when off / unreachable.
    pub fn fetch_now(&self) -> Option<WatcherView> {
        if self.off() { return None; }
        fetch(&self.agent, &self.url(), &self.sh)
    }

    /// The update not yet delivered (no push set).
    pub fn take(&self) -> Option<WatchUpdate> { lock(&self.sh.st).pending.take() }

    /// Wait up to `max` for notes and a fetch still in flight (before exit, so they are not cut off).
    pub fn flush(&self, max: Duration) {
        let t0 = Instant::now();
        while (self.sh.inflight.load(Ordering::SeqCst) > 0 || self.sh.fetching.load(Ordering::SeqCst)) && t0.elapsed() < max {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn url(&self) -> String { format!("{}/v1/streamdense/watcher", self.base) }
}

/// Holder of the `fetching` flag: fetch until no refresh came in meanwhile, then release the flag.
fn fetch_loop(agent: &ureq::Agent, url: &str, sh: &Shared) {
    loop {
        fetch(agent, url, sh);
        if sh.off.load(Ordering::SeqCst) { break; }
        if sh.again.swap(false, Ordering::SeqCst) { continue; }
        // a refresh may land between the check and the release: take it here
        sh.fetching.store(false, Ordering::SeqCst);
        if !sh.again.swap(false, Ordering::SeqCst) || sh.fetching.swap(true, Ordering::SeqCst) { return; }
    }
    sh.fetching.store(false, Ordering::SeqCst);
}

fn fetch(agent: &ureq::Agent, url: &str, sh: &Shared) -> Option<WatcherView> {
    let mut r = agent.get(url).call().ok()?;
    match r.status().as_u16() {
        404 | 405 => { sh.deliver(None); None }
        200 => {
            let body = r.body_mut().with_config().limit(1 << 20).read_to_string().ok()?;
            let v: Value = serde_json::from_str(&body).ok()?;
            if !v.is_object() { return None; }
            let w = WatcherView::parse(&v);
            sh.deliver(Some(w.clone()));
            Some(w)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_gets_only_mismatches_of_this_session() {
        let w = Watcher::new("http://127.0.0.1:1", "p");
        for id in ["m1", "m2", "m3"] { w.mine(id); }
        let a = |id: &str, r: &str| Audit { message_id: id.into(), result: r.into(), cause: "mask".into(), ..Default::default() };
        let v = WatcherView { audits: vec![a("m1", "match"), a("m2", "skipped"), a("m3", "mismatch"), a("other", "mismatch"), a("m3", "pending")], ..Default::default() };
        w.sh.deliver(Some(v.clone()));
        let ids: Vec<String> = w.take().unwrap().audits.into_iter().map(|a| a.message_id).collect();
        assert_eq!(ids, ["m3"], "match / skipped / other sessions: no line");
        // shown once: the next view repeats the same audits, nothing new
        w.sh.deliver(Some(v));
        assert!(w.take().unwrap().audits.is_empty());
    }

    #[test]
    fn scorecard_old_and_new_shapes() {
        // old gateway: vote columns (ignored), `live` bool; new: no votes, maybe `status`
        let old = json!({"scorecard": [{"domain": "code-rust", "audited_tokens": 100, "agreement": 0.9, "likes": 3, "dislikes": 1, "dislikes_mask": 1,
            "mismatches": 1, "clean_audits": 2, "go_live_at": 20, "live": true}]});
        let new = json!({"scorecard": [{"domain": "code-rust", "audited_tokens": "100", "agreement": 0.9, "mismatches": 1, "skipped": 2,
            "points": 2, "required": 50, "status": "LIVE"}, "junk"]});
        for g in [old, new] {
            let v = WatcherView::parse(&g);
            assert_eq!((v.scorecard.len(), v.scorecard[0].audited_tokens, v.scorecard[0].live), (1, 100, true));
            let t = full_lines(&v).join("\n");
            let head = t.lines().find(|l| l.contains("agree")).unwrap();
            assert!(["agree", "tokens", "miss", "skip", "clean", "points", "status"].iter().all(|c| head.contains(c)), "{head}");
            assert!(!t.contains('👍') && !t.contains('👎') && !t.to_lowercase().contains("like"), "{t}");
        }
    }
}
