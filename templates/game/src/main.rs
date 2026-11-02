fn main() -> engine::Result<()> {
    let runtime = my_game::load()?;
    engine::desktop::run(runtime, engine::desktop::RunOptions::default())?;
    Ok(())
}
