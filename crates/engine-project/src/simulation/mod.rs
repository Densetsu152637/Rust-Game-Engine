//! Authored simulation services. Native updates run first; services own their declared poses.
use crate::{Error, Result, Transform};
use bevy_ecs::prelude::*;
use glam::{Mat4, Quat, Vec3};
use std::collections::{BTreeMap, BTreeSet};
mod animation;
mod animation_graph;
pub use animation_graph::AnimationGraphPlayer;
mod crowds;
mod descriptors;
mod imported;
mod navigation;
mod navmesh;
pub mod physics;
mod skinning;
mod types;
pub use animation::{
    ClipDocument, ClipTrack, Event, RotationChannel, Sampling, VectorChannel, load_clip,
};
pub use descriptors::component_descriptors;
pub use imported::{imported_clip, imported_skeleton, skin_palette};
pub use navmesh::*;
pub use physics::{
    BodyMaterial, CombineRule, CompoundPart2D, CompoundPart3D, Motion, NamedMaterial,
    PhysicsBody2D, PhysicsBody3D, PhysicsSettings, Shape,
};
pub use types::*;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("simulation.invalid", message)
}
fn mapped(error: impl std::fmt::Display) -> Error {
    invalid(error.to_string())
}
pub fn register_components(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<SkinBinding>(app, "engine.SkinBinding", 1)?;
    physics::register_components(app)?;
    crate::register_component::<AnimationPlayer>(app, "engine.AnimationPlayer", 1)?;
    crate::register_component::<AnimationGraphPlayer>(app, "engine.AnimationGraphPlayer", 1)?;
    crate::register_component::<NavigationAgent>(app, "engine.NavigationAgent", 1)?;
    crate::register_component::<CrowdAgent>(app, "engine.CrowdAgent", 1)?;
    crate::register_component::<Formation>(app, "engine.Formation", 1)?;
    crate::register_component::<NavigationObstacle>(app, "engine.NavigationObstacle", 1)?;
    crate::register_component::<OffMeshLink>(app, "engine.OffMeshLink", 1)
}
#[derive(Resource)]
struct Services {
    physics: physics::Physics,
    animations: animation::Animations,
    graph_animations: animation_graph::GraphAnimations,
    crowds: crowds::Crowds,
    navigation: navigation::Navigation,
    skins: skinning::Skins,
}
#[derive(Resource, Clone)]
struct Settings(Option<SimulationConfig>);
pub fn install(world: &mut World, config: Option<SimulationConfig>) -> Result<()> {
    let services = if let Some(config) = config.clone() {
        Some(Services {
            physics: physics::Physics::new(config)?,
            animations: Default::default(),
            graph_animations: Default::default(),
            crowds: Default::default(),
            navigation: Default::default(),
            skins: Default::default(),
        })
    } else {
        None
    };
    if let Some(previous) = world.remove_resource::<Services>() {
        previous.skins.clear(world);
    }
    if let Some(services) = services {
        world.insert_resource(services);
    }
    world.insert_resource(Settings(config));
    Ok(())
}
/// Rebuild scene-local state, including retained entities whose spawn pose changed.
/// Hosts call this after scene hydration and before new scene startup callbacks.
pub fn reset(world: &mut World) -> Result<()> {
    let config = world.get_resource::<Settings>().and_then(|s| s.0.clone());
    install(world, config)?;
    prepare(world)
}
fn validate(world: &World) -> Result<()> {
    let enabled = world.contains_resource::<Services>();
    let mut animated = BTreeSet::new();
    for e in world.iter_entities() {
        let body2 = e.get::<PhysicsBody2D>();
        let body3 = e.get::<PhysicsBody3D>();
        let nav = e.get::<NavigationAgent>();
        let animation = e.get::<AnimationPlayer>();
        let graph = e.get::<AnimationGraphPlayer>();
        let crowd = e.contains::<CrowdAgent>();
        if crowd
            || e.contains::<NavigationObstacle>()
            || e.contains::<OffMeshLink>()
            || e.contains::<Formation>()
        {
            if !enabled {
                return Err(invalid(
                    "Simulation services disabled for an active component",
                ));
            }
            if !e.contains::<Transform>() {
                return Err(invalid("Simulation components require Transform"));
            }
        }
        if crowd
            && (nav.is_some()
                || e.contains::<physics::CharacterController2D>()
                || e.contains::<physics::CharacterController3D>())
        {
            return Err(invalid(
                "A crowd agent cannot share an entity with a navigation agent or character controller",
            ));
        }
        if crowd
            && body2
                .map(|b| b.motion)
                .or(body3.map(|b| b.motion))
                .is_some_and(|m| m != Motion::Kinematic)
        {
            return Err(invalid(
                "Crowd agents can drive only kinematic physics bodies",
            ));
        }
        if body2.is_some()
            || body3.is_some()
            || nav.is_some()
            || animation.is_some()
            || graph.is_some()
            || e.contains::<SkinBinding>()
        {
            if !enabled {
                return Err(invalid(
                    "Simulation services disabled for an active component",
                ));
            }
            if !e.contains::<Transform>() {
                return Err(invalid("Simulation components require Transform"));
            }
        }
        if body2.is_some() && body3.is_some()
            || (body2.is_some() || body3.is_some()) && e.contains::<crate::Collider2D>()
        {
            return Err(invalid(
                "One physics body per entity; legacy Collider2D cannot share it",
            ));
        }
        if nav.is_some()
            && body2
                .map(|b| b.motion)
                .or(body3.map(|b| b.motion))
                .is_some_and(|m| m != Motion::Kinematic)
        {
            return Err(invalid(
                "Navigation can drive only kinematic physics bodies",
            ));
        }
        let mut animation_targets = Vec::new();
        if let Some(player) = animation {
            animation_targets.extend(animation::bindings(world, e.id(), player)?);
        }
        if let Some(player) = graph {
            animation_targets.extend(animation_graph::bindings(world, e.id(), player)?);
        }
        for target in animation_targets {
            if !animated.insert(target) {
                return Err(invalid("Multiple animations write the same entity"));
            }
            if animated.len() > 65536 {
                return Err(invalid("Animation world binding budget exceeded"));
            }
            if world.get::<PhysicsBody2D>(target).is_some()
                || world.get::<PhysicsBody3D>(target).is_some()
                || world.get::<NavigationAgent>(target).is_some()
                || world.get::<CrowdAgent>(target).is_some()
            {
                return Err(invalid(
                    "Animation target already has a physics or navigation pose authority",
                ));
            }
        }
    }
    physics::validate(world)
}
pub fn prepare(world: &mut World) -> Result<()> {
    validate(world)?;
    let Some(mut services) = world.remove_resource::<Services>() else {
        return Ok(());
    };
    let result = (|| {
        services.animations.sync(world)?;
        services.graph_animations.sync(world)?;
        services.navigation.sync(world)?;
        services.crowds.sync(world)?;
        services.physics.sync(world)?;
        services.skins.sync(world)?;
        services.skins.evaluate(world)
    })();
    world.insert_resource(services);
    result
}
pub fn step(world: &mut World, dt: f32) -> Result<()> {
    validate(world)?;
    let Some(mut services) = world.remove_resource::<Services>() else {
        return Ok(());
    };
    let result = (|| {
        services.animations.sync(world)?;
        services.graph_animations.sync(world)?;
        services.navigation.sync(world)?;
        services.crowds.sync(world)?;
        services.animations.step(world, dt)?;
        services.graph_animations.step(world, dt)?;
        services.navigation.step(world, dt)?;
        services.crowds.step(world, dt)?;
        services.physics.sync(world)?;
        services.physics.step(world, dt)?;
        services.skins.sync(world)?;
        services.skins.evaluate(world)
    })();
    world.insert_resource(services);
    result
}
fn emit(
    world: &mut World,
    target: Entity,
    source: Option<Entity>,
    name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    crate::emit_event(
        world,
        crate::EngineEvent {
            name: name.into(),
            source,
            target: Some(target),
            payload,
        },
    )
}
fn world_pose(world: &World, entity: Entity) -> Result<(Vec3, Quat, Vec3)> {
    decompose(Mat4::from_cols_array_2d(
        &crate::world_transform(world, entity)?.matrix,
    ))
}
fn decompose(matrix: Mat4) -> Result<(Vec3, Quat, Vec3)> {
    let (scale, rotation, position) = matrix.to_scale_rotation_translation();
    if !scale.is_finite()
        || scale.min_element() <= 1e-7
        || !rotation.is_finite()
        || !position.is_finite()
        || !Mat4::from_scale_rotation_translation(scale, rotation, position)
            .abs_diff_eq(matrix, 0.0001)
    {
        return Err(invalid(
            "Simulation requires positive nonsingular TRS without shear",
        ));
    }
    Ok((scale, rotation.normalize(), position))
}
fn write_world_pose(
    world: &mut World,
    entity: Entity,
    scale: Vec3,
    rotation: Quat,
    position: Vec3,
) -> Result<()> {
    let mut matrix = Mat4::from_scale_rotation_translation(scale, rotation, position);
    if let Some(parent) = world.get::<crate::Parent>(entity) {
        let parent = crate::services::find_entity(world, &parent.0)
            .ok_or_else(|| invalid("Missing parent"))?;
        matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, parent)?.matrix).inverse()
            * matrix;
    }
    let (scale, rotation, position) = decompose(matrix)?;
    let mut transform = world
        .get_mut::<Transform>(entity)
        .ok_or_else(|| invalid("Missing Transform"))?;
    transform.position = position.to_array();
    transform.rotation = rotation.to_array();
    transform.scale = scale.to_array();
    Ok(())
}
/// Prefab entity references include every node binding, not only skeleton joints.
pub fn remap_animation_bindings(value: &mut serde_json::Value, ids: &BTreeMap<String, String>) {
    if let Some(bindings) = value
        .get_mut("bindings")
        .and_then(serde_json::Value::as_array_mut)
    {
        for binding in bindings {
            if let Some(mapped) = binding.as_str().and_then(|id| ids.get(id)) {
                *binding = serde_json::Value::String(mapped.clone());
            }
        }
    }
}
