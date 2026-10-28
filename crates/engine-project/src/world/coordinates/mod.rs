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

/// Origin applied to local transforms, including while a direct origin write awaits reconciliation.
pub fn render_origin(world: &World) -> [f64; 3] {
    service::render_origin(world)
}

#[cfg(test)]
mod indirect_origin_tests {
    use super::*;

    #[test]
    fn render_origin_follows_applied_frame_across_pending_write_and_rebase() {
        let mut world = World::new();
        world.insert_resource(WorldOrigin {
            offset: [999_999_995_904.0, 0.0, 0.0],
        });
        install(&mut world).unwrap();
        assert_eq!(render_origin(&world)[0], 999_999_995_904.0);
        capture_origin(&mut world);
        world.resource_mut::<WorldOrigin>().offset = [999_999_997_952.0, 0.0, 0.0];
        assert_eq!(
            render_origin(&world)[0],
            999_999_995_904.0,
            "unreconciled direct write must not move rendered probes alone"
        );
        pre_simulation(&mut world, 0.0).unwrap();
        assert_eq!(render_origin(&world)[0], 999_999_997_952.0);
        rebase_now(&mut world, [999_999_999_000.0, 0.0, 0.0]).unwrap();
        assert_eq!(render_origin(&world)[0], 999_999_999_000.0);
    }
}
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

/// Exact baseline when the coordinates service still owns its matching local value.
pub(crate) struct TranslationSnapshot {
    pub exact: Option<[f64; 3]>,
    pub residual: [f64; 3],
    pub local: [f32; 3],
    pub component: Option<AbsolutePosition>,
}
pub(crate) fn capture_translation(
    world: &World,
    entity: Entity,
    local: [f32; 3],
) -> Result<TranslationSnapshot> {
    let component = world.get::<AbsolutePosition>(entity).cloned();
    if let Some(absolute) = &component {
        absolute.check()?;
    }
    let exact = component
        .as_ref()
        .filter(|absolute| absolute.exact_for(local))
        .map(|absolute| absolute.position);
    let residual = component
        .as_ref()
        .map_or([0.; 3], |absolute| absolute.residual_for(local));
    Ok(TranslationSnapshot {
        exact,
        residual,
        local,
        component,
    })
}
/// Mutates an already-owned root Transform and existing exact-position state together.
/// All fallible checks precede writes; no component is inserted, removed, or reclaimed.
pub(crate) fn write_captured_translation(
    world: &mut World,
    entity: Entity,
    expected: &Option<AbsolutePosition>,
    local: [f32; 3],
    absolute: Option<[f64; 3]>,
    residual: [f64; 3],
) -> Result<Option<AbsolutePosition>> {
    if world.get::<crate::Parent>(entity).is_some()
        || world.get::<AbsolutePosition>(entity) != expected.as_ref()
        || expected.is_some() != absolute.is_some()
    {
        return Err(crate::Error::new(
            "sequence.invalid",
            "Sequence exact-position ownership changed",
        ));
    }
    if local.iter().any(|v| !v.is_finite())
        || absolute.iter().flatten().any(|v| !v.is_finite())
        || residual.iter().any(|v| !v.is_finite())
    {
        return Err(crate::Error::new(
            "sequence.invalid",
            "Nonfinite sequence translation",
        ));
    }
    if let Some(absolute) = absolute {
        AbsolutePosition::new(absolute).check()?;
    }
    let mut transform = world
        .get_mut::<crate::Transform>(entity)
        .ok_or_else(|| crate::Error::new("sequence.invalid", "Sequence transform removed"))?;
    transform.position = local;
    if let (Some(mut component), Some(absolute)) =
        (world.get_mut::<AbsolutePosition>(entity), absolute)
    {
        component.position = absolute;
        component.mark_applied(local, residual);
        return Ok(Some(component.clone()));
    }
    Ok(None)
}
