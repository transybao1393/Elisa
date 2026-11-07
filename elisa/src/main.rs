fn main() {
    // elisa-mem worker (embedding / compaction / query embedding): a short-lived process spawned by Elisa itself.
    // Checked before any other setup: no TUI, no config, no signal handlers.
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some(elisa_mem::WORKER_ARG) { std::process::exit(elisa_mem::worker_main(&a[1..])); }
    std::process::exit(elisa::repl::main_entry());
}
