use engine_mesh_lod::{Aabb, BoundingSphere, Containment, Frustum, cull_aabbs, cull_spheres};
use glam::{Mat4, Vec3};

/// Camera at the origin looking down -Z, 90 degree vertical FOV, square, near 0.1, far 100.
fn camera() -> Frustum {
    let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
    let proj = Mat4::perspective_rh(90f32.to_radians(), 1.0, 0.1, 100.0);
    Frustum::from_view_projection(&(proj * view))
}

fn sphere(c: Vec3, r: f32) -> BoundingSphere {
    BoundingSphere { center: c, radius: r }
}

#[test]
fn planes_are_normalised_and_point_inward() {
    let f = camera();
    for p in &f.planes {
        assert!((p.truncate().length() - 1.0).abs() < 1e-5);
    }
    // The frustum interior point is on the positive side of every plane.
    assert!(f.contains_point(Vec3::new(0.0, 0.0, -10.0)));
    // Translated/rotated camera keeps planes normalised.
    let view = Mat4::look_at_rh(Vec3::new(3.0, 4.0, 5.0), Vec3::new(-1.0, 0.5, -2.0), Vec3::Y);
    let proj = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.5, 250.0);
    let f2 = Frustum::from_view_projection(&(proj * view));
    assert!(f2.planes.iter().all(|p| (p.truncate().length() - 1.0).abs() < 1e-5));
}

#[test]
fn points_inside_outside() {
    let f = camera();
    assert!(f.contains_point(Vec3::new(0.0, 0.0, -10.0)));
    assert!(f.contains_point(Vec3::new(9.0, -9.0, -10.0)));
    assert!(!f.contains_point(Vec3::new(11.0, 0.0, -10.0)), "right of 90 degree frustum at depth 10");
    assert!(!f.contains_point(Vec3::new(0.0, 11.0, -10.0)));
    assert!(!f.contains_point(Vec3::new(0.0, 0.0, 5.0)), "behind camera");
    assert!(!f.contains_point(Vec3::new(0.0, 0.0, -0.05)), "in front of near plane");
    assert!(!f.contains_point(Vec3::new(0.0, 0.0, -101.0)), "beyond far plane");
    // Exact edge of the near plane (depth 0 in WebGPU clip space) is inside.
    assert!(f.contains_point(Vec3::new(0.0, 0.0, -0.1 - 1e-4)));
}

#[test]
fn sphere_classification() {
    let f = camera();
    assert_eq!(f.test_sphere(&sphere(Vec3::new(0.0, 0.0, -10.0), 1.0)), Containment::Inside);
    assert_eq!(f.test_sphere(&sphere(Vec3::new(0.0, 0.0, 10.0), 1.0)), Containment::Outside);
    assert_eq!(f.test_sphere(&sphere(Vec3::new(30.0, 0.0, -10.0), 1.0)), Containment::Outside);
    assert_eq!(f.test_sphere(&sphere(Vec3::new(0.0, 0.0, -150.0), 10.0)), Containment::Outside);
    // Straddles the right plane (x = 10 at depth 10).
    assert_eq!(f.test_sphere(&sphere(Vec3::new(10.0, 0.0, -10.0), 2.0)), Containment::Intersecting);
    // Straddles the near plane.
    assert_eq!(f.test_sphere(&sphere(Vec3::new(0.0, 0.0, -0.1), 0.5)), Containment::Intersecting);
    // Straddles the far plane.
    assert_eq!(f.test_sphere(&sphere(Vec3::new(0.0, 0.0, -100.0), 5.0)), Containment::Intersecting);
    // Huge sphere containing the camera is not culled.
    assert!(f.intersects_sphere(&sphere(Vec3::ZERO, 1000.0)));
}

#[test]
fn aabb_classification() {
    let f = camera();
    let bx = |c: Vec3, h: f32| Aabb { min: c - Vec3::splat(h), max: c + Vec3::splat(h) };
    assert_eq!(f.test_aabb(&bx(Vec3::new(0.0, 0.0, -10.0), 1.0)), Containment::Inside);
    assert_eq!(f.test_aabb(&bx(Vec3::new(0.0, 0.0, 10.0), 1.0)), Containment::Outside);
    assert_eq!(f.test_aabb(&bx(Vec3::new(40.0, 0.0, -10.0), 2.0)), Containment::Outside);
    assert_eq!(f.test_aabb(&bx(Vec3::new(10.0, 0.0, -10.0), 2.0)), Containment::Intersecting);
    assert_eq!(f.test_aabb(&bx(Vec3::new(0.0, 0.0, -100.0), 5.0)), Containment::Intersecting);
    assert_eq!(f.test_aabb(&bx(Vec3::ZERO, 500.0)), Containment::Intersecting);
    // Conservative: never Outside when any corner is inside.
    let straddle = Aabb { min: Vec3::new(9.0, -1.0, -11.0), max: Vec3::new(30.0, 1.0, -9.0) };
    assert!(f.intersects_aabb(&straddle));
}

#[test]
fn conservative_against_random_points() {
    // Any box/sphere that contains a point inside the frustum must not be culled.
    let f = camera();
    let mut state = 0x1234_5678u32;
    let mut rnd = move || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut checked = 0;
    for _ in 0..5000 {
        let p = Vec3::new(rnd() * 80.0 - 40.0, rnd() * 80.0 - 40.0, -rnd() * 120.0 + 10.0);
        if f.contains_point(p) {
            checked += 1;
            let r = rnd() * 3.0;
            assert!(f.intersects_sphere(&sphere(p + Vec3::splat(r * 0.5), r.max(0.9))));
            assert!(f.intersects_aabb(&Aabb { min: p - Vec3::splat(r), max: p + Vec3::splat(r) }));
        }
    }
    assert!(checked > 100);
}

#[test]
fn infinite_and_reversed_projections_work() {
    let view = Mat4::look_at_rh(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
    let inf = Frustum::from_view_projection(&(Mat4::perspective_infinite_rh(1.0, 1.0, 0.1) * view));
    assert!(inf.contains_point(Vec3::new(0.0, 0.0, -1.0e6)));
    assert!(!inf.contains_point(Vec3::new(0.0, 0.0, 1.0)));
    let rev = Frustum::from_view_projection(&(Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1) * view));
    assert!(rev.contains_point(Vec3::new(0.0, 0.0, -1.0e6)));
    assert!(!rev.contains_point(Vec3::new(0.0, 0.0, -0.01)));
    assert!(!rev.contains_point(Vec3::new(0.0, 0.0, 1.0)));
}

#[test]
fn batch_cull_returns_ascending_visible_indices_and_reuses_buffer() {
    let f = camera();
    let spheres = [
        sphere(Vec3::new(0.0, 0.0, -10.0), 1.0),
        sphere(Vec3::new(0.0, 0.0, 10.0), 1.0),
        sphere(Vec3::new(5.0, 0.0, -20.0), 1.0),
        sphere(Vec3::new(500.0, 0.0, -20.0), 1.0),
        sphere(Vec3::new(10.0, 0.0, -10.0), 2.0),
    ];
    let mut out = vec![99];
    cull_spheres(&f, &spheres, &mut out);
    assert_eq!(out, vec![0, 2, 4]);

    let boxes: Vec<Aabb> = spheres
        .iter()
        .map(|s| Aabb { min: s.center - Vec3::splat(s.radius), max: s.center + Vec3::splat(s.radius) })
        .collect();
    cull_aabbs(&f, &boxes, &mut out);
    assert_eq!(out, vec![0, 2, 4]);
}

#[test]
fn bounds_builders() {
    let pts = [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(3.0, 2.0, 1.0), Vec3::new(0.0, -4.0, 2.0), Vec3::ZERO];
    let b = Aabb::from_points(&pts).unwrap();
    assert_eq!(b.min, Vec3::new(-1.0, -4.0, 0.0));
    assert_eq!(b.max, Vec3::new(3.0, 2.0, 2.0));
    let s = BoundingSphere::from_points(&pts).unwrap();
    for p in pts {
        assert!(p.distance(s.center) <= s.radius * (1.0 + 1e-6));
    }
    assert!(Aabb::from_points(&[]).is_none() && BoundingSphere::from_points(&[]).is_none());
    let t = b.transformed(&Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)));
    assert_eq!(t.min.x, 9.0);
    let rot = b.transformed(&Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2));
    assert!(rot.max.x >= 2.0 - 1e-4, "{:?}", rot);
}
