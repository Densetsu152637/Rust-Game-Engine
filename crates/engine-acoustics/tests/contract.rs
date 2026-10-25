//! Frozen layout sizes and documented defaults.

use engine_acoustics::gpu_layout::{BvhNode, GpuInstance, GpuMaterial, GpuTriangle};
use engine_acoustics::*;

#[test]
fn gpu_layouts_have_the_frozen_sizes() {
    assert_eq!(size_of::<BvhNode>(), 32);
    assert_eq!(size_of::<GpuTriangle>(), 48);
    assert_eq!(size_of::<GpuInstance>(), 112);
    assert_eq!(size_of::<GpuMaterial>(), 48);
}

#[test]
fn defaults_match_the_documented_values() {
    assert_eq!(BackendPreference::default(), BackendPreference::Auto);
    let limits = WorldLimits::default();
    assert_eq!(
        (limits.max_mesh_triangles, limits.max_instances),
        (262_144, 16_384)
    );
    let settings = PropagationSettings::default();
    assert_eq!(
        (
            settings.occlusion_rays,
            settings.reverb_rays,
            settings.max_bounces
        ),
        (16, 4096, 64)
    );
    assert_eq!(AcousticStatus::default().state, AcousticState::Disabled);
}
