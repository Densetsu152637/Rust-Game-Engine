//! Render features that draw the physics module's output through the render extension API
//! (`engine-render-pipeline`): the debug view as lines, cloth and soft-body surfaces, particle
//! fluids as sphere impostors and destruction fragments. See `docs/physics-rendering.md`.
//!
//! ```ignore
//! // Game setup, before the runner starts:
//! install_physics_extraction(runtime.app_mut(), PhysicsRenderConfig::default())?;
//! let mut features = PipelineFeatures::default();
//! let status = register_physics_features(&mut features)?;
//! runtime.app_mut().world_mut().insert_resource(features);
//! ```

mod buffers;
mod config;
mod extract;
mod fluid;
mod gpu;
mod lines;
mod status;
mod surface;

pub use buffers::{GrowBuffer, MIN_BUFFER_BYTES, grown_capacity};
pub use config::{
    FluidColorMode, FluidStyle, MAX_DEBUG_SEGMENTS, MAX_FLUID_PARTICLES, MAX_FRAGMENTS,
    MAX_SURFACE_VERTICES, PhysicsRenderConfig, PhysicsRenderToggles, SurfaceShading,
};
pub use extract::{
    DebugState, FragmentDraw, PhysicsRenderExtract, build_extract, extract_physics_render,
    install_physics_extraction, transform_matrix,
};
pub use fluid::{FluidFeature, ParticleBatch, ParticleInstance, build_particles, particle_color};
pub use lines::{DebugLinesFeature, LineBatch, LineInstance, build_lines};
pub use status::{
    FeatureActivity, FeatureStatus, PhysicsRenderStatus, PhysicsRenderStatusHandle, ShadingReport,
};
pub use surface::{
    IndexedObject, SurfaceBatch, SurfaceFeature, SurfaceKind, build_fragments, build_indexed,
};

use anyhow::Result;
use engine_render_pipeline::PipelineFeatures;

/// Feature names registered by [`register_physics_features`].
pub mod names {
    pub use crate::fluid::NAME as FLUID;
    pub use crate::lines::NAME as DEBUG_LINES;
    pub use crate::surface::{CLOTH, FRAGMENTS, SOFT_BODY};
    /// All five, in registration order.
    pub const ALL: [&str; 5] = [DEBUG_LINES, CLOTH, SOFT_BODY, FRAGMENTS, FLUID];
}

/// Registers the five physics features with the default [`PhysicsRenderConfig`].
pub fn register_physics_features(
    features: &mut PipelineFeatures,
) -> Result<PhysicsRenderStatusHandle> {
    register_physics_features_with(features, &PhysicsRenderConfig::default())
}

/// Registers `physics.debug_lines`, `physics.cloth`, `physics.soft_body`, `physics.fragments`
/// and `physics.fluid`. A feature whose flag in `config.enabled` is off is registered disabled
/// by default (cameras can still enable it with `Camera3D.render_features`). Use the same
/// `config` for [`install_physics_extraction`]. The returned handle reports what each feature
/// drew and why it did not.
pub fn register_physics_features_with(
    features: &mut PipelineFeatures,
    config: &PhysicsRenderConfig,
) -> Result<PhysicsRenderStatusHandle> {
    config.validate()?;
    let status = PhysicsRenderStatusHandle::default();
    features.register(DebugLinesFeature::new(config.clone(), status.clone()))?;
    for kind in [
        SurfaceKind::Cloth,
        SurfaceKind::SoftBody,
        SurfaceKind::Fragments,
    ] {
        features.register(SurfaceFeature::new(kind, config.clone(), status.clone()))?;
    }
    features.register(FluidFeature::new(config.clone(), status.clone()))?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_adds_five_named_features_and_rejects_a_second_set() {
        let mut features = PipelineFeatures::default();
        register_physics_features(&mut features).unwrap();
        let registered = features.0.names();
        for name in names::ALL {
            assert!(registered.contains(&name), "{name} registered");
        }
        assert!(register_physics_features(&mut features).is_err());
    }

    #[test]
    fn disabled_flags_register_features_disabled_by_default() {
        let mut features = PipelineFeatures::default();
        let mut config = PhysicsRenderConfig::default();
        config.enabled.fluid = false;
        config.enabled.debug_lines = false;
        register_physics_features_with(&mut features, &config).unwrap();
        let descriptors: Vec<_> = features
            .0
            .descriptors()
            .filter(|d| d.name.starts_with("physics."))
            .map(|d| (d.name.clone(), d.enabled_by_default))
            .collect();
        assert_eq!(descriptors.len(), 5);
        for (name, enabled) in descriptors {
            assert_eq!(
                enabled,
                name != names::FLUID && name != names::DEBUG_LINES,
                "{name}"
            );
        }
    }

    #[test]
    fn invalid_config_is_rejected_before_registering() {
        let mut features = PipelineFeatures::default();
        let config = PhysicsRenderConfig {
            max_fragments: 0,
            ..PhysicsRenderConfig::default()
        };
        assert!(register_physics_features_with(&mut features, &config).is_err());
        assert!(!features.0.names().contains(&names::CLOTH));
    }
}
