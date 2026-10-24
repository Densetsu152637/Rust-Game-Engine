//! World-building feature module `coordinates`; see `crate::world` for the hook contract.
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

/// Absolute world-space position, in f64 metres, of the local simulation origin.
///
/// Local `Transform`/`WorldTransform` values are f32 and relative to this origin, so an
/// absolute position is `offset + local`. Installed into every runtime world.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldOrigin {
    pub offset: [f64; 3],
}
impl WorldOrigin {
    /// Absolute f64 position of a local f32 position.
    pub fn absolute(&self, local: [f32; 3]) -> [f64; 3] {
        std::array::from_fn(|i| self.offset[i] + f64::from(local[i]))
    }
}

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    Vec::new()
}
pub fn register_components(_app: &mut EngineApp) -> Result<()> {
    Ok(())
}
pub fn install(world: &mut World) -> Result<()> {
    world.init_resource::<WorldOrigin>();
    Ok(())
}
pub fn pre_simulation(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
pub fn post_transforms(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
