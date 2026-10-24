//! World-building feature module `water`; see `crate::world` for the hook contract.
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    Vec::new()
}
pub fn register_components(_app: &mut EngineApp) -> Result<()> {
    Ok(())
}
pub fn install(_world: &mut World) -> Result<()> {
    Ok(())
}
pub fn pre_simulation(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
pub fn post_transforms(_world: &mut World, _dt: f32) -> Result<()> {
    Ok(())
}
