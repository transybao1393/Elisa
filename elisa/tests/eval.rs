//! `elisa eval`'s deterministic cases (evals/*.toml, kind "fake") as a test: every scripted case runs the real binary
//! over `--serve-jsonl` against the built-in fake model, each in its own temp project and temp HOME, and must pass.
use elisa::eval::{load_cases, report, run_cases, select, table, Options};
use std::path::PathBuf;

#[test]
fn every_deterministic_eval_case_passes() {
    let all = load_cases(None).expect("the built-in cases parse");
    let cases = select(&all, "fake", None).unwrap();
    assert!(cases.len() >= 20, "{} scripted cases", cases.len());
    let opts = Options { suite: "fake".into(), exe: PathBuf::from(env!("CARGO_BIN_EXE_elisa")), jobs: 4, ..Default::default() };
    let results = run_cases(&cases, &opts);
    assert_eq!(results.len(), cases.len());
    let failed: Vec<_> = results.iter().filter(|r| r.status == "fail").collect();
    assert!(failed.is_empty(), "eval cases failed:\n{}", table(&results));
    // the JSON report the scorecard reads: schema, per-case results, per-model aggregates
    let rep = report(&results, &opts, 0, None);
    assert_eq!(rep["schema"], "elisa-eval/1");
    assert_eq!(rep["cases"].as_array().unwrap().len(), cases.len());
    let agg = rep["aggregates"].as_array().unwrap();
    assert_eq!(agg.len(), 1, "one model: the fake");
    assert_eq!((agg[0]["model"].as_str(), agg[0]["kind"].as_str(), agg[0]["pass_rate"].as_f64()), (Some("eval-fake"), Some("fake"), Some(1.0)));
    assert!(agg[0]["loop_guard_stops"].as_u64().unwrap() >= 2, "{}", agg[0]);
}

/// The cases live in the repo next to the code; the binary embeds the same files.
#[test]
fn the_case_files_on_disk_are_the_embedded_ones() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("evals");
    let disk = load_cases(Some(&dir)).unwrap();
    let embedded = load_cases(None).unwrap();
    let ids = |v: &[elisa::eval::Case]| { let mut i: Vec<String> = v.iter().map(|c| c.id.clone()).collect(); i.sort(); i };
    assert_eq!(ids(&disk), ids(&embedded));
}
