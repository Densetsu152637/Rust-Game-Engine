//! Native Rust implementation of the synthetic desktop lab, shared by visible
//! play and deterministic walkthroughs. Assets and authored IDs stay external.
pub mod bindings;
pub mod controller;
mod player;
pub mod progression;
#[path = "pure-tests.rs"]
mod pure_tests;
#[path = "test-new-female.rs"]
mod test_new_female;
#[path = "test-new-male.rs"]
mod test_new_male;
#[path = "test-restore-female.rs"]
mod test_restore_female;
#[path = "test-restore-male.rs"]
mod test_restore_male;

use engine_core::{EngineApp, Result};
use engine_project::register_script;
pub fn configure(app: &mut EngineApp) -> Result<()> {
    register_script(app, "asset:player", player::factory)?;
    register_script(app, "asset:test-new-male", test_new_male::factory)?;
    register_script(app, "asset:test-new-female", test_new_female::factory)?;
    register_script(app, "asset:test-restore-male", test_restore_male::factory)?;
    register_script(
        app,
        "asset:test-restore-female",
        test_restore_female::factory,
    )?;
    register_script(app, "asset:pure-tests", pure_tests::factory)?;
    Ok(())
}
