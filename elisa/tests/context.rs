use elisa::context::{self, estimate_messages, estimate_tokens, Store};
use elisa::types::{Block, Message, Role};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("elisa-t1-ctx-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn estimates() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abc"), 1);
    assert_eq!(estimate_tokens(&"a".repeat(33)), 10);
    assert_eq!(estimate_tokens(&"a".repeat(34)), 11);
    assert_eq!(estimate_tokens(&"é".repeat(33)), 10, "counts chars, not bytes");
    assert_eq!(estimate_messages(&[]), 0);
    assert_eq!(estimate_messages(&[Message::user_text("")]), 6);
    assert_eq!(estimate_messages(&[Message::user_text("a".repeat(33)), Message::assistant_text("b")]), 6 + 10 + 6 + 1);
    let input = json!({"path": "x".repeat(330)});
    let tu = Message { role: Role::Assistant, content: vec![Block::ToolUse { id: "i".into(), name: "read".into(), input: input.clone() }], ..Default::default() };
    assert!(estimate_messages(&[tu]) >= 6 + estimate_tokens(&input.to_string()));
}

#[test]
fn system_prompt_stable_notes_last_caps() {
    let cwd = Path::new("/some/project");
    let a = context::system_prompt(cwd, None, Some("PROJECT RULES"), Some("NOTE BODY"));
    let b = context::system_prompt(cwd, None, Some("PROJECT RULES"), Some("NOTE BODY"));
    assert_eq!(a.as_bytes(), b.as_bytes());
    assert!(!a.contains("/some/project"), "cwd is in the first message, not the system prompt");
    assert!(!a.contains("{cwd}") && !a.contains("{os}") && !a.contains("{date}"));
    assert_eq!(context::system_prompt(Path::new("/other"), None, None, None), context::system_prompt(cwd, None, None, None));
    let f = context::first_message(cwd, Some("NOTE BODY"), "do it");
    assert!(f.starts_with("Working directory: /some/project\nOS: ") && f.contains(std::env::consts::OS));
    assert!(f.contains(&format!("{}\nNOTE BODY\n", context::NOTES_HEADING)) && f.ends_with("## Request\ndo it"));
    assert!(!context::first_message(cwd, None, "x").contains("## "));
    let date = f.lines().find(|l| l.starts_with("Date: ")).expect("date line");
    let d = &date[6..];
    assert_eq!(d.len(), 10, "{d}");
    assert!(d.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() }), "{d}");
    let pi = a.find("## Project instructions (ELISA.md)").unwrap();
    let ni = a.find("## Notes from earlier sessions").unwrap();
    assert!(pi < ni);
    assert!(a.trim_end().ends_with("NOTE BODY"), "notes must be last");

    let base = context::system_prompt(cwd, None, None, None);
    assert!(a.starts_with(base.trim_end()), "base part must be a stable prefix");
    let with_notes = context::system_prompt(cwd, None, None, Some("n"));
    assert!(with_notes.starts_with(base.trim_end()));
    assert!(!base.contains("## Notes"));
    assert_eq!(context::system_prompt(cwd, None, Some("  "), Some("")), base);

    let big = context::system_prompt(cwd, None, Some(&"p".repeat(10_000)), Some(&"n".repeat(10_000)));
    assert_eq!(big.matches('p').count() - base.matches('p').count() - "## Project instructions".matches('p').count(), 3000);
    let notes_part = &big[big.find("## Notes from earlier sessions").unwrap()..];
    assert_eq!(notes_part.matches('n').count() - "## Notes from earlier sessions".matches('n').count() - "\n[... cut at 4000 chars]".matches('n').count(), 4000);

    let toks = estimate_tokens(&base);
    let tools = estimate_tokens(&serde_json::to_string(&elisa::tools::defs()).unwrap());
    println!("system prompt ~{toks} tokens ({} chars), tools ~{tools}, total ~{}", base.chars().count(), toks + tools);
    let raw = estimate_tokens(include_str!("../prompts/system.md"));
    // +1 line (2026-11-01, one mode): "use tools only when … answer plain questions directly"
    // +1 line (2026-11-01, after a reset the model typed tool calls as text): "call tools via the tool interface"
    // +1 line and +3 tools (2026-11-03, syntax-tree tools outline / read_symbol / edit_symbol): +~34 prompt, +~250 tools
    // tokens, paid once in the cached system prefix; `ast = false` drops both
    // +1 line (2026-11-04) "use the session's language …", then -1 (2026-11-08) when the session language went
    assert!(raw <= 376, "system.md ~{raw} tokens");
    let plain = estimate_tokens(&serde_json::to_string(&elisa::tools::defs_for(false)).unwrap());
    let plain_sys = estimate_tokens(&base.lines().filter(|l| !l.contains("`outline`")).collect::<Vec<_>>().join("\n"));
    assert!(plain_sys + plain <= 790, "without ast: system + tools ~{} tokens", plain_sys + plain);
    assert!(toks + tools <= 1120, "system + tools ~{} tokens", toks + tools);
}

#[test]
fn system_prompt_override_file() {
    let t = Tmp::new();
    let f = t.0.join("sys.md");
    std::fs::write(&f, "Custom at {cwd}\n").unwrap();
    let s = context::system_prompt(Path::new("/w"), Some(&f), None, Some("N"));
    assert!(s.starts_with("Custom at /w\n"));
    assert!(s.trim_end().ends_with("N"));
}

fn tool_turn(i: usize, big: usize) -> Vec<Message> {
    let id = format!("tu_{i}");
    vec![
        Message { role: Role::Assistant, content: vec![
            Block::Text { text: format!("step {i} ") + &"t".repeat(big) },
            Block::ToolUse { id: id.clone(), name: "write".into(), input: json!({"path": format!("f{i}.rs"), "content": "c".repeat(big)}) },
        ], ..Default::default() },
        Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: id, content: format!("first line {i}\n") + &"r\n".repeat(big / 2), is_error: false }], ..Default::default() },
    ]
}

fn history(turns: usize, big: usize) -> Vec<Message> {
    let mut h = vec![];
    for i in 0..turns {
        h.push(Message::user_text(format!("task {i}")));
        h.extend(tool_turn(i, big));
        h.push(Message::assistant_text(format!("done {i} ") + &"d".repeat(big)));
    }
    h
}

fn pairs_ok(msgs: &[Message]) -> bool {
    for (i, m) in msgs.iter().enumerate() {
        for b in &m.content {
            if let Block::ToolUse { id, .. } = b {
                let ok = msgs.get(i + 1).is_some_and(|n| n.content.iter().any(|b| matches!(b, Block::ToolResult { tool_use_id, .. } if tool_use_id == id)));
                if !ok { return false; }
            }
        }
    }
    true
}

#[test]
fn trim_old_contract() {
    let orig = history(8, 2000);
    let mut h = orig.clone();
    let before = estimate_messages(&h);
    let target = before / 2;
    let freed = context::trim_old(&mut h, 3, target);
    let after = estimate_messages(&h);
    println!("trim: {before} -> {after} (target {target}, freed {freed})");
    assert_eq!(h.len(), orig.len(), "never removes messages");
    assert!(pairs_ok(&h));
    for (a, b) in h.iter().zip(&orig) {
        assert_eq!(a.role, b.role);
        assert_eq!(a.content.len(), b.content.len());
        for (x, y) in a.content.iter().zip(&b.content) {
            match (x, y) {
                (Block::ToolUse { id: i1, name: n1, .. }, Block::ToolUse { id: i2, name: n2, .. }) => assert!(i1 == i2 && n1 == n2),
                (Block::ToolResult { tool_use_id: i1, .. }, Block::ToolResult { tool_use_id: i2, .. }) => assert_eq!(i1, i2),
                (Block::Text { .. }, Block::Text { .. }) => {}
                _ => panic!("block kind changed"),
            }
        }
    }
    assert!(after <= target, "after {after} > target {target}");
    assert_eq!(freed, before - after);
    // last 3 user text turns and everything after are untouched
    let start = orig.iter().enumerate().filter(|(_, m)| m.role == Role::User && matches!(m.content[0], Block::Text { .. })).map(|(i, _)| i).rev().nth(2).unwrap();
    assert_eq!(&h[start..], &orig[start..]);
    // shapes of trimmed blocks
    let Block::ToolResult { content, .. } = &h[2].content[0] else { panic!() };
    assert!(content.starts_with("[trimmed: first line 0 … ") && content.ends_with(" lines]"), "{content}");
    let Block::ToolUse { input, .. } = &h[1].content[1] else { panic!() };
    assert_eq!(input["content"], json!("[trimmed 2000 chars]"));
    assert_eq!(input["path"], json!("f0.rs"));
    let Block::Text { text } = &h[3].content[0] else { panic!() };
    assert_eq!(text.chars().count(), 301);
    assert!(text.ends_with('…'));
}

#[test]
fn trim_old_stops_early_and_noop() {
    let orig = history(8, 2000);
    let mut h = orig.clone();
    let before = estimate_messages(&h);
    assert_eq!(context::trim_old(&mut h, 3, before), 0);
    assert_eq!(h, orig);
    // small cut: only the oldest result needs trimming
    let target = before - 100;
    let freed = context::trim_old(&mut h, 3, target);
    assert!(freed >= 100);
    assert_ne!(h[2], orig[2]);
    assert_eq!(&h[3..], &orig[3..], "should stop after the first trim");
    // fewer user turns than keep_recent → nothing touched
    let mut h2 = history(2, 2000);
    let o2 = h2.clone();
    assert_eq!(context::trim_old(&mut h2, 3, 0), 0);
    assert_eq!(h2, o2);
    // unreachable target: trims what it can, still valid
    let mut h3 = history(5, 2000);
    context::trim_old(&mut h3, 1, 0);
    assert_eq!(h3.len(), 20);
    assert!(pairs_ok(&h3));
    // multibyte content must not panic
    let mut h4 = history(4, 0);
    h4[2] = Message { role: Role::User, content: vec![Block::ToolResult { tool_use_id: "tu_0".into(), content: "😀".repeat(500), is_error: false }], ..Default::default() };
    h4[3] = Message::assistant_text("ä".repeat(900));
    context::trim_old(&mut h4, 1, 0);
    assert!(pairs_ok(&h4));
}

#[test]
fn compact_request_fits_and_cuts() {
    let h = history(60, 3000);
    let total = estimate_messages(&h);
    let budget = 4000;
    let m = context::compact_request(&h, Some("OLD NOTES"), budget);
    assert_eq!(m.role, Role::User);
    let est = estimate_messages(std::slice::from_ref(&m));
    println!("compact: history ~{total} tokens -> request ~{est} (budget {budget})");
    assert!(est <= budget, "{est} > {budget}");
    let Block::Text { text } = &m.content[0] else { panic!() };
    assert!(text.contains("[... earlier part cut ...]"));
    assert!(text.find("OLD NOTES").unwrap() < text.find("Transcript:").unwrap());
    assert!(text.contains("User: task 0"), "keeps start");
    assert!(text.contains("done 59"), "keeps end");
    assert!(text.contains("[tool write f0.rs]"));
    assert!(text.contains("[result: first line 0"));
    assert!(!text.contains("User: task 30\n"), "middle cut");
    assert!(text.trim_end().ends_with("Output only the notes, nothing before or after."));
    assert!(text.contains("## Goal") && text.contains("## Facts to remember"));
    let pre = &text[..text.find("[... earlier part cut ...]").unwrap()];
    let post = &text[text.find("[... earlier part cut ...]").unwrap()..];
    assert!(pre.chars().count() < post.chars().count());

    for budget in [600, 1500, 12_000] {
        let m = context::compact_request(&h, None, budget);
        let est = estimate_messages(std::slice::from_ref(&m));
        assert!(est <= budget, "budget {budget}: {est}");
    }
    let m = context::compact_request(&h[..4], None, 4000);
    let Block::Text { text } = &m.content[0] else { panic!() };
    assert!(!text.contains("earlier part cut"));
    assert!(!text.contains("Earlier notes"));
    let res = text.lines().find(|l| l.starts_with("[result: ")).unwrap();
    assert!(res.chars().count() <= "[result: ".len() + 401 + 1, "result capped at 400: {}", res.chars().count());

    let multi = vec![Message::user_text("😀ä".repeat(20_000))];
    let m = context::compact_request(&multi, None, 1000);
    assert!(estimate_messages(&[m]) <= 1000);
}

#[test]
fn store_notes_sessions() {
    let t = Tmp::new();
    let st = Store::new(&t.0);
    assert_eq!(st.dir, t.0.join(".elisa"));
    assert!(st.load_notes().is_none());
    assert!(st.load_project().is_none());
    assert!(st.clear_notes().is_ok(), "clearing absent notes is fine");
    st.save_notes("  hello notes \n").unwrap();
    assert_eq!(st.load_notes().as_deref(), Some("hello notes\n"));
    st.clear_notes().unwrap();
    assert!(st.load_notes().is_none());

    std::fs::write(t.0.join("ELISA.md"), "proj rules").unwrap();
    assert_eq!(st.load_project().as_deref(), Some("proj rules"));

    assert!(st.load_last_session().is_none());
    let sd = st.dir.join("sessions");
    std::fs::create_dir_all(&sd).unwrap();
    let older = vec![Message::user_text("older")];
    let newer = vec![Message::user_text("newer"), Message::assistant_text("x")];
    std::fs::write(sd.join("1000.json"), serde_json::to_string(&older).unwrap()).unwrap();
    std::fs::write(sd.join("900000.json"), serde_json::to_string(&newer).unwrap()).unwrap();
    std::fs::write(sd.join("5000.json"), serde_json::to_string(&older).unwrap()).unwrap();
    std::fs::write(sd.join("99999999.txt"), "junk").unwrap();
    assert_eq!(st.load_last_session(), Some(newer.clone()), "newest by timestamp, not by name");

    // this run's own file is saved and overwritten in place, and skipped by load_last_session
    let mine = vec![Message::user_text("mine")];
    let p1 = st.save_session(&mine).unwrap();
    let p2 = st.save_session(&[Message::user_text("mine2")]).unwrap();
    assert_eq!(p1, p2);
    let back: Vec<Message> = serde_json::from_str(&std::fs::read_to_string(&p2).unwrap()).unwrap();
    assert_eq!(back, vec![Message::user_text("mine2")]);
    assert_eq!(st.load_last_session(), Some(newer));
}

#[test]
fn state_card_drops_leaked_tool_markup() {
    // Test 23: a card ended in a literal `</parameter>` (no tool call opened; the gateway passed it through as text)
    let r = "Because remove mutates the map.\n<state>\ngoal: explain &mut vs &self\nnext: awaiting further request\n</parameter>\n</state>";
    let (vis, card, broken) = context::split_state(r);
    assert_eq!(vis, "Because remove mutates the map.");
    assert_eq!(card.as_deref(), Some("goal: explain &mut vs &self\nnext: awaiting further request"));
    assert!(!broken);
    assert_eq!(context::strip_markup("now: <function=read><parameter=path>a.rs</parameter></function>\n</tool_call>\nkeep: x </think>"),
        "now: a.rs\nkeep: x");
    // a card that was only markup is no card
    let (_, card, broken) = context::split_state("ok<state>\n</parameter>\n</function>\n</state>");
    assert!(card.is_none() && broken);
    // ordinary angle brackets survive
    assert_eq!(context::strip_markup("keep: Vec<&Item> vs Vec<Item>; a < b"), "keep: Vec<&Item> vs Vec<Item>; a < b");
}

#[test]
fn old_session_vote_field_still_loads() {
    // sessions saved before votes were removed carry "vote": ignored on load, the gateway id kept
    let m: Message = serde_json::from_str(r#"{"role":"assistant","content":[{"type":"text","text":"x"}],"id":"msg_1","vote":"like"}"#).unwrap();
    assert_eq!((m.role, m.id.as_deref()), (Role::Assistant, Some("msg_1")));
    assert!(!serde_json::to_string(&m).unwrap().contains("vote"));
}

/// 2026-11-01 (test-elisa sessions): after a reset the model treated Elisa's header, old notes and recalled
/// [memory] as the user's words. The request section and the summary transcript carry only what the user typed.
#[test]
fn user_words_kept_apart_from_elisa_context() {
    let cwd = Path::new("/p");
    let mem = vec!["Sea water is salty because of dissolved minerals".to_string()];
    let f = context::first_message_full(cwd, Some("NOTE-X"), Some("goal: g"), &mem, "how is it going?");
    let (head, req) = f.rsplit_once("## Request\n").unwrap();
    assert_eq!(req, "how is it going?", "memory never inside the request: {f}");
    assert!(head.contains(context::MEMORY_OPEN) && head.contains("- Sea water") && head.contains("[/memory]"));
    assert!(head.contains(context::NOTES_HEADING) && head.contains("written by Elisa, not the user"));
    assert!(head.find("NOTE-X").unwrap() < head.find("[state]").unwrap() && head.find("[state]").unwrap() < head.find("[memory").unwrap());
    // memory only: still a request heading, so the user's words stay separable
    let f = context::first_message_full(cwd, None, None, &mem, "hi");
    assert!(f.ends_with("## Request\nhi") && context::request_body(&f) == "hi", "{f}");
    // nothing extra: env lines, then the input
    assert!(context::first_message_full(cwd, None, None, &[], "x").ends_with("\n\nx"));

    // request_body: the user's words of every shape a session file may hold
    assert_eq!(context::request_body(&context::with_memory(&mem, "later question")), "later question");
    assert_eq!(context::request_body("[memory]\n- old style\n[/memory]\n\nsao rồi?"), "sao rồi?");
    assert_eq!(context::request_body("Working directory: /p\nOS: macos\nDate: 2026-11-01\n\n## Request\n[memory]\n- m\n[/memory]\n\nhi"), "hi");
    assert_eq!(context::request_body("plain text"), "plain text");

    // the summary transcript: header, old notes, [state], [memory] and Elisa's own correction are not "User:" lines
    let first = context::first_message_full(cwd, Some("NOTE-SECRET wants to learn"), Some("goal: CARD-SECRET"), &mem, "can you code?");
    let h = vec![
        Message::user_text(first),
        Message::assistant_text("Yes."),
        Message::user_text(context::with_memory(&["MEM-SECRET".to_string()], "add a login endpoint")),
        Message::assistant_text("<tool><tool_name>read</tool_name></tool>"),
        Message::user_text(context::tool_text_nudge(&["read"])),
        Message::assistant_text("Done."),
    ];
    let m = context::compact_request(&h, None, 4000);
    let Block::Text { text } = &m.content[0] else { panic!() };
    let tr = &text[text.find("Transcript:").unwrap()..text.find("The conversation above").unwrap()];
    assert!(tr.contains("User: can you code?\n") && tr.contains("User: add a login endpoint\n"), "{tr}");
    for s in ["NOTE-SECRET", "CARD-SECRET", "MEM-SECRET", "Sea water", "Working directory", "[elisa]"] { assert!(!tr.contains(s), "{s} in {tr}"); }
    assert_eq!(tr.matches("User: ").count(), 2, "{tr}");
}

#[test]
fn summary_prompt_records_only_what_was_said() {
    let c = context::compact_instructions();
    assert!(c.contains("quoted word for word"), "Goal quotes the request");
    assert!(c.contains("(unverified)"), "guesses are labelled");
    assert!(c.contains("Never invent a goal"), "no invented user traits");
    assert!(c.contains("not the user's words"), "notes/memory/tool output are not the user");
    for h in ["## Goal", "## Done", "## Files touched", "## Current state / next step", "## Facts to remember"] { assert!(c.contains(h), "{h}"); }
    assert!(c.contains("will be erased") && c.trim_end().ends_with("Output only the notes, nothing before or after."));
}

#[test]
fn text_tool_calls_are_detected() {
    // the exact reply from test-elisa session 1793541894 (twice in a row, nothing ran)
    let seen = "Mình cần xem lại file `main.py`.\n\n\n\n<tool>\n<tool_name>read_file</tool_name>\n<path>/x/python_files/main.py</path>\n</tool>";
    assert!(context::looks_like_text_tool_call(seen));
    for t in [
        "<tool_call>\n<function=read>\n<parameter=path>\na.rs\n</parameter>\n</function>\n</tool_call>",
        "Reading it.\n<function=bash>\n<parameter=command>ls</parameter>",
        "<invoke name=\"read\">",
        "{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}",
        "OK {\"tool\":\"bash\",\"input\":{\"command\":\"ls\"}}",
    ] { assert!(context::looks_like_text_tool_call(t), "{t}"); }
    for t in [
        "Done: added the endpoint. Vec<Item> and a < b are fine.",
        "Đã xong. Bạn muốn làm gì tiếp?",
        "Use `<tool_call>` tags? No: the gateway parses those.",
        "Example:\n```xml\n<tool>\n<tool_name>x</tool_name>\n</tool>\n```\nThat is the format.",
        "{\"name\": \"widget\", \"size\": 3}",
        "<tools> is a list",
    ] { assert!(!context::looks_like_text_tool_call(t), "{t}"); }
    let n = context::tool_text_nudge(&["read", "bash"]);
    assert!(n.starts_with(context::NUDGE_PREFIX) && n.contains("read, bash"), "{n}");
}

#[test]
fn recall_gate_needs_keywords_and_coverage() {
    // values seen on a copy of the user's store (2026-11-01, keyword part only)
    let (min, sup) = (10.0, 0.45);
    // fresh history: keyword score and coverage at the base bars
    assert!(context::recall_passes(12.0, 0.45, false, min, sup));
    assert!(!context::recall_passes(0.0, 1.0, false, min, sup), "vector-only hit (lex 0) never qualifies");
    assert!(!context::recall_passes(7.8, 0.42, false, min, sup), "weak keyword match (the river snippet)");
    assert!(!context::recall_passes(20.4, 0.40, false, min, sup), "common words only (the 'how is it going?' case)");
    assert!(!context::recall_passes(19.7, 0.09, false, min, sup), "covers little of the query");
    // mid-session: the keyword bar doubles
    assert!(!context::recall_passes(14.5, 1.0, true, min, sup), "short follow-up: low keyword mass");
    assert!(context::recall_passes(14.5, 1.0, false, min, sup));
    assert!(context::recall_passes(33.2, 0.45, true, min, sup));
}

#[test]
fn card_fold_merges_cards_into_notes_without_a_model_call() {
    use elisa::context::merge_card_notes;
    let cards = vec![
        "goal: word-count CLI\ndone: count_words lowercases\nkeep: tests in tests/\nnow: add top_n\nnext: wire --top".to_string(),
        "goal: word-count CLI with --top\ndone: add top_n; tests pass.\nnext: wire --top".to_string(),
    ];
    let n = merge_card_notes(Some("## Goal\n\"make a wc tool\""), &cards);
    assert_eq!(n, "## Goal\n\"make a wc tool\"\n\n## From state cards\ngoal: word-count CLI with --top\ndone: count_words lowercases; add top_n; tests pass\nkeep: tests in tests/\nopen: wire --top");
    // a later fold: done items accumulate, an open item that got done leaves `open`, the section is replaced
    let n2 = merge_card_notes(Some(&n), &["done: wire --top\nnext: README".to_string()]);
    assert!(n2.starts_with("## Goal\n\"make a wc tool\"\n\n## From state cards\n"), "{n2}");
    assert!(n2.contains("done: count_words lowercases; add top_n; tests pass; wire --top\n") && n2.ends_with("open: README"), "{n2}");
    assert_eq!(n2.matches("## From state cards").count(), 1);
    // bounded: the oldest done items go first
    let many: Vec<String> = (0..200).map(|i| format!("done: item number {i} finished")).collect();
    let n3 = merge_card_notes(None, &many);
    assert!(n3.chars().count() <= 1500 && n3.contains("item number 199") && !n3.contains("item number 0 "), "{n3}");
}

#[test]
fn step_results_are_water_filled_into_the_room_left() {
    use elisa::context::cap_step_results;
    let r = |id: &str, n: usize| Block::ToolResult { tool_use_id: id.into(), content: "x".repeat(n - 1) + "\n", is_error: false };
    let len = |b: &Block| match b { Block::ToolResult { content, .. } => content.chars().count(), _ => 0 };
    // fits: unchanged
    let mut v = vec![r("a", 1000), r("b", 2000)];
    assert_eq!(cap_step_results(&mut v, 5000), 0);
    // ui_run1: four ~6.4K-char files into ~2K tokens: the small one stays whole, the big ones share the rest
    let mut v = vec![r("a", 6500), r("b", 5700), r("c", 300), r("d", 6400)];
    assert_eq!(cap_step_results(&mut v, 2000), 3);
    assert_eq!(len(&v[2]), 300);
    let total: usize = v.iter().map(len).sum();
    assert!(total <= 2000 * 33 / 10, "{total}");
    let big: Vec<usize> = [0, 1, 3].iter().map(|&i| len(&v[i])).collect();
    assert!(big.iter().max().unwrap() - big.iter().min().unwrap() < 40, "{big:?}");
    match &v[0] { Block::ToolResult { content, .. } => assert!(content.ends_with(" chars]") && content.contains("\n[trimmed "), "{content}"), _ => unreachable!() }
    // no room at all: each keeps a head + marker, never empty
    let mut v = vec![r("a", 5000)];
    cap_step_results(&mut v, 0);
    assert!(len(&v[0]) > 100 && len(&v[0]) < 400);
}

#[test]
fn cut_read_says_the_window_is_full_not_where_to_go_on() {
    use elisa::context::{cap_step_results, READ_CUT};
    let body: String = (1..=54).map(|i| format!("{i}\t{}\n", " ".repeat(100))).collect();
    let read = format!("{body}[lines 1-54 of 313] next: offset=55");
    // the window is full: a head, the plain reason, no offset to follow, and still the minimum size
    let mut v = vec![Block::ToolResult { tool_use_id: "a".into(), content: read.clone(), is_error: false }];
    assert_eq!(cap_step_results(&mut v, 0), 1);
    match &v[0] {
        Block::ToolResult { content, .. } => {
            assert!(content.starts_with("1\t") && content.ends_with(READ_CUT), "{content}");
            assert!(!content.contains("next: offset"), "{content}");
            assert!(content.chars().count() <= 240, "{content}");
        }
        _ => unreachable!(),
    }
    // it fits: the footer stays
    let mut v = vec![Block::ToolResult { tool_use_id: "a".into(), content: read.clone(), is_error: false }];
    assert_eq!(cap_step_results(&mut v, 5000), 0);
    // any other result keeps its plain marker
    let mut v = vec![Block::ToolResult { tool_use_id: "a".into(), content: "x\n".repeat(3000), is_error: false }];
    cap_step_results(&mut v, 0);
    match &v[0] { Block::ToolResult { content, .. } => assert!(content.ends_with(" chars]") && !content.contains(READ_CUT), "{content}"), _ => unreachable!() }
}

#[test]
fn session_goal_from_the_first_request_or_a_stated_new_goal() {
    use elisa::context::{goal_statement, next_goal, with_session_goal, GOAL_NOTES};
    let first = "read python_files/main.py and list its routes in one line each. Our goal for this session: add a PUT /items/{item_id} endpoint, add a GET /items/search endpoint, and cap item quantity, all in python_files/main.py.";
    let g = next_goal(None, first).unwrap();
    assert!(g.starts_with("add a PUT /items/{item_id} endpoint, add a GET /items/search"), "{g}");
    // later requests (also a correction and a question about the goal) do not move it
    for later in ["add a quantity limit in python_files/main.py: ItemCreate.quantity must be between 1 and 50", "that's wrong, use 500 not 50",
        "what was the goal of this session?", "what is the goal?", "/stats"] {
        assert_eq!(next_goal(Some(&g), later).as_deref(), Some(g.as_str()), "{later}");
    }
    // no stated goal: the first request is the goal
    assert_eq!(next_goal(None, "fix load so it trims the name").as_deref(), Some("fix load so it trims the name"));
    // a stated new goal changes it
    assert_eq!(next_goal(Some(&g), "new goal: port the API to Go").as_deref(), Some("port the API to Go"));
    assert_eq!(goal_statement("ok. The goal is now to make the tests pass").as_deref(), Some("to make the tests pass"));
    assert_eq!(goal_statement("what was the goal of this session?"), None);
    // notes: the section goes on top and is replaced, not repeated
    let n = with_session_goal("## Goal\n\"latest\"\n\n## Done\n- x", "G1 goal");
    assert!(n.starts_with(&format!("{GOAL_NOTES}\nG1 goal\n\n## Goal")), "{n}");
    let n2 = with_session_goal(&n, "G2 goal");
    assert_eq!(n2.matches(GOAL_NOTES).count(), 1);
    assert!(n2.contains("G2 goal") && !n2.contains("G1 goal") && n2.contains("## Done\n- x"), "{n2}");
}

/// run2 call 30: the server had reported 8,047 prompt tokens + 54 output for call 29, call 30 added ~98 tokens of
/// text, and the chars/token estimate of the whole request was lower. The safe size takes the larger, and the request
/// does not fit 8,192 with the 3% margin and MIN_OUT, so it is never sent.
#[test]
fn safe_request_tokens_takes_the_report_and_the_ratio() {
    assert_eq!(context::safe_request_tokens(7_600, Some((8_101, 98)), None), 8_199);
    assert!(!context::request_fits(8_199, 256 + 64, 8_192));
    // after a rewrite (no report): the text estimate scaled by the last real/estimate ratio
    assert_eq!(context::safe_request_tokens(4_000, None, Some((5_000, 4_000))), 5_000);
    // an estimate that ran high is never scaled down
    assert_eq!(context::safe_request_tokens(4_000, None, Some((3_000, 4_000))), 4_000);
    assert_eq!(context::ctx_margin(8_192), 246);
    assert!(context::request_fits(8_192 - 246 - 320, 320, 8_192));
    assert!(!context::request_fits(8_192 - 246 - 319, 320, 8_192));
}

/// run2: a fold's one-line shrink turned "False\n[exit 0]" into the longer "[trimmed: False … 2 lines]".
#[test]
fn shrink_never_makes_a_result_longer() {
    let r = |id: &str, c: &str| Message::new(Role::User, vec![Block::ToolResult { tool_use_id: id.into(), content: c.into(), is_error: false }]);
    let big: String = (1..=40).map(|i| format!("line {i} of a long read\n")).collect();
    let mut msgs = vec![r("a", "False\n[exit 0]"), r("b", &big), r("c", "newest\n[exit 0]")];
    assert!(context::shrink_results_but_newest(&mut msgs));
    let content = |m: &Message| match &m.content[0] { Block::ToolResult { content, .. } => content.clone(), _ => String::new() };
    assert_eq!(content(&msgs[0]), "False\n[exit 0]");
    assert!(content(&msgs[1]).starts_with("[trimmed: line 1"));
    assert_eq!(content(&msgs[2]), "newest\n[exit 0]");
    // nothing left that a shrink would make shorter
    assert!(!context::shrink_results_but_newest(&mut msgs));
}

#[test]
fn a_digest_keeps_old_notes_and_the_newest_exchanges() {
    let h = vec![
        Message::user_text("first question"), Message::assistant_text("first answer"),
        Message::user_text("second question"), Message::assistant_text("long ".repeat(100)),
    ];
    let d = context::digest(&h, Some("OLD NOTES"), 10_000);
    assert!(d.starts_with("OLD NOTES"));
    assert!(d.contains("- User: first question") && d.contains("- Elisa: first answer"));
    assert!(d.lines().all(|l| l.chars().count() < 200), "each line is shortened");
    // too small for all: the newest stay
    let small = context::digest(&h, None, 220);
    assert!(small.contains("second question") && !small.contains("first question"));
}

#[test]
fn a_copied_card_instruction_is_not_shown() {
    // the constant must match the prompt's line, or the copies would show again
    assert!(include_str!("../prompts/system.md").contains(context::CARD_ECHO));
    let (vis, card, _) = context::split_state("2 + 3 = 5. <=80-token card of what the next step needs:\n<state>\ngoal: add\n</state>");
    assert_eq!((vis.as_str(), card.as_deref()), ("2 + 3 = 5.", Some("goal: add")));
    let (vis, _, broken) = context::split_state("Blue. <=80-token card of what the next step needs:");
    assert_eq!((vis.as_str(), broken), ("Blue.", true));
}
