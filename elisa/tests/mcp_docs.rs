//! The built-in documents MCP server (`elisa --mcp-serve docs`) as a session starts it: through the MCP host, against
//! the committed fixtures (tests/fixtures/docs: a two-sheet xlsx with a date column, an ods) and a PDF written here.
//! Paths outside `--root` are refused; output stays under `--max-chars`.
use elisa::mcp_host::{Host, ServerCfg};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicBool;

mod common;
use common::TempDir;

fn tmp(tag: &str) -> TempDir { TempDir::new(&format!("mcp-docs-{tag}")) }

/// a minimal PDF, one Helvetica text line per page
fn pdf(pages: &[&str]) -> Vec<u8> {
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

fn start(root: &Path, max_chars: usize) -> Host {
    let cfg = ServerCfg { name: "docs".into(), command: vec![env!("CARGO_BIN_EXE_elisa").into(), "--mcp-serve".into(), "docs".into(),
        "--root".into(), root.display().to_string(), "--max-chars".into(), max_chars.to_string()], ..ServerCfg::default() };
    let (host, notes) = Host::start(&[cfg], &|n| ["read", "write", "edit", "bash"].contains(&n));
    assert!(notes.is_empty(), "{notes:?}");
    host
}

fn call(h: &mut Host, name: &str, args: Value) -> (String, bool) {
    let o = h.call(name, &args, &AtomicBool::new(false), 6000);
    (o.content, o.is_error)
}

#[test]
fn docs_server_reads_pdf_xlsx_ods_inside_the_project_only() {
    let base = tmp("main");
    let proj = base.join("proj");
    std::fs::create_dir_all(proj.join("data")).unwrap();
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs");
    for f in ["sales.xlsx", "plan.ods"] { std::fs::copy(fx.join(f), proj.join("data").join(f)).unwrap(); }
    std::fs::write(proj.join("report.pdf"), pdf(&["Quarterly report", "Revenue grew"])).unwrap();
    std::fs::write(base.join("outside.pdf"), pdf(&["secret"])).unwrap();

    let mut h = start(&proj, 6000);
    // the tool names reach the model unchanged (they start with the server name)
    let names: Vec<String> = h.defs().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["docs_pdf", "docs_sheet"]);
    assert_eq!(h.server_of("docs_pdf"), Some("docs"));
    let defs = serde_json::to_string(&h.defs().iter().map(|d| json!({"name": d.name, "description": d.description, "input_schema": d.input_schema})).collect::<Vec<_>>()).unwrap();
    assert!(defs.len() < 800, "tool defs too big for a small context: {} chars", defs.len());

    let (t, err) = call(&mut h, "docs_pdf", json!({"path": "report.pdf", "pages": "2"}));
    assert!(!err && t.contains("[report.pdf: 2 pages · showing 2]") && t.contains("Revenue grew") && !t.contains("Quarterly"), "{t}");

    let (t, err) = call(&mut h, "docs_sheet", json!({"path": "data/sales.xlsx"}));
    assert!(!err, "{t}");
    assert!(t.contains("sheet \"Sales\" (sheets: Sales, Notes)"), "{t}");
    assert!(t.contains("1\tRegion\tUnits\tDate") && t.contains("2\tNorth\t42\t2023-03-15") && t.contains("3\tSouth\t7.5\t2023-03-16"), "{t}");
    let (t, _) = call(&mut h, "docs_sheet", json!({"path": "data/sales.xlsx", "sheet": "notes"}));
    assert!(t.contains("2\tremember the milk"), "{t}");
    let (t, _) = call(&mut h, "docs_sheet", json!({"path": "data/sales.xlsx", "range": "A3:B3"}));
    assert!(t.contains("3\tSouth\t7.5") && !t.contains("North") && !t.contains("2023"), "{t}");
    let (t, err) = call(&mut h, "docs_sheet", json!({"path": "data/plan.ods"}));
    assert!(!err && t.contains("2\tWrite\t3"), "{t}");
    // a made-up sheet name (small models always send "Sheet1"): a one-sheet workbook reads its sheet and says so
    let (t1, err) = call(&mut h, "docs_sheet", json!({"path": "data/plan.ods", "sheet": "Sheet1"}));
    assert!(!err && t1.starts_with("[no sheet \"Sheet1\"; read the only sheet, ") && t1.contains("2\tWrite\t3"), "{t1}");
    assert_eq!(t1.lines().skip(1).collect::<Vec<_>>(), t.lines().collect::<Vec<_>>());
    // … a CSV ignores it
    std::fs::write(proj.join("leads.csv"), "name,city\nAda,London\n").unwrap();
    let (t, err) = call(&mut h, "docs_sheet", json!({"path": "leads.csv", "sheet": "Sheet1"}));
    assert!(!err && t.contains("2\tAda\tLondon") && !t.contains("Sheet1"), "{t}");

    // errors: missing, unknown format, wrong tool, no such sheet, outside the project
    for (tool, path, want) in [
        ("docs_pdf", "nope.pdf", "no such file"),
        ("docs_sheet", "report.pdf", "use docs_pdf"),
        ("docs_pdf", "data/sales.xlsx", "not a PDF (use docs_sheet)"),
        ("docs_pdf", "../outside.pdf", "outside the project"),
        ("docs_pdf", base.join("outside.pdf").to_str().unwrap(), "outside the project"),
    ] {
        let (t, err) = call(&mut h, tool, json!({"path": path}));
        assert!(err && t.contains(want), "{tool} {path}: {t}");
    }
    let (t, err) = call(&mut h, "docs_sheet", json!({"path": "data/sales.xlsx", "sheet": "Gone"}));
    assert!(err && t.contains("sheets: Sales, Notes"), "{t}");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.join("outside.pdf"), proj.join("link.pdf")).unwrap();
        let (t, err) = call(&mut h, "docs_pdf", json!({"path": "link.pdf"}));
        assert!(err && t.contains("outside the project"), "{t}");
    }
}

#[test]
fn docs_server_output_stays_under_its_cap_with_a_next_hint() {
    let d = tmp("cap");
    let rows: String = (1..=2000).map(|i| format!("{i},item {i},{}\n", i * 3)).collect();
    std::fs::write(d.join("big.csv"), rows).unwrap();
    let pages: Vec<String> = (1..=30).map(|i| format!("page {i} {}", "lorem ipsum ".repeat(40))).collect();
    std::fs::write(d.join("big.pdf"), pdf(&pages.iter().map(String::as_str).collect::<Vec<_>>())).unwrap();
    let mut h = start(&d, 1500);
    let (t, err) = call(&mut h, "docs_sheet", json!({"path": "big.csv"}));
    assert!(!err && t.chars().count() <= 1500 && t.contains("next: range=\""), "{t}");
    let next = t.split("next: range=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let (t2, _) = call(&mut h, "docs_sheet", json!({"path": "big.csv", "range": next}));
    assert!(t2.contains(&format!("rows {}", next.split('-').next().unwrap())), "{t2}");
    let (t, err) = call(&mut h, "docs_pdf", json!({"path": "big.pdf"}));
    assert!(!err && t.chars().count() <= 1500 && t.contains("next: pages=\""), "{t}");
}

#[test]
fn docs_is_on_by_default_and_a_project_can_only_switch_it_off() {
    use elisa::config::Config;
    let d = tmp("config");
    let (home, proj) = (d.join("home"), d.join("proj"));
    std::fs::create_dir_all(home.join(".config/elisa")).unwrap();
    std::fs::create_dir_all(proj.join(".elisa")).unwrap();
    let load = || Config::load_from(&proj, Some(home.clone()), |_| None).unwrap();
    assert_eq!(load().docs, "on");
    // a project may switch it off
    std::fs::write(proj.join(".elisa/config.toml"), "docs = \"off\"\n").unwrap();
    let c = load();
    assert!(c.docs == "off" && c.notes.is_empty(), "{:?}", c.notes);
    // but not on, when the user switched it off
    std::fs::write(home.join(".config/elisa/config.toml"), "docs = \"off\"\n").unwrap();
    std::fs::write(proj.join(".elisa/config.toml"), "docs = \"on\"\n").unwrap();
    let c = load();
    assert_eq!(c.docs, "off");
    assert!(c.notes.iter().any(|n| n.contains("a project config can only switch the documents server off")), "{:?}", c.notes);
    // a bad value: off, one note
    std::fs::remove_file(proj.join(".elisa/config.toml")).unwrap();
    std::fs::write(home.join(".config/elisa/config.toml"), "docs = \"yes\"\n").unwrap();
    let c = load();
    assert!(c.docs == "off" && c.notes.iter().any(|n| n.contains("docs: off — docs = \"yes\" is not a setting")), "{:?}", c.notes);
    // the session's server: rooted at the project, capped at the tool output limit
    let s = elisa::mcp_host::builtin_docs(&proj, 4000);
    assert_eq!(s.name, "docs");
    assert_eq!(s.command[1..], ["--mcp-serve".to_string(), "docs".into(), "--root".into(), proj.display().to_string(), "--max-chars".into(), "4000".into()]);
}
