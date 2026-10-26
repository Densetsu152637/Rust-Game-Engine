//! Numeric behaviour of scatter: density accuracy, exact limits, determinism, spacing,
//! exclusion and zones.
use engine_foliage::*;
use engine_pcg::{Aabb, FlatSurface, FnSurface, Surface};

fn square(side: f32) -> Aabb {
    Aabb::new([0.0, 0.0, 0.0], [side, 200.0, side])
}

fn grass(density: f32) -> FoliageType {
    let mut t = FoliageType::new("grass", "grass");
    t.density = density;
    t
}

/// Tolerance for a Poisson-like count with expectation `n`: 5 sigma plus 1 %.
fn tolerance(n: f32) -> f32 {
    5.0 * n.sqrt() + 0.01 * n
}

#[test]
fn density_matches_the_authored_instances_per_square_metre() {
    let surface = FlatSurface::new(0.0);
    for (density, side) in [
        (0.05, 400.0),
        (0.5, 200.0),
        (2.0, 100.0),
        (16.0, 40.0),
        (64.0, 20.0),
    ] {
        let r = scatter(
            &[grass(density)],
            &surface,
            &ScatterParams::new(11, square(side)),
        )
        .unwrap();
        let expected = density * side * side;
        let got = r.field.len() as f32;
        assert!(
            (got - expected).abs() <= tolerance(expected),
            "density {density}: {got} vs {expected} (tolerance {})",
            tolerance(expected)
        );
        assert_eq!(r.types[0].accepted, r.field.len());
    }
}

#[test]
fn density_maps_scale_the_count_by_their_mean() {
    // A grid with density 0.25 on the left half and 1.0 on the right half. Cell centres sit at
    // x = 25 and 75: between them the map ramps linearly, outside it stays at the cell value.
    let grid = DensityGrid {
        origin: [0.0, 0.0],
        cell_size: 50.0,
        columns: 2,
        rows: 2,
        values: vec![0.25, 1.0, 0.25, 1.0],
    };
    let mut t = grass(1.0);
    t.density_map = DensityMap::Grid(grid);
    let r = scatter(
        &[t],
        &FlatSurface::new(0.0),
        &ScatterParams::new(4, square(100.0)),
    )
    .unwrap();
    // Mean over x: 25 m at 0.25, 50 m ramp (mean 0.625), 25 m at 1.0 -> 0.625 over 100 m.
    let expected = 100.0 * 100.0 * 0.625;
    assert!(
        (r.field.len() as f32 - expected).abs() <= tolerance(expected),
        "{} vs {expected}",
        r.field.len()
    );
    let left = r.field.iter().filter(|i| i.position[0] < 25.0).count() as f32;
    let right = r.field.iter().filter(|i| i.position[0] > 75.0).count() as f32;
    assert!(
        (left - 25.0 * 100.0 * 0.25).abs() <= tolerance(625.0),
        "{left}"
    );
    assert!((right - 25.0 * 100.0).abs() <= tolerance(2500.0), "{right}");
}

#[test]
fn slope_height_and_altitude_limits_hold_exactly() {
    // Rolling ground: slope varies from 0 to about 50 degrees, heights 0..100.
    let surface = FnSurface::new(square(100.0), |x, z| 0.6 * x + 8.0 * (z * 0.2).sin() + 5.0);
    let mut t = grass(2.0);
    t.slope_degrees = [25.0, 45.0];
    t.height = [20.0, 50.0];
    t.altitude = [22.0, 48.0];
    let bounds = Aabb::new([0.0, 0.0, 0.0], [100.0, 100.0, 100.0]);
    let r = scatter(&[t.clone()], &surface, &ScatterParams::new(9, bounds)).unwrap();
    assert!(r.field.len() > 200, "{}", r.field.len());
    for i in r.field.iter() {
        let (x, z) = (i.position[0], i.position[2]);
        let ground = surface.height(x, z);
        assert!((20.0..=50.0).contains(&ground), "height {ground}");
        assert!((22.0..=48.0).contains(&(ground - bounds.min.y)), "altitude");
        let slope = surface.normal(x, z).y.acos().to_degrees();
        assert!(
            (25.0..=45.0).contains(&slope),
            "slope {slope} at ({x}, {z})"
        );
        assert!((i.position[1] - ground).abs() < 1e-5, "sits on the ground");
    }
    let report = &r.types[0];
    assert!(report.rejected_slope > 0 && report.rejected_height > 0);
}

#[test]
fn identical_inputs_give_identical_output_and_seeds_differ() {
    let surface = FnSurface::new(square(80.0), |x, z| (x * 0.1).sin() * (z * 0.1).cos() * 3.0);
    let mut tree = FoliageType::new("tree", "tree");
    tree.density = 0.05;
    tree.min_spacing = 3.0;
    tree.tilt_degrees = 10.0;
    tree.scale = [0.8, 1.4];
    let types = [grass(0.5), tree];
    let run = |seed| {
        scatter(&types, &surface, &ScatterParams::new(seed, square(80.0)))
            .unwrap()
            .field
    };
    let (a, b, c) = (run(5), run(5), run(6));
    let list = |f: &FoliageField| f.iter().copied().collect::<Vec<_>>();
    assert_eq!(list(&a), list(&b));
    assert_ne!(list(&a), list(&c));
    // Relaxing a limit never reshuffles the survivors of the stricter run.
    let mut loose = types.clone();
    loose[0].slope_degrees = [0.0, 90.0];
    let mut strict = types.clone();
    strict[0].slope_degrees = [0.0, 8.0];
    let strict_field = scatter(&strict, &surface, &ScatterParams::new(5, square(80.0)))
        .unwrap()
        .field;
    let loose_field = scatter(&loose, &surface, &ScatterParams::new(5, square(80.0)))
        .unwrap()
        .field;
    let loose_positions: std::collections::HashSet<[u32; 3]> = loose_field
        .iter()
        .filter(|i| i.type_index == 0)
        .map(|i| i.position.map(f32::to_bits))
        .collect();
    let strict_grass = strict_field.iter().filter(|i| i.type_index == 0);
    assert!(strict_grass.count() > 0);
    assert!(
        strict_field
            .iter()
            .filter(|i| i.type_index == 0)
            .all(|i| loose_positions.contains(&i.position.map(f32::to_bits)))
    );
}

#[test]
fn minimum_spacing_is_honoured_and_nearly_saturates() {
    let surface = FlatSurface::new(0.0);
    let mut t = grass(2.0);
    t.min_spacing = 1.5;
    let r = scatter(&[t], &surface, &ScatterParams::new(21, square(60.0))).unwrap();
    let pts: Vec<[f32; 2]> = r
        .field
        .iter()
        .map(|i| [i.position[0], i.position[2]])
        .collect();
    let mut closest = f32::INFINITY;
    for (a, p) in pts.iter().enumerate() {
        for q in &pts[a + 1..] {
            closest = closest.min(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt());
        }
    }
    assert!(closest >= 1.5, "closest pair {closest}");
    // Random sequential packing of discs of radius 0.75 jams near 0.547 of the area; a finite
    // candidate budget must still reach a third.
    let packing = pts.len() as f32 * std::f32::consts::PI * 0.75 * 0.75 / (60.0 * 60.0);
    assert!(
        (0.33..0.56).contains(&packing),
        "packing fraction {packing}"
    );
    assert!(r.types[0].rejected_spacing > 0);
}

#[test]
fn per_type_exclusion_and_zones_keep_areas_clear() {
    let surface = FlatSurface::new(0.0);
    let mut tree = FoliageType::new("tree", "tree");
    tree.density = 0.02;
    let mut bush = FoliageType::new("bush", "bush");
    bush.density = 1.0;
    bush.excludes = vec![TypeExclusion {
        name: "tree".into(),
        radius: 2.0,
    }];
    let zones = [ExclusionZone::Circle {
        center: [50.0, 50.0],
        radius: 15.0,
        types: vec!["bush".into()],
    }];
    let mut params = ScatterParams::new(2, square(100.0));
    params.zones = &zones;
    let r = scatter(&[tree, bush], &surface, &params).unwrap();
    let trees: Vec<_> = r.field.iter().filter(|i| i.type_index == 0).collect();
    let bushes: Vec<_> = r.field.iter().filter(|i| i.type_index == 1).collect();
    assert!(trees.len() > 100 && bushes.len() > 5000);
    for b in &bushes {
        assert!(trees.iter().all(|t| {
            (t.position[0] - b.position[0]).hypot(t.position[2] - b.position[2]) >= 2.0
        }));
        assert!((b.position[0] - 50.0).hypot(b.position[2] - 50.0) > 15.0);
    }
    // The zone names bushes only: trees may stand in it.
    assert!(
        trees
            .iter()
            .any(|t| (t.position[0] - 50.0).hypot(t.position[2] - 50.0) <= 15.0)
    );
    assert!(r.types[1].rejected_exclusion > 0 && r.types[1].rejected_zone > 0);
}

#[test]
fn holes_and_surface_bounds_clip_placement() {
    let surface = FnSurface::new(Aabb::new([0.0; 3], [60.0, 10.0, 60.0]), |_, _| 1.0);
    let hole = |x: f32, z: f32| (20.0..40.0).contains(&x) && (20.0..40.0).contains(&z);
    let mut params = ScatterParams::new(1, square(100.0));
    params.holes = Some(&hole);
    let r = scatter(&[grass(1.0)], &surface, &params).unwrap();
    assert!(
        r.field
            .iter()
            .all(|i| i.position[0] < 60.0 && i.position[2] < 60.0)
    );
    assert!(r.field.iter().all(|i| !hole(i.position[0], i.position[2])));
    let expected = 60.0 * 60.0 - 20.0 * 20.0;
    assert!((r.field.len() as f32 - expected).abs() <= tolerance(expected));
    assert!(r.types[0].rejected_hole > 300);
}

#[test]
fn align_to_normal_tilts_the_up_axis_toward_the_ground_normal() {
    let surface = FnSurface::new(square(40.0), |x, _| 0.5 * x);
    let normal = surface.normal(10.0, 10.0);
    for (align, expected_dot) in [(0.0f32, normal.y), (1.0, 1.0)] {
        let mut t = grass(0.5);
        t.align_to_normal = align;
        let r = scatter(&[t], &surface, &ScatterParams::new(3, square(40.0))).unwrap();
        for i in r.field.iter().take(50) {
            let up =
                glam::Quat::from_xyzw(i.rotation[0], i.rotation[1], i.rotation[2], i.rotation[3])
                    * glam::Vec3::Y;
            let n = surface.normal(i.position[0], i.position[2]);
            assert!(
                (up.dot(n) - expected_dot).abs() < 1e-3,
                "align {align}: {}",
                up.dot(n)
            );
        }
    }
}

#[test]
fn scale_yaw_and_phase_follow_their_ranges() {
    let mut t = grass(2.0);
    t.scale = [0.5, 2.0];
    t.yaw_degrees = [0.0, 90.0];
    let r = scatter(
        &[t],
        &FlatSurface::new(0.0),
        &ScatterParams::new(8, square(50.0)),
    )
    .unwrap();
    let scales: Vec<f32> = r.field.iter().map(|i| i.scale[0]).collect();
    assert!(scales.iter().all(|s| (0.5..=2.0).contains(s)));
    let mean = scales.iter().sum::<f32>() / scales.len() as f32;
    assert!((mean - 1.25).abs() < 0.05, "mean scale {mean}");
    for i in r.field.iter() {
        let q = glam::Quat::from_xyzw(i.rotation[0], i.rotation[1], i.rotation[2], i.rotation[3]);
        let (yaw, _, _) = q.to_euler(glam::EulerRot::YXZ);
        assert!(
            (-1e-3..=std::f32::consts::FRAC_PI_2 + 1e-3).contains(&yaw),
            "yaw {yaw}"
        );
        assert!((0.0..1.0).contains(&i.phase));
    }
}

#[test]
fn scatter_into_the_asset_format_round_trips() {
    let surface = FlatSurface::new(2.0);
    let types = vec![grass(1.0)];
    let r = scatter(&types, &surface, &ScatterParams::new(1, square(30.0))).unwrap();
    let bytes = asset::encode(&types, &r.field).unwrap();
    let back = asset::decode(&bytes).unwrap();
    assert_eq!(back.field.len(), r.field.len());
    assert!(
        back.field
            .iter()
            .all(|i| (i.position[1] - 2.0).abs() < 1e-6)
    );
}
