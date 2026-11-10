//! The shell sandbox (src/sandbox.rs): the Seatbelt profile around `bash` and the checks. Everything happens in
//! temp folders: "outside" targets are sibling temp folders that the test policy does not list as writable, and the
//! "secret store" is a temp folder injected into the policy's deny list (the real ~/.ssh is never touched).
use elisa::config::Config;
use elisa::sandbox::{self, Mode, Policy};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("elisa-sbx-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}
impl Drop for Tmp {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn modes_parse_and_only_tighten() {
    assert_eq!(Mode::parse("write"), Some(Mode::Write));
    assert_eq!(Mode::parse(" strict "), Some(Mode::Strict));
    assert_eq!(Mode::parse("off"), Some(Mode::Off));
    assert_eq!(Mode::parse("on"), None);
    assert_eq!(Mode::Write.stricter(Mode::Off), Mode::Write);
    assert_eq!(Mode::Write.stricter(Mode::Strict), Mode::Strict);
    assert_eq!(Mode::Off.stricter(Mode::Write), Mode::Write);
}

#[test]
fn quote_escapes_and_refuses_control_characters() {
    assert_eq!(sandbox::quote(Path::new("/a b/c")).unwrap(), "\"/a b/c\"");
    assert_eq!(sandbox::quote(Path::new("/a\"b\\c")).unwrap(), "\"/a\\\"b\\\\c\"");
    assert!(sandbox::quote(Path::new("/a\nb")).is_err());
}

#[test]
fn hint_is_one_line_and_only_when_on() {
    let h = sandbox::hint(Mode::Write, "zsh:1: operation not permitted: /x").unwrap();
    assert!(!h.contains('\n') && h.contains("shell sandbox"));
    assert!(sandbox::hint(Mode::Off, "Operation not permitted").is_none());
    assert!(sandbox::hint(Mode::Write, "curl: (6) Could not resolve host: example.com").is_none());
    assert!(sandbox::hint(Mode::Strict, "curl: (6) Could not resolve host: example.com").unwrap().contains("strict"));
    assert!(sandbox::hint(Mode::Write, "all fine").is_none());
}

fn write_file(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

#[test]
fn config_project_may_only_tighten_and_bad_values_note() {
    let home = Tmp::new("home");
    let proj = Tmp::new("cfg");
    let none = |_: &str| None;
    let default = Mode::platform_default().word();
    let c = Config::load_from(&proj.0, Some(home.0.clone()), none).unwrap();
    assert_eq!(c.shell_sandbox, default);

    write_file(&proj.0.join(".elisa/config.toml"), "shell_sandbox = \"off\"\n");
    let c = Config::load_from(&proj.0, Some(home.0.clone()), none).unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(c.shell_sandbox, "write");
        assert_eq!(c.notes.iter().filter(|n| n.contains("shell_sandbox")).count(), 1, "{:?}", c.notes);
    }

    write_file(&proj.0.join(".elisa/config.toml"), "shell_sandbox = \"strict\"\n");
    let c = Config::load_from(&proj.0, Some(home.0.clone()), none).unwrap();
    if cfg!(target_os = "macos") { assert_eq!(c.shell_sandbox, "strict"); }

    // the user may switch it off; a project can switch it back on
    write_file(&home.0.join(".config/elisa/config.toml"), "shell_sandbox = \"off\"\n");
    write_file(&proj.0.join(".elisa/config.toml"), "shell_sandbox = \"write\"\n");
    let c = Config::load_from(&proj.0, Some(home.0.clone()), none).unwrap();
    if cfg!(target_os = "macos") { assert_eq!(c.shell_sandbox, "write"); }

    std::fs::remove_file(proj.0.join(".elisa/config.toml")).unwrap();
    write_file(&home.0.join(".config/elisa/config.toml"), "shell_sandbox = \"maybe\"\n");
    let c = Config::load_from(&proj.0, Some(home.0.clone()), none).unwrap();
    assert_eq!(c.shell_sandbox, default);
    assert_eq!(c.notes.iter().filter(|n| n.contains("shell_sandbox")).count(), 1, "{:?}", c.notes);
}

/// The deny list covers the real home folder (password database) even when $HOME points somewhere else, and the
/// injected $HOME as well.
#[test]
fn deny_list_uses_the_real_home_and_home_env() {
    let fake = Tmp::new("fakehome");
    let proj = Tmp::new("proj");
    let real = sandbox::real_home().expect("password database home");
    let real = sandbox::real(&real);
    assert_ne!(real, fake.0);
    assert_eq!(sandbox::homes(Some(&fake.0)), vec![real.clone(), fake.0.clone()]);
    assert_eq!(sandbox::homes(Some(&real)), vec![real.clone()], "same folder once");
    assert_eq!(sandbox::homes(None), vec![real.clone()]);
    let p = Policy::with_home(Mode::Write, &proj.0, Some(&fake.0));
    for h in [&real, &fake.0] {
        for r in [".ssh", ".aws", ".config/elisa", "Library/Keychains", ".colima/_lima/_config"] {
            assert!(p.deny.contains(&h.join(r)), "{} not denied", h.join(r).display());
        }
        for r in [".colima", ".lima", ".docker/run"] {
            assert!(p.deny_sockets.contains(&h.join(r)), "{} sockets not denied", h.join(r).display());
        }
    }
    assert!(p.deny_sockets.contains(&std::path::PathBuf::from("/var/run/docker.sock")));
    let prof = p.profile().unwrap();
    assert!(prof.contains(&format!("(subpath \"{}/.ssh\")", real.display())), "{prof}");
    assert!(prof.contains(&format!("(remote unix-socket (subpath \"{}/.colima\"))", real.display())), "{prof}");
    // the socket deny comes last, after strict mode's unix-socket allowance
    let strict = Policy::with_home(Mode::Strict, &proj.0, Some(&fake.0)).profile().unwrap();
    assert!(strict.rfind("(deny network*").unwrap() > strict.rfind("(allow network*").unwrap(), "{strict}");
}

#[test]
fn quiet_strict_network_failures_get_the_hint() {
    let q = |cmd: &str, out: &str, code: Option<i32>| sandbox::quiet_network_hint(Mode::Strict, cmd, out, code);
    for cmd in ["curl -s https://example.com", "cd x && /usr/bin/curl -fsS https://e.com -o f", "FOO=1 wget -q https://e.com",
        "git clone https://github.com/a/b", "git -C repo fetch origin", "npm install left-pad", "pip install requests",
        "python3 -m pip install x", "cargo add serde", "ssh host true", "echo $(curl -s e.com)", "nc -z e.com 443"] {
        let h = q(cmd, "", Some(6)).unwrap_or_else(|| panic!("no hint for {cmd}"));
        assert!(h.contains("strict") && !h.contains('\n'), "{h}");
        assert!(h.starts_with(sandbox::HINT_MARK));
    }
    assert!(q("curl -s https://e.com", "curl: (7) Failed to connect\n", Some(7)).is_some(), "terse output");
    for cmd in ["ls", "git status", "cargo build", "npm test", "echo curl", "grep curl file"] {
        assert!(q(cmd, "", Some(1)).is_none(), "{cmd}");
    }
    let long: String = (0..10).map(|i| format!("line {i}\n")).collect();
    assert!(q("curl -s e.com", &long, Some(6)).is_none(), "a long output explains itself");
    assert!(q("curl -s e.com", "", Some(0)).is_none(), "success");
    assert!(sandbox::quiet_network_hint(Mode::Write, "curl -s e.com", "", Some(6)).is_none(), "write mode has network");
    assert!(sandbox::quiet_network_hint(Mode::Off, "curl -s e.com", "", Some(6)).is_none());
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use elisa::tools::{self, ToolCtx};
    use serde_json::json;
    use std::process::Stdio;

    /// A policy for `proj` with no temp / cache allowances (temp folders are where the test's "outside" targets
    /// live) and `secret` as the only denied store.
    fn policy(mode: Mode, proj: &Path, secret: &Path) -> Policy {
        Policy { write_dirs: vec![], write_files: vec![], deny: vec![secret.to_path_buf()], read_ok: vec![], socket_dirs: vec![proj.to_path_buf()], ..Policy::new(mode, proj) }
    }

    /// (exit code, stdout + stderr)
    fn sh(p: &Policy, cmd: &str) -> (i32, String) {
        let o = p.command(cmd).unwrap().current_dir(&p.project).stdin(Stdio::null()).output().unwrap();
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    #[test]
    fn sandbox_exec_exists() { assert!(Path::new(sandbox::SANDBOX_EXEC).is_file()); }

    #[test]
    fn writes_inside_ok_outside_denied() {
        let (proj, out, sec) = (Tmp::new("proj"), Tmp::new("out"), Tmp::new("sec"));
        let p = policy(Mode::Write, &proj.0, &sec.0);
        let (code, o) = sh(&p, "echo hi > a.txt && mkdir -p target/x && echo ok > target/x/b && cat a.txt");
        assert_eq!((code, o.trim()), (0, "hi"));
        let t = out.0.join("evil.txt");
        let (code, o) = sh(&p, &format!("echo x > '{}'", t.display()));
        assert_ne!(code, 0);
        assert!(o.to_lowercase().contains("operation not permitted"), "{o}");
        assert!(!t.exists());
        // nor delete / rename outside
        write_file(&out.0.join("keep"), "k");
        let (code, _) = sh(&p, &format!("rm -f '{0}/keep'; mv '{0}/keep' '{0}/moved'", out.0.display()));
        assert_ne!(code, 0);
        assert!(out.0.join("keep").exists());
    }

    #[test]
    fn secret_store_unreadable() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        write_file(&sec.0.join("id_ed25519"), "PRIVATE");
        let p = policy(Mode::Write, &proj.0, &sec.0);
        let (code, o) = sh(&p, &format!("cat '{}/id_ed25519'", sec.0.display()));
        assert_ne!(code, 0);
        assert!(!o.contains("PRIVATE") && o.contains("Operation not permitted"), "{o}");
        let (code, _) = sh(&p, &format!("ls '{}'", sec.0.display()));
        assert_ne!(code, 0);
        // a deny wins even over the project: a secret inside the project folder stays unreadable
        let inner = proj.0.join("secrets");
        write_file(&inner.join("k"), "INNER");
        let p2 = Policy { deny: vec![inner.clone()], ..policy(Mode::Write, &proj.0, &sec.0) };
        let (_, o) = sh(&p2, "cat secrets/k");
        assert!(!o.contains("INNER"), "{o}");
    }

    #[test]
    fn elisa_permission_files_and_git_hooks_protected() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        write_file(&proj.0.join(".elisa/allow"), "read\n");
        std::process::Command::new("git").args(["init", "-q"]).current_dir(&proj.0).status().unwrap();
        let p = policy(Mode::Write, &proj.0, &sec.0);
        for cmd in ["echo bash >> .elisa/allow", "echo 'shell_sandbox = \"off\"' > .elisa/config.toml", "echo '[x]' > .elisa/checks.toml",
            "mv .elisa .elisa2", "rm -rf .elisa", "echo '#!/bin/sh' > .git/hooks/pre-commit", "git config core.fsmonitor 'touch /x'", "mv .git .git2"] {
            let (code, o) = sh(&p, cmd);
            assert_ne!(code, 0, "{cmd} ran: {o}");
        }
        assert_eq!(std::fs::read_to_string(proj.0.join(".elisa/allow")).unwrap(), "read\n");
        assert!(!proj.0.join(".elisa/config.toml").exists() && !proj.0.join(".git/hooks/pre-commit").exists());
        // ordinary git work still runs
        let (code, o) = sh(&p, "echo x > f && git add f && git -c user.email=t@t -c user.name=t commit -qm one && git log --oneline | wc -l");
        assert_eq!(code, 0, "{o}");
        assert_eq!(o.trim(), "1");
        // notes and sessions inside .elisa stay writable
        let (code, o) = sh(&p, "echo n > .elisa/notes.md");
        assert_eq!(code, 0, "{o}");
    }

    #[test]
    fn git_init_works_in_a_fresh_project() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let (code, o) = sh(&policy(Mode::Write, &proj.0, &sec.0), "git init -q && git status --short");
        assert_eq!(code, 0, "{o}");
    }

    #[test]
    fn toolchain_runs() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let p = Policy::new(Mode::Write, &proj.0);
        let _ = sec;
        let (code, o) = sh(&p, "cargo --version && git --version");
        assert_eq!(code, 0, "{o}");
        assert!(o.contains("cargo ") && o.contains("git version"), "{o}");
    }

    #[test]
    fn children_inherit_the_sandbox() {
        let (proj, out, sec) = (Tmp::new("proj"), Tmp::new("out"), Tmp::new("sec"));
        let p = policy(Mode::Write, &proj.0, &sec.0);
        let t = out.0.join("child.txt");
        let d = out.0.join("daemon.txt");
        let cmd = format!("sh -c \"echo c > '{}'\"; python3 -c \"open('{}','w').write('x')\"; (nohup sh -c \"sleep 0.2; echo d > '{}'\" >/dev/null 2>&1 &); sleep 0.6",
            t.display(), t.display(), d.display());
        let _ = sh(&p, &cmd);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!t.exists() && !d.exists());
    }

    #[test]
    fn hand_offs_outside_the_sandbox_blocked() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let p = policy(Mode::Write, &proj.0, &sec.0);
        for cmd in ["/usr/bin/osascript -e 'return 1'", "/usr/bin/open -h", "/bin/launchctl list"] {
            let (code, o) = sh(&p, cmd);
            assert_ne!(code, 0, "{cmd}: {o}");
        }
    }

    #[test]
    fn odd_project_path_is_quoted() {
        let base = Tmp::new("odd");
        let proj = base.0.join("we\"ird \\ (dir)");
        std::fs::create_dir_all(&proj).unwrap();
        let out = base.0.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let p = Policy { write_dirs: vec![], write_files: vec![], deny: vec![], read_ok: vec![], socket_dirs: vec![], ..Policy::new(Mode::Write, &proj) };
        let (code, o) = sh(&p, "echo hi > a && cat a");
        assert_eq!((code, o.trim()), (0, "hi"));
        let (code, _) = sh(&p, "echo x > ../out/b");
        assert_ne!(code, 0);
        assert!(!out.join("b").exists());
    }

    #[test]
    fn strict_blocks_network_but_not_localhost() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let p = policy(Mode::Strict, &proj.0, &sec.0);
        let (code, o) = sh(&p, &format!("/usr/bin/nc -z -w 3 127.0.0.1 {port}"));
        assert_eq!(code, 0, "localhost: {o}");
        let (code, o) = sh(&p, "curl -sS -m 8 -o /dev/null https://example.com");
        assert_ne!(code, 0, "{o}\n{}", p.profile().unwrap());
        assert!(sandbox::hint(Mode::Strict, &o).is_some(), "{o}");
        let (code, o) = sh(&p, "/usr/bin/nc -z -w 3 1.1.1.1 443");
        assert_ne!(code, 0, "{o}");
        // unix sockets outside the project / temp folders (here: syslog) are off limits too
        let (code, o) = sh(&p, "/usr/bin/python3 -c \"import socket; s=socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM); s.connect('/private/var/run/syslog')\"");
        assert_ne!(code, 0, "{o}");
        // the same command in write mode reaches it (the check above failed because of the sandbox)
        let (code, o) = sh(&policy(Mode::Write, &proj.0, &sec.0), "/usr/bin/python3 -c \"import socket; s=socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM); s.connect('/private/var/run/syslog')\"");
        assert_eq!(code, 0, "{o}");
    }

    #[test]
    #[ignore = "needs the network"]
    fn write_mode_allows_network() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let (code, o) = sh(&policy(Mode::Write, &proj.0, &sec.0), "curl -sS -m 15 -o /dev/null -w '%{http_code}' https://example.com");
        assert_eq!(code, 0, "{o}");
    }

    /// Through the bash tool itself (default policy): inside writes run, a blocked one gets the one-line hint.
    #[test]
    fn bash_tool_is_sandboxed_with_hint() {
        sandbox::set_mode(Mode::Write);
        let proj = Tmp::new("tool");
        write_file(&proj.0.join(".elisa/allow"), "read\n");
        let ctx = ToolCtx { cwd: proj.0.clone(), max_chars: 100_000, bash_timeout_s: 20, ..ToolCtx::default() };
        let r = tools::run("bash", &json!({"command": "echo hi > x.txt && cat x.txt"}), &ctx);
        assert!(!r.is_error && r.content.contains("hi") && r.content.contains("[exit 0]"), "{}", r.content);
        let r = tools::run("bash", &json!({"command": "echo bash >> .elisa/allow"}), &ctx);
        assert!(r.content.contains("blocked by the shell sandbox") && r.content.contains("[exit 1]"), "{}", r.content);
        assert_eq!(std::fs::read_to_string(proj.0.join(".elisa/allow")).unwrap(), "read\n");
    }

    /// With $HOME pointed at another folder, the real home folder's secret stores stay unreadable (read-only probe of
    /// the real ~/.ssh; nothing there is written) and so do the injected home's.
    #[test]
    fn changed_home_env_still_denies_real_home() {
        let (proj, fake) = (Tmp::new("proj"), Tmp::new("fakehome"));
        write_file(&fake.0.join(".ssh/id_ed25519"), "FAKEKEY");
        let p = Policy { write_dirs: vec![], write_files: vec![], ..Policy::with_home(Mode::Write, &proj.0, Some(&fake.0)) };
        let (code, o) = sh(&p, &format!("cat '{}/.ssh/id_ed25519'", fake.0.display()));
        assert!(code != 0 && !o.contains("FAKEKEY"), "{o}");
        let real = sandbox::real(&sandbox::real_home().unwrap());
        if real.join(".ssh").is_dir() {
            let (code, o) = sh(&p, &format!("HOME='{}' ls '{}/.ssh'", fake.0.display(), real.display()));
            assert_ne!(code, 0, "{o}");
            assert!(o.contains("Operation not permitted"), "{o}");
        }
    }

    /// A unix socket on the deny list (here a test socket standing in for Colima's docker.sock) cannot be connected
    /// to in write or strict mode, although its folder is otherwise allowed; without the entry it can.
    #[test]
    fn docker_sockets_blocked_in_every_mode() {
        use std::os::unix::net::UnixListener;
        // short path: a unix socket path has at most 104 bytes
        let dir = PathBuf::from(format!("/private/tmp/esk-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = Tmp(dir);
        let sock = dir.0.join("docker.sock");
        let l = UnixListener::bind(&sock).unwrap();
        l.set_nonblocking(true).unwrap();
        let proj = Tmp::new("proj");
        let connect = format!("/usr/bin/python3 -c \"import socket; s=socket.socket(socket.AF_UNIX); s.connect('{}'); print('connected')\"", sock.display());
        let base = Policy { write_dirs: vec![], write_files: vec![], deny: vec![], read_ok: vec![], deny_sockets: vec![], ..Policy::new(Mode::Write, &proj.0) };
        let (code, o) = sh(&base, &connect);
        // any line: macOS's python3 shim may warn that the sandbox kept it from writing its xcrun cache, and that
        // stderr line can land before or after "connected"
        assert_eq!(code, 0, "without the deny entry: {o}");
        assert!(o.lines().any(|l| l.trim() == "connected"), "without the deny entry: {o}");
        for mode in [Mode::Write, Mode::Strict] {
            for entry in [sock.clone(), dir.0.clone()] {
                let p = Policy { mode, socket_dirs: vec![proj.0.clone(), dir.0.clone()], deny_sockets: vec![entry.clone()], ..base.clone() };
                let (code, o) = sh(&p, &connect);
                assert_ne!(code, 0, "{mode:?} {}: {o}", entry.display());
                assert!(o.contains("Operation not permitted") && !o.contains("connected"), "{o}");
            }
        }
        // the default policy carries the real home's Colima / Lima / Docker socket folders
        let d = Policy::new(Mode::Write, &proj.0);
        let real = sandbox::real(&sandbox::real_home().unwrap());
        assert!(d.deny_sockets.contains(&real.join(".colima")) && d.deny_sockets.contains(&real.join(".lima")));
    }

    /// A strict-mode `curl -s` that fails silently gets the network hint; through `checks::run`, a passing check whose
    /// writes the sandbox blocked says so.
    #[test]
    fn quiet_curl_hint_and_check_note() {
        let (proj, sec) = (Tmp::new("proj"), Tmp::new("sec"));
        let p = policy(Mode::Strict, &proj.0, &sec.0);
        let cmd = "curl -s -m 8 -o /dev/null https://example.com";
        let (code, o) = sh(&p, cmd);
        assert_ne!(code, 0, "{o}");
        assert!(sandbox::quiet_network_hint(Mode::Strict, cmd, &o, Some(code)).is_some(), "{o:?}");

        sandbox::set_mode(Mode::Write);
        write_file(&proj.0.join(".elisa/allow"), "read\n");
        let c = elisa::checks::Check { name: "sh check".into(), cmd: "echo x >> .elisa/allow; true".into(), stage: elisa::checks::Stage::Fast, timeout_s: 20 };
        let out = elisa::checks::run(&c, &proj.0, 4000);
        assert_eq!(out.code, Some(0));
        assert_eq!(out.status, elisa::frontend::CheckStatus::Pass);
        assert_eq!(out.summary, elisa::checks::SANDBOX_NOTE);
        let c = elisa::checks::Check { cmd: "true".into(), ..c };
        assert_eq!(elisa::checks::run(&c, &proj.0, 4000).summary, "");
    }
}
