//! Two processes on one memory store: two Elisa sessions with the same HOME and different projects.
//! Session A idles while session B writes turns, ends its session (/clear), writes again and compacts; A's next
//! turn used to panic in elisa-mem (`text`: index out of bounds, B had rewritten the shared hot files under A), and
//! the ids both handed out collided. The child process is this test binary run again with `MEM_RACE_CHILD` set.
use elisa_mem::{Memory, Role};
use std::path::{Path, PathBuf};
use std::process::Command;

fn open(d: &Path) -> Memory {
    let mut m = Memory::open(d).unwrap();
    // no embedding model and no worker processes in a test (the worker would be this test binary)
    (m.use_vectors, m.use_worker, m.auto_index, m.compact_every) = (false, false, false, 0);
    m
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("elisa-memrace-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Session B (the child process): turns, end of session, a new session, more turns, a compaction, interleaved with
/// whatever A does at the same time.
fn session_b(d: &Path, rounds: usize) {
    let mut b = open(d);
    let s = b.begin_session("/proj/b");
    for i in 0..rounds { b.add_turn(s, Role::User, &format!("beta owner Bob widget {i}")).unwrap(); }
    b.end_session(s).unwrap();
    let s = b.begin_session("/proj/b");
    for i in 0..rounds {
        b.add_turn(s, Role::Assistant, &format!("beta reply {i}")).unwrap();
        let _ = b.recall("beta owner Bob widget", "/proj/b", 3, true);
        if i == rounds / 2 { b.compact().unwrap(); }
    }
    b.end_session(s).unwrap();
}

#[test]
fn mem_race_child() {
    // a no-op unless run as the child of `two_processes_share_one_memory_store`
    let Ok(d) = std::env::var("MEM_RACE_CHILD") else { return };
    let rounds = std::env::var("MEM_RACE_ROUNDS").ok().and_then(|r| r.parse().ok()).unwrap_or(40);
    session_b(Path::new(&d), rounds);
}

fn child(d: &Path, rounds: usize) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "mem_race_child", "--nocapture", "--test-threads", "1"])
        .env("MEM_RACE_CHILD", d).env("MEM_RACE_ROUNDS", rounds.to_string())
        .spawn().unwrap()
}

/// The sequence: A opens and writes, B runs to the end (turns, end, new session, compaction), then A's next
/// recall and write. Then both at once, many writes each: every record lands once, ids stay dense, nothing panics.
#[test]
fn two_processes_share_one_memory_store() {
    let d = tmp("seq");
    let mut a = open(&d);
    let sa = a.begin_session("/proj/a");
    a.add_turn(sa, Role::User, "hi there, what is 5+5?").unwrap();
    // B runs entirely while A idles
    assert!(child(&d, 6).wait().unwrap().success(), "session B failed");
    // A's next turn: recall over its old snapshot, then writes (which catch up with B first)
    for h in a.recall("my token please remember it", "/proj/a", 5, true) { assert!(!h.text.is_empty()); }
    a.add_turn(sa, Role::User, "by the way my token is secret, please remember it").unwrap();
    a.add_turn(sa, Role::Assistant, "noted").unwrap();
    assert!(a.recall("beta owner Bob widget", "/proj/a", 5, true).iter().any(|h| h.text.contains("Bob")), "A sees B's memories after catching up");
    assert!(a.recall("token please remember", "/proj/a", 5, true).iter().any(|h| h.text.contains("remember it")));
    a.end_session(sa).unwrap();
    drop(a);
    let st = open(&d).stats();
    assert_eq!(st.turns, 3 + 6 + 6, "{st:?}");

    // both at once
    let d = tmp("par");
    let mut a = open(&d);
    let sa = a.begin_session("/proj/a");
    let rounds = 40;
    let mut c = child(&d, rounds);
    for i in 0..rounds * 2 {
        a.add_turn(sa, Role::User, &format!("alpha turn {i}")).unwrap();
        for h in a.recall("alpha turn beta reply", "/proj/a", 5, true) { assert!(!h.text.is_empty()); }
        if i % 10 == 0 { a.refresh().unwrap(); }
    }
    assert!(c.wait().unwrap().success(), "session B failed");
    a.end_session(sa).unwrap();
    drop(a);
    let r = open(&d);
    let st = r.stats();
    assert_eq!(st.turns, rounds * 2 + rounds * 2, "every turn of both processes is in the store once: {st:?}");
    assert_eq!(st.sessions, 3, "{st:?}");
    for i in [0, rounds, rounds * 2 - 1] {
        let q = format!("alpha turn {i}");
        assert!(r.recall(&q, "/proj/a", 5, false).iter().any(|h| h.text == q), "{q} missing");
    }
    drop(r);
    let _ = std::fs::remove_dir_all(&d);
}
