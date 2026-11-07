//! Full-flow bench: ingest, recall@3 (graph / aliases / vectors on-off), hot-log recall, latency, disk,
//! bounded-memory embedding + compaction (measured in child processes), state cards, crash recovery.
use crate::gen::{self, Ev, Question, PROJECT};
use crate::{Hit, Memory, Role};
use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SEED: u64 = 0x5EED_2026_1101;
const MB: f64 = 1048576.0;

fn role(user: bool) -> Role { if user { Role::User } else { Role::Assistant } }

fn dir_bytes(p: &Path) -> u64 {
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| {
        let m = match e.metadata() { Ok(m) => m, Err(_) => return 0 };
        if m.is_dir() { dir_bytes(&e.path()) } else { m.len() }
    }).sum()).unwrap_or(0)
}
/// bytes of files whose name matches `f`
fn bytes_where(p: &Path, f: impl Fn(&str) -> bool) -> u64 {
    std::fs::read_dir(p).map(|rd| rd.flatten().filter(|e| f(&e.file_name().to_string_lossy())).map(|e| e.metadata().map_or(0, |m| m.len())).sum()).unwrap_or(0)
}

fn fresh(base: &Path, tag: &str) -> PathBuf {
    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = base.join(format!("elisa-mem-bench-{tag}-{}-{ns}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create bench dir");
    d
}

/// current resident size of this process
fn cur_rss_bytes() -> u64 {
    let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let sz = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut libc::c_void, sz) };
    if n == sz { ti.pti_resident_size } else { 0 }
}

#[repr(C)] struct MStats { blocks_in_use: u32, size_in_use: usize, max_size_in_use: usize, size_allocated: usize }
extern "C" { fn malloc_zone_statistics(zone: *mut libc::c_void, s: *mut MStats); }
/// bytes the allocator reports as live (all zones)
fn malloc_in_use() -> u64 { let mut s = MStats { blocks_in_use: 0, size_in_use: 0, max_size_in_use: 0, size_allocated: 0 }; unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut s) }; s.size_in_use as u64 }

fn peak_rss_bytes() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let v = ru.ru_maxrss as u64;
    if cfg!(target_os = "macos") { v } else { v * 1024 }
}

#[derive(Default)]
struct Run { pass: Vec<bool>, ids: Vec<Vec<u64>>, hits: Vec<Vec<Hit>>, lat: Vec<Duration> }

fn ask(m: &mut Memory, qs: &[Question], graph: bool, vectors: bool) -> Run {
    m.use_vectors = vectors;
    let mut r = Run::default();
    for q in qs {
        let t = Instant::now();
        let hits = m.recall(q.text, PROJECT, 3, graph);
        r.lat.push(t.elapsed());
        r.pass.push(hits.iter().take(3).any(|h| q.matches(&h.text)));
        r.ids.push(hits.iter().map(|h| h.id).collect());
        r.hits.push(hits);
    }
    m.use_vectors = true;
    r
}

fn pct(xs: &[bool], f: impl Fn(usize) -> bool) -> f64 {
    let sel: Vec<bool> = xs.iter().enumerate().filter(|(i, _)| f(*i)).map(|(_, &b)| b).collect();
    100.0 * sel.iter().filter(|&&b| b).count() as f64 / sel.len().max(1) as f64
}
fn all(xs: &[bool]) -> f64 { pct(xs, |_| true) }

fn cut(s: &str, n: usize) -> String {
    let one = s.replace('\n', " ⏎ ");
    if one.chars().count() <= n { one } else { one.chars().take(n).collect::<String>() + "…" }
}

struct Ingest { add: Duration, end: Duration, total: Duration, turns: u32, notes: u32, cards: u32, last_card: u64 }
/// Feed the conversation; `hook` runs just before the final end_session (the last session is still hot).
fn ingest(m: &mut Memory, conv: &[gen::Session], mut hook: impl FnMut(&mut Memory)) -> Ingest {
    let mut g = Ingest { add: Duration::ZERO, end: Duration::ZERO, total: Duration::ZERO, turns: 0, notes: 0, cards: 0, last_card: 0 };
    let t0 = Instant::now();
    let mut prev_card: Option<u64> = None;
    for (si, sess) in conv.iter().enumerate() {
        let s = m.begin_session(PROJECT);
        let (mut pending, mut since_card): (Vec<u64>, Vec<u64>) = (vec![], vec![]);
        for ev in &sess.evs {
            match ev {
                Ev::Turn { user, text } => {
                    let t = Instant::now();
                    let id = m.add_turn(s, role(*user), text).expect("add_turn");
                    g.add += t.elapsed(); g.turns += 1;
                    pending.push(id); since_card.push(id);
                }
                Ev::Note(text) => { m.add_note(s, text, &pending).expect("add_note"); pending.clear(); g.notes += 1; }
                Ev::Card(text) => {
                    let id = m.add_card(s, text, prev_card, &since_card).expect("add_card");
                    prev_card = Some(id); since_card.clear(); g.cards += 1; g.last_card = id;
                }
            }
        }
        if std::env::var_os("ELISA_MEM_DEBUG").is_some() && si % 5 == 0 { crate::trim_heap(); eprintln!("bench rss @session {si}: {:.2} MB", cur_rss_bytes() as f64 / MB) }
        if si + 1 == conv.len() { hook(m) }
        let t = Instant::now();
        m.end_session(s).expect("end_session");
        g.end += t.elapsed();
    }
    g.total = t0.elapsed();
    g
}

/// Crash test: child ingests in a loop and reports progress through a pipe; parent SIGKILLs it.
fn crash_test(base: &Path) -> (bool, u64, u64, bool, String) {
    let dir = fresh(base, "crash");
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 { return (false, 0, 0, false, "pipe failed".into()); }
    let pid = unsafe { libc::fork() };
    if pid < 0 { return (false, 0, 0, false, "fork failed".into()); }
    if pid == 0 {
        unsafe { libc::close(fds[0]) };
        let mut m = match Memory::open(&dir) { Ok(m) => m, Err(_) => unsafe { libc::_exit(2) } };
        let s = m.begin_session(PROJECT);
        let mut r = gen::Rng::new(SEED ^ 0xC4A5);
        let mut n: u64 = 0;
        loop {
            let user = n % 2 == 0;
            let text = gen::filler(&mut r, user);
            if m.add_turn(s, role(user), &text).is_err() { unsafe { libc::_exit(3) } }
            n += 1;
            let b = n.to_le_bytes();
            unsafe { libc::write(fds[1], b.as_ptr() as *const libc::c_void, 8) };
        }
    }
    unsafe {
        libc::close(fds[1]);
        let fl = libc::fcntl(fds[0], libc::F_GETFL);
        libc::fcntl(fds[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    let mut pipe = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let (mut buf, mut chunk) = (Vec::new(), [0u8; 4096]);
    let mut drain = |buf: &mut Vec<u8>| loop {
        match pipe.read(&mut chunk) { Ok(0) => break, Ok(k) => buf.extend_from_slice(&chunk[..k]), Err(_) => break }
    };
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(50) { drain(&mut buf); std::thread::sleep(Duration::from_millis(1)); }
    let mut status = 0;
    unsafe { libc::kill(pid, libc::SIGKILL); libc::waitpid(pid, &mut status, 0); }
    drain(&mut buf);
    let progress = if buf.len() >= 8 { let k = buf.len() / 8 * 8; u64::from_le_bytes(buf[k - 8..k].try_into().unwrap()) } else { 0 };
    let killed = libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL;
    let res = match Memory::open(&dir) {
        Err(e) => (false, progress, 0, false, format!("reopen error: {e}")),
        Ok(mut m) => {
            m.use_vectors = false;
            let st = m.stats();
            let hits = m.recall("cargo test build error", PROJECT, 3, true);
            (true, progress, st.turns as u64, !hits.is_empty(), if killed { "child SIGKILLed".into() } else { format!("child exited early, status {status}") })
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    res
}

/// Child-process entry (`elisa-mem bench-child <embed|compact|search> <dir>`): one step on its own, then
/// prints `rss=<bytes> ms=<f64> n=<count> updates=<k> first=<msg> last=<msg>` for a clean peak-RSS reading.
pub fn child(mode: &str, dir: &Path) -> i32 {
    let t = Instant::now();
    let mut m = match Memory::open(dir) { Ok(m) => m, Err(e) => { eprintln!("open: {e}"); return 1 } };
    m.use_worker = false; // this child IS the worker being measured
    if std::env::var_os("ELISA_MEM_DEBUG").is_some() { eprintln!("child open: rss {:.1} MB", crate::embed::rss_mb()) }
    let log = std::sync::Arc::new(std::sync::Mutex::new((0usize, String::new(), String::new())));
    let l2 = log.clone();
    m.set_progress(Box::new(move |w, f| {
        let mut g = l2.lock().unwrap();
        let s = match f { Some(f) => format!("{w} {:.0}%", f * 100.0), None => w.to_string() };
        if g.0 == 0 { g.1 = s.clone() }
        g.0 += 1; g.2 = s;
    }));
    let n = match mode {
        "embed" => m.index_pending().expect("index_pending"),
        "compact" => { m.compact().expect("compact"); 1 }
        "search" => {
            let qs: Vec<Question> = gen::questions().into_iter().chain(gen::semantic_questions()).collect();
            for q in &qs { let _ = m.recall(q.text, PROJECT, 3, true); }
            qs.len()
        }
        _ => return 2,
    };
    let g = log.lock().unwrap();
    println!("rss={} ms={:.1} n={n} updates={} first={} last={}", peak_rss_bytes(), t.elapsed().as_secs_f64() * 1e3, g.0, g.1.replace(' ', "_"), g.2.replace(' ', "_"));
    0
}
struct Child { rss: u64, ms: f64, n: u64, updates: u64, first: String, last: String }
fn run_child(mode: &str, dir: &Path) -> Option<Child> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new(exe).args(["bench-child", mode]).arg(dir).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let kv = |k: &str| s.split_whitespace().find_map(|x| x.strip_prefix(&format!("{k}=")).map(str::to_string));
    Some(Child { rss: kv("rss")?.parse().ok()?, ms: kv("ms")?.parse().ok()?, n: kv("n")?.parse().ok()?, updates: kv("updates")?.parse().ok()?,
        first: kv("first")?.replace('_', " "), last: kv("last")?.replace('_', " ") })
}
fn copy_dir(a: &Path, b: &Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap().flatten() { if e.path().is_file() { std::fs::copy(e.path(), b.join(e.file_name())).unwrap(); } }
}

pub fn run(dir: &Path) -> i32 {
    let qs = gen::questions();
    let sem = gen::semantic_questions();
    let hotq = [gen::hot_question()];
    let cardq = [gen::card_question()];
    let conv = gen::conversation(SEED);
    let model = crate::embed::model_dir();

    // ===== store A: default settings (auto-compact every 10 sessions, embedding at end_session, worker processes) =====
    // baseline after a warm-up (code paths paged in once), heap trimmed
    {
        let wd = fresh(dir, "warm");
        // one of each call (incl. index/compact worker processes) so one-time code/library page-ins are in the baseline
        let mut w = Memory::open(&wd).expect("warm open");
        let s = w.begin_session(PROJECT);
        let t = w.add_turn(s, Role::User, "warm up the store").unwrap();
        w.add_note(s, "Fold: warm.\nFacts: the warm key is 1.\nAlso known as: warm = hot", &[t]).unwrap();
        w.add_card(s, "State: warm", None, &[t]).unwrap();
        w.end_session(s).unwrap();
        w.compact().unwrap();
        let _ = w.recall("warm up", PROJECT, 3, true);
        let _ = (w.latest_card(PROJECT), w.pinned_facts(PROJECT), w.stats());
        drop(w);
        let w = Memory::open(&wd).expect("warm reopen");
        drop(w);
        let _ = std::fs::remove_dir_all(&wd);
    }
    crate::trim_heap();
    let rss0 = cur_rss_bytes();
    let work = fresh(dir, "main");
    let mut m = match Memory::open(&work) { Ok(m) => m, Err(e) => { eprintln!("open failed: {e}"); return 1; } };
    let plog = std::sync::Arc::new(std::sync::Mutex::new((0usize, Vec::<String>::new())));
    let pl2 = plog.clone();
    m.set_progress(Box::new(move |w, _| { let mut g = pl2.lock().unwrap(); g.0 += 1; if !g.1.iter().any(|x| x == w) && g.1.len() < 40 { g.1.push(w.to_string()) } }));
    let (mut hot_on, mut hot_off, mut hot_in_log) = (Run::default(), Run::default(), false);
    if std::env::var_os("ELISA_MEM_DEBUG").is_some() { eprintln!("bench rss @open A: {:.2} MB", cur_rss_bytes() as f64 / MB) }
    let ing = ingest(&mut m, &conv, |m| {
        // the last session's turns are only in the hot log (not compacted, not yet embedded)
        hot_on = ask(m, &hotq, true, true);
        hot_off = ask(m, &hotq, false, false);
        let base = m.hot.base as u64;
        hot_in_log = hot_on.hits[0].iter().chain(&hot_off.hits[0]).filter(|h| hotq[0].matches(&h.text)).all(|h| h.id >= base) && hot_on.pass[0];
    });
    let st = m.stats();
    let dbg = |w: &str| if std::env::var_os("ELISA_MEM_DEBUG").is_some() { crate::trim_heap(); eprintln!("bench rss @{w}: {:.2} MB", cur_rss_bytes() as f64 / MB) };
    dbg("after ingest");
    let full = ask(&mut m, &qs, true, true);
    dbg("after full asks");     // graph + aliases + vectors
    let graph = ask(&mut m, &qs, true, false);   // graph + aliases
    let kw = ask(&mut m, &qs, false, false);     // keyword only
    let kwv = ask(&mut m, &qs, false, true);     // keyword + vectors
    let sem_on = ask(&mut m, &sem, true, true);
    let sem_off = ask(&mut m, &sem, true, false);
    let card_on = ask(&mut m, &cardq, true, true);
    let card_off = ask(&mut m, &cardq, false, false);
    let hot_after = ask(&mut m, &hotq, true, true);
    // cards: chain walk of 50 with milestone skips vs a plain prev walk
    let (last_id, _) = m.latest_card(PROJECT).expect("latest_card");
    let t = Instant::now();
    let chain = m.card_chain(last_id, 50);
    let chain_t = t.elapsed();
    let plain: Vec<u64> = { // reference: follow prev one hop at a time
        let mut v = vec![last_id as u32];
        while v.len() < 50 { match m.edges(*v.last().unwrap()).iter().find(|e| e.0 == crate::E_CPREV) { Some(e) => v.push(e.1), None => break } }
        v.into_iter().map(|x| x as u64).collect()
    };
    let chain_ok = chain.iter().map(|c| c.0).collect::<Vec<_>>() == plain && chain.len() == 50;
    let skips_used = chain.iter().filter(|c| m.edges(c.0 as u32).iter().any(|e| e.0 == crate::E_SKIP)).count();
    let pinned = m.pinned_facts(PROJECT);
    let pinned_ok = pinned.iter().any(|f| f.contains("blake3")) && !pinned.iter().any(|f| f.contains("xxh3")) && pinned.iter().any(|f| f.contains("Larkspur"));
    let worker_peak = m.worker_peak.get();
    crate::trim_heap();
    let rss_live = cur_rss_bytes();
    drop(m);
    crate::trim_heap();
    let rss_after_a = cur_rss_bytes();
    let m2 = Memory::open(&work).expect("reopen A");
    let latest_reopen = m2.latest_card(PROJECT);
    let card_reopen_ok = latest_reopen.as_ref().map(|c| c.0) == Some(ing.last_card) && latest_reopen.as_ref().map_or(false, |c| c.1.contains("session 30, end"));
    drop(m2);

    // ===== store B: compact_every = 0, no auto-embedding → everything in the hot log =====
    let wb = fresh(dir, "disk");
    let mut mb = Memory::open(&wb).expect("open B");
    mb.compact_every = 0; mb.auto_index = false; mb.use_worker = false;
    ingest(&mut mb, &conv, |_| {});
    drop(mb);
    let emb = if model.is_some() { run_child("embed", &wb) } else { None };
    let wb2 = fresh(dir, "disk2");
    copy_dir(&wb, &wb2);
    let cmp = run_child("compact", &wb2);
    let srch = run_child("search", &wb2);
    let _ = std::fs::remove_dir_all(&wb2);

    let mut mb = Memory::open(&wb).expect("reopen B");
    mb.use_worker = false;
    let logs_before = bytes_where(&wb, |n| n.starts_with("log-") || n == "index.mg");
    let vec_before = bytes_where(&wb, |n| n.starts_with("vectors"));
    let disk_before = dir_bytes(&wb);
    let groups_before = mb.alias.groups.len();
    let b1 = (ask(&mut mb, &qs, true, true), ask(&mut mb, &qs, false, false));
    mb.compact().expect("compact B");
    let logs_after = bytes_where(&wb, |n| n.starts_with("log-") || n == "index.mg");
    let vec_after = bytes_where(&wb, |n| n.starts_with("vectors"));
    let disk_after = dir_bytes(&wb);
    let b2 = (ask(&mut mb, &qs, true, true), ask(&mut mb, &qs, false, false));
    let stb = mb.stats();
    drop(mb);
    let mut mb = Memory::open(&wb).expect("reopen B after compact");
    mb.use_worker = false;
    let b3 = (ask(&mut mb, &qs, true, true), ask(&mut mb, &qs, false, false));
    let groups_after = mb.alias.groups.len();
    drop(mb);
    let _ = std::fs::remove_dir_all(&wb);

    // ===== store C: creep check — the conversation 3× through the worker path (embedding included) =====
    let wc = fresh(dir, "creep");
    let mut mc = Memory::open(&wc).expect("open C");
    let mut creep = vec![];
    for _ in 0..3 {
        ingest(&mut mc, &conv, |_| {});
        let _ = ask(&mut mc, &qs, true, true);
        crate::trim_heap();
        creep.push((cur_rss_bytes(), malloc_in_use()));
    }
    drop(mc);
    let _ = std::fs::remove_dir_all(&wc);

    let rss = peak_rss_bytes();
    let (c_open, c_prog, c_turns, c_recall, c_det) = crash_test(dir);
    let _ = std::fs::remove_dir_all(&work);

    // ----- derived numbers -----
    let pctl = |v: &mut Vec<Duration>, q: f64| { v.sort(); v[((v.len() as f64 * q).ceil() as usize).saturating_sub(1).min(v.len() - 1)].as_secs_f64() * 1e3 };
    let mut lat_kg: Vec<Duration> = graph.lat.iter().chain(&kw.lat).copied().collect();
    let mut lat_v: Vec<Duration> = full.lat.iter().chain(&kwv.lat).chain(&sem_on.lat).copied().collect();
    let (p50, p99, v50, v99) = (pctl(&mut lat_kg, 0.5), pctl(&mut lat_kg, 0.99), pctl(&mut lat_v, 0.5), pctl(&mut lat_v, 0.99));
    let rw = |i: usize| qs[i].reworded;
    let r3 = |r: &Run, f: &dyn Fn(usize) -> bool| pct(&r.pass, f);
    let (o_full, o_graph, o_kw) = (r3(&full, &|i| !rw(i)), r3(&graph, &|i| !rw(i)), r3(&kw, &|i| !rw(i)));
    let (w_full, w_graph, w_kw, w_kwv) = (r3(&full, &rw), r3(&graph, &rw), r3(&kw, &rw), r3(&kwv, &rw));
    let ratio = logs_before as f64 / logs_after.max(1) as f64;
    let lost = c_prog as i64 - c_turns as i64;
    let crash_ok = c_open && c_recall && (-1..=1).contains(&lost);
    let off_same = b1.1.ids == b2.1.ids && b2.1.ids == b3.1.ids;
    let on_same = b2.0.ids == b3.0.ids;
    let on_diff = b1.0.ids.iter().zip(&b2.0.ids).filter(|(a, b)| a != b).count();
    let vec_on = model.is_some();
    let ch = |c: &Option<Child>| c.as_ref().map_or("n/a".to_string(), |c| format!("{:.1} MB, {:.0} ms", c.rss as f64 / MB, c.ms));

    let pf = |b: bool| if b { "PASS" } else { "FAIL" };
    let mut rows: Vec<(String, String, String, Option<bool>)> = vec![
        ("recall@3 overall (graph + vectors)".into(), format!("{:.0}%", all(&full.pass)), "≥ 80%".into(), Some(all(&full.pass) >= 80.0)),
        ("recall@3 original: full / graph / keyword".into(), format!("{o_full:.0}% / {o_graph:.0}% / {o_kw:.0}%"), "".into(), None),
        ("recall@3 reworded: graph (aliases) vs keyword".into(), format!("{w_graph:.0}% / {w_kw:.0}%"), "graph ≥ keyword + 10 pts".into(), Some(w_graph >= w_kw + 10.0)),
        ("recall@3 reworded: vectors on / off (graph on)".into(), format!("{w_full:.0}% / {w_graph:.0}%"), "on ≥ off".into(), if vec_on { Some(w_full >= w_graph) } else { None }),
        ("recall@3 reworded: keyword + vectors".into(), format!("{w_kwv:.0}%"), "".into(), None),
        ("no-shared-word questions (5): vectors on / off".into(), format!("{:.0}% / {:.0}%", all(&sem_on.pass), all(&sem_off.pass)), "on > off".into(), if vec_on { Some(all(&sem_on.pass) > all(&sem_off.pass)) } else { None }),
        ("hot-log recall: final-session fact before compaction (graph / keyword)".into(), format!("{} / {}", pf(hot_on.pass[0]), pf(hot_off.pass[0])), "found, hit id in hot log".into(), Some(hot_on.pass[0] && hot_off.pass[0] && hot_in_log)),
        ("same fact after compaction".into(), pf(hot_after.pass[0]).into(), "found".into(), Some(hot_after.pass[0])),
        ("query p50 / p99, graph+keyword (no vectors)".into(), format!("{p50:.3} ms / {p99:.3} ms"), "< 5 ms / < 20 ms".into(), Some(p50 < 5.0 && p99 < 20.0)),
        ("query p50 / p99, hybrid (incl. query embedding)".into(), format!("{v50:.1} ms / {v99:.1} ms"), "< 20 ms / < 50 ms".into(), if vec_on { Some(v50 < 20.0 && v99 < 50.0) } else { None }),
        ("add_turn mean".into(), format!("{:.1} µs", ing.add.as_secs_f64() * 1e6 / ing.turns as f64), "< 1 ms".into(), Some(ing.add.as_secs_f64() * 1e6 / (ing.turns as f64) < 1000.0)),
        ("embedding: peak RSS, time (child)".into(), emb.as_ref().map_or("n/a".into(), |c| format!("{:.1} MB, {:.0} ms for {} texts ({:.1} ms/text)", c.rss as f64 / MB, c.ms, c.n, c.ms / c.n.max(1) as f64)), "≤ 32 MB".into(), emb.as_ref().map(|c| c.rss as f64 <= 32.0 * MB)),
        ("compaction: peak RSS, time (child)".into(), ch(&cmp), "≤ 32 MB".into(), cmp.as_ref().map(|c| c.rss as f64 <= 32.0 * MB)),
        ("hybrid search: peak RSS (child, 25 queries)".into(), ch(&srch), "≤ 32 MB".into(), srch.as_ref().map(|c| c.rss as f64 <= 32.0 * MB)),
        ("parent RSS (store A, worker processes): baseline → live store after full run → after drop".into(), format!("{:.1} → {:.1} → {:.1} MB", rss0 as f64 / MB, rss_live as f64 / MB, rss_after_a as f64 / MB), "".into(), None),
        ("parent RSS held by the live Memory (live − dropped)".into(), format!("{:+.2} MB", (rss_live as f64 - rss_after_a as f64) / MB), "~0 MB".into(), Some((rss_live as f64 - rss_after_a as f64) / MB < 0.5)),
        ("parent RSS growth over the full run (live − warm baseline)".into(), format!("{:+.2} MB", (rss_live as f64 - rss0 as f64) / MB), "".into(), None),
        ("parent creep: RSS after 3 more full runs (store C, workers)".into(), format!("{} MB; malloc live {} MB", creep.iter().map(|c| format!("{:.2}", c.0 as f64 / MB)).collect::<Vec<_>>().join(" → "), creep.iter().map(|c| format!("{:.2}", c.1 as f64 / MB)).collect::<Vec<_>>().join(" → ")), "plateau: last − first < 0.5 MB".into(), Some((creep[2].0 as f64 - creep[0].0 as f64) / MB < 0.5)),
        ("worker child peak RSS (store A: embed/compact/query)".into(), format!("{:.1} MB", worker_peak as f64 / MB), "≤ 32 MB".into(), Some(worker_peak as f64 <= 32.0 * MB)),
        ("peak RSS, bench process (store B in-process, all asks)".into(), format!("{:.1} MB", rss as f64 / MB), "≤ 32 MB".into(), Some(rss as f64 <= 32.0 * MB)),
        ("pinned_facts: latest per key".into(), format!("{} facts", pinned.len()), "latest only".into(), Some(pinned_ok)),
        ("disk: hot logs → index (compact)".into(), format!("{:.1} KB → {:.1} KB ({ratio:.2}×)", logs_before as f64 / 1024.0, logs_after as f64 / 1024.0), "≥ 2×".into(), Some(ratio >= 2.0)),
        ("disk: vectors (int8, 396 B each)".into(), format!("{:.1} KB → {:.1} KB, {} vectors; dir total {:.1} → {:.1} KB", vec_before as f64 / 1024.0, vec_after as f64 / 1024.0, stb.vectors, disk_before as f64 / 1024.0, disk_after as f64 / 1024.0), "".into(), None),
        ("recall@3 before / after compact / after reopen (full)".into(), format!("{:.0}% / {:.0}% / {:.0}%", all(&b1.0.pass), all(&b2.0.pass), all(&b3.0.pass)), "no loss".into(), Some(all(&b2.0.pass) >= all(&b1.0.pass) && all(&b3.0.pass) == all(&b2.0.pass))),
        ("recall@3 before / after compact / after reopen (keyword)".into(), format!("{:.0}% / {:.0}% / {:.0}%", all(&b1.1.pass), all(&b2.1.pass), all(&b3.1.pass)), "".into(), None),
        ("identical top-3 ids after compact & reopen".into(), format!("keyword {}; full compact→reopen {}; full hot→compact differs on {on_diff}/20 (co-occurrence is built at compaction)", if off_same { "yes" } else { "no" }, if on_same { "yes" } else { "no" }), "yes / yes".into(), Some(off_same && on_same)),
        ("alias groups survive compact + reopen".into(), format!("{groups_before} → {groups_after}"), "equal, > 0".into(), Some(groups_before == groups_after && groups_before > 0)),
        ("latest_card after reopen".into(), latest_reopen.as_ref().map_or("none".into(), |c| format!("#{} \"{}\"", c.0, cut(&c.1, 48))), format!("#{}", ing.last_card), Some(card_reopen_ok)),
        ("card_chain 50 (milestone skips)".into(), format!("{:.1} µs, {} cards, {} milestones used, matches prev walk: {}", chain_t.as_secs_f64() * 1e6, chain.len(), skips_used, chain_ok), "= prev walk".into(), Some(chain_ok)),
        ("recall: fact only in an old card (graph / keyword)".into(), format!("{} / {}", pf(card_on.pass[0]), pf(card_off.pass[0])), "found".into(), Some(card_on.pass[0])),
        ("crash recovery".into(), format!("opened={c_open} child={c_prog} stored={c_turns} lost={lost} recall={c_recall}"), "opens, ≤ 1 lost".into(), Some(crash_ok)),
    ];
    if !vec_on { rows.push(("embedding model".into(), "not found: vectors off".into(), "".into(), None)) }

    println!("elisa-mem bench: {} sessions, {} turns, {} notes, {} cards; ingest {:.0} ms (end_session incl. embedding {:.0} ms)",
        conv.len(), ing.turns, ing.notes, ing.cards, ing.total.as_secs_f64() * 1e3, ing.end.as_secs_f64() * 1e3);
    println!("stats A: turns={} notes={} facts={} cards={} entities={} edges={} vectors={} ({} B) cold={}B",
        st.turns, st.notes, st.facts, st.cards, st.entities, st.edges, st.vectors, st.vector_bytes, st.cold_bytes);
    { let g = plog.lock().unwrap(); println!("progress (store A via workers): {} updates: {}", g.0, g.1.join(" | ")); }
    for (k, c) in [("embed", &emb), ("compact", &cmp)] { if let Some(c) = c { println!("progress ({k}): {} updates, first \"{}\", last \"{}\"", c.updates, c.first, c.last) } }
    println!("crash: {c_det}\n");
    println!("| Check | Value | Target | PASS/FAIL |\n|---|---|---|---|");
    for (c, v, t, ok) in &rows { println!("| {c} | {v} | {t} | {} |", ok.map(pf).unwrap_or("-")); }

    let mut any = false;
    let sets: [(&str, &Run, &[Question]); 6] = [("full", &full, &qs), ("graph", &graph, &qs), ("keyword", &kw, &qs), ("semantic, vectors on", &sem_on, &sem), ("semantic, vectors off", &sem_off, &sem), ("card, graph", &card_on, &cardq)];
    for (label, run, qq) in sets {
        for (i, q) in qq.iter().enumerate() {
            if run.pass[i] { continue; }
            if !any { println!("\nFailed questions:"); any = true; }
            println!("\n[{label}] {} ({}{}) want {:?}", q.text, q.fact, if q.reworded { ", reworded" } else { "" }, q.want);
            if run.hits[i].is_empty() { println!("   (no hits)"); }
            for (j, h) in run.hits[i].iter().take(3).enumerate() { println!("   {}. [{:?} s{} {:.3}] {}", j + 1, h.kind, h.session, h.score, cut(&h.text, 100)); }
        }
    }
    if rows.iter().all(|r| r.3 != Some(false)) { 0 } else { 1 }
}
