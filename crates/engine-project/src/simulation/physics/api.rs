//! Native Rust API for scripts and systems (`NativeScript` callbacks receive `&mut World`).
//!
//! Bodies exist after the simulation synced them (scene start, then every tick), so an
//! entity spawned during the current tick reports "no synced physics body" until the next
//! step. Body IDs in engine-physics are `Entity::to_bits()`.
use super::super::{invalid, mapped};
use super::{service, service_mut};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{World2D, World3D};
use glam::{Quat, Vec2, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Two,
    Three,
}
/// Simulated state of one body in world space. 2D bodies report their synced Z, rotation
/// about Z, and angular velocity as `(0, 0, w)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyState {
    pub dimension: Dimension,
    pub position: Vec3,
    pub rotation: Quat,
    pub linear_velocity: Vec3,
    pub angular_velocity: Vec3,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaycastHit {
    pub entity: Entity,
    pub distance: f32,
}
/// engine-physics body ID of an entity.
pub fn body_id(entity: Entity) -> u64 {
    entity.to_bits()
}
/// Entity owning an engine-physics body ID, if the bits are a valid entity.
pub fn body_entity(id: u64) -> Option<Entity> {
    Entity::try_from_bits(id)
}
/// Read-only engine-physics worlds for queries this module does not wrap yet.
pub fn physics_worlds(world: &World) -> Result<(&World2D, &World3D)> {
    let physics = service(world)?;
    Ok((&physics.worlds.two, &physics.worlds.three))
}
fn dimension_of(world: &World, entity: Entity) -> Result<Dimension> {
    let physics = service(world)?;
    if physics.bodies.two.contains_key(&entity) {
        Ok(Dimension::Two)
    } else if physics.bodies.three.contains_key(&entity) {
        Ok(Dimension::Three)
    } else {
        Err(invalid("Entity has no synced physics body"))
    }
}
pub fn body_state(world: &World, entity: Entity) -> Result<BodyState> {
    let physics = service(world)?;
    let id = body_id(entity);
    let missing = || invalid("Entity has no synced physics body");
    match dimension_of(world, entity)? {
        Dimension::Two => {
            let z = physics.bodies.two[&entity].pose.2.z;
            let pose = physics.worlds.two.pose(id).ok_or_else(missing)?;
            let velocity = physics.worlds.two.velocity(id).ok_or_else(missing)?;
            Ok(BodyState {
                dimension: Dimension::Two,
                position: Vec3::new(pose.position[0], pose.position[1], z),
                rotation: Quat::from_rotation_z(pose.rotation),
                linear_velocity: Vec2::from(velocity.linear).extend(0.),
                angular_velocity: Vec3::new(0., 0., velocity.angular),
            })
        }
        Dimension::Three => {
            let pose = physics.worlds.three.pose(id).ok_or_else(missing)?;
            let velocity = physics.worlds.three.velocity(id).ok_or_else(missing)?;
            Ok(BodyState {
                dimension: Dimension::Three,
                position: Vec3::from(pose.position),
                rotation: Quat::from_array(pose.rotation),
                linear_velocity: Vec3::from(velocity.linear),
                angular_velocity: Vec3::from(velocity.angular),
            })
        }
    }
}
/// Linear impulse in N*s on a dynamic body. 2D bodies require `impulse.z == 0`.
pub fn apply_impulse(world: &mut World, entity: Entity, impulse: Vec3) -> Result<()> {
    let dimension = dimension_of(world, entity)?;
    let mut physics = service_mut(world)?;
    let id = body_id(entity);
    let result = match dimension {
        Dimension::Two if impulse.z != 0. => return Err(invalid("2D impulses must have zero Z")),
        Dimension::Two => physics.worlds.two.apply_impulse(id, [impulse.x, impulse.y]),
        Dimension::Three => physics.worlds.three.apply_impulse(id, impulse.to_array()),
    };
    result.map_err(mapped)
}
/// Angular impulse in N*m*s on a dynamic body. 2D bodies accept only the Z component.
pub fn apply_angular_impulse(world: &mut World, entity: Entity, impulse: Vec3) -> Result<()> {
    let dimension = dimension_of(world, entity)?;
    let mut physics = service_mut(world)?;
    let id = body_id(entity);
    let result = match dimension {
        Dimension::Two if impulse.x != 0. || impulse.y != 0. => {
            return Err(invalid("2D angular impulses act about Z only"));
        }
        Dimension::Two => physics.worlds.two.apply_angular_impulse(id, impulse.z),
        Dimension::Three => physics
            .worlds
            .three
            .apply_angular_impulse(id, impulse.to_array()),
    };
    result.map_err(mapped)
}
fn hit(hit: Option<engine_physics::RayHit>) -> Option<RaycastHit> {
    let hit = hit?;
    Some(RaycastHit {
        entity: body_entity(hit.id)?,
        distance: hit.distance,
    })
}
/// Closest 2D body hit; `mask` selects collider layer bits (see [`layer_mask`]).
pub fn raycast_2d(
    world: &World,
    origin: Vec2,
    direction: Vec2,
    max_distance: f32,
    mask: u32,
) -> Result<Option<RaycastHit>> {
    let physics = service(world)?;
    let result =
        physics
            .worlds
            .two
            .raycast(origin.to_array(), direction.to_array(), max_distance, mask);
    Ok(hit(result.map_err(mapped)?))
}
/// Closest 3D body hit; `mask` selects collider layer bits (see [`layer_mask`]).
pub fn raycast_3d(
    world: &World,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    mask: u32,
) -> Result<Option<RaycastHit>> {
    let physics = service(world)?;
    let result =
        physics
            .worlds
            .three
            .raycast(origin.to_array(), direction.to_array(), max_distance, mask);
    Ok(hit(result.map_err(mapped)?))
}
/// Query mask of named `engine.PhysicsSettings` layers, as last synced.
pub fn layer_mask(world: &World, names: &[&str]) -> Result<u32> {
    service(world)?
        .settings
        .layers
        .query_mask(names)
        .map_err(mapped)
}
