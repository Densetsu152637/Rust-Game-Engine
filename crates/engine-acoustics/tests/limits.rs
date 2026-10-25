//! Test 10 (library half): every section 5.3 limit and invalid field returns its
//! `acoustics.*` code, rejected calls leave the builder unchanged, degenerate
//! triangles are counted and no partial world is produced.

use engine_acoustics::reference::box_mesh;
use engine_acoustics::*;

const LIMIT: &str = "acoustics.world.limit";
const INVALID: &str = "acoustics.world.invalid";
const MATERIAL: &str = "acoustics.material.invalid";
const PROXY: &str = "acoustics.proxy.invalid";
const REQUEST: &str = "acoustics.request.invalid";

fn mat() -> AcousticMaterial {
    AcousticMaterial {
        absorption: [0.1; BANDS],
        scattering: 0.1,
        transmission: [0.05; BANDS],
    }
}
fn builder(limits: WorldLimits) -> (AcousticWorldBuilder, MaterialId) {
    let mut b = AcousticWorldBuilder::new(limits);
    let m = b.add_material(mat()).unwrap();
    (b, m)
}
fn unit_box(m: MaterialId) -> AcousticMesh {
    box_mesh([0.0; 3], [1.0; 3], m)
}
fn translate(x: f32) -> Affine3 {
    [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
}
fn code<T: std::fmt::Debug>(r: Result<T>) -> &'static str {
    r.unwrap_err().code
}

#[test]
fn geometry_limits_return_world_limit() {
    let base = WorldLimits::default();
    let (mut b, m) = builder(WorldLimits {
        max_mesh_triangles: 11,
        ..base
    });
    assert_eq!(code(b.add_mesh(unit_box(m))), LIMIT);

    let (mut b, m) = builder(WorldLimits {
        max_static_triangles: 20,
        ..base
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    assert_eq!(
        code(b.add_instance(mesh, translate(3.0), Mobility::Static, 1)),
        LIMIT
    );

    let (mut b, m) = builder(WorldLimits {
        max_dynamic_triangles: 20,
        ..base
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Dynamic, 1)
        .unwrap();
    assert_eq!(
        code(b.add_instance(mesh, translate(3.0), Mobility::Dynamic, 1)),
        LIMIT
    );

    let (mut b, m) = builder(WorldLimits {
        max_dynamic_instances: 1,
        ..base
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Dynamic, 1)
        .unwrap();
    assert_eq!(
        code(b.add_instance(mesh, translate(3.0), Mobility::Dynamic, 1)),
        LIMIT
    );

    let (mut b, m) = builder(WorldLimits {
        max_instances: 2,
        ..base
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    b.add_instance(mesh, translate(2.0), Mobility::Static, 1)
        .unwrap();
    assert_eq!(
        code(b.add_instance(mesh, translate(4.0), Mobility::Static, 1)),
        LIMIT
    );
    let volume = AcousticVolume {
        shape: VolumeShape::Box {
            half_extents: [1.0; 3],
        },
        kind: VolumeKind::Solid,
        material: m,
        mobility: Mobility::Static,
    };
    assert_eq!(code(b.add_volume(volume, translate(8.0))), LIMIT);

    let (mut b, _) = builder(WorldLimits {
        max_materials: 1,
        ..base
    });
    assert_eq!(code(b.add_material(mat())), LIMIT);

    let (mut b, m) = builder(WorldLimits {
        max_volumes: 1,
        ..base
    });
    let volume = AcousticVolume {
        material: m,
        ..volume
    };
    b.add_volume(volume, IDENTITY_AFFINE).unwrap();
    assert_eq!(code(b.add_volume(volume, translate(5.0))), LIMIT);

    let (mut b, m) = builder(WorldLimits {
        max_coordinate_m: 100,
        ..base
    });
    let mut far = unit_box(m);
    far.positions[0] = [200.0, 0.0, 0.0];
    assert_eq!(code(b.add_mesh(far)), LIMIT);
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    assert_eq!(
        code(b.add_instance(mesh, translate(150.0), Mobility::Static, 1)),
        LIMIT
    );
    assert_eq!(
        code(b.add_instance(mesh, translate(99.5), Mobility::Static, 1)),
        LIMIT
    );
}

#[test]
fn build_budget_and_plane_cluster_limits() {
    let (mut b, m) = builder(WorldLimits {
        build_work_budget: 10,
        ..WorldLimits::default()
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    assert_eq!(code(b.build()), "acoustics.world.budget");

    let (mut b, m) = builder(WorldLimits {
        max_plane_clusters: 5,
        ..WorldLimits::default()
    });
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    assert_eq!(code(b.build()), LIMIT, "a box has 6 planes");
}

#[test]
fn invalid_geometry_and_materials_are_rejected() {
    let (mut b, m) = builder(WorldLimits::default());
    let bad = |f: fn(&mut AcousticMaterial)| {
        let mut x = mat();
        f(&mut x);
        x
    };
    for material in [
        bad(|x| x.absorption[0] = 1.5),
        bad(|x| x.absorption[2] = f32::NAN),
        bad(|x| x.transmission[1] = 0.2),
        bad(|x| x.scattering = -0.1),
        bad(|x| x.transmission[3] = -0.0001),
    ] {
        assert_eq!(code(b.add_material(material)), MATERIAL, "{material:?}");
    }
    assert_eq!(code(b.add_mesh(unit_box(MaterialId(9)))), MATERIAL);
    let mut mesh = unit_box(m);
    mesh.triangles[3] = [0, 1, 99];
    assert_eq!(code(b.add_mesh(mesh)), INVALID);
    let mut mesh = unit_box(m);
    mesh.materials = vec![m; 5];
    assert_eq!(code(b.add_mesh(mesh)), INVALID);
    let mut mesh = unit_box(m);
    mesh.positions[2] = [f32::INFINITY, 0.0, 0.0];
    assert_eq!(code(b.add_mesh(mesh)), INVALID);
    assert_eq!(
        code(b.add_instance(MeshId(7), IDENTITY_AFFINE, Mobility::Static, 1)),
        INVALID
    );
    let mesh = b.add_mesh(unit_box(m)).unwrap();
    let singular = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    assert_eq!(
        code(b.add_instance(mesh, singular, Mobility::Static, 1)),
        INVALID
    );
    let mut nan = IDENTITY_AFFINE;
    nan[3] = f32::NAN;
    assert_eq!(
        code(b.add_instance(mesh, nan, Mobility::Static, 1)),
        INVALID
    );
    let room = AcousticVolume {
        shape: VolumeShape::Box {
            half_extents: [1.0, 0.0, 1.0],
        },
        kind: VolumeKind::Room { open_faces: 0 },
        material: m,
        mobility: Mobility::Static,
    };
    assert_eq!(code(b.add_volume(room, IDENTITY_AFFINE)), INVALID);
    let room = AcousticVolume {
        shape: VolumeShape::Box {
            half_extents: [1.0; 3],
        },
        kind: VolumeKind::Room { open_faces: 1 << 6 },
        ..room
    };
    assert_eq!(code(b.add_volume(room, IDENTITY_AFFINE)), INVALID);
}

#[test]
fn degenerate_triangles_are_dropped_and_counted_and_rejections_change_nothing() {
    let (mut b, m) = builder(WorldLimits {
        max_static_triangles: 12,
        ..WorldLimits::default()
    });
    let mut mesh = unit_box(m);
    mesh.positions.push([0.5, 0.5, 0.5]);
    mesh.triangles.push([8, 8, 0]); // zero area
    mesh.triangles.push([0, 1, 1]); // zero area
    let mesh = b.add_mesh(mesh).unwrap();
    let id = b
        .add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    // Rejected additions leave the builder as it was.
    assert_eq!(
        code(b.add_instance(mesh, translate(3.0), Mobility::Static, 1)),
        LIMIT
    );
    let bad = Footprint {
        shape: Footprint2D::Circle {
            center: [0.0, 0.0],
            radius: -1.0,
        },
        material: m,
        mobility: Mobility::Static,
    };
    assert_eq!(code(b.add_proxy_2d(&[bad], &proxy_settings(m))), PROXY);
    let world = b.build().unwrap();
    let stats = world.stats();
    assert_eq!(stats.dropped_degenerate, 2);
    assert_eq!((stats.triangles, stats.instances), (12, 1));
    assert!(world.proxy().is_none());
    let hit = world
        .raycast([0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 5.0, 1)
        .unwrap();
    assert_eq!(hit.instance, id);
    assert!(hit.triangle < 12, "original triangle index");
    let mut world = world;
    assert_eq!(
        code(world.set_instance_transform(id, translate(1.0))),
        INVALID,
        "static"
    );
}

fn proxy_settings(m: MaterialId) -> Proxy2DSettings {
    Proxy2DSettings {
        plane: ProxyPlane::SideView,
        units_per_meter: 1.0,
        depth_m: 8.0,
        enclose_depth: true,
        wall_height_m: 3.0,
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

#[test]
fn proxy_fields_and_limits_are_validated() {
    let (mut b, m) = builder(WorldLimits {
        max_proxy_footprints: 1,
        max_footprint_vertices: 4,
        ..WorldLimits::default()
    });
    let rect = Footprint {
        shape: Footprint2D::Rect {
            center: [0.0, 0.0],
            half_extents: [1.0, 1.0],
            rotation: 0.0,
        },
        material: m,
        mobility: Mobility::Static,
    };
    let s = proxy_settings(m);
    assert_eq!(
        code(b.add_proxy_2d(&[rect.clone(), rect.clone()], &s)),
        LIMIT
    );
    let pentagon = Footprint {
        shape: Footprint2D::Polygon {
            points: vec![[0.0, 0.0], [2.0, 0.0], [3.0, 1.0], [1.0, 3.0], [-1.0, 1.0]],
        },
        ..rect.clone()
    };
    assert_eq!(code(b.add_proxy_2d(&[pentagon], &s)), LIMIT);
    // The 12-gon circle also exceeds 4 vertices.
    let circle = Footprint {
        shape: Footprint2D::Circle {
            center: [0.0, 0.0],
            radius: 1.0,
        },
        ..rect.clone()
    };
    assert_eq!(code(b.add_proxy_2d(&[circle], &s)), LIMIT);
    let cases: Vec<Proxy2DSettings> = vec![
        Proxy2DSettings {
            units_per_meter: 0.0005,
            ..s
        },
        Proxy2DSettings {
            units_per_meter: 2000.0,
            ..s
        },
        Proxy2DSettings { depth_m: 0.0, ..s },
        Proxy2DSettings {
            wall_height_m: f32::NAN,
            ..s
        },
        Proxy2DSettings {
            plane: ProxyPlane::TopDown,
            ear_height_m: 5.0,
            ..s
        },
        Proxy2DSettings {
            enclosure: Enclosure::Auto { margin_m: -1.0 },
            ..s
        },
        Proxy2DSettings {
            enclosure: Enclosure::Bounds {
                min: [1.0, 0.0],
                max: [0.0, 1.0],
            },
            ..s
        },
    ];
    for case in cases {
        assert_eq!(
            code(b.add_proxy_2d(std::slice::from_ref(&rect), &case)),
            PROXY,
            "{case:?}"
        );
    }
    assert_eq!(
        code(b.add_proxy_2d(
            std::slice::from_ref(&rect),
            &Proxy2DSettings {
                floor_material: MaterialId(3),
                ..s
            }
        )),
        MATERIAL
    );
    let far = Footprint {
        shape: Footprint2D::Rect {
            center: [2e5, 0.0],
            half_extents: [1.0, 1.0],
            rotation: 0.0,
        },
        ..rect.clone()
    };
    assert_eq!(code(b.add_proxy_2d(&[far], &s)), LIMIT);
    b.add_proxy_2d(&[rect], &s).unwrap();
}

#[test]
fn request_fields_are_validated() {
    let ok = PropagationRequest {
        sequence: 1,
        listener: ListenerPose {
            position: [0.0; 3],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        },
        sources: vec![SourceRequest {
            id: 1,
            voice: None,
            position: [1.0, 0.0, 0.0],
            radius_m: 0.5,
            occlusion: true,
            reflections: true,
            reverb: true,
            hero: false,
            priority: 1.0,
        }],
        settings: PropagationSettings::default(),
    };
    validate_request(&ok).unwrap();
    let settings: Vec<fn(&mut PropagationSettings)> = vec![
        |s| s.reflection_order = 4,
        |s| s.max_reflections = 17,
        |s| s.occlusion_rays = 5000,
        |s| s.er_rays = 0,
        |s| s.reverb_rays = 1_048_577,
        |s| s.max_bounces = 257,
        |s| s.histogram_bin_s = 0.0001,
        |s| s.histogram_bin_s = 0.02,
        |s| s.histogram_length_s = 4.5,
        |s| s.late_start_s = 3.0,
        |s| s.receiver_radius_m = 0.0,
        |s| s.speed_of_sound = 250.0,
        |s| s.air_np_per_m[1] = -0.1,
        |s| s.time_budget_us = 0,
    ];
    for (i, f) in settings.into_iter().enumerate() {
        let mut r = ok.clone();
        f(&mut r.settings);
        assert_eq!(code(validate_request(&r)), REQUEST, "settings case {i}");
    }
    let sources: Vec<fn(&mut PropagationRequest)> = vec![
        |r| r.sources[0].position[1] = f32::NAN,
        |r| r.sources[0].radius_m = -1.0,
        |r| r.sources[0].priority = f32::INFINITY,
        |r| r.listener.up = [0.0, 0.0, -2.0],
        |r| r.listener.position[0] = f32::NAN,
        |r| {
            let s = r.sources[0];
            r.sources = vec![s; 257];
            for (i, s) in r.sources.iter_mut().enumerate() {
                s.id = i as u64;
            }
        },
        |r| {
            let s = r.sources[0];
            r.sources.push(s);
        },
    ];
    for (i, f) in sources.into_iter().enumerate() {
        let mut r = ok.clone();
        f(&mut r);
        assert_eq!(code(validate_request(&r)), REQUEST, "request case {i}");
    }
}
