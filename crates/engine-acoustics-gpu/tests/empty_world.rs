//! Worlds with nothing to hit are valid: every ray escapes, as on the CPU, and
//! the tier stays alive. Ignored by default; run with
//! `cargo test -p engine-acoustics-gpu --test empty_world -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_acoustics::{
    AcousticMesh, AcousticWorld, AcousticWorldBuilder, CpuBackend, Mobility, PropagationBackend,
    WorldLimits,
};
use engine_acoustics_gpu::{GpuBackend, GpuOptions};

fn empty_world() -> AcousticWorld {
    AcousticWorldBuilder::new(WorldLimits::default())
        .build()
        .expect("empty world")
}

/// One instance of a mesh whose only triangle is degenerate (dropped at build).
fn empty_mesh_world() -> AcousticWorld {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material(REFERENCE_ALPHA, 0.5)).unwrap();
    let mesh = b
        .add_mesh(AcousticMesh {
            positions: vec![[1.0, 1.0, 1.0]; 3],
            triangles: vec![[0, 1, 2]],
            materials: vec![m],
        })
        .unwrap();
    b.add_instance(mesh, IDENTITY, Mobility::Dynamic, 0xff)
        .unwrap();
    b.build().expect("world of empty meshes")
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn empty_worlds_escape_everything_and_keep_the_tier_alive() {
    let settings = settings(1.0);
    let jobs = [job([1.0, 2.0, 3.0], [4.0, 2.0, 1.0], 4096, 64, 9)];
    let mut cpu = CpuBackend::new(1).unwrap();
    for make in [GpuBackend::ray_query, GpuBackend::compute] {
        let mut backend = make(GpuOptions::default());
        backend.initialize().expect("tier");
        for (label, world) in [
            ("empty", empty_world()),
            ("empty meshes", empty_mesh_world()),
        ] {
            println!("{label}: {:?}", world.stats());
            backend.sync_world(&world).expect("sync an empty world");
            let mut out = Vec::new();
            let stats = backend
                .trace(&world, &settings, &jobs, &mut out)
                .expect("trace an empty world");
            let mut reference = Vec::new();
            cpu.trace(&world, &settings, &jobs, &mut reference).unwrap();
            println!(
                "{:?} {label}: escaped {} (CPU {}), segments {}",
                backend.info().kind,
                out[0].escaped,
                reference[0].escaped,
                stats.segments
            );
            assert_eq!(out, reference, "{label}: GPU and CPU disagree");
            assert_eq!(out[0].escaped, 1.0);
            assert_eq!(stats.segments, u64::from(jobs[0].rays));
            assert_eq!(
                backend
                    .raycast_batch(&world, &[([0.0; 3], [1.0, 0.0, 0.0], 1e6)])
                    .unwrap(),
                vec![None]
            );
            assert!(backend.info().adapter.is_some(), "{label}: the tier died");
        }
        // A real world afterwards still uploads and traces.
        let world = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
        let mut out = Vec::new();
        backend.sync_world(&world).expect("sync");
        backend.trace(&world, &settings, &jobs, &mut out).unwrap();
        assert!(out[0].escaped < 1e-3 && out[0].mean_free_path_m > 4.0);
    }
}
