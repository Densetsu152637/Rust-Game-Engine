//! Self-tests that gate each GPU tier (docs/spatial-audio-design.md section 6.2):
//! a transformed-quad probe with an exact hit count, hit distance and triangle
//! ids, and a 4,096-ray closed shoebox whose mean free path must be within 2% of
//! 4V/S. A tier counts as available only after both pass.

use crate::device::{GpuDevice, Tier};
use crate::error;
use crate::kernel::{self, Kernel, ProbeSlot};
use crate::scene::{FlatScene, leaf_over};
use crate::upload::GpuScene;
use engine_acoustics::gpu_layout::{BvhNode, GpuInstance, GpuMaterial, GpuTriangle};
use engine_acoustics::{BackendError, EnergyJob, TraceSettings};
use std::time::Duration;

/// Measured self-test values (kept for status and test output).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfTestReport {
    pub quad_rays: u32,
    pub quad_hits: u32,
    pub quad_expected_hits: u32,
    pub quad_max_t_error: f32,
    pub shoebox_rays: u32,
    pub shoebox_mean_free_path_m: f32,
    pub shoebox_expected_mean_free_path_m: f32,
    pub shoebox_escaped: f32,
    pub shoebox_aborted: u32,
}

const QUAD_DISTANCE: f32 = 5.0;
const SHOEBOX: [f32; 3] = [10.0, 6.0, 8.0];
const SHOEBOX_RAYS: u32 = 4096;
/// Generous: self-tests run once per initialization, on an idle device.
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn triangle(a: [f32; 3], b: [f32; 3], c: [f32; 3], material: u32) -> GpuTriangle {
    GpuTriangle {
        v0: [a[0], a[1], a[2], f32::from_bits(material)],
        v1: [b[0], b[1], b[2], 0.0],
        v2: [c[0], c[1], c[2], 0.0],
    }
}

/// Builds a one-instance scene with single-leaf BVHs at both levels.
fn single_instance_scene(
    triangles: Vec<GpuTriangle>,
    world_from_object: [f32; 12],
    object_from_world: [f32; 12],
    world_bounds: ([f32; 3], [f32; 3]),
    material: GpuMaterial,
) -> FlatScene {
    let mesh_root = leaf_over(&triangles);
    FlatScene {
        nodes: vec![mesh_root],
        top_nodes: vec![BvhNode {
            min: world_bounds.0,
            left_or_first: 0,
            max: world_bounds.1,
            count: 1,
        }],
        triangles,
        instances: vec![GpuInstance {
            world_from_object,
            object_from_world,
            node_offset: 0,
            triangle_offset: 0,
            mask: 0xff,
            _pad: 0,
        }],
        materials: vec![material],
    }
}

/// Quad x, y in [-1, 1] at object z = 0, rotated 90 degrees about +Z and moved to
/// world z = 5.
fn quad_scene() -> FlatScene {
    let triangles = vec![
        triangle([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], 0),
        triangle([-1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], 0),
    ];
    single_instance_scene(
        triangles,
        [
            0.0,
            -1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            QUAD_DISTANCE,
        ],
        [
            0.0,
            1.0,
            0.0,
            0.0,
            -1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            -QUAD_DISTANCE,
        ],
        ([-1.0, -1.0, QUAD_DISTANCE], [1.0, 1.0, QUAD_DISTANCE]),
        GpuMaterial::default(),
    )
}

/// 8 x 8 parallel rays along +Z. Offsets keep every ray off the quad edges and
/// diagonal, so 4 x 4 = 16 hit and the triangle of each hit is known.
fn quad_rays() -> Vec<(ProbeSlot, Option<u32>)> {
    let mut rays = Vec::with_capacity(64);
    for i in 0..8 {
        for j in 0..8 {
            let x = -1.65 + 0.5 * i as f32;
            let y = -1.6 + 0.5 * j as f32;
            let inside = x.abs() < 1.0 && y.abs() < 1.0;
            // World -> object for a +90 degree rotation about Z: (x, y) -> (y, -x).
            let (ox, oy) = (y, -x);
            let expected = inside.then_some(if oy < ox { 0 } else { 1 });
            rays.push((
                ProbeSlot::new([x, y, 0.0], [0.0, 0.0, 1.0], 100.0),
                expected,
            ));
        }
    }
    rays
}

/// Closed box [0, size], 12 triangles, all with material 0.
pub(crate) fn shoebox_triangles(size: [f32; 3]) -> Vec<GpuTriangle> {
    let [x, y, z] = size;
    let p = |i: u32| {
        [
            if i & 1 != 0 { x } else { 0.0 },
            if i & 2 != 0 { y } else { 0.0 },
            if i & 4 != 0 { z } else { 0.0 },
        ]
    };
    // Faces as corner index quads (two triangles each).
    let faces = [
        [0, 2, 6, 4], // -X
        [1, 5, 7, 3], // +X
        [0, 4, 5, 1], // -Y
        [2, 3, 7, 6], // +Y
        [0, 1, 3, 2], // -Z
        [4, 6, 7, 5], // +Z
    ];
    faces
        .iter()
        .flat_map(|f| {
            [
                triangle(p(f[0]), p(f[1]), p(f[2]), 0),
                triangle(p(f[0]), p(f[2]), p(f[3]), 0),
            ]
        })
        .collect()
}

fn shoebox_scene() -> FlatScene {
    const IDENTITY: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    single_instance_scene(
        shoebox_triangles(SHOEBOX),
        IDENTITY,
        IDENTITY,
        ([0.0; 3], SHOEBOX),
        // Lossless, fully diffuse walls: the free paths sample the room's mean
        // chord 4V/S.
        GpuMaterial {
            reflect: [1.0; 4],
            transmission: [0.0; 4],
            scattering: 1.0,
            _pad: [0.0; 3],
        },
    )
}

fn step_failed(tier: Tier, step: &str, detail: String) -> BackendError {
    error::unavailable(format!(
        "{} self-test step '{step}' failed: {detail}",
        crate::device::tier_name(tier)
    ))
}

fn upload(gpu: &GpuDevice, tier: Tier, scene: &FlatScene) -> Result<GpuScene, BackendError> {
    let (meshes, mesh_of_instance) = scene.validate()?;
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let uploaded = GpuScene::upload(gpu, tier, scene, &meshes, &mesh_of_instance)?;
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(error::failed(format!(
            "validation error uploading the self-test scene: {e}"
        )));
    }
    Ok(uploaded)
}

/// Runs both self-test steps. `force_failure` is the `GpuOptions` test hook.
pub(crate) fn run(
    gpu: &GpuDevice,
    kernel: &Kernel,
    tier: Tier,
    force_failure: bool,
) -> Result<SelfTestReport, BackendError> {
    let mut report = SelfTestReport::default();
    if force_failure {
        return Err(step_failed(
            tier,
            "quad hit count",
            "forced by GpuOptions::force_self_test_failure (test hook)".into(),
        ));
    }

    // Step 1: exact hits on a transformed quad.
    let quad = upload(gpu, tier, &quad_scene())
        .map_err(|e| step_failed(tier, "quad upload", e.message))?;
    let rays = quad_rays();
    let slots: Vec<ProbeSlot> = rays.iter().map(|(slot, _)| *slot).collect();
    let results = kernel::cast_rays(gpu, kernel, &quad, &slots, SELF_TEST_TIMEOUT)
        .map_err(|e| step_failed(tier, "quad dispatch", e.message))?;
    report.quad_rays = rays.len() as u32;
    report.quad_expected_hits = rays.iter().filter(|(_, e)| e.is_some()).count() as u32;
    for (result, (_, expected)) in results.iter().zip(&rays) {
        if result.aborted() {
            return Err(step_failed(
                tier,
                "quad hit count",
                "a probe ray aborted".into(),
            ));
        }
        match (result.hit(), expected) {
            (Some((t, triangle, instance)), Some(expected_triangle)) => {
                report.quad_hits += 1;
                report.quad_max_t_error = report.quad_max_t_error.max((t - QUAD_DISTANCE).abs());
                if triangle != *expected_triangle || instance != 0 {
                    return Err(step_failed(
                        tier,
                        "quad triangle ids",
                        format!(
                            "hit triangle {triangle} instance {instance}, expected triangle {expected_triangle} instance 0"
                        ),
                    ));
                }
            }
            (None, None) => {}
            (Some(_), None) => report.quad_hits += 1,
            (None, Some(_)) => {}
        }
    }
    if report.quad_hits != report.quad_expected_hits {
        return Err(step_failed(
            tier,
            "quad hit count",
            format!(
                "{} hits, expected {}",
                report.quad_hits, report.quad_expected_hits
            ),
        ));
    }
    if report.quad_max_t_error.is_nan() || report.quad_max_t_error > 1e-4 {
        return Err(step_failed(
            tier,
            "quad hit distance",
            format!("max |t - 5| = {}", report.quad_max_t_error),
        ));
    }

    // Step 2: mean free path of a closed shoebox through the energy kernel.
    let shoebox = upload(gpu, tier, &shoebox_scene())
        .map_err(|e| step_failed(tier, "shoebox upload", e.message))?;
    let settings = TraceSettings {
        bin_s: 0.001,
        bins: 1000,
        speed_of_sound: 343.0,
        air_np_per_m: [0.0; 4],
    };
    let job = EnergyJob {
        emitter: [3.1, 2.2, 4.3],
        receiver: [6.7, 3.9, 2.6],
        receiver_radius_m: 1.0,
        rays: SHOEBOX_RAYS,
        max_bounces: 64,
        seed: 0x5eed_0001,
    };
    let trace = kernel::trace_blocking(gpu, kernel, &shoebox, &settings, &[job], SELF_TEST_TIMEOUT)
        .map_err(|e| step_failed(tier, "shoebox dispatch", e.message))?;
    let histogram = &trace.histograms[0];
    let [x, y, z] = SHOEBOX;
    let expected = 4.0 * x * y * z / (2.0 * (x * y + y * z + x * z));
    report.shoebox_rays = SHOEBOX_RAYS;
    report.shoebox_mean_free_path_m = histogram.mean_free_path_m;
    report.shoebox_expected_mean_free_path_m = expected;
    report.shoebox_escaped = histogram.escaped;
    report.shoebox_aborted = histogram.aborted;
    if histogram.aborted != 0 {
        return Err(step_failed(
            tier,
            "shoebox traversal",
            format!("{} rays aborted", histogram.aborted),
        ));
    }
    if histogram.escaped > 1e-3 {
        return Err(step_failed(
            tier,
            "shoebox closure",
            format!(
                "{:.4} of the energy escaped a closed room",
                histogram.escaped
            ),
        ));
    }
    let deposited: f32 = histogram.bins.iter().flatten().sum();
    if !(deposited.is_finite() && deposited > 0.0) {
        return Err(step_failed(
            tier,
            "shoebox receiver",
            format!("deposited energy {deposited}"),
        ));
    }
    let error = (histogram.mean_free_path_m - expected).abs() / expected;
    if error.is_nan() || error > 0.02 {
        return Err(step_failed(
            tier,
            "shoebox mean free path",
            format!(
                "{:.4} m against 4V/S = {expected:.4} m ({:.2}% > 2%)",
                histogram.mean_free_path_m,
                error * 100.0
            ),
        ));
    }
    Ok(report)
}
