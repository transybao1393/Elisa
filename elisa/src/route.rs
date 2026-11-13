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
//! Why no tiny model classification call: on a small local model one extra round trip costs a full prefill of
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
/// not a group: the file tools of a files turn are offered read-only (no `write`, `edit`, `edit_symbol`), on an ask
/// that only asks or checks (`check_only`); never in `ALL`, never named in `describe`
pub const READ_ONLY: u16 = 256;

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
    /// small talk by meaning ("I went for a run this morning and now I'm starving") or a question about what Elisa can
    /// do ("can you search the web?"): no tool belongs to the turn, none is kept from the last one, and a tool call on
    /// it does not widen the route (`repl/turn.rs` `widen_for`; such a turn is a small-talk turn there)
    Talk,
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
    /// the router was reached but gave no usable answer (the hardcoded rules stay off): every tool, the model decides
    RouterMiss,
    /// a question pointing at a page Elisa holds ("trang đó dùng để làm gì?" after a page was read), naming no URL of
    /// its own: the page answers it (`repl/agent_loop.rs` `held_page_line`), so no tool belongs to the turn and a
    /// call on it does not widen the route (a search would answer from results that are not the page)
    Held,
}

impl Why {
    pub fn word(self) -> &'static str {
        match self { Why::Chat => "chat", Why::Talk => "small talk or about Elisa", Why::Signals => "signals", Why::FollowUp => "follow-up", Why::Unclear => "unclear: default set", Why::Widened => "widened by a tool call", Why::Off => "route = off", Why::Router => "intent router", Why::RouterMiss => "intent router gave no answer: every tool", Why::Held => "a page Elisa holds" }
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
    /// a router miss with Elisa's own rules off: every tool group, and the servers named in the words
    /// (`servers_named`), as on a router answer
    pub fn router_miss(others: Vec<String>) -> Route {
        let g = Groups(ALL & !TIME & !OTHER);
        Route { groups: if others.is_empty() { g } else { g.with(OTHER) }, others, why: Why::RouterMiss }
    }
    /// a question about a page Elisa holds: no tool (`Why::Held`)
    pub fn held_page() -> Route { Route { groups: Groups(0), others: Vec::new(), why: Why::Held } }
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

/// The deterministic signals in the user's words. A question about what Elisa can do ("can you search the web?")
/// has none: it names a tool without asking for it (`about_elisa`).
pub fn signals(msg: &str) -> Groups {
    if about_elisa(msg) { return Groups::default(); }
    let lower = msg.to_lowercase();
    let w = words(msg);
    let any = |list: &[&str]| w.iter().any(|x| list.contains(&x.as_str()));
    let phrase = |list: &[&str]| list.iter().any(|p| lower.contains(p));
    let mut g = 0;
    // web: a URL, or news / search / current-facts words ("search the codebase" stays local), in English here and
    // in any language of `data/route_words.txt` ("giá vàng hôm nay thế nào nhỉ", "thời tiết Hà Nội hôm nay?")
    // (a date or weekday question, "what's today's date?", asks the clock, not the news: its "today's" is no web word)
    let local = any(LOCAL_SCOPE);
    let date = date_ask(msg);
    let web_word = w.iter().any(|x| WEB_WORDS.contains(&x.as_str()) && !(local && matches!(x.as_str(), "search" | "lookup" | "browse" | "test" | "release" | "released")));
    if has_url(&lower) || (!date && (web_word || (phrase(WEB_PHRASES) && !local))) || has_route_word(msg, "web") { g |= WEB; }
    // documents: a PDF / spreadsheet, by word or file name (a .csv / .txt is a plain file: the file tools read it)
    if crate::mcp_reason::about_docs(msg) { g |= DOCS; }
    // time: "what time is it in Tokyo", "time zone", "3pm PST", the asks of `data/clock_words.txt` in any language
    // ("got the time?", "wats the time rn", "mấy giờ rồi?") and a date or weekday question (`date_ask`: "hôm nay là
    // ngày mấy?", "what day of the week is it?"; the clock line has the date)
    if phrase(TIME_PHRASES) || any(TZ_WORDS) || crate::situation::asks_time(msg) || date {
        g |= TIME;
        // a given clock time to convert ("3pm PST to London"): the clock line cannot do that arithmetic for a small
        // model, the time server's convert tool can
        if clock_time(msg) { g |= TIME_TOOLS; }
    }
    // files / code: a file name or path, a code block, a traceback, code words, a task verb with a code object,
    // a language named in a request (not a short question about it: "what is python?" is chat); the file words of
    // `data/route_words.txt` in any language count as code words ("tìm giúp mình file nào có chữ ORCHID", "tệp").
    // A question with a code word is chat ("what is a file?") unless it is about the user's own project, folder or
    // a file the conversation has ("which file in this folder mentions a meeting code?", "what else is in that
    // file?", "file nào trong thư mục này có chữ ORCHID?": `[here]` and `[that_file]` of route_words.txt)
    let first = w.iter().find(|x| !SKIP.contains(&x.as_str())).map(String::as_str).unwrap_or("");
    let question = is_question(msg, &w);
    let code_obj = any(CODE_WORDS) || phrase(CODE_PHRASES) || has_route_word(msg, "files");
    let own_files = phrase(CODE_PHRASES) || has_route_word(msg, "here") || has_route_word(msg, "that_file");
    let lang = any(LANGS);
    let trace = msg.contains("```") || TRACE_MARKERS.iter().any(|m| lower.contains(m)) || traceback_lines(msg);
    let file = crate::mcp_reason::names_file(msg) && !has_url(&lower);
    let task = TASK_VERBS.contains(&first) && !SOFT.contains(&first) && !any(CHAT_NOUNS);
    let soft_task = SOFT.contains(&first) && (code_obj || lang || file);
    if file || trace || task || soft_task || (lang && (!question || code_obj)) || (code_obj && !question) || (code_obj && question && own_files) {
        g |= CODE;
    }
    // a program named to use ("how much disk space is free here? use df", "đếm bằng wc"): a shell turn (`named_command`)
    if named_command(msg).is_some() { g |= CODE; }
    // the code sandbox: run / execute a snippet, or the sandbox by name
    let run_word = w.iter().any(|x| matches!(x.as_str(), "run" | "execute" | "exec" | "evaluate"));
    if any(&["sandbox", "run_code"]) || (run_word && (msg.contains("```") || any(&["snippet", "sandbox"]) || lower.contains("this code") || lower.contains("this snippet"))) {
        g |= RUN;
    }
    if crate::mcp_reason::about_sec(msg) { g |= SEC; }
    // an ask for the shell (`repl::shell_ask`: "check … with wget: try http://1.1.1.1 in the shell"): the shell's
    // tools, and a URL in it is the command's, not a page to fetch here
    if crate::repl::shell_ask(msg) { g = (g | CODE) & !WEB; }
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

/// `chatty` for a whole message ("tell me a short joke", "write a poem"): a chat ask of its own, so a word it shares
/// with an earlier document makes it no question about that document (`topics::pick_for`).
pub fn chat_ask(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    chatty(&lower, &words(&lower))
}

/// A short greeting, thanks or goodbye ("hey", "thanks!", "good night", "chào", "cảm ơn nhé"): at most six words,
/// opening with a greeting word or a small-talk phrase. `repl.rs` adds the situation line (`situation.rs`) to such a
/// turn when it is routed as chat.
/// the user asks what Elisa can do or who she is (`situation::ABOUT` goes with that turn), by its wordings or by
/// meaning (`ability_ask`: "can you search the web?", "bạn có đọc được file trên máy mình không?")
pub fn about_elisa(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    // ("can you help me with something?" is an ask for help, not about Elisa: answered "Sure, what do you need?")
    ["what can you do", "who are you", "what are you", "about yourself", "your name", "what do you do",
     "your capabilities", "what are your features", "bạn làm được gì", "bạn là ai", "bạn có thể làm gì"].iter().any(|p| lower.contains(p))
        || ability_ask(msg)
}

/// Example data for `ability_ask`: an `[ability]` and an `[other]` section, one example per line.
static ABILITY_ASKS: Meaning = Meaning::new(include_str!("../data/ability_asks.txt"));
/// the similarity to the nearest `[ability]` example from which a question is about Elisa, and how much nearer it
/// must be than the nearest `[other]` example (measured on held-out EN/VI questions: see the tests)
pub const ABILITY_MIN: f32 = 0.6;
pub const ABILITY_LEAD: f32 = 0.1;
/// a question as near an `[ability]` example as this, and no nearer an `[other]` one than `ABILITY_TIE`, is about
/// Elisa when a word of it is only in the `[ability]` examples (`Meaning::distinct_word`: the model reads Vietnamese
/// questions of one shape, "bạn có … không?", as the same question on both sides: "bạn có lên mạng được không?" is
/// 0.96 from each, "bạn có vào mạng được không?" 0.88 from each; "bạn có khỏe không?" has no word of its own)
pub const ABILITY_NEAR: f32 = 0.85;
pub const ABILITY_TIE: f32 = 0.02;

/// A question about what Elisa can do, by meaning (`data/ability_asks.txt`): a question (it ends with "?" or opens
/// with a question word) that names no file, web address or code, nearest an `[ability]` example at `ABILITY_MIN` or
/// more and `ABILITY_LEAD` nearer than any `[other]` one, or as near as `ABILITY_NEAR` with a word only the
/// `[ability]` examples have. "can you search the web for the latest news?" asks for the search (an `[other]` example
/// is nearer); "can you read notes.txt?" names a file. False without the embedding model.
pub fn ability_ask(msg: &str) -> bool {
    let t = msg.trim();
    let w = words(t);
    if w.is_empty() || w.len() > 14 || !is_question(t, &w) || has_url(&t.to_lowercase()) || t.contains("```")
        || crate::mcp_reason::names_file(t) || file_name(t).is_some() { return false; }
    ABILITY_ASKS.scores(t).is_some_and(|(name, top, other)| name == "ability" && top >= ABILITY_MIN
        && (top - other >= ABILITY_LEAD || (top >= ABILITY_NEAR && other <= top + ABILITY_TIE && ABILITY_ASKS.distinct_word(t))))
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
    // (a question whether Elisa can search, "can you search the web?", asks for no search: `about_elisa`)
    if about_elisa(msg) { return false; }
    // the word rules, else by meaning (`tool_ask_by_meaning`: "tìm trên mạng: …", "tim tren mang: …"); a question
    // about today's date or weekday ("hôm nay là ngày bao nhiêu?", `date_ask`) asks the clock line, not the web,
    // however near it comes to a "today's price" example
    google || PHRASES.iter().any(|p| lower.contains(p)) || (search_verb && w.iter().any(|x| x == "online"))
        || (!date_ask(msg) && tool_ask_by_meaning(msg) == Some("web"))
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
    // Elisa's own bracketed notes (`[elisa: …]`, sent with tool results) copied into a reply: never reply text
    let notes = elisa_notes(reply);
    let reply = notes.as_str();
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

/// `text` without Elisa's own bracketed notes (`[elisa: …]`, the form of every note she adds to a tool result), a
/// line left empty by them dropped; unchanged when it has none.
pub fn elisa_notes(text: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\[elisa: [^\]\n]*\]").unwrap());
    if !re.is_match(text) { return text.to_string(); }
    let out = re.replace_all(text, "");
    let mut lines: Vec<&str> = Vec::new();
    for (a, b) in text.lines().zip(out.lines()) {
        if b.trim().is_empty() && !a.trim().is_empty() { continue; }
        lines.push(b.trim_end());
    }
    lines.join("\n").trim().to_string()
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

/// Small talk by meaning (`small_talk_by_meaning`) or made of closing and greeting words only (`talk_words_only`);
/// the word rule only when the embedding model is not there.
pub fn small_talk(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    let w = words(&lower);
    if w.is_empty() { return false; }
    small_talk_by_meaning(msg).map(|b| b || talk_words_only(&w))
        .unwrap_or_else(|| w.len() <= 6 && (GREETINGS.contains(&w[0].as_str()) || SMALL_TALK_PHRASES.iter().any(|p| lower.starts_with(p))))
}

/// sections of `SMALL_TALK_DATA` whose words make a thanks, goodbye or greeting
const TALK_WORD_SECTIONS: &[&str] = &["thanks", "bye", "greet_words"];

/// Whether `w` (a short message's words) is only words of the `TALK_WORD_SECTIONS` examples, one of them a whole
/// example by itself ("thanks!", "ok thx", "bye for now", "chào bạn"): a one- or two-word closing is too short for
/// its meaning to be read reliably. A message with any other word ("ok", "hey, read notes.txt") is left to meaning.
fn talk_words_only(w: &[String]) -> bool {
    if w.len() > 6 { return false; }
    let lines: Vec<Vec<String>> = TALK_WORD_SECTIONS.iter().flat_map(|s| small_talk_examples(s)).map(words).collect();
    w.iter().all(|x| lines.iter().any(|l| l.contains(x))) && w.iter().any(|x| lines.iter().any(|l| l.len() == 1 && l[0] == *x))
}

/// Example data for `small_talk_by_meaning`: `[small_talk]` and `[sharing]` sections and an `[other]` section, one
/// example per line.
const SMALL_TALK_DATA: &str = include_str!("../data/small_talk.txt");
/// the all-MiniLM-L6-v2 similarity to the nearest small-talk example from which a message is small talk, and how
/// much nearer it must be than the nearest `[other]` example (measured on held-out EN/VI messages: greetings and
/// check-ins 0.61–1.0 with a lead of 0.22 or more, how the user's day went 0.62–1.0 with a lead of 0.2 or more; an
/// ask at 0.6 or more leads by at most 0.11, and a short follow-up, "và cái kia?", stays under 0.6)
pub const SMALL_TALK_MIN: f32 = 0.6;
pub const SMALL_TALK_LEAD: f32 = 0.15;
/// small talk is short: a longer message carries an ask ("I went for a run this morning and now I'm starving" has
/// eleven words)
const SMALL_TALK_MAX_WORDS: usize = 12;

/// The examples of one section of `SMALL_TALK_DATA`.
fn small_talk_examples(section: &str) -> Vec<&'static str> {
    let mut on = false;
    SMALL_TALK_DATA.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| { if l.starts_with('[') { on = *l == format!("[{section}]"); false } else { on } }).collect()
}

/// Whether `msg` is small talk by meaning: short, no tool signal (`signals`), and closer to a `[small_talk]` or
/// `[sharing]` example of `data/small_talk.txt` than to any `[other]` one, by `SMALL_TALK_LEAD`, at `SMALL_TALK_MIN`
/// or more. None when the embedding model is not installed (the word rule decides then).
pub fn small_talk_by_meaning(msg: &str) -> Option<bool> {
    type Vecs = (Vec<Vec<f32>>, Vec<Vec<f32>>);
    static EXAMPLES: std::sync::OnceLock<Option<Vecs>> = std::sync::OnceLock::new();
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = std::sync::OnceLock::new();
    let dir = elisa_mem::embed::model_dir()?;
    let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
    let embed = |t: &[&str]| elisa_mem::embed::embed(&dir, &vocab, t, &mut |_| {}).ok().map(|v| v.into_iter().map(|x| x.to_vec()).collect::<Vec<_>>());
    let (talk, other) = EXAMPLES.get_or_init(|| {
        let mut talk = small_talk_examples("small_talk");
        talk.extend(small_talk_examples("sharing"));
        Some((embed(&talk)?, embed(&small_talk_examples("other"))?))
    }).as_ref()?;
    let m = msg.trim();
    if words(m).len() > SMALL_TALK_MAX_WORDS || !signals(m).is_empty() { return Some(false); }
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&b) = seen.lock().ok()?.get(m) { return Some(b); }
    let q = embed(&[m])?.pop()?;
    let best = |ex: &[Vec<f32>]| ex.iter().map(|e| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>()).fold(f32::MIN, f32::max);
    let (t, o) = (best(talk), best(other));
    let yes = t >= SMALL_TALK_MIN && t - o >= SMALL_TALK_LEAD;
    if let Ok(mut s) = seen.lock() { if s.len() < 512 { s.insert(m.to_string(), yes); } }
    Some(yes)
}

/// How near `ask` is in meaning to each of `texts` (all-MiniLM-L6-v2 cosine, one per text, in order): Elisa's way of
/// telling which tool description an ask is about. None when the embedding model is not installed.
pub fn meaning_scores(ask: &str, texts: &[&str]) -> Option<Vec<f32>> { meaning_scores_in(ask, texts, MEANING_SEEN_MAX) }

/// texts whose vectors `meaning_scores` keeps between calls; past it, only the current call's texts stay
const MEANING_SEEN_MAX: usize = 512;

/// `meaning_scores` with the cache limit `max` (the tests fill a small one)
fn meaning_scores_in(ask: &str, texts: &[&str], max: usize) -> Option<Vec<f32>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Vec<f32>>>> = std::sync::OnceLock::new();
    let dir = elisa_mem::embed::model_dir()?;
    let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
    let seen = SEEN.get_or_init(Default::default);
    let ask = ask.trim();
    let want: Vec<&str> = std::iter::once(ask).chain(texts.iter().copied()).collect();
    // The vectors this call has, copied out under one lock. A cache past `max` keeps this call's texts alone, before
    // the missing ones are counted (cleared after that count, it lost texts cached earlier and wanted now: one silent
    // miss at every overflow), and the scores come from the copy, so another call's overflow in between takes nothing.
    let (mut vecs, missing) = {
        let mut s = seen.lock().ok()?;
        if s.len() > max { s.retain(|k, _| want.contains(&k.as_str())); }
        let mut vecs: std::collections::HashMap<&str, Vec<f32>> = Default::default();
        let mut missing: Vec<&str> = vec![];
        for &t in &want {
            match s.get(t) { Some(v) => { vecs.insert(t, v.clone()); } None => if !missing.contains(&t) { missing.push(t) } }
        }
        (vecs, missing)
    };
    if !missing.is_empty() {
        let v = elisa_mem::embed::embed(&dir, &vocab, &missing, &mut |_| {}).ok()?;
        let mut s = seen.lock().ok()?;
        for (t, e) in missing.iter().zip(v) { s.insert(t.to_string(), e.to_vec()); vecs.insert(t, e.to_vec()); }
    }
    let q = vecs.get(ask)?;
    texts.iter().map(|t| vecs.get(t).map(|e| e.iter().zip(q).map(|(a, b)| a * b).sum::<f32>())).collect()
}

/// What a tool does, in plain words, from its description's first clause ("Search the web; returns …" → "search the
/// web", "Find leaked secrets in project files (masked)." → "find leaked secrets in project files"): for Elisa's own
/// line about a tool she cannot run, so the user reads what she can't do, never a tool name. None when the clause
/// does not start with a verb Elisa can say ("Known vulnerabilities in …", "A symbol's source …").
pub fn tool_action(description: &str) -> Option<String> {
    let first = description.split(". ").next().unwrap_or(description);
    let end = first.find([';', '(', ':', ',']).unwrap_or(first.len());
    let clause = first[..end].trim().trim_end_matches('.').trim();
    let verb = clause.split_whitespace().next()?.to_lowercase();
    let says = verb.chars().all(|c| c.is_alphabetic()) && clause.split_whitespace().count() >= 2
        && !matches!(verb.as_str(), "a" | "an" | "the") && !verb.ends_with("ed") && !verb.ends_with("wn")
        && (!verb.ends_with('s') || verb.ends_with("ss"));
    says.then(|| { let mut c = clause.chars(); c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default() })
}

/// Example data for `tool_ask_by_meaning`: `[web]`, `[run]` and `[other]` sections, one example per line.
static TOOL_ASKS: Meaning = Meaning::new(include_str!("../data/tool_asks.txt"));

/// the sections of one example file: (name, its examples' vectors)
type Sections = Vec<(&'static str, Vec<Vec<f32>>)>;
/// a text's measure: the nearest section's name, its similarity, and the nearest `[other]` similarity
type Measure = (&'static str, f32, f32);

/// Example data read by meaning (all-MiniLM-L6-v2): `[name]` sections with one example per line and an `[other]`
/// section, embedded once on first use, with the texts measured so far kept.
struct Meaning {
    data: &'static str,
    sections: std::sync::OnceLock<Option<Sections>>,
    seen: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Measure>>>,
    /// the words of the named sections' examples, and the words of the `[other]` examples (`distinct_word`)
    words: std::sync::OnceLock<(std::collections::HashSet<String>, std::collections::HashSet<String>)>,
}

impl Meaning {
    const fn new(data: &'static str) -> Meaning { Meaning { data, sections: std::sync::OnceLock::new(), seen: std::sync::OnceLock::new(), words: std::sync::OnceLock::new() } }

    /// Whether `text` has a word that the named sections' examples use and no `[other]` example does ("lên" of "bạn
    /// có lên mạng được không?"): the model reads Vietnamese poorly, so a question of one shape ("bạn có … không?")
    /// is as near the examples of either side; the word only one side has tells them apart.
    fn distinct_word(&self, text: &str) -> bool {
        let (own, other) = self.words.get_or_init(|| {
            let (mut own, mut other, mut in_other) = (std::collections::HashSet::new(), std::collections::HashSet::new(), false);
            for l in self.data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                match l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                    Some(name) => in_other = name == "other",
                    None => for w in words(l) { if in_other { other.insert(w); } else { own.insert(w); } },
                }
            }
            (own, other)
        });
        words(text).iter().any(|w| own.contains(w) && !other.contains(w))
    }

    /// `text`'s measure: the nearest section other than `[other]`, its similarity, and the nearest `[other]`
    /// similarity; ("", 0, 0) for an empty text. None when the embedding model is not installed.
    fn scores(&self, text: &str) -> Option<Measure> {
        let dir = elisa_mem::embed::model_dir()?;
        let vocab = std::env::temp_dir().join("elisa-minilm-vocab.bin");
        let embed = |t: &[&str]| elisa_mem::embed::embed(&dir, &vocab, t, &mut |_| {}).ok().map(|v| v.into_iter().map(|x| x.to_vec()).collect::<Vec<_>>());
        let ex = self.sections.get_or_init(|| {
            let mut out: Sections = Vec::new();
            let mut cur: Option<(&'static str, Vec<&'static str>)> = None;
            for l in self.data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                    if let Some((n, v)) = cur.take() { out.push((n, embed(&v)?)); }
                    cur = Some((name, Vec::new()));
                } else if let Some((_, v)) = cur.as_mut() { v.push(l); }
            }
            if let Some((n, v)) = cur.take() { out.push((n, embed(&v)?)); }
            Some(out)
        }).as_ref()?;
        let m = text.trim();
        if m.is_empty() { return Some(("", 0.0, 0.0)); }
        let seen = self.seen.get_or_init(Default::default);
        if let Some(&b) = seen.lock().ok()?.get(m) { return Some(b); }
        let q = embed(&[m])?.pop()?;
        let best = |v: &[Vec<f32>]| v.iter().map(|e| e.iter().zip(&q).map(|(a, b)| a * b).sum::<f32>()).fold(f32::MIN, f32::max);
        let other = ex.iter().filter(|(n, _)| *n == "other").map(|(_, v)| best(v)).fold(f32::MIN, f32::max);
        let (name, top) = ex.iter().filter(|(n, _)| *n != "other").map(|(n, v)| (*n, best(v))).fold(("", f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
        if let Ok(mut s) = seen.lock() { if s.len() < 512 { s.insert(m.to_string(), (name, top, other)); } }
        Some((name, top, other))
    }
}
/// the similarity to the nearest `[web]` / `[run]` example from which a message is that ask, and how much nearer it
/// must be than the nearest `[other]` example (measured on held-out EN/VI messages, accented and not: see the tests)
pub const TOOL_ASK_MIN: f32 = 0.55;
pub const TOOL_ASK_LEAD: f32 = 0.1;

/// An explicit ask for one of Elisa's own tools by meaning: Some("web") (search the web) or Some("run") (run a
/// snippet) when the message is nearest a `[web]` / `[run]` example of `data/tool_asks.txt`, at `TOOL_ASK_MIN` or
/// more and `TOOL_ASK_LEAD` nearer than any `[other]` one; Some("") for none. None when the embedding model is not
/// installed (the word rules decide then). Only the message's words before a ':' or a code block are read (the
/// ask, not the question or code after it).
pub fn tool_ask_by_meaning(msg: &str) -> Option<&'static str> {
    let (name, top, other) = tool_ask_scores(msg)?;
    Some(if top >= TOOL_ASK_MIN && top - other >= TOOL_ASK_LEAD { name } else { "" })
}

/// `tool_ask_by_meaning`'s measure: the nearest tool section, its similarity, and the nearest `[other]` similarity.
pub fn tool_ask_scores(msg: &str) -> Option<(&'static str, f32, f32)> {
    // the ask part: up to a code block, and the words before a ':' when some follow it ("tìm trên mạng: …")
    let head = msg.split("```").next().unwrap_or(msg);
    let m = match head.split_once(':') { Some((a, b)) if !a.trim().is_empty() && !b.trim().is_empty() => a, _ => head }.trim();
    TOOL_ASKS.scores(m)
}

/// Routing words by language (`data/route_words.txt`): the lines of one section, lowercased, and without their
/// Vietnamese marks (`reply_lang::fold_mark`) for a message typed without them.
fn route_words(section: &str) -> &'static [(String, String)] {
    static W: std::sync::OnceLock<std::collections::HashMap<String, Vec<(String, String)>>> = std::sync::OnceLock::new();
    let all = W.get_or_init(|| {
        let mut out: std::collections::HashMap<String, Vec<(String, String)>> = Default::default();
        let mut cur = String::new();
        for l in include_str!("../data/route_words.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                Some(name) => cur = name.to_string(),
                None => out.entry(cur.clone()).or_default().push((l.to_lowercase(), fold_marks(l))),
            }
        }
        out
    });
    all.get(section).map_or(&[], Vec::as_slice)
}

/// `s` lowercased and without its Vietnamese marks ("Giá vàng" → "gia vang").
fn fold_marks(s: &str) -> String { s.chars().map(crate::reply_lang::fold_mark).collect() }

/// where `text` holds the words `p` as whole words (both lowercased): the first such place
fn word_index(text: &str, p: &str) -> Option<usize> {
    text.match_indices(p).map(|(i, _)| i).find(|&i| {
        let before = text[..i].chars().next_back();
        let after = text[i + p.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// whether `text` holds the words `p` as whole words (both lowercased)
fn holds_words(text: &str, p: &str) -> bool { word_index(text, p).is_some() }

/// `lower` with every word of `section` of data/route_words.txt (as written, or without its marks) blanked out.
fn without_route_words(lower: &str, section: &str) -> String {
    let mut s = lower.to_string();
    for (w, f) in route_words(section) {
        for p in [w.as_str(), f.as_str()].into_iter().filter(|p| !p.is_empty()) {
            while let Some(i) = word_index(&s, p) { s.replace_range(i..i + p.len(), " "); }
        }
    }
    s
}

/// Whether `msg` holds a word of `section` of `data/route_words.txt` as written, or without its marks: a phrase of two
/// or more words in any message ("gia vang hom nay the nao nhi" holds "giá vàng"; no English runs such words together,
/// and a message typed without marks is not always read as Vietnamese), a single word only when the message reads as
/// Vietnamese ("tep" alone proves little).
fn has_route_word(msg: &str, section: &str) -> bool {
    let lower = msg.to_lowercase();
    let plain = fold_marks(msg);
    let vi = vietnamese(msg);
    route_words(section).iter().any(|(w, f)| holds_words(&lower, w) || (f != w && (vi || f.contains(' ')) && holds_words(&plain, f)))
}

/// The values of every "`key` = a | b | c" line of `data` (the clock and date word files of `situation`, read here as
/// data), lowercase, from every language section.
fn keyed_values(data: &str, key: &str) -> Vec<String> {
    data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| l.split_once('=')).filter(|(k, _)| k.trim() == key)
        .flat_map(|(_, v)| v.split('|').map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect::<Vec<_>>()).collect()
}

/// The wordings of a date or weekday question, every language: the `asks` of data/clock_words.txt that are not
/// `time asks`, its `date asks` and `day asks`, and the `weekday`, `until`, `since` and `from now` asks of
/// data/date_words.txt.
fn date_words() -> &'static [String] {
    static W: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let clock = include_str!("../data/clock_words.txt");
        let dates = include_str!("../data/date_words.txt");
        let time = keyed_values(clock, "time asks");
        let mut out: Vec<String> = keyed_values(clock, "asks").into_iter().filter(|a| !time.contains(a)).collect();
        out.extend(keyed_values(clock, "date asks"));
        out.extend(keyed_values(clock, "day asks"));
        for k in ["weekday", "until", "since", "from now"] { out.extend(keyed_values(dates, k)); }
        out.sort();
        out.dedup();
        out
    })
}

/// A question about the date or the day ("what's today's date?", "what day is it?", "hôm nay là ngày mấy?", "how
/// many days until Christmas?"), by the wordings of `date_words`: a clock turn, never a news one.
pub fn date_ask(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    date_words().iter().any(|p| holds_words(&lower, p))
}

/// Whether the user's words name a file or files: a `[files]` or `[that_file]` word of data/route_words.txt, with or
/// without marks ("tim giup minh file nao co chu ORCHID", "the files here", "thư mục này", "that file"), or a file name
/// ("notes.txt").
pub fn names_files(msg: &str) -> bool {
    has_route_word(msg, "files") || has_route_word(msg, "that_file") || crate::mcp_reason::names_file(msg) || file_name(msg).is_some()
}

/// A plain statement: the user tells a fact about themselves or shares how they are ("My name is Linh and I live in
/// Hanoi.", "I'm so tired today"), with no question and no ask in it (no question mark or question word, no task,
/// search or news word, no yes/ok to an offer). Such a turn is never widened to the web by a tool call
/// (`web_kept_out`): the model answers in words.
pub fn plain_statement(msg: &str) -> bool {
    let t = msg.trim();
    let lower = t.to_lowercase();
    let w = words(t);
    if w.is_empty() || t.contains('?') || is_question(t, &w) || has_url(&lower) || t.contains("```") || is_follow_up(t) || ack(t) { return false; }
    let ask = w.iter().any(|x| TASK_VERBS.contains(&x.as_str()) || SOFT.contains(&x.as_str()) || WEB_WORDS.contains(&x.as_str()))
        || WEB_PHRASES.iter().any(|p| lower.contains(p)) || has_route_word(msg, "web");
    !ask && (tells_own_facts(t) || small_talk(t))
}

/// The user's whole typed message (`context::own_words`) only tells facts about them, asking nothing: "My name is
/// Linh and I live in Hanoi." (`context::only_user_facts`), "Mình tên là Linh và mình sống ở Hà Nội."
/// (`self_statement`). No question mark.
fn tells_own_facts(t: &str) -> bool {
    !t.contains('?') && crate::context::own_words(t) == Some(t) && (crate::context::only_user_facts(t) || self_statement(t))
}

/// The user's words for themself and the question words (`me`, `ask` of data/person_words.txt, every language), without
/// their marks.
fn person_words() -> &'static (Vec<String>, Vec<String>) {
    static WORDS: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
    WORDS.get_or_init(|| {
        let data = include_str!("../data/person_words.txt");
        let folded = |k: &str| keyed_values(data, k).iter().map(|v| fold_marks(v)).collect::<Vec<_>>();
        (folded("me"), folded("ask"))
    })
}

/// A Vietnamese message about the user that asks nothing ("Mình tên là Linh và mình sống ở Hà Nội.", typed with or
/// without marks): a word the user says for themself (`me` of data/person_words.txt), no question word of that file
/// (`ask`), and no word of a request (`[request]` of data/route_words.txt). The fact check of `context` reads English.
fn self_statement(t: &str) -> bool {
    let (me, asks) = person_words();
    if !vietnamese(t) || t.contains('?') { return false; }
    let plain = words(&fold_marks(t)).join(" ");
    me.iter().any(|p| holds_words(&plain, p)) && !asks.iter().any(|p| holds_words(&plain, p)) && !has_route_word(t, "request")
}

/// Whether a tool call must not widen this turn to the web: the route offers no web tool, and the message is about
/// the user's files (a files/code turn whose words name a file or files, `names_files`: "tim giup minh file nao co
/// chu ORCHID") or a plain statement (`plain_statement`: "My name is Linh and I live in Hanoi."). A call written as
/// text widens to every other group then (`repl/agent_loop.rs`).
pub fn web_kept_out(route: &Route, msg: &str) -> bool {
    !route.groups.has(WEB) && ((route.groups.has(CODE) && names_files(msg)) || plain_statement(msg))
}

/// A call to a web tool on a turn the web is kept out of (`web_kept_out`): the wrong tool, so the turn is not
/// widened to the web for it; refused with this note (the wrong-tool nudge of `repl/agent_loop.rs`), and the model
/// goes on with the file tools or in words. None for any other call.
pub fn web_call_kept_out(name: &str, server: Option<&str>, route: &Route, msg: &str) -> Option<String> {
    if tool_group(name, server) != WEB || !web_kept_out(route, msg) { return None; }
    Some(if route.groups.has(CODE) && names_files(msg) {
        format!("Not run: this message is about the user's files, not the web, so `{name}` does not apply. Use the file tools (grep, read, glob) on them and answer from what they return.")
    } else {
        format!("Not run: the user told you something and asked for nothing, so `{name}` does not apply. Answer in words.")
    })
}

/// The thing an ask for the file that holds it asks about ("which file in this folder mentions a meeting code?" →
/// "meeting code", "file nào có chữ ORCHID?" → "ORCHID"), by the frames of data/which_file_asks.txt, as the user wrote
/// it: Elisa searches the project's files for it herself (grep) and the answer names the file. The words of `[here]`
/// ("in this folder", "trong thư mục này") are left out first, and a frame opening word of `drop` ("a", "chữ") is not
/// part of the thing. None for any other message, or a thing of more than six words.
pub fn which_file_term(msg: &str) -> Option<String> {
    type Frame = (Vec<String>, Vec<String>);
    static DATA: std::sync::OnceLock<(Vec<Frame>, Vec<Vec<String>>)> = std::sync::OnceLock::new();
    let split = |l: &str| l.split_whitespace().map(fold_marks).collect::<Vec<_>>();
    let (frames, drop) = DATA.get_or_init(|| {
        let data = include_str!("../data/which_file_asks.txt");
        let lines = data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
        let frames = lines.filter(|l| !l.starts_with("drop ="))
            .filter_map(|l| l.split_once("{x}").map(|(a, b)| (split(a), split(b)))).collect();
        let mut drop: Vec<Vec<String>> = keyed_values(data, "drop").iter().map(|d| split(d)).collect();
        drop.sort_by_key(|d| std::cmp::Reverse(d.len()));
        (frames, drop)
    });
    // the message's words with their places (one folded char per char of the message), quotes and end marks off
    let chars: Vec<char> = msg.chars().collect();
    let mut toks: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() { i += 1; continue; }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() { i += 1; }
        let (mut a, mut b) = (start, i);
        let edge = |c: char| "\"'`‘’“”,;:?!.()[]".contains(c);
        while a < b && edge(chars[a]) { a += 1; }
        while b > a && edge(chars[b - 1]) { b -= 1; }
        if a < b { toks.push((chars[a..b].iter().map(|&c| crate::reply_lang::fold_mark(c)).collect(), a, b)); }
    }
    // the words of `[here]` out
    let mut here: Vec<Vec<String>> = route_words("here").iter().map(|(_, f)| split(f)).collect();
    here.sort_by_key(|h| std::cmp::Reverse(h.len()));
    let mut k = 0;
    while k < toks.len() {
        match here.iter().find(|h| !h.is_empty() && toks.len() >= k + h.len() && toks[k..k + h.len()].iter().map(|t| &t.0).eq(h.iter())) {
            Some(h) => { toks.drain(k..k + h.len()); }
            None => k += 1,
        }
    }
    let words: Vec<&String> = toks.iter().map(|t| &t.0).collect();
    let mut best: Option<(usize, usize, usize)> = None;
    for (pre, suf) in frames {
        if suf.len() > words.len() || (!suf.is_empty() && !words[words.len() - suf.len()..].iter().copied().eq(suf.iter())) { continue; }
        let end = words.len() - suf.len();
        let starts: Vec<usize> = if pre.is_empty() { vec![0] } else {
            (0..=end.saturating_sub(pre.len())).filter(|&s| s + pre.len() <= end && words[s..s + pre.len()].iter().copied().eq(pre.iter())).collect()
        };
        if let Some(s) = starts.first() {
            let from = s + pre.len();
            if from < end && best.is_none_or(|(_, _, n)| pre.len() + suf.len() > n) { best = Some((from, end, pre.len() + suf.len())); }
        }
    }
    let (mut from, end, _) = best?;
    if let Some(d) = drop.iter().find(|d| end > from + d.len() && words[from..from + d.len()].iter().copied().eq(d.iter())) { from += d.len(); }
    if from >= end || end - from > 6 { return None; }
    Some(chars[toks[from].1..toks[end - 1].2].iter().collect())
}

/// Whether the router's file slot `file` belongs to this message: the message names it (its name, or its name
/// without the folder), or points back at a file (`[that_file]` of data/route_words.txt: "that file", "file đó").
/// A slot left from an earlier turn ("notes.txt" on "trong todo.txt, đổi 'buy milk' thành 'buy bread'") does not.
pub fn slot_named(msg: &str, file: &str) -> bool {
    let lower = msg.to_lowercase();
    let f = file.trim().to_lowercase();
    let base = f.rsplit('/').next().unwrap_or(&f).to_string();
    (!base.is_empty() && (holds_words(&lower, &f) || holds_words(&lower, &base))) || has_route_word(msg, "that_file")
}

/// A call of a file or shell tool on a time ask (the route is the clock line alone: "wonder what time it is in London
/// right now"): the time is already in this turn's clock line, so the call is not run (a shell `date -d` failed in its
/// place) and the note says to answer from that line. None for any other call or turn.
pub fn time_call_kept_out(name: &str, server: Option<&str>, route: &Route) -> Option<String> {
    let clock_only = route.groups.has(TIME) && route.groups.0 & !(TIME | TIME_TOOLS | READ_ONLY) == 0;
    (clock_only && tool_group(name, server) & CODE != 0).then(|| format!("Not run: the time asked for is already in this turn's clock line, \
        so `{name}` does not apply. Answer from that line."))
}

/// A tool call this turn's words keep out (`web_call_kept_out`, `change_kept_out`, `time_call_kept_out`): not widened
/// to, refused with the note.
pub fn call_kept_out(name: &str, server: Option<&str>, route: &Route, msg: &str) -> Option<String> {
    web_call_kept_out(name, server, route, msg).or_else(|| change_kept_out(name, server, route)).or_else(|| time_call_kept_out(name, server, route))
}

/// A program the user names to use ("how much disk space is free here? use df", "count them with wc", "đếm bằng wc"):
/// the word after a `[command]` word of `data/route_words.txt` that is a program on this machine's PATH and no word
/// of its dictionary (`program_named`), as written. None otherwise ("use python" names a language, "with cat" a word).
pub fn named_command(msg: &str) -> Option<String> {
    let toks: Vec<&str> = msg.split_whitespace().collect();
    let leads = route_words("command");
    toks.windows(2).find_map(|p| {
        let lead = fold_marks(p[0].trim_matches(|c: char| !c.is_alphanumeric()));
        if !leads.iter().any(|(w, f)| lead == *w || lead == *f) { return None; }
        let prog = p[1].trim_matches(|c: char| !(c.is_ascii_alphanumeric() || "._-".contains(c)));
        program_named(prog).then(|| prog.to_string())
    })
}

/// Whether `w` reads as a program's name: letters, digits and `._-` (not an option), a program of that name in a
/// folder of PATH, and no word of this machine's dictionary (`/usr/share/dict/words`: "df", "wc", "grep" are programs,
/// "cat", "find", "python" are words first). (`repl/turn.rs` reads the same files for Elisa's own shell asks.)
fn program_named(w: &str) -> bool {
    (2..=12).contains(&w.len()) && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)) && !w.starts_with('-')
        && !dictionary().contains(w) && on_path(w)
}

/// The words of this machine's dictionary (`/usr/share/dict/words`), lowercase; empty when it has none.
fn dictionary() -> &'static std::collections::HashSet<String> {
    static D: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    D.get_or_init(|| std::fs::read_to_string("/usr/share/dict/words").map(|t| t.lines().map(|l| l.trim().to_lowercase()).collect()).unwrap_or_default())
}

/// A program of that name in a folder of PATH.
fn on_path(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty())
        .any(|d| std::fs::metadata(std::path::Path::new(d).join(cmd)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// A `bash` call whose command is one bare word lifted from the user's own sentence ("starving" from "… and now I'm
/// starving"): no option, no path, and no program of that name on this machine. The model took a word for a command;
/// the call is refused with this note (the wrong-tool check, `repl/agent_loop.rs`). None for any other call, and for
/// a command the user asked the shell to run (`repl::shell_ask`: the shell's own "command not found" answers that).
pub fn word_as_command(name: &str, input: &serde_json::Value, msg: &str) -> Option<String> {
    if name != "bash" { return None; }
    let cmd = input.get("command").and_then(|c| c.as_str())?.trim();
    let word = cmd.to_lowercase();
    if word.is_empty() || word.starts_with('-') || !word.chars().all(|c| c.is_alphanumeric() || c == '\'') || on_path(&word) { return None; }
    (words(msg).contains(&word) && !crate::repl::shell_ask(msg))
        .then(|| format!("Not run: `{cmd}` is a word of the user's message, not a command on this machine. Answer the message in words."))
}

/// Whether the user writes Vietnamese, with or without its marks (`reply_lang::detect`: one place for every path).
pub fn vietnamese(msg: &str) -> bool { crate::reply_lang::detect(msg) == Some(crate::reply_lang::Lang::Vi) }

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

/// Elisa's own short reply to the small talk `msg`, in its language (Vietnamese when it is: `reply_lang::detect`), of
/// its kind: the `[thanks]` or `[bye]` section of `data/small_talk.txt` with the longest example whose words it all
/// has, else a feeling or a bit of the day told (`reply_check::tells_a_moment`: `[reply_sharing]`), else a greeting;
/// the line from that kind's `[reply_…]` section.
pub fn small_talk_line(msg: &str) -> &'static str { small_talk_line_at(msg, false) }

/// `small_talk_line`, and when `first` (no earlier turn in the chat) a greeting gets the `[reply_start]` line ("Hi!
/// What can I do for you?"), never one that asks what comes next.
pub fn small_talk_line_at(msg: &str, first: bool) -> &'static str {
    let w: std::collections::HashSet<String> = words(&msg.trim().to_lowercase()).into_iter().collect();
    // (an example all of whose words the message has; the longest one decides)
    let shared = |k: &str| small_talk_examples(k).iter().map(|e| words(&e.to_lowercase())).filter(|e| e.iter().all(|x| w.contains(x))).map(|e| e.len()).max().unwrap_or(0);
    // (a check-in asking how the assistant is, "bạn khỏe không?", is answered as one: "Mình khỏe, …")
    let kind = ["thanks", "bye", "wellbeing"].into_iter().map(|k| (shared(k), k)).filter(|(n, _)| *n > 0).max_by_key(|(n, _)| *n).map_or("greeting", |(_, k)| k);
    let kind = if kind == "greeting" && crate::reply_check::tells_a_moment(msg) { "sharing" } else { kind };
    let lang = if crate::reply_lang::detect(msg) == Some(crate::reply_lang::Lang::Vi) { "vi:" } else { "en:" };
    let pick = |k: &str| small_talk_examples(&format!("reply_{k}")).into_iter().find_map(|l| l.strip_prefix(lang)).map(str::trim);
    let kind = if first && kind == "greeting" { "start" } else { kind };
    pick(kind).or_else(|| pick("greeting")).unwrap_or_default()
}

/// Whether `msg` opens with a greeting word ("hello, can you help me with something?").
pub fn opens_with_greeting(msg: &str) -> bool { words(&msg.trim().to_lowercase()).first().is_some_and(|f| GREETINGS.contains(&f.as_str())) }

/// A short reply that carries the last turn on ("ok", "continue", "tiếp đi").
pub fn is_follow_up(msg: &str) -> bool { FOLLOW_UPS.contains(&words(msg.trim()).join(" ").as_str()) }

/// The message continues the last turn's work ("now also add X", "it still fails").
fn continues(lower: &str, w: &[String]) -> bool {
    // a short message about "it" / "this" / "that file": the thing the last turn worked on ("what license does it use?");
    // "this folder" / "this project" / "in here" (`[here]` of data/route_words.txt) name the workspace, not that thing
    let rest = words(&without_route_words(lower, "here"));
    let refers = w.len() <= 12 && rest.iter().any(|x| matches!(x.as_str(), "it" | "its" | "it's" | "this" | "that" | "these" | "those" | "them" | "they"));
    refers || w.first().is_some_and(|f| CONT_STARTS.contains(&f.as_str())) || CONT_PHRASES.iter().any(|p| lower.contains(p))
}

/// This turn's groups from the user's words and the last turn's route (`prev`; its groups carry a follow-up on).
/// `servers`: the other MCP / plugin servers of the session (routed when their name is in the user's words).
pub fn classify(msg: &str, prev: &Route, servers: &[String]) -> Route { read_only_for(classify_words(msg, prev, servers), msg) }

/// `classify` without the read-only rule (`read_only_for`).
fn classify_words(msg: &str, prev: &Route, servers: &[String]) -> Route {
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
    // a message that only tells facts about the user ("My name is Linh, I live in Hanoi, my meeting code is
    // ORCHID-42"): chat, whatever words its values hold ("code" there is no code), so no tool is offered to act on it.
    // Only the user's whole typed message (`own_words`), never a paste with a fact line in it, and no question in it
    // (the fact check leaves questions out).
    if tells_own_facts(t) {
        return mk(Groups::default(), others, Why::Chat);
    }
    // a question about what Elisa can do ("can you search the web?", "bạn có đọc được file trên máy mình không?"):
    // about her, decided before any tool signal in it (`about_elisa`; her own line answers it, no tool runs)
    if about_elisa(t) { return mk(Groups::default(), others, Why::Talk); }
    let g = signals(t);
    let chat = chatty(&lower, &w);
    // small talk by meaning, a greeting, a check-in or how the user's day went ("I went for a run this morning and
    // now I'm starving"): chat, whatever the last turn worked on, so no follow-up carry keeps its tools
    let talk = g.is_empty() && small_talk(t);
    // a time ask in any wording ("wonder what time it is in London right now": "it" there is no thing of the last
    // turn): the clock line alone, never the last turn's tools (a shell `date` failed in its place)
    let time_ask = !g.is_empty() && g.0 & !(TIME | TIME_TOOLS) == 0;
    let cont = continues(&lower, &w) && !prev.groups.is_empty() && !chat && !talk && !time_ask;
    if !g.is_empty() {
        return if cont { mk(Groups(g.0 | prev.groups.0), carried(), Why::FollowUp) } else { mk(g, others, Why::Signals) };
    }
    if !others.is_empty() { return mk(g, others, Why::Signals); }
    if talk { return mk(Groups::default(), others, Why::Talk); }
    if cont { return mk(prev.groups, prev.others.clone(), Why::FollowUp); }
    // (a question anywhere in it, not only at its end: "… how many are left? think it through" is a question)
    if chat || is_question(t, &w) || t.contains('?') || w.len() <= 12 { return mk(Groups::default(), others, Why::Chat); }
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
    read_only_for(Route { groups: g, others, why: Why::Router }, msg)
}

/// A files turn on an ask that only asks or checks (`check_only`: "file app.py có lỗi gì không? xem hàm add", "is
/// there a mistake in app.py?") gets the file tools read-only (`READ_ONLY`); one that asks for a change keeps them all.
/// Never carried from the last turn: each message decides for itself.
fn read_only_for(mut r: Route, msg: &str) -> Route {
    r.groups = Groups(r.groups.0 & !READ_ONLY);
    if r.groups.has(CODE) && check_only(msg) { r.groups = r.groups.with(READ_ONLY); }
    r
}

/// An ask that only asks or checks: a question (a question mark, an English question opening, a Vietnamese question
/// word of data/person_words.txt) with no word of a change (`[change]` of data/route_words.txt: fix, sửa, đổi, thêm,
/// lưu, …). "can you fix the add function?" asks for a change; "is there a mistake in app.py?" does not.
pub fn check_only(msg: &str) -> bool {
    let t = msg.trim();
    let w = words(t);
    let vi_question = vietnamese(t) && { let plain = words(&fold_marks(t)).join(" "); person_words().1.iter().any(|p| holds_words(&plain, p)) };
    !w.is_empty() && (t.contains('?') || is_question(t, &w) || vi_question) && !has_route_word(t, "change")
}

/// The tools that change files (Elisa's own tools of the write class, `tools::access`): left out of a read-only turn.
pub fn changes_files(name: &str, server: Option<&str>) -> bool { server.is_none() && crate::tools::access(name) == crate::tools::Access::Write }

/// A call of a tool that changes files on a read-only turn (`READ_ONLY`): not run, and the note says why.
pub fn change_kept_out(name: &str, server: Option<&str>, route: &Route) -> Option<String> {
    (route.groups.has(READ_ONLY) && changes_files(name, server)).then(|| format!("Not run: the user only asked or asked to check, and asked for no change, \
        so `{name}` does not apply. Answer from what was read; say what you would change, and the user decides."))
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
        // a zone named after part of a longer place ("Christmas" for Christmas Island, "Easter" for Easter Island:
        // the zone database's own place name) is named by that whole place only, so a holiday is not a place
        let full = place_names();
        for (n, z) in v.iter_mut() {
            if let Some(p) = full.get(z.as_str()).filter(|p| p.starts_with(&format!("{n} "))) { *n = p.clone(); }
        }
        v.sort();
        v
    })
}

/// zone -> its place's full name, lowercased, from the zone database's tables: the zone's own comment in
/// `zone.tab` ("Easter Island"), else its country in `iso3166.tab` ("Christmas Island").
fn place_names() -> std::collections::HashMap<String, String> {
    let read = |f: &str| std::fs::read_to_string(format!("/usr/share/zoneinfo/{f}")).unwrap_or_default();
    let countries: std::collections::HashMap<String, String> = read("iso3166.tab").lines().filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t')).map(|(c, n)| (c.to_string(), n.trim().to_lowercase())).collect();
    read("zone.tab").lines().filter(|l| !l.starts_with('#')).filter_map(|l| {
        let f: Vec<&str> = l.split('\t').collect();
        let name = f.get(3).map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty()).or_else(|| countries.get(*f.first()?).cloned())?;
        Some((f.get(2)?.to_string(), name))
    }).collect()
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

/// The words that name `zone` in a message (`zones_named`'s aliases and its zoneinfo city), lowercased.
pub fn zone_words(zone: &str) -> Vec<String> {
    let mut v: Vec<String> = ZONE_ALIASES.iter().filter(|(_, z)| *z == zone).map(|(n, _)| n.to_string()).collect();
    v.extend(zone_cities().iter().filter(|(_, z)| z == zone).map(|(n, _)| n.clone()));
    v
}

/// `Time in Tokyo now: 2026-11-09 15:16 Monday (UTC+09:00, Asia/Tokyo)`, from the system's zone database (`date`
/// with that `TZ`: no environment change in this process). None when the zone is unknown there.
fn time_in(zone: &str) -> Option<String> {
    let o = std::process::Command::new("date").env("TZ", zone).arg("+%Y-%m-%d %H:%M %A %z").output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let (rest, off) = s.rsplit_once(' ')?;
    if !o.status.success() || off.len() != 5 { return None; }
    let city = zone.rsplit('/').next().unwrap_or(zone).replace('_', " ");
    // the part of the day there, from its clock (as `local_now`)
    let hour = rest.split_whitespace().nth(1).and_then(|t| t.split(':').next()?.parse::<u32>().ok());
    let part = hour.map(|h| format!(" {}", crate::situation::part_of_day(h))).unwrap_or_default();
    Some(format!("Time in {city} now: {rest}{part} (UTC{}:{}, {zone})", &off[..3], &off[3..]))
}

/// The clock lines for a turn about the time: the local time, then each place the message names.
/// A place named right after a time turn ("and in Tokyo?", "còn ở Tokyo thì sao?") is a time ask too, when the turn
/// asks for nothing else: a turn whose own words or the router's answer call for tools ("search the web: what is the
/// capital of Australia?") is that ask, and a clock line for Sydney in it was read out as the answer. (A route kept
/// from the last turn or the default set is no ask of its own.)
pub fn place_after_time(route: &Route, msg: &str, after_time: impl FnOnce() -> bool) -> bool {
    let own_ask = matches!(route.why, Why::Signals | Why::Router) && !route.groups.tool_free();
    !own_ask && !zones_named(msg).is_empty() && after_time()
}

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

    #[test]
    fn elisas_own_small_talk_line_fits_its_kind() {
        assert_eq!(small_talk_line("thanks!"), "You're welcome! Anything else I can help with?");
        assert_eq!(small_talk_line("cảm ơn bạn nhé"), "Không có gì! Bạn cần mình giúp gì nữa không?");
        assert_eq!(small_talk_line("ok bye, see you"), "Bye! Talk to you soon.");
        assert_eq!(small_talk_line("hey"), "Hi! What would you like to do next?");
        // (asked how Elisa is, she says so: a greeting alone does not answer it)
        assert_eq!(small_talk_line("Xin chào, bạn khỏe không?"), "Mình khỏe, cảm ơn bạn! Bạn cần mình giúp gì?");
        assert_eq!(small_talk_line("how are you?"), "I'm doing well, thanks! What can I do for you?");
        // the chat's first greeting asks nothing about what comes next
        assert_eq!(small_talk_line_at("hey", true), "Hi! What can I do for you?");
        assert_eq!(small_talk_line_at("chào bạn", true), "Chào bạn! Bạn cần mình giúp gì?");
        assert_eq!(small_talk_line_at("thanks!", true), "You're welcome! Anything else I can help with?");
    }

    use super::*;

    fn route(msg: &str) -> Groups { Groups(classify(msg, &Route::default(), &[]).groups.0 & !READ_ONLY) }

    #[test]
    fn a_question_inside_a_longer_message_is_chat() {
        // (routed to the web default set, the word-problem step skipped it)
        assert!(route("if I have 3 boxes with 7 apples and give away 5 apples, how many are left? think it through").tool_free());
        assert!(route("what is 12.5% of 3,480 plus 19 squared?").tool_free());
    }

    /// a place named right after a time turn is a time ask only when the turn asks for nothing else
    #[test]
    fn a_web_search_naming_a_country_after_the_time_is_no_time_ask() {
        if !std::path::Path::new("/usr/share/zoneinfo/zone.tab").exists() { return; }
        let r = |g: u16, why: Why| Route { groups: Groups(g), others: Vec::new(), why };
        assert!(place_after_time(&r(0, Why::Router), "and in Tokyo?", || true));
        assert!(place_after_time(&r(TIME, Why::Router), "and in Tokyo?", || true));
        // (a route kept from the last turn, or the default set: no ask of its own)
        assert!(place_after_time(&r(WEB, Why::Unclear), "and in Tokyo?", || true));
        assert!(place_after_time(&r(WEB, Why::FollowUp), "and in Tokyo?", || true));
        assert!(!place_after_time(&r(WEB, Why::Router), "search the web: what is the capital of Australia?", || true));
        assert!(!place_after_time(&r(WEB, Why::Signals), "search the web: what is the capital of Australia?", || true));
        assert!(!place_after_time(&r(0, Why::Chat), "and in Tokyo?", || false));
        assert!(!place_after_time(&r(0, Why::Chat), "tell me a joke", || true));
    }

    #[test]
    fn a_holiday_is_not_a_place() {
        // (the zone database names Indian/Christmas after Christmas Island: only the whole place name counts)
        if !std::path::Path::new("/usr/share/zoneinfo/zone.tab").exists() { return; }
        assert!(zones_named("how many days until Christmas?").is_empty());
        assert_eq!(zones_named("what time is it on Christmas Island?"), vec!["Indian/Christmas".to_string()]);
        assert_eq!(zones_named("and in Tokyo?"), vec!["Asia/Tokyo".to_string()]);
    }

    /// `meaning_scores` at a cache overflow still scores every wanted text (it cleared the cache after noting what was
    /// missing, so the texts cached earlier came back None: one miss at every overflow, also in a long session).
    #[test]
    fn meaning_scores_past_the_cache_limit_scores_every_text() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        let full = |ask: &str, texts: &[&str]| {
            let v = meaning_scores_in(ask, texts, 3).expect("the model is installed");
            assert_eq!(v.len(), texts.len(), "{ask:?}");
            assert!(v.iter().all(|x| x.is_finite()), "{v:?}");
            v
        };
        let first = full("read the file", &["open a document", "search the web"]);
        full("run a command", &["open a document", "write a poem"]);
        // past the limit now, and the texts wanted here were cached by the first call
        let again = full("read the file", &["open a document", "search the web", "tell a joke"]);
        assert!(first.iter().zip(&again).all(|(a, b)| (a - b).abs() < 1e-5), "{first:?} {again:?}");
    }

    #[test]
    fn web_and_run_asks_by_meaning_in_vietnamese() {
        // held-out wordings (none is an example line), accented, unaccented and with a typo
        if elisa_mem::embed::model_dir().is_none() { return; }
        for m in ["tìm trên mạng: thủ đô của Úc là gì?", "tim tren mang: thu do cua Uc la gi?", "tim tren mnag: thu do Uc la gi?",
            "tra trên mạng giúp mình: dân số Nhật Bản", "lên google tìm xem ai là tổng thống Mỹ"] {
            assert_eq!(tool_ask_by_meaning(m), Some("web"), "{m:?}");
            assert!(web_ask(m), "{m:?}");
        }
        for m in ["chạy đoạn python này: print(sum(range(11)))", "chay doan python nay: print(sum(range(11)))", "chạy đaon python này: print(2)", "chay giup minh doan code nay: print(2)"] {
            assert_eq!(tool_ask_by_meaning(m), Some("run"), "{m:?}");
        }
        for m in ["thủ đô của Úc là gì?", "thu do cua Uc la gi?", "what is the capital of Australia?", "mạng nhà mình chậm quá", "bạn có biết chạy bộ không?",
            "python có khó không?", "hôm nay trời đẹp quá", "tim trong du an chu Hello", "who won the world cup in 2022?", "explain recursion in python", "chào bạn",
            // today's date or weekday: the clock line's, not today's price or news
            "hôm nay là ngày bao nhiêu?", "hom nay la ngay bao nhieu?", "hôm nay là thứ mấy?", "what's today's date?"] {
            assert_eq!(tool_ask_by_meaning(m), Some(""), "{m:?}");
        }
    }

    #[test]
    fn vietnamese_without_marks_is_vietnamese() {
        for m in ["tim tren mang: thu do cua Uc la gi?", "chay doan python nay: print(sum(range(11)))", "tìm trên mạng: thủ đô của Úc là gì?"] { assert!(vietnamese(m), "{m:?}"); }
        for m in ["run this python: print(2)", "search the web: what is the capital of Australia?", "print(sum(range(11)))"] { assert!(!vietnamese(m), "{m:?}"); }
    }

    #[test]
    fn a_help_offer_is_not_about_elisa() {
        assert!(!about_elisa("hello, can you help me with something?"));
        assert!(about_elisa("what can you do?"));
    }

    #[test]
    fn tool_actions_in_plain_words() {
        assert_eq!(tool_action("Search the web; returns the top results (title, url, snippet). For news.").as_deref(), Some("search the web"));
        assert_eq!(tool_action("Find leaked secrets in project files (masked).").as_deref(), Some("find leaked secrets in project files"));
        assert_eq!(tool_action("Read a file, line-numbered.").as_deref(), Some("read a file"));
        assert_eq!(tool_action("Known vulnerabilities in lockfile deps. Sends names+versions to osv.dev."), None);
        assert_eq!(tool_action("Signatures and line ranges of a code file (dir: names per file)."), None);
        assert_eq!(tool_action("A symbol's source by name, e.g. Type::method."), None);
    }

    #[test]
    fn elisa_notes_never_stay_in_a_reply() {
        let r = "Done.\n[elisa: counted exactly: notes.txt has 8 words; use this number]\nnotes.txt has 8 words. [elisa: this exact call has now returned this same result 3 times]";
        assert_eq!(strip_echoed_notes(r), "Done.\nnotes.txt has 8 words.");
        assert_eq!(strip_echoed_notes("plain [x] text"), "plain [x] text");
    }

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
        for m in ["what is the capital of France?", "what is google's revenue?", "search the codebase for TODO", "latest news about Apple", "hi", "is the site online?",
                  // a question about today's date or weekday asks the clock line (`date_ask`), never a search
                  "what's today's date?", "what day is it today?", "hôm nay là ngày bao nhiêu?", "hom nay la ngay bao nhieu?", "hôm nay là thứ mấy?"] {
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

    /// telling facts about oneself is chat: no tool to act on it, whatever words the facts hold
    #[test]
    fn a_self_introduction_is_chat() {
        for m in ["My name is Linh, I live in Hanoi, my favourite colour is green and my meeting code is ORCHID-42.",
            "My name is Linh and I live in Hanoi.", "Hi! I'm Linh, I work as a backend developer and my project code is P-7."] {
            let r = classify(m, &Route::default(), &[]);
            assert!(r.groups.is_empty() && r.why == Why::Chat, "{m:?} -> {}", r.describe());
            let code = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
            assert!(classify(m, &code, &[]).groups.is_empty(), "{m:?} after code work");
        }
        // facts with an ask in them keep the ask's tools
        assert!(route("My name is Linh. Search the web for the weather in Hanoi.").has(WEB));
        assert!(route("My meeting code is ORCHID-42, save it to notes.txt").has(CODE));
        assert!(route("My name is Linh. What's the latest news in Hanoi?").has(WEB));
    }

    /// By meaning (when the embedding model is installed): greetings and check-ins in English and Vietnamese, with
    /// and without accents, none of them in the example data; asks are not small talk.
    #[test]
    fn small_talk_by_meaning_reads_en_and_vi_check_ins() {
        if small_talk_by_meaning("hi").is_none() { return; }
        for m in ["ê, rảnh ko?", "e dang ranh ko", "hey, you free?", "yo, you there?", "hi Elisa!", "good afternoon", "chào em",
            "xin chào, bạn khỏe không?", "ê, có đó không?", "hello again", "chao buoi toi", "how are you?", "what's new with you?", "what's up",
            "what's up with you?", "sup", "yo sup", "bạn ơi", "ơi bạn", "bạn ơi, có đó không", "ban oi", "ban oi co do ko"] {
            assert_eq!(small_talk_by_meaning(m), Some(true), "{m:?}");
        }
        for m in ["tell me a story", "what time is it in Tokyo?", "who wrote Hamlet?", "open README.md", "why?", "ok", "write a haiku about rain",
            "what is 12 * 7?", "are you sure?", "dịch giúp mình đoạn này", "viết một câu chuyện ngắn", "bây giờ là mấy giờ?", "giải bài toán này",
            "thủ đô của Pháp là gì?", "tim tin tuc moi nhat", "hey can you read README.md and summarize the install steps for me",
            "what's new in Python 3.13?", "what's new in the docs?", "anything new in the news today?", "what's up with my code?",
            "bạn ơi, dịch giúp mình câu này", "ban oi viet giup minh bai tho"] {
            assert_eq!(small_talk_by_meaning(m), Some(false), "{m:?}");
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
            // each clock line says its part of the day, from its own hour
            for line in l.lines() {
                let hour: u32 = line.split_whitespace().find(|w| w.len() == 5 && w.as_bytes()[2] == b':').and_then(|t| t[..2].parse().ok()).unwrap();
                assert!(line.contains(&format!(" {} (UTC", crate::situation::part_of_day(hour))), "{line}");
            }
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

    /// a program named to use is a shell turn (the file / code tools), whatever question comes before it
    #[test]
    fn a_program_named_to_use_is_a_shell_signal() {
        if !on_path("df") || dictionary().is_empty() { return; }
        for (m, prog) in [("how much disk space is free here? use df", "df"), ("count the lines with wc", "wc"), ("đếm số dòng bằng wc", "wc"), ("dem so dong bang wc", "wc"),
            ("check it via `grep`", "grep")] {
            assert_eq!(named_command(m).as_deref(), Some(prog), "{m:?}");
            assert!(route(m).has(CODE), "{m:?}: {}", route(m).describe());
        }
        // a language, a word, an option, a capitalised name: no program named
        for m in ["use python to rename the photos", "what's with cat videos", "I went with Du to the market", "try -v", "use it", "how much disk space is free here?"] {
            assert_eq!(named_command(m), None, "{m:?}");
        }
        assert!(route("how much disk space is free here?").tool_free());
    }

    /// casual and misspelt time asks are time turns, by the asks of data/clock_words.txt
    #[test]
    fn casual_time_asks_are_time() {
        for m in ["got the time?", "wats the time rn", "what's the time?", "time check", "mấy giờ rồi?", "may gio roi"] {
            assert_eq!(route(m), Groups(TIME), "{m:?}: {}", route(m).describe());
        }
        for m in ["do you have time to help me?", "I had a great time", "time flies"] { assert!(!route(m).has(TIME), "{m:?}"); }
    }

    /// a question about today's facts in Vietnamese (a price, the weather), with or without its marks, is a web turn
    #[test]
    fn vietnamese_current_facts_are_web() {
        for m in ["giá vàng hôm nay thế nào nhỉ", "thời tiết Hà Nội hôm nay thế nào?", "gia vang hom nay the nao nhi", "thoi tiet Ha Noi hom nay the nao?", "tin tức mới nhất về Apple"] {
            assert_eq!(route(m), Groups(WEB), "{m:?}: {}", route(m).describe());
        }
        // a Vietnamese word without its marks is no English word: "ham" (hàm) in English is ham
        for m in ["I love ham and eggs", "hôm nay mệt quá", "cảm ơn nhé"] { assert!(!route(m).has(WEB), "{m:?}"); }
    }

    /// Vietnamese file words count, with or without their marks; a .csv in the words is a file read, not a document
    #[test]
    fn vietnamese_file_words_and_plain_tables_are_file_turns() {
        for m in ["tim giup minh file nao co chu ORCHID", "tìm giúp mình file nào có chữ ORCHID", "tìm giúp mình tệp nào có chữ ORCHID", "tim giup minh tep nao co chu ORCHID",
            "liệt kê các tệp trong thư mục này"] {
            assert_eq!(route(m), Groups(CODE), "{m:?}: {}", route(m).describe());
        }
        for m in ["không biết trong sales.csv vùng nào bán nhiều nhất nhỉ", "which region sold the most in sales.csv?", "export the three region names from sales.csv to regions.txt"] {
            assert_eq!(route(m), Groups(CODE), "{m:?}: {}", route(m).describe());
            assert!(!crate::mcp_reason::about_docs(m), "{m:?}");
        }
        assert_eq!(route("summarise report.xlsx"), Groups(DOCS));
        assert!(crate::mcp_reason::about_docs("convert the sheet to csv"));
    }

    /// a bash command that is one bare word of the user's sentence is no command
    #[test]
    fn a_bare_word_of_the_message_is_no_command() {
        let call = |c: &str| serde_json::json!({"command": c});
        let msg = "I went for a run this morning and now I'm starving";
        assert!(word_as_command("bash", &call("starving"), msg).is_some_and(|w| w.contains("`starving`")));
        assert!(word_as_command("bash", &call("Starving"), msg).is_some());
        assert_eq!(word_as_command("bash", &call("df"), "how much disk space is free here? use df"), None);
        assert_eq!(word_as_command("bash", &call("ls -la"), msg), None);
        assert_eq!(word_as_command("bash", &call("./starving"), msg), None);
        assert_eq!(word_as_command("read", &serde_json::json!({"path": "starving"}), msg), None);
        // asked of the shell as written: the shell's own "command not found" answers that
        assert_eq!(word_as_command("bash", &call("starving"), "run starving in the shell"), None);
    }

    /// small talk by meaning keeps no tools from the last turn, and short follow-ups still do
    #[test]
    fn small_talk_keeps_no_tools_from_the_last_turn() {
        if small_talk_by_meaning("hi").is_none() { return; }
        let web = Route { groups: Groups(WEB), others: Vec::new(), why: Why::Signals };
        let code = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
        for m in ["I went for a run this morning and now I'm starving", "mình vừa chạy bộ xong", "I'm hungry", "just got home from work, exhausted", "hom nay troi dep qua", "hey"] {
            let r = classify(m, &web, &[]);
            assert!(r.groups.is_empty() && r.why == Why::Talk, "{m:?} after web: {}", r.describe());
            assert!(small_talk(m), "{m:?}");
        }
        for m in ["and South?", "what else is in that file?", "why is that slow?", "the second one", "what license does it use?"] {
            assert!(classify(m, &code, &[]).groups.has(CODE), "{m:?}");
            assert!(!small_talk(m), "{m:?}");
        }
    }

    /// a question about what Elisa can do is about her, decided before the tool its words name; an ask with an
    /// object, a file or "it" is the ask
    #[test]
    fn ability_questions_are_about_elisa() {
        if small_talk_by_meaning("hi").is_none() { return; }
        for m in ["can you search the web?", "bạn có đọc được file trên máy mình không?", "ban co len mang duoc khong?", "bạn có lên mạng được không?", "bạn có vào mạng được không?",
            "are you able to run shell commands?", "do you have internet access?", "can you read my files?"] {
            assert!(about_elisa(m), "{m:?}");
            let r = classify(m, &Route::default(), &[]);
            assert!(r.groups.is_empty() && r.why == Why::Talk, "{m:?}: {}", r.describe());
            assert!(!web_ask(m) && signals(m).is_empty(), "{m:?}");
        }
        for m in ["can you search the web for the latest news about Apple?", "can you google it?", "can you look it up?", "can you read notes.txt?", "search the web",
            "can you help me with something?", "can you write a poem?", "do you know what time it is?", "bạn tìm giúp mình thời tiết Hà Nội được không?", "what's in this directory?",
            // (questions of the same shape that are small talk or a plain question: the word only the ability examples have is missing)
            "bạn có khỏe không?", "bạn có rảnh không?", "bạn có thích mùa đông không?", "ban co khoe khong?"] {
            assert!(!about_elisa(m), "{m:?}");
        }
        assert!(route("can you search the web for the latest news about Apple?").has(WEB));
        assert!(web_ask("can you google it?"));
    }

    /// a date or weekday question asks the clock, not the news: time only, whatever "today's" says
    #[test]
    fn date_and_weekday_asks_are_time_only() {
        for m in ["what's today's date?", "what is today's date", "what day is it today?", "what's the date today?", "hôm nay là ngày mấy?", "hom nay la thu may?",
            "what day of the week is it?", "how many days until Christmas?"] {
            assert!(date_ask(m), "{m:?}");
            assert_eq!(route(m), Groups(TIME), "{m:?}: {}", route(m).describe());
        }
        for m in ["what's in the news today?", "today's headlines please", "what's the weather today?"] {
            assert!(!date_ask(m) && route(m).has(WEB) && !route(m).has(TIME), "{m:?}: {}", route(m).describe());
        }
        assert!(!date_ask("what time is it?") && route("what time is it?").has(TIME));
    }

    /// a question about the files of this folder, and one naming "that file" after a file turn, are file turns
    #[test]
    fn questions_about_the_files_here_are_file_turns() {
        for m in ["which file in this folder mentions a meeting code?", "how many files are here?", "what's in this directory?", "which files here are the biggest?",
            "file nào trong thư mục này có chữ ORCHID?", "file nao trong thu muc nay co chu ORCHID?", "what else is in that file?", "what's the first line of that file?",
            "trong file đó còn gì nữa?"] {
            assert!(route(m).has(CODE) && !route(m).has(WEB), "{m:?}: {}", route(m).describe());
            assert!(names_files(m), "{m:?}");
        }
        let code = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
        assert!(classify("what else is in that file?", &code, &[]).groups.has(CODE));
        // "this folder" names the workspace, not the last turn's thing: a fresh file turn after a date ask or a web
        // turn, nothing carried on; "that file" after a file turn carries it on
        for prev in [TIME, WEB] {
            let last = Route { groups: Groups(prev), others: Vec::new(), why: Why::Signals };
            let r = classify("which file in this folder mentions a meeting code?", &last, &[]);
            assert_eq!((r.groups.0 & !READ_ONLY, r.why), (CODE, Why::Signals), "{}", r.describe());
        }
        let r = classify("what else is in that file?", &code, &[]);
        assert_eq!((r.groups.0 & !READ_ONLY, r.why), (CODE, Why::FollowUp), "{}", r.describe());
        for m in ["what is a file?", "which file format is best for photos?", "what's the weather here?"] { assert!(!route(m).has(CODE), "{m:?}: {}", route(m).describe()); }
    }

    /// a web tool called on a files turn whose words name a file, or on a plain statement, is refused, not widened to
    #[test]
    fn a_web_call_is_kept_out_of_file_turns_and_statements() {
        let code = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
        let both = Route { groups: Groups(CODE | WEB), others: Vec::new(), why: Why::Signals };
        let chat = Route::default();
        // the web tools live on the "web" MCP server (what `server_of` answers); grep is Elisa's own tool
        let web = Some("web");
        assert!(web_call_kept_out("web_search", web, &code, "tim giup minh file nao co chu ORCHID").is_some_and(|w| w.contains("grep")));
        assert!(web_call_kept_out("web_fetch", web, &code, "which file in this folder mentions a meeting code?").is_some());
        assert_eq!(web_call_kept_out("grep", None, &code, "tim giup minh file nao co chu ORCHID"), None);
        assert_eq!(web_call_kept_out("web_search", web, &both, "tim giup minh file nao co chu ORCHID"), None);
        assert_eq!(web_call_kept_out("web_search", web, &code, "fix the failing test"), None);
        // a statement: the user tells a fact or shares, asks nothing
        for m in ["My name is Linh and I live in Hanoi.", "my meeting code is ORCHID-42", "I'm so tired today"] {
            assert!(plain_statement(m), "{m:?}");
            assert!(web_call_kept_out("web_search", web, &chat, m).is_some_and(|w| w.contains("Answer in words")), "{m:?}");
            assert!(!route(m).has(WEB), "{m:?}");
        }
        for m in ["what's the population of Iceland?", "look up the population of Iceland", "population of Iceland please", "yes", "ok do it",
            "search the web for Linh Nguyen", "the weather in Hanoi tomorrow", "translate this into English"] {
            assert!(!plain_statement(m), "{m:?}");
            assert_eq!(web_call_kept_out("web_search", web, &chat, m), None, "{m:?}");
        }
        assert!(web_kept_out(&chat, "My name is Linh and I live in Hanoi.") && !web_kept_out(&chat, "what's the population of Iceland?"));
    }

    /// a Vietnamese line telling something about the user, with or without marks, is a plain statement too: chat, and
    /// no web call widens it; a Vietnamese question or ask is not
    #[test]
    fn vietnamese_self_statements_are_plain_statements() {
        let chat = Route::default();
        for m in ["Mình tên là Linh và mình sống ở Hà Nội.", "Minh ten la Linh va minh song o Ha Noi.", "Tôi tên là Nam, tôi làm kỹ sư ở Đà Nẵng.",
            "mình là sinh viên năm hai", "tớ thích mùa thu ở Hà Nội"] {
            assert!(plain_statement(m), "{m:?}");
            assert!(web_call_kept_out("web_search", Some("web"), &chat, m).is_some_and(|w| w.contains("Answer in words")), "{m:?}");
            let r = route(m);
            assert!(!r.has(WEB) && r.tool_free(), "{m:?}: {}", r.describe());
        }
        for m in ["Mình tên là gì?", "mình sống ở đâu", "tên mình là gì", "tìm giúp mình file nào có chữ ORCHID", "tim giup minh file nao co chu ORCHID",
            "mình muốn biết giá vàng hôm nay", "đọc giúp mình notes.txt", "cho mình xem thời tiết Hà Nội", "giá vàng hôm nay thế nào nhỉ", "mình có nên mang ô không"] {
            assert!(!plain_statement(m), "{m:?}");
            assert_eq!(web_call_kept_out("web_search", Some("web"), &chat, m), None, "{m:?}");
        }
    }

    /// an ask that only asks or checks gets the file tools read-only, in any language; one that asks for a change keeps
    /// them all; a model's change call on a read-only turn is not run
    #[test]
    fn a_check_question_gets_read_only_file_tools() {
        let ro = |m: &str| classify(m, &Route::default(), &[]).groups.has(READ_ONLY);
        for m in ["file app.py có lỗi gì không? xem hàm add", "file app.py co loi gi khong? xem ham add", "is there a mistake in app.py?", "does the add function in app.py work?",
            "hàm add trong app.py có đúng không?", "what does greet in app.py return?", "which file in this folder mentions a meeting code?", "app.py có bao nhiêu dòng?"] {
            assert!(check_only(m) && ro(m), "{m:?}");
        }
        for m in ["fix the add function in app.py", "can you fix the add function in app.py?", "sửa hàm add trong app.py giúp mình", "sua ham add trong app.py giup minh",
            "file app.py có lỗi gì không? sửa luôn nhé", "can you add a test to app.py?", "đổi tên hàm add thành plus trong app.py được không?", "save a summary of notes.txt to out.md",
            "lưu tóm tắt notes.txt vào tomtat.md nhé?"] {
            assert!(!ro(m), "{m:?}");
        }
        // not carried: a change ask after a check question has every file tool again
        let checked = classify("is there a mistake in app.py?", &Route::default(), &[]);
        assert!(!classify("ok fix it", &checked, &[]).groups.has(READ_ONLY));
        assert!(change_kept_out("edit_symbol", None, &checked).is_some() && change_kept_out("write", None, &checked).is_some());
        assert_eq!(change_kept_out("read_symbol", None, &checked), None);
        assert_eq!(change_kept_out("edit", None, &classify("fix app.py", &Route::default(), &[])), None);
        // the router's files_code label too
        let p = crate::router::Plan { groups: Groups(CODE), ..Default::default() };
        assert!(from_router(&p, "file app.py có lỗi gì không? xem hàm add", &Route::default(), Vec::new(), false, false).groups.has(READ_ONLY));
        assert!(!from_router(&p, "sửa hàm add trong app.py", &Route::default(), Vec::new(), false, false).groups.has(READ_ONLY));
    }

    /// a question about what Elisa can do is answered from her own tools whatever the reply said ("yes, if you upload
    /// it"), in other wordings and both languages
    #[test]
    fn ability_questions_get_the_verified_answer_in_many_wordings() {
        if elisa_mem::embed::model_dir().is_none() { return; }
        let tools: Vec<(String, String)> = crate::tools::defs_for(false).into_iter().map(|t| (t.name, t.description))
            .chain([("web_search".to_string(), "Search the web; returns the top results (title, url, snippet).".to_string())]).collect();
        for (m, yes) in [("bạn có đọc được file trên máy mình không?", "Có"), ("ban co doc duoc file tren may minh khong?", "Có"), ("bạn đọc được file trong máy mình không?", "Có"),
            ("can you read files on my computer?", "Yes"), ("are you able to read my files?", "Yes"), ("can you search the web?", "Yes"),
            ("bạn có tìm trên mạng được không?", "Có"), ("can you run shell commands?", "Yes")] {
            let a = crate::situation::ability_answer(m, "", &tools, false);
            assert!(a.as_deref().is_some_and(|a| a.starts_with(yes)), "{m:?}: {a:?}");
        }
        assert!(crate::situation::ability_answer("can you see images?", "", &tools, false).is_some_and(|a| a.starts_with("No")));
    }

    /// a which-file ask names the thing to search for, in either language, with the folder words and a leading "a" or
    /// "chữ" left out; other asks name none
    #[test]
    fn a_which_file_ask_names_the_thing_to_search_for() {
        for (m, want) in [("which file in this folder mentions a meeting code?", "meeting code"), ("which files contain TODO?", "TODO"),
            ("where is ORCHID-42 mentioned?", "ORCHID-42"), ("what file has the word Lotus in it?", "Lotus in it"), ("find the file that mentions \"buy milk\"", "buy milk"),
            ("file nào trong thư mục này có chữ ORCHID?", "ORCHID"), ("file nao trong thu muc nay co chu ORCHID?", "ORCHID"), ("mã cuộc họp nằm trong file nào?", "mã cuộc họp"),
            ("tìm file có chữ Lotus", "Lotus")] {
            assert_eq!(which_file_term(m).as_deref(), Some(want), "{m:?}");
        }
        for m in ["which file format is best for photos?", "what is a file?", "where is Hanoi?", "read notes.txt", "file app.py có lỗi gì không?"] {
            assert_eq!(which_file_term(m), None, "{m:?}");
        }
    }

    /// a router file slot runs only when the message names it or points back at a file
    #[test]
    fn a_router_file_slot_runs_only_when_the_message_names_it() {
        assert!(!slot_named("trong todo.txt, đổi 'buy milk' thành 'buy bread'", "notes.txt"));
        assert!(slot_named("trong todo.txt, đổi 'buy milk' thành 'buy bread'", "todo.txt"));
        assert!(slot_named("đọc file notes.txt và cho mình biết mã cuộc họp", "notes.txt") && slot_named("summarize docs/notes.txt", "notes.txt"));
        assert!(slot_named("what else is in that file?", "notes.txt") && slot_named("trong file đó còn gì?", "notes.txt"));
        assert!(!slot_named("what time is it?", "notes.txt") && !slot_named("rewrite the notes", "notes.txt"));
    }

    /// a time ask in any wording is the clock line alone, whatever the last turn was; a file or shell call on it is
    /// not run
    #[test]
    fn a_time_ask_never_carries_the_last_turns_tools() {
        let files = Route { groups: Groups(CODE), others: Vec::new(), why: Why::Signals };
        for m in ["wonder what time it is in London right now", "I wonder what time it is in Tokyo", "what time is it there in Paris?", "mấy giờ rồi ở London?"] {
            let r = classify(m, &files, &[]);
            assert!(r.groups.has(TIME) && !r.groups.has(CODE), "{m:?}: {}", r.describe());
            assert!(time_call_kept_out("bash", None, &r).is_some() && call_kept_out("read", None, &r, m).is_some(), "{m:?}");
        }
        assert_eq!(time_call_kept_out("bash", None, &files), None);
        assert!(classify("what does it print?", &files, &[]).groups.has(CODE), "a follow-up with no time ask still carries");
    }
}
