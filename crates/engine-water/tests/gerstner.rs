//! Gerstner waves: dispersion, crest speed, bounded steepness (no loops), displacement inversion
//! and normals.
use engine_water::{
    GRAVITY, GerstnerSet, GerstnerWave, INVERSE_TOLERANCE, MAX_STEEPNESS_SUM, angular_frequency,
};
use glam::{Mat2, Vec2};
use std::f32::consts::TAU;

fn wave(direction: f32, wavelength: f32, amplitude: f32, steepness: f32) -> GerstnerWave {
    GerstnerWave {
        direction_degrees: direction,
        wavelength,
        amplitude,
        steepness,
        phase: 0.3 * direction,
    }
}

fn storm() -> Vec<GerstnerWave> {
    vec![
        wave(0.0, 24.0, 1.2, 1.0),
        wave(35.0, 13.0, 0.6, 1.0),
        wave(-50.0, 7.0, 0.3, 1.0),
        wave(110.0, 4.0, 0.15, 1.0),
    ]
}

#[test]
fn dispersion_relation_deep_and_finite_depth() {
    for wavelength in [1.0f32, 10.0, 100.0] {
        let k = TAU / wavelength;
        let deep = angular_frequency(k, None, GRAVITY);
        assert!((deep * deep - GRAVITY * k).abs() <= 1.0e-5 * GRAVITY * k);
        for depth in [0.5f32, 3.0, 50.0] {
            let w = angular_frequency(k, Some(depth), GRAVITY);
            let expected = GRAVITY * k * (k * depth).tanh();
            assert!(
                (w * w - expected).abs() <= 1.0e-5 * expected,
                "{wavelength} {depth}"
            );
            // Shallow water is slower; the shallow limit is c = sqrt(g h).
            assert!(w <= deep + 1.0e-6);
        }
    }
    let k = TAU / 200.0;
    let c = angular_frequency(k, Some(1.0), GRAVITY) / k;
    assert!((c - GRAVITY.sqrt()).abs() < 0.01 * GRAVITY.sqrt(), "{c}");
    let set = GerstnerSet::new(&[wave(0.0, 10.0, 0.2, 0.5)], Some(2.0), GRAVITY).unwrap();
    let (k, w) = set.dispersion(0).unwrap();
    assert!((w * w - GRAVITY * k * (2.0 * k).tanh()).abs() < 1.0e-4);
}

/// The crest of a single steep wave, located on the displaced surface through the height
/// query, travels at the phase speed `w / k`.
#[test]
fn crest_moves_at_the_phase_speed() {
    let wavelength = 16.0;
    let set = GerstnerSet::deep(&[wave(0.0, wavelength, 0.8, 0.7)]).unwrap();
    let (k, w) = set.dispersion(0).unwrap();
    let crest = |t: f32| {
        let mut best = (f32::NEG_INFINITY, 0.0);
        for i in 0..16_000 {
            let x = i as f32 * 0.001;
            let (point, _) = set.sample(Vec2::new(x, 2.0), t, 1.0);
            if point.offset.y > best.0 {
                best = (point.offset.y, x);
            }
        }
        best.1
    };
    let (t0, t1) = (0.0, 1.5);
    let speed = (crest(t1) - crest(t0)).rem_euclid(wavelength) / (t1 - t0);
    let expected = w / k;
    eprintln!("crest speed {speed:.4} m/s, phase speed {expected:.4} m/s");
    assert!((speed - expected).abs() < 0.01 * expected);
}

#[test]
fn steepness_sum_is_capped_and_the_surface_never_loops() {
    let set = GerstnerSet::deep(&storm()).unwrap();
    assert!((set.lipschitz() - MAX_STEEPNESS_SUM).abs() < 1.0e-5);
    assert!((set.steepness_scale() - MAX_STEEPNESS_SUM / 4.0).abs() < 1.0e-6);
    // Jacobian determinant of the horizontal map >= (1 - 0.9)^2 everywhere.
    let mut lowest = f32::INFINITY;
    for i in 0..200 {
        for j in 0..200 {
            let x0 = Vec2::new(i as f32 * 0.37, j as f32 * 0.29);
            for t in [0.0, 2.7] {
                let p = set.evaluate(x0, t, 1.0);
                lowest = lowest.min((Mat2::IDENTITY + p.jacobian).determinant());
            }
        }
    }
    eprintln!("lowest Jacobian determinant {lowest:.4}");
    assert!(lowest >= 0.01 - 1.0e-4, "{lowest}");
    // A gentle set keeps its authored steepness.
    let gentle =
        GerstnerSet::deep(&[wave(0.0, 10.0, 0.2, 0.3), wave(90.0, 6.0, 0.1, 0.2)]).unwrap();
    assert_eq!(gentle.steepness_scale(), 1.0);
    assert!((gentle.lipschitz() - 0.5).abs() < 1.0e-5);
}

#[test]
fn rejects_invalid_waves() {
    let too_tall = wave(0.0, 10.0, 1.0, 0.5);
    assert_eq!(
        GerstnerSet::deep(&[too_tall]).unwrap_err().code,
        "water.waves.invalid"
    );
    assert!(GerstnerSet::deep(&[wave(0.0, 0.01, 0.0, 0.5)]).is_err());
    assert!(GerstnerSet::deep(&[wave(0.0, 10.0, 0.1, 1.5)]).is_err());
    assert!(GerstnerSet::deep(&vec![wave(0.0, 10.0, 0.1, 0.1); 17]).is_err());
    assert!(GerstnerSet::new(&storm(), Some(-1.0), GRAVITY).is_err());
}

/// Height queries invert the horizontal displacement: the found rest point displaces onto the
/// query position within the 1e-4 m tolerance, at the steepest allowed setting.
#[test]
fn inverse_displacement_error_is_within_tolerance() {
    let set = GerstnerSet::deep(&storm()).unwrap();
    let mut worst: f32 = 0.0;
    let mut iterations = 0u32;
    let mut most = 0u32;
    let count = 4000;
    for i in 0..count {
        let p = Vec2::new(
            (i % 80) as f32 * 0.613 - 20.0,
            (i / 80) as f32 * 0.811 - 20.0,
        );
        let t = (i % 7) as f32 * 0.9;
        let (point, inversion) = set.sample(p, t, 1.0);
        assert!(inversion.converged, "{p} at {t}: {inversion:?}");
        let landed = inversion.rest + Vec2::new(point.offset.x, point.offset.z);
        worst = worst.max(landed.distance(p));
        iterations += inversion.iterations;
        most = most.max(inversion.iterations);
    }
    eprintln!(
        "max inverse error {worst:e} m, mean iterations {:.2}, max {most}",
        iterations as f32 / count as f32
    );
    assert!(worst <= INVERSE_TOLERANCE);
    // Horizontal displacement is real: without inversion the error would be up to sum q A.
    let raw = set.evaluate(Vec2::new(1.0, 2.0), 0.0, 1.0);
    assert!(Vec2::new(raw.offset.x, raw.offset.z).length() > 0.01);
}

#[test]
fn normals_match_finite_differences_of_the_displaced_surface() {
    let set = GerstnerSet::deep(&storm()).unwrap();
    let t = 1.3;
    let e = 1.0e-3;
    for (x, z) in [(0.5f32, 0.25f32), (3.0, -7.0), (11.0, 4.5)] {
        let x0 = Vec2::new(x, z);
        let at = |d: Vec2| {
            let p = set.evaluate(x0 + d, t, 1.0);
            glam::Vec3::new(x0.x + d.x + p.offset.x, p.offset.y, x0.y + d.y + p.offset.z)
        };
        let tx = (at(Vec2::new(e, 0.0)) - at(Vec2::new(-e, 0.0))) / (2.0 * e);
        let tz = (at(Vec2::new(0.0, e)) - at(Vec2::new(0.0, -e))) / (2.0 * e);
        let numeric = tz.cross(tx).normalize();
        let analytic = set.evaluate(x0, t, 1.0).normal();
        assert!(
            numeric.angle_between(analytic) < 2.0e-3,
            "{numeric} {analytic}"
        );
        assert!(analytic.y > 0.0);
    }
}

#[test]
fn evaluation_is_deterministic_and_scales_with_attenuation() {
    let set = GerstnerSet::deep(&storm()).unwrap();
    let a = set.evaluate(Vec2::new(1.5, -2.5), 3.25, 1.0);
    let b = set.evaluate(Vec2::new(1.5, -2.5), 3.25, 1.0);
    assert_eq!(a, b);
    let half = set.evaluate(Vec2::new(1.5, -2.5), 3.25, 0.5);
    assert!((half.offset - a.offset * 0.5).length() < 1.0e-6);
    let flat = set.evaluate(Vec2::new(1.5, -2.5), 3.25, 0.0);
    assert_eq!(flat.offset, glam::Vec3::ZERO);
}
