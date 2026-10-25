//! Skeleton ragdolls (`engine.Ragdoll`): one dynamic body per bone, joints to the parent bone,
//! and three modes. `animated` keeps the bones kinematic on the animation pose, `ragdoll` is
//! limp, and `powered` drives the joint motors toward the animation pose. The physics pose is
//! blended with the animation pose into the bone `Transform`s after the physics step.
//!
//! The bone entities keep animation as their pose writer: the animation service writes the
//! pose first, this feature reads it (a `Transform` it did not write itself is the animation
//! pose) and then writes the blended result, so no `engine.PhysicsBody*` goes on bones. The
//! blended pose is rewritten on **every** frame (`after_frame`), including frames that run no
//! physics step, because the animation service rewrites the bones every frame; with
//! `interpolation` on it is blended between the previous and the latest step. See
//! `docs/physics-ragdoll.md`.
use super::{Context, Feature, Frame, Interpolation, Resolved, Worlds, emit};
use crate::Result;
use crate::simulation::{decompose, invalid, mapped};
use bevy_ecs::prelude::*;
use dim3::Dof;
use engine_physics::{BodyKind, CollisionLayers, JointOptions, MotorDesc, dim3};
use glam::{Mat4, Quat, Vec3};
use serde_json::json;
use std::collections::BTreeMap;

mod api;
mod build;
mod component;

pub use api::{
    BoneReadout, HitReaction, RagdollStatus, add_ragdoll, ragdoll_blend_out, ragdoll_bone_impulse,
    ragdoll_bone_state, ragdoll_from_hierarchy, ragdoll_hit, ragdoll_joint_state,
    ragdoll_set_blend, ragdoll_set_mode, ragdoll_set_strength, ragdoll_status,
};
pub use component::{BoneJoint, BoneShape, Ragdoll, RagdollBone, RagdollMode};

use api::{Command, Hub};
use build::{Layout, Rest};

/// Most bones in one ragdoll.
pub const MAX_BONES: usize = 64;

/// Joint IDs live in their own namespace; this keeps them apart from authored joints, which
/// use entity bits.
fn joint_id(bone: Entity) -> u64 {
    bone.to_bits() ^ (1 << 63)
}

struct Bone {
    entity: Entity,
    layout: Layout,
    /// Pose below the parent as the animation (or the user) wrote it.
    anim_local: (Vec3, Quat),
    /// The `Transform` this feature wrote last; a different value is a new animation pose.
    written: Option<([f32; 3], [f32; 4])>,
    anim_world: (Vec3, Quat),
    anim_velocity: (Vec3, Vec3),
    seen: bool,
}
struct Fade {
    elapsed: f32,
    total: f32,
    explicit: bool,
}
struct Hit {
    scale: f32,
    recover: f32,
    elapsed: f32,
}
/// Live state of one bone body, kept when the bodies are rebuilt in place.
#[derive(Clone, Copy)]
struct BodyState {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    angular: [f32; 3],
}
/// A kinematic bone body moving from where it stood at the start of the frame to its target.
struct Drive {
    start: (Vec3, Quat),
    target: (Vec3, Quat),
}
struct Rig {
    structure: Ragdoll,
    /// Contact exclusion group of the bones when `self_collision` is off: the owner entity.
    group: u64,
    entities: Vec<Entity>,
    rest_local: Vec<(Vec3, Quat)>,
    bones: Vec<Bone>,
    /// Bone indices, parents first.
    order: Vec<usize>,
    active: RagdollMode,
    /// `Some(dynamic)` once bodies exist.
    built: Option<bool>,
    motors: bool,
    /// Rest-pose moment of inertia (kg m^2) of each bone's subtree about its joint.
    inertia: Vec<f32>,
    fade: Option<Fade>,
    hit: Option<Hit>,
    slow: f32,
    settled: bool,
    /// Bone world poses (physics, rotated by the bone offset) before the latest step.
    previous: Vec<(Vec3, Quat)>,
    /// Kinematic bodies' motion over the frame, by bone index.
    drives: Vec<Option<Drive>>,
}
impl Rig {
    /// Weight of the physics pose in the written bone pose.
    fn weight(&self, config: &Ragdoll) -> f32 {
        let fade = self.fade.as_ref().map_or(1., |f| {
            if f.elapsed >= f.total {
                0.
            } else {
                (1. - f.elapsed / f.total).clamp(0., 1.)
            }
        });
        config.blend * fade
    }
    fn hit_scale(&self) -> f32 {
        self.hit.as_ref().map_or(1., |h| {
            h.scale + (1. - h.scale) * (h.elapsed / h.recover).clamp(0., 1.)
        })
    }
}

enum Resolution {
    Inactive(String),
    Bones(Vec<Entity>),
}
fn resolve(world: &World, config: &Ragdoll) -> Result<Resolution> {
    let mut entities = vec![];
    for bone in &config.bones {
        match crate::services::find_entity(world, &bone.bone) {
            Some(e) if world.get::<crate::Transform>(e).is_some() => entities.push(e),
            Some(_) => {
                return Err(invalid(format!(
                    "Ragdoll bone {} requires Transform",
                    bone.bone
                )));
            }
            None => {
                return Ok(Resolution::Inactive(format!(
                    "bone {} does not exist",
                    bone.bone
                )));
            }
        }
    }
    Ok(Resolution::Bones(entities))
}
/// Parent bone of each bone: the direct entity parent, which must be a bone if any ancestor is.
fn bone_parents(world: &World, entities: &[Entity]) -> Result<Vec<Option<usize>>> {
    let index: BTreeMap<Entity, usize> =
        entities.iter().enumerate().map(|(i, e)| (*e, i)).collect();
    entities
        .iter()
        .map(|entity| {
            let mut current = *entity;
            let mut direct = true;
            for _ in 0..256 {
                let Some(parent) = world.get::<crate::Parent>(current) else {
                    return Ok(None);
                };
                let Some(next) = crate::services::find_entity(world, &parent.0) else {
                    return Ok(None);
                };
                if let Some(&p) = index.get(&next) {
                    return if direct {
                        Ok(Some(p))
                    } else {
                        Err(invalid(
                            "A non-bone entity sits between ragdoll bones; list it as a bone",
                        ))
                    };
                }
                direct = false;
                current = next;
            }
            Err(invalid("Ragdoll bone hierarchy is too deep"))
        })
        .collect()
}
fn unit(scale: Vec3) -> bool {
    scale.abs_diff_eq(Vec3::ONE, 1e-3)
}
/// World pose of the entity above a root bone (identity without a parent).
fn parent_world(world: &World, bone: Entity) -> Result<(Vec3, Quat)> {
    let Some(parent) = world
        .get::<crate::Parent>(bone)
        .and_then(|p| crate::services::find_entity(world, &p.0))
    else {
        return Ok((Vec3::ZERO, Quat::IDENTITY));
    };
    let matrix = Mat4::from_cols_array_2d(&crate::world_transform(world, parent)?.matrix);
    let (scale, rotation, position) = decompose(matrix)?;
    if !unit(scale) {
        return Err(invalid("Ragdoll requires unit world scale above the bones"));
    }
    Ok((position, rotation))
}
fn transform_pose(world: &World, bone: Entity) -> Result<(Vec3, Quat)> {
    let t = world
        .get::<crate::Transform>(bone)
        .ok_or_else(|| invalid("Ragdoll bone lost its Transform"))?;
    if !unit(Vec3::from(t.scale)) {
        return Err(invalid("Ragdoll bones require unit scale"));
    }
    Ok((
        Vec3::from(t.position),
        Quat::from_array(t.rotation).normalize(),
    ))
}
fn compose(parent: (Vec3, Quat), local: (Vec3, Quat)) -> (Vec3, Quat) {
    (
        parent.0 + parent.1 * local.0,
        (parent.1 * local.1).normalize(),
    )
}
fn order_of(parents: &[Option<usize>]) -> Vec<usize> {
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = parents[i] {
            d += 1;
            i = p;
            if d > parents.len() {
                break;
            }
        }
        d
    };
    let mut order: Vec<usize> = (0..parents.len()).collect();
    order.sort_by_key(|i| (depth(*i), *i));
    order
}
fn arc_y(direction: Vec3) -> Quat {
    Quat::from_rotation_arc(Vec3::Y, direction).normalize()
}

impl Rig {
    fn create(
        world: &World,
        group: u64,
        config: &Ragdoll,
        entities: Vec<Entity>,
        previous_rest: Option<Vec<(Vec3, Quat)>>,
    ) -> Result<Self> {
        let parents = bone_parents(world, &entities)?;
        let order = order_of(&parents);
        let rest_local = match previous_rest {
            Some(rest) if rest.len() == entities.len() => rest,
            _ => entities
                .iter()
                .map(|e| transform_pose(world, *e))
                .collect::<Result<_>>()?,
        };
        let mut rest: Vec<Option<Rest>> = (0..entities.len()).map(|_| None).collect();
        for &i in &order {
            let base = match parents[i] {
                Some(p) => {
                    let parent = rest[p].as_ref().expect("parents come first");
                    (parent.position, parent.rotation)
                }
                None => parent_world(world, entities[i])?,
            };
            let (position, rotation) = compose(base, rest_local[i]);
            rest[i] = Some(Rest {
                parent: parents[i],
                position,
                rotation,
            });
        }
        let rest: Vec<Rest> = rest.into_iter().flatten().collect();
        let layouts = build::layouts(config, &rest, &order)?;
        let layouts: Vec<Layout> = layouts
            .into_iter()
            .map(|l| l.expect("every bone has a layout"))
            .collect();
        let inertia = (0..entities.len())
            .map(|root| {
                (0..entities.len())
                    .filter(|&j| {
                        let mut at = Some(j);
                        while let Some(k) = at {
                            if k == root {
                                return true;
                            }
                            at = parents[k];
                        }
                        false
                    })
                    .map(|j| {
                        let l = &layouts[j];
                        let center = rest[j].position + l.center;
                        l.mass
                            * ((center - rest[root].position).length_squared()
                                + l.length * l.length / 12.)
                    })
                    .sum::<f32>()
                    .max(1e-6)
            })
            .collect();
        let bones = layouts
            .into_iter()
            .enumerate()
            .map(|(i, layout)| Bone {
                entity: entities[i],
                layout,
                anim_local: rest_local[i],
                written: None,
                anim_world: (rest[i].position, rest[i].rotation),
                anim_velocity: (Vec3::ZERO, Vec3::ZERO),
                seen: false,
            })
            .collect();
        let count = entities.len();
        Ok(Self {
            structure: config.structure(),
            group,
            entities,
            rest_local,
            bones,
            order,
            active: config.mode,
            built: None,
            motors: false,
            inertia,
            fade: None,
            hit: None,
            slow: 0.,
            settled: false,
            previous: vec![],
            drives: (0..count).map(|_| None).collect(),
        })
    }
    /// Reads the animation pose (any `Transform` this feature did not write) and derives the
    /// world poses and, over `dt` seconds, the animation velocities.
    fn refresh_animation(&mut self, world: &World, dt: f32) -> Result<()> {
        for &i in &self.order {
            let entity = self.bones[i].entity;
            let (position, rotation) = transform_pose(world, entity)?;
            let t = world
                .get::<crate::Transform>(entity)
                .ok_or_else(|| invalid("Ragdoll bone lost its Transform"))?;
            let current = (t.position, t.rotation);
            if self.bones[i].written != Some(current) {
                self.bones[i].anim_local = (position, rotation);
            }
            let base = match self.bones[i].layout.parent {
                Some(p) => self.bones[p].anim_world,
                None => parent_world(world, entity)?,
            };
            let bone = &mut self.bones[i];
            let pose = compose(base, bone.anim_local);
            if dt > 0. && bone.seen {
                bone.anim_velocity = (
                    (pose.0 - bone.anim_world.0) / dt,
                    build::angular_velocity(bone.anim_world.1, pose.1, dt),
                );
            }
            bone.anim_world = pose;
            bone.seen = true;
        }
        Ok(())
    }
    /// Body pose that places a bone at its animation pose.
    fn anim_body(&self, i: usize) -> (Vec3, Quat) {
        let bone = &self.bones[i];
        (
            bone.anim_world.0,
            (bone.anim_world.1 * bone.layout.offset.inverse()).normalize(),
        )
    }
    fn remove_bodies(&mut self, worlds: &mut Worlds) {
        for bone in &self.bones {
            // Removing a body removes its joints.
            worlds.three.remove(bone.entity.to_bits());
        }
        self.built = None;
        self.motors = false;
    }
    /// The live state of every bone body, if all of them exist.
    fn capture(&self, worlds: &Worlds) -> Option<Vec<BodyState>> {
        self.bones
            .iter()
            .map(|bone| {
                let id = bone.entity.to_bits();
                let pose = worlds.three.pose(id)?;
                let velocity = worlds.three.velocity(id)?;
                Some(BodyState {
                    position: pose.position,
                    rotation: pose.rotation,
                    velocity: velocity.linear,
                    angular: velocity.angular,
                })
            })
            .collect()
    }
    /// Creates the bone bodies and their joints. `keep` rebuilds them where they are, with
    /// their velocities (a settings change); otherwise they start at the animation pose, with
    /// the animation velocity when `inherit` is set. If the body table has no room for every
    /// bone nothing is built and `built` stays `None`.
    fn build_bodies(
        &mut self,
        worlds: &mut Worlds,
        settings: &Resolved,
        config: &Ragdoll,
        inherit: bool,
        keep: Option<Vec<BodyState>>,
    ) -> Result<()> {
        let dynamic = self.active != RagdollMode::Animated;
        let inherit = inherit && dynamic && self.built == Some(false) && config.inherit_velocity;
        self.remove_bodies(worlds);
        self.drives = (0..self.bones.len()).map(|_| None).collect();
        if worlds.three.len() + self.bones.len() > worlds.three.capacity() {
            return Ok(());
        }
        for i in 0..self.bones.len() {
            let bone = &self.bones[i];
            let layout = &bone.layout;
            let pinned = config.pin_root && layout.parent.is_none();
            let (position, rotation) = self.anim_body(i);
            let (position, rotation, velocity, angular) = match keep.as_ref().map(|k| k[i]) {
                Some(state) => (
                    Vec3::from(state.position),
                    Quat::from_array(state.rotation),
                    Vec3::from(state.velocity),
                    Vec3::from(state.angular),
                ),
                None if inherit && !pinned => (
                    position,
                    rotation,
                    bone.anim_velocity.0,
                    bone.anim_velocity.1,
                ),
                None => (position, rotation, Vec3::ZERO, Vec3::ZERO),
            };
            let desc = dim3::BodyDesc {
                kind: if dynamic && !pinned {
                    BodyKind::Dynamic
                } else {
                    BodyKind::Kinematic
                },
                position: position.to_array(),
                rotation: rotation.to_array(),
                velocity: velocity.to_array(),
                angular_velocity: angular.to_array(),
                ccd: false,
            };
            // The bones keep the authored (or default) collision groups, so they collide with
            // whatever the layer matrix says they collide with.
            let material = settings.material(&crate::simulation::BodyMaterial {
                density: layout.density,
                friction: config.friction,
                restitution: config.restitution,
                layer_name: config.layer_name.clone(),
                ..Default::default()
            })?;
            let shape = dim3::Shape::Compound {
                parts: vec![dim3::CompoundPart {
                    position: layout.center.to_array(),
                    rotation: arc_y(layout.direction).to_array(),
                    shape: layout.shape.clone(),
                }],
            };
            let id = bone.entity.to_bits();
            worlds
                .three
                .insert(id, desc, shape, material)
                .map_err(mapped)?;
            if !config.self_collision {
                // Bones of one ragdoll never touch each other, whatever their layers say.
                worlds
                    .three
                    .set_contact_exclusion(id, self.group)
                    .map_err(mapped)?;
            }
        }
        if dynamic {
            for bone in &self.bones {
                let (Some(parent), Some(desc)) = (bone.layout.parent, bone.layout.joint) else {
                    continue;
                };
                worlds
                    .three
                    .insert_joint_with(
                        joint_id(bone.entity),
                        self.bones[parent].entity.to_bits(),
                        bone.entity.to_bits(),
                        desc,
                        JointOptions::default(),
                    )
                    .map_err(mapped)?;
            }
        }
        self.built = Some(dynamic);
        Ok(())
    }
    /// Plans the motion of the kinematic bodies (every bone when animated, the pinned root
    /// otherwise) over the coming frame; `apply_drives` spreads it over the steps.
    fn follow_animation(&mut self, worlds: &Worlds, config: &Ragdoll) {
        let dynamic = self.active != RagdollMode::Animated;
        for i in 0..self.bones.len() {
            let bone = &self.bones[i];
            if dynamic && !(config.pin_root && bone.layout.parent.is_none()) {
                self.drives[i] = None;
                continue;
            }
            let Some(now) = worlds.three.pose(bone.entity.to_bits()) else {
                continue;
            };
            self.drives[i] = Some(Drive {
                start: (Vec3::from(now.position), Quat::from_array(now.rotation)),
                target: self.anim_body(i),
            });
        }
    }
    /// Moves the kinematic bodies `progress` (0..=1) of the way along their frame motion.
    fn apply_drives(&self, worlds: &mut Worlds, progress: f32) -> Result<()> {
        for (bone, drive) in self.bones.iter().zip(&self.drives) {
            let Some(drive) = drive else {
                continue;
            };
            let (position, rotation) = if progress >= 1. {
                drive.target
            } else {
                (
                    drive.start.0.lerp(drive.target.0, progress),
                    drive.start.1.slerp(drive.target.1, progress),
                )
            };
            worlds
                .three
                .set_kinematic_target(
                    bone.entity.to_bits(),
                    position.to_array(),
                    rotation.to_array(),
                )
                .map_err(mapped)?;
        }
        Ok(())
    }
    /// Drives the joint motors toward the animation pose (powered mode) or releases them.
    /// Gains scale with the rest-pose inertia of the bone and everything below it, so a joint
    /// holds its subtree with the same stiffness whatever the bone sizes.
    fn set_motors(&mut self, worlds: &mut Worlds, config: &Ragdoll) -> Result<()> {
        let scale = config.strength * self.hit_scale();
        let drive = self.active == RagdollMode::Powered && scale > 0.;
        if !drive && !self.motors {
            return Ok(());
        }
        for (i, bone) in self.bones.iter().enumerate() {
            let Some(parent) = bone.layout.parent else {
                continue;
            };
            let id = joint_id(bone.entity);
            let strength = scale * config.bones[i].strength;
            if !drive || strength <= 0. {
                for dof in [Dof::AngX, Dof::AngY, Dof::AngZ] {
                    if worlds.three.joint_motor(id, dof).is_some() {
                        worlds.three.clear_joint_motor(id, dof).map_err(mapped)?;
                    }
                }
                continue;
            }
            let relative = self.anim_body(parent).1.inverse() * self.anim_body(i).1;
            for (dof, target) in build::motor_targets(&bone.layout, relative) {
                let motor = MotorDesc {
                    target_position: target,
                    stiffness: config.motor_stiffness * strength * self.inertia[i],
                    damping: config.motor_damping * strength * self.inertia[i],
                    force_based: true,
                    ..MotorDesc::default()
                };
                worlds
                    .three
                    .set_joint_motor(id, dof, motor)
                    .map_err(mapped)?;
            }
        }
        self.motors = drive;
        Ok(())
    }
}

/// The ragdoll feature: rigs by owning entity, plus events waiting for `after_step`.
pub(super) struct Ragdolls {
    rigs: BTreeMap<Entity, Rig>,
    pending: Vec<(Entity, &'static str, serde_json::Value)>,
    /// Seconds simulated during the current tick.
    tick: f32,
    /// Layers and materials the bodies were last built with; a change rebuilds them.
    groups: Option<(CollisionLayers, BTreeMap<String, engine_physics::Material>)>,
}
impl Ragdolls {
    pub(super) fn new() -> Self {
        Self {
            rigs: BTreeMap::new(),
            pending: vec![],
            tick: 0.,
            groups: None,
        }
    }
    fn destroy(&mut self, worlds: &mut Worlds, entity: Entity) {
        if let Some(mut rig) = self.rigs.remove(&entity) {
            rig.remove_bodies(worlds);
        }
    }
    fn change_mode(&mut self, entity: Entity, to: RagdollMode) {
        let Some(rig) = self.rigs.get_mut(&entity) else {
            return;
        };
        let from = std::mem::replace(&mut rig.active, to);
        rig.fade = None;
        rig.slow = 0.;
        rig.settled = false;
        self.pending.push((
            entity,
            "mode_changed",
            json!({"from":from.as_str(),"to":to.as_str()}),
        ));
    }
    #[allow(clippy::too_many_arguments)]
    fn sync_one(
        &mut self,
        worlds: &mut Worlds,
        settings: &Resolved,
        world: &World,
        hub: &Hub,
        entity: Entity,
        config: &Ragdoll,
        dt: f32,
        commands: &[Command],
        regroup: bool,
    ) -> Result<()> {
        let bones = match resolve(world, config)? {
            Resolution::Inactive(reason) => {
                self.destroy(worlds, entity);
                hub.lock().status.insert(
                    entity,
                    RagdollStatus {
                        inactive_reason: Some(reason),
                        ..RagdollStatus::default()
                    },
                );
                return Ok(());
            }
            Resolution::Bones(bones) => bones,
        };
        let stale = self
            .rigs
            .get(&entity)
            .is_none_or(|r| r.entities != bones || r.structure != config.structure());
        if stale {
            let previous = self
                .rigs
                .get(&entity)
                .filter(|r| r.entities == bones)
                .map(|r| r.rest_local.clone());
            self.destroy(worlds, entity);
            self.rigs.insert(
                entity,
                Rig::create(world, entity.to_bits(), config, bones, previous)?,
            );
        }
        let rig = self.rigs.get_mut(&entity).expect("rig exists");
        rig.refresh_animation(world, dt)?;
        for command in commands {
            match command {
                Command::CancelFade { entity: e } if *e == entity => {
                    rig.fade = None;
                }
                Command::BlendOut { entity: e, seconds } if *e == entity => {
                    if rig.active != RagdollMode::Animated {
                        rig.fade = Some(Fade {
                            elapsed: 0.,
                            total: *seconds,
                            explicit: true,
                        });
                    }
                }
                Command::Impulse {
                    entity: e,
                    bone,
                    impulse,
                    point,
                    reaction,
                } if *e == entity => {
                    if rig.active == RagdollMode::Animated {
                        continue;
                    }
                    apply_impulse(worlds, rig, *bone, *impulse, *point)?;
                    if let Some(r) = reaction {
                        rig.hit = Some(Hit {
                            scale: r.strength_scale,
                            recover: r.recover_seconds,
                            elapsed: 0.,
                        });
                    }
                }
                _ => {}
            }
        }
        // Mode transitions. A fade started by the config (blend_out_seconds) is dropped when
        // the authored mode leaves `animated` again.
        let want = config.mode;
        if want != RagdollMode::Animated && rig.fade.as_ref().is_some_and(|f| !f.explicit) {
            rig.fade = None;
        }
        let animated_request = want == RagdollMode::Animated;
        if rig.fade.is_none() && want != rig.active {
            if animated_request && config.blend_out_seconds > 0. {
                rig.fade = Some(Fade {
                    elapsed: 0.,
                    total: config.blend_out_seconds,
                    explicit: false,
                });
            } else {
                self.change_mode(entity, want);
            }
        }
        let rig = self.rigs.get_mut(&entity).expect("rig exists");
        let dynamic = rig.active != RagdollMode::Animated;
        if rig.built != Some(dynamic) {
            let from_animated = rig.built == Some(false);
            rig.build_bodies(worlds, settings, config, from_animated, None)?;
        } else if regroup {
            // Layers or materials changed: the bodies take the new groups where they are.
            let keep = rig.capture(worlds);
            rig.build_bodies(worlds, settings, config, false, keep)?;
        }
        if rig.built.is_none() {
            // Not enough room in the body table for every bone: wait, and say why.
            if worlds.starved.insert(entity) {
                worlds.starved_new += 1;
            }
            hub.lock().status.insert(
                entity,
                RagdollStatus {
                    inactive_reason: Some("physics body capacity exceeded".into()),
                    ..RagdollStatus::default()
                },
            );
            return Ok(());
        }
        worlds.starved.remove(&entity);
        rig.follow_animation(worlds, config);
        rig.set_motors(worlds, config)?;
        let mut status = hub.lock();
        let entry = status.status.entry(entity).or_default();
        entry.active = true;
        entry.inactive_reason = None;
        entry.mode = rig.active;
        Ok(())
    }
}
fn apply_impulse(
    worlds: &mut Worlds,
    rig: &mut Rig,
    bone: Entity,
    impulse: Vec3,
    point: Option<Vec3>,
) -> Result<()> {
    let Some(i) = rig.entities.iter().position(|e| *e == bone) else {
        return Ok(());
    };
    let id = bone.to_bits();
    let Some(pose) = worlds.three.pose(id) else {
        return Ok(());
    };
    if rig.structure.pin_root && rig.bones[i].layout.parent.is_none() {
        return Ok(());
    }
    worlds
        .three
        .apply_impulse(id, impulse.to_array())
        .map_err(mapped)?;
    if let Some(point) = point {
        let center = Vec3::from(pose.position)
            + Quat::from_array(pose.rotation) * rig.bones[i].layout.center;
        let torque = (point - center).cross(impulse);
        worlds
            .three
            .apply_angular_impulse(id, torque.to_array())
            .map_err(mapped)?;
    }
    rig.slow = 0.;
    rig.settled = false;
    Ok(())
}

impl Feature for Ragdolls {
    fn name(&self) -> &'static str {
        "ragdoll"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        component::descriptors()
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        crate::register_component::<Ragdoll>(app, "engine.Ragdoll", 1)?;
        app.world_mut().insert_resource(Hub::default());
        Ok(())
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        let mut owners: BTreeMap<Entity, Entity> = BTreeMap::new();
        for entity in world.iter_entities() {
            let Some(config) = entity.get::<Ragdoll>() else {
                continue;
            };
            config.validate()?;
            if !entity.contains::<crate::Transform>() {
                return Err(invalid("engine.Ragdoll requires Transform"));
            }
            let Resolution::Bones(bones) = resolve(world, config)? else {
                continue;
            };
            bone_parents(world, &bones)?;
            for bone in bones {
                if world
                    .get::<crate::simulation::PhysicsBody2D>(bone)
                    .is_some()
                    || world
                        .get::<crate::simulation::PhysicsBody3D>(bone)
                        .is_some()
                    || world
                        .get::<crate::simulation::NavigationAgent>(bone)
                        .is_some()
                {
                    return Err(invalid(
                        "Ragdoll bones cannot also have a physics body or navigation agent",
                    ));
                }
                if owners.insert(bone, entity.id()).is_some() {
                    return Err(invalid("A bone belongs to at most one ragdoll"));
                }
            }
        }
        Ok(())
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        let Some(hub) = world.get_resource::<Hub>() else {
            return Ok(());
        };
        let Context {
            worlds,
            settings,
            frame_dt,
            ..
        } = cx;
        let commands = std::mem::take(&mut hub.lock().commands);
        // The animation advanced by this frame's delta since the previous sample.
        let dt = frame_dt;
        let regroup = self.groups.as_ref().is_some_and(|(layers, materials)| {
            *layers != settings.layers || *materials != settings.materials
        });
        self.groups = Some((settings.layers.clone(), settings.materials.clone()));
        let mut present: Vec<(Entity, Ragdoll)> = world
            .iter_entities()
            .filter_map(|e| Some((e.id(), e.get::<Ragdoll>()?.clone())))
            .collect();
        present.sort_by_key(|(e, _)| *e);
        let gone: Vec<Entity> = self
            .rigs
            .keys()
            .filter(|e| present.iter().all(|(p, _)| p != *e))
            .copied()
            .collect();
        for entity in gone {
            self.destroy(worlds, entity);
        }
        hub.lock()
            .status
            .retain(|e, _| present.iter().any(|(p, _)| p == e));
        for (entity, config) in &present {
            self.sync_one(
                worlds, settings, world, hub, *entity, config, dt, &commands, regroup,
            )?;
        }
        Ok(())
    }
    fn before_substep(&mut self, cx: Context<'_>, dt: f32) -> Result<()> {
        self.tick += dt;
        for rig in self.rigs.values_mut() {
            if rig.built.is_none() {
                continue;
            }
            // The pose before the step is the start of the interpolation toward its result.
            rig.previous = rig.bone_poses(&*cx.worlds)?;
            rig.apply_drives(cx.worlds, cx.progress)?;
        }
        Ok(())
    }
    fn after_step(&mut self, cx: Context<'_>, world: &mut World) -> Result<()> {
        let tick = std::mem::take(&mut self.tick);
        let worlds = &*cx.worlds;
        let entities: Vec<Entity> = self.rigs.keys().copied().collect();
        for entity in entities {
            let Some(config) = world.get::<Ragdoll>(entity).cloned() else {
                continue;
            };
            let Some(rig) = self.rigs.get_mut(&entity) else {
                continue;
            };
            if rig.built.is_none() {
                continue;
            }
            let outcome = finish(rig, worlds, world, &config, tick)?;
            if let Some(hub) = world.get_resource::<Hub>() {
                hub.lock().status.insert(entity, outcome.status);
            }
            if outcome.settled {
                self.pending.push((
                    entity,
                    "settled",
                    json!({"mode":rig.active.as_str(),"seconds":rig.slow}),
                ));
            }
            if outcome.faded {
                world
                    .get_mut::<Ragdoll>(entity)
                    .expect("checked above")
                    .mode = RagdollMode::Animated;
                self.change_mode(entity, RagdollMode::Animated);
            }
        }
        for (entity, event, payload) in std::mem::take(&mut self.pending) {
            emit(world, &*self, event, entity, None, payload)?;
        }
        Ok(())
    }
    fn after_frame(&mut self, cx: Context<'_>, world: &mut World, frame: Frame) -> Result<()> {
        // The animation service rewrote the bones this frame; put the physics pose back on
        // top of it, blended between the previous and the latest step when interpolating.
        let alpha = if cx.settings.interpolation == Interpolation::Interpolate {
            frame.alpha
        } else {
            1.
        };
        let worlds = &*cx.worlds;
        for (entity, rig) in &mut self.rigs {
            if rig.built != Some(true) || rig.active == RagdollMode::Animated {
                continue;
            }
            let Some(config) = world.get::<Ragdoll>(*entity).cloned() else {
                continue;
            };
            write_pose(rig, worlds, world, &config, alpha)?;
        }
        Ok(())
    }
}
struct Outcome {
    status: RagdollStatus,
    settled: bool,
    faded: bool,
}
impl Rig {
    /// World pose of every bone as the bodies hold it now.
    fn bone_poses(&self, worlds: &Worlds) -> Result<Vec<(Vec3, Quat)>> {
        self.bones
            .iter()
            .map(|bone| {
                let pose = worlds
                    .three
                    .pose(bone.entity.to_bits())
                    .ok_or_else(|| invalid("Ragdoll body missing"))?;
                let rotation = (Quat::from_array(pose.rotation) * bone.layout.offset).normalize();
                Ok((Vec3::from(pose.position), rotation))
            })
            .collect()
    }
    /// Pose of every bone below its parent for the given world poses.
    fn locals(&self, world: &World, phys: &[(Vec3, Quat)]) -> Result<Vec<(Vec3, Quat)>> {
        let mut locals = vec![(Vec3::ZERO, Quat::IDENTITY); self.bones.len()];
        for &i in &self.order {
            let bone = &self.bones[i];
            let (position, rotation) = match bone.layout.parent {
                Some(p) => phys[p],
                None => parent_world(world, bone.entity)?,
            };
            locals[i] = (
                rotation.inverse() * (phys[i].0 - position),
                (rotation.inverse() * phys[i].1).normalize(),
            );
        }
        Ok(locals)
    }
}
/// Writes the bone `Transform`s: the physics pose (blended between the previous and the
/// latest step by `alpha`) mixed with the animation pose by the ragdoll weight. Runs every
/// frame, because the animation service rewrites the bones every frame.
fn write_pose(
    rig: &mut Rig,
    worlds: &Worlds,
    world: &mut World,
    config: &Ragdoll,
    alpha: f32,
) -> Result<()> {
    let mut phys = rig.bone_poses(worlds)?;
    if rig.previous.len() == phys.len() {
        for (now, before) in phys.iter_mut().zip(&rig.previous) {
            *now = (
                before.0.lerp(now.0, alpha),
                before.1.slerp(now.1, alpha).normalize(),
            );
        }
    }
    let locals = rig.locals(world, &phys)?;
    let weight = rig.weight(config);
    for i in rig.order.clone() {
        let bone = &rig.bones[i];
        let (position, rotation) = if weight <= 0. {
            bone.anim_local
        } else if weight >= 1. {
            locals[i]
        } else {
            (
                bone.anim_local.0.lerp(locals[i].0, weight),
                bone.anim_local.1.slerp(locals[i].1, weight).normalize(),
            )
        };
        let mut transform = world
            .get_mut::<crate::Transform>(bone.entity)
            .ok_or_else(|| invalid("Ragdoll bone lost its Transform"))?;
        transform.position = position.to_array();
        transform.rotation = rotation.to_array();
        let written = (transform.position, transform.rotation);
        rig.bones[i].written = Some(written);
    }
    Ok(())
}
/// Advances fades and hits, detects settling, and reads the bones back for the status.
fn finish(
    rig: &mut Rig,
    worlds: &Worlds,
    world: &World,
    config: &Ragdoll,
    tick: f32,
) -> Result<Outcome> {
    let dynamic = rig.active != RagdollMode::Animated;
    let mut status = RagdollStatus {
        active: true,
        inactive_reason: None,
        mode: rig.active,
        blend: 0.,
        fading: rig.fade.is_some(),
        settled: rig.settled,
        pose_error: 0.,
        hit_scale: rig.hit_scale(),
        bones: vec![],
    };
    let mut readouts = vec![];
    let phys = rig.bone_poses(worlds)?;
    for (bone, pose) in rig.bones.iter().zip(&phys) {
        let id = bone.entity.to_bits();
        let velocity = worlds
            .three
            .velocity(id)
            .ok_or_else(|| invalid("Ragdoll body missing"))?;
        readouts.push(BoneReadout {
            entity: bone.entity,
            position: pose.0,
            rotation: pose.1,
            linear_velocity: Vec3::from(velocity.linear),
            angular_velocity: Vec3::from(velocity.angular),
            dynamic: dynamic && !(config.pin_root && bone.layout.parent.is_none()),
            pose_error: 0.,
        });
    }
    let mut faded = false;
    if dynamic {
        if let Some(fade) = &mut rig.fade {
            fade.elapsed += tick;
            faded = fade.elapsed >= fade.total;
        }
        status.blend = if faded { 0. } else { rig.weight(config) };
        let locals = rig.locals(world, &phys)?;
        let mut errors = vec![];
        for i in rig.order.clone() {
            if rig.bones[i].layout.parent.is_some() {
                let error = build::angle_between(locals[i].1, rig.bones[i].anim_local.1);
                readouts[i].pose_error = error;
                errors.push(error);
            }
        }
        if !errors.is_empty() {
            status.pose_error = errors.iter().sum::<f32>() / errors.len() as f32;
        }
        if let Some(hit) = &mut rig.hit {
            hit.elapsed += tick;
            if hit.elapsed >= hit.recover {
                rig.hit = None;
            }
        }
    }
    let mut newly_settled = false;
    if dynamic {
        let slow = readouts.iter().filter(|b| b.dynamic).all(|b| {
            b.linear_velocity.length() < config.settle_speed
                && b.angular_velocity.length() < 3. * config.settle_speed
        });
        if slow {
            rig.slow += tick;
            if rig.slow >= config.settle_seconds && !rig.settled {
                rig.settled = true;
                newly_settled = true;
            }
        } else {
            rig.slow = 0.;
            rig.settled = false;
        }
    }
    status.settled = rig.settled;
    status.bones = readouts;
    Ok(Outcome {
        status,
        settled: newly_settled,
        faded,
    })
}
