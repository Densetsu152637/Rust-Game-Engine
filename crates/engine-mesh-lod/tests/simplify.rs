mod common;

use common::*;
use engine_mesh_lod::{MeshData, MeshLodError, SimplifyOptions, mesh_error, simplify};
use glam::{Vec2, Vec3};

#[test]
fn flat_plane_collapses_to_corners_with_zero_error_and_boundary_preserved() {
    let mesh = plane(32, 10.0);
    assert_eq!(mesh.triangle_count(), 2048);
    let r = simplify(&mesh, &SimplifyOptions::to_triangle_count(4)).unwrap();
    println!(
        "plane: {} -> {} tris, achieved_error {:e}",
        r.original_triangles, r.final_triangles, r.achieved_error
    );
    assert!(r.final_triangles <= 4, "got {}", r.final_triangles);
    assert!(r.reached_target);
    assert!(r.achieved_error < 1e-4, "error {}", r.achieved_error);
    // Boundary preserved: same bounds and same total area as the 10x10 square.
    let b = r.mesh.bounds().unwrap();
    assert!((b.min - Vec3::ZERO).length() < 1e-4);
    assert!((b.max - Vec3::new(10.0, 0.0, 10.0)).length() < 1e-4);
    assert!(
        (total_area(&r.mesh) - 100.0).abs() < 1e-2,
        "area {}",
        total_area(&r.mesh)
    );
    assert!(r.mesh.positions.iter().all(|p| p.y.abs() < 1e-5));
    assert!(min_double_area(&r.mesh) > 1e-3);
}

#[test]
fn uv_seam_vertices_are_kept() {
    let (mesh, seam) = plane_with_uv_seam(8, 8.0);
    assert_eq!(seam.len(), 9);
    let r = simplify(&mesh, &SimplifyOptions::to_triangle_count(2)).unwrap();
    assert!(r.final_triangles < mesh.triangle_count());
    let uvs = r.mesh.uvs.as_ref().unwrap();
    for s in &seam {
        let copies: Vec<Vec2> = r
            .mesh
            .positions
            .iter()
            .zip(uvs)
            .filter(|(p, _)| (**p - *s).length() < 1e-5)
            .map(|(_, uv)| *uv)
            .collect();
        assert_eq!(copies.len(), 2, "seam vertex {s:?} lost a copy");
        assert!(
            (copies[0].x - copies[1].x).abs() > 0.4,
            "UV discontinuity lost at {s:?}"
        );
    }
}

#[test]
fn sphere_to_ten_percent_stays_on_surface_without_flips() {
    let radius = 2.0;
    let mesh = uv_sphere(50, 50, radius, Vec3::ZERO);
    assert_eq!(mesh.triangle_count(), 4900);
    let target = mesh.triangle_count() / 10;
    let r = simplify(&mesh, &SimplifyOptions::to_triangle_count(target)).unwrap();
    let m = &r.mesh;
    println!(
        "sphere: {} -> {} tris (target {}), achieved_error {:.5} ({:.2}% of r)",
        r.original_triangles,
        r.final_triangles,
        target,
        r.achieved_error,
        100.0 * r.achieved_error / radius
    );
    assert!(r.final_triangles <= target && r.final_triangles + 1 >= target);

    let max_vertex_dev = m
        .positions
        .iter()
        .map(|p| (p.length() - radius).abs())
        .fold(0.0, f32::max);
    println!(
        "sphere max vertex |r - R| = {:.5} ({:.3}% of R)",
        max_vertex_dev,
        100.0 * max_vertex_dev / radius
    );
    assert!(
        max_vertex_dev <= 0.02 * radius,
        "vertex deviation {max_vertex_dev}"
    );
    // Surface (face centroids) is inside the sphere by at most the chord sagitta.
    let max_centroid_dev = m
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let c = (m.positions[t[0] as usize]
                + m.positions[t[1] as usize]
                + m.positions[t[2] as usize])
                / 3.0;
            (c.length() - radius).abs()
        })
        .fold(0.0, f32::max);
    println!("sphere max centroid |r - R| = {max_centroid_dev:.5}");
    assert!(max_centroid_dev <= 0.15 * radius);
    assert!(r.achieved_error <= 0.15 * radius);
    assert!(r.achieved_error > 0.0);

    // No flipped faces (outward normal agrees with radial direction) and no degenerate faces.
    for t in m.indices.as_chunks::<3>().0 {
        let (a, b, c) = (
            m.positions[t[0] as usize],
            m.positions[t[1] as usize],
            m.positions[t[2] as usize],
        );
        let n = (b - a).cross(c - a);
        let centroid = (a + b + c) / 3.0;
        assert!(
            n.normalize().dot(centroid.normalize()) > 0.0,
            "flipped face"
        );
        assert!(n.length() > 1e-6, "degenerate face");
    }
    // Output normals are unit and indices valid.
    m.validate().unwrap();
    for n in m.normals.as_ref().unwrap() {
        assert!((n.length() - 1.0).abs() < 1e-3);
    }
}

#[test]
fn simplification_is_deterministic() {
    let mesh = uv_sphere(30, 30, 1.0, Vec3::new(5.0, -3.0, 2.0));
    let opts = SimplifyOptions::to_triangle_count(200);
    let a = simplify(&mesh, &opts).unwrap();
    let b = simplify(&mesh, &opts).unwrap();
    assert_eq!(a, b);
}

#[test]
fn target_count_is_respected_within_one_triangle() {
    let mesh = uv_sphere(50, 50, 1.0, Vec3::ZERO);
    for target in [3000usize, 1000, 333, 120] {
        let r = simplify(&mesh, &SimplifyOptions::to_triangle_count(target)).unwrap();
        println!("target {target}: got {}", r.final_triangles);
        assert!(r.final_triangles <= target && r.final_triangles + 1 >= target);
    }
}

#[test]
fn max_error_mode_bounds_the_measured_error() {
    let mesh = uv_sphere(50, 50, 1.0, Vec3::ZERO);
    let r = simplify(&mesh, &SimplifyOptions::to_max_error(0.01)).unwrap();
    println!(
        "max_error 0.01: {} -> {} tris, measured {:.5}",
        r.original_triangles, r.final_triangles, r.achieved_error
    );
    assert!(r.final_triangles < mesh.triangle_count());
    assert!(r.achieved_error <= 0.02, "measured {}", r.achieved_error);
    // Measuring independently agrees.
    let again = mesh_error(&mesh, &r.mesh).unwrap();
    assert!((again - r.achieved_error).abs() < 1e-6);
}

#[test]
fn lock_boundary_keeps_boundary_vertices() {
    let mesh = plane(8, 8.0);
    let opts = SimplifyOptions {
        lock_boundary: true,
        ..SimplifyOptions::to_triangle_count(2)
    };
    let r = simplify(&mesh, &opts).unwrap();
    let boundary_count = mesh
        .positions
        .iter()
        .filter(|p| p.x == 0.0 || p.x == 8.0 || p.z == 0.0 || p.z == 8.0)
        .count();
    let kept = r
        .mesh
        .positions
        .iter()
        .filter(|p| p.x == 0.0 || p.x == 8.0 || p.z == 0.0 || p.z == 8.0)
        .count();
    assert_eq!(kept, boundary_count);
}

#[test]
fn invalid_input_is_rejected() {
    let ok = plane(2, 1.0);
    let opts = SimplifyOptions::to_triangle_count(2);

    let mut bad = ok.clone();
    bad.indices[0] = 999;
    assert!(matches!(
        simplify(&bad, &opts),
        Err(MeshLodError::IndexOutOfRange { .. })
    ));

    let mut bad = ok.clone();
    bad.indices.pop();
    assert!(matches!(
        simplify(&bad, &opts),
        Err(MeshLodError::IndexCountNotMultipleOfThree { .. })
    ));

    let mut bad = ok.clone();
    bad.positions[1].x = f32::NAN;
    assert!(matches!(
        simplify(&bad, &opts),
        Err(MeshLodError::NonFiniteValue { .. })
    ));

    let mut bad = ok.clone();
    bad.normals.as_mut().unwrap().pop();
    assert!(matches!(
        simplify(&bad, &opts),
        Err(MeshLodError::AttributeLengthMismatch { .. })
    ));

    let mut bad = ok.clone();
    bad.uvs.as_mut().unwrap()[0] = Vec2::new(f32::INFINITY, 0.0);
    assert!(matches!(
        simplify(&bad, &opts),
        Err(MeshLodError::NonFiniteValue { .. })
    ));

    assert!(matches!(
        simplify(&MeshData::default(), &opts),
        Err(MeshLodError::EmptyMesh)
    ));
    assert!(matches!(
        simplify(&ok, &SimplifyOptions::default()),
        Err(MeshLodError::InvalidOption { .. })
    ));
    assert!(matches!(
        simplify(&ok, &SimplifyOptions::to_triangle_count(0)),
        Err(MeshLodError::InvalidOption { .. })
    ));
    assert!(matches!(
        simplify(&ok, &SimplifyOptions::to_max_error(f32::NAN)),
        Err(MeshLodError::InvalidOption { .. })
    ));
}

#[test]
fn mesh_error_is_zero_for_identical_and_positive_for_offset() {
    let mesh = uv_sphere(12, 12, 1.0, Vec3::ZERO);
    assert!(mesh_error(&mesh, &mesh).unwrap() < 1e-6);
    let mut moved = mesh.clone();
    for p in &mut moved.positions {
        *p += Vec3::new(0.25, 0.0, 0.0);
    }
    let e = mesh_error(&mesh, &moved).unwrap();
    assert!(e > 0.05 && e <= 0.25 + 1e-4, "e = {e}");
}
