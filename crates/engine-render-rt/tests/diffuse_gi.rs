//! RT diffuse GI: colour bleeding from a sun-lit red wall onto a white floor,
//! and a closed furnace whose multi-bounce result matches the geometric series
//! with no NaN/Inf. Ignored by default; run with
//! `cargo test -p engine-render-rt --test diffuse_gi -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    DiffuseGiKernel, GiInputs, RtDevice, RtGiSettings, RtLighting, RtMaterial, RtMaterialTable,
    RtScene,
};

const SIZE: u32 = 64;

/// Mean GI over `frames` dispatches, per pixel (rgb).
fn gi(
    rt: &RtDevice,
    scene: &RtScene,
    materials: &RtMaterialTable,
    gbuffer: &GBuffer,
    lighting: &RtLighting,
    settings: &RtGiSettings,
    frames: u32,
) -> Vec<[f32; 3]> {
    let kernel = DiffuseGiKernel::new(rt).unwrap();
    let output = output_texture(rt, wgpu::TextureFormat::Rgba16Float, SIZE, SIZE);
    let output_view = view(&output);
    let mut sum = vec![[0f32; 3]; (SIZE * SIZE) as usize];
    let start = std::time::Instant::now();
    for frame in 0..frames {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        kernel
            .record(
                &mut encoder,
                scene,
                &GiInputs {
                    gbuffer: gbuffer.views(),
                    output: &output_view,
                    materials,
                    lighting,
                    frame_index: frame,
                },
                settings,
            )
            .unwrap();
        submit(rt, encoder);
        for (s, t) in sum.iter_mut().zip(read_rgba16f(rt, &output)) {
            assert!(
                t.iter().all(|v| v.is_finite() && *v >= 0.0),
                "GI texel {t:?}"
            );
            assert_eq!(t[3], 1.0);
            for c in 0..3 {
                s[c] += t[c];
            }
        }
    }
    println!(
        "  {SIZE}x{SIZE}, {} rays x {} bounces: {:?} per frame incl. readback",
        settings.rays_per_pixel,
        settings.bounces,
        start.elapsed() / frames
    );
    sum.iter().map(|s| s.map(|v| v / frames as f32)).collect()
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn sun_lit_red_wall_bleeds_onto_the_white_floor() {
    let rt = open();
    let mut scene = scene(&rt);
    // White floor x in [0, 32]; red wall x = 0 facing +x, 4 m tall.
    let floor = quad([
        [0.0, 0.0, -16.0],
        [32.0, 0.0, -16.0],
        [32.0, 0.0, 16.0],
        [0.0, 0.0, 16.0],
    ]);
    let wall = quad([
        [0.0, 0.0, -16.0],
        [0.0, 4.0, -16.0],
        [0.0, 4.0, 16.0],
        [0.0, 0.0, 16.0],
    ]);
    let floor_id = scene.add_mesh(&floor.data()).unwrap();
    let wall_id = scene.add_mesh(&wall.data()).unwrap();
    scene
        .add_instance(&instance(floor_id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    let mut wall_instance = instance(wall_id, translation(0.0, 0.0, 0.0));
    wall_instance.material_index = 1;
    scene.add_instance(&wall_instance).unwrap();
    scene.commit().unwrap();
    let materials = RtMaterialTable::new(
        &rt,
        &[
            RtMaterial {
                albedo: [0.8; 3],
                emissive: [0.0; 3],
            },
            RtMaterial {
                albedo: [0.8, 0.05, 0.05],
                emissive: [0.0; 3],
            },
        ],
    )
    .unwrap();
    // The sun lights the wall's +x face and the floor; dim grey sky.
    let lighting = RtLighting {
        sun_direction: [1.0, 1.0, 0.0],
        sun_illuminance: [3.0; 3],
        sky_zenith: [0.2; 3],
        sky_horizon: [0.2; 3],
    };
    // Column i: distance from the wall 0.1 .. 12.7 m; rows along z in [-4, 4].
    let distance = |i: u32| 0.1 + 0.2 * i as f32;
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = (k % SIZE, k / SIZE);
            let z = -4.0 + 8.0 * (j as f32 + 0.5) / SIZE as f32;
            Texel::surface([distance(i), 0.0, z], [0.0, 1.0, 0.0])
        })
        .collect();
    let gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    let settings = RtGiSettings {
        rays_per_pixel: 16,
        bounces: 2,
        ..Default::default()
    };
    let values = gi(&rt, &scene, &materials, &gbuffer, &lighting, &settings, 8);
    let column = |i: u32| {
        let mut s = [0f64; 3];
        for j in 0..SIZE {
            let t = values[(j * SIZE + i) as usize];
            for c in 0..3 {
                s[c] += f64::from(t[c]) / f64::from(SIZE);
            }
        }
        s
    };
    let ratio = |c: [f64; 3]| c[0] / c[1];
    let near = column(0);
    let mid = column(8);
    let far = column(SIZE - 1);
    println!(
        "colour bleeding: d = {:.1} m rgb ({:.4}, {:.4}, {:.4}) R/G {:.3}; d = {:.1} m R/G {:.3}; \
         d = {:.1} m rgb ({:.4}, {:.4}, {:.4}) R/G {:.3}",
        distance(0),
        near[0],
        near[1],
        near[2],
        ratio(near),
        distance(8),
        ratio(mid),
        distance(SIZE - 1),
        far[0],
        far[1],
        far[2],
        ratio(far)
    );
    assert!(
        ratio(near) > 1.5 * ratio(far),
        "no colour bleeding near the wall"
    );
    assert!(ratio(near) > ratio(mid) && ratio(mid) > ratio(far));
    assert!(
        (ratio(far) - 1.0).abs() < 0.1,
        "far from the wall GI is neutral: {}",
        ratio(far)
    );
    assert!(rt.status().is_hardware());
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn closed_furnace_matches_the_geometric_series() {
    // Inside a closed box of albedo a and emission E (no sun, no sky reaches
    // it), B segments gather E (1 - a^B) / (1 - a) exactly.
    let rt = open();
    let mut scene = scene(&rt);
    let shell = cuboid([-2.0, -2.0, -2.0], [2.0, 2.0, 2.0]);
    let id = scene.add_mesh(&shell.data()).unwrap();
    scene
        .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();
    let (a, e) = ([0.5f32, 0.7, 0.9], 0.25f32);
    let materials = RtMaterialTable::new(
        &rt,
        &[RtMaterial {
            albedo: a,
            emissive: [e; 3],
        }],
    )
    .unwrap();
    let lighting = RtLighting {
        sun_direction: [0.3, 1.0, 0.2],
        sun_illuminance: [5.0; 3],
        sky_zenith: [10.0; 3],
        sky_horizon: [10.0; 3],
    };
    // Points on the inside of the floor and of one wall.
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = (k % SIZE, k / SIZE);
            let u = -1.9 + 3.8 * (i as f32 + 0.5) / SIZE as f32;
            let v = -1.9 + 3.8 * (j as f32 + 0.5) / SIZE as f32;
            if j % 2 == 0 {
                Texel::surface([u, -2.0, v], [0.0, 1.0, 0.0])
            } else {
                Texel::surface([2.0, u, v], [-1.0, 0.0, 0.0])
            }
        })
        .collect();
    let gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    for bounces in [1u32, 2, 3, 8] {
        let settings = RtGiSettings {
            rays_per_pixel: 8,
            bounces,
            ..Default::default()
        };
        let values = gi(&rt, &scene, &materials, &gbuffer, &lighting, &settings, 4);
        for c in 0..3 {
            let expected = e * (1.0 - a[c].powi(bounces as i32)) / (1.0 - a[c]);
            let measured =
                values.iter().map(|t| f64::from(t[c])).sum::<f64>() / values.len() as f64;
            let error = (measured as f32 - expected).abs() / expected;
            println!(
                "furnace {bounces} bounce(s), albedo {}: mean {measured:.5}, expected {expected:.5}, error {:.3}%",
                a[c],
                error * 100.0
            );
            // Every path gathers exactly the series (no randomness reaches the
            // value when nothing escapes); f16 output rounding only.
            assert!(error <= 2e-3, "furnace error {error}");
        }
    }
    assert!(rt.status().is_hardware());
}
