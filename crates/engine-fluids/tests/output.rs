//! Renderer-facing outputs: surface flags, normals and the density grid.

mod common;

use common::{block, params, run, tank};
use glam::Vec3;

#[test]
fn settled_pool_reports_surface_normals_and_density_grid() {
    let p = params();
    let min = Vec3::ZERO;
    let max = Vec3::new(0.5, 0.6, 0.5);
    let mut volume = tank(p, min, max, 1024);
    volume
        .fill(block(min, Vec3::new(0.5, 0.3, 0.5)), Vec3::ZERO, None)
        .unwrap();
    run(&mut volume, 1.0, &[]);
    let out = volume.render_output();
    let n = volume.len();
    assert_eq!(out.positions.len(), n);
    assert_eq!(out.velocities.len(), n);
    assert_eq!(out.densities.len(), n);
    assert_eq!(out.normals.len(), n);
    assert_eq!(out.surface.len(), n);
    assert_eq!(out.ids.len(), n);
    let top = out.positions.iter().map(|x| x.y).fold(f32::MIN, f32::max);
    let s = p.spacing();
    let h = p.kernel_radius();
    for i in 0..n {
        let x = out.positions[i];
        let y = x.y;
        if y > top - 0.5 * s {
            assert!(out.surface[i], "top particle {i} at {y} not flagged");
            // Away from the side walls the normal points up.
            let interior = x.x > h && x.x < max.x - h && x.z > h && x.z < max.z - h;
            if interior {
                assert!(out.normals[i].y > 0.9, "top normal {:?}", out.normals[i]);
            }
        }
        if y < top - 2.5 * s {
            assert!(
                !out.surface[i],
                "deep particle {i} at {y} flagged as surface"
            );
        }
    }
    // Marching-cubes input: about 1 inside, about 0 well above the surface, crossing 0.5 near
    // the free surface.
    let grid = volume
        .sample_density_grid(
            Vec3::new(0.25, 0.0, 0.25),
            Vec3::new(0.25001, 0.6, 0.25001),
            [2, 61, 2],
        )
        .unwrap();
    assert_eq!(grid.values.len(), 2 * 61 * 2);
    let column: Vec<f32> = (0..61).map(|y| grid.value(0, y, 0).unwrap()).collect();
    let inside = column[20];
    let above = column[55];
    assert!((inside - 1.0).abs() < 0.1, "inside {inside}");
    assert!(above < 1.0e-6, "above {above}");
    let crossing = column.iter().rposition(|&v| v >= 0.5).unwrap() as f32 * 0.01;
    assert!(
        (crossing - top).abs() < s,
        "iso crossing {crossing} vs top particle {top}"
    );
}
