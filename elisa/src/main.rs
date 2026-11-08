fn main() {
    // elisa-mem worker (embedding / compaction / query embedding): a short-lived process spawned by Elisa itself.
    // Checked before any other setup: no TUI, no config, no signal handlers.
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some(elisa_mem::WORKER_ARG) { std::process::exit(elisa_mem::worker_main(&a[1..])); }
    // Elisa's own MCP servers (`elisa --mcp-serve web`): started by a session like any other MCP server
    if a.first().map(String::as_str) == Some("--mcp-serve") {
        std::process::exit(match a.get(1).map(String::as_str) {
            Some("web") => elisa::mcp_web::serve(&a[2..]),
            Some("run") => elisa::mcp_run::serve(&a[2..]),
            Some("docs") => elisa::mcp_docs::serve(&a[2..]),
            Some("reason") => elisa::mcp_reason::serve(&a[2..]),
            Some("sec") => elisa::mcp_sec::serve(&a[2..]),
            Some(other) => { eprintln!("elisa --mcp-serve: unknown server \"{other}\" (known: web, run, docs, reason, sec)"); 2 }
            None => { eprintln!("elisa --mcp-serve: name the server to serve (known: web, run, docs, reason, sec)"); 2 }
        });
    }
    // a plugin's server (`elisa --plugin-run PLUGIN SERVER`): checks the plugin, then becomes its `docker run`
    if a.first().map(String::as_str) == Some(elisa::plugins::RUN_ARG) { std::process::exit(elisa::plugins::run_server(&a[1..])); }
    std::process::exit(elisa::repl::main_entry());
}
