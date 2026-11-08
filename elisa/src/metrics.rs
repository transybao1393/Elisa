//! Per-request metrics: every model call of one user request (kind, tokens, first-token time, total time, decode
//! tok/s; recorded by `api::Client`) and the request's totals. One JSON line per request goes to
//! `.elisa/metrics.jsonl`; `/stats` prints the last request and the session totals. Config `metrics = false` turns it
//! off.
use crate::api::CallMetric;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "metrics.jsonl";
/// prompt chars kept in a line (enough to match the same prompt across runs)
const PROMPT_CHARS: usize = 120;

/// Sums over a set of calls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub calls: usize,
    /// prefilled prompt tokens (excludes the cached part)
    pub input_tokens: usize,
    /// reused prompt tokens, summed over the calls that reported them
    pub cached_input_tokens: usize,
    pub output_tokens: usize,
    /// sum of the calls' total times
    pub model_s: f64,
    /// sum of the calls' first-token times
    pub ttft_s: f64,
    /// decode time of the calls with a decode window, and the output tokens / that time
    pub decode_s: f64,
    pub decode_tok_s: Option<f64>,
    #[serde(skip)]
    decode_tokens: usize,
}

impl Totals {
    pub fn add(&mut self, c: &CallMetric) {
        self.calls += 1;
        self.input_tokens += c.input_tokens;
        self.cached_input_tokens += c.cached_input_tokens.unwrap_or(0);
        self.output_tokens += c.output_tokens;
        self.model_s += c.total_s;
        self.ttft_s += c.ttft_s;
        if c.decode_tok_s.is_some() {
            self.decode_s += c.total_s - c.ttft_s;
            self.decode_tokens += c.output_tokens;
        }
        self.decode_tok_s = (self.decode_s > 0.0).then(|| self.decode_tokens as f64 / self.decode_s);
    }
    fn merge(&mut self, o: &Totals) {
        self.calls += o.calls;
        self.input_tokens += o.input_tokens;
        self.cached_input_tokens += o.cached_input_tokens;
        self.output_tokens += o.output_tokens;
        self.model_s += o.model_s;
        self.ttft_s += o.ttft_s;
        self.decode_s += o.decode_s;
        self.decode_tokens += o.decode_tokens;
        self.decode_tok_s = (self.decode_s > 0.0).then(|| self.decode_tokens as f64 / self.decode_s);
    }
    /// `in 1.2K · cached 0.9K · out 310 · model 24.1s · 11.8 tok/s`
    fn line(&self) -> String {
        format!("in {} · cached {} · out {} · model {} · {}", crate::ui::k(self.input_tokens), crate::ui::k(self.cached_input_tokens),
            crate::ui::k(self.output_tokens), crate::ui::dur(self.model_s, 1), self.decode_tok_s.map_or("no decode window".into(), |t| format!("{t:.1} tok/s")))
    }
}

/// One user request: the JSON line written to `.elisa/metrics.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestMetrics {
    /// unix seconds at the start of the request
    pub ts: u64,
    /// the first PROMPT_CHARS chars of the prompt
    pub prompt: String,
    /// done | stopped | failed
    pub outcome: String,
    pub calls: Vec<CallMetric>,
    pub totals: Totals,
    /// totals per call kind (agent, fold, summary)
    pub by_kind: BTreeMap<String, Totals>,
    /// prompt entered → request finished
    pub wall_s: f64,
    /// wall - model: tools, checks, approvals, local work
    pub other_s: f64,
}

impl RequestMetrics {
    /// Fill in a request with its identity fields, calls and wall_s set: trims the prompt and computes totals,
    /// by_kind and other_s.
    pub fn finish(mut self) -> RequestMetrics {
        self.prompt = self.prompt.chars().take(PROMPT_CHARS).collect();
        (self.totals, self.by_kind) = (Totals::default(), BTreeMap::new());
        for c in &self.calls {
            self.totals.add(c);
            self.by_kind.entry(c.kind.clone()).or_default().add(c);
        }
        self.other_s = (self.wall_s - self.totals.model_s).max(0.0);
        self
    }

    /// The `/stats` block for this request.
    pub fn text(&self) -> String {
        let mut s = format!("last request ({}): wall {} = model {} + other {} · {} calls\n  {}\n", self.outcome, crate::ui::dur(self.wall_s, 1),
            crate::ui::dur(self.totals.model_s, 1), crate::ui::dur(self.other_s, 1), self.totals.calls, self.totals.line());
        for c in &self.calls {
            s += &format!("  {:<11} in {} · cached {} · out {} · ttft {} · total {} · {}\n", c.kind, crate::ui::k(c.input_tokens),
                c.cached_input_tokens.map_or("n/a".into(), crate::ui::k), crate::ui::k(c.output_tokens), crate::ui::dur(c.ttft_s, 1), crate::ui::dur(c.total_s, 1),
                c.decode_tok_s.map_or("no decode window".into(), |t| format!("{t:.1} tok/s")));
        }
        s
    }
}

/// Session totals and the last request (what `/stats` shows).
#[derive(Default)]
pub struct Stats { pub last: Option<RequestMetrics>, pub requests: usize, pub totals: Totals, pub wall_s: f64 }

impl Stats {
    pub fn add(&mut self, r: RequestMetrics) {
        self.requests += 1;
        self.totals.merge(&r.totals);
        self.wall_s += r.wall_s;
        self.last = Some(r);
    }

    pub fn text(&self, file: &Path) -> String {
        let Some(last) = &self.last else { return format!("no requests yet in this session (lines go to {})", file.display()) };
        let n = self.requests;
        format!("{}session: {n} requests · wall {} (avg {}) · {} calls · {}\nlines: {}", last.text(), crate::ui::dur(self.wall_s, 1),
            crate::ui::dur(self.wall_s / n as f64, 1), self.totals.calls, self.totals.line(), file.display())
    }
}

/// Appends one JSON line (creates the state dir).
pub fn append(dir: &Path, r: &RequestMetrics) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join(FILE);
    // on disk: the prompt never keeps a secret (redacted on the value, so the JSON stays valid)
    let mut v = serde_json::to_value(r).map_err(|e| e.to_string())?;
    crate::secrets::redact_json(&mut v);
    let line = v.to_string();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(kind: &str, inp: usize, cached: Option<usize>, out: usize, ttft: f64, total: f64) -> CallMetric {
        let d = total - ttft;
        CallMetric { kind: kind.into(), input_tokens: inp, cached_input_tokens: cached, output_tokens: out, ttft_s: ttft, total_s: total,
            decode_tok_s: (d >= crate::api::MIN_DECODE_S).then(|| out as f64 / d), stop_reason: "end_turn".into() }
    }

    #[test]
    fn totals_split_by_kind() {
        let calls = vec![call("fold", 1000, Some(0), 100, 10.0, 20.0), call("agent", 50, Some(900), 40, 1.0, 1.1), call("agent", 30, None, 200, 1.0, 11.0)];
        let req = |ts, calls, wall_s| RequestMetrics { ts, prompt: "fix it".into(), outcome: "done".into(), calls, wall_s, ..Default::default() }.finish();
        let r = req(1, calls, 45.0);
        assert_eq!(r.totals.calls, 3);
        assert_eq!((r.totals.input_tokens, r.totals.cached_input_tokens, r.totals.output_tokens), (1080, 900, 340));
        assert!((r.totals.model_s - 32.1).abs() < 1e-9);
        // the 0.1 s call has no decode window: 300 tokens over 20 s
        assert!((r.totals.decode_tok_s.unwrap() - 15.0).abs() < 1e-9);
        assert!((r.other_s - (45.0 - 32.1)).abs() < 1e-9);
        assert_eq!(r.by_kind["fold"].calls, 1);
        assert_eq!(r.by_kind["agent"].calls, 2);
        let mut s = Stats::default();
        s.add(r.clone());
        s.add(req(2, vec![call("agent", 10, Some(0), 5, 0.5, 0.6)], 1.0));
        assert_eq!((s.requests, s.totals.calls), (2, 4));
        let t = s.text(Path::new(".elisa/metrics.jsonl"));
        assert!(t.contains("last request (done)") && t.contains("session: 2 requests"), "{t}");
        let back: RequestMetrics = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.calls, r.calls);
    }
}
