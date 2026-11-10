use super::*;

/// /resume lists at most this many sessions (newest first)
const LIST_MAX: usize = 50;
/// bytes read from the head of an old session file (no .meta) to find its title
const HEAD_BYTES: u64 = 8192;

static RUN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn unix_now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) }

/// Per-session id so every save of one session goes to the same session file (set at the first use; /clear starts
/// a new one with `new_session`).
pub fn run_id() -> u64 {
    use std::sync::atomic::Ordering;
    let id = RUN_ID.load(Ordering::SeqCst);
    if id != 0 { return id; }
    let _ = RUN_ID.compare_exchange(0, unix_now().max(1), Ordering::SeqCst, Ordering::SeqCst);
    RUN_ID.load(Ordering::SeqCst)
}

/// A new session file from now on (/clear): the one saved so far stays as it is, so /resume can go back to it.
pub fn new_session() {
    use std::sync::atomic::Ordering;
    let next = unix_now().max(run_id() + 1);
    RUN_ID.store(next, Ordering::SeqCst);
}

/// This run's session id: the session file is `.elisa/sessions/<id>.json`.
pub fn session_id() -> String { run_id().to_string() }

fn non_empty(s: String) -> Option<String> { if s.trim().is_empty() { None } else { Some(s) } }

/// Project-local state under <cwd>/.elisa/
pub struct Store { pub dir: PathBuf }
impl Store {
    pub fn new(cwd: &Path) -> Store { Store { dir: cwd.join(".elisa") } }

    /// This session's notes: `.elisa/notes/<session id>.md`, one file per session, so a new session never overwrites
    /// or loses an earlier one's (one shared `notes.md` was carried into every new session, and a small model could
    /// answer "hey" with the earlier session's task).
    fn notes_path(&self) -> PathBuf { self.notes_path_of(run_id()) }
    fn notes_path_of(&self, id: u64) -> PathBuf { self.dir.join("notes").join(format!("{id}.md")) }
    fn allow_path(&self) -> PathBuf { self.dir.join("allow") }

    /// Tools the user chose "always allow" for in this project (`.elisa/allow`, one name per line).
    pub fn load_allowed(&self) -> HashSet<String> {
        fs::read_to_string(self.allow_path()).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    }
    /// Save the always-allowed tools (empty set removes the file).
    pub fn save_allowed(&self, tools: &HashSet<String>) -> std::io::Result<()> {
        if tools.is_empty() { return match fs::remove_file(self.allow_path()) { Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e), _ => Ok(()) } }
        fs::create_dir_all(&self.dir)?;
        let mut v: Vec<&str> = tools.iter().map(String::as_str).collect();
        v.sort_unstable();
        fs::write(self.allow_path(), v.join("\n") + "\n")
    }
    fn sessions_dir(&self) -> PathBuf { self.dir.join("sessions") }

    pub fn load_project(&self) -> Option<String> {
        let root = self.dir.parent()?;
        fs::read_to_string(root.join("ELISA.md")).ok().and_then(non_empty)
    }

    /// This session's own notes (None for a new session: it starts clean).
    pub fn load_notes(&self) -> Option<String> { fs::read_to_string(self.notes_path()).ok().and_then(non_empty) }

    /// The notes of saved session `id` (for `--continue` / `/resume`): only that session's own
    /// `.elisa/notes/<id>.md`. The shared `.elisa/notes.md` of Elisa before per-session notes is never read
    /// (otherwise it comes back with `--continue`, and an unrelated old task's facts get
    /// merged forward into the conversation's notes by every fold).
    pub fn session_notes(&self, id: u64) -> Option<String> {
        fs::read_to_string(self.notes_path_of(id)).ok().and_then(non_empty)
    }

    /// Saves this session's notes (`.elisa/notes/<session id>.md`); earlier sessions' notes are not touched.
    pub fn save_notes(&self, notes: &str) -> Result<(), String> {
        let p = self.notes_path();
        let dir = p.parent().unwrap_or(&self.dir);
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // on disk: never a secret, whatever the `redact` setting
        let (notes, _) = crate::secrets::redact(notes.trim());
        fs::write(&p, format!("{notes}\n")).map_err(|e| format!("{}: {e}", p.display()))
    }

    pub fn clear_notes(&self) -> Result<(), String> {
        let p = self.notes_path();
        match fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
            _ => Ok(()),
        }
    }

    pub fn save_session(&self, msgs: &[Message]) -> Result<PathBuf, String> {
        let dir = self.sessions_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let p = dir.join(format!("{}.json", run_id()));
        // the title below comes from the redacted copy too (so a password typed in the prompt never reaches the .meta)
        let mut msgs = redact_messages(msgs);
        // a turn's situation line / follow-up hint is that request's only: the saved user words go without it
        for m in msgs.iter_mut().filter(|m| m.role == Role::User) {
            for b in m.content.iter_mut() { if let Block::Text { text } = b { if let Some(t) = strip_turn_lines(text) { *text = t; } } }
        }
        let msgs = &msgs[..];
        let json = serde_json::to_string(msgs).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, &p)).map_err(|e| format!("{}: {e}", p.display()))?;
        // tiny sidecar (.elisa/session-meta/<ts>.meta) so /resume never loads whole sessions: "<n_msgs>\t<title>"
        let title = msgs.iter().filter(|m| m.role == Role::User).find_map(|m| m.content.iter().find_map(|b| match b {
            Block::Text { text } => Some(session_title(text)),
            _ => None,
        })).unwrap_or_default();
        let meta = self.dir.join("session-meta");
        if fs::create_dir_all(&meta).is_ok() { let _ = fs::write(meta.join(format!("{}.meta", run_id())), format!("{}\t{title}", msgs.len())); }
        Ok(p)
    }

    /// The newest earlier session with real content (`real_content`), up to 20 back; a file over 2 MB counts as
    /// real without being read. A terminal `--continue` and the explicit "continue the last one" use it, so a dead
    /// session ("continue the last one" → the generic intro) never shadows the real one under it. `/resume` lists
    /// everything.
    pub fn newest_real_session(&self) -> Option<(u64, PathBuf)> {
        let own = run_id();
        let mut v: Vec<(u64, PathBuf)> = fs::read_dir(self.sessions_dir()).ok()?.filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "json" { return None; }
            let ts: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, p))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.into_iter().take(20).find(|(_, p)| {
            if fs::metadata(p).is_ok_and(|m| m.len() > 2_000_000) { return true; }
            self.load_session(p).is_some_and(|h| real_content(&h))
        })
    }

    /// Newest saved session from an earlier run (this run's own file is skipped).
    pub fn load_last_session(&self) -> Option<Vec<Message>> { self.load_last_session_with_notes().map(|(h, _)| h) }

    /// Newest saved session from an earlier run with its own notes (`--continue`).
    pub fn load_last_session_with_notes(&self) -> Option<(Vec<Message>, Option<String>)> {
        let (id, p) = self.newest_real_session()?;
        let h = serde_json::from_str(&fs::read_to_string(p).ok()?).ok()?;
        Some((h, self.session_notes(id)))
    }

    /// The newest earlier session with any message, up to 20 back: the conversation that was open last, whatever was
    /// said in it. `--continue-open` takes it: the app's restart of the open conversation (a model switch, a
    /// reconnect, a dev reload), even a one-turn "hi" chat that is not `real_content`, so a switch never says
    /// "no saved session to continue".
    pub fn newest_session(&self) -> Option<(u64, PathBuf)> {
        let own = run_id();
        let mut v: Vec<(u64, PathBuf)> = fs::read_dir(self.sessions_dir()).ok()?.filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "json" { return None; }
            let ts: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, p))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.into_iter().take(20).find(|(_, p)| fs::metadata(p).is_ok_and(|m| m.len() > 2_000_000) || self.load_session(p).is_some_and(|h| !h.is_empty()))
    }

    /// Saved sessions from earlier runs, newest first, at most LIST_MAX (this run's own file is skipped). Built from file
    /// names plus the tiny `.meta` sidecars; never loads a whole session (old files without one: title from the
    /// first HEAD_BYTES, n_msgs 0).
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let own = run_id();
        let Ok(rd) = fs::read_dir(self.sessions_dir()) else { return Vec::new() };
        let mut v: Vec<(u64, PathBuf)> = rd.filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension()? != "json" { return None; }
            let ts: u64 = path.file_stem()?.to_str()?.parse().ok()?;
            (ts != own).then_some((ts, path))
        }).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.0));
        v.truncate(LIST_MAX);
        v.into_iter().map(|(ts, path)| {
            let (n_msgs, title) = match fs::read_to_string(self.dir.join(format!("session-meta/{ts}.meta"))) {
                Ok(m) => {
                    let (n, t) = m.split_once('\t').unwrap_or((&m, ""));
                    (n.trim().parse().unwrap_or(0), t.to_string())
                }
                Err(_) => (0, head_title(&path)),
            };
            SessionInfo { path, ts, n_msgs, title }
        }).collect()
    }

    pub fn load_session(&self, path: &Path) -> Option<Vec<Message>> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    }

    /// The notes of the saved session at `path` (`/resume`).
    pub fn load_session_notes(&self, path: &Path) -> Option<String> {
        self.session_notes(path.file_stem()?.to_str()?.parse().ok()?)
    }

    fn card_reset_path(&self) -> PathBuf { self.dir.join("card-reset") }

    /// /reset and /forget dropped the state card: cards up to `id` (elisa-mem ids) are not carried into a new session.
    pub fn save_card_reset(&self, id: u64) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.card_reset_path(), format!("{id}\n")).map_err(|e| format!("{}: {e}", self.card_reset_path().display()))
    }

    /// The newest card id a /reset or /forget dropped (None = never).
    pub fn card_reset(&self) -> Option<u64> { fs::read_to_string(self.card_reset_path()).ok()?.trim().parse().ok() }

    fn forgot_path(&self) -> PathBuf { self.dir.join("memory-forgotten") }

    /// /forget: this project's memories (elisa-mem ids) below `next` are never recalled again (the store is
    /// append-only; the saved conversation files are kept).
    pub fn save_forgot(&self, next: u64) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        fs::write(self.forgot_path(), format!("{next}\n")).map_err(|e| format!("{}: {e}", self.forgot_path().display()))
    }

    /// The memory id the last /forget stopped at: this project's memories below it are forgotten (None = never).
    pub fn forgot(&self) -> Option<u64> { fs::read_to_string(self.forgot_path()).ok()?.trim().parse().ok() }

    fn cards_path_of(&self, id: u64) -> PathBuf { self.dir.join("session-meta").join(format!("{id}.cards.json")) }

    /// This session's state cards with their topics (`topics.rs`), beside its `.meta`: what `--continue` / `/resume`
    /// load (nothing is sent until a message picks one). None written while there is none.
    pub fn save_cards(&self, cards: &[crate::topics::Card]) {
        if cards.is_empty() { return; }
        let p = self.cards_path_of(run_id());
        let redacted: Vec<crate::topics::Card> = cards.iter().map(|c| crate::topics::Card { text: crate::secrets::redact(&c.text).0, ..c.clone() }).collect();
        if let (Some(dir), Ok(json)) = (p.parent(), serde_json::to_string(&redacted)) {
            if fs::create_dir_all(dir).is_ok() { let _ = fs::write(&p, json); }
        }
    }

    /// The state cards saved with session `id` (`save_cards`); None for an older session without them.
    pub fn load_cards(&self, id: u64) -> Option<Vec<crate::topics::Card>> {
        serde_json::from_str(&fs::read_to_string(self.cards_path_of(id)).ok()?).ok()
    }
}

/// Title of a session file without a `.meta`: the first `"text":"…"` string in its first HEAD_BYTES.
fn head_title(path: &Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if fs::File::open(path).and_then(|f| f.take(HEAD_BYTES).read_to_end(&mut buf)).is_err() { return String::new(); }
    let s = String::from_utf8_lossy(&buf);
    let Some(i) = s.find("\"text\":\"") else { return String::new() };
    let rest = &s[i + 7..];
    // the JSON string up to its closing quote (or the cut), decoded by serde
    let mut end = rest.len();
    let mut esc = false;
    for (j, c) in rest.char_indices().skip(1) {
        if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { end = j + 1; break; }
    }
    let lit = if end == rest.len() { format!("{}\"", rest.trim_end_matches('\\')) } else { rest[..end].to_string() };
    serde_json::from_str::<String>(&lit).map(|t| session_title(&t)).unwrap_or_default()
}

/// The user's own words of a first message (without the `first_message` header), one line, <= 80 chars.
fn session_title(text: &str) -> String { one_line(request_body(text), 79) }
