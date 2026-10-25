//! The denoiser applied to the real RT signals (1 ray per pixel per frame):
//! AO, soft shadows, diffuse GI and glossy reflections, each against a
//! high-sample reference of the same kernel. Also checks the soft-shadow
//! penumbra against a CPU integration of the sun cone. Ignored by default;
//! run with `cargo test -p engine-render-rt --test denoise_signals -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{
    AmbientOcclusionKernel, AoInputs, Denoiser, DenoiserInputs, DenoiserSettings, DenoiserSignal,
    DiffuseGiKernel, GiInputs, REFLECTION_ROUGH_FLAG, ReflectionInputs, ReflectionKernel,
    RtAoSettings, RtDevice, RtGiSettings, RtLighting, RtMaterial, RtMaterialTable,
    RtReflectionSettings, RtScalarFormat, RtScene, RtShadowSettings, ShadowMaskKernel,
    ShadowMaskParams, ShadowMaskTargets,
};

const SIZE: u32 = 128;
/// Top-down view of x, z in [-4, 4].
const EYE: [f32; 3] = [0.0, 10.0, 0.0];
const BOX_MIN: [f32; 3] = [-1.0, 0.0, -1.0];
const BOX_MAX: [f32; 3] = [1.0, 1.5, 1.0];
const SUN: [f32; 3] = [1.0, 1.0, 0.3];
const FRAMES: u32 = 16;

fn world(i: u32) -> f32 {
    -4.0 + 8.0 * (i as f32 + 0.5) / SIZE as f32
}

/// Floor y = 0 with the box top where the box covers it.
fn top_down_gbuffer(rt: &RtDevice) -> GBuffer {
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (x, z) = (world(k % SIZE), world(k / SIZE));
            let on_box =
                (BOX_MIN[0]..=BOX_MAX[0]).contains(&x) && (BOX_MIN[2]..=BOX_MAX[2]).contains(&z);
            let y = if on_box { BOX_MAX[1] } else { 0.0 };
            Texel::surface([x, y, z], [0.0, 1.0, 0.0])
        })
        .collect();
    GBuffer::new(rt, SIZE, SIZE, &texels)
}

fn box_scene(rt: &RtDevice) -> RtScene {
    let mut scene = scene(rt);
    let floor = ground(32.0, 0.0);
    let block = cuboid(BOX_MIN, BOX_MAX);
    let floor_id = scene.add_mesh(&floor.data()).unwrap();
    let block_id = scene.add_mesh(&block.data()).unwrap();
    scene
        .add_instance(&instance(floor_id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    let mut block_instance = instance(block_id, translation(0.0, 0.0, 0.0));
    block_instance.material_index = 1;
    scene.add_instance(&block_instance).unwrap();
    scene.commit().unwrap();
    scene
}

/// Flattened channels of a read-back signal (x only for scalars).
fn channels(texels: &[[f32; 4]], signal: DenoiserSignal) -> Vec<f32> {
    match signal {
        DenoiserSignal::Scalar => texels.iter().map(|t| t[0]).collect(),
        _ => texels.iter().flat_map(|t| [t[0], t[1], t[2]]).collect(),
    }
}

fn read_signal(rt: &RtDevice, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    if texture.format() == wgpu::TextureFormat::Rgba16Float {
        read_rgba16f(rt, texture)
    } else {
        read_scalar(rt, texture)
            .iter()
            .map(|&v| [v, 0.0, 0.0, 1.0])
            .collect()
    }
}

/// Runs `dispatch(encoder, frame)` (one kernel dispatch into `signal`) for
/// FRAMES frames through a denoiser, and returns (1-spp RMSE of the first
/// frame, denoised RMSE after FRAMES frames) against `reference`.
fn denoise(
    rt: &RtDevice,
    label: &str,
    gbuffer: &GBuffer,
    kind: DenoiserSignal,
    signal: &wgpu::Texture,
    reference: &[f32],
    mut dispatch: impl FnMut(&mut wgpu::CommandEncoder, u32),
) -> (f64, f64) {
    let signal_view = view(signal);
    let motion = input_texture(
        rt,
        wgpu::TextureFormat::Rg32Float,
        SIZE,
        SIZE,
        &vec![0u8; (SIZE * SIZE * 8) as usize],
    );
    let motion_view = view(&motion);
    let mut denoiser = Denoiser::new(rt, SIZE, SIZE, kind, &DenoiserSettings::default()).unwrap();
    let mut input_rmse = 0.0;
    let start = std::time::Instant::now();
    for frame in 0..FRAMES {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        // Frame indices far from the reference's, so the noise is independent.
        dispatch(&mut encoder, 1000 + frame);
        denoiser
            .record(
                &mut encoder,
                &DenoiserInputs {
                    gbuffer: gbuffer.views(),
                    signal: &signal_view,
                    motion: &motion_view,
                    eye: EYE,
                    camera_cut: false,
                },
            )
            .unwrap();
        submit(rt, encoder);
        if frame == 0 {
            input_rmse = rmse(&channels(&read_signal(rt, signal), kind), reference);
        }
    }
    let per_frame = start.elapsed() / FRAMES;
    let out = read_rgba16f(rt, denoiser.output());
    assert!(out.iter().flatten().all(|v| v.is_finite()));
    let out_rmse = rmse(&channels(&out, kind), reference);
    println!(
        "{label}: 1-spp RMSE {input_rmse:.4} -> denoised ({FRAMES} frames) {out_rmse:.4}, \
         {:.1}x lower; {per_frame:?} per frame (kernel + denoiser + wait)",
        input_rmse / out_rmse
    );
    assert!(
        input_rmse / out_rmse >= 3.0,
        "{label}: RMSE only {:.2}x lower",
        input_rmse / out_rmse
    );
    (input_rmse, out_rmse)
}

/// Mean of `frames` reads of `signal` after `dispatch(encoder, frame)`.
fn reference(
    rt: &RtDevice,
    kind: DenoiserSignal,
    signal: &wgpu::Texture,
    frames: u32,
    mut dispatch: impl FnMut(&mut wgpu::CommandEncoder, u32),
) -> Vec<f32> {
    let mut sum: Vec<f64> = Vec::new();
    for frame in 0..frames {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        dispatch(&mut encoder, frame);
        submit(rt, encoder);
        let values = channels(&read_signal(rt, signal), kind);
        sum.resize(values.len(), 0.0);
        for (s, v) in sum.iter_mut().zip(values) {
            *s += f64::from(v);
        }
    }
    sum.iter().map(|s| (s / f64::from(frames)) as f32).collect()
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn denoiser_reduces_ao_and_soft_shadow_noise() {
    let rt = open();
    let scene = box_scene(&rt);
    let gbuffer = top_down_gbuffer(&rt);
    let format = scalar_format(&rt);
    let output = output_texture(&rt, format.texture_format(), SIZE, SIZE);
    let output_view = view(&output);

    let ao = AmbientOcclusionKernel::new(&rt, format).unwrap();
    let ao_dispatch = |rays: u32| {
        let (ao, scene, gbuffer, output_view) = (&ao, &scene, &gbuffer, &output_view);
        move |encoder: &mut wgpu::CommandEncoder, frame: u32| {
            let settings = RtAoSettings {
                radius: 1.0,
                rays_per_pixel: rays,
                ..Default::default()
            };
            let inputs = AoInputs {
                gbuffer: gbuffer.views(),
                output: output_view,
                frame_index: frame,
            };
            ao.record(encoder, scene, &inputs, &settings).unwrap();
        }
    };
    let ao_reference = reference(&rt, DenoiserSignal::Scalar, &output, 16, ao_dispatch(64));
    denoise(
        &rt,
        "AO (radius 1)",
        &gbuffer,
        DenoiserSignal::Scalar,
        &output,
        &ao_reference,
        ao_dispatch(1),
    );

    let shadow = ShadowMaskKernel::new(&rt, format).unwrap();
    let shadow_dispatch = |samples: u32| {
        let (shadow, scene, gbuffer, output_view) = (&shadow, &scene, &gbuffer, &output_view);
        move |encoder: &mut wgpu::CommandEncoder, frame: u32| {
            let settings = RtShadowSettings {
                sun_angular_radius: 0.05,
                samples_per_pixel: samples,
                ..Default::default()
            };
            let targets = ShadowMaskTargets {
                world_position: &gbuffer.position_view,
                world_normal: &gbuffer.normal_view,
                output: output_view,
                width: SIZE,
                height: SIZE,
            };
            let params = ShadowMaskParams::from_settings(&settings, SUN, frame);
            shadow.record(encoder, scene, &targets, &params).unwrap();
        }
    };
    let shadow_reference = reference(
        &rt,
        DenoiserSignal::Scalar,
        &output,
        16,
        shadow_dispatch(64),
    );
    denoise(
        &rt,
        "soft shadow (angular radius 0.05)",
        &gbuffer,
        DenoiserSignal::Scalar,
        &output,
        &shadow_reference,
        shadow_dispatch(1),
    );
    assert!(rt.status().is_hardware());
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn denoiser_reduces_gi_and_glossy_reflection_noise() {
    let rt = open();
    let scene = box_scene(&rt);
    let gbuffer = top_down_gbuffer(&rt);
    let materials = RtMaterialTable::new(
        &rt,
        &[
            RtMaterial {
                albedo: [0.7; 3],
                emissive: [0.0; 3],
            },
            RtMaterial {
                albedo: [0.8, 0.2, 0.1],
                emissive: [0.5, 0.1, 0.05],
            },
        ],
    )
    .unwrap();
    let lighting = RtLighting {
        sun_direction: SUN,
        sun_illuminance: [3.0; 3],
        sky_zenith: [0.2, 0.3, 0.6],
        sky_horizon: [0.4, 0.4, 0.4],
    };
    let output = output_texture(&rt, wgpu::TextureFormat::Rgba16Float, SIZE, SIZE);
    let output_view = view(&output);

    let gi = DiffuseGiKernel::new(&rt).unwrap();
    let gi_dispatch = |rays: u32| {
        let (gi, scene, gbuffer, output_view, materials, lighting) =
            (&gi, &scene, &gbuffer, &output_view, &materials, &lighting);
        move |encoder: &mut wgpu::CommandEncoder, frame: u32| {
            let settings = RtGiSettings {
                rays_per_pixel: rays,
                bounces: 2,
                ..Default::default()
            };
            let inputs = GiInputs {
                gbuffer: gbuffer.views(),
                output: output_view,
                materials,
                lighting,
                frame_index: frame,
            };
            gi.record(encoder, scene, &inputs, &settings).unwrap();
        }
    };
    let gi_reference = reference(&rt, DenoiserSignal::Rgb, &output, 16, gi_dispatch(64));
    denoise(
        &rt,
        "diffuse GI (2 bounces)",
        &gbuffer,
        DenoiserSignal::Rgb,
        &output,
        &gi_reference,
        gi_dispatch(1),
    );

    // Glossy floor (roughness 0.3) and box top; columns 0..8 are rough
    // (above the cutoff) and must pass through the denoiser flagged.
    let roughness: Vec<f32> = (0..SIZE * SIZE)
        .map(|k| if k % SIZE < 8 { 0.9 } else { 0.3 })
        .collect();
    let roughness_texture = input_texture(
        &rt,
        wgpu::TextureFormat::R32Float,
        SIZE,
        SIZE,
        bytemuck::cast_slice(&roughness),
    );
    let roughness_view = view(&roughness_texture);
    // A camera off to the side so the glossy floor reflects the box.
    let reflections = ReflectionKernel::new(&rt).unwrap();
    let reflection_dispatch = {
        let (reflections, scene, gbuffer, output_view, materials, lighting, roughness_view) = (
            &reflections,
            &scene,
            &gbuffer,
            &output_view,
            &materials,
            &lighting,
            &roughness_view,
        );
        move |encoder: &mut wgpu::CommandEncoder, frame: u32| {
            let inputs = ReflectionInputs {
                gbuffer: gbuffer.views(),
                roughness: roughness_view,
                output: output_view,
                materials,
                lighting,
                eye: [0.0, 3.0, 8.0],
                frame_index: frame,
            };
            reflections
                .record(encoder, scene, &inputs, &RtReflectionSettings::default())
                .unwrap();
        }
    };
    let reflection_reference = reference(
        &rt,
        DenoiserSignal::RgbHitDistance,
        &output,
        1024,
        reflection_dispatch,
    );
    let signal_view = view(&output);
    let motion = input_texture(
        &rt,
        wgpu::TextureFormat::Rg32Float,
        SIZE,
        SIZE,
        &vec![0u8; (SIZE * SIZE * 8) as usize],
    );
    let motion_view = view(&motion);
    // Flagged pixels pass through unchanged.
    let mut denoiser = Denoiser::new(
        &rt,
        SIZE,
        SIZE,
        DenoiserSignal::RgbHitDistance,
        &DenoiserSettings::default(),
    )
    .unwrap();
    let mut encoder = rt.device().create_command_encoder(&Default::default());
    reflection_dispatch(&mut encoder, 0);
    denoiser
        .record(
            &mut encoder,
            &DenoiserInputs {
                gbuffer: gbuffer.views(),
                signal: &signal_view,
                motion: &motion_view,
                eye: EYE,
                camera_cut: false,
            },
        )
        .unwrap();
    submit(&rt, encoder);
    let out = read_rgba16f(&rt, denoiser.output());
    let raw = read_rgba16f(&rt, &output);
    for k in 0..(SIZE * SIZE) as usize {
        if (k as u32) % SIZE < 8 {
            assert_eq!(out[k], [0.0, 0.0, 0.0, REFLECTION_ROUGH_FLAG]);
        } else {
            assert_eq!(out[k][3], raw[k][3], "hit distance is kept");
        }
    }
    denoise(
        &rt,
        "glossy reflections (roughness 0.3)",
        &gbuffer,
        DenoiserSignal::RgbHitDistance,
        &output,
        &reflection_reference,
        reflection_dispatch,
    );
    assert!(rt.status().is_hardware());
}

/// CPU visibility of the sun cone (half-angle `radius` about `SUN`) from a
/// floor point: 4096 stratified cone directions against the box.
fn cpu_soft_visibility(p: [f32; 3], radius: f32) -> f32 {
    let d = normalize(SUN);
    // Basis about d.
    let t = normalize([d[2], 0.0, -d[0]]);
    let b = [
        d[1] * t[2] - d[2] * t[1],
        d[2] * t[0] - d[0] * t[2],
        d[0] * t[1] - d[1] * t[0],
    ];
    let n = 64;
    let cos_max = radius.cos();
    let mut visible = 0;
    for i in 0..n {
        for j in 0..n {
            let cos_t = 1.0 - (i as f32 + 0.5) / n as f32 * (1.0 - cos_max);
            let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
            let phi = std::f32::consts::TAU * (j as f32 + 0.5) / n as f32;
            let (s, c) = phi.sin_cos();
            let dir: [f32; 3] =
                std::array::from_fn(|a| d[a] * cos_t + (t[a] * c + b[a] * s) * sin_t);
            let origin = [p[0], p[1] + 1e-3, p[2]];
            if ray_box(origin, dir, BOX_MIN, BOX_MAX, 1e4).is_none() {
                visible += 1;
            }
        }
    }
    visible as f32 / (n * n) as f32
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn soft_shadow_penumbra_matches_the_sun_cone() {
    let rt = open();
    let scene = box_scene(&rt);
    let gbuffer = top_down_gbuffer(&rt);
    let output = output_texture(&rt, wgpu::TextureFormat::R32Float, SIZE, SIZE);
    let output_view = view(&output);
    let kernel = ShadowMaskKernel::new(&rt, RtScalarFormat::R32Float).unwrap();
    for radius in [0.02f32, 0.05, 0.1] {
        let mask = reference(
            &rt,
            DenoiserSignal::Scalar,
            &output,
            32,
            |encoder, frame| {
                let params = ShadowMaskParams {
                    direction_to_light: SUN,
                    angular_radius: radius,
                    samples: 64,
                    frame_index: frame,
                    ..Default::default()
                };
                let targets = ShadowMaskTargets {
                    world_position: &gbuffer.position_view,
                    world_normal: &gbuffer.normal_view,
                    output: &output_view,
                    width: SIZE,
                    height: SIZE,
                };
                kernel.record(encoder, &scene, &targets, &params).unwrap();
            },
        );
        // Compare a strip of floor rows through the shadow (z near 0).
        let (mut max_error, mut penumbra) = (0.0f32, 0u32);
        for j in (SIZE / 2 - 4)..(SIZE / 2 + 4) {
            for i in 0..SIZE / 2 - 16 {
                let p = [world(i), 0.0, world(j)];
                let expected = cpu_soft_visibility(p, radius);
                let v = mask[(j * SIZE + i) as usize];
                max_error = max_error.max((v - expected).abs());
                if j == SIZE / 2 && v > 0.02 && v < 0.98 {
                    penumbra += 1;
                }
            }
        }
        println!(
            "soft shadow, angular radius {radius}: max |GPU - CPU| visibility {max_error:.4} \
             (2048 GPU samples per pixel); penumbra {penumbra} px wide along the shadow axis"
        );
        assert!(max_error <= 0.05, "penumbra error {max_error}");
        assert!(penumbra >= 1);
    }
    assert!(rt.status().is_hardware());
}
