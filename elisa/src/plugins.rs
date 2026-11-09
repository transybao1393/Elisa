//! Plugins: a local folder with a `plugin.toml` that bundles MCP servers, slash commands (`commands/*.md`,
//! `commands.rs`) and permission rules (`perms.rs`). v1 takes local folders only (no git, no URLs, no registry).
//!
//! - `/plugin add PATH` (or `elisa plugin add PATH`) shows exactly what will happen first: each server's package and
//!   pinned version, installer, image, mounts, environment names, the rules it asks for, its commands, and whether
//!   anything runs directly on the Mac. Nothing is installed until the user says yes.
//! - A server is installed inside Colima, never on the Mac: a throwaway container (`--rm`, network on only for this
//!   step, `--pull never`) puts the pinned package into a docker volume Elisa owns (`elisa-plugin-PLUGIN-SERVER`). The
//!   lockfile (`plugin.lock`) records package, version, image and a sha256 of the installed tree.
//! - It runs as an MCP stdio server through `elisa --plugin-run PLUGIN SERVER`, which checks the plugin is still enabled
//!   and trusted and then becomes `docker run -i --rm --network none --read-only …` with only the declared mounts
//!   (read-only unless `write = true`), the package volume read-only, no other host path and no docker socket. It
//!   lives in the session's MCP host like an `[mcp.NAME]` server: tool names, `tools` / `exclude`, approval key
//!   `mcp:NAME`, tool-list pins, process-group shutdown (closing its input ends the container; a container left behind
//!   by a killed session is removed the next time one starts). There is no network option for a running server.
//! - `runs = "host"` runs the server's command directly on the Mac like an `[mcp.NAME]` server (clean environment,
//!   process group); Elisa installs nothing for it, and the setup screen warns.
//! - The installed copy (`~/.elisa/plugins/NAME/`: plugin.toml and the commands folder) is hash-pinned: if it changes,
//!   the plugin is off until `/trust NAME` (or `/plugin trust NAME`). Every add / remove / enable / disable / update /
//!   trust goes into the audit log.
//! - Project plugins (`.elisa/plugins/NAME/plugin.toml` in a repository) may bring only slash commands and deny / ask
//!   rules; a server there is ignored with a note. Project commands and project plugins load only after a one-time
//!   trust prompt showing their full content; the trust is kept in `~/.elisa/project-trust.json` (repository path +
//!   content hash), never in the repository, and any change asks again.
use crate::commands::{self, FileCommand, Source};
use crate::mcp_host::{ServerCfg, ToolFilter};
use crate::perms::Level;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// under the home folder
pub const DIR: &str = ".elisa/plugins";
pub const MANIFEST: &str = "plugin.toml";
pub const LOCK: &str = "plugin.lock";
const STATE: &str = "plugins.json";
const PROJECT_TRUST: &str = ".elisa/project-trust.json";
/// where a server's package volume is mounted inside its containers
const PKG: &str = "/opt/plugin";
/// the argument `main.rs` hands to `run_server`
pub const RUN_ARG: &str = "--plugin-run";

// ---------- the manifest ----------

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    commands: Option<String>,
    plugin: MetaFile,
    #[serde(default)]
    server: Vec<ServerFile>,
    permissions: Option<PermsFile>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct MetaFile { name: String, version: String, #[serde(default)] description: String, commands: Option<String> }

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct ServerFile {
    name: String,
    installer: Option<String>,
    package: Option<String>,
    version: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    tools: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    mounts: Vec<MountFile>,
    runs: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct MountFile { path: String, #[serde(default)] write: bool }

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
struct PermsFile { #[serde(default)] allow: Vec<String>, #[serde(default)] ask: Vec<String>, #[serde(default)] deny: Vec<String> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installer { Npm, Pip, Cargo }

impl Installer {
    fn parse(s: &str) -> Option<Installer> { match s { "npm" => Some(Installer::Npm), "pip" => Some(Installer::Pip), "cargo" => Some(Installer::Cargo), _ => None } }
    pub fn word(self) -> &'static str { match self { Installer::Npm => "npm", Installer::Pip => "pip", Installer::Cargo => "cargo" } }
    /// the sandbox language whose image installs and runs it
    pub fn lang(self) -> &'static str { match self { Installer::Npm => "javascript", Installer::Pip => "python", Installer::Cargo => "rust" } }
    /// the stable image of its language on Elisa's list (`images.rs`), used when none of its lines is present
    /// (`Docker::installer_image` picks); a manifest cannot name an image. Downloaded only when the user says yes.
    pub fn image(self) -> &'static str { crate::images::stable(self.lang()).image }
    /// the install command inside the throwaway container (no shell: each word one argument)
    fn install_args(self, package: &str, version: &str) -> Vec<String> {
        let v: Vec<&str> = match self {
            // no install scripts: nothing of the package runs while the network is on
            Installer::Npm => vec!["npm", "install", "--prefix", PKG, "--no-audit", "--no-fund", "--ignore-scripts", "--omit=dev", "--loglevel=error"],
            Installer::Pip => vec!["pip", "install", "--no-cache-dir", "--disable-pip-version-check", "--no-warn-script-location", "--root-user-action=ignore", "--target", "/opt/plugin/lib"],
            Installer::Cargo => vec!["cargo", "install", "--locked", "--root", PKG],
        };
        let mut a: Vec<String> = v.into_iter().map(String::from).collect();
        match self {
            Installer::Npm => a.push(format!("{package}@{version}")),
            Installer::Pip => a.push(format!("{package}=={version}")),
            Installer::Cargo => a.extend([package.to_string(), "--version".into(), version.to_string()]),
        }
        a
    }
    /// the environment a running server gets for its package
    fn run_env(self) -> Vec<String> {
        match self {
            Installer::Npm => vec!["PATH=/opt/plugin/node_modules/.bin:/usr/local/bin:/usr/bin:/bin".into(), "NODE_ENV=production".into()],
            Installer::Pip => vec!["PATH=/opt/plugin/lib/bin:/usr/local/bin:/usr/bin:/bin".into(), "PYTHONPATH=/opt/plugin/lib".into(),
                "PYTHONUNBUFFERED=1".into(), "PYTHONDONTWRITEBYTECODE=1".into()],
            Installer::Cargo => vec!["PATH=/opt/plugin/bin:/usr/local/bin:/usr/bin:/bin".into()],
        }
    }
    fn timeout(self) -> Duration { Duration::from_secs(if self == Installer::Cargo { 1800 } else { 600 }) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mount { pub path: String, pub write: bool }

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    /// None: `runs = "host"` (nothing installed)
    pub installer: Option<Installer>,
    pub package: String,
    pub version: String,
    pub command: Vec<String>,
    pub filter: ToolFilter,
    pub env: Vec<(String, String)>,
    pub mounts: Vec<Mount>,
    pub host: bool,
}

impl Server {
    /// its package volume; `tag` keeps two Elisa homes on one Colima apart (`home_tag`)
    pub fn volume(&self, plugin: &str, tag: &str) -> String { format!("elisa-plugin-{plugin}-{}-{tag}", self.name) }
    fn runs(&self) -> &'static str { if self.host { "host" } else { "container" } }
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub servers: Vec<Server>,
    pub rules: Vec<(Level, String)>,
    /// the commands folder, relative to the plugin folder
    pub commands_dir: String,
    /// what was ignored (a project plugin's servers and allow rules)
    pub notes: Vec<String>,
}

fn re(p: &str) -> regex::Regex { regex::Regex::new(p).expect("plugin regex") }

/// a plugin or server name: lowercase letters, digits, `-` and `_` (it becomes part of a docker volume name)
fn plugin_name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn exact_version(inst: Installer, v: &str) -> bool {
    match inst {
        Installer::Npm | Installer::Cargo => re(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$").is_match(v),
        Installer::Pip => re(r"^\d+(\.\d+)*((a|b|rc)\d+)?(\.post\d+)?(\.dev\d+)?$").is_match(v),
    }
}

fn package_ok(inst: Installer, p: &str) -> bool {
    match inst {
        Installer::Npm => re(r"^(@[a-z0-9][a-z0-9._~-]*/)?[a-z0-9][a-z0-9._~-]*$").is_match(p),
        Installer::Pip => re(r"^[A-Za-z0-9]([A-Za-z0-9._-]*[A-Za-z0-9])?(\[[A-Za-z0-9,._-]+\])?$").is_match(p),
        Installer::Cargo => re(r"^[A-Za-z0-9_-]+$").is_match(p),
    }
}

/// environment names a plugin may not set: they would steer the docker client on the Mac or the loader
fn reserved_env(k: &str) -> bool {
    ["PATH", "HOME", "PYTHONPATH", "NODE_OPTIONS"].contains(&k) || ["DOCKER_", "DYLD_", "LD_", "ELISA_"].iter().any(|p| k.starts_with(p))
}

/// Parses and checks a manifest. `project`: a repository's plugin (servers and allow rules are left out, with notes).
pub fn parse(text: &str, project: bool) -> Result<Manifest, String> {
    let f: ManifestFile = toml::from_str(text).map_err(|e| {
        let e = e.to_string();
        if e.contains("`network`") { format!("{e}\nplugin servers have no network option: they run with --network none") } else { e }
    })?;
    let name = f.plugin.name.trim().to_string();
    if !plugin_name_ok(&name) { return Err(format!("plugin name {name:?}: use lowercase letters, digits, - and _ (at most 40)")); }
    let version = f.plugin.version.trim().to_string();
    if version.is_empty() || version.len() > 40 || version.contains(char::is_whitespace) { return Err("[plugin] version must be a short version string".into()); }
    let commands_dir = f.commands.or(f.plugin.commands).unwrap_or_else(|| "commands".into());
    if commands_dir.is_empty() || !commands_dir.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) || commands_dir.starts_with('.') {
        return Err(format!("commands = {commands_dir:?}: name a folder inside the plugin (letters, digits, _ - .)"));
    }
    let mut notes = Vec::new();
    let mut servers: Vec<Server> = Vec::new();
    if project && !f.server.is_empty() {
        notes.push(format!("plugin {name}: {} server(s) ignored — a project plugin may only bring commands and deny / ask rules", f.server.len()));
    }
    for s in f.server.into_iter().filter(|_| !project) {
        let sn = s.name.trim().to_string();
        let at = format!("server {sn:?}");
        if !plugin_name_ok(&sn) { return Err(format!("{at}: a server name uses lowercase letters, digits, - and _")); }
        if crate::tools::reserved_server_name(&sn) || ["web", "docs", "sec", "run", crate::mcp_reason::NAME].contains(&sn.as_str()) {
            return Err(format!("{at}: that name belongs to Elisa; give the server another name"));
        }
        if servers.iter().any(|o| o.name == sn) { return Err(format!("{at}: two servers have that name")); }
        let host = match s.runs.as_deref() {
            None | Some("container") => false,
            Some("host") => true,
            Some(o) => return Err(format!("{at}: runs = {o:?} (use \"container\", the default, or \"host\")")),
        };
        if s.command.is_empty() || s.command[0].trim().is_empty() { return Err(format!("{at}: command = [\"program\", \"args\"…] is required")); }
        if s.command.iter().any(|a| a.contains(['\0', '\n'])) { return Err(format!("{at}: command has a NUL or newline")); }
        let installer = match (s.installer.as_deref(), host) {
            (Some(i), _) => Some(Installer::parse(i).ok_or_else(|| format!("{at}: installer = {i:?} (use \"npm\", \"pip\" or \"cargo\")"))?),
            (None, true) => None,
            (None, false) => return Err(format!("{at}: installer = \"npm\" | \"pip\" | \"cargo\" is required for a container server")),
        };
        let (package, sversion) = (s.package.unwrap_or_default().trim().to_string(), s.version.unwrap_or_default().trim().to_string());
        // a host server installs nothing: Elisa never installs on the Mac
        let installer = if host { None } else { installer };
        if let Some(i) = installer {
            if !package_ok(i, &package) { return Err(format!("{at}: package {package:?} is not a valid {} package name", i.word())); }
            if !exact_version(i, &sversion) {
                return Err(format!("{at}: version {sversion:?} must be one exact version (like 1.2.3), not a range or a tag"));
            }
        }
        let mut env = Vec::new();
        for (k, v) in s.env {
            if !re(r"^[A-Za-z_][A-Za-z0-9_]*$").is_match(&k) { return Err(format!("{at}: env name {k:?} is not a variable name")); }
            if reserved_env(&k) { return Err(format!("{at}: env {k} may not be set by a plugin")); }
            env.push((k, v));
        }
        let mut mounts = Vec::new();
        for m in s.mounts {
            let p = m.path.trim().to_string();
            if p.is_empty() || p.contains([',', ':', '\0', '\n']) { return Err(format!("{at}: mount path {p:?} (no commas, colons or newlines)")); }
            if !(p.starts_with('/') || p.starts_with("~/") || p == "{project}" || p.starts_with("{project}/")) {
                return Err(format!("{at}: mount path {p:?} must be absolute, start with ~/ or be {{project}}"));
            }
            if host { notes.push(format!("{at}: mounts are ignored for runs = \"host\" (it sees the whole Mac)")); continue; }
            mounts.push(Mount { path: p, write: m.write });
        }
        let filter = ToolFilter { allow: s.tools.into_iter().collect(), deny: s.exclude.unwrap_or_default() };
        servers.push(Server { name: sn, installer, package, version: sversion, command: s.command, filter, env, mounts, host });
    }
    let mut rules = Vec::new();
    let p = f.permissions.unwrap_or_default();
    for (level, list) in [(Level::Allow, p.allow), (Level::Ask, p.ask), (Level::Deny, p.deny)] {
        for pat in list {
            crate::perms::parse(&pat).map_err(|e| format!("[permissions] {}: {e}", level.word()))?;
            if project && level == Level::Allow { notes.push(format!("plugin {name}: allow = {pat:?} ignored — a project plugin may only add deny / ask rules")); continue; }
            rules.push((level, pat));
        }
    }
    Ok(Manifest { name, version, description: crate::context::one_line(f.plugin.description.trim(), 200), servers, rules, commands_dir, notes })
}

// ---------- files: paths, content hash, state, lock ----------

pub fn root(home: &Path) -> PathBuf { home.join(DIR) }
pub fn plugin_dir(home: &Path, name: &str) -> PathBuf { root(home).join(name) }

/// the command files of a plugin folder (sorted), within the size limits
fn command_files(dir: &Path, commands_dir: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir.join(commands_dir)) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md") && p.is_file() && !p.is_symlink()).collect();
    v.sort();
    v
}

/// sha256 over a plugin folder's manifest and command files (name and bytes of each), the pinned content
pub fn content_hash(dir: &Path) -> Result<String, String> {
    let mpath = dir.join(MANIFEST);
    let text = std::fs::read(&mpath).map_err(|e| format!("{}: {e}", mpath.display()))?;
    let cdir = toml::from_str::<toml::Value>(&String::from_utf8_lossy(&text)).ok()
        .and_then(|v| v.get("commands").or_else(|| v.get("plugin").and_then(|p| p.get("commands"))).and_then(|c| c.as_str()).map(String::from))
        .unwrap_or_else(|| "commands".into());
    let mut buf: Vec<u8> = Vec::new();
    for (name, bytes) in std::iter::once((MANIFEST.to_string(), text)).chain(command_files(dir, &cdir).into_iter().filter_map(|p| {
        let n = format!("{cdir}/{}", p.file_name()?.to_string_lossy());
        std::fs::read(&p).ok().map(|b| (n, b))
    })) {
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
    }
    Ok(crate::pins::sha256_hex(&buf))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Entry { enabled: bool, source: String, hash: String, version: String, #[serde(default)] added: u64 }

#[derive(Serialize, Deserialize, Default, Debug)]
struct State { #[serde(default)] plugins: BTreeMap<String, Entry> }

fn now_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) }

fn write_json(path: &Path, v: &impl Serialize) -> Result<(), String> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?; }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(v).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

impl State {
    fn load(home: &Path) -> State {
        std::fs::read_to_string(root(home).join(STATE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    fn save(&self, home: &Path) -> Result<(), String> { write_json(&root(home).join(STATE), self) }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Lock { pub plugin: String, pub version: String, #[serde(default)] pub servers: Vec<LockServer> }

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct LockServer {
    pub name: String,
    pub installer: String,
    pub package: String,
    pub version: String,
    pub image: String,
    pub volume: String,
    /// sha256 of the installed tree (each file's path and sha256, each symlink's target)
    pub sha256: String,
    pub installed: u64,
    #[serde(default)]
    pub tools: Option<usize>,
    #[serde(default)]
    pub tokens: Option<usize>,
    /// why its last probe (after install / update) could not start it; None once it started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Lock {
    pub fn load(dir: &Path) -> Option<Lock> { std::fs::read_to_string(dir.join(LOCK)).ok().and_then(|t| serde_json::from_str(&t).ok()) }
    fn save(&self, dir: &Path) -> Result<(), String> { write_json(&dir.join(LOCK), self) }
    /// the locked install of `s`, when it is what the manifest declares
    fn of(&self, s: &Server) -> Option<&LockServer> {
        let i = s.installer?;
        self.servers.iter().find(|l| l.name == s.name && l.installer == i.word() && l.package == s.package && l.version == s.version)
    }
}

// ---------- docker (Colima) ----------

/// the docker CLI: on PATH, else where Homebrew puts it (as `mcp_run`)
fn docker_bin() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join("docker"))
        .chain(["/opt/homebrew/bin/docker", "/usr/local/bin/docker"].map(PathBuf::from))
        .find(|p| p.is_file())
}

pub struct Docker { bin: PathBuf, host: String, from_env: bool, tag: String }

/// 8 hex characters of the home folder's path: in volume names, so another home (another user, a test) on the same
/// Colima never shares or removes this one's packages
pub fn home_tag(home: &Path) -> String { crate::pins::sha256_hex(home.display().to_string().as_bytes())[..8].to_string() }

/// (exit code, stdout, stderr) of a finished command; Err when it could not start or ran past `timeout` (killed)
pub(crate) fn run_cmd(mut cmd: Command, timeout: Duration) -> Result<(Option<i32>, String, String), String> {
    use std::io::Read;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot run docker: {e}"))?;
    let read = |r: Option<Box<dyn Read + Send>>| std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(mut r) = r { let _ = r.by_ref().take(4 << 20).read_to_end(&mut s); }
        String::from_utf8_lossy(&s).into_owned()
    });
    let out = read(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let t0 = Instant::now();
    let status = loop {
        if let Ok(Some(s)) = child.try_wait() { break s; }
        if t0.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("docker did not finish in {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok((status.code(), out.join().unwrap_or_default(), err.join().unwrap_or_default()))
}

/// the last few non-empty lines of a command's output, for a message
fn tail(s: &str, n: usize) -> String {
    let l: Vec<&str> = s.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    l[l.len().saturating_sub(n)..].join("\n")
}

impl Docker {
    /// The docker CLI and Colima's daemon (`DOCKER_HOST` if set, else `~/.colima/default/docker.sock`, else
    /// `/var/run/docker.sock`), checked with one call. Elisa never starts Colima: Err says how to.
    pub fn find(home: &Path) -> Result<Docker, String> {
        let bin = docker_bin().ok_or("the docker CLI is not installed (brew install docker colima)")?;
        let d = crate::mcp_run::find_daemon(std::env::var("DOCKER_HOST").ok().as_deref(), Some(home))?;
        let docker = Docker { bin, host: d.host, from_env: d.from_env, tag: home_tag(home) };
        let (code, _, err) = run_cmd(docker.cmd(&["version", "--format", "{{.Server.Version}}"]), Duration::from_secs(15))?;
        if code != Some(0) { return Err(docker.why(&err)); }
        Ok(docker)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(args).env("DOCKER_HOST", &self.host).env_remove("DOCKER_CONTEXT");
        c
    }

    /// the docker CLI with these arguments, pointed at the same daemon as every run (`images::pull`)
    pub fn command(&self, args: &[&str]) -> Command { self.cmd(args) }

    fn why(&self, err: &str) -> String {
        if err.contains("Cannot connect to the Docker daemon") || err.contains("failed to connect to the docker API") || err.contains("Is the docker daemon running") {
            crate::mcp_run::NOT_RUNNING.to_string()
        } else { format!("docker: {}", tail(err, 4)) }
    }

    /// How to get a missing image: a listed one Elisa downloads when the user says yes (`/sandbox pull`, `elisa
    /// sandbox pull`); anything else (an image an older install recorded) the user pulls themselves.
    pub fn pull_hint(&self, image: &str) -> String {
        if crate::images::listed(image).is_some() { return format!("Elisa can download it: /sandbox pull {image} (or elisa sandbox pull {image})"); }
        if self.from_env { format!("pull it with: docker pull {image}") } else { format!("pull it with: docker -H {} pull {image}", self.host) }
    }

    pub fn image_present(&self, image: &str) -> Result<bool, String> { Ok(self.image_info(image)?.is_some()) }

    /// The image an installer uses: the newest listed line of its language that is present, else stable (which then
    /// is offered for download).
    pub fn installer_image(&self, inst: Installer) -> &'static str {
        let have = self.image_sizes();
        crate::images::default_line(inst.lang(), &|i| have.contains_key(i.image)).image
    }

    /// a local image's id, size and repository digest; None when it is not there
    pub fn image_info(&self, image: &str) -> Result<Option<crate::images::Local>, String> {
        let (code, out, err) = run_cmd(self.cmd(&["image", "inspect", "--format", "{{.Id}}|{{.Size}}|{{range .RepoDigests}}{{.}} {{end}}", image]), Duration::from_secs(15))?;
        if code == Some(0) {
            let mut p = out.trim().splitn(3, '|');
            let id = p.next().unwrap_or("").to_string();
            let bytes = p.next().and_then(|b| b.trim().parse().ok()).unwrap_or(0);
            let digest = p.next().and_then(|d| d.split_whitespace().next()).map(str::to_string);
            return Ok(Some(crate::images::Local { id, bytes, digest }));
        }
        if err.contains("No such image") || err.contains("No such object") || err.contains("not found") { return Ok(None); }
        Err(self.why(&err))
    }

    /// What `docker image ls` reports as each image's size (unpacked, on disk; `image inspect` gives the content
    /// size, which with the containerd store is the download): REPO:TAG → bytes
    pub fn image_sizes(&self) -> std::collections::HashMap<String, u64> {
        let Ok((Some(0), out, _)) = run_cmd(self.cmd(&["image", "ls", "--format", "{{.Repository}}:{{.Tag}}|{{.Size}}"]), Duration::from_secs(15)) else { return Default::default() };
        out.lines().filter_map(|l| {
            let (name, size) = l.trim().split_once('|')?;
            let s = size.trim();
            let at = s.find(|c: char| c.is_ascii_alphabetic())?;
            let n: f64 = s[..at].trim().parse().ok()?;
            let mult = match &s[at..] { "B" => 1.0, "kB" | "KB" => 1e3, "MB" => 1e6, "GB" => 1e9, "TB" => 1e12, _ => return None };
            Some((name.to_string(), (n * mult) as u64))
        }).collect()
    }

    /// `docker tag SRC DST` (puts a pinned image back under its tag)
    pub fn tag(&self, src: &str, dst: &str) -> Result<(), String> {
        let (code, _, err) = run_cmd(self.cmd(&["tag", src, dst]), Duration::from_secs(30))?;
        if code == Some(0) { Ok(()) } else { Err(self.why(&err)) }
    }

    /// `docker rmi IMAGE`
    pub fn rmi(&self, image: &str) -> Result<(), String> {
        let (code, _, err) = run_cmd(self.cmd(&["rmi", image]), Duration::from_secs(60))?;
        if code == Some(0) { Ok(()) } else { Err(self.why(&err)) }
    }

    /// the volume is there (docker would otherwise make an empty one when a container names it)
    fn volume_exists(&self, vol: &str) -> Result<bool, String> {
        let (code, _, err) = run_cmd(self.cmd(&["volume", "inspect", "--format", "{{.Name}}", vol]), Duration::from_secs(15))?;
        if code == Some(0) { return Ok(true); }
        if err.contains("no such volume") || err.contains("No such volume") { return Ok(false); }
        Err(self.why(&err))
    }

    fn volume_rm(&self, vol: &str) -> Result<(), String> {
        let (code, _, err) = run_cmd(self.cmd(&["volume", "rm", "-f", vol]), Duration::from_secs(30))?;
        if code == Some(0) { Ok(()) } else { Err(self.why(&err)) }
    }

    /// Installs `s` into a fresh volume in a throwaway container (network on for this step only), then hashes the
    /// installed tree in a second one with no network.
    fn install(&self, plugin: &str, s: &Server, say: &mut dyn FnMut(&str)) -> Result<LockServer, String> {
        let inst = s.installer.ok_or("nothing to install for a host server")?;
        let (image, vol) = (self.installer_image(inst), s.volume(plugin, &self.tag));
        self.volume_rm(&vol)?;
        let label = format!("elisa.plugin={plugin}");
        let (code, _, err) = run_cmd(self.cmd(&["volume", "create", "--label", &label, "--label", &format!("elisa.home={}", self.tag), &vol]), Duration::from_secs(30))?;
        if code != Some(0) { return Err(self.why(&err)); }
        say(&format!("{}: installing {} {}@{} in a throwaway container (network on for this step only)…", s.name, inst.word(), s.package, s.version));
        let mount = format!("{vol}:{PKG}");
        let mut args: Vec<String> = ["run", "--rm", "--pull", "never", "--label", &format!("elisa.plugin.install={plugin}"), "--network", "bridge",
            "--cap-drop", "ALL", "--cap-add", "CHOWN", "--cap-add", "DAC_OVERRIDE", "--cap-add", "FOWNER", "--cap-add", "SETUID", "--cap-add", "SETGID",
            "--security-opt", "no-new-privileges", "--memory", "2g", "-v", &mount, "--workdir", "/tmp", "-e", "HOME=/tmp", image]
            .iter().map(|x| x.to_string()).collect();
        args.extend(inst.install_args(&s.package, &s.version));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = run_cmd(self.cmd(&refs), inst.timeout());
        let fail = |e: String| { let _ = self.volume_rm(&vol); e };
        let (code, out, err) = r.map_err(fail)?;
        if code != Some(0) {
            if code == Some(125) && (err.contains("No such image") || err.contains("Unable to find image")) {
                return Err(fail(format!("{}={image} · the image {image} is not on this machine; {}", crate::images::MARKER, self.pull_hint(image))));
            }
            let detail = tail(&format!("{out}\n{err}"), 6);
            return Err(fail(format!("{}: {} install failed (exit {}):\n{detail}", s.name, inst.word(), code.map_or("?".into(), |c| c.to_string()))));
        }
        let sha = self.tree_hash(image, &vol).map_err(fail)?;
        Ok(LockServer { name: s.name.clone(), installer: inst.word().into(), package: s.package.clone(), version: s.version.clone(), image: image.into(),
            volume: vol, sha256: sha, installed: now_s(), tools: None, tokens: None, error: None })
    }

    /// sha256 of a package volume's tree, computed in a container with no network
    fn tree_hash(&self, image: &str, vol: &str) -> Result<String, String> {
        let script = "cd /opt/plugin && find . \\( -type f -o -type l \\) | LC_ALL=C sort | while IFS= read -r f; do \
            if [ -L \"$f\" ]; then echo \"L $f $(readlink \"$f\")\"; else echo \"F $f $(sha256sum < \"$f\" | cut -d' ' -f1)\"; fi; done | sha256sum | cut -d' ' -f1";
        let mount = format!("{vol}:{PKG}:ro");
        let (code, out, err) = run_cmd(self.cmd(&["run", "--rm", "--pull", "never", "--network", "none", "-v", &mount, image, "sh", "-c", script]), Duration::from_secs(300))?;
        let h = out.trim();
        if code != Some(0) || h.len() != 64 { return Err(format!("could not hash the installed package: {}", tail(&err, 3))); }
        Ok(h.to_string())
    }

    /// Containers of plugin servers whose Elisa session is gone (a killed session leaves its container running):
    /// removed. Their names end in the docker client's pid (`elisa-plugin-P-S-PID`).
    fn sweep(&self) {
        let Ok((Some(0), out, _)) = run_cmd(self.cmd(&["ps", "--filter", "label=elisa.plugin", "--format", "{{.Names}}"]), Duration::from_secs(10)) else { return };
        for n in out.lines().map(str::trim).filter(|n| n.starts_with("elisa-plugin-")) {
            let Some(pid) = n.rsplit('-').next().and_then(|p| p.parse::<i32>().ok()) else { continue };
            // SAFETY: signal 0 only checks whether the process exists
            let gone = unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone { let _ = run_cmd(self.cmd(&["kill", n]), Duration::from_secs(10)); }
        }
    }
}

// ---------- mounts and the run command ----------

/// paths a plugin may never mount (or mount a parent of): secrets, Elisa's own settings, the container engine
fn sensitive(home: &Path) -> Vec<PathBuf> {
    let h = |p: &str| home.join(p);
    let mut v: Vec<PathBuf> = [".ssh", ".aws", ".gnupg", ".config", ".elisa", ".docker", ".colima", ".lima", ".kube", ".azure", "Library",
        ".netrc", ".git-credentials", ".password-store", ".cargo/credentials", ".cargo/credentials.toml", ".npmrc", ".pypirc", ".orbstack"]
        .iter().map(|p| h(p)).collect();
    v.extend(["/var/run", "/private/var/run", "/etc", "/private/etc", "/var/db", "/private/var/db", "/System", "/usr", "/bin", "/sbin"].map(PathBuf::from));
    v
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c { Component::CurDir => {} Component::ParentDir => { out.pop(); } c => out.push(c.as_os_str()) }
    }
    out
}

/// A mount's host path: `~/x` under `home`, `{project}` the session's project. Err: not allowed, and why.
pub fn mount_path(m: &Mount, home: &Path, project: Option<&Path>) -> Result<PathBuf, String> {
    let p = if let Some(r) = m.path.strip_prefix("~/") { home.join(r) }
        else if let Some(r) = m.path.strip_prefix("{project}") {
            let proj = project.ok_or("{project} needs a project folder")?;
            proj.join(r.trim_start_matches('/'))
        } else { PathBuf::from(&m.path) };
    let p = lexical(&p);
    let real = p.canonicalize().unwrap_or_else(|_| p.clone());
    for q in [&p, &real] {
        if q.parent().is_none() || q == home || home.starts_with(q) { return Err(format!("{}: too wide to mount (it holds your home folder)", m.path)); }
        if q.to_string_lossy().contains("docker.sock") { return Err(format!("{}: the container engine's socket is never mounted", m.path)); }
        if let Some(s) = sensitive(home).iter().find(|s| q.starts_with(s) || s.starts_with(q)) {
            return Err(format!("{}: {} holds secrets or system files and is never mounted", m.path, s.display()));
        }
    }
    Ok(p)
}

/// The `docker run` arguments of a container server: locked down, no network, the package volume read-only, only the
/// declared mounts, the plugin's environment by name only (its values come from the docker client's environment).
pub fn run_args(plugin: &str, s: &Server, lock: &LockServer, cname: &str, home: &Path, project: Option<&Path>) -> Result<Vec<String>, String> {
    let inst = s.installer.ok_or("a host server has no container")?;
    let mut a: Vec<String> = ["run", "-i", "--rm", "--pull", "never", "--name", cname, "--label", &format!("elisa.plugin={plugin}"), "--init",
        "--network", "none", "--read-only", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=64m", "--memory", "512m", "--memory-swap", "512m",
        "--cpus", "1", "--pids-limit", "128", "--user", "65534:65534", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--log-driver", "none", "-e", "HOME=/tmp"].iter().map(|x| x.to_string()).collect();
    for e in inst.run_env() { a.extend(["-e".into(), e]); }
    a.extend(["--mount".into(), format!("type=volume,src={},dst={PKG},readonly", lock.volume)]);
    let mut workdir = "/tmp".to_string();
    for m in &s.mounts {
        let p = mount_path(m, home, project)?;
        let ps = p.to_string_lossy().into_owned();
        if m.path.starts_with("{project}") && workdir == "/tmp" { workdir = ps.clone(); }
        a.extend(["--mount".into(), format!("type=bind,src={ps},dst={ps}{}", if m.write { "" } else { ",readonly" })]);
    }
    for (k, _) in &s.env { a.extend(["-e".into(), k.clone()]); }
    a.extend(["--workdir".into(), workdir, lock.image.clone()]);
    a.extend(s.command.iter().cloned());
    Ok(a)
}

// ---------- the setup preview ----------

pub struct Preview {
    pub text: String,
    pub json: Value,
    /// what stops the install (no Colima, a missing image): shown, and nothing can be approved
    pub problems: Vec<String>,
    /// listed images that are not on this machine (a session offers to download them, then previews again)
    pub missing_images: Vec<&'static str>,
}

/// What `add` / `update` will do, before anything happens. `old`: the installed manifest (update).
pub fn preview(m: &Manifest, src: &Path, old: Option<&Manifest>, home: &Path) -> Preview {
    let mut problems = Vec::new();
    let mut missing_images: Vec<&'static str> = Vec::new();
    let containers = m.servers.iter().any(|s| !s.host);
    let docker = if containers { Some(Docker::find(home)) } else { None };
    if let Some(Err(e)) = &docker { problems.push(e.clone()); }
    let mut t = match old {
        Some(o) => format!("Update plugin {} {} → {}", m.name, o.version, m.version),
        None => format!("Plugin {} {}", m.name, m.version),
    };
    if !m.description.is_empty() { t.push_str(&format!(" — {}", m.description)); }
    t.push_str(&format!("\nfrom {}\n", src.display()));
    let mut servers = Vec::new();
    if m.servers.is_empty() { t.push_str("Servers: none\n"); } else { t.push_str("Servers:\n"); }
    for s in &m.servers {
        let filter = if s.filter.is_empty() { "all its tools (known after install)".to_string() } else { s.filter.describe() };
        let env: Vec<&str> = s.env.iter().map(|(k, _)| k.as_str()).collect();
        let mut image_present = Value::Null;
        let mut image_name: Option<&'static str> = None;
        if s.host {
            t.push_str(&format!("  ⚠ {} runs DIRECTLY ON THIS MAC, not in a container: `{}`\n", s.name, s.command.join(" ")));
            t.push_str("    it can read and change anything you can; Elisa installs nothing for it\n");
        } else if let Some(i) = s.installer {
            t.push_str(&format!("  {} · {} {}@{} · runs in a container with no network\n", s.name, i.word(), s.package, s.version));
            let img = match &docker { Some(Ok(d)) => d.installer_image(i), _ => i.image() };
            image_name = Some(img);
            let have = match &docker { Some(Ok(d)) => match d.image_present(img) {
                Ok(true) if crate::images::listed(img).is_some_and(|l| !l.stable) => {
                    image_present = json!(true);
                    format!("on this machine; stable is {}, /sandbox pull to use it", i.image())
                }
                Ok(true) => { image_present = json!(true); "on this machine".to_string() }
                Ok(false) => {
                    image_present = json!(false);
                    if !missing_images.contains(&img) { missing_images.push(img); }
                    problems.push(format!("the image {img} is not on this machine; {}", d.pull_hint(img)));
                    "NOT on this machine".to_string()
                }
                Err(e) => { problems.push(e.clone()); "unknown".to_string() }
            }, _ => "unknown".to_string() };
            t.push_str(&format!("    image {img} ({have}) · install: a throwaway container, network on for the download only\n"));
            t.push_str(&format!("    command: {}\n", s.command.join(" ")));
            let mounts: Vec<String> = s.mounts.iter().map(|mt| {
                let ok = mount_path(mt, home, None).map_or_else(|e| if mt.path.starts_with("{project}") { String::new() } else { format!(" — refused: {e}") }, |_| String::new());
                if !ok.is_empty() { problems.push(format!("{}: mount {}{ok}", s.name, mt.path)); }
                format!("{} ({}){ok}", mt.path, if mt.write { "read + write" } else { "read-only" })
            }).collect();
            t.push_str(&format!("    mounts: {}\n", if mounts.is_empty() { "none".to_string() } else { mounts.join(", ") }));
        }
        if !env.is_empty() { t.push_str(&format!("    environment: {}\n", env.join(", "))); }
        t.push_str(&format!("    tools offered: {filter}\n"));
        servers.push(json!({"name": s.name, "runs": s.runs(), "installer": s.installer.map(Installer::word), "package": s.package, "version": s.version,
            "image": image_name, "image_present": image_present, "command": s.command,
            "mounts": s.mounts.iter().map(|mt| json!({"path": mt.path, "write": mt.write})).collect::<Vec<_>>(), "env": env, "tools": filter}));
    }
    let mut rules = Vec::new();
    if !m.rules.is_empty() {
        t.push_str("Permission rules it adds:\n");
        let mut rs = crate::perms::Rules::default();
        for (l, p) in &m.rules {
            let _ = rs.add(*l, p, "preview");
            let broad = rs.rules.last().is_some_and(crate::perms::Rule::broad);
            t.push_str(&format!("  {} {p}{}\n", l.word(), if broad { "   ⚠ broad: covers every call of this kind" } else { "" }));
            rules.push(json!({"level": l.word(), "pattern": p, "broad": broad}));
        }
    } else { t.push_str("Permission rules: none\n"); }
    let mut notes = Vec::new();
    let cmds = commands::load_dir(&src.join(&m.commands_dir), &Source::Plugin(m.name.clone()), &mut notes);
    if !cmds.is_empty() {
        // each command's full text: it becomes a prompt to the model, so the user sees what it says before saying yes
        t.push_str("Commands (the full text each one sends to the model):\n");
        for c in &cmds {
            t.push_str(&format!("  {} — {}\n", c.name, c.description));
            for l in c.body.lines() { t.push_str(&format!("    │ {l}\n")); }
        }
    }
    for n in m.notes.iter().chain(&notes) { t.push_str(&format!("note: {n}\n")); }
    if problems.is_empty() { t.push_str("Nothing is installed until you say yes."); }
    else { t.push_str(&format!("Cannot install now:\n{}", problems.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n"))); }
    let json = json!({"name": m.name, "version": m.version, "old_version": old.map(|o| o.version.clone()), "description": m.description,
        "source": src.display().to_string(), "servers": servers, "rules": rules, "host": m.servers.iter().any(|s| s.host),
        "commands": cmds.iter().map(|c| json!({"name": c.name, "description": c.description, "args": c.args, "text": c.body})).collect::<Vec<_>>(),
        "notes": m.notes, "problems": problems, "missing_images": missing_images});
    Preview { text: t, json, problems, missing_images }
}

// ---------- operations ----------

fn read_manifest(dir: &Path, project: bool) -> Result<Manifest, String> {
    let p = dir.join(MANIFEST);
    let t = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    parse(&t, project).map_err(|e| format!("{}: {e}", p.display()))
}

/// `~/x` and relative paths made absolute, a folder with a plugin.toml
fn source_dir(path: &str, cwd: &Path, home: &Path) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() { return Err("give the plugin's folder: /plugin add PATH".into()); }
    if path.contains("://") || path.starts_with("git@") { return Err("plugins come from a local folder only (no git or URLs in this version)".into()); }
    let p = if let Some(r) = path.strip_prefix("~/") { home.join(r) } else if Path::new(path).is_absolute() { PathBuf::from(path) } else { cwd.join(path) };
    let p = p.canonicalize().map_err(|e| format!("{}: {e}", p.display()))?;
    if !p.join(MANIFEST).is_file() { return Err(format!("{}: no {MANIFEST} there", p.display())); }
    Ok(p)
}

/// A plugin to add, read and previewed: (manifest, its folder, preview). Err: it cannot be added.
pub fn prepare_add(home: &Path, cwd: &Path, path: &str) -> Result<(Manifest, PathBuf, Preview), String> {
    let src = source_dir(path, cwd, home)?;
    if src.starts_with(root(home)) { return Err("that is an installed plugin's folder; add it from where it came from".into()); }
    let m = read_manifest(&src, false)?;
    if State::load(home).plugins.contains_key(&m.name) || plugin_dir(home, &m.name).exists() {
        return Err(format!("plugin {} is already installed: /plugin update {0} reads it again from its folder", m.name));
    }
    let p = preview(&m, &src, None, home);
    Ok((m, src, p))
}

/// copies the manifest and the commands folder (only those) into the installed folder
fn copy_plugin(src: &Path, dst: &Path, m: &Manifest) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let _ = std::fs::remove_dir_all(dst);
    std::fs::create_dir_all(dst).map_err(|e| err(dst, e))?;
    std::fs::copy(src.join(MANIFEST), dst.join(MANIFEST)).map_err(|e| err(&src.join(MANIFEST), e))?;
    let files = command_files(src, &m.commands_dir);
    if !files.is_empty() {
        let cd = dst.join(&m.commands_dir);
        std::fs::create_dir_all(&cd).map_err(|e| err(&cd, e))?;
        for f in files {
            if std::fs::metadata(&f).map(|x| x.len() as usize > commands::MAX_BYTES).unwrap_or(true) { continue; }
            let Some(n) = f.file_name() else { continue };
            std::fs::copy(&f, cd.join(n)).map_err(|e| err(&f, e))?;
        }
    }
    Ok(())
}

/// Installs a previewed plugin (the user said yes): each container server into its volume, the files copied, the
/// lockfile and the plugin's state (enabled, its content hash pinned) written. On a failure nothing stays behind.
pub fn install(home: &Path, src: &Path, m: &Manifest, say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let mut locked = Vec::new();
    let docker = if m.servers.iter().any(|s| !s.host) { Some(Docker::find(home)?) } else { None };
    for s in m.servers.iter().filter(|s| !s.host) {
        let Some(d) = &docker else { break };
        match d.install(&m.name, s, say) {
            Ok(l) => { say(&format!("{}: installed · sha256 {}", s.name, &l.sha256[..16])); locked.push(l) }
            Err(e) => {
                for l in &locked { let _ = d.volume_rm(&l.volume); }
                return Err(e);
            }
        }
    }
    let dir = plugin_dir(home, &m.name);
    let done = (|| {
        copy_plugin(src, &dir, m)?;
        Lock { plugin: m.name.clone(), version: m.version.clone(), servers: locked.clone() }.save(&dir)?;
        let mut st = State::load(home);
        st.plugins.insert(m.name.clone(), Entry { enabled: true, source: src.display().to_string(), hash: content_hash(&dir)?, version: m.version.clone(), added: now_s() });
        st.save(home)
    })();
    if let Err(e) = done {
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(d) = &docker { for l in &locked { let _ = d.volume_rm(&l.volume); } }
        return Err(e);
    }
    Ok(())
}

/// one server's probe: its name, and (tools, tokens) or why it did not start
pub type Probed = (String, Result<(usize, usize), String>);

/// Starts the plugin's servers once (as a session would) to learn their tools and token cost; written to the lock.
/// (server, Ok((tools, tokens)) or why it did not start).
pub fn probe(home: &Path, cwd: &Path, name: &str) -> Vec<Probed> {
    let l = load_user(home, cwd, &|_| false);
    let cfgs: Vec<ServerCfg> = l.servers.into_iter().filter(|(p, _)| p == name).map(|(_, c)| c).collect();
    if cfgs.is_empty() { return Vec::new(); }
    let own: Vec<String> = crate::tools::defs_for(true).into_iter().map(|d| d.name).collect();
    let (host, _) = crate::mcp_host::Host::start_with_timeout(&cfgs, &|n| own.iter().any(|o| o == n), Duration::from_secs(60));
    // their tool lists, so a session offers them without starting the servers (`mcp_host::Host::open`)
    host.cache_lists(&crate::pins::lists_in(home));
    let mut out = Vec::new();
    let dir = plugin_dir(home, name);
    let mut lock = Lock::load(&dir);
    for c in &cfgs {
        let r = host.server_tools(&c.name).ok_or_else(|| host.down_reason(&c.name).unwrap_or_else(|| "did not start".into()));
        if let Some(s) = lock.as_mut().and_then(|lk| lk.servers.iter_mut().find(|s| s.name == c.name)) {
            match &r {
                Ok((n, tok)) => { s.tools = Some(*n); s.tokens = Some(*tok); s.error = None; }
                Err(e) => s.error = Some(e.clone()),
            }
        }
        out.push((c.name.clone(), r));
    }
    if let Some(lk) = lock { let _ = lk.save(&dir); }
    out
}

/// Removes an installed plugin: its volumes, its folder, its state. Ok: notes (a volume left behind, and how to remove it).
pub fn remove(home: &Path, name: &str) -> Result<Vec<String>, String> {
    let mut st = State::load(home);
    let dir = plugin_dir(home, name);
    if !st.plugins.contains_key(name) && !dir.exists() { return Err(format!("no plugin {name:?} is installed (/plugin list)")); }
    let mut notes = Vec::new();
    let vols: Vec<String> = Lock::load(&dir).map(|l| l.servers.into_iter().map(|s| s.volume).collect()).unwrap_or_default();
    if !vols.is_empty() {
        match Docker::find(home) {
            Ok(d) => for v in &vols { if let Err(e) = d.volume_rm(v) { notes.push(format!("volume {v} not removed ({e}); later: docker volume rm {v}")); } },
            Err(e) => notes.push(format!("{e}; the volumes stay until you run: docker volume rm {}", vols.join(" "))),
        }
    }
    std::fs::remove_dir_all(&dir).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| format!("{}: {e}", dir.display()))?;
    st.plugins.remove(name);
    st.save(home)?;
    Ok(notes)
}

/// Switches an installed plugin on or off. On needs its content to match its pin (else: trust it first).
pub fn set_enabled(home: &Path, name: &str, on: bool) -> Result<String, String> {
    let mut st = State::load(home);
    let e = st.plugins.get_mut(name).ok_or_else(|| format!("no plugin {name:?} is installed (/plugin list)"))?;
    if on && content_hash(&plugin_dir(home, name)).ok().as_deref() != Some(e.hash.as_str()) {
        return Err(format!("plugin {name}: its files changed since you approved them; review with /plugin list, then /trust {name}"));
    }
    if e.enabled == on { return Ok(format!("plugin {name} is already {}", if on { "on" } else { "off" })); }
    e.enabled = on;
    st.save(home)?;
    Ok(format!("plugin {name} {}", if on { "enabled" } else { "disabled" }))
}

/// the installed plugin's files differ from what was approved
pub fn changed(home: &Path, name: &str) -> bool {
    State::load(home).plugins.get(name).is_some_and(|e| content_hash(&plugin_dir(home, name)).ok().as_deref() != Some(e.hash.as_str()))
}

/// The plugin state file's (modified time, size): a session compares it before each turn and before a plugin tool
/// call, so a plugin removed, switched off or on by another Elisa (a window, `elisa plugin`) is noticed cheaply.
pub fn state_stamp(home: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(root(home).join(STATE)).ok()?;
    Some((m.modified().ok()?, m.len()))
}

pub fn installed(home: &Path, name: &str) -> bool { State::load(home).plugins.contains_key(name) }

/// What `trust` would accept: the installed manifest's preview. Err: nothing to trust.
pub fn prepare_trust(home: &Path, name: &str) -> Result<(Manifest, Preview), String> {
    let st = State::load(home);
    st.plugins.get(name).ok_or_else(|| format!("no plugin {name:?} is installed"))?;
    if !changed(home, name) { return Err(format!("plugin {name}: its files match what you approved; nothing to trust")); }
    let dir = plugin_dir(home, name);
    let m = read_manifest(&dir, false).map_err(|e| unparsable(name, &e))?;
    let p = preview(&m, &dir, None, home);
    Ok((m, p))
}

/// the words of `unparsable`'s message a session and the CLI look for (to offer the removal)
pub const UNPARSABLE: &str = "no longer parses";

/// An installed plugin whose changed plugin.toml does not parse cannot be trusted (there is nothing to review or run):
/// it stays off, and the message says the ways out (longrun2: only removal worked, and nothing said so).
fn unparsable(name: &str, e: &str) -> String {
    format!("plugin {name}: its plugin.toml {UNPARSABLE}, so it cannot be trusted and stays off — {e}
        Ways out: /plugin update {name} installs it again from its source folder; /plugin remove {name} removes it; \
        or leave it as it is (it stays off)")
}

/// the installed plugin changed and its manifest does not parse: the parse error
pub fn unparsable_now(home: &Path, name: &str) -> Option<String> {
    if !installed(home, name) || !changed(home, name) { return None; }
    read_manifest(&plugin_dir(home, name), false).err()
}

/// Accepts the installed plugin's current files (pins their hash). Servers whose package or version no longer match
/// the lock stay off until `/plugin update`.
pub fn trust(home: &Path, name: &str) -> Result<String, String> {
    let mut st = State::load(home);
    let dir = plugin_dir(home, name);
    let e = st.plugins.get_mut(name).ok_or_else(|| format!("no plugin {name:?} is installed"))?;
    // only files that parse can be trusted
    let m = read_manifest(&dir, false).map_err(|err| unparsable(name, &err))?;
    e.hash = content_hash(&dir)?;
    st.save(home)?;
    let lock = Lock::load(&dir).unwrap_or_default();
    let stale: Vec<&str> = m.servers.iter().filter(|s| !s.host && lock.of(s).is_none()).map(|s| s.name.as_str()).collect();
    Ok(if stale.is_empty() { format!("plugin {name}: its current files are trusted") }
        else { format!("plugin {name}: trusted; {} not installed as declared — /plugin update {name}", stale.join(", ")) })
}

/// An update read from the plugin's recorded folder: (manifest, folder, preview).
pub fn prepare_update(home: &Path, name: &str) -> Result<(Manifest, PathBuf, Preview), String> {
    let st = State::load(home);
    let e = st.plugins.get(name).ok_or_else(|| format!("no plugin {name:?} is installed (/plugin list)"))?;
    let src = PathBuf::from(&e.source);
    if !src.join(MANIFEST).is_file() { return Err(format!("plugin {name}: its folder {} has no {MANIFEST} any more", src.display())); }
    let m = read_manifest(&src, false)?;
    if m.name != name { return Err(format!("{}: names plugin {:?}, not {name:?}", src.display(), m.name)); }
    let old = read_manifest(&plugin_dir(home, name), false).ok();
    let p = preview(&m, &src, old.as_ref(), home);
    Ok((m, src, p))
}

/// Applies an update (the user said yes): servers whose package, version or installer changed (or whose volume is
/// missing from the lock) are installed again; the files, lock and pin are replaced. The enabled state is kept.
pub fn update(home: &Path, src: &Path, m: &Manifest, say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let dir = plugin_dir(home, &m.name);
    let old = Lock::load(&dir).unwrap_or_default();
    let mut locked = Vec::new();
    let docker = if m.servers.iter().any(|s| !s.host) { Some(Docker::find(home)?) } else { None };
    for s in m.servers.iter().filter(|s| !s.host) {
        let Some(d) = &docker else { break };
        // kept when installed as declared and its volume is still there; installed again otherwise
        match old.of(s) {
            Some(l) if d.volume_exists(&l.volume)? => locked.push(l.clone()),
            _ => locked.push(d.install(&m.name, s, say)?),
        }
    }
    // volumes of servers that are gone
    let gone: Vec<&LockServer> = old.servers.iter().filter(|l| !locked.iter().any(|n| n.volume == l.volume)).collect();
    if !gone.is_empty() { if let Ok(d) = Docker::find(home) { for l in gone { let _ = d.volume_rm(&l.volume); } } }
    copy_plugin(src, &dir, m)?;
    Lock { plugin: m.name.clone(), version: m.version.clone(), servers: locked }.save(&dir)?;
    let mut st = State::load(home);
    let enabled = st.plugins.get(&m.name).is_none_or(|e| e.enabled);
    st.plugins.insert(m.name.clone(), Entry { enabled, source: src.display().to_string(), hash: content_hash(&dir)?, version: m.version.clone(), added: now_s() });
    st.save(home)
}

// ---------- loading for a session ----------

#[derive(Clone, Debug, Default)]
pub struct SrvInfo {
    pub name: String,
    pub runs: &'static str,
    pub installed: bool,
    pub tools: Option<usize>,
    pub tokens: Option<usize>,
    /// it did not start: why (this session's start, else the last probe after install / update)
    pub error: Option<String>,
}

/// One plugin as `/plugin list` and the app show it.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub name: String,
    pub version: String,
    pub description: String,
    /// "user" | "project"
    pub source: &'static str,
    pub enabled: bool,
    pub trusted: bool,
    pub servers: Vec<SrvInfo>,
    pub rules: usize,
    pub commands: Vec<String>,
    pub notes: Vec<String>,
}

/// What enabled user plugins bring to a session.
#[derive(Default)]
pub struct Loaded {
    /// (plugin, server)
    pub servers: Vec<(String, ServerCfg)>,
    /// (level, pattern, source label)
    pub rules: Vec<(Level, String, String)>,
    pub commands: Vec<FileCommand>,
    pub infos: Vec<Info>,
    pub notes: Vec<String>,
}

/// The installed plugins (every one in `infos`; the enabled and trusted ones' servers, rules and commands).
/// `taken`: server names already in use (the user's `[mcp.NAME]`, Elisa's own); a plugin server with one is skipped.
pub fn load_user(home: &Path, cwd: &Path, taken: &dyn Fn(&str) -> bool) -> Loaded {
    let mut l = Loaded::default();
    let st = State::load(home);
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "elisa".into());
    for (name, e) in &st.plugins {
        let dir = plugin_dir(home, name);
        let trusted = content_hash(&dir).ok().as_deref() == Some(e.hash.as_str());
        let mut info = Info { name: name.clone(), version: e.version.clone(), source: "user", enabled: e.enabled, trusted, ..Info::default() };
        let m = match read_manifest(&dir, false) {
            Ok(m) => m,
            Err(err) => { info.notes.push(err.clone()); if e.enabled { l.notes.push(format!("plugin {name}: not loaded — {err}")); } l.infos.push(info); continue; }
        };
        info.description = m.description.clone();
        info.rules = m.rules.len();
        let lock = Lock::load(&dir).unwrap_or_default();
        let mut notes = Vec::new();
        let cmds = commands::load_dir(&dir.join(&m.commands_dir), &Source::Plugin(name.clone()), &mut notes);
        info.commands = cmds.iter().map(|c| c.name.clone()).collect();
        for s in &m.servers {
            let lk = lock.of(s);
            info.servers.push(SrvInfo { name: s.name.clone(), runs: s.runs(), installed: s.host || lk.is_some(),
                tools: lk.and_then(|x| x.tools), tokens: lk.and_then(|x| x.tokens), error: lk.and_then(|x| x.error.clone()) });
        }
        let on = e.enabled && trusted;
        if e.enabled && !trusted {
            l.notes.push(format!("plugin {name}: its files changed since you approved them, so it is off — review with /plugin list, then /trust {name}"));
        }
        if !on { l.infos.push(info); continue; }
        l.notes.extend(notes);
        for s in &m.servers {
            if taken(&s.name) || l.servers.iter().any(|(_, c)| c.name == s.name) {
                l.notes.push(format!("plugin {name}: server {} not started — another server has that name", s.name));
                continue;
            }
            if !s.host && lock.of(s).is_none() {
                l.notes.push(format!("plugin {name}: server {} is not installed as declared — /plugin update {name}", s.name));
                continue;
            }
            let cfg = if s.host {
                ServerCfg { name: s.name.clone(), command: s.command.clone(), env: s.env.clone(), filter: s.filter.clone(), ..ServerCfg::default() }
            } else {
                let mut env = s.env.clone();
                env.push(("ELISA_PROJECT".into(), cwd.display().to_string()));
                ServerCfg { name: s.name.clone(), command: vec![exe.clone(), RUN_ARG.into(), name.clone(), s.name.clone()], env,
                    env_pass: vec!["DOCKER_HOST".into()], filter: s.filter.clone(), ..ServerCfg::default() }
            };
            l.servers.push((name.clone(), cfg));
        }
        for (lv, p) in &m.rules { l.rules.push((*lv, p.clone(), format!("plugin {name}"))); }
        l.commands.extend(cmds);
        l.infos.push(info);
    }
    l
}

/// A project's own commands and plugins (`.elisa/commands`, `.elisa/plugins/*`), read but not yet trusted.
pub struct Project {
    pub hash: String,
    /// every file in full, for the trust prompt
    pub text: String,
    pub commands: Vec<FileCommand>,
    pub rules: Vec<(Level, String, String)>,
    pub infos: Vec<Info>,
    pub notes: Vec<String>,
}

/// The project's commands and plugins, None when it has none.
pub fn load_project(cwd: &Path) -> Option<Project> {
    let base = cwd.join(".elisa");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut notes = Vec::new();
    let mut cmd_lists = Vec::new();
    let rel = |p: &Path| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let push = |files: &mut Vec<(String, Vec<u8>)>, p: &Path| if let Ok(b) = std::fs::read(p) { files.push((rel(p), b)); };
    let own = base.join("commands");
    for p in command_files(&base, "commands") { push(&mut files, &p); }
    cmd_lists.push(commands::load_dir(&own, &Source::Project, &mut notes));
    let mut rules = Vec::new();
    let mut infos = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base.join("plugins")).map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join(MANIFEST).is_file()).collect()).unwrap_or_default();
    dirs.sort();
    for d in dirs {
        push(&mut files, &d.join(MANIFEST));
        match read_manifest(&d, true) {
            Ok(m) => {
                for p in command_files(&d, &m.commands_dir) { push(&mut files, &p); }
                let cmds = commands::load_dir(&d.join(&m.commands_dir), &Source::ProjectPlugin(m.name.clone()), &mut notes);
                infos.push(Info { name: m.name.clone(), version: m.version.clone(), description: m.description.clone(), source: "project", enabled: true,
                    rules: m.rules.len(), commands: cmds.iter().map(|c| c.name.clone()).collect(), notes: m.notes.clone(), ..Info::default() });
                for (l, p) in &m.rules { rules.push((*l, p.clone(), format!("project plugin {}", m.name))); }
                notes.extend(m.notes);
                cmd_lists.push(cmds);
            }
            Err(e) => notes.push(format!("project plugin not loaded: {e}")),
        }
    }
    if files.is_empty() { return None; }
    let mut buf = Vec::new();
    let mut text = format!("This project ({}) brings its own commands and permission rules. They load only if you trust this content; \
        the trust is kept in ~/.elisa (never in the repository) and any change asks again.\n", cwd.display());
    for (n, b) in &files {
        buf.extend_from_slice(n.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&(b.len() as u64).to_le_bytes());
        buf.extend_from_slice(b);
        text.push_str(&format!("── {n} ──\n{}\n", String::from_utf8_lossy(b).trim_end()));
    }
    for n in &notes { text.push_str(&format!("note: {n}\n")); }
    let commands = cmd_lists.into_iter().flatten().collect();
    Some(Project { hash: crate::pins::sha256_hex(&buf), text: text.trim_end().to_string(), commands, rules, infos, notes })
}

fn project_key(cwd: &Path) -> String { cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()).display().to_string() }

/// the user trusted exactly this content of this project
pub fn project_trusted(home: &Path, cwd: &Path, hash: &str) -> bool {
    let v: Value = std::fs::read_to_string(home.join(PROJECT_TRUST)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v[project_key(cwd)]["hash"].as_str() == Some(hash)
}

pub fn trust_project(home: &Path, cwd: &Path, hash: &str) -> Result<(), String> {
    let p = home.join(PROJECT_TRUST);
    let mut v: Value = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    v[project_key(cwd)] = json!({"hash": hash, "ts": now_s()});
    write_json(&p, &v)
}

/// A session's own view of its plugin servers: `ours` (plugin, server) are the ones it started; `down` gives why one of
/// them did not start (None: it is running, so an older probe's error no longer applies).
pub fn set_live(infos: &mut [Info], ours: &[(String, String)], down: &dyn Fn(&str) -> Option<String>) {
    for i in infos.iter_mut() {
        for s in i.servers.iter_mut() {
            if ours.iter().any(|(p, n)| p == &i.name && n == &s.name) { s.error = down(&s.name); }
        }
    }
}

/// the servers of a plugin that did not start, as "name: why" lines
pub fn failed_servers(i: &Info) -> Vec<String> {
    i.servers.iter().filter_map(|s| s.error.as_ref().map(|e| format!("{}: {e}", s.name))).collect()
}

/// `/plugin list` text for these plugins
pub fn list_text(infos: &[Info]) -> String {
    if infos.is_empty() { return "no plugins · /plugin add PATH installs one from a local folder".into(); }
    let on = infos.iter().filter(|i| i.enabled && i.trusted).count();
    let mut out = format!("Plugins: {} ({on} on)\n", infos.len());
    for i in infos {
        let state = match (i.source, i.enabled, i.trusted) {
            ("project", _, false) => "not trusted yet (run one of its commands, or /plugin trust project)".to_string(),
            ("project", _, true) => "trusted".to_string(),
            (_, _, false) => format!("CHANGED since you approved it — off until /trust {}", i.name),
            (_, true, true) if i.servers.iter().any(|s| s.error.is_some()) => "on, but NOT WORKING: a server failed to start (its tools are missing)".to_string(),
            (_, true, true) => "on".to_string(),
            (_, false, true) => "off".to_string(),
        };
        let mut parts = vec![format!("{} {}", i.name, i.version), i.source.to_string(), state];
        let srv: Vec<String> = i.servers.iter().map(|s| {
            let mut d = vec![s.runs.to_string()];
            if !s.installed { d.push("not installed".into()); }
            if s.error.is_some() { d.push("FAILED to start".into()); }
            if let Some(n) = s.tools { d.push(format!("{n} tool{}", if n == 1 { "" } else { "s" })); }
            if let Some(t) = s.tokens { d.push(format!("~{} tokens", crate::ui::k(t))); }
            format!("{} ({})", s.name, d.join(", "))
        }).collect();
        if !srv.is_empty() { parts.push(format!("servers: {}", srv.join(", "))); }
        if i.rules > 0 { parts.push(format!("{} rule{}", i.rules, if i.rules == 1 { "" } else { "s" })); }
        if !i.commands.is_empty() { parts.push(format!("commands: {}", i.commands.join(" "))); }
        out.push_str(&parts.join(" · "));
        out.push('\n');
        if !i.description.is_empty() { out.push_str(&format!("  {}\n", i.description)); }
        for n in &i.notes { out.push_str(&format!("  note: {n}\n")); }
        if i.enabled && i.trusted { for f in failed_servers(i) { out.push_str(&format!("  failed: {f}\n")); } }
    }
    out.push_str("/plugin add PATH · remove · enable · disable · update NAME · trust NAME");
    out
}

/// one plugin as the app's `plugins` event carries it
pub fn info_json(i: &Info) -> Value {
    let tools: usize = i.servers.iter().filter_map(|s| s.tools).sum();
    let tokens: usize = i.servers.iter().filter_map(|s| s.tokens).sum();
    json!({"name": i.name, "version": i.version, "description": i.description, "source": i.source, "enabled": i.enabled, "trusted": i.trusted,
        "servers": i.servers.iter().map(|s| json!({"name": s.name, "runs": s.runs, "installed": s.installed, "tools": s.tools, "tokens": s.tokens,
            "state": if s.error.is_some() { "failed" } else if s.installed { "ok" } else { "not installed" }, "error": s.error})).collect::<Vec<_>>(),
        "tools": tools, "tokens": tokens, "rules": i.rules, "commands": i.commands, "notes": i.notes})
}

// ---------- `elisa --plugin-run PLUGIN SERVER` ----------

/// What `--plugin-run` runs: the docker client and its arguments, after checking the plugin is enabled, trusted and
/// installed as declared.
fn prepare_run(home: &Path, plugin: &str, server: &str, project: Option<&Path>) -> Result<(Docker, Vec<String>), String> {
    let st = State::load(home);
    let e = st.plugins.get(plugin).ok_or("the plugin is not installed")?;
    if !e.enabled { return Err("the plugin is disabled".into()); }
    let dir = plugin_dir(home, plugin);
    if content_hash(&dir)? != e.hash { return Err(format!("its files changed since you approved them: /trust {plugin}")); }
    let m = read_manifest(&dir, false)?;
    let s = m.servers.iter().find(|s| s.name == server).ok_or("no such server in the plugin")?;
    if s.host { return Err("a host server is started by the session itself".into()); }
    let lock = Lock::load(&dir).unwrap_or_default();
    let lk = lock.of(s).ok_or_else(|| format!("not installed as declared: /plugin update {plugin}"))?;
    let d = Docker::find(home)?;
    // a missing image: said first on the line, so the session can offer to download it (`images::marked`)
    if !d.image_present(&lk.image)? {
        return Err(format!("{}={} · the image {} is not on this machine; {}", crate::images::MARKER, lk.image, lk.image, d.pull_hint(&lk.image)));
    }
    if !d.volume_exists(&lk.volume)? { return Err(format!("its installed package (volume {}) is gone: /plugin update {plugin}", lk.volume)); }
    let cname = format!("elisa-plugin-{plugin}-{server}-{}", std::process::id());
    let args = run_args(plugin, s, lk, &cname, home, project)?;
    Ok((d, args))
}

/// what a start failure's message says when the container engine cannot see a mounted folder
pub const SHARE_HINT: &str = "Colima shares only your home folder by default: move the project under ~ or add its path \
    to Colima's mounts (colima stop; colima start --edit)";

/// The words for a container that docker could not start (exit 125: the daemon refused; 126 / 127: the command
/// cannot run or is not in the image): the meaningful lines of its stderr (not "Run 'docker run --help'"), and a
/// hint for a project folder the VM does not share.
pub fn start_failure(code: i32, stderr: &[String]) -> String {
    let lines: Vec<&str> = stderr.iter().map(|l| l.trim()).filter(|l| !l.is_empty())
        .filter(|l| !(l.starts_with("Run 'docker") || l.starts_with("See 'docker")))
        .map(|l| l.strip_prefix("docker: ").unwrap_or(l)).collect();
    let msg = lines[lines.len().saturating_sub(3)..].join(" · ");
    let msg = if msg.is_empty() { "no message from docker".to_string() } else { crate::context::one_line(&msg, 260) };
    let low = msg.to_ascii_lowercase();
    let what = match code { 125 => "docker could not start the container", 126 => "the server's command cannot run in the container", _ => "the server's command is not in the container" };
    let hint = if low.contains("bind source path does not exist") || low.contains("mounts denied") || low.contains("is not shared from the host") {
        format!(" — {SHARE_HINT}")
    } else { String::new() };
    format!("{what} (exit {code}): {msg}{hint}")
}

/// `elisa --plugin-run PLUGIN SERVER`: runs the server's `docker run` as its child (same process group, so the
/// session's MCP host stops both like any server). Errors go to stderr (the host shows the last line).
pub fn run_server(args: &[String]) -> i32 {
    let (Some(plugin), Some(server)) = (args.first(), args.get(1)) else { eprintln!("usage: elisa {RUN_ARG} PLUGIN SERVER"); return 2 };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { eprintln!("no HOME"); return 2 };
    let project = std::env::var_os("ELISA_PROJECT").map(PathBuf::from);
    match prepare_run(&home, plugin, server, project.as_deref()) {
        Ok((d, a)) => {
            d.sweep();
            // a child, not exec: its stderr passes through, and when docker could not start the container (125-127)
            // the last line says why in words (the session keeps only the last stderr line; longrun2 F5: that was
            // "Run 'docker run --help' for more information")
            let mut c = Command::new(&d.bin);
            c.args(&a).env("DOCKER_HOST", &d.host).env_remove("DOCKER_CONTEXT").stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::piped());
            let mut child = match c.spawn() { Ok(c) => c, Err(e) => { eprintln!("plugin {plugin}/{server}: cannot run docker: {e}"); return 1 } };
            let mut kept: std::collections::VecDeque<String> = std::collections::VecDeque::new();
            if let Some(se) = child.stderr.take() {
                use std::io::BufRead;
                for l in std::io::BufReader::new(se).lines().map_while(Result::ok) {
                    eprintln!("{l}");
                    kept.push_back(l);
                    if kept.len() > 40 { kept.pop_front(); }
                }
            }
            let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
            if matches!(code, 125..=127) {
                let lines: Vec<String> = kept.into_iter().collect();
                eprintln!("plugin {plugin}/{server}: {}", start_failure(code, &lines));
            }
            code
        }
        // the marker stays at the start of the line (the session keeps only the first part of the last stderr line)
        Err(e) if e.starts_with(crate::images::MARKER) => { eprintln!("{e} · plugin {plugin}/{server}"); 2 }
        Err(e) => { eprintln!("plugin {plugin}/{server}: {e}"); 2 }
    }
}

// ---------- `elisa plugin …` ----------

const CLI_USAGE: &str = "usage: elisa plugin list [--json] | add PATH [--yes] | remove NAME | enable NAME | disable NAME | update NAME [--yes] | trust NAME [--yes]";

pub(crate) fn confirm(q: &str, yes: bool) -> bool {
    if yes { return true; }
    // SAFETY: isatty only inspects the descriptor
    if unsafe { libc::isatty(0) } != 1 { eprintln!("{q} — no terminal to ask: add --yes to approve"); return false; }
    eprint!("{q} [y/N] ");
    let mut a = String::new();
    let _ = std::io::stdin().read_line(&mut a);
    matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn print_probe(p: &[Probed]) {
    for (s, r) in p {
        match r { Ok((n, t)) => println!("{s}: {n} tool{} · ~{} tokens", if *n == 1 { "" } else { "s" }, crate::ui::k(*t)), Err(e) => println!("{s}: did not start: {e}") }
    }
}

/// The last line after an install, update or trust: "installed and on" only when every server started in the probe;
/// else which ones did not (their tools are missing) and that the reason is above. (all started, the line)
pub fn outcome_line(done: &str, probed: &[Probed]) -> (bool, String) {
    let bad: Vec<&str> = probed.iter().filter(|(_, r)| r.is_err()).map(|(s, _)| s.as_str()).collect();
    if bad.is_empty() { return (true, done.to_string()); }
    let done = done.replace(" and on", "");
    (false, format!("{done}, but it is NOT WORKING: server {} did not start, so its tools are missing (the reason is above; \
        /plugin list shows it). Fix that, then start a new session or /plugin disable and enable it", bad.join(", ")))
}

/// one audit line for a plugin operation
pub fn audit(log: &crate::audit::Log, op: &str, name: &str, outcome: &str, detail: &str) {
    log.note("plugin", &[("summary", &format!("{op} {name}")), ("outcome", outcome), ("detail", detail)]);
}

/// `elisa plugin …` (the same operations as `/plugin` in a session). Exit 0 = done, 1 = failed or declined, 2 = usage.
pub fn cli(args: &[String]) -> i32 {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { eprintln!("no HOME"); return 2 };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let log = crate::audit::Log::open(&home, &cwd);
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let as_json = args.iter().any(|a| a == "--json");
    let rest: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--yes" && *a != "-y" && *a != "--json").collect();
    let fail = |e: &str| { eprintln!("{e}"); 1 };
    let arg = rest.get(1).copied().unwrap_or("");
    match rest.first().copied() {
        Some("list") | None => {
            let mut infos = load_user(&home, &cwd, &|_| false).infos;
            if let Some(p) = load_project(&cwd) {
                let t = project_trusted(&home, &cwd, &p.hash);
                infos.extend(p.infos.into_iter().map(|mut i| { i.trusted = t; i }));
            }
            // --json: the app's `plugins` event payload, for the app's plugin window when no session is open
            if as_json { println!("{}", json!({"plugins": infos.iter().map(info_json).collect::<Vec<_>>()})); } else { println!("{}", list_text(&infos)); }
            0
        }
        Some("add") => {
            let (m, src, p) = match prepare_add(&home, &cwd, &rest[1..].join(" ")) { Ok(x) => x, Err(e) => return fail(&e) };
            println!("{}", p.text);
            if !p.problems.is_empty() { audit(&log, "add", &m.name, "error", &p.problems.join("; ")); return 1; }
            if !confirm(&format!("Install plugin {} {}?", m.name, m.version), yes) {
                audit(&log, "add", &m.name, "declined", "");
                println!("nothing was installed");
                return 1;
            }
            if let Err(e) = install(&home, &src, &m, &mut |l| println!("{l}")) { audit(&log, "add", &m.name, "error", &e); return fail(&e); }
            let pr = probe(&home, &cwd, &m.name);
            print_probe(&pr);
            let (ok, line) = outcome_line(&format!("plugin {} {} installed and on (new sessions use it)", m.name, m.version), &pr);
            audit(&log, "add", &m.name, if ok { "ok" } else { "error" }, if ok { &m.version } else { &line });
            println!("{line}");
            // installed, but a server did not start: not a success for a script either
            if ok { 0 } else { 1 }
        }
        Some("remove") if !arg.is_empty() => match remove(&home, arg) {
            Ok(notes) => { for n in notes { println!("{n}"); } audit(&log, "remove", arg, "ok", ""); println!("plugin {arg} removed"); 0 }
            Err(e) => { audit(&log, "remove", arg, "error", &e); fail(&e) }
        },
        Some(op @ ("enable" | "disable")) if !arg.is_empty() => match set_enabled(&home, arg, op == "enable") {
            Ok(m) => { audit(&log, op, arg, "ok", ""); println!("{m}"); 0 }
            Err(e) => { audit(&log, op, arg, "error", &e); fail(&e) }
        },
        Some("update") if !arg.is_empty() => {
            let (m, src, p) = match prepare_update(&home, arg) { Ok(x) => x, Err(e) => return fail(&e) };
            println!("{}", p.text);
            if !p.problems.is_empty() { audit(&log, "update", arg, "error", &p.problems.join("; ")); return 1; }
            if !confirm(&format!("Update plugin {arg} to {}?", m.version), yes) { audit(&log, "update", arg, "declined", ""); return 1; }
            match update(&home, &src, &m, &mut |l| println!("{l}")) {
                Ok(()) => {
                    let pr = probe(&home, &cwd, arg);
                    print_probe(&pr);
                    let (ok, line) = outcome_line(&format!("plugin {arg} updated to {}", m.version), &pr);
                    audit(&log, "update", arg, if ok { "ok" } else { "error" }, if ok { &m.version } else { &line });
                    println!("{line}");
                    if ok { 0 } else { 1 }
                }
                Err(e) => { audit(&log, "update", arg, "error", &e); fail(&e) }
            }
        }
        Some("trust") if !arg.is_empty() => {
            let (_, p) = match prepare_trust(&home, arg) {
                Ok(x) => x,
                Err(e) if e.contains(UNPARSABLE) => {
                    eprintln!("{e}");
                    audit(&log, "trust", arg, "error", &e);
                    // the way out the user is most likely to want, asked (never assumed: --yes approves a trust, not a removal)
                    if confirm(&format!("Remove plugin {arg} now? (no keeps it off)"), false) {
                        return match remove(&home, arg) {
                            Ok(notes) => { for n in notes { println!("{n}"); } audit(&log, "remove", arg, "ok", ""); println!("plugin {arg} removed"); 0 }
                            Err(e) => { audit(&log, "remove", arg, "error", &e); fail(&e) }
                        };
                    }
                    println!("plugin {arg} stays off");
                    return 1;
                }
                Err(e) => return fail(&e),
            };
            println!("{}", p.text);
            // same as /plugin trust in a session: a plugin that cannot run as it stands is not trusted
            if !p.problems.is_empty() { audit(&log, "trust", arg, "error", &p.problems.join("; ")); return 1; }
            if !confirm(&format!("Trust plugin {arg}'s current files?"), yes) { audit(&log, "trust", arg, "declined", ""); return 1; }
            match trust(&home, arg) {
                Ok(m) => { audit(&log, "trust", arg, "ok", ""); println!("{m}"); 0 }
                Err(e) => { audit(&log, "trust", arg, "error", &e); fail(&e) }
            }
        }
        _ => { eprintln!("{CLI_USAGE}"); 2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[plugin]
name = "thinking"
version = "1.0.0"
description = "Sequential thinking"

[[server]]
name = "thinking"
installer = "npm"
package = "@modelcontextprotocol/server-sequential-thinking"
version = "2025.7.1"
command = ["mcp-server-sequential-thinking"]
tools = ["sequentialthinking"]
env = { DISABLE_THOUGHT_LOGGING = "true" }
mounts = [{ path = "{project}" }, { path = "/tmp/out", write = true }]

[[server]]
name = "helper"
runs = "host"
command = ["/usr/bin/true"]

[permissions]
allow = ["mcp:thinking"]
deny = ["shell(rm *)"]
"#;

    #[test]
    fn a_good_manifest_parses() {
        let m = parse(GOOD, false).unwrap();
        assert_eq!((m.name.as_str(), m.version.as_str(), m.servers.len(), m.rules.len()), ("thinking", "1.0.0", 2, 2));
        let s = &m.servers[0];
        assert_eq!((s.installer, s.host, s.volume("thinking", "ab12cd34").as_str()), (Some(Installer::Npm), false, "elisa-plugin-thinking-thinking-ab12cd34"));
        assert_ne!(home_tag(Path::new("/Users/a")), home_tag(Path::new("/Users/b")));
        assert_eq!(s.mounts, [Mount { path: "{project}".into(), write: false }, Mount { path: "/tmp/out".into(), write: true }]);
        assert!(m.servers[1].host && m.servers[1].installer.is_none());
        assert_eq!(Installer::Npm.install_args("p", "1.2.3").last().unwrap(), "p@1.2.3");
        assert!(Installer::Npm.install_args("p", "1.2.3").contains(&"--ignore-scripts".to_string()));
        assert_eq!(Installer::Pip.install_args("p", "1.2").last().unwrap(), "p==1.2");
    }

    #[test]
    fn bad_manifests_say_why() {
        let with = |from: &str, to: &str| parse(&GOOD.replace(from, to), false).unwrap_err();
        assert!(with("tools = [\"sequentialthinking\"]", "network = true").contains("no network option"), "{}", with("tools = [\"sequentialthinking\"]", "network = true"));
        assert!(with("version = \"2025.7.1\"", "version = \"^2025.7.1\"").contains("one exact version"));
        assert!(with("version = \"2025.7.1\"", "version = \"latest\"").contains("one exact version"));
        assert!(with("installer = \"npm\"", "installer = \"brew\"").contains("installer"));
        assert!(with("name = \"thinking\"\ninstaller", "name = \"web\"\ninstaller").contains("belongs to Elisa"));
        assert!(with("name = \"thinking\"\nversion = \"1.0.0\"", "name = \"Thinking!\"\nversion = \"1.0.0\"").contains("plugin name"));
        assert!(with("DISABLE_THOUGHT_LOGGING", "DYLD_INSERT_LIBRARIES").contains("may not be set"));
        assert!(with("DISABLE_THOUGHT_LOGGING", "DOCKER_HOST").contains("may not be set"));
        assert!(with("\"{project}\"", "\"relative/dir\"").contains("must be absolute"));
        assert!(with("\"shell(rm *)\"", "\"nope(x)\"").contains("unknown kind"));
        assert!(with("package = \"@model", "package = \"; rm -rf /@model").contains("not a valid npm package"));
        assert!(with("runs = \"host\"", "runs = \"vm\"").contains("runs ="));
    }

    #[test]
    fn a_project_plugin_brings_only_commands_and_deny_ask_rules() {
        let m = parse(GOOD, true).unwrap();
        assert!(m.servers.is_empty());
        assert_eq!(m.rules, [(Level::Deny, "shell(rm *)".to_string())]);
        assert!(m.notes.iter().any(|n| n.contains("2 server(s) ignored")), "{:?}", m.notes);
        assert!(m.notes.iter().any(|n| n.contains("allow = \"mcp:thinking\" ignored")), "{:?}", m.notes);
    }

    #[test]
    fn secrets_and_wide_paths_are_never_mounted() {
        let home = PathBuf::from("/Users/someone");
        let m = |p: &str| Mount { path: p.into(), write: false };
        assert!(mount_path(&m("~/.ssh"), &home, None).unwrap_err().contains("never mounted"));
        assert!(mount_path(&m("~/.config/elisa"), &home, None).is_err());
        assert!(mount_path(&m("~/Library/Keychains"), &home, None).is_err());
        assert!(mount_path(&m("~/"), &home, None).unwrap_err().contains("too wide"));
        assert!(mount_path(&m("/Users"), &home, None).unwrap_err().contains("too wide"));
        assert!(mount_path(&m("/"), &home, None).is_err());
        assert!(mount_path(&m("/var/run/docker.sock"), &home, None).is_err());
        assert!(mount_path(&m("~/work/../.aws"), &home, None).is_err(), "`..` is worked out");
        assert_eq!(mount_path(&m("~/notes"), &home, None).unwrap(), home.join("notes"));
        assert_eq!(mount_path(&m("{project}/docs"), &home, Some(Path::new("/Users/someone/p"))).unwrap(), PathBuf::from("/Users/someone/p/docs"));
        assert!(mount_path(&m("{project}"), &home, None).is_err());
    }

    #[test]
    fn the_run_command_is_locked_down() {
        let m = parse(GOOD, false).unwrap();
        let s = &m.servers[0];
        let lk = LockServer { name: s.name.clone(), installer: "npm".into(), package: s.package.clone(), version: s.version.clone(),
            image: "node:22-alpine".into(), volume: s.volume("thinking", "ab12cd34"), ..LockServer::default() };
        let a = run_args("thinking", s, &lk, "elisa-plugin-thinking-thinking-1", Path::new("/Users/x"), Some(Path::new("/Users/x/proj"))).unwrap().join(" ");
        for f in ["run -i --rm --pull never", "--network none", "--read-only", "--cap-drop ALL", "--security-opt no-new-privileges", "--user 65534:65534",
            "--mount type=volume,src=elisa-plugin-thinking-thinking-ab12cd34,dst=/opt/plugin,readonly", "--mount type=bind,src=/Users/x/proj,dst=/Users/x/proj,readonly",
            "--mount type=bind,src=/tmp/out,dst=/tmp/out ", "-e DISABLE_THOUGHT_LOGGING ", "--workdir /Users/x/proj node:22-alpine mcp-server-sequential-thinking"] {
            assert!(a.contains(f), "missing {f}: {a}");
        }
        assert!(!a.contains("docker.sock") && !a.contains("--privileged") && !a.contains("--network bridge") && !a.contains("true"), "{a}");
        // the env value never reaches the arguments
        assert!(!a.contains("DISABLE_THOUGHT_LOGGING=true"), "{a}");
    }

    #[test]
    fn the_pin_covers_the_manifest_and_the_commands() {
        let d = std::env::temp_dir().join(format!("elisa-plugin-hash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("commands")).unwrap();
        std::fs::write(d.join(MANIFEST), GOOD).unwrap();
        let h0 = content_hash(&d).unwrap();
        std::fs::write(d.join("commands/x.md"), "do x").unwrap();
        let h1 = content_hash(&d).unwrap();
        assert_ne!(h0, h1);
        std::fs::write(d.join("commands/notes.txt"), "not a command").unwrap();
        assert_eq!(content_hash(&d).unwrap(), h1, "only .md command files count");
        std::fs::write(d.join(MANIFEST), GOOD.replace("1.0.0", "1.0.1")).unwrap();
        assert_ne!(content_hash(&d).unwrap(), h1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
