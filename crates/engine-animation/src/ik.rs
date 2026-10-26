//! Rotation-only two-bone inverse kinematics in skeleton coordinates.
use crate::{LocalPose, Quat, Skeleton, Vec3};
use anyhow::{Result, ensure};
use glam::DVec3;

/// Bend angle between the two bone directions: zero is straight, PI is folded.
#[derive(Clone, Copy, Debug)]
pub struct BendLimits {
    min: f32,
    max: f32,
}
impl BendLimits {
    pub fn new(min: f32, max: f32) -> Result<Self> {
        ensure!(
            min.is_finite()
                && max.is_finite()
                && 0. <= min
                && min <= max
                && max <= std::f32::consts::PI,
            "invalid IK bend limits"
        );
        Ok(Self { min, max })
    }
}
impl Default for BendLimits {
    fn default() -> Self {
        Self {
            min: 0.,
            max: std::f32::consts::PI,
        }
    }
}
/// A direct root -> middle -> end chain validated against a Skeleton.
#[derive(Clone, Debug)]
pub struct TwoBone {
    chain: [usize; 3],
    limits: BendLimits,
}
impl TwoBone {
    pub fn new(skeleton: &Skeleton, chain: [usize; 3], limits: BendLimits) -> Result<Self> {
        let solver = Self { chain, limits };
        solver.topology(skeleton)?;
        solver.geometry(skeleton, skeleton.rest_pose())?;
        Ok(solver)
    }
    fn topology(&self, skeleton: &Skeleton) -> Result<()> {
        let [root, mid, end] = self.chain;
        ensure!(
            self.chain.iter().all(|i| *i < skeleton.joint_count())
                && root != mid
                && mid != end
                && root != end,
            "invalid IK joint indices"
        );
        ensure!(
            skeleton.parents()[mid] == Some(root) && skeleton.parents()[end] == Some(mid),
            "IK requires a direct root -> middle -> end chain"
        );
        Ok(())
    }
    fn geometry(&self, skeleton: &Skeleton, pose: &[LocalPose]) -> Result<[DVec3; 3]> {
        self.topology(skeleton)?;
        ensure!(
            pose.len() == skeleton.joint_count(),
            "IK pose/skeleton mismatch"
        );
        // Uniform positive scale avoids shear and makes rotation-only length preservation exact.
        let mut current = Some(self.chain[2]);
        while let Some(i) = current {
            let scale = pose[i].scale;
            ensure!(
                scale.is_finite()
                    && scale.min_element() > 0.
                    && (scale.max_element() - scale.min_element()) <= scale.max_element() * 1e-5,
                "IK requires positive uniform scale on the chain and its ancestors"
            );
            current = skeleton.parents()[i];
        }
        let globals = skeleton.global_matrices(pose)?;
        let positions = self
            .chain
            .map(|i| globals[i].transform_point3(Vec3::ZERO).as_dvec3());
        let a = positions[1].distance(positions[0]);
        let b = positions[2].distance(positions[1]);
        ensure!(
            a.is_finite() && b.is_finite() && a > 1e-6 && b > 1e-6,
            "degenerate IK bone length"
        );
        Ok(positions)
    }
    /// Returns a new pose; errors never mutate the input. Target and pole are points in
    /// skeleton space. Unreachable targets clamp to the reachable interval and bend limits.
    /// A zero weight disables directional solving, while still validating the rig and pose.
    pub fn solve(
        &self,
        skeleton: &Skeleton,
        pose: &[LocalPose],
        target: Vec3,
        pole: Vec3,
        weight: f32,
    ) -> Result<Vec<LocalPose>> {
        ensure!(
            target.is_finite()
                && pole.is_finite()
                && weight.is_finite()
                && (0.0..=1.).contains(&weight),
            "invalid IK target, pole or weight"
        );
        let [r, m, e] = self.geometry(skeleton, pose)?;
        if weight == 0. {
            return Ok(pose.to_vec());
        }
        let target = target.as_dvec3();
        let pole = pole.as_dvec3();
        let direction = target - r;
        let distance = direction.length();
        ensure!(
            distance.is_finite() && distance > 1e-6,
            "IK target coincides with chain root"
        );
        let direction = direction / distance;
        let pole_direction = pole - r - direction * (pole - r).dot(direction);
        ensure!(
            pole_direction.length() > 1e-6,
            "IK pole is collinear with the target direction"
        );
        let pole_direction = pole_direction.normalize();
        let a = m.distance(r);
        let b = e.distance(m);
        let cosine = ((distance * distance - a * a - b * b) / (2. * a * b)).clamp(-1., 1.);
        let bend = cosine.acos().clamp(
            f64::from(self.limits.min).min(std::f64::consts::PI),
            f64::from(self.limits.max).min(std::f64::consts::PI),
        );
        let reach = (a * a + b * b + 2. * a * b * bend.cos()).max(0.).sqrt();
        ensure!(
            reach > 1e-6,
            "IK bend constraints collapse the end onto its root"
        );
        let along = (a * a - b * b + reach * reach) / (2. * reach);
        let height = (a * a - along * along).max(0.).sqrt();
        let desired_mid = r + direction * along + pole_direction * height;
        let desired_end = r + direction * reach;
        let mut solved = pose.to_vec();
        rotate_to(
            skeleton,
            &mut solved,
            self.chain[0],
            m - r,
            desired_mid - r,
            direction.cross(pole_direction),
        )?;
        let globals = skeleton.global_matrices(&solved)?;
        let mid = globals[self.chain[1]]
            .transform_point3(Vec3::ZERO)
            .as_dvec3();
        let end = globals[self.chain[2]]
            .transform_point3(Vec3::ZERO)
            .as_dvec3();
        rotate_to(
            skeleton,
            &mut solved,
            self.chain[1],
            end - mid,
            desired_end - mid,
            direction.cross(pole_direction),
        )?;
        for index in [self.chain[0], self.chain[1]] {
            solved[index].rotation = pose[index]
                .rotation
                .slerp(solved[index].rotation, weight)
                .normalize();
        }
        let result = self.geometry(skeleton, &solved)?;
        for (before, after) in [
            (a, result[1].distance(result[0])),
            (b, result[2].distance(result[1])),
        ] {
            ensure!(
                (before - after).abs() <= before * 2e-4 + 1e-6,
                "IK failed to preserve bone length"
            );
        }
        if weight == 1. {
            let tolerance = (a + b) * 2e-4 + 1e-6;
            ensure!(
                result[1].distance(desired_mid) <= tolerance
                    && result[2].distance(desired_end) <= tolerance,
                "IK failed to reach its constrained pose"
            );
        }
        Ok(solved)
    }
}
fn rotation(skeleton: &Skeleton, pose: &[LocalPose], joint: usize) -> Quat {
    let mut result = pose[joint].rotation;
    let mut current = skeleton.parents()[joint];
    while let Some(parent) = current {
        result = pose[parent].rotation * result;
        current = skeleton.parents()[parent];
    }
    result.normalize()
}
fn rotate_to(
    skeleton: &Skeleton,
    pose: &mut [LocalPose],
    joint: usize,
    from: DVec3,
    to: DVec3,
    plane_normal: DVec3,
) -> Result<()> {
    ensure!(
        from.length() > 1e-6 && to.length() > 1e-6,
        "degenerate IK rotation direction"
    );
    let from = from.normalize();
    let to = to.normalize();
    let cross = from.cross(to);
    let sine = cross.length();
    let cosine = from.dot(to).clamp(-1., 1.);
    let delta = if sine > 1e-8 {
        Quat::from_axis_angle((cross / sine).as_vec3(), sine.atan2(cosine) as f32)
    } else if cosine < 0. {
        // Antiparallel directions use the authored pole plane rather than an invented axis.
        let axis = plane_normal - from * plane_normal.dot(from);
        ensure!(axis.length() > 1e-8, "degenerate IK rotation plane");
        Quat::from_axis_angle(axis.normalize().as_vec3(), std::f32::consts::PI)
    } else {
        Quat::IDENTITY
    };
    let parent =
        skeleton.parents()[joint].map_or(Quat::IDENTITY, |parent| rotation(skeleton, pose, parent));
    pose[joint].rotation =
        (parent.conjugate() * delta * rotation(skeleton, pose, joint)).normalize();
    pose[joint].validate()
}
