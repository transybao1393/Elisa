use elisa_mem::Memory;
use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some(elisa_mem::WORKER_ARG) { std::process::exit(elisa_mem::worker_main(&a[1..])) }
    let dir = |i: usize| a.get(i).cloned().unwrap_or_else(|| ".elisa/mem".into());
    let open = |d: &str| Memory::open(Path::new(d)).unwrap_or_else(|e| { eprintln!("open {d}: {e}"); std::process::exit(1) });
    match a.first().map(|s| s.as_str()) {
        Some("bench") => {
            let d = a.get(1).cloned().unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
            std::process::exit(elisa_mem::bench::run(Path::new(&d)))
        }
        Some("bench-child") if a.len() >= 3 => std::process::exit(elisa_mem::bench::child(&a[1], Path::new(&a[2]))),
        Some("compact") => { let mut m = open(&dir(1)); m.compact().expect("compact"); println!("{:?}", m.stats()) }
        Some("embed") if a.len() >= 2 => {
            let d = elisa_mem::embed::model_dir().expect("no model");
            let t: Vec<&str> = a[1..].iter().map(|s| s.as_str()).collect();
            let t0 = std::time::Instant::now();
            let v = elisa_mem::embed::embed(&d, &std::env::temp_dir().join("elisa-mem-vocab.bin"), &t, &mut |_| {}).expect("embed");
            eprintln!("{} texts in {:.1} ms", t.len(), t0.elapsed().as_secs_f64() * 1e3);
            for (i, e) in v.iter().enumerate() { println!("cos(q, #{i}) = {:.3}  {}", e.iter().zip(&v[0]).map(|(a, b)| a * b).sum::<f32>(), t[i]) }
        }
        Some("stats") => println!("{:?}", open(&dir(1)).stats()),
        Some("recall") if a.len() >= 3 => {
            let m = open(&a[1]);
            let graph = !a.iter().any(|x| x == "--no-graph");
            let q: Vec<&str> = a[2..].iter().map(|s| s.as_str()).filter(|s| *s != "--no-graph").collect();
            for h in m.recall(&q.join(" "), "", 5, graph) { println!("#{} {:?} s{} {:.2}\n  {}", h.id, h.kind, h.session, h.score, h.text.replace('\n', " ⏎ ")) }
        }
        _ => eprintln!("usage: elisa-mem bench [dir] | compact [dir] | stats [dir] | recall <dir> <query...> [--no-graph]"),
    }
}
