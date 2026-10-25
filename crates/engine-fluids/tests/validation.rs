//! Invalid inputs are rejected and leave state unchanged.

mod common;

use common::{DT, block, params, tank};
use engine_fluids::{
    ColliderProxy, Domain, EmitterDesc, EmitterKind, Error, FluidParams, FluidVolume, FluidWorld,
    MAX_PARTICLES, ProxyShape, Region, VolumeDesc,
};
use glam::{Quat, Vec3};

fn desc(params: FluidParams, capacity: usize) -> VolumeDesc {
    VolumeDesc {
        params,
        domain: Domain::new(Vec3::ZERO, Vec3::ONE),
        capacity,
    }
}

fn rejected(params: FluidParams) -> bool {
    matches!(FluidVolume::new(desc(params, 16)), Err(Error::Invalid(_)))
}

#[test]
fn invalid_parameters_are_rejected() {
    let base = params();
    assert!(FluidVolume::new(desc(base, 16)).is_ok());
    let cases: Vec<(&str, FluidParams)> = vec![
        (
            "nan radius",
            FluidParams {
                particle_radius: f32::NAN,
                ..base
            },
        ),
        (
            "zero radius",
            FluidParams {
                particle_radius: 0.0,
                ..base
            },
        ),
        (
            "negative radius",
            FluidParams {
                particle_radius: -0.1,
                ..base
            },
        ),
        (
            "huge radius",
            FluidParams {
                particle_radius: 100.0,
                ..base
            },
        ),
        (
            "small kernel",
            FluidParams {
                kernel_scale: 1.0,
                ..base
            },
        ),
        (
            "nan kernel",
            FluidParams {
                kernel_scale: f32::NAN,
                ..base
            },
        ),
        (
            "zero density",
            FluidParams {
                rest_density: 0.0,
                ..base
            },
        ),
        (
            "negative density",
            FluidParams {
                rest_density: -1000.0,
                ..base
            },
        ),
        (
            "nan density",
            FluidParams {
                rest_density: f32::NAN,
                ..base
            },
        ),
        (
            "infinite density",
            FluidParams {
                rest_density: f32::INFINITY,
                ..base
            },
        ),
        (
            "nan gravity",
            FluidParams {
                gravity: Vec3::new(0.0, f32::NAN, 0.0),
                ..base
            },
        ),
        (
            "zero substeps",
            FluidParams {
                substeps: 0,
                ..base
            },
        ),
        (
            "many substeps",
            FluidParams {
                substeps: 1000,
                ..base
            },
        ),
        (
            "zero iterations",
            FluidParams {
                iterations: 0,
                ..base
            },
        ),
        (
            "zero relaxation",
            FluidParams {
                relaxation: 0.0,
                ..base
            },
        ),
        (
            "viscosity above one",
            FluidParams {
                viscosity: 1.5,
                ..base
            },
        ),
        (
            "nan viscosity",
            FluidParams {
                viscosity: f32::NAN,
                ..base
            },
        ),
        (
            "negative vorticity",
            FluidParams {
                vorticity: -1.0,
                ..base
            },
        ),
        (
            "tensile k",
            FluidParams {
                tensile_k: 2.0,
                ..base
            },
        ),
        (
            "zero max speed",
            FluidParams {
                max_speed: 0.0,
                ..base
            },
        ),
        (
            "nan surface",
            FluidParams {
                surface_threshold: f32::NAN,
                ..base
            },
        ),
    ];
    for (name, case) in cases {
        assert!(rejected(case), "{name} accepted");
    }
}

#[test]
fn invalid_capacity_and_domain_are_rejected() {
    let base = params();
    assert!(FluidVolume::new(desc(base, 0)).is_err());
    assert!(FluidVolume::new(desc(base, MAX_PARTICLES + 1)).is_err());
    assert!(FluidVolume::new(desc(base, MAX_PARTICLES)).is_ok());
    let bad_domains = [
        Domain::new(Vec3::ONE, Vec3::ZERO),
        Domain::new(Vec3::ZERO, Vec3::new(1.0, f32::NAN, 1.0)),
        Domain::new(Vec3::ZERO, Vec3::new(1.0, 0.05, 1.0)),
        Domain {
            restitution: 2.0,
            ..Domain::new(Vec3::ZERO, Vec3::ONE)
        },
        Domain {
            friction: -1.0,
            ..Domain::new(Vec3::ZERO, Vec3::ONE)
        },
    ];
    for domain in bad_domains {
        let result = FluidVolume::new(VolumeDesc {
            params: base,
            domain,
            capacity: 16,
        });
        assert!(result.is_err(), "{domain:?} accepted");
    }
}

#[test]
fn invalid_runtime_inputs_leave_the_volume_unchanged() {
    let p = params();
    let mut volume = tank(p, Vec3::ZERO, Vec3::ONE, 512);
    volume
        .fill(
            block(Vec3::ZERO, Vec3::new(0.3, 0.3, 0.3)),
            Vec3::ZERO,
            None,
        )
        .unwrap();
    volume.step(DT, &[]).unwrap();
    let before = volume.positions().to_vec();
    for dt in [0.0, -DT, f32::NAN, 1.0] {
        assert!(volume.step(dt, &[]).is_err(), "dt {dt} accepted");
    }
    let good = ColliderProxy {
        id: 1,
        position: Vec3::splat(0.5),
        rotation: Quat::IDENTITY,
        linear_velocity: Vec3::ZERO,
        angular_velocity: Vec3::ZERO,
        shape: ProxyShape::Sphere { radius: 0.1 },
        friction: 0.0,
    };
    let bad_proxies = [
        ColliderProxy {
            position: Vec3::NAN,
            ..good
        },
        ColliderProxy {
            shape: ProxyShape::Sphere { radius: 0.0 },
            ..good
        },
        ColliderProxy {
            shape: ProxyShape::Capsule {
                half_height: -1.0,
                radius: 0.1,
            },
            ..good
        },
        ColliderProxy {
            shape: ProxyShape::Box {
                half_extents: Vec3::new(0.1, -0.1, 0.1),
            },
            ..good
        },
        ColliderProxy {
            shape: ProxyShape::Plane {
                normal: Vec3::ZERO,
                offset: 0.0,
            },
            ..good
        },
        ColliderProxy {
            rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
            ..good
        },
        ColliderProxy {
            rotation: Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0),
            ..good
        },
        ColliderProxy {
            friction: f32::NAN,
            ..good
        },
        ColliderProxy {
            linear_velocity: Vec3::new(f32::INFINITY, 0.0, 0.0),
            ..good
        },
    ];
    for proxy in bad_proxies {
        assert!(volume.step(DT, &[proxy]).is_err(), "{proxy:?} accepted");
    }
    assert!(
        volume.step(DT, &[good, good]).is_err(),
        "duplicate ids accepted"
    );
    assert!(volume.spawn(&[Vec3::NAN], Vec3::ZERO, None).is_err());
    assert!(volume.spawn(&[Vec3::splat(2.0)], Vec3::ZERO, None).is_err());
    assert!(volume.spawn(&[Vec3::splat(0.5)], Vec3::NAN, None).is_err());
    assert!(
        volume
            .spawn(&[Vec3::splat(0.5)], Vec3::ZERO, Some(0.0))
            .is_err()
    );
    let bad_emitters = [
        EmitterKind::Rate {
            region: Region::Sphere {
                center: Vec3::splat(0.5),
                radius: 0.1,
            },
            per_second: f32::NAN,
            velocity: Vec3::ZERO,
            seed: 0,
        },
        EmitterKind::Rate {
            region: Region::Sphere {
                center: Vec3::splat(0.5),
                radius: -0.1,
            },
            per_second: 10.0,
            velocity: Vec3::ZERO,
            seed: 0,
        },
        EmitterKind::Nozzle {
            position: Vec3::splat(0.5),
            direction: Vec3::ZERO,
            radius: 0.05,
            speed: 1.0,
        },
        EmitterKind::Nozzle {
            position: Vec3::splat(0.5),
            direction: Vec3::X,
            radius: 100.0,
            speed: 1.0,
        },
    ];
    for kind in bad_emitters {
        let desc = EmitterDesc {
            kind,
            lifetime: None,
            enabled: true,
        };
        assert!(volume.set_emitter(1, desc).is_err(), "{kind:?} accepted");
    }
    assert!(volume.emitter(1).is_none());
    assert!(
        volume
            .sample_density_grid(Vec3::ZERO, Vec3::ONE, [1, 4, 4])
            .is_err()
    );
    assert!(
        volume
            .sample_density_grid(Vec3::ONE, Vec3::ZERO, [4, 4, 4])
            .is_err()
    );
    assert!(
        volume
            .sample_density_grid(Vec3::ZERO, Vec3::ONE, [4096, 4096, 4])
            .is_err()
    );
    let mut invalid = p;
    invalid.rest_density = 0.0;
    assert!(volume.set_params(invalid).is_err());
    assert_eq!(volume.params(), &p);
    assert_eq!(volume.positions(), &before[..]);
}

#[test]
fn world_rejects_duplicate_and_unknown_ids() {
    let mut world = FluidWorld::new();
    world.create_volume(1, desc(params(), 16)).unwrap();
    assert_eq!(
        world.create_volume(1, desc(params(), 16)).err(),
        Some(Error::DuplicateVolume(1))
    );
    assert_eq!(world.volume_mut(2).err(), Some(Error::UnknownVolume(2)));
    assert!(world.step(f32::NAN, &[]).is_err());
    assert!(world.remove_volume(1).is_some());
    assert!(world.is_empty());
}
