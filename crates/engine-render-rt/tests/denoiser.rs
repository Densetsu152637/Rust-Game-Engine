//! Spatio-temporal denoiser on synthetic signals: noise reduction against a
//! known smooth target, edge preservation at a depth discontinuity, no ghost
//! after disocclusion, and history reset on a camera cut. Ignored by default;
//! run with `cargo test -p engine-render-rt --test denoiser -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{Denoiser, DenoiserInputs, DenoiserSettings, DenoiserSignal, RtDevice};

const SIZE: u32 = 128;
const EYE: [f32; 3] = [0.0, 0.0, 10.0];
const PIXEL: f32 = 0.05;

/// A camera-facing plane z = 0 (10 m from the eye) covering the image.
fn plane_point(i: u32, j: u32) -> [f32; 3] {
    [
        (i as f32 + 0.5 - SIZE as f32 / 2.0) * PIXEL,
        -(j as f32 + 0.5 - SIZE as f32 / 2.0) * PIXEL,
        0.0,
    ]
}

/// The same pixel on a parallel plane at view distance `distance`.
fn point_at(i: u32, j: u32, distance: f32) -> [f32; 3] {
    let p = plane_point(i, j);
    let s = distance / 10.0;
    [p[0] * s, p[1] * s, EYE[2] - distance]
}

struct Harness {
    gbuffer: GBuffer,
    signal: wgpu::Texture,
    signal_view: wgpu::TextureView,
    motion: wgpu::Texture,
    motion_view: wgpu::TextureView,
}

impl Harness {
    fn new(rt: &RtDevice, texels: &[Texel]) -> Self {
        let gbuffer = GBuffer::new(rt, SIZE, SIZE, texels);
        let zeros = vec![0u8; (SIZE * SIZE * 8) as usize];
        let signal = input_texture(
            rt,
            wgpu::TextureFormat::R32Float,
            SIZE,
            SIZE,
            &zeros[..(SIZE * SIZE * 4) as usize],
        );
        let motion = input_texture(rt, wgpu::TextureFormat::Rg32Float, SIZE, SIZE, &zeros);
        Self {
            gbuffer,
            signal_view: view(&signal),
            motion_view: view(&motion),
            signal,
            motion,
        }
    }

    fn frame(
        &self,
        rt: &RtDevice,
        denoiser: &mut Denoiser,
        signal: &[f32],
        motion: Option<&[[f32; 2]]>,
        camera_cut: bool,
    ) {
        upload(rt, &self.signal, bytemuck::cast_slice(signal), 4);
        if let Some(motion) = motion {
            upload(rt, &self.motion, bytemuck::cast_slice(motion), 8);
        }
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        denoiser
            .record(
                &mut encoder,
                &DenoiserInputs {
                    gbuffer: self.gbuffer.views(),
                    signal: &self.signal_view,
                    motion: &self.motion_view,
                    eye: EYE,
                    camera_cut,
                },
            )
            .unwrap();
        submit(rt, encoder);
    }
}

fn output(rt: &RtDevice, denoiser: &Denoiser) -> Vec<f32> {
    read_rgba16f(rt, denoiser.output())
        .iter()
        .map(|t| {
            assert!(t.iter().all(|v| v.is_finite()), "{t:?}");
            t[0]
        })
        .collect()
}

fn bernoulli(target: &[f32], rng: &mut Rng) -> Vec<f32> {
    target
        .iter()
        .map(|&p| if rng.next_f32() < p { 1.0 } else { 0.0 })
        .collect()
}

fn residual_variance(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let mean_error = a.iter().zip(b).map(|(x, y)| f64::from(x - y)).sum::<f64>() / n;
    a.iter()
        .zip(b)
        .map(|(x, y)| (f64::from(x - y) - mean_error).powi(2))
        .sum::<f64>()
        / n
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn noisy_one_sample_signal_converges_to_the_smooth_target() {
    let rt = open();
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| Texel::surface(plane_point(k % SIZE, k / SIZE), [0.0, 0.0, 1.0]))
        .collect();
    let harness = Harness::new(&rt, &texels);
    let tau = std::f32::consts::TAU;
    let target: Vec<f32> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = ((k % SIZE) as f32, (k / SIZE) as f32);
            0.5 + 0.35 * (tau * i / 64.0).sin() * (tau * j / 48.0).cos()
        })
        .collect();
    let frames = 16;
    for (label, settings) in [
        ("temporal + 5 a-trous", DenoiserSettings::default()),
        (
            "temporal only",
            DenoiserSettings {
                atrous_iterations: 0,
                ..Default::default()
            },
        ),
    ] {
        let mut denoiser =
            Denoiser::new(&rt, SIZE, SIZE, DenoiserSignal::Scalar, &settings).unwrap();
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut input_rmse = 0.0;
        let mut input_variance = 0.0;
        let start = std::time::Instant::now();
        for frame in 0..frames {
            let signal = bernoulli(&target, &mut rng);
            harness.frame(&rt, &mut denoiser, &signal, None, false);
            if frame == 0 {
                input_rmse = rmse(&signal, &target);
                input_variance = residual_variance(&signal, &target);
                let first = output(&rt, &denoiser);
                println!(
                    "{label}: 1-spp input RMSE {input_rmse:.4}; after frame 1 (spatial only) RMSE {:.4}",
                    rmse(&first, &target)
                );
            }
        }
        let per_frame = start.elapsed() / frames;
        let out = output(&rt, &denoiser);
        let out_rmse = rmse(&out, &target);
        let out_variance = residual_variance(&out, &target);
        println!(
            "{label}: after {frames} frames RMSE {out_rmse:.4} ({:.1}x lower), residual variance \
             {out_variance:.6} vs {input_variance:.6} ({:.1}x lower); {per_frame:?} per frame \
             incl. upload",
            input_rmse / out_rmse,
            input_variance / out_variance
        );
        if settings.atrous_iterations > 0 {
            assert!(
                input_rmse / out_rmse >= 3.0,
                "RMSE only {:.2}x lower",
                input_rmse / out_rmse
            );
            assert!(input_variance / out_variance >= 9.0);
        } else {
            // A 16-frame running mean alone: about sqrt(16) = 4x.
            assert!(input_rmse / out_rmse >= 3.0);
        }
    }
    assert!(rt.status().is_hardware());
}

/// Columns whose row-mean lies inside the 10%-90% band between `low` and `high`.
fn transition_width(values: &[f32], low: f32, high: f32) -> usize {
    let (a, b) = (low + 0.1 * (high - low), high - 0.1 * (high - low));
    (0..SIZE)
        .filter(|&i| {
            let m = (0..SIZE)
                .map(|j| values[(j * SIZE + i) as usize])
                .sum::<f32>()
                / SIZE as f32;
            m > a && m < b
        })
        .count()
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn depth_discontinuity_keeps_a_sharp_step() {
    let rt = open();
    // Left half 10 m away (value 0.2), right half 20 m away (value 0.8).
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = (k % SIZE, k / SIZE);
            let distance = if i < SIZE / 2 { 10.0 } else { 20.0 };
            Texel::surface(point_at(i, j, distance), [0.0, 0.0, 1.0])
        })
        .collect();
    let harness = Harness::new(&rt, &texels);
    let target: Vec<f32> = (0..SIZE * SIZE)
        .map(|k| if k % SIZE < SIZE / 2 { 0.2 } else { 0.8 })
        .collect();
    let mut widths = Vec::new();
    for (label, settings) in [
        ("default edge stops", DenoiserSettings::default()),
        (
            "control: no depth or luminance stop",
            DenoiserSettings {
                phi_depth: 10.0,
                phi_luminance: 1000.0,
                ..Default::default()
            },
        ),
    ] {
        let mut denoiser =
            Denoiser::new(&rt, SIZE, SIZE, DenoiserSignal::Scalar, &settings).unwrap();
        let mut rng = Rng(0x0bad_cafe_dead_beef);
        for _ in 0..16 {
            harness.frame(
                &rt,
                &mut denoiser,
                &bernoulli(&target, &mut rng),
                None,
                false,
            );
        }
        let out = output(&rt, &denoiser);
        let width = transition_width(&out, 0.2, 0.8);
        let row_mean =
            |i: u32| (0..SIZE).map(|j| out[(j * SIZE + i) as usize]).sum::<f32>() / SIZE as f32;
        println!(
            "{label}: 10-90% transition {width} px; column means around the edge {:.3} {:.3} | {:.3} {:.3}",
            row_mean(SIZE / 2 - 2),
            row_mean(SIZE / 2 - 1),
            row_mean(SIZE / 2),
            row_mean(SIZE / 2 + 1)
        );
        widths.push(width);
    }
    assert!(widths[0] <= 1, "edge blurred over {} px", widths[0]);
    assert!(
        widths[1] >= 3,
        "the control must blur ({} px) or the test is blind",
        widths[1]
    );
    assert!(rt.status().is_hardware());
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn disocclusion_leaves_no_ghost_and_camera_cuts_reset_history() {
    let rt = open();
    // Background 10 m away with value 0; an object 5 m away with value 1 at
    // columns [x0, x0 + 32), rows [32, 96).
    let object = |i: u32, j: u32, x0: u32| (x0..x0 + 32).contains(&i) && (32..96).contains(&j);
    let texels = |x0: u32| -> Vec<Texel> {
        (0..SIZE * SIZE)
            .map(|k| {
                let (i, j) = (k % SIZE, k / SIZE);
                let d = if object(i, j, x0) { 5.0 } else { 10.0 };
                Texel::surface(point_at(i, j, d), [0.0, 0.0, 1.0])
            })
            .collect()
    };
    let signal = |x0: u32| -> Vec<f32> {
        (0..SIZE * SIZE)
            .map(|k| {
                if object(k % SIZE, k / SIZE, x0) {
                    1.0
                } else {
                    0.0
                }
            })
            .collect()
    };
    // The object moves 4 px right per frame: its pixels were 4 px left.
    let motion = |x0: u32| -> Vec<[f32; 2]> {
        (0..SIZE * SIZE)
            .map(|k| {
                if object(k % SIZE, k / SIZE, x0) {
                    [-4.0, 0.0]
                } else {
                    [0.0, 0.0]
                }
            })
            .collect()
    };
    let still = vec![[0.0f32; 2]; (SIZE * SIZE) as usize];
    let mut ghosts = Vec::new();
    for (label, settings) in [
        ("default rejection", DenoiserSettings::default()),
        (
            "control: rejection off",
            DenoiserSettings {
                reprojection_depth_tolerance: 1.0,
                reprojection_normal_threshold: -1.0,
                ..Default::default()
            },
        ),
    ] {
        let harness = Harness::new(&rt, &texels(32));
        let mut denoiser =
            Denoiser::new(&rt, SIZE, SIZE, DenoiserSignal::Scalar, &settings).unwrap();
        for _ in 0..16 {
            harness.frame(&rt, &mut denoiser, &signal(32), Some(&still), false);
        }
        let mut x0 = 32;
        for _ in 0..8 {
            x0 += 4;
            harness.gbuffer.update(&rt, &texels(x0));
            harness.frame(&rt, &mut denoiser, &signal(x0), Some(&motion(x0)), false);
        }
        let out = output(&rt, &denoiser);
        // Uncovered background: the object's first footprint, now background.
        let mut ghost = 0.0f32;
        let mut object_min = 1.0f32;
        for j in 32..96 {
            for i in 32..x0 {
                ghost = ghost.max(out[(j * SIZE + i) as usize]);
            }
            for i in x0..x0 + 32 {
                object_min = object_min.min(out[(j * SIZE + i) as usize]);
            }
        }
        println!(
            "{label}: max ghost in the uncovered region {ghost:.4}; min on the moved object {object_min:.4}"
        );
        if ghosts.is_empty() {
            assert!(
                object_min >= 0.95,
                "moved object lost its value: {object_min}"
            );
        }
        ghosts.push(ghost);
    }
    assert!(ghosts[0] <= 0.05, "ghost {} above 0.05", ghosts[0]);
    assert!(
        ghosts[1] >= 0.5,
        "the control must ghost ({}) or the test is blind",
        ghosts[1]
    );

    // Camera cut: 8 frames of 0.2, then 0.9 with and without a cut.
    let plane: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| Texel::surface(plane_point(k % SIZE, k / SIZE), [0.0, 0.0, 1.0]))
        .collect();
    let harness = Harness::new(&rt, &plane);
    let low = vec![0.2f32; (SIZE * SIZE) as usize];
    let high = vec![0.9f32; (SIZE * SIZE) as usize];
    let mut results = Vec::new();
    for cut in [true, false] {
        let mut denoiser = Denoiser::new(
            &rt,
            SIZE,
            SIZE,
            DenoiserSignal::Scalar,
            &DenoiserSettings::default(),
        )
        .unwrap();
        for _ in 0..8 {
            harness.frame(&rt, &mut denoiser, &low, None, false);
        }
        harness.frame(&rt, &mut denoiser, &high, None, cut);
        let m = mean(&output(&rt, &denoiser));
        println!("after 8 frames of 0.2 then 0.9 (camera cut {cut}): mean {m:.4}");
        results.push(m);
    }
    assert!(
        (results[0] - 0.9).abs() < 2e-3,
        "a camera cut must drop the history"
    );
    assert!(results[1] < 0.35, "without a cut the history is kept");
    // reset() behaves like a camera cut.
    let mut denoiser = Denoiser::new(
        &rt,
        SIZE,
        SIZE,
        DenoiserSignal::Scalar,
        &DenoiserSettings::default(),
    )
    .unwrap();
    for _ in 0..8 {
        harness.frame(&rt, &mut denoiser, &low, None, false);
    }
    denoiser.reset();
    harness.frame(&rt, &mut denoiser, &high, None, false);
    assert!((mean(&output(&rt, &denoiser)) - 0.9).abs() < 2e-3);
    assert!(rt.status().is_hardware());
}
