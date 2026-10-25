//! BLAS/TLAS management: incremental updates, limits and statistics on real
//! hardware. Ignored by default; run with
//! `cargo test -p engine-render-rt --test scene_updates -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    ProbeRay, RtError, RtIndices, RtInstanceDesc, RtMeshData, RtScene, RtSceneLimits,
};

fn down_z(x: f32, y: f32) -> ProbeRay {
    ProbeRay::new([x, y, 0.0], [0.0, 0.0, 1.0], 0.0, 100.0, 0xff)
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn transform_and_mesh_edits_rebuild_only_what_changed() {
    let rt = open();
    let mut scene = scene(&rt);
    let quad = ground(1.0, 0.0);
    // The ground helper lies in y = 0; turn it to face the probe rays (z).
    let quad_xy: Vec<[f32; 3]> = quad.positions.iter().map(|p| [p[0], p[2], 0.0]).collect();
    let indices16: Vec<u16> = quad.indices.iter().map(|&i| i as u16).collect();
    let mesh = scene
        .add_mesh(&RtMeshData {
            positions: &quad_xy,
            indices: RtIndices::U16(&indices16),
            normals: None,
        })
        .unwrap();
    let id = scene
        .add_instance(&RtInstanceDesc {
            custom_index: 42,
            material_index: 9,
            ..instance(mesh, translation(0.0, 0.0, 5.0))
        })
        .unwrap();
    assert!(scene.is_dirty());
    let first = scene.commit().unwrap();
    println!("first commit: {first:?}");
    assert_eq!(first.blas_built, 1);
    assert!(first.tlas_built && first.geometry_repacked);
    assert!(!scene.is_dirty());
    let generation = scene.bind_group_generation();
    let hit = rt.cast_rays(&scene, &[down_z(0.3, 0.2)]).unwrap()[0];
    assert!(hit.hit() && (hit.t - 5.0).abs() < 1e-5);
    assert_eq!((hit.custom_index, hit.material_index), (42, 9));

    // Transform change: TLAS only.
    scene
        .set_instance_transform(id, translation(0.0, 0.0, 7.0))
        .unwrap();
    let moved = scene.commit().unwrap();
    println!("transform commit: {moved:?}");
    assert_eq!(moved.blas_built, 0);
    assert!(moved.tlas_built && !moved.geometry_repacked);
    assert_eq!(scene.bind_group_generation(), generation, "no reallocation");
    let hit = rt.cast_rays(&scene, &[down_z(0.3, 0.2)]).unwrap()[0];
    assert!((hit.t - 7.0).abs() < 1e-5, "t = {}", hit.t);

    // Same-size mesh change: that BLAS rebuilt in place, buffers rewritten.
    let raised: Vec<[f32; 3]> = quad_xy.iter().map(|p| [p[0], p[1], 1.0]).collect();
    scene
        .update_mesh(
            mesh,
            &RtMeshData {
                positions: &raised,
                indices: RtIndices::U16(&indices16),
                normals: None,
            },
        )
        .unwrap();
    let rebuilt = scene.commit().unwrap();
    println!("mesh commit: {rebuilt:?}");
    assert_eq!(rebuilt.blas_built, 1);
    assert!(rebuilt.tlas_built && !rebuilt.geometry_repacked);
    assert_eq!(rebuilt.build_input_bytes, 4 * 12 + 6 * 4 + 64);
    let hit = rt.cast_rays(&scene, &[down_z(0.3, 0.2)]).unwrap()[0];
    assert!((hit.t - 8.0).abs() < 1e-5, "t = {}", hit.t);

    // Resized mesh: repack.
    let cube = cuboid([-1.0, -1.0, 0.0], [1.0, 1.0, 2.0]);
    scene.update_mesh(mesh, &cube.data()).unwrap();
    let resized = scene.commit().unwrap();
    assert!(resized.geometry_repacked && resized.blas_built == 1);
    let hit = rt.cast_rays(&scene, &[down_z(0.3, 0.2)]).unwrap()[0];
    assert!((hit.t - 7.0).abs() < 1e-5, "t = {}", hit.t);

    // Uncommitted edits are refused, removal makes the ray miss.
    scene.remove_instance(id).unwrap();
    assert!(matches!(
        rt.cast_rays(&scene, &[down_z(0.3, 0.2)]),
        Err(RtError::Invalid(m)) if m.contains("uncommitted")
    ));
    scene.commit().unwrap();
    let hit = rt.cast_rays(&scene, &[down_z(0.3, 0.2)]).unwrap()[0];
    assert!(!hit.hit() && !hit.occluded());
    assert!(scene.remove_instance(id).is_err(), "stale handle");
    scene.remove_mesh(mesh).unwrap();
    assert_eq!(scene.stats().blas_count, 0);
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn many_instances_trace_and_report_statistics() {
    let rt = open();
    let mut scene = scene(&rt);
    let sphere = icosphere(2);
    let cube = cuboid([-0.4; 3], [0.4; 3]);
    let sphere_id = scene.add_mesh(&sphere.data()).unwrap();
    let cube_id = scene.add_mesh(&cube.data()).unwrap();
    // A 40 x 25 grid of alternating meshes at z = 10, 2 m apart.
    let mut ids = Vec::new();
    for i in 0..1000u32 {
        let (gx, gy) = (i % 40, i / 40);
        let mesh = if i % 2 == 0 { sphere_id } else { cube_id };
        ids.push(
            scene
                .add_instance(&RtInstanceDesc {
                    custom_index: i,
                    ..instance(mesh, translation(gx as f32 * 2.0, gy as f32 * 2.0, 10.0))
                })
                .unwrap(),
        );
    }
    let start = std::time::Instant::now();
    let commit = scene.commit().unwrap();
    let rays: Vec<ProbeRay> = (0..1000u32)
        .map(|i| down_z((i % 40) as f32 * 2.0 + 0.05, (i / 40) as f32 * 2.0 + 0.05))
        .collect();
    let hits = rt.cast_rays(&scene, &rays).unwrap();
    let elapsed = start.elapsed();
    let stats = scene.stats();
    println!("commit {commit:?}\nstats {stats:?}\ncommit + 1000 probes: {elapsed:?}");
    for (i, hit) in hits.iter().enumerate() {
        assert!(hit.hit(), "ray {i} missed");
        assert_eq!(hit.custom_index, i as u32);
        assert_eq!(hit.instance_index, ids[i].index());
        let expected_t = if i % 2 == 0 { 9.0 } else { 9.6 };
        // The icosphere is inscribed: its surface is at most 1 m from the centre.
        assert!(
            (hit.t - expected_t).abs() < if i % 2 == 0 { 0.03 } else { 1e-4 },
            "ray {i}: t = {}",
            hit.t
        );
    }
    assert_eq!(stats.blas_count, 2);
    assert_eq!(stats.instance_count, 1000);
    assert_eq!(stats.triangle_count, 320 + 12);
    assert_eq!(stats.instanced_triangle_count, 500 * 320 + 500 * 12);
    assert_eq!(stats.tlas_capacity, 1024);
    assert_eq!(
        stats.blas_input_bytes,
        sphere.positions.len() as u64 * 12 + 320 * 12 + 8 * 12 + 36 * 4
    );
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn limits_are_enforced_at_the_boundary() {
    let rt = open();
    let limits = RtSceneLimits {
        max_instances: 2,
        max_meshes: 1,
        ..RtSceneLimits::for_device(rt.limits())
    };
    let mut scene = RtScene::new(&rt, limits).unwrap();
    let cube = cuboid([-1.0; 3], [1.0; 3]);
    let mesh = scene.add_mesh(&cube.data()).unwrap();
    assert!(scene.add_mesh(&cube.data()).is_err(), "mesh limit");
    let a = scene
        .add_instance(&instance(mesh, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene
        .add_instance(&instance(mesh, translation(3.0, 0.0, 0.0)))
        .unwrap();
    let err = scene
        .add_instance(&instance(mesh, translation(6.0, 0.0, 0.0)))
        .unwrap_err();
    println!("third instance: {err}");
    assert!(err.to_string().contains("instances (the limit)"));
    assert!(
        scene
            .remove_mesh(mesh)
            .unwrap_err()
            .to_string()
            .contains("used by 2")
    );
    let mut bad = translation(0.0, 0.0, 0.0);
    bad[0] = f32::NAN;
    assert!(scene.set_instance_transform(a, bad).is_err());
    let too_big = RtSceneLimits {
        max_instances: u32::MAX,
        ..limits
    };
    assert!(RtScene::new(&rt, too_big).is_err());
}
