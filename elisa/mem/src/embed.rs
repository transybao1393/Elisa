//! Streaming all-MiniLM-L6-v2 sentence embedder (BERT, 6 layers, 384 dims), pure Rust on CPU.
//!
//! Nothing stays resident: every call reads the vocabulary, then streams the weights one layer at a time
//! from `model.safetensors` with pread (≈7 MB per layer), runs that layer over the whole batch, and drops it.
//! Word/position embeddings are read row by row for the tokens in the batch only. The OS page cache keeps
//! the file warm between calls; that cache is not this process's RSS.
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

pub const DIM: usize = 384;
const LAYERS: usize = 6;
const HEADS: usize = 12;
const FF: usize = 1536;
const MAX_TOK: usize = 128; // the model's tokenizer.json truncation
const EPS: f32 = 1e-12;
/// tokens per forward batch: bounds activations (hidden state = TOK_BUDGET × 384 × 4 B ≈ 3 MB)
pub const TOK_BUDGET: usize = 1024;
const THREADS: usize = 4;
fn threads() -> usize { THREADS }
thread_local! { static PAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) } }

fn bad(m: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, m.to_string()) }

/// Model directory: `$ELISA_MEM_EMBED_MODEL` (old name `$MEMGRAPH_EMBED_MODEL` still read), else `~/.elisa/models/all-MiniLM-L6-v2`; None if the files are missing.
pub fn model_dir() -> Option<PathBuf> {
    let d = std::env::var_os("ELISA_MEM_EMBED_MODEL").or_else(|| std::env::var_os("MEMGRAPH_EMBED_MODEL")).map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".elisa/models/all-MiniLM-L6-v2")))?;
    (d.join("model.safetensors").is_file() && d.join("tokenizer.json").is_file()).then_some(d)
}

// ---------- minimal JSON (enough for tokenizer.json and the safetensors header) ----------
enum J { Null, Bool, Num(f64), Str(String), Arr(Vec<J>), Obj(Vec<(String, J)>) }
impl J {
    fn get(&self, k: &str) -> Option<&J> { if let J::Obj(v) = self { v.iter().find(|x| x.0 == k).map(|x| &x.1) } else { None } }
}
struct P<'a> { b: &'a [u8], i: usize }
impl<'a> P<'a> {
    fn ws(&mut self) { while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() { self.i += 1 } }
    fn val(&mut self) -> io::Result<J> {
        self.ws();
        let c = *self.b.get(self.i).ok_or_else(|| bad("json eof"))?;
        match c {
            b'{' => {
                self.i += 1; let mut v = vec![];
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') { self.i += 1; break }
                    let k = match self.val()? { J::Str(s) => s, _ => return Err(bad("json key")) };
                    self.ws(); if self.b.get(self.i) != Some(&b':') { return Err(bad("json :")) } self.i += 1;
                    v.push((k, self.val()?)); self.ws();
                    match self.b.get(self.i) { Some(b',') => self.i += 1, Some(b'}') => { self.i += 1; break } _ => return Err(bad("json obj")) }
                }
                Ok(J::Obj(v))
            }
            b'[' => {
                self.i += 1; let mut v = vec![];
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') { self.i += 1; break }
                    v.push(self.val()?); self.ws();
                    match self.b.get(self.i) { Some(b',') => self.i += 1, Some(b']') => { self.i += 1; break } _ => return Err(bad("json arr")) }
                }
                Ok(J::Arr(v))
            }
            b'"' => {
                self.i += 1; let mut s: Vec<u8> = vec![];
                loop {
                    let c = *self.b.get(self.i).ok_or_else(|| bad("json str"))?; self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = self.b[self.i]; self.i += 1;
                            match e {
                                b'n' => s.push(b'\n'), b't' => s.push(b'\t'), b'r' => s.push(b'\r'), b'b' => s.push(8), b'f' => s.push(12),
                                b'u' => {
                                    let hx = |p: &mut P| -> u32 { let h = std::str::from_utf8(&p.b[p.i..p.i + 4]).ok().and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0xfffd); p.i += 4; h };
                                    let mut cp = hx(self);
                                    if (0xd800..0xdc00).contains(&cp) && self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                        self.i += 2; let lo = hx(self); cp = 0x10000 + ((cp - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
                                    }
                                    let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                                    let mut t = [0u8; 4]; s.extend_from_slice(ch.encode_utf8(&mut t).as_bytes());
                                }
                                x => s.push(x),
                            }
                        }
                        x => s.push(x),
                    }
                }
                Ok(J::Str(String::from_utf8_lossy(&s).into_owned()))
            }
            b't' => { self.i += 4; Ok(J::Bool) }
            b'f' => { self.i += 5; Ok(J::Bool) }
            b'n' => { self.i += 4; Ok(J::Null) }
            _ => {
                let s = self.i;
                while self.i < self.b.len() && (self.b[self.i].is_ascii_digit() || b"+-.eE".contains(&self.b[self.i])) { self.i += 1 }
                std::str::from_utf8(&self.b[s..self.i]).ok().and_then(|x| x.parse().ok()).map(J::Num).ok_or_else(|| bad("json num"))
            }
        }
    }
}

// ---------- BERT WordPiece tokenizer (uncased) ----------
/// Vocabulary as a sorted binary table read through mmap (no parse, nothing resident):
/// `[n u32][str offsets u32 × (n+1)][ids u32 × n][strings]`. Built once from tokenizer.json next to the store.
struct Vocab { p: *mut u8, len: usize, n: usize }
impl Drop for Vocab { fn drop(&mut self) { unsafe { libc::munmap(self.p as *mut libc::c_void, self.len); } } }
impl Vocab {
    fn build(tok: &Path, out: &Path) -> io::Result<()> {
        let b = std::fs::read(tok)?;
        let j = P { b: &b, i: 0 }.val()?;
        let Some(J::Obj(v)) = j.get("model").and_then(|m| m.get("vocab")) else { return Err(bad("tokenizer.json: no model.vocab")) };
        let mut e: Vec<(&str, u32)> = v.iter().filter_map(|(k, x)| if let J::Num(n) = x { Some((k.as_str(), *n as u32)) } else { None }).collect();
        e.sort();
        let mut o = vec![]; o.extend((e.len() as u32).to_le_bytes());
        let mut off = 0u32;
        for (k, _) in &e { o.extend(off.to_le_bytes()); off += k.len() as u32 } o.extend(off.to_le_bytes());
        for (_, id) in &e { o.extend(id.to_le_bytes()) }
        for (k, _) in &e { o.extend(k.as_bytes()) }
        let tmp = out.with_extension("tmp");
        std::fs::write(&tmp, &o)?; std::fs::rename(&tmp, out)
    }
    fn open(tok: &Path, cache: &Path) -> io::Result<Vocab> {
        if !cache.exists() { Vocab::build(tok, cache)? }
        let f = File::open(cache)?;
        let len = f.metadata()?.len() as usize;
        if len < 8 { return Err(bad("vocab cache")) }
        let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, std::os::unix::io::AsRawFd::as_raw_fd(&f), 0) };
        if p == libc::MAP_FAILED { return Err(io::Error::last_os_error()) }
        let b = unsafe { std::slice::from_raw_parts(p as *const u8, len) };
        let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
        Ok(Vocab { p: p as *mut u8, len, n })
    }
    fn b(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.p, self.len) } }
    fn u(&self, i: usize) -> usize { u32::from_le_bytes(self.b()[i * 4..i * 4 + 4].try_into().unwrap()) as usize }
    fn key(&self, i: usize) -> &[u8] { let s0 = 4 + (self.n + 1) * 4 + self.n * 4; &self.b()[s0 + self.u(1 + i)..s0 + self.u(2 + i)] }
    fn id(&self, t: &str) -> Option<u32> {
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let m = (lo + hi) / 2;
            match self.key(m).cmp(t.as_bytes()) { std::cmp::Ordering::Less => lo = m + 1, std::cmp::Ordering::Greater => hi = m, _ => return Some(self.u(1 + self.n + 1 + m) as u32) }
        }
        None
    }
    /// [CLS] wordpieces… [SEP], truncated to MAX_TOK
    fn encode(&self, text: &str) -> Vec<u32> {
        let unk = self.id("[UNK]").unwrap_or(100);
        let mut out = vec![self.id("[CLS]").unwrap_or(101)];
        let mut norm = String::with_capacity(text.len());
        for c in text.chars() {
            if c == '\0' || c == '\u{fffd}' || (c.is_control() && !c.is_whitespace()) { continue }
            if c.is_whitespace() { norm.push(' '); continue }
            let cjk = matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F);
            if cjk || c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric()) { norm.push(' '); norm.push(c); norm.push(' '); continue }
            for l in c.to_lowercase() { norm.push(strip_accent(l)) }
        }
        'w: for w in norm.split(' ').filter(|w| !w.is_empty()) {
            if out.len() >= MAX_TOK - 1 { break }
            let cs: Vec<(usize, char)> = w.char_indices().collect();
            if cs.len() > 100 { out.push(unk); continue }
            let (mut s, mut pieces) = (0usize, vec![]);
            while s < cs.len() {
                let mut e = cs.len();
                let mut found = None;
                while e > s {
                    let (a, b) = (cs[s].0, if e < cs.len() { cs[e].0 } else { w.len() });
                    let sub = if s > 0 { format!("##{}", &w[a..b]) } else { w[a..b].to_string() };
                    if let Some(id) = self.id(&sub) { found = Some(id); break }
                    e -= 1;
                }
                match found { Some(id) => { pieces.push(id); s = e } None => { out.push(unk); continue 'w } }
            }
            out.extend(pieces);
        }
        out.truncate(MAX_TOK - 1);
        out.push(self.id("[SEP]").unwrap_or(102));
        out
    }
}
fn strip_accent(c: char) -> char {
    match c {
        'à'..='å' => 'a', 'ç' => 'c', 'è'..='ë' => 'e', 'ì'..='ï' => 'i', 'ñ' => 'n', 'ò'..='ö' | 'ø' => 'o', 'ù'..='ü' => 'u', 'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

// ---------- safetensors, read tensor by tensor ----------
struct St { f: File, base: u64, t: HashMap<String, (u64, usize)> }
impl St {
    fn open(path: &Path) -> io::Result<St> {
        let mut f = File::open(path)?;
        let mut n = [0u8; 8]; f.read_exact(&mut n)?;
        let n = u64::from_le_bytes(n);
        let mut h = vec![0u8; n as usize]; f.read_exact(&mut h)?;
        let J::Obj(v) = (P { b: &h, i: 0 }).val()? else { return Err(bad("safetensors header")) };
        let mut t = HashMap::new();
        for (k, x) in v {
            let (Some(J::Str(dt)), Some(J::Arr(o))) = (x.get("dtype"), x.get("data_offsets")) else { continue };
            if dt != "F32" { continue }
            if let (J::Num(a), J::Num(b)) = (&o[0], &o[1]) { t.insert(k.trim_start_matches("bert.").to_string(), (*a as u64, (*b - *a) as usize / 4)); }
        }
        Ok(St { f, base: 8 + n, t })
    }
    fn read_into(&self, name: &str, off: usize, out: &mut [f32]) -> io::Result<()> {
        let &(o, n) = self.t.get(name).ok_or_else(|| bad(&format!("missing tensor {name}")))?;
        if off + out.len() > n { return Err(bad("tensor range")) }
        let mut b = vec![0u8; out.len() * 4];
        self.f.read_exact_at(&mut b, self.base + o + off as u64 * 4)?;
        for (d, c) in out.iter_mut().zip(b.chunks_exact(4)) { *d = f32::from_le_bytes(c.try_into().unwrap()) }
        Ok(())
    }
    /// whole tensor into `out` (same length), through a reusable byte buffer
    fn read_buf(&self, name: &str, out: &mut [f32], raw: &mut Vec<u8>) -> io::Result<()> {
        let &(o, n) = self.t.get(name).ok_or_else(|| bad(&format!("missing tensor {name}")))?;
        if n != out.len() { return Err(bad("tensor shape")) }
        let _ = raw;
        // little-endian host: read straight into the f32 buffer
        let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, n * 4) };
        self.f.read_exact_at(bytes, self.base + o)?;
        if cfg!(target_endian = "big") { for x in out.iter_mut() { *x = f32::from_bits(x.to_bits().swap_bytes()) } }
        Ok(())
    }
    fn get(&self, name: &str) -> io::Result<Vec<f32>> {
        let n = self.t.get(name).ok_or_else(|| bad(&format!("missing tensor {name}")))?.1;
        let mut v = vec![0f32; n]; self.read_into(name, 0, &mut v)?; Ok(v)
    }
}

/// debug: current RSS in MB (ELISA_MEM_DEBUG)
pub fn rss_mb() -> f64 {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let sz = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        if libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut libc::c_void, sz) == sz { return ti.pti_resident_size as f64 / 1048576.0 }
    }
    0.0
}

// ---------- math ----------
#[cfg(target_arch = "aarch64")]
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len().min(b.len());
    let mut i = 0;
    let mut s = 0f32;
    unsafe {
        let (mut c0, mut c1, mut c2, mut c3) = (vdupq_n_f32(0.0), vdupq_n_f32(0.0), vdupq_n_f32(0.0), vdupq_n_f32(0.0));
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        while i + 16 <= n {
            c0 = vfmaq_f32(c0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            c1 = vfmaq_f32(c1, vld1q_f32(pa.add(i + 4)), vld1q_f32(pb.add(i + 4)));
            c2 = vfmaq_f32(c2, vld1q_f32(pa.add(i + 8)), vld1q_f32(pb.add(i + 8)));
            c3 = vfmaq_f32(c3, vld1q_f32(pa.add(i + 12)), vld1q_f32(pb.add(i + 12)));
            i += 16;
        }
        while i + 4 <= n { c0 = vfmaq_f32(c0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i))); i += 4 }
        s += vaddvq_f32(vaddq_f32(vaddq_f32(c0, c1), vaddq_f32(c2, c3)));
    }
    while i < n { s += a[i] * b[i]; i += 1 }
    s
}
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) { for k in 0..8 { acc[k] += x[k] * y[k] } }
    let mut s = acc.iter().sum::<f32>();
    for (x, y) in ra.iter().zip(rb) { s += x * y }
    s
}
/// out[r][o] = x[r]·w[o] + b[o]   (w is [out][in], PyTorch Linear layout)
fn linear(x: &[f32], din: usize, w: &[f32], b: &[f32], out: &mut [f32]) {
    let dout = b.len();
    let rows = x.len() / din;
    // a single short sequence (a query): split output columns over threads instead
    if PAR.with(|p| p.get()) && threads() > 1 && rows * dout * din >= 1 << 20 {
        let per = dout.div_ceil(THREADS);
        let mut parts: Vec<Vec<f32>> = vec![vec![]; THREADS];
        std::thread::scope(|s| {
            for (t, part) in parts.iter_mut().enumerate() {
                let (o0, o1) = (t * per, ((t + 1) * per).min(dout));
                s.spawn(move || { *part = vec![0f32; rows * (o1 - o0)];
                    for (r, xr) in x.chunks_exact(din).enumerate() { for o in o0..o1 { part[r * (o1 - o0) + o - o0] = dot(xr, &w[o * din..(o + 1) * din]) + b[o] } } });
            }
        });
        for (t, part) in parts.iter().enumerate() {
            let (o0, o1) = (t * per, ((t + 1) * per).min(dout));
            for r in 0..rows { out[r * dout + o0..r * dout + o1].copy_from_slice(&part[r * (o1 - o0)..(r + 1) * (o1 - o0)]) }
        }
        return
    }
    for (xr, or) in x.chunks_exact(din).zip(out.chunks_exact_mut(dout)) {
        for o in 0..dout { or[o] = dot(xr, &w[o * din..(o + 1) * din]) + b[o] }
    }
}
fn layer_norm(x: &mut [f32], g: &[f32], b: &[f32]) {
    for r in x.chunks_exact_mut(DIM) {
        let m = r.iter().sum::<f32>() / DIM as f32;
        let v = r.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / DIM as f32;
        let s = 1.0 / (v + EPS).sqrt();
        for i in 0..DIM { r[i] = (r[i] - m) * s * g[i] + b[i] }
    }
}
fn erf(x: f32) -> f32 {
    // Abramowitz–Stegun 7.1.26 (|err| < 1.5e-7), in f64
    let x = x as f64;
    let (s, a) = (x.signum(), x.abs());
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-a * a).exp();
    (s * y) as f32
}
fn gelu(x: f32) -> f32 { 0.5 * x * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2)) }

/// f32 buffer in its own anonymous mapping, unmapped on drop, so big per-call buffers never linger in
/// the allocator's caches (RSS returns to baseline as soon as a call ends)
struct Big { p: *mut f32, n: usize }
unsafe impl Send for Big {}
unsafe impl Sync for Big {}
impl Big {
    fn new(n: usize) -> Big {
        let p = unsafe { libc::mmap(std::ptr::null_mut(), (n * 4).max(1), libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        assert!(p != libc::MAP_FAILED, "elisa-mem: mmap {n} floats failed");
        Big { p: p as *mut f32, n }
    }
}
impl std::ops::Deref for Big { type Target = [f32]; fn deref(&self) -> &[f32] { unsafe { std::slice::from_raw_parts(self.p, self.n) } } }
impl std::ops::DerefMut for Big { fn deref_mut(&mut self) -> &mut [f32] { unsafe { std::slice::from_raw_parts_mut(self.p, self.n) } } }
impl Drop for Big { fn drop(&mut self) { unsafe { libc::munmap(self.p as *mut libc::c_void, (self.n * 4).max(1)); } } }

/// per-thread activations for one sequence (≤ MAX_TOK tokens), allocated once per embed call
struct Scratch { q: Big, k: Big, v: Big, ctx: Big, a: Big, it: Big, p: Big }
impl Scratch {
    fn new() -> Scratch { let z = Big::new; Scratch { q: z(MAX_TOK * DIM), k: z(MAX_TOK * DIM), v: z(MAX_TOK * DIM), ctx: z(MAX_TOK * DIM), a: z(MAX_TOK * DIM), it: z(MAX_TOK * FF), p: z(MAX_TOK) } }
}
struct Layer { q: Big, qb: Big, k: Big, kb: Big, v: Big, vb: Big, o: Big, ob: Big,
    g1: Big, b1: Big, wi: Big, bi: Big, wo: Big, bo: Big, g2: Big, b2: Big }
impl Layer {
    fn new() -> Layer {
        let z = Big::new;
        Layer { q: z(DIM * DIM), qb: z(DIM), k: z(DIM * DIM), kb: z(DIM), v: z(DIM * DIM), vb: z(DIM), o: z(DIM * DIM), ob: z(DIM),
            g1: z(DIM), b1: z(DIM), wi: z(FF * DIM), bi: z(FF), wo: z(DIM * FF), bo: z(DIM), g2: z(DIM), b2: z(DIM) }
    }
    /// read layer `l` into the existing buffers (one set of ≈7 MB reused for all layers)
    fn load(&mut self, st: &St, l: usize) -> io::Result<()> {
        let mut raw = vec![];
        for (n, d) in [("attention.self.query.weight", &mut self.q), ("attention.self.query.bias", &mut self.qb), ("attention.self.key.weight", &mut self.k),
            ("attention.self.key.bias", &mut self.kb), ("attention.self.value.weight", &mut self.v), ("attention.self.value.bias", &mut self.vb),
            ("attention.output.dense.weight", &mut self.o), ("attention.output.dense.bias", &mut self.ob), ("attention.output.LayerNorm.weight", &mut self.g1),
            ("attention.output.LayerNorm.bias", &mut self.b1), ("intermediate.dense.weight", &mut self.wi), ("intermediate.dense.bias", &mut self.bi),
            ("output.dense.weight", &mut self.wo), ("output.dense.bias", &mut self.bo), ("output.LayerNorm.weight", &mut self.g2), ("output.LayerNorm.bias", &mut self.b2)] {
            st.read_buf(&format!("encoder.layer.{l}.{n}"), d, &mut raw)?;
        }
        Ok(())
    }
    /// one encoder layer over one sequence `h` (L×384), in place
    fn run(&self, h: &mut [f32], s: &mut Scratch) {
        let l = h.len() / DIM;
        let Scratch { q, k, v, ctx, a, it, p } = s;
        let (q, k, v, ctx, a, it, p) = (&mut q[..l * DIM], &mut k[..l * DIM], &mut v[..l * DIM], &mut ctx[..l * DIM], &mut a[..l * DIM], &mut it[..l * FF], &mut p[..l]);
        ctx.fill(0.0);
        linear(h, DIM, &self.q, &self.qb, q); linear(h, DIM, &self.k, &self.kb, k); linear(h, DIM, &self.v, &self.vb, v);
        let hd = DIM / HEADS;
        let sc = 1.0 / (hd as f32).sqrt();
        for hh in 0..HEADS {
            let c0 = hh * hd;
            for i in 0..l {
                let qi = &q[i * DIM + c0..i * DIM + c0 + hd];
                let mut mx = f32::MIN;
                for j in 0..l { p[j] = dot(qi, &k[j * DIM + c0..j * DIM + c0 + hd]) * sc; mx = mx.max(p[j]) }
                let mut z = 0.0; for x in p.iter_mut() { *x = (*x - mx).exp(); z += *x }
                let out = &mut ctx[i * DIM + c0..i * DIM + c0 + hd];
                for j in 0..l { let w = p[j] / z; for (o, x) in out.iter_mut().zip(&v[j * DIM + c0..j * DIM + c0 + hd]) { *o += w * x } }
            }
        }
        linear(ctx, DIM, &self.o, &self.ob, a);
        for (x, y) in a.iter_mut().zip(h.iter()) { *x += y }
        layer_norm(a, &self.g1, &self.b1);
        linear(a, DIM, &self.wi, &self.bi, it);
        for x in it.iter_mut() { *x = gelu(*x) }
        linear(it, FF, &self.wo, &self.bo, h);
        for (x, y) in h.iter_mut().zip(a.iter()) { *x += y }
        layer_norm(h, &self.g2, &self.b2);
    }
}

/// Embed `texts` (mean-pooled, L2-normalised). Streams the 6 layers once per batch of ≤ TOK_BUDGET tokens.
/// `progress(done_texts)` is called after each batch.
/// `vocab_cache` is where the binary vocabulary table lives (built on first use).
pub fn embed(dir: &Path, vocab_cache: &Path, texts: &[&str], progress: &mut dyn FnMut(usize)) -> io::Result<Vec<[f32; DIM]>> {
    let t0 = std::time::Instant::now();
    let vocab = Vocab::open(&dir.join("tokenizer.json"), vocab_cache)?;
    let st = St::open(&dir.join("model.safetensors"))?;
    let (ln_g, ln_b, tt) = (st.get("embeddings.LayerNorm.weight")?, st.get("embeddings.LayerNorm.bias")?, st.get("embeddings.token_type_embeddings.weight")?);
    let mut out = Vec::with_capacity(texts.len());
    let mut ly = Layer::new();
    let mut scr: Vec<Scratch> = (0..THREADS.min(texts.len()).max(1)).map(|_| Scratch::new()).collect();
    let dbg = std::env::var_os("ELISA_MEM_DEBUG").is_some();
    if dbg { eprintln!("  setup {:.2} ms rss {:.1} MB", t0.elapsed().as_secs_f64() * 1e3, rss_mb()) }
    let mut i = 0;
    while i < texts.len() {
        // one batch: tokenise lazily up to the token budget
        let mut ids: Vec<Vec<u32>> = vec![];
        let mut ntok = 0;
        while i < texts.len() {
            let e = vocab.encode(texts[i]);
            if !ids.is_empty() && ntok + e.len() > TOK_BUDGET { break }
            ntok += e.len(); ids.push(e); i += 1;
        }
        let mut hs: Vec<Vec<f32>> = ids.iter().map(|s| {
            let mut h = vec![0f32; s.len() * DIM];
            for (p, (&t, r)) in s.iter().zip(h.chunks_exact_mut(DIM)).enumerate() {
                let mut w = [0f32; DIM]; let mut pe = [0f32; DIM];
                let _ = st.read_into("embeddings.word_embeddings.weight", t as usize * DIM, &mut w);
                let _ = st.read_into("embeddings.position_embeddings.weight", p * DIM, &mut pe);
                for d in 0..DIM { r[d] = w[d] + pe[d] + tt[d] }
            }
            layer_norm(&mut h, &ln_g, &ln_b);
            h
        }).collect();
        for l in 0..LAYERS {
            let tl = std::time::Instant::now();
            ly.load(&st, l)?;
            if dbg { eprintln!("  layer {l} read {:.2} ms rss {:.1} MB", tl.elapsed().as_secs_f64() * 1e3, rss_mb()) }
            let per = hs.len().div_ceil(threads());
            if hs.len() == 1 { PAR.with(|p| p.set(true)); ly.run(&mut hs[0], &mut scr[0]); PAR.with(|p| p.set(false)) }
            else if threads() == 1 { for h in hs.iter_mut() { ly.run(h, &mut scr[0]) } } else {
                std::thread::scope(|s| { for (ch, sc) in hs.chunks_mut(per.max(1)).zip(scr.iter_mut()) { let ly = &ly; s.spawn(move || for h in ch { ly.run(h, sc) }); } });
            }
        }
        for h in hs {
            let n = h.len() / DIM;
            let mut v = [0f32; DIM];
            for r in h.chunks_exact(DIM) { for d in 0..DIM { v[d] += r[d] } }
            let norm = v.iter().map(|x| (x / n as f32).powi(2)).sum::<f32>().sqrt().max(1e-12);
            for x in v.iter_mut() { *x = *x / n as f32 / norm }
            out.push(v);
        }
        progress(out.len());
    }
    drop((ly, scr, st, vocab));
    crate::trim_heap();
    if std::env::var_os("ELISA_MEM_DEBUG").is_some() { eprintln!("embed: {} texts {:.1} ms rss {:.1} MB", texts.len(), t0.elapsed().as_secs_f64() * 1e3, rss_mb()) }
    Ok(out)
}
