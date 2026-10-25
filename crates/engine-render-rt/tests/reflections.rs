//! RT reflections: a mirror floor below an emissive, sun-lit box shows the
//! box exactly where a CPU mirror-ray test hits it and the sky elsewhere;
//! rough pixels are flagged, glossy pixels are sampled stochastically.
//! Ignored by default; run with
//! `cargo test -p engine-render-rt --test reflections -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    REFLECTION_MISS_DISTANCE, REFLECTION_NO_GEOMETRY_FLAG, REFLECTION_ROUGH_FLAG, ReflectionInputs,
    ReflectionKernel, RtLighting, RtMaterial, RtMaterialTable, RtReflectionSettings,
};

const SIZE: u32 = 128;
const EYE: [f32; 3] = [0.0, 3.0, 6.0];
const BOX_MIN: [f32; 3] = [-1.0, 0.5, -3.0];
const BOX_MAX: [f32; 3] = [1.0, 1.5, -1.0];
const EMISSIVE: [f32; 3] = [1.0, 0.25, 0.125];
const ALBEDO: f32 = 0.5;
const SKY: [f32; 3] = [0.2, 0.4, 0.8];
const SUN: [f32; 3] = [0.0, 0.6, 0.8];
const NORMAL_BIAS: f32 = 1e-3;

/// Columns 0..16 are rough (above the cutoff); further odd columns are
/// glossy and even columns mirrors.
fn roughness_of_column(i: u32) -> f32 {
    match i {
        0..16 => 0.9,
        _ if i % 2 == 1 => 0.3,
        _ => 0.0,
    }
}

fn floor_point(i: u32, j: u32) -> [f32; 3] {
    let x = -4.0 + 8.0 * (i as f32 + 0.5) / SIZE as f32;
    let z = -4.0 + 8.0 * (j as f32 + 0.5) / SIZE as f32;
    [x, 0.0, z]
}

/// Closest entry into the box grown by `grow`: distance and the entry face's
/// outward normal.
fn hit_box(origin: [f32; 3], dir: [f32; 3], grow: f32) -> Option<(f32, [f32; 3])> {
    let (mut near, mut far, mut axis) = (0.0f32, 1000.0f32, 0usize);
    for a in 0..3 {
        if dir[a].abs() < 1e-12 {
            if origin[a] < BOX_MIN[a] - grow || origin[a] > BOX_MAX[a] + grow {
                return None;
            }
            continue;
        }
        let t0 = (BOX_MIN[a] - grow - origin[a]) / dir[a];
        let t1 = (BOX_MAX[a] + grow - origin[a]) / dir[a];
        if t0.min(t1) > near {
            near = t0.min(t1);
            axis = a;
        }
        far = far.min(t0.max(t1));
        if near > far {
            return None;
        }
    }
    let mut n = [0.0; 3];
    n[axis] = -dir[axis].signum();
    Some((near, n))
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn mirror_floor_reflects_the_box_and_the_sky_and_flags_rough_pixels() {
    let rt = open();
    let mut scene = scene(&rt);
    let floor = ground(8.0, 0.0);
    let block = cuboid(BOX_MIN, BOX_MAX);
    let floor_id = scene.add_mesh(&floor.data()).unwrap();
    let block_id = scene.add_mesh(&block.data()).unwrap();
    scene
        .add_instance(&instance(floor_id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    let mut box_instance = instance(block_id, translation(0.0, 0.0, 0.0));
    box_instance.material_index = 1;
    scene.add_instance(&box_instance).unwrap();
    scene.commit().unwrap();
    let materials = RtMaterialTable::new(
        &rt,
        &[
            RtMaterial {
                albedo: [0.5; 3],
                emissive: [0.0; 3],
            },
            RtMaterial {
                albedo: [ALBEDO; 3],
                emissive: EMISSIVE,
            },
        ],
    )
    .unwrap();
    let pi = std::f32::consts::PI;
    let lighting = RtLighting {
        sun_direction: SUN,
        sun_illuminance: [pi; 3],
        sky_zenith: SKY,
        sky_horizon: SKY,
    };
    // Row 0 is background.
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| match (k % SIZE, k / SIZE) {
            (_, 0) => Texel::BACKGROUND,
            (i, j) => Texel::surface(floor_point(i, j), [0.0, 1.0, 0.0]),
        })
        .collect();
    let gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    let roughness: Vec<f32> = (0..SIZE * SIZE)
        .map(|k| roughness_of_column(k % SIZE))
        .collect();
    let roughness_texture = input_texture(
        &rt,
        wgpu::TextureFormat::R32Float,
        SIZE,
        SIZE,
        bytemuck::cast_slice(&roughness),
    );
    let roughness_view = view(&roughness_texture);
    let output = output_texture(&rt, wgpu::TextureFormat::Rgba16Float, SIZE, SIZE);
    let output_view = view(&output);
    let kernel = ReflectionKernel::new(&rt).unwrap();
    let settings = RtReflectionSettings {
        roughness_cutoff: 0.5,
        normal_bias: NORMAL_BIAS,
        ..Default::default()
    };
    let frames = 8;
    let mut glossy_hit_frames = vec![0u32; (SIZE * SIZE) as usize];
    for frame in 0..frames {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        kernel
            .record(
                &mut encoder,
                &scene,
                &ReflectionInputs {
                    gbuffer: gbuffer.views(),
                    roughness: &roughness_view,
                    output: &output_view,
                    materials: &materials,
                    lighting: &lighting,
                    eye: EYE,
                    frame_index: frame,
                },
                &settings,
            )
            .unwrap();
        let start = std::time::Instant::now();
        submit(&rt, encoder);
        let elapsed = start.elapsed();
        let texels = read_rgba16f(&rt, &output);
        let (mut robust, mut hits, mut mismatches, mut max_rgb_error, mut max_t_error) =
            (0u32, 0u32, 0u32, 0.0f32, 0.0f32);
        let (mut flagged, mut background) = (0u32, 0u32);
        for j in 0..SIZE {
            for i in 0..SIZE {
                let k = (j * SIZE + i) as usize;
                let out = texels[k];
                assert!(
                    out.iter().all(|v| v.is_finite()),
                    "pixel ({i}, {j}): {out:?}"
                );
                if j == 0 {
                    assert_eq!(out, [0.0, 0.0, 0.0, REFLECTION_NO_GEOMETRY_FLAG]);
                    background += 1;
                    continue;
                }
                match roughness_of_column(i) {
                    r if r > 0.5 => {
                        assert_eq!(
                            out,
                            [0.0, 0.0, 0.0, REFLECTION_ROUGH_FLAG],
                            "rough pixel ({i}, {j})"
                        );
                        flagged += 1;
                    }
                    r if r > 0.0 => {
                        // One GGX sample: either the box or the sky.
                        assert!(out[3] > 0.0);
                        if out[3] < REFLECTION_MISS_DISTANCE {
                            glossy_hit_frames[k] += 1;
                        } else {
                            assert!((out[2] - SKY[2]).abs() < 1e-3, "glossy miss {out:?}");
                        }
                    }
                    _ => {
                        let p = floor_point(i, j);
                        let v = normalize([EYE[0] - p[0], EYE[1] - p[1], EYE[2] - p[2]]);
                        let dir = [-v[0], v[1], -v[2]];
                        let origin = [p[0], p[1] + NORMAL_BIAS, p[2]];
                        let inner = hit_box(origin, dir, -2e-3);
                        let outer = hit_box(origin, dir, 2e-3);
                        if inner.is_some() != outer.is_some() {
                            continue;
                        }
                        robust += 1;
                        match inner.and(hit_box(origin, dir, 0.0)) {
                            Some((t, n)) => {
                                hits += 1;
                                let cos = (n[1] * SUN[1] + n[2] * SUN[2]).max(0.0);
                                let expected: Vec<f32> =
                                    EMISSIVE.iter().map(|e| e + ALBEDO * cos).collect();
                                if out[3] >= REFLECTION_MISS_DISTANCE {
                                    mismatches += 1;
                                    continue;
                                }
                                for c in 0..3 {
                                    max_rgb_error = max_rgb_error
                                        .max((out[c] - expected[c]).abs() / expected[c]);
                                }
                                max_t_error = max_t_error.max((out[3] - t).abs() / t);
                            }
                            None => {
                                if out[3] != REFLECTION_MISS_DISTANCE {
                                    mismatches += 1;
                                    continue;
                                }
                                for c in 0..3 {
                                    max_rgb_error =
                                        max_rgb_error.max((out[c] - SKY[c]).abs() / SKY[c]);
                                }
                            }
                        }
                    }
                }
            }
        }
        if frame == 0 {
            println!(
                "mirror: {robust} robust pixels, {hits} reflect the box, {mismatches} mismatches \
                 vs the CPU mirror ray; max relative radiance error {max_rgb_error:.5}, max \
                 relative hit-distance error {max_t_error:.5}; {flagged} rough pixels flagged, \
                 {background} background; dispatch + wait {elapsed:?}"
            );
        }
        assert_eq!(mismatches, 0);
        assert!(hits >= 250, "only {hits} mirror pixels reflect the box");
        // f16 output: 2^-10 relative.
        assert!(max_rgb_error <= 2e-3, "radiance error {max_rgb_error}");
        assert!(max_t_error <= 2e-3, "hit distance error {max_t_error}");
        assert_eq!(flagged, 16 * (SIZE - 1));
    }
    // Glossy pixels near the box's reflection hit it in some frames only.
    let partial = glossy_hit_frames
        .iter()
        .filter(|&&n| n > 0 && n < frames)
        .count();
    let always = glossy_hit_frames.iter().filter(|&&n| n == frames).count();
    println!(
        "glossy (roughness 0.3): {partial} pixels hit the box in some of {frames} frames, {always} in all"
    );
    assert!(partial > 20, "GGX sampling must vary between frames");
    assert!(rt.status().is_hardware());
}
