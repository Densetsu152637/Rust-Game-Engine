//! Test 15 (library half): the 2D proxy reproduces the shoebox, scales with
//! `units_per_meter`, and opens to the sky.

use engine_acoustics::reference::*;
use engine_acoustics::*;

const ALPHA: f32 = 0.2;

fn material() -> AcousticMaterial {
    AcousticMaterial {
        absorption: [ALPHA; 4],
        scattering: 1.0,
        transmission: [0.0; 4],
    }
}

fn rect(center: [f32; 2], half_extents: [f32; 2], m: MaterialId) -> Footprint {
    Footprint {
        shape: Footprint2D::Rect {
            center,
            half_extents,
            rotation: 0.0,
        },
        material: m,
        mobility: Mobility::Static,
    }
}

/// Four wall boxes around an inner 10 x 6 unit room `[0, 10] x [0, 6]`, corners
/// overlapping so the enclosure has no other air.
fn walls(m: MaterialId, ceiling: bool) -> Vec<Footprint> {
    room_walls(m, [10.0, 6.0], ceiling)
}
/// Wall boxes 1 unit thick around an inner `[0, w] x [0, h]` room.
fn room_walls(m: MaterialId, [w, h]: [f32; 2], top: bool) -> Vec<Footprint> {
    let mut f = vec![
        rect([-0.5, h / 2.0], [0.5, h / 2.0 + 1.0], m),
        rect([w + 0.5, h / 2.0], [0.5, h / 2.0 + 1.0], m),
        rect([w / 2.0, -0.5], [w / 2.0 + 1.0, 0.5], m),
    ];
    if top {
        f.push(rect([w / 2.0, h + 0.5], [w / 2.0 + 1.0, 0.5], m));
    }
    f
}

fn settings(
    plane: ProxyPlane,
    units_per_meter: f32,
    extent_m: f32,
    m: MaterialId,
) -> Proxy2DSettings {
    Proxy2DSettings {
        plane,
        units_per_meter,
        depth_m: extent_m,
        enclose_depth: true,
        wall_height_m: extent_m,
        ear_height_m: 1.6,
        floor: true,
        ceiling: true,
        open_sky: false,
        enclosure: Enclosure::Auto { margin_m: 0.0 },
        material: m,
        floor_material: m,
        ceiling_material: m,
        side_material: m,
    }
}

fn build(
    footprints: impl Fn(MaterialId) -> Vec<Footprint>,
    s: impl Fn(MaterialId) -> Proxy2DSettings,
) -> (AcousticWorld, ProxyMapping) {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material()).unwrap();
    let mapping = b.add_proxy_2d(&footprints(m), &s(m)).unwrap();
    (b.build().unwrap(), mapping)
}

fn trace(world: &AcousticWorld, at: [f32; 3]) -> (EnergyHistogram, [BandFit; 4]) {
    let job = EnergyJob {
        emitter: at,
        receiver: at,
        ..reference_job(65_536, 21)
    };
    let h = cpu_reference_histogram(world, &reference_trace_settings(3.0), &job, 8).unwrap();
    let fits = fit_bands(&h, 0.08).unwrap();
    (h, fits)
}

#[test]
fn side_view_proxy_matches_the_shoebox() {
    let (world, mapping) = build(
        |m| walls(m, true),
        |m| settings(ProxyPlane::SideView, 1.0, 8.0, m),
    );
    let air = mapping.air().unwrap();
    println!("proxy air {air:?}");
    assert!((air.volume_m3 - 480.0).abs() / 480.0 < 1e-3);
    assert!((air.surface_m2 - 376.0).abs() / 376.0 < 1e-3);
    assert_eq!(air.open_m2, 0.0);
    // Interior distances equal the shoebox's.
    let p = mapping.to_acoustic([3.7, 2.1, 99.0]);
    assert_eq!(p, [3.7, 2.1, 0.0]);
    for (dir, t) in [
        ([1.0, 0.0, 0.0], 6.3),
        ([0.0, -1.0, 0.0], 2.1),
        ([0.0, 0.0, 1.0], 4.0),
    ] {
        let hit = world.raycast(p, dir, f32::INFINITY, u8::MAX).unwrap();
        assert!((hit.t - t).abs() < 1e-5, "{dir:?}: {}", hit.t);
    }
    // Traced RT60 equals the analytic shoebox traced with the same material.
    let (_, proxy) = trace(&world, p);
    let shoebox = shoebox_world(SHOEBOX_DIMS_M, material()).unwrap();
    let (_, reference) = trace(&shoebox, [3.7, 2.1, 4.0]);
    let eyring = eyring_rt60(480.0, &[(376.0, [ALPHA; 4])], [0.0; 4]).unwrap();
    for b in 0..BANDS {
        let err = (proxy[b].rt60_s - reference[b].rt60_s).abs() / reference[b].rt60_s;
        println!(
            "band {b}: proxy {:.4} s, shoebox {:.4} s ({:+.2}%), eyring {:.3}",
            proxy[b].rt60_s,
            reference[b].rt60_s,
            err * 100.0,
            eyring[b]
        );
        assert!(err < 0.05);
    }
}

#[test]
fn halving_units_per_meter_doubles_rt60() {
    let (_, base) = build(
        |m| walls(m, true),
        |m| settings(ProxyPlane::SideView, 1.0, 8.0, m),
    );
    let (world, mapping) = build(
        |m| walls(m, true),
        |m| settings(ProxyPlane::SideView, 0.5, 16.0, m),
    );
    let air = mapping.air().unwrap();
    assert!((air.volume_m3 - 3840.0).abs() / 3840.0 < 1e-3);
    assert!((air.surface_m2 - 1504.0).abs() / 1504.0 < 1e-3);
    assert_eq!(mapping.metres_per_unit(), 2.0);
    let (world1, _) = build(
        |m| walls(m, true),
        |m| settings(ProxyPlane::SideView, 1.0, 8.0, m),
    );
    let p1 = base.to_acoustic([3.7, 2.1, 0.0]);
    let p2 = mapping.to_acoustic([3.7, 2.1, 0.0]);
    let (_, small) = trace(&world1, p1);
    let (_, large) = trace(&world, p2);
    let eyring = eyring_rt60(3840.0, &[(1504.0, [ALPHA; 4])], [0.0; 4]).unwrap();
    assert!((eyring[0] - 1.842).abs() < 2e-3, "eyring {}", eyring[0]);
    for b in 0..BANDS {
        let ratio = large[b].rt60_s / small[b].rt60_s;
        println!(
            "band {b}: {:.3} s / {:.3} s = {ratio:.3}",
            large[b].rt60_s, small[b].rt60_s
        );
        assert!((ratio - 2.0).abs() <= 0.2);
    }
}

fn assert_opens_to_the_sky(closed: &AcousticWorld, open: &AcousticWorld, at: [f32; 3]) {
    let (hc, fc) = trace(closed, at);
    let (ho, fo) = trace(open, at);
    for b in 0..BANDS {
        let drop_db = 10.0 * (fc[b].late_energy / fo[b].late_energy).log10();
        println!(
            "band {b}: escaped {:.3} -> {:.3}, late level drop {drop_db:.1} dB",
            hc.escaped, ho.escaped
        );
        assert!(drop_db >= 6.0, "band {b}");
    }
    assert!(hc.escaped < 1e-3 && ho.escaped > 0.3);
}

#[test]
fn removing_the_ceiling_opens_the_side_view_room() {
    let (closed, mapping) = build(
        |m| walls(m, true),
        |m| settings(ProxyPlane::SideView, 1.0, 8.0, m),
    );
    let (open, open_mapping) = build(
        |m| walls(m, false),
        |m| Proxy2DSettings {
            open_sky: true,
            ..settings(ProxyPlane::SideView, 1.0, 8.0, m)
        },
    );
    let air = open_mapping.air().unwrap();
    assert!(air.open_m2 > 79.0, "open top 10 x 8 m: {air:?}");
    assert_opens_to_the_sky(&closed, &open, mapping.to_acoustic([3.7, 2.1, 0.0]));
}

#[test]
fn top_down_room_matches_and_opens_without_a_ceiling() {
    // The same 10 x 6 x 8 m shoebox, laid out so the ceiling is the 10 x 8 m face
    // that the side-view test opens: footprint 10 x 8 units, walls 6 m high.
    let plan = |m| room_walls(m, [10.0, 8.0], true);
    let (closed, mapping) = build(plan, |m| settings(ProxyPlane::TopDown, 1.0, 6.0, m));
    let air = mapping.air().unwrap();
    assert!((air.volume_m3 - 480.0).abs() / 480.0 < 1e-3);
    assert!((air.surface_m2 - 376.0).abs() / 376.0 < 1e-3);
    let p = mapping.to_acoustic([3.7, 2.1, 0.0]);
    assert_eq!(p, [3.7, 1.6, -2.1]);
    let pose = mapping.pose_to_acoustic(ListenerPose {
        position: [3.7, 2.1, 0.0],
        forward: [0.0, 0.0, -1.0],
        up: [0.0, 1.0, 0.0],
    });
    assert_eq!((pose.forward, pose.up), ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]));
    let up = closed
        .raycast(p, [0.0, 1.0, 0.0], f32::INFINITY, u8::MAX)
        .unwrap();
    assert!((up.t - 4.4).abs() < 1e-5, "ceiling at 6 m");
    let (open, open_mapping) = build(plan, |m| Proxy2DSettings {
        ceiling: false,
        ..settings(ProxyPlane::TopDown, 1.0, 6.0, m)
    });
    assert!((open_mapping.air().unwrap().open_m2 - 80.0).abs() < 1e-3);
    assert_opens_to_the_sky(&closed, &open, p);
}

#[test]
fn shapes_validation_and_dynamic_footprints() {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material()).unwrap();
    let mut s = settings(ProxyPlane::SideView, 1.0, 4.0, m);
    s.enclosure = Enclosure::None;
    s.enclose_depth = false;
    let shapes = vec![
        Footprint {
            shape: Footprint2D::Circle {
                center: [0.0, 0.0],
                radius: 1.0,
            },
            material: m,
            mobility: Mobility::Static,
        },
        Footprint {
            shape: Footprint2D::Capsule {
                center: [5.0, 0.0],
                half_height: 1.0,
                radius: 0.5,
                rotation: 0.3,
            },
            material: m,
            mobility: Mobility::Static,
        },
        Footprint {
            // Clockwise input is accepted and reoriented.
            shape: Footprint2D::Polygon {
                points: vec![[10.0, 0.0], [10.0, 2.0], [12.0, 2.0], [12.0, 0.0]],
            },
            material: m,
            mobility: Mobility::Dynamic,
        },
    ];
    // Invalid inputs are rejected without changing the builder.
    let concave = Footprint {
        shape: Footprint2D::Polygon {
            points: vec![[0.0, 0.0], [4.0, 0.0], [1.0, 1.0], [0.0, 4.0]],
        },
        ..shapes[0].clone()
    };
    assert_eq!(
        b.add_proxy_2d(&[concave], &s).unwrap_err().code,
        "acoustics.proxy.invalid"
    );
    let bad = Proxy2DSettings {
        units_per_meter: 0.0,
        ..s
    };
    assert_eq!(
        b.add_proxy_2d(&shapes, &bad).unwrap_err().code,
        "acoustics.proxy.invalid"
    );
    let mapping = b.add_proxy_2d(&shapes, &s).unwrap();
    assert_eq!(
        b.add_proxy_2d(&shapes, &s).unwrap_err().code,
        "acoustics.proxy.invalid"
    );
    let mut world = b.build().unwrap();
    assert!(mapping.air().is_none());
    // 12-gon prism: 12 side quads plus two 12-gon caps; capsule 14-gon; box.
    assert_eq!(world.stats().triangles, (24 + 20) + (28 + 24) + (8 + 4));
    let ids = mapping.footprint_instances().to_vec();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(world.instance_mobility(ids[2]), Some(Mobility::Dynamic));
    // Circle of radius 1: ray from x = -5 hits at t = 4.
    let hit = world
        .raycast([-5.0, 0.0, 0.0], [1.0, 0.0, 0.0], 10.0, u8::MAX)
        .unwrap();
    assert!((hit.t - 4.0).abs() < 1e-5);
    // Move the dynamic box up by 10 units and rotate it 90 degrees about its centre.
    let motion = mapping.rigid_motion([0.0, 10.0], std::f32::consts::FRAC_PI_2, [11.0, 1.0]);
    world.set_instance_transform(ids[2], motion).unwrap();
    let hit = world
        .raycast([11.0, 5.0, 0.0], [0.0, 1.0, 0.0], 20.0, u8::MAX)
        .unwrap();
    assert!(
        (hit.t - 5.0).abs() < 1e-4,
        "box now spans y 10-12: {}",
        hit.t
    );
    assert!(
        world
            .raycast([11.0, -5.0, 0.0], [0.0, 1.0, 0.0], 6.0, u8::MAX)
            .is_none()
    );
}
