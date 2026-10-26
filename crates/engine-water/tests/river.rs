//! Rivers: flow continuity (constant discharge), the cross-section profile, locating points
//! and the ribbon mesh.
use engine_water::{RiverSettings, RiverSpline};
use glam::Vec2;

fn river() -> RiverSettings {
    RiverSettings {
        points: vec![
            [0.0, 2.0, 0.0],
            [20.0, 1.5, 5.0],
            [40.0, 1.0, -5.0],
            [60.0, 0.5, 0.0],
        ],
        widths: vec![6.0, 4.0, 9.0, 3.0],
        depths: vec![1.0, 1.5, 0.6, 2.0],
        flow_speed: 1.2,
        subdivisions: 12,
    }
}

/// Discharge through every section, integrated across the width (256-point midpoint rule of
/// the parabolic profile times depth), equals `Q = flow_speed * width_0 * depth_0` within
/// 0.1 %, while the mean speed changes with the section area.
#[test]
fn flow_is_continuous_along_the_river() {
    let spline = RiverSpline::new(&river()).unwrap();
    let q = spline.discharge();
    assert!((q - 1.2 * 6.0 * 1.0).abs() < 1.0e-5);
    let mut speeds = Vec::new();
    for s in spline.samples() {
        let steps = 256;
        let dn = s.width / steps as f32;
        let flux: f32 = (0..steps)
            .map(|i| {
                let n = -0.5 * s.width + (i as f32 + 0.5) * dn;
                spline.speed_at(s.width, s.depth, n) * s.depth * dn
            })
            .sum();
        assert!(
            (flux - q).abs() < 1.0e-3 * q,
            "flux {flux} vs {q} at {}",
            s.distance
        );
        speeds.push(spline.mean_speed(s.width, s.depth));
    }
    let (lo, hi) = speeds
        .iter()
        .fold((f32::INFINITY, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
    eprintln!("discharge {q} m^3/s, mean speed {lo:.3}..{hi:.3} m/s");
    // Narrowest x shallowest section (9 x 0.6 = 5.4 m^2 vs 3 x 2 = 6 m^2 vs 6 x 1).
    assert!(hi > 1.3 && lo < 1.2);
}

#[test]
fn located_points_carry_the_flow_profile() {
    let spline = RiverSpline::new(&river()).unwrap();
    let centre = spline.locate(Vec2::new(0.0, 0.0));
    assert!(centre.inside);
    assert!(centre.offset.abs() < 1.0e-4);
    assert!((centre.height - 2.0).abs() < 1.0e-4);
    let mean = spline.mean_speed(6.0, 1.0);
    // Centreline speed is 1.5 x the mean; the flow follows the tangent.
    assert!((centre.velocity.length() - 1.5 * mean).abs() < 1.0e-4);
    assert!(centre.velocity.normalize().dot(centre.tangent) > 0.9999);
    // Near a bank the flow slows to zero; beyond it the point is outside.
    let t = spline.samples()[0].tangent;
    let side = Vec2::new(t.x, t.z).normalize().perp();
    let near_bank = spline.locate(side * 2.94);
    assert!((near_bank.offset - 2.94).abs() < 1.0e-3, "{near_bank:?}");
    assert!(near_bank.inside && near_bank.velocity.length() < 0.15 * mean);
    let outside = spline.locate(side * 3.2);
    assert!(!outside.inside);
    assert_eq!(outside.velocity, glam::Vec3::ZERO);
    // Arc length grows downstream and ends at the spline length.
    let end = spline.locate(Vec2::new(60.0, 0.0));
    assert!((end.distance - spline.length()).abs() < 1.0e-3);
    assert!(spline.length() > 60.0);
    // Downstream: the flow at the end points along +X (the last span heads east).
    assert!(end.tangent.x > 0.5);
}

#[test]
fn ribbon_spans_the_banks() {
    let spline = RiverSpline::new(&river()).unwrap();
    let (positions, uvs, indices) = spline.ribbon(4);
    let samples = spline.samples().len();
    assert_eq!(samples, 3 * 12 + 1);
    assert_eq!(positions.len(), samples * 5);
    assert_eq!(uvs.len(), positions.len());
    assert_eq!(indices.len(), (samples - 1) * 4 * 6);
    // First row: bank to bank across width 6 at the source, at the surface height.
    let width = positions[0].distance(positions[4]);
    assert!((width - 6.0).abs() < 1.0e-3, "{width}");
    assert!(positions[..5].iter().all(|p| (p.y - 2.0).abs() < 1.0e-5));
    // Triangles face up.
    for tri in indices.chunks(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| positions[i as usize]);
        assert!((b - a).cross(c - a).y > 0.0);
    }
}

#[test]
fn rejects_invalid_rivers() {
    let mut bad = river();
    bad.points.truncate(1);
    assert_eq!(
        RiverSpline::new(&bad).unwrap_err().code,
        "water.river.invalid"
    );
    let mut bad = river();
    bad.widths = vec![1.0, 2.0];
    assert!(RiverSpline::new(&bad).is_err());
    let mut bad = river();
    bad.points[1] = bad.points[0];
    assert!(RiverSpline::new(&bad).is_err());
    let mut bad = river();
    bad.flow_speed = -1.0;
    assert!(RiverSpline::new(&bad).is_err());
}
