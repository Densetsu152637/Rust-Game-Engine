//! RT ambient occlusion: an open plane is unoccluded, a crease and a corner
//! match a CPU integration of the cosine-weighted visibility, and AO falls
//! monotonically with the radius. Ignored by default; run with
//! `cargo test -p engine-render-rt --test ambient_occlusion -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_render_rt::{AmbientOcclusionKernel, AoInputs, RtAoSettings, RtDevice, RtScene};

const SIZE: u32 = 128;

/// Runs `frames` AO dispatches of `rays` rays and returns the per-pixel mean.
fn ao(
    rt: &RtDevice,
    scene: &RtScene,
    gbuffer: &GBuffer,
    radius: f32,
    rays: u32,
    frames: u32,
) -> Vec<f32> {
    let format = scalar_format(rt);
    let kernel = AmbientOcclusionKernel::new(rt, format).unwrap();
    let output = output_texture(rt, format.texture_format(), SIZE, SIZE);
    let output_view = view(&output);
    let settings = RtAoSettings {
        radius,
        rays_per_pixel: rays,
        ..Default::default()
    };
    let mut sum = vec![0f32; (SIZE * SIZE) as usize];
    let start = std::time::Instant::now();
    for frame in 0..frames {
        let mut encoder = rt.device().create_command_encoder(&Default::default());
        kernel
            .record(
                &mut encoder,
                scene,
                &AoInputs {
                    gbuffer: gbuffer.views(),
                    output: &output_view,
                    frame_index: frame,
                },
                &settings,
            )
            .unwrap();
        submit(rt, encoder);
        for (s, v) in sum.iter_mut().zip(read_scalar(rt, &output)) {
            assert!((0.0..=1.0).contains(&v), "AO {v}");
            *s += v;
        }
    }
    let per_frame = start.elapsed() / frames;
    println!(
        "  {format:?} {SIZE}x{SIZE}, radius {radius}, {rays} rays: {per_frame:?} per frame incl. readback"
    );
    sum.iter().map(|s| s / frames as f32).collect()
}

/// Expected AO of a floor point `d` from the wall x = 0 (height `h`, and a
/// second wall z = 0 when `corner`), by midpoint integration of the cosine
/// hemisphere (1000 x 1000 cells of (u1, u2), exact cosine weighting).
fn expected(d: f32, z: f32, radius: f32, height: f32, corner: bool) -> f32 {
    let n = 1000;
    let mut occluded = 0u64;
    for i in 0..n {
        for j in 0..n {
            let u1 = (i as f32 + 0.5) / n as f32;
            let u2 = (j as f32 + 0.5) / n as f32;
            let r = u2.sqrt();
            let phi = std::f32::consts::TAU * u1;
            let dir = [r * phi.cos(), (1.0 - u2).max(0.0).sqrt(), r * phi.sin()];
            let mut hit = false;
            if dir[0] < 0.0 {
                let t = d / -dir[0];
                hit |= t <= radius && t * dir[1] <= height;
            }
            if corner && dir[2] < 0.0 {
                let t = z / -dir[2];
                hit |= t <= radius && t * dir[1] <= height && d + t * dir[0] >= 0.0;
            }
            occluded += u64::from(hit);
        }
    }
    1.0 - occluded as f32 / (n * n) as f32
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn open_plane_is_unoccluded() {
    let rt = open();
    let mut scene = scene(&rt);
    let plane = ground(64.0, 0.0);
    let id = scene.add_mesh(&plane.data()).unwrap();
    scene
        .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
        .unwrap();
    scene.commit().unwrap();
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = (k % SIZE, k / SIZE);
            if j == 0 {
                return Texel::BACKGROUND;
            }
            let x = -8.0 + 16.0 * (i as f32 + 0.5) / SIZE as f32;
            let z = -8.0 + 16.0 * (j as f32 + 0.5) / SIZE as f32;
            Texel::surface([x, 0.0, z], [0.0, 1.0, 0.0])
        })
        .collect();
    let gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    let values = ao(&rt, &scene, &gbuffer, 4.0, 64, 4);
    let min = values.iter().copied().fold(f32::INFINITY, f32::min);
    println!("open plane: min AO {min}, mean {:.6}", mean(&values));
    // Tolerance: exactly 1 (no ray can hit anything); allow one f16 ulp.
    assert!(min >= 0.999, "open plane AO {min}");
    assert!(rt.status().is_hardware());
}

/// Floor y = 0 with a wall x = 0 (height 10), and optionally a second wall
/// z = 0. Column `i` of the G-buffer is at distance `d(i)` from the wall.
fn crease_scene(rt: &RtDevice, corner: bool) -> RtScene {
    let mut scene = scene(rt);
    let floor = quad([
        [0.0, 0.0, -32.0],
        [32.0, 0.0, -32.0],
        [32.0, 0.0, 32.0],
        [0.0, 0.0, 32.0],
    ]);
    let wall = quad([
        [0.0, 0.0, -32.0],
        [0.0, 10.0, -32.0],
        [0.0, 10.0, 32.0],
        [0.0, 0.0, 32.0],
    ]);
    let mut meshes = vec![floor, wall];
    if corner {
        meshes.push(quad([
            [0.0, 0.0, 0.0],
            [32.0, 0.0, 0.0],
            [32.0, 10.0, 0.0],
            [0.0, 10.0, 0.0],
        ]));
    }
    for mesh in &meshes {
        let id = scene.add_mesh(&mesh.data()).unwrap();
        scene
            .add_instance(&instance(id, translation(0.0, 0.0, 0.0)))
            .unwrap();
    }
    scene.commit().unwrap();
    scene
}

fn distance_of_column(i: u32) -> f32 {
    0.05 + 2.0 * i as f32 / SIZE as f32
}

/// Per column: mean AO over its rows.
fn column_means(values: &[f32]) -> Vec<f32> {
    (0..SIZE)
        .map(|i| {
            (0..SIZE)
                .map(|j| values[(j * SIZE + i) as usize])
                .sum::<f32>()
                / SIZE as f32
        })
        .collect()
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn crease_and_corner_match_the_cpu_integral_and_fall_with_radius() {
    let rt = open();
    // Crease: rows spread along the wall (z in [-8, 8]), away from its ends.
    let crease = crease_scene(&rt, false);
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let (i, j) = (k % SIZE, k / SIZE);
            let z = -8.0 + 16.0 * (j as f32 + 0.5) / SIZE as f32;
            Texel::surface([distance_of_column(i), 0.0, z], [0.0, 1.0, 0.0])
        })
        .collect();
    let gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    let radii = [0.25f32, 0.5, 1.0, 2.0, 8.0];
    let mut previous: Option<Vec<f32>> = None;
    let mut previous_mean = f64::INFINITY;
    for &radius in &radii {
        // Same frames and rays for every radius: each ray's hit distance is
        // fixed, so per-pixel AO can only fall as the radius grows.
        let values = ao(&rt, &crease, &gbuffer, radius, 64, 8);
        let columns = column_means(&values);
        let mut max_error = 0.0f32;
        for i in (0..SIZE).step_by(16) {
            let d = distance_of_column(i);
            let e = expected(d, 0.0, radius, 10.0, false);
            max_error = max_error.max((columns[i as usize] - e).abs());
        }
        let near = columns[0];
        let far = columns[(SIZE - 1) as usize];
        let m = mean(&values);
        println!(
            "crease radius {radius}: AO at d = {:.3}: {near:.4} (CPU {:.4}), at d = {:.3}: {far:.4} \
             (CPU {:.4}); mean {m:.4}; max |GPU - CPU| over 8 distances {max_error:.4}",
            distance_of_column(0),
            expected(distance_of_column(0), 0.0, radius, 10.0, false),
            distance_of_column(SIZE - 1),
            expected(distance_of_column(SIZE - 1), 0.0, radius, 10.0, false),
        );
        // 64 rays x 8 frames x 128 rows per column: standard error < 0.002.
        assert!(max_error <= 0.01, "radius {radius}: error {max_error}");
        assert!(near < 0.75, "the crease must be occluded: {near}");
        if let Some(previous) = &previous {
            let rising = values
                .iter()
                .zip(previous)
                .filter(|(now, before)| **now > **before + 1e-3)
                .count();
            assert_eq!(rising, 0, "AO rose with the radius at {rising} pixels");
            assert!(
                m < previous_mean - 0.005,
                "mean AO must fall: {m} vs {previous_mean}"
            );
        }
        previous = Some(values);
        previous_mean = m;
    }
    // Infinite-radius limit at the wall: half the cosine-weighted hemisphere.
    let last = column_means(previous.as_ref().unwrap());
    println!(
        "crease radius 8 at d = 0.05: {:.4} (tall-wall limit 0.5)",
        last[0]
    );

    // Corner: a second wall z = 0; texels at (d, 0, d).
    let corner = crease_scene(&rt, true);
    let texels: Vec<Texel> = (0..SIZE * SIZE)
        .map(|k| {
            let i = k % SIZE;
            let d = distance_of_column(i);
            Texel::surface([d, 0.0, d], [0.0, 1.0, 0.0])
        })
        .collect();
    let corner_gbuffer = GBuffer::new(&rt, SIZE, SIZE, &texels);
    let corner_values = column_means(&ao(&rt, &corner, &corner_gbuffer, 1.0, 64, 8));
    let crease_values = column_means(&ao(&rt, &crease, &gbuffer, 1.0, 64, 8));
    // Within the radius (1 m) of both walls.
    for i in [0u32, 16, 32] {
        let d = distance_of_column(i);
        let e = expected(d, d, 1.0, 10.0, true);
        let (c, s) = (corner_values[i as usize], crease_values[i as usize]);
        println!("corner d = {d:.3}: AO {c:.4} (CPU {e:.4}); crease {s:.4}");
        assert!((c - e).abs() <= 0.01, "corner error at {d}: {c} vs {e}");
        assert!(
            c < s - 0.02,
            "the corner must be darker than the crease at {d}"
        );
    }
    assert!(rt.status().is_hardware());
}
