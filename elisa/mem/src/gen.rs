//! Deterministic synthetic project conversation for the elisa-mem bench.
//! 30 sessions of a coding agent building "quill" (a log search CLI), 20 planted
//! facts (some changed later), fold notes every ~6 turns, a final note per session.

pub const SESSIONS: usize = 30;
pub const PROJECT: &str = "quill";

/// xorshift64*: tiny, deterministic.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng { Rng(seed.max(1)) }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12; x ^= x << 25; x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str { xs[self.below(xs.len())] }
}

/// One conversation event. `Note` folds every turn since the previous note; `Card` is a small state card
/// written right after each note (chained to the previous card, linked to the turns since the last card).
pub enum Ev { Turn { user: bool, text: String }, Note(String), Card(String) }

/// A detail stated ONLY in one early state card (session 2's first card), never in turns or notes.
pub const CARD_ONLY: &str = "s3://quill-staging-eu7";
pub fn card_question() -> Question {
    Question { text: "Which bucket does the staging environment upload to?", fact: "staging bucket (card only)", want: CARD_ONLY, reject: vec![], reworded: false }
}
pub struct Session { pub evs: Vec<Ev> }

pub struct Fact {
    pub name: &'static str,
    /// (1-based session, value); last entry is the answer.
    pub vals: &'static [(usize, &'static str)],
    /// Planted sentence; `{}` is replaced by the value.
    pub tpl: &'static str,
    pub q: &'static str,
    pub q_reworded: &'static str,
}

pub const FACTS: &[Fact] = &[
    Fact { name: "port", vals: &[(3, "8080"), (19, "9090")], tpl: "the dev server port is {}",
        q: "What is the dev server port?", q_reworded: "Which number does the local backend listen on?" },
    Fact { name: "config", vals: &[(2, "quill-tool.toml"), (7, "quill.toml"), (22, "quill.config.toml")], tpl: "the config file is called {}",
        q: "What is the config file called?", q_reworded: "What is the settings file named now?" },
    Fact { name: "ram", vals: &[(5, "512 MB"), (16, "768 MB")], tpl: "the RAM cap for the indexer is {}",
        q: "What is the RAM cap for the indexer?", q_reworded: "How much memory may the indexing process use at most?" },
    Fact { name: "jsonlib", vals: &[(4, "serde_json"), (13, "sonic-rs")], tpl: "we chose {} as the JSON parser library",
        q: "Which JSON parser library did we choose?", q_reworded: "Which library do we decode payloads with?" },
    Fact { name: "rootcause", vals: &[(8, "an off-by-one in the ring buffer wraparound"), (14, "a stale mmap after truncate")],
        tpl: "the root cause of the flaky crash is {}",
        q: "What is the root cause of the flaky crash?", q_reworded: "Why did the intermittent segfault happen?" },
    Fact { name: "deadline", vals: &[(1, "November 14"), (17, "December 5")], tpl: "the release deadline is {}",
        q: "When is the release deadline?", q_reworded: "By what date must the release ship?" },
    Fact { name: "reviewer", vals: &[(6, "Morgan Ellis"), (24, "Riley Chen")], tpl: "{} owns code review for the parser",
        q: "Who owns code review for the parser?", q_reworded: "Which person approves pull requests touching parsing?" },
    Fact { name: "storage", vals: &[(9, "SQLite"), (21, "redb")], tpl: "the storage backend decision is {}",
        q: "What is the storage backend decision?", q_reworded: "Which database backend keeps the data?" },
    Fact { name: "logfmt", vals: &[(10, "logfmt"), (25, "ndjson")], tpl: "the log output format is {}",
        q: "What is the log output format?", q_reworded: "How are emitted diagnostic lines encoded?" },
    Fact { name: "ci", vals: &[(11, "Buildkite")], tpl: "CI runs on {}",
        q: "Where does CI run?", q_reworded: "Which service runs our continuous integration pipeline?" },
    Fact { name: "authvar", vals: &[(12, "QUILL_TOKEN"), (26, "QUILL_API_KEY")], tpl: "the auth env var is {}",
        q: "What is the auth env var?", q_reworded: "Which environment variable carries the credential?" },
    Fact { name: "batch", vals: &[(4, "64 items"), (23, "256 items")], tpl: "the max batch size is {}",
        q: "What is the max batch size?", q_reworded: "What is the biggest chunk size we allow?" },
    Fact { name: "timeout", vals: &[(13, "30 s"), (27, "45 s")], tpl: "the request timeout is {}",
        q: "What is the request timeout?", q_reworded: "How long before a call gives up waiting?" },
    Fact { name: "msrv", vals: &[(2, "Rust 1.74"), (20, "Rust 1.79")], tpl: "the MSRV is {}",
        q: "What is the MSRV?", q_reworded: "Which Rust compiler release is the oldest we support?" },
    Fact { name: "license", vals: &[(3, "Apache-2.0"), (15, "MPL-2.0")], tpl: "we license the project under {}",
        q: "Which license is the project under?", q_reworded: "Under what terms is the code distributed?" },
    Fact { name: "branch", vals: &[(5, "trunk")], tpl: "the default branch is {}",
        q: "What is the default branch?", q_reworded: "Which git line do we merge changes into by default?" },
    Fact { name: "cachedir", vals: &[(9, "~/.cache/quill"), (28, "/var/tmp/quill-cache")], tpl: "the cache directory is {}",
        q: "What is the cache directory?", q_reworded: "Where are temporary artifacts stored on disk?" },
    Fact { name: "oncall", vals: &[(7, "Sam Okafor")], tpl: "{} is on call for the release",
        q: "Who is on call for the release?", q_reworded: "Which teammate handles incidents during the release?" },
    Fact { name: "hash", vals: &[(12, "xxh3"), (24, "blake3")], tpl: "the content hash is {}",
        q: "What is the content hash?", q_reworded: "What digest function fingerprints the files?" },
    Fact { name: "pool", vals: &[(18, "12 workers"), (29, "6 workers")], tpl: "the worker pool size is {}",
        q: "What is the worker pool size?", q_reworded: "What size is the thread scheduler set to?" },
];

/// Fact planted only in the final session: asked while that session is still in the hot log.
pub static HOT_FACT: Fact = Fact { name: "codename", vals: &[(30, "Larkspur")], tpl: "the release codename is {}",
    q: "What is the release codename?", q_reworded: "What do we call the release internally?" };

/// The `Also known as:` group a model would write for each fact (FACTS order, then HOT_FACT); `{}` = current value.
pub const AKA: &[&str] = &[
    "dev server port = local backend port = listen port",
    "config file = settings file = {}",
    "RAM cap = memory limit = indexer memory budget",
    "JSON parser = JSON library = decode payloads",
    "flaky crash = intermittent segfault = crash bug",
    "release deadline = ship date = due date",
    "parser reviewer = code review owner = PR approver",
    "storage backend = database = persistence layer",
    "log output format = log encoding = diagnostic output",
    "CI = continuous integration = build pipeline",
    "auth env var = API credential = access token variable",
    "max batch size = chunk size = batch limit",
    "request timeout = call deadline = wait limit",
    "MSRV = minimum supported Rust version = oldest supported compiler",
    "license = distribution terms = {}",
    "default branch = git line = merge target",
    "cache directory = temp artifacts dir = {}",
    "on call = incidents = pager duty",
    "content hash = file digest = fingerprint",
    "worker pool = thread pool = scheduler threads",
    "release codename = release name",
];

/// All planted facts: FACTS then HOT_FACT (index FACTS.len()).
pub fn all_facts() -> impl Iterator<Item = &'static Fact> { FACTS.iter().chain(std::iter::once(&HOT_FACT)) }

pub fn fact_sentence(f: &Fact, v: &str) -> String { f.tpl.replacen("{}", v, 1) }

pub struct Question {
    pub text: &'static str,
    pub fact: &'static str,
    pub want: &'static str,
    pub reject: Vec<&'static str>,
    pub reworded: bool,
}
impl Question {
    /// Hit text names the latest value and none of the outdated ones.
    pub fn matches(&self, text: &str) -> bool {
        let t = text.to_lowercase();
        t.contains(&self.want.to_lowercase()) && !self.reject.iter().any(|r| t.contains(&r.to_lowercase()))
    }
}

/// 20 questions: even-indexed facts use planted wording, odd-indexed are reworded
/// (rare key words replaced by synonyms; one common anchor word kept, since recall without any shared term needs embeddings).
/// The question about HOT_FACT (planted wording).
pub fn hot_question() -> Question {
    Question { text: HOT_FACT.q, fact: HOT_FACT.name, want: HOT_FACT.vals[0].1, reject: vec![], reworded: false }
}

/// Reworded questions sharing NO content word with the planted sentence or its alias line
/// (only an embedding can bridge them). (fact name, question)
pub const SEMANTIC: &[(&str, &str)] = &[
    ("rootcause", "Why did the program die at random?"),
    ("hash", "Which checksum algorithm did we settle on?"),
    ("license", "Which open-source licence did we pick?"),
    ("timeout", "How many seconds until a stuck network fetch is abandoned?"),
    ("deadline", "When do we have to be finished?"),
];
pub fn semantic_questions() -> Vec<Question> {
    SEMANTIC.iter().map(|&(name, q)| {
        let f = FACTS.iter().find(|f| f.name == name).expect("semantic fact");
        let n = f.vals.len();
        Question { text: q, fact: f.name, want: f.vals[n - 1].1, reject: f.vals[..n - 1].iter().map(|v| v.1).collect(), reworded: true }
    }).collect()
}

pub fn questions() -> Vec<Question> {
    FACTS.iter().enumerate().map(|(i, f)| {
        let n = f.vals.len();
        Question {
            text: if i % 2 == 0 { f.q } else { f.q_reworded },
            fact: f.name,
            want: f.vals[n - 1].1,
            reject: f.vals[..n - 1].iter().map(|v| v.1).collect(),
            reworded: i % 2 == 1,
        }
    }).collect()
}

const FILES: &[&str] = &["src/main.rs", "src/scan.rs", "src/index.rs", "src/query.rs", "src/render.rs", "src/cli.rs",
    "src/watch.rs", "src/shard.rs", "src/codec.rs", "src/cache_layer.rs", "tests/e2e.rs", "benches/scan_bench.rs"];
const FNS: &[&str] = &["scan_dir", "build_index", "parse_line", "merge_shards", "render_table", "tokenize", "flush_segment",
    "open_reader", "match_glob", "decode_frame", "rebuild_postings", "spawn_watcher"];
const THINGS: &[&str] = &["lifetime on the reader", "error handling", "glob matching", "unicode column widths", "shard merge order",
    "retry loop", "progress bar", "postings encoding", "regex cache", "file watcher debounce"];
const FLAGS: &[&str] = &["follow", "since", "json-out", "no-color", "max-hits", "case-insensitive", "context", "dry-run"];
const ERRS: &[(&str, &str)] = &[("E0308", "mismatched types"), ("E0502", "cannot borrow `buf` as mutable because it is also borrowed as immutable"),
    ("E0425", "cannot find value `offset` in this scope"), ("E0599", "no method named `as_bytes` found for struct `Frame`"),
    ("E0382", "borrow of moved value: `path`"), ("E0277", "the trait bound `Shard: Send` is not satisfied")];
const TOPICS: &[&str] = &["the scanner", "index layout", "query planner", "terminal rendering", "CLI flags", "file watching",
    "shard merging", "frame codec", "the cache layer", "end-to-end tests"];

fn filler_user(r: &mut Rng) -> String {
    let f = r.pick(FILES); let func = r.pick(FNS);
    match r.below(10) {
        0 => format!("can you look at {f}? the {} looks wrong", r.pick(THINGS)),
        1 => "run the tests again please".into(),
        2 => format!("why is `{func}` so slow on the big fixture? it used to finish in {}.{:02}s", r.below(4), r.below(100)),
        3 => format!("add a --{} flag to the cli, wire it through {f}", r.pick(FLAGS)),
        4 => "the build is broken after your last change".into(),
        5 => format!("refactor `{func}` into its own module, it's {} lines now", 80 + r.below(300)),
        6 => "ok looks good, commit it with a short message".into(),
        7 => { let (c, m) = ERRS[r.below(ERRS.len())]; format!("what does this mean: error[{c}]: {m}") }
        8 => format!("let's clean up the {} before moving on", r.pick(THINGS)),
        _ => format!("open {f} and explain what `{func}` does"),
    }
}

fn filler_assistant(r: &mut Rng) -> String {
    let f = r.pick(FILES); let func = r.pick(FNS);
    match r.below(8) {
        0 => format!("I'll read {f}.\n```rust\nfn {func}(buf: &mut Vec<u8>, off: usize) -> io::Result<usize> {{\n    let n = buf.len().saturating_sub(off);\n    Ok(n)\n}}\n```\nThe issue is that `{func}` clones the buffer on every call; I'll pass a slice instead."),
        1 => { let n = 20 + r.below(140); format!("$ cargo test\nrunning {n} tests\ntest {}::tests::roundtrip ... ok\ntest {}::tests::empty_input ... ok\ntest result: ok. {n} passed; 0 failed; 0 ignored; finished in {}.{:02}s",
            func, r.pick(FNS), r.below(9), r.below(100)) }
        2 => { let (c, m) = ERRS[r.below(ERRS.len())]; let l = 10 + r.below(400);
            format!("$ cargo build\nerror[{c}]: {m}\n --> {f}:{l}:{}\n   |\n{l} |     let x = {func}(&mut buf, pos)?;\n   |             ^^^^ \n\nFixing it by cloning the path before the move.", 5 + r.below(40)) }
        3 => format!("Benchmark: `{func}` took {}.{:02}ms over {} iterations (p99 {}.{:02}ms). Most time is in memchr.",
            r.below(30), r.below(100), 1000 * (1 + r.below(9)), r.below(60), r.below(100)),
        4 => format!("Done. Changed {} lines in {f}; clippy is clean and rustfmt passes.", 3 + r.below(90)),
        5 => format!("$ rg -n \"{func}\" src\n{f}:{}:pub fn {func}(\nsrc/cli.rs:{}:    {func}(&opts)?;\n\nTwo call sites; I'll update both.", r.below(300), r.below(200)),
        6 => format!("Committed {:07x}: \"{} {}\". {} files changed, {} insertions(+), {} deletions(-).",
            r.next() & 0xfff_ffff, r.pick(&["fix", "refactor", "speed up", "clean up"]), r.pick(THINGS), 1 + r.below(6), r.below(120), r.below(60)),
        _ => format!("The {} in {f} was holding a lock across an await; I moved the guard into a block so it drops before `{func}` runs.", r.pick(THINGS)),
    }
}

/// A single filler turn (used by the crash child).
pub fn filler(r: &mut Rng, user: bool) -> String { if user { filler_user(r) } else { filler_assistant(r) } }

/// "Facts: ..." plus, when any fact is known, an "Also known as: ..." line for the facts it mentions.
fn facts_section(known: &[Option<&str>]) -> String {
    let parts: Vec<String> = all_facts().zip(known).filter_map(|(f, v)| v.map(|v| fact_sentence(f, v))).collect();
    if parts.is_empty() { return "Facts: none yet.".into() }
    let aka: Vec<String> = AKA.iter().zip(known).filter_map(|(a, v)| v.map(|v| a.replacen("{}", v, 1))).collect();
    format!("Facts: {}.\nAlso known as: {}.", parts.join("; "), aka.join("; "))
}

/// Build the whole conversation from `seed`.
pub fn conversation(seed: u64) -> Vec<Session> {
    let mut r = Rng::new(seed);
    let mut known: Vec<Option<&str>> = vec![None; FACTS.len() + 1];
    let facts: Vec<&Fact> = all_facts().collect();
    let mut out = Vec::with_capacity(SESSIONS);
    for s in 1..=SESSIONS {
        let pairs = 9 + r.below(3); // 18..22 turns
        let mut turns: Vec<(bool, String, Option<usize>)> = Vec::new();
        for _ in 0..pairs { turns.push((true, filler_user(&mut r), None)); turns.push((false, filler_assistant(&mut r), None)); }
        // plant this session's facts at distinct user turns
        let mut slot = 1 + r.below(2);
        for (fi, f) in facts.iter().enumerate() {
            for &(fs, v) in f.vals { if fs == s {
                let sent = fact_sentence(f, v);
                let lead = r.pick(&["ok, decision:", "heads up,", "update:", "for the record,", "just so you know,"]);
                turns[2 * slot] = (true, format!("{lead} {sent}. Please keep that in mind."), Some(fi));
                turns[2 * slot + 1] = (false, format!("Noted: {sent}. I'll use that from now on."), Some(fi));
                slot = (slot + 2 + r.below(2)) % pairs;
            }}
        }
        let mut evs = Vec::new();
        let (mut since, mut topics) = (0usize, Vec::<&str>::new());
        let fold_at = 5 + r.below(3);
        let mut ncard = 0;
        for (i, (user, text, fi)) in turns.into_iter().enumerate() {
            if let Some(fi) = fi { if !user { known[fi] = facts[fi].vals.iter().find(|x| x.0 == s).map(|x| x.1); } }
            evs.push(Ev::Turn { user, text });
            since += 1;
            if since == 1 { topics.push(r.pick(TOPICS)); }
            if since >= fold_at {
                evs.push(Ev::Note(format!("Fold summary (session {s}, turns {}-{}): worked on {}; tests and build green after fixes. {}",
                    i + 2 - since, i + 1, topics.join(", "), facts_section(&known))));
                let extra = if s == 2 && ncard == 0 { format!(" Staging bucket for uploads: {CARD_ONLY}.") } else { String::new() };
                evs.push(Ev::Card(format!("State (session {s}, card {}): {} in progress; next up {}.{extra}", ncard + 1, topics.join(" + "), r.pick(TOPICS))));
                ncard += 1;
                since = 0; topics.clear();
            }
        }
        let wrap = r.pick(TOPICS);
        evs.push(Ev::Note(format!("Session {s} wrap-up before /clear: progress on {wrap}. {}", facts_section(&known))));
        evs.push(Ev::Card(format!("State (session {s}, end): wrapped up {wrap}; resume from here next session.")));
        out.push(Session { evs });
    }
    out
}
