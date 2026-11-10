//! Intent routing (config `route`): which tool groups a user turn's requests carry, judged from the user's words with
//! cheap deterministic signals (no model call), decided once per turn like the reasoning gate (`mcp_reason::wanted`).
//!
//! A plain chat turn ("hey", "tell me a joke", "what's the capital of France") carries no tool definitions at all;
//! "search the latest news" carries the web tools; a file name, a code block or a traceback carries the file / code
//! tools; a PDF or spreadsheet the documents tools; "what time is it in Tokyo" the time server. A short follow-up
//! ("now also add X", "it still fails") keeps the last turn's groups. A long message with no signal at all gets the
//! small default set (web), never every tool. A call to a session tool outside the routed set widens the set
//! (`repl.rs`), so a wrong guess costs one tool-list change, never a failed turn.
//!
//! With the hosted intent router configured (`router.rs`), its answer decides the groups instead (`from_router`,
//! `Why::Router`); when it is off or misses, everything here runs unchanged.
//!
//! Why no tiny model classification call: on the measured 2B local model one extra round trip costs a full prefill of
//! its own prompt plus a first-token wait (~0.3-0.6 s warm) on every unclear turn, more than the ~150 tokens of web tool
//! definitions the default set sends, and the widen path already repairs a miss.

/// A set of tool groups (bit flags). Empty = plain chat: no tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Groups(pub u16);

pub const CODE: u16 = 1;
pub const WEB: u16 = 2;
pub const DOCS: u16 = 4;
pub const RUN: u16 = 8;
pub const TIME: u16 = 16;
pub const SEC: u16 = 32;
/// any other MCP / plugin server (routed per server by name: `Route::others`)
pub const OTHER: u16 = 64;
/// the time server's tools: never routed by words (a `TIME` turn gets the clock line instead, `context::local_now`),
/// only offered once the model calls one of them (the widen path)
pub const TIME_TOOLS: u16 = 128;
pub const ALL: u16 = CODE | WEB | DOCS | RUN | TIME | SEC | OTHER | TIME_TOOLS;

/// group names in display order
const NAMES: [(u16, &str); 8] = [(CODE, "files/code"), (WEB, "web"), (DOCS, "docs"), (RUN, "run"), (TIME, "time (clock line)"), (TIME_TOOLS, "time tools"), (SEC, "security"), (OTHER, "other")];

impl Groups {
    pub fn has(self, g: u16) -> bool { self.0 & g != 0 }
    pub fn is_empty(self) -> bool { self.0 == 0 }
    /// no tool definitions for these groups (chat, or only the clock line)
    pub fn tool_free(self) -> bool { self.0 & !TIME == 0 }
    pub fn with(self, g: u16) -> Groups { Groups(self.0 | g) }
    /// "chat (no tools)" or "files/code, web"
    pub fn describe(self) -> String {
        if self.is_empty() { return "chat (no tools)".into(); }
        NAMES.iter().filter(|(b, _)| self.has(*b)).map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
    }
}

/// Why a turn got its groups (shown by `/context` and the debug line).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Why {
    /// greeting, small talk, a short question or creative writing with no file / URL / code marker
    #[default]
    Chat,
    /// signals in the user's words (a URL, a file name, "search", a traceback …)
    Signals,
    /// a follow-up that continues the last turn's work (its groups kept, plus any new signal)
    FollowUp,
    /// no signal and not chat-like: the small default set (web), or the last turn's groups
    Unclear,
    /// a tool call outside the routed set widened it
    Widened,
    /// `route = "off"`: every tool
    Off,
    /// the hosted intent router's answer (`router.rs`)
    Router,
}

impl Why {
    pub fn word(self) -> &'static str {
        match self { Why::Chat => "chat", Why::Signals => "signals", Why::FollowUp => "follow-up", Why::Unclear => "unclear: default set", Why::Widened => "widened by a tool call", Why::Off => "route = off", Why::Router => "intent router" }
    }
}

/// This turn's route: the groups, the other servers named in the user's words, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Route {
    pub groups: Groups,
    /// other MCP / plugin servers routed this turn (their name was in the user's words)
    pub others: Vec<String>,
    pub why: Why,
}

impl Route {
    /// `route = "off"`: every tool group (no clock line: that is a routed turn's)
    pub fn all() -> Route { Route { groups: Groups(ALL & !TIME), others: Vec::new(), why: Why::Off } }
    pub fn describe(&self) -> String {
        let mut s = self.groups.describe();
        if !self.others.is_empty() { s += &format!(" + {}", self.others.join(", ")); }
        format!("{s} ({})", self.why.word())
    }
}

/// The group of one tool. `server`: its MCP server (None = Elisa's own tool). `read` serves documents too.
pub fn tool_group(name: &str, server: Option<&str>) -> u16 {
    match server {
        None if name == "read" => CODE | DOCS,
        None => CODE,
        Some("web") => WEB,
        Some("docs") => DOCS,
        Some("sec") => SEC,
        Some("run") => RUN,
        Some(s) if s.contains("time") || s.contains("clock") => TIME_TOOLS,
        Some(_) => OTHER,
    }
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '+' || c == '#')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

const GREETINGS: &[&str] = &["hi", "hey", "hello", "yo", "hiya", "howdy", "sup", "morning", "evening", "hola", "heya", "greetings", "thanks", "thank", "thx", "ty", "cheers", "bye", "goodbye", "goodnight", "gm", "gn", "lol", "haha", "cool", "nice", "great", "wow"];
/// creative or conversational asks: chat, whatever verb comes first ("write a poem", "tell me a joke")
const CHAT_NOUNS: &[&str] = &["joke", "jokes", "poem", "poems", "haiku", "story", "stories", "limerick", "riddle", "song", "lyrics", "essay", "letter", "tweet", "speech", "toast", "slogan", "caption", "pun", "puns", "recipe", "fact", "quote", "compliment", "advice", "rhyme", "sonnet", "fairy", "bedtime"];
const CHAT_PHRASES: &[&str] = &["how are you", "who are you", "what can you do", "what are you", "what's up", "whats up", "good morning", "good evening", "good afternoon", "good night", "about yourself", "your name", "can you help", "how's it going", "hows it going", "nice to meet"];
const WEB_WORDS: &[&str] = &["search", "google", "googling", "news", "latest", "headlines", "weather", "forecast", "lookup", "browse", "website", "online", "internet", "web", "trending", "stock", "stocks", "price", "prices", "score", "scores", "election", "released", "release"];
const WEB_PHRASES: &[&str] = &["look up", "look it up", "find out", "who won", "right now on", "this week", "today's", "current events", "what happened", "recent news", "on the web", "on the internet"];
/// words that keep "search" / "find" inside the project ("search the codebase for X")
const LOCAL_SCOPE: &[&str] = &["codebase", "repo", "repository", "project", "file", "files", "folder", "directory", "code", "function", "functions", "src", "symbol", "class"];
const TIME_PHRASES: &[&str] = &["what time", "time is it", "time in ", "timezone", "time zone", "time difference", "current time", "local time", "what's the date", "what is the date", "today's date", "what day is", "what date", "o'clock", "time now", "time there", "what day", "day is it", "day today", "date today", "the date", "what year", "what month", "what's today"];
const TZ_WORDS: &[&str] = &["utc", "gmt", "pst", "pdt", "est", "edt", "cst", "cet", "cest", "jst", "bst", "aest"];
/// a traceback, compiler error or log line
const TRACE_MARKERS: &[&str] = &["traceback (most recent call last)", "panicked at", "stack trace", "stacktrace", "segmentation fault", "segfault", "syntaxerror", "typeerror", "valueerror", "keyerror", "attributeerror", "importerror", "modulenotfounderror", "indexerror", "nameerror", "zerodivisionerror", "nullpointerexception", "exception in thread", "undefined is not", "cannot find module", "error[e", "error:", "npm err", "exit code", "exit status", "build failed", "compile error", "core dumped", "  file \""];
const CODE_WORDS: &[&str] = &["repo", "repository", "codebase", "function", "functions", "method", "class", "variable", "compile", "compiler", "bug", "bugs", "crash", "crashing", "crashes", "crashed", "lint", "linter", "refactor", "commit", "git", "branch", "rebase", "endpoint", "regex", "unittest", "pytest", "cargo", "npm", "pip", "makefile", "dockerfile", "script", "scripts", "program", "snippet", "cli", "module", "library", "dependency", "dependencies", "folder", "directory", "files", "code", "debug", "stacktrace", "tests", "test", "build", "deploy", "api", "sdk", "json", "yaml", "frontend", "backend", "app"];
/// "this project", "the file": code phrases
const CODE_PHRASES: &[&str] = &["this project", "the project", "my project", "this file", "the file", "my code", "the code", "this code", "this repo", "the repo", "unit test", "pull request", "merge conflict", "command line", "keeps crashing", "doesn't compile", "won't compile", "fails to build"];
/// verbs that start real work in a project (first real word)
const TASK_VERBS: &[&str] = &["fix", "build", "refactor", "implement", "debug", "migrate", "deploy", "install", "configure", "scaffold", "compile", "lint", "commit", "rename", "grep", "edit", "open", "list", "read", "create", "write", "add", "run", "test", "update", "delete", "remove"];
/// soft task verbs: work only with a code / file object ("write a poem" is chat, "write a script" is code)
const SOFT: &[&str] = &["create", "write", "add", "update", "delete", "remove", "open", "list", "read", "run", "test", "make", "generate", "build"];
const LANGS: &[&str] = &["python", "rust", "javascript", "typescript", "swift", "golang", "java", "kotlin", "ruby", "php", "c++", "c#", "html", "css", "sql", "bash", "zsh", "shell", "perl", "haskell", "scala", "lua", "react", "node", "nodejs", "django", "flask", "swiftui", "vue", "svelte", "dart", "flutter", "elixir", "julia", "matlab", "powershell", "sh", "js", "ts", "py", "rs"];
/// politeness skipped to find the first real word
const SKIP: &[&str] = &["please", "pls", "can", "could", "would", "will", "you", "now", "then", "also", "i", "want", "need", "to", "let's", "lets", "help", "me", "go", "ahead", "and", "just", "quickly", "kindly", "hey", "ok", "okay", "so"];
const QUESTION_STARTS: &[&str] = &["what", "what's", "whats", "who", "who's", "when", "where", "why", "how", "how's", "which", "is", "are", "does", "do", "did", "can", "could", "should", "would", "will", "explain", "define", "describe", "tell", "give", "suggest", "recommend", "summarize", "summarise", "translate"];
/// openers that continue the last turn's work
const CONT_STARTS: &[&str] = &["now", "also", "and", "then", "but", "still", "it", "it's", "its", "that", "same", "again", "instead", "next", "try", "plus", "another", "too"];
const CONT_PHRASES: &[&str] = &["still fail", "still crash", "still broken", "still error", "still doesn't", "still does not", "still not", "didn't work", "doesn't work", "does not work", "not working", "same error", "same issue", "same problem", "it fails", "it crashes", "what about", "how about", "do the same", "make it", "change it", "can you also", "one more", "that worked", "that broke", "undo"];
/// short replies that carry the previous turn on
const FOLLOW_UPS: &[&str] = &["continue", "go on", "go ahead", "keep going", "proceed", "next", "do it", "yes", "y", "yep", "yeah", "ok", "okay", "sure", "try again", "retry", "again", "carry on", "yes please", "please continue", "ok continue", "resume", "no", "nope", "n",
    "more", "more please", "tell me more", "the first one", "the second one", "the third one", "the last one",
    // Vietnamese: "go on", "continue", "do it", "yes", "ok", "more"
    "tiếp", "tiếp đi", "tiếp tục", "tiếp tục đi", "làm đi", "ok làm đi", "ừ", "ừ làm đi", "có", "được", "đồng ý", "thêm nữa"];
/// small talk that opens or closes ("hey", "thanks", "bye", "chào", "cảm ơn"): the turns that get a situation line
const SMALL_TALK_PHRASES: &[&str] = &["how are you", "what's up", "whats up", "good morning", "good evening", "good afternoon", "good night", "how's it going", "hows it going", "nice to meet",
    "chào", "xin chào", "cảm ơn", "cám ơn", "tạm biệt", "alo", "bạn khỏe không", "see you", "thank you"];

fn has_url(lower: &str) -> bool { lower.contains("http://") || lower.contains("https://") || lower.contains("www.") }

/// The deterministic signals in the user's words.
pub fn signals(msg: &str) -> Groups {
    let lower = msg.to_lowercase();
    let w = words(msg);
    let any = |list: &[&str]| w.iter().any(|x| list.contains(&x.as_str()));
    let phrase = |list: &[&str]| list.iter().any(|p| lower.contains(p));
    let mut g = 0;
    // web: a URL, or news / search / current-facts words ("search the codebase" stays local)
    let local = any(LOCAL_SCOPE);
    let web_word = w.iter().any(|x| WEB_WORDS.contains(&x.as_str()) && !(local && matches!(x.as_str(), "search" | "lookup" | "browse" | "test" | "release" | "released")));
    if has_url(&lower) || web_word || (phrase(WEB_PHRASES) && !local) { g |= WEB; }
    // documents: a PDF / spreadsheet / CSV, by word or file name
    if crate::mcp_reason::about_docs(msg) { g |= DOCS; }
    // time: "what time is it in Tokyo", "time zone", "3pm PST"
    if phrase(TIME_PHRASES) || any(TZ_WORDS) {
        g |= TIME;
        // a given clock time to convert ("3pm PST to London"): the clock line cannot do that arithmetic for a small
        // model, the time server's convert tool can
        if clock_time(msg) { g |= TIME_TOOLS; }
    }
    // files / code: a file name or path, a code block, a traceback, code words, a task verb with a code object,
    // a language named in a request (not a short question about it: "what is python?" is chat)
    let first = w.iter().find(|x| !SKIP.contains(&x.as_str())).map(String::as_str).unwrap_or("");
    let question = is_question(msg, &w);
    let code_obj = any(CODE_WORDS) || phrase(CODE_PHRASES);
    let lang = any(LANGS);
    let trace = msg.contains("```") || TRACE_MARKERS.iter().any(|m| lower.contains(m)) || traceback_lines(msg);
    let file = crate::mcp_reason::names_file(msg) && !has_url(&lower);
    let task = TASK_VERBS.contains(&first) && !SOFT.contains(&first) && !any(CHAT_NOUNS);
    let soft_task = SOFT.contains(&first) && (code_obj || lang || file);
    if file || trace || task || soft_task || (lang && (!question || code_obj)) || (code_obj && !question) || (code_obj && question && phrase(CODE_PHRASES)) {
        g |= CODE;
    }
    // the code sandbox: run / execute a snippet, or the sandbox by name
    let run_word = w.iter().any(|x| matches!(x.as_str(), "run" | "execute" | "exec" | "evaluate"));
    if any(&["sandbox", "run_code"]) || (run_word && (msg.contains("```") || any(&["snippet", "sandbox"]) || lower.contains("this code") || lower.contains("this snippet"))) {
        g |= RUN;
    }
    if crate::mcp_reason::about_sec(msg) { g |= SEC; }
    Groups(g)
}

/// a given clock time to convert ("3pm", "14:30"): the time server's convert tool does that arithmetic
fn clock_time(msg: &str) -> bool {
    static CLOCK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    CLOCK.get_or_init(|| regex::Regex::new(r"(?i)\b\d{1,2}(:\d{2})?\s?(am|pm)\b|\b\d{1,2}:\d{2}\b").unwrap()).is_match(msg)
}

/// two or more lines that look like a stack trace (`  at foo (x.js:3:9)`, `  File "a.py", line 3`)
fn traceback_lines(msg: &str) -> bool {
    msg.lines().filter(|l| {
        let t = l.trim_start();
        (l.starts_with(' ') || l.starts_with('\t')) && (t.starts_with("at ") || t.starts_with("File \"") || t.contains(".py\", line") || t.contains(".rs:"))
    }).count() >= 2
}

fn is_question(msg: &str, w: &[String]) -> bool {
    msg.trim_end().ends_with('?') || w.first().is_some_and(|f| QUESTION_STARTS.contains(&f.as_str()))
}

/// Chat-like: a greeting or thanks, small talk, creative writing, a joke.
fn chatty(lower: &str, w: &[String]) -> bool {
    w.first().is_some_and(|f| GREETINGS.contains(&f.as_str())) || CHAT_PHRASES.iter().any(|p| lower.contains(p)) || w.iter().any(|x| CHAT_NOUNS.contains(&x.as_str()))
}

/// A short greeting, thanks or goodbye ("hey", "thanks!", "good night", "chào", "cảm ơn nhé"): at most six words,
/// opening with a greeting word or a small-talk phrase. `repl.rs` adds the situation line (`situation.rs`) to such a
/// turn when it is routed as chat.
/// the user asks what Elisa can do or who she is (`situation::ABOUT` goes with that turn)
pub fn about_elisa(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    ["what can you do", "who are you", "what are you", "about yourself", "your name", "can you help", "what do you do",
     "your capabilities", "what are your features", "bạn làm được gì", "bạn là ai", "bạn có thể làm gì"].iter().any(|p| lower.contains(p))
}

/// The user explicitly asks for a web search ("search the web: …", "look up X online", "google it"): `repl.rs` then
/// tells the model, in the turn's first request only, to call `web_search`. Without that note a small model,
/// even with the web tools offered, answers such a request from memory. A plain question ("what is the capital of France?") is not an explicit ask.
pub fn web_ask(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    let w = words(&lower);
    const PHRASES: &[&str] = &["search the web", "search web", "web search", "search online", "search the internet", "internet search",
        "on the web", "on the internet", "browse the web", "check the web", "check online", "from the web", "from the internet"];
    let search_verb = w.iter().any(|x| matches!(x.as_str(), "search" | "look" | "lookup" | "find" | "check" | "browse"));
    // "google" as the verb ("google the weather", "can you google it"), not the company ("what is google's revenue?")
    let first = w.iter().find(|x| !SKIP.contains(&x.as_str())).map(String::as_str).unwrap_or("");
    let google = matches!(first, "google" | "googling") || ["google it", "google that", "google this", "google for"].iter().any(|p| lower.contains(p));
    google || PHRASES.iter().any(|p| lower.contains(p)) || (search_verb && w.iter().any(|x| x == "online"))
}

/// how an explicit-ask line starts (`repl.rs` `ask_note`); `strip_ask` removes it from saved views
pub const ASK_PREFIX: &str = "\n\n(The user asked ";

/// the user's words without a trailing explicit-ask line
pub fn strip_ask(text: &str) -> &str {
    match text.rfind(ASK_PREFIX) { Some(i) if text.ends_with(')') => &text[..i], _ => text }
}

/// The user explicitly asks to save or write something to a named file ("save that plan to plan.md", "write it into
/// notes.txt", "export the list as list.csv"): that file name. The save target is the file named after the saving
/// word, not the first one in the message ("read notes.txt and budget.py … then save a short trip summary
/// to summary.md" saves to summary.md, not notes.txt); only when none comes after it, the first one.
pub fn save_ask(msg: &str) -> Option<String> {
    const VERBS: [&str; 5] = ["save", "write", "put", "export", "store"];
    let lower = msg.to_lowercase();
    let verb = VERBS.iter().any(|v| lower.split_whitespace().any(|w| w == *v));
    let to = [" to ", " into ", " in ", " as "].iter().any(|p| lower.contains(p));
    if !verb || !to { return None; }
    // after the last saving word that has a file name after it: the one right after "to" / "into" / "in" / "as"
    // ("write a summary of notes.txt to summary.md"), else the first one after the word
    let words: Vec<&str> = msg.split_whitespace().collect();
    let bare = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let mut target = None;
    for (i, w) in words.iter().enumerate() {
        if !VERBS.contains(&bare(w).as_str()) { continue; }
        let rest = &words[i + 1..];
        let after_to = rest.windows(2).find(|p| matches!(bare(p[0]).as_str(), "to" | "into" | "in" | "as") && file_name(p[1]).is_some())
            .and_then(|p| file_name(p[1]));
        if let Some(f) = after_to.or_else(|| file_name(&rest.join(" "))) { target = Some(f); }
    }
    target.or_else(|| file_name(msg))
}

/// the first file name in the message ("notes.txt", "src/main.rs"; not "3.14")
pub fn file_name(msg: &str) -> Option<String> {
    msg.split(|c: char| c.is_whitespace() || matches!(c, ',' | '?' | '!' | '"' | '\'' | '`' | '(' | ')'))
        .map(|t| t.trim_end_matches(['.', ':', ';']))
        .find(|t| {
            let Some((stem, ext)) = t.rsplit_once('.') else { return false };
            !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && !ext.chars().all(|c| c.is_ascii_digit())
        })
        .map(String::from)
}

/// A reply that says a file was saved, written, created or changed ("I've saved "hello" to deny.txt", "Done, the file
/// is created", "đã lưu"), in a sentence that does not say it was not ("I did not save it", "nothing was written"):
/// the check after a write the user said no to or that failed (`repl.rs` `denied_claim_retry`).
pub fn claims_saved(reply: &str) -> bool {
    const DONE: &[&str] = &["saved", "i've written", "i have written", "has been written", "was written", "i wrote", "created",
        "updated", "i've added", "i have added", "has been added", "is now in", "now contains", "đã lưu", "đã ghi", "đã tạo", "đã cập nhật"];
    const NOT: &[&str] = &["not ", "n't", "nothing", "no file", "unable", "cannot", "said no", "declined", "denied", "skipped",
        "cancel", "không", "chưa"];
    reply.to_lowercase().split(['.', '!', '?', '\n', ';']).any(|s| DONE.iter().any(|p| s.contains(p)) && !NOT.iter().any(|p| s.contains(p)))
}

/// A reply that says a named file was saved or written ("Okay, here's the marketing plan saved to marketing-plan.md",
/// "I've written it to notes.txt", "the file has been saved"), in a sentence that does not say it was not: the check
/// on a turn where no write / edit was made at all (`repl.rs` `denied_claim_retry`). Narrower than
/// `claims_saved` (a saving word next to a file name or "file"), so a plan that says "keep your menu updated" is no claim.
pub fn claims_file_saved(reply: &str) -> bool { !file_claims(reply).is_empty() }

/// The sentences of `reply` (lowercased) that `claims_file_saved` takes as saying a file was saved.
pub fn file_claims(reply: &str) -> Vec<String> {
    const DONE: &[&str] = &["saved", "i've written", "i have written", "has been written", "was written", "i wrote", "written to", "đã lưu", "đã ghi"];
    const NOT: &[&str] = &["not ", "n't", "nothing", "no file", "unable", "cannot", "said no", "declined", "denied", "skipped",
        "cancel", "không", "chưa", "can save", "could save", "will save", "to save", "want to save", "if you"];
    let lower = reply.to_ascii_lowercase();
    lower.split(['!', '?', '\n', ';']).flat_map(|s| s.split(". ")).filter(|s| {
        DONE.iter().any(|p| s.contains(p)) && !NOT.iter().any(|p| s.contains(p))
            && (file_name(s).is_some() || s.contains("file") || s.contains("tệp"))
    }).map(String::from).collect()
}

/// The user explicitly asks to read a named file ("read notes.txt","open report.md and …", "what's in config.toml?"):
/// that file name. A bare mention ("I renamed notes.txt") is not an ask.
pub fn file_ask(msg: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    let verb = ["read ", "open ", "look at ", "check ", "summarize ", "summarise ", "show me ", "what's in ", "what is in ", "whats in ", "cat "]
        .iter().any(|v| lower.starts_with(v) || lower.contains(&format!(" {v}")));
    if !verb { return None; }
    file_name(msg)
}

/// Text from one of Elisa's request-only notes that a model repeated as its reply (e.g. "The
/// user asked to save the plan to plan.md: call write now…"): those sentences removed. A reply that
/// is only that comes back empty (the empty-reply retry then asks again).
/// What the history keeps for a reply with no text: never the stop reason itself. "[end_turn]" there gets
/// copied by small models as their next replies, word for word.
/// (A cancelled reply with no text never gets one: that turn leaves the history.)
pub fn no_answer_note(_stop_reason: &str) -> &'static str { NO_ANSWER }

/// the history's stand-in for a reply with no text (`no_answer_note`)
pub const NO_ANSWER: &str = "[no answer]";

/// Stop reasons and end-of-turn tokens written as reply text: a small model copies an old "[end_turn]",
/// or a template token gets through. Never reply text: taken out wherever they stand (`strip_stop_markers`).
const STOP_MARKERS: &[&str] = &["[end_turn]", "[tool_use]", "[max_tokens]", "[stop_sequence]", "[no output]", "[no answer]", "[cancelled]",
    "<|endoftext|>", "<|im_end|>", "<|eot_id|>", "<end_of_turn>", "<|end|>"];

/// `reply` without any `STOP_MARKERS` (case-insensitive), trimmed when one was taken out.
pub fn strip_stop_markers(reply: &str) -> String {
    let lower = reply.to_ascii_lowercase();
    if !STOP_MARKERS.iter().any(|m| lower.contains(m)) { return reply.to_string(); }
    let mut out = reply.to_string();
    for m in STOP_MARKERS {
        while let Some(i) = out.to_ascii_lowercase().find(m) { out.replace_range(i..i + m.len(), ""); }
    }
    out.trim().to_string()
}

pub fn strip_echoed_notes(reply: &str) -> String {
    let stripped = strip_stop_markers(reply);
    let reply = stripped.as_str();
    // the fixed wording of `repl.rs`'s own notes (`ask_note`, WEB_ASK_NOTE, EMPTY_REPLY_NUDGE, SMALL_TALK_NOTE,
    // REASONED_OUT_NUDGE, and the "… for this reply: …" ones: FINAL_ANSWER_NOTE, `retry_note`), not the user's words
    const MARKS: &[&str] = &["(the user asked ", "the user asked to save", "the user asked to read", "the user asked for a web search", "call write now",
        "call read on it now", "call web_search now", "had no text for the user", "reply in words; no tools", "ran out of room while reasoning",
        "for this reply:", "answer this message in one plain sentence", "no long reasoning first", "the message below asks about", "not yours: in a short reply"];
    let hit = |t: &str| { let t = t.to_lowercase(); MARKS.iter().any(|m| t.contains(m)) };
    if !hit(reply) { return reply.to_string(); }
    // sentence by sentence: "The user asked to read a.txt: call read on it now. It says hi." keeps the last one
    let mut out: Vec<String> = Vec::new();
    for line in reply.lines() {
        if !hit(line) { out.push(line.to_string()); continue; }
        let mut kept = String::new();
        let mut start = 0;
        let b = line.as_bytes();
        for (i, ch) in line.char_indices() {
            let end = matches!(ch, '.' | '!' | '?') && (i + 1 == line.len() || b.get(i + 1) == Some(&b' '));
            if end || i + ch.len_utf8() == line.len() {
                let sent = &line[start..i + ch.len_utf8()];
                if !hit(sent) { kept.push_str(sent); }
                start = i + ch.len_utf8();
            }
        }
        let kept = kept.trim();
        if !kept.is_empty() { out.push(kept.to_string()); }
    }
    out.join("\n").trim().to_string()
}

/// A reply's lines that only restate the user's own message, removed (a small model may begin a
/// document follow-up's reply with the question itself, or "The user's new question
/// is: '…'", and the next replies copy it). A line goes when it holds the start of the message, word for word (at
/// least 4 words and 80% of them), and little else (at most 8 more words); a message under 4 words is never looked
/// for. No wording of a reply is fixed here: only the user's own words are matched.
pub fn strip_echoed_ask(reply: &str, ask: &str) -> String {
    let words = |s: &str| s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect::<Vec<_>>();
    let a = words(ask);
    if a.len() < 4 { return reply.to_string(); }
    let k = (a.len() * 4).div_ceil(5).max(4);
    let head = format!(" {} ", a[..k].join(" "));
    let echo = |line: &str| {
        let w = words(line);
        w.len() <= a.len() + 8 && format!(" {} ", w.join(" ")).contains(&head)
    };
    let sentences = |line: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut start = 0;
        let b = line.as_bytes();
        for (i, ch) in line.char_indices() {
            let end = matches!(ch, '.' | '!' | '?') && (i + 1 == line.len() || b.get(i + 1) == Some(&b' '));
            if end || i + ch.len_utf8() == line.len() {
                out.push(line[start..i + ch.len_utf8()].to_string());
                start = i + ch.len_utf8();
            }
        }
        out
    };
    // sentence by sentence: "You asked what my name is. It is Linh." keeps the answer
    if !reply.lines().any(|l| sentences(l).iter().any(|s| echo(s))) { return reply.to_string(); }
    let mut out: Vec<String> = Vec::new();
    for line in reply.lines() {
        let all = sentences(line);
        if !all.iter().any(|s| echo(s)) { out.push(line.to_string()); continue; }
        // (a list number left alone, "1.", goes with it)
        let kept: String = all.into_iter().filter(|s| !echo(s)).collect();
        let kept = if kept.chars().any(char::is_alphabetic) { kept } else { String::new() };
        if !kept.trim().is_empty() { out.push(kept.trim().to_string()); }
    }
    out.join("\n").trim().to_string()
}

pub fn small_talk(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    let w = words(&lower);
    !w.is_empty() && w.len() <= 6 && (GREETINGS.contains(&w[0].as_str()) || SMALL_TALK_PHRASES.iter().any(|p| lower.starts_with(p)))
}

/// A thanks, goodbye or plain acknowledgement ("thanks!", "bye", "cool", "cảm ơn"): small talk that closes or
/// acknowledges, answered well from the history alone, so it gets no situation line.
pub fn closing(msg: &str) -> bool {
    const FIRST: &[&str] = &["thanks", "thank", "thx", "ty", "cheers", "bye", "goodbye", "goodnight", "gn", "lol", "haha", "cool", "nice", "great", "wow"];
    const PHRASES: &[&str] = &["cảm ơn", "cám ơn", "tạm biệt", "see you", "good night"];
    let lower = msg.trim().to_lowercase();
    words(&lower).first().is_some_and(|f| FIRST.contains(&f.as_str())) || PHRASES.iter().any(|p| lower.starts_with(p))
}

/// A bare acknowledgement ("ok", "got it", "ờ"): small talk unless it answers an offer ("Want me to search?"), where it
/// means yes (`repl.rs` checks the last answer).
pub fn ack(msg: &str) -> bool {
    const ACKS: &[&str] = &["ok", "okay", "k", "kk", "alright", "got it", "i see", "oh", "ah", "oh ok", "ok ok", "ờ", "vâng", "ok nhé", "à"];
    ACKS.contains(&words(msg.trim()).join(" ").as_str())
}

/// What an explicit ask to go back to the last conversation wants (`resume_intent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// "continue the last one", "resume", "tiếp tục cuộc trò chuyện trước": go on with its last answer or task
    Continue,
    /// "what were we talking about?", "where were we": a short recap
    Recap,
}

/// An explicit ask to go back to the last conversation ("continue the last one", "continue where we left off",
/// "resume", "go back to the last chat", "what were we talking about?", "tiếp tục", "quay lại cuộc trước"), from a
/// fixed phrase list (no model call). `repl.rs` acts on it only in a session with no history yet: the user's own
/// decision to carry the last session on; a new session never carries anything otherwise.
pub fn resume_intent(msg: &str) -> Option<Resume> {
    const CONTINUE: &[&str] = &["continue", "resume", "continue the last one", "continue the previous one", "continue last one",
        "continue where we left off", "continue from where we left off", "pick up where we left off", "carry on where we left off", "where we left off",
        "continue our conversation", "continue our last conversation", "continue our chat", "continue our last chat", "continue the conversation",
        "go back to the last chat", "go back to the last conversation", "go back to the last session", "go back to the previous chat", "go back to our last chat",
        "back to the last chat", "back to the last conversation", "back to where we were",
        "tiếp tục", "tiếp tục cuộc trước", "tiếp tục cuộc trò chuyện trước", "tiếp tục cuộc trò chuyện lúc nãy", "tiếp tục cuộc trò chuyện", "tiếp tục chỗ cũ",
        "tiếp tục phần trước", "tiếp tục cái trước", "tiếp cuộc trước", "quay lại cuộc trước", "quay lại cuộc trò chuyện trước", "quay lại chỗ cũ", "quay lại phần trước"];
    const CONTINUE_OF: &[&str] = &["continue the last", "continue the previous", "continue our last", "continue my last", "resume the last", "resume the previous", "resume our last", "resume my last",
        "go back to the last", "go back to the previous", "go back to our last", "back to the last", "back to the previous"];
    const NOUNS: &[&str] = &["chat", "conversation", "session", "discussion", "talk", "thread", "task", "one"];
    const RECAP: &[&str] = &["what were we talking about", "what were we doing", "where were we", "what was the last chat about", "what was the last conversation about",
        "what did we talk about last time", "what did we discuss last time", "chúng ta đang nói về gì", "mình đang nói về gì", "đang nói về gì vậy", "lúc nãy mình nói gì", "lúc nãy nói gì", "mình đang nói gì"];
    const LEAD: &[&str] = &["please", "pls", "ok", "okay", "hey", "hi", "can", "could", "we", "you", "let's", "lets", "so", "now", "ừ", "ok", "mình", "hãy"];
    const TAIL: &[&str] = &["please", "pls", "đi", "nhé", "nha", "thôi", "đã", "now", "again"];
    let mut w = words(msg.trim());
    while w.first().is_some_and(|x| LEAD.contains(&x.as_str())) { w.remove(0); }
    while w.last().is_some_and(|x| TAIL.contains(&x.as_str())) { w.pop(); }
    let j = w.join(" ");
    if RECAP.contains(&j.as_str()) { return Some(Resume::Recap); }
    if CONTINUE.contains(&j.as_str()) { return Some(Resume::Continue); }
    // "continue the last chat", "resume the previous session", "go back to our last conversation"
    let of = CONTINUE_OF.iter().any(|p| j.strip_prefix(p).is_some_and(|r| NOUNS.contains(&r.trim()))) ;
    of.then_some(Resume::Continue)
}

/// A short greeting that opens ("hey", "hi Elisa", "good morning", "chào"): small talk that is not a closing.
pub fn greeting(msg: &str) -> bool { small_talk(msg) && !closing(msg) }

/// A short reply that carries the last turn on ("ok", "continue", "tiếp đi").
pub fn is_follow_up(msg: &str) -> bool { FOLLOW_UPS.contains(&words(msg.trim()).join(" ").as_str()) }

/// The message continues the last turn's work ("now also add X", "it still fails").
fn continues(lower: &str, w: &[String]) -> bool {
    // a short message about "it" / "this" / "that file": the thing the last turn worked on ("what license does it use?")
    let refers = w.len() <= 12 && w.iter().any(|x| matches!(x.as_str(), "it" | "its" | "it's" | "this" | "that" | "these" | "those" | "them" | "they"));
    refers || w.first().is_some_and(|f| CONT_STARTS.contains(&f.as_str())) || CONT_PHRASES.iter().any(|p| lower.contains(p))
}

/// This turn's groups from the user's words and the last turn's route (`prev`; its groups carry a follow-up on).
/// `servers`: the other MCP / plugin servers of the session (routed when their name is in the user's words).
pub fn classify(msg: &str, prev: &Route, servers: &[String]) -> Route {
    let t = msg.trim();
    let lower = t.to_lowercase();
    let w = words(t);
    let others = servers_named(t, servers);
    let mk = |groups: Groups, others: Vec<String>, why: Why| Route { groups: if others.is_empty() { groups } else { groups.with(OTHER) }, others, why };
    if w.is_empty() { return mk(Groups::default(), others, Why::Chat); }
    let carried = || { let mut o = prev.others.clone(); for x in &others { if !o.contains(x) { o.push(x.clone()); } } o };
    if FOLLOW_UPS.contains(&w.join(" ").as_str()) {
        return Route { groups: prev.groups, others: prev.others.clone(), why: if prev.groups.is_empty() { Why::Chat } else { Why::FollowUp } };
    }
    let g = signals(t);
    let chat = chatty(&lower, &w);
    let cont = continues(&lower, &w) && !prev.groups.is_empty() && !chat;
    if !g.is_empty() {
        return if cont { mk(Groups(g.0 | prev.groups.0), carried(), Why::FollowUp) } else { mk(g, others, Why::Signals) };
    }
    if !others.is_empty() { return mk(g, others, Why::Signals); }
    if cont { return mk(prev.groups, prev.others.clone(), Why::FollowUp); }
    if chat || is_question(t, &w) || w.len() <= 12 { return mk(Groups::default(), others, Why::Chat); }
    // no signal, not chat-like: the small default set (the last turn's groups when it had some)
    if prev.groups.is_empty() { mk(Groups(WEB), others, Why::Unclear) } else { mk(prev.groups, prev.others.clone(), Why::Unclear) }
}

/// The session's other MCP / plugin servers named in the user's words (routed by name, router or not).
pub fn servers_named(msg: &str, servers: &[String]) -> Vec<String> {
    let w = words(msg);
    servers.iter().filter(|s| w.iter().any(|x| x == &s.to_lowercase())).cloned().collect()
}

/// This turn's route from the hosted intent router's plan (`router::plan`). A `what_about` with no companion intent
/// keeps the last turn's groups. `doc_text`: the turn's message carries document text (as `with_documents`: the
/// documents tools only when it is too large to go in whole, `large`). A clock time to convert adds the time tools,
/// as on the word path. `others`: servers named in the words (`servers_named`).
pub fn from_router(p: &crate::router::Plan, msg: &str, prev: &Route, others: Vec<String>, doc_text: bool, large: bool) -> Route {
    let mut g = if p.carry { Groups(prev.groups.0 & !OTHER) } else { p.groups };
    if doc_text { g = Groups(g.0 & !DOCS); if large { g = g.with(DOCS); } }
    if g.has(TIME) && clock_time(msg) { g = g.with(TIME_TOOLS); }
    let others = if p.carry && others.is_empty() { prev.others.clone() } else { others };
    if !others.is_empty() { g = g.with(OTHER); }
    Route { groups: g, others, why: Why::Router }
}

/// A turn whose message carries document text (an attached document, or a follow-up's pages: `attach.rs`) carries
/// only the tools its own words ask for: none carried over from the last turn's work, none from the default set, and
/// the documents' own names (`names`) left out of the words, since such a name is the text already in the turn, not a
/// file to open (otherwise a document question gets the working directory and the web tools, and a model reads
/// and outlines the attached files until the loop guard stops it). A document too large to go in whole (`large`)
/// adds the documents tools (and `read`, in the docs group too). `route = "off"` keeps every tool.
pub fn with_documents(r: Route, msg: &str, names: &[String], large: bool) -> Route {
    if r.why == Why::Off { return r; }
    let mut m = msg.to_string();
    for n in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        m = regex::RegexBuilder::new(&regex::escape(n)).case_insensitive(true).build().map_or(m.clone(), |re| re.replace_all(&m, " ").into_owned());
    }
    let others = if r.why == Why::Signals { r.others } else { Vec::new() };
    let mut g = Groups(signals(&m).0 & !(DOCS | OTHER));
    if large { g = g.with(DOCS); }
    if !others.is_empty() { g = g.with(OTHER); }
    Route { groups: g, why: if g.is_empty() { Why::Chat } else { Why::Signals }, others }
}

/// common names that are not a zoneinfo city: (words, zone)
const ZONE_ALIASES: &[(&str, &str)] = &[
    ("nyc", "America/New_York"), ("new york city", "America/New_York"), ("la", "America/Los_Angeles"), ("san francisco", "America/Los_Angeles"),
    ("seattle", "America/Los_Angeles"), ("silicon valley", "America/Los_Angeles"), ("california", "America/Los_Angeles"), ("boston", "America/New_York"),
    ("washington", "America/New_York"), ("miami", "America/New_York"), ("atlanta", "America/New_York"), ("dallas", "America/Chicago"), ("houston", "America/Chicago"),
    ("austin", "America/Chicago"), ("beijing", "Asia/Shanghai"), ("china", "Asia/Shanghai"), ("japan", "Asia/Tokyo"), ("osaka", "Asia/Tokyo"), ("korea", "Asia/Seoul"),
    ("india", "Asia/Kolkata"), ("delhi", "Asia/Kolkata"), ("new delhi", "Asia/Kolkata"), ("mumbai", "Asia/Kolkata"), ("bangalore", "Asia/Kolkata"),
    ("vietnam", "Asia/Ho_Chi_Minh"), ("saigon", "Asia/Ho_Chi_Minh"), ("hanoi", "Asia/Bangkok"), ("uk", "Europe/London"), ("england", "Europe/London"),
    ("france", "Europe/Paris"), ("germany", "Europe/Berlin"), ("spain", "Europe/Madrid"), ("italy", "Europe/Rome"), ("australia", "Australia/Sydney"),
    ("melbourne", "Australia/Melbourne"), ("thailand", "Asia/Bangkok"), ("hawaii", "Pacific/Honolulu"), ("utc", "UTC"), ("gmt", "UTC"),
    ("pst", "America/Los_Angeles"), ("pdt", "America/Los_Angeles"), ("est", "America/New_York"), ("edt", "America/New_York"), ("cst", "America/Chicago"),
    ("cet", "Europe/Paris"), ("cest", "Europe/Paris"), ("jst", "Asia/Tokyo"), ("bst", "Europe/London"), ("aest", "Australia/Sydney"), ("ist", "Asia/Kolkata"),
];

/// zoneinfo cities ("tokyo" → Asia/Tokyo, "new york" → America/New_York), read once from /usr/share/zoneinfo
fn zone_cities() -> &'static [(String, String)] {
    static Z: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    Z.get_or_init(|| {
        let mut v = Vec::new();
        for region in ["Africa", "America", "Asia", "Atlantic", "Australia", "Europe", "Indian", "Pacific"] {
            let Ok(rd) = std::fs::read_dir(format!("/usr/share/zoneinfo/{region}")) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_file() { v.push((name.replace('_', " ").to_lowercase(), format!("{region}/{name}"))); }
            }
        }
        v.sort();
        v
    })
}

/// The time zones a message names (a city, a country or an abbreviation), at most three, in first-seen order.
pub fn zones_named(msg: &str) -> Vec<String> {
    let w = words(msg);
    let text = format!(" {} ", w.join(" "));
    let mut hits: Vec<(usize, String)> = Vec::new();
    let mut add = |name: &str, zone: &str| {
        if let Some(i) = text.find(&format!(" {name} ")) { if !hits.iter().any(|(_, z)| z == zone) { hits.push((i, zone.to_string())); } }
    };
    for (n, z) in ZONE_ALIASES { add(n, z); }
    for (n, z) in zone_cities() { add(n, z); }
    hits.sort();
    hits.into_iter().map(|(_, z)| z).take(3).collect()
}

/// `Time in Tokyo now: 2026-11-09 15:16 Monday (UTC+09:00, Asia/Tokyo)`, from the system's zone database (`date`
/// with that `TZ`: no environment change in this process). None when the zone is unknown there.
fn time_in(zone: &str) -> Option<String> {
    let o = std::process::Command::new("date").env("TZ", zone).arg("+%Y-%m-%d %H:%M %A %z").output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let (rest, off) = s.rsplit_once(' ')?;
    if !o.status.success() || off.len() != 5 { return None; }
    let city = zone.rsplit('/').next().unwrap_or(zone).replace('_', " ");
    Some(format!("Time in {city} now: {rest} (UTC{}:{}, {zone})", &off[..3], &off[3..]))
}

/// The clock lines for a turn about the time: the local time, then each place the message names.
pub fn time_lines(msg: &str) -> String {
    let mut s = crate::context::local_now();
    for z in zones_named(msg) { if let Some(l) = time_in(&z) { s.push('\n'); s.push_str(&l); } }
    s
}

/// The system prompt for a route: the paragraphs only tool turns need (the file / code rules, the state card) come
/// last and only when the route carries them, so a chat turn does not pay for coding rules. A paragraph is a
/// blank-line-separated block; the file / code one starts "When working with files or code", the card one holds
/// `<state>`. Everything else stays as written, in order.
pub fn system_for(system: &str, groups: Groups) -> String {
    let tools = !groups.tool_free();
    let paras: Vec<&str> = system.split("\n\n").collect();
    let is_code = |p: &str| { let h = p.trim_start().lines().next().unwrap_or("").to_lowercase(); h.starts_with("when ") && (h.contains("code") || h.contains("file")) && h.trim_end().ends_with(':') };
    let is_card = |p: &str| p.contains("<state>");
    // the tool-use rules ("When tools are offered:"): meaningless on a turn that sends no tools
    let is_tool_rules = |p: &str| p.trim_start().to_lowercase().starts_with("when tools are offered");
    let mut out: Vec<&str> = paras.iter().copied().filter(|p| !is_code(p) && !is_card(p) && (tools || !is_tool_rules(p))).collect();
    if groups.has(CODE) { out.extend(paras.iter().copied().filter(|p| is_code(p))); }
    if tools { out.extend(paras.iter().copied().filter(|p| is_card(p))); }
    let mut s = out.iter().map(|p| p.trim_matches('\n')).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(msg: &str) -> Groups { classify(msg, &Route::default(), &[]).groups }

    #[test]
    fn a_stop_reason_is_never_reply_text() {
        // a model may answer "[end_turn]", copied from the history's old stand-in
        for r in ["[end_turn]", " [END_TURN]\n", "[end_turn][end_turn]", "<|im_end|>", "[no answer]", "[max_tokens]"] {
            assert_eq!(strip_echoed_notes(r), "", "{r:?}");
        }
        assert_eq!(strip_echoed_notes("Hi Linh! [end_turn]"), "Hi Linh!");
        assert_eq!(strip_echoed_notes("Café <s>old</s> [end_turn]"), "Café <s>old</s>");
        assert_eq!(strip_echoed_notes("a plain reply"), "a plain reply");
        assert_eq!(no_answer_note("end_turn"), NO_ANSWER);
        assert_eq!(no_answer_note(""), NO_ANSWER);
        assert_eq!(no_answer_note("max_tokens"), NO_ANSWER);
        assert_eq!(no_answer_note("cancelled"), NO_ANSWER);
        assert_eq!(strip_stop_markers("[cancelled]"), "");
        assert!(!NO_ANSWER.contains("end_turn"));
    }

    #[test]
    fn a_reply_that_restates_the_message_loses_that_part() {
        // a document follow-up's reply that starts with the question
        let ask = "What is the main contribution of the paper?";
        assert_eq!(strip_echoed_ask("1. What is the main contribution of the paper?\nThe main contribution is Kronos, a foundation model.", ask),
            "The main contribution is Kronos, a foundation model.");
        assert_eq!(strip_echoed_ask("The user's new question is: 'What is the main contribution of the paper?'", ask), "");
        assert_eq!(strip_echoed_ask("You asked what is the main contribution of the paper. It is Kronos.", ask), "It is Kronos.");
        // an answer that uses some of the words stays; so does any reply to a short message
        let a = "The main contribution of the paper is Kronos, a family of decoder-only models pre-trained on K-line data.";
        assert_eq!(strip_echoed_ask(a, ask), a);
        assert_eq!(strip_echoed_ask("who am I?", "who am I?"), "who am I?");
        // one sentence that holds the question and an answer stays
        let long = "The answer to what is the main contribution of the paper is a tokenizer and an autoregressive model trained on twelve billion records";
        assert_eq!(strip_echoed_ask(long, ask), long);
    }

    #[test]
    fn explicit_file_asks() {
        assert_eq!(file_ask("read notes.txt and tell me the meeting code").as_deref(), Some("notes.txt"));
        assert_eq!(file_ask("can you open src/main.rs?").as_deref(), Some("src/main.rs"));
        assert_eq!(file_ask("what's in config.toml?").as_deref(), Some("config.toml"));
        for m in ["I renamed notes.txt yesterday", "read me a poem", "what is 3.14 times 2?", "hey", "open the pod bay doors"] {
            assert_eq!(file_ask(m), None, "{m}");
        }
        assert_eq!(save_ask("save that plan to marketing-plan.md").as_deref(), Some("marketing-plan.md"));
        assert_eq!(save_ask("write it into notes.txt please").as_deref(), Some("notes.txt"));
        // the file after the saving word, not the first one in the message
        assert_eq!(save_ask("step by step: read notes.txt and budget.py, add up the budget, then save a short trip summary to summary.md").as_deref(), Some("summary.md"));
        assert_eq!(save_ask("write a summary of notes.txt to summary.md").as_deref(), Some("summary.md"));
        assert_eq!(save_ask("save the plan to plan.md and write a short note in the chat").as_deref(), Some("plan.md"));
        for m in ["read notes.txt", "I saved notes.txt yesterday", "save me some time", "what is 3.14?"] { assert_eq!(save_ask(m), None, "{m}"); }
    }

    /// one of Elisa's own notes repeated as the reply is removed, the rest kept
    #[test]
    fn a_reply_naming_a_saved_file_is_a_claim() {
        for r in ["Okay, here's the marketing plan saved to marketing-plan.md", "I've written the plan to plan.md.", "Done! The file has been saved.",
            "Mình đã lưu kế hoạch vào plan.md."] {
            assert!(claims_file_saved(r), "{r}");
        }
        for r in ["Keep your menu updated and post daily.", "I haven't saved it to plan.md yet.", "I can save this to plan.md if you want.",
            "Here is the plan:\n1. Saved-money deals on Mondays", "Would you like me to save it to a file?", "Customers saved 20% with the loyalty card."] {
            assert!(!claims_file_saved(r), "{r}");
        }
    }

    #[test]
    fn echoed_notes_are_removed() {
        assert_eq!(strip_echoed_notes("The user asked to save the marketing plan to marketing-plan.md: call write now with the full content."), "");
        assert_eq!(strip_echoed_notes("Here is the plan."), "Here is the plan.");
        assert_eq!(strip_echoed_notes("The user asked to read notes.txt: call read on it now. The meeting code is ORCHID-42."), "The meeting code is ORCHID-42.");
    }

    /// a reply after a denied write: a success claim is caught, a plain "not done" is not
    #[test]
    fn saved_claims() {
        for r in ["I've saved \"hello\" to deny.txt for you.", "I have saved \"hello\" to deny.txt as requested.",
            "Done! deny.txt has been created. Don't hesitate to ask for more.", "Tôi đã lưu vào deny.txt."] {
            assert!(claims_saved(r), "{r}");
        }
        for r in ["Okay, I did not save it.", "I couldn't save the file as requested.", "Nothing was written to deny.txt.",
            "You said no, so deny.txt was not created.", "Tôi chưa lưu tệp.", "Sure, what should I write?"] {
            assert!(!claims_saved(r), "{r}");
        }
    }

    /// a picture's own questions (the app's text for an image sent alone, and the usual ones) are chat: the model
    /// sees the picture, no tool is needed, and no tool rules or state card go in the prompt
    #[test]
    fn questions_about_an_attached_picture_are_chat() {
        for m in ["What is in this image?", "Describe this picture in one sentence.", "what colour are the ears in the photo?", "How many images are in this message?"] {
            let r = classify(m, &Route::default(), &[]);
            assert!(r.groups.is_empty(), "{m:?} -> {}", r.describe());
        }
    }

    /// an explicit ask to search the web (the turn's first request then tells the model to call web_search)
    #[test]
    fn explicit_web_asks() {
        for m in ["search the web: what is the capital of Australia?", "look up the latest news about Apple online", "google the weather in Paris",
                  "can you google it?", "please search online for flights to Hanoi", "find it on the internet", "do a web search for rust 2024"] {
            assert!(web_ask(m), "{m:?}");
            assert!(route(m).has(WEB), "{m:?}");
        }
        for m in ["what is the capital of France?", "what is google's revenue?", "search the codebase for TODO", "latest news about Apple", "hi", "is the site online?"] {
            assert!(!web_ask(m), "{m:?}");
        }
    }

    #[test]
    fn a_large_attached_document_routes_to_docs() {
        let chat = classify("summarise this", &Route::default(), &[]);
        assert!(chat.groups.is_empty());
        assert_eq!(with_documents(chat.clone(), "summarise this", &[], false), chat, "a small document goes in whole: no tools");
        let r = with_documents(chat, "summarise this", &[], true);
        assert!(r.groups.has(DOCS) && r.why == Why::Signals, "{}", r.describe());
        assert_eq!(tool_group("docs_pdf", Some("docs")) & r.groups.0, DOCS);
    }

    #[test]
    fn a_turn_with_document_text_carries_only_what_its_own_words_ask_for() {
        // decided by the attached document (structure), whatever the words
        let names = vec!["sales.xlsx".to_string(), "plan.ods".to_string()];
        let after_tools = Route { groups: Groups(CODE), others: vec!["fake".into()], why: Why::FollowUp };
        for (msg, prev) in [("what's in these two files?", &after_tools), ("and what about these?", &after_tools),
            ("compare sales.xlsx with plan.ods and tell me which numbers stand out across both of them please", &Route::default()),
            ("what's in Sales.xlsx?", &after_tools)] {
            let r = classify(msg, prev, &[]);
            let d = with_documents(r.clone(), msg, &names, false);
            assert!(d.groups.is_empty() && d.others.is_empty() && d.why == Why::Chat, "{msg:?}: {} → {}", r.describe(), d.describe());
            assert!(with_documents(r, msg, &names, true).groups == Groups(DOCS), "{msg:?}: large → docs tools only");
        }
        // the turn's own words still ask for tools: a file to write, the web
        let msg = "save a summary of sales.xlsx to notes.md";
        assert!(with_documents(classify(msg, &after_tools, &[]), msg, &names, false).groups.has(CODE));
        let msg = "search the web for the latest prices of these products";
        assert!(with_documents(classify(msg, &Route::default(), &[]), msg, &names, false).groups.has(WEB));
        // route = "off": every tool stays
        assert_eq!(with_documents(Route::all(), "what's in these?", &names, false), Route::all());
    }

    #[test]
    fn chat_turns_get_no_tools() {
        for m in ["hey", "hi there!", "what can you do?", "tell me a joke", "what's the capital of France", "explain recursion simply",
            "can you write a poem", "write a haiku about autumn", "thanks!", "how are you today?", "who are you", "good morning",
            "what is python?", "give me some advice on sleeping better", "translate 'good night' into Spanish",
            "what's the difference between a virus and a bacterium?", "write me a bedtime story about a dragon", "lol", "2+2=?",
            "summarize the plot of Hamlet", "recommend a good book"] {
            assert_eq!(route(m), Groups::default(), "{m:?}: {}", route(m).describe());
        }
    }

    #[test]
    fn small_talk_is_greetings_thanks_and_byes() {
        for m in ["hey", "hi there!", "thanks!", "thank you so much", "bye", "good night", "chào", "xin chào", "cảm ơn nhé", "how are you?", "Hey Elisa"] {
            assert!(small_talk(m), "{m:?}");
        }
        for m in ["hey can you read README.md and summarize the install steps for me", "what time is it", "why?", "tell me a joke", "", "ok"] {
            assert!(!small_talk(m), "{m:?}");
        }
        // an explicit ask for the last conversation, English and Vietnamese
        for m in ["continue the last one", "Continue where we left off", "resume", "go back to the last chat", "resume the last session", "please continue our last conversation",
            "tiếp tục", "tiếp tục cuộc trò chuyện trước", "quay lại cuộc trước", "tiếp tục đi"] { assert_eq!(resume_intent(m), Some(Resume::Continue), "{m:?}"); }
        for m in ["what were we talking about?", "where were we?", "chúng ta đang nói về gì?"] { assert_eq!(resume_intent(m), Some(Resume::Recap), "{m:?}"); }
        for m in ["continue writing the poem", "continue the story about dragons", "resume my CV: make it shorter", "go back to the last step of the recipe", "hey", "what is a resume?", "tiếp tục viết bài thơ"] {
            assert_eq!(resume_intent(m), None, "{m:?}");
        }
        // a bare acknowledgement: small talk only when no offer is pending (`repl.rs`)
        for m in ["ok", "Okay!", "got it", "ờ"] { assert!(ack(m), "{m:?}"); }
        for m in ["ok do it", "ok, search the news", "okay so what about Osaka?"] { assert!(!ack(m), "{m:?}"); }
    }

    #[test]
    fn greetings_open_closings_close() {
        for m in ["hey", "hi Elisa", "good morning", "chào", "xin chào", "how are you?"] { assert!(greeting(m) && !closing(m), "{m:?}"); }
        for m in ["thanks!", "thank you", "bye", "cảm ơn", "good night", "cool"] { assert!(closing(m) && !greeting(m), "{m:?}"); }
    }

    #[test]
    fn short_follow_ups_keep_the_last_turns_groups() {
        let prev = classify("search the latest news about Apple", &Route::default(), &[]);
        for m in ["more", "the second one", "tiếp đi", "ok", "do it"] {
            assert_eq!(classify(m, &prev, &[]).groups, prev.groups, "{m:?}");
        }
        // with nothing before them: chat, no tools
        for m in ["more", "the second one", "tiếp đi", "why?"] { assert_eq!(route(m), Groups::default(), "{m:?}"); }
    }

    #[test]
    fn signals_pick_the_groups() {
        let cases: &[(&str, u16)] = &[
            ("search the latest news about Apple", WEB),
            ("look up the weather in Paris", WEB),
            ("what's on https://example.com/page", WEB),
            ("who won the election?", WEB),
            ("read README.md and summarise it", CODE),
            ("write a python script that renames photos", CODE),
            ("fix the failing test in calc.py", CODE),
            ("this keeps crashing\nTraceback (most recent call last):\n  File \"app.py\", line 3, in <module>\n    main()\nZeroDivisionError: division by zero", CODE),
            ("search the codebase for todo comments", CODE),
            ("build a habit tracker app", CODE),
            ("refactor the parser", CODE),
            ("what's in this pdf: a.pdf", DOCS),
            ("summarise report.xlsx", DOCS),
            ("what time is it in Tokyo", TIME),
            ("what time is it", TIME),
            ("what day is today", TIME),
            ("convert 3pm PST to London time", TIME),
            ("check this project for leaked secrets", CODE | SEC),
            ("run this snippet in the sandbox: print(1)", RUN),
        ];
        for (m, want) in cases {
            let got = route(m);
            assert!(got.0 & want == *want, "{m:?}: want {}, got {}", Groups(*want).describe(), got.describe());
        }
        // no stray groups on the clear ones
        assert_eq!(route("search the latest news about Apple"), Groups(WEB));
        assert_eq!(route("what time is it in Tokyo"), Groups(TIME));
        assert_eq!(route("convert 3pm PST to London time"), Groups(TIME | TIME_TOOLS));
        assert_eq!(route("what's in this pdf: a.pdf"), Groups(DOCS));
        assert!(!route("read README.md and summarise it").has(WEB));
    }

    #[test]
    fn follow_ups_keep_the_last_turns_work() {
        let code = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
        for m in ["what license does it use?", "why is that slow?", "now also add a --verbose flag", "it still fails", "continue", "yes", "and the tests?", "same error again", "make it faster"] {
            assert!(classify(m, &code, &[]).groups.has(CODE), "{m:?}");
        }
        // a clear chat turn after code work drops the tools
        assert_eq!(classify("tell me a joke", &code, &[]).groups, Groups::default());
        assert_eq!(classify("thanks!", &code, &[]).groups, Groups::default());
        // a follow-up after chat stays chat
        assert_eq!(classify("ok", &Route::default(), &[]).groups, Groups::default());
        assert_eq!(classify("what does it mean?", &Route::default(), &[]).groups, Groups::default());
        // a new signal on a follow-up adds to the groups
        let g = classify("now also search the web for the docs of serde", &code, &[]).groups;
        assert!(g.has(CODE) && g.has(WEB), "{}", g.describe());
    }

    #[test]
    fn unclear_turns_get_the_small_default_set_and_servers_by_name() {
        let r = classify("I am planning a trip to Japan next spring with my family and we have about three thousand dollars in total", &Route::default(), &[]);
        assert_eq!((r.groups, r.why), (Groups(WEB), Why::Unclear));
        let r = classify("use the browser to open the login page", &Route::default(), &["browser".to_string()]);
        assert!(r.others == ["browser"] && r.groups.has(OTHER), "{r:?}");
    }

    #[test]
    fn places_in_the_words_get_their_clock_line() {
        assert_eq!(zones_named("what time is it in Tokyo"), ["Asia/Tokyo"]);
        assert_eq!(zones_named("convert 3pm PST to London time"), ["America/Los_Angeles", "Europe/London"]);
        assert_eq!(zones_named("time in new york and in paris?"), ["America/New_York", "Europe/Paris"]);
        assert!(zones_named("what time is it").is_empty());
        if std::path::Path::new("/usr/share/zoneinfo/Asia/Tokyo").exists() {
            let l = time_lines("what time is it in Tokyo");
            assert!(l.starts_with("Local time now: ") && l.contains("\nTime in Tokyo now: ") && l.contains("(UTC+09:00, Asia/Tokyo)"), "{l}");
        }
    }

    #[test]
    fn routed_headers_are_not_the_users_words() {
        use crate::context::request_body;
        assert_eq!(request_body("OS: macos\nDate: 2026-11-09\nLocal time now: x\n\nhey"), "hey");
        assert_eq!(request_body("Local time now: x\nTime in Tokyo now: y\n\nwhat time is it in Tokyo"), "what time is it in Tokyo");
        assert_eq!(request_body("Working directory: /p\n\nLocal time now: x\n\nfix it"), "fix it");
        assert_eq!(request_body("Local time now: x\n\nWorking directory: /p\n\nfix it"), "fix it");
    }

    #[test]
    fn the_router_plan_becomes_the_route() {
        use crate::router::Plan;
        let web = Route { groups: Groups(WEB), others: vec!["browser".into()], why: Why::Signals };
        let p = |g: u16| Plan { groups: Groups(g), ..Default::default() };
        let r = from_router(&p(CODE | WEB), "search and save", &Route::default(), Vec::new(), false, false);
        assert_eq!((r.groups, r.why), (Groups(CODE | WEB), Why::Router));
        assert_eq!(r.describe(), "files/code, web (intent router)");
        // chat: no tools, whatever came before
        assert!(from_router(&p(0), "the second one", &web, Vec::new(), false, false).groups.is_empty());
        // a what_about with no companion keeps the last turn's groups and servers
        let c = from_router(&Plan { carry: true, ..Default::default() }, "and in Osaka?", &web, Vec::new(), false, false);
        assert_eq!((c.groups, c.others.as_slice()), (Groups(WEB | OTHER), &["browser".to_string()][..]));
        // document text in the message: docs tools only when too large
        assert_eq!(from_router(&p(DOCS), "summarise", &Route::default(), Vec::new(), true, false).groups, Groups::default());
        assert_eq!(from_router(&p(DOCS | WEB), "summarise", &Route::default(), Vec::new(), true, true).groups, Groups(DOCS | WEB));
        // a clock time to convert adds the time tools; a server named adds OTHER
        assert_eq!(from_router(&p(TIME), "3pm PST in London?", &Route::default(), Vec::new(), false, false).groups, Groups(TIME | TIME_TOOLS));
        let o = from_router(&p(0), "use the browser", &Route::default(), servers_named("use the browser", &["browser".into()]), false, false);
        assert_eq!((o.groups, o.others.len()), (Groups(OTHER), 1));
    }

    #[test]
    fn tools_fall_into_their_groups() {
        assert_eq!(tool_group("bash", None), CODE);
        assert_eq!(tool_group("read", None), CODE | DOCS);
        assert_eq!(tool_group("web_search", Some("web")), WEB);
        assert_eq!(tool_group("docs_pdf", Some("docs")), DOCS);
        assert_eq!(tool_group("run_code", Some("run")), RUN);
        assert_eq!(tool_group("time_get_current_time", Some("time")), TIME_TOOLS);
        assert_eq!(tool_group("browser_click", Some("browser")), OTHER);
    }

    #[test]
    fn chat_prompt_leaves_out_the_tool_paragraphs() {
        let sys = "You are Elisa.\n\nRules:\n- Be brief.\n\nWhen working with files or code:\n- Inspect first.\n\nNever add a card to plain chat. Only after tools:\n<state>\ngoal: …\n</state>\n";
        let chat = system_for(sys, Groups::default());
        assert!(!chat.contains("Inspect") && !chat.contains("<state>") && chat.contains("Be brief"), "{chat}");
        let web = system_for(sys, Groups(WEB));
        assert!(!web.contains("Inspect") && web.contains("<state>"), "{web}");
        assert!(!system_for(sys, Groups(TIME)).contains("<state>"), "the clock line alone is no tool turn");
        let code = system_for(sys, Groups(CODE));
        assert!(code.contains("Inspect") && code.contains("<state>"), "{code}");
        assert!(code.find("Be brief").unwrap() < code.find("Inspect").unwrap());
    }
}
