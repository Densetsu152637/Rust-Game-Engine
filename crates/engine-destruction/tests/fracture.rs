use engine_destruction::{
    Error, Fracture, Limits, Mesh, Pattern, Pattern2, fracture, fracture_polygon,
    fracture_with_seeds, generate_seeds, topology,
};
use glam::{Vec2, Vec3};

fn cube() -> Mesh {
    Mesh::cuboid(Vec3::splat(0.5))
}
fn uniform(count: usize, seed: u64) -> Pattern {
    Pattern::Uniform { count, seed }
}
/// L-shaped prism: the polygon (0,0) (2,0) (2,1) (1,1) (1,2) (0,2) extruded 1 m along Z.
fn l_prism() -> Mesh {
    let outline = [
        Vec2::new(0., 0.),
        Vec2::new(2., 0.),
        Vec2::new(2., 1.),
        Vec2::new(1., 1.),
        Vec2::new(1., 2.),
        Vec2::new(0., 2.),
    ];
    let n = outline.len() as u32;
    let mut positions: Vec<Vec3> = outline.iter().map(|p| p.extend(0.)).collect();
    positions.extend(outline.iter().map(|p| p.extend(1.)));
    let cap = [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]];
    let mut triangles = Vec::new();
    for t in cap {
        triangles.push([t[0] + n, t[1] + n, t[2] + n]);
        triangles.push([t[0], t[2], t[1]]);
    }
    for k in 0..n {
        let next = (k + 1) % n;
        triangles.push([k, next, next + n]);
        triangles.push([k, next + n, k + n]);
    }
    Mesh::new(positions, triangles)
}
fn assert_closed_convex_pieces(result: &Fracture) {
    for fragment in &result.fragments {
        let topo = topology(&fragment.mesh.triangles);
        assert!(
            topo.watertight,
            "fragment {} is not watertight",
            fragment.seed_index
        );
        assert_eq!(
            topo.euler_characteristic(),
            2,
            "fragment {} is not a closed sphere-like surface",
            fragment.seed_index
        );
        assert!(
            (fragment.hull_volume - fragment.volume).abs() <= 1e-4 * fragment.volume,
            "hull of fragment {} differs from the piece",
            fragment.seed_index
        );
        assert_eq!(fragment.mesh.interior.len(), fragment.mesh.triangles.len());
    }
}

#[test]
fn cube_fragments_conserve_volume_and_are_closed_convex_pieces() {
    for count in [2, 8, 24, 64] {
        let started = std::time::Instant::now();
        let result = fracture(&cube(), &uniform(count, 7), &Limits::default()).unwrap();
        println!("cube {count}: {:?}", started.elapsed());
        assert_eq!(result.fragments.len(), count, "one fragment per seed");
        assert!(result.convex_source);
        let total = result.total_volume();
        assert!(
            (total - 1.).abs() <= 1e-4,
            "{count} seeds: total volume {total}"
        );
        assert_closed_convex_pieces(&result);
        // The volume-weighted centroid of the pieces is the cube centre.
        let centre: Vec3 = result
            .fragments
            .iter()
            .map(|f| f.centroid * f.volume)
            .sum::<Vec3>()
            / total;
        assert!(centre.length() < 1e-4, "{count} seeds: centroid {centre}");
        // Fracture surface exists on every piece, original surface on the outer ones.
        assert!(
            result
                .fragments
                .iter()
                .all(|f| f.mesh.interior.iter().any(|&i| i))
        );
    }
}

#[test]
fn a_single_seed_returns_the_whole_cube_with_its_analytic_inertia() {
    let result = fracture(&cube(), &uniform(1, 3), &Limits::default()).unwrap();
    let fragment = &result.fragments[0];
    assert!((fragment.volume - 1.).abs() < 1e-6);
    assert!(fragment.centroid.length() < 1e-6);
    // Unit cube about its centre: m/6 on every axis, no products of inertia.
    for r in 0..3 {
        for c in 0..3 {
            let expected = if r == c { 1. / 6. } else { 0. };
            assert!(
                (fragment.inertia[r][c] - expected).abs() < 1e-5,
                "inertia[{r}][{c}] = {}",
                fragment.inertia[r][c]
            );
        }
    }
    assert!(fragment.mesh.interior.iter().all(|&i| !i));
}

#[test]
fn output_is_deterministic_per_seed_and_differs_between_seeds() {
    let limits = Limits::default();
    let a = fracture(&cube(), &uniform(16, 99), &limits).unwrap();
    let b = fracture(&cube(), &uniform(16, 99), &limits).unwrap();
    let c = fracture(&cube(), &uniform(16, 100), &limits).unwrap();
    assert_eq!(a, b);
    assert_ne!(a.seeds, c.seeds);
    let impact = Pattern::Impact {
        count: 16,
        seed: 5,
        point: Vec3::new(0.5, 0., 0.),
        radius: 0.3,
    };
    assert_eq!(
        fracture(&cube(), &impact, &limits).unwrap(),
        fracture(&cube(), &impact, &limits).unwrap()
    );
}

#[test]
fn impact_pattern_makes_small_fragments_near_the_impact() {
    let point = Vec3::new(0.5, 0., 0.);
    let pattern = Pattern::Impact {
        count: 48,
        seed: 11,
        point,
        radius: 0.35,
    };
    let result = fracture(&cube(), &pattern, &Limits::default()).unwrap();
    assert!((result.total_volume() - 1.).abs() < 1e-4);
    let (mut near, mut far) = (vec![], vec![]);
    for f in &result.fragments {
        if (f.centroid - point).length() < 0.4 {
            near.push(f.volume);
        } else {
            far.push(f.volume);
        }
    }
    assert!(!near.is_empty() && !far.is_empty());
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (near, far) = (mean(&near), mean(&far));
    println!("mean volume near {near:.5} far {far:.5}");
    assert!(
        near < far * 0.5,
        "near {near} should be well below far {far}"
    );
}

#[test]
fn explicit_seeds_and_generated_seeds_agree() {
    let limits = Limits::default();
    let pattern = uniform(10, 4);
    let seeds = generate_seeds(&cube(), &pattern, &limits).unwrap();
    assert_eq!(seeds.len(), 10);
    let from_seeds = fracture_with_seeds(&cube(), &seeds, &limits).unwrap();
    let generated = fracture(&cube(), &pattern, &limits).unwrap();
    // The explicit seeds went through f32, so the pieces agree to rounding, not bitwise.
    assert_eq!(from_seeds.fragments.len(), generated.fragments.len());
    for (a, b) in from_seeds.fragments.iter().zip(&generated.fragments) {
        assert!(
            (a.volume - b.volume).abs() < 1e-4,
            "{} vs {}",
            a.volume,
            b.volume
        );
    }
    assert!(seeds.iter().all(|s| s.abs().max_element() <= 0.5));
}

#[test]
fn one_seed_returns_the_convex_parts_of_a_non_convex_mesh() {
    let result = fracture(&l_prism(), &uniform(1, 1), &Limits::default()).unwrap();
    let mut volumes: Vec<f32> = result.fragments.iter().map(|f| f.volume).collect();
    volumes.sort_by(f32::total_cmp);
    println!("parts: {volumes:?}");
    assert_eq!(volumes.len(), 2);
    assert!(
        (volumes[0] - 1.).abs() < 1e-5 && (volumes[1] - 2.).abs() < 1e-5,
        "{volumes:?}"
    );
}

#[test]
fn a_non_convex_l_prism_conserves_volume_with_convex_fragments() {
    let mesh = l_prism();
    assert!((mesh.volume() - 3.).abs() < 1e-6);
    let limits = Limits::default();
    let mut worst = 0f32;
    for (count, seed) in [(6, 1), (16, 2), (40, 3), (96, 4)] {
        let started = std::time::Instant::now();
        let result = fracture(&mesh, &uniform(count, seed), &limits).unwrap();
        assert!(!result.convex_source);
        assert_eq!(result.parts, 2, "the L splits along one plane");
        let total = result.total_volume();
        let error = (total - 3.).abs() / 3.;
        worst = worst.max(error);
        assert!(
            error <= 1e-4,
            "{count} seeds: volume {total}, error {error}"
        );
        assert!(result.fragments.len() >= count);
        assert!(result.fragments.len() <= 2 * count);
        assert_closed_convex_pieces(&result);
        // The Voronoi cell of every seed is fully covered by its fragments.
        let mut per_seed = vec![0.; count];
        for f in &result.fragments {
            per_seed[f.seed_index] += f.volume;
        }
        assert!(per_seed.iter().all(|v| *v > 0.));
        println!(
            "L prism {count} seeds: {} fragments, volume error {error:.2e}, {:?}",
            result.fragments.len(),
            started.elapsed()
        );
    }
    println!("worst L-prism volume error {worst:.2e}");
}

#[test]
fn limits_and_bad_input_are_rejected_with_a_reason() {
    let limits = Limits::default();
    assert!(matches!(
        fracture(&cube(), &uniform(0, 1), &limits),
        Err(Error::InvalidPattern(_))
    ));
    assert!(matches!(
        fracture(&cube(), &uniform(257, 1), &limits),
        Err(Error::InvalidPattern(_))
    ));
    let tight = Limits {
        max_vertices: 20,
        ..Limits::default()
    };
    assert!(matches!(
        fracture(&cube(), &uniform(20, 1), &tight),
        Err(Error::LimitExceeded(_))
    ));
    // An open mesh (one face removed) is not a solid.
    let mut open = cube();
    open.triangles.pop();
    assert!(matches!(
        fracture(&open, &uniform(4, 1), &limits),
        Err(Error::InvalidMesh(_))
    ));
    let impact = Pattern::Impact {
        count: 4,
        seed: 1,
        point: Vec3::ZERO,
        radius: -1.,
    };
    assert!(matches!(
        fracture(&cube(), &impact, &limits),
        Err(Error::InvalidPattern(_))
    ));
}

#[test]
fn a_convex_hull_of_points_fractures_like_any_convex_mesh() {
    let points = [
        Vec3::new(0., 0., 0.),
        Vec3::new(1., 0., 0.),
        Vec3::new(0., 1., 0.),
        Vec3::new(0., 0., 1.),
        Vec3::new(1., 1., 1.),
        Vec3::new(0.3, 0.3, 0.3),
    ];
    let mesh = Mesh::from_convex_points(&points).unwrap();
    assert!(mesh.is_watertight());
    let result = fracture(&mesh, &uniform(12, 8), &Limits::default()).unwrap();
    assert!(result.convex_source);
    assert!((result.total_volume() - mesh.volume()).abs() <= 1e-4 * mesh.volume());
    assert_closed_convex_pieces(&result);
}

#[test]
fn polygon_fragments_sum_to_the_polygon_area() {
    let square = [
        Vec2::new(-1., -1.),
        Vec2::new(1., -1.),
        Vec2::new(1., 1.),
        Vec2::new(-1., 1.),
    ];
    let limits = Limits::default();
    for count in [2, 9, 40] {
        let result =
            fracture_polygon(&square, &Pattern2::Uniform { count, seed: 21 }, &limits).unwrap();
        assert_eq!(result.fragments.len(), count);
        assert!(
            (result.total_area() - 4.).abs() <= 1e-4,
            "{}",
            result.total_area()
        );
        let centre: Vec2 = result
            .fragments
            .iter()
            .map(|f| f.centroid * f.area)
            .sum::<Vec2>()
            / result.total_area();
        assert!(centre.length() < 1e-4);
        for f in &result.fragments {
            assert!(f.area > 0. && f.points.len() >= 3);
            assert!(f.fracture_edge.iter().any(|&e| e) || count == 1);
        }
    }
    let one = fracture_polygon(&square, &Pattern2::Uniform { count: 1, seed: 1 }, &limits).unwrap();
    // Polar moment of a 2x2 square about its centre: m (w^2 + h^2) / 12 with unit density.
    assert!((one.fragments[0].inertia - 4. * 8. / 12.).abs() < 1e-4);
    assert_eq!(
        fracture_polygon(&square, &Pattern2::Uniform { count: 9, seed: 21 }, &limits).unwrap(),
        fracture_polygon(&square, &Pattern2::Uniform { count: 9, seed: 21 }, &limits).unwrap()
    );
    let mut clockwise = square;
    clockwise.reverse();
    let reversed = fracture_polygon(
        &clockwise,
        &Pattern2::Uniform { count: 5, seed: 2 },
        &limits,
    )
    .unwrap();
    assert!((reversed.total_area() - 4.).abs() <= 1e-4);
    let concave = [
        Vec2::new(0., 0.),
        Vec2::new(2., 0.),
        Vec2::new(1., 0.5),
        Vec2::new(2., 2.),
        Vec2::new(0., 2.),
    ];
    assert!(matches!(
        fracture_polygon(&concave, &Pattern2::Uniform { count: 3, seed: 1 }, &limits),
        Err(Error::InvalidMesh(_))
    ));
}
