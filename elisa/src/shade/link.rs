//! The link to the native companion window (`app/ElisaCompanion`): Shade drawn by the brand kit's own renderer in a
//! small floating macOS window, instead of the terminal's half-block dock.
//!
//! Local only: a Unix domain socket, `$ELISA_COMPANION_SOCK` or `~/.elisa/companion.sock` (no network port). The
//! companion listens; Elisa connects as a client when `--companion` / config `companion = true` is set.
//!
//! Protocol v1, UTF-8 JSON, one object per line:
//! - Elisa → companion: `{"v":1,"type":"hello","app":"elisa","pid":N}` on connect; then
//!   `{"v":1,"type":"state","state":"editing","verb":"EDITING","target":"split.rs","tone":"accent","pop":false,"theme":"dark"}`
//!   each time what Shade shows changes (state, bubble, attention pop, theme); `{"v":1,"type":"bye"}` on exit.
//!   `tone` = the bubble verb's colour role (accent / warn / error); `pop` = the 1.22x attention pop.
//! - companion → Elisa: `{"v":1,"type":"care","action":"poke"|"water"|"food"|"toy"}` (a click on Shade or the tray),
//!   handled like the terminal's own care keys; the new state goes back as a state line, so both stay in sync.
//!
//! Never blocks or slows the TUI: the socket is non-blocking, a connect is tried at most once a second, a frame that
//! cannot be written now is superseded by the next one, and when the companion is not running nothing is sent.
use super::{Companion, Event, St, Tone, View};
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// protocol version
pub const VERSION: u64 = 1;
/// how often a missing companion is looked for again
const RETRY: Duration = Duration::from_millis(1000);
/// unsent bytes kept at most (beyond this, older whole lines are dropped: a newer state supersedes them)
const OUT_CAP: usize = 4096;
/// unparsed bytes kept at most from the companion
const IN_CAP: usize = 64 * 1024;

/// `$ELISA_COMPANION_SOCK`, else `~/.elisa/companion.sock` (the companion app uses the same rule)
pub fn socket_path() -> PathBuf {
    socket_path_from(std::env::var("ELISA_COMPANION_SOCK").ok(), std::env::var_os("HOME").map(PathBuf::from))
}

pub fn socket_path_from(sock: Option<String>, home: Option<PathBuf>) -> PathBuf {
    match sock.filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => home.unwrap_or_else(|| PathBuf::from(".")).join(".elisa/companion.sock"),
    }
}

/// what the companion window shows: one state line
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub state: St, pub verb: String, pub target: String, pub tone: Tone, pub pop: bool, pub light: bool,
    /// a long tool's occasional fidget (`View::fidget`): draw the `listening` pose now, unless motion is reduced
    pub fidget: bool,
}

impl Frame {
    /// the companion's view of this frame (`light` = the light theme)
    pub fn of(v: &View, light: bool) -> Frame {
        let (verb, target, tone) = v.bubble.as_ref().map_or((String::new(), String::new(), Tone::Accent), |b| (b.verb.to_string(), b.target.clone(), b.tone));
        Frame { state: v.st, verb, target, tone, pop: v.st.pops(), light, fidget: v.fidget }
    }
}

fn tone_name(t: Tone) -> &'static str { match t { Tone::Accent => "accent", Tone::Warn => "warn", Tone::Error => "error" } }

/// a state line, newline-terminated
pub fn encode(f: &Frame) -> String {
    let j = json!({"v": VERSION, "type": "state", "state": f.state.name(), "verb": f.verb, "target": f.target,
        "tone": tone_name(f.tone), "pop": f.pop, "theme": if f.light { "light" } else { "dark" }, "fidget": f.fidget});
    format!("{j}\n")
}

/// a state line back into a frame (None: not a v1 state line)
pub fn decode(line: &str) -> Option<Frame> {
    let j: Value = serde_json::from_str(line.trim()).ok()?;
    if j["type"] != "state" || j["v"].as_u64() != Some(VERSION) { return None; }
    let s = |k: &str| j[k].as_str().unwrap_or("").to_string();
    Some(Frame {
        state: St::parse(j["state"].as_str()?)?,
        verb: s("verb"), target: s("target"),
        tone: match j["tone"].as_str() { Some("warn") => Tone::Warn, Some("error") => Tone::Error, _ => Tone::Accent },
        pop: j["pop"].as_bool().unwrap_or(false),
        light: j["theme"] == "light",
        fidget: j["fidget"].as_bool().unwrap_or(false),
    })
}

/// a care action from the companion (kit v2 care tray, or a click on Shade)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Care { Poke, Water, Food, Toy }

impl Care {
    pub const ALL: [Care; 4] = [Care::Poke, Care::Water, Care::Food, Care::Toy];
    pub fn name(self) -> &'static str { match self { Care::Poke => "poke", Care::Water => "water", Care::Food => "food", Care::Toy => "toy" } }
    pub fn event(self) -> Event<'static> { match self { Care::Poke => Event::Poke, Care::Water => Event::Water, Care::Food => Event::Food, Care::Toy => Event::Toy } }
}

pub fn encode_care(c: Care) -> String { format!("{}\n", json!({"v": VERSION, "type": "care", "action": c.name()})) }

/// a care line (None: anything else, including unknown actions)
pub fn decode_care(line: &str) -> Option<Care> {
    let j: Value = serde_json::from_str(line.trim()).ok()?;
    if j["type"] != "care" { return None; }
    let a = j["action"].as_str()?;
    Care::ALL.into_iter().find(|c| c.name() == a)
}

fn hello() -> String { format!("{}\n", json!({"v": VERSION, "type": "hello", "app": "elisa", "pid": std::process::id()})) }
fn bye() -> String { format!("{}\n", json!({"v": VERSION, "type": "bye"})) }

/// The client end. Call `pump` once per drawn frame.
pub struct Link {
    path: PathBuf,
    stream: Option<UnixStream>,
    out: Vec<u8>,
    inbuf: Vec<u8>,
    /// the last frame queued on this connection
    sent: Option<Frame>,
    next_try: Option<Instant>,
}

impl Link {
    pub fn new(path: impl AsRef<Path>) -> Link {
        Link { path: path.as_ref().to_path_buf(), stream: None, out: vec![], inbuf: vec![], sent: None, next_try: None }
    }

    /// a companion window is connected (the terminal dock hides while it is)
    pub fn connected(&self) -> bool { self.stream.is_some() }

    /// Shade's view at `now`, mirrored to the companion; care clicks from the companion are applied first-hand
    /// (so the returned view already shows them)
    pub fn pump(&mut self, c: &mut Companion, now: u64, light: bool) -> View {
        let v = c.view(now);
        let cares = self.sync(Some(&Frame::of(&v, light)));
        if cares.is_empty() { return v; }
        for k in cares { c.on(now, k.event()); }
        let v = c.view(now);
        self.sync(Some(&Frame::of(&v, light)));
        v
    }

    /// connect if due, send `f` if it changed, read care actions. Never blocks.
    pub fn sync(&mut self, f: Option<&Frame>) -> Vec<Care> {
        self.ensure();
        if self.stream.is_none() { return vec![]; }
        if let Some(f) = f.filter(|f| self.sent.as_ref() != Some(*f)) {
            self.queue(&encode(f));
            self.sent = Some(f.clone());
        }
        self.flush();
        self.read()
    }

    fn ensure(&mut self) {
        if self.stream.is_some() || self.next_try.is_some_and(|t| Instant::now() < t) { return; }
        self.next_try = Some(Instant::now() + RETRY);
        // a missing socket / no listener fails at once (ENOENT / ECONNREFUSED)
        let Ok(s) = UnixStream::connect(&self.path) else { return };
        if s.set_nonblocking(true).is_err() { return; }
        self.stream = Some(s);
        self.sent = None;
        self.out.clear();
        self.inbuf.clear();
        self.queue(&hello());
    }

    fn queue(&mut self, line: &str) {
        if self.out.len() + line.len() > OUT_CAP {
            // keep only the line being written (its head may be on the wire already); the rest is superseded
            let keep = self.out.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
            self.out.truncate(keep);
        }
        self.out.extend_from_slice(line.as_bytes());
    }

    fn flush(&mut self) {
        while !self.out.is_empty() {
            let Some(s) = self.stream.as_mut() else { return };
            match s.write(&self.out) {
                Ok(0) => return self.drop_conn(),
                Ok(n) => { self.out.drain(..n); }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return self.drop_conn(),
            }
        }
    }

    fn read(&mut self) -> Vec<Care> {
        let mut buf = [0u8; 1024];
        while let Some(s) = self.stream.as_mut() {
            match s.read(&mut buf) {
                Ok(0) => { self.drop_conn(); break; }
                Ok(n) => self.inbuf.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => { self.drop_conn(); break; }
            }
        }
        let mut cares = vec![];
        while let Some(i) = self.inbuf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.inbuf.drain(..=i).collect();
            if let Some(c) = std::str::from_utf8(&line).ok().and_then(decode_care) { cares.push(c); }
        }
        if self.inbuf.len() > IN_CAP { self.inbuf.clear(); }
        cares
    }

    fn drop_conn(&mut self) {
        self.stream = None;
        self.sent = None;
        self.out.clear();
        self.inbuf.clear();
        self.next_try = Some(Instant::now() + RETRY);
    }
}

impl Drop for Link {
    /// best effort: tell the companion Elisa is gone (it shows Shade asleep)
    fn drop(&mut self) {
        if self.stream.is_some() {
            self.queue(&bye());
            self.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// a socket path in a folder of the test's own (the folder goes with the returned guard)
    fn tmp_sock(tag: &str) -> (crate::testdir::TempDir, PathBuf) {
        let d = crate::testdir::TempDir::new(&format!("link-{tag}"));
        let p = d.join("c.sock");
        (d, p)
    }

    fn frame(st: St) -> Frame { Frame { state: st, verb: super::super::verb(st).into(), target: "split.rs".into(), tone: Tone::Accent, pop: st.pops(), light: false, fidget: false } }

    /// read lines from the companion side until `n` arrive (or 2 s pass)
    fn lines(r: &mut std::io::BufReader<UnixStream>, n: usize) -> Vec<String> {
        use std::io::BufRead;
        // macOS: EINVAL once the peer closed (the bye check); the data is still readable
        let _ = r.get_ref().set_read_timeout(Some(Duration::from_secs(2)));
        (0..n).map_while(|_| { let mut s = String::new(); r.read_line(&mut s).ok().filter(|&k| k > 0).map(|_| s) }).collect()
    }

    #[test]
    fn encode_decode_round_trip() {
        for st in St::ALL {
            for (tone, light) in [(Tone::Accent, false), (Tone::Warn, true), (Tone::Error, false)] {
                let f = Frame { tone, light, ..frame(st) };
                let line = encode(&f);
                assert!(line.ends_with('\n') && line.matches('\n').count() == 1, "one line: {line}");
                assert_eq!(decode(&line), Some(f), "{line}");
            }
        }
        let j: Value = serde_json::from_str(&encode(&Frame { tone: Tone::Warn, ..frame(St::Permission) })).unwrap();
        assert_eq!(j, json!({"v": 1, "type": "state", "state": "permission", "verb": "NEEDS YOU", "target": "split.rs", "tone": "warn", "pop": true, "theme": "dark", "fidget": false}));
        // a target with quotes, a newline and non-ASCII stays one line and round-trips
        let f = Frame { target: "say \"hi\"\nnext — ünïcode".into(), ..frame(St::Running) };
        assert_eq!(decode(&encode(&f)), Some(f));
        assert_eq!(decode("{\"v\":1,\"type\":\"state\",\"state\":\"nope\"}"), None);
        assert_eq!(decode("{\"v\":2,\"type\":\"state\",\"state\":\"idle\"}"), None, "other protocol versions are ignored");
        assert_eq!(decode("not json"), None);
    }

    #[test]
    fn care_lines() {
        for c in Care::ALL { assert_eq!(decode_care(&encode_care(c)), Some(c)); }
        assert_eq!(decode_care(r#"{"v":1,"type":"care","action":"water"}"#), Some(Care::Water));
        assert_eq!(decode_care(r#"{"v":1,"type":"care","action":"pizza"}"#), None);
        assert_eq!(decode_care(r#"{"v":1,"type":"state","state":"idle"}"#), None);
        assert_eq!(decode_care("garbage"), None);
        assert_eq!([Care::Poke, Care::Water, Care::Food, Care::Toy].map(|c| super::super::state_for(c.event())),
            [St::Poked, St::Drinking, St::Eating, St::Playing]);
    }

    #[test]
    fn socket_path_rule() {
        assert_eq!(socket_path_from(Some("/x/y.sock".into()), Some("/h".into())), PathBuf::from("/x/y.sock"));
        assert_eq!(socket_path_from(Some(String::new()), Some("/h".into())), PathBuf::from("/h/.elisa/companion.sock"));
        assert_eq!(socket_path_from(None, Some("/h".into())), PathBuf::from("/h/.elisa/companion.sock"));
    }

    #[test]
    fn not_running_is_silent_and_fast() {
        let (_d, p) = tmp_sock("absent");
        let mut l = Link::new(&p);
        let mut c = Companion::new(0, None);
        let t = Instant::now();
        for i in 0..200 { let v = l.pump(&mut c, i * 16, false); assert_eq!(v, c.view(i * 16)); }
        assert!(!l.connected());
        assert!(t.elapsed() < Duration::from_millis(500), "200 frames with no companion: {:?}", t.elapsed());
        // a stale socket file with no listener behaves the same
        drop(UnixListener::bind(&p).unwrap());
        let mut l = Link::new(&p);
        assert!(l.sync(Some(&frame(St::Idle))).is_empty() && !l.connected());
    }

    #[test]
    fn talks_to_a_companion_and_reconnects() {
        let (_d, p) = tmp_sock("live");
        let srv = UnixListener::bind(&p).unwrap();
        let mut l = Link::new(&p);
        let mut c = Companion::new(0, None);
        c.on(0, Event::Launch);
        l.pump(&mut c, 10, false);
        assert!(l.connected());
        let (s, _) = srv.accept().unwrap();
        let mut r = std::io::BufReader::new(s.try_clone().unwrap());
        let got = lines(&mut r, 2);
        assert!(got[0].contains("\"type\":\"hello\"") && got[0].contains("\"app\":\"elisa\""), "{got:?}");
        assert_eq!(decode(&got[1]).map(|f| (f.state, f.verb, f.pop)), Some((St::Greeting, "HI!".into(), true)));
        // unchanged frames are not re-sent; a change is
        for i in 0..5 { l.pump(&mut c, 20 + i, false); }
        c.on(30, Event::Tool("edit", "/a/b/split.rs"));
        l.pump(&mut c, 3000, false);
        let got = lines(&mut r, 1);
        assert_eq!(decode(&got[0]).map(|f| (f.state, f.target)), Some((St::Editing, "/a/b/split.rs".into())));
        // a care click from the window is applied at once and echoed back
        (&s).write_all(encode_care(Care::Toy).as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let v = l.pump(&mut c, 3100, false);
        assert_eq!(v.st, St::Playing);
        assert_eq!(decode(&lines(&mut r, 1)[0]).map(|f| f.state), Some(St::Playing));
        // the window goes away: the link notices, then finds a new one (state re-sent from scratch)
        drop(r);
        drop(s);
        for i in 0..3 { l.pump(&mut c, 3200 + i, false); std::thread::sleep(Duration::from_millis(20)); }
        assert!(!l.connected());
        l.next_try = None;
        l.pump(&mut c, 3300, false);
        assert!(l.connected());
        let (s2, _) = srv.accept().unwrap();
        let mut r2 = std::io::BufReader::new(s2);
        let got = lines(&mut r2, 2);
        assert!(got[0].contains("hello") && decode(&got[1]).is_some(), "{got:?}");
        // bye on drop
        drop(l);
        assert!(lines(&mut r2, 1)[0].contains("\"type\":\"bye\""));
    }

    #[test]
    fn a_stalled_companion_never_blocks() {
        let (_d, p) = tmp_sock("stall");
        let srv = UnixListener::bind(&p).unwrap();
        let mut l = Link::new(&p);
        l.sync(None);
        let (_s, _) = srv.accept().unwrap(); // accepted, never read
        let t = Instant::now();
        for i in 0..20_000u64 {
            let f = Frame { target: format!("file-{i}.rs {}", "x".repeat(200)), ..frame(St::Editing) };
            l.sync(Some(&f));
        }
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert!(l.out.len() <= OUT_CAP + 512, "bounded queue: {}", l.out.len());
        let _ = std::fs::remove_file(&p);
    }
}
