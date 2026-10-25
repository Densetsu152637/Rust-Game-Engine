//! Visual smoothing of prediction corrections.
use crate::{PredictError, Result};
use glam::{Quat, Vec3};

/// Decaying visual offset that hides reconciliation snaps.
///
/// The simulation always uses the corrected state; only the rendered pose adds the offset.
/// After a correction the offset makes the rendered pose continuous, then it decays
/// geometrically (half-life in ticks) to exactly zero once below `epsilon`.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorSmoother {
    translation: Vec3,
    rotation: Quat,
    retention: f32,
    snap_distance: f32,
    epsilon: f32,
}
impl ErrorSmoother {
    /// Corrections whose accumulated translation exceeds `snap_distance` are applied instantly
    /// (teleports); smaller ones decay with the given half-life.
    pub fn new(half_life_ticks: f32, snap_distance: f32) -> Result<Self> {
        if !half_life_ticks.is_finite() || half_life_ticks <= 0.0 {
            return Err(PredictError::Invalid("half-life must be finite and > 0"));
        }
        if !snap_distance.is_finite() || snap_distance <= 0.0 {
            return Err(PredictError::Invalid(
                "snap distance must be finite and > 0",
            ));
        }
        Ok(Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            retention: 0.5f32.powf(1.0 / half_life_ticks),
            snap_distance,
            epsilon: 1.0e-4,
        })
    }
    /// Offsets (metres, radians) below which the smoother snaps to zero; default 1e-4.
    pub fn with_epsilon(mut self, epsilon: f32) -> Result<Self> {
        if !epsilon.is_finite() || epsilon <= 0.0 {
            return Err(PredictError::Invalid("epsilon must be finite and > 0"));
        }
        self.epsilon = epsilon;
        Ok(self)
    }
    /// Per-tick multiplier applied to the offset.
    pub fn retention(&self) -> f32 {
        self.retention
    }
    /// Records a correction from the previously predicted pose to the corrected pose.
    pub fn add_correction(
        &mut self,
        old_translation: Vec3,
        old_rotation: Quat,
        new_translation: Vec3,
        new_rotation: Quat,
    ) {
        let translation = self.translation + (old_translation - new_translation);
        let rotation = (self.rotation * old_rotation * new_rotation.inverse()).normalize();
        if !translation.is_finite() || !rotation.is_finite() {
            self.reset();
            return;
        }
        if translation.length() > self.snap_distance {
            self.reset();
            return;
        }
        self.translation = translation;
        self.rotation = rotation;
    }
    /// Decays the offset by one tick.
    pub fn tick(&mut self) {
        self.translation *= self.retention;
        self.rotation = scale_rotation(self.rotation, self.retention);
        if self.translation.length() < self.epsilon {
            self.translation = Vec3::ZERO;
        }
        if self.rotation_error() < self.epsilon {
            self.rotation = Quat::IDENTITY;
        }
    }
    /// Rendered pose for a simulated pose.
    pub fn apply(&self, translation: Vec3, rotation: Quat) -> (Vec3, Quat) {
        (translation + self.translation, self.rotation * rotation)
    }
    pub fn translation_offset(&self) -> Vec3 {
        self.translation
    }
    pub fn rotation_offset(&self) -> Quat {
        self.rotation
    }
    /// Remaining rotation offset in radians.
    pub fn rotation_error(&self) -> f32 {
        rotation_angle(self.rotation)
    }
    pub fn is_settled(&self) -> bool {
        self.translation == Vec3::ZERO && self.rotation == Quat::IDENTITY
    }
    pub fn reset(&mut self) {
        self.translation = Vec3::ZERO;
        self.rotation = Quat::IDENTITY;
    }
}

/// Rotation angle via atan2 (precise for small angles, unlike acos of w).
fn rotation_angle(q: Quat) -> f32 {
    2.0 * q.xyz().length().atan2(q.w.abs())
}

/// Same axis, angle multiplied by `factor` (shortest arc).
fn scale_rotation(q: Quat, factor: f32) -> Quat {
    let q = if q.w < 0.0 { -q } else { q };
    let s = q.xyz().length();
    if s <= f32::EPSILON {
        return Quat::IDENTITY;
    }
    Quat::from_axis_angle(q.xyz() / s, rotation_angle(q) * factor)
}
