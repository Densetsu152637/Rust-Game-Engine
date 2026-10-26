//! Native API for game crates, scripts and systems (`NativeScript` callbacks get `&mut World`).
use super::WorldOrigin;
use super::component::{AbsolutePosition, MAX_ABSOLUTE};
use super::service::{self, ShiftCause, ShiftReport, WorldOriginStatus};
use crate::simulation::physics::{self, Motion, PhysicsBody2D, PhysicsBody3D};
use crate::{Error, Parent, Result, Transform};
use bevy_ecs::prelude::*;
use glam::Vec3;

/// Current `WorldOrigin.offset` in f64 metres.
pub fn origin(world: &World) -> [f64; 3] {
    service::current_origin(world)
}

/// Absolute f64 position of `entity`: its exact `engine.AbsolutePosition` while the entity has
/// not moved since that value was applied, otherwise `offset + world position`.
pub fn absolute_position(world: &World, entity: Entity) -> Result<[f64; 3]> {
    let origin = WorldOrigin {
        offset: service::current_origin(world),
    };
    if world.get::<Parent>(entity).is_none()
        && let (Some(absolute), Some(transform)) = (
            world.get::<AbsolutePosition>(entity),
            world.get::<Transform>(entity),
        )
        && absolute.exact_for(transform.position)
    {
        return Ok(absolute.position);
    }
    let local = crate::transforms::world_transform(world, entity)?.position();
    Ok(origin.absolute(local))
}

fn moving_body(world: &World, entity: Entity) -> bool {
    let moving = world
        .get::<PhysicsBody3D>(entity)
        .is_some_and(|b| b.motion != Motion::Fixed)
        || world
            .get::<PhysicsBody2D>(entity)
            .is_some_and(|b| b.motion != Motion::Fixed);
    moving && physics::body_state(world, entity).is_ok()
}

/// Places `entity` at the absolute f64 `position`. A dynamic or kinematic body is teleported
/// with its velocities kept (`set_body_pose`); anything else gets its `Transform` written. A
/// root entity also gets (or updates) `engine.AbsolutePosition`, so the f64 value survives
/// later rebases exactly.
pub fn set_absolute_position(world: &mut World, entity: Entity, position: [f64; 3]) -> Result<()> {
    let candidate = AbsolutePosition::new(position);
    candidate.check()?;
    if world.get::<Transform>(entity).is_none() {
        return Err(Error::new(
            "world.absolute_position.invalid",
            "entity requires Transform",
        ));
    }
    let origin = WorldOrigin {
        offset: service::applied_origin(world),
    };
    let local = origin.local(position);
    let root = world.get::<Parent>(entity).is_none();
    if moving_body(world, entity) {
        let rotation = physics::body_state(world, entity)?.rotation;
        physics::set_body_pose(world, entity, Vec3::from(local), rotation, None)?;
    } else if root {
        world
            .get_mut::<Transform>(entity)
            .expect("checked above")
            .position = local;
    } else {
        crate::transforms::set_world_position(world, entity, local)?;
    }
    if root {
        let applied = world.get::<Transform>(entity).map(|t| t.position);
        match world.get_mut::<AbsolutePosition>(entity) {
            Some(mut absolute) => {
                absolute.position = position;
                absolute.applied = applied;
                absolute.applied_absolute = applied.map(|_| position);
                absolute.residual = [0.; 3];
            }
            None => {
                world.entity_mut(entity).insert(candidate);
            }
        }
    }
    Ok(())
}

/// Moves the origin to `new_offset` now: every root `Transform` moves by the opposite delta,
/// bodies are teleported with their velocities, world transforms are refreshed and
/// `world.origin_shifted` is emitted (delivered with the next tick's events). `new_offset` is
/// used as given; snap it to a power-of-two grid to keep repeated shifts exact. Fails without
/// changing anything when the offset is out of range (|v| > 1e12 m) or ragdoll/deformable
/// physics exists (see `docs/world-coordinates.md#limits`).
pub fn rebase_now(world: &mut World, new_offset: [f64; 3]) -> Result<ShiftReport> {
    if new_offset
        .iter()
        .any(|v| !v.is_finite() || v.abs() > MAX_ABSOLUTE)
    {
        return Err(Error::new(
            "world.origin.invalid",
            "origin offsets must be finite with |x|, |y|, |z| <= 1e12 metres",
        ));
    }
    if !world.contains_resource::<WorldOriginStatus>() {
        service::install(world);
    }
    let from = service::applied_origin(world);
    service::shift(world, from, new_offset, ShiftCause::Api)
}

/// The coordinates telemetry as of the last tick or shift.
pub fn status(world: &World) -> Option<WorldOriginStatus> {
    world.get_resource::<WorldOriginStatus>().cloned()
}

/// Telemetry for the runtime snapshot: present once the world uses large-world coordinates
/// (an `engine.LargeWorld`, a nonzero offset or a shift).
pub fn snapshot(world: &World) -> Option<WorldOriginStatus> {
    let status = world.get_resource::<WorldOriginStatus>()?;
    let used = status.settings.is_some()
        || status.shifts > 0
        || service::current_origin(world) != [0.0; 3];
    used.then(|| WorldOriginStatus {
        offset: service::current_origin(world),
        ..status.clone()
    })
}
