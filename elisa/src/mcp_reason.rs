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

// ----- arithmetic in the user's words (Elisa computes it herself) -----

/// the most expressions of one message Elisa computes
const SUMS_MAX: usize = 3;

/// The arithmetic written in a message, each as (the expression as written, its `calc` result): "what is 2 + 3?" →
/// [("2 + 3", "5")]. From the message's structure only: a run of numbers joined by operators and parentheses, with
/// at least two numbers and one operator between them. `-` and `/` count only with a space on both sides or next to
/// another operator or a parenthesis (so a date "2026-11-10", a range "3-5" or "24/7" is no sum); `%` is a percent
/// sign, not a sum. A run touching a letter, a `.`-joined version or a `:` (a time, a path) is left out, and so is
/// a message with code in it.
pub fn sums_in(msg: &str) -> Vec<(String, String)> {
    if msg.contains('`') || msg.lines().count() > 6 { return Vec::new(); }
    let msg = ops_in_words(msg);
    let cs: Vec<char> = msg.chars().collect();
    let part = |c: char| c.is_ascii_digit() || matches!(c, '.' | ',' | '(' | ')' | '+' | '-' | '*' | '/' | '×' | '÷' | '^' | '−' | ' ');
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < cs.len() && out.len() < SUMS_MAX {
        if !part(cs[i]) || cs[i] == ' ' { i += 1; continue; }
        let start = i;
        while i < cs.len() && part(cs[i]) { i += 1; }
        // the run ends at its last char that is not a space
        let end = (start..i).rev().find(|&k| cs[k] != ' ').map_or(start, |k| k + 1);
        let (before, after) = (start.checked_sub(1).map(|j| cs[j]), cs.get(end).copied());
        let run: String = cs[start..end].iter().collect();
        // a run inside a word, a version or a time: "v1.2+3", "x2", "20:39"
        let glued = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '%' | '=' | '<' | '>' | '[' | '@' | '#' | '$' | '€' | '£'));
        if glued(before) || glued(after) { continue; }
        if let Some(s) = sum_of(&run) { out.push(s); }
    }
    out
}

/// How arithmetic is said in words (`data/sum_words.txt`): one section per operator, a percentage, a square, a cube
/// and a square root, the wordings in English and Vietnamese.
const SUM_WORDS: &str = include_str!("../data/sum_words.txt");

/// the `key` wordings of `data/sum_words.txt` as one regex alternation, the longest first, spaces matching any run
fn sum_words(key: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut in_key = false;
    for l in SUM_WORDS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            Some(name) => in_key = name.trim() == key,
            None if in_key => words.extend(l.split('|').map(str::trim).filter(|w| !w.is_empty()).map(|w| regex::escape(w).replace(' ', r"\s+"))),
            None => {}
        }
    }
    words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    words.join("|")
}

/// `msg` with arithmetic said in words written with operators (`data/sum_words.txt`), only where numbers stand on
/// both sides (or before "squared" / "cubed"): "12.5% of 3,480 plus 19 squared" → "(12.5/100*3480) + 19^2",
/// "144 chia 12" → "144 / 12", "25% của 480" → "(25/100*480)". Numbers keep their thousands separators out
/// ("3,480" → "3480"). Anything else stays as it is.
fn ops_in_words(msg: &str) -> String {
    use std::sync::OnceLock;
    const NUM: &str = r"(\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?)";
    static RES: OnceLock<Vec<(regex::Regex, String)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        let (percent, of, squared, cubed, root) = (sum_words("percent"), sum_words("of"), sum_words("squared"), sum_words("cubed"), sum_words("square root of"));
        let mut v: Vec<(regex::Regex, String)> = vec![
            // "12.5% of 3,480", "12.5 percent of 3480", "25% của 480"
            (regex::Regex::new(&format!(r"(?i)\b{NUM}\s*(?:%|{percent})\s+(?:{of})\s+{NUM}")).unwrap(), "(${1}/100*${2})".into()),
            (regex::Regex::new(&format!(r"(?i)\b{NUM}\s+(?:{squared})\b")).unwrap(), "${1}^2".into()),
            (regex::Regex::new(&format!(r"(?i)\b{NUM}\s+(?:{cubed})\b")).unwrap(), "${1}^3".into()),
            (regex::Regex::new(&format!(r"(?i)\b(?:{root})\s+{NUM}")).unwrap(), "(${1}^0.5)".into()),
        ];
        for (key, op) in [("plus", "+"), ("minus", "-"), ("times", "*"), ("divided by", "/"), ("to the power of", "^")] {
            let w = sum_words(key);
            if w.is_empty() { continue; }
            // after a number or a closing parenthesis, before a number or an opening one
            v.push((regex::Regex::new(&format!(r"(?i)([\d)])\s+(?:{w})\s+([\d(])")).unwrap(), format!("${{1}} {op} ${{2}}")));
        }
        v
    });
    let mut s = msg.to_string();
    for (re, to) in res {
        // twice: "1 plus 2 plus 3" overlaps at the middle number
        for _ in 0..2 { s = re.replace_all(&s, to.as_str()).into_owned(); }
    }
    // thousands separators inside the parts made here
    let thousands = regex::Regex::new(r"(\d),(\d{3})\b").unwrap();
    if s != msg { for _ in 0..3 { s = thousands.replace_all(&s, "$1$2").into_owned(); } }
    s
}

/// What a counting ask counts: the units `wc` counts (words, lines, characters), each when the message names it
/// with a counting word ("count", "how many", "number of"). Empty for any other message.
pub fn count_units(msg: &str) -> Vec<&'static str> {
    let low = msg.to_lowercase();
    if !(low.contains("count") || low.contains("how many") || low.contains("number of")) { return Vec::new(); }
    let w: Vec<&str> = low.split(|c: char| !c.is_alphabetic()).collect();
    let has = |u: &str| w.iter().any(|x| *x == u || x.strip_suffix('s') == Some(u));
    [("word", "words"), ("line", "lines"), ("character", "characters")].iter()
        .filter(|(one, _)| has(one) || (*one == "character" && has("char"))).map(|(_, many)| *many).collect()
}

/// Elisa's own count of a file's text for a counting ask (`count_units`), to go with the read's result so no model
/// guesses it (gemma3:1b saved "100" for an 8-word file): "[elisa: notes.txt has 8 words]". None when the message
/// asks no count.
pub fn count_note(msg: &str, name: &str, text: &str) -> Option<String> {
    let units = count_units(msg);
    if units.is_empty() { return None; }
    let n = |u: &str| match u { "words" => text.split_whitespace().count(), "lines" => text.lines().count(), _ => text.chars().count() };
    let list = units.iter().map(|u| { let k = n(u); format!("{k} {}", if k == 1 { u.trim_end_matches('s') } else { u }) }).collect::<Vec<_>>().join(", ");
    Some(format!("[elisa: counted exactly: {name} has {list}; use this number]"))
}

// ----- a number question in words: the model writes the expression, Elisa checks and computes it -----

/// The system prompt of the one extra call that asks only for the expression (`word_problem`).
pub const EXPR_SYSTEM: &str = "You turn a question into one arithmetic expression.";
/// most output tokens that call may use
pub const EXPR_MAX_TOKENS: usize = 64;

/// The numbers of a text as values ("3,480" is 3480; "12.5" is 12.5).
fn numbers_of(text: &str) -> Vec<f64> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?").unwrap());
    re.find_iter(text).filter_map(|m| m.as_str().replace(',', "").parse().ok()).collect()
}

/// Whether a message is a number question Elisa cannot compute from its structure alone: a question with two or
/// more numbers, no arithmetic `sums_in` finds, no counting ask (`count_units`) and no code.
pub fn needs_expression(msg: &str) -> bool {
    // (a date, a time or a version is not two numbers to compute with: "is 2026-11-10 a Tuesday?")
    static DATED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let dated = DATED.get_or_init(|| regex::Regex::new(r"\d[-/.]\d+[-/.]\d|\d:\d\d").unwrap());
    // (a clock time plus a duration, "at 3 pm … takes 2 hours", is Elisa's own clock sum: `situation::clock_sum_part`)
    msg.contains('?') && !msg.contains('`') && !dated.is_match(msg) && numbers_of(msg).len() >= 2 && sums_in(msg).is_empty() && count_units(msg).is_empty()
        && !number_choice(msg) && !crate::situation::is_clock_sum(msg)
}

/// The comparison operators `calc` evaluates (`prepare` lets them through): a message with one of them between two
/// numbers asks a comparison, not a sum.
const COMPARE_OPS: &str = "<>=!";

/// Whether a message asks a choice between numbers, not a sum: two numbers joined by "or", "vs" / "versus" or
/// "than" (each number with up to two words after it: "3 GB or 5 GB", "is 7 more than 4", "10 vs 12"), or by one of
/// calc's comparison operators ("is 7 > 4?"). Such a question never gets a computed arithmetic result ("which is
/// bigger, 3 GB or 5 GB?" was read as "5 - 3" and would have been answered "2").
pub fn number_choice(msg: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(&format!(
        r"(?i)\d(?:[\d.,]*)(?:\s*[\p{{L}}%]+){{0,2}}\s+(?:or|vs\.?|versus|than)\s+(?:[\p{{L}}$€£]+\s+)?[$€£]?\d|\d\s*[{}]+\s*\d",
        regex::escape(COMPARE_OPS))).unwrap());
    re.is_match(msg)
}

/// what the extra call writes for a question that needs no arithmetic
const NO_MATH: &str = "none";

/// The user message of the extra call: the question, and the ask for its expression alone.
pub fn expression_ask(msg: &str) -> String {
    format!("{}\n\nWrite only the arithmetic expression that answers this, with numbers and + - * / ( ) only; no words, no result. \
        If it needs no calculation, write only: {NO_MATH}", msg.trim())
}

/// The expression in a model's answer to `expression_ask`: the first line (code fences and backticks left out, any
/// "= result" cut off) made only of numbers, operators and parentheses, with an operator in it.
pub fn expression_in(reply: &str) -> Option<String> {
    reply.lines().map(|l| l.replace('`', "")).map(|l| ops_in_words(&l)).find_map(|l| {
        let e = l.split('=').next().unwrap_or("").trim().trim_end_matches('.').trim().to_string();
        let ok = !e.is_empty() && e.chars().all(|c| c.is_ascii_digit() || " .,()+-*/^×÷−".contains(c))
            && e.chars().any(|c| "+-*/^×÷−".contains(c)) && e.chars().any(|c| c.is_ascii_digit());
        ok.then_some(e)
    })
}

/// how the confirm part of a number question Elisa could not work out starts (`situation::ReplyFacts` holds a reply
/// to it: no number stated)
pub const UNSURE: &str = "Elisa could not work out the arithmetic of this question";

/// What Elisa makes of a number question (`word_problem`).
#[derive(Clone, Debug, PartialEq)]
pub enum WordProblem {
    /// the expression checked against the message and computed: the situation part to give the model
    Computed(String),
    /// not sure (the expression leaves out or adds a number, or there is none): the part that has the reply ask the
    /// user, never state a number
    Confirm(String),
}

impl WordProblem {
    /// the situation part (`situation::with_turn_facts_plus`)
    pub fn part(self) -> String { match self { WordProblem::Computed(s) | WordProblem::Confirm(s) => s } }
}

/// A number question in words ("3 boxes with 7 apples, give away 5: how many are left?"): one extra model call
/// (`ask`: the session sends `expression_ask(msg)` with `EXPR_SYSTEM`, no tools, no history, and returns the text)
/// gives the expression, computed with `calc`. Checked against the message: it uses every number of the message and
/// no other one → Elisa's result (`Computed`). One left out → the reply asks the user whether that reading is meant
/// (`Confirm`, never stated as the answer); no expression at all → the reply asks what to compute. None when the
/// message is no such question (`needs_expression`), the model says it needs no arithmetic, the expression brings
/// a number the message does not have (no reading of it), or the call failed.
/// A number question with its groups worked out by Elisa from the words' structure: "N <things> with / of /
/// containing / holding M <items>" is N × M items ("3 boxes with 7 apples" → "21 apples", "4 packs of 6 eggs" → "24
/// eggs"), so the extra call only has the rest to read (gemma3:1b read "3 boxes with 7 apples, give away 5" as 3 - 5,
/// and "21 apples, give away 5" as 21 - 5). Returns the message so rewritten and each group's (value, "(3*7)").
fn grouped(msg: &str) -> (String, Vec<(f64, String)>) {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\b(\d+(?:\.\d+)?)\s+\p{L}+\s+(?:with|of|containing|holding)\s+(\d+(?:\.\d+)?)\s+(\p{L}+)").unwrap());
    let mut groups: Vec<(f64, String)> = Vec::new();
    let out = re.replace_all(msg, |c: &regex::Captures| {
        let (a, b) = (&c[1], &c[2]);
        match calc(&format!("{a}*{b}")) {
            Ok(v) => { if let Ok(f) = v.parse::<f64>() { groups.push((f, format!("({a}*{b})"))); } format!("{v} {}", &c[3]) }
            Err(_) => c[0].to_string(),
        }
    }).into_owned();
    (out, groups)
}

/// Elisa's own reading of a word problem (`msg` with its groups worked out, `grouped`): each number after the first
/// joins the total by the word between it and the number before it (`data/math_words.txt`: "give away" subtracts,
/// "buy" adds, "share equally among" divides): "21 apples and give away 5 apples, how many are left?" → "21 - 5".
/// None when a number has no such word before it, or the question has fewer than two numbers.
fn own_reading(msg: &str) -> Option<String> {
    static WORDS: std::sync::OnceLock<Vec<(String, char)>> = std::sync::OnceLock::new();
    let words = WORDS.get_or_init(|| {
        let mut v = Vec::new();
        let mut op = ' ';
        for l in include_str!("../data/math_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l { "[minus]" => op = '-', "[plus]" => op = '+', "[divide]" => op = '/', _ if op != ' ' => v.extend(l.split('|').map(|w| (w.trim().to_lowercase(), op)).filter(|(w, _)| !w.is_empty())), _ => {} }
        }
        v.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count()));
        v
    });
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?").unwrap());
    let low = msg.to_lowercase();
    let nums: Vec<regex::Match> = re.find_iter(&low).collect();
    if nums.len() < 2 { return None; }
    let mut e = nums[0].as_str().replace(',', "");
    for w in nums.windows(2) {
        let between = &low[w[0].end()..w[1].start()];
        // the last such word before the number, as a whole word
        let op = words.iter().filter_map(|(x, op)| {
            between.match_indices(x.as_str()).filter(|(i, _)| {
                let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                edge(between[..*i].chars().next_back()) && edge(between[i + x.len()..].chars().next())
            }).map(|(i, _)| (i + x.len(), *op)).last()
        }).max_by_key(|(end, _)| *end).map(|(_, op)| op)?;
        e = if op == '/' { format!("({e}) / {}", w[1].as_str().replace(',', "")) } else { format!("{e} {op} {}", w[1].as_str().replace(',', "")) };
    }
    Some(e)
}

pub fn word_problem(msg: &str, mut ask: impl FnMut(&str) -> Option<String>) -> Option<WordProblem> {
    if !needs_expression(msg) { return None; }
    let original = numbers_of(msg);
    let (msg, groups) = grouped(msg);
    let msg = msg.as_str();
    // an expression with each group's value written as its product again: "21 - 5" → "(3*7) - 5"
    let ungrouped = |e: &str| -> String {
        let mut out = e.to_string();
        for (v, g) in &groups {
            let n = format!("{v}");
            let re = regex::Regex::new(&format!(r"(^|[^\d.]){}($|[^\d.])", regex::escape(&n))).unwrap();
            out = re.replacen(&out, 1, |c: &regex::Captures| format!("{}{g}{}", &c[1], &c[2])).into_owned();
        }
        out
    };
    // no usable reading from the model (a tiny one writes a result or nothing): Elisa's own reading of the words, when
    // every number has its joining word; else the user is asked
    let own = own_reading(msg).and_then(|e| calc(&e).ok().filter(|r| r != "true" && r != "false").map(|r| (e, r)));
    let ask_what = || match &own {
        Some((e, r)) => WordProblem::Computed(format!("Elisa computed it exactly: {} = {r} · answer with this result", ungrouped(e))),
        None => WordProblem::Confirm(format!("{UNSURE} · give no number; ask the user, in the \
        language of their message, what exactly they want computed")),
    };
    let sep = regex::Regex::new(r"(\d),(\d{3})\b").unwrap();
    let same = |a: &f64, b: &f64| (a - b).abs() < 1e-9;
    let want = numbers_of(msg);
    let mut q = expression_ask(msg);
    // a reading that leaves a number out is asked for once more, naming the numbers it left out ("3 boxes with 7
    // apples, give away 5" read as 3 - 5 by gemma3:1b); still left out: the user is asked, with no number stated
    for round in 0..2 {
        let Some(reply) = ask(&q) else { return own.as_ref().map(|_| ask_what()) };
        // no arithmetic in it after all ("which is bigger, 3 or 5 GB?"): nothing to add
        if reply.trim().trim_matches(['`', '.', '"', '\'']).trim().eq_ignore_ascii_case(NO_MATH) { return None; }
        // a bare number: a result, not a reading of the question (moondream:1.8b answered " 14"); asked once more for
        // the expression, then the user is asked
        let bare = reply.trim().trim_end_matches('.').replace(',', "");
        if !bare.is_empty() && bare.parse::<f64>().is_ok() {
            if round == 0 {
                q = format!("{}

{bare} is a result. Write the expression itself, with the question's numbers.", expression_ask(msg));
                continue;
            }
            return Some(ask_what());
        }
        let Some(e) = expression_in(&reply) else {
            // numbers written some other way (a comparison, a formula): no reading of this question, nothing to add;
            // nothing at all: ask
            return if reply.chars().any(|c| c.is_ascii_digit()) { None } else { Some(ask_what()) };
        };
        let mut plain = e.clone();
        while sep.is_match(&plain) { plain = sep.replace_all(&plain, "$1$2").into_owned(); }
        let result = calc(&plain).ok().filter(|r| r != "true" && r != "false");
        let have = numbers_of(&plain);
        // read against the message as Elisa grouped it, or as written ("(3*7) - 5" is a reading of both)
        let within = |w: &[f64]| have.iter().all(|x| w.iter().any(|y| same(x, y)));
        let want = if within(&want) { &want } else { &original };
        // a number the message does not have: not a reading of it ("which is bigger, 3 or 5 GB?" got "3 + 5 - 2")
        if !within(want) { return None; }
        let left: Vec<String> = want.iter().filter(|y| !have.iter().any(|x| same(x, y))).map(|y| format!("{y}")).collect();
        let shown = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        match result {
            None => return Some(ask_what()),
            Some(r) if left.is_empty() => return Some(WordProblem::Computed(format!("Elisa computed it exactly: {} = {r} · answer with this result", ungrouped(&shown)))),
            Some(_) if round == 0 => {
                q = format!("{}\n\nThe expression {shown} leaves out {}. Every number of the question must be used once, the way the \
                    question uses it (a number of groups and the size of each group are multiplied).", expression_ask(msg), left.join(" and "));
            }
            Some(_) => {}
        }
    }
    Some(ask_what())
}

/// The note for a message with arithmetic in it (`sums_in`): Elisa's own results, for the model to answer with (a
/// small model answered "what is 2 + 3?" with "4"). None when the message has none.
pub fn sums_note(msg: &str) -> Option<String> {
    let s = sums_in(msg);
    if s.is_empty() { return None; }
    let list = s.iter().map(|(e, r)| format!("{e} = {r}")).collect::<Vec<_>>().join("; ");
    Some(format!("Elisa computed it exactly: {list} · answer with this result"))
}

/// One run of `sums_in`: (the expression, its result) when it is a sum.
fn sum_of(run: &str) -> Option<(String, String)> {
    let e = run.trim().trim_end_matches(['.', ',', ' ']).trim().to_string();
    let e = e.trim_start_matches(['.', ',', ' ']).to_string();
    // balanced parentheses (a run cut out of "(see 2 + 3)" keeps its own)
    let e = if e.matches('(').count() < e.matches(')').count() { e.trim_end_matches(')').trim().to_string() } else { e };
    let e = if e.matches('(').count() > e.matches(')').count() { e.trim_start_matches('(').trim().to_string() } else { e };
    let cs: Vec<char> = e.chars().collect();
    let numbers = e.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).filter(|w| w.chars().any(|c| c.is_ascii_digit())).count();
    if numbers < 2 { return None; }
    // a number with a comma not followed by exactly three digits is a list ("1, 2, 3"), not 1,000
    let mut plain = String::new();
    for (k, &c) in cs.iter().enumerate() {
        if c == ',' {
            let three = cs.get(k + 1..k + 4).is_some_and(|d| d.iter().all(char::is_ascii_digit)) && cs.get(k + 4).is_none_or(|d| !d.is_ascii_digit());
            if !(k > 0 && cs[k - 1].is_ascii_digit() && three) { return None; }
            continue;
        }
        plain.push(c);
    }
    let pc: Vec<char> = plain.chars().collect();
    let strong = |c: char| matches!(c, '+' | '*' | '×' | '÷' | '^');
    let spaced = |k: usize| pc.get(k.wrapping_sub(1)).is_some_and(|c| *c == ' ') && pc.get(k + 1).is_some_and(|c| *c == ' ');
    // a binary operator: one with a number or ')' before it (spaces between)
    let mut binary = false;
    for (k, &c) in pc.iter().enumerate() {
        if !(strong(c) || matches!(c, '-' | '−' | '/')) { continue; }
        let prev = pc[..k].iter().rev().find(|c| **c != ' ');
        if !prev.is_some_and(|p| p.is_ascii_digit() || *p == ')') { continue; }
        if strong(c) { binary = true; continue; }
        // "-" or "/" between two plain numbers with no spaces: a date, a range, a fraction like 24/7
        let near = pc[..k].iter().rev().take_while(|c| **c != ' ').any(|c| matches!(c, ')')) || pc.get(k + 1) == Some(&'(');
        if spaced(k) || near || pc.iter().any(|c| strong(*c) || *c == '(') { binary = true; } else { return None; }
    }
    if !binary { return None; }
    // a dot between digits more than once in one number is a version ("1.2.3")
    if plain.split(|c: char| !(c.is_ascii_digit() || c == '.')).any(|n| n.matches('.').count() > 1) { return None; }
    let r = calc(&plain).ok()?;
    if r == "true" || r == "false" { return None; }
    Some((e.split_whitespace().collect::<Vec<_>>().join(" "), r))
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
    // plain-text tables: a file the file tools read, no document (`about_docs`)
    "csv", "tsv",
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

/// Whether `text` (the user's words, or recent context) is about documents. A .csv / .tsv the words name is a plain
/// file ("which region sold the most in sales.csv?": `read` serves it, as the file tools do), not a document: its name
/// is left out, so only a document word or a document file (a PDF, a spreadsheet) makes the turn about documents.
pub fn about_docs(text: &str) -> bool {
    let plain_table = |t: &str| t.trim_matches(|c: char| "`'\"()[]{},;:!?".contains(c)).rsplit_once('.')
        .is_some_and(|(a, e)| !a.is_empty() && matches!(e.to_lowercase().as_str(), "csv" | "tsv"));
    let text = &text.split_whitespace().filter(|t| !plain_table(t)).collect::<Vec<_>>().join(" ");
    let w = words(text);
    if w.iter().any(|x| DOC_WORDS.contains(&x.as_str())) || names_tool(&w, "docs") { return true; }
    if w.iter().enumerate().any(|(i, x)| DOC_NOUNS.contains(&x.as_str()) && points_at_doc(&w, i)) { return true; }
    text.split(|c: char| c.is_whitespace() || "`'\"()[]{},;:!?<>".contains(c)).any(|t| {
        t.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && DOC_EXTS.contains(&e.trim_end_matches('.').to_lowercase().as_str()))
    })
}

/// Whether `text` (the user's words) is about security: its words, or an ask by meaning for one of the `sec` tools
/// (a `[sec_…]` section of `data/own_tools.txt`: "kiểm tra requirements.txt xem có gói nào bị lỗ hổng không").
pub fn about_sec(text: &str) -> bool {
    let w = words(text);
    w.iter().any(|x| SEC_WORDS.contains(&x.as_str())) || names_tool(&w, "sec") || text.to_lowercase().contains("api key")
        || crate::repl::meant_tool(text).is_some_and(|(n, _)| n.starts_with("sec_"))
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
        // (a .csv / .txt of the workspace is a plain file the file tools read, not a document: no documents tools for it)
        assert_eq!(gate(false, "which item in data/inventory.csv costs most?", "", none), (false, false));
        assert_eq!(gate(false, "which item in data/inventory.xlsx costs most?", "", none), (true, false));
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

    /// a .csv / .tsv the words name is a plain file (the file tools read it), no document; the word "csv" still is one
    #[test]
    fn a_named_csv_is_a_file_not_a_document() {
        for m in ["không biết trong sales.csv vùng nào bán nhiều nhất nhỉ", "which region sold the most in sales.csv?", "open data.tsv", "what's in `sales.csv`?"] {
            assert!(!about_docs(m), "{m:?}");
            assert!(names_file(m), "{m:?}");
        }
        for m in ["convert the sheet to csv", "sales.csv and budget.xlsx: which is larger?", "read report.pdf"] { assert!(about_docs(m), "{m:?}"); }
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
        // bare numbers are not steps (a small model may send "1; 2; 3"); the turn's one list is not used up by it
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
        // 16 empty calls: all past the third are capped
        let mut st = State::default();
        let capped = (0..16).filter(|_| call("think", &json!({}), &mut st).0 == ENOUGH).count();
        assert_eq!(capped, 16 - THINK_MAX);
    }

    /// Arithmetic in a message is found by its structure and computed; dates, ranges, times, versions, lists and
    /// code are not sums
    #[test]
    fn sums_said_in_vietnamese_or_as_a_percentage_are_read() {
        let s = |m: &str| sums_in(m).into_iter().map(|(e, r)| format!("{e}={r}")).collect::<Vec<_>>();
        assert_eq!(s("25% của 480 là bao nhiêu?"), ["(25/100*480)=120"]);
        assert_eq!(s("25 phần trăm của 480"), ["(25/100*480)=120"]);
        assert_eq!(s("144 chia 12 bằng mấy?"), ["144 / 12=12"]);
        assert_eq!(s("12 nhân 15 là bao nhiêu"), ["12 * 15=180"]);
        assert_eq!(s("100 trừ 37 bằng bao nhiêu?"), ["100 - 37=63"]);
        assert_eq!(s("7 cộng 8"), ["7 + 8=15"]);
        assert_eq!(s("2 mũ 10 là mấy"), ["2 ^ 10=1024"]);
        assert_eq!(s("căn bậc hai của 81"), ["(81^0.5)=9"]);
        assert_eq!(s("5 bình phương"), ["5^2=25"]);
        assert_eq!(s("what's 12 times 15?"), ["12 * 15=180"]);
        for m in ["hôm nay là thứ mấy?", "tôi có 3 con mèo", "uống 2 lần mỗi ngày", "can 5 people fit?"] { assert!(s(m).is_empty(), "{m:?}: {:?}", s(m)); }
        assert_eq!(crate::reply_check::computed_answer("144 chia 12 bằng mấy?").as_deref(), Some("Kết quả là 12."));
        assert_eq!(crate::reply_check::computed_answer("25% của 480 là bao nhiêu?").as_deref(), Some("Kết quả là 120."));
    }

    #[test]
    fn sums_in_a_message_are_found_and_computed() {
        let s = |m: &str| sums_in(m).into_iter().map(|(e, r)| format!("{e}={r}")).collect::<Vec<_>>();
        assert_eq!(s("Reply in one short sentence: what is 2 + 3?"), ["2 + 3=5"]);
        assert_eq!(s("what's 17*23?"), ["17*23=391"]);
        assert_eq!(s("(5-3)*2 and 10 / 4 please"), ["(5-3)*2=4", "10 / 4=2.5"]);
        assert_eq!(s("how much is 1,250 + 3,000."), ["1,250 + 3,000=4250"]);
        assert_eq!(s("2^10"), ["2^10=1024"]);
        assert_eq!(s("is 7 − 9 negative?"), ["7 − 9=-2"]);
        for m in ["today is 2026-11-10", "it's 20:39 now", "3-5 days", "open 24/7", "rust 1.2.3+4", "call +84 912 345 678",
            "list 1, 2, 3", "15% off", "x + 1", "v2 + 3", "`a + 1`", "hey", "what is 42?", "$5 + $3", "page 3", "item_2+3"] {
            assert!(s(m).is_empty(), "{m:?}: {:?}", s(m));
        }
        // said in words: calc's operators by name, a percentage of a number, squared
        assert_eq!(s("what is 12.5% of 3,480 plus 19 squared?"), ["(12.5/100*3480) + 19^2=796"]);
        assert_eq!(s("what's 7 times 8 minus 6?"), ["7 * 8 - 6=50"]);
        assert_eq!(s("20 percent of 150"), ["(20/100*150)=30"]);
        assert_eq!(s("100 divided by 8"), ["100 / 8=12.5"]);
        for m in ["I have 2 cats plus a dog", "Disney plus 3 shows", "the times 2 table"] {
            assert!(s(m).is_empty(), "{m:?}: {:?}", s(m));
        }
        let n = sums_note("Reply in one short sentence: what is 2 + 3?").unwrap();
        assert!(n.contains("2 + 3 = 5"), "{n}");
        assert_eq!(sums_note("hello"), None);
    }

    /// A number question in words: the extra call's expression is checked against the message's numbers
    #[test]
    fn a_word_problem_is_computed_only_when_its_expression_checks_out() {
        let apples = "if I have 3 boxes with 7 apples and give away 5 apples, how many are left?";
        assert!(needs_expression(apples));
        for m in ["what is 2 + 3?", "is 2026-11-10 a Tuesday?", "what time is 20:30 in Tokyo?", "how many words in notes.txt?", "how old is 1 cat?", "my code is 42"] {
            assert!(!needs_expression(m), "{m:?}");
        }
        let asked = std::cell::RefCell::new(String::new());
        let w = word_problem(apples, |q| { *asked.borrow_mut() = q.to_string(); Some("```\n(3*7) - 5\n```".into()) }).unwrap();
        // the groups worked out by Elisa first: the call reads "21 apples", and its "21 - 5" is shown as (3*7) - 5
        assert!(asked.borrow().starts_with("if I have 21 apples and give away 5") && asked.borrow().contains("only the arithmetic expression"), "{}", asked.borrow());
        assert_eq!(word_problem(apples, |_| Some("21 - 5".into())), Some(WordProblem::Computed("Elisa computed it exactly: (3*7) - 5 = 16 · answer with this result".into())));
        assert_eq!(grouped("I buy 4 packs of 6 eggs and break 3").0, "I buy 24 eggs and break 3");
        assert_eq!(w, WordProblem::Computed("Elisa computed it exactly: (3*7) - 5 = 16 · answer with this result".into()));
        // "= result" written too: the model's result is never used, Elisa's is
        assert!(matches!(word_problem(apples, |_| Some("3 * 7 - 5 = 15".into())), Some(WordProblem::Computed(p)) if p.contains("= 16")));
        // a number left out: asked to confirm, with the reading and its value, not stated
        // a number left out: asked once more, naming it; the second reading counts
        let asks = std::cell::RefCell::new(Vec::<String>::new());
        let mut replies = vec!["3 * 7 - 5", "3 - 5"];
        let w = word_problem(apples, |q| { asks.borrow_mut().push(q.to_string()); replies.pop().map(str::to_string) }).unwrap();
        assert!(matches!(&w, WordProblem::Computed(p) if p.contains("3 * 7 - 5 = 16")), "{w:?}");
        assert!(asks.borrow()[1].contains("leaves out 7"), "{:?}", asks.borrow());
        // left out again: the user is asked, and no wrong reading is offered
        // (here Elisa reads the words herself: "give away" takes 5 from the 21; no wrong reading is offered)
        let c = word_problem(apples, |_| Some("3 - 5".into())).unwrap();
        assert!(matches!(&c, WordProblem::Computed(p) if p.contains("(3*7) - 5 = 16") && !p.contains("-2")), "{c:?}");
        // a bare result instead of an expression: asked once more; still a result, or no expression: Elisa's own
        // reading ("think it through" changes nothing), or with no word joining the numbers, ask what to compute
        let mut replies = vec!["14", "14"];
        assert!(matches!(word_problem(apples, |_| replies.pop().map(str::to_string)), Some(WordProblem::Computed(p)) if p.contains("= 16")));
        assert!(matches!(word_problem("3 boxes with 7 apples and give away 5 apples, how many are left? think it through", |_| Some(" 2".into())), Some(WordProblem::Computed(p)) if p.contains("(3*7) - 5 = 16")));
        let unclear = "if I have 3 boxes with 7 apples and 5 pears, how many?";
        let mut replies = vec!["14", "14"];
        assert!(matches!(word_problem(unclear, |_| replies.pop().map(str::to_string)), Some(WordProblem::Confirm(_))));
        assert_eq!(own_reading("I have 20 dollars, buy 3 more and spend 4, how many are left?").as_deref(), Some("20 + 3 - 4"));
        assert_eq!(own_reading("12 cookies shared equally among 4 friends, how many each?").as_deref(), Some("(12) / 4"));
        assert_eq!(own_reading("Có 21 quả táo, cho đi 5 quả, còn lại bao nhiêu?").as_deref(), Some("21 - 5"));
        let mut replies = vec!["3 * 7 - 5", "14"];
        assert!(matches!(word_problem(apples, |_| replies.pop().map(str::to_string)), Some(WordProblem::Computed(p)) if p.contains("= 16")));
        assert!(matches!(word_problem(unclear, |_| Some("I am not sure.".into())), Some(WordProblem::Confirm(p)) if p.contains("give no number")));
        // a number the message lacks, no arithmetic, a comparison, or a failed call: nothing
        let bigger = "which is bigger, 3 GB or 5 GB?";
        // a choice between numbers is no sum: no extra call, no computed number
        for m in [bigger, "is 7 more than 4?", "which is cheaper, 10 vs 12 dollars?", "is 7 > 4?", "is 3.5 kg heavier than 2 kg?", "1 or 2?"] {
            assert!(number_choice(m) && !needs_expression(m), "{m:?}");
            assert_eq!(word_problem(m, |_| panic!("no call for {m:?}")), None);
        }
        assert!(!number_choice("3 boxes with 7 apples, give away 5: how many left?"));
        assert!(!number_choice("I buy 4 tickets for 12 dollars each, or so, what do I pay?"));
        assert_eq!(word_problem(bigger, |_| Some("3 + 5 - 2".into())), None);
        assert_eq!(word_problem(bigger, |_| Some("none".into())), None);
        assert_eq!(word_problem(bigger, |_| Some("3*1024^3 < 5*1024^3".into())), None);
        // (a failed call: Elisa's own reading when the words give one, else nothing)
        assert!(matches!(word_problem(apples, |_| None), Some(WordProblem::Computed(p)) if p.contains("= 16")));
        assert_eq!(word_problem(unclear, |_| None), None);
        // not a number question: no call at all
        assert_eq!(word_problem("hey", |_| panic!("no call")), None);
        // thousands separators
        assert!(matches!(word_problem("I buy 3 shirts at 250,000 dong and pay 1,000,000; what change do I get?", |_| Some("1,000,000 - 3 * 250,000".into())),
            Some(WordProblem::Computed(p)) if p.contains("= 250000")));
    }

    #[test]
    fn a_counting_ask_is_counted_by_elisa() {
        let ask = "step by step: read notes.txt, then count the words in it, then save the count to count.txt";
        assert_eq!(count_units(ask), ["words"]);
        assert_eq!(count_note(ask, "notes.txt", "meeting code ORCHID-42\nbring the slides please\n").as_deref(),
            Some("[elisa: counted exactly: notes.txt has 7 words; use this number]"));
        assert_eq!(count_units("how many lines and characters does it have?"), ["lines", "characters"]);
        assert!(count_units("read notes.txt").is_empty());
        assert!(count_units("count to ten").is_empty());
        assert_eq!(count_note("summarise notes.txt", "notes.txt", "a b"), None);
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
