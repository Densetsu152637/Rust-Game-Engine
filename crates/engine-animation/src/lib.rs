//! Validated skeletal clips, CPU pose evaluation, crossfades and skinning.
mod clip;
mod player;
mod skeleton;
use anyhow::{Result, ensure};
pub use clip::{Clip, ClipEvent, Interpolation, RotationTrack, Track, VectorTrack};
pub use glam::{Mat4, Quat, Vec3};
pub use player::{Animator, Playback};
pub use skeleton::{Skeleton, skin_vertices};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalPose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}
impl Default for LocalPose {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}
impl LocalPose {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.translation.is_finite() && self.scale.is_finite() && self.rotation.is_finite(),
            "nonfinite pose"
        );
        ensure!(
            (self.rotation.length_squared() - 1.).abs() < 0.001,
            "rotation must be normalized"
        );
        ensure!(self.scale.abs().min_element() > 1e-6, "singular scale");
        Ok(())
    }
    pub fn matrix(self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}
pub fn blend_poses(a: &[LocalPose], b: &[LocalPose], weight: f32) -> Result<Vec<LocalPose>> {
    ensure!(
        a.len() == b.len() && weight.is_finite() && (0.0..=1.0).contains(&weight),
        "invalid blend"
    );
    a.iter()
        .zip(b)
        .map(|(a, b)| {
            a.validate()?;
            b.validate()?;
            let pose = LocalPose {
                translation: a.translation.lerp(b.translation, weight),
                rotation: a.rotation.slerp(b.rotation, weight).normalize(),
                scale: a.scale.lerp(b.scale, weight),
            };
            pose.validate()?;
            Ok(pose)
        })
        .collect()
}
pub mod graph;

pub mod ik;

pub mod retarget;
