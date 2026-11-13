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
        .map(|c| fact_after_that(&c).unwrap_or(c))
        .collect()
}

/// A clause of one or two lead-in words and "that" before a fact the user states about themself ("remember that my
/// dog is Bo"): the fact ("my dog is Bo"). None for any other clause.
fn fact_after_that(clause: &str) -> Option<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)^(?:[\p{L}']+\s+){1,2}that\s+(?P<f>(?:my|i|i'm)\b.*)$").unwrap());
    let f = re.captures(clause.trim())?.name("f")?.as_str().to_string();
    user_fact_kv(&f).is_some().then_some(f)
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
            // a name of someone or something of the user's ("my dog is Bo"): said as that one's name, so it never
            // reads as the user's own ("The user's dog is called Bo"; read as "dog is Bo", models greeted the user as Bo)
            Some(k) if name_like(v) && !k.as_str().to_lowercase().split_whitespace().any(|w| w.trim_end_matches("'s") == "name") =>
                format!("The user's {} {} called {v}", k.as_str(), m.name("b").map_or("is", |b| b.as_str())),
            Some(k) => format!("The user's {} {} {v}", k.as_str(), m.name("b").map_or("is", |b| b.as_str())),
            // the value without its quotes: expanded from the trimmed text
            None => { let mut o = String::new(); m.expand(tmpl, &mut o); o.replacen(m.name("v").map_or("", |x| x.as_str()), v, 1) }
        };
        return Some((out.split_whitespace().collect::<Vec<_>>().join(" "), v.to_string()));
    }
    None
}

/// A value that reads as a name: one to three words, each of letters only and starting upper-case ("Bo", "Mai
/// Anh"), not a code ("ORCHID-42") or a thing ("green").
fn name_like(v: &str) -> bool {
    let w: Vec<&str> = v.split_whitespace().collect();
    (1..=3).contains(&w.len()) && w.iter().all(|x| x.starts_with(char::is_uppercase) && x.chars().all(|c| c.is_alphabetic() || c == '\''))
        && !w.iter().all(|x| x.chars().all(|c| !c.is_lowercase()) && x.chars().count() > 1)
}

/// The value of the fact one clause states in the first person ("my name is Jonathan" → "Jonathan"; `user_fact`'s
/// wordings), None when it states none.
pub fn fact_value(clause: &str) -> Option<String> { user_fact_kv(clause).map(|(_, v)| v) }

/// The values of the facts the user stated about themself in `text` ("Linh", "Hanoi", "ORCHID-42"; `user_facts`).
pub fn user_fact_values(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (_, x) in user_clauses(text).iter().filter_map(|c| user_fact_kv(c)) { if !v.contains(&x) { v.push(x); } }
    v
}

/// The user's fact values (`user_fact_values`) a reply says as Elisa's own ("I'm Elisa, and I live in
/// Hanoi.", "I'm from Hanoi too", "I told you my name is Linh"): a value in a reply clause after a first-person word,
/// with no "you" / "your" between the nearest such word and the value (that part speaks to the user: "I'm glad
/// you're from Hanoi", "I know you live in Hanoi"). Only the session's own fact values are looked for, never fixed
/// words of a reply. Empty when none is claimed.
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
        let low = clause.to_lowercase();
        for v in values {
            let lv = v.to_lowercase();
            if lv.is_empty() || out.contains(v) { continue; }
            let hit = low.match_indices(&lv).any(|(i, _)| {
                let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                if !(edge(low[..i].chars().next_back()) && edge(low[i + lv.len()..].chars().next())) { return false; }
                // the nearest first-person word before the value, and no "you" between it and the value
                let before = &low[..i];
                first.find_iter(before).last().is_some_and(|m| !you.is_match(&before[m.end()..]))
            });
            if hit { out.push(v.clone()); }
        }
    }
    out
}

/// The who-told-whom wordings of `data/told_words.txt`: (always wrong, wrong when asked what the user told, the
/// asks that make such a question).
#[allow(clippy::type_complexity)]
fn told_words() -> &'static (Vec<(regex::Regex, String)>, Vec<(regex::Regex, String)>, Vec<String>) {
    static W: OnceLock<(Vec<(regex::Regex, String)>, Vec<(regex::Regex, String)>, Vec<String>)> = OnceLock::new();
    W.get_or_init(|| {
        let (mut always, mut asked, mut asks) = (Vec::new(), Vec::new(), Vec::new());
        let mut in_asked = false;
        for l in include_str!("../../data/told_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { in_asked = l == "[asked]"; continue; }
            let Some((a, b)) = l.split_once(" = ") else { continue };
            let (a, b) = (a.trim(), b.trim());
            if in_asked && a == "asks" { asks = b.split('|').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect(); continue; }
            let Ok(re) = regex::Regex::new(&format!(r"(?i)(^|[^\p{{L}}]){}($|[^\p{{L}}])", regex::escape(a))) else { continue };
            if in_asked { asked.push((re, b.to_string())) } else { always.push((re, b.to_string())) }
        }
        (always, asked, asks)
    })
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

/// One of Elisa's own third-person fact lines (`user_fact`'s "The user's name is Linh") as (its thing in words,
/// its value): ("name", "Linh"). None for any other line.
fn fact_parts(line: &str) -> Option<(String, String)> {
    let rest = line.trim().trim_start_matches(['-', '*', ' ']).strip_prefix("The user's ")?;
    let (k, v) = rest.split_once(" is ").or_else(|| rest.split_once(" are "))?;
    let v = norm_words(v.trim_start().strip_prefix("called ").unwrap_or(v));
    (!v.is_empty()).then(|| (norm_words(k), v))
}

/// The lines of `notes` in the form of Elisa's own fact lines (`fact_parts`, or another of `user_fact`'s: "The user
/// lives in Hanoi"), without their bullets.
pub fn fact_lines(notes: &str) -> Vec<String> {
    notes.lines().map(|l| l.trim().trim_start_matches(['-', '*', ' ']).trim_end_matches('.'))
        .filter(|l| fact_parts(l).is_some() || fact_to_you(l).is_some()).map(str::to_string).collect()
}

/// `notes` without the lines about one of the user's things (`facts`: Elisa's own lines, `user_facts`; "The user's
/// name is Linh") that hold none of the values Elisa knows for it: a summary's "User's name: Elisa" or "The user's
/// name is Elisa" or "Linh's name: Elisa" next to "The user's name is Linh" (a small model mixes up the two parties;
/// read back, the user's name became Elisa's). The user is named by "user" or by one of the known values. A sentence
/// that gives one of those things to Elisa ("retrieve Elisa's name and meeting code" for the user's question) goes
/// too. Lines of `facts` themselves always stay.
pub fn without_contradicted_facts(notes: &str, facts: &[String]) -> String {
    let known: Vec<(String, String)> = facts.iter().filter_map(|f| fact_parts(f)).collect();
    if known.is_empty() { return notes.to_string(); }
    let own: HashSet<String> = facts.iter().map(|f| bullet_key(f)).collect();
    let subjects: Vec<&str> = std::iter::once("user").chain(known.iter().map(|(_, v)| v.as_str())).collect();
    let assistant = norm_words(ASSISTANT);
    let owned = |n: &str, s: &str, k: &str| [format!(" {s} s {k} "), format!(" {s} {k} ")].into_iter().find_map(|p| n.find(&p).map(|i| i + p.len() - 1));
    let wrong = |sent: &str| {
        let n = format!(" {} ", norm_words(sent));
        known.iter().any(|(k, _)| {
            owned(&n, &assistant, k).is_some()
                || subjects.iter().find_map(|s| owned(&n, s, k)).is_some_and(|at| !known.iter().any(|(k2, v)| k2 == k && n[at..].contains(&format!(" {v} "))))
        })
    };
    let mut out: Vec<String> = Vec::new();
    for l in notes.lines() {
        if l.trim().starts_with('#') || own.contains(&bullet_key(l)) || !wrong(l) { out.push(l.to_string()); continue; }
        let kept: Vec<String> = sentences(l.trim_end()).into_iter().filter(|s| !wrong(s)).collect();
        let line = kept.join(" ");
        if !line.trim().trim_start_matches(['-', '*']).trim().is_empty() { out.push(line); }
    }
    clean_notes(&out.join("\n"))
}

/// `notes` without a "Facts to remember" sentence saying one of the user's things ("The user's name is Bo") that the
/// user's own words (`user_said`: their messages only) do not support: its value is in none of them, or it is the
/// value the user gave another of their things (`facts`, Elisa's own lines: "The user's dog is called Bo"). A 2B
/// summary of "remember that my dog is Bo" wrote "The user's name is Bo". Lines of `facts` themselves always stay.
pub fn without_unsupported_user_facts(notes: &str, facts: &[String], user_said: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:^| )user s ((?:[\p{L}\p{N}]+ ){0,3}?[\p{L}\p{N}]+) (?:is|are|was) (?:called |named )?(.+)$").unwrap());
    let known: Vec<(String, String)> = facts.iter().filter_map(|f| fact_parts(f)).collect();
    let own: HashSet<String> = facts.iter().map(|f| bullet_key(f)).collect();
    let said = format!(" {} ", norm_words(user_said));
    let unsupported = |sent: &str| {
        let n = norm_words(sent);
        let Some(c) = re.captures(&n) else { return false };
        let (k, v) = (c[1].to_string(), c[2].to_string());
        // the user's words name the value nowhere
        let words: Vec<&str> = v.split_whitespace().filter(|w| w.chars().count() >= 2).collect();
        if !words.is_empty() && !words.iter().any(|w| said.contains(&format!(" {w} "))) { return true; }
        // the value of another of the user's things, never of this one
        let theirs = |v2: &str| v == v2 || v.starts_with(&format!("{v2} "));
        known.iter().any(|(k2, v2)| *k2 != k && theirs(v2)) && !known.iter().any(|(k2, v2)| *k2 == k && theirs(v2))
    };
    let mut out: Vec<String> = Vec::new();
    let mut facts_sec = false;
    for l in notes.lines() {
        let t = l.trim();
        if t.starts_with('#') { facts_sec = norm_words(t).starts_with("facts"); out.push(l.to_string()); continue; }
        if !facts_sec || own.contains(&bullet_key(l)) { out.push(l.to_string()); continue; }
        let kept: Vec<String> = sentences(l.trim_end()).into_iter().filter(|s| !unsupported(s)).collect();
        let line = kept.join(" ");
        if !line.trim().trim_start_matches(['-', '*']).trim().is_empty() { out.push(line); }
    }
    clean_notes(&out.join("\n"))
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
/// (plain: the notes are shown in the app; it is Elisa's own list of the files her edits changed)
pub const FILES_NOTES: &str = "## Files changed";
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

// ----- whose names the facts give: a reply calling the user by another's name -----

/// One language's person words (`data/person_words.txt`).
#[derive(Default)]
struct PersonWords { vi: bool, me: Vec<String>, you: Vec<String>, about: Vec<String>, asks: Vec<String>, say: String, fact: String, things: Vec<(String, String)>, lives: String, works: String,
    /// the lead-ins of a sentence that says something over again ("as for", "in other words")
    restate: Vec<String> }

fn person_words() -> &'static [PersonWords] {
    static P: std::sync::OnceLock<Vec<PersonWords>> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let mut out: Vec<PersonWords> = Vec::new();
        let list = |v: &str| v.split('|').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect::<Vec<_>>();
        for l in include_str!("../../data/person_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if l.starts_with('[') { out.push(PersonWords { vi: l == "[vi]", ..Default::default() }); continue; }
            let (Some(cur), Some((k, v))) = (out.last_mut(), l.split_once(" = ")) else { continue };
            let v = v.trim();
            match k.trim() {
                "me" => cur.me = list(v), "you" => cur.you = list(v), "about me" => cur.about = list(v), "ask" => cur.asks = list(v), "say" => cur.say = v.to_string(), "fact" => cur.fact = v.to_string(),
                "things" => cur.things = v.split('|').filter_map(|p| p.split_once(':').map(|(a, b)| (a.trim().to_lowercase(), b.trim().to_string()))).collect(),
                "lives" => cur.lives = v.to_string(), "works" => cur.works = v.to_string(), "restate" => cur.restate = list(v),
                _ => {}
            }
        }
        out
    })
}

/// A verb after "he" / "she" / "the user" as it goes after "you": lives → live, is → are, has → have.
fn agree_you(verb: &str) -> String {
    match verb {
        "is" => "are".into(), "was" => "were".into(), "has" => "have".into(), "does" => "do".into(),
        v if v.ends_with("ies") && v.len() > 4 => format!("{}y", &v[..v.len() - 3]),
        v if ["sses", "shes", "ches", "xes", "zes", "oes"].iter().any(|e| v.ends_with(e)) => v[..v.len() - 2].to_string(),
        v if v.ends_with('s') && !v.ends_with("ss") && v.len() > 2 => v[..v.len() - 1].to_string(),
        v => v.to_string(),
    }
}

/// words a reply to a question about the user may have and still be read as one bare value (`NameFix::recall_answer`)
const RECALL_FEW_WORDS: usize = 2;

/// The value one of Elisa's fact lines gives, lowercase: "Linh" of "The user's name is Linh", "hanoi" of "The user
/// lives in Hanoi" (the place word before it left out). None for any other line.
fn line_value(l: &str) -> Option<String> {
    if fact_parts(l).is_some() {
        let rest = l.strip_prefix("The user's ")?;
        return Some(rest.split_once(" is called ").or_else(|| rest.split_once(" is ")).or_else(|| rest.split_once(" are "))?.1.trim().to_lowercase());
    }
    let what = l.strip_prefix("The user ")?.split_once(' ')?.1;
    Some(what.split_once(' ').filter(|(p, _)| ["in", "at", "as", "for", "on"].contains(p)).map_or(what, |(_, r)| r).trim().to_lowercase())
}

/// The words of a fact's sentence to the user in `w`'s language up to its value ("your name is", "bạn sống ở"), from
/// the language's templates (`data/person_words.txt`): a reply holding them states that fact. None for a line with no
/// template.
fn fact_lead(w: &PersonWords, l: &str) -> Option<String> {
    let tmpl = match fact_parts(l) {
        Some((k, _)) => w.fact.replace("{thing}", &w.things.iter().find(|(e, _)| *e == k).map_or(k.clone(), |(_, t)| t.clone())),
        None => match l.strip_prefix("The user ")?.split_once(' ')?.0 { "lives" => w.lives.clone(), "works" => w.works.clone(), _ => return None },
    };
    let lead = tmpl.split('{').next()?.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
    (lead.split_whitespace().count() >= 2).then_some(lead)
}

/// Whose names the facts give (`situation::ReplyFacts`): the user's own name, and the names of the user's people
/// and things (`user_fact`'s "The user's dog is called Bo": ("dog", "Bo")). After "remember that my dog is Bo"
/// models answered "nice to meet you, Bo" and, after /reset, "Hey Bo!".
#[derive(Debug, Clone, Default)]
pub struct NameFix { user: Option<String>, others: Vec<(String, String)>, values: Vec<String>, keys: Vec<String>, lines: Vec<String> }

impl NameFix {
    /// From Elisa's own third-person fact lines (`user_facts`, the notes' `fact_lines`).
    pub fn new(facts: &[String]) -> NameFix {
        let mut f = NameFix::default();
        // (a summary may join several facts on one line with "; ": each is its own, and a part that is no fact of the
        // user's, an earlier answer "Elisa computed … = 36", is none of them)
        let parts: Vec<&str> = facts.iter().flat_map(|l| l.split("; ")).collect();
        for l in parts {
            let l = l.trim().trim_start_matches(['-', '*', ' ']).trim_end_matches('.');
            // what the fact says of the user: a "The user's X is V" value, or the named or numbered words of another
            // ("The user lives in Hanoi": Hanoi)
            if l.starts_with("The user") && fact_to_you(l).is_some() && !f.lines.iter().any(|x| x == l) { f.lines.push(l.to_string()); }
            if let Some(rest) = l.strip_prefix("The user") {
                if let Some(w) = fact_parts(l).and_then(|(k, _)| k.split_whitespace().next().map(str::to_string)) { f.keys.push(w); }
                match fact_parts(l) {
                    Some(_) => if let Some((_, v)) = rest.split_once(" is ").or_else(|| rest.split_once(" are ")) {
                        let v = v.trim().strip_prefix("called ").unwrap_or(v.trim());
                        if !v.is_empty() { f.values.push(v.to_lowercase()); }
                    },
                    None => f.values.extend(rest.split_whitespace().filter(|w| w.chars().any(|c| c.is_uppercase() || c.is_ascii_digit())).map(str::to_lowercase)),
                }
            }
            if let Some(v) = l.strip_prefix("The user wants to be called ").or_else(|| l.strip_prefix("The user's name is ")) {
                f.user = Some(v.trim().to_string());
                continue;
            }
            let Some(rest) = l.strip_prefix("The user's ") else { continue };
            let Some((k, v)) = rest.split_once(" is called ").or_else(|| rest.split_once(" are called ")).or_else(|| rest.split_once(" is ")) else { continue };
            // "wife's name is Mai": the wife's
            let k = k.trim().trim_end_matches(" name").trim_end_matches("'s").to_string();
            let v = v.trim().to_string();
            if name_like(&v) && k != "name" && !f.others.iter().any(|(_, x)| *x == v) { f.others.push((k, v)); }
        }
        if let Some(u) = &f.user { f.others.retain(|(_, v)| !v.eq_ignore_ascii_case(u)); }
        f
    }

    pub fn is_empty(&self) -> bool { self.others.is_empty() && self.values.is_empty() }

    /// `reply` with the user no longer called by another's name: the name where it is said to the user (after a
    /// comma and before the end of the sentence, "Nice to meet you, Bo!"; after one or two opening words, "Hey Bo!";
    /// opening the reply with a comma, "Bo, …") becomes the user's own name, or goes when Elisa knows none. A name
    /// talked about ("your dog, Bo, …" with its thing before it, "Bo is a lovely name") stays.
    /// `reply` to `ask` with every fact of the user's it asks for that Elisa holds: a reply that leaves one out gets
    /// it said ("What's my name, where do I live and what is my meeting code?" answered "Linh" → "Linh. You live in
    /// Hanoi. Your meeting code is ORCHID-42."), and a template in place of the answer ("My name is [Your Name]")
    /// becomes those facts. A fact is asked by the user's own words for it, the first person of its line: "my
    /// meeting code" for "The user's meeting code is …", "I live" for "The user lives in …".
    pub fn answer(&self, ask: &str, reply: &str) -> String {
        static SLOT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let slot = SLOT.get_or_init(|| regex::Regex::new(r"\[[A-Z][A-Za-z' ]{1,40}\](?:[^(]|$)").unwrap());
        let a = format!(" {} ", ask.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "));
        let asked: Vec<&String> = self.lines.iter().filter(|l| {
            let rest = l.strip_prefix("The user").unwrap_or("");
            match rest.strip_prefix("'s ") {
                Some(_) => fact_parts(l).is_some_and(|(k, _)| a.contains(&format!(" my {k} "))),
                None => rest.split_whitespace().next().is_some_and(|v| {
                    let stem = v.strip_suffix("es").filter(|x| x.ends_with(['h', 's', 'x'])).or_else(|| v.strip_suffix('s')).unwrap_or(v);
                    let verb = if v == "is" { "am" } else { stem };
                    a.contains(&format!(" i {verb} "))
                }),
            }
        }).collect();
        // (a question only: "my name is Linh and I live in Hanoi" tells them)
        if asked.is_empty() || !ask.contains(['?', '？']) { return reply.to_string(); }
        let say = |l: &String| fact_to_you(l).map(|y| { let mut c = y.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) + "." });
        if slot.is_match(reply) { return asked.iter().filter_map(|l| say(l)).collect::<Vec<_>>().join(" "); }
        let low = reply.to_lowercase();
        let value = |l: &str| -> String {
            // the value as written ("ORCHID-42"), not as `fact_parts` compares it
            match l.split_once(" is called ").or_else(|| l.split_once(" is ")).filter(|_| fact_parts(l).is_some()) { Some((_, v)) => v.to_lowercase(), None => l.rsplit_once(" in ").or_else(|| l.rsplit_once(" from ")).or_else(|| l.rsplit_once(" at ")).map_or(l, |(_, v)| v).to_lowercase() }
        };
        // only a reply that answers already (one asked value in it) is completed: one that says none of them is no
        // answer to fill (a copy of an earlier paste must stay a copy, for the copy check to ask again)
        if !asked.iter().any(|l| low.contains(&value(l).to_lowercase())) { return reply.to_string(); }
        let missing: Vec<String> = asked.iter().filter(|l| !low.contains(&value(l).to_lowercase())).filter_map(|l| say(l)).collect();
        if missing.is_empty() { return reply.to_string(); }
        let r = reply.trim_end();
        let sep = if r.ends_with(['.', '!', '?']) { " " } else if r.is_empty() { "" } else { ". " };
        format!("{r}{sep}{}", missing.join(" "))
    }

    /// `reply` to a question about the user's own facts (`ask`: a word of theirs for themself, `data/person_words.txt`,
    /// with "about me" or one of their facts' things in it: "tóm tắt những gì bạn biết về mình", "tên mình là gì")
    /// that says none of those facts and speaks of itself instead ("Tôi là Elisa, một trợ lý…"): Elisa's answer from
    /// the user's facts, in the ask's language (the asked ones, or all when it asks about the user as a whole).
    pub fn about_user(&self, ask: &str, reply: &str) -> String {
        if self.lines.is_empty() { return reply.to_string(); }
        let vi = matches!(crate::reply_lang::detect(ask), Some(crate::reply_lang::Lang::Vi));
        // the ask's language first, then the other: the one whose words the ask uses
        let mut langs: Vec<&PersonWords> = person_words().iter().collect();
        langs.sort_by_key(|w| w.vi != vi);
        for w in langs {
            if let Some(t) = self.about_user_in(w, ask, reply) { return t; }
        }
        reply.to_string()
    }

    /// `about_user` in one language's words: Elisa's answer, or None when the ask is no question about the user in
    /// these words or the reply stands.
    fn about_user_in(&self, w: &PersonWords, ask: &str, reply: &str) -> Option<String> {
        let words = |t: &str| format!(" {} ", t.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" "));
        // (a fact's thing right next to a "me" word, either side: "my name", "tên mình", "mình sống"; in a question)
        let (whole, asked) = self.asked_facts(w, ask)?;
        // a fact of theirs said to them ("Your name is Linh", "Bạn sống ở Hà Nội"; values compared without accents or
        // spaces) is an answer, kept; so is a reply that does not speak of itself ("Linh.", "**Tên**: Linh", a copy of
        // an earlier paste, left to the checks after this one). One speaking of itself instead ("Tôi là Elisa…", "I
        // only have the details about Linh – her name…", "Tôi là Linh, sinh năm 2000…") gets Elisa's answer.
        let fold = |t: &str| t.chars().map(crate::reply_lang::fold_mark).filter(|c| c.is_alphanumeric()).collect::<String>();
        // the values that answer it: the asked facts' own (another fact of theirs, "You live in Hanoi" for "what is my
        // name?", is no answer), or any of theirs when it asks about them as a whole
        let pick: Vec<&String> = if whole { self.lines.iter().collect() } else { asked };
        let answers: Vec<String> = if whole { self.values.clone() } else { pick.iter().filter_map(|l| line_value(l)).collect() };
        let you: Vec<&String> = person_words().iter().flat_map(|p| p.you.iter()).collect();
        let me: Vec<&String> = person_words().iter().flat_map(|p| p.me.iter()).collect();
        let mut itself = false;
        let mut rest = reply;
        while !rest.is_empty() {
            let cut = rest.char_indices().find(|(i, ch)| *ch == '\n' || (matches!(ch, '.' | '!' | '?') && rest[i + 1..].chars().next().is_none_or(char::is_whitespace)))
                .map_or(rest.len(), |(i, ch)| i + ch.len_utf8());
            let sent = &rest[..cut];
            rest = &rest[cut..];
            let sw = words(sent);
            let value = answers.iter().any(|v| !v.is_empty() && fold(sent).contains(&fold(v)));
            if value && you.iter().any(|y| sw.contains(&format!(" {y} "))) { return None; }
            itself |= me.iter().any(|m| sw.contains(&format!(" {m} ")) || sw.contains(&format!(" {m}'")));
        }
        if !itself { return None; }
        self.said_facts(w, &pick, whole)
    }

    /// The facts of `pick` said to the user in `w`'s language ("Tên của bạn là Linh.", "You live in Hanoi."): one
    /// alone, several joined, all of them (`whole`) after the language's lead ("Here is what I know about you:").
    fn said_facts(&self, w: &PersonWords, pick: &[&String], whole: bool) -> Option<String> {
        let said = self.fact_sentences(w, pick);
        if said.is_empty() { return None; }
        if !whole { return Some(said.join(" ")); }
        Some(w.say.replace("{facts}", &said.join(" ")))
    }

    /// Each fact of `pick` as one sentence to the user in `w`'s language.
    fn fact_sentences(&self, w: &PersonWords, pick: &[&String]) -> Vec<String> {
        pick.iter().filter_map(|l| {
            if let Some((k, _)) = fact_parts(l) {
                let rest = l.strip_prefix("The user's ")?;
                let v = rest.split_once(" is called ").or_else(|| rest.split_once(" is ")).or_else(|| rest.split_once(" are "))?.1.trim();
                let thing = w.things.iter().find(|(e, _)| *e == k).map_or(k.clone(), |(_, t)| t.clone());
                return Some(w.fact.replace("{thing}", &thing).replace("{value}", v));
            }
            let rest = l.strip_prefix("The user ")?;
            let (verb, what) = rest.split_once(' ')?;
            // (the English place word goes: "lives in Hanoi" → "sống ở Hanoi")
            let place = what.split_once(' ').filter(|(p, _)| ["in", "at", "as", "for", "on"].contains(p)).map_or(what, |(_, r)| r);
            let t = match verb { "lives" => &w.lives, "works" => &w.works, _ => return fact_to_you(l).map(|y| { let mut c = y.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) + "." }) };
            Some(t.replace("{what}", what).replace("{place}", place))
        }).collect()
    }

    /// Elisa's answer from the user's facts alone, for a question about them that got no usable reply (a tiny model
    /// after a compaction): the asked facts ("what's my name and my meeting code?"), or all of them when it asks
    /// about the user as a whole, in the ask's language. None when it asks about none Elisa holds.
    pub fn answer_from_facts(&self, ask: &str) -> Option<String> {
        if self.lines.is_empty() { return None; }
        let vi = matches!(crate::reply_lang::detect(ask), Some(crate::reply_lang::Lang::Vi));
        let mut langs: Vec<&PersonWords> = person_words().iter().collect();
        langs.sort_by_key(|w| w.vi != vi);
        langs.into_iter().find_map(|w| {
            let (whole, asked) = self.asked_facts(w, ask)?;
            let pick: Vec<&String> = if whole { self.lines.iter().collect() } else { asked };
            self.said_facts(w, &pick, whole)
        })
    }

    /// Elisa's answer from the user's facts to a question about them (`asked_facts`) whose reply gives none of the
    /// asked values: the assistant's own name instead (`own`: "Elisa" to "what did I tell you my name was?",
    /// moondream:1.8b after a model switch), a word or two that are no value of theirs ("Unknown"), or the asked
    /// thing said with another value ("Your name is Minh."). None when the ask is no such question, the reply holds
    /// an asked value (the checks after it complete it), or it says more in other words.
    pub fn recall_answer(&self, ask: &str, reply: &str, own: &[String]) -> Option<String> {
        if self.lines.is_empty() { return None; }
        let vi = matches!(crate::reply_lang::detect(ask), Some(crate::reply_lang::Lang::Vi));
        let mut langs: Vec<&PersonWords> = person_words().iter().collect();
        langs.sort_by_key(|w| w.vi != vi);
        let fold = |t: &str| t.chars().map(crate::reply_lang::fold_mark).filter(|c| c.is_alphanumeric()).collect::<String>();
        let rw: Vec<String> = reply.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).map(str::to_string).collect();
        let spaced = format!(" {} ", rw.join(" "));
        for w in langs {
            let Some((whole, asked)) = self.asked_facts(w, ask) else { continue };
            let pick: Vec<&String> = if whole { self.lines.iter().collect() } else { asked };
            let answers: Vec<String> = if whole { self.values.clone() } else { pick.iter().filter_map(|l| line_value(l)).collect() };
            let folded = fold(reply);
            if answers.iter().any(|v| !v.is_empty() && folded.contains(&fold(v))) { return None; }
            let names_itself = own.iter().map(|o| fold(o)).filter(|o| o.chars().count() >= 3).any(|o| rw.iter().any(|x| fold(x) == o));
            // the asked thing said with a value of its own ("your name is …")
            let other_value = pick.iter().filter_map(|l| fact_lead(w, l)).any(|p| spaced.contains(&format!(" {p} ")));
            let few = !rw.is_empty() && rw.len() <= RECALL_FEW_WORDS && !reply.chars().any(|c| c.is_ascii_digit());
            return if names_itself || other_value || few { self.said_facts(w, &pick, whole) } else { None };
        }
        None
    }

    /// Every fact of the user's said to them, in Vietnamese (`vi`) or English: for Elisa's own resume line.
    pub fn all_facts_said(&self, vi: bool) -> Vec<String> {
        let Some(w) = person_words().iter().find(|w| w.vi == vi) else { return Vec::new() };
        self.fact_sentences(w, &self.lines.iter().collect::<Vec<_>>())
    }

    /// What `ask` asks about the user in `w`'s words: (about them as a whole, the asked facts' lines). None when it
    /// is no question about them in these words.
    fn asked_facts(&self, w: &PersonWords, ask: &str) -> Option<(bool, Vec<&String>)> {
        let words = |t: &str| format!(" {} ", t.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" "));
        let low = words(ask);
        let has = |p: &str| low.contains(&format!(" {p} "));
        if !w.me.iter().any(|m| has(m)) { return None; }
        let thing_of = |l: &str| -> Option<String> {
            fact_parts(l).map(|(k, _)| k).or_else(|| l.strip_prefix("The user ").and_then(|r| r.split_whitespace().next()).map(str::to_string))
        };
        let local = |k: &str| w.things.iter().find(|(e, _)| e == k).map(|(_, t)| t.to_lowercase());
        let beside = |t: &str| w.me.iter().any(|m| has(&format!("{m} {t}")) || has(&format!("{t} {m}")));
        let question = ask.contains(['?', '？']) || w.asks.iter().any(|a| has(a));
        let asked: Vec<&String> = if !question { Vec::new() } else { self.lines.iter().filter(|l| thing_of(l).is_some_and(|k| beside(&k) || local(&k).is_some_and(|t| beside(&t))
            || [("lives", &w.lives), ("works", &w.works)].iter().any(|(v, t)| k == *v && (beside(v.trim_end_matches('s')) || t.split_whitespace().nth(1).is_some_and(|x| beside(&x.to_lowercase())))))).collect() };
        let whole = w.about.iter().any(|a| has(a));
        (whole || !asked.is_empty()).then_some((whole, asked))
    }

    pub fn fix(&self, reply: &str) -> String {
        let mut s = self.without_restated(&self.to_you(&self.told_by_user(reply)));
        for (k, v) in &self.others {
            // at most a few rounds: each fixes one place
            for _ in 0..4 {
                let Some((a, b, with)) = self.addressed(&s, k, v) else { break };
                s = format!("{}{with}{}", &s[..a], &s[b..]);
            }
        }
        s
    }

    /// `reply` without a sentence that only says the user's facts again: it names one of them (`values`), each one it
    /// names was said earlier in the reply, and every other word of it was there too or is a short joining word ("Your
    /// name is Linh. … Your name is Linh and the meeting code is ORCHID-42." keeps the first). A sentence with a new
    /// word ("Nice to meet you, Linh!") stays. So does a sentence that opens by saying something over again (a
    /// `restate` lead-in of `data/person_words.txt`: "As for …", "In other words …") and says a fact of theirs to them
    /// once more: each value it names was said earlier, and it speaks to the user (a `you` word) with that fact's own
    /// thing word ("Your name is Linh and you live in Hanoi. As for where you live, you are in the capital city of
    /// Vietnam, Hanoi." keeps the first: "live" is the thing of "The user lives in Hanoi"). One telling something
    /// new of a value ("Your name, Linh, means 'soul'.") has no such lead-in and stays.
    fn without_restated(&self, reply: &str) -> String {
        if self.values.is_empty() { return reply.to_string(); }
        let words = |t: &str| t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>();
        let things = self.thing_words();
        let you: Vec<&String> = person_words().iter().flat_map(|p| p.you.iter()).collect();
        let leads: Vec<Vec<String>> = person_words().iter().flat_map(|p| p.restate.iter()).map(|l| words(l)).filter(|l| !l.is_empty()).collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut said = String::new();
        let mut out = String::new();
        let mut rest = reply;
        while !rest.is_empty() {
            let cut = rest.char_indices().find(|(i, ch)| *ch == '\n' || (matches!(ch, '.' | '!' | '?') && rest[i + 1..].chars().next().is_none_or(char::is_whitespace)))
                .map_or(rest.len(), |(i, ch)| i + ch.len_utf8());
            let sent = &rest[..cut];
            rest = &rest[cut..];
            let low = sent.to_lowercase();
            let vals: Vec<&String> = self.values.iter().filter(|v| !v.is_empty() && low.contains(v.as_str())).collect();
            let ws = words(sent);
            let thing_said = |v: &str| things.iter().filter(|(x, _)| x == v).flat_map(|(_, t)| t)
                .any(|t| ws.iter().any(|w| w == t || (t.chars().count() >= 4 && t.is_ascii() && w.starts_with(t.trim_end_matches(['e', 's'])))));
            let restates = leads.iter().any(|l| ws.starts_with(l)) && ws.iter().any(|w| you.contains(&w)) && vals.iter().all(|v| thing_said(v));
            let again = !vals.is_empty() && vals.iter().all(|v| said.contains(v.as_str()))
                && (ws.iter().all(|w| seen.contains(w) || w.chars().count() <= 3) || restates);
            if again && !out.trim().is_empty() {
                // (the space before it goes with it)
                continue;
            }
            seen.extend(ws);
            said.push_str(&low);
            out.push_str(sent);
        }
        out
    }

    /// Each fact's value (lowercase) with the words of its thing, in English and in each language of
    /// `data/person_words.txt`: "The user's name is Linh" → ("linh", ["name", "tên"]); "The user lives in Hanoi" →
    /// ("hanoi", ["live", "sống"]): the verb without its "s", and the words of the language's `lives` line that are no
    /// `you` word, place holder or short joining word.
    fn thing_words(&self) -> Vec<(String, Vec<String>)> {
        let low_words = |t: &str| t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>();
        let langs = person_words();
        let you: Vec<&String> = langs.iter().flat_map(|p| p.you.iter()).collect();
        let line_words = |l: &str| -> Vec<String> {
            low_words(&l.replace("{what}", " ").replace("{place}", " ")).into_iter()
                .filter(|w| w.chars().count() >= 3 && !you.contains(&w)).collect()
        };
        self.lines.iter().filter_map(|l| {
            if let Some((k, v)) = fact_parts(l) {
                let mut t = low_words(&k);
                t.extend(langs.iter().flat_map(|p| p.things.iter()).filter(|(e, _)| *e == k).flat_map(|(_, x)| low_words(x)));
                return Some((v, t));
            }
            let rest = l.strip_prefix("The user ")?;
            let (verb, what) = rest.split_once(' ')?;
            let v = what.split_once(' ').filter(|(p, _)| ["in", "at", "as", "for", "on", "from"].contains(p)).map_or(what, |(_, r)| r).trim().to_lowercase();
            // ("is from Hanoi": "from" is its thing)
            let head = if verb == "is" { what.split_whitespace().next().unwrap_or(verb) } else { verb.strip_suffix('s').unwrap_or(verb) };
            let mut t = vec![head.to_string()];
            for p in langs {
                match verb { "lives" => t.extend(line_words(&p.lives)), "works" => t.extend(line_words(&p.works)), _ => {} }
            }
            Some((v, t))
        }).collect()
    }

    /// `reply` with who told whom the right way round: a sentence naming one of the user's facts (`values`) that says
    /// Elisa told it to them ("I told you your name is Linh") says they told Elisa ("You told me…"), from the wordings
    /// of `data/told_words.txt`.
    fn told_by_user(&self, reply: &str) -> String { self.told_swap(reply, &told_words().0) }

    /// `told_by_user` for a question asking what the user told Elisa (`asks` of the [asked] section of
    /// `data/told_words.txt`): "I said your name is Linh" says "You told me your name is Linh". Other questions: as
    /// written ("as I said" may be Elisa's own saying).
    pub fn told_answer(&self, ask: &str, reply: &str) -> String {
        let (_, asked, asks) = told_words();
        let low = ask.to_lowercase().replace(['’', '‘'], "'");
        let asked_told = asks.iter().any(|a| low.match_indices(a.as_str()).any(|(i, _)| {
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            edge(low[..i].chars().next_back()) && edge(low[i + a.len()..].chars().next())
        }));
        if asked_told { self.told_swap(reply, asked) } else { reply.to_string() }
    }

    fn told_swap(&self, reply: &str, pairs: &[(regex::Regex, String)]) -> String {
        if self.values.is_empty() { return reply.to_string(); }
        let low_values: Vec<String> = self.values.iter().map(|v| v.to_lowercase()).collect();
        let mut out = String::new();
        for sent in reply.split_inclusive(['.', '!', '?', '\n']) {
            let low = sent.to_lowercase();
            let mut t = sent.to_string();
            // (a sentence saying the fact as Elisa's own, "I told you my name is Linh", is no mix-up of who told whom:
            // the claimed-facts check asks for it again, `claimed_user_facts`)
            if low_values.iter().any(|v| !v.is_empty() && low.contains(v.as_str())) && claimed_user_facts(sent, &self.values).is_empty() {
                for (re, to) in pairs.iter() {
                    let Some(c) = re.captures(&t) else { continue };
                    let (m, pre, post) = (c.get(0).unwrap(), c.get(1).map_or("", |x| x.as_str()), c.get(2).map_or("", |x| x.as_str()));
                    let said = &t[m.start() + pre.len()..m.end() - post.len()];
                    let upper = said.chars().next().is_some_and(char::is_uppercase);
                    let to = if upper { let mut ch = to.chars(); ch.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + ch.as_str()) } else { to.clone() };
                    t = format!("{}{pre}{to}{post}{}", &t[..m.start()], &t[m.end()..]);
                    break;
                }
            }
            out.push_str(&t);
        }
        out
    }

    /// `reply` speaking to the user, not about them: a sentence that says one of their facts (`values`) in Elisa's
    /// own third-person wording ("The user's name is Linh, they live in Hanoi" after a compaction, copied by
    /// minicpm-v) is put in the second person ("Your name is Linh, you live in Hanoi"), as is any sentence naming
    /// "the user" once they told Elisa about themself; in Vietnamese "người dùng" becomes "bạn". With no facts of
    /// theirs, a reply is left as written.
    fn to_you(&self, reply: &str) -> String {
        // (they told Elisa about themself: "the user" in a reply to them is them, "The user's late…", "I have no
        // personal information about the user" included)
        if self.values.is_empty() { return reply.to_string(); }
        static THIRD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        // (a curly apostrophe too: "The user’s name" read as "the user" + "’s" became "You’s name")
        let re = THIRD.get_or_init(|| regex::Regex::new(r"(?i)\b(the user['’]s|the user|người dùng)(\s+([a-z]+))?\b|\b(they|their|them|they're|they've|themselves|themself)\b").unwrap());
        let mut out = String::new();
        let mut rest = reply;
        // the sentence before spoke of the user: a third-person one after it about their facts is them too
        let mut about_user = false;
        while !rest.is_empty() {
            let end = rest.find(['.', '!', '?', '\n']).map_or(rest.len(), |i| i + rest[i..].chars().next().map_or(1, char::len_utf8));
            let (sent, next) = rest.split_at(end);
            rest = next;
            let low = sent.to_lowercase().replace('’', "'");
            let about = low.contains("the user") || low.contains("người dùng");
            if !about {
                let (t, was) = self.pronoun_to_you(sent, about_user);
                about_user = was || (sent.trim().is_empty() && about_user);
                out.push_str(&t);
                continue;
            }
            about_user = true;
            let cap = |w: &str, like: &str| if like.starts_with(char::is_uppercase) { let mut c = w.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str()) } else { w.to_string() };
            // "they / their" are the user only right after a phrase naming the user ("The user's name is Linh, they
            // live in Hanoi"), never after one naming someone of theirs ("The user's parents live in Da Nang, where
            // they run a shop"; "The user's dog Bo … they love walks")
            let mut theirs = false;
            out.push_str(&re.replace_all(sent, |c: &regex::Captures| {
                if let Some(p) = c.get(4) {
                    let w = p.as_str();
                    if !theirs { return w.to_string(); }
                    let you = match w.to_lowercase().as_str() { "their" => "your", "they're" => "you're", "they've" => "you've", "themselves" | "themself" => "yourself", _ => "you" };
                    return cap(you, w);
                }
                let who = c[1].replace('’', "'");
                let who = who.as_str();
                let verb = c.get(3).map(|v| v.as_str()).unwrap_or("");
                // "the user's X": X a fact of theirs, or someone or something of theirs; "the user is X" when nothing but
                // the end, punctuation or a linking word follows X ("The user's late." / "late for the meeting")
                let after = sent[c.get(0).map_or(sent.len(), |x| x.end())..].trim_start();
                let next: String = after.chars().take_while(|ch| ch.is_alphabetic()).collect::<String>().to_lowercase();
                let linking = after.is_empty() || after.starts_with(|ch: char| !ch.is_alphanumeric())
                    || include_str!("../../data/linking_words.txt").lines().map(str::trim).any(|l| !l.starts_with('#') && !l.is_empty() && l == next);
                let mine = self.keys.iter().any(|k| k.eq_ignore_ascii_case(verb));
                let someone = self.others.iter().any(|(k, _)| k.split_whitespace().any(|w| w.eq_ignore_ascii_case(verb)));
                let is_form = !verb.is_empty() && !mine && !someone && linking;
                theirs = match who.to_lowercase().as_str() {
                    "the user's" => (mine && !someone) || is_form,
                    _ => true,
                };
                let (head, verb) = match who.to_lowercase().as_str() {
                    "the user's" if !is_form => ("your", verb.to_string()),
                    "the user's" => ("you're", verb.to_string()),
                    "người dùng" => ("bạn", verb.to_string()),
                    // the verb after "the user" agrees with "you": lives → live, is → are, has → have
                    _ => ("you", agree_you(verb)),
                };
                let head = cap(head, who);
                if verb.is_empty() { head } else { format!("{head} {verb}") }
            }));
        }
        out
    }

    /// `sent` (no "the user" in it) with the user spoken to, not of: a sentence opening with a third-person word ("She
    /// lives in Hanoi.", "Her meeting code is ORCHID-42.", "Linh's meeting code…", "Cô ấy sống ở Hà Nội.") that says
    /// one of the user's own facts (a value of theirs, not of someone of theirs), after a sentence about the user or
    /// naming the fact's own word ("meeting code"), says "you" / "your" ("bạn"). Whether it is about the user.
    fn pronoun_to_you(&self, sent: &str, after_user: bool) -> (String, bool) {
        static OPEN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let open = OPEN.get_or_init(|| regex::Regex::new(r"(?i)^(\s*)(she|he|they|her|his|their|cô ấy|anh ấy|chị ấy|em ấy|họ)\b(\s+([\p{L}]+))?").unwrap());
        let low = sent.to_lowercase();
        let others: Vec<String> = self.others.iter().map(|(_, v)| v.to_lowercase()).collect();
        let own_value = self.values.iter().any(|v| !v.is_empty() && !others.contains(v) && low.contains(v.as_str()));
        if !own_value { return (sent.to_string(), low.contains("your ") || low.trim_start().starts_with("you ")); }
        let names_key = self.keys.iter().any(|k| low.contains(&k.to_lowercase()));
        // the user's own name as an owner: "Linh's meeting code" → "Your meeting code"
        if let Some(u) = &self.user {
            for ap in ["'s ", "’s "] {
                let lead = &sent[..sent.len() - sent.trim_start().len()];
                if let Some(r) = sent.trim_start().strip_prefix(&format!("{u}{ap}")) {
                    if names_key || after_user { return (format!("{lead}Your {r}"), true); }
                }
            }
        }
        let Some(c) = open.captures(sent) else { return (sent.to_string(), low.contains("your ") || low.trim_start().starts_with("you ")) };
        if !(after_user || names_key) { return (sent.to_string(), false); }
        let w = c[2].to_lowercase();
        let verb = c.get(4).map_or("", |v| v.as_str());
        let to = match w.as_str() {
            "her" | "his" | "their" => ("your".to_string(), verb.to_string()),
            "she" | "he" | "they" => ("you".to_string(), agree_you(verb)),
            _ => ("bạn".to_string(), verb.to_string()),
        };
        let head = if c[2].starts_with(char::is_uppercase) { let mut ch = to.0.chars(); ch.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + ch.as_str()) } else { to.0 };
        let new = if c.get(3).is_some() { format!("{}{head} {}", &c[1], to.1) } else { format!("{}{head}", &c[1]) };
        (format!("{new}{}", &sent[c.get(0).unwrap().end()..]), true)
    }

    /// The first place `reply` calls the user `v`: (start, end, replacement).
    fn addressed(&self, reply: &str, key: &str, v: &str) -> Option<(usize, usize, String)> {
        let key_words: Vec<String> = key.to_lowercase().split_whitespace().map(str::to_string).collect();
        for (i, _) in reply.match_indices(v) {
            let j = i + v.len();
            let before_c = reply[..i].chars().next_back();
            let after = &reply[j..];
            if before_c.is_some_and(|c| c.is_alphanumeric()) || after.chars().next().is_some_and(|c| c.is_alphanumeric()) { continue; }
            // the name ends its sentence or clause: punctuation or the line's end next
            let next = after.trim_start_matches([' ', '\t']).chars().next();
            if !next.is_none_or(|c| matches!(c, '!' | '.' | '?' | ',' | ';' | ':' | '\n' | ')' | '—')) { continue; }
            // its sentence so far
            let line = &reply[..i];
            let start = line.rfind(['.', '!', '?', '\n']).map_or(0, |p| p + 1);
            let sent = &line[start..];
            let words: Vec<String> = sent.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
            if words.iter().any(|w| key_words.contains(w)) { continue; }
            let trimmed = sent.trim_end();
            let user = self.user.clone();
            if trimmed.ends_with(',') {
                // ", Bo!": the comma and the name, or the name made the user's
                let comma = start + trimmed.len() - 1;
                return Some(match user { Some(u) => (i, j, u), None => (comma, j, String::new()) });
            }
            if words.is_empty() && next == Some(',') {
                // "Bo, nice to meet you": the name and its comma go; the next word starts the sentence
                if let Some(u) = user { return Some((i, j, u)); }
                let rest = after.trim_start_matches([' ', '\t']).strip_prefix(',').unwrap_or(after).trim_start();
                let mut cs = rest.chars();
                let up = cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default();
                return Some((i, reply.len(), up));
            }
            if (1..=2).contains(&words.len()) && !trimmed.ends_with([':', ';']) {
                // "Hey Bo!": the name and the space before it
                let sp = line.trim_end_matches([' ', '\t']).len();
                return Some(match user { Some(u) => (i, j, u), None => (sp, j, String::new()) });
            }
        }
        None
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    /// A pet's name is kept as the pet's, and a reply calling the user by it is put right
    #[test]
    fn a_reply_calling_the_user_by_a_pets_name_is_put_right() {
        assert_eq!(user_facts("hi! remember that my dog is Bo"), ["The user's dog is called Bo"]);
        assert_eq!(user_facts("my favourite colour is green"), ["The user's favourite colour is green"]);
        assert_eq!(user_facts("my meeting code is ORCHID-42"), ["The user's meeting code is ORCHID-42"]);
        assert_eq!(fact_lines("- The user's dog is called Bo"), ["The user's dog is called Bo"]);
        let f = NameFix::new(&user_facts("hi! remember that my dog is Bo"));
        assert_eq!(f.fix("Nice to meet you, Bo!"), "Nice to meet you!");
        assert_eq!(f.fix("Hey Bo! How can I help?"), "Hey! How can I help?");
        assert_eq!(f.fix("Bo, nice to meet you."), "Nice to meet you.");
        assert_eq!(f.fix("hello again, Bo. What's up?"), "hello again. What's up?");
        // the name talked about stays
        for r in ["Got it! Your dog is called Bo.", "Bo is a lovely name for a dog!", "Say hi to Bo for me!", "Your dog, Bo, sounds sweet."] {
            assert_eq!(f.fix(r), r, "{r:?}");
        }
        // with the user's own name known: that name instead
        let f = NameFix::new(&["The user's name is Linh".into(), "The user's dog is called Bo".into()]);
        // a reply about the user in Elisa's own fact wording speaks to them
        let p = NameFix::new(&["The user's name is Linh".into(), "The user lives in Hanoi".into(), "The user's meeting code is ORCHID-42".into()]);
        // facts joined on one notes line are each their own; an earlier answer among them is none
        let joined = NameFix::new(&fact_lines("The user's name is Linh; the meeting code is ORCHID-42; Elisa computed (15/100*240) = 36.\n- The user's meeting code is ORCHID-42"));
        assert_eq!(joined.answer("what is my name and my meeting code?", "Your name is Linh and your meeting code is ORCHID-42."), "Your name is Linh and your meeting code is ORCHID-42.");
        assert_eq!(joined.fix("I told you your name is Linh; you originally said it."), "You told me your name is Linh; you originally said it.");
        // the user told Elisa their facts, not the other way round
        assert_eq!(p.fix("I told you your name is Linh."), "You told me your name is Linh.");
        assert_eq!(p.fix("As I told you, your meeting code is ORCHID-42."), "As you told me, your meeting code is ORCHID-42.");
        assert_eq!(p.fix("Mình đã nói với bạn rằng tên bạn là Linh."), "Bạn đã nói với mình rằng tên bạn là Linh.");
        assert_eq!(p.fix("I told you a joke earlier."), "I told you a joke earlier.");
        // asked what the user told: any first-person telling of their fact is theirs; asked anything else, as written
        let ask = "what did I tell you my name was?";
        assert_eq!(p.told_answer(ask, &p.fix("I said your name is Linh.")), "You told me your name is Linh.");
        assert_eq!(p.told_answer(ask, "As I mentioned, your name is Linh."), "As you told me, your name is Linh.");
        assert_eq!(p.told_answer("tôi đã nói tên tôi là gì?", "Tôi đã nói tên bạn là Linh."), "Bạn đã nói với tôi tên bạn là Linh.");
        assert_eq!(p.told_answer("what's my name?", "As I said, your name is Linh."), "As I said, your name is Linh.");
        assert_eq!(p.told_answer(ask, "I said hello to you."), "I said hello to you.");
        // the user spoken of ("The user’s name", "She lives…", "Her meeting code…", "cô ấy"): spoken to
        assert_eq!(p.fix("The user’s name is Linh. She lives in Hanoi. Her meeting code is ORCHID-42."), "Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42.");
        assert_eq!(p.fix("Her name is Linh. She lives in Hanoi."), "Your name is Linh. You live in Hanoi.");
        assert_eq!(p.fix("Linh's meeting code is ORCHID-42."), "Your meeting code is ORCHID-42.");
        assert_eq!(p.fix("Tên của người dùng là Linh. Cô ấy sống ở Hanoi."), "Tên của bạn là Linh. Bạn sống ở Hanoi.");
        let w = NameFix::new(&["The user's name is Linh".into(), "The user's wife's name is Mai".into()]);
        assert_eq!(w.fix("Your name is Linh. Her name is Mai."), "Your name is Linh. Her name is Mai.", "someone of theirs");
        assert_eq!(p.fix("She is a famous singer."), "She is a famous singer.", "no fact of theirs");
        // a question about the user answered about the assistant: Elisa's answer from the user's facts
        let ask_vi = "tóm tắt những gì bạn biết về mình";
        assert_eq!(p.about_user(ask_vi, "Tôi là Elisa, một trợ lý. Tôi giúp bạn với mọi việc."),
            "Những gì mình biết về bạn: Tên của bạn là Linh. Bạn sống ở Hanoi. Mã cuộc họp của bạn là ORCHID-42.");
        assert_eq!(p.about_user("tên mình là gì?", "Mình là Elisa."), "Tên của bạn là Linh.");
        assert_eq!(p.about_user("mình sống ở đâu?", "Mình là trợ lý ảo."), "Bạn sống ở Hanoi.");
        assert_eq!(p.about_user("what do you know about me?", "I am Elisa, an assistant."),
            "Here is what I know about you: Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42.");
        // an answer with their facts, or a question about the assistant: kept
        assert_eq!(p.about_user(ask_vi, "Bạn tên Linh, sống ở Hà Nội."), "Bạn tên Linh, sống ở Hà Nội.");
        assert_eq!(p.about_user(ask_vi, "Tôi là Linh, sinh năm 2000 ở Hà Nội. Tôi đang chuẩn bị một bài thuyết trình."),
            "Những gì mình biết về bạn: Tên của bạn là Linh. Bạn sống ở Hanoi. Mã cuộc họp của bạn là ORCHID-42.", "the user's facts as its own");
        assert_eq!(p.about_user("what do you know about me?", "I don't have enough information to know about you. I only have the details about Linh – her name, meeting code, and location."),
            "Here is what I know about you: Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42.", "the user in the third person");
        assert_eq!(p.about_user("mình sống ở đâu?", "Bạn sống ở Hà Nội."), "Bạn sống ở Hà Nội.", "Hà Nội is Hanoi");
        // a question about the user answered with the assistant's own name, a bare word that is no value of theirs,
        // or the asked thing with another value: Elisa's answer from the facts (moondream:1.8b, after a model switch
        // into the saved conversation, answered "what did I tell you my name was?" with "Elisa")
        let own = ["Elisa".to_string(), "moondream:1.8b".to_string()];
        assert_eq!(p.recall_answer("what did I tell you my name was?", "Elisa", &own).as_deref(), Some("Your name is Linh."));
        assert_eq!(p.recall_answer("what's my name?", " Elisa.", &own).as_deref(), Some("Your name is Linh."));
        assert_eq!(p.recall_answer("what's my name?", "Unknown", &own).as_deref(), Some("Your name is Linh."));
        assert_eq!(p.recall_answer("what's my name?", "Your name is Minh.", &own).as_deref(), Some("Your name is Linh."));
        assert_eq!(p.recall_answer("where do I live?", "You live in Saigon.", &own).as_deref(), Some("You live in Hanoi."));
        assert_eq!(p.recall_answer("tên mình là gì?", "Elisa", &own).as_deref(), Some("Tên của bạn là Linh."));
        assert_eq!(p.recall_answer("what do you know about me?", "Elisa", &own).as_deref(), Some("Here is what I know about you: Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42."));
        // the fact given, in any words, stands; so does a reply to no question about them, or one saying more
        assert_eq!(p.recall_answer("what's my name?", "Linh", &own), None);
        assert_eq!(p.recall_answer("what did I tell you my name was?", "You told me your name is Linh.", &own), None);
        assert_eq!(p.recall_answer("where do I live?", "Hà Nội", &own), None);
        assert_eq!(p.recall_answer("tell me a short joke", "Elisa", &own), None);
        assert_eq!(p.recall_answer("what's my name?", "I don't have that information yet.", &own), None);
        assert_eq!(NameFix::new(&[]).recall_answer("what's my name?", "Elisa", &own), None);
        assert_eq!(p.about_user(ask_vi, "- **Tên**: Linh\n- **Mã cuộc họp**: ORCHID-42"), "- **Tên**: Linh\n- **Mã cuộc họp**: ORCHID-42");
        assert_eq!(p.about_user("bạn là ai?", "Mình là Elisa."), "Mình là Elisa.");
        assert_eq!(p.about_user("who are you?", "I am Elisa."), "I am Elisa.");
        let mail = NameFix::new(&["The user's email is linh@x.vn".into()]);
        assert_eq!(mail.about_user("can you help me write my email to the team.", "Sure, I can help."), "Sure, I can help.", "no question about it");
        assert_eq!(p.about_user("where do I live?", "I am an assistant."), "You live in Hanoi.");
        // no usable reply at all: the asked facts, or all of them, from Elisa's notes
        assert_eq!(p.answer_from_facts("What's my name and my meeting code?").as_deref(), Some("Your name is Linh. Your meeting code is ORCHID-42."));
        assert_eq!(p.answer_from_facts("tóm tắt những gì bạn biết về mình").as_deref(), Some("Những gì mình biết về bạn: Tên của bạn là Linh. Bạn sống ở Hanoi. Mã cuộc họp của bạn là ORCHID-42."));
        assert_eq!(p.answer_from_facts("what is the capital of Peru?"), None);
        assert_eq!(p.all_facts_said(false), ["Your name is Linh.", "You live in Hanoi.", "Your meeting code is ORCHID-42."]);
        // its own name and another fact of theirs, not the asked one: the asked fact
        assert_eq!(p.about_user("what is my name?", "Hello there! My name is Elisa. You live in Hanoi."), "Your name is Linh.");
        assert_eq!(p.about_user("what is my name?", "Your name is Linh. I'm glad you live in Hanoi."), "Your name is Linh. I'm glad you live in Hanoi.");
        assert_eq!(p.about_user("what is your name?", "My name is Elisa."), "My name is Elisa.", "about the assistant");
        let copy = "The quarterly report lists shipping volumes, warehouse counts and supplier names for every region.";
        assert_eq!(p.about_user("What's my name and where do I live?", copy), copy, "a copy, not the assistant about itself");
        // facts said again later in the same reply: said once
        assert_eq!(p.fix("Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42. Your name is Linh and the meeting code is ORCHID-42."),
            "Your name is Linh. You live in Hanoi. Your meeting code is ORCHID-42.");
        assert_eq!(p.fix("Your name is Linh. Nice to meet you, Linh!"), "Your name is Linh. Nice to meet you, Linh!");
        assert_eq!(p.fix("I told you my name is Linh, and you live in Hanoi."), "I told you my name is Linh, and you live in Hanoi.", "a claim is left for the retry");
        assert_eq!(p.fix("The user's name is Linh, they live in Hanoi, and their meeting code is ORCHID-42."),
            "Your name is Linh, you live in Hanoi, and your meeting code is ORCHID-42.");
        assert_eq!(p.fix("The user lives in Hanoi. The user's late for the meeting."), "You live in Hanoi. You're late for the meeting.");
        assert_eq!(p.fix("The user is from Hanoi."), "You are from Hanoi.");
        assert_eq!(p.fix("Tên của người dùng là Linh."), "Tên của bạn là Linh.");
        assert_eq!(p.fix("I have no personal information about the user."), "I have no personal information about you.");
        assert_eq!(NameFix::new(&[]).fix("When the user clicks Save, the file is written."), "When the user clicks Save, the file is written.", "no facts told");
        // an asked fact Elisa holds is in the reply; a template in place of the answer becomes the facts
        assert_eq!(p.answer("What's my name and my meeting code?", "My name is [Your Name] and my meeting code is [Your Meeting Code]."),
            "Your name is Linh. Your meeting code is ORCHID-42.");
        assert_eq!(p.answer("What's my name, where do I live and what is my meeting code?", "Linh"),
            "Linh. You live in Hanoi. Your meeting code is ORCHID-42.");
        assert_eq!(p.answer("What's my name?", "Your name is Linh."), "Your name is Linh.");
        assert_eq!(p.answer("what's the name of this file?", "It is notes.txt."), "It is notes.txt.", "not about the user");
        assert_eq!(p.answer("Where do I live?", "You live in Hanoi!"), "You live in Hanoi!");
        assert_eq!(p.answer("What's my name and my meeting code?", "Linh, ORCHID-42"), "Linh, ORCHID-42");
        assert_eq!(p.answer("my name is Linh and I live in Hanoi.", "Linh"), "Linh", "telling, not asking");
        let copy = "The quarterly report lists shipping volumes for every region in detail.";
        assert_eq!(p.answer("What's my name and where do I live?", copy), copy, "no answer at all: left for the copy check");
        assert_eq!(fact_lines("- The user lives in Hanoi.\n- The user's name is Linh"), ["The user lives in Hanoi", "The user's name is Linh"]);
        // someone else's "they" stays: parents, a friend, a pet
        let q = NameFix::new(&["The user's name is Linh".into(), "The user's dog is called Bo".into(), "The user lives in Hanoi".into()]);
        assert_eq!(q.fix("They live in Da Nang."), "They live in Da Nang.", "no user named in it");
        assert_eq!(q.fix("The user's parents live in Da Nang, where they run a shop."), "Your parents live in Da Nang, where they run a shop.");
        assert_eq!(q.fix("The user's dog Bo is lovely, and they love walks."), "Your dog Bo is lovely, and they love walks.");
        assert_eq!(q.fix("The user's friend Mai says they will visit Linh."), "Your friend Mai says they will visit Linh.");
        assert_eq!(q.fix("The user lives in Hanoi, and their name is Linh."), "You live in Hanoi, and your name is Linh.");
        assert_eq!(p.fix("Hi Linh! You live in Hanoi."), "Hi Linh! You live in Hanoi.");
        assert_eq!(f.fix("Hey Bo!"), "Hey Linh!");
        assert_eq!(f.fix("Welcome back, Bo."), "Welcome back, Linh.");
        assert_eq!(NameFix::new(&["The user lives in Hanoi".into()]).fix("Hey Hanoi!"), "Hey Hanoi!", "a place is no name");
    }

    #[test]
    fn a_sentence_that_only_says_a_fact_over_again_goes() {
        // (phi4-mini:3.8b after a model switch: the second sentence says where the user lives once more)
        let p = NameFix::new(&["The user's name is Linh".into(), "The user lives in Hanoi".into()]);
        assert_eq!(p.fix("Your name is Linh and you live in Hanoi. As for where you live, you are in the capital city of Vietnam, Hanoi."),
            "Your name is Linh and you live in Hanoi.");
        assert_eq!(p.fix("Tên của bạn là Linh. Nói cách khác, bạn sống ở Hanoi, bạn tên Linh."), "Tên của bạn là Linh. Nói cách khác, bạn sống ở Hanoi, bạn tên Linh.", "a fact not said before stays");
        assert_eq!(p.fix("Bạn sống ở Hanoi. Nói cách khác, bạn đang sống tại Hanoi."), "Bạn sống ở Hanoi.");
        // something new about a value, or a sentence with no restating lead-in, stays
        assert_eq!(p.fix("Your name is Linh. Your name, Linh, means 'soul' in Vietnamese."), "Your name is Linh. Your name, Linh, means 'soul' in Vietnamese.");
        assert_eq!(p.fix("You live in Hanoi. Hanoi is lovely in autumn, you should visit the lake."), "You live in Hanoi. Hanoi is lovely in autumn, you should visit the lake.");
        assert_eq!(p.fix("As for where you live, you are in Hanoi."), "As for where you live, you are in Hanoi.", "the first sentence always stays");
    }
}
