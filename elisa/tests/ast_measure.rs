//! Token savings of outline + read_symbol vs reading the whole file, on real files (run on demand):
//! `cargo test --offline --test ast_measure -- --ignored --nocapture`
//! Task per file: outline it, then read_symbol one function (the median-sized one, and the largest). Tokens are
//! Elisa's estimate (`context::estimate_tokens`), outputs exactly as the tools return them (6000-char cap). Also counts
//! files whose current (valid) text the parser reports errors in: a guard false positive would hit those on a write.
use elisa::ast::{self, Lang};
use elisa::context::estimate_tokens;
use elisa::tools::{self, ToolCtx};
use serde_json::json;
use std::path::{Path, PathBuf};

fn files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() { if !n.starts_with('.') && !["target", "target-dev", "node_modules", ".build"].contains(&n) { files(&p, out); } }
        else if Lang::of(&p).is_some() { out.push(p); }
    }
}

#[test]
#[ignore]
fn measure() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let demo = elisa::demo_gateway::project_in(&std::env::temp_dir().join(format!("elisa-measure-{}", std::process::id()))).unwrap();
    let sets: Vec<(String, PathBuf, Vec<PathBuf>)> = [("demo", demo.clone(), vec![demo.join("crates")]),
        ("elisa", repo.clone(), vec![repo.join("elisa/src"), repo.join("elisa/mem/src")]),
        ("engine", repo.clone(), vec![repo.join("engine/crates")]),
        ("app (swift)", repo.clone(), vec![repo.join("app")])]
        .into_iter().map(|(n, root, dirs)| { let mut v = vec![]; for d in &dirs { files(d, &mut v); } (n.to_string(), root, v) }).collect();
    for (name, root, list) in sets {
        let ctx = ToolCtx { cwd: root.clone(), ..ToolCtx::default() };
        let (mut whole, mut med, mut big, mut n, mut bad) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut rows = vec![];
        for f in &list {
            let rel = f.strip_prefix(&root).unwrap().to_string_lossy().to_string();
            let Ok(src) = std::fs::read_to_string(f) else { continue };
            let lang = Lang::of(f).unwrap();
            let Some(tree) = ast::parse(lang, &src) else { continue };
            if !ast::errors(&src, &tree).is_empty() { bad += 1; }
            let o = ast::outline_tree(lang, &src, &tree);
            let mut fns: Vec<&ast::Sym> = o.syms.iter().filter(|y| !y.container).collect();
            if fns.is_empty() { continue; }
            fns.sort_by_key(|y| y.end - y.start);
            let pick = |y: &ast::Sym| {
                let line = if o.syms.iter().filter(|z| z.path == y.path).count() > 1 { json!(y.line_a) } else { json!(null) };
                let r = tools::run("read_symbol", &json!({"path": rel, "name": y.path, "line": line}), &ctx);
                assert!(!r.is_error, "{rel} {}: {}", y.path, r.content);
                estimate_tokens(&r.content)
            };
            let ol = tools::run("outline", &json!({"path": rel}), &ctx);
            assert!(!ol.is_error, "{}", ol.content);
            let ot = estimate_tokens(&ol.content);
            let w = estimate_tokens(&tools::whole_read_text(&ctx, &rel).unwrap());
            let (m, b) = (ot + pick(fns[fns.len() / 2]), ot + pick(fns[fns.len() - 1]));
            whole += w; med += m; big += b; n += 1;
            rows.push((rel, src.lines().count(), w, ot, m, b));
        }
        println!("\n== {name}: {n} files, parse errors in {bad} of them (valid code the grammar does not fully know)");
        println!("{:<58} {:>6} {:>7} {:>7} {:>8} {:>8}", "file", "lines", "whole", "outline", "+median", "+largest");
        rows.sort_by_key(|r| std::cmp::Reverse(r.2));
        for r in rows.iter().take(12) { println!("{:<58} {:>6} {:>7} {:>7} {:>8} {:>8}", r.0, r.1, r.2, r.3, r.4, r.5); }
        if n > 0 {
            println!("total: whole {whole} tok · outline+median symbol {med} ({:.1}x fewer) · outline+largest {big} ({:.1}x fewer)",
                whole as f64 / med as f64, whole as f64 / big as f64);
        }
    }
    let _ = std::fs::remove_dir_all(demo.parent().unwrap());
}
