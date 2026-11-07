//! Shade drawn with the kit's own geometry: a port of `05-character/renderer/elisa-shade.js` (`svg`, `figure`, `eye`,
//! `heldProps`, `freeProps`, kit v2 incl. the care states and the status-bar `faceSvg`) and the core-state props of `elisa-character.js`. Coordinates are the kit's 200 x 200
//! artboard; `base` maps the artboard to device pixels. One terminal-resolution change: outline strokes are at least
//! `MIN_RIM` art pixels wide, so the ink-black body keeps its rim when the artboard is ~30 pixels across.
use super::motion::{pose, Eye, St};
use super::raster::{hex, mix, Canvas, Path, Rgb, Tf};
use std::f32::consts::PI;

pub const BODY: &str = "M50 132C46 92 66 66 100 66C134 66 154 92 150 132C154 154 142 172 120 172H80C58 172 46 154 50 132Z";
const EAR: &str = "M-15 10C-17 -20 -8 -44 -2 -46C4 -46 10 -20 15 4Z";
const EAR_IN: &str = "M-8 4C-9 -16 -5 -34 -2 -36C1 -36 5 -16 8 2Z";
const EY: f32 = 112.0;
const EX: [f32; 2] = [75.0, 125.0];
const RX: f32 = 21.0;
const BASE_LOOK_Y: f32 = -3.0;
/// minimum outline width in device pixels (see the module doc)
const MIN_RIM: f32 = 1.0;

struct C;
impl C {
    // 02-color/colors.json "character" + the renderer's COL
    fn body() -> Rgb { hex("#141418") }
    fn rim() -> Rgb { hex("#3A3A46") }
    fn mini() -> Rgb { hex("#3A3A46") }
    fn paw() -> Rgb { hex("#1C1C22") }
    fn ear_in() -> Rgb { hex("#82C8DC") }
    fn white() -> Rgb { hex("#EEF0E6") }
    fn pupil() -> Rgb { hex("#0E0E12") }
    fn ink() -> Rgb { hex("#1E1E24") }
    fn cyan() -> Rgb { hex("#82C8DC") }
    fn amber() -> Rgb { hex("#E6BE50") }
    fn coral() -> Rgb { hex("#D77757") }
    fn green() -> Rgb { hex("#78C878") }
    fn pink() -> Rgb { hex("#FF8FB1") }
    fn paper() -> Rgb { hex("#EEF0E6") }
    fn line() -> Rgb { hex("#3A3A46") }
    fn red() -> Rgb { hex("#F06464") }
    fn sec() -> Rgb { hex("#9696A0") }
    fn eye_core() -> Rgb { hex("#E8E8E8") }
}

/// a drawing context: canvas + current transform + group opacity
struct D<'a> { c: &'a mut Canvas, t: Tf, op: f32 }
impl D<'_> {
    fn fill(&mut self, p: &Path, rgb: Rgb, op: f32) { self.c.fill(p, &self.t, rgb, op * self.op) }
    fn stroke(&mut self, p: &Path, rgb: Rgb, w: f32, op: f32) {
        let s = self.t.scale().max(1e-6);
        let k = self.c.scale;
        self.c.stroke(p, &self.t, rgb, w.max(k / s), op * self.op)
    }
    /// a stroke at its true width (hairlines that must not grow at low resolution)
    fn hair(&mut self, p: &Path, rgb: Rgb, w: f32, op: f32) { self.c.stroke(p, &self.t, rgb, w, op * self.op) }
    fn rim(&mut self, p: &Path, rgb: Rgb, w: f32) {
        let s = self.t.scale().max(1e-6);
        let k = self.c.scale;
        self.c.stroke(p, &self.t, rgb, w.max(MIN_RIM * k / s), self.op)
    }
    fn with(&mut self, t: Tf, f: impl FnOnce(&mut D)) {
        let old = self.t;
        self.t = old.then(t);
        f(self);
        self.t = old;
    }
}

/// Shade in state `st` at loop phase `u` (0..1). `mini` = the ≤ 24 px build (no rim, paws or props, motion ×0.45);
/// `props` = draw props/effects (the dock does).
pub fn shade(c: &mut Canvas, base: &Tf, st: St, u: f32, mini: bool, props: bool) {
    let u = u.rem_euclid(1.0);
    let ms = u * st.dur_ms() as f32;
    let p = pose(st, u);
    let yk = if mini { 0.45 } else { 1.0 };
    let tr = Tf::tr(100.0 + p.x * yk, 172.0 + p.y * yk).then(Tf::rot(p.rot)).then(Tf::sc(p.sx, p.sy)).then(Tf::tr(-100.0, -172.0));
    let lift = (-p.y).max(0.0);
    let c_crisp = c.crisp;
    let mut d = D { c, t: *base, op: 1.0 };
    if !mini {
        let rx = 48.0 * p.sx * (1.0 - lift / 70.0);
        d.fill(&Path::ellipse(100.0 + p.x, 176.0, rx.max(0.0), 5.0), (0, 0, 0), 0.32 * (1.0 - lift / 50.0).max(0.0));
    }
    let gop = if c_crisp { 1.0 } else { 1.0 - p.dim * 0.35 };
    d.op = gop;
    d.with(tr, |d| {
        figure(d, st, &p, mini);
        if !mini { held_props(d, st, &p, ms); }
    });
    d.op = 1.0;
    if !mini && props {
        match st {
            St::Greeting | St::Reading | St::Searching | St::Editing | St::Building | St::Testing | St::Celebrating | St::Confused
            | St::Poked | St::Drinking | St::Eating | St::Playing => free_props(&mut d, st, &p, ms),
            _ => core_props(&mut d, st, &p, ms),
        }
    }
}

/// the status-bar face (kit v2 `faceSvg`): ears and eyes only, no body. `bg` is the window background: the sad
/// lid is filled with it, so the face sits on any theme (the kit's dark / light variants differ only there).
pub fn face(c: &mut Canvas, base: &Tf, st: St, u: f32, bg: Rgb) {
    let p = pose(st, u.rem_euclid(1.0));
    let tr = Tf::tr(100.0 + p.x * 0.4, 112.0 + p.y * 0.3).then(Tf::rot(p.rot * 0.8)).then(Tf::tr(-100.0, -112.0));
    let op = if c.crisp { 1.0 } else { 1.0 - p.dim * 0.35 };
    let mut d = D { c, t: *base, op };
    d.with(tr, |d| {
        for (x, a, m) in [(74.0, -p.ear_l, 1.0), (126.0, p.ear_r, -1.0)] {
            d.with(Tf::tr(x, 80.0).then(Tf::rot(a)).then(Tf::sc(m, 1.0)), |d| {
                d.fill(&Path::svg(EAR), C::mini(), 1.0);
                d.fill(&Path::svg(EAR_IN), C::ear_in(), 1.0);
            });
        }
        if p.glow > 0.01 { d.fill(&Path::ellipse(100.0, EY, 58.0, 30.0), C::cyan(), 0.2 * p.glow); }
        for (i, x) in EX.into_iter().enumerate() { eye(d, x, i, &p, false, bg); }
    });
}

fn figure(d: &mut D, st: St, p: &super::motion::Pose, mini: bool) {
    let body = if mini { C::mini() } else { C::body() };
    for (x, a, m) in [(74.0, -p.ear_l, 1.0), (126.0, p.ear_r, -1.0)] {
        d.with(Tf::tr(x, 80.0).then(Tf::rot(a)).then(Tf::sc(m, 1.0)), |d| {
            let ear = Path::svg(EAR);
            d.fill(&ear, body, 1.0);
            if !mini { d.rim(&ear, C::rim(), 2.5); }
            d.fill(&Path::svg(EAR_IN), C::ear_in(), if mini { 1.0 } else { 0.9 });
        });
    }
    let b = Path::svg(BODY);
    d.fill(&b, body, 1.0);
    if !mini {
        d.rim(&b, C::rim(), 2.5);
        d.stroke(&Path::svg("M70 102C72 84 84 74 96 72"), (255, 255, 255), 7.0, 0.12);
    }
    if p.glow > 0.01 && !mini { d.fill(&Path::ellipse(100.0, EY, 58.0, 30.0), C::cyan(), 0.14 * p.glow); }
    for (i, x) in EX.into_iter().enumerate() { eye(d, x, i, p, mini, body); }
    if !mini {
        let both = if st == St::Celebrating { p.paw_r } else { 0.0 };
        let wv = if st == St::Greeting { p.p1 } else { 0.0 };
        let (lx, ly) = (82.0 - 18.0 * both, 170.0 - 42.0 * both);
        paw(d, lx, ly, 35.0 * both);
        let (px, py) = (118.0 + (136.0 - 118.0) * p.paw_r + 6.0 * wv, 170.0 + (128.0 - 170.0) * p.paw_r - 2.0 * wv.abs());
        paw(d, px, py, -35.0 * p.paw_r - 28.0 * wv);
    }
}

fn paw(d: &mut D, x: f32, y: f32, rot: f32) {
    d.with(Tf::rot_at(rot, x, y), |d| {
        let e = Path::ellipse(x, y, 11.0, 6.0);
        d.fill(&e, C::paw(), 1.0);
        d.rim(&e, C::rim(), 2.5);
    });
}

fn eye(d: &mut D, x: f32, i: usize, p: &super::motion::Pose, mini: bool, body: Rgb) {
    let side = if i == 1 { 1.0 } else { -1.0 };
    let white = mix(C::white(), C::cyan(), p.glow * 0.35);
    let sw = if mini { 6.0 } else { 4.5 };
    let q = |a: f32, b: f32, c: f32| format!("M{} {}Q{} {} {} {}", x - a, EY + b, x, EY + c, x + a, EY + b);
    match p.eye {
        Eye::Happy => return d.stroke(&Path::svg(&q(14.0, 5.0, -13.0)), white, sw, 1.0),
        Eye::Closed => return d.stroke(&Path::svg(&q(14.0, 0.0, 8.0)), white, sw * 0.85, 1.0),
        _ => {}
    }
    let (wide, sad) = (p.eye == Eye::Wide, p.eye == Eye::Sad);
    let rx = RX * if wide { 1.08 } else { 1.0 };
    let bl = p.eye_open.clamp(0.05, 1.15);
    if bl < 0.2 { return d.stroke(&Path::svg(&q(15.0, 0.0, 4.0)), white, sw * 0.8, 1.0); }
    let ry = rx * 1.15 * bl;
    let cl = |v: f32, a: f32| v.clamp(-a, a);
    let (lx, ly) = (cl(p.look_x * rx / 10.0, rx * 0.42), cl((p.look_y + BASE_LOOK_Y) * 0.9 + 2.0, ry * 0.4));
    let w = Path::ellipse(x, EY, rx, ry);
    d.fill(&w, white, 1.0);
    if !mini { d.hair(&w, C::ink(), 2.0, 1.0); }
    let prx = rx * if wide { 0.42 } else { 0.55 };
    let pry = ry.min(rx * 1.15) * if wide { 0.46 } else { 0.58 };
    d.fill(&Path::ellipse(x + lx, EY + ly, prx, pry), mix(C::pupil(), hex("#1E5A6A"), p.glow), 1.0);
    if !mini {
        d.fill(&Path::circle(x + lx - rx * 0.22, EY + ly - rx * 0.25, rx * if wide { 0.16 } else { 0.13 }), (255, 255, 255), 0.85);
        if p.glow > 0.01 { d.fill(&Path::circle(x + lx, EY + ly, prx * 0.5), C::cyan(), 0.8 * p.glow); }
    }
    if sad {
        let (inner, outer) = (x - side * (rx + 3.0), x + side * (rx + 3.0));
        let lid = Path { subs: vec![(vec![(inner, EY - ry * 0.55), (outer, EY - ry * 0.05), (outer, EY - ry - 6.0), (inner, EY - ry - 6.0)], true)] };
        d.fill(&lid, body, 1.0);
        if !mini { d.stroke(&Path::line(&[(inner, EY - ry * 0.55), (outer, EY - ry * 0.05)]), C::rim(), 2.5, 1.0); }
    }
}

fn held_props(d: &mut D, st: St, p: &super::motion::Pose, ms: f32) {
    match st {
        St::Reading => d.with(Tf::rot_at(-4.0, 100.0, 150.0), |d| {
            let pg = Path::rr(62.0, 126.0, 76.0, 48.0, 5.0);
            d.fill(&pg, C::paper(), 1.0);
            d.stroke(&pg, C::line(), 2.0, 1.0);
            let line = (p.p1 * 4.0).floor() as i32 % 4;
            for i in 0..4 {
                let y = 138.0 + i as f32 * 9.0;
                let w = if i == 3 { 34.0 } else { 56.0 };
                let on = i == line;
                d.stroke(&Path::line(&[(72.0, y), (72.0 + w, y)]), if on { C::cyan() } else { hex("#B8B8C4") }, if on { 4.0 } else { 3.0 }, 1.0);
            }
        }),
        St::Editing => {
            let k = p.p1 * 3.0;
            let row = k.floor();
            let fr = k - row;
            d.with(Tf::rot_at(6.0, 160.0, 140.0), |d| {
                let pad = Path::rr(138.0, 118.0, 48.0, 50.0, 4.0);
                d.fill(&pad, C::paper(), 1.0);
                d.stroke(&pad, C::line(), 2.0, 1.0);
                for i in 0..3 {
                    let i = i as f32;
                    let len = if i < row { 30.0 } else if i == row { 30.0 * fr } else { 0.0 };
                    if len > 0.5 {
                        let s = format!("M146 {}q{} -4 {} 0t{} 0", 132.0 + i * 11.0, len / 4.0, len / 2.0, len / 2.0);
                        d.stroke(&Path::svg(&s), C::line(), 2.2, 1.0);
                    }
                }
                let (tx, ty) = (146.0 + 30.0 * fr, 132.0 + 11.0 * row.min(2.0));
                let pencil = Path::svg(&format!("M{tx} {ty}l4 -3 16 -22 -4 -3 -16 22z"));
                d.fill(&pencil, C::coral(), 1.0);
                d.stroke(&pencil, C::line(), 1.5, 1.0);
                d.fill(&Path::svg(&format!("M{} {}l-4 -3 3 -4 4 3z", tx + 20.0, ty - 25.0)), C::pink(), 1.0);
            });
        }
        St::Searching => {
            let a = -0.75 + 1.5 * p.p1;
            let (cx, cy) = (100.0 + 46.0 * a.sin(), 108.0 - 6.0 * a.cos());
            let h = Path::line(&[(cx + 13.0, cy + 13.0), (cx + 31.0, cy + 31.0)]);
            d.stroke(&h, C::line(), 7.0, 1.0);
            d.stroke(&h, C::amber(), 4.0, 1.0);
            let g = Path::circle(cx, cy, 18.0);
            d.fill(&g, C::cyan(), 0.22);
            d.stroke(&g, C::amber(), 5.0, 1.0);
            d.stroke(&Path::svg(&format!("M{} {}a10 10 0 0 1 8 -6", cx - 9.0, cy - 6.0)), (255, 255, 255), 3.0, 0.7);
        }
        St::Building => {
            let blk = Path::rr(144.0, 150.0, 40.0, 22.0, 3.0);
            d.fill(&blk, hex("#2A2A33"), 1.0);
            d.stroke(&blk, C::line(), 2.0, 1.0);
            d.stroke(&Path::svg("M152 158h8M166 164h10"), C::line(), 2.0, 1.0);
            d.with(Tf::rot_at(-75.0 + 75.0 * p.p1, 132.0, 132.0), |d| {
                d.stroke(&Path::line(&[(132.0, 132.0), (164.0, 132.0)]), hex("#8A6A46"), 5.0, 1.0);
                let hd = Path::rr(158.0, 120.0, 12.0, 24.0, 2.0);
                d.fill(&hd, C::sec(), 1.0);
                d.stroke(&hd, C::line(), 2.0, 1.0);
            });
            if p.p2 > 0.0 && p.p2 < 1.0 {
                for (dx, dy) in [(-1.0, -1.0), (1.0, -1.2), (1.4, -0.3), (-1.4, -0.4)] {
                    let (x0, y0) = (164.0 + dx * (4.0 + 8.0 * p.p2), 150.0 + dy * (4.0 + 8.0 * p.p2));
                    d.stroke(&Path::line(&[(x0, y0), (x0 + dx * 6.0, y0 + dy * 6.0)]), C::amber(), 3.0, 1.0 - p.p2);
                }
            }
        }
        St::Testing => {
            let fl = Path::svg("M152 104h20M156 104v22l-14 32c-2 6 2 12 8 12h24c6 0 10-6 8-12l-14-32v-22");
            d.fill(&fl, C::paper(), 0.9);
            d.stroke(&fl, C::line(), 2.5, 1.0);
            d.fill(&Path::svg("M147 146h30l7 14c2 4 -1 8 -5 8h-34c-4 0 -7 -4 -5 -8z"), C::cyan(), 1.0);
            for i in 0..3 {
                let ph = (p.p1 * 2.0 + i as f32 / 3.0) % 1.0;
                d.fill(&Path::circle(158.0 + i as f32 * 5.0 + 3.0 * (ph * 9.0).sin(), 160.0 - 56.0 * ph, 2.5 + i as f32), C::cyan(), 1.0 - ph);
            }
        }
        St::Drinking => {
            // the water bottle (kit v2): tips up to the mouth, the level drops, bubbles rise while it gulps
            let (lvl, k) = (1.0 - 0.55 * p.p1, p.p2);
            d.with(Tf::tr(162.0 - 4.0 * k, 162.0 - 54.0 * k).then(Tf::rot(-4.0 - 114.0 * k)), |d| {
                let b = Path::rr(-10.0, -44.0, 20.0, 44.0, 6.0);
                d.fill(&b, hex("#DDEFF4"), 0.85);
                d.stroke(&b, C::line(), 2.0, 1.0);
                let wh = 38.0 * lvl;
                let wy = if k > 0.5 { -41.0 } else { -3.0 - wh };
                d.fill(&Path::rr(-7.0, wy, 14.0, wh, 4.0), C::cyan(), 1.0);
                d.stroke(&Path::line(&[(-5.0, -38.0), (-5.0, -14.0)]), (255, 255, 255), 2.5, 0.7);
                let cap = Path::rr(-7.0, -52.0, 14.0, 9.0, 2.0);
                d.fill(&cap, C::coral(), 1.0);
                d.stroke(&cap, C::line(), 2.0, 1.0);
                let nz = Path::svg("M-2 -52v-8h4v8");
                d.fill(&nz, C::sec(), 1.0);
                d.stroke(&nz, C::line(), 1.5, 1.0);
                if p.p2 > 0.6 {
                    for i in 0..3 {
                        let ph = (ms / 500.0 + i as f32 / 3.0) % 1.0;
                        d.fill(&Path::circle(-2.0 + 3.0 * (i as f32 * 2.0 + ph * 6.0).sin(), -40.0 + wh * ph, 1.6 + i as f32 * 0.5), (255, 255, 255), 0.8 * (1.0 - ph));
                    }
                }
            });
        }
        _ => {}
    }
}

fn qmark(d: &mut D, x: f32, y: f32, s: f32, o: f32) {
    if s <= 0.0 || o <= 0.0 { return; }
    let old = d.op;
    d.op = o;
    d.with(Tf::tr(x, y).then(Tf::sc(s, s)), |d| {
        d.stroke(&Path::svg("M-5 -6a5.5 5.5 0 1 1 7 5.2c-2 .8-2 2-2 4"), C::amber(), 3.2, 1.0);
        d.fill(&Path::circle(0.0, 8.5, 2.0), C::amber(), 1.0);
    });
    d.op = old;
}

fn free_props(d: &mut D, st: St, p: &super::motion::Pose, ms: f32) {
    match st {
        St::Celebrating if p.p1 > 0.0 && p.p1 < 1.0 => {
            let cols = [C::coral(), C::cyan(), C::amber(), C::green(), C::pink()];
            let t = p.p1;
            for i in 0..16 {
                let a = -PI * (0.08 + 0.84 * (i as f32 / 15.0));
                let v = 70.0 + 30.0 * ((i * 37) % 7) as f32 / 7.0;
                let (x, y) = (100.0 + a.cos() * v * t, 80.0 + a.sin() * v * t + 60.0 * t * t);
                d.with(Tf::rot_at(i as f32 * 47.0 + t * 540.0, x, y), |d| d.fill(&Path::rr(x - 3.0, y - 5.0, 6.0, 10.0, 1.5), cols[i % 5], 1.0 - t * t * t));
            }
        }
        St::Confused => {
            qmark(d, 150.0, 50.0, p.p1, p.p1);
            qmark(d, 170.0, 30.0, p.p1 * 0.7, (p.p1 * 1.4 - 0.4).max(0.0));
        }
        St::Greeting => {
            let s = 0.7 + 0.4 * (ms / 160.0).sin().abs();
            let k = s * p.paw_r;
            if k > 0.0 { d.with(Tf::tr(162.0, 92.0).then(Tf::sc(k, k)), |d| d.fill(&Path::svg("M0 -8L2 -2L8 0L2 2L0 8L-2 2L-8 0L-2 -2Z"), C::amber(), 1.0)); }
        }
        St::Poked if p.p1 < 1.0 => {
            // amber impact lines on the poked side and a "!" (kit v2)
            let (t, a) = (p.p1, 1.0 - p.p1);
            for (dx, dy) in [(-1.0, -0.4), (-1.0, 0.3), (-0.6, -1.0)] {
                let (x0, y0) = (44.0 + dx * (4.0 + 14.0 * t), 108.0 + dy * (4.0 + 14.0 * t));
                d.stroke(&Path::line(&[(x0, y0), (x0 + dx * 8.0, y0 + dy * 8.0)]), C::amber(), 3.5, a);
            }
            let o = (a * 2.0).min(1.0);
            d.with(Tf::tr(160.0, 64.0 - 10.0 * t), |d| {
                d.fill(&Path::svg("M-2.5 -14h5l-1 14h-3z"), C::amber(), o);
                d.fill(&Path::circle(0.0, 6.0, 3.0), C::amber(), o);
            });
        }
        St::Eating => {
            // the food box with kibble and a heart, crumbs popping out (kit v2)
            d.with(Tf::tr(152.0, 150.0), |d| {
                let flaps = Path::svg("M-22 -8L-30 -20L-14 -14Z M22 -8L30 -20L14 -14Z");
                d.fill(&flaps, hex("#B48A5E"), 1.0);
                d.stroke(&flaps, C::line(), 2.0, 1.0);
                for (x, y, c) in [(-10.0, -12.0, "#8A5A3E"), (-2.0, -15.0, "#A86E48"), (7.0, -12.0, "#8A5A3E"), (13.0, -15.0, "#A86E48"), (-14.0, -9.0, "#A86E48")] {
                    d.fill(&Path::circle(x, y, 3.4), hex(c), 1.0);
                }
                let bx = Path::svg("M-22 -10H22L20 24H-20Z");
                d.fill(&bx, hex("#C8A27A"), 1.0);
                d.stroke(&bx, C::line(), 2.0, 1.0);
                d.stroke(&Path::line(&[(-22.0, -10.0), (22.0, -10.0)]), C::line(), 2.0, 1.0);
                d.fill(&Path::svg("M0 14c-6-4-9-7-9-10a4 4 0 0 1 9-1a4 4 0 0 1 9 1c0 3-3 6-9 10z"), C::pink(), 1.0);
            });
            let ph = (p.p1 * 2.0) % 1.0;
            if ph > 0.2 && ph < 0.7 {
                let k = (ph - 0.2) / 0.5;
                for (dx, s) in [(-1.0, 1.0), (1.0, 1.3), (0.0, 1.6)] {
                    d.fill(&Path::circle(150.0 + dx * 12.0 * k, 138.0 - 16.0 * s * k + 18.0 * k * k, 2.2), hex("#A86E48"), 1.0 - k);
                }
            }
        }
        St::Playing => {
            // the yarn ball bounces off the paw (kit v2)
            let v = ((p.p1 - 0.12) % 0.5 + 0.5) % 0.5 / 0.5;
            let sv = (PI * v).sin();
            let (bx, by, rot) = (160.0 + 10.0 * sv, 162.0 - 34.0 * sv, p.p1 * 720.0);
            d.stroke(&Path::svg(&format!("M{bx} {}C{} 176 184 172 196 176", by + 6.0, bx + 6.0)), C::pink(), 2.0, 1.0);
            d.fill(&Path::ellipse(bx, 175.0, 11.0 - 5.0 * sv, 3.0), (0, 0, 0), 0.25);
            d.with(Tf::tr(bx, by).then(Tf::rot(rot)), |d| {
                let ball = Path::circle(0.0, 0.0, 11.0);
                d.fill(&ball, C::pink(), 1.0);
                d.stroke(&ball, C::line(), 2.0, 1.0);
                d.stroke(&Path::svg("M-8 -6C-2 -2 2 2 8 7M-10 1C-4 2 2 5 5 10M-3 -10C1 -5 6 -2 10 -2"), hex("#D9668E"), 1.8, 1.0);
            });
        }
        _ => {}
    }
}

fn core_props(d: &mut D, st: St, p: &super::motion::Pose, ms: f32) {
    match st {
        St::Thinking => for (x, y, r, a) in [(140.0, 52.0, 4.0, p.p1), (154.0, 37.0, 5.5, p.p2), (172.0, 26.0, 7.0, p.p3)] {
            d.fill(&Path::circle(x, y - a * 3.0, r), C::cyan(), a);
        },
        St::Streaming => {
            let ws = [30.0f32, 26.0, 18.0];
            let mut left = p.p1 * ws.iter().sum::<f32>();
            let mut cur = None;
            for (i, &w) in ws.iter().enumerate() {
                let l = left.clamp(0.0, w);
                left -= l;
                let y = 92.0 + i as f32 * 12.0;
                if l > 0.0 { d.stroke(&Path::line(&[(164.0, y), (164.0 + l, y)]), C::eye_core(), 4.0, 0.85); }
                if cur.is_none() && (l < w || i == 2) { cur = Some((164.0 + l + 4.0, y)); }
            }
            if let Some((x, y)) = cur { if (ms / 280.0).floor() as i64 % 2 == 0 { d.fill(&Path::rr(x, y - 5.0, 4.5, 10.0, 1.0), C::coral(), 1.0); } }
        }
        St::Running => {
            let t = Path::rr(148.0, 98.0, 46.0, 34.0, 5.0);
            d.fill(&t, C::ink(), 1.0);
            d.stroke(&t, C::cyan(), 2.0, 1.0);
            d.stroke(&Path::svg("M155 107l4 3.5-4 3.5M162 115h6"), C::cyan(), 2.0, 1.0);
            d.stroke(&Path::line(&[(155.0, 124.0), (187.0, 124.0)]), hex("#46465A"), 3.0, 1.0);
            d.stroke(&Path::line(&[(155.0, 124.0), (155.0 + (32.0 * p.p1).max(0.1), 124.0)]), C::cyan(), 3.0, 1.0);
        }
        St::Permission => d.with(Tf::tr(46.0, 46.0).then(Tf::sc(p.p1, p.p1)), |d| {
            let c = Path::circle(0.0, 0.0, 15.0);
            d.fill(&c, C::ink(), 1.0);
            d.stroke(&c, C::amber(), 2.5, 1.0);
            d.stroke(&Path::svg("M-4.5 -4a4.5 4.5 0 1 1 6.5 4c-1.3.7-2 1.5-2 3"), C::amber(), 2.6, 1.0);
            d.fill(&Path::circle(0.0, 7.5, 1.7), C::amber(), 1.0);
        }),
        St::Success => {
            if p.p2 > 0.0 && p.p2 < 1.0 { d.stroke(&Path::circle(152.0, 50.0, 13.0 + 16.0 * p.p2), C::green(), 2.0, 1.0 - p.p2); }
            if p.p1 > 0.01 {
                d.with(Tf::tr(152.0, 50.0).then(Tf::sc(p.p1, p.p1)), |d| {
                    d.fill(&Path::circle(0.0, 0.0, 13.0), C::green(), 1.0);
                    d.stroke(&Path::svg("M-6 0l4 4 8-8"), C::ink(), 3.0, 1.0);
                });
            }
        }
        St::Error => d.with(Tf::tr(152.0, 52.0).then(Tf::sc(p.p1, p.p1)), |d| {
            d.fill(&Path::circle(0.0, 0.0, 12.0), C::red(), 1.0);
            d.stroke(&Path::svg("M0 -6v6"), C::ink(), 3.0, 1.0);
            d.fill(&Path::circle(0.0, 5.5, 1.7), C::ink(), 1.0);
        }),
        St::Interrupted if p.p1 > 0.01 => d.with(Tf::tr(150.0, 50.0).then(Tf::sc(p.p1, p.p1)), |d| {
            d.fill(&Path::rr(-12.0, -12.0, 24.0, 24.0, 6.0), C::amber(), 1.0);
            d.fill(&Path::rr(-4.5, -4.5, 9.0, 9.0, 1.5), C::ink(), 1.0);
        }),
        St::Memory => {
            let s = p.p1;
            let (x, y) = if s <= 1.0 { (100.0 + 22.0 * (s * PI).sin(), 142.0 - 112.0 * s) } else { (100.0, 30.0 + 62.0 * (s - 1.0) / 0.2) };
            if p.p2 > 0.01 {
                d.fill(&Path::circle(x, y, 13.0), C::cyan(), 0.22 * p.p2);
                d.fill(&Path::circle(x, y, 6.5), C::cyan(), p.p2);
            }
            if p.p3 > 0.0 && p.p3 < 1.0 { d.stroke(&Path::circle(100.0, 104.0, 30.0 + 44.0 * p.p3), C::cyan(), 2.0, (1.0 - p.p3) * 0.8); }
        }
        St::Sleeping => for i in 0..3 {
            let ph = (p.p1 + i as f32 / 3.0) % 1.0;
            let (x, y, s) = (140.0 + 20.0 * ph + 3.0 * (ph * 6.0).sin(), 92.0 - 52.0 * ph, 0.7 + 0.7 * ph);
            d.with(Tf::tr(x, y).then(Tf::sc(s, s)), |d| d.stroke(&Path::svg("M-4 -4h8l-8 8h8"), C::sec(), 2.0, (ph * PI).sin()));
        },
        St::Waking if p.p1 > 0.0 && p.p1 < 1.0 => d.stroke(&Path::circle(100.0, 124.0, 52.0 + 34.0 * p.p1), C::cyan(), 2.0, (1.0 - p.p1) * 0.9),
        _ => {}
    }
}
