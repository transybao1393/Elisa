//! Attached documents (src/attach.rs) end to end: `elisa -p "…" --attach FILE` against the scripted model
//! (`eval_fake::FakeModel`), the file always outside the project. A small document goes into the turn whole (one call,
//! no tools); a large one gets an overview, or the pages a question needs, inline, a refusal is retried once,
//! and `docs_pdf` still reads the copy;
//! docx and CSV are read; a file over the cap is refused with a reason and the turn goes on.
use elisa::eval_fake::{FakeModel, Scripted};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("elisa-attach-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// a minimal PDF, one Helvetica text line per page
fn pdf(pages: &[String]) -> Vec<u8> {
    let n = pages.len();
    let font = 3 + 2 * n;
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect::<Vec<_>>().join(" ")),
    ];
    for (i, t) in pages.iter().enumerate() {
        let c = format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET");
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + 2 * i));
        objs.push(format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()));
    }
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into());
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offs { out.extend(format!("{o:010} 00000 n \n").bytes()); }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    out
}

/// a Word .docx with the given paragraphs (stored, not deflated)
fn docx(paras: &[&str]) -> Vec<u8> {
    let body: String = paras.iter().map(|p| format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>")).collect();
    let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#);
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    w.start_file("[Content_Types].xml", o).unwrap();
    w.write_all(b"<Types/>").unwrap();
    w.start_file("word/document.xml", o).unwrap();
    w.write_all(xml.as_bytes()).unwrap();
    w.finish().unwrap().into_inner()
}

struct Run { code: i32, stderr: String, reqs: Vec<Value>, project: PathBuf }

/// `elisa -p PROMPT --attach F…` in a fresh project (route = "auto", the default) with HOME next to it
fn run(tag: &str, prompt: &str, files: &[PathBuf], script: Vec<Scripted>) -> Run { run_with(tag, prompt, files, script, &[], |_| {}) }

/// the same, with extra arguments and the project prepared first (a saved session, an upload)
fn run_with(tag: &str, prompt: &str, files: &[PathBuf], script: Vec<Scripted>, extra: &[&str], setup: impl FnOnce(&Path)) -> Run {
    let base = tmp(tag);
    let (project, home) = (base.join("project"), base.join("home"));
    std::fs::create_dir_all(&project).unwrap();
    setup(&project);
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), "memory = false\n").unwrap();
    let fake = FakeModel::start(script, 32768).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_elisa"));
    cmd.args(["-p", prompt, "--base-url", &fake.url(), "--type", "streamdense", "--model", "eval-fake"]).arg("--cwd").arg(&project);
    for f in files { cmd.arg("--attach").arg(f); }
    cmd.args(extra);
    for (k, _) in std::env::vars() { if k.starts_with("ELISA_") || k.starts_with("ANTHROPIC_") { cmd.env_remove(&k); } }
    let out = cmd.current_dir(&base).env("HOME", &home).output().unwrap();
    Run { code: out.status.code().unwrap_or(-1), stderr: String::from_utf8_lossy(&out.stderr).into(), reqs: fake.requests(), project }
}

fn reply(t: &str) -> Scripted { Scripted { text: Some(t.into()), ..Default::default() } }
fn tools(req: &Value) -> Vec<String> { req["tools"].as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()).unwrap_or_default() }
fn last_user(req: &Value) -> String { req["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "user").unwrap()["content"].to_string() }

/// the copies in the project's upload area
fn uploads(project: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for s in std::fs::read_dir(project.join(".elisa/uploads")).unwrap() {
        for f in std::fs::read_dir(s.unwrap().path()).unwrap() { v.push(f.unwrap().path()); }
    }
    v.sort();
    v
}

#[test]
fn a_small_pdf_goes_into_the_turn_whole_with_no_tool() {
    let outside = tmp("small-src");
    let f = outside.join("invoice.pdf");
    std::fs::write(&f, pdf(&["Invoice 4471 total due 312.50 EUR".into(), "Thank you for your order".into()])).unwrap();
    let r = run("small", "summarise this", &[f], vec![reply("Invoice 4471: 312.50 EUR due.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.reqs.len(), 1, "one model call");
    assert!(tools(&r.reqs[0]).is_empty(), "no tools: {:?}", tools(&r.reqs[0]));
    let u = last_user(&r.reqs[0]);
    assert!(u.contains("[Attached: invoice.pdf, 2 pages]") && u.contains("312.50 EUR") && u.contains("Thank you"), "{u}");
    assert!(u.contains("<untrusted source=\\\"attached invoice.pdf\\\">"), "fenced as outside text: {u}");
    // copied in from outside the project, under the session's upload area
    let up = uploads(&r.project);
    assert_eq!(up.len(), 1);
    assert!(up[0].ends_with("invoice.pdf") && up[0].starts_with(r.project.join(".elisa/uploads")), "{up:?}");
}

fn handbook_pdf() -> Vec<u8> {
    let pages: Vec<String> = (1..=40).map(|i| format!("Page {i} of the staff handbook says rule number {i} applies to every team member in every office and region, {}{}", "and it is repeated here so that the page carries a realistic amount of text for a document. ".repeat(30),
        if i == 23 { " Parking permits are issued by the facilities desk on the ground floor." } else { "" })).collect();
    pdf(&pages)
}

/// each test its own folder: the tests run in parallel, and a shared one was removed under another test
fn handbook(tag: &str) -> PathBuf {
    let f = tmp(&format!("{tag}-src")).join("handbook.pdf");
    std::fs::write(&f, handbook_pdf()).unwrap();
    f
}

#[test]
fn a_large_pdf_gets_an_overview_inline_and_docs_pdf_still_reads_the_copy() {
    let r = run("large", "summarise this", &[handbook("large")], vec![reply("It is a staff handbook.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.reqs.len(), 1, "one model call");
    let t = tools(&r.reqs[0]);
    assert!(!t.iter().any(|n| n.starts_with("web_")), "no web tools: {t:?}");
    let u = last_user(&r.reqs[0]);
    assert!(u.contains("[Attached: handbook.pdf, 40 pages; overview below, ask about any part for detail]"), "{u}");
    assert!(u.contains("The full file is saved at .elisa/uploads/") && u.contains("summarise this document"), "{u}");
    assert!(u.contains("Page 1 of the staff"), "{u}");
    // the docs server (project root only) reads the upload path given in the block
    let up = uploads(&r.project);
    let rel = up[0].strip_prefix(&r.project).unwrap().display().to_string();
    let cfg = elisa::mcp_docs::DocsCfg { root: elisa::outside::root(&r.project), max_chars: 6000 };
    let (out, err) = elisa::mcp_docs::call("docs_pdf", &json!({"path": rel, "pages": "40"}), &cfg);
    assert!(!err && out.contains("rule number 40"), "{out}");
}

#[test]
fn a_specific_question_gets_the_page_that_answers_it() {
    let r = run("pick", "who issues parking permits?", &[handbook("pick")], vec![reply("The facilities desk.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let u = last_user(&r.reqs[0]);
    assert!(u.contains("handbook.pdf, 40 pages; page") && u.contains("picked for this question"), "{u}");
    assert!(u.contains("Parking permits are issued") && !u.contains("rule number 2 applies"), "{u}");
}

#[test]
fn a_refusal_is_asked_once_more_with_the_text() {
    let r = run("refuse", "read this one for me", &[handbook("refuse")],
        vec![reply("I'm sorry, but I can't process the PDF file you've attached."), reply("It is a staff handbook with 40 rules.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.reqs.len(), 2, "asked once more");
    let u = last_user(&r.reqs[1]);
    assert!(u.contains("answer from it directly") && u.contains("overview below"), "{u}");
    let msgs = r.reqs[1]["messages"].as_array().unwrap();
    assert!(!msgs.iter().any(|m| m["content"].to_string().contains("can't process")), "the refusal is dropped");
}

#[test]
fn docx_and_csv_are_read() {
    let outside = tmp("docx-src");
    let d = outside.join("memo.docx");
    std::fs::write(&d, docx(&["Team memo", "The offsite moves to Friday &amp; starts at 9"])).unwrap();
    let c = outside.join("sales.csv");
    std::fs::write(&c, "region,amount\nnorth,120\nsouth,80\n").unwrap();
    let r = run("docx", "what do these say?", &[d, c], vec![reply("The offsite is on Friday; sales total 200.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.reqs.len(), 1);
    let u = last_user(&r.reqs[0]);
    assert!(u.contains("[Attached: memo.docx, 2 lines]") && u.contains("moves to Friday & starts at 9"), "{u}");
    assert!(u.contains("[Attached: sales.csv, 3 rows]") && u.contains("south,80"), "{u}");
}

#[test]
fn a_file_over_the_cap_is_refused_and_the_turn_goes_on() {
    let outside = tmp("cap-src");
    let f = outside.join("huge.pdf");
    std::fs::File::create(&f).unwrap().set_len(elisa::attach::MAX_BYTES + 1).unwrap();
    let r = run("cap", "summarise this", &[f], vec![reply("I can't see an attached document.")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stderr.contains("huge.pdf is too large to attach") && r.stderr.contains("50 MB"), "{}", r.stderr);
    let u = last_user(&r.reqs[0]);
    assert!(!u.contains("[Attached:"));
    // the model is told in the turn's own message, so it never answers from another file
    assert!(u.contains("Elisa could not read it: huge.pdf is too large to attach") && u.contains("never guess what it says"), "{u}");
    assert!(!r.project.join(".elisa/uploads").exists() || uploads(&r.project).is_empty(), "nothing copied");
}

#[test]
fn a_binary_file_is_refused_with_a_reason() {
    let outside = tmp("bin-src");
    let f = outside.join("slides.pptx");
    std::fs::write(&f, [0u8, 1, 2, 3, 0, 9]).unwrap();
    let r = run("bin", "summarise this", &[f], vec![reply("ok")]);
    assert!(r.stderr.contains("not attached: slides.pptx is not a document Elisa can read"), "{}", r.stderr);
}

/// a saved session in `project` whose first turn attached handbook.pdf (`block`: what that turn kept), with the upload
/// copy when `upload`
fn saved_session(project: &Path, block: &str, upload: bool) {
    if upload {
        let d = project.join(".elisa/uploads/1700000000");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("handbook.pdf"), handbook_pdf()).unwrap();
    }
    let s = project.join(".elisa/sessions");
    std::fs::create_dir_all(&s).unwrap();
    let h = json!([
        {"role": "user", "content": [{"type": "text", "text": "summarise this"}, {"type": "text", "text": block}]},
        {"role": "assistant", "content": [{"type": "text", "text": "It is a staff handbook with forty rules."}]}
    ]);
    std::fs::write(s.join("1700000000.json"), h.to_string()).unwrap();
}

const REF: &str = "[Attached: handbook.pdf, 40 pages · .elisa/uploads/1700000000/handbook.pdf]";

#[test]
fn a_continued_session_attaches_its_document_again() {
    let r = run_with("resume", "who issues parking permits?", &[], vec![reply("The facilities desk.")], &["--continue"], |p| saved_session(p, REF, true));
    assert_eq!(r.code, 0, "{}", r.stderr);
    let u = last_user(&r.reqs[0]);
    assert!(u.contains("picked for this question") && u.contains("Parking permits are issued"), "{u}");
    assert!(r.reqs[0].to_string().contains(&REF.replace('"', "\\\"")), "the saved turn keeps its reference");
}

#[test]
fn a_continued_session_whose_upload_is_gone_says_so() {
    let r = run_with("resume-gone", "who issues parking permits?", &[], vec![reply("I don't know.")], &["--continue"], |p| saved_session(p, REF, false));
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stderr.contains("handbook.pdf is no longer in this folder's uploads; attach it again"), "{}", r.stderr);
    assert!(!last_user(&r.reqs[0]).contains("picked for this question"));
}

#[test]
fn an_older_session_with_inline_text_gets_its_reference_and_the_text_is_dropped() {
    let old = "[Attached: handbook.pdf, 40 pages; overview below, ask about any part for detail]\n<untrusted source=\"attached handbook.pdf\">\nOLD-INLINE-OVERVIEW-TEXT\n</untrusted>\n\
        (The full file is saved at .elisa/uploads/1700000000/handbook.pdf; docs_pdf reads any other page.)";
    let r = run_with("resume-old", "who issues parking permits?", &[], vec![reply("The facilities desk.")], &["--continue"], |p| saved_session(p, old, true));
    assert_eq!(r.code, 0, "{}", r.stderr);
    let all = r.reqs[0].to_string();
    assert!(!all.contains("OLD-INLINE-OVERVIEW-TEXT"), "stale inline text is not sent again");
    assert!(all.contains("handbook.pdf, 40 pages · .elisa/uploads/1700000000/handbook.pdf"), "{all}");
    assert!(last_user(&r.reqs[0]).contains("Parking permits are issued"));
}

/// two sessions saved with no reference line (folded before references were kept), each with its own upload:
/// `newest` is the one `--continue` loads
fn two_sessions(project: &Path, newest: u64) {
    let garden: Vec<String> = (1..=40).map(|i| format!("Garden note {i}: roses and tulips need water in the morning, {}{}", "and the soil is checked every week by the gardening club volunteers. ".repeat(30),
        if i == 17 { " Parking for garden visitors is on the north lawn." } else { "" })).collect();
    for (ts, name, bytes) in [(1700000000u64, "handbook.pdf", handbook_pdf()), (1700000500, "garden.pdf", pdf(&garden))] {
        let d = project.join(format!(".elisa/uploads/{ts}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(name), bytes).unwrap();
        let s = project.join(".elisa/sessions");
        std::fs::create_dir_all(&s).unwrap();
        let h = json!([
            {"role": "user", "content": [{"type": "text", "text": "summarise this for me please"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "It covers several topics."}]}
        ]);
        // the file's name is its id; the newest id is the one `--continue` loads
        let id = if ts == newest { 1800000000 } else { 1600000000 + ts % 1000 };
        std::fs::write(s.join(format!("{id}.json")), h.to_string()).unwrap();
        // the upload sits under the id of the session that made it
        std::fs::rename(d, project.join(format!(".elisa/uploads/{id}"))).unwrap();
    }
}

#[test]
fn a_resumed_session_gets_only_its_own_upload() {
    for (newest, want, other) in [(1700000000u64, "handbook.pdf", "garden.pdf"), (1700000500, "garden.pdf", "handbook.pdf")] {
        let r = run_with(&format!("own-{newest}"), "where is parking?", &[], vec![reply("ok")], &["--continue"], |p| two_sessions(p, newest));
        assert_eq!(r.code, 0, "{}", r.stderr);
        let u = last_user(&r.reqs[0]);
        assert!(u.contains(&format!("[Attached: {want}, 40 pages; page")), "{want}: {u}");
        assert!(!u.contains(&format!("[Attached: {other}")), "only its own: {u}");
    }
}

#[test]
fn a_resumed_session_naming_no_document_gets_none_and_no_note() {
    let r = run_with("own-none", "where is parking?", &[], vec![reply("ok")], &["--continue"], |p| {
        two_sessions(p, 1700000000);
        // the newest session now has no upload of its own and names neither file
        let s = p.join(".elisa/sessions");
        let h = json!([
            {"role": "user", "content": [{"type": "text", "text": "plan a trip to Da Nang for me"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "Day 1: the beach."}]}
        ]);
        std::fs::write(s.join("1900000000.json"), h.to_string()).unwrap();
    });
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(!last_user(&r.reqs[0]).contains("[Attached:"), "{}", last_user(&r.reqs[0]));
    assert!(!r.stderr.contains("attach it again"), "{}", r.stderr);
}
