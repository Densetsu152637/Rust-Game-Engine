mod common;

use common::*;
use engine_mesh_lod::{HlodInstance, HlodOptions, MeshLodError, build_hlod};
use glam::{Mat4, Vec3};

fn group(meshes_index: usize, origin: Vec3, count: usize) -> Vec<HlodInstance> {
    (0..count)
        .map(|i| HlodInstance {
            mesh: meshes_index,
            transform: Mat4::from_translation(
                origin + Vec3::new(i as f32 * 3.0, 0.0, (i % 2) as f32 * 2.0),
            ),
        })
        .collect()
}

#[test]
fn two_distant_groups_make_two_clusters_with_bounded_proxies() {
    let meshes = vec![uv_sphere(8, 8, 1.0, Vec3::ZERO)];
    let mut instances = group(0, Vec3::ZERO, 5);
    instances.extend(group(0, Vec3::new(1000.0, 0.0, 0.0), 5));
    let opts = HlodOptions::default();
    let clusters = build_hlod(&meshes, &instances, &opts).unwrap();
    assert_eq!(clusters.len(), 2);
    assert_eq!(clusters[0].members, vec![0, 1, 2, 3, 4]);
    assert_eq!(clusters[1].members, vec![5, 6, 7, 8, 9]);
    for c in &clusters {
        let merged = 5 * meshes[0].triangle_count();
        assert_eq!(c.merged_triangles, merged);
        let target = ((merged as f64 * 0.1).ceil() as usize).min(opts.max_proxy_triangles);
        println!(
            "cluster: merged {merged} -> proxy {} (target {target}), error {:.4}, switch {:.1}",
            c.proxy.triangle_count(),
            c.proxy_error,
            c.switch_distance
        );
        assert!(c.proxy.triangle_count() <= target);
        assert!(c.proxy.triangle_count() < merged);
        c.proxy.validate().unwrap();
        assert!(c.switch_distance.is_finite() && c.switch_distance >= c.bounding_sphere.radius);
        // Proxy lives in world space inside the cluster bounds padded by its error.
        for p in &c.proxy.positions {
            assert!(
                p.cmpge(c.bounds.min - Vec3::splat(c.proxy_error + 1e-3))
                    .all()
            );
            assert!(
                p.cmple(c.bounds.max + Vec3::splat(c.proxy_error + 1e-3))
                    .all()
            );
        }
    }
    assert!(clusters[0].bounds.max.x < 100.0);
    assert!(clusters[1].bounds.min.x > 900.0);
}

#[test]
fn instance_count_limit_splits_clusters_and_covers_every_instance_once() {
    let meshes = vec![uv_sphere(6, 6, 0.5, Vec3::ZERO)];
    let instances = group(0, Vec3::ZERO, 9);
    let opts = HlodOptions {
        max_cluster_instances: 3,
        ..HlodOptions::default()
    };
    let clusters = build_hlod(&meshes, &instances, &opts).unwrap();
    assert!(clusters.len() >= 3);
    let mut all: Vec<usize> = clusters.iter().flat_map(|c| c.members.clone()).collect();
    all.sort_unstable();
    assert_eq!(all, (0..9).collect::<Vec<_>>());
    assert!(clusters.iter().all(|c| c.members.len() <= 3));
}

#[test]
fn extent_limit_splits_clusters() {
    let meshes = vec![uv_sphere(6, 6, 0.5, Vec3::ZERO)];
    let instances = group(0, Vec3::ZERO, 8); // spans ~24 units
    let opts = HlodOptions {
        max_cluster_extent: 6.0,
        ..HlodOptions::default()
    };
    let clusters = build_hlod(&meshes, &instances, &opts).unwrap();
    assert!(clusters.len() >= 4, "{}", clusters.len());
    assert!(
        clusters
            .iter()
            .all(|c| c.members.len() == 1 || c.bounds.max_extent() <= 6.0)
    );
}

#[test]
fn build_is_deterministic_and_validates_input() {
    let meshes = vec![uv_sphere(6, 6, 0.5, Vec3::ZERO)];
    let instances = group(0, Vec3::ZERO, 6);
    let a = build_hlod(&meshes, &instances, &HlodOptions::default()).unwrap();
    let b = build_hlod(&meshes, &instances, &HlodOptions::default()).unwrap();
    assert_eq!(a, b);

    let bad = [HlodInstance {
        mesh: 3,
        transform: Mat4::IDENTITY,
    }];
    assert!(matches!(
        build_hlod(&meshes, &bad, &HlodOptions::default()),
        Err(MeshLodError::InvalidOption { .. })
    ));
    let bad = [HlodInstance {
        mesh: 0,
        transform: Mat4::ZERO,
    }];
    assert!(build_hlod(&meshes, &bad, &HlodOptions::default()).is_err());
    let bad = [HlodInstance {
        mesh: 0,
        transform: Mat4::from_translation(Vec3::NAN),
    }];
    assert!(matches!(
        build_hlod(&meshes, &bad, &HlodOptions::default()),
        Err(MeshLodError::NonFiniteValue { .. })
    ));
    let opts = HlodOptions {
        proxy_ratio: 0.0,
        ..HlodOptions::default()
    };
    assert!(build_hlod(&meshes, &instances, &opts).is_err());
    assert!(
        build_hlod(&meshes, &[], &HlodOptions::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mirrored_instances_keep_outward_winding() {
    let meshes = vec![uv_sphere(8, 8, 1.0, Vec3::ZERO)];
    let instances = [HlodInstance {
        mesh: 0,
        transform: Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)),
    }];
    let opts = HlodOptions {
        proxy_ratio: 1.0,
        ..HlodOptions::default()
    };
    let clusters = build_hlod(&meshes, &instances, &opts).unwrap();
    let m = &clusters[0].proxy;
    for t in m.indices.as_chunks::<3>().0 {
        let (a, b, c) = (
            m.positions[t[0] as usize],
            m.positions[t[1] as usize],
            m.positions[t[2] as usize],
        );
        assert!((b - a).cross(c - a).dot((a + b + c) / 3.0) > 0.0);
    }
}
