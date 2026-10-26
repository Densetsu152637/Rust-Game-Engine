//! Large-world coordinates: floating-origin rebasing with f64 absolute positions.
//!
//! [`WorldOrigin`] holds the absolute f64 position of the local simulation origin; every
//! local f32 `Transform`/`WorldTransform` is relative to it, so an absolute position is
//! `offset + local`. This module is the **single owner of shifting**: when the origin moves
//! (automatically through `engine.LargeWorld`, explicitly through [`rebase_now`], or because a
//! caller wrote [`WorldOrigin`] directly) it shifts every root `Transform` by the opposite
//! delta once, teleports simulated physics bodies through the public physics API, publishes
//! [`WorldOriginStatus`] and emits [`EVENT_ORIGIN_SHIFTED`]. Feature services never shift
//! entities themselves; services that cache world-space data that is not an entity
//! `Transform` (navigation paths, foliage instance fields) remember the offset their data is
//! expressed in and re-express it when [`WorldOrigin`] differs.
//!
//! See `docs/world-coordinates.md`.
mod api;
mod component;
mod service;

use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

pub use api::{absolute_position, origin, rebase_now, set_absolute_position, snapshot, status};
pub use component::{
    ABSOLUTE_POSITION_NAME, AbsolutePosition, LARGE_WORLD_NAME, LargeWorld, MAX_ABSOLUTE,
    SNAP_VALUES, THRESHOLD_RANGE, validate_scene,
};
pub use service::{
    EVENT_ORIGIN_SHIFTED, MAX_DIAGNOSTICS, ShiftCause, ShiftReport, WorldOriginStatus,
};

/// Absolute world-space position, in f64 metres, of the local simulation origin.
///
/// Local `Transform`/`WorldTransform` values are f32 and relative to this origin, so an
/// absolute position is `offset + local`. Installed into every runtime world. A value written
/// before the first tick is the initial origin; later changes should go through
/// [`rebase_now`] (a direct write is reconciled by the next tick, see the module docs).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldOrigin {
    pub offset: [f64; 3],
}
impl WorldOrigin {
    /// Absolute f64 position of a local f32 position.
    pub fn absolute(&self, local: [f32; 3]) -> [f64; 3] {
        std::array::from_fn(|i| self.offset[i] + f64::from(local[i]))
    }
    /// Local f32 position of an absolute f64 position (rounded once, after the subtraction).
    pub fn local(&self, absolute: [f64; 3]) -> [f32; 3] {
        std::array::from_fn(|i| (absolute[i] - self.offset[i]) as f32)
    }
}

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    component::descriptors()
}
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<AbsolutePosition>(app, ABSOLUTE_POSITION_NAME, 1)?;
    crate::register_component::<LargeWorld>(app, LARGE_WORLD_NAME, 1)
}
pub fn install(world: &mut World) -> Result<()> {
    world.init_resource::<WorldOrigin>();
    service::install(world);
    Ok(())
}
/// Reconciles direct origin writes, then rebases once when the anchor passed the threshold.
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    service::pre_simulation(world)
}
/// Keeps `engine.AbsolutePosition` values in step with entities that moved.
pub fn post_transforms(world: &mut World, _dt: f32) -> Result<()> {
    service::sync_absolute(world);
    Ok(())
}

/// Latch the frame of a newly acquired cinematic capture. Later direct origin writes
/// must shift that captured frame just like a rebase through the native coordinates API.
pub(crate) fn capture_origin(world: &mut World) -> [f64; 3] {
    service::applied_origin(world)
}
