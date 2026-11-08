//! Sandbox images (src/images.rs) end to end with the real binary, a temp HOME (never the real ~/.elisa) and a fake
//! `docker` (no network, no Colima): `elisa sandbox list / pull`, the digest pin, cancelling a pull, and a session in
//! which a `run_code` call is blocked by a missing image (Elisa asks; yes downloads and runs the call again, no hands
//! the model the message) and a plugin server that could not start for a missing image.
use elisa::eval_fake::{FakeModel, Scripted};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// A docker stand-in: images live as files in `state/` (`id|bytes|digest`); `pull` prints layer lines and (with
/// FAKE_PULL_HANG) waits; `run` succeeds only when the image it names is there; every call is logged.
const FAKE: &str = r#"#!/bin/sh
# MCP servers get a clean environment: the state folder is next to this script
S="${FAKE_STATE:-$(dirname "$0")/../state}"
echo "ARGS $*" >> "$S/log"
key() { echo "$1" | tr '/:@' '___'; }
case "$1" in
  version) echo 27.0.0; exit 0;;
  image)
    if [ "$2" = ls ]; then for f in "$S"/*_*; do [ -f "$f" ] && echo "$(basename "$f" | sed 's/_/:/')|7MB"; done; exit 0; fi
    f="$S/$(key "$5")"
    if [ -f "$f" ]; then cat "$f"; exit 0; fi
    echo "Error response from daemon: No such image: $5" >&2; exit 1;;
  pull)
    echo "${2#*:}: Pulling from library/x"
    echo "aaaaaaaaaaaa: Pulling fs layer"
    echo "bbbbbbbbbbbb: Pulling fs layer"
    echo "aaaaaaaaaaaa: Pull complete"
    if [ -n "$FAKE_PULL_HANG" ]; then echo $$ > "$S/pull.pid"; sleep 30 & echo $! > "$S/sleep.pid"; wait; fi
    echo "bbbbbbbbbbbb: Pull complete"
    d="${FAKE_DIGEST:-sha256:1111111111111111}"
    echo "sha256:id-$d|7000000|x@$d " > "$S/$(key "$2")"
    echo "Digest: $d"; exit 0;;
  tag) echo "sha256:old|7000000|x@restored " > "$S/$(key "$3")"; exit 0;;
  rmi) rm -f "$S/$(key "$2")"; exit 0;;
  run)
    for a in "$@"; do if [ -f "$S/$(key "$a")" ]; then echo "fake ran in $a"; exit 0; fi; done
    echo "docker: Error response from daemon: No such image" >&2; exit 125;;
esac
exit 0
"#;

/// root/{home, project, bin, state}
fn setup(tag: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("elisa-images-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["home/.config/elisa", "project", "bin", "state"] { std::fs::create_dir_all(d.join(sub)).unwrap(); }
    let d = d.canonicalize().unwrap();
    std::fs::write(d.join("bin/docker"), FAKE).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(d.join("bin/docker"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(d.join("sock"), "").unwrap();
    std::fs::write(d.join("home/.config/elisa/config.toml"), "sandbox = \"colima\"\n").unwrap();
    Dir(d)
}

fn env(d: &Path) -> Vec<(&'static str, String)> {
    vec![("HOME", d.join("home").display().to_string()), ("PATH", format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default())),
        ("DOCKER_HOST", format!("unix://{}", d.join("sock").display())), ("FAKE_STATE", d.join("state").display().to_string())]
}

fn elisa(d: &Path, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_elisa"));
    c.args(args).current_dir(d.join("project")).stdin(Stdio::null());
    for (k, v) in env(d) { c.env(k, v); }
    for (k, v) in extra { c.env(k, v); }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn log(d: &Path) -> String { std::fs::read_to_string(d.join("state/log")).unwrap_or_default() }
fn audit(d: &Path) -> String {
    std::fs::read_dir(d.join("home/.elisa/audit")).into_iter().flatten().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap_or_default()).collect()
}

#[test]
fn the_cli_lists_asks_pulls_only_listed_images_and_pins_the_digest() {
    let d = setup("cli");
    let (code, out) = elisa(&d.0, &["sandbox", "list", "--json"], &[]);
    assert_eq!(code, 0, "{out}");
    let j: Value = serde_json::from_str(out.trim()).unwrap();
    let bash = j["images"].as_array().unwrap().iter().find(|i| i["image"] == "bash:5.3").unwrap();
    assert_eq!((bash["present"].as_bool(), bash["stable"].as_bool(), bash["purpose"].as_str()), (Some(false), Some(true), Some("Bash sandbox")));
    // no terminal and no --yes: asked, nothing downloaded
    let (code, out) = elisa(&d.0, &["sandbox", "pull", "bash"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("bash:5.3 (~7 MB), downloaded into Colima's VM — nothing is installed on this Mac") && out.contains("nothing was downloaded"), "{out}");
    assert!(!log(&d.0).contains("ARGS pull"), "{}", log(&d.0));
    // an image that is not on the list is never pulled
    let (code, out) = elisa(&d.0, &["sandbox", "pull", "ubuntu:latest", "--yes"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("not on Elisa's image list") && !log(&d.0).contains("pull ubuntu"), "{out}");
    // approved: JSON lines for the app, the digest pinned, audited
    let (code, out) = elisa(&d.0, &["sandbox", "pull", "bash", "--yes", "--json"], &[]);
    assert_eq!(code, 0, "{out}");
    let evs: Vec<Value> = out.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    assert!(evs.iter().any(|e| e["type"] == "image_pull" && e["state"] == "pulling" && e["layers"] == 2), "{out}");
    assert!(evs.iter().any(|e| e["type"] == "image_pull" && e["state"] == "done" && e["text"].as_str().unwrap().contains("pinned in ~/.elisa/images.lock")), "{out}");
    assert!(evs.iter().any(|e| e["type"] == "sandbox_images"), "{out}");
    assert!(log(&d.0).contains("ARGS pull bash:5.3"));
    let lock = std::fs::read_to_string(d.0.join("home/.elisa/images.lock")).unwrap();
    assert!(lock.contains("x@sha256:1111111111111111"), "{lock}");
    assert!(audit(&d.0).contains("pull bash:5.3") && audit(&d.0).contains("\"declined\""), "{}", audit(&d.0));
    // a later pull with another digest asks again; with nobody to say yes the pinned image is put back
    let (code, out) = elisa(&d.0, &["sandbox", "pull", "bash", "--yes"], &[("FAKE_DIGEST", "sha256:2222222222222222")]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("WARNING: bash:5.3 now has another digest") && out.contains("kept the pinned bash:5.3"), "{out}");
    assert!(log(&d.0).contains("ARGS tag sha256:id-sha256:1111111111111111 bash:5.3"), "{}", log(&d.0));
    assert!(std::fs::read_to_string(d.0.join("home/.elisa/images.lock")).unwrap().contains("1111111111111111"));
    // `stable` takes only the missing ones
    let (_, out) = elisa(&d.0, &["sandbox", "pull", "stable"], &[]);
    assert!(out.contains("Download 4 images") && !out.contains("bash:5.3 (~"), "{out}");
}

#[test]
fn a_cancelled_pull_ends_its_whole_process_group() {
    let d = setup("cancel");
    let mut c = Command::new(env!("CARGO_BIN_EXE_elisa"));
    c.args(["sandbox", "pull", "go", "--yes", "--json"]).current_dir(d.0.join("project")).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env(&d.0) { c.env(k, v); }
    c.env("FAKE_PULL_HANG", "1");
    let child = c.spawn().unwrap();
    let pidf = d.0.join("state/sleep.pid");
    let t0 = Instant::now();
    while !pidf.exists() { assert!(t0.elapsed() < Duration::from_secs(10), "the pull never started"); std::thread::sleep(Duration::from_millis(20)); }
    std::thread::sleep(Duration::from_millis(200));
    let sleeper: i32 = std::fs::read_to_string(&pidf).unwrap().trim().parse().unwrap();
    // what the app does to cancel: SIGTERM to the CLI
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("\"state\":\"cancelled\""), "{text}");
    std::thread::sleep(Duration::from_millis(200));
    // the grandchild (`sleep`, same group as docker pull) is gone too
    let alive = unsafe { libc::kill(sleeper, 0) } == 0;
    assert!(!alive, "the pull's process group survived the cancel");
    assert!(audit(&d.0).contains("\"cancelled\""), "{}", audit(&d.0));
    assert!(!d.0.join("home/.elisa/images.lock").exists());
}

/// a `--serve-jsonl` session
struct Sess { child: Child, stdin: ChildStdin, rx: Receiver<Value> }

impl Sess {
    fn start(d: &Path, model: &FakeModel) -> Sess {
        let mut c = Command::new(env!("CARGO_BIN_EXE_elisa"));
        c.args(["--serve-jsonl", "--cwd"]).arg(d.join("project")).args(["--base-url", &model.url(), "--type", "streamdense", "--model", "eval-fake"])
            .current_dir(d.join("project")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env(d) { c.env(k, v); }
        let mut child = c.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || for l in BufReader::new(out).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l) { if tx.send(v).is_err() { break; } }
        });
        Sess { child, stdin, rx }
    }
    fn cmd(&mut self, v: Value) { writeln!(self.stdin, "{v}").unwrap(); }
    fn until(&mut self, kind: &str, answers: &mut Vec<&str>) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut evs = Vec::new();
        loop {
            let e = self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap_or_else(|_| panic!("no {kind}: {evs:?}"));
            if e["type"] == "approve" {
                let a = if answers.is_empty() { "no" } else { answers.remove(0) };
                self.cmd(json!({"type": "approve", "answer": a}));
            }
            let end = e["type"] == kind;
            evs.push(e);
            if end { return evs; }
        }
    }
    fn turn(&mut self, text: &str, answers: &[&str]) -> Vec<Value> {
        self.cmd(json!({"type": "send", "text": text}));
        let mut a = answers.to_vec();
        let evs = self.until("turn_done", &mut a);
        self.until("ready", &mut Vec::new());
        evs
    }
}

impl Drop for Sess {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

fn tool(name: &str, input: Value) -> Scripted { Scripted { tools: vec![(name.into(), input)], ..Scripted::default() } }
fn text(t: &str) -> Scripted { Scripted { text: Some(t.into()), ..Scripted::default() } }
fn of<'a>(evs: &'a [Value], kind: &str) -> Vec<&'a Value> { evs.iter().filter(|e| e["type"] == kind).collect() }

#[test]
fn a_run_blocked_by_a_missing_image_asks_the_user_and_runs_again_after_the_download() {
    let d = setup("session");
    // the project pins python 3.14: that line is the one asked for
    std::fs::create_dir_all(d.0.join("project/.git")).unwrap();
    std::fs::write(d.0.join("project/.python-version"), "3.14\n").unwrap();
    let call = json!({"language": "python", "code": "print(6*7)"});
    let model = FakeModel::start(vec![tool("run_code", call.clone()), text("ran"), tool("run_code", call), text("could not run")], 32768).unwrap();
    let mut s = Sess::start(&d.0, &model);
    s.until("ready", &mut Vec::new());
    // the run is approved, then Elisa itself asks about the image: yes
    let evs = s.turn("run it", &["yes", "yes"]);
    let asked = of(&evs, "approve");
    assert_eq!(asked.len(), 2, "{evs:?}");
    assert_eq!((asked[1]["question"].as_bool(), asked[1]["tool"].as_str()), (Some(true), Some("sandbox pull python:3.14-alpine")), "{asked:?}");
    assert_eq!(asked[1]["summary"], "The Python 3.14 sandbox needs python:3.14-alpine (~18 MB), downloaded into Colima's VM — nothing is installed on this Mac. Download it now?");
    assert!(of(&evs, "image_pull").iter().any(|e| e["state"] == "done"), "{evs:?}");
    assert!(of(&evs, "sandbox_images").iter().any(|e| e["images"].as_array().unwrap().iter().any(|i| i["image"] == "python:3.14-alpine" && i["present"] == true && i["project"] == true)), "{evs:?}");
    let done = of(&evs, "tool_done");
    assert_eq!(done.len(), 1, "one tool result for the call: {evs:?}");
    assert!(done[0]["output"].as_str().unwrap().contains("fake ran in python:3.14-alpine"), "{done:?}");
    let l = log(&d.0);
    assert_eq!(l.matches("ARGS pull ").count(), 1, "{l}");
    assert!(l.contains("ARGS pull python:3.14-alpine"), "{l}");
    // the model never got the missing-image text
    assert!(!model.requests().iter().any(|r| r.to_string().contains("is not on this machine")));
    // no: the model gets the message, nothing is pulled
    std::fs::remove_file(d.0.join("state/python_3.14-alpine")).unwrap();
    let evs = s.turn("again", &["yes", "no"]);
    let done = of(&evs, "tool_done");
    assert!(done[0]["output"].as_str().unwrap().contains("python:3.14-alpine is not on this machine"), "{done:?}");
    assert_eq!(log(&d.0).matches("ARGS pull ").count(), 1);
    let a = audit(&d.0);
    assert!(a.contains("\"image_pull\"") && a.contains("\"declined\"") && a.contains("\"ok\""), "{a}");
    // /sandbox lists the images; the request form works too
    s.cmd(json!({"type": "sandbox", "op": "list"}));
    let evs = s.until("turn_done", &mut Vec::new());
    assert!(of(&evs, "info").iter().any(|e| e["text"].as_str().unwrap_or("").contains("Sandbox images")), "{evs:?}");
}

#[test]
fn a_present_older_line_runs_when_the_user_says_no_and_is_named_once() {
    let d = setup("fallback");
    // on this Mac: python 3.13 and bash 5.2 (not stable); the project asks for python >= 3.13
    for i in ["python_3.13-alpine", "bash_5.2"] { std::fs::write(d.0.join("state").join(i), "sha256:old|7000000|x@sha256:old ").unwrap(); }
    std::fs::create_dir_all(d.0.join("project/.git")).unwrap();
    std::fs::write(d.0.join("project/pyproject.toml"), "[project]\nname = 'x'\nrequires-python = '>=3.13'\n").unwrap();
    // the list marks the line runs and installers take
    let (_, out) = elisa(&d.0, &["sandbox", "list", "--json"], &[]);
    let j: Value = serde_json::from_str(&out).unwrap();
    let row = |img: &str| j["images"].as_array().unwrap().iter().find(|i| i["image"] == img).cloned().unwrap();
    assert_eq!((row("bash:5.2")["default"].as_bool(), row("bash:5.3")["default"].as_bool(), row("python:3.15-alpine")["project"].as_bool()), (Some(true), Some(false), Some(true)), "{j}");
    assert_eq!(row("python:3.13-alpine")["purpose"], "Python sandbox · pip plugins");
    let (_, out) = elisa(&d.0, &["sandbox", "list"], &[]);
    assert!(out.lines().any(|l| l.contains("bash:5.2") && l.contains("on this Mac")), "{out}");
    let py = json!({"language": "python", "code": "print(6*7)"});
    let sh = json!({"language": "bash", "code": "echo hi"});
    let model = FakeModel::start(vec![tool("run_code", py.clone()), text("a"), tool("run_code", py), text("b"),
        tool("run_code", sh.clone()), text("c"), tool("run_code", sh), text("d")], 32768).unwrap();
    let mut s = Sess::start(&d.0, &model);
    s.until("ready", &mut Vec::new());
    // the newest line that fits (3.15) is asked for; no: the run uses 3.13, the newest present one that fits
    let evs = s.turn("run it", &["yes", "no"]);
    let asked = of(&evs, "approve");
    assert_eq!(asked.len(), 2, "{evs:?}");
    assert_eq!(asked[1]["tool"], "sandbox pull python:3.15-alpine", "{asked:?}");
    let out = of(&evs, "tool_done")[0]["output"].as_str().unwrap().to_string();
    assert!(out.contains("fake ran in python:3.13-alpine") && out.contains("python:3.15-alpine is not on this Mac (not downloaded); ran on Python 3.13"), "{out}");
    // not asked again this session
    let evs = s.turn("again", &["yes"]);
    assert_eq!(of(&evs, "approve").len(), 1, "{evs:?}");
    assert!(of(&evs, "tool_done")[0]["output"].as_str().unwrap().contains("fake ran in python:3.13-alpine"), "{evs:?}");
    // no project version: the present line, named once per session
    let evs = s.turn("bash", &["yes"]);
    assert_eq!(of(&evs, "approve").len(), 1, "no download question: {evs:?}");
    let out = of(&evs, "tool_done")[0]["output"].as_str().unwrap().to_string();
    assert!(out.contains("fake ran in bash:5.2") && out.contains("note: using bash 5.2 (on this Mac); stable is 5.3, /sandbox pull bash to get it"), "{out}");
    let evs = s.turn("bash again", &["yes"]);
    let out = of(&evs, "tool_done")[0]["output"].as_str().unwrap().to_string();
    assert!(out.contains("fake ran in bash:5.2") && !out.contains("note:"), "{out}");
    assert_eq!(log(&d.0).matches("ARGS pull ").count(), 0);
}

#[test]
fn a_plugin_server_down_for_a_missing_image_is_offered_at_startup() {
    let d = setup("plugin");
    // a host-run server that fails the way `--plugin-run` does when its image is missing
    std::fs::create_dir_all(d.0.join("src")).unwrap();
    std::fs::write(d.0.join("src/plugin.toml"), "[plugin]\nname = \"p\"\nversion = \"1.0.0\"\n\n[[server]]\nname = \"srv\"\nruns = \"host\"\n\
        command = [\"sh\", \"-c\", \"echo 'elisa:missing-image=bash:5.3 · the image bash:5.3 is not on this machine · plugin p/srv' >&2; exit 2\"]\n").unwrap();
    let (code, out) = elisa(&d.0, &["plugin", "add", &d.0.join("src").display().to_string(), "--yes"], &[]);
    assert_eq!(code, 0, "{out}");
    let model = FakeModel::start(vec![text("hi")], 32768).unwrap();
    let mut s = Sess::start(&d.0, &model);
    let evs = s.until("ready", &mut vec!["no"]);
    let q = of(&evs, "approve");
    assert_eq!(q.len(), 1, "{evs:?}");
    assert_eq!(q[0]["tool"], "sandbox pull bash:5.3");
    assert!(q[0]["summary"].as_str().unwrap().starts_with("Plugin server srv needs bash:5.3 (~7 MB)"), "{q:?}");
    assert!(!log(&d.0).contains("ARGS pull"), "{}", log(&d.0));
}

/// With the real Colima (ignored by default; no download ever happens in it): the list reads what Colima has, a run on
/// an image that is there works, and a run whose image is missing asks — answered no, nothing is pulled.
///   DOCKER_HOST=unix://$HOME/.colima/default/docker.sock cargo test --release --test images -- --ignored
#[test]
#[ignore = "needs Colima with python:3.13-alpine (and no newer python), rust:1.91-alpine without rust:1.99-alpine, and no golang image"]
fn colima_a_missing_image_is_asked_and_no_pulls_nothing() {
    let host = std::env::var("DOCKER_HOST").expect("set DOCKER_HOST to Colima's socket");
    let d = setup("colima");
    let real = |args: &[&str]| -> String {
        let o = Command::new("docker").args(args).env("DOCKER_HOST", &host).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let go = elisa::images::stable("go").image;
    let has = |i: &str| !real(&["images", "-q", i]).trim().is_empty();
    assert!(elisa::images::lines("go").iter().all(|i| !has(i.image)), "a golang image is pulled: this test needs none");
    assert!(has("python:3.13-alpine") && !has("python:3.15-alpine") && !has("python:3.14-alpine"), "needs python 3.13 as the newest present line");
    assert!(has("rust:1.91-alpine") && !has("rust:1.99-alpine"), "needs rust:1.91-alpine and not rust:1.99-alpine");
    // the project asks for rust >= 1.80: 1.99 is asked for, 1.91 (present) fits too
    std::fs::write(d.0.join("project/Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\nrust-version = \"1.80\"\n").unwrap();
    let images_before = real(&["images", "-q", "--no-trunc"]);
    // the real docker CLI, not the fake
    let path = std::env::var("PATH").unwrap_or_default();
    let envs = [("HOME", d.0.join("home").display().to_string()), ("PATH", path), ("DOCKER_HOST", host.clone())];
    let mut c = Command::new(env!("CARGO_BIN_EXE_elisa"));
    c.args(["sandbox", "list", "--json"]).current_dir(d.0.join("project")).envs(envs.clone());
    let out = String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned();
    eprintln!("sandbox list: {out}");
    let j: Value = serde_json::from_str(out.trim()).unwrap();
    let row = |img: &str| j["images"].as_array().unwrap().iter().find(|i| i["image"] == img).cloned().unwrap();
    assert_eq!(row("python:3.13-alpine")["present"], true);
    assert!(row("python:3.13-alpine")["disk_mb"].as_u64().unwrap() > 10);
    assert_eq!(row(go)["present"], false);
    assert_eq!((row("python:3.13-alpine")["default"].as_bool(), row("rust:1.91-alpine")["default"].as_bool(), row("rust:1.99-alpine")["project"].as_bool()), (Some(true), Some(true), Some(true)));
    let model = FakeModel::start(vec![
        tool("run_code", json!({"language": "python", "code": "print(6*7)"})), text("ok"),
        tool("run_code", json!({"language": "rust", "code": "fn main() { println!(\"{}\", 6 * 7); }"})), text("rust ok"),
        tool("run_code", json!({"language": "go", "code": "package main\nimport \"fmt\"\nfunc main() { fmt.Println(6 * 7) }"})), text("no go"),
    ], 32768).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
    cmd.args(["--serve-jsonl", "--cwd"]).arg(d.0.join("project")).args(["--base-url", &model.url(), "--type", "streamdense", "--model", "eval-fake"])
        .current_dir(d.0.join("project")).envs(envs).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let stdin = child.stdin.take().unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || for l in BufReader::new(out).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&l) { if tx.send(v).is_err() { break; } }
    });
    let mut s = Sess { child, stdin, rx };
    s.until("ready", &mut Vec::new());
    let evs = s.turn("run python", &["yes"]);
    let done = of(&evs, "tool_done");
    eprintln!("python: {}", done[0]["output"]);
    let o = done[0]["output"].as_str().unwrap();
    assert!(o.contains("python:3.13-alpine") && o.contains("42") && o.contains("note: using python 3.13 (on this Mac); stable is 3.15"), "{done:?}");
    // rust: the project's best line is missing → asked → no → runs on the present 1.91, which meets the minimum
    let evs = s.turn("run rust", &["yes", "no"]);
    let asked = of(&evs, "approve");
    assert_eq!(asked.len(), 2, "{evs:?}");
    assert_eq!(asked[1]["tool"], "sandbox pull rust:1.99-alpine");
    let done = of(&evs, "tool_done");
    eprintln!("rust: {}", done[0]["output"]);
    let o = done[0]["output"].as_str().unwrap();
    assert!(o.contains("exit 0") && o.contains("rust:1.91-alpine") && o.contains("stdout:\n42") && o.contains("rust:1.99-alpine is not on this Mac (not downloaded)"), "{done:?}");
    let evs = s.turn("run go", &["yes", "no"]);
    let asked = of(&evs, "approve");
    eprintln!("asked: {}", asked.iter().map(|a| a["summary"].to_string()).collect::<Vec<_>>().join(" | "));
    assert_eq!(asked.len(), 2, "{evs:?}");
    assert_eq!(asked[1]["tool"], format!("sandbox pull {go}"));
    let done = of(&evs, "tool_done");
    eprintln!("go: {}", done[0]["output"]);
    assert!(done[0]["output"].as_str().unwrap().contains("is not on this machine"), "{done:?}");
    assert!(of(&evs, "image_pull").is_empty(), "{evs:?}");
    assert_eq!(real(&["images", "-q", "--no-trunc"]), images_before, "an image appeared");
    assert!(audit(&d.0).contains("\"declined\""));
}
