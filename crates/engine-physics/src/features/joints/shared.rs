//! Dimension-generic joint side table and `World` methods; the reference feature for the
//! extension seam (see `src/world.rs`). `super` is the dimension's joint descriptor module
//! and `super::super` the dimension module.
use super::super::{Core, Extension, World};
use super::{Angles, Dof, JointDesc, angular_from, angular_norm, lever, relative};
use super::{GenericJoint, ImpulseJoint, ImpulseJointHandle, MotorModel, Position};
use crate::{AxisMode, BreakLimits, Error, JointBroken, JointOptions, MotorDesc, Result};
use std::collections::BTreeMap;

/// Most break records kept for [`World::broken_joint`] queries and pending drains.
const MAX_BROKEN: usize = 1024;
/// Damping gain of the armed minimum-distance motor; large enough to act as a hard stop.
const MIN_DISTANCE_GAIN: f32 = 1.0e5;

struct Record {
    handle: ImpulseJointHandle,
    first: u64,
    second: u64,
    break_limits: BreakLimits,
    /// Minimum anchor distance (distance joints only), enforced by `Joints::pre_step`.
    min_distance: Option<f32>,
    min_active: bool,
}

/// Joint ID -> Rapier handle, body IDs and break thresholds, plus break history.
#[derive(Default)]
struct Joints {
    joints: BTreeMap<u64, Record>,
    broken: BTreeMap<u64, JointBroken>,
    pending: Vec<JointBroken>,
    dt: f32,
}

/// Loads of a joint from the last solve, scaled by `inv_dt`. Rapier writes back the impulses
/// of its decoupled constraint rows: the linear rows are a force through the center of mass
/// of the dynamic body and the angular rows a moment about that center. The returned torque
/// is re-expressed about the joint anchor (`moment + arm x force`), so a load hanging off a
/// lever arm counts toward the torque. Both are in frame-1 axes.
fn loads(core: &Core, joint: &ImpulseJoint, inv_dt: f32) -> (Position, Angles) {
    let dof_load = |dof: Dof| {
        let i = dof.index();
        (joint.impulses[i] + joint.data.limits[i].impulse + joint.data.motors[i].impulse) * inv_dt
    };
    let mut force = Position::default();
    for (slot, dof) in force.iter_mut().zip(Dof::ALL) {
        *slot = dof_load(dof);
    }
    let moment: Vec<f32> = Dof::ALL[Dof::LINEAR..]
        .iter()
        .map(|dof| dof_load(*dof))
        .collect();
    let moment = angular_from(&moment);
    let bodies = &core.rapier.bodies;
    let reference = [joint.body2(), joint.body1()]
        .into_iter()
        .filter_map(|handle| bodies.get(handle))
        .find(|body| body.is_dynamic());
    let (Some(body1), Some(reference)) = (bodies.get(joint.body1()), reference) else {
        return (force, moment);
    };
    let frame1 = *body1.position() * joint.data.local_frame1;
    let arm = frame1.inverse_transform_point(reference.center_of_mass());
    (force, lever(moment, arm.to_array(), force))
}
fn magnitude(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

impl Joints {
    /// Converts Rapier's written-back impulses to forces. See `IMPULSE_SUBSTEPS`.
    fn inv_dt(&self, core: &Core) -> f32 {
        if self.dt > 0. {
            substeps(core) / self.dt
        } else {
            0.
        }
    }
}
fn substeps(core: &Core) -> f32 {
    core.rapier
        .integration_parameters
        .num_solver_iterations
        .max(1) as f32
}
impl Extension for Joints {
    fn pre_step(&mut self, core: &mut Core, dt: f32) -> Result<()> {
        self.dt = dt;
        for record in self.joints.values_mut() {
            let Some(min) = record.min_distance else {
                continue;
            };
            let Some(joint) = core.rapier.impulse_joints.get(record.handle) else {
                continue;
            };
            let bodies = &core.rapier.bodies;
            let (Some(b1), Some(b2)) = (bodies.get(joint.body1()), bodies.get(joint.body2()))
            else {
                continue;
            };
            let p1 = b1.position().transform_point(joint.data.local_anchor1());
            let p2 = b2.position().transform_point(joint.data.local_anchor2());
            let sep = p2 - p1;
            let dist = sep.length();
            let approach = if dist > 1e-6 {
                (b2.velocity_at_point(p2) - b1.velocity_at_point(p1)).dot(sep / dist)
            } else {
                0.
            };
            // Arm a hard velocity-level push once the next step would reach the minimum.
            let active = dist + approach * dt < min + 1e-3;
            if active != record.min_active {
                record.min_active = active;
                if let Some(joint) = core.rapier.impulse_joints.get_mut(record.handle, true) {
                    let axis = Dof::LinX.axis();
                    if active {
                        joint.data.set_motor(axis, min, 0., 0., MIN_DISTANCE_GAIN);
                        joint.data.set_motor_max_force(axis, f32::MAX);
                    } else {
                        joint.data.motor_axes.remove(axis.into());
                    }
                }
            }
        }
        Ok(())
    }
    fn post_step(&mut self, core: &mut Core, dt: f32) -> Result<()> {
        self.dt = dt;
        let inv_dt = self.inv_dt(core);
        let mut broke = Vec::new();
        for (&id, record) in &self.joints {
            if record.break_limits == BreakLimits::default() {
                continue;
            }
            let Some(joint) = core.rapier.impulse_joints.get(record.handle) else {
                continue;
            };
            let (force, torque) = loads(core, joint, inv_dt);
            let (force, torque) = (magnitude(&force), angular_norm(torque));
            if record.break_limits.force.is_some_and(|limit| force > limit)
                || record
                    .break_limits
                    .torque
                    .is_some_and(|limit| torque > limit)
            {
                broke.push(JointBroken {
                    id,
                    first: record.first,
                    second: record.second,
                    force,
                    torque,
                });
            }
        }
        for event in broke {
            if let Some(record) = self.joints.remove(&event.id) {
                core.rapier.remove_impulse_joint(record.handle);
            }
            self.broken.insert(event.id, event);
            self.pending.push(event);
        }
        while self.broken.len() > MAX_BROKEN {
            self.broken.pop_first();
        }
        if self.pending.len() > MAX_BROKEN {
            let excess = self.pending.len() - MAX_BROKEN;
            self.pending.drain(..excess);
        }
        Ok(())
    }
    fn body_removed(&mut self, _core: &mut Core, id: u64) {
        // Rapier already removed the attached impulse joints with the body. Break records
        // stay so a rebuilt body does not resurrect a broken joint; they end with
        // `remove_joint` or when the ID is re-created.
        self.joints
            .retain(|_, record| record.first != id && record.second != id);
    }
}

/// Relative pose, loads and applied forces of one joint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointState {
    /// Frame 2's origin in frame 1 coordinates (meters), per linear axis.
    pub position: Position,
    /// Frame 2's orientation relative to frame 1 (radians), per angular axis. In 3D these are
    /// XYZ intrinsic Euler angles, so a hinge about X reads on the first component.
    pub angles: Angles,
    /// Constraint force per linear axis in frame 1 axes (N): locked rows + limits + motors.
    pub force: Position,
    /// Constraint torque about the joint anchor per angular axis in frame 1 axes (N*m),
    /// shaped like `angles`; includes the lever arm of the force (see `loads`).
    pub torque: Angles,
}

fn motor_model(force_based: bool) -> MotorModel {
    if force_based {
        MotorModel::ForceBased
    } else {
        MotorModel::AccelerationBased
    }
}

impl World {
    /// Joint IDs have a separate namespace from body IDs. At most four joints per
    /// configured body-capacity slot may be allocated in the world. Contacts between the
    /// connected bodies are disabled and the joint never breaks; see
    /// [`World::insert_joint_with`].
    pub fn insert_joint(
        &mut self,
        id: u64,
        first: u64,
        second: u64,
        desc: JointDesc,
    ) -> Result<()> {
        self.insert_joint_with(id, first, second, desc, JointOptions::default())
    }
    /// [`World::insert_joint`] with break thresholds and contact options. After each step a
    /// joint whose constraint force or torque magnitude exceeds its threshold is removed and
    /// reported through [`World::drain_joint_broken`].
    pub fn insert_joint_with(
        &mut self,
        id: u64,
        first: u64,
        second: u64,
        desc: JointDesc,
        options: JointOptions,
    ) -> Result<()> {
        if first == second {
            return Err(Error("joint requires distinct bodies"));
        }
        options.break_limits.validate()?;
        let (core, table) = self.extension_mut::<Joints>();
        if table.joints.contains_key(&id) {
            return Err(Error("duplicate joint ID"));
        }
        if table.joints.len() >= core.capacity() * 4 {
            return Err(Error("joint capacity exceeded"));
        }
        let a = core
            .entry(first)
            .map_err(|_| Error("unknown first joint body"))?
            .body;
        let b = core
            .entry(second)
            .map_err(|_| Error("unknown second joint body"))?
            .body;
        let min_distance = desc.min_distance();
        let mut joint = desc.build()?;
        joint.set_contacts_enabled(options.contacts_enabled);
        let handle = core.rapier.insert_impulse_joint(a, b, joint);
        table.broken.remove(&id);
        table.joints.insert(
            id,
            Record {
                handle,
                first,
                second,
                break_limits: options.break_limits,
                min_distance,
                min_active: false,
            },
        );
        Ok(())
    }
    pub fn remove_joint(&mut self, id: u64) -> bool {
        let (core, table) = self.extension_mut::<Joints>();
        table.broken.remove(&id);
        if let Some(record) = table.joints.remove(&id) {
            core.rapier.remove_impulse_joint(record.handle);
            true
        } else {
            false
        }
    }
    pub fn joint_count(&self) -> usize {
        self.extension::<Joints>().map_or(0, |t| t.joints.len())
    }
    pub fn has_joint(&self, id: u64) -> bool {
        self.extension::<Joints>()
            .is_some_and(|t| t.joints.contains_key(&id))
    }
    /// Live joint IDs in ascending order.
    pub fn joint_ids(&self) -> Vec<u64> {
        self.extension::<Joints>()
            .map_or_else(Vec::new, |t| t.joints.keys().copied().collect())
    }
    /// The two body IDs a live joint connects.
    pub fn joint_bodies(&self, id: u64) -> Option<(u64, u64)> {
        let record = self.extension::<Joints>()?.joints.get(&id)?;
        Some((record.first, record.second))
    }
    /// Edits a live joint's Rapier data in place and wakes its bodies.
    fn update_joint<R>(
        &mut self,
        id: u64,
        edit: impl FnOnce(&mut GenericJoint) -> Result<R>,
    ) -> Result<R> {
        let (core, table) = self.extension_mut::<Joints>();
        let record = table.joints.get(&id).ok_or(Error("unknown joint"))?;
        let (handle, first, second) = (record.handle, record.first, record.second);
        let joint = core
            .rapier
            .impulse_joints
            .get_mut(handle, true)
            .ok_or(Error("unknown joint"))?;
        let result = edit(&mut joint.data)?;
        for body in [first, second] {
            if let Ok(body) = core.body_mut(body) {
                body.wake_up(true);
            }
        }
        Ok(result)
    }
    fn read_joint<R>(&self, id: u64, read: impl FnOnce(&ImpulseJoint) -> R) -> Option<R> {
        let record = self.extension::<Joints>()?.joints.get(&id)?;
        self.core.rapier.impulse_joints.get(record.handle).map(read)
    }
    /// The `LinX` motor of a distance joint with a positive minimum is used internally.
    fn check_motor_free(&self, id: u64, dof: Dof) -> Result<()> {
        let reserved = self
            .extension::<Joints>()
            .and_then(|t| t.joints.get(&id))
            .is_some_and(|r| r.min_distance.is_some());
        if reserved && dof == Dof::LinX {
            return Err(Error(
                "distance joints with a minimum reserve the linear motor",
            ));
        }
        Ok(())
    }
    /// Replaces the motor on one degree of freedom (target, gains and force cap) without
    /// rebuilding the joint. Locked axes cannot be driven.
    pub fn set_joint_motor(&mut self, id: u64, dof: Dof, motor: MotorDesc) -> Result<()> {
        motor.validate()?;
        self.check_motor_free(id, dof)?;
        self.update_joint(id, |joint| {
            let axis = dof.axis();
            if joint.locked_axes.contains(axis.into()) {
                return Err(Error("cannot drive a locked joint axis"));
            }
            joint
                .set_motor(
                    axis,
                    motor.target_position,
                    motor.target_velocity,
                    motor.stiffness,
                    motor.damping,
                )
                .set_motor_max_force(axis, motor.max_force)
                .set_motor_model(axis, motor_model(motor.force_based));
            Ok(())
        })
    }
    /// Turns the motor on `dof` off.
    pub fn clear_joint_motor(&mut self, id: u64, dof: Dof) -> Result<()> {
        self.check_motor_free(id, dof)?;
        self.update_joint(id, |joint| {
            joint.motor_axes.remove(dof.axis().into());
            joint.motors[dof.index()] = Default::default();
            Ok(())
        })
    }
    /// The active motor on `dof`, if any.
    pub fn joint_motor(&self, id: u64, dof: Dof) -> Option<MotorDesc> {
        self.read_joint(id, |joint| {
            joint.data.motor(dof.axis()).map(|m| MotorDesc {
                target_position: m.target_pos,
                target_velocity: m.target_vel,
                stiffness: m.stiffness,
                damping: m.damping,
                max_force: m.max_force,
                force_based: m.model == MotorModel::ForceBased,
            })
        })
        .flatten()
    }
    /// Sets or clears (`None`) the limits on `dof`. Angular limits must lie within
    /// -pi..=pi; locked axes cannot be limited.
    pub fn set_joint_limits(&mut self, id: u64, dof: Dof, limits: Option<[f32; 2]>) -> Result<()> {
        if let Some([min, max]) = limits {
            AxisMode::Limited { min, max }.validate(dof.is_angular())?;
        }
        self.update_joint(id, |joint| {
            let axis = dof.axis();
            if joint.locked_axes.contains(axis.into()) {
                return Err(Error("cannot limit a locked joint axis"));
            }
            match limits {
                Some(limits) => {
                    joint.set_limits(axis, limits);
                }
                None => joint.limit_axes.remove(axis.into()),
            }
            Ok(())
        })
    }
    /// The limits on `dof`, if limited.
    pub fn joint_limits(&self, id: u64, dof: Dof) -> Option<[f32; 2]> {
        self.read_joint(id, |joint| {
            joint.data.limits(dof.axis()).map(|l| [l.min, l.max])
        })
        .flatten()
    }
    /// Replaces the break thresholds of a live joint.
    pub fn set_joint_break_limits(&mut self, id: u64, limits: BreakLimits) -> Result<()> {
        limits.validate()?;
        let (_, table) = self.extension_mut::<Joints>();
        table
            .joints
            .get_mut(&id)
            .ok_or(Error("unknown joint"))?
            .break_limits = limits;
        Ok(())
    }
    pub fn joint_break_limits(&self, id: u64) -> Option<BreakLimits> {
        Some(self.extension::<Joints>()?.joints.get(&id)?.break_limits)
    }
    /// Relative pose and applied force/torque of a live joint. Forces are those of the last
    /// step (zero before the first step).
    pub fn joint_state(&self, id: u64) -> Option<JointState> {
        let table = self.extension::<Joints>()?;
        let record = table.joints.get(&id)?;
        let joint = self.core.rapier.impulse_joints.get(record.handle)?;
        let body1 = self.core.rapier.bodies.get(joint.body1())?;
        let body2 = self.core.rapier.bodies.get(joint.body2())?;
        let (position, angles) = relative(
            *body1.position() * joint.data.local_frame1,
            *body2.position() * joint.data.local_frame2,
        );
        let (force, torque) = loads(&self.core, joint, table.inv_dt(&self.core));
        Some(JointState {
            position,
            angles,
            force,
            torque,
        })
    }
    /// The break record of a joint that broke (and was not removed or re-created since).
    pub fn broken_joint(&self, id: u64) -> Option<JointBroken> {
        self.extension::<Joints>()?.broken.get(&id).copied()
    }
    /// Broken joints in ascending ID order (bounded history).
    pub fn broken_joints(&self) -> Vec<JointBroken> {
        self.extension::<Joints>()
            .map_or_else(Vec::new, |t| t.broken.values().copied().collect())
    }
    /// Joints that broke since the last drain, oldest first. Storage is bounded; the oldest
    /// events are dropped if the host never drains.
    pub fn drain_joint_broken(&mut self) -> Vec<JointBroken> {
        let (_, table) = self.extension_mut::<Joints>();
        std::mem::take(&mut table.pending)
    }
}
