//! Work outside the project folder (the session cwd) asks first. Every file tool's path and every path a bash command
//! names (best effort, erring toward asking) is resolved — `..` and symlinks included — and checked against the
//! project root; anything outside becomes an [`Outside`] request (repl.rs `approve_outside`). Config `outside`:
//! "ask" (default) | "read-ok" (outside reads run, outside writes ask) | "deny" | "allow" (no outside prompt; the
//! normal tool rules apply, so `--yes` then allows outside writes). Contract in DESIGN.md "Outside the project".
use crate::frontend::Outside;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Ask, ReadOk, Deny, Allow }

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s { "ask" => Mode::Ask, "read-ok" => Mode::ReadOk, "deny" => Mode::Deny, "allow" => Mode::Allow, _ => return None })
    }
    pub fn word(self) -> &'static str {
        match self { Mode::Ask => "ask", Mode::ReadOk => "read-ok", Mode::Deny => "deny", Mode::Allow => "allow" }
    }
    fn rank(self) -> u8 { match self { Mode::Allow => 0, Mode::ReadOk => 1, Mode::Ask => 2, Mode::Deny => 3 } }
    /// the stricter of the two (a project's `.elisa/config.toml` may only tighten the user's setting)
    pub fn stricter(self, o: Mode) -> Mode { if o.rank() > self.rank() { o } else { self } }
}

/// devices a command may name freely (`2>/dev/null`): never "outside"
const DEVICES: [&str; 7] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/stdin", "/dev/tty", "/dev/zero", "/dev/urandom"];

fn home() -> PathBuf { std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from) }

/// `p` (relative to `base`) made absolute with `.` / `..` applied and symlinks followed the way the kernel does
/// (`link/..` is the link target's parent): each existing prefix is canonicalized; the part that does not exist yet
/// is applied lexically.
pub fn resolve(base: &Path, p: &Path) -> PathBuf {
    let abs = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut out = PathBuf::new();
    let mut exists = true;
    for c in abs.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            Component::Normal(n) => {
                out.push(n);
                if exists {
                    match std::fs::canonicalize(&out) { Ok(t) => out = t, Err(_) => exists = false }
                }
            }
        }
    }
    out
}

/// the canonical project root
pub fn root(cwd: &Path) -> PathBuf { resolve(Path::new("/"), cwd) }

/// `p` (already resolved) is the root or under it
pub fn inside(root: &Path, p: &Path) -> bool { p.starts_with(root) }

fn sv<'a>(input: &'a Value, k: &str) -> Option<&'a str> { input.get(k).and_then(Value::as_str) }

/// The outside-the-project request a tool call makes, if any. `cwd` = the project root (the tools' cwd).
pub fn check(name: &str, input: &Value, cwd: &Path) -> Option<Outside> {
    let root = root(cwd);
    let (access, paths): (&str, Vec<String>) = match name {
        "read" | "outline" | "read_symbol" | "write" | "edit" | "edit_symbol" | "glob" | "grep" => {
            let dflt = if matches!(name, "glob" | "grep") { "." } else { "" };
            let p = sv(input, "path").filter(|p| !p.is_empty()).unwrap_or(dflt);
            if p.is_empty() { return None; }
            let r = resolve(&root, Path::new(p));
            if inside(&root, &r) { return None; }
            let access = if crate::tools::access(name) == crate::tools::Access::Write { "write" } else { "read" };
            (access, vec![r.to_string_lossy().into_owned()])
        }
        "bash" => {
            let paths = bash_outside(sv(input, "command").unwrap_or(""), &root);
            if paths.is_empty() { return None; }
            ("bash", paths)
        }
        _ => return None,
    };
    Some(Outside { access: access.to_string(), paths })
}

/// A shell word or a separator between simple commands (`;` `&` `|` `(` `)` newline, backquote).
#[derive(Debug, PartialEq)]
enum Tok { Word(String), Sep, Redir, Data }

/// zsh-ish words: quotes and backslashes handled, redirections (`>` `<`) end a word, separators split commands.
/// A heredoc body (`<<EOF … EOF`, `<<-'EOF'`) is dropped (it is stdin data, not shell words); a here-string
/// (`<<< word`) emits `Data` so its word is skipped.
fn words(cmd: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut w = String::new();
    let mut in_word = false;
    let cs: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    // heredoc delimiters waiting for the end of the current line
    let mut pending: Vec<(String, bool)> = Vec::new();
    let flush = |w: &mut String, in_word: &mut bool, out: &mut Vec<Tok>| {
        if *in_word { out.push(Tok::Word(std::mem::take(w))); *in_word = false; }
    };
    while i < cs.len() {
        let c = cs[i];
        i += 1;
        match c {
            '\'' => { in_word = true; while i < cs.len() { let d = cs[i]; i += 1; if d == '\'' { break; } w.push(d); } }
            '"' => {
                in_word = true;
                while i < cs.len() {
                    let d = cs[i];
                    i += 1;
                    match d {
                        '"' => break,
                        '\\' => { if i < cs.len() { w.push(cs[i]); i += 1; } }
                        _ => w.push(d),
                    }
                }
            }
            '\\' => { in_word = true; if i < cs.len() { let e = cs[i]; i += 1; if e != '\n' { w.push(e); } } }
            ' ' | '\t' => flush(&mut w, &mut in_word, &mut out),
            '<' if cs.get(i) == Some(&'<') && cs.get(i + 1) == Some(&'<') => {
                flush(&mut w, &mut in_word, &mut out);
                i += 2;
                out.push(Tok::Data);
            }
            '<' if cs.get(i) == Some(&'<') => {
                flush(&mut w, &mut in_word, &mut out);
                i += 1;
                let strip = cs.get(i) == Some(&'-');
                if strip { i += 1; }
                while i < cs.len() && (cs[i] == ' ' || cs[i] == '\t') { i += 1; }
                // the delimiter word, quotes removed
                let mut delim = String::new();
                while i < cs.len() {
                    let d = cs[i];
                    if d.is_whitespace() || ";&|()<>`".contains(d) { break; }
                    i += 1;
                    if d == '\'' || d == '"' {
                        while i < cs.len() && cs[i] != d { delim.push(cs[i]); i += 1; }
                        i += 1;
                    } else if d != '\\' { delim.push(d); }
                }
                if !delim.is_empty() { pending.push((delim, strip)); }
            }
            '<' => flush(&mut w, &mut in_word, &mut out),
            // the next word is a redirect target (`> f`, `>> f`, `2>f`): always checked as a path
            '>' => { flush(&mut w, &mut in_word, &mut out); out.push(Tok::Redir); }
            '\n' if !pending.is_empty() => {
                flush(&mut w, &mut in_word, &mut out);
                out.push(Tok::Sep);
                // skip each pending heredoc body up to its delimiter line
                for (delim, strip) in std::mem::take(&mut pending) {
                    while i < cs.len() {
                        let e = cs[i..].iter().position(|&x| x == '\n').map_or(cs.len(), |n| i + n);
                        let line: String = cs[i..e].iter().collect();
                        i = (e + 1).min(cs.len());
                        let line = if strip { line.trim_start_matches('\t') } else { line.as_str() };
                        if line.trim_end() == delim { break; }
                    }
                }
            }
            ';' | '&' | '|' | '(' | ')' | '\n' | '`' => {
                flush(&mut w, &mut in_word, &mut out);
                out.push(Tok::Sep);
            }
            _ => { in_word = true; w.push(c); }
        }
    }
    flush(&mut w, &mut in_word, &mut out);
    out
}

/// script interpreters: the argument of their inline-code option (`python -c CODE`, `node -e CODE`) is program
/// text, not shell words (writes inside a script are out of scope)
fn interpreter(name: &str) -> bool {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    ["python", "pypy", "node", "nodejs", "deno", "bun", "ruby", "perl", "php", "Rscript", "julia", "lua", "osascript", "ipython"].contains(&base)
}
const CODE_OPTS: [&str; 6] = ["-c", "-e", "-E", "--eval", "-p", "--print"];
/// shells: their `-c` argument is a shell command, checked as one
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// HTTP clients: their URL / data arguments name routes, not files (only their output options write files)
const HTTP_CLIENTS: [&str; 6] = ["curl", "wget", "http", "https", "httpie", "xh"];
/// options of the HTTP clients whose value is a local file (written or read)
const HTTP_FILE_OPTS: [&str; 12] = ["-o", "--output", "-O", "--output-document", "-D", "--dump-header", "-c", "--cookie-jar", "-b", "--cookie", "-T", "--upload-file"];

/// a URL or a `host:port/…` word: never a local path (`http://localhost:8000/items`, `localhost:8000/auth/register`)
fn is_url(w: &str) -> bool {
    if w.contains("://") { return true; }
    let w = w.split_once('=').map_or(w, |(_, v)| v);
    let Some((host, rest)) = w.split_once(':') else { return false };
    !host.is_empty() && !host.contains('/') && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && rest.split('/').next().is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// an absolute path whose top-level directory does not exist (`/items/9999`, `/auth/register,`): a URL route or an
/// API path in a script / JSON argument, not a file a command can write (nothing can be made under a missing `/x`)
fn route_like(p: &Path) -> bool {
    let mut c = p.components();
    if c.next() != Some(Component::RootDir) { return false; }
    match c.next() { Some(Component::Normal(top)) => !Path::new("/").join(top).exists(), _ => false }
}

/// the path a word may name, with `~` / `$HOME` / `$PWD` expanded: Some(path) when it looks like a path that can
/// leave the folder (absolute, or with a `..` component); Err(raw) for `$VAR/…` (unknown: treated as outside)
fn candidate(word: &str, cur: &Path) -> Option<Result<PathBuf, String>> {
    let mut w = word;
    // NAME=value, --flag=value, -I/usr/include
    if let Some((k, v)) = w.split_once('=') {
        let k = k.trim_start_matches('-');
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { w = v; }
    } else if w.starts_with('-') {
        w = &w[w.find('/')?..];
    }
    if w.is_empty() { return None; }
    let var = |name: &str| -> Option<&str> {
        w.strip_prefix(&format!("${{{name}}}")).or_else(|| w.strip_prefix(&format!("${name}")))
            .filter(|r| r.is_empty() || r.starts_with('/'))
    };
    let expanded = if w == "~" || w.starts_with("~/") {
        home().join(w[1..].trim_start_matches('/'))
    } else if let Some(r) = var("HOME") {
        home().join(r.trim_start_matches('/'))
    } else if let Some(r) = var("PWD") {
        cur.join(r.trim_start_matches('/'))
    } else if w.starts_with('$') && w.contains('/') {
        return Some(Err(word.to_string()));
    } else if w.starts_with('~') {
        // ~user/…: another user's home
        return Some(Err(word.to_string()));
    } else {
        PathBuf::from(w)
    };
    let p = expanded.as_path();
    let leaves = p.is_absolute() || p.components().any(|c| c == Component::ParentDir);
    if !leaves { return None; }
    let s = p.to_string_lossy();
    if DEVICES.contains(&s.as_ref()) || s.starts_with("/dev/fd/") { return None; }
    Some(Ok(resolve(cur, p)))
}

/// The paths outside `root` that a bash command names (best effort): absolute paths, `~` / `$HOME`, `..` escaping
/// the root, and `cd` / `pushd` out of it (also a bare `cd`, `cd -`, `cd ~`). A `cd` inside moves the base the later
/// relative words resolve against. Unknown `$VAR/…` words count as outside (raw text shown).
pub fn bash_outside(cmd: &str, root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| if !out.contains(&s) { out.push(s) };
    let toks = words(cmd);
    let mut cur = root.to_path_buf();
    let mut start = true;
    // the simple command's name, and the word before this one (an HTTP client's output option)
    let mut cmd_name = String::new();
    let mut prev = String::new();
    let mut redir = false;
    // an interpreter / shell named in this simple command (also after `uv run`, `env`, `time`, …)
    let mut interp = false;
    let mut shell = false;
    let mut data = false;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Sep => { start = true; redir = false; data = false; interp = false; shell = false; cmd_name.clear(); i += 1; continue; }
            Tok::Redir => { redir = true; i += 1; continue; }
            Tok::Data => { data = true; i += 1; continue; }
            Tok::Word(w) if start && (w == "cd" || w == "pushd") => {
                start = false;
                let arg = match toks.get(i + 1) { Some(Tok::Word(a)) => { i += 1; Some(a.as_str()) } _ => None };
                let arg = arg.filter(|a| *a != "--");
                match arg {
                    None => { let h = resolve(Path::new("/"), &home()); if !inside(root, &h) { add(h.to_string_lossy().into_owned()); } cur = h; }
                    Some("-") => add("- (cd to the previous directory)".into()),
                    Some(a) => match candidate(a, &cur) {
                        Some(Ok(p)) => { if !inside(root, &p) { add(p.to_string_lossy().into_owned()); } cur = p; }
                        Some(Err(raw)) => add(raw),
                        None => cur = resolve(&cur, Path::new(a)),
                    },
                }
            }
            Tok::Word(w) => {
                let target = std::mem::take(&mut redir);
                if start { cmd_name = w.rsplit('/').next().unwrap_or(w).to_string(); }
                let first = start;
                start = false;
                let before = std::mem::replace(&mut prev, w.clone());
                let base = w.rsplit('/').next().unwrap_or(w);
                if !target {
                    // a here-string's word is stdin data
                    if std::mem::take(&mut data) { i += 1; continue; }
                    // inline program text: `python -c CODE`, `node -e CODE`; a shell's `-c CMD` is parsed as a command
                    if CODE_OPTS.contains(&before.as_str()) && (interp || shell) {
                        if shell && before == "-c" { for p in bash_outside(w, root) { add(p); } }
                        if interp || before == "-c" { i += 1; continue; }
                    }
                    if interpreter(base) { interp = true; }
                    if SHELLS.contains(&base) { shell = true; }
                }
                // not a redirect target: URLs, an HTTP client's arguments (except its file options), and absolute
                // words under a top-level directory that does not exist are routes, not paths
                if !target && !first {
                    let http = HTTP_CLIENTS.contains(&cmd_name.as_str());
                    let file_opt = HTTP_FILE_OPTS.contains(&before.as_str()) || HTTP_FILE_OPTS.iter().any(|o| o.starts_with("--") && w.starts_with(&format!("{o}=")));
                    if is_url(w) || (http && !file_opt) { i += 1; continue; }
                    if let Some(Ok(p)) = candidate(w, &cur) { if route_like(&p) { i += 1; continue; } }
                }
                match candidate(w, &cur) {
                    Some(Ok(p)) if !inside(root, &p) => add(p.to_string_lossy().into_owned()),
                    Some(Err(raw)) => add(raw),
                    _ => {
                        // a relative word after a `cd` out of the folder: outside too
                        if !inside(root, &cur) && !w.starts_with('-') && w.contains('/') {
                            add(resolve(&cur, Path::new(w)).to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// session "allow this path" grants: (path, write) — a write grant covers reads too; a grant covers the path and
/// everything under it
pub fn covered(grants: &[(PathBuf, bool)], o: &Outside) -> bool {
    let write = o.write();
    o.paths.iter().all(|p| {
        let p = Path::new(p);
        p.is_absolute() && grants.iter().any(|(g, w)| p.starts_with(g) && (*w || !write))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("elisa-outside-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("proj/src")).unwrap();
        std::fs::create_dir_all(d.join("other")).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn resolve_handles_dotdot_absolute_and_symlinks() {
        let d = tmp("resolve");
        let proj = d.join("proj");
        assert_eq!(resolve(&proj, Path::new("src/../src/a.rs")), proj.join("src/a.rs"));
        assert_eq!(resolve(&proj, Path::new("../other/x")), d.join("other/x"));
        assert_eq!(resolve(&proj, Path::new("../../../../../../../../etc")), PathBuf::from("/etc").canonicalize().unwrap());
        assert_eq!(resolve(&proj, &d.join("other")), d.join("other"));
        assert_eq!(resolve(&proj, Path::new("new/dir/f.txt")), proj.join("new/dir/f.txt"), "missing tail: lexical");
        // a symlink inside the project that points outside resolves outside
        std::os::unix::fs::symlink(d.join("other"), proj.join("link")).unwrap();
        assert_eq!(resolve(&proj, Path::new("link/f.txt")), d.join("other/f.txt"));
        // `link/..` is the target's parent (kernel rule), not the project
        assert_eq!(resolve(&proj, Path::new("link/..")), d);
        // a symlink outside pointing in resolves inside
        std::os::unix::fs::symlink(proj.join("src"), d.join("in")).unwrap();
        assert!(inside(&proj, &resolve(&proj, &d.join("in/a.rs"))));
        // the project's own .elisa/ is inside
        assert!(inside(&proj, &resolve(&proj, Path::new(".elisa/allow"))));
        // a sibling with the root as a string prefix is not inside
        std::fs::create_dir_all(d.join("proj2")).unwrap();
        assert!(!inside(&proj, &resolve(&proj, &d.join("proj2/x"))));
    }

    #[test]
    fn file_tools_report_outside_paths_with_access() {
        let d = tmp("tools");
        let proj = d.join("proj");
        assert_eq!(check("read", &json!({"path": "src/a.rs"}), &proj), None);
        assert_eq!(check("grep", &json!({"pattern": "x"}), &proj), None, "default path . is inside");
        let o = check("read", &json!({"path": "../other/a.txt"}), &proj).unwrap();
        assert_eq!((o.access.as_str(), o.paths.clone()), ("read", vec![d.join("other/a.txt").to_string_lossy().into_owned()]));
        assert!(!o.write());
        let o = check("write", &json!({"path": d.join("other/w.txt").to_str().unwrap(), "content": ""}), &proj).unwrap();
        assert_eq!(o.access, "write");
        assert!(o.write());
        for (n, w) in [("edit", true), ("edit_symbol", true), ("outline", false), ("read_symbol", false), ("glob", false)] {
            let o = check(n, &json!({"path": "..", "pattern": "*"}), &proj).unwrap_or_else(|| panic!("{n}"));
            assert_eq!((o.write(), o.paths[0].as_str()), (w, d.to_str().unwrap()), "{n}");
        }
        std::os::unix::fs::symlink(d.join("other"), proj.join("ln")).unwrap();
        assert!(check("edit", &json!({"path": "ln/x.rs"}), &proj).is_some(), "symlink out");
    }

    #[test]
    fn bash_parse_finds_paths_that_leave_the_folder() {
        let d = tmp("bash");
        let proj = d.join("proj");
        let out = |c: &str| bash_outside(c, &proj);
        let other = d.join("other").to_string_lossy().into_owned();
        // inside: nothing
        for c in ["cargo test", "cat src/a.rs 2>/dev/null | head", "cd src && ls ..", "ls ./src/../src", "git log HEAD..main",
                  "echo \"a b\" > out.txt", "grep -n foo src", "curl https://example.com/x", "FOO=bar make", "cd src; cd .."] {
            assert!(out(c).is_empty(), "{c}: {:?}", out(c));
        }
        assert_eq!(out(&format!("cat {other}/a.txt")), vec![format!("{other}/a.txt")]);
        assert_eq!(out("cat ../other/a.txt"), vec![format!("{other}/a.txt")]);
        assert_eq!(out("cd src && cat ../../other/b"), vec![format!("{other}/b")]);
        assert_eq!(out("cd .. && ls"), vec![d.to_string_lossy().into_owned()]);
        assert_eq!(out(&format!("cd {other}; rm -rf x/y")), vec![other.clone(), format!("{other}/x/y")]);
        assert_eq!(out(&format!("echo hi >{other}/o.txt")), vec![format!("{other}/o.txt")]);
        assert_eq!(out(&format!("echo hi>>'{other}/q t.txt'")), vec![format!("{other}/q t.txt")]);
        assert_eq!(out(&format!("cc -I{other}/inc x.c")), vec![format!("{other}/inc")]);
        assert_eq!(out(&format!("make OUT={other}/b")), vec![format!("{other}/b")]);
        assert_eq!(out(&format!("tool --out={other}/b")), vec![format!("{other}/b")]);
        assert_eq!(out("ls $TMPDIR/x"), vec!["$TMPDIR/x".to_string()], "unknown var: ask");
        assert_eq!(out("cd -"), vec!["- (cd to the previous directory)".to_string()]);
        let h = resolve(Path::new("/"), &home()).to_string_lossy().into_owned();
        // (HOME is normally outside the temp dir the project is in)
        if !proj.starts_with(&h) {
            assert_eq!(out("cd"), vec![h.clone()]);
            assert_eq!(out("ls ~/x"), vec![format!("{h}/x")]);
            assert_eq!(out("ls $HOME/x"), vec![format!("{h}/x")]);
        }
        assert_eq!(out("(cd /; ls)").first().map(String::as_str), Some("/"));
        assert_eq!(out("ls /usr/bin/env"), vec![resolve(Path::new("/"), Path::new("/usr/bin/env")).to_string_lossy().into_owned()]);
        // absolute paths inside the project are fine
        assert!(out(&format!("cat {}/src/a.rs", proj.display())).is_empty());
    }

    #[test]
    fn url_paths_are_routes_not_outside_writes() {
        // g40 session: `denied: run (may write) outside the project folder (/auth/register,, /auth/login,, /items,, /items/9999,)`
        let d = tmp("urls");
        let proj = d.join("proj");
        let out = |c: &str| bash_outside(c, &proj);
        for c in ["curl -s -X POST http://localhost:8000/auth/register -H 'Content-Type: application/json' -d '{\"user\": \"a\"}'",
                  "curl -s localhost:8000/items/9999", "curl -s -o out.json \"$BASE/items/9999\"", "http POST :8000/auth/login user=a",
                  "wget -q -O - 127.0.0.1:8000/items", "for p in /auth/register, /auth/login, /items, /items/9999; do echo $p; done",
                  "python3 check.py /items/9999 /auth/register", "echo GET /items/search?q=x 2>/dev/null"] {
            assert!(out(c).is_empty(), "{c}: {:?}", out(c));
        }
        // real outside writes still ask: redirects, cp / mv / tee targets, an HTTP client's output file
        assert_eq!(out("echo hi > /etc/x"), vec![resolve(Path::new("/"), Path::new("/etc/x")).to_string_lossy().into_owned()]);
        let tmpx = resolve(Path::new("/"), Path::new("/tmp/x")).to_string_lossy().into_owned();
        assert_eq!(out("cp a /tmp/x"), vec![tmpx.clone()]);
        assert_eq!(out("curl -s http://localhost:8000/items -o /tmp/x"), vec![tmpx.clone()]);
        assert_eq!(out("curl -s --output=/tmp/x http://localhost:8000/items"), vec![tmpx.clone()]);
        assert_eq!(out("curl -s http://localhost:8000/items > /tmp/x"), vec![tmpx]);
        // a redirect into a missing top-level directory is still a write target
        assert_eq!(out("echo x > /items/9999"), vec!["/items/9999".to_string()]);
        assert!(is_url("localhost:8000/a") && is_url("https://x.y/z") && !is_url("a:b/c") && !is_url("/x/y"));
    }

    #[test]
    fn inline_scripts_and_heredocs_are_not_shell_paths() {
        // 27B straight-mode run: a FastAPI TestClient script was refused as `run (may write) outside (/auth/register,, …)`
        let d = tmp("scripts");
        let proj = d.join("proj");
        let out = |c: &str| bash_outside(c, &proj);
        let heredoc = "python - <<'EOF'\nfrom fastapi.testclient import TestClient\nfrom app.main import app\nclient = TestClient(app)\n\
r = client.post(\"/auth/register\", json={\"username\": \"a\", \"password\": \"b\"})\nprint(r.status_code, r.json())\n\
r = client.post(\"/auth/login\", data={\"username\": \"a\", \"password\": \"b\"})\nprint(client.get(\"/items/9999\").status_code)\nEOF";
        let dash = "cd src && python3 <<-EOF\n\tclient.get(\"/items/9999\")\n\topen('/etc/x', 'w')\n\tEOF\necho done";
        let pyc = "python -c \"from fastapi.testclient import TestClient; from app.main import app; c = TestClient(app); \
print(c.post('/auth/register', json={'u': 'a'}).status_code); print(c.get('/items/9999').status_code)\"";
        let pyc2 = "python3 -c '\nfrom fastapi.testclient import TestClient\nc = TestClient(app)\nc.post(\"/auth/register\", json={})\n'";
        for c in [heredoc, dash, pyc, pyc2, "uv run python -c 'print(open(\"/etc/hosts\").read())'", "node -e \"fetch('/items/9999')\"",
                  "cat <<EOF > out.txt\n/auth/register\n/etc/passwd\nEOF", "grep x <<< '/auth/register /etc/x'"] {
            assert!(out(c).is_empty(), "{c}: {:?}", out(c));
        }
        // real shell-level writes still ask
        let r = |p: &str| resolve(Path::new("/"), Path::new(p)).to_string_lossy().into_owned();
        assert_eq!(out("python x.py > /etc/x"), vec![r("/etc/x")]);
        assert_eq!(out("cp a /tmp/b"), vec![r("/tmp/b")]);
        assert_eq!(out("python - <<'EOF' > /tmp/b\nprint('/auth/x')\nEOF"), vec![r("/tmp/b")]);
        assert_eq!(out("python - <<EOF\nprint(1)\nEOF\ncp a /tmp/b"), vec![r("/tmp/b")], "commands after the heredoc are checked");
        assert_eq!(out("bash -c 'cp a /tmp/b'"), vec![r("/tmp/b")], "a shell's -c is a command");
        assert_eq!(out("python -c 'print(1)' > /tmp/b"), vec![r("/tmp/b")]);
    }

    #[test]
    fn modes_and_session_grants() {
        assert_eq!(["ask", "read-ok", "deny", "allow"].map(|s| Mode::parse(s).unwrap().word()), ["ask", "read-ok", "deny", "allow"]);
        assert_eq!(Mode::parse("yes"), None);
        assert_eq!(Mode::Allow.stricter(Mode::Ask), Mode::Ask);
        assert_eq!(Mode::Ask.stricter(Mode::Allow), Mode::Ask);
        assert_eq!(Mode::ReadOk.stricter(Mode::Deny), Mode::Deny);
        let o = |a: &str, p: &str| Outside { access: a.into(), paths: vec![p.into()] };
        let g = vec![(PathBuf::from("/x/lib"), false)];
        assert!(covered(&g, &o("read", "/x/lib/a.rs")));
        assert!(!covered(&g, &o("write", "/x/lib/a.rs")), "a read grant does not cover a write");
        assert!(!covered(&g, &o("read", "/x/libz")));
        let g = vec![(PathBuf::from("/x/lib"), true)];
        assert!(covered(&g, &o("bash", "/x/lib")));
        assert!(!covered(&g, &o("bash", "$TMPDIR/x")), "unknown paths are never covered");
    }
}
