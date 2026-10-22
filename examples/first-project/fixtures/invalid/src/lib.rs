// Intentionally invalid candidate: compile diagnostics must reject this crate
// without replacing an already compiled working runtime generation.
pub fn configure(_app: &mut engine_core::EngineApp) -> engine_core::Result<()> {
    let deliberately_invalid = ;
    Ok(())
}
