//! The shell sandbox (config `shell_sandbox`): on macOS every `bash` tool command and every check command
//! (`tools::shell`) runs under `/usr/bin/sandbox-exec` with a Seatbelt profile built here for the project folder, so
//! an approved command still cannot write outside the project, rewrite Elisa's own permission files, plant git hooks
//! or read the usual secret stores.
//!
//! Modes: "write" (default on macOS) — file writes only inside the project, the temp folders and the build tool
//! caches; secret stores unreadable; network as usual. "strict" — the same, and no network except localhost (TCP/UDP)
//! and unix sockets inside the project / temp folders (DNS lookups are blocked too). "off" — no sandbox (the default
//! elsewhere; on other systems the setting is a no-op). Seatbelt (`sandbox-exec`) is deprecated by Apple but still
//! ships and works; it is best effort, not a security boundary against a determined attacker (see README).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Off, Write, Strict }

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() { "off" => Some(Mode::Off), "write" => Some(Mode::Write), "strict" => Some(Mode::Strict), _ => None }
    }
    pub fn word(self) -> &'static str { match self { Mode::Off => "off", Mode::Write => "write", Mode::Strict => "strict" } }
    fn rank(self) -> u8 { match self { Mode::Off => 0, Mode::Write => 1, Mode::Strict => 2 } }
    /// the stricter of the two (a project's `.elisa/config.toml` may only tighten the user's setting)
    pub fn stricter(self, o: Mode) -> Mode { if o.rank() > self.rank() { o } else { self } }
    /// "write" on macOS, "off" elsewhere
    pub fn platform_default() -> Mode { if cfg!(target_os = "macos") { Mode::Write } else { Mode::Off } }
}

/// 0 = not set (the platform default), else 1 + rank
static MODE: AtomicU8 = AtomicU8::new(0);

/// The session's mode (from `Config::shell_sandbox`); used by every `tools::shell` run after this.
pub fn set_mode(m: Mode) { MODE.store(1 + m.rank(), Ordering::SeqCst); }

/// The mode `tools::shell` uses: what `set_mode` said, else the platform default. Always Off off macOS.
pub fn mode() -> Mode {
    if !cfg!(target_os = "macos") { return Mode::Off; }
    match MODE.load(Ordering::SeqCst) { 1 => Mode::Off, 2 => Mode::Write, 3 => Mode::Strict, _ => Mode::platform_default() }
}

/// Secret stores under the home folder that commands may neither read nor write. The Colima / Lima ssh keys are
/// here too: with them a command could ssh into the VM, which mounts the home folder writable.
const SECRET_HOME: [&str; 27] = [
    ".ssh", ".aws", ".gnupg", ".netrc", ".git-credentials", ".password-store", ".kube", ".azure", ".pypirc",
    ".docker/config.json", ".cargo/credentials", ".cargo/credentials.toml", ".config/elisa", ".config/gh", ".config/gcloud",
    "Library/Keychains", "Library/Cookies", "Library/Safari", "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox", "Library/Application Support/BraveSoftware", "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Arc", "Library/Application Support/Chromium", ".mozilla",
    ".colima/_lima/_config", ".lima/_config",
];
/// readable again inside a denied store (ssh needs them to check host keys; keys stay unreadable, an agent still works)
const SECRET_HOME_READ_OK: [&str; 2] = [".ssh/known_hosts", ".ssh/config"];
/// build tool caches under $HOME that builds write to (directories)
const CACHE_HOME: [&str; 7] = [".cargo/registry", ".cargo/git", ".npm", ".cache", "Library/Caches", ".gradle/caches", "Library/org.swift.swiftpm"];
/// cargo's lock / index files directly in ~/.cargo
const CACHE_HOME_FILES: [&str; 6] = [
    ".cargo/.package-cache", ".cargo/.package-cache-mutate", ".cargo/.global-cache",
    ".cargo/.global-cache-journal", ".cargo/.global-cache-wal", ".cargo/.global-cache-shm",
];
/// Container engine sockets under the home folder: every unix socket below these may not be connected to (a command
/// that reaches the Docker API can start a container that mounts any host folder, i.e. leave the sandbox). Colima's
/// `~/.colima/<profile>/docker.sock`, Lima's `~/.lima/<vm>/sock/*.sock`, Docker Desktop, OrbStack, Rancher Desktop.
const DOCKER_HOME_DIRS: [&str; 6] = [".colima", ".lima", ".docker/run", ".orbstack/run", ".rd", "Library/Containers/com.docker.docker/Data"];
/// system-wide Docker socket paths (each also as its symlink target, when it is one)
const DOCKER_SOCKETS: [&str; 2] = ["/var/run/docker.sock", "/private/var/run/docker.sock"];
/// programs that hand work to processes outside the sandbox (LaunchServices, Apple events, launchd)
const NO_EXEC: [&str; 3] = ["/usr/bin/open", "/usr/bin/osascript", "/bin/launchctl"];

/// What one sandboxed run may touch. `Policy::new` fills in the defaults; tests replace / extend the lists.
#[derive(Clone, Debug)]
pub struct Policy {
    pub mode: Mode,
    /// the project folder: writable (except the protected paths below)
    pub project: PathBuf,
    /// writable directories besides the project (temp folders, build caches)
    pub write_dirs: Vec<PathBuf>,
    /// writable single files (cargo's lock files)
    pub write_files: Vec<PathBuf>,
    /// neither readable nor writable (secret stores); wins over everything above
    pub deny: Vec<PathBuf>,
    /// readable again inside `deny`
    pub read_ok: Vec<PathBuf>,
    /// directories where unix sockets may be used in strict mode (default: the project and the temp folders)
    pub socket_dirs: Vec<PathBuf>,
    /// unix sockets that may never be connected to, in every mode (container engines: Docker, Colima, Lima); each a
    /// socket path or a directory (every socket below it). Wins over `socket_dirs`.
    pub deny_sockets: Vec<PathBuf>,
}

/// The user's real home folder from the password database (`getpwuid_r(getuid())`), not `$HOME`: a changed `$HOME`
/// must not move the secret-store deny list away from the real ~/.ssh.
pub fn real_home() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0 as libc::c_char; 4096];
    loop {
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe { libc::getpwuid_r(libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut res) };
        if rc == libc::ERANGE && buf.len() < (1 << 20) { buf.resize(buf.len() * 4, 0); continue; }
        if rc != 0 || res.is_null() || pw.pw_dir.is_null() { return None; }
        let dir = unsafe { CStr::from_ptr(pw.pw_dir) };
        if dir.to_bytes().is_empty() { return None; }
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())));
    }
}

/// The home folders the policy covers: the real one (password database) and `env_home` (`$HOME`) when it differs;
/// both resolved (`real`), real one first.
pub fn homes(env_home: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for h in [real_home(), env_home.filter(|h| !h.as_os_str().is_empty()).map(Path::to_path_buf)].into_iter().flatten() {
        let h = real(&h);
        if !v.contains(&h) { v.push(h); }
    }
    v
}

/// The user's temp folders: $TMPDIR, the per-user /var/folders/xx/yyy (temp + cache), /private/tmp, /private/var/tmp.
fn temp_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/private/tmp"), PathBuf::from("/private/var/tmp")];
    if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) { v.push(PathBuf::from(t)); }
    #[cfg(target_os = "macos")]
    for name in [libc::_CS_DARWIN_USER_TEMP_DIR, libc::_CS_DARWIN_USER_CACHE_DIR] {
        let mut buf = [0u8; 1024];
        let n = unsafe { libc::confstr(name, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if n > 1 && n <= buf.len() {
            let p = PathBuf::from(String::from_utf8_lossy(&buf[..n - 1]).into_owned());
            // /var/folders/xx/yyy/T/ -> /var/folders/xx/yyy (holds T, C and 0, all the user's own)
            if let Some(parent) = p.parent().filter(|q| q.starts_with("/var/folders") || q.starts_with("/private/var/folders")) {
                v.push(parent.to_path_buf());
            } else { v.push(p); }
        }
    }
    v
}

/// `p` with symlinks resolved the way the kernel sees it (Seatbelt matches real paths: /tmp is /private/tmp): the
/// longest existing ancestor canonicalized, the rest appended (so not-yet-existing files are covered too).
pub fn real(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() { return c; }
    let mut rest = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        match cur.file_name() {
            Some(n) => rest.push(n.to_os_string()),
            None => return p.to_path_buf(),
        }
        if !cur.pop() { return p.to_path_buf(); }
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for n in rest.iter().rev() { out.push(n); }
            return out;
        }
    }
}

/// An SBPL string literal for `p`. Paths with control characters or that are not UTF-8 are refused (the profile
/// would otherwise not say what it seems to say).
pub fn quote(p: &Path) -> Result<String, String> {
    let s = p.to_str().ok_or_else(|| format!("shell sandbox: path is not UTF-8: {}", p.display()))?;
    if s.chars().any(char::is_control) { return Err(format!("shell sandbox: path has a control character: {s:?}")); }
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        if ch == '"' || ch == '\\' { o.push('\\'); }
        o.push(ch);
    }
    o.push('"');
    Ok(o)
}

impl Policy {
    /// The default policy for `project` from the home folder(s) (`homes`: the real one and `$HOME`) and the temp
    /// folders.
    pub fn new(mode: Mode, project: &Path) -> Policy {
        let env_home = std::env::var_os("HOME").map(PathBuf::from);
        Policy::with_home(mode, project, env_home.as_deref())
    }

    /// `new` with `env_home` standing in for `$HOME` (tests inject one). The secret stores, caches and container
    /// sockets of the real home folder are always covered, and those of `env_home` too when it is another folder.
    pub fn with_home(mode: Mode, project: &Path, env_home: Option<&Path>) -> Policy {
        let project = real(project);
        let temps: Vec<PathBuf> = temp_dirs().iter().map(|p| real(p)).collect();
        let mut write_dirs = temps.clone();
        let (mut write_files, mut deny, mut read_ok, mut deny_sockets) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for h in homes(env_home) {
            write_dirs.extend(CACHE_HOME.iter().map(|r| h.join(r)));
            write_files.extend(CACHE_HOME_FILES.iter().map(|r| h.join(r)));
            deny.extend(SECRET_HOME.iter().map(|r| h.join(r)));
            read_ok.extend(SECRET_HOME_READ_OK.iter().map(|r| h.join(r)));
            deny_sockets.extend(DOCKER_HOME_DIRS.iter().map(|r| h.join(r)));
        }
        for s in DOCKER_SOCKETS.iter().map(PathBuf::from) {
            let r = real(&s);
            if !deny_sockets.contains(&s) { deny_sockets.push(s); }
            if !deny_sockets.contains(&r) { deny_sockets.push(r); }
        }
        let mut socket_dirs = vec![project.clone()];
        socket_dirs.extend(temps);
        Policy { mode, project, write_dirs, write_files, deny, read_ok, socket_dirs, deny_sockets }
    }

    /// Paths inside the project a command may not write: Elisa's permission / config / checks files (a command could
    /// otherwise grant itself tools or plant check commands) and, when the project is a git repository, the hooks
    /// and `.git/config` (`core.hooksPath`, `core.fsmonitor` run code at the next git command outside the sandbox).
    /// The `.elisa` and `.git` directories themselves are protected too, so they cannot be renamed away and rebuilt.
    pub fn protected(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let p = &self.project;
        let mut literal: Vec<PathBuf> = [".elisa", ".elisa/allow", ".elisa/config.toml", ".elisa/checks.toml"].iter().map(|r| p.join(r)).collect();
        let mut subpath = Vec::new();
        // no .git yet: `git init` must work (the next command's profile protects the new repository)
        if p.join(".git").is_dir() {
            literal.extend([".git", ".git/config", ".git/config.lock"].iter().map(|r| p.join(r)));
            subpath.push(p.join(".git/hooks"));
        }
        (literal, subpath)
    }

    /// The Seatbelt profile (SBPL). Later rules win over earlier ones for the same operation.
    pub fn profile(&self) -> Result<String, String> {
        let list = |kind: &str, ps: &[PathBuf]| -> Result<String, String> {
            ps.iter().map(|p| quote(p).map(|q| format!(" ({kind} {q})"))).collect()
        };
        let mut s = String::from("(version 1)\n(allow default)\n");
        // writes: nowhere, then the project, devices, temp folders and caches
        s.push_str("(deny file-write*)\n");
        s.push_str(&format!("(allow file-write* (subpath {}) (subpath \"/dev\"){}{})\n",
            quote(&self.project)?, list("subpath", &self.write_dirs)?, list("literal", &self.write_files)?));
        let (lit, sub) = self.protected();
        s.push_str(&format!("(deny file-write*{}{})\n", list("literal", &lit)?, list("subpath", &sub)?));
        // secrets: neither read nor write (wins over the project / temp allowances)
        if !self.deny.is_empty() {
            s.push_str(&format!("(deny file-read* file-write*{})\n", list("subpath", &self.deny)?));
        }
        if !self.read_ok.is_empty() {
            s.push_str(&format!("(allow file-read*{})\n", list("literal", &self.read_ok)?));
        }
        // hand-offs to processes outside the sandbox
        s.push_str(&format!("(deny process-exec{})\n", list("literal", &NO_EXEC.map(PathBuf::from))?));
        s.push_str("(deny mach-lookup (global-name \"com.apple.coreservices.appleevents\"))\n");
        if self.mode == Mode::Strict {
            s.push_str("(deny network*)\n");
            // outbound only to localhost; local servers may bind / accept. (Not `network* (local ip "localhost:*")`:
            // that filter also matches an outbound socket that is not bound yet, i.e. every connection.)
            s.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
            s.push_str("(allow network-bind network-inbound (local ip \"localhost:*\"))\n");
            if !self.socket_dirs.is_empty() {
                s.push_str(&format!("(allow network*{}{})\n",
                    self.socket_dirs.iter().map(|p| quote(p).map(|q| format!(" (remote unix-socket (subpath {q}))"))).collect::<Result<String, String>>()?,
                    self.socket_dirs.iter().map(|p| quote(p).map(|q| format!(" (local unix-socket (subpath {q}))"))).collect::<Result<String, String>>()?));
            }
        }
        // container engine sockets, in every mode and last so nothing above re-allows them: the Docker API starts
        // containers that mount host folders (the built-in `run_code` server talks to Docker itself, outside this)
        if !self.deny_sockets.is_empty() {
            s.push_str(&format!("(deny network*{})\n",
                self.deny_sockets.iter().map(|p| quote(p).map(|q| format!(" (remote unix-socket (subpath {q}))"))).collect::<Result<String, String>>()?));
        }
        Ok(s)
    }

    /// `zsh -c cmd`, under sandbox-exec unless the mode is Off. Fails (rather than running unsandboxed) when the
    /// profile cannot be built or sandbox-exec is missing.
    pub fn command(&self, cmd: &str) -> Result<Command, String> {
        if self.mode == Mode::Off || !cfg!(target_os = "macos") {
            let mut c = Command::new("/bin/zsh");
            c.arg("-c").arg(cmd);
            return Ok(c);
        }
        if !Path::new(SANDBOX_EXEC).is_file() {
            return Err(format!("shell sandbox: {SANDBOX_EXEC} not found; set shell_sandbox = \"off\" in ~/.config/elisa/config.toml to run commands without it"));
        }
        let mut c = Command::new(SANDBOX_EXEC);
        c.arg("-p").arg(self.profile()?).arg("/bin/zsh").arg("-c").arg(cmd);
        Ok(c)
    }
}

/// How every sandbox hint line starts (`blocked` looks for it, e.g. in a check's output).
pub const HINT_MARK: &str = "[Elisa: probably blocked by the shell sandbox";
const HINT_WRITE: &str = "[Elisa: probably blocked by the shell sandbox: writes outside the project (and to .elisa/ permission files, .git/hooks, secret stores) are not allowed; ask the user to run it themselves or change shell_sandbox]";
const HINT_NET: &str = "[Elisa: probably blocked by the shell sandbox: shell_sandbox = \"strict\" allows no network except localhost; ask the user to run it themselves or use shell_sandbox = \"write\"]";

/// One line for a command's output that looks blocked by the sandbox (None when it does not, or the mode is Off).
pub fn hint(mode: Mode, out: &str) -> Option<&'static str> {
    if mode == Mode::Off { return None; }
    let l = out.to_ascii_lowercase();
    if l.contains("operation not permitted") { return Some(HINT_WRITE); }
    if mode == Mode::Strict && ["could not resolve host", "nodename nor servname", "could not connect to server", "network is unreachable", "failed to lookup address", "name resolution"].iter().any(|k| l.contains(k)) {
        return Some(HINT_NET);
    }
    None
}

/// Did the sandbox (probably) block something in this output? True when `shell` appended a hint line to it.
pub fn blocked(out: &str) -> bool { out.contains(HINT_MARK) }

/// A strict-mode run that failed quietly (`curl -s` exits 6 / 7 and prints nothing, so `hint` sees no message): the
/// network hint when it exited non-zero with little or no output and the command line runs a network tool (curl,
/// wget, ssh / scp / sftp, nc, telnet, rsync, git clone / fetch / pull / push / ls-remote, a package manager's
/// install / add / fetch). None in the other modes, on success, or when `hint` already said something.
pub fn quiet_network_hint(mode: Mode, cmd: &str, out: &str, code: Option<i32>) -> Option<&'static str> {
    if mode != Mode::Strict || code == Some(0) || hint(mode, out).is_some() { return None; }
    let t = out.trim();
    if t.lines().count() > 2 || t.chars().count() > 200 { return None; }
    runs_network_tool(cmd).then_some(HINT_NET)
}

/// Does the command line start a network tool in one of its simple commands (split at `;`, `&`, `|`, newlines,
/// parentheses, backticks and `$(`)? Leading `VAR=value` words, `sudo` / `env` / `command` / `time` / `exec` are
/// skipped; the tool is matched by its file name (`/usr/bin/curl` too).
pub fn runs_network_tool(cmd: &str) -> bool {
    const TOOLS: [&str; 11] = ["curl", "wget", "ssh", "scp", "sftp", "nc", "ncat", "netcat", "telnet", "rsync", "ftp"];
    const GIT: [&str; 5] = ["clone", "fetch", "pull", "push", "ls-remote"];
    const PKG: [&str; 14] = ["npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "poetry", "cargo", "gem", "go", "brew", "pod", "composer"];
    const PKG_SUB: [&str; 7] = ["install", "i", "add", "fetch", "get", "update", "download"];
    let seg_breaks = |c: char| matches!(c, ';' | '&' | '|' | '\n' | '(' | ')' | '`');
    cmd.replace("$(", "(").split(seg_breaks).any(|seg| {
        let words: Vec<&str> = seg.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'')).collect();
        let mut i = 0;
        while i < words.len() && (words[i].contains('=') && !words[i].starts_with('-') || ["sudo", "env", "command", "time", "exec", "nohup"].contains(&words[i])) { i += 1; }
        let Some(first) = words.get(i) else { return false };
        let name = first.rsplit('/').next().unwrap_or(first);
        let rest = &words[i + 1..];
        TOOLS.contains(&name)
            || (name == "git" && rest.iter().any(|w| GIT.contains(w)))
            || (PKG.contains(&name) && rest.iter().any(|w| PKG_SUB.contains(w)))
            || (name.starts_with("python") && rest.windows(3).any(|w| w[0] == "-m" && w[1].starts_with("pip") && PKG_SUB.contains(&w[2])))
    })
}
