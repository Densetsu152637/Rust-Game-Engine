//! Native ragdoll API and the hub shared between it and the physics feature.
//!
//! The feature owns its rigs; the API talks to it through a [`Hub`] world resource: commands
//! flow in (applied at the next simulation sync) and read-outs flow out (published after every
//! step). Authored controls (mode, blend, strength) are edited on the component itself.
use super::component::{BoneJoint, BoneShape, Ragdoll, RagdollBone, RagdollMode};
use super::{MAX_BONES, joint_id};
use crate::Result;
use crate::simulation::invalid;
use crate::simulation::physics::service;
use bevy_ecs::prelude::*;
use glam::{Quat, Vec3};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

/// Simulated state of one bone in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneReadout {
    pub entity: Entity,
    /// World position of the bone (its joint).
    pub position: Vec3,
    /// World rotation of the bone entity.
    pub rotation: Quat,
    pub linear_velocity: Vec3,
    pub angular_velocity: Vec3,
    /// `false` for kinematic bones (animated mode, pinned root).
    pub dynamic: bool,
    /// Angle in radians between the physics and the animation local rotation (0 for a root).
    pub pose_error: f32,
}
/// What a ragdoll is doing, from [`ragdoll_status`].
#[derive(Clone, Debug, PartialEq)]
pub struct RagdollStatus {
    /// `false` when the ragdoll has no bodies, with the reason in `inactive_reason`.
    pub active: bool,
    pub inactive_reason: Option<String>,
    /// The mode being simulated (differs from the authored mode during a blend-out).
    pub mode: RagdollMode,
    /// Weight of the physics pose written last step (authored blend times the fade).
    pub blend: f32,
    pub fading: bool,
    /// All dynamic bones stayed below `settle_speed` for `settle_seconds`.
    pub settled: bool,
    /// Mean local-rotation error against the animation pose over bones with a parent.
    pub pose_error: f32,
    /// Motor strength multiplier of a running hit reaction (1 when none).
    pub hit_scale: f32,
    pub bones: Vec<BoneReadout>,
}
impl Default for RagdollStatus {
    fn default() -> Self {
        Self {
            active: false,
            inactive_reason: Some("not simulated yet".into()),
            mode: RagdollMode::Animated,
            blend: 0.,
            fading: false,
            settled: false,
            pose_error: 0.,
            hit_scale: 1.,
            bones: vec![],
        }
    }
}
/// Temporary strength drop after [`ragdoll_hit`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitReaction {
    /// Motor strength multiplier right after the hit, 0..=1.
    pub strength_scale: f32,
    /// Seconds over which the multiplier recovers linearly to 1.
    pub recover_seconds: f32,
}

pub(super) enum Command {
    Impulse {
        entity: Entity,
        bone: Entity,
        impulse: Vec3,
        point: Option<Vec3>,
        reaction: Option<HitReaction>,
    },
    BlendOut {
        entity: Entity,
        seconds: f32,
    },
    CancelFade {
        entity: Entity,
    },
}
#[derive(Default)]
pub(super) struct Inner {
    pub commands: Vec<Command>,
    pub status: BTreeMap<Entity, RagdollStatus>,
}
/// Command queue and read-outs, shared through the world.
#[derive(Resource, Default)]
pub(super) struct Hub(Mutex<Inner>);
impl Hub {
    pub(super) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
fn hub(world: &World) -> Result<&Hub> {
    world
        .get_resource::<Hub>()
        .ok_or_else(|| invalid("Ragdoll services are unavailable"))
}
fn component(world: &mut World, entity: Entity) -> Result<Mut<'_, Ragdoll>> {
    world
        .get_mut::<Ragdoll>(entity)
        .ok_or_else(|| invalid("Entity has no engine.Ragdoll"))
}
/// Validates and attaches a ragdoll component (the entity needs `Transform`).
pub fn add_ragdoll(world: &mut World, entity: Entity, ragdoll: Ragdoll) -> Result<()> {
    ragdoll.validate()?;
    if world.get::<crate::Transform>(entity).is_none() {
        return Err(invalid("engine.Ragdoll requires Transform"));
    }
    world.entity_mut(entity).insert(ragdoll);
    Ok(())
}
/// Switches the authored mode. Bodies change at the next sync; leaving `ragdoll`/`powered` for
/// `animated` fades out over `blend_out_seconds`.
pub fn ragdoll_set_mode(world: &mut World, entity: Entity, mode: RagdollMode) -> Result<()> {
    component(world, entity)?.mode = mode;
    if mode != RagdollMode::Animated {
        hub(world)?
            .lock()
            .commands
            .push(Command::CancelFade { entity });
    }
    Ok(())
}
/// Sets the weight (0..=1) of the physics pose written to the joint transforms.
pub fn ragdoll_set_blend(world: &mut World, entity: Entity, blend: f32) -> Result<()> {
    if !(0.0..=1.0).contains(&blend) {
        return Err(invalid("Ragdoll blend must be in 0..=1"));
    }
    component(world, entity)?.blend = blend;
    Ok(())
}
/// Sets the overall powered-mode strength multiplier (0..=10).
pub fn ragdoll_set_strength(world: &mut World, entity: Entity, strength: f32) -> Result<()> {
    if !(0.0..=10.0).contains(&strength) {
        return Err(invalid("Ragdoll strength must be in 0..=10"));
    }
    component(world, entity)?.strength = strength;
    Ok(())
}
/// Starts a get-up: the physics pose fades out over `seconds` while the simulation keeps
/// running, then the ragdoll returns to `animated` (the component mode is updated).
pub fn ragdoll_blend_out(world: &mut World, entity: Entity, seconds: f32) -> Result<()> {
    if !(seconds.is_finite() && seconds > 0. && seconds <= 60.) {
        return Err(invalid("Blend-out seconds must be in (0, 60]"));
    }
    component(world, entity)?;
    hub(world)?
        .lock()
        .commands
        .push(Command::BlendOut { entity, seconds });
    Ok(())
}
fn bone_checked(world: &World, entity: Entity, bone: Entity, impulse: Vec3) -> Result<()> {
    if !impulse.is_finite() {
        return Err(invalid("Ragdoll impulse must be finite"));
    }
    let status = ragdoll_status(world, entity)?;
    match status.bones.iter().find(|b| b.entity == bone) {
        None => Err(invalid("Entity is not a simulated bone of this ragdoll")),
        Some(b) if !b.dynamic => Err(invalid(
            "Ragdoll bone is kinematic; switch to ragdoll or powered mode first",
        )),
        Some(_) => Ok(()),
    }
}
/// Applies a world-space impulse (N*s) to a dynamic bone at the next sync, at `point` (world
/// space) or the bone's center of mass.
pub fn ragdoll_bone_impulse(
    world: &World,
    entity: Entity,
    bone: Entity,
    impulse: Vec3,
    point: Option<Vec3>,
) -> Result<()> {
    bone_checked(world, entity, bone, impulse)?;
    hub(world)?.lock().commands.push(Command::Impulse {
        entity,
        bone,
        impulse,
        point,
        reaction: None,
    });
    Ok(())
}
/// A hit reaction: [`ragdoll_bone_impulse`] plus a temporary drop of the motor strength that
/// recovers over `reaction.recover_seconds` (powered mode).
pub fn ragdoll_hit(
    world: &World,
    entity: Entity,
    bone: Entity,
    impulse: Vec3,
    point: Option<Vec3>,
    reaction: HitReaction,
) -> Result<()> {
    if !(0.0..=1.0).contains(&reaction.strength_scale)
        || !(reaction.recover_seconds.is_finite()
            && reaction.recover_seconds > 0.
            && reaction.recover_seconds <= 60.)
    {
        return Err(invalid(
            "Hit reaction needs strength_scale in 0..=1 and recover_seconds in (0, 60]",
        ));
    }
    bone_checked(world, entity, bone, impulse)?;
    hub(world)?.lock().commands.push(Command::Impulse {
        entity,
        bone,
        impulse,
        point,
        reaction: Some(reaction),
    });
    Ok(())
}
/// State of the ragdoll as of the last step.
pub fn ragdoll_status(world: &World, entity: Entity) -> Result<RagdollStatus> {
    if world.get::<Ragdoll>(entity).is_none() {
        return Err(invalid("Entity has no engine.Ragdoll"));
    }
    Ok(hub(world)?
        .lock()
        .status
        .get(&entity)
        .cloned()
        .unwrap_or_default())
}
/// World pose, velocity and pose error of one bone as of the last step.
pub fn ragdoll_bone_state(world: &World, entity: Entity, bone: Entity) -> Result<BoneReadout> {
    ragdoll_status(world, entity)?
        .bones
        .into_iter()
        .find(|b| b.entity == bone)
        .ok_or_else(|| invalid("Entity is not a simulated bone of this ragdoll"))
}
/// Relative pose and loads of the joint between `bone` and its parent bone.
pub fn ragdoll_joint_state(
    world: &World,
    entity: Entity,
    bone: Entity,
) -> Result<engine_physics::dim3::JointState> {
    ragdoll_bone_state(world, entity, bone)?;
    service(world)?
        .worlds
        .three
        .joint_state(joint_id(bone))
        .ok_or_else(|| invalid("Bone has no joint (it is a root bone or the ragdoll is animated)"))
}
fn has_any(world: &World, entity: Entity) -> bool {
    world
        .get::<crate::simulation::PhysicsBody2D>(entity)
        .is_some()
        || world
            .get::<crate::simulation::PhysicsBody3D>(entity)
            .is_some()
}
/// Bone-local direction of a bone: toward its farthest child bone, or the continuation of the
/// parent-to-bone direction for a leaf. `None` when the bone sits on its parent's joint.
fn local_direction(world: &World, bone: Entity, candidates: &[Entity]) -> Option<Vec3> {
    let position = |e: Entity| {
        world
            .get::<crate::Transform>(e)
            .map(|t| Vec3::from(t.position))
    };
    let child = candidates
        .iter()
        .filter(|c| {
            world
                .get::<crate::Parent>(**c)
                .and_then(|p| crate::services::find_entity(world, &p.0))
                == Some(bone)
        })
        .filter_map(|c| position(*c))
        .fold(None::<Vec3>, |best, p| match best {
            Some(b) if b.length() >= p.length() => Some(b),
            _ => Some(p),
        });
    if let Some(child) = child.filter(|c| c.length() > 1e-5) {
        return Some(child.normalize());
    }
    let own = position(bone)?;
    let rotation = Quat::from_array(world.get::<crate::Transform>(bone)?.rotation).normalize();
    (own.length() > 1e-5).then(|| rotation.inverse() * own.normalize())
}
/// Builds a default definition for the entities below `root` that carry a `Transform`: auto
/// capsules between joints, hinges (about an axis perpendicular to the bone) for knees and
/// elbows, cone-limited ball joints elsewhere. At most 64 bones; the entities need authored
/// ids and no physics bodies.
pub fn ragdoll_from_hierarchy(world: &World, root: Entity) -> Result<Ragdoll> {
    let mut found: Vec<(usize, String, Entity)> = vec![];
    for entity in world.iter_entities() {
        let (Some(id), true) = (
            entity.get::<crate::AuthoredId>(),
            entity.contains::<crate::Transform>(),
        ) else {
            continue;
        };
        let mut depth = 0;
        let mut current = entity.id();
        while let Some(parent) = world.get::<crate::Parent>(current) {
            depth += 1;
            let Some(next) = crate::services::find_entity(world, &parent.0) else {
                break;
            };
            current = next;
            if current == root || depth > 256 {
                break;
            }
        }
        if current == root && entity.id() != root {
            if has_any(world, entity.id()) {
                return Err(invalid(format!(
                    "Bone {} already has a physics body",
                    id.id
                )));
            }
            found.push((depth, id.id.clone(), entity.id()));
        }
    }
    found.sort();
    if found.is_empty() {
        return Err(invalid("The root entity has no descendants with Transform"));
    }
    if found.len() > MAX_BONES {
        return Err(invalid(format!(
            "More than {MAX_BONES} bones below the root; list the bones explicitly"
        )));
    }
    let entities: Vec<Entity> = found.iter().map(|(_, _, e)| *e).collect();
    let bones = found
        .iter()
        .map(|(_, id, entity)| {
            let axis = local_direction(world, *entity, &entities)
                .map(|d| {
                    let axis = d.cross(Vec3::Z);
                    if axis.length() > 1e-3 {
                        axis
                    } else {
                        d.cross(Vec3::X)
                    }
                })
                .map_or([1., 0., 0.], |a| a.normalize().to_array());
            default_bone(id, axis)
        })
        .collect();
    Ok(Ragdoll {
        bones,
        ..Ragdoll::default()
    })
}
fn default_bone(id: &str, axis: [f32; 3]) -> RagdollBone {
    let name = id.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    let base = RagdollBone {
        bone: id.into(),
        shape: BoneShape::Capsule,
        axis,
        ..RagdollBone::default()
    };
    if has(&[
        "knee",
        "elbow",
        "forearm",
        "lowerarm",
        "lower_arm",
        "shin",
        "calf",
        "lowerleg",
        "lower_leg",
    ]) {
        RagdollBone {
            joint: BoneJoint::Hinge,
            ..base
        }
    } else if has(&["neck", "head"]) {
        RagdollBone {
            swing_limit: 0.6,
            twist_limits: [-0.6, 0.6],
            ..base
        }
    } else if has(&["spine", "chest", "torso"]) {
        RagdollBone {
            swing_limit: 0.4,
            twist_limits: [-0.3, 0.3],
            ..base
        }
    } else if has(&["hand", "foot", "wrist", "ankle", "toe"]) {
        RagdollBone {
            swing_limit: 0.6,
            twist_limits: [-0.3, 0.3],
            ..base
        }
    } else {
        base
    }
}
