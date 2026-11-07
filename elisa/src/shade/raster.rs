//! A tiny anti-aliased vector rasterizer for Shade: an SVG path subset (M L H V C Q T A Z, absolute and relative),
//! affine transforms, nonzero fills and round-cap strokes. Pure and deterministic: the same calls give the same pixels.
//! Canvases are tiny (a terminal dock is at most ~40 x 40 "pixels"), so this stays well under a millisecond a frame.

pub type Rgb = (u8, u8, u8);

pub fn hex(s: &str) -> Rgb {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0);
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round().clamp(0.0, 255.0) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// 2D affine: x' = a x + c y + e, y' = b x + d y + f (SVG matrix order)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tf { pub a: f32, pub b: f32, pub c: f32, pub d: f32, pub e: f32, pub f: f32 }
impl Tf {
    pub const ID: Tf = Tf { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };
    pub fn tr(x: f32, y: f32) -> Tf { Tf { e: x, f: y, ..Tf::ID } }
    pub fn sc(x: f32, y: f32) -> Tf { Tf { a: x, d: y, ..Tf::ID } }
    pub fn rot(deg: f32) -> Tf { let (s, c) = deg.to_radians().sin_cos(); Tf { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 } }
    /// rotate about (cx, cy), like SVG `rotate(a cx cy)`
    pub fn rot_at(deg: f32, cx: f32, cy: f32) -> Tf { Tf::tr(cx, cy).then(Tf::rot(deg)).then(Tf::tr(-cx, -cy)) }
    /// self ∘ o: apply `o` first, then self (SVG `transform="self o"` nesting order)
    pub fn then(self, o: Tf) -> Tf {
        Tf {
            a: self.a * o.a + self.c * o.b, b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d, d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e, f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    pub fn ap(&self, x: f32, y: f32) -> (f32, f32) { (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f) }
    /// mean linear scale (stroke widths)
    pub fn scale(&self) -> f32 { (self.a * self.d - self.b * self.c).abs().sqrt() }
}

/// a path: subpaths of flattened points (user units) + whether each is closed
#[derive(Clone, Debug, Default)]
pub struct Path { pub subs: Vec<(Vec<(f32, f32)>, bool)> }

const CURVE_SEGS: usize = 10;

impl Path {
    pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
        let n = 28;
        let pts = (0..n).map(|i| { let a = i as f32 / n as f32 * std::f32::consts::TAU; (cx + rx * a.cos(), cy + ry * a.sin()) }).collect();
        Path { subs: vec![(pts, true)] }
    }
    pub fn circle(cx: f32, cy: f32, r: f32) -> Path { Path::ellipse(cx, cy, r, r) }
    /// rounded rect (the kit's `rr`)
    pub fn rr(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let mut pts = vec![];
        for (cx, cy, a0) in [(x + w - r, y + r, -90.0f32), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 90.0), (x + r, y + r, 180.0)] {
            for i in 0..=4 {
                let a = (a0 + 90.0 * i as f32 / 4.0).to_radians();
                pts.push((cx + r * a.cos(), cy + r * a.sin()));
            }
        }
        Path { subs: vec![(pts, true)] }
    }
    pub fn line(pts: &[(f32, f32)]) -> Path { Path { subs: vec![(pts.to_vec(), false)] } }

    /// parse an SVG path `d` (the subset the kit's renderer uses)
    pub fn svg(d: &str) -> Path {
        let mut toks: Vec<Tok> = vec![];
        let b = d.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let c = b[i] as char;
            if c.is_ascii_alphabetic() && c != 'e' && c != 'E' { toks.push(Tok::C(c)); i += 1; continue; }
            if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
                let s = i;
                i += 1;
                let mut dot = c == '.';
                while i < b.len() {
                    let ch = b[i] as char;
                    if ch.is_ascii_digit() { i += 1; }
                    else if ch == '.' && !dot { dot = true; i += 1; }
                    else if (ch == 'e' || ch == 'E') && i + 1 < b.len() { i += 1; if b[i] == b'-' || b[i] == b'+' { i += 1; } }
                    else { break; }
                }
                toks.push(Tok::N(d[s..i].parse().unwrap_or(0.0)));
                continue;
            }
            i += 1;
        }
        let mut p = Path::default();
        let mut cur: Vec<(f32, f32)> = vec![];
        let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut last_q: Option<(f32, f32)> = None;
        let mut cmd = 'M';
        let mut j = 0;
        let num = |j: &mut usize| -> Option<f32> { match toks.get(*j) { Some(Tok::N(v)) => { *j += 1; Some(*v) } _ => None } };
        while j < toks.len() {
            if let Tok::C(c) = toks[j] { cmd = c; j += 1; if c == 'Z' || c == 'z' {
                if !cur.is_empty() { p.subs.push((std::mem::take(&mut cur), true)); }
                x = sx; y = sy; last_q = None; continue;
            } }
            let rel = cmd.is_ascii_lowercase();
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            match cmd.to_ascii_uppercase() {
                'M' => {
                    let (Some(a), Some(b2)) = (num(&mut j), num(&mut j)) else { break };
                    if cur.len() > 1 { p.subs.push((std::mem::take(&mut cur), false)); } else { cur.clear(); }
                    x = ox + a; y = oy + b2; sx = x; sy = y;
                    cur.push((x, y));
                    cmd = if rel { 'l' } else { 'L' };
                    last_q = None;
                }
                'L' => { let (Some(a), Some(b2)) = (num(&mut j), num(&mut j)) else { break }; x = ox + a; y = oy + b2; cur.push((x, y)); last_q = None; }
                'H' => { let Some(a) = num(&mut j) else { break }; x = ox + a; cur.push((x, y)); last_q = None; }
                'V' => { let Some(a) = num(&mut j) else { break }; y = oy + a; cur.push((x, y)); last_q = None; }
                'C' => {
                    let v: Vec<f32> = (0..6).filter_map(|_| num(&mut j)).collect();
                    if v.len() < 6 { break }
                    let (p0, p1, p2, p3) = ((x, y), (ox + v[0], oy + v[1]), (ox + v[2], oy + v[3]), (ox + v[4], oy + v[5]));
                    for s in 1..=CURVE_SEGS { cur.push(cubic(p0, p1, p2, p3, s as f32 / CURVE_SEGS as f32)); }
                    (x, y) = p3;
                    last_q = None;
                }
                'Q' | 'T' => {
                    let is_t = cmd.eq_ignore_ascii_case(&'T');
                    let c1 = if is_t {
                        last_q.map_or((x, y), |(qx, qy)| (2.0 * x - qx, 2.0 * y - qy))
                    } else {
                        let (Some(a), Some(b2)) = (num(&mut j), num(&mut j)) else { break };
                        (ox + a, oy + b2)
                    };
                    let (Some(a), Some(b2)) = (num(&mut j), num(&mut j)) else { break };
                    let p3 = (ox + a, oy + b2);
                    let p0 = (x, y);
                    for s in 1..=CURVE_SEGS {
                        let t = s as f32 / CURVE_SEGS as f32;
                        let u = 1.0 - t;
                        cur.push((u * u * p0.0 + 2.0 * u * t * c1.0 + t * t * p3.0, u * u * p0.1 + 2.0 * u * t * c1.1 + t * t * p3.1));
                    }
                    (x, y) = p3;
                    last_q = Some(c1);
                }
                'A' => {
                    let v: Vec<f32> = (0..7).filter_map(|_| num(&mut j)).collect();
                    if v.len() < 7 { break }
                    let to = (ox + v[5], oy + v[6]);
                    arc(&mut cur, (x, y), v[0], v[1], v[2], v[3] != 0.0, v[4] != 0.0, to);
                    (x, y) = to;
                    last_q = None;
                }
                _ => { j += 1; }
            }
        }
        if cur.len() > 1 { p.subs.push((cur, false)); }
        p
    }

    pub fn tf(&self, t: &Tf) -> Vec<(Vec<(f32, f32)>, bool)> {
        self.subs.iter().map(|(v, c)| (v.iter().map(|&(x, y)| t.ap(x, y)).collect(), *c)).collect()
    }
}

enum Tok { C(char), N(f32) }

fn cubic(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1)
}

/// SVG endpoint arc → points (F.6.5 of the SVG spec)
#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Vec<(f32, f32)>, p0: (f32, f32), rx: f32, ry: f32, phi: f32, large: bool, sweep: bool, p1: (f32, f32)) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 { out.push(p1); return; }
    let (s, c) = phi.to_radians().sin_cos();
    let (dx, dy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
    let (x1, y1) = (c * dx + s * dy, -s * dx + c * dy);
    let l = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if l > 1.0 { rx *= l.sqrt(); ry *= l.sqrt(); }
    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut k = if den > 0.0 { (num / den).sqrt() } else { 0.0 };
    if large == sweep { k = -k; }
    let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
    let (cx, cy) = (c * cx1 - s * cy1 + (p0.0 + p1.0) / 2.0, s * cx1 + c * cy1 + (p0.1 + p1.1) / 2.0);
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut dt = ang((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && dt > 0.0 { dt -= std::f32::consts::TAU; }
    if sweep && dt < 0.0 { dt += std::f32::consts::TAU; }
    let n = ((dt.abs() / std::f32::consts::FRAC_PI_4).ceil() as usize * 4).max(4);
    for i in 1..=n {
        let t = t1 + dt * i as f32 / n as f32;
        let (ex, ey) = (rx * t.cos(), ry * t.sin());
        out.push((c * ex - s * ey + cx, s * ex + c * ey + cy));
    }
}

/// premultiplied RGBA canvas, `w` x `h` pixels
#[derive(Clone)]
pub struct Canvas {
    pub w: usize, pub h: usize, px: Vec<[f32; 4]>, cov: Vec<f32>,
    /// device pixels per art pixel (a supersampled canvas: >1); minimum line widths are in art pixels
    pub scale: f32,
    /// crisp pixel art: no group translucency (a dimmed Shade would blend into in-between colours)
    pub crisp: bool,
}

const SS: usize = 5;

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas { Canvas { w, h, px: vec![[0.0; 4]; w * h], cov: vec![0.0; w * h], scale: 1.0, crisp: false } }

    /// straight RGB + alpha 0..1 of a pixel
    pub fn get(&self, x: usize, y: usize) -> (Rgb, f32) {
        let p = self.px[y * self.w + x];
        if p[3] <= 1e-4 { return ((0, 0, 0), 0.0); }
        let c = |v: f32| (v / p[3] * 255.0).round().clamp(0.0, 255.0) as u8;
        ((c(p[0]), c(p[1]), c(p[2])), p[3].min(1.0))
    }

    /// nonzero-winding fill of device-space polygons with `rgb` at opacity `op`
    pub fn fill_polys(&mut self, polys: &[Vec<(f32, f32)>], rgb: Rgb, op: f32) {
        if op <= 0.002 { return; }
        let edges: Vec<(f32, f32, f32, f32)> = polys.iter().filter(|p| p.len() >= 3).flat_map(|p| {
            (0..p.len()).map(move |i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); (a.0, a.1, b.0, b.1) })
        }).filter(|e| e.1 != e.3).collect();
        if edges.is_empty() { return; }
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for e in &edges { y0 = y0.min(e.1).min(e.3); y1 = y1.max(e.1).max(e.3); }
        let (ry0, ry1) = ((y0.floor().max(0.0)) as usize, (y1.ceil().max(0.0) as usize).min(self.h));
        let w = self.w;
        let mut xs: Vec<(f32, i32)> = vec![];
        for py in ry0..ry1 {
            let row = &mut self.cov[py * w..(py + 1) * w];
            row.iter_mut().for_each(|c| *c = 0.0);
            let mut any = false;
            for s in 0..SS {
                let sy = py as f32 + (s as f32 + 0.5) / SS as f32;
                xs.clear();
                for &(ax, ay, bx, by) in &edges {
                    let (lo, hi, dir) = if ay < by { (ay, by, 1) } else { (by, ay, -1) };
                    if sy < lo || sy >= hi { continue; }
                    xs.push((ax + (sy - ay) / (by - ay) * (bx - ax), dir));
                }
                if xs.is_empty() { continue; }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                for i in 0..xs.len() {
                    let before = wind;
                    wind += xs[i].1;
                    if before == 0 && wind != 0 {
                        // span start: find where winding returns to 0
                        let xa = xs[i].0;
                        let mut wv = wind;
                        let mut k = i + 1;
                        while k < xs.len() { wv += xs[k].1; if wv == 0 { break; } k += 1; }
                        let xb = xs.get(k).map_or(w as f32, |x| x.0);
                        span(row, xa, xb, 1.0 / SS as f32);
                        any = true;
                    }
                }
            }
            if !any { continue; }
            for (px, &c) in self.px[py * w..(py + 1) * w].iter_mut().zip(row.iter()) {
                let a = (c.min(1.0)) * op;
                if a <= 0.0 { continue; }
                let src = [rgb.0 as f32 / 255.0 * a, rgb.1 as f32 / 255.0 * a, rgb.2 as f32 / 255.0 * a, a];
                for ch in 0..4 { px[ch] = src[ch] + px[ch] * (1.0 - a); }
            }
        }
    }

    pub fn fill(&mut self, p: &Path, t: &Tf, rgb: Rgb, op: f32) {
        let polys: Vec<Vec<(f32, f32)>> = p.tf(t).into_iter().map(|(v, _)| v).collect();
        self.fill_polys(&polys, rgb, op);
    }

    /// stroke with round caps and joins, width in user units (scaled by the transform)
    pub fn stroke(&mut self, p: &Path, t: &Tf, rgb: Rgb, width: f32, op: f32) {
        let r = (width * t.scale() / 2.0).max(0.05);
        let mut polys = vec![];
        for (pts, closed) in p.tf(t) {
            let n = pts.len();
            if n == 0 { continue; }
            let segs = if closed { n } else { n - 1 };
            for i in 0..segs {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l = (dx * dx + dy * dy).sqrt();
                if l < 1e-5 { continue; }
                let (nx, ny) = (-dy / l * r, dx / l * r);
                polys.push(ccw(vec![(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]));
            }
            for &(x, y) in &pts {
                let m = 10;
                polys.push(ccw((0..m).map(|i| { let a = i as f32 / m as f32 * std::f32::consts::TAU; (x + r * a.cos(), y + r * a.sin()) }).collect()));
            }
        }
        self.fill_polys(&polys, rgb, op);
    }

    /// soft radial glow: alpha `a` at the centre falling to 0 at the ellipse edge (CSS radial-gradient closest-side)
    pub fn glow(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: Rgb, a: f32) {
        if a <= 0.002 { return; }
        for y in 0..self.h {
            for x in 0..self.w {
                let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
                let d = (dx * dx + dy * dy).sqrt();
                if d >= 1.0 { continue; }
                let al = a * (1.0 - d);
                let px = &mut self.px[y * self.w + x];
                let src = [rgb.0 as f32 / 255.0 * al, rgb.1 as f32 / 255.0 * al, rgb.2 as f32 / 255.0 * al, al];
                for ch in 0..4 { px[ch] = src[ch] + px[ch] * (1.0 - al); }
            }
        }
    }
}

/// counter-clockwise (positive signed area in y-down device space is clockwise on screen; any one fixed orientation
/// works for a nonzero union)
fn ccw(mut p: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let a: f32 = (0..p.len()).map(|i| { let (u, v) = (p[i], p[(i + 1) % p.len()]); u.0 * v.1 - v.0 * u.1 }).sum();
    if a < 0.0 { p.reverse(); }
    p
}

/// add `k` x the horizontal coverage of [xa, xb) to each pixel of `row`
fn span(row: &mut [f32], xa: f32, xb: f32, k: f32) {
    let w = row.len() as f32;
    let (xa, xb) = (xa.clamp(0.0, w), xb.clamp(0.0, w));
    if xb <= xa { return; }
    let (ia, ib) = (xa.floor() as usize, xb.floor() as usize);
    if ia == ib { if ia < row.len() { row[ia] += (xb - xa) * k; } return; }
    row[ia] += (ia as f32 + 1.0 - xa) * k;
    for c in row.iter_mut().take(ib).skip(ia + 1) { *c += k; }
    if ib < row.len() { row[ib] += (xb - ib as f32) * k; }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_and_strokes_cover_the_right_pixels() {
        let mut c = Canvas::new(10, 10);
        c.fill(&Path::rr(2.0, 2.0, 6.0, 6.0, 0.0), &Tf::ID, (255, 0, 0), 1.0);
        assert_eq!(c.get(5, 5), ((255, 0, 0), 1.0));
        assert_eq!(c.get(0, 0).1, 0.0);
        // relative path commands, an arc and a closing Z
        let p = Path::svg("M1 1h8v8h-8z");
        let mut d = Canvas::new(10, 10);
        d.fill(&p, &Tf::ID, (0, 255, 0), 1.0);
        assert!((d.get(4, 4).1 - 1.0).abs() < 1e-3 && d.get(0, 4).1 == 0.0);
        let q = Path::svg("M-4.5 -4a4.5 4.5 0 1 1 6.5 4c-1.3.7-2 1.5-2 3");
        assert!(q.subs[0].0.len() > 8, "arc flattened");
        let mut e = Canvas::new(10, 10);
        e.stroke(&Path::line(&[(1.0, 5.0), (9.0, 5.0)]), &Tf::ID, (0, 0, 255), 2.0, 1.0);
        assert!(e.get(5, 5).1 > 0.9 && e.get(5, 1).1 == 0.0);
    }
    #[test]
    fn transforms_compose_like_svg() {
        let t = Tf::tr(10.0, 0.0).then(Tf::sc(2.0, 2.0));
        assert_eq!(t.ap(1.0, 1.0), (12.0, 2.0));
        let r = Tf::rot_at(90.0, 1.0, 1.0).ap(2.0, 1.0);
        assert!((r.0 - 1.0).abs() < 1e-5 && (r.1 - 2.0).abs() < 1e-5);
    }
}
