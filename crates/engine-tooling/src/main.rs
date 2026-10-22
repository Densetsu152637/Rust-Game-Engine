fn main() {
    std::process::exit(engine_tooling::cli::main(
        std::env::args_os().skip(1).collect(),
    ));
}
