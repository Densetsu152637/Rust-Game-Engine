//! Feature status: what each physics render feature drew last frame and why it did not.
//! Nothing is silent (pattern: `UpscalerStatus`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// What a feature did in its last `encode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeatureActivity {
    /// Not run yet (no view rendered, or disabled in every view).
    #[default]
    NotRun,
    /// Nothing to draw (no objects, or an empty extract).
    Idle,
    /// Primitives were drawn.
    Drew,
    /// Could not run; see [`FeatureStatus::reason`].
    Skipped,
}

/// How surfaces were shaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShadingReport {
    #[default]
    Unknown,
    /// The engine light list through the view bind group.
    Lit,
    /// Configured fallback without lights (`SurfaceShading::NormalShaded`).
    NormalShaded,
}

/// One feature's last-frame report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureStatus {
    pub activity: FeatureActivity,
    /// Lines, vertices or particles uploaded.
    pub primitives: usize,
    /// Capacity of the primitive cap (elements).
    pub cap: usize,
    /// More primitives existed than `cap` (or than the GPU buffer cap).
    pub truncated: bool,
    /// Why the feature skipped or drew less than it was given.
    pub reason: Option<String>,
    pub shading: ShadingReport,
}

/// Status of all physics render features by feature name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhysicsRenderStatus {
    pub features: BTreeMap<&'static str, FeatureStatus>,
}
impl PhysicsRenderStatus {
    pub fn get(&self, name: &str) -> Option<&FeatureStatus> {
        self.features.get(name)
    }
}

/// Shared handle returned by [`crate::register_physics_features`]; clone and read from
/// anywhere (the features update it from the render thread).
#[derive(Clone, Debug, Default)]
pub struct PhysicsRenderStatusHandle(Arc<Mutex<PhysicsRenderStatus>>);
impl PhysicsRenderStatusHandle {
    pub fn snapshot(&self) -> PhysicsRenderStatus {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn set(&self, name: &'static str, status: FeatureStatus) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .features
            .insert(name, status);
    }
}
