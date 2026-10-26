//! Explicit rest-relative rotation retargeting with optional local root translation.
use crate::{LocalPose, Mat4, Quat, Skeleton};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
pub const MAX_MAPPINGS: usize = 128;
fn identity() -> [f32; 4] {
    [0., 0., 0., 1.]
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointMapping {
    pub source: usize,
    pub target: usize,
    /// Maps source rest-joint motion axes into target rest-joint motion axes.
    #[serde(default = "identity")]
    pub basis: [f32; 4],
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RootTranslation {
    #[default]
    Discard,
    Scaled {
        source: usize,
        target: usize,
        scale: f32,
        #[serde(default = "identity")]
        alignment: [f32; 4],
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetargetDefinition {
    pub joints: Vec<JointMapping>,
    #[serde(default)]
    pub root: RootTranslation,
}
fn quaternion(value: [f32; 4]) -> Result<Quat> {
    let q = Quat::from_array(value);
    ensure!(
        q.is_finite() && (q.length_squared() - 1.).abs() < 0.001,
        "retarget basis must be a finite normalized quaternion"
    );
    Ok(q.normalize())
}
impl RetargetDefinition {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_MAPPINGS).contains(&self.joints.len()),
            "retarget map requires 1..128 mappings"
        );
        let mut sources = BTreeMap::new();
        let mut targets = BTreeMap::new();
        for joint in &self.joints {
            ensure!(
                joint.source < 4096
                    && joint.target < 4096
                    && sources.insert(joint.source, joint.target).is_none()
                    && targets.insert(joint.target, joint.source).is_none(),
                "invalid or repeated retarget joint index"
            );
            quaternion(joint.basis)?;
        }
        if let RootTranslation::Scaled {
            source,
            target,
            scale,
            alignment,
        } = &self.root
        {
            ensure!(
                scale.is_finite() && *scale > 0. && sources.get(source) == Some(target),
                "root translation requires a mapped pair and positive finite scale"
            );
            quaternion(*alignment)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
struct BoundJoint {
    source: usize,
    source_parent: Option<usize>,
    target_parent: Option<usize>,
    basis: Quat,
    source_rest: Quat,
    target_rest: Quat,
}
#[derive(Clone, Debug)]
struct RootBinding {
    source: usize,
    target: usize,
    scale: f32,
    alignment: Quat,
    source_parent: Mat4,
    target_parent_inverse: Mat4,
}
#[derive(Clone, Debug)]
pub struct RetargetMap {
    source: Arc<Skeleton>,
    target: Arc<Skeleton>,
    joints: Vec<BoundJoint>,
    by_target: Vec<Option<usize>>,
    source_relevant: Vec<bool>,
    root: Option<RootBinding>,
}
fn relevant(skeleton: &Skeleton, indices: impl Iterator<Item = usize>) -> Vec<bool> {
    let mut mask = vec![false; skeleton.joint_count()];
    for index in indices {
        let mut current = Some(index);
        while let Some(i) = current {
            if mask[i] {
                break;
            }
            mask[i] = true;
            current = skeleton.parents()[i];
        }
    }
    mask
}
fn frame(skeleton: &Skeleton, pose: &[LocalPose], relevant: &[bool]) -> Result<Vec<Mat4>> {
    ensure!(
        pose.len() == skeleton.joint_count(),
        "retarget pose/skeleton mismatch"
    );
    let globals = skeleton.global_matrices(pose)?;
    for (i, pose) in pose.iter().enumerate() {
        if relevant[i] {
            let scale = pose.scale;
            let determinant = globals[i].determinant();
            ensure!(
                scale.min_element() > 0.
                    && scale.max_element() - scale.min_element() <= scale.max_element() * 1e-5
                    && determinant.is_finite()
                    && determinant.abs() > 1e-12,
                "retarget mapped ancestry requires positive uniform nonsingular scale"
            );
        }
    }
    Ok(globals)
}
fn rotations(skeleton: &Skeleton, pose: &[LocalPose]) -> Vec<Quat> {
    let mut out = vec![Quat::IDENTITY; pose.len()];
    for &i in skeleton.joint_order() {
        out[i] = (skeleton.parents()[i].map_or(Quat::IDENTITY, |p| out[p]) * pose[i].rotation)
            .normalize();
    }
    out
}
fn nearest(skeleton: &Skeleton, index: usize, mapped: &BTreeMap<usize, usize>) -> Option<usize> {
    let mut current = skeleton.parents()[index];
    while let Some(parent) = current {
        if mapped.contains_key(&parent) {
            return Some(parent);
        }
        current = skeleton.parents()[parent];
    }
    None
}
impl RetargetMap {
    pub fn new(
        source: Arc<Skeleton>,
        target: Arc<Skeleton>,
        definition: &RetargetDefinition,
    ) -> Result<Self> {
        definition.validate()?;
        ensure!(
            definition
                .joints
                .iter()
                .all(|m| m.source < source.joint_count() && m.target < target.joint_count()),
            "retarget map index outside its rig"
        );
        let sources: BTreeMap<_, _> = definition
            .joints
            .iter()
            .map(|m| (m.source, m.target))
            .collect();
        let targets: BTreeMap<_, _> = definition
            .joints
            .iter()
            .map(|m| (m.target, m.source))
            .collect();
        let source_relevant = relevant(&source, sources.keys().copied());
        let target_relevant = relevant(&target, targets.keys().copied());
        let source_globals = frame(&source, source.rest_pose(), &source_relevant)?;
        let target_globals = frame(&target, target.rest_pose(), &target_relevant)?;
        let source_rotations = rotations(&source, source.rest_pose());
        let target_rotations = rotations(&target, target.rest_pose());
        let mut joints = Vec::new();
        let mut by_target = vec![None; target.joint_count()];
        for mapping in &definition.joints {
            let source_parent = nearest(&source, mapping.source, &sources);
            let target_parent = nearest(&target, mapping.target, &targets);
            ensure!(
                source_parent.map(|p| sources[&p]) == target_parent,
                "retarget mapped ancestor topology differs between rigs"
            );
            if let (Some(a), Some(b)) = (source_parent, target_parent) {
                let source_length = source_globals[mapping.source]
                    .w_axis
                    .truncate()
                    .as_dvec3()
                    .distance(source_globals[a].w_axis.truncate().as_dvec3());
                let target_length = target_globals[mapping.target]
                    .w_axis
                    .truncate()
                    .as_dvec3()
                    .distance(target_globals[b].w_axis.truncate().as_dvec3());
                ensure!(
                    source_length > 1e-6 && target_length > 1e-6,
                    "degenerate retarget mapped rest segment"
                );
            }
            let index = joints.len();
            by_target[mapping.target] = Some(index);
            joints.push(BoundJoint {
                source: mapping.source,
                source_parent,
                target_parent,
                basis: quaternion(mapping.basis)?,
                source_rest: (source_parent
                    .map_or(Quat::IDENTITY, |p| source_rotations[p])
                    .conjugate()
                    * source_rotations[mapping.source])
                    .normalize(),
                target_rest: (target_parent
                    .map_or(Quat::IDENTITY, |p| target_rotations[p])
                    .conjugate()
                    * target_rotations[mapping.target])
                    .normalize(),
            });
        }
        let root = if let RootTranslation::Scaled {
            source: from,
            target: to,
            scale,
            alignment,
        } = &definition.root
        {
            let mapped = &joints[by_target[*to].expect("validated root mapping")];
            ensure!(
                mapped.source_parent.is_none() && mapped.target_parent.is_none(),
                "root translation pair must be topmost mapped in both rigs"
            );
            let target_parent_inverse =
                target.parents()[*to].map_or(Mat4::IDENTITY, |p| target_globals[p].inverse());
            ensure!(
                target_parent_inverse.is_finite(),
                "retarget root parent inverse overflow"
            );
            Some(RootBinding {
                source: *from,
                target: *to,
                scale: *scale,
                alignment: quaternion(*alignment)?,
                source_parent: source.parents()[*from]
                    .map_or(Mat4::IDENTITY, |p| source_globals[p]),
                target_parent_inverse,
            })
        } else {
            None
        };
        Ok(Self {
            source,
            target,
            joints,
            by_target,
            source_relevant,
            root,
        })
    }
    pub fn source(&self) -> &Skeleton {
        &self.source
    }
    pub fn target(&self) -> &Skeleton {
        &self.target
    }
    /// Ignores source translations/scales except the explicitly selected root's local delta.
    /// Output translations/scales and all unmapped local rotations retain target rest values.
    pub fn apply(&self, source_pose: &[LocalPose]) -> Result<Vec<LocalPose>> {
        frame(&self.source, source_pose, &self.source_relevant)?;
        let source_rotations = rotations(&self.source, source_pose);
        let mut target_pose = self.target.rest_pose().to_vec();
        let mut target_rotations = vec![Quat::IDENTITY; target_pose.len()];
        if let Some(root) = &self.root {
            let delta = source_pose[root.source].translation
                - self.source.rest_pose()[root.source].translation;
            let delta = root.target_parent_inverse.transform_vector3(
                root.alignment * root.source_parent.transform_vector3(delta) * root.scale,
            );
            ensure!(delta.is_finite(), "retarget root translation overflow");
            target_pose[root.target].translation += delta;
        }
        for &i in self.target.joint_order() {
            let parent = self.target.parents()[i].map_or(Quat::IDENTITY, |p| target_rotations[p]);
            if let Some(index) = self.by_target[i] {
                let joint = &self.joints[index];
                let source_relative = (joint
                    .source_parent
                    .map_or(Quat::IDENTITY, |p| source_rotations[p])
                    .conjugate()
                    * source_rotations[joint.source])
                    .normalize();
                let motion = (joint.source_rest.conjugate() * source_relative).normalize();
                let relative = joint.target_rest * joint.basis * motion * joint.basis.conjugate();
                let global = joint
                    .target_parent
                    .map_or(Quat::IDENTITY, |p| target_rotations[p])
                    * relative;
                target_pose[i].rotation = (parent.conjugate() * global).normalize();
            }
            target_pose[i].validate()?;
            target_rotations[i] = (parent * target_pose[i].rotation).normalize();
        }
        self.target.global_matrices(&target_pose)?;
        Ok(target_pose)
    }
}
