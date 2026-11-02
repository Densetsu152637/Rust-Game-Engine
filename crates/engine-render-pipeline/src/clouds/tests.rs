use super::*;
use crate::atmosphere::{ATMOSPHERE_AERIAL_FEATURE, AtmosphereControls};
use crate::fog::{FOG_FEATURE, FogControls};

fn bad(change: impl FnOnce(&mut CloudSettings)) -> bool {
    let mut settings = CloudSettings::default();
    change(&mut settings);
    settings.validate().is_err()
}

/// A layer 100..300 m above sea level: thin enough to cross with few steps.
fn thin() -> CloudSettings {
    CloudSettings {
        base_altitude: 100.0,
        top_altitude: 300.0,
        noise_scale: 80.0,
        primary_steps: 96,
        ..Default::default()
    }
}

fn frame_with(settings: CloudSettings, camera: [f64; 3], time: f64) -> CloudFrame {
    CloudFrame::new(settings, camera, time).unwrap()
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|c| c / l)
}

#[test]
fn defaults_validate_and_every_range_is_enforced() {
    CloudSettings::default().validate().unwrap();
    assert!(bad(|s| s.base_altitude = f32::NAN));
    assert!(bad(|s| s.top_altitude = f32::INFINITY));
    assert!(bad(|s| s.base_altitude = 2.0e7));
    // Base at or above the top is rejected.
    assert!(bad(|s| s.top_altitude = s.base_altitude));
    assert!(bad(|s| s.top_altitude = s.base_altitude - 10.0));
    assert!(bad(|s| s.top_altitude = s.base_altitude + 0.5));
    assert!(bad(|s| s.top_altitude = s.base_altitude + 2.0e5));
    assert!(bad(|s| s.density = f32::NAN));
    assert!(bad(|s| s.density = -0.1));
    assert!(bad(|s| s.density = 1.5));
    assert!(bad(|s| s.coverage = -0.1));
    assert!(bad(|s| s.coverage = 1.1));
    assert!(bad(|s| s.coverage = f32::NAN));
    assert!(bad(|s| s.albedo[2] = 1.1));
    assert!(bad(|s| s.anisotropy = 0.995));
    assert!(bad(|s| s.anisotropy = f32::NAN));
    assert!(bad(|s| s.sun_intensity = -1.0));
    assert!(bad(|s| s.ambient_intensity = 1001.0));
    assert!(bad(|s| s.noise_amount = 1.1));
    assert!(bad(|s| s.noise_scale = 0.5));
    assert!(bad(|s| s.noise_scale = f32::NAN));
    assert!(bad(|s| s.noise_octaves = 0));
    assert!(bad(|s| s.noise_octaves = MAX_OCTAVES + 1));
    assert!(bad(|s| s.wind = [0.0, f32::NAN, 0.0]));
    assert!(bad(|s| s.wind = [201.0, 0.0, 0.0]));
    assert!(bad(|s| s.max_distance = 0.0));
    assert!(bad(|s| s.max_distance = 2.0e6));
    assert!(bad(|s| s.fade_distance = -1.0));
    assert!(bad(|s| s.fade_distance = 2.0e7));
    assert!(bad(|s| s.primary_steps = 0));
    assert!(bad(|s| s.primary_steps = MAX_PRIMARY_STEPS + 1));
    assert!(bad(|s| s.light_steps = 0));
    assert!(bad(|s| s.light_steps = MAX_LIGHT_STEPS + 1));
    assert!(bad(|s| s.light_distance = 0.0));
    assert!(bad(|s| s.light_distance = f32::INFINITY));
    // The extremes of every range are valid.
    CloudSettings {
        base_altitude: -MAX_ALTITUDE,
        top_altitude: -MAX_ALTITUDE + MAX_THICKNESS,
        density: MAX_DENSITY,
        coverage: 1.0,
        albedo: [0.0, 1.0, 0.0],
        anisotropy: -0.99,
        sun_intensity: MAX_INTENSITY,
        ambient_intensity: 0.0,
        noise_amount: 0.0,
        noise_scale: 1.0e6,
        noise_octaves: MAX_OCTAVES,
        wind: [-MAX_WIND, 0.0, MAX_WIND],
        max_distance: 1.0e6,
        fade_distance: MAX_FADE_DISTANCE,
        primary_steps: MAX_PRIMARY_STEPS,
        light_steps: MAX_LIGHT_STEPS,
        light_distance: 1.0e5,
        ..Default::default()
    }
    .validate()
    .unwrap();
    // Visibility.
    assert!(CloudSettings::default().is_visible());
    assert!(
        !CloudSettings {
            density: 0.0,
            ..Default::default()
        }
        .is_visible()
    );
    assert!(
        !CloudSettings {
            coverage: 0.0,
            ..Default::default()
        }
        .is_visible()
    );
}

#[test]
fn frames_and_controls_validate_without_mutation() {
    assert!(CloudFrame::new(CloudSettings::default(), [f64::NAN, 0.0, 0.0], 0.0).is_err());
    assert!(CloudFrame::new(CloudSettings::default(), [0.0; 3], f64::INFINITY).is_err());
    let controls = CloudControls::default();
    let good = frame_with(CloudSettings::default(), [1.0, 2.0, 3.0], 4.0);
    controls.set(ViewId(1), Some(good)).unwrap();
    let mut invalid = good;
    invalid.settings.coverage = 2.0;
    assert!(controls.set(ViewId(1), Some(invalid)).is_err());
    assert_eq!(controls.get(ViewId(1)).unwrap(), Some(good));
    assert_eq!(controls.view_count(), 1);
    controls.set(ViewId(1), None).unwrap();
    assert!(controls.get(ViewId(1)).unwrap().is_none());
    assert_eq!(controls.view_count(), 0);
}

#[test]
fn the_profile_integrates_to_three_quarters() {
    let n = 200_000;
    let sum: f64 = (0..n)
        .map(|i| profile((f64::from(i) + 0.5) / f64::from(n)))
        .sum::<f64>()
        / f64::from(n);
    assert!((sum - PROFILE_INTEGRAL).abs() < 1e-6, "{sum}");
    assert_eq!(profile(-0.1), 0.0);
    assert_eq!(profile(0.0), 0.0);
    assert_eq!(profile(1.0), 0.0);
    assert_eq!(profile(1.5), 0.0);
    assert_eq!(profile(0.3), 1.0);
    assert!(profile(0.05) > 0.0 && profile(0.05) < 1.0);
    assert!(profile(0.8) > 0.0 && profile(0.8) < 1.0);
}

#[test]
fn coverage_and_density_stay_within_their_ranges() {
    assert_eq!(remap(0.4, 0.0), 0.0);
    assert_eq!(remap(1.0, 0.0), 0.0);
    // Full coverage passes the noise through.
    assert!((remap(0.37, 1.0) - 0.37).abs() < 1e-12);
    // Half coverage clips the lower half and stretches the rest.
    assert_eq!(remap(0.5, 0.5), 0.0);
    assert!((remap(0.75, 0.5) - 0.5).abs() < 1e-12);
    assert_eq!(remap(1.0, 0.5), 1.0);
    let mut s = thin();
    for coverage in [0.0f32, 0.2, 0.5, 0.9, 1.0] {
        s.coverage = coverage;
        for i in 0..200 {
            let f = f64::from(i);
            let c = [f * 0.37, f * 0.11, f * 0.73];
            let sigma = extinction(&s, c, 20.0 + f * 0.9);
            assert!(
                sigma.is_finite() && (0.0..=f64::from(s.density)).contains(&sigma),
                "{sigma}"
            );
        }
    }
    // Outside the layer there is no extinction at all.
    s.coverage = 1.0;
    assert_eq!(extinction(&s, [1.0; 3], -1.0), 0.0);
    assert_eq!(extinction(&s, [1.0; 3], 200.0), 0.0);
    assert_eq!(extinction(&s, [1.0; 3], 500.0), 0.0);
    // More coverage never removes cloud from a point.
    let c = [3.3, 1.2, 8.8];
    let at = |coverage: f32| extinction(&CloudSettings { coverage, ..s }, c, 100.0);
    assert!(at(0.3) <= at(0.6) && at(0.6) <= at(0.9) && at(0.9) <= at(1.0));
}

#[test]
fn fractal_noise_is_deterministic_bounded_and_seeded() {
    let c = [12.3, 4.5, 678.9];
    assert_eq!(fbm(c, 3, 7), fbm(c, 3, 7));
    assert_ne!(fbm(c, 3, 7), fbm(c, 3, 8));
    assert_ne!(fbm(c, 1, 7), fbm(c, 4, 7), "octaves add detail");
    for i in 0..500 {
        let f = f64::from(i);
        let v = fbm([f * 0.31, f * 0.17, f * 0.53], 4, 3);
        assert!((0.0..=1.0).contains(&v), "{v}");
    }
    // The lattice period wraps every octave.
    let period = f64::from(model::NOISE_PERIOD);
    for octaves in 1..=4 {
        let a = fbm([1.7, 2.3, 3.1], octaves, 9);
        let b = fbm([1.7 + period, 2.3 - period, 3.1 + 2.0 * period], octaves, 9);
        assert!((a - b).abs() < 1e-9, "octaves {octaves}");
    }
}

#[test]
fn layer_intervals_cover_the_crossed_slab_and_respect_the_limit() {
    let (lo, hi) = layer_interval(-100.0, 1.0, 200.0, 1.0e9).unwrap();
    assert!((lo - 100.0).abs() < 1e-9 && (hi - 300.0).abs() < 1e-9);
    // Slanted: distances scale with 1 / dy.
    let (lo, hi) = layer_interval(-100.0, 0.5, 200.0, 1.0e9).unwrap();
    assert!((lo - 200.0).abs() < 1e-9 && (hi - 600.0).abs() < 1e-9);
    // Inside the layer the ray starts at the camera.
    assert_eq!(layer_interval(50.0, 0.5, 200.0, 1.0e9).unwrap().0, 0.0);
    // From above looking down, from below looking down, and past the limit.
    let (lo, _) = layer_interval(300.0, -1.0, 200.0, 1.0e9).unwrap();
    assert!((lo - 100.0).abs() < 1e-9);
    assert!(layer_interval(-100.0, -1.0, 200.0, 1.0e9).is_none());
    assert!(layer_interval(400.0, 1.0, 200.0, 1.0e9).is_none());
    assert!(layer_interval(-100.0, 1.0, 200.0, 50.0).is_none());
    let (_, hi) = layer_interval(-100.0, 1.0, 200.0, 150.0).unwrap();
    assert!((hi - 150.0).abs() < 1e-9);
    // Horizontal rays only see the layer from inside it.
    assert_eq!(layer_interval(50.0, 0.0, 200.0, 777.0), Some((0.0, 777.0)));
    assert!(layer_interval(-50.0, 0.0, 200.0, 777.0).is_none());
    assert!(layer_interval(250.0, 0.0, 200.0, 777.0).is_none());
}

#[test]
fn a_constant_slab_matches_the_closed_form_transmittance() {
    // Noise off: a uniform layer. The camera is 100 m below it.
    for density in [0.002f32, 0.005, 0.01] {
        for dy in [1.0f64, 0.8, 0.5] {
            let settings = CloudSettings {
                density,
                noise_amount: 0.0,
                coverage: 0.7,
                ..thin()
            };
            let frame = frame_with(settings, [0.0, 0.0, 0.0], 0.0);
            let dir = unit([(1.0 - dy * dy).sqrt(), dy, 0.0]);
            let m = march(&frame, dir, None, None, [0.0; 3]);
            let want = slab_transmittance(f64::from(density), 200.0, dir[1]);
            assert!(
                (m.transmittance - want).abs() < 0.003 * want + 1e-6,
                "density {density} dy {dy}: {} vs {want}",
                m.transmittance
            );
            assert!((m.entry - 100.0 / dir[1]).abs() < 1e-6);
        }
    }
    // More density, lower transmittance.
    let t = |density: f32| {
        let settings = CloudSettings {
            density,
            noise_amount: 0.0,
            ..thin()
        };
        march(
            &frame_with(settings, [0.0; 3], 0.0),
            [0.0, 1.0, 0.0],
            None,
            None,
            [0.0; 3],
        )
        .transmittance
    };
    assert!(t(0.002) > t(0.004) && t(0.004) > t(0.008));
}

#[test]
fn coverage_lowers_the_transmittance_of_a_noisy_layer() {
    let at = |coverage: f32| {
        let settings = CloudSettings {
            coverage,
            density: 0.01,
            ..thin()
        };
        let frame = frame_with(settings, [0.0; 3], 0.0);
        let mut sum = 0.0;
        for i in 0..64 {
            let a = f64::from(i) / 64.0 * std::f64::consts::TAU;
            let dir = unit([a.cos(), 1.2, a.sin()]);
            sum += march(&frame, dir, None, None, [0.0; 3]).transmittance;
        }
        sum / 64.0
    };
    let (a, b, c) = (at(0.3), at(0.6), at(0.9));
    assert!(a > b && b > c, "{a} {b} {c}");
    assert_eq!(at(0.0), 1.0, "no coverage: no cloud");
}

#[test]
fn geometry_limits_the_march_and_clouds_in_front_cover_it() {
    let settings = CloudSettings {
        noise_amount: 0.0,
        density: 0.01,
        ..thin()
    };
    let frame = frame_with(settings, [0.0; 3], 0.0);
    let up = [0.0, 1.0, 0.0];
    let sky = march(&frame, up, None, None, [0.0; 3]);
    // Geometry nearer than the layer entry hides the clouds completely.
    let near = march(&frame, up, Some(50.0), None, [0.0; 3]);
    assert_eq!(near.transmittance, 1.0);
    assert_eq!(near.inscatter, [0.0; 3]);
    // Geometry inside the layer lets only part through.
    let inside = march(&frame, up, Some(200.0), None, [0.0; 3]);
    assert!(inside.transmittance < 1.0 && inside.transmittance > sky.transmittance);
    // Geometry beyond the layer sees the same clouds as the sky.
    let behind = march(&frame, up, Some(5000.0), None, [0.0; 3]);
    assert!((behind.transmittance - sky.transmittance).abs() < 1e-12);
    // The composite covers the farther surface and leaves the nearer alone.
    let scene = [0.5; 3];
    let ambient = [0.4; 3];
    let covered = composite(&frame, up, Some(5000.0), scene, None, ambient);
    assert_ne!(covered, scene);
    let kept = composite(&frame, up, Some(50.0), scene, None, ambient);
    assert_eq!(kept, scene);
}

#[test]
fn the_march_is_bounded_and_exits_early_in_opaque_cloud() {
    // Even an extreme layer terminates and stays finite.
    let settings = CloudSettings {
        density: 1.0,
        coverage: 1.0,
        noise_amount: 0.0,
        ..thin()
    };
    let frame = frame_with(settings, [0.0; 3], 0.0);
    let sun = Some((unit([0.3, 1.0, 0.2]), [3.0; 3]));
    let m = march(&frame, [0.0, 1.0, 0.0], None, sun, [0.2; 3]);
    assert!(m.transmittance < CUTOFF_TRANSMITTANCE);
    assert!(m.transmittance >= 0.0);
    assert!(m.inscatter.iter().all(|v| v.is_finite() && *v >= 0.0));
    // The march never reads past the maximum distance.
    let short = CloudSettings {
        max_distance: 50.0,
        ..thin()
    };
    let m = march(
        &frame_with(short, [0.0; 3], 0.0),
        [0.0, 1.0, 0.0],
        None,
        None,
        [0.0; 3],
    );
    assert_eq!(m.transmittance, 1.0, "the layer starts beyond 50 m");
    // A large primary step count changes the answer by little: the result
    // has converged at the documented defaults.
    let at = |steps: u32| {
        let settings = CloudSettings {
            primary_steps: steps,
            noise_amount: 0.0,
            density: 0.005,
            ..thin()
        };
        march(
            &frame_with(settings, [0.0; 3], 0.0),
            [0.0, 1.0, 0.0],
            None,
            None,
            [0.0; 3],
        )
        .transmittance
    };
    assert!((at(24) - at(96)).abs() < 0.01);
}

fn lit(settings: CloudSettings, sun: [f64; 3], dir: [f64; 3]) -> CloudMarch {
    march(
        &frame_with(settings, [0.0; 3], 0.0),
        dir,
        None,
        Some((unit(sun), [3.0; 3])),
        [0.0; 3],
    )
}

#[test]
fn sun_light_scatters_forward_and_follows_the_sun() {
    let settings = CloudSettings {
        noise_amount: 0.0,
        density: 0.004,
        anisotropy: 0.7,
        ..thin()
    };
    // Looking at the sun (forward scatter) is brighter than away from it.
    let sun = [0.0, 1.0, 1.0];
    let toward = lit(settings, sun, unit([0.0, 1.0, 1.0]));
    let away = lit(settings, sun, unit([0.0, 1.0, -1.0]));
    assert!(
        toward.inscatter[0] > 1.5 * away.inscatter[0],
        "silver lining: {} vs {}",
        toward.inscatter[0],
        away.inscatter[0]
    );
    // Moving the sun to the other side swaps them.
    let flipped = lit(settings, [0.0, 1.0, -1.0], unit([0.0, 1.0, 1.0]));
    assert!(flipped.inscatter[0] < toward.inscatter[0]);
    // A sun below the horizon plane contributes nothing.
    let night = lit(settings, [0.0, -1.0, 1.0], unit([0.0, 1.0, 1.0]));
    assert_eq!(night.inscatter, [0.0; 3]);
    // The sun intensity scales the in-scatter.
    let doubled = lit(
        CloudSettings {
            sun_intensity: 2.0,
            ..settings
        },
        sun,
        unit([0.0, 1.0, 1.0]),
    );
    assert!((doubled.inscatter[0] / toward.inscatter[0] - 2.0).abs() < 1e-9);
    // The ambient term is separate and lights an unlit layer.
    let ambient = march(
        &frame_with(settings, [0.0; 3], 0.0),
        [0.0, 1.0, 0.0],
        None,
        None,
        [0.5; 3],
    );
    assert!(ambient.inscatter[0] > 0.0);
}

#[test]
fn self_shadowing_darkens_the_base_and_multiple_scattering_brightens() {
    let settings = CloudSettings {
        noise_amount: 0.0,
        density: 0.01,
        coverage: 1.0,
        ..thin()
    };
    let c = [1.0, 1.0, 1.0];
    let sun = unit([0.0, 1.0, 0.2]);
    let dir = [0.0, 1.0, 0.0];
    let base = source(&settings, c, 40.0, dir, Some((sun, [3.0; 3])), [0.0; 3]);
    let top = source(&settings, c, 160.0, dir, Some((sun, [3.0; 3])), [0.0; 3]);
    assert!(
        base[0] < 0.9 * top[0],
        "the base sees the sun through more cloud: {} vs {}",
        base[0],
        top[0]
    );
    // The optical depth toward the sun falls with height.
    let depth = |y| light_depth(&settings, c, y, sun);
    assert!(depth(40.0) > depth(160.0));
    // Multiple scattering adds light, most inside thick cloud.
    let single = CloudSettings {
        multiple_scattering: false,
        ..settings
    };
    let s = source(&single, c, 40.0, dir, Some((sun, [3.0; 3])), [0.0; 3]);
    assert!(base[0] > s[0]);
    // Albedo tints.
    let tinted = CloudSettings {
        albedo: [1.0, 0.5, 0.0],
        ..settings
    };
    let t = source(&tinted, c, 40.0, dir, Some((sun, [3.0; 3])), [0.0; 3]);
    assert!((t[1] - 0.5 * base[1]).abs() < 1e-12 && t[2] == 0.0);
}

#[test]
fn wind_over_time_equals_moving_the_camera_against_it() {
    let settings = CloudSettings {
        wind: [8.0, 0.0, -4.0],
        ..thin()
    };
    let moved = frame_with(settings, [100.0, 50.0, 30.0], 3.0);
    let calm = CloudSettings {
        wind: [0.0; 3],
        ..settings
    };
    let stand_in = frame_with(calm, [100.0 - 24.0, 50.0, 30.0 + 12.0], 0.0);
    for k in 0..3 {
        assert!((moved.noise_origin()[k] - stand_in.noise_origin()[k]).abs() < 1e-9);
    }
    let dir = unit([0.3, 0.8, -0.2]);
    let a = march(&moved, dir, None, None, [0.1; 3]);
    let b = march(&stand_in, dir, None, None, [0.1; 3]);
    assert!((a.transmittance - b.transmittance).abs() < 1e-12);
    // Zero wind: the clock changes nothing.
    let a = frame_with(calm, [1.0, 2.0, 3.0], 0.0).noise_origin();
    let b = frame_with(calm, [1.0, 2.0, 3.0], 1.0e6).noise_origin();
    assert_eq!(a, b);
    // Absolute positions are rebasing invariant.
    let p = absolute_position([1.0e9, 0.0, 0.0], [1.5, 2.0, 0.0]);
    assert_eq!(p, [1_000_000_001.5, 2.0, 0.0]);
}

#[test]
fn the_horizon_fade_reduces_the_cloud_contribution_with_distance() {
    let settings = CloudSettings {
        noise_amount: 0.0,
        density: 0.01,
        fade_distance: 500.0,
        ..thin()
    };
    assert_eq!(fade(&CloudSettings::default(), 1.0e6), 1.0);
    assert!((fade(&settings, 500.0) - (-1.0f64).exp()).abs() < 1e-12);
    let near = frame_with(settings, [0.0; 3], 0.0);
    let scene = [0.5; 3];
    let ambient = [0.4; 3];
    let steep = composite(&near, [0.0, 1.0, 0.0], None, scene, None, ambient);
    let flat = composite(&near, unit([1.0, 0.2, 0.0]), None, scene, None, ambient);
    // The flat ray enters the layer farther away: closer to the plain scene.
    assert!((flat[0] - scene[0]).abs() < (steep[0] - scene[0]).abs());
}

#[test]
fn uniform_layout_is_seven_vec4s_and_carries_the_frame() {
    assert_eq!(std::mem::size_of::<CloudUniform>(), 112);
    let settings = CloudSettings {
        base_altitude: 1000.0,
        top_altitude: 3000.0,
        density: 0.01,
        coverage: 0.6,
        noise_amount: 0.5,
        noise_scale: 8.0,
        noise_octaves: 2,
        noise_seed: 77,
        sun_intensity: 2.0,
        ambient_intensity: 3.0,
        wind: [8.0, 0.0, 0.0],
        max_distance: 9000.0,
        fade_distance: 7000.0,
        primary_steps: 40,
        light_steps: 5,
        light_distance: 600.0,
        multiple_scattering: false,
        ..Default::default()
    };
    let frame = CloudFrame::new(settings, [80.0, 1025.0, -16.0], 2.0).unwrap();
    let u = CloudUniform::new(&frame);
    assert_eq!(u.albedo, [1.0, 1.0, 1.0, 0.6]);
    assert_eq!(u.layer, [2000.0, 25.0, 9000.0, 7000.0]);
    assert_eq!(u.density, [0.01, 0.6, 0.5, 0.125]);
    assert_eq!(u.steps, [2.0, 40.0, 5.0, 600.0]);
    // (80 - 8 * 2) / 8 = 8 cells; y: 1025 / 8; z: -16 / 8 wraps to 1022.
    assert_eq!(u.origin, [8.0, 128.125, 1022.0, 0.0]);
    assert_eq!(u.lighting[..2], [2.0, 3.0]);
    assert_eq!(u.seed, [77, 0, 0, 0]);
    let on = CloudUniform::new(&CloudFrame {
        settings: CloudSettings {
            multiple_scattering: true,
            ..settings
        },
        ..frame
    });
    assert_eq!(on.origin[3], 1.0);
}

#[test]
fn descriptor_is_disabled_after_aerial_perspective_and_before_fog() {
    let mut registry = crate::builtin_registry().unwrap();
    crate::atmosphere::register_features(&mut registry, &AtmosphereControls::default()).unwrap();
    crate::fog::register_features(&mut registry, &FogControls::default()).unwrap();
    register_features(&mut registry, &CloudControls::default()).unwrap();
    let clouds = registry.descriptor(CLOUDS_FEATURE).unwrap();
    assert_eq!(clouds.stage, Stage::OpaqueEffects);
    assert!(!clouds.enabled_by_default);
    let names = registry.names();
    let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
    assert!(at("engine.decals") < at("engine.ssr"));
    assert!(at("engine.ssr") < at(CLOUDS_FEATURE));
    assert!(at(ATMOSPHERE_AERIAL_FEATURE) < at(CLOUDS_FEATURE));
    assert!(at(CLOUDS_FEATURE) < at(FOG_FEATURE));
    assert!(at(FOG_FEATURE) < at("engine.transparent"));
    // Registration order does not matter.
    let mut reversed = crate::builtin_registry().unwrap();
    register_features(&mut reversed, &CloudControls::default()).unwrap();
    crate::fog::register_features(&mut reversed, &FogControls::default()).unwrap();
    let names = reversed.names();
    let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
    assert!(at(CLOUDS_FEATURE) < at(FOG_FEATURE));
}

#[test]
fn shader_composes_and_validates() {
    let composer = crate::ShaderComposer::new();
    let source = composer.compose(&pass_source()).unwrap();
    crate::validate_wgsl(&source).unwrap();
}
