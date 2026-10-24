//! Reference path tracer: white-furnace style convergence, cosine sampling,
//! bounce accounting and no non-finite output. Ignored by default; run with
//! `cargo test -p engine-render-rt --release --test path_tracer -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{PathTracer, PathTracerMaterial, PathTracerParams, RtDevice, RtScene};
use glam::Vec3;

/// Relative tolerance of every converged mean.
const TOLERANCE: f32 = 0.02;

fn camera(eye: Vec3, target: Vec3, up: Vec3, fov_deg: f32, aspect: f32) -> [[f32; 4]; 4] {
    let view = glam::camera::rh::view::look_at_mat4(eye, target, up);
    let proj =
        glam::camera::rh::proj::directx::perspective(fov_deg.to_radians(), aspect, 0.05, 100.0);
    (proj * view).inverse().to_cols_array_2d()
}

/// Runs `frames` frames and returns the accumulated RGBA texels.
fn run(
    rt: &RtDevice,
    scene: &RtScene,
    size: u32,
    materials: &[PathTracerMaterial],
    params: PathTracerParams,
    frames: u32,
) -> Vec<[f32; 4]> {
    let mut tracer = PathTracer::new(rt, size, size, materials).unwrap();
    let start = std::time::Instant::now();
    for frame in 0..frames {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        tracer
            .record_frame(
                &mut encoder,
                scene,
                &PathTracerParams { seed: 17, ..params },
            )
            .unwrap();
        // Submit in batches to keep each submission short.
        if frame % 16 == 15 || frame + 1 == frames {
            submit(rt, encoder);
        } else {
            rt.queue().submit(Some(encoder.finish()));
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(tracer.frame_count(), frames);
    let texels: Vec<[f32; 4]> =
        bytemuck::cast_slice(&read_texture(rt, tracer.output(), 16)).to_vec();
    let non_finite = tracer.read_non_finite_samples().unwrap();
    println!(
        "  {frames} frames of {size}x{size}: {elapsed:?} ({:.2} ms/frame), non-finite samples {non_finite}",
        elapsed.as_secs_f64() * 1e3 / f64::from(frames)
    );
    assert_eq!(non_finite, 0, "non-finite samples were produced");
    for t in &texels {
        assert!(t.iter().all(|v| v.is_finite() && *v >= 0.0), "texel {t:?}");
        assert_eq!(t[3], frames as f32, "sample count channel");
    }
    texels
}

fn mean(texels: &[[f32; 4]]) -> [f32; 3] {
    let n = texels.len() as f64;
    let sum = texels.iter().fold([0f64; 3], |s, t| {
        [
            s[0] + f64::from(t[0]),
            s[1] + f64::from(t[1]),
            s[2] + f64::from(t[2]),
        ]
    });
    [
        (sum[0] / n) as f32,
        (sum[1] / n) as f32,
        (sum[2] / n) as f32,
    ]
}

fn assert_close(label: &str, measured: [f32; 3], expected: [f32; 3]) {
    for c in 0..3 {
        let error = (measured[c] - expected[c]).abs() / expected[c];
        println!(
            "  {label} channel {c}: measured {:.5}, expected {:.5}, error {:.3}%",
            measured[c],
            expected[c],
            error * 100.0
        );
        assert!(error <= TOLERANCE, "{label} channel {c}: {error}");
    }
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn convex_diffuse_object_under_uniform_sky_converges_to_albedo_times_sky() {
    // White furnace: a convex Lambertian object of albedo a under uniform sky
    // radiance L reflects exactly a * L in every direction.
    let rt = open();
    let mut scene = scene(&rt);
    let sphere = icosphere(3);
    let id = scene.add_mesh(&sphere.data()).unwrap();
    scene
        .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();
    let albedo = [0.25, 0.5, 0.75];
    let sky = 2.0;
    let size = 96;
    let fov: f32 = 60.0;
    let eye = Vec3::new(0.0, 0.0, 3.0);
    let params = PathTracerParams {
        inv_view_proj: camera(eye, Vec3::ZERO, Vec3::Y, fov, 1.0),
        eye: eye.to_array(),
        sky_zenith: [sky; 3],
        sky_horizon: [sky; 3],
        max_bounces: 8,
        rr_start: 3,
        ..Default::default()
    };
    println!("convex furnace:");
    let texels = run(
        &rt,
        &scene,
        size,
        &[PathTracerMaterial {
            albedo,
            emissive: [0.0; 3],
        }],
        params,
        64,
    );
    // Classify pixels by the angle of their centre ray to the view axis; the
    // sphere subtends asin(1/3) from the eye.
    let radius = (1.0f32 / 3.0).asin();
    let tan_half = (fov.to_radians() / 2.0).tan();
    let angle = |i: u32, j: u32| {
        let x = ((i as f32 + 0.5) / size as f32 * 2.0 - 1.0) * tan_half;
        let y = ((j as f32 + 0.5) / size as f32 * 2.0 - 1.0) * tan_half;
        (x * x + y * y).sqrt().atan()
    };
    let (mut object, mut background) = (Vec::new(), Vec::new());
    for j in 0..size {
        for i in 0..size {
            let t = texels[(j * size + i) as usize];
            let a = angle(i, j);
            if a < 0.8 * radius {
                object.push(t);
            } else if a > 1.2 * radius {
                background.push(t);
            }
        }
    }
    println!(
        "  {} object pixels, {} sky pixels",
        object.len(),
        background.len()
    );
    let expected = [albedo[0] * sky, albedo[1] * sky, albedo[2] * sky];
    assert_close("object mean", mean(&object), expected);
    let worst = object
        .iter()
        .map(|t| {
            (0..3)
                .map(|c| (t[c] - expected[c]).abs() / expected[c])
                .fold(0.0, f32::max)
        })
        .fold(0.0, f32::max);
    println!("  worst object pixel relative error {:.4}%", worst * 100.0);
    assert!(worst <= TOLERANCE);
    assert_close("sky mean", mean(&background), [sky; 3]);
}

/// Inside a closed box of albedo a and emission E, radiance is E / (1 - a) for
/// unlimited bounces; this exercises multi-bounce transport and Russian roulette.
#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn closed_furnace_converges_to_emission_over_one_minus_albedo() {
    let rt = open();
    let mut scene = scene(&rt);
    let room = cuboid([-1.0; 3], [1.0; 3]);
    let id = scene.add_mesh(&room.data()).unwrap();
    scene
        .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();
    let material = PathTracerMaterial {
        albedo: [0.5, 0.5, 0.8],
        emissive: [0.5; 3],
    };
    let eye = Vec3::new(0.1, 0.2, 0.3);
    let base = PathTracerParams {
        inv_view_proj: camera(eye, Vec3::new(0.0, 0.0, -1.0), Vec3::Y, 90.0, 1.0),
        eye: eye.to_array(),
        sky_zenith: [0.0; 3],
        sky_horizon: [0.0; 3],
        ..Default::default()
    };

    // Deterministic bounce accounting without roulette: sum of E a^k, k <= n.
    println!("closed furnace, 2 bounces, no roulette:");
    let truncated = run(
        &rt,
        &scene,
        64,
        &[material],
        PathTracerParams {
            max_bounces: 2,
            rr_start: 1000,
            ..base
        },
        4,
    );
    let geometric = |a: f32, n: i32| 0.5 * (1.0 - a.powi(n + 1)) / (1.0 - a);
    assert_close(
        "2-bounce mean",
        mean(&truncated),
        [geometric(0.5, 2), geometric(0.5, 2), geometric(0.8, 2)],
    );

    println!("closed furnace, 256 bounces, roulette from bounce 2:");
    let converged = run(
        &rt,
        &scene,
        64,
        &[material],
        PathTracerParams {
            max_bounces: 256,
            rr_start: 2,
            ..base
        },
        256,
    );
    // E / (1 - a): 1.0 for a = 0.5 and 2.5 for a = 0.8 (truncation at 256
    // bounces loses 0.8^257, negligible).
    assert_close("furnace mean", mean(&converged), [1.0, 1.0, 2.5]);
}

/// An upward-facing plane under a sky L(w) = H + (Z - H) max(w.y, 0) reflects
/// a (H + (Z - H) 2/3) with cosine-weighted sampling (uniform hemisphere
/// sampling without the cosine weight would give a (H + (Z - H) / 2)).
#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn plane_under_gradient_sky_checks_cosine_sampling() {
    let rt = open();
    let mut scene = scene(&rt);
    let plane = ground(50.0, 0.0);
    let id = scene.add_mesh(&plane.data()).unwrap();
    scene
        .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();
    let (zenith, horizon, albedo) = (2.0f32, 0.5f32, 0.6f32);
    let eye = Vec3::new(0.0, 5.0, 0.0);
    println!("gradient sky:");
    let texels = run(
        &rt,
        &scene,
        64,
        &[PathTracerMaterial {
            albedo: [albedo; 3],
            emissive: [0.0; 3],
        }],
        PathTracerParams {
            inv_view_proj: camera(eye, Vec3::ZERO, Vec3::Z, 40.0, 1.0),
            eye: eye.to_array(),
            sky_zenith: [zenith; 3],
            sky_horizon: [horizon; 3],
            max_bounces: 4,
            rr_start: 1000,
            ..Default::default()
        },
        256,
    );
    let expected = albedo * (horizon + (zenith - horizon) * 2.0 / 3.0);
    let uniform = albedo * (horizon + (zenith - horizon) * 0.5);
    let measured = mean(&texels);
    println!("  uniform-hemisphere (wrong) answer would be {uniform:.4}");
    assert_close("plane mean", measured, [expected; 3]);
}
