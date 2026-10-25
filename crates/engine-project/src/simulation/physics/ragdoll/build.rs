//! Pure geometry: bone layout (collider, joint and joint frame) from the rest pose, and the
//! joint-angle targets that make a powered ragdoll follow an animation pose.
//!
//! Conventions. Every body is built with identity orientation at rest, so the body frame is
//! world aligned and `bone_rotation = body_rotation * offset`. A body's origin is its bone's
//! joint position. Joint frames are expressed in body frames, identical on both sides.
use super::component::{BoneJoint, BoneShape, Ragdoll, RagdollBone};
use crate::Result;
use crate::simulation::invalid;
use dim3::Dof;
use engine_physics::{AxisMode, dim3};
use glam::{Quat, Vec3};
use std::f32::consts::PI;

/// A bone's world pose in the rest configuration.
pub(super) struct Rest {
    pub parent: Option<usize>,
    pub position: Vec3,
    pub rotation: Quat,
}
/// Static description of one bone derived from the rest pose.
pub(super) struct Layout {
    pub parent: Option<usize>,
    /// `bone world rotation = body rotation * offset`.
    pub offset: Quat,
    pub direction: Vec3,
    pub length: f32,
    /// Collider center in the body frame.
    pub center: Vec3,
    pub shape: dim3::Shape,
    pub density: f32,
    pub mass: f32,
    /// Joint frame orientation (body frame, shared by both sides).
    pub frame: Quat,
    pub kind: BoneJoint,
    pub swing_limit: f32,
    pub twist_limits: [f32; 2],
    pub hinge_limits: [f32; 2],
    pub angular_limits: [[f32; 2]; 3],
    pub joint: Option<dim3::JointDesc>,
}
/// Euler Y/Z limits stay below the gimbal singularity at pi/2.
const MAX_SWING: f32 = 1.5;
fn arc(from: Vec3, to: Vec3) -> Quat {
    Quat::from_rotation_arc(from, to).normalize()
}
fn volume(shape: &dim3::Shape) -> f32 {
    let sphere = |r: f32| 4. / 3. * PI * r * r * r;
    match shape {
        dim3::Shape::Box { half_extents } => 8. * half_extents.iter().product::<f32>(),
        dim3::Shape::Ball { radius } => sphere(*radius),
        dim3::Shape::Capsule {
            half_height,
            radius,
        } => PI * radius * radius * 2. * half_height + sphere(*radius),
        _ => 0.,
    }
}
fn shape(config: &RagdollBone, length: f32) -> dim3::Shape {
    let radius = config
        .radius
        .unwrap_or_else(|| (0.2 * length).clamp(0.02, 0.12));
    match config.shape {
        BoneShape::Capsule => dim3::Shape::Capsule {
            half_height: (length * 0.5 - radius).max(0.005),
            radius,
        },
        BoneShape::Box => dim3::Shape::Box {
            half_extents: config
                .half_extents
                .unwrap_or([radius, length * 0.5, radius]),
        },
        BoneShape::Ball => dim3::Shape::Ball {
            radius: config.radius.unwrap_or(length * 0.5),
        },
    }
}
/// Layouts in bone order. `order` lists bone indices parents first.
pub(super) fn layouts(
    config: &Ragdoll,
    rest: &[Rest],
    order: &[usize],
) -> Result<Vec<Option<Layout>>> {
    let mut result: Vec<Option<Layout>> = (0..rest.len()).map(|_| None).collect();
    for &i in order {
        let bone = &config.bones[i];
        let here = &rest[i];
        let far = rest
            .iter()
            .enumerate()
            .filter(|(_, r)| r.parent == Some(i))
            .map(|(_, r)| r.position - here.position)
            .filter(|d| d.length() > 1e-5)
            .fold(None::<Vec3>, |best, d| match best {
                Some(b) if b.length() >= d.length() => Some(b),
                _ => Some(d),
            });
        let parent = here.parent.and_then(|p| result[p].as_ref());
        let direction = if let Some(d) = bone.direction {
            (here.rotation * Vec3::from(d)).normalize()
        } else if let Some(d) = far {
            d.normalize()
        } else {
            parent.map_or(Vec3::Y, |p| p.direction)
        };
        let length = bone.length.unwrap_or_else(|| {
            far.map_or_else(
                || parent.map_or(0.2, |p| (0.5 * p.length).clamp(0.08, 1.0)),
                Vec3::length,
            )
        });
        let shape = shape(bone, length);
        let volume = volume(&shape);
        if !(volume.is_finite() && volume > 1e-9) {
            return Err(invalid(format!(
                "Ragdoll bone {} has a degenerate collider",
                bone.bone
            )));
        }
        let density = bone
            .mass
            .map(|m| m / volume)
            .or(bone.density)
            .unwrap_or(config.density);
        let anchor = here
            .parent
            .map_or(Vec3::ZERO, |p| here.position - rest[p].position);
        let (frame, joint) = joint_desc(bone, here, direction, anchor);
        result[i] = Some(Layout {
            parent: here.parent,
            offset: here.rotation,
            direction,
            length,
            center: direction * (length * 0.5),
            shape,
            density,
            mass: density * volume,
            frame,
            kind: bone.joint,
            swing_limit: bone.swing_limit.min(MAX_SWING),
            twist_limits: bone.twist_limits,
            hinge_limits: bone.hinge_limits,
            angular_limits: bone.angular_limits,
            joint: here.parent.map(|_| joint),
        });
    }
    Ok(result)
}
fn joint_desc(
    bone: &RagdollBone,
    here: &Rest,
    direction: Vec3,
    anchor: Vec3,
) -> (Quat, dim3::JointDesc) {
    let a = anchor.to_array();
    let b = [0.; 3];
    let limited = |l: [f32; 2]| AxisMode::Limited {
        min: l[0],
        max: l[1],
    };
    let generic = |frame: Quat, angular: [AxisMode; 3]| {
        let at = |position: [f32; 3]| dim3::Frame {
            position,
            rotation: frame.to_array(),
        };
        dim3::JointDesc::Generic {
            frame1: at(a),
            frame2: at(b),
            axes: [
                AxisMode::Locked,
                AxisMode::Locked,
                AxisMode::Locked,
                angular[0],
                angular[1],
                angular[2],
            ],
        }
    };
    match bone.joint {
        BoneJoint::Fixed => (
            Quat::IDENTITY,
            dim3::JointDesc::Fixed {
                anchor1: a,
                anchor2: b,
            },
        ),
        BoneJoint::Spherical => {
            // Rapier ignores motors on the coupled axes of a cone, so the swing is two
            // independent limits about the frame's Y and Z axes (twist about X).
            let swing = bone.swing_limit.min(MAX_SWING);
            let frame = arc(Vec3::X, direction);
            (
                frame,
                generic(
                    frame,
                    [
                        limited(bone.twist_limits),
                        limited([-swing, swing]),
                        limited([-swing, swing]),
                    ],
                ),
            )
        }
        BoneJoint::Hinge => {
            let axis = (here.rotation * Vec3::from(bone.axis)).normalize();
            (
                arc(Vec3::X, axis),
                dim3::JointDesc::Revolute {
                    anchor1: a,
                    anchor2: b,
                    axis: axis.to_array(),
                    limits: Some(bone.hinge_limits),
                },
            )
        }
        BoneJoint::Generic => (
            here.rotation,
            generic(
                here.rotation,
                [
                    limited(bone.angular_limits[0]),
                    limited(bone.angular_limits[1]),
                    limited(bone.angular_limits[2]),
                ],
            ),
        ),
    }
}
/// XYZ intrinsic Euler angles of `q` (`R = Rx(a) * Ry(b) * Rz(c)`), the convention of
/// engine-physics `JointState::angles`.
pub(super) fn euler_xyz(q: Quat) -> [f32; 3] {
    let [x, y, z, w] = q.to_array();
    let sin_b = (2. * (x * z + y * w)).clamp(-1., 1.);
    let a = (-2. * (y * z - x * w)).atan2(1. - 2. * (x * x + y * y));
    let c = (-2. * (x * y - z * w)).atan2(1. - 2. * (y * y + z * z));
    [a, sin_b.asin(), c]
}
/// Angle in radians of the rotation taking `a` to `b`.
pub(super) fn angle_between(a: Quat, b: Quat) -> f32 {
    2. * a.dot(b).abs().min(1.).acos()
}
/// Angular velocity (rad/s) that rotates `from` into `to` in `dt` seconds.
pub(super) fn angular_velocity(from: Quat, to: Quat, dt: f32) -> Vec3 {
    let (axis, angle) = (to * from.inverse()).normalize().to_axis_angle();
    let angle = if angle > PI { angle - 2. * PI } else { angle };
    if axis.is_finite() {
        axis * (angle / dt)
    } else {
        Vec3::ZERO
    }
}
/// Position-motor targets that drive a joint to the relative rotation `relative` (child body
/// rotation in the parent body frame). Targets respect the joint limits.
pub(super) fn motor_targets(layout: &Layout, relative: Quat) -> Vec<(Dof, f32)> {
    let [x, y, z] = euler_xyz(layout.frame.inverse() * relative * layout.frame);
    let clamp = |v: f32, l: [f32; 2]| v.clamp(l[0], l[1]);
    let swing = [-layout.swing_limit, layout.swing_limit];
    match layout.kind {
        BoneJoint::Fixed => vec![],
        BoneJoint::Hinge => vec![(Dof::AngX, clamp(x, layout.hinge_limits))],
        BoneJoint::Spherical => vec![
            (Dof::AngX, clamp(x, layout.twist_limits)),
            (Dof::AngY, clamp(y, swing)),
            (Dof::AngZ, clamp(z, swing)),
        ],
        BoneJoint::Generic => vec![
            (Dof::AngX, clamp(x, layout.angular_limits[0])),
            (Dof::AngY, clamp(y, layout.angular_limits[1])),
            (Dof::AngZ, clamp(z, layout.angular_limits[2])),
        ],
    }
}
