//! Secret detection and redaction (gitleaks-style rules, hand written).
//!
//! `find` reports secrets without ever exposing a full value (`Hit::masked` is the first
//! four characters plus "…"); `redact` replaces each one with `[redacted:<rule>]`.
//! Used for tool output headed to a remote model (config `redact`), for everything Elisa
//! writes to disk (notes, session logs, run logs), and by the security MCP server.
//!
//! Rules with a recognisable prefix (AKIA…, ghp_…, sk-ant-…, xox…, AIza…, sk_live_…, JWTs,
//! PEM private key blocks) match on shape alone. The context rules — `password = "…"` /
//! `password: value` style assignments and `.env` lines whose KEY looks secret (also written
//! `KEY: value`, `"KEY": "value"`, `- KEY: value`) — also need the value to look
//! random (Shannon entropy and a mix of character classes), so `password = ""`,
//! `token = os.environ[...]`, placeholders and plain words stay untouched.

use regex::Regex;
use std::sync::OnceLock;

/// One secret found in a text. `masked` never holds the full value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rule: &'static str,
    /// 1-based line of the secret's first character
    pub line: usize,
    /// first 4 characters + "…"
    pub masked: String,
}

/// How a rule's match is checked before it counts.
#[derive(Clone, Copy)]
enum Check {
    /// the shape is distinctive enough
    None,
    /// the value must look random (entropy + character classes) and not be a placeholder
    Random,
    /// `Random`, and not a path, URL or dotted name (`looks_like_reference(v, false)`)
    Env,
    /// `Random`, and an unquoted value must not look like a reference to one (see `looks_like_reference`)
    Bare,
    /// a key=value inside a sentence: `Bare`, and the value holds a digit and a letter (not a type or a word)
    Mid,
}

struct Rule {
    name: &'static str,
    re: Regex,
    /// capture group holding the secret (0 = whole match)
    group: usize,
    check: Check,
    /// cheap literal prefilter: skip the regex when none of these occur (empty = always run)
    needles: &'static [&'static str],
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let r = |name, pat: &str, group, check, needles| Rule {
            name,
            re: Regex::new(pat).expect("secret rule regex"),
            group,
            check,
            needles,
        };
        vec![
            r(
                "private-key",
                r"-----BEGIN[A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END[A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|\z)",
                0,
                Check::None,
                &["-----BEGIN"],
            ),
            r(
                "aws-access-key",
                r"\b(?:AKIA|ASIA|ABIA|ACCA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[0-9A-Z]{16}\b",
                0,
                Check::None,
                &["AKIA", "ASIA", "ABIA", "ACCA", "AGPA", "AIDA", "AROA", "ANPA", "ANVA", "AIPA"],
            ),
            r(
                "aws-secret-key",
                r#"(?i)aws[a-z0-9_.\-]{0,20}(?:secret|private)[a-z0-9_.\-]{0,20}["']?\s*(?:=|:|:=|=>)\s*["']?([A-Za-z0-9/+=]{40})(?:[^A-Za-z0-9/+=]|$)"#,
                1,
                Check::Random,
                &["aws", "AWS", "Aws"],
            ),
            r(
                "github-token",
                r"\b(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})\b",
                0,
                Check::None,
                &["gh", "github_pat_"],
            ),
            r("anthropic-key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}", 0, Check::None, &["sk-ant-"]),
            r(
                "openai-key",
                r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{20,}",
                0,
                Check::Random,
                &["sk-"],
            ),
            r(
                "slack-webhook",
                r"https://hooks\.slack\.com/services/T[A-Za-z0-9_]+/B[A-Za-z0-9_]+/[A-Za-z0-9_]+",
                0,
                Check::None,
                &["hooks.slack.com"],
            ),
            r("slack-token", r"\bxox[abposr]-[A-Za-z0-9\-]{10,}", 0, Check::None, &["xox"]),
            r("google-api-key", r"\bAIza[0-9A-Za-z_\-]{35}", 0, Check::None, &["AIza"]),
            r(
                "stripe-key",
                r"\b(?:sk|rk)_live_[0-9A-Za-z]{16,}",
                0,
                Check::None,
                &["_live_"],
            ),
            r(
                "jwt",
                r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
                0,
                Check::None,
                &["eyJ"],
            ),
            // KEY=value in .env / shell style, KEY in caps and secret-looking (also behind a `read` line number
            // `12\t` or a grep prefix `file:12:`); also `KEY: value` / `KEY = value` / `"KEY": "value"` and a markdown
            // list item (`- KEY: value`, `1. **KEY**: value`), the shapes a model reformats a .env file into
            r(
                "env-secret",
                r#"(?m)^(?:[ \t]*\d+\t|[^\s:]+:\d+[:\-])?[ \t]*(?:(?:[-*+]|\d{1,3}[.)])[ \t]+)?(?:export[ \t]+)?[*`"']{0,2}(?:[A-Z][A-Z0-9_]*)?(?:PASSWORD|PASSWD|SECRET|TOKEN|API_?KEY|ACCESS_?KEY|PRIVATE_?KEY|AUTH_?KEY|CREDENTIALS?)[A-Z0-9_]*[*`"']{0,2}[ \t]*(?::=|=|:)[ \t]*["'`]?([^\s"'`#]{8,})"#,
                1,
                Check::Env,
                &[],
            ),
            // the same caps KEY=value / KEY: value inside a sentence, after a space or punctuation (QA N2: "my db password
            // is DB_PASSWORD=Zr4W…" typed by the user); the value must also hold a digit (`Check::Mid`)
            r(
                "env-secret",
                r#"(?:^|[\s(\[{,;])[*`"']{0,2}(?:[A-Z][A-Z0-9_]*)?(?:PASSWORD|PASSWD|SECRET|TOKEN|API_?KEY|ACCESS_?KEY|PRIVATE_?KEY|AUTH_?KEY|CREDENTIALS?)[A-Z0-9_]*[*`"']{0,2}[ \t]*(?::=|=|:)[ \t]*["'`]?([^\s"'`#,;]{8,})"#,
                1,
                Check::Mid,
                &[],
            ),
            // lowercase `password: value` / `api_key=value` inside a sentence, unquoted: as the line-start rule below, and
            // the value must hold a digit and a letter, so a type (`token: TokenStream,`) or a word stays
            r(
                "generic-secret",
                r#"(?i)(?:^|[\s(\[{,;])[*`]{0,2}[a-z0-9_.\-]*(?:password|passwd|pwd|secret|token|api[_\-]?key|apikey|access[_\-]?key|auth[_\-]?key|private[_\-]?key)[a-z0-9_.\-]*[*`]{0,2}[ \t]*(?::=|=>|=|:)[ \t]*`?([^\s"'`#,;]{8,200})"#,
                1,
                Check::Mid,
                &[],
            ),
            // password|secret|token|api_key = "..." in code and config (quoted values)
            r(
                "generic-secret",
                r#"(?i)(?:password|passwd|pwd|secret|token|api[_\-]?key|apikey|access[_\-]?key|auth[_\-]?key|private[_\-]?key)[a-z0-9_.\-]*["']?[ \t]*(?:=|:|:=|=>)[ \t]*["']([^"'\s]{8,200})["']"#,
                1,
                Check::Random,
                &[],
            ),
            // the same keys with an unquoted value that fills the rest of the line (YAML `password: value`, ini
            // `db.password = value`, a model's `- db password: value` list): the key starts the line and the value
            // must also not look like a reference (dotted name, path, URL, kebab-case name)
            r(
                "generic-secret",
                r#"(?im)^(?:[ \t]*\d+\t|[^\s:]+:\d+[:\-])?[ \t]*(?:(?:[-*+]|\d{1,3}[.)])[ \t]+)?(?:export[ \t]+)?[*`]{0,2}[a-z0-9_.\-]*(?:password|passwd|pwd|secret|token|api[_\-]?key|apikey|access[_\-]?key|auth[_\-]?key|private[_\-]?key)[a-z0-9_.\-]*[*`]{0,2}[ \t]*(?::=|=>|=|:)[ \t]*`?([^\s"'`#,;]{8,200})`?[ \t]*(?:#.*)?$"#,
                1,
                Check::Bare,
                &[],
            ),
        ]
    })
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    let mut n = 0u32;
    for b in s.bytes() {
        counts[b as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Does `v` look like a real random credential rather than a word, placeholder or reference?
fn looks_random(v: &str) -> bool {
    if v.len() < 8 {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    const PLACEHOLDERS: &[&str] = &[
        "xxxx", "****", "....", "changeme", "change_me", "example", "your_", "your-", "<", ">",
        "${", "{{", "$(", "%(", "redacted", "placeholder", "dummy", "sample", "process.env",
        "os.environ", "getenv",
    ];
    if v.starts_with('$') || PLACEHOLDERS.iter().any(|p| lower.contains(p)) {
        return false;
    }
    if v.contains('(') || v.contains('[') {
        return false;
    }
    let classes = [
        v.bytes().any(|b| b.is_ascii_lowercase()),
        v.bytes().any(|b| b.is_ascii_uppercase()),
        v.bytes().any(|b| b.is_ascii_digit()),
        v.bytes().any(|b| !b.is_ascii_alphanumeric() && !b"_-.".contains(&b)),
    ]
    .iter()
    .filter(|&&c| c)
    .count();
    classes >= 2 && entropy(v) >= 3.0
}

/// An unquoted config value that names a secret rather than holding it: a dotted name (`settings.API_KEY`), a path
/// (`/run/secrets/db`), a URL without credentials (`https://auth.host/token`), a kebab / snake / dotted lowercase name
/// (`db-credentials-v2`; only with `names`: a .env `SECRET_KEY=my-app-secret-2` is a value).
fn looks_like_reference(v: &str, names: bool) -> bool {
    if v.starts_with('/') || v.starts_with("./") || v.starts_with("~/") || (v.contains("://") && !v.contains('@')) {
        return true;
    }
    let ident = |s: &str| {
        let mut c = s.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    };
    if v.contains('.') && v.split('.').all(ident) {
        return true;
    }
    if !names {
        return false;
    }
    // lowercase words joined by - _ . (the first one a word): a resource name, not a random value
    let parts: Vec<&str> = v.split(['-', '_', '.']).collect();
    parts.len() > 1
        && parts[0].len() >= 2
        && parts[0].bytes().all(|b| b.is_ascii_lowercase())
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// Byte spans (start, end, rule) of every secret, sorted, overlaps merged (first wins).
fn spans(text: &str) -> Vec<(usize, usize, &'static str)> {
    let mut out: Vec<(usize, usize, &'static str)> = Vec::new();
    for rule in rules() {
        if !rule.needles.is_empty() && !rule.needles.iter().any(|n| text.contains(n)) {
            continue;
        }
        for caps in rule.re.captures_iter(text) {
            let Some(m) = caps.get(rule.group) else { continue };
            let mut end = m.end();
            let ok = match rule.check {
                Check::None => true,
                Check::Random => looks_random(m.as_str()),
                Check::Env => looks_random(m.as_str()) && !looks_like_reference(m.as_str(), false),
                Check::Bare => looks_random(m.as_str()) && !looks_like_reference(m.as_str(), true),
                Check::Mid => {
                    let v = m.as_str().trim_end_matches(['.', ')', ']', '}', '!', '?']);
                    end = m.start() + v.len();
                    looks_random(v)
                        && !looks_like_reference(v, true)
                        && v.bytes().any(|b| b.is_ascii_digit())
                        && v.bytes().any(|b| b.is_ascii_alphabetic())
                }
            };
            if !ok {
                continue;
            }
            out.push((m.start(), end, rule.name));
        }
    }
    merge(out)
}

fn mask(v: &str) -> String {
    let head: String = v.chars().take(4).collect();
    format!("{head}…")
}

/// The secret values a session has seen, so a later restatement of one in any context (a model's prose "the key is
/// Zp…", a quote with no key name) is hidden too (QA N1: a value read from `app.cfg` was stored redacted in the tool
/// result but raw in the model's sentence about it). In memory only, never written anywhere. A value is learned
/// only when a rule matched it and it is long and random enough (`learnable`), so ordinary words never get scrubbed.
#[derive(Default, Debug)]
pub struct Scrubber {
    /// (value, rule that found it), longest first so a value that contains another wins
    values: Vec<(String, &'static str)>,
}

/// The most values one session keeps (each scrub scans the text once per value).
const SCRUB_MAX: usize = 512;

/// A matched secret worth remembering: one token of 8..=512 chars that looks random.
fn learnable(v: &str) -> bool {
    (8..=512).contains(&v.len()) && !v.chars().any(char::is_whitespace) && looks_random(v)
}

impl Scrubber {
    /// Remember every secret value the rules find in `text`; returns how many were new.
    pub fn learn(&mut self, text: &str) -> usize {
        let found: Vec<(usize, usize, &'static str)> = spans(text);
        self.add_spans(text, &found)
    }

    fn add_spans(&mut self, text: &str, sp: &[(usize, usize, &'static str)]) -> usize {
        let mut n = 0;
        for &(s, e, rule) in sp {
            // a value that ends a sentence: without its full stop
            let v = text[s..e].trim_end_matches(['.', ')', ']', '}', '!', '?']);
            if self.values.len() >= SCRUB_MAX || !learnable(v) || self.values.iter().any(|(x, _)| x == v) { continue; }
            self.values.push((v.to_string(), rule));
            n += 1;
        }
        if n > 0 { self.values.sort_by_key(|(v, _)| std::cmp::Reverse(v.len())); }
        n
    }

    /// Byte spans of every learned value in `text` (unsorted, may overlap).
    fn occurrences(&self, text: &str) -> Vec<(usize, usize, &'static str)> {
        let mut out = Vec::new();
        for (v, rule) in &self.values {
            out.extend(text.match_indices(v.as_str()).map(|(i, _)| (i, i + v.len(), *rule)));
        }
        out
    }

    /// `text` with every learned value replaced by `[redacted:<rule>]`, and how many were replaced.
    pub fn scrub(&self, text: &str) -> (String, usize) {
        replace(text, merge(self.occurrences(text)))
    }

    /// how many values are remembered
    pub fn len(&self) -> usize { self.values.len() }
    pub fn is_empty(&self) -> bool { self.values.is_empty() }
}

/// This process's (= this session's) scrubber: `find`, `redact` and `learn` feed it, `redact` applies it.
fn session() -> std::sync::MutexGuard<'static, Scrubber> {
    static S: OnceLock<std::sync::Mutex<Scrubber>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Remember the secret values in `text` (a tool's output, a file read, the user's message) for this session, so a
/// later `redact` hides them wherever they appear again, whatever the redact mode for the model is.
pub fn learn(text: &str) -> usize { session().learn(text) }

fn merge(mut out: Vec<(usize, usize, &'static str)>) -> Vec<(usize, usize, &'static str)> {
    out.sort_by_key(|&(s, e, _)| (s, std::cmp::Reverse(e)));
    let mut merged: Vec<(usize, usize, &'static str)> = Vec::with_capacity(out.len());
    for s in out {
        match merged.last_mut() {
            Some(last) if s.0 < last.1 => last.1 = last.1.max(s.1),
            _ => merged.push(s),
        }
    }
    merged
}

fn replace(text: &str, sp: Vec<(usize, usize, &'static str)>) -> (String, usize) {
    if sp.is_empty() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for &(s, e, rule) in &sp {
        out.push_str(&text[pos..s]);
        out.push_str("[redacted:");
        out.push_str(rule);
        out.push(']');
        pos = e;
    }
    out.push_str(&text[pos..]);
    (out, sp.len())
}

/// Every secret in `text`, in order. Values are masked. The values are learned for this session (`learn`).
pub fn find(text: &str) -> Vec<Hit> {
    let mut line = 1;
    let mut pos = 0;
    let sp = spans(text);
    session().add_spans(text, &sp);
    sp
        .into_iter()
        .map(|(s, e, rule)| {
            line += text.as_bytes()[pos..s].iter().filter(|&&b| b == b'\n').count();
            pos = s;
            Hit { rule, line, masked: mask(&text[s..e]) }
        })
        .collect()
}

/// `text` with each secret replaced by `[redacted:<rule>]`, and how many were replaced. Besides the rules' matches,
/// every value this session has seen before (`Scrubber`) is hidden wherever it appears, prose included; the values
/// found here are learned first, so a value repeated later in the same text is hidden too.
pub fn redact(text: &str) -> (String, usize) {
    let mut sp = spans(text);
    let mut s = session();
    s.add_spans(text, &sp);
    if !s.is_empty() { sp.extend(s.occurrences(text)); }
    drop(s);
    replace(text, merge(sp))
}

/// Every string inside a JSON value redacted in place (keys stay); returns the count.
pub fn redact_json(v: &mut serde_json::Value) -> usize {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            let (r, n) = redact(s);
            if n > 0 { *s = r; }
            n
        }
        Value::Array(a) => a.iter_mut().map(redact_json).sum(),
        Value::Object(o) => o.values_mut().map(redact_json).sum(),
        _ => 0,
    }
}

/// Config `redact`: when tool output is redacted before the model sees it. Whatever is written to disk (notes,
/// memory, saved sessions) is redacted in every mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    /// never for the model
    Off,
    /// only when the conversation leaves this machine (a cloud connection) — the default
    Remote,
    /// also for local models
    Always,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() {
            "off" => Some(Mode::Off),
            "remote" | "" => Some(Mode::Remote),
            "always" => Some(Mode::Always),
            _ => None,
        }
    }
    pub fn word(self) -> &'static str {
        match self { Mode::Off => "off", Mode::Remote => "remote", Mode::Always => "always" }
    }
    /// the stricter of the two (a project config may only move up: off < remote < always)
    pub fn stricter(self, o: Mode) -> Mode { self.max(o) }
    /// redact tool output for a model whose requests leave the machine (`remote`) or not
    pub fn applies(self, remote: bool) -> bool {
        match self { Mode::Off => false, Mode::Remote => remote, Mode::Always => true }
    }
}

/// "1 secret" / "3 secrets"
pub fn count_words(n: usize) -> String { format!("{n} secret{}", if n == 1 { "" } else { "s" }) }

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_of(t: &str) -> Vec<&'static str> {
        find(t).into_iter().map(|h| h.rule).collect()
    }

    // Test fixtures are assembled at runtime so this source file never contains a
    // literal that a repository secret scanner would flag.
    fn k(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn aws() {
        let id = k(&["AKIA", "IOSFODNN7EXAMPLQ"]);
        assert_eq!(rules_of(&format!("key: {id}\n")), ["aws-access-key"]);
        let sec = k(&["wJalrXUtnFEMI/K7MDENG/", "bPxRfiCYzQ1w2E3r4T"]);
        assert_eq!(sec.len(), 40);
        assert_eq!(rules_of(&format!("aws_secret_access_key = {sec}\n")), ["aws-secret-key"]);
    }

    #[test]
    fn github() {
        for p in ["ghp_", "gho_", "ghs_"] {
            let t = k(&[p, "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5"]);
            assert_eq!(rules_of(&t), ["github-token"], "{p}");
        }
        let pat = k(&["github_pat_", "11ABCDEFG0123456789_abcdefghijklmnopqrstuv"]);
        assert_eq!(rules_of(&pat), ["github-token"]);
    }

    #[test]
    fn model_keys() {
        let ant = k(&["sk-ant-", "api03-", "Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0"]);
        assert_eq!(rules_of(&format!("ANTHROPIC={ant}")), ["anthropic-key"]);
        let oai = k(&["sk-", "proj-", "Zx9Yw8Vu7Ts6Rq5Po4Nm3Lk2Ji1Hg0"]);
        assert_eq!(rules_of(&format!("key {oai} end")), ["openai-key"]);
    }

    #[test]
    fn slack_google_stripe() {
        let slack = k(&["xox", "b-", "123456789012-1234567890123-AbCdEfGhIjKlMnOp"]);
        assert_eq!(rules_of(&slack), ["slack-token"]);
        let hook = k(&["https://hooks.slack.com/services/", "T00000000/B00000000/XXXXabcd1234"]);
        assert_eq!(rules_of(&hook), ["slack-webhook"]);
        let g = k(&["AIza", "SyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q"]);
        assert_eq!(rules_of(&format!("\"{g}\"")), ["google-api-key"]);
        for p in ["sk_live_", "rk_live_"] {
            let s = k(&[p, "51H8abcDEF123ghiJKL456mno"]);
            assert_eq!(rules_of(&s), ["stripe-key"], "{p}");
        }
    }

    #[test]
    fn private_key_block() {
        let begin = k(&["-----BEGIN ", "RSA PRIVATE KEY-----"]);
        let end = k(&["-----END ", "RSA PRIVATE KEY-----"]);
        let t = format!("x\n{begin}\nMIIEowIBAAKCAQEA\nabc\n{end}\ny");
        let (r, n) = redact(&t);
        assert_eq!(n, 1);
        assert_eq!(r, "x\n[redacted:private-key]\ny");
        assert_eq!(find(&t)[0].line, 2);
        // truncated output (no END line) still hides the rest
        let (r, n) = redact(&format!("{begin}\nMIIEowIBAAKCAQEA"));
        assert_eq!((r.as_str(), n), ("[redacted:private-key]", 1));
        let open = k(&["-----BEGIN ", "OPENSSH PRIVATE KEY-----"]);
        assert_eq!(rules_of(&format!("{open}\nb3BlbnNzaC1rZXk")), ["private-key"]);
    }

    #[test]
    fn jwt() {
        let t = k(&[
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0.",
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ]);
        assert_eq!(rules_of(&format!("Authorization: Bearer {t}")), ["jwt"]);
    }

    #[test]
    fn generic_and_env() {
        let (r, n) = redact(r#"db_password = "Tr0ub4dor&3xQ""#);
        assert_eq!(n, 1);
        assert_eq!(r, r#"db_password = "[redacted:generic-secret]""#);
        assert_eq!(rules_of(r#"{"api_key": "9f8a7B6c5D4e3F2a1b"}"#), ["generic-secret"]);
        assert_eq!(rules_of("token: 'Qm9iMzQ1NjdBYkNk'"), ["generic-secret"]);
        let (r, n) = redact("PORT=8080\nDB_PASSWORD=s3cr3tPassW0rd!\nexport STRIPE_SECRET=abcD1234efgH5678\n");
        assert_eq!(n, 2);
        assert_eq!(redact("2\tDB_PASSWORD=s3cr3tPassW0rd!\n").0, "2\tDB_PASSWORD=[redacted:env-secret]\n");
        assert_eq!(redact(".env:3:API_KEY=abcD1234efgH5678").1, 1);
        assert_eq!(
            r,
            "PORT=8080\nDB_PASSWORD=[redacted:env-secret]\nexport STRIPE_SECRET=[redacted:env-secret]\n"
        );
    }

    /// The shapes a model (or a config file) writes a secret in besides `KEY=value` (QA 2026-11-08: a 2B model
    /// listed a .env file as `- DB_PASSWORD: value` and the raw value reached the saved session).
    #[test]
    fn reformatted_assignments() {
        let v = "TPcpXk9q2LmR7vWz4bNs";
        let forms = [
            format!("DB_PASSWORD: {v}"),
            format!("- DB_PASSWORD: {v}"),
            format!("* DB_PASSWORD: `{v}`"),
            format!("1. **DB_PASSWORD**: {v}"),
            format!("DB_PASSWORD = {v}"),
            format!("export DB_PASSWORD={v}"),
            format!("  \"DB_PASSWORD\": \"{v}\","),
            format!("{{\"password\": \"{v}\"}}"),
            format!("password: {v}"),
            format!("  db_password: {v}   # prod"),
            format!("database.password = {v}"),
            format!("api_key: '{v}'"),
            format!("STRIPE_SECRET: {v}"),
            format!("12\tpassword: {v}"),
            format!("config.yml:4:  password: {v}"),
        ];
        for f in &forms {
            let (r, n) = redact(f);
            assert_eq!(n, 1, "missed {f:?}");
            assert!(!r.contains(v), "{f:?} -> {r:?}");
        }
        // a whole reformatted listing
        let list = format!("Settings:\n- PORT: 8080\n- DB_PASSWORD: {v}\n- DEBUG: true\n");
        assert_eq!(redact(&list).0, "Settings:\n- PORT: 8080\n- DB_PASSWORD: [redacted:env-secret]\n- DEBUG: true\n");
    }

    #[test]
    fn false_positives() {
        let clean = [
            "id = 550e8400-e29b-41d4-a716-446655440000",
            "user_id: \"123e4567-e89b-12d3-a456-426614174000\"",
            "commit 9fceb02d0ae598e95dc970b74767f19372d61af8",
            "Merge: 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
            "src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==\"",
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.",
            "password = \"\"",
            "password = ''",
            "token = os.environ[\"GITHUB_TOKEN\"]",
            "api_key = os.getenv(\"OPENAI_API_KEY\")",
            "const token = process.env.TOKEN;",
            "password = \"changeme\"",
            "secret: \"<your-secret-here>\"",
            "GITHUB_TOKEN=${{ secrets.GITHUB_TOKEN }}",
            "API_KEY=$OPENAI_API_KEY",
            "token_type = \"bearer_token\"",
            "SECRET_KEY_BASE=xxxxxxxxxxxxxxxx",
            "password: \"password\"",
            "let task_risk-assessment-framework-something = 1;",
            "pip install scikit-learn-extra-tools-and-more",
            "aws_region = \"us-east-1\"",
            // unquoted values that name a secret rather than hold it
            "password: ${DB_PASSWORD}",
            "api_key = settings.OPENAI_API_KEY",
            "PASSWORD_FILE=/run/secrets/db_password",
            "token_url: https://login.microsoftonline.com/common/oauth2/v2.0/token",
            "  secretName: db-credentials-v2",
            "password: password",
            "password: hunter2",
            "token_type: bearer_token",
            "TOKEN: expired",
            "let token = compute(x);",
            "    token = get_token(cfg)",
            "PASSWORD: ********",
            "- secret: {{ vault_secret }}",
            "GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}",
            "  password_min_length: 12345678",
        ];
        for c in clean {
            assert!(find(c).is_empty(), "false positive on {c:?}: {:?}", find(c));
        }
    }

    /// QA N2: caps `KEY=value` and `key: value` inside a sentence (the user typing a password into the prompt).
    #[test]
    fn mid_sentence_assignments() {
        let v = k(&["Zr4W", "q8Lp2Nx6Tb9Vk3Hs"]);
        let t = format!("Just reply OK, no tools: my db password is DB_PASSWORD={v} and password: {v}");
        let (r, n) = redact(&t);
        assert!(!r.contains(&v), "{r}");
        assert!(r.contains("DB_PASSWORD=[redacted:env-secret]") && n == 2, "{r}");
        for f in [
            format!("set (API_KEY: {v}) first"),
            format!("then STRIPE_SECRET={v}."),
            format!("the token={v}, ok"),
            format!("use api_key: `{v}` here"),
        ] {
            let (r, n) = redact(&f);
            assert!(n == 1 && !r.contains(&v), "{f:?} -> {r:?}");
        }
        // the full stop that ends the sentence is not part of the value
        assert_eq!(redact(&format!("it is DB_PASSWORD={v}.")).0, "it is DB_PASSWORD=[redacted:env-secret].");
    }

    /// Sentences and code with a key word mid-line that hold no secret.
    #[test]
    fn mid_sentence_false_positives() {
        let clean = [
            "    fn new(token: TokenStream, secret: SecretString) -> Self {",
            "struct Cfg { api_key: Option<String>, password: String }",
            "the password: changeme please, the token: expired",
            "let t = Lexer { token: TokenKind::Ident, pos: 0 };",
            "call login(password=hunter2) and api_key=settings.OPENAI_API_KEY then",
            "see https://example.com/?token=abc for the token: see docs",
            "pass DB_PASSWORD=$DB_PASSWORD to it, or API_KEY=${{ secrets.API_KEY }}",
            "the field password_min_length: 12345678 stays",
            "it reads PASSWORD_FILE=/run/secrets/db and secret: db-credentials-v2 here",
        ];
        for c in clean {
            assert!(spans(c).is_empty(), "false positive on {c:?}: {:?}", spans(c));
        }
    }

    /// QA N1: a value seen once is hidden later wherever it appears, prose included.
    #[test]
    fn scrubber_learns_values_and_scrubs_any_context() {
        let v = k(&["ZpQ8", "1kLmN3vB7xR2tY6w"]);
        let mut s = Scrubber::default();
        assert_eq!(s.learn(&format!("1\tNAME=demo\n2\tAPI_KEY_PROD={v}\n")), 1);
        assert_eq!(s.learn(&format!("API_KEY_PROD={v}")), 0, "known already");
        let (r, n) = s.scrub(&format!("There is a secret in app.cfg with the value {v}. Rotate {v}!"));
        assert_eq!(n, 2);
        assert_eq!(r, "There is a secret in app.cfg with the value [redacted:env-secret]. Rotate [redacted:env-secret]!");
        // short, low-entropy and multi-line matches are never learned, so plain words are never scrubbed
        let mut s = Scrubber::default();
        assert_eq!(s.learn("password = \"changeme\"\nTOKEN: expired\nPASSWORD: hunter2"), 0);
        assert!(s.is_empty());
        let begin = k(&["-----BEGIN ", "RSA PRIVATE KEY-----"]);
        assert_eq!(s.learn(&format!("{begin}\nMIIEowIBAAKCAQEA\nabc\n")), 0);
        assert_eq!(s.scrub("nothing here"), ("nothing here".to_string(), 0));
    }

    /// The session's own scrubber: `redact` learns what it hides and hides it again in later text without a key.
    #[test]
    fn redact_hides_a_value_seen_before() {
        let v = k(&["Qx7m", "Lp3Rt9Vb2Nw5Kz8J"]);
        assert_eq!(redact(&format!("no secret {v} yet")).1, 0, "unknown value in prose stays");
        assert_eq!(redact(&format!("SERVICE_TOKEN={v}")).1, 1);
        assert_eq!(redact(&format!("the value was {v}")).0, "the value was [redacted:env-secret]");
        let w = k(&["Hb4n", "Wc8Ty2Pq6Ms1Ld9F"]);
        assert_eq!(learn(&format!("2\tAUTH_KEY={w}\n")), 1);
        assert_eq!(redact(&format!("[\"{w}\"]")).0, "[\"[redacted:env-secret]\"]");
        // the same text: the keyed occurrence teaches the bare one
        let x = k(&["Mv5c", "Rk2Xs7Bn4Gt9Wp3D"]);
        assert_eq!(redact(&format!("x {x} and SECRET_KEY={x}")).0, "x [redacted:env-secret] and SECRET_KEY=[redacted:env-secret]");
    }

    #[test]
    fn masked_never_full() {
        let t = k(&["ghp_", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5"]);
        let h = &find(&t)[0];
        assert_eq!(h.masked, "ghp_…");
        assert!(!h.masked.contains(&t[4..]));
    }

    #[test]
    fn lines_and_counts() {
        let a = k(&["AKIA", "IOSFODNN7EXAMPLQ"]);
        let g = k(&["ghp_", "aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5"]);
        let t = format!("one\n{a}\nthree\n\nfive {g}\n");
        let hits = find(&t);
        assert_eq!(hits.iter().map(|h| h.line).collect::<Vec<_>>(), [2, 5]);
        let (r, n) = redact(&t);
        assert_eq!(n, 2);
        assert!(!r.contains(&a) && !r.contains(&g));
        assert_eq!(redact("nothing here"), ("nothing here".to_string(), 0));
    }

    #[test]
    fn one_megabyte_is_fast() {
        let mut big = String::with_capacity(1 << 20);
        let line = "fn main() { let token = compute(x); println!(\"{}\", 9fceb02d0ae598e95dc970b74767f19372d61af8); } // lorem ipsum\n";
        while big.len() < (1 << 20) {
            big.push_str(line);
        }
        big.push_str(&k(&["password = \"Tr0ub4dor&3xQ\"\n"]));
        let t0 = std::time::Instant::now();
        let (_, n) = redact(&big);
        let dt = t0.elapsed();
        assert_eq!(n, 1);
        let limit = if cfg!(debug_assertions) { 2000 } else { 100 };
        assert!(dt.as_millis() < limit, "1 MB took {dt:?}");
    }
}
