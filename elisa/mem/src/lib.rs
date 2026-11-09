//! elisa-mem: a small disk-only graph memory store for Elisa (prototype).
//!
//! Files in `dir`:
//! - `log-NNNNNN.seg`: append-only hot records `[len u32][crc32 u32][payload]`, fsync'd per record.
//!   A torn or corrupt tail is detected by length/crc and truncated on open.
//! - `index.mg`: built by compaction, read through mmap. Sections: doc table (fixed 32 B rows),
//!   sorted term table, postings (delta+varint, tf) + entity co-occurrence lists, adjacency lists
//!   (edges + mentioned term ids), block directory, lz4 text blocks (~64 KB each).
//!
//! - `lock`: an flock taken (exclusive) around every write, reload and compaction, plus an 8-byte generation
//!   counter at offset 0 that every write bumps. Several processes (two Elisa windows, the app and a terminal) share
//!   one store: before it writes, a process whose generation is behind re-reads what the others added (`sync`).
//!   Reads between writes see the process's own consistent snapshot (its index mapping, its segment handles and its
//!   private hot files); `refresh` brings the snapshot up to date.
//! - hot postings/edges: derived from the log into files private to each open store (created and unlinked at once),
//!   never shared between processes (before 2026-11-09 they were the shared `hot.post`/`hot.adj`: another process's
//!   open rewrote them under a running session, whose next read indexed past its own doc table and panicked).
//!
//! Every node (project, session, turn, note, fact) gets a dense sequential id, so the doc table is an array.
//! Entities are the extracted terms; turn→entity "mentions" are the postings/forward lists.
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod bench;
pub mod gen;
pub mod embed;
mod extsort;

/// Mirrors `elisa::types::Role` (copied so this crate does not pull in Elisa's TUI deps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role { User, Assistant }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind { Turn, Note, Fact, Card }

/// `score` ranks (keywords + vectors + graph). `lex` is the same score without the vector part: a hit that only
/// an embedding found has lex 0. `support` is the idf-weighted share of the query's own terms that the hit (or the
/// hit whose question/note/fact link brought it in) contains, 0..=1: common words weigh little, a query made only of
/// words the store has never seen gives every hit a low support.
#[derive(Clone, Debug)]
pub struct Hit { pub id: u64, pub kind: HitKind, pub session: u64, pub score: f32, pub lex: f32, pub support: f32, pub text: String }

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub turns: usize, pub notes: usize, pub facts: usize, pub sessions: usize, pub cards: usize,
    pub entities: usize, pub edges: usize, pub hot_bytes: u64, pub cold_bytes: u64, pub segments: usize,
    /// true if the last open dropped a torn/corrupt log tail
    pub torn_tail: bool,
    /// stored embedding vectors, and their bytes on disk
    pub vectors: usize, pub vector_bytes: u64,
    /// highest peak RSS reported by a worker process so far (bytes; 0 = no worker ran)
    pub worker_peak: u64,
}

const PROJECT: u8 = 0; const SESSION: u8 = 1; const TURN: u8 = 2; const NOTE: u8 = 3; const FACT: u8 = 4; const END: u8 = 5; const CARD: u8 = 6;
const E_NEXT: u8 = 0; const E_PREV: u8 = 1; const E_SUMS: u8 = 2; const E_SUMBY: u8 = 3;
const E_REPL: u8 = 4; const E_REPLBY: u8 = 5; const E_SRC: u8 = 6; const E_SRCOF: u8 = 7;
// state cards (edge kinds ≥ 8 need the 4-bit kind field of v2 indexes)
const E_CPREV: u8 = 8; const E_CNEXT: u8 = 9; const E_SKIP: u8 = 10; const E_SKIPBY: u8 = 11; const E_MPREV: u8 = 12; const E_MNEXT: u8 = 13;
/// every MILESTONE-th card of a project links to the MILESTONE cards before it and to the previous milestone
const MILESTONE: u32 = 10;
const NONE: u32 = u32::MAX;
const BLOCK: usize = 64 << 10;
const MAGIC: u64 = 0x0031_5844_4947_4d4d; // "MMGIDX1"
const SNIP_CHARS: usize = 1200; // ~300 tokens

#[derive(Clone, Copy, Debug, Default)]
struct Meta { kind: u8, role: u8, session: u32, project: u32, len: u32 }
fn scored(k: u8) -> bool { k == TURN || k == NOTE || k == FACT || k == CARD }

// ---------- encoding helpers ----------
fn pv(b: &mut Vec<u8>, mut v: u64) { while v >= 0x80 { b.push(v as u8 | 0x80); v >>= 7 } b.push(v as u8) }
fn gv(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut s) = (0u64, 0);
    loop { let x = b[*p]; *p += 1; v |= ((x & 0x7f) as u64) << s; if x < 0x80 { return v } s += 7 }
}
fn r32(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }
fn r64(b: &[u8], o: usize) -> u64 { u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) }
fn bad(m: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, m.to_string()) }

// ---------- entity extraction (rules only) ----------
const STOP: &str = "a about above after again all also am an and any are as at be because been before being below between both but by can could did do does doing done down during each few for from further had has have having he her here hers him his how i if in into is it its itself just let lets me more most my no nor not now of off on once only or other our out over own same she should so some such than that the their them then there these they this those through to too under until up us very was we were what when where which while who whom why will with would you your yours yet get got ok okay still already ll ve re don didn doesn isn wasn won can't";
const UNITS: &[&str] = &["gb", "mb", "kb", "tb", "ms", "us", "ns", "s", "k", "m", "tok/s", "x", "%"];
fn stop(w: &str) -> bool { static S: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new(); S.get_or_init(|| STOP.split(' ').collect()).contains(w) }
fn has_ext(s: &str) -> bool {
    s.rsplit_once('.').map_or(false, |(a, e)| !a.is_empty() && (1..=5).contains(&e.len())
        && e.chars().all(|c| c.is_ascii_alphanumeric()) && e.chars().any(|c| c.is_ascii_alphabetic()))
}
fn stem(w: &str) -> String {
    let mut s = w;
    for suf in ["ing", "ed", "es", "s"] {
        if s.len() >= suf.len() + 3 && s.ends_with(suf) {
            if suf == "es" && !["ses", "xes", "ches", "shes"].iter().any(|x| s.ends_with(x)) { continue }
            if suf == "s" && s.ends_with("ss") { break }
            s = &s[..s.len() - suf.len()]; break;
        }
    }
    if s.len() > 4 && s.ends_with('e') { s = &s[..s.len() - 1] }
    s.to_string()
}
fn word(w: &str, out: &mut Vec<String>) {
    let w = w.to_lowercase();
    if w.chars().count() < 2 { return }
    if w.chars().all(|c| c.is_ascii_alphabetic()) { if !stop(&w) { out.push(stem(&w)) } }
    // identifiers with digits, and words of any other script, as they are (no stop list or stemmer for other
    // scripts; idf weighs their common words down). Before 2026-11-01 non-ASCII words were dropped, so a query in
    // such a language matched only on its stray ASCII syllables.
    else if w.chars().all(char::is_alphanumeric) { out.push(w) }
}
/// Terms of a text: paths/file names, identifiers (+ their parts), --flags, /commands, error codes,
/// numbers (+unit), and stemmed content words minus stop words. Non-plain-word terms count as "entities".
pub fn extract(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let toks: Vec<&str> = text.split(|c: char| c.is_whitespace() || ",;()[]{}\"'`<>=|*!?:@#$&^~".contains(c)).filter(|s| !s.is_empty()).collect();
    for (i, raw) in toks.iter().enumerate() {
        let t = raw.trim_end_matches(['.', '-']).trim_start_matches(['-', '.']).trim_start_matches('+');
        let t = if raw.starts_with("--") { raw.trim_end_matches('.') } else { t };
        if t.is_empty() { continue }
        let lo = t.to_lowercase();
        let c0 = t.chars().next().unwrap();
        if c0.is_ascii_digit() {
            let e = lo.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(lo.len());
            let (num, unit) = lo.split_at(e);
            if unit.is_empty() {
                if let Some(u) = toks.get(i + 1).map(|u| u.trim_end_matches('.').to_lowercase()) { if UNITS.contains(&u.as_str()) { out.push(format!("{num}{u}")) } }
            }
            out.push(lo); continue;
        }
        if lo.starts_with("--") { out.push(lo); continue }
        if lo.contains('/') || has_ext(&lo) {
            let p = lo.trim_start_matches("./").to_string();
            if let Some((_, f)) = p.rsplit_once('/') { if has_ext(f) { out.push(f.to_string()) } }
            out.push(p); continue;
        }
        if t.len() >= 4 && c0.is_ascii_uppercase() && t[1..].chars().all(|c| c.is_ascii_digit()) { out.push(lo); continue }
        let camel = t.chars().zip(t.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase());
        if t.contains('_') || camel {
            out.push(lo.clone());
            let mut part = String::new();
            let cs: Vec<char> = t.chars().collect();
            for (j, &c) in cs.iter().enumerate() {
                if c == '_' || (j > 0 && c.is_uppercase() && cs[j - 1].is_lowercase()) { word(&part, &mut out); part.clear() }
                if c != '_' { part.push(c) }
            }
            word(&part, &mut out); continue;
        }
        for w in lo.split(['-', '.']) { word(w, &mut out) }
    }
    out
}
/// Not a plain word: paths, identifiers, numbers, flags (a plain word is letters only, none uppercase, any script).
fn is_entity(t: &str) -> bool { !t.chars().all(|c| c.is_alphabetic() && !c.is_uppercase()) }

// ---------- alias lines ("Also known as: a = b = c; d = e") ----------
/// query weight of an alias neighbour (damped vs. a direct query term)
const ALIAS_W: f32 = 0.5;
/// The group list of an alias line, or None if `line` is not one. Prefixes: "Also known as:", "Aliases:", "AKA:".
fn alias_rest(line: &str) -> Option<&str> {
    let t = line.trim().trim_start_matches(['-', '*', '#', '>', ' ', '\t']);
    for p in ["also known as", "aliases", "aka"] {
        let Some(h) = t.get(..p.len()) else { continue };
        if !h.eq_ignore_ascii_case(p) { continue }
        if let Some(r) = t[p.len()..].trim_start_matches(['*', ' ']).strip_prefix(':') { return Some(r.trim_start_matches('*')) }
    }
    None
}
/// alias term as stored: lowercase, single-spaced, without surrounding markup/punctuation
fn alias_norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(['.', ',', '*', '`', '"', '\'', ' ']).to_lowercase()
}
/// tokens used to match an alias against a query: like `extract` but stop words are kept
fn atoks(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || ",;()[]{}\"'`<>=|*!?:@#$&^~".contains(c)).filter_map(|w| {
        let w = w.trim_matches(['.', '-']).to_lowercase();
        if w.is_empty() { None } else if w.chars().all(|c| c.is_ascii_alphabetic()) { Some(stem(&w)) } else { Some(w) }
    }).collect()
}
fn akey(s: &str) -> String { atoks(s).join(" ") }
/// Alias groups of a note: terms sorted, deduplicated by key; terms with no content word are dropped.
fn alias_groups(text: &str) -> Vec<Vec<String>> {
    let mut out = vec![];
    for line in text.lines() {
        let Some(rest) = alias_rest(line) else { continue };
        for g in rest.split(';') {
            let mut ts: Vec<String> = g.split('=').map(alias_norm).filter(|t| !t.is_empty() && !extract(t).is_empty()).collect();
            ts.sort(); ts.dedup_by(|a, b| akey(a) == akey(b));
            if !ts.is_empty() { out.push(ts) }
        }
    }
    out
}
/// text minus its alias lines (alias terms are indexed only as `a:` terms, which graph-off recall never queries)
fn strip_alias(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.lines().any(|l| alias_rest(l).is_some()) { return text.into() }
    text.split_inclusive('\n').filter(|l| alias_rest(l).is_none()).collect::<String>().into()
}
/// byte ranges of alias lines (snippets avoid centring on them)
fn alias_ranges(text: &str) -> Vec<(usize, usize)> {
    let (mut v, mut off) = (vec![], 0);
    for l in text.split_inclusive('\n') { if alias_rest(l).is_some() { v.push((off, off + l.len())) } off += l.len() }
    v
}
/// Alias graph: every term of a group links to every other term of it (union over all groups seen).
#[derive(Default)]
struct Aliases { groups: std::collections::BTreeSet<Vec<String>>, nb: BTreeMap<String, std::collections::BTreeSet<String>>, raw: HashMap<String, String> }
impl Aliases {
    fn add(&mut self, g: Vec<String>) {
        if !self.groups.insert(g.clone()) { return }
        let ks: Vec<String> = g.iter().map(|t| akey(t)).collect();
        for (i, k) in ks.iter().enumerate() {
            self.raw.entry(k.clone()).or_insert_with(|| g[i].clone());
            let e = self.nb.entry(k.clone()).or_default();
            for k2 in &ks { if k2 != k { e.insert(k2.clone()); } }
        }
    }
    fn encode(&self) -> Vec<u8> {
        let mut b = vec![];
        pv(&mut b, self.groups.len() as u64);
        for g in &self.groups { pv(&mut b, g.len() as u64); for t in g { pv(&mut b, t.len() as u64); b.extend_from_slice(t.as_bytes()) } }
        b
    }
    fn decode(&mut self, b: &[u8]) {
        if b.is_empty() { return }
        let mut p = 0;
        for _ in 0..gv(b, &mut p) {
            let g = (0..gv(b, &mut p)).map(|_| { let l = gv(b, &mut p) as usize; let t = String::from_utf8_lossy(&b[p..p + l]).into_owned(); p += l; t }).collect();
            self.add(g);
        }
    }
}

fn tf_counts(kind: u8, text: &str) -> (Vec<(String, u32)>, u32) {
    let mut ts = if kind == NOTE { extract(&strip_alias(text)) } else { extract(text) };
    if kind == NOTE { for g in alias_groups(text) { for t in g { ts.push(format!("a:{}", akey(&t))) } } }
    let n = ts.len() as u32;
    let mut m: BTreeMap<String, u32> = BTreeMap::new();
    for t in ts { *m.entry(t).or_default() += 1 }
    (m.into_iter().collect(), n)
}
/// Byte ranges of `- item` lines under a heading/line that mentions "Facts".
fn fact_lines(text: &str) -> Vec<(usize, usize)> {
    let (mut v, mut on, mut off) = (vec![], false, 0);
    for line in text.split_inclusive('\n') {
        let l = line.trim_end_matches(['\n', '\r']);
        let tl = l.trim();
        let low = l.to_lowercase();
        if let Some(i) = low.find("facts:").filter(|&i| l.len() > i + 6 && !l[i + 6..].trim().is_empty() && l.is_char_boundary(i)) {
            // inline form: "Facts: a; b; c."
            let mut o = i + 6;
            for part in l[i + 6..].split(';') {
                let s = part.trim().trim_end_matches('.');
                let lead = part.len() - part.trim_start().len();
                if !s.is_empty() && !s.to_lowercase().starts_with("none") { v.push((off + o + lead, s.len())) }
                o += part.len() + 1;
            }
            on = false;
        } else if low.contains("facts") && (tl.starts_with('#') || tl.ends_with(':')) { on = true }
        else if on {
            if let Some(r) = tl.strip_prefix("- ") {
                if !r.trim().eq_ignore_ascii_case("none") { v.push((off + (l.len() - l.trim_start().len()) + 2, r.len())) }
            } else if tl.is_empty() || tl.starts_with('#') { on = false }
        }
        off += line.len();
    }
    v
}
fn fact_key(t: &str) -> Option<String> {
    // "key: value" or "<key> is <value>"
    let k = t.split_once(':').map(|x| x.0).or_else(|| t.find(" is ").map(|i| &t[..i]))?;
    let k = k.trim().to_lowercase();
    (!k.is_empty() && k.len() <= 40).then_some(k)
}
fn norm(t: &str) -> String { t.trim().to_lowercase() }
/// Text of a fact that unpins `key`: it takes the key's place as its latest version, so the fact drops out of
/// `pinned_facts` (the log is append-only; nothing is deleted).
const UNPIN: &str = "\u{1}unpinned:";
/// fallback key: the sentence minus value-like tokens (anything with digits, _, -, ., /), so "we chose X as the Y" versions chain
fn fkey(t: &str) -> String {
    if let Some(k) = t.strip_prefix(UNPIN) { return k.to_string() }
    fact_key(t).unwrap_or_else(|| t.split_whitespace().filter(|w| w.chars().all(char::is_alphabetic)).map(str::to_lowercase).collect::<Vec<_>>().join(" "))
}

// ---------- vectors: [id u32][scale f32][crc32 u32][384 × i8], sorted by id ----------
const VREC: usize = 12 + embed::DIM;
const EMBED_CHUNK: usize = 64;
const SORT_CAP: usize = 4 << 20;
const WBUF: usize = 64 << 10;
/// hybrid weight on cosine similarity (BM25 scores of a good hit are ~5–15)
const VEC_W: f32 = 16.0;
const VEC_K: usize = 24;
/// matching state cards get a small boost
const CARD_BOOST: f32 = 1.15;
fn vrec(id: u32, v: &[f32; embed::DIM]) -> Vec<u8> {
    let s = v.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9) / 127.0;
    let mut r = Vec::with_capacity(VREC);
    r.extend(id.to_le_bytes()); r.extend(s.to_le_bytes()); r.extend([0u8; 4]);
    r.extend(v.iter().map(|x| (x / s).round().clamp(-127.0, 127.0) as i8 as u8));
    let c = crc32fast::hash(&[&r[..8], &r[12..]].concat());
    r[8..12].copy_from_slice(&c.to_le_bytes());
    r
}

// ---------- mmap'd index ----------
struct Mmap { p: *mut u8, n: usize, f: File }
impl Mmap {
    fn open(path: &Path) -> io::Result<Mmap> {
        let f = File::open(path)?;
        let n = f.metadata()?.len() as usize;
        if n == 0 { return Err(bad("empty index")) }
        let p = unsafe { libc::mmap(std::ptr::null_mut(), n, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
        if p == libc::MAP_FAILED { return Err(io::Error::last_os_error()) }
        Ok(Mmap { p: p as *mut u8, n, f })
    }
    /// Map the same file afresh over the same range (MAP_FIXED): pages touched so far leave this process's RSS
    /// (they stay in the OS page cache). Callers must hold no slices into the mapping. The open handle is used, not
    /// the path: another process may have renamed a new index over it (a remap by path mapped that one in).
    fn evict(&self) {
        unsafe { libc::mmap(self.p as *mut libc::c_void, self.n, libc::PROT_READ, libc::MAP_PRIVATE | libc::MAP_FIXED, self.f.as_raw_fd(), 0) };
    }
    fn b(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.p, self.n) } }
}
impl Drop for Mmap { fn drop(&mut self) { unsafe { libc::munmap(self.p as *mut libc::c_void, self.n); } } }

struct Index { m: Mmap, nd: u32, nt: u32, od: usize, ot: usize, os: usize, op: usize, oa: usize, ob: usize, scored: u32, sum_len: u64, oal: usize, nal: usize, eb: u32 }
impl Index {
    fn load(p: &Path) -> io::Result<Index> {
        let m = Mmap::open(p)?;
        let b = m.b();
        if b.len() < 96 || r64(b, 0) != MAGIC { return Err(bad("bad index magic")) }
        let h: Vec<u64> = (0..12).map(|i| r64(b, i * 8)).collect();
        // v1 headers were 96 B; the 112 B header adds the alias-group section (offset, length)
        let (oal, nal) = if h[4] >= 112 && b.len() >= 112 { (r64(b, 96) as usize, r64(b, 104) as usize) } else { (0, 0) };
        if oal + nal > b.len() { return Err(bad("bad alias section")) }
        Ok(Index { nd: h[1] as u32, nt: h[2] as u32, od: h[4] as usize, ot: h[5] as usize, os: h[6] as usize, op: h[7] as usize,
            oa: h[8] as usize, ob: h[9] as usize, scored: h[10] as u32, sum_len: h[11], oal, nal, eb: if h[4] >= 112 { 4 } else { 3 }, m })
    }
    fn b(&self) -> &[u8] { self.m.b() }
    fn row(&self, id: u32) -> usize { self.od + id as usize * 20 }
    fn meta(&self, id: u32) -> Meta {
        let (b, o) = (self.b(), self.row(id));
        let (kind, s) = (b[o], r32(b, o + 4));
        // a SESSION row stores its project in the session slot; other rows derive the project from their session
        let (session, project) = match kind { SESSION => (id, s), PROJECT => (NONE, id), _ if s == NONE => (NONE, NONE), _ => (s, r32(b, self.row(s) + 4)) };
        Meta { kind, role: b[o + 1], session, project, len: u16::from_le_bytes([b[o + 2], b[o + 3]]) as u32 }
    }
    fn term(&self, i: u32) -> &str {
        let (b, o) = (self.b(), self.ot + i as usize * 8);
        let (s, e) = (self.os + r32(b, o) as usize, self.os + r32(b, o + 8) as usize);
        std::str::from_utf8(&b[s..e]).unwrap_or("")
    }
    fn find(&self, t: &str) -> Option<u32> {
        let (mut lo, mut hi) = (0, self.nt);
        while lo < hi {
            let m = (lo + hi) / 2;
            match self.term(m).cmp(t) { std::cmp::Ordering::Less => lo = m + 1, std::cmp::Ordering::Greater => hi = m, _ => return Some(m) }
        }
        None
    }
    /// postings (doc, tf) and co-occurring entity neighbours (term id, weight 0..1)
    fn post(&self, i: u32) -> (Vec<(u32, u32)>, Vec<(u32, f32)>) {
        let (b, o) = (self.b(), self.ot + i as usize * 8);
        let mut p = self.op + r32(b, o + 4) as usize;
        let df = gv(b, &mut p);
        let mut d = 0u32;
        let v = (0..df).map(|_| { let x = gv(b, &mut p); d += (x >> 1) as u32; (d, if x & 1 == 1 { gv(b, &mut p) as u32 } else { 1 }) }).collect();
        let nn = gv(b, &mut p);
        let nb = (0..nn).map(|_| { let t = gv(b, &mut p) as u32; let w = b[p] as f32 / 255.0; p += 1; (t, w) }).collect();
        (v, nb)
    }
    fn adj(&self, id: u32) -> Vec<(u8, u32)> {
        let b = self.b();
        let mut p = self.oa + r32(b, self.row(id) + 16) as usize;
        let ne = gv(b, &mut p);
        let (sh, mk) = (self.eb, (1u64 << self.eb) - 1);
        let e: Vec<(u8, u32)> = (0..ne).map(|_| { let x = gv(b, &mut p); let z = (x >> sh) as i64; ((x & mk) as u8, (id as i64 + ((z >> 1) ^ -(z & 1))) as u32) }).collect();
        e
    }
    fn block(&self, k: u32) -> Buf {
        let (b, o) = (self.b(), self.ob + k as usize * 16);
        let (off, cl, ul) = (r64(b, o) as usize, r32(b, o + 8) as usize, r32(b, o + 12) as usize);
        let mut out = Buf::new(ul);
        let n = lz4_flex::block::decompress_into(&b[off..off + cl], &mut out).unwrap_or(0);
        out.n = n;
        out
    }
}

// ---------- hot (uncompacted) state, rebuilt from the log on open ----------
/// Per-doc metadata and counters only. Hot postings and hot edges live in the derived files `hot.post`
/// (`[doc u32][tf u32][len u16][term]` per term occurrence) and `hot.adj` (`[from u32][kind u8][to u32]`),
/// rebuilt from the log on open; a read call builds a transient `HotView` from them and drops it at its end.
#[derive(Default)]
struct Hot { base: u32, metas: Vec<Meta>, locs: Vec<(u32, u64, u32)>, scored: u32, sum_len: u64, sessions: usize }
#[derive(Default)]
struct HotView { post: HashMap<String, Vec<(u32, u32)>>, adj: HashMap<u32, Vec<(u8, u32)>> }
/// the shared derived files of stores before 2026-11-09 (removed on open; the hot files are private now)
const HOT_POST: &str = "hot.post";
const HOT_ADJ: &str = "hot.adj";
const LOCK: &str = "lock";
/// `Memory::gen` after a write that did not finish (a panic): the next write re-reads the whole store
const STALE: u64 = u64::MAX;

/// A derived file private to this open store: created and unlinked at once, so no other process can see or rewrite
/// it, and nothing is left behind when the process ends.
fn private_file(dir: &Path, tag: &str) -> io::Result<File> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let p = dir.join(format!(".{tag}-{}-{}.tmp", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = fs::remove_file(&p);
    let f = OpenOptions::new().read(true).append(true).create_new(true).open(&p)?;
    let _ = fs::remove_file(&p);
    Ok(f)
}
/// (device, inode, length) of a file, to tell whether another process replaced it
fn file_id(p: &Path) -> Option<(u64, u64, u64)> { fs::metadata(p).ok().map(|m| (m.dev(), m.ino(), m.len())) }

/// The store's cross-process lock (`dir/lock`): flock, re-entrant within one store, plus the generation counter.
struct Lock { f: File, depth: std::cell::Cell<u32> }
type LockRef = std::rc::Rc<Lock>;
impl Lock {
    fn open(dir: &Path) -> io::Result<LockRef> {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK))?;
        Ok(std::rc::Rc::new(Lock { f, depth: Default::default() }))
    }
    fn gen(&self) -> u64 { let mut b = [0u8; 8]; self.f.read_exact_at(&mut b, 0).map_or(0, |_| u64::from_le_bytes(b)) }
    fn bump(&self) -> u64 { let g = self.gen().wrapping_add(1) % STALE; let _ = self.f.write_all_at(&g.to_le_bytes(), 0); g }
}
/// The lock held; released when the outermost holder drops it (also when a panic unwinds through it, after a
/// generation bump so the other processes re-read what the unfinished write left).
struct Held(LockRef);
fn hold(l: &LockRef) -> io::Result<Held> {
    if l.depth.get() == 0 {
        while unsafe { libc::flock(l.f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted { return Err(e) }
        }
    }
    l.depth.set(l.depth.get() + 1);
    Ok(Held(l.clone()))
}
impl Drop for Held {
    fn drop(&mut self) {
        let d = self.0.depth.get().saturating_sub(1);
        self.0.depth.set(d);
        if d == 0 {
            if std::thread::panicking() { self.0.bump(); }
            unsafe { libc::flock(self.0.f.as_raw_fd(), libc::LOCK_UN); }
        }
    }
}

pub struct Memory {
    dir: PathBuf, idx: Option<Index>, hot: Hot,
    /// the cross-process lock (None in a worker process: its parent holds it for the job)
    lock: Option<LockRef>,
    /// the lock file's generation this snapshot matches (STALE: re-read everything before the next write)
    gen: u64,
    /// identity of the index file this snapshot mapped (another process's compaction replaces it)
    idx_id: Option<(u64, u64, u64)>,
    hpost: File, hadj: File, view: std::cell::RefCell<Option<std::rc::Rc<HotView>>>,
    segs: Vec<(u32, File)>, seg_len: u64,
    projects: HashMap<String, u32>, fact_keys: HashMap<String, (u32, String)>, alias: Aliases,
    last_turn: HashMap<u32, u32>, last_note: HashMap<u32, u32>,
    /// per project: (cards so far, newest card, newest milestone)
    cards: HashMap<u32, (u32, u32, u32)>,
    torn: bool,
    model: Option<PathBuf>, vhot: Option<File>, vec_last: Option<u32>,
    progress: Option<Box<dyn FnMut(&str, Option<f32>) + Send>>,
    /// embed pending memories at end_session (default true; needs the embedding model)
    pub auto_index: bool,
    /// add cosine similarity to recall scores when vectors exist (default true)
    pub use_vectors: bool,
    /// run heavy jobs (embedding, compaction, query embedding) in a short-lived worker process (spawn + exec,
    /// never a bare fork), so this process never maps the model or holds job buffers (default true)
    pub use_worker: bool,
    /// executable for worker processes; it must call `worker_main` when its first argument is `WORKER_ARG`.
    /// Default: `std::env::current_exe()`.
    pub worker_exe: Option<PathBuf>,
    /// highest peak RSS reported by a worker (bytes)
    worker_peak: std::cell::Cell<u64>,
    /// auto-compact after this many ended sessions (default 10)
    pub compact_every: usize,
}

type Cache = HashMap<u32, Buf>;

/// Transient byte buffer in its own anonymous mapping (unmapped on drop): big short-lived buffers never
/// enter the allocator, so they can't leave freed-but-resident pages behind in a long-lived process.
struct Buf { p: *mut u8, cap: usize, n: usize }
impl Buf {
    fn new(n: usize) -> Buf {
        let cap = n.max(1);
        let p = unsafe { libc::mmap(std::ptr::null_mut(), cap, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        assert!(p != libc::MAP_FAILED, "elisa-mem: mmap {n} bytes failed");
        Buf { p: p as *mut u8, cap, n }
    }
}
impl std::ops::Deref for Buf { type Target = [u8]; fn deref(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.p, self.n) } } }
impl std::ops::DerefMut for Buf { fn deref_mut(&mut self) -> &mut [u8] { unsafe { std::slice::from_raw_parts_mut(self.p, self.n) } } }
impl Drop for Buf { fn drop(&mut self) { unsafe { libc::munmap(self.p as *mut libc::c_void, self.cap); } } }

impl Memory {
    /// Open (or create) the store in `dir`. Takes the store's lock while it reads the log, so a torn tail it cuts
    /// off can never be another process's record still being written.
    pub fn open(dir: &Path) -> io::Result<Memory> {
        fs::create_dir_all(dir)?;
        let l = Lock::open(dir)?;
        let _h = hold(&l)?;
        // the shared derived files of older versions (now private per process)
        for f in [HOT_POST, HOT_ADJ] { let _ = fs::remove_file(dir.join(f)); }
        let mut m = Memory::load(dir, Some(l.clone()))?;
        m.gen = l.gen();
        Ok(m)
    }
    /// Open without the lock: a worker process, whose parent holds the lock for the whole job.
    fn open_unlocked(dir: &Path) -> io::Result<Memory> { Memory::load(dir, None) }

    fn load(dir: &Path, lock: Option<LockRef>) -> io::Result<Memory> {
        fs::create_dir_all(dir)?;
        let ip = dir.join("index.mg");
        let idx_id = file_id(&ip);
        let idx = if ip.exists() { Some(Index::load(&ip)?) } else { None };
        let base = idx.as_ref().map_or(0, |i| i.nd);
        let mut m = Memory { dir: dir.to_path_buf(), idx, hot: Hot { base, ..Default::default() }, lock, gen: 0, idx_id,
            hpost: private_file(dir, "hot-post")?, hadj: private_file(dir, "hot-adj")?,
            view: Default::default(), segs: vec![], seg_len: 0,
            projects: HashMap::new(), fact_keys: HashMap::new(), alias: Aliases::default(), last_turn: HashMap::new(), last_note: HashMap::new(), cards: HashMap::new(),
            torn: false, compact_every: 10, model: embed::model_dir(), vhot: None, vec_last: None, progress: None,
            auto_index: true, use_vectors: true, use_worker: true, worker_exe: std::env::current_exe().ok(), worker_peak: Default::default() };
        // projects and the latest fact per key come from the index (few, small)
        let mut cache = Cache::new();
        for id in 0..base {
            let k = m.meta(id).kind;
            if k == PROJECT { let t = m.text(id, &mut cache); m.projects.insert(t, id); }
            if k == FACT { let t = m.text(id, &mut cache); m.fact_keys.insert(fkey(&t), (id, norm(&t))); }
            if k == CARD { let pr = m.meta(id).project; m.count_card(pr, id); }
        }
        drop(cache);
        if let Some(ix) = &m.idx { let a = ix.b()[ix.oal..ix.oal + ix.nal].to_vec(); m.alias.decode(&a) }
        m.open_vectors()?;
        for n in seg_nums(dir)? {
            let f = OpenOptions::new().read(true).append(true).open(m.seg_path(n))?;
            m.segs.push((n, f));
            m.scan_seg(m.segs.len() as u32 - 1, 0)?;
        }
        if m.segs.is_empty() { m.new_seg(1)?; }
        m.settle();
        Ok(m)
    }
    /// Replay the records of segment `si` from byte `from` to its end; a torn or corrupt tail is cut off (callers
    /// hold the lock, so no record is half written by another process). Sets `seg_len` to the segment's good end.
    fn scan_seg(&mut self, si: u32, from: u64) -> io::Result<()> {
        let path = self.seg_path(self.segs[si as usize].0);
        let total = self.segs[si as usize].1.metadata()?.len();
        // stream records through a 64 KB reader (RAM bounded by the largest record, not the segment)
        let mut rd = BufReader::with_capacity(WBUF, File::open(&path)?);
        rd.seek_relative(from as i64)?;
        let (mut p, mut hdr, mut pl) = (from, [0u8; 8], Vec::new());
        while p + 8 <= total {
            if rd.read_exact(&mut hdr).is_err() { break }
            let len = r32(&hdr, 0) as u64;
            if p + 8 + len > total { break }
            pl.resize(len as usize, 0);
            if rd.read_exact(&mut pl).is_err() || crc32fast::hash(&pl) != r32(&hdr, 4) { break }
            self.replay(&pl, si, p + 8);
            p += 8 + len;
        }
        drop(rd);
        if p < total { self.segs[si as usize].1.set_len(p)?; self.segs[si as usize].1.sync_all()?; self.torn = true; }
        self.seg_len = p;
        Ok(())
    }

    /// Run a write under the store's lock. First, if another process wrote since this snapshot (the generation moved
    /// on), catch up with it (`sync`), so ids stay dense and every record lands at the offset this process records
    /// for it. Afterwards bump the generation so the other processes catch up before their next write.
    fn write<R>(&mut self, f: impl FnOnce(&mut Self) -> io::Result<R>) -> io::Result<R> {
        let Some(l) = self.lock.clone() else { return f(self) };
        let outer = l.depth.get() == 0;
        let _h = hold(&l)?;
        if !outer { return f(self) }
        if l.gen() != self.gen { self.sync()?; }
        // a panic inside leaves this snapshot unsure: the next write re-reads the whole store
        self.gen = STALE;
        let r = f(self);
        self.gen = l.bump();
        r
    }
    /// Bring this snapshot up to date with what other processes wrote (no-op when nothing changed). Returns
    /// whether anything was re-read. Reads never need it for safety (a snapshot stays consistent); call it before
    /// a read that should see the other processes' newest memories.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let Some(l) = self.lock.clone() else { return Ok(false) };
        let _h = hold(&l)?;
        if l.gen() == self.gen { return Ok(false) }
        self.sync()?;
        self.gen = l.gen();
        Ok(true)
    }
    /// Catch up with the store on disk (lock held). Same index and segment files: replay only the records appended
    /// after this snapshot's end (into the private hot files). Otherwise (another process compacted, or this
    /// snapshot is STALE) re-open the whole store.
    fn sync(&mut self) -> io::Result<()> {
        let same = self.gen != STALE && file_id(&self.dir.join("index.mg")) == self.idx_id
            && seg_nums(&self.dir)? == self.segs.iter().map(|s| s.0).collect::<Vec<_>>();
        if same && !self.segs.is_empty() {
            let si = self.segs.len() as u32 - 1;
            let from = self.seg_len;
            self.scan_seg(si, from)?;
            self.vhot = None;
            self.open_vectors()?;
            self.settle();
            Ok(())
        } else {
            self.reload()
        }
    }

    fn seg_path(&self, n: u32) -> PathBuf { self.dir.join(format!("log-{n:06}.seg")) }
    fn new_seg(&mut self, n: u32) -> io::Result<()> {
        let f = OpenOptions::new().read(true).append(true).create(true).open(self.seg_path(n))?;
        self.segs.push((n, f)); self.seg_len = 0; Ok(())
    }
    fn next(&self) -> u32 { self.hot.base + self.hot.metas.len() as u32 }

    // ----- record log -----
    fn append(&mut self, kind: u8, id: u32, a: u32, role: u8, ids: &[u32], text: &str) -> io::Result<(u32, u64, u32)> {
        let mut p = vec![kind];
        pv(&mut p, id as u64); pv(&mut p, a as u64); p.push(role); pv(&mut p, ids.len() as u64);
        for &i in ids { pv(&mut p, i as u64) }
        pv(&mut p, text.len() as u64);
        let tp = p.len();
        p.extend_from_slice(text.as_bytes());
        let mut r = Vec::with_capacity(p.len() + 8);
        r.extend((p.len() as u32).to_le_bytes()); r.extend(crc32fast::hash(&p).to_le_bytes()); r.extend(&p);
        let f = &self.segs.last().unwrap().1;
        (&*f).write_all(&r)?;
        // plain fsync (not F_FULLFSYNC): durable against process crash, cheap enough per turn
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 { return Err(io::Error::last_os_error()) }
        let loc = (self.segs.len() as u32 - 1, self.seg_len + 8 + tp as u64, text.len() as u32);
        self.seg_len += r.len() as u64;
        Ok(loc)
    }
    fn replay(&mut self, pl: &[u8], si: u32, off: u64) {
        let mut p = 1;
        let kind = pl[0];
        let id = gv(pl, &mut p) as u32; let a = gv(pl, &mut p) as u32;
        let role = pl[p]; p += 1;
        let n = gv(pl, &mut p);
        let ids: Vec<u32> = (0..n).map(|_| gv(pl, &mut p) as u32).collect();
        let tl = gv(pl, &mut p) as usize;
        let text = String::from_utf8_lossy(&pl[p..p + tl]).into_owned();
        if kind == END { if id >= self.hot.base { self.hot.sessions += 1 } return }
        if id != self.next() { return } // already in the index (crash between index rename and log delete)
        self.apply(kind, id, a, role, &ids, &text, (si, off + p as u64, tl as u32));
    }
    fn put(&mut self, kind: u8, a: u32, role: u8, ids: &[u32], text: &str) -> io::Result<u32> {
        let id = self.next();
        let loc = self.append(kind, id, a, role, ids, text)?;
        self.apply(kind, id, a, role, ids, text, loc);
        self.settle();
        Ok(id)
    }

    // ----- graph construction (shared by live writes and replay) -----
    fn edge(&mut self, a: u32, k: u8, b: u32) {
        let mut r = [0u8; 9];
        r[..4].copy_from_slice(&a.to_le_bytes()); r[4] = k; r[5..].copy_from_slice(&b.to_le_bytes());
        (&self.hadj).write_all(&r).expect("elisa-mem: hot.adj write");
        *self.view.get_mut() = None;
    }
    /// transient view of the hot postings/edges for the current call (dropped by `settle`)
    fn view(&self) -> std::rc::Rc<HotView> {
        if let Some(v) = self.view.borrow().as_ref() { return v.clone() }
        let mut v = HotView::default();
        let read = |f: &File| -> Buf { let n = f.metadata().map_or(0, |m| m.len()) as usize; let mut b = Buf::new(n); let _ = f.read_exact_at(&mut b, 0); b };
        let b = read(&self.hpost);
        let mut p = 0;
        while p + 10 <= b.len() {
            let (d, tf, l) = (r32(&b, p), r32(&b, p + 4), u16::from_le_bytes([b[p + 8], b[p + 9]]) as usize);
            if p + 10 + l > b.len() { break }
            let t = String::from_utf8_lossy(&b[p + 10..p + 10 + l]).into_owned();
            v.post.entry(t).or_default().push((d, tf));
            p += 10 + l;
        }
        drop(b);
        let b = read(&self.hadj);
        for r in b.chunks_exact(9) { v.adj.entry(r32(r, 0)).or_default().push((r[4], r32(r, 5))) }
        let v = std::rc::Rc::new(v);
        *self.view.borrow_mut() = Some(v.clone());
        v
    }
    fn push_doc(&mut self, id: u32, mut m: Meta, text: &str, loc: (u32, u64, u32)) {
        debug_assert_eq!(id, self.next());
        if scored(m.kind) {
            let (tf, n) = tf_counts(m.kind, text);
            m.len = n.min(65535);
            self.hot.scored += 1; self.hot.sum_len += n as u64;
            let mut r = Vec::with_capacity(tf.len() * 16);
            for (t, c) in tf {
                let tb = &t.as_bytes()[..t.len().min(u16::MAX as usize)];
                r.extend(id.to_le_bytes()); r.extend(c.to_le_bytes()); r.extend((tb.len() as u16).to_le_bytes()); r.extend_from_slice(tb);
            }
            (&self.hpost).write_all(&r).expect("elisa-mem: hot.post write");
            *self.view.get_mut() = None;
        }
        self.hot.metas.push(m); self.hot.locs.push(loc);
    }
    fn fact(&mut self, id: u32, session: u32, project: u32, text: &str, loc: (u32, u64, u32), src: Option<u32>) {
        self.push_doc(id, Meta { kind: FACT, role: 0, session, project, len: 0 }, text, loc);
        if let Some(s) = src { self.edge(id, E_SRC, s); self.edge(s, E_SRCOF, id) }
        if let Some((p, _)) = self.fact_keys.insert(fkey(text), (id, norm(text))) { self.edge(id, E_REPL, p); self.edge(p, E_REPLBY, id) }
    }
    fn apply(&mut self, kind: u8, id: u32, a: u32, role: u8, ids: &[u32], text: &str, loc: (u32, u64, u32)) {
        match kind {
            PROJECT => { self.push_doc(id, Meta { kind, role, session: NONE, project: id, len: 0 }, text, loc); self.projects.insert(text.to_string(), id); }
            SESSION => self.push_doc(id, Meta { kind, role, session: id, project: a, len: 0 }, text, loc),
            TURN => {
                let pr = self.meta(a).project;
                self.push_doc(id, Meta { kind, role, session: a, project: pr, len: 0 }, text, loc);
                if let Some(p) = self.last_turn.insert(a, id) { self.edge(p, E_NEXT, id); self.edge(id, E_PREV, p) }
            }
            NOTE => {
                let pr = self.meta(a).project;
                self.push_doc(id, Meta { kind, role, session: a, project: pr, len: 0 }, text, loc);
                for &t in ids { self.edge(id, E_SUMS, t); self.edge(t, E_SUMBY, id) }
                for g in alias_groups(text) { self.alias.add(g) }
                if let Some(p) = self.last_note.insert(a, id) { self.edge(id, E_REPL, p); self.edge(p, E_REPLBY, id) }
                // facts are derived deterministically from the note's Facts section (same ids on replay)
                // a line equal to the current version of its fact links to that node instead of making a new one
                let mut nid = id + 1;
                for (o, l) in fact_lines(text) {
                    let ft = &text[o..o + l];
                    match self.fact_keys.get(&fkey(ft)) {
                        Some(&(ex, ref v)) if *v == norm(ft) => { self.edge(ex, E_SRC, id); self.edge(id, E_SRCOF, ex) }
                        _ => { self.fact(nid, a, pr, ft, (loc.0, loc.1 + o as u64, l as u32), Some(id)); nid += 1 }
                    }
                }
            }
            CARD => {
                let pr = self.meta(a).project;
                self.push_doc(id, Meta { kind, role, session: a, project: pr, len: 0 }, text, loc);
                let prev = ids.first().copied().unwrap_or(NONE);
                if prev != NONE { self.edge(id, E_CPREV, prev); self.edge(prev, E_CNEXT, id) }
                for &t in ids.iter().skip(1) { self.edge(id, E_SUMS, t); self.edge(t, E_SUMBY, id) }
                let prev_ms = self.cards.get(&pr).map_or(NONE, |c| c.2);
                if self.count_card(pr, id) {
                    // milestone: skip edges to the MILESTONE cards before it on its prev chain, and to the previous milestone
                    let (mut c, mut k) = (prev, 0);
                    while c != NONE && k < MILESTONE { self.edge(id, E_SKIP, c); self.edge(c, E_SKIPBY, id); c = self.card_prev(c); k += 1 }
                    if prev_ms != NONE { self.edge(id, E_MPREV, prev_ms); self.edge(prev_ms, E_MNEXT, id) }
                }
            }
            FACT => { let pr = if a == NONE { NONE } else { self.meta(a).project }; self.fact(id, a, pr, text, loc, None) }
            _ => {}
        }
    }

    // ----- unified reads over index + hot -----
    fn meta(&self, id: u32) -> Meta {
        if id < self.hot.base { self.idx.as_ref().unwrap().meta(id) } else { self.hot.metas.get((id - self.hot.base) as usize).copied().unwrap_or_default() }
    }
    fn text(&self, id: u32, cache: &mut Cache) -> String { self.text_opt(id, cache).unwrap_or_default() }
    /// The text of `id`, None for an id this snapshot does not have (never indexes past its tables).
    fn text_opt(&self, id: u32, cache: &mut Cache) -> Option<String> {
        if id < self.hot.base {
            let ix = self.idx.as_ref().unwrap();
            let (b, o) = (ix.b(), ix.row(id));
            let (blk, bo) = (r32(b, o + 8), r32(b, o + 12) as usize);
            let d = cache.entry(blk).or_insert_with(|| ix.block(blk));
            // texts are contiguous: a doc ends where the next one starts, or at the end of its block
            let end = if id + 1 < ix.nd && r32(b, o + 28) == blk { r32(b, o + 32) as usize } else { d.len() };
            Some(String::from_utf8_lossy(d.get(bo..end).unwrap_or(&[])).into_owned())
        } else {
            let &(si, off, len) = self.hot.locs.get((id - self.hot.base) as usize)?;
            let (_, f) = self.segs.get(si as usize)?;
            let mut v = vec![0u8; len as usize];
            f.read_exact_at(&mut v, off).ok()?;
            Some(String::from_utf8_lossy(&v).into_owned())
        }
    }
    fn edges(&self, id: u32) -> Vec<(u8, u32)> {
        let mut e = if id < self.hot.base { self.idx.as_ref().unwrap().adj(id) } else { vec![] };
        if id >= self.hot.base || fs_len(&self.hadj) > 0 { if let Some(h) = self.view().adj.get(&id) { e.extend(h) } }
        e
    }
    fn latest(&self, mut id: u32) -> u32 { while let Some(&(_, t)) = self.edges(id).iter().find(|e| e.0 == E_REPLBY) { id = t } id }
    fn postings(&self, t: &str) -> Vec<(u32, u32)> {
        let mut v = self.idx.as_ref().and_then(|ix| ix.find(t).map(|i| ix.post(i).0)).unwrap_or_default();
        if !self.hot.metas.is_empty() { if let Some(h) = self.view().post.get(t) { v.extend(h) } }
        v
    }

    /// count a card of project `pr`; true if it is a milestone
    fn count_card(&mut self, pr: u32, id: u32) -> bool {
        let c = self.cards.entry(pr).or_insert((0, NONE, NONE));
        c.0 += 1; c.1 = id;
        let ms = c.0 % MILESTONE == 0;
        if ms { c.2 = id }
        ms
    }
    fn card_prev(&self, id: u32) -> u32 { self.edges(id).iter().find(|e| e.0 == E_CPREV).map_or(NONE, |e| e.1) }

    // ----- public API -----
    /// A state card: a small node chained to `prev` (any earlier card) and linked to the `turns` it covers.
    /// Every 10th card of a project is a milestone with skip edges to the 10 cards before it on its chain
    /// and to the previous milestone.
    pub fn add_card(&mut self, session: u64, text: &str, prev: Option<u64>, turns: &[u64]) -> io::Result<u64> {
        let mut ids = vec![prev.map_or(NONE, |p| p as u32)];
        ids.extend(turns.iter().map(|&t| t as u32));
        self.write(|m| Ok(m.put(CARD, session as u32, 0, &ids, text)? as u64))
    }
    /// Newest card of `project` across all sessions.
    pub fn latest_card(&self, project: &str) -> Option<(u64, String)> {
        let p = *self.projects.get(project)?;
        let &(_, id, _) = self.cards.get(&p)?;
        let t = self.text_opt(id, &mut Cache::new());
        self.settle();
        Some((id as u64, t?))
    }
    /// Up to `n` cards walking back from `id` (inclusive) along prev links; from a milestone, its skip list
    /// yields the next 10 cards in one adjacency read instead of 10 hops.
    pub fn card_chain(&self, id: u64, n: usize) -> Vec<(u64, String)> {
        let mut out: Vec<u32> = vec![];
        let mut cur = id as u32;
        if cur >= self.next() || self.meta(cur).kind != CARD { return vec![] }
        while cur != NONE && out.len() < n {
            out.push(cur);
            let es = self.edges(cur);
            let mut skip: Vec<u32> = es.iter().filter(|e| e.0 == E_SKIP).map(|e| e.1).collect();
            if n - out.len() >= MILESTONE as usize && skip.len() == MILESTONE as usize {
                skip.sort_unstable_by(|a, b| b.cmp(a)); // newest first = prev order
                let last = *skip.last().unwrap();
                out.extend(skip);
                cur = self.card_prev(last);
            } else {
                cur = es.iter().find(|e| e.0 == E_CPREV).map_or(NONE, |e| e.1);
            }
        }
        out.truncate(n);
        let mut cache = Cache::new();
        let v = out.into_iter().map(|c| (c as u64, self.text(c, &mut cache))).collect();
        self.settle();
        v
    }
    pub fn begin_session(&mut self, project: &str) -> u64 {
        self.try_begin_session(project).expect("elisa-mem: log write")
    }
    /// `begin_session` that returns a write error instead of panicking.
    pub fn try_begin_session(&mut self, project: &str) -> io::Result<u64> {
        self.write(|m| {
            let p = match m.projects.get(project) { Some(&p) => p, None => m.put(PROJECT, NONE, 0, &[], project)? };
            Ok(m.put(SESSION, p, 0, &[], "")? as u64)
        })
    }
    pub fn add_turn(&mut self, session: u64, role: Role, text: &str) -> io::Result<u64> {
        self.write(|m| Ok(m.put(TURN, session as u32, (role == Role::Assistant) as u8, &[], text)? as u64))
    }
    pub fn add_note(&mut self, session: u64, text: &str, summarised: &[u64]) -> io::Result<u64> {
        let ids: Vec<u32> = summarised.iter().map(|&i| i as u32).collect();
        self.write(|m| Ok(m.put(NOTE, session as u32, 0, &ids, text)? as u64))
    }
    pub fn pin_fact(&mut self, session: u64, text: &str) -> io::Result<u64> { self.write(|m| Ok(m.put(FACT, session as u32, 0, &[], text)? as u64)) }
    pub fn end_session(&mut self, session: u64) -> io::Result<()> {
        self.write(|m| {
            m.append(END, session as u32, NONE, 0, &[], "")?;
            m.last_turn.remove(&(session as u32)); m.last_note.remove(&(session as u32));
            m.hot.sessions += 1;
            if m.auto_index { m.index_pending()?; }
            if m.compact_every > 0 && m.hot.sessions >= m.compact_every { m.compact()? }
            Ok(())
        })
    }

    /// Merge the index and the hot log into a new index segment (lossless), then drop the hot logs.
    /// Embeds pending memories first and merges the vector segments. Memory stays bounded regardless of store
    /// size: docs stream through fixed buffers, postings/co-occurrence go through external sorts (SORT_CAP each),
    /// the old index is re-mapped periodically so its touched pages leave the RSS, and sections are assembled
    /// from temp files with buffered copies.
    pub fn compact(&mut self) -> io::Result<()> { self.write(|m| m.compact_now()) }
    fn compact_now(&mut self) -> io::Result<()> {
        if self.use_worker {
            let d = self.dir_arg()?;
            self.spawn_job(&["compact", &d])?;
            return self.reload();
        }
        self.index_pending()?;
        if self.hot.metas.is_empty() && fs_len(&self.hadj) == 0 { self.hot.sessions = 0; return self.merge_vectors() }
        const STEPS: usize = 7;
        let n = self.next();
        let tmpd = self.dir.join("compact.tmp");
        let _ = fs::remove_dir_all(&tmpd);
        fs::create_dir_all(&tmpd)?;
        let bw = |name: &str| -> io::Result<BufWriter<File>> { Ok(BufWriter::with_capacity(WBUF, File::create(tmpd.join(name))?)) };
        let ip = self.dir.join("index.mg");

        // 1. stream docs in id order: text blocks, rows, adjacency; postings tuples into an external sort
        let (mut frows, mut fadj, mut fblk, mut fdir) = (bw("rows")?, bw("adj")?, bw("blocks")?, bw("blkdir")?);
        let (mut nblk, mut blk_off, mut adj_len, mut nsc, mut sum_len) = (0u32, 0u64, 0u64, 0u32, 0u64);
        let mut cur: Vec<u8> = Vec::with_capacity(BLOCK + 4096);
        let mut sa = extsort::Sorter::new(&tmpd, "post", SORT_CAP);
        let mut cache = Cache::new();
        let mut rec = Vec::new();
        for id in 0..n {
            if id % 1024 == 0 {
                self.report(&format!("compacting memory 1/{STEPS}"), Some(id as f32 / n as f32));
                // drop the old index's touched pages from our RSS
                if id > 0 && id < self.hot.base { self.idx = Some(Index::load(&ip)?) }
            }
            if cache.len() > 2 { cache.clear() }
            let m = self.meta(id);
            let t = self.text(id, &mut cache);
            frows.write_all(&[m.kind, m.role])?; frows.write_all(&(m.len.min(65535) as u16).to_le_bytes())?;
            for v in [if m.kind == SESSION { m.project } else { m.session }, nblk, cur.len() as u32, adj_len as u32] { frows.write_all(&v.to_le_bytes())? }
            cur.extend_from_slice(t.as_bytes());
            if cur.len() >= BLOCK {
                let c = lz4_flex::block::compress(&cur);
                fdir.write_all(&blk_off.to_le_bytes())?; fdir.write_all(&(c.len() as u32).to_le_bytes())?; fdir.write_all(&(cur.len() as u32).to_le_bytes())?;
                fblk.write_all(&c)?; blk_off += c.len() as u64; nblk += 1; cur.clear();
            }
            if scored(m.kind) {
                let (tf, len) = tf_counts(m.kind, &t);
                nsc += 1; sum_len += len as u64;
                for (term, c) in tf {
                    rec.clear(); rec.extend_from_slice(term.as_bytes()); rec.push(0); rec.extend(id.to_be_bytes()); rec.extend(c.to_be_bytes());
                    sa.push(&rec)?;
                }
            }
            let es = self.edges(id);
            rec.clear(); pv(&mut rec, es.len() as u64);
            for (k, t) in es { let dl = t as i64 - id as i64; pv(&mut rec, (((dl << 1) ^ (dl >> 63)) as u64) << 4 | k as u64) }
            fadj.write_all(&rec)?; adj_len += rec.len() as u64;
        }
        if !cur.is_empty() {
            let c = lz4_flex::block::compress(&cur);
            fdir.write_all(&blk_off.to_le_bytes())?; fdir.write_all(&(c.len() as u32).to_le_bytes())?; fdir.write_all(&(cur.len() as u32).to_le_bytes())?;
            fblk.write_all(&c)?; nblk += 1;
        }
        drop(cur); drop(cache);
        for w in [&mut frows, &mut fadj, &mut fblk, &mut fdir] { w.flush()? }
        drop((frows, fadj, fblk, fdir));

        // 2. merged postings stream → term strings, postings bodies, per-term meta; rare terms' docs → pair source sort
        self.report(&format!("compacting memory 2/{STEPS}"), None);
        let rare = 3.max(nsc / 50);
        let (mut fstr, mut fp0, mut fmeta) = (bw("strs")?, bw("pst0")?, bw("tmeta")?);
        let mut sb = extsort::Sorter::new(&tmpd, "pdoc", SORT_CAP);
        let mut ma = sa.finish()?;
        let (mut nt, mut str_off, mut p0_off) = (0u32, 0u32, 0u64);
        let (mut term, mut df, mut last, mut p0_len, mut docs): (Vec<u8>, u32, u32, u32, Vec<u32>) = (vec![], 0, 0, 0, vec![]);
        let mut body = Vec::new();
        let end_term = |term: &[u8], df: u32, p0_len: u32, docs: &[u32], nt: &mut u32, str_off: &mut u32, p0_off: &mut u64, fstr: &mut BufWriter<File>, fmeta: &mut BufWriter<File>, sb: &mut extsort::Sorter| -> io::Result<()> {
            fstr.write_all(term)?;
            for v in [*str_off, term.len() as u32] { fmeta.write_all(&v.to_le_bytes())? }
            fmeta.write_all(&p0_off.to_le_bytes())?; fmeta.write_all(&p0_len.to_le_bytes())?; fmeta.write_all(&df.to_le_bytes())?;
            if (2..=rare).contains(&df) { for &d in docs { let mut r = [0u8; 12]; r[..4].copy_from_slice(&d.to_be_bytes()); r[4..8].copy_from_slice(&nt.to_be_bytes()); r[8..].copy_from_slice(&df.to_be_bytes()); sb.push(&r)? } }
            *str_off += term.len() as u32; *p0_off += p0_len as u64; *nt += 1;
            Ok(())
        };
        while let Some(r) = ma.next()? {
            let k = r.len() - 9;
            let (t, doc, c) = (&r[..k], u32::from_be_bytes(r[k + 1..k + 5].try_into().unwrap()), u32::from_be_bytes(r[k + 5..].try_into().unwrap()));
            if t != term.as_slice() {
                if df > 0 { end_term(&term, df, p0_len, &docs, &mut nt, &mut str_off, &mut p0_off, &mut fstr, &mut fmeta, &mut sb)? }
                term.clear(); term.extend_from_slice(t); df = 0; last = 0; p0_len = 0; docs.clear();
            }
            body.clear();
            pv(&mut body, ((doc - last) as u64) << 1 | (c > 1) as u64); if c > 1 { pv(&mut body, c as u64) }
            fp0.write_all(&body)?; p0_len += body.len() as u32;
            last = doc; df += 1;
            if docs.len() <= rare as usize { docs.push(doc) }
        }
        if df > 0 { end_term(&term, df, p0_len, &docs, &mut nt, &mut str_off, &mut p0_off, &mut fstr, &mut fmeta, &mut sb)? }
        drop(ma); drop(docs);
        for w in [&mut fstr, &mut fp0, &mut fmeta] { w.flush()? }
        drop((fstr, fp0, fmeta));

        // 3. per doc: its rare terms (ascending id, first 24) → co-occurring pairs
        self.report(&format!("compacting memory 3/{STEPS}"), None);
        let mut sc = extsort::Sorter::new(&tmpd, "pair", SORT_CAP);
        let mut mb = sb.finish()?;
        let mut grp: Vec<(u32, u32)> = vec![];
        let mut gdoc = u32::MAX;
        let emit = |grp: &mut Vec<(u32, u32)>, sc: &mut extsort::Sorter| -> io::Result<()> {
            let r = &grp[..grp.len().min(24)];
            for i in 0..r.len() { for j in i + 1..r.len() {
                let mut x = [0u8; 16];
                for (o, v) in [r[i].0, r[j].0, r[i].1, r[j].1].iter().enumerate() { x[o * 4..o * 4 + 4].copy_from_slice(&v.to_be_bytes()) }
                sc.push(&x)?;
            }}
            grp.clear(); Ok(())
        };
        while let Some(r) = mb.next()? {
            let (d, t, f) = (u32::from_be_bytes(r[..4].try_into().unwrap()), u32::from_be_bytes(r[4..8].try_into().unwrap()), u32::from_be_bytes(r[8..].try_into().unwrap()));
            if d != gdoc { emit(&mut grp, &mut sc)?; gdoc = d }
            grp.push((t, f));
        }
        emit(&mut grp, &mut sc)?;
        drop(mb);

        // 4. count pairs → weighted neighbour candidates, sorted by (term, weight desc, other)
        self.report(&format!("compacting memory 4/{STEPS}"), None);
        let mut sd = extsort::Sorter::new(&tmpd, "nb", SORT_CAP);
        let mut mc = sc.finish()?;
        let (mut prev, mut cnt): (Option<[u8; 16]>, u32) = (None, 0);
        let flush_pair = |p: &[u8; 16], c: u32, sd: &mut extsort::Sorter| -> io::Result<()> {
            if c < 2 { return Ok(()) }
            let g = |o: usize| u32::from_be_bytes(p[o..o + 4].try_into().unwrap());
            let (a, b, da, db) = (g(0), g(4), g(8), g(12));
            let w = c as f32 / ((da * db) as f32).sqrt();
            for (x, y) in [(a, b), (b, a)] {
                let mut r = [0u8; 12];
                r[..4].copy_from_slice(&x.to_be_bytes()); r[4..8].copy_from_slice(&(!w.to_bits()).to_be_bytes()); r[8..].copy_from_slice(&y.to_be_bytes());
                sd.push(&r)?;
            }
            Ok(())
        };
        while let Some(r) = mc.next()? {
            let r: [u8; 16] = r[..16].try_into().unwrap();
            if prev.map_or(false, |p| p[..8] == r[..8]) { cnt += 1; continue }
            if let Some(p) = prev { flush_pair(&p, cnt, &mut sd)? }
            prev = Some(r); cnt = 1;
        }
        if let Some(p) = prev { flush_pair(&p, cnt, &mut sd)? }
        drop(mc);

        // 5. term table + final postings section (df, bodies, top-6 neighbours)
        self.report(&format!("compacting memory 5/{STEPS}"), None);
        let (mut fterms, mut fpst) = (bw("terms")?, bw("pst")?);
        let mut rmeta = BufReader::with_capacity(WBUF, File::open(tmpd.join("tmeta"))?);
        let mut rp0 = BufReader::with_capacity(WBUF, File::open(tmpd.join("pst0"))?);
        let mut md = sd.finish()?;
        let mut nbr = md.next()?;
        let mut pst_len = 0u64;
        let mut buf = vec![0u8; WBUF];
        for tid in 0..nt {
            let mut mrow = [0u8; 24]; rmeta.read_exact(&mut mrow)?;
            let so = r32(&mrow, 0); let p0l = r32(&mrow, 16) as usize; let df = r32(&mrow, 20);
            fterms.write_all(&so.to_le_bytes())?; fterms.write_all(&(pst_len as u32).to_le_bytes())?;
            body.clear(); pv(&mut body, df as u64); fpst.write_all(&body)?; pst_len += body.len() as u64;
            let mut left = p0l;
            while left > 0 { let k = left.min(buf.len()); rp0.read_exact(&mut buf[..k])?; fpst.write_all(&buf[..k])?; left -= k }
            pst_len += p0l as u64;
            let mut l: Vec<(u32, f32)> = vec![];
            while let Some(r) = nbr.as_ref().filter(|r| u32::from_be_bytes(r[..4].try_into().unwrap()) == tid) {
                if l.len() < 6 { l.push((u32::from_be_bytes(r[8..12].try_into().unwrap()), f32::from_bits(!u32::from_be_bytes(r[4..8].try_into().unwrap())))) }
                nbr = md.next()?;
            }
            body.clear(); pv(&mut body, l.len() as u64);
            for &(t, w) in &l { pv(&mut body, t as u64); body.push((w.min(1.0) * 255.0).round() as u8) }
            fpst.write_all(&body)?; pst_len += body.len() as u64;
        }
        fterms.write_all(&str_off.to_le_bytes())?; fterms.write_all(&(pst_len as u32).to_le_bytes())?; // sentinel
        drop(md); drop((rmeta, rp0));
        fterms.flush()?; fpst.flush()?; drop((fterms, fpst));

        // 6. assemble index.tmp from the section files
        self.report(&format!("compacting memory 6/{STEPS}"), None);
        let sz = |f: &str| fs::metadata(tmpd.join(f)).map(|m| m.len() as usize);
        let al = self.alias.encode();
        let od = 112; let ot = od + n as usize * 20; let os = ot + sz("terms")?; let op = os + sz("strs")?;
        let oa = op + sz("pst")?; let ob = oa + sz("adj")?; let odata = ob + nblk as usize * 16; let oal = odata + sz("blocks")?;
        if std::env::var_os("ELISA_MEM_DEBUG").is_some() { eprintln!("compact: docs {n} terms {nt} strs {} post {} adj {} blocks {nblk}/{} alias {}", sz("strs")?, sz("pst")?, sz("adj")?, sz("blocks")?, al.len()) }
        let tmp = self.dir.join("index.tmp");
        {
            let mut out = BufWriter::with_capacity(WBUF, File::create(&tmp)?);
            for v in [MAGIC, n as u64, nt as u64, nblk as u64, od as u64, ot as u64, os as u64, op as u64, oa as u64, ob as u64, nsc as u64, sum_len, oal as u64, al.len() as u64] { out.write_all(&v.to_le_bytes())? }
            for f in ["rows", "terms", "strs", "pst", "adj"] { io::copy(&mut BufReader::with_capacity(WBUF, File::open(tmpd.join(f))?), &mut out)?; }
            let mut rd = BufReader::with_capacity(WBUF, File::open(tmpd.join("blkdir"))?);
            for _ in 0..nblk { let mut e = [0u8; 16]; rd.read_exact(&mut e)?; out.write_all(&(odata as u64 + r64(&e, 0)).to_le_bytes())?; out.write_all(&e[8..])? }
            io::copy(&mut BufReader::with_capacity(WBUF, File::open(tmpd.join("blocks"))?), &mut out)?;
            out.write_all(&al)?;
            out.flush()?; out.get_ref().sync_all()?;
        }
        self.idx = None;
        fs::rename(&tmp, &ip)?;
        self.idx = Some(Index::load(&ip)?);
        self.idx_id = file_id(&ip);
        let _ = fs::remove_dir_all(&tmpd);
        // logs are now redundant (replay skips ids < index size even if this is interrupted)
        let last = self.segs.last().map_or(0, |s| s.0);
        for (k, _) in std::mem::take(&mut self.segs) { let _ = fs::remove_file(self.seg_path(k)); }
        self.hot = Hot { base: n, ..Default::default() };
        self.hpost.set_len(0)?; self.hadj.set_len(0)?;
        *self.view.get_mut() = None;
        self.new_seg(last + 1)?;
        // 7. vectors
        self.report(&format!("compacting memory 7/{STEPS}"), None);
        self.merge_vectors()?;
        self.report("compacting memory done", Some(1.0));
        Ok(())
    }

    // ----- short-lived workers -----
    /// Run a job in a worker process (see `run_worker`); progress goes to `set_progress`.
    fn spawn_job(&mut self, args: &[&str]) -> io::Result<String> {
        let mut prog = self.progress.take();
        let r = run_worker(self.worker_exe.as_deref(), args, &mut |w, f| if let Some(p) = prog.as_mut() { p(w, f) });
        self.progress = prog;
        let (res, rss) = r?;
        self.worker_peak.set(self.worker_peak.get().max(rss));
        Ok(res)
    }
    fn dir_arg(&self) -> io::Result<String> { self.dir.to_str().map(str::to_string).ok_or_else(|| bad("elisa-mem: store path is not UTF-8")) }
    /// re-open after a worker rewrote the files (keeps settings and the progress callback)
    fn reload(&mut self) -> io::Result<()> {
        let mut n = Memory::load(&self.dir, self.lock.clone())?;
        n.gen = self.gen;
        n.compact_every = self.compact_every; n.auto_index = self.auto_index; n.use_vectors = self.use_vectors;
        n.use_worker = self.use_worker; n.worker_exe = self.worker_exe.take(); n.worker_peak = self.worker_peak.clone(); n.progress = self.progress.take();
        n.last_turn = std::mem::take(&mut self.last_turn); n.last_note = std::mem::take(&mut self.last_note);
        *self = n;
        Ok(())
    }
    /// end of a public call: drop the index pages it touched from RSS
    fn settle(&self) {
        *self.view.borrow_mut() = None;
        if let Some(ix) = &self.idx { ix.m.evict() }
        trim_heap();
    }
    /// (id, cosine) of the nearest vectors to `query`; embedding + scan run in a worker process if `use_worker`
    fn vector_hits(&self, model: &Path, query: &str) -> Vec<(u32, f32)> {
        if !self.use_worker { return query_vectors(&self.dir, model, query, VEC_K).unwrap_or_default() }
        let (Some(d), Some(md)) = (self.dir.to_str(), model.to_str()) else { return vec![] };
        match run_worker(self.worker_exe.as_deref(), &["query", d, md, &VEC_K.to_string(), query], &mut |_, _| {}) {
            Ok((s, rss)) => {
                self.worker_peak.set(self.worker_peak.get().max(rss));
                s.split_whitespace().filter_map(|x| { let (d, c) = x.split_once(':')?; Some((d.parse().ok()?, c.parse().ok()?)) }).collect()
            }
            Err(e) => { if std::env::var_os("ELISA_MEM_DEBUG").is_some() { eprintln!("{e}") } vec![] }
        }
    }
    /// Current pinned facts of `project` (latest version per key, oldest first), including facts with no project.
    pub fn pinned_facts(&self, project: &str) -> Vec<String> {
        let proj = self.projects.get(project).copied();
        let mut ids: Vec<u32> = self.fact_keys.values().map(|v| v.0)
            .filter(|&id| { let p = self.meta(id).project; p == NONE || Some(p) == proj }).collect();
        ids.sort_unstable();
        let mut cache = Cache::new();
        let v = ids.into_iter().map(|id| self.text(id, &mut cache)).filter(|t| !t.starts_with(UNPIN)).collect();
        self.settle();
        v
    }
    /// Unpin every current fact of `project` (and facts with no project); returns how many were unpinned.
    pub fn unpin_all(&mut self, session: u64, project: &str) -> io::Result<usize> {
        self.write(|m| {
            let proj = m.projects.get(project).copied();
            let keys: Vec<String> = m.fact_keys.iter()
                .filter(|(_, v)| { let p = m.meta(v.0).project; (p == NONE || Some(p) == proj) && !v.1.starts_with(UNPIN) })
                .map(|(k, _)| k.clone()).collect();
            for k in &keys { m.put(FACT, session as u32, 0, &[], &format!("{UNPIN}{k}"))?; }
            Ok(keys.len())
        })
    }

    // ----- vectors -----
    fn report(&mut self, what: &str, frac: Option<f32>) { if let Some(f) = self.progress.as_mut() { f(what, frac) } }
    fn vec_paths(&self) -> (PathBuf, PathBuf) { vec_paths(&self.dir) }
    /// Open the hot vector segment: drop a torn tail, drop it entirely if a crash left it already merged.
    fn open_vectors(&mut self) -> io::Result<()> {
        let (cp, hp) = self.vec_paths();
        let last_of = |p: &Path| -> Option<u32> {
            let f = File::open(p).ok()?; let n = f.metadata().ok()?.len() / VREC as u64;
            if n == 0 { return None }
            let mut b = [0u8; 4]; f.read_exact_at(&mut b, (n - 1) * VREC as u64).ok()?; Some(u32::from_le_bytes(b))
        };
        let cold_last = last_of(&cp);
        let f = OpenOptions::new().read(true).append(true).create(true).open(&hp)?;
        let len = f.metadata()?.len();
        let mut good = 0u64;
        let mut r = vec![0u8; VREC];
        while good + VREC as u64 <= len {
            f.read_exact_at(&mut r, good)?;
            if crc32fast::hash(&[&r[..8], &r[12..]].concat()) != r32(&r, 8) { break }
            if good == 0 && cold_last.map_or(false, |c| r32(&r, 0) <= c) { good = 0; break } // already merged
            good += VREC as u64;
        }
        if good < len { f.set_len(good)?; f.sync_all()?; }
        self.vec_last = last_of(&hp).or(cold_last);
        self.vhot = Some(f);
        Ok(())
    }
    fn merge_vectors(&mut self) -> io::Result<()> {
        let (cp, hp) = self.vec_paths();
        if fs::metadata(&hp).map_or(0, |m| m.len()) == 0 { return Ok(()) }
        let tmp = self.dir.join("vectors.tmp");
        {
            let mut out = BufWriter::with_capacity(WBUF, File::create(&tmp)?);
            if cp.exists() { io::copy(&mut BufReader::with_capacity(WBUF, File::open(&cp)?), &mut out)?; }
            io::copy(&mut BufReader::with_capacity(WBUF, File::open(&hp)?), &mut out)?;
            out.flush()?; out.get_ref().sync_all()?;
        }
        fs::rename(&tmp, &cp)?;
        self.vhot = None;
        fs::remove_file(&hp)?;
        self.open_vectors()
    }
    /// Embed every turn/note/fact that has no vector yet (batched; layers streamed from disk). Returns how many.
    /// Called by end_session (if `auto_index`) and compact. No-op without the model.
    pub fn index_pending(&mut self) -> io::Result<usize> {
        if self.model.is_none() { return Ok(0) }
        self.write(|m| m.index_now())
    }
    fn index_now(&mut self) -> io::Result<usize> {
        let Some(dir) = self.model.clone() else { return Ok(0) };
        if self.use_worker {
            if !(self.vec_last.map_or(0, |x| x + 1)..self.next()).any(|i| scored(self.meta(i).kind)) { self.settle(); return Ok(0) }
            let d = self.dir_arg()?;
            let r = self.spawn_job(&["index", &d]);
            // the worker appended to vectors.hot (and rebuilt the derived hot files from the log): re-read
            self.vhot = None;
            self.open_vectors()?;
            self.settle();
            return Ok(r?.trim().parse().unwrap_or(0));
        }
        let start = self.vec_last.map_or(0, |x| x + 1);
        let ids: Vec<u32> = (start..self.next()).filter(|&i| scored(self.meta(i).kind)).collect();
        if ids.is_empty() { return Ok(0) }
        let total = ids.len();
        let what = format!("indexing {total} memories");
        self.report(&what, Some(0.0));
        let mut cache = Cache::new();
        let mut done = 0;
        for chunk in ids.chunks(EMBED_CHUNK) {
            let texts: Vec<String> = chunk.iter().map(|&i| { if cache.len() > 2 { cache.clear() } self.text(i, &mut cache) }).collect();
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            let mut prog = self.progress.take();
            let vs = embed::embed(&dir, &self.dir.join("vocab.bin"), &refs, &mut |k| if let Some(f) = prog.as_mut() { f(&what, Some((done + k) as f32 / total as f32)) });
            self.progress = prog;
            let vs = vs?;
            let mut out = Vec::with_capacity(chunk.len() * VREC);
            for (&id, v) in chunk.iter().zip(&vs) { out.extend(vrec(id, v)) }
            let f = self.vhot.as_ref().unwrap();
            (&*f).write_all(&out)?;
            if unsafe { libc::fsync(f.as_raw_fd()) } != 0 { return Err(io::Error::last_os_error()) }
            self.vec_last = chunk.last().copied();
            done += chunk.len();
        }
        Ok(total)
    }
    /// The embedder holds nothing between calls (weights stream from disk per batch), so this is a no-op;
    /// kept so callers can state intent.
    pub fn release_embedder(&mut self) {}
    /// Progress callback for steps that can take > ~100 ms (embedding, compaction): (what, fraction done).
    pub fn set_progress(&mut self, f: Box<dyn FnMut(&str, Option<f32>) + Send>) { self.progress = Some(f) }
    pub fn stats(&self) -> Stats {
        let mut s = Stats { segments: self.segs.len() + self.idx.is_some() as usize, torn_tail: self.torn, worker_peak: self.worker_peak.get(), ..Default::default() };
        for id in 0..self.next() {
            match self.meta(id).kind { CARD => s.cards += 1, TURN => s.turns += 1, NOTE => s.notes += 1, FACT => s.facts += 1, SESSION => s.sessions += 1, _ => {} }
        }
        let ix_terms = self.idx.as_ref().map_or(0, |i| i.nt as usize);
        let hv = self.view();
        s.entities = ix_terms + hv.post.keys().filter(|t| self.idx.as_ref().map_or(true, |i| i.find(t).is_none())).count();
        let ix_edges: usize = self.idx.as_ref().map_or(0, |ix| (0..ix.nd).map(|i| ix.adj(i).len()).sum());
        s.edges = (ix_edges + hv.adj.values().map(|v| v.len()).sum::<usize>()) / 2;
        drop(hv);
        s.cold_bytes = fs::metadata(self.dir.join("index.mg")).map_or(0, |m| m.len());
        let (cp, hp) = self.vec_paths();
        s.vector_bytes = [cp, hp].iter().map(|p| fs::metadata(p).map_or(0, |m| m.len())).sum();
        s.vectors = s.vector_bytes as usize / VREC;
        s.hot_bytes = self.segs.iter().map(|(k, _)| fs::metadata(self.seg_path(*k)).map_or(0, |m| m.len())).sum();
        self.settle();
        s
    }

    /// BM25 + entity overlap + recency/project bonus; `graph` adds co-occurrence query expansion,
    /// 1-hop expansion (paired turn, summarising note, latest replacement, ≥2 shared rare entities)
    /// and demotes superseded notes/facts. Returns top `k` with text capped at ~300 tokens total.
    /// Whether memory `id` belongs to `project` (memories with no project, e.g. global ones, count as in it).
    /// For a turn: (is a user turn, the other half of its exchange + the cards covering it). None for other kinds.
    pub fn exchange(&self, id: u64) -> Option<(bool, Vec<u64>)> {
        if id >= self.next() as u64 { return None }
        let m = self.meta(id as u32);
        if m.kind != TURN { return None }
        let user = m.role == 0;
        let half = if user { E_NEXT } else { E_PREV };
        let rel = self.edges(id as u32).into_iter().filter(|&(k, t)| {
            (k == E_NEXT || k == E_PREV || k == E_SUMBY) && t < self.next() && {
                let tm = self.meta(t);
                (k == half && tm.kind == TURN && tm.role != m.role) || (k == E_SUMBY && tm.kind == CARD)
            }
        }).map(|(_, t)| t as u64).collect();
        self.settle();
        Some((user, rel))
    }
    pub fn in_project(&self, id: u64, project: &str) -> bool {
        if id >= self.next() as u64 { return false }
        let p = self.meta(id as u32).project;
        p == NONE || self.projects.get(project) == Some(&p)
    }
    pub fn recall(&self, query: &str, project: &str, k: usize, graph: bool) -> Vec<Hit> {
        let proj = self.projects.get(project).copied().unwrap_or(NONE);
        let mut cache = Cache::new();
        let n = self.next().max(1) as f32;
        let nd = (self.idx.as_ref().map_or(0, |i| i.scored) + self.hot.scored).max(1) as f32;
        let avg = (self.idx.as_ref().map_or(0, |i| i.sum_len) + self.hot.sum_len) as f32 / nd;
        let mut qt: Vec<(String, f32)> = vec![];
        for t in extract(query) { if !qt.iter().any(|q| q.0 == t) { qt.push((t, 1.0)) } }
        // the query's own terms (expansions below are not part of `support`)
        let n_direct = qt.len();
        if graph {
            if let Some(ix) = &self.idx {
                // co-occurrence expansion only from entity terms (plain words expand into noise)
                for (t, _) in qt.clone() {
                    if !is_entity(&t) { continue }
                    let Some(i) = ix.find(&t) else { continue };
                    for (nbt, w) in ix.post(i).1 { let s = ix.term(nbt); if !qt.iter().any(|q| q.0 == s) { qt.push((s.to_string(), 0.3 * w)) } }
                }
            }
        }
        if graph && !self.alias.nb.is_empty() {
            // alias edges: an alias named in the query pulls in its group's other terms, damped
            let qa: std::collections::HashSet<String> = atoks(query).into_iter().collect();
            let hits: Vec<&String> = self.alias.nb.keys().filter(|k| k.split(' ').all(|t| qa.contains(t))).collect();
            let mut push = |t: String, w: f32| if !qt.iter().any(|q| q.0 == t) { qt.push((t, w)) };
            for k in &hits { push(format!("a:{k}"), 1.0) }
            for k in hits {
                for n in &self.alias.nb[k] {
                    push(format!("a:{n}"), ALIAS_W);
                    for t in extract(&self.alias.raw[n]) { push(t, ALIAS_W) }
                }
            }
        }
        let mut sc: HashMap<u32, f32> = HashMap::new();
        let mut df_of: HashMap<String, f32> = HashMap::new();
        // support: idf mass of the query's own terms each doc contains, over the idf mass of all of them (a term
        // the store has never seen counts with df 0: it is the most specific part of the query and nothing has it)
        let idf_of = |df: f32| ((nd - df + 0.5) / (df + 0.5) + 1.0).ln();
        let mut sup: HashMap<u32, f32> = HashMap::new();
        let mut sup_total = 0.0f32;
        for (i, (t, w)) in qt.iter().enumerate() {
            let ps = self.postings(t);
            let direct = i < n_direct;
            if direct { sup_total += idf_of(ps.len() as f32) }
            if ps.is_empty() { continue }
            let df = ps.len() as f32;
            df_of.insert(t.clone(), df);
            let idf = idf_of(df);
            let ent = if is_entity(t) { 1.0 } else { 0.0 };
            let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for (d, tf) in ps {
                let (tf, dl) = (tf as f32, self.meta(d).len as f32);
                *sc.entry(d).or_default() += w * (idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * dl / avg)) + ent);
                if direct && seen.insert(d) { *sup.entry(d).or_default() += idf }
            }
        }
        if sup_total > 0.0 { for s in sup.values_mut() { *s = (*s / sup_total).min(1.0) } }
        let kw_hit: std::collections::HashSet<u32> = sc.keys().copied().collect();
        // `lx`: the score without the vector part (same boosts and graph spread, seeded with keyword scores only)
        let mut lx: HashMap<u32, f32> = sc.clone();
        if self.use_vectors && self.vec_last.is_some() {
            if let Some(dir) = &self.model {
                let w = std::env::var("ELISA_MEM_VEC_W").ok().and_then(|x| x.parse().ok()).unwrap_or(VEC_W);
                for (d, c) in self.vector_hits(dir, query) { if c > 0.0 && d < self.next() { *sc.entry(d).or_default() += w * c } }
            }
        }
        for map in [&mut sc, &mut lx] {
            for (d, s) in map.iter_mut() {
                let m = self.meta(*d);
                *s *= 1.0 + 0.1 * *d as f32 / n;
                if m.project == proj || m.project == NONE { *s *= 1.1 }
                if m.kind == CARD && kw_hit.contains(d) { *s *= CARD_BOOST }
            }
        }
        if graph {
            let mut seeds: Vec<(u32, f32)> = sc.iter().map(|(&d, &s)| (d, s)).collect();
            seeds.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
            seeds.truncate(10);
            let rare = 2.max((nd / 100.0) as usize);
            // (score, lex score, support) a linked doc gets from its seed
            let mut add: HashMap<u32, (f32, f32, f32)> = HashMap::new();
            let bump = |add: &mut HashMap<u32, (f32, f32, f32)>, t: u32, f: f32, v: (f32, f32, f32)| {
                let a = add.entry(t).or_default();
                *a = (a.0.max(f * v.0), a.1.max(f * v.1), a.2.max(v.2));
            };
            let mut stale: Vec<u32> = vec![];
            for &(d, v) in &seeds {
                let m = self.meta(d);
                let vv = (v, lx.get(&d).copied().unwrap_or(0.0), sup.get(&d).copied().unwrap_or(0.0));
                for (e, t) in self.edges(d) {
                    let f = match e {
                        E_NEXT if m.kind == TURN && m.role == 0 => 0.6, // question -> its answer
                        E_PREV if m.kind == TURN && m.role == 1 => 0.4, // answer -> its question
                        E_SUMBY => 0.35,
                        _ => 0.0,
                    };
                    if f > 0.0 { bump(&mut add, t, f, vv) }
                }
                let cur = self.latest(d);
                if cur != d { bump(&mut add, cur, 0.8, vv) }
                // note -> the facts it states that match the query -> their latest version
                // (a note whose matching facts are all superseded is itself stale for this query)
                let (mut hit, mut old) = (0, 0);
                for (e, f) in self.edges(d) {
                    if e != E_SRCOF { continue }
                    if extract(&self.text(f, &mut cache)).iter().any(|t| qt.iter().any(|q| q.1 >= ALIAS_W && q.0 == *t)) {
                        let l = self.latest(f); bump(&mut add, l, 0.7, vv);
                        hit += 1; if l != f { old += 1 }
                    }
                }
                if hit > 0 && hit == old { stale.push(d) }
                let mut cnt: HashMap<u32, u32> = HashMap::new();
                let mut ms = extract(&strip_alias(&self.text(d, &mut cache))); ms.sort(); ms.dedup();
                for t in ms {
                    let ps = self.postings(&t);
                    if ps.len() > rare { continue }
                    for (d2, _) in ps { if d2 != d { *cnt.entry(d2).or_default() += 1 } }
                }
                // shared rare terms only: no support carried over (the link says nothing about the query)
                for (d2, c) in cnt { if c >= 2 { bump(&mut add, d2, 0.3, (vv.0, vv.1, 0.0)) } }
            }
            for (d, a) in add {
                if !scored(self.meta(d).kind) { continue }
                *sc.entry(d).or_default() += a.0;
                if a.1 > 0.0 { *lx.entry(d).or_default() += a.1 }
                let s = sup.entry(d).or_default(); *s = s.max(a.2);
            }
            for map in [&mut sc, &mut lx] {
                for (d, s) in map.iter_mut() { if stale.contains(d) || self.edges(*d).iter().any(|e| e.0 == E_REPLBY) { *s *= 0.6 } }
            }
        }
        let mut top: Vec<(u32, f32)> = sc.into_iter().collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
        // drop exact-duplicate texts (cumulative notes repeat the same lines)
        let mut uniq: Vec<(u32, f32, String)> = vec![];
        for &(d, s) in &top {
            if uniq.len() == k { break }
            let t = self.text(d, &mut cache);
            if !uniq.iter().any(|u| u.2 == t) { uniq.push((d, s, t)) }
        }
        // snippets: highest-idf query term first, shared budget
        let mut order: Vec<&(String, f32)> = qt.iter().filter(|q| df_of.contains_key(&q.0)).collect();
        order.sort_by(|a, b| df_of[&a.0].total_cmp(&df_of[&b.0]));
        let uniq: Vec<_> = uniq.into_iter().filter(|u| !u.2.starts_with(UNPIN)).collect();
        let (mut budget, kk) = (SNIP_CHARS, uniq.len());
        let hits = uniq.into_iter().enumerate().map(|(i, (d, s, full))| {
            let m = self.meta(d);
            let per = budget / (kk - i);
            let t = snippet(&full, &order, per);
            budget = budget.saturating_sub(t.len());
            let kind = match m.kind { TURN => HitKind::Turn, NOTE => HitKind::Note, CARD => HitKind::Card, _ => HitKind::Fact };
            let (lex, support) = (lx.get(&d).copied().unwrap_or(0.0), sup.get(&d).copied().unwrap_or(0.0));
            Hit { id: d as u64, kind, session: m.session as u64, score: s, lex, support, text: t }
        }).collect::<Vec<Hit>>();
        self.settle();
        hits
    }
}

fn fs_len(f: &File) -> u64 { f.metadata().map_or(0, |m| m.len()) }
/// the log segment numbers in `dir`, ascending
fn seg_nums(dir: &Path) -> io::Result<Vec<u32>> {
    let mut nums: Vec<u32> = fs::read_dir(dir)?.flatten().filter_map(|e| {
        let n = e.file_name().into_string().ok()?;
        n.strip_prefix("log-")?.strip_suffix(".seg")?.parse().ok()
    }).collect();
    nums.sort();
    Ok(nums)
}

/// Hand freed heap pages back to the OS (the allocator otherwise keeps them resident for reuse).
pub(crate) fn trim_heap() {
    #[cfg(target_os = "macos")]
    { extern "C" { fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: usize) -> usize; } unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0); } }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    { extern "C" { fn malloc_trim(pad: usize) -> i32; } unsafe { malloc_trim(0); } }
}

fn vec_paths(dir: &Path) -> (PathBuf, PathBuf) { (dir.join("vectors.mg"), dir.join("vectors.hot")) }
/// top `k` (id, cosine) over all stored vectors of the store in `dir`: mmap scan of both segments, int8 dot products
fn vsearch(dir: &Path, q: &[f32; embed::DIM], k: usize) -> Vec<(u32, f32)> {
    let qs = q.iter().fold(0f32, |m, x| m.max(x.abs())) / 127.0;
    let qq: Vec<i32> = q.iter().map(|x| (x / qs).round() as i32).collect();
    let mut top: Vec<(u32, f32)> = vec![];
    let (cp, hp) = vec_paths(dir);
    for p in [cp, hp] {
        let Ok(mm) = Mmap::open(&p) else { continue };
        for r in mm.b().chunks_exact(VREC) {
            let s: i32 = r[12..].iter().zip(&qq).map(|(&a, &b)| a as i8 as i32 * b).sum();
            let c = s as f32 * qs * f32::from_le_bytes(r[4..8].try_into().unwrap());
            if top.len() < k { top.push((r32(r, 0), c)) }
            else if let Some(mi) = top.iter().enumerate().min_by(|a, b| a.1 .1.total_cmp(&b.1 .1)).filter(|m| m.1 .1 < c).map(|m| m.0) { top[mi] = (r32(r, 0), c) }
        }
    }
    top
}
/// embed `query` with the model in `model` and return the nearest `k` vectors of the store in `dir`
fn query_vectors(dir: &Path, model: &Path, query: &str, k: usize) -> io::Result<Vec<(u32, f32)>> {
    let v = embed::embed(model, &dir.join("vocab.bin"), &[query], &mut |_| {})?;
    Ok(vsearch(dir, &v[0], k))
}

// ---------- worker processes ----------
/// Hidden first argument that turns a binary into an elisa-mem worker: `<exe> __elisa-mem-worker <job args...>`.
/// A binary that embeds elisa-mem checks for it before any other setup and calls `worker_main(&args[2..])`.
pub const WORKER_ARG: &str = "__elisa-mem-worker";

/// Spawn `<exe> __elisa-mem-worker <args>` (posix_spawn via `std::process::Command`: safe from a multithreaded
/// parent, unlike a bare fork) and read its stdout: `label\tfrac` progress lines (frac empty = unknown), then
/// `=rss\t<bytes>` and `=ok\t<result>` or `=err\t<message>`. The worker exits when the job is done, so the OS
/// reclaims everything it used. Returns (result, worker peak RSS).
fn run_worker(exe: Option<&Path>, args: &[&str], on_progress: &mut dyn FnMut(&str, Option<f32>)) -> io::Result<(String, u64)> {
    use std::process::{Command, Stdio};
    let exe = exe.ok_or_else(|| io::Error::other("elisa-mem worker: no executable (set Memory::worker_exe)"))?;
    let dbg = std::env::var_os("ELISA_MEM_DEBUG").is_some();
    let mut ch = Command::new(exe).arg(WORKER_ARG).args(args)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(if dbg { Stdio::inherit() } else { Stdio::null() })
        .spawn().map_err(|e| io::Error::other(format!("elisa-mem worker {}: {e}", exe.display())))?;
    let out = ch.stdout.take().expect("piped stdout");
    let (mut res, mut err, mut rss) = (None, None, 0u64);
    for line in BufReader::with_capacity(4096, out).lines() {
        let Ok(l) = line else { break };
        if let Some(r) = l.strip_prefix("=ok\t") { res = Some(r.to_string()) }
        else if let Some(e) = l.strip_prefix("=err\t") { err = Some(e.to_string()) }
        else if let Some(x) = l.strip_prefix("=rss\t") { rss = x.trim().parse().unwrap_or(0) }
        else if let Some((w, f)) = l.rsplit_once('\t') { on_progress(w, f.parse().ok()) }
    }
    let st = ch.wait()?;
    if dbg && res.is_none() { eprintln!("elisa-mem worker {:?} died: {st}, err {err:?}", args.first()) }
    if let Some(e) = err { return Err(io::Error::other(format!("elisa-mem worker: {e}"))) }
    res.map(|r| (r, rss)).ok_or_else(|| io::Error::other(format!("elisa-mem worker died ({st})")))
}

/// Worker entry point (`args` = the job arguments after `WORKER_ARG`):
/// `index <store>` · `compact <store>` · `query <store> <model dir> <k> <query text>`.
/// Streams progress and the result to stdout (format in `run_worker`) and returns the exit code.
pub fn worker_main(args: &[String]) -> i32 {
    // Ctrl+C in the parent's terminal must not kill a job mid-write (the parent handles cancelling itself)
    unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN); }
    fn send(s: &str) { let mut o = io::stdout().lock(); let _ = o.write_all(s.as_bytes()); let _ = o.flush(); }
    let progress = || -> Box<dyn FnMut(&str, Option<f32>) + Send> {
        Box::new(|w: &str, f: Option<f32>| send(&format!("{}\t{}\n", w.replace(['\t', '\n', '\r'], " ").trim_start_matches('='), f.map_or(String::new(), |f| f.to_string()))))
    };
    let open = |d: &str| -> io::Result<Memory> {
        // the parent holds the store's lock for the whole job
        let mut m = Memory::open_unlocked(Path::new(d))?;
        m.use_worker = false; m.auto_index = false; m.compact_every = 0; m.set_progress(progress());
        Ok(m)
    };
    let job = || -> io::Result<String> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["index", d] => Ok(open(d)?.index_pending()?.to_string()),
            ["compact", d] => { open(d)?.compact()?; Ok(String::new()) }
            ["query", d, model, k, q] => {
                let k = k.parse().map_err(|_| bad("bad k"))?;
                Ok(query_vectors(Path::new(d), Path::new(model), q, k)?.iter().map(|(i, c)| format!("{i}:{c}")).collect::<Vec<_>>().join(" "))
            }
            _ => Err(bad(&format!("unknown worker job {:?}", a))),
        }
    };
    let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
        Ok(r) => r,
        Err(e) => Err(io::Error::other(format!("panic: {}", e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()))),
    };
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let rss = ru.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 };
    send(&format!("=rss\t{rss}\n"));
    match r {
        Ok(v) => { send(&format!("=ok\t{}\n", v.replace('\n', " "))); 0 }
        Err(e) => { send(&format!("=err\t{}\n", e.to_string().replace('\n', " "))); 1 }
    }
}

fn snippet(t: &str, order: &[&(String, f32)], per: usize) -> String {
    if t.len() <= per { return t.to_string() }
    let lo = t.to_lowercase();
    let skip = alias_ranges(t);
    let pos = order.iter().find_map(|q| lo.match_indices(q.0.as_str()).map(|x| x.0).find(|&i| !skip.iter().any(|r| (r.0..r.1).contains(&i)))).unwrap_or(0);
    let fix = |mut i: usize| { i = i.min(t.len()); while !t.is_char_boundary(i) { i -= 1 } i };
    let s = fix(pos.saturating_sub(per / 3));
    let e = fix(s + per);
    t[s..e].to_string()
}

#[cfg(test)]
mod unicode_recall_tests {
    use super::*;

    fn store(tag: &str) -> (PathBuf, Memory) {
        let d = std::env::temp_dir().join(format!("elisa-mem-uni-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let mut m = Memory::open(&d).unwrap();
        (m.use_vectors, m.use_worker, m.auto_index, m.compact_every) = (false, false, false, 0);
        (d, m)
    }

    /// Words of any script are terms (before 2026-11-01 only ASCII words were: a Vietnamese query matched on
    /// stray ASCII syllables like "sa" in "phù sa"); ASCII behaviour is unchanged.
    #[test]
    fn extract_keeps_words_of_any_script() {
        assert_eq!(extract("Bạn có đang làm tiếp không?"), ["bạn", "có", "đang", "làm", "tiếp", "không"]);
        assert_eq!(extract("Привет мир"), ["привет", "мир"]);
        assert_eq!(extract("the running tests in src/main.rs"), ["runn", "test", "main.rs", "src/main.rs"]);
        assert!(!is_entity("không") && !is_entity("test") && is_entity("main.rs") && is_entity("abc123"));
    }

    /// `lex` is the score without vectors; `support` the idf-weighted share of the query's terms a hit has.
    #[test]
    fn support_and_lex_on_a_mixed_store() {
        let (d, mut m) = store("sup");
        let s = m.begin_session("/p");
        for t in ["Tại sao nước biển lại mặn?", "Nước biển mặn do muối hòa tan từ đá, rồi theo sông ra biển.",
                  "Bạn có thể code không?", "Có. Tôi có thể viết code, sửa lỗi và chạy test.",
                  "Hoàng Sa và Trường Sa là của Việt Nam.", "sông Mê Kông mang phù sa về đồng bằng",
                  "fix the auth endpoint in main.py", "added auth endpoints to main.py; make test fails on import"] {
            m.add_turn(s, Role::User, t).unwrap();
        }
        let top = |q: &str| m.recall(q, "/p", 3, true);
        let h = top("nước biển có mặn không");
        assert!(h[0].text.contains("biển") && h[0].support > 0.5 && h[0].lex > 0.0, "{h:?}");
        assert!((h[0].lex - h[0].score).abs() < 1e-3, "no vectors: lex == score");
        // a query of words the store has never seen: every hit covers little of it
        for x in top("hôm nay thời tiết thế nào nhỉ bạn") { assert!(x.support < 0.3, "{x:?}"); }
        let h = top("auth endpoint main.py");
        assert!(h[0].text.contains("auth") && h[0].support > 0.6, "{h:?}");
        assert!(h.iter().all(|x| x.support <= 1.0));
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    /// `unpin_all` drops every current fact of the project from `pinned_facts` (also after a reopen), keeps the
    /// unpin markers out of recall, and a fact pinned again afterwards comes back.
    #[test]
    fn unpin_all_clears_pinned_facts() {
        let (d, mut m) = store("unpin");
        let s = m.begin_session("/p");
        m.pin_fact(s, "User: wants to learn coding").unwrap();
        m.pin_fact(s, "Port: 8000").unwrap();
        assert_eq!(m.pinned_facts("/p").len(), 2);
        assert_eq!(m.unpin_all(s, "/p").unwrap(), 2);
        assert!(m.pinned_facts("/p").is_empty());
        assert!(m.recall("user wants learn coding unpinned", "/p", 5, true).iter().all(|h| !h.text.contains("unpinned")));
        drop(m);
        let mut m = Memory::open(&d).unwrap();
        (m.use_vectors, m.use_worker, m.auto_index, m.compact_every) = (false, false, false, 0);
        assert!(m.pinned_facts("/p").is_empty());
        let s = m.begin_session("/p");
        m.pin_fact(s, "Port: 9000").unwrap();
        assert_eq!(m.pinned_facts("/p"), ["Port: 9000"]);
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    /// Two open stores on one directory (as two processes): neither assumes it is the only writer. Before
    /// 2026-11-09 B's open rewrote the shared hot files under A, and A's next recall indexed past its doc table
    /// (`text`: index out of bounds); B's ids also collided with A's.
    #[test]
    fn two_handles_share_one_store() {
        let (d, mut a) = store("two");
        let sa = a.begin_session("/a");
        a.add_turn(sa, Role::User, "alpha widget count is 42").unwrap();
        let mut b = Memory::open(&d).unwrap();
        (b.use_vectors, b.use_worker, b.auto_index, b.compact_every) = (false, false, false, 0);
        let sb = b.begin_session("/b");
        for i in 0..6 { b.add_turn(sb, Role::User, &format!("beta owner Bob item {i}")).unwrap(); }
        b.end_session(sb).unwrap();
        let sb2 = b.begin_session("/b");
        b.add_turn(sb2, Role::User, "beta after clear").unwrap();
        // A's snapshot predates all of that: reads stay consistent, nothing panics
        assert!(a.recall("beta owner Bob item", "/a", 5, true).iter().all(|h| !h.text.is_empty()));
        assert!(a.recall("alpha widget count", "/a", 5, true)[0].text.contains("42"));
        // A's next write catches up first: its id follows B's, and B's memories become visible to A
        let t = a.add_turn(sa, Role::Assistant, "alpha noted").unwrap();
        assert!(t > sb2, "A's id {t} must follow B's records");
        assert!(a.recall("beta owner Bob item", "/a", 5, true).iter().any(|h| h.text.contains("Bob")));
        // B sees A's write after a refresh
        assert!(b.refresh().unwrap());
        assert!(!b.refresh().unwrap(), "nothing new the second time");
        assert!(b.recall("alpha noted", "/b", 5, true).iter().any(|h| h.text.contains("alpha noted")));
        // B compacts (replaces the index, deletes the segments A still reads from): A keeps working
        b.compact().unwrap();
        assert!(a.recall("alpha widget count", "/a", 5, true)[0].text.contains("42"));
        a.add_turn(sa, Role::User, "alpha after compaction").unwrap();
        b.add_turn(sb2, Role::User, "beta after compaction").unwrap();
        a.end_session(sa).unwrap();
        b.end_session(sb2).unwrap();
        drop((a, b));
        // one consistent store: every record once, dense ids
        let r = Memory::open(&d).unwrap();
        let st = r.stats();
        assert_eq!(st.turns, 1 + 6 + 1 + 1 + 1 + 1, "{st:?}");
        assert!(r.recall("alpha after compaction", "/a", 3, true).iter().any(|h| h.text == "alpha after compaction"));
        assert!(r.recall("beta after compaction", "/b", 3, true).iter().any(|h| h.text == "beta after compaction"));
        drop(r);
        let _ = fs::remove_dir_all(d);
    }

    /// `text` on an id the snapshot does not have is None, not a panic.
    #[test]
    fn text_of_an_unknown_id_is_none() {
        let (d, mut m) = store("stale");
        let s = m.begin_session("/p");
        let t = m.add_turn(s, Role::User, "hello there").unwrap() as u32;
        assert_eq!(m.text_opt(t, &mut Cache::new()).as_deref(), Some("hello there"));
        assert_eq!(m.text_opt(t + 100, &mut Cache::new()), None);
        assert!(m.card_chain(t as u64 + 100, 3).is_empty());
        drop(m);
        let _ = fs::remove_dir_all(d);
    }
}
