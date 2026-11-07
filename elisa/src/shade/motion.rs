//! Shade's 24 states (20 + the v2 care states) and their keyframe tracks (brand kit 07-motion + 05-character/shade/motion), evaluated to a pose.
//! Same semantics as the kit's renderer (`elisa-character.js` `val`/`pose`): a segment eases with its START key's
//! ease; expression (eye) keys hold until the next key; missing channels keep the default pose.
use super::tables as tb;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum St {
    Idle, Listening, Thinking, Streaming, Running, Permission, Success, Error, Interrupted, Memory, Sleeping, Waking,
    Greeting, Reading, Searching, Editing, Building, Testing, Celebrating, Confused,
    // care (kit v2): the user pokes Elisa, gives water, food or a toy
    Poked, Drinking, Eating, Playing,
}

impl St {
    pub const ALL: [St; 24] = [St::Idle, St::Listening, St::Thinking, St::Streaming, St::Running, St::Permission, St::Success,
        St::Error, St::Interrupted, St::Memory, St::Sleeping, St::Waking, St::Greeting, St::Reading, St::Searching, St::Editing,
        St::Building, St::Testing, St::Celebrating, St::Confused, St::Poked, St::Drinking, St::Eating, St::Playing];
    pub fn name(self) -> &'static str {
        ["idle", "listening", "thinking", "streaming", "running", "permission", "success", "error", "interrupted", "memory",
            "sleeping", "waking", "greeting", "reading", "searching", "editing", "building", "testing", "celebrating", "confused",
            "poked", "drinking", "eating", "playing"][self as usize]
    }
    pub fn parse(s: &str) -> Option<St> { St::ALL.into_iter().find(|x| x.name() == s.trim().to_ascii_lowercase()) }
    fn table(self) -> (u32, &'static [Track]) {
        match self {
            St::Idle => tb::IDLE, St::Listening => tb::LISTENING, St::Thinking => tb::THINKING, St::Streaming => tb::STREAMING,
            St::Running => tb::RUNNING, St::Permission => tb::PERMISSION, St::Success => tb::SUCCESS, St::Error => tb::ERROR,
            St::Interrupted => tb::INTERRUPTED, St::Memory => tb::MEMORY, St::Sleeping => tb::SLEEPING, St::Waking => tb::WAKING,
            St::Greeting => tb::GREETING, St::Reading => tb::READING, St::Searching => tb::SEARCHING, St::Editing => tb::EDITING,
            St::Building => tb::BUILDING, St::Testing => tb::TESTING, St::Celebrating => tb::CELEBRATING, St::Confused => tb::CONFUSED,
            St::Poked => tb::POKED, St::Drinking => tb::DRINKING, St::Eating => tb::EATING, St::Playing => tb::PLAYING,
        }
    }
    /// loop length (kit `durationMs`)
    pub fn dur_ms(self) -> u32 { self.table().0 }
    /// the kit's attention pop (07-motion/state-mapping.md v2, prototype `BIG`): docked companion scales 1.22x + bubble lifts
    pub fn pops(self) -> bool {
        matches!(self, St::Greeting | St::Permission | St::Celebrating | St::Confused | St::Error | St::Waking | St::Poked | St::Playing)
    }
    /// a care state (kit v2): started by the user, not by the agent
    pub fn care(self) -> bool { matches!(self, St::Poked | St::Drinking | St::Eating | St::Playing) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease { Io, Out, In, Back, Lin, Step }
impl Ease {
    /// the kit's easing functions (elisa-character.js `EASE`)
    pub fn f(self, t: f32) -> f32 {
        match self {
            Ease::Lin => t,
            Ease::Io => if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 },
            Ease::Out => 1.0 - (1.0 - t).powi(3),
            Ease::In => t * t * t,
            Ease::Back => { let c = 1.9; 1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2) }
            Ease::Step => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Eye { Block, Happy, Closed, Wide, Sad }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ch { X, Y, Sx, Sy, Rot, EarL, EarR, EyeOpen, LookX, LookY, Tail, PawR, Glow, Dim, P1, P2, P3, Eye }

#[derive(Clone, Copy, Debug)]
pub enum V { N(f32), E(Eye) }
#[derive(Clone, Copy, Debug)]
pub struct Key { pub t: f32, pub v: V, pub e: Ease }
pub struct Track { pub ch: Ch, pub keys: &'static [Key] }
pub const fn k(t: f32, v: f32, e: Ease) -> Key { Key { t, v: V::N(v), e } }
pub const fn ke(t: f32, v: Eye, e: Ease) -> Key { Key { t, v: V::E(v), e } }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub x: f32, pub y: f32, pub sx: f32, pub sy: f32, pub rot: f32, pub ear_l: f32, pub ear_r: f32, pub eye_open: f32,
    pub look_x: f32, pub look_y: f32, pub eye: Eye, pub tail: f32, pub paw_r: f32, pub glow: f32, pub dim: f32,
    pub p1: f32, pub p2: f32, pub p3: f32,
}
impl Default for Pose {
    fn default() -> Self {
        Pose { x: 0.0, y: 0.0, sx: 1.0, sy: 1.0, rot: 0.0, ear_l: 0.0, ear_r: 0.0, eye_open: 1.0, look_x: 0.0, look_y: 0.0,
            eye: Eye::Block, tail: 0.0, paw_r: 0.0, glow: 0.0, dim: 0.0, p1: 0.0, p2: 0.0, p3: 0.0 }
    }
}

fn val(keys: &[Key], t: f32) -> V {
    if keys.len() == 1 || t <= keys[0].t { return keys[0].v; }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.t {
            let (V::N(av), V::N(bv)) = (a.v, b.v) else { return a.v };
            let span = b.t - a.t;
            let u = (t - a.t) / if span == 0.0 { 1.0 } else { span };
            return V::N(av + (bv - av) * a.e.f(u));
        }
    }
    keys[keys.len() - 1].v
}

/// the pose of `st` at loop phase `u` (0..1)
pub fn pose(st: St, u: f32) -> Pose {
    let u = u.rem_euclid(1.0);
    let mut p = Pose::default();
    for tr in st.table().1 {
        match (tr.ch, val(tr.keys, u)) {
            (Ch::Eye, V::E(e)) => p.eye = e,
            (ch, V::N(v)) => *match ch {
                Ch::X => &mut p.x, Ch::Y => &mut p.y, Ch::Sx => &mut p.sx, Ch::Sy => &mut p.sy, Ch::Rot => &mut p.rot,
                Ch::EarL => &mut p.ear_l, Ch::EarR => &mut p.ear_r, Ch::EyeOpen => &mut p.eye_open, Ch::LookX => &mut p.look_x,
                Ch::LookY => &mut p.look_y, Ch::Tail => &mut p.tail, Ch::PawR => &mut p.paw_r, Ch::Glow => &mut p.glow,
                Ch::Dim => &mut p.dim, Ch::P1 => &mut p.p1, Ch::P2 => &mut p.p2, Ch::P3 => &mut p.p3, Ch::Eye => continue,
            } = v,
            _ => {}
        }
    }
    p
}

/// CSS cubic-bezier(x1,y1,x2,y2) at time x (0..1): the kit's attention spring is cubic-bezier(.34,1.7,.64,1) 350 ms
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let c = |a: f32, b: f32, t: f32| 3.0 * a * t * (1.0 - t) * (1.0 - t) + 3.0 * b * t * t * (1.0 - t) + t * t * t;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let m = (lo + hi) / 2.0;
        if c(x1, x2, m) < x { lo = m } else { hi = m }
    }
    c(y1, y2, (lo + hi) / 2.0)
}
