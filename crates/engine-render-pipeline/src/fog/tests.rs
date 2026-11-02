use super::*;
use crate::atmosphere::{ATMOSPHERE_AERIAL_FEATURE, AtmosphereControls};

fn bad(change: impl FnOnce(&mut FogSettings)) -> bool {
    let mut settings = FogSettings::default();
    change(&mut settings);
    settings.validate().is_err()
}

/// Midpoint integration of `density * exp(-falloff * h)` along the ray, the
/// independent check of the closed form.
fn numeric_depth(density: f64, falloff: f64, h0: f64, dy: f64, length: f64) -> f64 {
    let n = 200_000;
    let dt = length / f64::from(n);
    (0..n)
        .map(|i| {
            let t = (f64::from(i) + 0.5) * dt;
            density * (-falloff * (h0 + dy * t)).exp() * dt
        })
        .sum()
}

#[test]
fn defaults_validate_and_every_range_is_enforced() {
    FogSettings::default().validate().unwrap();
    assert!(bad(|s| s.density = f32::NAN));
    assert!(bad(|s| s.density = -0.1));
    assert!(bad(|s| s.density = 11.0));
    assert!(bad(|s| s.height_falloff = -1.0));
    assert!(bad(|s| s.height_falloff = f32::INFINITY));
    assert!(bad(|s| s.height_falloff = 1.5));
    assert!(bad(|s| s.base_height = f32::NAN));
    assert!(bad(|s| s.base_height = 2.0e7));
    assert!(bad(|s| s.albedo[1] = 1.1));
    assert!(bad(|s| s.albedo[0] = -0.1));
    assert!(bad(|s| s.anisotropy = 0.995));
    assert!(bad(|s| s.anisotropy = f32::NAN));
    assert!(bad(|s| s.sun_intensity = -1.0));
    assert!(bad(|s| s.ambient_intensity = 1001.0));
    assert!(bad(|s| s.sky_distance = -1.0));
    assert!(bad(|s| s.sky_distance = 2.0e6));
    assert!(bad(|s| s.noise_amount = 1.1));
    assert!(bad(|s| s.noise_amount = f32::NAN));
    assert!(bad(|s| s.noise_scale = 0.1));
    assert!(bad(|s| s.noise_scale = f32::NAN));
    assert!(bad(|s| s.noise_distance = 0.0));
    assert!(bad(|s| s.noise_distance = 1.0e6));
    assert!(bad(|s| s.noise_steps = 0));
    assert!(bad(|s| s.noise_steps = MAX_NOISE_STEPS + 1));
    assert!(bad(|s| s.wind = [0.0, f32::NAN, 0.0]));
    assert!(bad(|s| s.wind = [0.0, 0.0, 201.0]));
    assert!(bad(|s| s.wind = [-250.0, 0.0, 0.0]));
    // The extremes of every range are valid.
    FogSettings {
        density: 0.0,
        height_falloff: 0.0,
        base_height: -MAX_BASE_HEIGHT,
        albedo: [0.0, 1.0, 0.0],
        anisotropy: -0.99,
        sun_intensity: MAX_INTENSITY,
        ambient_intensity: 0.0,
        sky_distance: MAX_SKY_DISTANCE,
        noise_amount: 1.0,
        noise_scale: 0.5,
        noise_distance: 1.0e5,
        noise_steps: MAX_NOISE_STEPS,
        noise_seed: u32::MAX,
        wind: [MAX_WIND, -MAX_WIND, 0.0],
    }
    .validate()
    .unwrap();
}

#[test]
fn frames_and_controls_validate_without_mutation() {
    let controls = FogControls::default();
    let (a, b) = (ViewId(3), ViewId(4));
    let frame = FogFrame::new(FogSettings::default(), [1.0, 2.0, 3.0], 5.0).unwrap();
    controls.set(a, Some(frame)).unwrap();
    assert_eq!(controls.get(a).unwrap(), Some(frame));
    assert!(controls.get(b).unwrap().is_none());
    let invalid = FogFrame {
        settings: FogSettings {
            density: -1.0,
            ..Default::default()
        },
        ..frame
    };
    assert!(controls.set(a, Some(invalid)).is_err());
    assert_eq!(
        controls.get(a).unwrap(),
        Some(frame),
        "rejected set kept state"
    );
    assert!(FogFrame::new(FogSettings::default(), [0.0, f64::NAN, 0.0], 0.0).is_err());
    assert!(FogFrame::new(FogSettings::default(), [0.0; 3], f64::INFINITY).is_err());
    controls.set(b, Some(frame)).unwrap();
    assert_eq!(controls.view_count(), 2);
    controls.set(a, None).unwrap();
    assert_eq!((controls.view_count(), controls.get(a).unwrap()), (1, None));
}

#[test]
fn absolute_position_is_summed_in_f64_and_rebasing_invariant() {
    let a = absolute_position([0.0; 3], [10.0, 5000.0, -3.0]);
    let b = absolute_position([4000.0, 4000.0, 0.0], [-3990.0, 1000.0, -3.0]);
    assert_eq!(a, [10.0, 5000.0, -3.0]);
    assert_eq!(b, [10.0, 5000.0, -3.0]);
    let far = absolute_position([1.0e9, -1.0e9, 3.0e8], [1.7, 0.25, 0.5]);
    assert!((far[0] - 1_000_000_001.7).abs() < 1e-5);
    assert!((far[1] - (-999_999_999.75)).abs() < 1e-5);
    // The height above the base uses the f64 sum too.
    let frame = FogFrame::new(
        FogSettings {
            base_height: 100.0,
            ..Default::default()
        },
        far,
        0.0,
    )
    .unwrap();
    assert!((frame.camera_height() - (-1_000_000_099.75)).abs() < 1e-5);
}

#[test]
fn height_depth_matches_numeric_integration() {
    // (density, falloff, h0, dy, length): up, down, level, below the base,
    // steep falloff, uniform fog.
    for (d, k, h0, dy, len) in [
        (0.02, 0.1, 2.0, 0.3, 500.0),
        (0.02, 0.1, 40.0, -0.5, 80.0),
        (0.05, 0.05, 1.0, 0.0, 200.0),
        (0.01, 0.2, -5.0, 0.7, 60.0),
        (0.5, 1.0, 3.0, 0.9, 20.0),
        (0.03, 0.0, 7.0, 0.4, 300.0),
        (0.03, 0.1, 10.0, 0.001, 50.0),
    ] {
        let exact = height_optical_depth(d, k, h0, dy, len);
        let numeric = numeric_depth(d, k, h0, dy, len);
        assert!(
            (exact - numeric).abs() <= 1e-6 * numeric.abs().max(1.0),
            "d={d} k={k} h0={h0} dy={dy} L={len}: {exact} vs {numeric}"
        );
    }
}

#[test]
fn height_depth_is_continuous_across_the_series_switch() {
    // |k dy L| = 0.01 is where the series gives way to the exponential form.
    for dy in [1.0, -1.0] {
        let (d, k, h0) = (0.1, 0.1, 3.0);
        let at = |x: f64| height_optical_depth(d, k, h0, dy, x / (k * dy.abs()));
        for x in [0.0099999, 0.01, 0.0100001] {
            let want = numeric_depth(d, k, h0, dy, x / (k * dy.abs()));
            assert!(
                (at(x) - want).abs() <= 1e-9 * want.abs().max(1e-3),
                "dy={dy} x={x}: {} vs {want}",
                at(x)
            );
        }
    }
    let (d, k, h0, dy) = (0.1, 0.1, 3.0, 1.0);
    assert!(height_optical_depth(d, k, h0, dy, 0.0) == 0.0);
}

#[test]
fn height_depth_stays_finite_and_nonnegative_in_extreme_geometry() {
    for h0 in [-1.0e7, -1.0e4, -300.0, 0.0, 300.0, 1.0e4, 1.0e9] {
        for dy in [-1.0, -0.3, -1e-6, 0.0, 1e-6, 0.3, 1.0] {
            for len in [0.0, 1.0, 1.0e3, 1.0e6] {
                let t = height_optical_depth(1.0, 1.0, h0, dy, len);
                assert!(!t.is_nan() && t >= 0.0, "h0={h0} dy={dy} len={len}: {t}");
            }
        }
    }
    // Looking up out of the layer: depth converges to d e^{-k h0} / (k dy).
    let t = height_optical_depth(0.02, 0.1, 5.0, 0.5, 1.0e9);
    let limit = 0.02 * (-0.5f64).exp() / (0.1 * 0.5);
    assert!((t - limit).abs() < 1e-9, "{t} vs {limit}");
}

#[test]
fn uniform_fog_follows_beer_lambert_and_density_orders_transmittance() {
    let t = height_transmittance(0.02, 0.0, 10.0, 0.0, 0.4, 100.0);
    assert!((t - (-2.0f64).exp()).abs() < 1e-12);
    let thin = height_transmittance(0.01, 0.1, 2.0, 0.0, 0.1, 300.0);
    let thick = height_transmittance(0.02, 0.1, 2.0, 0.0, 0.1, 300.0);
    assert!(thick < thin && thin < 1.0);
    // The same ray, camera and base shifted together.
    let shifted = height_transmittance(0.01, 0.1, 1002.0, 1000.0, 0.1, 300.0);
    assert!((shifted - thin).abs() < 1e-12);
}

#[test]
fn value_noise_is_deterministic_bounded_periodic_and_seeded() {
    let c = [3.7, -12.2, 100.9];
    assert_eq!(value_noise(c, 0), value_noise(c, 0));
    assert_ne!(value_noise(c, 0), value_noise(c, 1));
    let mut sum = 0.0;
    let mut count = 0.0;
    for i in 0..40 {
        for j in 0..40 {
            for k in 0..8 {
                let p = [
                    f64::from(i) * 0.37,
                    f64::from(j) * 0.41,
                    f64::from(k) * 0.53,
                ];
                let v = value_noise(p, 7);
                assert!((0.0..=1.0).contains(&v), "{v}");
                sum += v;
                count += 1.0;
            }
        }
    }
    let mean = sum / count;
    assert!((0.4..0.6).contains(&mean), "mean {mean}");
    // Lattice values are the cell hashes; the noise reproduces them exactly.
    assert_eq!(value_noise([5.0, 6.0, 7.0], 3), cell_value([5, 6, 7], 3));
    // Periodic: one noise period apart is the same value, negative cells wrap.
    let period = f64::from(NOISE_PERIOD);
    let p = [1.3, 2.6, 3.9];
    assert!(
        (value_noise(p, 2) - value_noise([p[0] + period, p[1] - period, p[2]], 2)).abs() < 1e-9
    );
    assert_eq!(
        cell_value([-1, 0, 0], 4),
        cell_value([NOISE_PERIOD as i64 - 1, 0, 0], 4)
    );
    // Smooth: nearby samples are close.
    let a = value_noise([4.5, 4.5, 4.5], 1);
    let b = value_noise([4.5001, 4.5, 4.5], 1);
    assert!((a - b).abs() < 1e-3);
}

#[test]
fn noise_origin_is_wrapped_rebasing_invariant_and_moves_with_the_wind() {
    let period = f64::from(NOISE_PERIOD);
    let wind = [3.0, 0.0, -1.5];
    let camera = [1500.0, 12.0, -800.0];
    let origin = noise_origin(camera, wind, 10.0, 5.0);
    assert!(origin.iter().all(|v| (0.0..period).contains(v)));
    // At time zero the origin is the camera in cells.
    let at_zero = noise_origin(camera, wind, 0.0, 5.0);
    assert_eq!(at_zero, [300.0, 2.4, (-160.0f64).rem_euclid(period)]);
    // The pattern moves with the wind: waiting dt is standing -wind*dt away.
    let moved = noise_origin(
        [camera[0] - 30.0, camera[1], camera[2] + 15.0],
        wind,
        0.0,
        5.0,
    );
    for k in 0..3 {
        assert!((origin[k] - moved[k]).abs() < 1e-9, "axis {k}");
    }
    // The same absolute camera gives the same origin however it was rebased,
    // even for huge coordinates and times.
    let far = FogFrame::new(
        FogSettings {
            wind,
            ..Default::default()
        },
        absolute_position([1.0e9, 0.0, -2.0e9], [3.25, 1.0, 0.5]),
        1.0e6,
    )
    .unwrap();
    let rebased = FogFrame {
        camera_position: absolute_position([1.0e9 + 2000.0, 0.0, -2.0e9], [-1996.75, 1.0, 0.5]),
        ..far
    };
    for k in 0..3 {
        assert!((far.noise_origin()[k] - rebased.noise_origin()[k]).abs() < 1e-6);
    }
    // Zero wind is static in time.
    let still = FogSettings::default();
    let (a, b) = (
        FogFrame::new(still, camera, 0.0).unwrap(),
        FogFrame::new(still, camera, 1.0e5).unwrap(),
    );
    assert_eq!(a.noise_origin(), b.noise_origin());
}

fn frame_with(settings: FogSettings, camera: [f64; 3], time: f64) -> FogFrame {
    FogFrame::new(settings, camera, time).unwrap()
}

#[test]
fn optical_depth_without_noise_is_the_analytic_height_integral() {
    let s = FogSettings::default();
    let frame = frame_with(s, [0.0, 3.0, 0.0], 0.0);
    let dir = crate::atmosphere::normalize([0.3, 0.2, -1.0]);
    let tau = optical_depth(&frame, dir, 250.0);
    let want = height_optical_depth(f64::from(0.01f32), f64::from(0.1f32), 3.0, dir[1], 250.0);
    assert_eq!(tau, want);
    // Noise on but a vanishing amount converges to the same value.
    let faint = frame_with(
        FogSettings {
            noise_amount: 1e-9,
            ..s
        },
        [0.0, 3.0, 0.0],
        0.0,
    );
    assert!((optical_depth(&faint, dir, 250.0) - want).abs() < 1e-6 * want);
}

#[test]
fn noise_modulates_within_its_bounds_and_only_inside_the_noise_distance() {
    let base = FogSettings {
        height_falloff: 0.0,
        noise_amount: 0.6,
        noise_scale: 15.0,
        noise_distance: 120.0,
        noise_steps: 32,
        ..Default::default()
    };
    let dir = [0.0, 0.0, -1.0];
    let flat = height_optical_depth(f64::from(0.01f32), 0.0, 2.0, 0.0, 100.0);
    let mut seen = Vec::new();
    for x in 0..12 {
        let frame = frame_with(base, [f64::from(x) * 37.0, 2.0, 0.0], 0.0);
        let tau = optical_depth(&frame, dir, 100.0);
        assert!(
            (0.4 * flat..=1.6 * flat).contains(&tau),
            "tau {tau} outside modulation bounds of {flat}"
        );
        seen.push(tau);
    }
    let (lo, hi) = seen
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
    assert!(hi - lo > 0.05 * flat, "noise varies the depth: {lo}..{hi}");
    // Beyond the noise distance the remainder is the unmodulated integral.
    let frame = frame_with(base, [0.0, 2.0, 0.0], 0.0);
    let near = optical_depth(&frame, dir, 120.0);
    let far = optical_depth(&frame, dir, 520.0);
    let rest = height_optical_depth(f64::from(0.01f32), 0.0, 2.0, 0.0, 400.0);
    assert!((far - near - rest).abs() < 1e-9);
    // Same time, same settings, same depth; another seed is another pattern.
    assert_eq!(
        optical_depth(&frame, dir, 100.0),
        optical_depth(&frame, dir, 100.0)
    );
    let reseeded = frame_with(
        FogSettings {
            noise_seed: 9,
            ..base
        },
        [0.0, 2.0, 0.0],
        0.0,
    );
    assert_ne!(
        optical_depth(&frame, dir, 100.0),
        optical_depth(&reseeded, dir, 100.0)
    );
}

#[test]
fn wind_over_time_equals_moving_the_camera_against_it() {
    let s = FogSettings {
        noise_amount: 0.8,
        noise_scale: 9.0,
        wind: [4.0, 0.0, 2.0],
        ..Default::default()
    };
    let dir = crate::atmosphere::normalize([0.2, 0.05, -1.0]);
    let at_time = frame_with(s, [10.0, 2.0, 5.0], 7.0);
    let moved = frame_with(s, [10.0 - 28.0, 2.0, 5.0 - 14.0], 0.0);
    let a = optical_depth(&at_time, dir, 150.0);
    let b = optical_depth(&moved, dir, 150.0);
    assert!((a - b).abs() < 1e-9 * a, "{a} vs {b}");
    let unmoved = frame_with(s, [10.0, 2.0, 5.0], 0.0);
    assert!((optical_depth(&unmoved, dir, 150.0) - a).abs() > 1e-6 * a);
}

#[test]
fn fogged_pixels_blend_the_scene_toward_the_source() {
    let s = FogSettings {
        density: 0.05,
        height_falloff: 0.0,
        albedo: [1.0, 0.5, 0.25],
        anisotropy: 0.7,
        ..Default::default()
    };
    let frame = frame_with(s, [0.0, 1.0, 0.0], 0.0);
    let scene = [0.5, 0.5, 0.5];
    let toward = [0.0, 0.0, -1.0];
    let sun = Some((toward, [2.0; 3]));
    let ambient = [0.1; 3];
    // No distance, no change; a very long path is the source colour.
    assert_eq!(fogged(&frame, toward, 0.0, scene, sun, ambient), scene);
    let source = source_radiance(&s, toward, sun, ambient);
    let far = fogged(&frame, toward, 1.0e5, scene, sun, ambient);
    for k in 0..3 {
        assert!((far[k] - source[k]).abs() < 1e-9);
    }
    // Looking at the sun is brighter than looking away from it (g > 0).
    let into = source_radiance(&s, toward, sun, ambient);
    let away = source_radiance(&s, [0.0, 0.0, 1.0], sun, ambient);
    assert!(into[0] > 2.0 * away[0], "{into:?} vs {away:?}");
    // Albedo scales the channels, ambient survives without a sun.
    let dark = source_radiance(&s, toward, None, ambient);
    assert_eq!(dark, [0.1, 0.05, 0.025].map(|v| v));
    // A zero-length weight leaves intermediate paths between scene and source.
    let mid = fogged(&frame, toward, 20.0, scene, sun, ambient);
    assert!(mid[0] > scene[0].min(source[0]) && mid[0] < scene[0].max(source[0]));
}

#[test]
fn uniform_layout_is_six_vec4s_and_carries_the_frame() {
    assert_eq!(std::mem::size_of::<FogUniform>(), 96);
    let settings = FogSettings {
        density: 0.02,
        height_falloff: 0.3,
        base_height: 10.0,
        noise_amount: 0.5,
        noise_scale: 8.0,
        noise_distance: 90.0,
        noise_steps: 24,
        noise_seed: 77,
        sun_intensity: 2.0,
        ambient_intensity: 3.0,
        sky_distance: 5000.0,
        wind: [8.0, 0.0, 0.0],
        ..Default::default()
    };
    let frame = FogFrame::new(settings, [80.0, 25.0, -16.0], 2.0).unwrap();
    let u = FogUniform::new(&frame);
    assert_eq!(u.albedo, [0.8, 0.85, 0.9, 0.3]);
    assert_eq!(u.medium, [0.02, 0.3, 15.0, 5000.0]);
    assert_eq!(u.noise, [0.5, 0.125, 90.0, 24.0]);
    // (80 - 8 * 2) / 8 = 8 cells; y: 25 / 8; z: -16 / 8 wraps to 1022.
    assert_eq!(u.origin, [8.0, 3.125, 1022.0, 0.0]);
    assert_eq!(u.lighting[..2], [2.0, 3.0]);
    assert_eq!(u.seed, [77, 0, 0, 0]);
}

#[test]
fn descriptor_is_disabled_and_runs_after_ssr_and_aerial_perspective() {
    let mut registry = crate::builtin_registry().unwrap();
    crate::atmosphere::register_features(&mut registry, &AtmosphereControls::default()).unwrap();
    register_features(&mut registry, &FogControls::default()).unwrap();
    let fog = registry.descriptor(FOG_FEATURE).unwrap();
    assert_eq!(fog.stage, Stage::OpaqueEffects);
    assert!(!fog.enabled_by_default);
    let names = registry.names();
    let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
    assert!(at("engine.surface_material") < at(FOG_FEATURE));
    assert!(at("engine.decals") < at("engine.ssr"));
    assert!(at("engine.ssr") < at(ATMOSPHERE_AERIAL_FEATURE));
    assert!(at(ATMOSPHERE_AERIAL_FEATURE) < at(FOG_FEATURE));
    assert!(at(FOG_FEATURE) < at("engine.transparent"));
    // Without the atmosphere registered the constraint is simply absent.
    let mut alone = crate::builtin_registry().unwrap();
    register_features(&mut alone, &FogControls::default()).unwrap();
    let names = alone.names();
    assert!(
        names.iter().position(|x| *x == "engine.ssr")
            < names.iter().position(|x| *x == FOG_FEATURE)
    );
}

#[test]
fn shader_composes_and_validates() {
    let composer = crate::ShaderComposer::new();
    let source = composer.compose(&pass_source()).unwrap();
    crate::validate_wgsl(&source).unwrap();
}
