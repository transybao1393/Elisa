//! Full-screen terminal UI (ratatui + crossterm). The UI owns the main thread; `repl::run_session` runs on a
//! worker thread against a `ChannelFrontend` that forwards every call over mpsc (blocking calls wait on a reply).
use crate::frontend::*;
use crate::ui::k;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};
use ratatui::{prelude::*, widgets::*};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::{Duration, Instant};
use crate::shade::{Art, Companion, Event as ShEv, Look, St, View as ShView};

// ---------- palette: the brand's dark theme (02-color); `brand::finish` maps it to light / 256 colours per frame ----------
use crate::brand::{c as bc, DARK as P};

mod activity;
mod app;
mod draw;
mod dump;
mod frontend;
mod markdown;

pub(crate) use activity::*;
use app::*;
use draw::*;
use dump::*;
pub use frontend::*;
use markdown::*;

/// body text
const FG: Color = bc(P.text);
/// titles, headlines, the user's turns
const TITLE: Color = bc(P.fg);
/// secondary text: the dimmest text Elisa draws
const DIM: Color = bc(P.muted);
const ACCENT: Color = bc(P.accent);
const CODE: Color = bc(P.code);
const GREEN: Color = bc(P.ok);
const RED: Color = bc(P.error);
const YELLOW: Color = bc(P.warn);
const SEL_BG: Color = bc(P.sel);
const CODE_BG: Color = bc(P.bg2);
const BG: Color = bc(P.bg);
const BG2: Color = bc(P.bg2);
const H3: Color = bc(P.fg);
const GUTTER: Color = bc(P.muted);
const CTX: Color = bc(P.text);
const SEP: Color = bc(P.sep);

fn st(c: Color) -> Style { Style::new().fg(c) }

// ---------- app state ----------
enum Entry { Recalled(Vec<String>), Foot(Status), Banner(Banner), User(String), Text(String), Tool(Box<Act>), Info(String), Err(String), Notes(String), Card(String, bool), Watch(String), WatchView(Box<WatcherView>), Full(String, String) }
impl Entry {
    /// rough heap bytes, for the transcript cap
    fn bytes(&self) -> usize {
        64 + match self {
            Entry::Recalled(h) => h.iter().map(String::len).sum(),
            Entry::Foot(_) => 0,
            Entry::Full(a, b) => a.len() + b.len(),
            Entry::Banner(b) => 256 + b.notes.as_ref().map_or(0, String::len),
            Entry::User(t) | Entry::Text(t) | Entry::Info(t) | Entry::Err(t) | Entry::Notes(t) | Entry::Card(t, _) | Entry::Watch(t) => t.len(),
            Entry::WatchView(v) => 512 + v.plan.per_layer_cold.len() * 8,
            Entry::Tool(a) => 128 + a.target.len() + a.end.as_ref().map_or(0, |e| e.note.len() + e.prev.iter().map(String::len).sum::<usize>()),
        }
    }
}
/// a transcript entry plus its render cache: height at width `w` (0 = stale) and the lines, kept only near the view
struct Item { e: Entry, w: usize, h: usize, lines: Option<Vec<Line<'static>>> }
impl Item {
    pub(super) fn new(e: Entry) -> Self { Item { e, w: 0, h: 0, lines: None } }
    /// animated (running tool): re-rendered every frame
    fn live(&self) -> bool { matches!(&self.e, Entry::Tool(a) if a.end.is_none()) }
}
/// one background job under the input, keyed by the first word of its label
struct Job { key: String, p: Progress, at: Instant }
enum Pending { Input(Sender<Option<String>>), Approve(String, String, usize, Sender<Approval>), Pick(Vec<SessionInfo>, usize, Sender<Option<usize>>) }
#[derive(PartialEq)]
enum Phase { Idle, Thinking, Streaming, Tool }

struct App {
    entries: Vec<Item>, dropped: bool, new_text: bool,
    input: Vec<char>, cur: usize, hist: Vec<String>, hist_i: Option<usize>, draft: Vec<char>,
    menu_sel: usize, menu_off: bool, queue: VecDeque<String>,
    pending: Option<Pending>, phase: Phase, since: Instant,
    status: Option<Status>, model: String, max_ctx: usize, yolo: bool,
    first_delta: Option<Instant>, dchars: usize, sum_in: usize, sum_out: usize,
    scroll: usize, last_total: usize, view_h: usize,
    quit_at: Option<Instant>, flash: Option<(String, Instant)>, code: Option<i32>,
    // edit panel
    views: Vec<View>, sel: Option<usize>, last_read: Option<ReadV>, last_cmd: Option<(String, Option<bool>)>, awaiting: Await,
    pscroll: usize, panel_h: usize, split: f32, panel_pref: Option<bool>, term_w: u16,
    main_r: Rect,
    // memory job under the input: the last update and when it arrived; `memory` = any memory event seen
    jobs: Vec<Job>, memory: bool,
    // self-check: own resident size, refreshed at most once per second
    rss: u64, rss_at: Option<Instant>,
    // state cards: the stream filter hides <state>…</state> (`hold` = a possible partial tag at the end of the stream)
    cardf: crate::context::CardFilter, cards: usize,
    // the gateway's domain watcher (None = no view yet, or an older gateway): one line under the status bar
    watch: Option<WatcherView>,
    /// the latest audit result, shown on its own line under the input box (not in the transcript)
    audit: Option<String>,
    /// the companion (None = off); its clock; where its art was drawn this frame (kept out of the theme remap);
    /// the last rendered frame (re-used while the frame index is unchanged); this frame's view
    shade: Option<Companion>, clock: Instant, dock: Option<Rect>, art: Option<(ArtKey, Art)>, view: Option<ShView>,
    /// the care tray's buttons (kit v2 layout.json `careTray`): where they were drawn, for clicks
    tray: Vec<(Rect, St)>,
    /// where the status-bar face was drawn (None: one-line text status)
    face: Option<Rect>,
    /// the companion window link (--companion / config `companion`; None = off): Shade mirrored to app/ElisaCompanion
    link: Option<crate::shade::link::Link>,
    /// the status line's live text (src/activity.rs): the shown step and when it (or its words) came; a new tool step
    /// waits in `act_next` until it has run `activity::QUICK_MS` (a quicker one never takes the line)
    act: Option<(crate::activity::Activity, Instant)>,
    act_next: Option<(crate::activity::Activity, Instant)>,
}

type ArtKey = (St, u64, usize, usize, i32, bool);

// ---------- action entries (one per tool call) ----------
#[derive(Clone, Copy)]
enum Info { None, Write(bool, usize), Edit(usize, usize, bool) }
struct Act { name: String, verb: String, target: String, info: Info, t0: Instant, wait: bool, end: Option<End> }
struct End { note: String, red: bool, prev: Vec<String>, more: usize }
