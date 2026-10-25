mod common;

use common::*;
use engine_mesh_lod::{
    BoundingSphere, LodSelectionParams, LodTarget, MeshLodError, ScreenSizeLods, SimplifyOptions,
    build_lod_chain, projected_error_pixels, screen_height_fraction, select_lod,
};
use glam::Vec3;

fn params(hysteresis: f32) -> LodSelectionParams {
    LodSelectionParams {
        vertical_fov: 60f32.to_radians(),
        viewport_height: 1080.0,
        pixel_threshold: 2.0,
        hysteresis,
    }
}

#[test]
fn chain_has_decreasing_triangles_and_nondecreasing_error() {
    let mesh = uv_sphere(40, 40, 1.0, Vec3::ZERO);
    let chain = build_lod_chain(
        &mesh,
        &[
            LodTarget::Ratio(0.5),
            LodTarget::Ratio(0.2),
            LodTarget::Triangles(100),
            LodTarget::Error(0.2),
        ],
        &SimplifyOptions::default(),
    )
    .unwrap();
    for l in &chain.levels {
        println!("level: {} tris, error {:.5}", l.triangle_count, l.error);
    }
    assert_eq!(chain.levels.len(), 5);
    assert_eq!(chain.levels[0].error, 0.0);
    assert_eq!(chain.levels[0].triangle_count, mesh.triangle_count());
    for w in chain.levels.windows(2) {
        assert!(w[1].triangle_count <= w[0].triangle_count);
        assert!(w[1].error >= w[0].error);
    }
    assert!(chain.levels[4].triangle_count < chain.levels[1].triangle_count);
    assert!((chain.bounds.radius - 1.0).abs() < 0.05);
}

#[test]
fn chain_rejects_bad_ratio() {
    let mesh = plane(2, 1.0);
    for r in [0.0, -1.0, 1.5, f32::NAN] {
        assert!(matches!(
            build_lod_chain(&mesh, &[LodTarget::Ratio(r)], &SimplifyOptions::default()),
            Err(MeshLodError::InvalidOption { .. })
        ));
    }
}

#[test]
fn projected_error_matches_the_pinhole_formula() {
    // 1 unit at 10 units with 90 deg FOV in a 1000 px viewport: 1000 / (2 * 10 * 1) = 50 px.
    let px = projected_error_pixels(1.0, 10.0, 90f32.to_radians(), 1000.0);
    assert!((px - 50.0).abs() < 1e-3, "{px}");
    // Sphere of radius 1 at distance 10 covers 1 / (10 * tan 45) = 0.1 of the height.
    let s = BoundingSphere {
        center: Vec3::ZERO,
        radius: 1.0,
    };
    assert!((screen_height_fraction(&s, 10.0, 90f32.to_radians()) - 0.1).abs() < 1e-6);
}

#[test]
fn selection_is_monotonic_in_distance() {
    let errors = [0.0, 0.01, 0.05, 0.2, 0.8];
    let sphere = BoundingSphere {
        center: Vec3::ZERO,
        radius: 1.0,
    };
    let p = params(0.0);
    let mut last = 0;
    let mut seen = [false; 5];
    let mut d = 1.01f32;
    while d < 20_000.0 {
        let level = select_lod(&errors, &sphere, d, &p, None);
        assert!(
            level >= last,
            "level dropped from {last} to {level} at distance {d}"
        );
        seen[level] = true;
        last = level;
        d *= 1.01;
    }
    assert!(seen.iter().all(|s| *s), "all levels reachable: {seen:?}");
    // Very close to the camera must use the finest level.
    assert_eq!(select_lod(&errors, &sphere, 1.001, &p, None), 0);
}

#[test]
fn hysteresis_stops_oscillation_at_a_threshold() {
    let errors = [0.0, 0.05];
    let sphere = BoundingSphere {
        center: Vec3::ZERO,
        radius: 1.0,
    };
    let none = params(0.0);
    // Find the switch distance for level 1 without hysteresis.
    let (mut lo, mut hi) = (1.5f32, 10_000.0f32);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if select_lod(&errors, &sphere, mid, &none, None) == 1 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let switch = hi;
    let wobble = |p: &LodSelectionParams| {
        let mut prev = select_lod(&errors, &sphere, switch * 1.02, p, None);
        let mut changes = 0;
        for k in 0..200 {
            let d = switch * if k % 2 == 0 { 0.98 } else { 1.02 };
            let level = select_lod(&errors, &sphere, d, p, Some(prev));
            if level != prev {
                changes += 1;
            }
            prev = level;
        }
        changes
    };
    let without = wobble(&none);
    let with = wobble(&params(0.1));
    println!("level changes over 200 wobbles: without hysteresis {without}, with {with}");
    assert!(without >= 190);
    assert!(with <= 1);
    // A large move still switches.
    assert_eq!(
        select_lod(&errors, &sphere, switch * 2.0, &params(0.1), Some(0)),
        1
    );
    assert_eq!(
        select_lod(&errors, &sphere, switch * 0.5, &params(0.1), Some(1)),
        0
    );
}

#[test]
fn select_lod_degenerate_inputs_return_finest() {
    let sphere = BoundingSphere {
        center: Vec3::ZERO,
        radius: 1.0,
    };
    assert_eq!(select_lod(&[], &sphere, 10.0, &params(0.0), None), 0);
    assert_eq!(
        select_lod(&[0.0, 1.0], &sphere, f32::NAN, &params(0.0), None),
        0
    );
    let mut bad = params(0.0);
    bad.viewport_height = 0.0;
    assert_eq!(select_lod(&[0.0, 1.0], &sphere, 10.0, &bad, None), 0);
}

#[test]
fn screen_size_mode_selects_by_fraction_with_hysteresis_and_culling() {
    let lods = ScreenSizeLods {
        thresholds: vec![0.6, 0.3, 0.1],
        cull_below: Some(0.02),
        hysteresis: 0.1,
    };
    lods.validate().unwrap();
    assert_eq!(lods.select(0.9, None), Some(0));
    assert_eq!(lods.select(0.6, None), Some(0));
    assert_eq!(lods.select(0.4, None), Some(1));
    assert_eq!(lods.select(0.2, None), Some(2));
    assert_eq!(lods.select(0.05, None), Some(2));
    assert_eq!(lods.select(0.01, None), None);
    // Dead band: 0.58 would be level 1, but level 0 is kept within 10%.
    assert_eq!(lods.select(0.58, Some(0)), Some(0));
    assert_eq!(lods.select(0.58, None), Some(1));
    assert_eq!(lods.select(0.62, Some(1)), Some(1));
    assert_eq!(lods.select(0.70, Some(1)), Some(0));
    // Cull hysteresis: a visible object survives slightly below the cull line.
    assert_eq!(lods.select(0.019, Some(2)), Some(2));
    assert_eq!(lods.select(0.019, None), None);
    // Monotonic in size.
    let mut last = 0;
    let mut f = 1.0f32;
    while f > 0.03 {
        let level = lods.select(f, None).unwrap();
        assert!(level >= last);
        last = level;
        f *= 0.97;
    }
    let bad = ScreenSizeLods {
        thresholds: vec![0.1, 0.3],
        cull_below: None,
        hysteresis: 0.0,
    };
    assert!(bad.validate().is_err());
}
