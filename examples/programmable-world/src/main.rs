fn main() -> engine::core::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--headless"] {
        return programmable_world::run_headless();
    }
    if args != ["--window"] {
        engine::core::anyhow::bail!("Usage: programmable-world [--headless|--window]");
    }
    #[cfg(feature = "desktop")]
    {
        engine::desktop::run(
            programmable_world::load()?,
            engine::desktop::RunOptions::default(),
        )?;
        Ok(())
    }
    #[cfg(not(feature = "desktop"))]
    engine::core::anyhow::bail!("Window mode requires cargo run --features desktop -- --window")
}
