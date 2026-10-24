//! Test 9: BVH traversal equals brute force, plus layout determinism.

use engine_acoustics::reference::{box_mesh, reference_shoebox};
use engine_acoustics::rng::{Pcg, uniform_sphere};
use engine_acoustics::*;

const RAYS: u32 = 100_000;

fn material() -> AcousticMaterial {
    AcousticMaterial {
        absorption: [0.1; BANDS],
        scattering: 0.2,
        transmission: [0.0; BANDS],
    }
}

/// 50,000 random triangles in a 20 m cube, seeded.
fn soup_world(seed: u64) -> AcousticWorld {
    let mut rng = Pcg::for_ray(seed, 0);
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for i in 0..50_000u32 {
        let c = [0.0f32; 3].map(|_| rng.next_f32() * 20.0 - 10.0);
        for _ in 0..3 {
            let p = [0usize, 1, 2].map(|k| c[k] + (rng.next_f32() - 0.5) * 1.5);
            positions.push(p);
        }
        triangles.push([3 * i, 3 * i + 1, 3 * i + 2]);
    }
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material()).unwrap();
    let mesh = b
        .add_mesh(AcousticMesh {
            positions,
            triangles,
            materials: vec![m],
        })
        .unwrap();
    b.add_instance(mesh, IDENTITY_AFFINE, Mobility::Static, 1)
        .unwrap();
    b.build().unwrap()
}

/// One box mesh instanced 64 times with rotation, non-uniform scale and
/// translation, plus a dynamic instance that is moved after the build.
fn instanced_world() -> AcousticWorld {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material()).unwrap();
    let mesh = b.add_mesh(box_mesh([-0.5; 3], [0.5; 3], m)).unwrap();
    for i in 0..64 {
        let a = i as f32 * 0.37;
        let (s, c) = a.sin_cos();
        let scale = [1.0 + (i % 3) as f32, 0.5 + (i % 5) as f32 * 0.3, 1.2];
        let t = [
            (i % 8) as f32 * 3.0 - 12.0,
            (i / 8) as f32 * 3.0 - 12.0,
            (i % 7) as f32 - 3.0,
        ];
        let affine = [
            c * scale[0],
            -s * scale[1],
            0.0,
            t[0],
            s * scale[0],
            c * scale[1],
            0.0,
            t[1],
            0.0,
            0.0,
            scale[2],
            t[2],
        ];
        let mask = if i % 4 == 0 { 2 } else { 1 };
        b.add_instance(mesh, affine, Mobility::Static, mask)
            .unwrap();
    }
    let dynamic = b
        .add_instance(mesh, IDENTITY_AFFINE, Mobility::Dynamic, 1)
        .unwrap();
    let mut world = b.build().unwrap();
    world
        .set_instance_transform(
            dynamic,
            [2.0, 0.0, 0.0, 1.5, 0.0, 3.0, 0.0, -2.0, 0.0, 0.0, 1.0, 0.5],
        )
        .unwrap();
    world
}

/// Compares BVH and brute force for `RAYS` seeded rays from points in
/// `offset + [-range, range]^3`, split across threads (brute force over 50k
/// triangles is slow in debug builds). Returns the number of hits.
fn compare(world: &AcousticWorld, range: f32, offset: [f32; 3], seed: u64, mask: u8) -> u32 {
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16) as u32;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                scope.spawn(move || {
                    let mut hits = 0;
                    for ray in (t..RAYS).step_by(threads as usize) {
                        let mut rng = Pcg::for_ray(seed, ray);
                        let o = [0usize, 1, 2]
                            .map(|k| offset[k] + (rng.next_f32() * 2.0 - 1.0) * range);
                        let d = uniform_sphere(rng.next_f32(), rng.next_f32());
                        let t_max = if ray % 5 == 0 { 3.0 } else { f32::INFINITY };
                        let fast = world.raycast(o, d, t_max, mask);
                        let slow = world.raycast_brute_force(o, d, t_max, mask);
                        match (fast, slow) {
                            (None, None) => {}
                            (Some(a), Some(b)) => {
                                assert_eq!(
                                    (a.instance, a.triangle),
                                    (b.instance, b.triangle),
                                    "ray {ray}: o {o:?} d {d:?}"
                                );
                                assert!((a.t - b.t).abs() <= 1e-5, "ray {ray}");
                                assert_eq!((a.normal, a.material), (b.normal, b.material));
                                hits += 1;
                            }
                            other => panic!("ray {ray}: BVH and brute force disagree: {other:?}"),
                        }
                    }
                    hits
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    })
}

#[test]
fn shoebox_bvh_matches_brute_force() {
    let world = reference_shoebox(0.5).unwrap();
    let hits = compare(&world, 4.5, [5.0, 3.0, 4.0], 11, u8::MAX);
    // Origins are inside and around the room; most rays hit.
    assert!(hits > RAYS / 2, "hits {hits}");
    let h = world
        .raycast([5.0, 3.0, 4.0], [1.0, 0.0, 0.0], f32::INFINITY, 1)
        .unwrap();
    assert!((h.t - 5.0).abs() < 1e-6, "t {}", h.t);
    assert_eq!(h.normal, [-1.0, 0.0, 0.0], "normal faces the incoming ray");
    assert_eq!(world.stats().plane_clusters, 6);
}

#[test]
fn triangle_soup_bvh_matches_brute_force() {
    let world = soup_world(5);
    assert_eq!(world.stats().triangles, 50_000);
    let hits = compare(&world, 12.0, [0.0; 3], 12, 1);
    assert!(hits > RAYS / 2, "hits {hits}");
}

#[test]
fn instanced_bvh_matches_brute_force_with_masks_and_refit() {
    let world = instanced_world();
    let a = compare(&world, 14.0, [0.0; 3], 13, 1);
    let b = compare(&world, 14.0, [0.0; 3], 14, 2);
    let c = compare(&world, 14.0, [0.0; 3], 15, u8::MAX);
    assert!(a > 0 && b > 0 && c > a && c > b, "{a} {b} {c}");
    // The moved dynamic instance (scaled 2 x 3 x 1 at (1.5, -2, 0.5)) is hit
    // where it now is: its +z face is at z = 1.0.
    let h = world
        .raycast([1.5, -2.0, 5.0], [0.0, 0.0, -1.0], 10.0, 1)
        .unwrap();
    assert_eq!(h.instance, InstanceId(64));
    assert!((h.t - 4.0).abs() < 1e-5, "t {}", h.t);
    assert_eq!(world.dynamic_generation(), 1);
}

#[test]
fn builds_are_deterministic_and_layouts_follow_the_conventions() {
    let a = soup_world(9);
    let b = soup_world(9);
    let (va, vb) = (a.gpu_view(), b.gpu_view());
    let bytes = |s: &[gpu_layout::BvhNode]| bytemuck::cast_slice::<_, u8>(s).to_vec();
    assert_eq!(bytes(va.nodes), bytes(vb.nodes));
    assert_eq!(bytes(va.top_nodes), bytes(vb.top_nodes));
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(va.triangles),
        bytemuck::cast_slice::<_, u8>(vb.triangles)
    );
    let world = instanced_world();
    let v = world.gpu_view();
    for (i, n) in v.top_nodes.iter().enumerate() {
        if n.count == 0 {
            let child = n.left_or_first as usize;
            assert!(child > i && child + 1 < v.top_nodes.len());
        } else {
            assert_eq!(n.count, 1);
            assert!((n.left_or_first as usize) < v.instances.len());
        }
    }
    for (i, n) in v.nodes.iter().enumerate() {
        if n.count == 0 {
            assert!(n.left_or_first as usize > i);
        } else {
            assert!(n.count as usize <= MESH_MAX_LEAF);
            assert!((n.left_or_first + n.count) as usize <= v.triangles.len());
        }
    }
    for t in v.triangles {
        assert_eq!(t.v0[3].to_bits(), 0, "material index in v0.w bits");
    }
    assert_eq!(v.materials[0].reflect, [0.9; 4]);
    for inst in v.instances {
        assert_eq!(inst.node_offset, 0, "every instance shares mesh 0");
    }
}
