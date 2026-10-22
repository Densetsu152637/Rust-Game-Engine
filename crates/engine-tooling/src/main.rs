use engine_log::LogConfig;

fn main() {
    // Logging goes to stderr only: stdout carries JSON-RPC (`engine mcp`) and
    // machine-readable command results. A logging failure must not stop the CLI.
    let _log = engine_log::init(LogConfig::new())
        .inspect_err(|error| eprintln!("warning: logging disabled: {error}"))
        .ok();
    std::process::exit(engine_tooling::cli::main(
        std::env::args_os().skip(1).collect(),
    ));
}
