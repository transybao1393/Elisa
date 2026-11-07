//! External sort of byte records (bytewise order) with a fixed memory budget: records are buffered up to
//! `cap` bytes, sorted and spilled as runs; `finish` merges runs (FANIN at a time) with 64 KB read buffers.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const FANIN: usize = 32;
const RBUF: usize = 64 << 10;

pub struct Sorter { dir: PathBuf, tag: String, cap: usize, buf: Vec<u8>, idx: Vec<(u32, u32)>, runs: Vec<PathBuf>, nrun: usize }

impl Sorter {
    pub fn new(dir: &Path, tag: &str, cap: usize) -> Sorter {
        Sorter { dir: dir.to_path_buf(), tag: tag.into(), cap, buf: Vec::new(), idx: Vec::new(), runs: vec![], nrun: 0 }
    }
    pub fn push(&mut self, rec: &[u8]) -> io::Result<()> {
        if !self.idx.is_empty() && self.buf.len() + rec.len() + self.idx.len() * 8 > self.cap { self.spill()? }
        self.idx.push((self.buf.len() as u32, rec.len() as u32));
        self.buf.extend_from_slice(rec);
        Ok(())
    }
    fn sort(&mut self) {
        let b = &self.buf;
        self.idx.sort_unstable_by(|x, y| b[x.0 as usize..(x.0 + x.1) as usize].cmp(&b[y.0 as usize..(y.0 + y.1) as usize]));
    }
    fn run_path(&mut self) -> PathBuf { self.nrun += 1; self.dir.join(format!("{}-{}.run", self.tag, self.nrun)) }
    fn spill(&mut self) -> io::Result<()> {
        self.sort();
        let p = self.run_path();
        let mut w = BufWriter::with_capacity(RBUF, File::create(&p)?);
        for &(o, l) in &self.idx { w.write_all(&l.to_le_bytes())?; w.write_all(&self.buf[o as usize..(o + l) as usize])? }
        w.flush()?;
        self.runs.push(p);
        self.buf = Vec::new(); self.idx = Vec::new();
        Ok(())
    }
    pub fn finish(mut self) -> io::Result<Merged> {
        if self.runs.is_empty() { self.sort(); return Ok(Merged::Mem { buf: self.buf, idx: self.idx, i: 0 }) }
        if !self.idx.is_empty() { self.spill()? }
        // multi-level merge keeps open readers (and their buffers) ≤ FANIN
        while self.runs.len() > FANIN {
            let group: Vec<PathBuf> = self.runs.drain(..FANIN).collect();
            let mut m = Merged::open(&group)?;
            let p = self.run_path();
            let mut w = BufWriter::with_capacity(RBUF, File::create(&p)?);
            while let Some(r) = m.next()? { w.write_all(&(r.len() as u32).to_le_bytes())?; w.write_all(&r)? }
            w.flush()?;
            drop(m);
            for g in group { let _ = fs::remove_file(g); }
            self.runs.push(p);
        }
        let m = Merged::open(&self.runs)?;
        Ok(m)
    }
}

pub enum Merged {
    Mem { buf: Vec<u8>, idx: Vec<(u32, u32)>, i: usize },
    Runs { rd: Vec<BufReader<File>>, heap: BinaryHeap<Reverse<(Vec<u8>, usize)>>, paths: Vec<PathBuf> },
}
fn read_rec(r: &mut BufReader<File>) -> io::Result<Option<Vec<u8>>> {
    let mut l = [0u8; 4];
    match r.read_exact(&mut l) { Ok(()) => {}, Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None), Err(e) => return Err(e) }
    let mut v = vec![0u8; u32::from_le_bytes(l) as usize];
    r.read_exact(&mut v)?;
    Ok(Some(v))
}
impl Merged {
    fn open(paths: &[PathBuf]) -> io::Result<Merged> {
        let mut rd = vec![]; let mut heap = BinaryHeap::new();
        for (i, p) in paths.iter().enumerate() {
            let mut r = BufReader::with_capacity(RBUF, File::open(p)?);
            if let Some(v) = read_rec(&mut r)? { heap.push(Reverse((v, i))) }
            rd.push(r);
        }
        Ok(Merged::Runs { rd, heap, paths: paths.to_vec() })
    }
    pub fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self {
            Merged::Mem { buf, idx, i } => {
                let Some(&(o, l)) = idx.get(*i) else { return Ok(None) };
                *i += 1;
                Ok(Some(buf[o as usize..(o + l) as usize].to_vec()))
            }
            Merged::Runs { rd, heap, .. } => {
                let Some(Reverse((v, i))) = heap.pop() else { return Ok(None) };
                if let Some(n) = read_rec(&mut rd[i])? { heap.push(Reverse((n, i))) }
                Ok(Some(v))
            }
        }
    }
}
impl Drop for Merged {
    fn drop(&mut self) { if let Merged::Runs { paths, .. } = self { for p in paths { let _ = fs::remove_file(p); } } }
}
