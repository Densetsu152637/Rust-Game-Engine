//! Procedural content generation in scenes: the `engine.PcgVolume` component.
//!
//! A volume names a `pcg` graph asset (see `engine-pcg`), a seed and a box. While the runtime
//! runs, a volume whose authored values changed since its last generation (including a bumped
//! `regenerate` counter) is regenerated: the graph executes against a [`PcgSurfaceRegistry`]
//! surface, the previous output is despawned and the new instances are spawned as ordinary
//! scene entities with deterministic ids `pcg:<volume id>:<n>`.
//!
//! The volume's position (its world translation plus `center`) places the box; rotation and
//! scale do not affect generation. Noise is sampled at [`WorldOrigin`] `offset + local`, so
//! rebasing the origin does not change the generated world.
//!
//! [`WorldOrigin`]: super::coordinates::WorldOrigin
mod component;
mod generate;
mod surfaces;

use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

pub use component::{COMPONENT_NAME, PcgMode, PcgVolume, load_graph, validate_scene};
pub use generate::{PcgReport, PcgState, PcgStatus, instance_entities};
pub use surfaces::{PcgNodes, PcgSurfaceRegistry};

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    vec![component::descriptor()]
}
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<PcgVolume>(app, COMPONENT_NAME, 1)
}
pub fn install(world: &mut World) -> Result<()> {
    world.init_resource::<PcgSurfaceRegistry>();
    world.init_resource::<PcgNodes>();
    world.init_resource::<PcgStatus>();
    world.init_resource::<generate::PcgOwned>();
    Ok(())
}
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    generate::run(world);
    Ok(())
}
pub fn post_transforms(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
