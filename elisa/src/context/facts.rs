use super::*;

/// The clauses of the user's own words (a statement sentence's parts split at `,` `;` and before "and my / and I"),
/// questions left out.
fn user_clauses(text: &str) -> Vec<String> {
    static AND: OnceLock<regex::Regex> = OnceLock::new();
    let and = AND.get_or_init(|| regex::Regex::new(r"(?i)\s+(?:and|but|also)\s+((?:my|i|i'm|i am)\b)").unwrap());
    let body = request_body(text).trim();
    if body.starts_with('/') || body.starts_with(NUDGE_PREFIX.trim_end()) { return Vec::new(); }
    let Some(own) = own_words(body) else { return Vec::new() };
    let own = without_greeting(own).replace(['’', '‘'], "'");
    // sentences end at . ! ? before a space / the end, or at a line break (not inside "v1.2" or "example.com")
    let mut sentences: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let cs: Vec<char> = own.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        let end = c == '\n' || (matches!(c, '.' | '!' | '?') && cs.get(i + 1).is_none_or(|n| n.is_whitespace()));
        if !end { cur.push(c); continue; }
        sentences.push((std::mem::take(&mut cur), c == '?'));
    }
    sentences.push((cur, false));
    sentences.into_iter().filter(|(s, q)| !q && !s.trim().is_empty())
        .flat_map(|(s, _)| with_subject(and.replace_all(&s, ", $1").split([',', ';']).map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()))
        .collect()
}

/// A sentence's clauses with the subject of an "I'm …" clause carried into the next ones that have none ("I'm Linh,
/// from Hanoi" → "I'm Linh", "I'm from Hanoi"; "from Hanoi" alone is no fact, so a reply's "I'm Elisa
/// from Hanoi" would not be checked for it). Only after an "I'm" clause that states a fact ("I'm tired, from work" carries
/// nothing), and only into a clause that starts lower-case or with a number (no subject of its own).
fn with_subject(clauses: Vec<String>) -> Vec<String> {
    static IM: OnceLock<regex::Regex> = OnceLock::new();
    let im = IM.get_or_init(|| regex::Regex::new(r"(?i)^(?:i'm|i\s+am)\s").unwrap());
    let mut subject = false;
    clauses.into_iter().map(|c| {
        if im.is_match(&c) { subject = user_fact_kv(&c).is_some(); return c; }
        let bare = c.chars().next().is_some_and(|x| x.is_lowercase() || x.is_ascii_digit()) && !c.starts_with("i ") && !c.starts_with("my ") && !c.starts_with("i'");
        if subject && bare { format!("I'm {c}") } else { subject = false; c }
    }).collect()
}

/// A fact the user stated about themself in one clause, said in the third person ("my name is Linh" → "The user's
/// name is Linh"). Notes are written by Elisa, so "My name is Linh, …" kept there as typed reads as Elisa's own
/// words, and "What's my name?" can get Elisa's name: the user's facts must never read as Elisa's.
fn user_fact(clause: &str) -> Option<String> { user_fact_kv(clause).map(|(f, _)| f) }

/// `user_fact` and the fact's value ("Linh", "Hanoi", "ORCHID-42").
fn user_fact_kv(clause: &str) -> Option<(String, String)> {
    static RES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| [
        (r"^my name(?:'s|\s+is)\s+(?P<v>.+)$", "The user's name is $v"),
        (r"^(?:i'm|i\s+am)\s+called\s+(?P<v>.+)$", "The user's name is $v"),
        // "I'm Linh": one capitalised word (checked below), never "I'm tired"
        (r"^(?:i'm|i\s+am)\s+(?P<n>[\p{L}][\p{L}'-]*)$", "The user's name is $n"),
        (r"^(?:please\s+)?call\s+me\s+(?P<v>.+)$", "The user wants to be called $v"),
        (r"^i\s+(?:live|stay)\s+in\s+(?P<v>.+)$", "The user lives in $v"),
        (r"^(?:i'm|i\s+am)\s+from\s+(?P<v>.+)$", "The user is from $v"),
        (r"^i\s+work\s+(?P<p>as|at|for|in)\s+(?P<v>.+)$", "The user works $p $v"),
        (r"^(?:i'm|i\s+am)\s+(?P<v>\d{1,3})\s+years?\s+old$", "The user is $v years old"),
        // any other "my X is Y" (X up to 4 words): "my favourite colour is green", "my meeting code's ORCHID-42"
        (r"^my\s+(?P<k>[a-z][a-z'-]*(?:\s+[a-z][a-z'-]*){0,3}?)(?:'s|\s+(?P<b>is|are))\s+(?P<v>.+)$", ""),
    ].into_iter().map(|(r, t)| (regex::Regex::new(&format!("(?i){r}")).unwrap(), t)).collect());
    let c = clause.trim().trim_end_matches(['.', '!', ';']).trim();
    for (re, tmpl) in res {
        let Some(m) = re.captures(c) else { continue };
        if let Some(n) = m.name("n") {
            if !n.as_str().starts_with(char::is_uppercase) { continue; }
            return Some((format!("The user's name is {}", n.as_str()), n.as_str().to_string()));
        }
        let v = m.name("v").map_or("", |v| v.as_str()).trim().trim_matches(['"', '\'', '`']).trim();
        let words = v.split_whitespace().count();
        if words == 0 || words > 6 || v.contains('?') { return None; }
        let out = match m.name("k") {
            Some(k) => format!("The user's {} {} {v}", k.as_str(), m.name("b").map_or("is", |b| b.as_str())),
            // the value without its quotes: expanded from the trimmed text
            None => { let mut o = String::new(); m.expand(tmpl, &mut o); o.replacen(m.name("v").map_or("", |x| x.as_str()), v, 1) }
        };
        return Some((out.split_whitespace().collect::<Vec<_>>().join(" "), v.to_string()));
    }
    None
}

/// The values of the facts the user stated about themself in `text` ("Linh", "Hanoi", "ORCHID-42"; `user_facts`).
pub fn user_fact_values(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (_, x) in user_clauses(text).iter().filter_map(|c| user_fact_kv(c)) { if !v.contains(&x) { v.push(x); } }
    v
}

/// The user's fact values (`user_fact_values`) a reply says as Elisa's own ("I'm Elisa, and I live in
/// Hanoi.", "I'm from Hanoi too"): a clause of the reply with a first-person word and,
/// after it, a value, and no "you" / "your" (that clause speaks to the user: "I'm glad you're from Hanoi"). Only
/// the session's own fact values are looked for, never fixed words of a reply. Empty when none is claimed.
// a few first- and second-person words, English and Vietnamese, kept short on purpose (a small fixed list, not a
// long one); never grow it per failure case
pub fn claimed_user_facts(reply: &str, values: &[String]) -> Vec<String> {
    static FIRST: OnceLock<regex::Regex> = OnceLock::new();
    static YOU: OnceLock<regex::Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| regex::Regex::new(r"(?i)\b(?:i|me|my|mine|myself|i'm|i've|i'd|i'll|tôi|mình|tớ|em)\b").unwrap());
    let you = YOU.get_or_init(|| regex::Regex::new(r"(?i)\b(?:you|your|yours|you're|you've|you'll|you'd|yourself|bạn|cậu|anh|chị)\b").unwrap());
    let (text, _, _) = split_state(reply);
    let text = text.replace(['’', '‘'], "'");
    let mut out: Vec<String> = Vec::new();
    for clause in text.split(['.', ',', ';', '!', '?', '\n', ':', '—']) {
        if you.is_match(clause) { continue; }
        let Some(at) = first.find(clause).map(|m| m.end()) else { continue };
        let after = clause[at..].to_lowercase();
        for v in values {
            let lv = v.to_lowercase();
            let hit = after.match_indices(&lv).any(|(i, _)| {
                let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                edge(after[..i].chars().next_back()) && edge(after[i + lv.len()..].chars().next())
            });
            if !lv.is_empty() && hit && !out.contains(v) { out.push(v.clone()); }
        }
    }
    out
}

/// The facts the user stated about themself in `text` (their own words: `request_body`, `own_words`), in the third
/// person (`user_fact`), for the notes' `Facts to remember`: a summary may drop them or write them so they read as
/// Elisa's ("Name: Linh", a quoted "My name is Linh").
pub fn user_facts(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for f in user_clauses(text).iter().filter_map(|c| user_fact(c)) { if !v.contains(&f) { v.push(f); } }
    v
}

/// A fact in the third person (`user_fact`'s "The user's name is Linh") as said to the user ("your name is Linh"),
/// None for any other line. Only Elisa's own templates are read, never the user's or a model's wording.
pub fn fact_to_you(fact: &str) -> Option<String> {
    let f = fact.trim();
    let rest = f.strip_prefix("The user")?;
    let you = if let Some(r) = rest.strip_prefix("'s ") { format!("your {r}") }
        else if let Some(r) = rest.strip_prefix(" is ") { format!("you are {r}") }
        else if let Some(r) = rest.strip_prefix(" wants ") { format!("you want {r}") }
        else if let Some(r) = rest.strip_prefix(" lives ") { format!("you live {r}") }
        else { format!("you work {}", rest.strip_prefix(" works ")?) };
    Some(you)
}

/// Whether a request only tells facts about the user ("My name is Linh, I live in Hanoi"): no session goal.
pub fn only_user_facts(text: &str) -> bool {
    let cl = user_clauses(text);
    !cl.is_empty() && cl.iter().all(|c| user_fact(c).is_some())
}

/// `notes` with `facts` (`user_facts`) in `## Facts to remember` (merged, duplicates dropped).
pub fn with_user_facts(notes: &str, facts: &[String]) -> String {
    if facts.is_empty() { return notes.to_string(); }
    let add = format!("## Facts to remember\n{}", facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n"));
    merge_notes(Some(notes), &add)
}

/// The user's words as Elisa shows them in notes or `[state]`: a stated fact in the third person (`user_facts`),
/// first-person words quoted as the user's (`the user said: "fix my build"`), anything else as it is. "I", "me" and
/// "my" outside the user's own messages would read as Elisa's.
pub fn users_words(text: &str) -> String {
    let t = text.trim();
    if only_user_facts(t) { return user_facts(t).join("; "); }
    static FIRST: OnceLock<regex::Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| regex::Regex::new(r"(?i)\b(?:i|me|my|mine|myself|i'm|i've|i'd|i'll)\b").unwrap());
    if first.is_match(&t.replace('’', "'")) && !t.starts_with("the user said:") { format!("the user said: \"{}\"", t.trim_matches('"')) } else { t.to_string() }
}

// ----- notes: Elisa's edit records, claim checks, merge, dedupe, priority cap (no model call) -----

/// Heading of the notes section Elisa writes from its own edit records (ground truth, never the model's claim).
pub const FILES_NOTES: &str = "## Files changed (Elisa's own edit records)";
const LEDGER_TEXT_CAP: usize = 200_000;

/// What Elisa's own tools changed in this session: the files `write` / `edit` / `edit_symbol` changed (with the text
/// those edits wrote and each file's text before its first change) and the shell commands run. The evidence a
/// summary's "added X" must be backed by.
#[derive(Default, Clone, Debug)]
pub struct Ledger {
    files: Vec<LedgerFile>,
    /// shell commands run (lowercased): a `sed -i` or `echo >>` change is evidence too
    shell: String,
}

#[derive(Clone, Debug)]
struct LedgerFile { path: String, writes: usize, edits: usize, wrote: String, before: String }

/// `path` relative to `cwd` when it lies inside it (the model often writes absolute paths)
fn rel_path(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    p.strip_prefix(cwd).map(|r| r.display().to_string()).unwrap_or_else(|_| path.trim_start_matches("./").to_string())
}

impl Ledger {
    /// Before a write tool runs: the file's text as it was (kept for its first change only).
    pub fn before(&mut self, input: &Value, cwd: &Path) {
        let Some(path) = input.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) else { return };
        let rel = rel_path(path, cwd);
        if self.files.iter().any(|f| f.path == rel) { return; }
        let text: String = fs::read_to_string(cwd.join(path)).unwrap_or_default().chars().take(LEDGER_TEXT_CAP).collect();
        self.files.push(LedgerFile { path: rel, writes: 0, edits: 0, wrote: String::new(), before: text.to_lowercase() });
    }

    /// A tool call that succeeded.
    pub fn record(&mut self, tool: &str, input: &Value, cwd: &Path) {
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match tool {
            "write" | "edit" | "edit_symbol" => {
                let path = s("path");
                if path.trim().is_empty() { return; }
                let rel = rel_path(&path, cwd);
                let i = match self.files.iter().position(|f| f.path == rel) {
                    Some(i) => i,
                    None => { self.files.push(LedgerFile { path: rel, writes: 0, edits: 0, wrote: String::new(), before: String::new() }); self.files.len() - 1 }
                };
                let f = &mut self.files[i];
                if tool == "write" { f.writes += 1 } else { f.edits += 1 }
                let new = match tool { "write" => s("content"), "edit" => s("new"), _ => format!("{} {}", s("name"), s("source")) };
                if f.wrote.len() + new.len() <= LEDGER_TEXT_CAP { f.wrote.push_str(&new.to_lowercase()); f.wrote.push('\n'); }
            }
            "bash" => {
                let c = s("command").to_lowercase();
                if self.shell.len() + c.len() <= LEDGER_TEXT_CAP { self.shell.push_str(&c); self.shell.push('\n'); }
            }
            _ => {}
        }
    }

    /// Whether any file changed (a `before` with no change after it does not count).
    pub fn is_empty(&self) -> bool { self.files.iter().all(|f| f.writes + f.edits == 0) }

    /// The paths that changed, in first-change order.
    pub fn paths(&self) -> Vec<String> { self.files.iter().filter(|f| f.writes + f.edits > 0).map(|f| f.path.clone()).collect() }

    /// The notes section: one line per changed file. None when nothing changed.
    pub fn section(&self) -> Option<String> {
        if self.is_empty() { return None; }
        let mut s = format!("{FILES_NOTES}\n");
        for f in self.files.iter().filter(|f| f.writes + f.edits > 0) {
            let what = match (f.writes, f.edits) {
                (0, e) => format!("edited {e}×"),
                (w, 0) => format!("{}{}", if f.before.is_empty() { "written (new file)" } else { "rewritten" }, if w > 1 { format!(" {w}×") } else { String::new() }),
                (w, e) => format!("written {w}×, edited {e}×"),
            };
            s += &format!("- {}: {what}\n", f.path);
        }
        Some(s.trim_end().to_string())
    }

    /// Whether a sentence saying a file was saved is about a file Elisa's tools really wrote this session: the file it
    /// names (`named`, as `route::file_name` found it) changed, or, naming none, it says a changed file's name or
    /// stem ("I saved the poem to the file" after `write poem.md`). A recall turn's true "I saved the poem to
    /// poem.md" is then no false claim.
    pub fn backs(&self, sentence: &str, named: Option<&str>) -> bool {
        if let Some(n) = named { return !self.matching(n).is_empty(); }
        let s = sentence.to_lowercase();
        let words = claim_words(&s);
        self.files.iter().filter(|f| f.writes + f.edits > 0).any(|f| {
            let name = f.path.rsplit(['/', '\\']).next().unwrap_or(&f.path).to_lowercase();
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(st, _)| st);
            s.contains(&name) || (stem.chars().count() >= 3 && words.iter().any(|w| w == stem))
        })
    }

    /// The changed files `path` (as a summary wrote it) may mean: the same path, or one ends with the other.
    fn matching(&self, path: &str) -> Vec<&LedgerFile> {
        let p = path.trim_start_matches("./").to_lowercase();
        if p.is_empty() { return Vec::new(); }
        self.files.iter().filter(|f| f.writes + f.edits > 0).filter(|f| {
            let q = f.path.to_lowercase();
            q == p || q.ends_with(&format!("/{p}")) || p.ends_with(&format!("/{q}"))
        }).collect()
    }
}

/// verbs that say something new was made: its name must be in what an edit wrote
const CLAIM_NEW: &[&str] = &[
    "added", "wrote", "written", "created", "implemented", "documented", "appended", "inserted", "introduced", "bumped",
    "add", "write", "create", "implement", "document", "append", "insert", "introduce", "bump",
];
/// past-tense change verbs: a line with one claims something was done to the code
const CLAIM_PAST: &[&str] = &[
    "added", "changed", "edited", "wrote", "written", "created", "fixed", "bumped", "updated", "implemented", "renamed",
    "removed", "deleted", "modified", "refactored", "replaced", "documented", "appended", "inserted", "introduced",
    "rewrote", "rewritten", "patched", "corrected",
];
/// the same verbs as a state card's `done:` items write them ("done: add restock_cost")
const CLAIM_BASE: &[&str] = &[
    "add", "change", "edit", "write", "create", "fix", "bump", "update", "implement", "rename", "remove", "delete",
    "modify", "refactor", "replace", "document", "append", "insert", "introduce", "rewrite", "patch", "correct",
];

fn claim_words(line: &str) -> Vec<String> {
    line.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).map(String::from).collect()
}

fn is_claim(line: &str, done: bool) -> bool {
    let l = line.to_lowercase();
    if l.contains("(unverified") || l.contains(" not ") || l.contains("never") || l.contains("failed") || l.contains("n't ") { return false; }
    claim_words(line).iter().any(|x| CLAIM_PAST.contains(&x.as_str()) || (done && CLAIM_BASE.contains(&x.as_str())))
}

/// The checkable words of a claim: (paths, identifiers). A path has a `/` or a file extension; an identifier has a
/// `_`, starts with `--`, ends with `()`, is a version (`0.3.2`) or a comparison (`<=`); backticked spans count too.
fn claim_tokens(line: &str) -> (Vec<String>, Vec<String>) {
    let (mut paths, mut ids) = (Vec::new(), Vec::new());
    let ver = |t: &str| { let t = t.trim_start_matches('v'); t.contains('.') && t.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) };
    let ext = |t: &str| t.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && !a.chars().all(|c| c.is_ascii_digit())
        && (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()) && e.chars().any(|c| c.is_ascii_alphabetic()));
    let mut words: Vec<String> = line.split('`').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s.trim().to_string()).collect();
    words.extend(line.split_whitespace().map(String::from));
    for w in words {
        let t = w.trim_matches(|c: char| "\"'`,;:()[]{}!?*".contains(c)).trim_end_matches('.').to_string();
        if t.is_empty() || paths.contains(&t) || ids.contains(&t) { continue; }
        if ["<=", ">=", "==", "!=", "<", ">"].contains(&t.as_str()) { ids.push(t); continue; }
        if t.starts_with("http") { continue; }
        if ver(&t) { ids.push(t); continue; }
        if (t.contains('/') && t.chars().filter(|c| c.is_alphanumeric()).count() >= 2) || ext(&t) { paths.push(t); continue; }
        let id = t.trim_end_matches("()");
        if (id.contains('_') && id.chars().any(char::is_alphanumeric)) || (id.starts_with("--") && id.len() > 3) || t.ends_with("()") {
            ids.push(id.to_string());
        }
    }
    (paths, ids)
}

/// One claim against the edit records: Some(true) backed, Some(false) contradicted, None nothing to check it by.
/// A claim that something new was made ("added restock_cost") needs its name in what an edit wrote (or a shell
/// command); a change to existing code ("fixed low_stock (< to <=)") may name code the changed file held before or
/// holds now. A named file narrows the evidence to that file's edits. With only a path, the path must have changed.
fn check_claim(line: &str, ledger: &Ledger, now: &dyn Fn(&str) -> Option<String>) -> Option<bool> {
    let (paths, ids) = claim_tokens(line);
    let named: Vec<&LedgerFile> = paths.iter().flat_map(|p| ledger.matching(p)).collect();
    if ids.is_empty() {
        return if paths.is_empty() { None } else { Some(!named.is_empty()) };
    }
    let new = claim_words(line).iter().any(|w| CLAIM_NEW.contains(&w.as_str()));
    let files: Vec<&LedgerFile> = if named.is_empty() { ledger.files.iter().collect() } else { named };
    let mut corpus = ledger.shell.clone();
    for f in files {
        corpus.push_str(&f.wrote);
        if !new {
            corpus.push_str(&f.before);
            if let Some(t) = now(&f.path) { corpus.push_str(&t.chars().take(LEDGER_TEXT_CAP).collect::<String>().to_lowercase()); }
        }
    }
    Some(ids.iter().any(|i| corpus.contains(&i.to_lowercase())))
}

/// The marker on a "done" line nothing could confirm.
pub const UNVERIFIED: &str = "(unverified: not in Elisa's edit records)";

/// New notes (a summary, or state cards) with their "done / changed" claims checked against Elisa's own edit records
/// (otherwise notes keep "done" items a summary invented, though no edit did any
/// of it; `check_claim`). A contradicted claim is dropped; one with nothing to check while nothing changed this
/// session is marked UNVERIFIED. The model's `## Files touched` lines keep only files the records have (others are
/// dropped, or marked when there are no records), and a "no files were touched" line goes. `now`: a changed file's text now (path relative to the project).
pub fn verify_claims(notes: &str, ledger: &Ledger, now: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut heading = String::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') { heading = norm_words(t); out.push(l.to_string()); continue; }
        if t.is_empty() { out.push(String::new()); continue; }
        if heading.starts_with("files touched") {
            let n = norm_words(t);
            // an attached document (`ground_notes`): read, not changed, and not a claim
            if n.starts_with("attached ") { out.push(l.to_string()); continue; }
            if n.starts_with("no files") || n.starts_with("none") || n.starts_with("nothing") || n.contains("no files were") { continue; }
            let (paths, _) = claim_tokens(t);
            // Elisa's own section lists what changed: a line it does not back is noise next to it (dropped); with no
            // records at all (bash may have changed something) it is marked instead
            if paths.iter().any(|p| !ledger.matching(p).is_empty()) {
                // the file did change; what the line says about it is a claim like any other
                if !(is_claim(t, true) && check_claim(t, ledger, now) == Some(false)) { out.push(l.to_string()); }
            } else if ledger.is_empty() { out.push(format!("{} {UNVERIFIED}", l.trim_end())); }
            continue;
        }
        // a card's `done: a; b` line: every item is a claim
        if let Some(rest) = t.strip_prefix("done:") {
            let kept: Vec<String> = rest.split(';').map(str::trim).filter(|x| !x.is_empty()).filter_map(|x| match check_claim(x, ledger, now) {
                Some(false) if is_claim(x, true) => None,
                None if is_claim(x, true) && ledger.is_empty() => Some(format!("{x} {UNVERIFIED}")),
                _ => Some(x.to_string()),
            }).collect();
            if !kept.is_empty() { out.push(format!("done: {}", kept.join("; "))); }
            continue;
        }
        // sentence by sentence: a paragraph that mixes a real change with an invented one keeps only the real one
        // (e.g. "…created CHANGELOG.md. The model has updated README.md…" when README.md never changed)
        let done = heading.starts_with("done");
        let mut kept: Vec<String> = Vec::new();
        for sent in sentences(l.trim_end()) {
            if !is_claim(&sent, done) { kept.push(sent); continue; }
            match check_claim(&sent, ledger, now) {
                Some(false) => {}
                None if ledger.is_empty() => kept.push(format!("{sent} {UNVERIFIED}")),
                _ => kept.push(sent),
            }
        }
        let line = kept.join(" ");
        if !line.trim().trim_start_matches(['-', '*']).trim().is_empty() { out.push(line); }
    }
    clean_notes(&out.join("\n"))
}

/// Words of a claim that the assistant gave code ("provided code examples", "wrote a script").
const CODE_CLAIM: &[&str] = &["code example", "code examples", "example code", "sample code", "code sample", "code samples", "code snippet",
    "code snippets", "python script", "a script", "scripts", "python code", "the code"];
const GAVE: &[&str] = &["provided", "generated", "wrote", "written", "shared", "gave", "given", "included", "includes", "including", "created", "showed", "shown", "offered"];

/// New notes checked against what the conversation actually holds (a small model's summary can claim code examples
/// no reply holds, list a PDF the user attached as a file it touched, or put raw tool lines
/// under Files touched). `said`: every assistant text of the
/// conversation; `docs`: the attached documents (file names); `ledger`: Elisa's edit records.
/// - a sentence claiming code was given goes when no reply holds code (a ``` fence or an indented block);
/// - a `## Done` sentence claiming a file was made or changed goes when no reply names that file and no edit
///   touched it;
/// - a `## Files touched` line naming only files no reply names and no edit touched goes;
/// - a `## Files touched` line naming an attached document (by name, or an `.elisa/uploads/…` path) becomes
///   `- attached: <name>`; an upload path anywhere else becomes `attached: <name>`;
/// - raw tool lines go (`clean_notes`).
pub fn ground_notes(notes: &str, said: &str, docs: &[String], ledger: &Ledger) -> String {
    let has_code = said.contains("```") || said.lines().filter(|l| l.starts_with("    ") && !l.trim().is_empty()).count() >= 3;
    let said_l = said.to_lowercase();
    let base = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string();
    let upload_name = |tok: &str| -> Option<String> {
        let t = tok.trim_matches(|c: char| "\"'`,;:()[]{}!?*".contains(c)).trim_end_matches('.');
        if t.contains(".elisa/uploads/") { return Some(base(t)); }
        let b = base(t);
        docs.iter().find(|d| !d.is_empty() && **d == b && (t.contains('/') || t == d.as_str())).cloned()
    };
    let mut out: Vec<String> = Vec::new();
    let mut heading = String::new();
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') { heading = norm_words(t); out.push(l.to_string()); continue; }
        if t.is_empty() { out.push(String::new()); continue; }
        // Elisa's own reference lines of attached documents stay exactly as they are
        if heading.starts_with("attached documents") || t.contains(crate::attach::MARK) { out.push(l.to_string()); continue; }
        if heading.starts_with("files touched") {
            if let Some(name) = t.split_whitespace().find_map(upload_name) { out.push(format!("- attached: {name}")); continue; }
            // a file no reply names and no edit touched (a 2B fold listed "TradingAgents_code_example.py (likely
            // modified or created)" in a chat with no files)
            let (paths, _) = claim_tokens(t);
            if !paths.is_empty() && paths.iter().all(|p| !said_l.contains(&base(p).to_lowercase()) && ledger.matching(p).is_empty()) { continue; }
            out.push(l.to_string());
            continue;
        }
        // upload paths read as what they are
        let line: String = l.split(' ').map(|w| match w.contains(".elisa/uploads/").then(|| upload_name(w)).flatten() {
            Some(n) => format!("attached: {n}"),
            None => w.to_string(),
        }).collect::<Vec<_>>().join(" ");
        let done = heading.starts_with("done");
        let mut kept: Vec<String> = Vec::new();
        for sent in sentences(line.trim_end()) {
            let low = sent.to_lowercase();
            let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            let gave = words.iter().any(|w| GAVE.contains(w));
            if !has_code && gave && CODE_CLAIM.iter().any(|c| low.contains(c)) { continue; }
            if done && is_claim(&sent, true) {
                let (paths, _) = claim_tokens(&sent);
                let real: Vec<&String> = paths.iter().filter(|p| upload_name(p).is_none() && !p.contains("attached:")).collect();
                if !real.is_empty() && real.iter().all(|p| !said_l.contains(&base(p).to_lowercase()) && ledger.matching(p).is_empty()) { continue; }
            }
            kept.push(sent);
        }
        let line = kept.join(" ");
        if !line.trim().trim_start_matches(['-', '*']).trim().is_empty() { out.push(line); }
    }
    clean_notes(&out.join("\n"))
}

/// Words too common in notes to say what a fact is about ("The user asked Elisa to …").
const FACT_COMMON: &[&str] = &["user", "elisa", "asked", "asking", "ask", "using", "use", "used", "provide", "provided", "providing"];

/// Whether a notes fact (or session goal) is something `talk` (the user's and the assistant's words of the
/// conversation being summarised) is about: at least 2 of its content words, and at least half of them, are in the
/// talk (a fact with one content word: that word). A fact with no content words never is.
pub fn fact_in_talk(fact: &str, talk: &str) -> bool {
    let cw: Vec<String> = content_words(fact).into_iter().filter(|w| !FACT_COMMON.contains(&w.as_str())).collect();
    if cw.is_empty() { return false; }
    let said = content_words(talk);
    let found = cw.iter().filter(|w| said.contains(*w)).count();
    found >= cw.len().min(2) && found * 2 >= cw.len()
}

/// The `Facts to remember`, `Session goal` and `Done` lines of `notes`, as keys (`fact_key`).
pub fn fact_keys(notes: &str) -> Vec<String> {
    sections(notes).into_iter().filter(|(h, _)| { let k = heading_key(h); k.starts_with("facts") || k.starts_with("session goal") || k.starts_with("done") })
        .flat_map(|(_, b)| b.into_iter().map(|l| fact_key(&l)).filter(|k| !k.is_empty()).collect::<Vec<_>>()).collect()
}

/// A notes line's words, for telling the same fact again (bullets and case ignored).
pub fn fact_key(line: &str) -> String { bullet_key(line) }

/// Whether a notes fact says what a pinned fact says (all the pinned fact's words in it, or the fact's words half
/// in the pinned fact).
pub fn fact_pinned(fact: &str, pinned: &[String]) -> bool {
    let f = format!(" {} ", norm_words(fact));
    pinned.iter().any(|p| { let k = norm_words(p); (!k.is_empty() && f.contains(&format!(" {k} "))) || fact_in_talk(fact, p) })
}

/// `facts` without the ones `notes` already say (all of a fact's words, in order, in the notes): with memory on, a
/// note's `Facts` lines are also pinned facts, and the same fact twice in one first message (in the notes and in
/// `[state]`) made the context after /clear bigger than the conversation it replaced.
pub fn facts_not_in(notes: Option<&str>, facts: Vec<String>) -> Vec<String> {
    let Some(n) = notes.map(norm_words).filter(|n| !n.is_empty()) else { return facts };
    let hay = format!(" {n} ");
    facts.into_iter().filter(|f| { let k = norm_words(f); k.is_empty() || !hay.contains(&format!(" {k} ")) }).collect()
}
