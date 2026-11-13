//! Pattern permissions: config `[permissions] allow = […], ask = […], deny = […]` with rules like `shell(git status *)`,
//! `write(src/**)`, `read(**/*.env)`, `mcp:browser:navigate(https://github.com/*)`.
//!
//! - Precedence: deny > ask > allow > the usual behaviour (auto_approve, `.elisa/allow`, the prompt). A deny refuses the
//!   call before anyone is asked; an ask always asks (no `--yes`, no "always", and with nobody to ask it is denied); an
//!   allow runs it without asking. Elisa's hard rules still come first: an unknown tool is refused before any rule, a
//!   call outside the project goes through its own prompt, a changed MCP tool list asks every time, and the shell
//!   sandbox wraps every command whatever a rule says.
//! - Kinds: `shell(CMD)` (also `bash(…)`) for the bash tool; `read(PATH)` for read / outline / read_symbol and the
//!   `path` of glob / grep; `write(PATH)` (also `edit(…)`) for write / edit / edit_symbol; `mcp:SERVER` or
//!   `mcp:SERVER:TOOL(ARG)` for an MCP tool, ARG being its URL / path / query / first text argument. No parentheses:
//!   any argument.
//! - Globs: in a path, `*` stays within one path segment and `**` crosses segments (`src/**`, `**/*.env`); a relative
//!   pattern matches paths inside the project, an absolute one (`/…`, `~/…`) absolute paths. In a command or an MCP
//!   argument `*` matches anything, and a trailing ` *` also matches nothing (`git status *` covers `git status`).
//! - A shell allow never covers a command with `;`, `&`, `|`, `<`, `>`, a backtick, `$(` or a newline (it would run more
//!   than the pattern says); deny and ask rules also look at each part of such a command.
//! - Only the user config may allow. A project's `.elisa/config.toml` may add ask / deny (an allow there is ignored with
//!   a note). User plugins may bring all three (listed at setup); project plugins ask / deny only.
use regex::Regex;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level { Allow, Ask, Deny }

impl Level {
    pub fn word(self) -> &'static str { match self { Level::Allow => "allow", Level::Ask => "ask", Level::Deny => "deny" } }
    pub fn parse(s: &str) -> Option<Level> { match s { "allow" => Some(Level::Allow), "ask" => Some(Level::Ask), "deny" => Some(Level::Deny), _ => None } }
}

/// What a rule is about
#[derive(Clone, Debug, PartialEq)]
pub enum Kind { Shell, Read, Write, Mcp { server: String, tool: Option<String> } }

#[derive(Clone, Debug)]
pub struct Rule {
    pub level: Level,
    /// as written
    pub pattern: String,
    /// where it came from: `~/.config/elisa/config.toml`, `project config`, `plugin NAME`, `project plugin NAME`
    pub source: String,
    pub kind: Kind,
    re: Option<Regex>,
    /// an absolute path pattern (`/…`, `~/…` expanded)
    abs: bool,
}

/// The rules of a session, in the order they were added.
#[derive(Clone, Debug, Default)]
pub struct Rules { pub rules: Vec<Rule> }

/// `kind(arg)` → (kind, arg); no parentheses = any argument
pub fn parse(pattern: &str) -> Result<(Kind, Option<String>), String> {
    let p = pattern.trim();
    if p.is_empty() { return Err("empty pattern".into()); }
    let (head, arg) = match p.find('(') {
        Some(i) => {
            if !p.ends_with(')') { return Err(format!("{p:?}: a pattern with an argument ends with `)`")); }
            let a = p[i + 1..p.len() - 1].trim();
            if a.is_empty() { return Err(format!("{p:?}: empty argument (leave out the parentheses for any argument)")); }
            (p[..i].trim(), Some(a.to_string()))
        }
        None => (p, None),
    };
    let ok_name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let kind = match head {
        "shell" | "bash" => Kind::Shell,
        "read" => Kind::Read,
        "write" | "edit" => Kind::Write,
        h if h.starts_with(crate::tools::MCP_KEY) => {
            let rest = &h[crate::tools::MCP_KEY.len()..];
            let (server, tool) = match rest.split_once(':') { Some((s, t)) => (s, Some(t)), None => (rest, None) };
            if !ok_name(server) { return Err(format!("{p:?}: mcp:SERVER needs a server name (letters, digits, _ and -)")); }
            let tool = match tool { None | Some("*") => None, Some(t) if ok_name(t) => Some(t.to_string()), Some(_) => return Err(format!("{p:?}: bad tool name")) };
            Kind::Mcp { server: server.to_string(), tool }
        }
        _ => return Err(format!("{p:?}: unknown kind {head:?} (use shell(…), read(…), write(…) or mcp:SERVER[:TOOL](…))")),
    };
    Ok((kind, arg))
}

fn home() -> PathBuf { std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from) }

/// a path glob: `**/` any number of directories, `**` anything, `*` and `?` within one segment
fn path_re(glob: &str) -> Result<Regex, String> {
    let cs: Vec<char> = glob.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '*' if cs.get(i + 1) == Some(&'*') => {
                if cs.get(i + 2) == Some(&'/') { re.push_str("(?:.*/)?"); i += 3; } else { re.push_str(".*"); i += 2; }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("bad pattern: {e}"))
}

/// a text glob (commands, MCP arguments): `*` anything, `?` one character, a trailing ` *` also nothing
fn text_re(glob: &str) -> Result<Regex, String> {
    let (body, tail) = match glob.strip_suffix(" *") { Some(b) => (b, true), None => (glob, false) };
    let mut re = String::from("^");
    for c in body.chars() {
        match c { '*' => re.push_str(".*"), '?' => re.push('.'), c => re.push_str(&regex::escape(&c.to_string())) }
    }
    if tail { re.push_str("(?: .*)?"); }
    re.push('$');
    Regex::new(&re).map_err(|e| format!("bad pattern: {e}"))
}

impl Rules {
    /// Adds one rule; Err says why the pattern is not one.
    pub fn add(&mut self, level: Level, pattern: &str, source: &str) -> Result<(), String> {
        let (kind, arg) = parse(pattern)?;
        let (re, abs) = match (&kind, &arg) {
            (_, None) => (None, false),
            (Kind::Read | Kind::Write, Some(a)) => {
                let a = if let Some(r) = a.strip_prefix("~/") { format!("{}/{r}", home().display().to_string().trim_end_matches('/')) } else { a.clone() };
                let a = a.trim_start_matches("./").to_string();
                (Some(path_re(&a)?), a.starts_with('/'))
            }
            (_, Some(a)) => (Some(text_re(a)?), false),
        };
        self.rules.push(Rule { level, pattern: pattern.trim().to_string(), source: source.to_string(), kind, re, abs });
        Ok(())
    }

    /// the rules from one source dropped (a plugin switched off)
    pub fn remove_source(&mut self, source: &str) { self.rules.retain(|r| r.source != source); }

    pub fn is_empty(&self) -> bool { self.rules.is_empty() }

    /// The strongest rule that matches this call (deny > ask > allow), the first of that level. `server` / `remote`:
    /// the MCP server and its own name for the tool (None for Elisa's own tools).
    pub fn decide(&self, tool: &str, input: &Value, server: Option<&str>, remote: Option<&str>, cwd: &Path) -> Option<&Rule> {
        let target = Target::of(tool, input, server, remote, cwd)?;
        let mut best: Option<&Rule> = None;
        for r in &self.rules {
            if best.is_some_and(|b| b.level >= r.level) { continue; }
            if r.matches(&target) { best = Some(r); }
        }
        best
    }
}

/// a tool call as the rules see it
enum Target {
    Shell(String),
    /// `inside`: every form of the path (as written, and its real path) is inside the project
    Path { write: bool, rel: Vec<String>, abs: Vec<String>, inside: bool },
    Mcp { server: String, remote: String, arg: Option<String> },
}

/// the characters that make a shell command more than one command (or redirect it)
fn compound(cmd: &str) -> bool { cmd.contains(['&', '|', '<', '>', '`', '\n', ';']) || cmd.contains("$(") }

/// the parts of a compound command (each trimmed), for deny / ask
fn parts(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        let split = match c {
            ';' | '&' | '|' | '\n' | '`' | '(' | ')' => true,
            '$' if it.peek() == Some(&'(') => { it.next(); true }
            _ => false,
        };
        if split { out.push(std::mem::take(&mut cur)); } else { cur.push(c); }
    }
    out.push(cur);
    out.into_iter().map(|p| one_space(&p)).filter(|p| !p.is_empty()).collect()
}

fn one_space(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }

/// `p` with `.` and `..` worked out (lexically)
fn normal(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// An MCP call's argument a rule looks at: its URL, path, file, query or command, else its first text argument.
pub fn mcp_arg(input: &Value) -> Option<String> {
    ["url", "path", "file", "query", "command"].iter().find_map(|k| input.get(k).and_then(Value::as_str))
        .or_else(|| input.as_object().and_then(|o| o.values().find_map(Value::as_str)))
        .map(str::to_string)
}

impl Target {
    fn of(tool: &str, input: &Value, server: Option<&str>, remote: Option<&str>, cwd: &Path) -> Option<Target> {
        if let Some(s) = server {
            return Some(Target::Mcp { server: s.to_string(), remote: remote.unwrap_or(tool).to_string(), arg: mcp_arg(input) });
        }
        let s = |k: &str| input.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        let (write, p) = match tool {
            "bash" => return Some(Target::Shell(s("command").unwrap_or("").trim().to_string())),
            "read" | "outline" | "read_symbol" => (false, s("path")?.to_string()),
            "grep" => (false, s("path").unwrap_or(".").to_string()),
            "glob" => (false, crate::tools::glob_args(input).0),
            "write" | "edit" | "edit_symbol" => (true, s("path")?.to_string()),
            _ => return None,
        };
        let p = if let Some(r) = p.strip_prefix("~/") { home().join(r) } else { PathBuf::from(&p) };
        let lexical = normal(&if p.is_absolute() { p } else { cwd.join(p) });
        // a file that exists (or its folder) also by its real path: a symlink does not dodge a deny
        let real = lexical.canonicalize().ok()
            .or_else(|| lexical.parent().and_then(|d| d.canonicalize().ok()).and_then(|d| lexical.file_name().map(|f| d.join(f))));
        let bases: Vec<PathBuf> = [Some(cwd.to_path_buf()), cwd.canonicalize().ok()].into_iter().flatten().collect();
        let mut rel = Vec::new();
        let mut abs = Vec::new();
        let mut inside = true;
        for a in [Some(lexical), real].into_iter().flatten() {
            match bases.iter().find_map(|b| a.strip_prefix(b).ok()) {
                Some(r) => {
                    let r = r.to_string_lossy().into_owned();
                    let r = if r.is_empty() { ".".to_string() } else { r };
                    if !rel.contains(&r) { rel.push(r); }
                }
                None => inside = false,
            }
            let a = a.to_string_lossy().into_owned();
            if !abs.contains(&a) { abs.push(a); }
        }
        Some(Target::Path { write, rel, abs, inside })
    }
}

impl Rule {
    fn matches(&self, t: &Target) -> bool {
        let strict = self.level == Level::Allow;
        match (&self.kind, t) {
            (Kind::Shell, Target::Shell(raw)) => {
                let Some(re) = &self.re else { return true };
                let cmd = one_space(raw);
                // the raw command for the compound check: a newline is a second command
                if strict { !compound(raw) && re.is_match(&cmd) } else { re.is_match(&cmd) || parts(raw).iter().any(|p| re.is_match(p)) }
            }
            (Kind::Read, Target::Path { write: false, rel, abs, inside }) | (Kind::Write, Target::Path { write: true, rel, abs, inside }) => {
                let Some(re) = &self.re else { return true };
                if self.abs {
                    if strict { abs.iter().all(|a| re.is_match(a)) } else { abs.iter().any(|a| re.is_match(a)) }
                } else if strict {
                    // inside the project, by every form it has (a symlink's target too)
                    *inside && !rel.is_empty() && rel.iter().all(|r| re.is_match(r))
                } else {
                    rel.iter().any(|r| re.is_match(r)) || (self.pattern_starts_anywhere() && abs.iter().any(|a| re.is_match(a)))
                }
            }
            (Kind::Mcp { server, tool }, Target::Mcp { server: s, remote, arg }) => {
                server == s
                    && tool.as_deref().is_none_or(|w| crate::mcp_host::ToolFilter::names(s, w, remote))
                    && match &self.re { None => true, Some(re) => arg.as_deref().is_some_and(|a| re.is_match(a)) }
            }
            _ => false,
        }
    }

    /// a relative path pattern that starts with `**`: a deny / ask also checks absolute paths with it
    fn pattern_starts_anywhere(&self) -> bool {
        self.pattern.find('(').is_some_and(|i| self.pattern[i + 1..].trim_start().starts_with("**"))
    }

    /// an allow that covers a whole kind (`shell`, `write(**)`, `shell(*)`): worth a warning at plugin setup
    pub fn broad(&self) -> bool {
        if self.level != Level::Allow { return false; }
        let arg = self.pattern.find('(').map(|i| self.pattern[i + 1..self.pattern.len() - 1].trim().to_string());
        match (&self.kind, arg.as_deref()) {
            (Kind::Mcp { .. }, _) => false,
            (_, None) => true,
            (_, Some(a)) => a.chars().all(|c| c == '*' || c == '/' || c == ' '),
        }
    }
}

/// Commands an "always allow this pattern" is never offered for: they run anything, delete, or reach out.
const NO_PATTERN: [&str; 24] = ["rm", "sudo", "su", "sh", "bash", "zsh", "fish", "eval", "exec", "xargs", "env", "dd", "mkfs",
    "chmod", "chown", "curl", "wget", "ssh", "scp", "nc", "python", "python3", "node", "perl"];

/// The pattern the approval prompt offers to always allow for this call (written to the user config), or None when
/// there is no sensible one: a compound or dangerous command, a path outside the project, an MCP tool of a server
/// whose tool list changed.
pub fn suggest(tool: &str, input: &Value, server: Option<&str>, remote: Option<&str>, cwd: &Path) -> Option<String> {
    if let Some(s) = server {
        let short = remote.map(|r| r.strip_prefix(&format!("{s}_")).unwrap_or(r)).unwrap_or(tool);
        if !short.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { return Some(format!("mcp:{s}")); }
        // a URL: its scheme and host
        if let Some(u) = input.get("url").and_then(Value::as_str) {
            if let Some((scheme, rest)) = u.split_once("://") {
                let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                if !host.is_empty() && !host.contains(['(', ')', '*', ' ']) && (scheme == "https" || scheme == "http") {
                    return Some(format!("mcp:{s}:{short}({scheme}://{host}/*)"));
                }
            }
        }
        return Some(format!("mcp:{s}:{short}"));
    }
    match tool {
        "bash" => {
            let cmd = one_space(input.get("command").and_then(Value::as_str).unwrap_or(""));
            if cmd.is_empty() || compound(&cmd) || cmd.contains(['*', '?', '(', ')', '"', '\'', '\\', '$']) { return None; }
            let w: Vec<&str> = cmd.split(' ').collect();
            let first = w[0].rsplit('/').next().unwrap_or(w[0]);
            if NO_PATTERN.contains(&first) || w[0].contains('=') { return None; }
            // the program and a subcommand word (`cargo test`, `git status`), not a flag or a path
            let sub = w.get(1).filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !s.starts_with('-'));
            let prefix = match sub { Some(s) => format!("{} {s}", w[0]), None => w[0].to_string() };
            Some(format!("shell({prefix} *)"))
        }
        "write" | "edit" | "edit_symbol" => {
            let Target::Path { rel, inside, .. } = Target::of(tool, input, None, None, cwd)? else { return None };
            let r = rel.first().filter(|_| inside && rel.len() == 1)?;
            if r == "." || r.starts_with("..") || r.contains(['*', '?', '(', ')']) { return None; }
            // never the project's own settings
            if r.starts_with(".elisa") || r.starts_with(".git/") || r == ".git" { return None; }
            Some(match Path::new(r).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty()) {
                Some(dir) => format!("write({dir}/**)"),
                None => format!("write({r})"),
            })
        }
        _ => None,
    }
}

/// `[permissions]` of a config file: (level, pattern) pairs; Err on a malformed table
pub fn parse_table(v: &toml::Value) -> Result<Vec<(Level, String)>, String> {
    let t = v.as_table().ok_or("[permissions] must be a table: allow = [\"…\"], ask = [\"…\"], deny = [\"…\"]")?;
    let mut out = Vec::new();
    for (k, v) in t {
        let level = Level::parse(k).ok_or_else(|| format!("[permissions] unknown key {k:?} (use allow, ask, deny)"))?;
        let a = v.as_array().ok_or_else(|| format!("[permissions] {k} must be a list of patterns"))?;
        for p in a { out.push((level, p.as_str().ok_or_else(|| format!("[permissions] {k} must be a list of strings"))?.to_string())); }
    }
    Ok(out)
}

/// `text` (a config.toml) with `pattern` added to `[permissions] allow`; the file's other lines are kept as they are.
/// Err when the result would not be a valid config (nothing is written then).
pub fn add_allow_to(text: &str, pattern: &str) -> Result<String, String> {
    parse(pattern)?;
    let lit = toml::Value::String(pattern.to_string()).to_string();
    let lines: Vec<&str> = text.lines().collect();
    let header = |l: &str| { let t = l.trim(); t.starts_with('[') };
    let is_perm = |l: &str| { let t = l.trim(); t == "[permissions]" || t.starts_with("[permissions]") && t["[permissions]".len()..].trim_start().starts_with('#') };
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    match lines.iter().position(|l| is_perm(l)) {
        None => {
            if out.last().is_some_and(|l| !l.trim().is_empty()) { out.push(String::new()); }
            out.push("[permissions]".into());
            out.push(format!("allow = [{lit}]"));
        }
        Some(h) => {
            let end = (h + 1..lines.len()).find(|&i| header(lines[i])).unwrap_or(lines.len());
            let key = (h + 1..end).find(|&i| { let t = lines[i].trim_start(); t.strip_prefix("allow").is_some_and(|r| r.trim_start().starts_with('=')) });
            match key {
                Some(i) => {
                    let l = lines[i];
                    let eq = l.find('=').ok_or("no = in the allow line")?;
                    let open = l[eq..].find('[').map(|o| eq + o).ok_or("[permissions] allow is not a list")?;
                    let after = l[open + 1..].trim_start();
                    let ins = if after.starts_with(']') { lit.clone() } else if after.is_empty() || after.starts_with('#') { format!("{lit},") } else { format!("{lit}, ") };
                    out[i] = format!("{}{ins}{}", &l[..open + 1], &l[open + 1..]);
                }
                None => out.insert(h + 1, format!("allow = [{lit}]")),
            }
        }
    }
    let new = out.join("\n") + "\n";
    let v: toml::Value = toml::from_str(&new).map_err(|e| format!("the config would not parse after the change: {e}"))?;
    let ok = v.get("permissions").and_then(|p| p.get("allow")).and_then(|a| a.as_array()).is_some_and(|a| a.iter().any(|x| x.as_str() == Some(pattern)));
    if !ok { return Err("could not add the pattern to [permissions] allow".into()); }
    Ok(new)
}

/// Adds `pattern` to `[permissions] allow` in the user config at `path` (created if missing, mode 600). Already there:
/// nothing changes.
pub fn save_allow(path: &Path, pattern: &str) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if let Ok(v) = toml::from_str::<toml::Value>(&text) {
        let have = v.get("permissions").and_then(|p| p.get("allow")).and_then(|a| a.as_array()).is_some_and(|a| a.iter().any(|x| x.as_str() == Some(pattern)));
        if have { return Ok(()); }
    }
    let new = add_allow_to(&text, pattern)?;
    if let Some(d) = path.parent() { std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?; }
    let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::write(&tmp, new).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if text.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    } else if let Ok(m) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules(list: &[(Level, &str)]) -> Rules {
        let mut r = Rules::default();
        for (l, p) in list { r.add(*l, p, "test").unwrap(); }
        r
    }

    fn cwd() -> PathBuf { PathBuf::from("/nonexistent-elisa-proj") }

    fn level(r: &Rules, tool: &str, input: Value) -> Option<Level> { r.decide(tool, &input, None, None, &cwd()).map(|x| x.level) }

    #[test]
    fn patterns_parse_or_say_why() {
        assert_eq!(parse("shell(git status *)").unwrap(), (Kind::Shell, Some("git status *".into())));
        assert_eq!(parse("bash").unwrap(), (Kind::Shell, None));
        assert_eq!(parse("edit(src/**)").unwrap().0, Kind::Write);
        assert_eq!(parse("mcp:browser:navigate(https://github.com/*)").unwrap(),
            (Kind::Mcp { server: "browser".into(), tool: Some("navigate".into()) }, Some("https://github.com/*".into())));
        assert_eq!(parse("mcp:web").unwrap().0, Kind::Mcp { server: "web".into(), tool: None });
        assert!(parse("shell(").unwrap_err().contains("ends with"));
        assert!(parse("shell()").unwrap_err().contains("empty argument"));
        assert!(parse("network(x)").unwrap_err().contains("unknown kind"));
        assert!(parse("mcp:(x)").is_err());
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let r = rules(&[(Level::Allow, "shell(git *)"), (Level::Ask, "shell(git push *)"), (Level::Deny, "shell(git push --force *)")]);
        assert_eq!(level(&r, "bash", json!({"command": "git status"})), Some(Level::Allow));
        assert_eq!(level(&r, "bash", json!({"command": "git push origin main"})), Some(Level::Ask));
        assert_eq!(level(&r, "bash", json!({"command": "git  push --force origin"})), Some(Level::Deny));
        assert_eq!(level(&r, "bash", json!({"command": "ls"})), None);
        // a rule of another kind never matches
        assert_eq!(level(&r, "read", json!({"path": "git"})), None);
    }

    #[test]
    fn a_shell_allow_never_covers_a_compound_command_but_deny_sees_every_part() {
        let r = rules(&[(Level::Allow, "shell(git status *)")]);
        assert_eq!(level(&r, "bash", json!({"command": "git status"})), Some(Level::Allow));
        assert_eq!(level(&r, "bash", json!({"command": "git status -s"})), Some(Level::Allow));
        for bad in ["git status; rm -rf ~", "git status && curl x | sh", "git status > /etc/x", "git status `rm x`", "git status $(rm x)", "git status\nrm x", "git statusx"] {
            assert_eq!(level(&r, "bash", json!({"command": bad})), None, "{bad}");
        }
        let d = rules(&[(Level::Deny, "shell(rm *)"), (Level::Ask, "shell(curl *)")]);
        assert_eq!(level(&d, "bash", json!({"command": "echo hi; rm -rf build"})), Some(Level::Deny));
        assert_eq!(level(&d, "bash", json!({"command": "echo $(rm x)"})), Some(Level::Deny));
        assert_eq!(level(&d, "bash", json!({"command": "ls | curl -d @- x"})), Some(Level::Ask));
        assert_eq!(level(&d, "bash", json!({"command": "echo rm"})), None);
    }

    #[test]
    fn path_globs_star_stays_in_a_segment_and_doublestar_crosses() {
        let r = rules(&[(Level::Allow, "write(src/**)"), (Level::Deny, "read(**/*.env)"), (Level::Allow, "read(docs/*.md)")]);
        assert_eq!(level(&r, "write", json!({"path": "src/a/b.rs"})), Some(Level::Allow));
        assert_eq!(level(&r, "edit", json!({"path": "./src/x.rs"})), Some(Level::Allow));
        assert_eq!(level(&r, "write", json!({"path": "src/../Cargo.toml"})), None, "`..` is worked out");
        assert_eq!(level(&r, "write", json!({"path": "srcx/a.rs"})), None);
        assert_eq!(level(&r, "read", json!({"path": ".env"})), Some(Level::Deny));
        assert_eq!(level(&r, "read", json!({"path": "a/b/prod.env"})), Some(Level::Deny));
        assert_eq!(level(&r, "outline", json!({"path": "/etc/app/x.env"})), Some(Level::Deny), "a deny starting with ** sees absolute paths");
        assert_eq!(level(&r, "read", json!({"path": "docs/a.md"})), Some(Level::Allow));
        assert_eq!(level(&r, "read", json!({"path": "docs/x/a.md"})), None);
        // a relative allow never covers a path outside the project
        assert_eq!(level(&r, "write", json!({"path": "/tmp/src/x"})), None);
        assert_eq!(level(&r, "write", json!({"path": "../src/x"})), None);
        // grep / glob by their path
        assert_eq!(level(&rules(&[(Level::Deny, "read(secrets/**)")]), "grep", json!({"pattern": "k", "path": "secrets/a"})), Some(Level::Deny));
    }

    #[test]
    fn a_symlink_does_not_dodge_a_deny_or_widen_an_allow() {
        let d = crate::testdir::TempDir::new("perms");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join(".env"), "x").unwrap();
        std::os::unix::fs::symlink(d.join(".env"), d.join("src/config.txt")).unwrap();
        let r = rules(&[(Level::Deny, "read(**/*.env)"), (Level::Allow, "write(src/**)")]);
        assert_eq!(r.decide("read", &json!({"path": "src/config.txt"}), None, None, &d).map(|x| x.level), Some(Level::Deny));
        // the link's real path is not under src/: the allow does not cover writing through it
        assert_eq!(r.decide("write", &json!({"path": "src/config.txt"}), None, None, &d).map(|x| x.level), None);
        assert_eq!(r.decide("write", &json!({"path": "src/new.rs"}), None, None, &d).map(|x| x.level), Some(Level::Allow));
    }

    #[test]
    fn mcp_rules_match_server_tool_and_argument() {
        let r = rules(&[(Level::Allow, "mcp:browser:navigate(https://github.com/*)"), (Level::Deny, "mcp:browser:evaluate"), (Level::Ask, "mcp:fs")]);
        let d = |tool: &str, remote: &str, server: &str, input: Value| r.decide(tool, &input, Some(server), Some(remote), &cwd()).map(|x| x.level);
        assert_eq!(d("browser_navigate", "browser_navigate", "browser", json!({"url": "https://github.com/a/b"})), Some(Level::Allow));
        assert_eq!(d("browser_navigate", "browser_navigate", "browser", json!({"url": "https://github.com.evil.io/"})), None);
        assert_eq!(d("browser_evaluate", "browser_evaluate", "browser", json!({"function": "x"})), Some(Level::Deny));
        assert_eq!(d("fs_read", "read", "fs", json!({})), Some(Level::Ask));
        assert_eq!(d("other_navigate", "navigate", "other", json!({"url": "https://github.com/"})), None);
    }

    #[test]
    fn suggestions_are_narrow_and_never_dangerous() {
        let s = |tool: &str, input: Value| suggest(tool, &input, None, None, &cwd());
        assert_eq!(s("bash", json!({"command": "cargo test --release"})).as_deref(), Some("shell(cargo test *)"));
        assert_eq!(s("bash", json!({"command": "ls -la"})).as_deref(), Some("shell(ls *)"));
        for bad in ["rm -rf build", "sudo ls", "git status; ls", "echo $HOME", "FOO=1 make", "python3 x.py", "curl https://x"] {
            assert_eq!(s("bash", json!({"command": bad})), None, "{bad}");
        }
        assert_eq!(s("write", json!({"path": "src/a/b.rs"})).as_deref(), Some("write(src/a/**)"));
        assert_eq!(s("edit", json!({"path": "README.md"})).as_deref(), Some("write(README.md)"));
        assert_eq!(s("write", json!({"path": "../x.rs"})), None);
        assert_eq!(s("write", json!({"path": ".elisa/config.toml"})), None);
        assert_eq!(suggest("browser_navigate", &json!({"url": "https://github.com/a?b"}), Some("browser"), Some("browser_navigate"), &cwd()).as_deref(),
            Some("mcp:browser:navigate(https://github.com/*)"));
        assert_eq!(suggest("time_now", &json!({}), Some("time"), Some("now"), &cwd()).as_deref(), Some("mcp:time:now"));
        // every suggestion is a pattern that matches its own call
        let mut r = Rules::default();
        r.add(Level::Allow, "shell(cargo test *)", "t").unwrap();
        assert_eq!(level(&r, "bash", json!({"command": "cargo test --release"})), Some(Level::Allow));
    }

    #[test]
    fn the_user_config_gets_the_pattern_and_keeps_its_lines() {
        let a = add_allow_to("model = \"m\"\n# my comment\n", "shell(ls *)").unwrap();
        assert_eq!(a, "model = \"m\"\n# my comment\n\n[permissions]\nallow = [\"shell(ls *)\"]\n");
        let b = add_allow_to("[permissions]\ndeny = [\"shell(rm *)\"]\n[mcp.x]\ncommand = [\"x\"]\n", "write(src/**)").unwrap();
        assert!(b.starts_with("[permissions]\nallow = [\"write(src/**)\"]\ndeny"), "{b}");
        let c = add_allow_to("[permissions]\nallow = [\"shell(ls *)\"] # mine\n", "shell(git status *)").unwrap();
        assert_eq!(c, "[permissions]\nallow = [\"shell(git status *)\", \"shell(ls *)\"] # mine\n");
        let d = add_allow_to("[permissions]\nallow = []\n", "read(**)").unwrap();
        assert_eq!(d, "[permissions]\nallow = [\"read(**)\"]\n");
        let e = add_allow_to("[permissions]\nallow = [\n  \"shell(ls *)\",\n]\n", "shell(pwd *)").unwrap();
        assert!(toml::from_str::<toml::Value>(&e).is_ok(), "{e}");
        // quotes in a pattern are escaped
        let q = add_allow_to("", "shell(echo \"hi\" *)").unwrap();
        assert!(toml::from_str::<toml::Value>(&q).is_ok(), "{q}");
        assert!(add_allow_to("", "nope(x)").is_err());
        // save: created with mode 600, a second save changes nothing
        let d = crate::testdir::TempDir::new("perms-save");
        let p = d.join(".config/elisa/config.toml");
        save_allow(&p, "shell(ls *)").unwrap();
        save_allow(&p, "shell(ls *)").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[permissions]\nallow = [\"shell(ls *)\"]\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn broad_allows_are_flagged() {
        let r = rules(&[(Level::Allow, "shell"), (Level::Allow, "write(**)"), (Level::Allow, "shell(git status *)"), (Level::Deny, "shell")]);
        assert_eq!(r.rules.iter().map(Rule::broad).collect::<Vec<_>>(), [true, true, false, false]);
    }
}
