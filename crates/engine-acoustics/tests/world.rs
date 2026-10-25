//! Rooms, documents, IR synthesis and the direct path (library half of test 14).

use engine_acoustics::direct::{ListenerFrame, direct_path};
use engine_acoustics::reference::*;
use engine_acoustics::*;

fn shoebox_material() -> AcousticMaterial {
    AcousticMaterial {
        absorption: SHOEBOX_ABSORPTION,
        scattering: 0.5,
        transmission: [0.0; 4],
    }
}

#[test]
fn room_volumes_give_the_analytic_eyring_time() {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(shoebox_material()).unwrap();
    let room = AcousticVolume {
        shape: VolumeShape::Box {
            half_extents: [5.0, 3.0, 4.0],
        },
        kind: VolumeKind::Room { open_faces: 0 },
        material: m,
        mobility: Mobility::Static,
    };
    let at = [1.0, 0.0, 0.0, 5.0, 0.0, 1.0, 0.0, 3.0, 0.0, 0.0, 1.0, 4.0];
    b.add_volume(room, at).unwrap();
    let world = b.build().unwrap();
    let info = world.room_at([5.0, 3.0, 4.0]).unwrap();
    assert!((info.volume_m3 - 480.0).abs() < 1e-3);
    assert!((info.surface_m2() - 376.0).abs() < 1e-3);
    let eyring = info.eyring_rt60([0.0; 4]).unwrap();
    for b in 0..BANDS {
        assert!((eyring[b] - SHOEBOX_EYRING_RT60_S[b]).abs() / SHOEBOX_EYRING_RT60_S[b] < 1e-3);
    }
    assert!(world.room_at([11.0, 3.0, 4.0]).is_none());
    // The shell reflects like the reference shoebox mesh.
    let hit = world
        .raycast([5.0, 3.0, 4.0], [0.0, 1.0, 0.0], f32::INFINITY, u8::MAX)
        .unwrap();
    assert!((hit.t - 3.0).abs() < 1e-5);
    // An open +y face counts as fully absorbing area.
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(shoebox_material()).unwrap();
    b.add_volume(
        AcousticVolume {
            kind: VolumeKind::Room { open_faces: 1 << 3 },
            material: m,
            ..room
        },
        at,
    )
    .unwrap();
    let world = b.build().unwrap();
    assert!((world.rooms()[0].open_area_m2 - 80.0).abs() < 1e-3);
    assert!(
        world
            .raycast([5.0, 3.0, 4.0], [0.0, 1.0, 0.0], f32::INFINITY, u8::MAX)
            .is_none()
    );
}

#[test]
fn documents_replay_into_identical_worlds_and_detect_tampering() {
    let mut doc = AcousticWorldDocument::new(Some("scenes/hall.json".into()));
    doc.materials.push(shoebox_material());
    doc.materials
        .push(AcousticMaterial::preset(MaterialPreset::Concrete).unwrap());
    doc.meshes
        .push(box_mesh([0.0; 3], SHOEBOX_DIMS_M, MaterialId(0)));
    doc.instances.push(DocumentInstance {
        mesh: MeshId(0),
        transform: IDENTITY_AFFINE,
        mobility: Mobility::Static,
        mask: 1,
    });
    doc.volumes.push(DocumentVolume {
        volume: AcousticVolume {
            shape: VolumeShape::Box {
                half_extents: [0.5; 3],
            },
            kind: VolumeKind::Solid,
            material: MaterialId(1),
            mobility: Mobility::Dynamic,
        },
        transform: [1.0, 0.0, 0.0, 2.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 2.0],
    });
    doc.seal();
    assert!(doc.content_hash.starts_with("fnv1a64:"));
    let a = doc.build(WorldLimits::default()).unwrap();
    let b = doc.build(WorldLimits::default()).unwrap();
    assert_eq!(a.stats(), b.stats());
    assert_eq!(a.stats().triangles, 24);
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(a.gpu_view().nodes),
        bytemuck::cast_slice::<_, u8>(b.gpu_view().nodes)
    );
    assert_eq!(a.dynamic_instances().count(), 1);
    let mut tampered = doc.clone();
    tampered.meshes[0].positions[1][0] = 10.5;
    assert_eq!(
        tampered.build(WorldLimits::default()).unwrap_err().code,
        "acoustics.world.invalid"
    );
    let mut future = doc.clone();
    future.version = 2;
    assert_eq!(
        future.build(WorldLimits::default()).unwrap_err().code,
        "acoustics.world.invalid"
    );
    // Limits apply on load: the document is source data, not trusted topology.
    let tight = WorldLimits {
        max_static_triangles: 11,
        ..WorldLimits::default()
    };
    assert_eq!(doc.build(tight).unwrap_err().code, "acoustics.world.limit");
}

#[test]
fn synthesized_ir_carries_the_histogram_energy() {
    let world = reference_shoebox(1.0).unwrap();
    let h = cpu_reference_histogram(
        &world,
        &reference_trace_settings(1.0),
        &reference_job(8192, 3),
        4,
    )
    .unwrap();
    let ir = synthesize_ir(&h, 48_000, 9).unwrap();
    assert_eq!(ir.left.len(), 48_000);
    assert_eq!(
        ir.left,
        synthesize_ir(&h, 48_000, 9).unwrap().left,
        "deterministic"
    );
    assert_ne!(ir.left, ir.right, "independent noise per ear");
    let expected: f64 = h.bins.iter().flatten().map(|&v| f64::from(v)).sum();
    for ear in [&ir.left, &ir.right] {
        let energy: f64 = ear.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        let db = 10.0 * (energy / expected).log10();
        println!("IR energy {energy:.4} vs histogram {expected:.4} ({db:+.2} dB)");
        assert!(db.abs() < 1.0);
    }
    assert!(synthesize_ir(&h, 1000, 9).is_err());
}

#[test]
fn a_transmitting_wall_attenuates_the_direct_path_per_band() {
    // Free field (no room) with a single 4 x 4 m wall halfway between source and
    // listener 10 m apart.
    let transmission = [0.3, 0.1, 0.03, 0.01];
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let wall = b
        .add_material(AcousticMaterial {
            absorption: [0.5; 4],
            scattering: 0.0,
            transmission,
        })
        .unwrap();
    let quad = b
        .add_mesh(AcousticMesh {
            positions: vec![
                [5.0, -2.0, -2.0],
                [5.0, 2.0, -2.0],
                [5.0, 2.0, 2.0],
                [5.0, -2.0, 2.0],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            materials: vec![wall],
        })
        .unwrap();
    b.add_instance(quad, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    let world = b.build().unwrap();
    let listener = ListenerFrame::new(&ListenerPose {
        position: [0.0; 3],
        forward: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
    })
    .unwrap();
    let blocked = direct_path(&world, &listener, [10.0, 0.0, 0.0], 0.25, 64, true);
    assert_eq!(blocked.visible_fraction, 0.0);
    assert_eq!(blocked.distance_m, 10.0);
    assert!(
        (blocked.direction[2] + 1.0).abs() < 1e-6,
        "ahead is -z in the listener frame"
    );
    let expected_db = [-5.2, -10.0, -15.2, -20.0];
    for (b, expected) in expected_db.iter().enumerate() {
        let db = 20.0 * blocked.gains[b].log10();
        assert!((db - expected).abs() < 0.1, "band {b}: {db}");
    }
    // Move the source so the wall edge (y = 2) splits the occlusion sphere.
    let edge = direct_path(&world, &listener, [10.0, 4.0, 0.0], 1.0, 64, true);
    println!("edge visible fraction {}", edge.visible_fraction);
    assert!((edge.visible_fraction - 0.5).abs() <= 0.15);
    let clear = direct_path(&world, &listener, [10.0, 9.0, 0.0], 0.25, 64, true);
    assert_eq!((clear.visible_fraction, clear.gains), (1.0, [1.0; 4]));
}
