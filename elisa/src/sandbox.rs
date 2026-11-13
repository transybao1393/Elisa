//! The shell sandbox (config `shell_sandbox`): on macOS every `bash` tool command and every check command
//! (`tools::shell`) runs under `/usr/bin/sandbox-exec` with a Seatbelt profile built here for the project folder, so
//! an approved command still cannot write outside the project, rewrite Elisa's own permission files, plant git hooks
//! or read the usual secret stores.
//!
//! Modes: "write" (default on macOS) — file writes only inside the project, the temp folders and the build tool
//! caches; secret stores unreadable; network as usual. "strict" — the same, and no network except localhost (TCP/UDP)
//! and unix sockets inside the project / temp folders (DNS lookups are blocked too). "colima" — no Seatbelt: the
//! command runs with bash in a throwaway container on Colima's docker (the run_code helpers in `mcp_run.rs`) that sees
//! only the project folder (`.elisa` and the git hooks / config read-only), with no network, a read-only root and no
//! capabilities; only images already on the machine are used (`--pull never`). "off" — no sandbox (the default
//! elsewhere; on other systems the setting is a no-op). Seatbelt (`sandbox-exec`) is deprecated by Apple but still
//! ships and works; it is best effort, not a security boundary against a determined attacker. See docs/sandbox.md.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Off, Write, Strict, Colima }

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() { "off" => Some(Mode::Off), "write" => Some(Mode::Write), "strict" => Some(Mode::Strict), "colima" => Some(Mode::Colima), _ => None }
    }
    pub fn word(self) -> &'static str { match self { Mode::Off => "off", Mode::Write => "write", Mode::Strict => "strict", Mode::Colima => "colima" } }
    /// "colima" ranks above "strict": it sees only the project folder and has no network at all
    fn rank(self) -> u8 { match self { Mode::Off => 0, Mode::Write => 1, Mode::Strict => 2, Mode::Colima => 3 } }
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
    match MODE.load(Ordering::SeqCst) { 1 => Mode::Off, 2 => Mode::Write, 3 => Mode::Strict, 4 => Mode::Colima, _ => Mode::platform_default() }
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
    /// profile cannot be built or sandbox-exec is missing. The "colima" mode needs `prepare` (its container must be
    /// stopped by name on a timeout).
    pub fn command(&self, cmd: &str) -> Result<Command, String> {
        if self.mode == Mode::Off || !cfg!(target_os = "macos") {
            let mut c = Command::new("/bin/zsh");
            c.arg("-c").arg(cmd);
            return Ok(c);
        }
        if self.mode == Mode::Colima { return Err("shell sandbox: the colima mode runs through Policy::prepare".into()); }
        if !Path::new(SANDBOX_EXEC).is_file() {
            return Err(missing_sandbox_exec(colima_ready()));
        }
        let mut c = Command::new(SANDBOX_EXEC);
        c.arg("-p").arg(self.profile()?).arg("/bin/zsh").arg("-c").arg(cmd);
        Ok(c)
    }

    /// The command for one run (`tools::shell`): `command` in every mode but "colima", which builds a `docker run`
    /// in a throwaway container (`container_args`). `timeout_s` is the caller's limit; the container also gets a
    /// backstop `timeout` a few seconds longer, in case Elisa itself dies.
    pub fn prepare(&self, cmd: &str, timeout_s: u64) -> Result<Prepared, String> {
        if self.mode != Mode::Colima || !cfg!(target_os = "macos") {
            return Ok(Prepared { cmd: self.command(cmd)?, container: None });
        }
        let (docker, daemon) = colima_daemon()?;
        if !daemon.from_env {
            // Colima shares the home folder with its VM by default (`mounts: []`): a project elsewhere would show up
            // as an empty folder in the container
            if let Some(h) = real_home().map(|h| real(&h)) {
                if !self.project.starts_with(&h) {
                    return Err(format!("shell sandbox: {} is outside your home folder, which is all Colima shares with its VM by default; \
                        move the project under {} or add it to `mounts` in ~/.colima/default/colima.yaml (see docs/sandbox.md)", self.project.display(), h.display()));
                }
            }
        }
        let image = shell_image(&docker, &daemon)?;
        let (ro, made) = self.container_mounts().map_err(|e| format!("shell sandbox: {e}"))?;
        let name = crate::mcp_run::unique_name("shell");
        let user = unsafe { (libc::getuid(), libc::getgid()) };
        let args = match container_args(&name, &image, &self.project, user, timeout_s, &ro) {
            Ok(a) => a,
            Err(e) => { remove_made(&made); return Err(e); }
        };
        let mut c = Command::new(&docker);
        c.args(args).env("DOCKER_HOST", &daemon.host).env_remove("DOCKER_CONTEXT").env("ELISA_CMD", cmd);
        Ok(Prepared { cmd: c, container: Some(Container { name, docker, host: daemon.host, made }) })
    }

    /// The paths inside the project the container gets read-only (Seatbelt's `protected` list, as mounts): `.elisa`,
    /// and in a git repository `.git/config` and `.git/hooks`. A missing `.elisa` or `.git/hooks` folder is made
    /// first (and removed again after the run when still empty, `made`), so the container cannot create it either.
    pub fn container_mounts(&self) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
        let p = &self.project;
        let (mut ro, mut made) = (Vec::new(), Vec::new());
        let mut need_dir = |d: PathBuf, ro: &mut Vec<PathBuf>| -> Result<(), String> {
            if !d.exists() {
                std::fs::create_dir(&d).map_err(|e| format!("cannot make {}: {e}", d.display()))?;
                made.push(d.clone());
            }
            ro.push(d);
            Ok(())
        };
        need_dir(p.join(".elisa"), &mut ro)?;
        if p.join(".git").is_dir() {
            if p.join(".git/config").exists() { ro.push(p.join(".git/config")); }
            need_dir(p.join(".git/hooks"), &mut ro)?;
        }
        Ok((ro, made))
    }
}

/// A command ready to spawn, and (the "colima" mode) its container.
pub struct Prepared {
    pub cmd: Command,
    pub container: Option<Container>,
}

/// The container of one "colima" run: `stop` it on a timeout or Ctrl-C (killing the docker client does not stop
/// it), `finish` after the run.
pub struct Container {
    pub name: String,
    docker: PathBuf,
    host: String,
    /// mount points this run made in the project (removed again when still empty)
    made: Vec<PathBuf>,
}

impl Container {
    /// `docker kill` the container (by its unique name)
    pub fn stop(&self) { crate::mcp_run::kill(&self.docker, &self.host, &self.name); }
    /// remove the mount points this run made, when they are still empty
    pub fn finish(&self) { remove_made(&self.made); }
}

fn remove_made(made: &[PathBuf]) {
    for d in made.iter().rev() { let _ = std::fs::remove_dir(d); }
}

/// The `docker run` arguments of one "colima" shell command: the project mounted at its own path (so paths read the
/// same inside) and the working folder, `ro` mounted read-only over it, no network, a read-only root with a tmpfs at
/// /tmp (HOME), the user's own uid / gid (files written in the project stay theirs), no capabilities, no new
/// privileges, at most 256 processes, 1 GB memory, one CPU, never a pull. The command itself travels in the docker
/// client's environment (`ELISA_CMD`, not its arguments, which `ps` shows) and runs with bash under a backstop
/// `timeout`. Paths with a comma (the `--mount` separator) or a control character are refused.
pub fn container_args(name: &str, image: &str, project: &Path, user: (u32, u32), timeout_s: u64, ro: &[PathBuf]) -> Result<Vec<String>, String> {
    let path = |p: &Path| -> Result<String, String> {
        let s = p.to_str().ok_or_else(|| format!("shell sandbox: path is not UTF-8: {}", p.display()))?;
        if s.contains(',') || s.chars().any(char::is_control) || !s.starts_with('/') {
            return Err(format!("shell sandbox: the colima mode cannot mount {s:?} (a comma or control character, or not absolute)"));
        }
        Ok(s.to_string())
    };
    let proj = path(project)?;
    let backstop = timeout_s.max(1) + 5;
    let mut a: Vec<String> = [
        "run", "--rm", "--pull", "never", "--name", name, "--init", "--network", "none", "--read-only",
        "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=512m", "--workdir", &proj, "--memory", "1024m", "--memory-swap", "1024m",
        "--cpus", "1", "--pids-limit", "256", "--user", &format!("{}:{}", user.0, user.1), "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges", "--log-driver", "none",
        "--mount", &format!("type=bind,src={proj},dst={proj}"),
    ].iter().map(|s| s.to_string()).collect();
    for p in ro {
        let q = path(p)?;
        a.extend(["--mount".into(), format!("type=bind,src={q},dst={q},readonly")]);
    }
    a.extend(["-e", "HOME=/tmp", "-e", "ELISA_CMD", image, "sh", "-c"].map(String::from));
    a.push(format!("exec timeout -s KILL {backstop} bash -c \"$ELISA_CMD\""));
    Ok(a)
}

/// The docker CLI and Colima's daemon (`mcp_run::find_daemon`: `DOCKER_HOST`, else `~/.colima/default/docker.sock`
/// under `$HOME`, then under the real home folder, else `/var/run/docker.sock`).
fn colima_daemon() -> Result<(PathBuf, crate::mcp_run::Daemon), String> {
    let env_host = std::env::var("DOCKER_HOST").ok();
    let env_home = std::env::var_os("HOME").map(PathBuf::from);
    let daemon = crate::mcp_run::find_daemon(env_host.as_deref(), env_home.as_deref())
        .or_else(|e| real_home().map_or(Err(e), |h| crate::mcp_run::find_daemon(env_host.as_deref(), Some(&h))))
        .map_err(|e| format!("shell sandbox (colima): {e}"))?;
    let docker = crate::mcp_run::docker_bin().ok_or("shell sandbox (colima): the docker CLI is not installed (brew install docker colima)")?;
    Ok((docker, daemon))
}

/// Colima (or another docker daemon) and the docker CLI are both here, so `shell_sandbox = "colima"` would work.
pub fn colima_ready() -> bool { colima_daemon().is_ok() }

/// The image "colima" commands run in: the bash line from the one pinned list (`images.rs`), the newest one present.
/// Never pulled: a missing image is an error that says how the user can download it.
fn shell_image(docker: &Path, daemon: &crate::mcp_run::Daemon) -> Result<String, String> {
    let want = crate::images::stable("bash").image;
    let have = crate::mcp_run::present_images(docker, daemon, want);
    let c = crate::images::resolve("bash", None, None, &|i| have.iter().any(|h| h == i.image))?;
    if have.iter().any(|h| h == c.img.image) { return Ok(c.img.image.to_string()); }
    Err(format!("shell sandbox (colima): the image {} is not on this machine and Elisa never downloads one by itself; \
        the user can get it with /sandbox pull bash (or `docker pull {}`)", c.img.image, c.img.image))
}

/// The error when `/usr/bin/sandbox-exec` is gone: Colima first when it is ready here, "off" last.
pub fn missing_sandbox_exec(colima: bool) -> String {
    if colima {
        format!("shell sandbox: {SANDBOX_EXEC} not found (Apple has deprecated it). Colima is running here: set shell_sandbox = \"colima\" \
            in ~/.config/elisa/config.toml to run commands in a container instead (see docs/sandbox.md); shell_sandbox = \"off\" runs them with no sandbox at all")
    } else {
        format!("shell sandbox: {SANDBOX_EXEC} not found (Apple has deprecated it). Install and start Colima (brew install colima docker; colima start) \
            and set shell_sandbox = \"colima\" in ~/.config/elisa/config.toml to run commands in a container (see docs/sandbox.md); \
            shell_sandbox = \"off\" runs them with no sandbox at all")
    }
}

/// How every sandbox hint line starts (`blocked` looks for it, e.g. in a check's output).
pub const HINT_MARK: &str = "[Elisa: probably blocked by the shell sandbox";
const HINT_WRITE: &str = "[Elisa: probably blocked by the shell sandbox: writes outside the project (and to .elisa/ permission files, .git/hooks, secret stores) are not allowed; ask the user to run it themselves or change shell_sandbox]";
const HINT_NET: &str = "[Elisa: probably blocked by the shell sandbox: shell_sandbox = \"strict\" allows no network except localhost; ask the user to run it themselves or use shell_sandbox = \"write\"]";
const HINT_COLIMA: &str = "[Elisa: probably blocked by the shell sandbox: shell_sandbox = \"colima\" runs commands in a container that sees only the project folder (.elisa/ and .git/hooks read-only), has no network and only the tools in its image; ask the user to run it themselves or change shell_sandbox]";

/// One line for a command's output that looks blocked by the sandbox (None when it does not, or the mode is Off).
pub fn hint(mode: Mode, out: &str) -> Option<&'static str> {
    if mode == Mode::Off { return None; }
    let l = out.to_ascii_lowercase();
    if mode == Mode::Colima {
        let k = ["operation not permitted", "read-only file system", "permission denied", "command not found", "could not resolve host",
            "nodename nor servname", "network is unreachable", "temporary failure in name resolution", "bad address"];
        return k.iter().any(|k| l.contains(k)).then_some(HINT_COLIMA);
    }
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
/// install / add / fetch). The same in "colima" (no network at all), with its own hint. None in the other modes, on
/// success, or when `hint` already said something.
pub fn quiet_network_hint(mode: Mode, cmd: &str, out: &str, code: Option<i32>) -> Option<&'static str> {
    if !matches!(mode, Mode::Strict | Mode::Colima) || code == Some(0) || hint(mode, out).is_some() { return None; }
    let t = out.trim();
    if t.lines().count() > 2 || t.chars().count() > 200 { return None; }
    runs_network_tool(cmd).then_some(if mode == Mode::Colima { HINT_COLIMA } else { HINT_NET })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> crate::testdir::TempDir { crate::testdir::TempDir::real(&format!("sbx-unit-{tag}")) }

    #[test]
    fn colima_mode_parses_and_is_the_strictest() {
        assert_eq!(Mode::parse(" colima "), Some(Mode::Colima));
        assert_eq!(Mode::Colima.word(), "colima");
        for m in [Mode::Off, Mode::Write, Mode::Strict] {
            assert_eq!(m.stricter(Mode::Colima), Mode::Colima);
            assert_eq!(Mode::Colima.stricter(m), Mode::Colima);
        }
        // the default stays what it was
        assert_eq!(Mode::platform_default(), if cfg!(target_os = "macos") { Mode::Write } else { Mode::Off });
    }

    #[test]
    fn container_args_lock_the_run_down() {
        let proj = Path::new("/Users/u/proj");
        let ro = [proj.join(".elisa"), proj.join(".git/hooks")];
        let a = container_args("elisa-shell-1", "bash:5.3", proj, (501, 20), 30, &ro).unwrap();
        let has = |w: &[&str]| a.windows(w.len()).any(|x| x.iter().zip(w).all(|(a, b)| a == b));
        assert_eq!(&a[..2], ["run", "--rm"]);
        for w in [&["--pull", "never"][..], &["--network", "none"], &["--read-only"], &["--cap-drop", "ALL"], &["--security-opt", "no-new-privileges"],
            &["--user", "501:20"], &["--workdir", "/Users/u/proj"], &["--name", "elisa-shell-1"], &["--mount", "type=bind,src=/Users/u/proj,dst=/Users/u/proj"],
            &["--mount", "type=bind,src=/Users/u/proj/.elisa,dst=/Users/u/proj/.elisa,readonly"],
            &["--mount", "type=bind,src=/Users/u/proj/.git/hooks,dst=/Users/u/proj/.git/hooks,readonly"], &["-e", "ELISA_CMD"], &["-e", "HOME=/tmp"]] {
            assert!(has(w), "{w:?} missing in {a:?}");
        }
        // the only writable host folder is the project; the command is not in the arguments; a backstop timeout
        assert_eq!(a.iter().filter(|x| x.starts_with("type=bind") && !x.ends_with(",readonly")).count(), 1);
        assert!(!a.iter().any(|x| x.contains("docker.sock") || x.starts_with("--privileged") || x == "host"));
        let i = a.iter().position(|x| x == "bash:5.3").unwrap();
        assert_eq!(&a[i + 1..i + 3], ["sh", "-c"]);
        assert_eq!(a[i + 3], "exec timeout -s KILL 35 bash -c \"$ELISA_CMD\"");
        assert_eq!(a.len(), i + 4);
    }

    #[test]
    fn container_args_refuse_paths_mount_cannot_say() {
        for bad in ["/Users/u/a,b", "/Users/u/a\nb", "relative/p"] {
            assert!(container_args("n", "bash:5.3", Path::new(bad), (1, 1), 5, &[]).is_err(), "{bad:?}");
        }
        assert!(container_args("n", "bash:5.3", Path::new("/ok"), (1, 1), 5, &[PathBuf::from("/ok/x,y")]).is_err());
    }

    #[test]
    fn container_mounts_protect_elisa_and_git_and_clean_up() {
        let p = tmp("mounts");
        // no .git: only .elisa, made for the run and removed after
        let pol = Policy::with_home(Mode::Colima, &p, None);
        let (ro, made) = pol.container_mounts().unwrap();
        assert_eq!(ro, [p.join(".elisa")]);
        assert_eq!(made, [p.join(".elisa")]);
        remove_made(&made);
        assert!(!p.join(".elisa").exists());
        // a git repository with its config and no hooks folder; an existing .elisa is kept
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git/config"), "[core]\n").unwrap();
        std::fs::create_dir_all(p.join(".elisa")).unwrap();
        std::fs::write(p.join(".elisa/allow"), "").unwrap();
        let (ro, made) = pol.container_mounts().unwrap();
        assert_eq!(ro, [p.join(".elisa"), p.join(".git/config"), p.join(".git/hooks")]);
        assert_eq!(made, [p.join(".git/hooks")]);
        remove_made(&made);
        assert!(!p.join(".git/hooks").exists() && p.join(".elisa/allow").exists());
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn missing_sandbox_exec_offers_colima_before_off() {
        for ready in [true, false] {
            let m = missing_sandbox_exec(ready);
            let (c, o) = (m.find("\"colima\"").unwrap(), m.find("\"off\"").unwrap());
            assert!(c < o, "{m}");
            assert!(m.contains("docs/sandbox.md") && m.contains(SANDBOX_EXEC));
            assert_eq!(m.contains("Colima is running here"), ready);
        }
    }

    #[test]
    fn colima_mode_needs_prepare_and_has_its_own_hint() {
        if cfg!(target_os = "macos") {
            let p = tmp("cmd");
            assert!(Policy::with_home(Mode::Colima, &p, None).command("true").is_err());
            // the other modes still go through `command` unchanged
            let w = Policy::with_home(Mode::Write, &p, None).prepare("true", 5).unwrap();
            assert!(w.container.is_none());
            assert_eq!(w.cmd.get_program(), SANDBOX_EXEC);
            let _ = std::fs::remove_dir_all(&p);
        }
        let h = hint(Mode::Colima, "touch: /etc/x: Read-only file system").unwrap();
        assert!(h.contains("colima") && !h.contains('\n'));
        assert!(hint(Mode::Colima, "bash: cargo: command not found").is_some());
        assert!(hint(Mode::Colima, "all fine").is_none());
        assert_eq!(quiet_network_hint(Mode::Colima, "curl -s e.com", "", Some(6)), Some(HINT_COLIMA));
        assert_eq!(quiet_network_hint(Mode::Colima, "curl -s e.com", "", Some(0)), None);
    }
}
