mod common;

use common::{DT, proxy};
use engine_xpbd::{
    ClothDesc, ClothParams, MAX_OBJECTS, MAX_PROXIES, MAX_STEP, ProxyShape, SoftBodyDesc,
    SoftBodyParams, XpbdSettings, XpbdWorld, box_tet_mesh, grid_mesh, surface_triangles,
};
use glam::{Quat, Vec3};

fn world() -> XpbdWorld {
    XpbdWorld::new(XpbdSettings::default(), 100_000).unwrap()
}

fn triangle_cloth() -> ClothDesc {
    ClothDesc {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Z],
        triangles: vec![[0, 1, 2]],
        areal_density: 0.2,
        inverse_masses: None,
        pinned: Vec::new(),
        params: ClothParams::default(),
    }
}

fn tet_body() -> SoftBodyDesc {
    SoftBodyDesc {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
        tets: vec![[0, 1, 2, 3]],
        density: 100.0,
        inverse_masses: None,
        pinned: Vec::new(),
        params: SoftBodyParams::default(),
    }
}

fn rejects_cloth(edit: impl FnOnce(&mut ClothDesc)) {
    let mut desc = triangle_cloth();
    edit(&mut desc);
    let mut world = world();
    assert!(world.insert_cloth(1, desc).is_err());
    assert!(world.is_empty() && world.particle_count() == 0);
}

fn rejects_body(edit: impl FnOnce(&mut SoftBodyDesc)) {
    let mut desc = tet_body();
    edit(&mut desc);
    assert!(world().insert_soft_body(1, desc).is_err());
}

#[test]
fn valid_minimal_objects_are_accepted() {
    let mut world = world();
    world.insert_cloth(1, triangle_cloth()).unwrap();
    // A negatively oriented tetrahedron is reoriented, not rejected.
    let mut body = tet_body();
    body.tets = vec![[0, 2, 1, 3]];
    world.insert_soft_body(2, body).unwrap();
    assert!(world.soft_body(2).unwrap().rest_volume() > 0.0);
    assert_eq!(world.len(), 2);
    assert_eq!(world.ids().collect::<Vec<_>>(), [1, 2]);
    assert!(world.cloth(2).is_none() && world.soft_body(1).is_none());
    assert!(world.remove(1) && !world.remove(1));
    assert_eq!(world.particle_count(), 4);
}

#[test]
fn invalid_cloth_inputs_are_rejected() {
    rejects_cloth(|d| d.positions[1] = Vec3::new(f32::NAN, 0.0, 0.0));
    rejects_cloth(|d| d.positions[1] = Vec3::splat(1.0e9));
    rejects_cloth(|d| d.triangles[0] = [0, 1, 3]);
    rejects_cloth(|d| d.triangles[0] = [0, 1, 1]);
    rejects_cloth(|d| d.positions[2] = Vec3::X * 2.0); // collinear: degenerate
    rejects_cloth(|d| d.positions.push(Vec3::Y)); // unreferenced vertex
    rejects_cloth(|d| d.triangles.push([2, 1, 0])); // duplicate triangle
    rejects_cloth(|d| d.triangles.clear());
    rejects_cloth(|d| d.inverse_masses = Some(vec![1.0, 1.0]));
    rejects_cloth(|d| d.inverse_masses = Some(vec![1.0, -1.0, 1.0]));
    rejects_cloth(|d| d.inverse_masses = Some(vec![1.0, f32::INFINITY, 1.0]));
    rejects_cloth(|d| d.areal_density = 0.0);
    rejects_cloth(|d| d.pinned = vec![3]);
    rejects_cloth(|d| d.params.stretch_compliance = -1.0);
    rejects_cloth(|d| d.params.bend_compliance = f32::NAN);
    rejects_cloth(|d| d.params.friction = f32::NAN);
    rejects_cloth(|d| d.params.tether_slack = Some(0.5));
    rejects_cloth(|d| d.params.thickness = -0.1);
    rejects_cloth(|d| d.params.drag = -1.0);
    let mut world = world();
    world.insert_cloth(1, triangle_cloth()).unwrap();
    let cloth = world.cloth_mut(1).unwrap();
    assert!(cloth.set_pin_target(3, Vec3::ZERO).is_err());
    assert!(cloth.set_pin_target(0, Vec3::splat(f32::NAN)).is_err());
    assert!(cloth.unpin(9).is_err());
    assert!(cloth.set_inverse_mass(0, -2.0).is_err());
    let bad = ClothParams {
        damping: -1.0,
        ..ClothParams::default()
    };
    assert!(cloth.set_params(bad).is_err());
}

#[test]
fn invalid_soft_body_inputs_are_rejected() {
    rejects_body(|d| d.positions[3] = Vec3::new(1.0, 1.0, 0.0) * 0.5); // coplanar
    rejects_body(|d| d.tets[0] = [0, 1, 2, 4]);
    rejects_body(|d| d.tets[0] = [0, 1, 2, 2]);
    rejects_body(|d| d.positions[0] = Vec3::splat(f32::NAN));
    rejects_body(|d| d.density = -5.0);
    rejects_body(|d| d.tets.push([3, 2, 1, 0])); // duplicate
    rejects_body(|d| d.params.volume_compliance = -1.0);
    rejects_body(|d| d.params.particle_radius = f32::INFINITY);
    assert!(surface_triangles(&[Vec3::ZERO; 4], &[[0, 1, 2, 3]]).is_err());
}

#[test]
fn generator_and_settings_inputs_are_rejected() {
    assert!(grid_mesh(Vec3::ZERO, Vec3::X, Vec3::Z, 0, 4).is_err());
    assert!(grid_mesh(Vec3::ZERO, Vec3::X, Vec3::X * 2.0, 4, 4).is_err());
    assert!(grid_mesh(Vec3::ZERO, Vec3::X, Vec3::Z, 1024, 1024).is_err());
    assert!(box_tet_mesh(Vec3::ONE, Vec3::ZERO, [1, 1, 1]).is_err());
    assert!(box_tet_mesh(Vec3::ZERO, Vec3::ONE, [0, 1, 1]).is_err());
    for settings in [
        XpbdSettings {
            substeps: 0,
            ..XpbdSettings::default()
        },
        XpbdSettings {
            iterations: 17,
            ..XpbdSettings::default()
        },
        XpbdSettings {
            gravity: Vec3::new(0.0, f32::NAN, 0.0),
            ..XpbdSettings::default()
        },
        XpbdSettings {
            air_density: -1.0,
            ..XpbdSettings::default()
        },
    ] {
        assert!(XpbdWorld::new(settings, 10).is_err());
        assert!(world().set_settings(settings).is_err());
    }
    assert!(XpbdWorld::new(XpbdSettings::default(), 0).is_err());
}

#[test]
fn capacity_limits_are_enforced() {
    let mut small = XpbdWorld::new(XpbdSettings::default(), 100).unwrap();
    let grid = ClothDesc::grid(Vec3::ZERO, Vec3::X, Vec3::Z, 16, 16).unwrap();
    assert!(small.insert_cloth(1, grid).is_err());
    small.insert_cloth(1, triangle_cloth()).unwrap();
    assert!(
        small.insert_cloth(1, triangle_cloth()).is_err(),
        "duplicate id"
    );
    assert!(
        small.insert_soft_body(1, tet_body()).is_err(),
        "id shared across kinds"
    );

    let mut many = world();
    for id in 0..MAX_OBJECTS as u64 {
        many.insert_cloth(id, triangle_cloth()).unwrap();
    }
    assert!(many.insert_cloth(u64::MAX, triangle_cloth()).is_err());

    let mut world = world();
    world.insert_cloth(1, triangle_cloth()).unwrap();
    let ball = proxy(
        1,
        Vec3::ZERO,
        Quat::IDENTITY,
        ProxyShape::Sphere { radius: 0.1 },
        0.5,
    );
    let too_many: Vec<_> = (0..=MAX_PROXIES as u64)
        .map(|id| engine_xpbd::ColliderProxy { id, ..ball })
        .collect();
    assert!(world.step(DT, &too_many).is_err());
}

#[test]
fn invalid_steps_and_proxies_are_rejected_without_changing_state() {
    let mut world = world();
    world.insert_cloth(1, triangle_cloth()).unwrap();
    let before = world.cloth(1).unwrap().positions().to_vec();
    let ball = proxy(
        1,
        Vec3::ZERO,
        Quat::IDENTITY,
        ProxyShape::Sphere { radius: 0.1 },
        0.5,
    );
    for dt in [0.0, -DT, f32::NAN, MAX_STEP * 2.0] {
        assert!(world.step(dt, &[]).is_err());
    }
    let invalid = [
        engine_xpbd::ColliderProxy {
            position: Vec3::splat(f32::NAN),
            ..ball
        },
        engine_xpbd::ColliderProxy {
            rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 2.0),
            ..ball
        },
        engine_xpbd::ColliderProxy {
            shape: ProxyShape::Sphere { radius: -1.0 },
            ..ball
        },
        engine_xpbd::ColliderProxy {
            shape: ProxyShape::Capsule {
                half_height: f32::NAN,
                radius: 0.1,
            },
            ..ball
        },
        engine_xpbd::ColliderProxy {
            shape: ProxyShape::Box {
                half_extents: Vec3::new(1.0, 0.0, 1.0),
            },
            ..ball
        },
        engine_xpbd::ColliderProxy {
            shape: ProxyShape::Plane {
                normal: Vec3::ZERO,
                offset: 0.0,
            },
            ..ball
        },
        engine_xpbd::ColliderProxy {
            friction: -0.5,
            ..ball
        },
        engine_xpbd::ColliderProxy {
            linear_velocity: Vec3::splat(f32::INFINITY),
            ..ball
        },
    ];
    for bad in invalid {
        assert!(bad.validate().is_err());
        assert!(world.step(DT, &[bad]).is_err());
    }
    assert!(
        world.step(DT, &[ball, ball]).is_err(),
        "duplicate proxy ids"
    );
    assert_eq!(world.cloth(1).unwrap().positions(), before.as_slice());
    world.step(DT, &[ball]).unwrap();
    assert_ne!(world.cloth(1).unwrap().positions(), before.as_slice());
}
