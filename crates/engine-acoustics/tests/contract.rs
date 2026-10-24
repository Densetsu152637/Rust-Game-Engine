//! The skeleton reports `acoustics.unimplemented` instead of fabricating results.

use engine_acoustics::gpu_layout::{BvhNode, GpuInstance, GpuMaterial, GpuTriangle};
use engine_acoustics::*;

#[test]
fn unimplemented_operations_return_structured_errors() {
    let mut builder = AcousticWorldBuilder::new(WorldLimits::default());
    let material = AcousticMaterial {
        absorption: [0.1; BANDS],
        scattering: 0.1,
        transmission: [0.0; BANDS],
    };
    assert_eq!(
        material.validate().unwrap_err().code,
        "acoustics.unimplemented"
    );
    assert_eq!(
        AcousticMaterial::preset(MaterialPreset::Concrete)
            .unwrap_err()
            .code,
        "acoustics.unimplemented"
    );
    assert_eq!(
        builder.add_material(material).unwrap_err().code,
        "acoustics.unimplemented"
    );
    assert_eq!(
        builder.build().err().unwrap().code,
        "acoustics.unimplemented"
    );
    assert_eq!(
        sabine_rt60(480.0, &[], [0.0; BANDS]).unwrap_err().code,
        "acoustics.unimplemented"
    );
}

#[test]
fn cpu_backend_validates_threads_and_reports_unimplemented() {
    assert!(CpuBackend::new(0).is_err() && CpuBackend::new(9).is_err());
    let mut cpu = CpuBackend::new(2).unwrap();
    assert_eq!(cpu.info().kind, BackendKind::Cpu);
    assert_eq!(
        cpu.initialize().unwrap_err().code,
        "acoustics.unimplemented"
    );
}

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
