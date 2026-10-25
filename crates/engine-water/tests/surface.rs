//! Water surfaces: bounds, underwater fog, shoreline attenuation over a ground sampler, and
//! buoyancy impulses.
use engine_water::*;
use glam::{Vec2, Vec3};
use std::sync::Arc;

fn lake(waves: Waves) -> WaterSurface {
    WaterSurface {
        shape: WaterShape::Lake {
            half_extent: Vec2::new(10.0, 5.0),
        },
        waves,
        optics: WaterOptics::default(),
        shoreline: ShorelineSettings::default(),
        time: 2.0,
    }
}

fn gentle() -> Arc<GerstnerSet> {
    Arc::new(
        GerstnerSet::deep(&[GerstnerWave {
            wavelength: 8.0,
            amplitude: 0.3,
            steepness: 0.6,
            ..GerstnerWave::default()
        }])
        .unwrap(),
    )
}

#[test]
fn lakes_are_bounded_and_oceans_are_not() {
    let calm = lake(Waves::Flat);
    assert!(calm.sample(Vec2::new(9.9, 4.9), &NoGround).is_some());
    assert!(calm.sample(Vec2::new(10.1, 0.0), &NoGround).is_none());
    let flat = calm.sample(Vec2::ZERO, &NoGround).unwrap();
    assert_eq!(flat.height, 0.0);
    assert_eq!(flat.normal, Vec3::Y);
    let ocean = WaterSurface {
        shape: WaterShape::Ocean {
            half_extent: Vec2::splat(50.0),
        },
        ..lake(Waves::Gerstner(gentle()))
    };
    assert!(
        ocean
            .sample(Vec2::new(5000.0, -3000.0), &NoGround)
            .is_some()
    );
}

#[test]
fn underwater_reports_depth_and_fog() {
    let water = lake(Waves::Gerstner(gentle()));
    let p = Vec2::new(1.0, 1.0);
    let surface = water.sample(p, &NoGround).unwrap().height;
    let below = water
        .underwater(Vec3::new(p.x, surface - 3.0, p.y), &NoGround)
        .expect("3 m below the surface");
    assert!((below.depth - 3.0).abs() < 1.0e-4);
    assert_eq!(below.fog_color, water.optics.fog_color);
    assert_eq!(below.fog_density, 0.15);
    // Fog over 10 m: 1 - exp(-1.5).
    assert!((below.fog_factor(10.0) - (1.0 - (-1.5f32).exp())).abs() < 1.0e-6);
    // Red is absorbed fastest.
    let t = below.transmittance;
    assert!(t[0] < t[1] && t[1] < t[2]);
    assert!((t[0] - (-0.45f32 * 3.0).exp()).abs() < 1.0e-6);
    assert!(
        water
            .underwater(Vec3::new(p.x, surface + 0.1, p.y), &NoGround)
            .is_none()
    );
    // Outside the lake nothing is underwater.
    assert!(
        water
            .underwater(Vec3::new(30.0, -5.0, 0.0), &NoGround)
            .is_none()
    );
}

/// Waves fade and foam grows toward a beach (ground rising along +X through the mean level).
#[test]
fn shoreline_attenuates_waves_and_adds_foam_with_depth() {
    let water = lake(Waves::Gerstner(gentle()));
    let beach = |x: f32, _z: f32| Some(-6.0 + 0.6 * (x + 10.0));
    let mut last_amplitude = 2.0;
    let mut last_foam = -1.0;
    for x in [-9.0f32, -5.0, -1.0, 0.0, 2.0, 3.0, 4.0] {
        let depth = -(-6.0 + 0.6 * (x + 10.0));
        let s = water.sample(Vec2::new(x, 0.0), &beach).unwrap();
        let a = s.attenuation;
        eprintln!(
            "x {x}: depth {depth:.2} amplitude {:.3} foam {:.3}",
            a.amplitude, a.foam
        );
        assert!(a.amplitude <= last_amplitude + 1.0e-6);
        assert!(a.foam >= last_foam - 1.0e-6);
        assert!(s.height.abs() <= 0.3 * a.amplitude + 1.0e-4);
        last_amplitude = a.amplitude;
        last_foam = a.foam;
    }
    let settings = ShorelineSettings::default();
    assert_eq!(settings.attenuation(Some(10.0)), Attenuation::NONE);
    assert_eq!(settings.attenuation(None), Attenuation::NONE);
    let shore = settings.attenuation(Some(0.0));
    assert_eq!(shore.amplitude, 0.0);
    assert_eq!(shore.foam, 1.0);
    // Halfway through the fade band: smoothstep(0.5) = 0.5.
    assert!((settings.attenuation(Some(2.0)).amplitude - 0.5).abs() < 1.0e-6);
    let off = ShorelineSettings {
        enabled: false,
        ..settings
    };
    assert_eq!(off.attenuation(Some(0.0)), Attenuation::NONE);
}

#[test]
fn ocean_frames_drive_surface_samples() {
    let spectrum = OceanSpectrum::new(&OceanSettings::default(), GRAVITY).unwrap();
    let frame = Arc::new(spectrum.frame(4.0).unwrap());
    let water = WaterSurface {
        shape: WaterShape::Ocean {
            half_extent: Vec2::splat(64.0),
        },
        ..lake(Waves::Ocean(frame.clone()))
    };
    let s = water.sample(Vec2::new(10.0, 20.0), &NoGround).unwrap();
    let (point, _) = frame.sample(Vec2::new(10.0, 20.0));
    assert!((s.height - point.offset.y).abs() < 1.0e-6);
    assert!(s.normal.y > 0.0);
    // At the shoreline the attenuated ocean is flat.
    let shore = water
        .sample(Vec2::new(10.0, 20.0), &|_: f32, _: f32| Some(0.0))
        .unwrap();
    assert_eq!(shore.height, 0.0);
    assert_eq!(shore.normal, Vec3::Y);
}

#[test]
fn optics_validate_and_crest_foam() {
    let optics = WaterOptics::default();
    optics.validate().unwrap();
    assert_eq!(optics.crest_foam(1.0), 0.0);
    assert_eq!(optics.crest_foam(0.0), 1.0);
    assert!((optics.crest_foam(0.2) - 0.5).abs() < 1.0e-6);
    let bad = WaterOptics {
        fog_density: -1.0,
        ..optics
    };
    assert_eq!(bad.validate().unwrap_err().code, "water.optics.invalid");
}

fn calm(surface: f32) -> impl Fn(Vec3) -> Option<WaterAt> {
    move |_| {
        Some(WaterAt {
            surface,
            velocity: Vec3::ZERO,
            density: 1.0,
        })
    }
}

/// A 1 m box of density 0.5 (mass 0.5) is in equilibrium when half submerged: the buoyant
/// impulse cancels gravity's impulse exactly with 2 point layers.
#[test]
fn half_submerged_box_balances_its_weight() {
    let (local, height) = box_points(Vec3::splat(0.5), [2, 2, 2]);
    assert_eq!(local.len(), 8);
    assert_eq!(height, 0.5);
    let points: Vec<BuoyancyPoint> = local
        .iter()
        .map(|p| BuoyancyPoint {
            position: *p,
            volume: 1.0 / 8.0,
            height,
        })
        .collect();
    let body = BodyMotion {
        center: Vec3::ZERO,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        mass: 0.5,
    };
    let params = DragParams {
        linear_drag: 1.0,
        angular_drag: 1.0,
        flow_drag: 0.0,
        gravity: Vec3::new(0.0, -GRAVITY, 0.0),
    };
    let dt = 0.02;
    for (level, expected_volume) in [
        (0.0, 0.5),
        (0.25, 0.75),
        (-0.5, 0.0),
        (1.0, 1.0),
        (-0.1, 0.4),
    ] {
        let impulse = buoyancy_impulse(&points, &body, &params, dt, calm(level));
        assert!(
            (impulse.submerged_volume - expected_volume).abs() < 1.0e-5,
            "level {level}: {impulse:?}"
        );
        assert!((impulse.linear.y - expected_volume * GRAVITY * dt).abs() < 1.0e-5);
        assert!(impulse.angular.length() < 1.0e-6, "symmetric: no torque");
    }
    let half = buoyancy_impulse(&points, &body, &params, dt, calm(0.0));
    assert!((half.linear.y - body.mass * GRAVITY * dt).abs() < 1.0e-5);
    // A tilted water line (points at +x deeper) produces a righting torque about -z... here the
    // +x side is more submerged, so buoyancy rolls the box toward -x (positive z torque).
    let tilted = buoyancy_impulse(&points, &body, &params, dt, |p: Vec3| {
        Some(WaterAt {
            surface: 0.2 * p.x,
            velocity: Vec3::ZERO,
            density: 1.0,
        })
    });
    assert!(tilted.angular.z > 0.0);
}

#[test]
fn drag_pulls_toward_the_flow_without_overshoot() {
    let points = [BuoyancyPoint {
        position: Vec3::ZERO,
        volume: 1.0,
        height: 1.0,
    }];
    let body = BodyMotion {
        center: Vec3::ZERO,
        linear_velocity: Vec3::new(0.0, -1.0, 0.0),
        angular_velocity: Vec3::new(0.0, 2.0, 0.0),
        mass: 2.0,
    };
    let flow = |_: Vec3| {
        Some(WaterAt {
            surface: 10.0,
            velocity: Vec3::new(3.0, 0.0, 0.0),
            density: 1.0,
        })
    };
    let params = DragParams {
        linear_drag: 2.0,
        angular_drag: 0.5,
        flow_drag: 5.0,
        gravity: Vec3::ZERO,
    };
    let small = buoyancy_impulse(&points, &body, &params, 0.01, flow);
    // linear 2 * 0.01 on (u - v) plus flow 5 * 0.01 horizontally.
    assert!((small.linear.x - 2.0 * (0.02 + 0.05) * 3.0).abs() < 1.0e-5);
    assert!((small.linear.y - 2.0 * 0.02 * 1.0).abs() < 1.0e-5);
    // A huge step clamps the drag: the body reaches the flow velocity, never beyond.
    let big = buoyancy_impulse(&points, &body, &params, 10.0, flow);
    let after = body.linear_velocity + big.linear / body.mass;
    assert!(after.x <= 3.0 + 1.0e-5 && after.x > 0.0, "{after}");
    // Out of the water nothing acts.
    let dry = buoyancy_impulse(&points, &body, &params, 0.01, |_: Vec3| None);
    assert_eq!(dry, BuoyancyImpulse::default());
}
