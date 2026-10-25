//! GPU nearest hits (both tiers) against the CPU BVH of engine-acoustics on the
//! same worlds. Ignored by default; run with
//! `cargo test -p engine-acoustics-gpu --test bvh_parity -- --ignored --nocapture`.

mod common;

use common::*;
use engine_acoustics::rng::{Pcg, uniform_sphere};
use engine_acoustics::{
    AcousticWorld, AcousticWorldBuilder, Affine3, Mobility, PropagationBackend, WorldLimits,
};
use engine_acoustics_gpu::{GpuBackend, GpuOptions};

const RAYS: usize = 100_000;

fn rays(seed: u64, min: [f32; 3], max: [f32; 3]) -> Vec<([f32; 3], [f32; 3], f32)> {
    let mut rng = Pcg::for_ray(seed, 0);
    (0..RAYS)
        .map(|_| {
            let o = std::array::from_fn(|k| min[k] + (max[k] - min[k]) * rng.next_f32());
            let d = uniform_sphere(rng.next_f32(), rng.next_f32());
            (o, d, 1e6)
        })
        .collect()
}

fn compare(
    label: &str,
    backend: &mut GpuBackend,
    world: &AcousticWorld,
    rays: &[([f32; 3], [f32; 3], f32)],
) {
    let gpu = backend.raycast_batch(world, rays).expect("GPU raycast");
    let view = world.gpu_view();
    let (mut presence, mut identity, mut max_dt, mut hits) = (0, 0, 0.0f32, 0);
    for ((o, d, t_max), g) in rays.iter().zip(&gpu) {
        let c = world.raycast(*o, *d, *t_max, 0xff);
        match (c, g) {
            (Some(c), Some(g)) => {
                hits += 1;
                max_dt = max_dt.max((c.t - g.t).abs());
                let original = view.triangles[g.triangle as usize].v1[3].to_bits();
                if c.instance.0 != g.instance || c.triangle != original {
                    identity += 1;
                }
            }
            (None, None) => {}
            _ => presence += 1,
        }
    }
    println!(
        "{label}: {} rays, {hits} hits, hit/miss disagreements {presence}, \
         instance/triangle mismatches {identity}, max |dt| {max_dt:.2e} m",
        rays.len()
    );
    // Edge and tie cases may resolve differently between implementations (the
    // hardware intersector is watertight, Moller-Trumbore is not): allow 0.05%.
    assert!(
        presence + identity <= rays.len() / 2000,
        "{label}: too many mismatches"
    );
    // Hit distances agree to well under 1 mm (hardware intersection is less
    // precise at grazing angles; acoustically irrelevant).
    assert!(max_dt <= 1e-3, "{label}: t differs by {max_dt} m");
}

fn rotation_y(angle: f32, scale: f32, t: [f32; 3]) -> Affine3 {
    let (s, c) = angle.sin_cos();
    [
        c * scale,
        0.0,
        s * scale,
        t[0],
        0.0,
        scale,
        0.0,
        t[1],
        -s * scale,
        0.0,
        c * scale,
        t[2],
    ]
}

/// Instanced, rotated and scaled copies of a subdivided box inside the shoebox.
fn instanced_world() -> AcousticWorld {
    let mut b = AcousticWorldBuilder::new(WorldLimits::default());
    let m = b.add_material(material(REFERENCE_ALPHA, 0.5)).unwrap();
    let room = b.add_mesh(box_mesh([10.0, 6.0, 8.0], 4, m)).unwrap();
    b.add_instance(room, IDENTITY, Mobility::Static, 0xff)
        .unwrap();
    let block = b.add_mesh(box_mesh([1.0, 1.0, 1.0], 3, m)).unwrap();
    for i in 0..12 {
        let angle = i as f32 * 0.37;
        let scale = 0.5 + 0.1 * i as f32;
        let t = [
            1.0 + 0.7 * i as f32,
            0.5 + 0.3 * (i % 4) as f32,
            1.0 + 0.5 * (i % 7) as f32,
        ];
        b.add_instance(block, rotation_y(angle, scale, t), Mobility::Static, 0xff)
            .unwrap();
    }
    b.build().unwrap()
}

fn tiers() -> Vec<GpuBackend> {
    let mut tiers = vec![
        GpuBackend::ray_query(GpuOptions::default()),
        GpuBackend::compute(GpuOptions::default()),
    ];
    for t in &mut tiers {
        t.initialize().expect("tier");
    }
    tiers
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn gpu_nearest_hits_match_the_cpu_bvh() {
    let shoebox = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
    let dense = shoebox_world(REFERENCE_ALPHA, 1.0, 40);
    let instanced = instanced_world();
    let (mut l_room, panel) = l_room_world();
    let inside = rays(11, [0.1; 3], [9.9, 5.9, 7.9]);
    let outside = rays(12, [-5.0; 3], [15.0, 11.0, 13.0]);
    let l_rays = rays(13, [0.1, 0.1, 0.1], [9.9, 2.9, 9.9]);
    for mut backend in tiers() {
        let tier = format!("{:?}", backend.info().kind);
        compare(&format!("{tier} shoebox"), &mut backend, &shoebox, &inside);
        compare(
            &format!("{tier} shoebox (outside)"),
            &mut backend,
            &shoebox,
            &outside,
        );
        compare(
            &format!("{tier} dense shoebox"),
            &mut backend,
            &dense,
            &inside,
        );
        compare(
            &format!("{tier} instanced"),
            &mut backend,
            &instanced,
            &inside,
        );
        compare(&format!("{tier} L-room"), &mut backend, &l_room, &l_rays);
        l_room
            .set_instance_transform(panel, translation(7.0, 0.0, 0.5))
            .unwrap();
        compare(
            &format!("{tier} L-room moved panel"),
            &mut backend,
            &l_room,
            &l_rays,
        );
        l_room
            .set_instance_transform(panel, translation(5.0, 0.0, 0.5))
            .unwrap();
    }
}
