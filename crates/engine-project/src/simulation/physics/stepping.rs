//! Fixed-step accumulator and interpolated pose write-back for `engine.PhysicsSettings`
//! `fixed_timestep`.
//!
//! With a fixed step `h`, each frame delta is added to an `f64` accumulator and whole steps
//! of exactly `h` run, so the simulation advances the same sequence of states however the
//! total time is split into frames. The step count uses a `1e-3 * h` tolerance so `f32`
//! frame deltas that sum to an exact multiple of `h` (for example thirty `1/30` frames
//! against sixty `1/60` frames) agree on the step count; the accumulator may therefore
//! carry a tiny negative remainder. At most `max_substeps` steps run per frame: further whole
//! steps are dropped (the spiral-of-death cap) and reported through [`StepStatus`] and the
//! `physics.step.clamped` event.
use super::bodies::{Bodies, depth};
use super::types::Motion;
use super::{Worlds, emit_step_clamped};
use crate::Result;
use bevy_ecs::prelude::*;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Tolerance, in fractions of a step, when counting whole steps in the accumulator.
const STEP_TOLERANCE: f64 = 1e-3;
/// Largest frame delta accepted when a fixed step is configured.
pub(super) const MAX_FIXED_FRAME: f32 = 10.0;

/// How dynamic body `Transform`s relate to the fixed steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// Transforms hold the latest simulated pose.
    #[default]
    None,
    /// Transforms hold the pose blended between the previous and latest fixed step by
    /// [`StepStatus::alpha`]. Physics state itself is never altered.
    Interpolate,
}
/// What the last physics frame did. Without `fixed_timestep`, `alpha` is 1 and `steps`
/// counts the variable substeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepStatus {
    pub fixed_timestep: Option<f32>,
    /// Steps run in the last frame.
    pub steps: u32,
    pub total_steps: u64,
    /// Fraction of a step the accumulator holds, in 0..=1; the interpolation blend factor.
    pub alpha: f32,
    /// Seconds carried to the next frame.
    pub accumulator: f32,
    /// The last frame hit `max_substeps` and dropped whole steps.
    pub clamped: bool,
    /// Seconds dropped by the last frame.
    pub dropped_time: f32,
    /// Frames that were clamped so far.
    pub clamped_frames: u64,
}
impl Default for StepStatus {
    fn default() -> Self {
        Self {
            fixed_timestep: None,
            steps: 0,
            total_steps: 0,
            alpha: 1.0,
            accumulator: 0.0,
            clamped: false,
            dropped_time: 0.0,
            clamped_frames: 0,
        }
    }
}
/// Steps to run this frame.
pub(super) struct Plan {
    pub(super) steps: usize,
    pub(super) dropped: f64,
}
/// Dynamic pose at the start of the last fixed step.
#[derive(Clone, Copy)]
struct Snapshot {
    position: Vec3,
    rotation: Quat,
}
#[derive(Default)]
pub(super) struct Stepper {
    accumulator: f64,
    previous: BTreeMap<Entity, Snapshot>,
    pub(super) status: StepStatus,
}
impl Stepper {
    /// Adds `dt` and decides how many fixed steps of `step` seconds run, at most `max`.
    pub(super) fn plan(&mut self, dt: f32, step: f32, max: usize) -> Plan {
        let step = f64::from(step);
        self.accumulator += f64::from(dt);
        let due = (self.accumulator / step + STEP_TOLERANCE).floor().max(0.0) as usize;
        let steps = due.min(max);
        let dropped = (due - steps) as f64 * step;
        self.accumulator -= due as f64 * step;
        let status = &mut self.status;
        status.fixed_timestep = Some(step as f32);
        status.steps = steps as u32;
        status.total_steps += steps as u64;
        status.accumulator = self.accumulator.max(0.0) as f32;
        status.alpha = (self.accumulator / step).clamp(0.0, 1.0) as f32;
        status.clamped = dropped > 0.0;
        status.dropped_time = dropped as f32;
        status.clamped_frames += u64::from(status.clamped);
        Plan { steps, dropped }
    }
    /// Records a variable-substep frame.
    pub(super) fn variable(&mut self, substeps: usize) {
        self.status = StepStatus {
            steps: substeps as u32,
            total_steps: self.status.total_steps + substeps as u64,
            clamped_frames: self.status.clamped_frames,
            ..StepStatus::default()
        };
    }
    /// Drops the remembered pose of `entity` after a teleport, so interpolation never blends
    /// from the pose it left.
    pub(super) fn forget(&mut self, entity: Entity) {
        self.previous.remove(&entity);
    }
    /// Remembers the current dynamic poses; call before each fixed step when interpolating.
    pub(super) fn capture(&mut self, bodies: &Bodies, worlds: &Worlds) {
        self.previous.clear();
        for (entity, record) in &bodies.two {
            if record.config.motion == Motion::Dynamic
                && let Some(pose) = worlds.two.pose(entity.to_bits())
            {
                let position = Vec3::new(pose.position[0], pose.position[1], record.pose.2.z);
                let rotation = Quat::from_rotation_z(pose.rotation);
                self.previous
                    .insert(*entity, Snapshot { position, rotation });
            }
        }
        for (entity, record) in &bodies.three {
            if record.config.motion == Motion::Dynamic
                && let Some(pose) = worlds.three.pose(entity.to_bits())
            {
                self.previous.insert(
                    *entity,
                    Snapshot {
                        position: Vec3::from(pose.position),
                        rotation: Quat::from_array(pose.rotation),
                    },
                );
            }
        }
    }
    /// Writes dynamic poses blended by the current alpha, parents before children.
    pub(super) fn write_interpolated(
        &self,
        bodies: &Bodies,
        worlds: &Worlds,
        world: &mut World,
    ) -> Result<()> {
        let alpha = self.status.alpha;
        let blend =
            |entity: Entity, position: Vec3, rotation: Quat| match self.previous.get(&entity) {
                Some(previous) => (
                    previous.position.lerp(position, alpha),
                    previous.rotation.slerp(rotation, alpha),
                ),
                None => (position, rotation),
            };
        let mut writes = vec![];
        for (entity, record) in &bodies.two {
            if record.config.motion == Motion::Dynamic
                && let Some(pose) = worlds.two.pose(entity.to_bits())
            {
                let position = Vec3::new(pose.position[0], pose.position[1], record.pose.2.z);
                let (position, rotation) =
                    blend(*entity, position, Quat::from_rotation_z(pose.rotation));
                writes.push((*entity, record.pose.0, rotation, position));
            }
        }
        for (entity, record) in &bodies.three {
            if record.config.motion == Motion::Dynamic
                && let Some(pose) = worlds.three.pose(entity.to_bits())
            {
                let (position, rotation) = blend(
                    *entity,
                    Vec3::from(pose.position),
                    Quat::from_array(pose.rotation),
                );
                writes.push((*entity, record.pose.0, rotation, position));
            }
        }
        writes.sort_by_key(|(entity, ..)| depth(world, *entity));
        for (entity, scale, rotation, position) in writes {
            super::super::write_world_pose(world, entity, scale, rotation, position)?;
        }
        Ok(())
    }
    /// Emits `physics.step.clamped` on the settings entity after a clamped frame.
    pub(super) fn report(&self, world: &mut World, plan: &Plan, max: usize) -> Result<()> {
        if plan.dropped > 0.0 {
            emit_step_clamped(world, &self.status, max)?;
        }
        Ok(())
    }
}

/// What the last physics frame did (steps run, interpolation alpha, clamping).
pub fn step_status(world: &World) -> Result<StepStatus> {
    Ok(super::service(world)?.stepper.status)
}
/// Fraction of a fixed step the accumulator holds (1 without `fixed_timestep`): the blend
/// factor between the last two fixed steps for renderers that interpolate themselves.
pub fn interpolation_alpha(world: &World) -> Result<f32> {
    Ok(step_status(world)?.alpha)
}
