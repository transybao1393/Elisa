//! The tool-loop guard of one user turn: cheap, deterministic checks on the CALLS a model makes (not their results,
//! which can differ slightly every time: a web search, a directory listing with a timestamp).
//!
//! - **Repeated call.** The same tool with the same normalized input a third time in a turn does not run (an MCP
//!   tool's, such as `web_search`: a second time); the model gets "you already ran this" instead. A successful edit / write to a file resets the count of that file's reads and
//!   of the commands that can see any file (bash, grep, glob), so read → edit → read and edit → test → fix → test are
//!   never blocked.
//! - **Short cycle.** A window of 2–4 calls repeated back to back (A B A B, A B C A B C, …) with no successful change in
//!   between gets a note once; if the cycle comes round again after that note the turn stops.
//! - **Same file, same result.** `read` calls on one path with different paging arguments (`limit:4`, `offset:1`,
//!   none) are one call when they give the same result: a third read of the path after two identical results does not
//!   run. A read that failed whatever its arguments (a binary file, a directory, a missing file) is not run again on
//!   that path at all; the model gets the reason and what to use instead. Paging through a long file (different
//!   results) is never blocked. A change to the file resets this like the counts above.
//! - **Attached document.** A `read` / `outline` / `read_symbol` / `grep` of a document attached to this turn, whose
//!   text is already in the user's message, does not run; the model is told the text is above (otherwise a small
//!   model can read and outline attached documents until the turn stops with no reply).
//! - **No progress.** After `NUDGE_STEPS` tool steps without a successful change, one soft note asks the model to
//!   answer with what it has or say what blocks it.
use serde_json::Value;
use std::collections::HashMap;

/// the call that would be the Nth identical one in a turn does not run
pub const SAME_CALL_MAX: usize = 3;
/// the same for a tool that is not Elisa's own (an MCP server's: `web_search`, `web_fetch`): its second identical call
/// in a turn does not run, the first result is already in the context
pub const SAME_REMOTE_CALL_MAX: usize = 2;
/// steps in a turn whose calls were blocked as repeats, before the turn stops
pub const BLOCKED_STEPS_MAX: usize = 2;
/// tool steps without a successful change before the soft "answer now" note
pub const NUDGE_STEPS: usize = 10;
/// cycle periods looked for, and how many back-to-back rounds of each count as a loop
const PERIODS: [(usize, usize); 3] = [(2, 2), (3, 2), (4, 2)];

/// What the guard says about one call before it runs.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict { Run, Block(String) }

/// What the guard says after a step's calls ran.
#[derive(Debug, PartialEq, Eq)]
pub enum After { Fine, Note(String), Stop(String) }

#[derive(Default)]
pub struct Guard {
    /// how often each call key ran (or was blocked) this turn, and the file path it is about (None: any file)
    counts: HashMap<String, (usize, Option<String>, bool)>,
    /// the call keys since the last successful change, oldest first
    seq: Vec<String>,
    /// a cycle note was given: (its period, seq length then)
    cycle_noted: Option<(usize, usize)>,
    blocked_steps: usize,
    blocked_now: bool,
    /// tool steps since the last successful change, and whether that streak already had its note
    idle_steps: usize,
    nudged: bool,
    /// a successful change in the step in progress
    changed_now: bool,
    /// per path read with `read`: its last result, how many reads in a row gave it, and whether it was an error no
    /// paging argument changes (binary file, directory, missing)
    reads: HashMap<String, ReadSeen>,
    /// the file names of the documents whose text is in this turn's message (`attached`)
    attached: Vec<String>,
}

struct ReadSeen { last: String, same: usize, unreadable: bool }

/// the tools whose calls on one path differ only in paging (`offset` / `limit`)
fn paged_read(name: &str) -> bool { name == "read" }

/// `name` + its input with object keys sorted, strings trimmed and inner whitespace collapsed (a search query also
/// lowercased), nulls dropped: two calls that mean the same thing get the same key.
pub fn key(name: &str, input: &Value) -> String {
    fn norm(v: &Value, lower: bool) -> Value {
        match v {
            Value::String(s) => {
                let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                Value::String(if lower { s.to_lowercase() } else { s })
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| norm(x, lower)).collect()),
            Value::Object(m) => {
                let mut ks: Vec<&String> = m.keys().filter(|k| !m[*k].is_null()).collect();
                ks.sort();
                Value::Object(ks.into_iter().map(|k| (k.clone(), norm(&m[k], lower || k == "query" || k == "q"))).collect())
            }
            x => x.clone(),
        }
    }
    format!("{name}\u{0}{}", norm(input, false))
}

/// The file a call is about (`path`), when it names one; None for a call that can see any file.
fn path_of(input: &Value) -> Option<String> {
    input.get("path").and_then(Value::as_str).map(|p| p.trim().trim_start_matches("./").to_string()).filter(|p| !p.is_empty() && p != ".")
}

impl Guard {
    /// The documents whose text is in this turn's message: a file call on one of them is not run (`check`).
    pub fn attached(&mut self, names: Vec<String>) { self.attached = names.into_iter().map(|n| n.to_lowercase()).collect(); }

    /// A file call of Elisa's own on a document attached to this turn: the note that its text is already there.
    fn attached_doc(&self, name: &str, input: &Value, local: bool) -> Option<String> {
        if !local || !matches!(name, "read" | "outline" | "read_symbol" | "grep") { return None; }
        let p = path_of(input)?;
        let file = p.rsplit(['/', '\\']).next().unwrap_or(&p).to_lowercase();
        self.attached.contains(&file).then(|| format!("[elisa: not run: {p} is a document attached to this message: Elisa \
            already extracted its text and put it in the user's message above. Do not open it with a tool; answer the user \
            from that text now.]"))
    }

    /// Before a call runs: Block when this exact call already ran `SAME_CALL_MAX - 1` times in this turn (since the
    /// last change to what it reads). `local` = one of Elisa's own tools (an edit can change what it returns).
    pub fn check(&mut self, name: &str, input: &Value, local: bool) -> Verdict {
        if let Some(why) = self.attached_doc(name, input, local).or_else(|| self.read_repeat(name, input, local)) {
            self.seq.push(key(name, input));
            self.blocked_now = true;
            return Verdict::Block(why);
        }
        let k = key(name, input);
        let e = self.counts.entry(k.clone()).or_insert((0, path_of(input), local));
        e.0 += 1;
        self.seq.push(k);
        // an MCP tool (a web search, a page fetch) reads nothing Elisa's edits change: its earlier result stands
        if e.0 < if local { SAME_CALL_MAX } else { SAME_REMOTE_CALL_MAX } { return Verdict::Run; }
        self.blocked_now = true;
        if !local {
            return Verdict::Block("[elisa: not run: this exact call already ran in this turn; its result is above and would be \
                the same. Answer from that result, or try a different call.]".into());
        }
        Verdict::Block(format!("[elisa: not run: this exact call already ran {} times in this turn and nothing it reads has changed since. \
            Use the earlier result, change the approach, or answer with what you have.]", e.0 - 1))
    }

    /// A read of a path that cannot give anything new whatever its paging arguments: the reason, else None.
    fn read_repeat(&self, name: &str, input: &Value, local: bool) -> Option<String> {
        if !local || !paged_read(name) { return None; }
        let p = path_of(input)?;
        let r = self.reads.get(&p)?;
        if r.unreadable {
            let first = r.last.lines().next().unwrap_or("").trim();
            return Some(format!("[elisa: not run: reading {p} already failed and no offset / limit changes that: {first} \
                Do not read it again; use another tool or answer with what you have.]"));
        }
        (r.same + 1 >= SAME_CALL_MAX).then(|| format!("[elisa: not run: the last {} reads of {p} returned the same text \
            (only offset / limit differed) and the file has not changed. Use that result, read another part with an \
            offset past it, or answer with what you have.]", r.same))
    }

    /// After a call ran, its result (`content` as the model gets it): keeps what each `read` path returned, so a read
    /// with other paging arguments that can only give the same again is blocked by `check`.
    pub fn result(&mut self, name: &str, input: &Value, content: &str, is_error: bool) {
        if !paged_read(name) { return; }
        let Some(p) = path_of(input) else { return };
        // an offset past the end depends on the arguments; every other read error (binary, directory, missing) not
        let unreadable = is_error && !content.starts_with("offset ");
        let r = self.reads.entry(p).or_insert(ReadSeen { last: String::new(), same: 0, unreadable: false });
        if r.same > 0 && r.last == content { r.same += 1; } else { r.last = content.to_string(); r.same = 1; }
        r.unreadable = unreadable;
    }

    /// After a call ran: `write` = it changes files (edit / write), `ok` = it succeeded, `changed` = it really changed
    /// something (an edit whose old and new text are the same did not). A real change resets the counts of the calls
    /// that read that file or any file, and the cycle and no-progress streaks. A shell command run for the first time
    /// may have changed any file (`sed -i`, `cat > f`): it resets the counts of Elisa's own reads, nothing else.
    pub fn ran(&mut self, name: &str, input: &Value, write: bool, ok: bool, changed: bool) {
        if name == "bash" && ok && self.counts.get(&key(name, input)).is_some_and(|c| c.0 == 1) {
            let k = key(name, input);
            self.counts.retain(|kk, (_, _, local)| !*local || *kk == k);
            self.reads.clear();
        }
        if !(write && ok && changed) { return; }
        self.changed_now = true;
        let p = path_of(input);
        self.counts.retain(|_, (_, path, local)| !(*local && (path.is_none() || *path == p)));
        match &p { Some(p) => { self.reads.remove(p); } None => self.reads.clear() }
        self.seq.clear();
        self.cycle_noted = None;
    }

    /// After a step's calls ran (and `ran` was called for each): a note to add to the step's last result, the turn's
    /// stop, or nothing.
    pub fn step_done(&mut self) -> After {
        if std::mem::take(&mut self.blocked_now) { self.blocked_steps += 1; }
        if std::mem::take(&mut self.changed_now) { (self.idle_steps, self.nudged) = (0, false); } else { self.idle_steps += 1; }
        if self.blocked_steps >= BLOCKED_STEPS_MAX {
            return After::Stop("the model kept repeating calls that already ran".into());
        }
        if let Some(p) = self.cycle() {
            match self.cycle_noted {
                Some((_, at)) if self.seq.len() >= at + p => {
                    return After::Stop(format!("the model kept cycling through the same {p} calls"));
                }
                Some(_) => {}
                None => {
                    self.cycle_noted = Some((p, self.seq.len()));
                    return After::Note(format!("[elisa: you are repeating the same {p} calls in a loop and their results will not change. \
                        Use what you already have and answer, or try something different.]"));
                }
            }
        }
        if self.idle_steps >= NUDGE_STEPS && !self.nudged {
            self.nudged = true;
            return After::Note(format!("[elisa: {} tool steps without a change. If you have enough to answer, answer now; \
                otherwise say what blocks you.]", self.idle_steps));
        }
        After::Fine
    }

    /// The period of a cycle at the end of the call sequence (a window of 2–4 calls repeated back to back).
    fn cycle(&self) -> Option<usize> {
        let s = &self.seq;
        PERIODS.iter().find(|&&(p, rounds)| {
            let n = p * rounds;
            // a window of one call repeated is a repeat, not a cycle (`check` handles it)
            s.len() >= n && (p..n).all(|i| s[s.len() - 1 - i] == s[s.len() - 1 - i + p])
                && s[s.len() - p..].iter().any(|k| *k != s[s.len() - 1])
        }).map(|&(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// one step of one call: the verdict, then (for a call that ran) its result
    fn step(g: &mut Guard, name: &str, input: Value, write: bool) -> (Verdict, After) {
        let v = g.check(name, &input, true);
        if v == Verdict::Run { g.ran(name, &input, write, true, true); }
        (v, g.step_done())
    }

    #[test]
    fn keys_ignore_key_order_spacing_and_query_case() {
        assert_eq!(key("grep", &json!({"pattern": "fn  main", "path": "src"})), key("grep", &json!({"path": "src ", "pattern": "fn main"})));
        assert_eq!(key("web_search", &json!({"query": "Rust Latest release"})), key("web_search", &json!({"query": "rust latest  release"})));
        assert_ne!(key("grep", &json!({"pattern": "Main"})), key("grep", &json!({"pattern": "main"})));
        assert_eq!(key("read", &json!({"path": "a", "offset": null})), key("read", &json!({"path": "a"})));
    }

    #[test]
    fn the_third_identical_call_does_not_run_even_with_different_results() {
        let mut g = Guard::default();
        let q = json!({"query": "rust programming language latest release"});
        assert_eq!(step(&mut g, "web_search", q.clone(), false).0, Verdict::Run);
        assert_eq!(step(&mut g, "web_search", q.clone(), false).0, Verdict::Run);
        let (v, a) = step(&mut g, "web_search", q.clone(), false);
        assert!(matches!(v, Verdict::Block(ref m) if m.contains("already ran 2 times")), "{v:?}");
        assert_eq!(a, After::Fine);
        // still at it: the turn stops
        assert!(matches!(step(&mut g, "web_search", q, false).1, After::Stop(_)));
    }

    #[test]
    fn an_mcp_call_repeated_does_not_run_the_second_time() {
        // a small model can run web_search again and again with one query
        let mut g = Guard::default();
        let q = json!({"query": "what's the weather in Hanoi today"});
        assert_eq!(g.check("web_search", &q, false), Verdict::Run);
        g.step_done();
        let v = g.check("web_search", &json!({"query": "What's the weather in  Hanoi today"}), false);
        assert!(matches!(v, Verdict::Block(ref m) if m.contains("result is above")), "{v:?}");
        assert_eq!(g.step_done(), After::Fine);
        // another query still runs
        assert_eq!(g.check("web_search", &json!({"query": "Hanoi forecast"}), false), Verdict::Run);
    }

    #[test]
    fn a_change_to_the_file_resets_its_reads_and_the_commands() {
        let mut g = Guard::default();
        let read = json!({"path": "calc.py"});
        let other = json!({"path": "other.py"});
        for _ in 0..2 { step(&mut g, "read", read.clone(), false); step(&mut g, "read", other.clone(), false); }
        step(&mut g, "edit", json!({"path": "calc.py", "old": "a", "new": "b"}), true);
        assert_eq!(step(&mut g, "read", read.clone(), false).0, Verdict::Run, "calc.py changed: read it again");
        assert!(matches!(step(&mut g, "read", other, false).0, Verdict::Block(_)), "other.py did not change");
        // an edit whose text did not change is no change
        let mut g = Guard::default();
        for _ in 0..2 { step(&mut g, "read", read.clone(), false); }
        let e = json!({"path": "calc.py", "old": "a", "new": "a"});
        assert_eq!(g.check("edit", &e, true), Verdict::Run);
        g.ran("edit", &e, true, true, false);
        g.step_done();
        assert!(matches!(g.check("read", &read, true), Verdict::Block(_)));
        // a new shell command may have changed the file; the same command again did not
        let mut g = Guard::default();
        for _ in 0..2 { step(&mut g, "read", read.clone(), false); }
        step(&mut g, "bash", json!({"command": "sed -i s/a/b/ calc.py"}), false);
        assert_eq!(step(&mut g, "read", read.clone(), false).0, Verdict::Run);
        step(&mut g, "bash", json!({"command": "sed -i s/a/b/ calc.py"}), false);
        assert_eq!(step(&mut g, "read", read.clone(), false).0, Verdict::Run);
        assert!(matches!(step(&mut g, "read", read, false).0, Verdict::Block(_)));
    }

    #[test]
    fn an_edit_test_fix_loop_is_never_stopped() {
        let mut g = Guard::default();
        let test = json!({"command": "python3 -m pytest"});
        for i in 0..12 {
            let (v, a) = step(&mut g, "bash", test.clone(), false);
            assert_eq!((v, a), (Verdict::Run, After::Fine), "test run {i}");
            let (v, a) = step(&mut g, "read", json!({"path": "calc.py"}), false);
            assert_eq!((v, a), (Verdict::Run, After::Fine));
            let (v, a) = step(&mut g, "edit", json!({"path": "calc.py", "old": format!("v{i}"), "new": format!("v{}", i + 1)}), true);
            assert_eq!((v, a), (Verdict::Run, After::Fine));
        }
    }

    #[test]
    fn a_cycle_of_four_gets_a_note_then_stops() {
        // files s4: outline → glob → read README.md → read inside.txt, round and round
        let mut g = Guard::default();
        let round = [("outline", json!({"path": "projD"})), ("glob", json!({"pattern": "**/*"})), ("read", json!({"path": "README.md"})), ("read", json!({"path": "inside.txt"}))];
        let mut afters = Vec::new();
        for _ in 0..3 { for (n, i) in &round { afters.push(step(&mut g, n, i.clone(), false).1); } }
        let note = afters.iter().position(|a| matches!(a, After::Note(m) if m.contains("same 4 calls"))).expect("a note");
        assert_eq!(note, 7, "after the second round: {afters:?}");
        assert!(afters.iter().any(|a| matches!(a, After::Stop(_))), "{afters:?}");
    }

    #[test]
    fn a_cycle_of_three_and_of_two() {
        // chat s3: grep → read → think(no thought)
        let mut g = Guard::default();
        let round = [("grep", json!({"pattern": "def average"})), ("read", json!({"path": "calc.py"})), ("think", json!({}))];
        let mut afters = Vec::new();
        for _ in 0..2 { for (n, i) in &round { afters.push(step(&mut g, n, i.clone(), false).1); } }
        assert!(matches!(afters.last(), Some(After::Note(m)) if m.contains("same 3 calls")), "{afters:?}");
        // web_fetch over two URLs: A B A B A B
        let mut g = Guard::default();
        let (a, b) = (json!({"url": "https://a.example"}), json!({"url": "https://b.example"}));
        let mut afters = Vec::new();
        for _ in 0..3 { afters.push(step(&mut g, "web_fetch", a.clone(), false).1); afters.push(step(&mut g, "web_fetch", b.clone(), false).1); }
        assert!(matches!(&afters[3], After::Note(m) if m.contains("same 2 calls")), "{afters:?}");
        assert!(matches!(afters.last(), Some(After::Stop(_))), "{afters:?}");
        // two different calls once each, then others: no cycle
        let mut g = Guard::default();
        for p in ["a", "b", "a", "c", "b", "d"] { assert_eq!(step(&mut g, "read", json!({"path": p}), false).1, After::Fine); }
    }

    /// one step of one call whose result is `out`
    fn step_out(g: &mut Guard, name: &str, input: Value, out: &str, err: bool) -> (Verdict, After) {
        let v = g.check(name, &input, true);
        if v == Verdict::Run { g.ran(name, &input, false, !err, true); g.result(name, &input, out, err); }
        (v, g.step_done())
    }

    #[test]
    fn a_binary_file_is_read_once_whatever_the_paging() {
        // retest N5: read revenue.xlsx ran ~6 times as {path}, {limit:4}, {limit:400}, {offset:1}
        let mut g = Guard::default();
        let err = "revenue.xlsx is a binary file: read shows text only, and no offset / limit changes that. Use docs_sheet …";
        assert_eq!(step_out(&mut g, "read", json!({"path": "revenue.xlsx"}), err, true).0, Verdict::Run);
        let (v, a) = step_out(&mut g, "read", json!({"path": "revenue.xlsx", "limit": 4}), err, true);
        assert!(matches!(v, Verdict::Block(ref m) if m.contains("already failed") && m.contains("docs_sheet")), "{v:?}");
        assert_eq!(a, After::Fine);
        let (v, a) = step_out(&mut g, "read", json!({"path": "./revenue.xlsx", "offset": 1}), err, true);
        assert!(matches!(v, Verdict::Block(_)));
        assert!(matches!(a, After::Stop(_)), "{a:?}");
        // other files still read; a write to the file lets it be read again
        let mut g = Guard::default();
        step_out(&mut g, "read", json!({"path": "a.bin"}), err, true);
        assert_eq!(step_out(&mut g, "read", json!({"path": "b.txt"}), "1\tb", false).0, Verdict::Run);
        step(&mut g, "write", json!({"path": "a.bin", "content": "text"}), true);
        assert_eq!(step_out(&mut g, "read", json!({"path": "a.bin", "limit": 4}), "1\ttext", false).0, Verdict::Run);
    }

    #[test]
    fn same_result_with_other_paging_counts_as_a_repeat_but_paging_does_not() {
        let mut g = Guard::default();
        let all = "1\ta\n2\tb";
        assert_eq!(step_out(&mut g, "read", json!({"path": "s.py"}), all, false).0, Verdict::Run);
        assert_eq!(step_out(&mut g, "read", json!({"path": "s.py", "offset": 1}), all, false).0, Verdict::Run);
        assert!(matches!(step_out(&mut g, "read", json!({"path": "s.py", "limit": 400}), all, false).0, Verdict::Block(ref m) if m.contains("same text")));
        // paging a long file: every page differs, never blocked
        let mut g = Guard::default();
        for o in (1..4000).step_by(400) {
            let (v, a) = step_out(&mut g, "read", json!({"path": "long.txt", "offset": o}), &format!("{o}\tline {o}"), false);
            assert_eq!(v, Verdict::Run, "offset {o}");
            assert!(!matches!(a, After::Stop(_)));
        }
        // an offset past the end is not "unreadable": another offset may work
        let mut g = Guard::default();
        step_out(&mut g, "read", json!({"path": "s.py", "offset": 99}), "offset 99 is past the end (2 lines)", true);
        assert_eq!(step_out(&mut g, "read", json!({"path": "s.py"}), all, false).0, Verdict::Run);
    }

    #[test]
    fn a_file_call_on_an_attached_document_says_the_text_is_already_there() {
        // with sales.xlsx + plan.ods attached, a model may still call outline / read on them
        let mut g = Guard::default();
        g.attached(vec!["sales.xlsx".into(), "Plan.ods".into()]);
        let (v, a) = step(&mut g, "read", json!({"path": "sales.xlsx"}), false);
        assert!(matches!(v, Verdict::Block(ref m) if m.contains("attached to this message") && m.contains("answer the user")), "{v:?}");
        assert_eq!(a, After::Fine);
        assert!(matches!(step(&mut g, "outline", json!({"path": ".elisa/uploads/7/plan.ods"}), false).0, Verdict::Block(_)), "by file name, any case");
        // other files, and MCP tools, still run
        let mut g = Guard::default();
        g.attached(vec!["sales.xlsx".into()]);
        assert_eq!(step(&mut g, "read", json!({"path": "notes.txt"}), false).0, Verdict::Run);
        assert_eq!(g.check("docs_sheet", &json!({"path": "sales.xlsx"}), false), Verdict::Run);
        // no attachment: a read runs
        assert_eq!(step(&mut Guard::default(), "read", json!({"path": "sales.xlsx"}), false).0, Verdict::Run);
    }

    #[test]
    fn many_steps_without_a_change_get_one_soft_note() {
        let mut g = Guard::default();
        let mut notes = 0;
        for i in 0..25 {
            if let (_, After::Note(m)) = step(&mut g, "web_search", json!({"query": format!("topic {i}")}), false) {
                assert!(m.contains("10 tool steps"), "{m}");
                notes += 1;
            }
        }
        assert_eq!(notes, 1);
        // a change starts the count again
        step(&mut g, "write", json!({"path": "out.md", "content": "x"}), true);
        let n = (0..NUDGE_STEPS).filter(|i| matches!(step(&mut g, "read", json!({"path": format!("f{i}")}), false).1, After::Note(_))).count();
        assert_eq!(n, 1);
    }
}
