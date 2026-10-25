//! Emitters, sinks, capacity and determinism.

mod common;

use common::{DT, block, params, tank};
use engine_fluids::{EmitterDesc, EmitterKind, Error, FluidVolume, Region};
use glam::Vec3;

fn rate_emitter(per_second: f32, lifetime: Option<f32>) -> EmitterDesc {
    EmitterDesc {
        kind: EmitterKind::Rate {
            region: Region::Box {
                center: Vec3::new(0.5, 0.6, 0.5),
                half_extents: Vec3::splat(0.2),
            },
            per_second,
            velocity: Vec3::ZERO,
            seed: 42,
        },
        lifetime,
        enabled: true,
    }
}

fn nozzle() -> EmitterDesc {
    EmitterDesc {
        kind: EmitterKind::Nozzle {
            position: Vec3::new(0.1, 0.5, 0.5),
            direction: Vec3::X,
            radius: 0.05,
            speed: 1.5,
        },
        lifetime: None,
        enabled: true,
    }
}

fn open_tank(capacity: usize) -> FluidVolume {
    tank(params(), Vec3::ZERO, Vec3::splat(1.0), capacity)
}

#[test]
fn rate_emitter_spawns_exact_counts() {
    let mut volume = open_tank(4096);
    volume.set_emitter(1, rate_emitter(120.0, None)).unwrap();
    for step in 1..=60 {
        let report = volume.step(DT, &[]).unwrap();
        assert_eq!(report.spawned, 2, "step {step}");
        assert_eq!(volume.len(), 2 * step);
    }
    // A fractional rate carries over: 50 per second at 60 Hz is 50 particles per 60 steps.
    let mut volume = open_tank(4096);
    volume.set_emitter(1, rate_emitter(50.0, None)).unwrap();
    for _ in 0..60 {
        volume.step(DT, &[]).unwrap();
    }
    assert_eq!(volume.len(), 50);
}

#[test]
fn nozzle_emits_disc_layers_one_spacing_apart() {
    let mut volume = open_tank(4096);
    volume.set_emitter(5, nozzle()).unwrap();
    // 1.5 m/s at 60 Hz travels half a 5 cm spacing per step: a 5-point disc every 2 steps.
    for step in 1..=60 {
        let report = volume.step(DT, &[]).unwrap();
        let expected = if step % 2 == 0 { 5 } else { 0 };
        assert_eq!(report.spawned, expected, "step {step}");
    }
    assert_eq!(volume.len(), 150);
    // Disabling the emitter stops emission.
    let mut desc = nozzle();
    desc.enabled = false;
    volume.set_emitter(5, desc).unwrap();
    for _ in 0..10 {
        assert_eq!(volume.step(DT, &[]).unwrap().spawned, 0);
    }
}

#[test]
fn lifetimes_and_kill_volumes_remove_particles() {
    let mut volume = open_tank(4096);
    volume
        .set_emitter(1, rate_emitter(120.0, Some(0.5)))
        .unwrap();
    let (mut spawned, mut expired) = (0, 0);
    let mut counts = Vec::new();
    for _ in 0..120 {
        let report = volume.step(DT, &[]).unwrap();
        spawned += report.spawned;
        expired += report.expired;
        counts.push(volume.len());
    }
    assert_eq!(spawned - expired, volume.len());
    // Steady state: rate * lifetime, within one step of emission.
    assert!(
        counts[60..].iter().all(|&c| (58..=60).contains(&c)),
        "{counts:?}"
    );
    assert!(volume.ages().iter().all(|&a| a < 0.5 + 1.0e-4));

    let mut volume = open_tank(4096);
    volume.set_emitter(5, nozzle()).unwrap();
    let sink = Region::Box {
        center: Vec3::new(0.85, 0.5, 0.5),
        half_extents: Vec3::new(0.15, 0.5, 0.5),
    };
    volume.set_kill_volume(1, sink).unwrap();
    let mut killed = 0;
    for _ in 0..120 {
        let report = volume.step(DT, &[]).unwrap();
        killed += report.killed;
        assert!(volume.positions().iter().all(|&x| !sink.contains(x)));
    }
    assert!(killed > 0);
    assert_eq!(volume.len() + killed, 300);
}

#[test]
fn capacity_is_enforced_with_explicit_status_and_error() {
    let mut volume = open_tank(50);
    volume.set_emitter(1, rate_emitter(120.0, None)).unwrap();
    let mut dropped = 0;
    let mut reached = false;
    for _ in 0..30 {
        let report = volume.step(DT, &[]).unwrap();
        dropped += report.dropped_at_capacity;
        reached = report.capacity_reached;
    }
    assert_eq!(volume.len(), 50);
    assert_eq!(dropped, 10);
    assert!(reached);
    let before = volume.positions().to_vec();
    let err = volume
        .spawn(&[Vec3::splat(0.5)], Vec3::ZERO, None)
        .unwrap_err();
    assert_eq!(
        err,
        Error::CapacityExceeded {
            requested: 1,
            available: 0
        }
    );
    assert_eq!(volume.positions(), &before[..]);
    let mut small = open_tank(10);
    let err = small
        .fill(block(Vec3::ZERO, Vec3::splat(0.5)), Vec3::ZERO, None)
        .unwrap_err();
    assert!(matches!(err, Error::CapacityExceeded { available: 10, .. }));
    assert!(small.is_empty());
    assert!(volume.set_capacity(10).is_err());
    volume.set_capacity(64).unwrap();
}

fn scripted_run() -> FluidVolume {
    let mut volume = open_tank(4096);
    volume
        .fill(
            block(Vec3::ZERO, Vec3::new(0.3, 0.3, 1.0)),
            Vec3::ZERO,
            None,
        )
        .unwrap();
    volume
        .set_emitter(1, rate_emitter(90.0, Some(0.8)))
        .unwrap();
    volume.set_emitter(2, nozzle()).unwrap();
    for _ in 0..60 {
        volume.step(DT, &[]).unwrap();
    }
    volume
}

#[test]
fn identical_runs_are_bitwise_identical() {
    let a = scripted_run();
    let b = scripted_run();
    assert_eq!(a.len(), b.len());
    assert_eq!(a.ids(), b.ids());
    let bits = |v: &FluidVolume| {
        v.positions()
            .iter()
            .chain(v.velocities())
            .flat_map(|x| x.to_array().map(f32::to_bits))
            .chain(v.densities().iter().map(|d| d.to_bits()))
            .collect::<Vec<u32>>()
    };
    assert_eq!(bits(&a), bits(&b));
}
