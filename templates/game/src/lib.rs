//! Game logic. `configure` is the single entry point: the executable, the tests and
//! the engine's native compiler all call it.
use engine::prelude::*;

/// Moves the authored player along +X while the `move_right` action is held.
pub struct Player;

impl NativeScript for Player {
    fn on_update(
        &mut self,
        world: &mut World,
        owner: Entity,
        dt: f32,
    ) -> engine::project::Result<()> {
        let pressed = world.resource::<ActionFrame>().pressed("move_right");
        if pressed && let Some(mut transform) = world.get_mut::<Transform>(owner) {
            transform.position[0] += dt;
        }
        Ok(())
    }
}

/// Registers scripts, components, systems and event handlers.
pub fn configure(app: &mut EngineApp) -> Result<()> {
    register_script(app, "asset:player-script", |_, _| Ok(Box::new(Player)))?;
    Ok(())
}

/// Loads `project.json` next to this crate and applies `configure`.
pub fn load() -> Result<Runtime> {
    let project =
        Project::load(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("project.json"))?;
    let mut runtime = Runtime::new(project)?;
    configure(runtime.app_mut())?;
    Ok(runtime)
}
