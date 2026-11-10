//! Elisa's own reasoning server for small models: `elisa --mcp-serve reason`, a stdio MCP server with four tiny tools:
//! `think(thought)` (a scratchpad: records the thought, says "act now"), `steps(list)` + `step_done(n)` (a step list
//! kept in this process for the session; each answer shows progress and the next step) and `calc(expr)` (arithmetic,
//! `evalexpr`). No plan mode and no plan UI: these are only tools the model may call.
//!
//! No waste on simple prompts: the session offers these tools only on turns whose request looks like a multi-step task
//! (`wanted`, a pure heuristic on the user's words, decided once at turn start, config `reason`), and the server caps
//! planning per turn (THINK_MAX thoughts, STEPS_MAX step lists): past that it answers "enough planning — act now".
//! The host starts a new turn with the notification `notifications/elisa/turn` (`TURN`).
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// the server name (config `[mcp.reason] enabled = false` switches it off); its tools keep their own names
pub const NAME: &str = "reason";
/// the host's "a new user turn starts" notification: per-turn caps start again
pub const TURN: &str = "notifications/elisa/turn";
pub const THINK_MAX: usize = 3;
pub const STEPS_MAX: usize = 1;
const EXPR_MAX: usize = 200;
const STEP_MAX_CHARS: usize = 120;
const STEPS_LIMIT: usize = 12;
const ENOUGH: &str = "Enough planning — act now.";
const EMPTY_THOUGHT: &str = "No thought given. Do not call think again: act now (call the tool you need) or answer.";

/// the tools this server lists (MCP `tools/list`); kept tiny: they ride in the prompt of every task turn
pub fn tools() -> Value {
    let one = |k: &str, t: &str| json!({"type": "object", "properties": {k: {"type": t}}, "required": [k]});
    json!([
        {"name": "think", "description": "Multi-step tasks only, never simple questions: note facts and your next action.", "inputSchema": one("thought", "string")},
        {"name": "steps", "description": "Multi-step tasks only: steps as \"a; b; c\".", "inputSchema": one("list", "string")},
        {"name": "step_done", "description": "Mark step n done; shows the next.", "inputSchema": one("n", "integer")},
        {"name": "calc", "description": "Exact math for tasks, e.g. (17*23)/4.", "inputSchema": one("expr", "string")},
    ])
}

/// the names of this server's tools
pub fn names() -> [&'static str; 4] { ["think", "steps", "step_done", "calc"] }

/// the server's session state: the step list, and this turn's planning calls
#[derive(Default, Debug)]
pub struct State { steps: Vec<(String, bool)>, thoughts: usize, lists: usize }

impl State {
    pub fn new_turn(&mut self) { self.thoughts = 0; self.lists = 0; }

    fn next(&self) -> String {
        match self.steps.iter().position(|(_, d)| !d) {
            Some(i) => format!("Next: {}) {}", i + 1, self.steps[i].0),
            None => "All steps done: check the result and answer.".into(),
        }
    }

    fn done_line(&self) -> String {
        format!("Done {}/{}. {}", self.steps.iter().filter(|(_, d)| *d).count(), self.steps.len(), self.next())
    }
}

/// one `tools/call`: (text, is_error)
pub fn call(name: &str, args: &Value, st: &mut State) -> (String, bool) {
    let arg = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    match name {
        "think" => {
            // every call counts, an empty one too (a small model can send many empty thinks in
            // one turn, and uncapped they never end); an empty one is not an error to retry but a push to act
            st.thoughts += 1;
            if st.thoughts > THINK_MAX { return (ENOUGH.into(), false); }
            if arg("thought").is_none() { return (EMPTY_THOUGHT.into(), false); }
            ("Noted. Now act: call the tool you need or answer.".into(), false)
        }
        "steps" => {
            if st.lists >= STEPS_MAX {
                return (if st.steps.is_empty() { ENOUGH.into() } else { format!("{ENOUGH} {}", st.done_line()) }, false);
            }
            let Some(list) = arg("list") else { return ("missing list: give the steps as \"a; b; c\"".into(), true) };
            let steps = parse_steps(list);
            if steps.is_empty() { return ("no steps: say what each one does, e.g. \"read calc.py; fix average; run the tests\"".into(), true); }
            st.lists += 1;
            st.steps = steps.into_iter().map(|s| (s, false)).collect();
            (format!("{} steps. {}. Call step_done when it is done.", st.steps.len(), st.next()), false)
        }
        "step_done" => {
            if st.steps.is_empty() { return ("no steps yet: act, or call steps first for a long task".into(), true); }
            let n = match &args["n"] {
                Value::Number(x) => x.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0).map(|f| f as usize),
                Value::String(s) => s.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect::<String>().parse().ok(),
                _ => None,
            };
            match n {
                Some(n) if (1..=st.steps.len()).contains(&n) => { st.steps[n - 1].1 = true; (st.done_line(), false) }
                _ => (format!("n must be a step number from 1 to {}", st.steps.len()), true),
            }
        }
        "calc" => match arg("expr") {
            Some(e) => match calc(e) { Ok(v) => (v, false), Err(e) => (e, true) },
            None => ("missing expr".into(), true),
        },
        n => (format!("unknown tool {n}"), true),
    }
}

/// "a; b; c" (or one step per line, numbered or bulleted) as steps
fn parse_steps(list: &str) -> Vec<String> {
    list.split([';', '\n']).map(|s| {
        let s = s.trim().trim_start_matches(['-', '*', '•']).trim();
        // "1." / "2)" / "Step 3:" numbering
        let s = s.strip_prefix("Step ").or_else(|| s.strip_prefix("step ")).unwrap_or(s);
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        let s = if digits > 0 && s[digits..].starts_with(['.', ')', ':']) { s[digits + 1..].trim() } else { s };
        s.chars().take(STEP_MAX_CHARS).collect::<String>()
    }).filter(|s| s.chars().any(char::is_alphabetic)).take(STEPS_LIMIT).collect()
}

/// friendly function names the model may use, and what evalexpr calls them
const FUNCS: &[(&str, &str)] = &[
    ("sqrt", "math::sqrt"), ("abs", "math::abs"), ("ln", "math::ln"), ("log10", "math::log10"), ("log2", "math::log2"),
    ("exp", "math::exp"), ("pow", "math::pow"), ("sin", "math::sin"), ("cos", "math::cos"), ("tan", "math::tan"),
    ("floor", "floor"), ("round", "round"), ("ceil", "ceil"), ("min", "min"), ("max", "max"),
];

/// The expression in evalexpr's words, every number a float when `float` (so 7/2 is 3.5, not 3). Only numbers,
/// operators, parentheses, commas and the FUNCS names (plus `pi`) pass: no variables, strings or assignments.
fn prepare(expr: &str, float: bool) -> Result<String, String> {
    let mut out = String::new();
    let cs: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_ascii_digit() || (c == '.' && cs.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.' || cs[i] == '_') { i += 1; }
            // exponent: 1e5, 2.5E-3
            if i < cs.len() && (cs[i] == 'e' || cs[i] == 'E') {
                let j = if cs.get(i + 1).is_some_and(|c| *c == '+' || *c == '-') { i + 2 } else { i + 1 };
                if cs.get(j).is_some_and(char::is_ascii_digit) {
                    i = j;
                    while i < cs.len() && cs[i].is_ascii_digit() { i += 1; }
                }
            }
            let num: String = cs[start..i].iter().filter(|c| **c != '_').collect();
            let num = if num.starts_with('.') { format!("0{num}") } else { num };
            let is_float = num.contains(['.', 'e', 'E']);
            out += &num;
            if float && !is_float { out += ".0"; } else if num.ends_with('.') { out += "0"; }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_') { i += 1; }
            let w: String = cs[start..i].iter().collect::<String>().to_lowercase();
            if w == "pi" { out += "3.141592653589793"; continue; }
            match FUNCS.iter().find(|(n, _)| *n == w) {
                Some((_, f)) => out += f,
                None => return Err(format!("calc: unknown name {w:?} (numbers, + - * / % ^, parentheses, sqrt abs ln log10 log2 exp pow sin cos tan floor round ceil min max pi)")),
            }
            continue;
        }
        match c {
            '×' | '·' => out.push('*'),
            '÷' => out.push('/'),
            '−' => out.push('-'),
            '+' | '-' | '*' | '/' | '%' | '^' | '(' | ')' | ',' | ' ' | '\t' | '<' | '>' | '=' | '!' => out.push(c),
            c => return Err(format!("calc: unexpected character {c:?}")),
        }
        i += 1;
    }
    Ok(out)
}

/// A float result for a person: whole numbers without ".0", others to 12 significant digits (0.1+0.2 = 0.3).
fn show(f: f64) -> Result<String, String> {
    if !f.is_finite() { return Err("calc: the result is not a finite number (division by zero?)".into()); }
    if f.fract() == 0.0 && f.abs() < 1e15 { return Ok(format!("{f:.0}")); }
    let sci = format!("{f:.11e}");
    if f.abs() >= 1e15 || f.abs() < 1e-6 {
        // 1.84467440737e19: the mantissa without trailing zeros
        let (m, x) = sci.split_once('e').unwrap_or((&sci, "0"));
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        return Ok(format!("{m}e{x}"));
    }
    let r: f64 = sci.parse().unwrap_or(f);
    Ok(format!("{r}"))
}

/// Exact arithmetic: integers stay exact (i64; overflow falls back to floats), any division is done in floats.
/// Errors are short sentences; never panics (evalexpr runs under catch_unwind).
pub fn calc(expr: &str) -> Result<String, String> {
    let e = expr.trim().trim_end_matches(['?', ' ']).trim_end_matches('=').trim();
    if e.is_empty() { return Err("calc: empty expression".into()); }
    if e.chars().count() > EXPR_MAX { return Err(format!("calc: expression too long (at most {EXPR_MAX} characters)")); }
    let run = |s: String| std::panic::catch_unwind(|| evalexpr::eval_with_context(&s, &evalexpr::EmptyContextWithBuiltinFunctions))
        .map_err(|_| "calc: could not evaluate the expression".to_string());
    let fmt = |v: evalexpr::Value| -> Result<String, String> {
        match v {
            evalexpr::Value::Int(i) => Ok(i.to_string()),
            evalexpr::Value::Float(f) => show(f),
            evalexpr::Value::Boolean(b) => Ok(b.to_string()),
            evalexpr::Value::Tuple(t) => Err(format!("calc: one expression at a time (got {} values)", t.len())),
            _ => Err("calc: not a number".into()),
        }
    };
    if !e.contains(['/', '÷']) {
        match run(prepare(e, false)?)? {
            Ok(v) => return fmt(v),
            // only an integer overflow is worth another try in floats
            Err(evalexpr::EvalexprError::AdditionError { .. } | evalexpr::EvalexprError::SubtractionError { .. }
                | evalexpr::EvalexprError::MultiplicationError { .. } | evalexpr::EvalexprError::NegationError { .. }) => {}
            Err(err) => return Err(format!("calc: {err}")),
        }
    }
    match run(prepare(e, true)?)? { Ok(v) => fmt(v), Err(err) => Err(format!("calc: {err}")) }
}

// ----- the per-turn gate (no model call) -----

/// config `reason`: when the session offers this server's tools
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Auto, Always, Off }

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() { "auto" => Some(Mode::Auto), "always" | "on" => Some(Mode::Always), "off" => Some(Mode::Off), _ => None }
    }
}

/// verbs that start real work in a codebase: a strong sign of several steps (first word: 2 points)
const TASK_VERBS: &[&str] = &[
    "fix", "build", "refactor", "implement", "add", "debug", "migrate", "compare", "port", "integrate", "optimize",
    "optimise", "install", "deploy", "configure", "setup", "upgrade", "rewrite", "investigate", "scaffold", "audit",
];
/// verbs that may or may not mean several steps ("write a haiku" vs "write tests for X"): 1 point
const SOFT_VERBS: &[&str] = &[
    "write", "create", "make", "update", "change", "replace", "remove", "delete", "rename", "move", "convert", "test",
    "set", "split", "merge", "clean", "extract", "wire", "hook", "support", "generate", "design", "review", "run",
];
/// short replies that carry the previous turn on (the tool set stays as it was: no cache flip)
const FOLLOW_UPS: &[&str] = &[
    "continue", "go on", "go ahead", "keep going", "proceed", "next", "do it", "yes", "y", "yep", "yeah", "ok", "okay",
    "sure", "try again", "retry", "again", "carry on", "yes please", "please continue", "ok continue", "resume",
];
const FILE_EXTS: &[&str] = &[
    "rs", "py", "swift", "ts", "tsx", "js", "jsx", "go", "c", "h", "cpp", "hpp", "java", "kt", "rb", "php", "cs", "sh",
    "md", "toml", "json", "yaml", "yml", "html", "css", "sql", "txt", "lock", "cfg", "ini", "xml", "vue", "lua",
];

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// a word that names a file or path: `src/web.rs`, `README.md`, `./x`, `Cargo.toml`
pub fn names_file(text: &str) -> bool {
    text.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| "`'\"()[]{},;:!?".contains(c));
        let ext = w.rsplit_once('.').map(|(a, e)| !a.is_empty() && FILE_EXTS.contains(&e.to_lowercase().as_str())).unwrap_or(false);
        let path = w.contains('/') && w.chars().filter(|c| c.is_alphanumeric()).count() >= 3 && !w.starts_with("http");
        ext || path || w == "readme" || w == "README" || w == "Makefile" || w == "Dockerfile"
    })
}

/// The user's message, judged without a model: Some(true) = a multi-step task (offer the reasoning tools),
/// Some(false) = a simple question, a greeting or chat (no tools), None = a short follow-up ("continue", "yes")
/// that keeps the previous turn's choice. Signals: a task verb (first, after "please" / "can you", or a second one
/// joined by "and" / "then"), a file / path or a code block, several sentences or list items, length.
pub fn wanted(msg: &str) -> Option<bool> {
    let t = msg.trim();
    let norm = words(t).join(" ");
    if norm.is_empty() { return Some(false); }
    if FOLLOW_UPS.contains(&norm.as_str()) { return None; }
    let w = words(t);
    let mut score = 0;
    if t.contains("```") { score += 2; }
    if names_file(t) { score += 1; }
    // the first real word, after politeness ("please", "can you", "could you please", "i want you to", "let's")
    const SKIP: &[&str] = &["please", "can", "could", "would", "will", "you", "pls", "now", "then", "also", "i", "want", "need", "to", "let's", "lets", "help", "me", "go", "ahead", "and"];
    let first = w.iter().find(|x| !SKIP.contains(&x.as_str()));
    let verb = |x: &str| TASK_VERBS.contains(&x) || SOFT_VERBS.contains(&x);
    match first {
        Some(f) if TASK_VERBS.contains(&f.as_str()) => score += 2,
        Some(f) if SOFT_VERBS.contains(&f.as_str()) => score += 1,
        _ => {}
    }
    // a second requirement: "and update", "then run", "also add"
    let joined = w.windows(2).filter(|p| matches!(p[0].as_str(), "and" | "then" | "also" | "plus") && verb(&p[1])).count();
    score += joined.min(2);
    // several sentences / lines / list items
    let parts = t.split(['.', '!', '?', '\n', ';']).filter(|s| s.split_whitespace().count() >= 3).count();
    if parts >= 3 { score += 1; }
    if w.len() > 40 { score += 1; }
    if w.len() > 80 { score += 1; }
    // a short question with no file or code is a lookup, whatever verb it uses ("how do I add a remote?")
    let question = t.ends_with('?') || w.first().is_some_and(|f| matches!(f.as_str(), "what" | "what's" | "whats" | "who" | "when" | "where" | "why" | "how" | "which" | "is" | "are" | "does" | "do" | "can" | "should" | "explain" | "define" | "tell"));
    if question && w.len() <= 15 && score < 3 && !names_file(t) && !t.contains("```") { return Some(false); }
    Some(score >= 2)
}

/// This turn's choice: the mode, the message and the previous turn's choice (a follow-up keeps it).
pub fn decide(mode: Mode, msg: &str, prev: bool) -> bool {
    match mode { Mode::Off => false, Mode::Always => true, Mode::Auto => wanted(msg).unwrap_or(prev) }
}

// ----- relevance gate for the built-in tool servers (config `gate_tools`, no model call) -----

/// The built-in servers whose tools come and go per turn (`gate`). Their tools sit after every stable tool in the
/// request, in this order, so a flip only changes the tail of the tool list.
pub const GATED: [&str; 2] = ["docs", "sec"];

/// words that make a turn about documents (the `docs` server reads PDFs and spreadsheets): the names of the file
/// kinds it reads
const DOC_WORDS: &[&str] = &[
    "pdf", "pdfs", "spreadsheet", "spreadsheets", "excel", "workbook", "csv", "tsv", "xlsx", "xls", "ods", "xlsm",
];
/// everyday nouns that name a document only when they point at one that exists ("the sheet", "this document", "my
/// sheets"); "a markdown cheat sheet" or "write a document about X" asks for new text, and the `docs` tools only
/// read files (a cheat sheet request must not get the documents tools and a `docs_sheet` call)
const DOC_NOUNS: &[&str] = &["document", "documents", "sheet", "sheets"];
/// words that point at something that exists (a definite or possessive determiner)
const POINTING: &[&str] = &["the", "this", "that", "these", "those", "my", "your", "our", "his", "her", "their", "its"];
/// words that introduce something new (an indefinite determiner)
const NEW_THING: &[&str] = &["a", "an", "another", "some", "any"];

/// A `DOC_NOUNS` word at `i` that points at an existing document: the nearest determiner in the three words before it
/// (modifiers may stand between: "the sales sheet", "my monthly budget sheet") is a pointing one.
fn points_at_doc(w: &[String], i: usize) -> bool {
    w[i.saturating_sub(3)..i].iter().rev().find_map(|x| {
        if POINTING.contains(&x.as_str()) { Some(true) } else if NEW_THING.contains(&x.as_str()) { Some(false) } else { None }
    }).unwrap_or(false)
}
/// file extensions the `docs` server reads
const DOC_EXTS: &[&str] = &["pdf", "xlsx", "xls", "xlsm", "ods", "csv", "tsv"];
/// words that make a turn about security (the `sec` server: secrets, vulnerable dependencies, binaries, JWTs)
const SEC_WORDS: &[&str] = &[
    "security", "secure", "secret", "secrets", "leak", "leaks", "leaked", "leaking", "credential", "credentials",
    "password", "passwords", "vulnerability", "vulnerabilities", "vulnerable", "vuln", "vulns", "cve", "cves", "osv",
    "dependency", "dependencies", "deps", "jwt", "jwts", "binary", "binaries", "malware", "exploit", "exploits",
    "sbom", "advisory", "advisories", "audit", "apikey", "api-key",
];
/// a name of one of the `sec` / `docs` tools in the user's words ("use sec_deps") also counts
fn names_tool(words: &[String], server: &str) -> bool { words.iter().any(|w| w.starts_with(&format!("{server}_"))) }

/// Whether `text` (the user's words, or recent context) is about documents.
pub fn about_docs(text: &str) -> bool {
    let w = words(text);
    if w.iter().any(|x| DOC_WORDS.contains(&x.as_str())) || names_tool(&w, "docs") { return true; }
    if w.iter().enumerate().any(|(i, x)| DOC_NOUNS.contains(&x.as_str()) && points_at_doc(&w, i)) { return true; }
    text.split(|c: char| c.is_whitespace() || "`'\"()[]{},;:!?<>".contains(c)).any(|t| {
        t.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && DOC_EXTS.contains(&e.trim_end_matches('.').to_lowercase().as_str()))
    })
}

/// Whether `text` (the user's words) is about security.
pub fn about_sec(text: &str) -> bool {
    let w = words(text);
    w.iter().any(|x| SEC_WORDS.contains(&x.as_str())) || names_tool(&w, "sec") || text.to_lowercase().contains("api key")
}

/// This turn's gated servers (`GATED`): which of `docs` / `sec` the request offers. `msg`: the user's words;
/// `recent`: text of the recent context (the last few messages: a document file read or listed there keeps `docs`);
/// `prev`: the last turn's choice, kept by a short follow-up ("continue", "yes"). `off` (config `gate_tools = "off"`):
/// both always. Decided once per turn, so a turn's requests keep one tool list.
pub fn gate(off: bool, msg: &str, recent: &str, prev: (bool, bool)) -> (bool, bool) {
    if off { return (true, true); }
    if wanted(msg).is_none() { return prev; }
    (about_docs(msg) || about_docs(recent), about_sec(msg))
}

/// the answer to one JSON-RPC message; None for a notification
pub fn answer(m: &Value, st: &mut State) -> Option<Value> {
    if m["method"] == TURN { st.new_turn(); }
    let id = m.get("id")?.clone();
    let result = match m["method"].as_str().unwrap_or("") {
        "initialize" => json!({"protocolVersion": m["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}}, "serverInfo": {"name": "elisa-reason", "version": env!("CARGO_PKG_VERSION")}}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let (text, err) = call(m["params"]["name"].as_str().unwrap_or(""), &m["params"]["arguments"], st);
            json!({"content": [{"type": "text", "text": text}], "isError": err})
        }
        other => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `--mcp-serve reason`: serve stdin / stdout until stdin closes
pub fn serve(_args: &[String]) -> i32 {
    let mut st = State::default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(a) = answer(&m, &mut st) {
            if writeln!(out, "{a}").and_then(|()| out.flush()).is_err() { return 0; }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_and_sec_tools_only_on_turns_about_them() {
        let none = (false, false);
        // a plain coding turn: neither
        assert_eq!(gate(false, "fix low_stock in invtrack/core.py and run the tests", "", none), (false, false));
        assert_eq!(gate(false, "hi Elisa! how are you today?", "", none), (false, false));
        // documents: the words, a file name, or a document in the recent context
        assert_eq!(gate(false, "Read docs/spec.pdf and tell me the reorder rule", "", none), (true, false));
        assert_eq!(gate(false, "which item in data/inventory.csv costs most?", "", none), (true, false));
        assert_eq!(gate(false, "what does the spreadsheet say?", "", none), (true, false));
        assert_eq!(gate(false, "who is the warehouse manager?", "[result: docs/spec.pdf\nREADME.md]", none), (true, false));
        // security
        assert_eq!(gate(false, "Scan this project for leaked secrets.", "", none), (false, true));
        assert_eq!(gate(false, "Check the dependencies in requirements.txt for known vulnerabilities.", "", none), (false, true));
        assert_eq!(gate(false, "decode this JWT for me", "", none), (false, true));
        assert_eq!(gate(false, "use sec_deps on this project", "", none), (false, true));
        // a follow-up keeps the last choice; off offers both
        assert_eq!(gate(false, "continue", "", (true, true)), (true, true));
        assert_eq!(gate(true, "fix the test", "", none), (true, true));
    }

    #[test]
    fn an_everyday_noun_is_a_document_only_when_it_points_at_one() {
        // asking for new text that is called a sheet / document
        for m in ["Show me a markdown cheat sheet.", "write a document about the history of tea", "make me a git cheat sheet",
            "document how this function works", "give me another cheat sheet, for vim"] {
            assert!(!about_docs(m), "{m:?}");
        }
        for m in ["what does the sheet say?", "summarize this document", "which row of my sales sheet is largest?",
            "compare these documents", "what's in the attached spreadsheet?", "read report.pdf"] {
            assert!(about_docs(m), "{m:?}");
        }
    }

    #[test]
    fn the_gate_offers_tools_only_for_multi_step_tasks() {
        let cases: &[(&str, Option<bool>)] = &[
            ("what is the capital of France", Some(false)),
            ("2+2=?", Some(false)),
            ("hi", Some(false)),
            ("thanks", Some(false)),
            ("thanks!", Some(false)),
            ("what's 15% of 80", Some(false)),
            ("how do I add a git remote?", Some(false)),
            ("explain what a mutex is", Some(false)),
            ("write a haiku about autumn", Some(false)),
            ("who wrote Dune?", Some(false)),
            ("fix the failing test in src/web.rs", Some(true)),
            ("add a dark mode toggle and update the README", Some(true)),
            ("refactor parse_args into its own module and add tests", Some(true)),
            ("please implement retries in the http client", Some(true)),
            ("can you debug why cargo test hangs on mcp_host.rs?", Some(true)),
            ("compare the two caching approaches and pick one", Some(true)),
            ("Here is the error:\n```\nthread main panicked at src/main.rs:3\n```\nwhat's wrong?", Some(true)),
            ("I need a small CLI tool. It should read a CSV file, group the rows by the second column, and print totals. \
              Add a --json flag that prints JSON instead. Write unit tests for the grouping and update the README with usage. \
              Keep it dependency-free and make sure clippy passes.", Some(true)),
            ("continue", None),
            ("ok", None),
            ("go on.", None),
        ];
        let bad: Vec<_> = cases.iter().filter(|(m, want)| wanted(m) != *want).map(|(m, w)| format!("{m:?}: want {w:?}, got {:?}", wanted(m))).collect();
        assert!(bad.is_empty(), "{bad:#?}");
        // a follow-up keeps the previous turn's choice; modes force it
        assert!(decide(Mode::Auto, "continue", true));
        assert!(!decide(Mode::Auto, "continue", false));
        assert!(!decide(Mode::Auto, "2+2=?", true));
        assert!(decide(Mode::Always, "hi", false));
        assert!(!decide(Mode::Off, "fix the failing test in src/web.rs", true));
        assert_eq!(Mode::parse("auto"), Some(Mode::Auto));
        assert_eq!(Mode::parse("sometimes"), None);
    }

    #[test]
    fn steps_show_progress_and_the_next_step() {
        let mut st = State::default();
        let (t, e) = call("step_done", &json!({"n": 1}), &mut st);
        assert!(e && t.contains("no steps"), "{t}");
        let (t, e) = call("steps", &json!({"list": "read the code; 2. fix the average;  ; run the tests; update docs"}), &mut st);
        assert!(!e);
        assert_eq!(t, "4 steps. Next: 1) read the code. Call step_done when it is done.");
        assert_eq!(call("step_done", &json!({"n": 1}), &mut st).0, "Done 1/4. Next: 2) fix the average");
        assert_eq!(call("step_done", &json!({"n": "2"}), &mut st).0, "Done 2/4. Next: 3) run the tests");
        assert_eq!(call("step_done", &json!({"n": "step 3"}), &mut st).0, "Done 3/4. Next: 4) update docs");
        let (t, e) = call("step_done", &json!({"n": 9}), &mut st);
        assert!(e && t.contains("1 to 4"), "{t}");
        assert!(call("step_done", &json!({"n": 0}), &mut st).1);
        assert!(call("step_done", &json!({"n": "x"}), &mut st).1);
        assert_eq!(call("step_done", &json!({"n": 4.0}), &mut st).0, "Done 4/4. All steps done: check the result and answer.");
        // one list per turn; the next turn may make a new one, and the list lives on across turns
        let (t, e) = call("steps", &json!({"list": "a; b"}), &mut st);
        assert!(!e && t.starts_with(ENOUGH), "{t}");
        assert!(answer(&json!({"jsonrpc": "2.0", "method": TURN}), &mut st).is_none());
        assert_eq!(call("step_done", &json!({"n": 4}), &mut st).0, "Done 4/4. All steps done: check the result and answer.");
        assert!(call("steps", &json!({"list": "a; b"}), &mut st).0.starts_with("2 steps."));
        assert!(call("steps", &json!({"list": ""}), &mut State::default()).1);
        // bare numbers are not steps (the 2B model sent "1; 2; 3"); the turn's one list is not used up by it
        let mut st = State::default();
        let (t, e) = call("steps", &json!({"list": "1; 2; 3"}), &mut st);
        assert!(e && t.contains("e.g."), "{t}");
        assert!(call("steps", &json!({"list": "1. read; 2) fix"}), &mut st).0.starts_with("2 steps. Next: 1) read"));
    }

    #[test]
    fn think_is_capped_per_turn_without_an_error_loop() {
        let mut st = State::default();
        for _ in 0..THINK_MAX {
            let (t, e) = call("think", &json!({"thought": "read main.rs first"}), &mut st);
            assert!(!e && t.contains("act"), "{t}");
        }
        assert_eq!(call("think", &json!({"thought": "more"}), &mut st), (ENOUGH.to_string(), false));
        assert_eq!(call("think", &json!({"thought": " "}), &mut st), (ENOUGH.to_string(), false));
        st.new_turn();
        assert!(call("think", &json!({"thought": "next turn"}), &mut st).0.starts_with("Noted"));
    }

    #[test]
    fn empty_thinks_count_toward_the_cap_and_say_act() {
        let mut st = State::default();
        // missing, empty and blank thoughts: a push to act (not an error to retry), and each one counts
        for a in [json!({}), json!({"thought": ""}), json!({"thought": "  "})] {
            let (t, e) = call("think", &a, &mut st);
            assert!(!e && t.contains("act now"), "{t}");
        }
        assert_eq!(call("think", &json!({}), &mut st), (ENOUGH.to_string(), false));
        assert_eq!(call("think", &json!({"thought": "a real one"}), &mut st), (ENOUGH.to_string(), false));
        // 16 empty calls (the QA run): all past the third are capped
        let mut st = State::default();
        let capped = (0..16).filter(|_| call("think", &json!({}), &mut st).0 == ENOUGH).count();
        assert_eq!(capped, 16 - THINK_MAX);
    }

    #[test]
    fn calc_is_exact_and_never_panics() {
        let ok = |e: &str| calc(e).unwrap_or_else(|x| panic!("{e}: {x}"));
        assert_eq!(ok("2+2"), "4");
        assert_eq!(ok("2+2=?"), "4");
        assert_eq!(ok("7/2"), "3.5");
        assert_eq!(ok("(17*23)/4"), "97.75");
        assert_eq!(ok("0.1+0.2"), "0.3");
        assert_eq!(ok("15% 4"), "3");
        assert_eq!(ok("0.15*80"), "12");
        assert_eq!(ok("2^10"), "1024");
        assert_eq!(ok("sqrt(16) + abs(-2)"), "6");
        assert_eq!(ok("max(3, 9) * pi"), "28.2743338823");
        assert_eq!(ok("9007199254740993 + 0"), "9007199254740993");
        assert_eq!(ok("3 × 4 ÷ 2"), "6");
        assert_eq!(ok("1_000 * 3"), "3000");
        assert_eq!(ok(".5 * 4"), "2");
        assert_eq!(ok("1e3 + 1"), "1001");
        assert_eq!(ok("9223372036854775807 * 2"), "1.84467440737e19");
        assert_eq!(ok("1/3000000000"), "3.33333333333e-10");
        for bad in ["", "1/0", "x + 1", "a = 3", "\"s\"", "2 +", "((1)", "1, 2", "std::process::exit(1)", &"1+".repeat(150)] {
            assert!(calc(bad).is_err(), "{bad:?} gave {:?}", calc(bad));
        }
    }

    #[test]
    fn speaks_mcp_and_the_tools_stay_small() {
        let mut st = State::default();
        let init = answer(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}), &mut st).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "elisa-reason");
        let list = answer(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &mut st).unwrap();
        let listed: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(listed, names());
        // every tool: one required input, and all four together stay small in the prompt (Elisa's estimate)
        for t in tools().as_array().unwrap() { assert_eq!(t["inputSchema"]["required"].as_array().unwrap().len(), 1); }
        let toks = crate::context::estimate_tokens(&tools().to_string());
        assert!(toks <= 250, "{toks} tokens");
        let r = answer(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "calc", "arguments": {"expr": "6*7"}}}), &mut st).unwrap();
        assert_eq!(r["result"]["content"][0]["text"], "42");
        assert_eq!(r["result"]["isError"], false);
    }
}
