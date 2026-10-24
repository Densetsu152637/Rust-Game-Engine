//! GPU versus CPU propagation on real hardware (docs/spatial-audio-design.md tests
//! 16-18). Ignored by default; run with
//! `cargo test -p engine-acoustics-gpu --release --test parity -- --ignored --nocapture --test-threads=1`.

mod common;

use common::*;
use engine_acoustics::{
    AcousticWorld, BANDS, BackendKind, CpuBackend, EnergyHistogram, EnergyJob, PropagationBackend,
    TraceSettings, TraceStats, fit_reverb,
};
use engine_acoustics_gpu::{GpuBackend, GpuOptions};

const RAYS: u32 = 65_536;
const BOUNCES: u16 = 256;
const LENGTH_S: f32 = 3.0;
const SOURCE: [f32; 3] = [3.1, 2.2, 4.3];
const LISTENER: [f32; 3] = [6.7, 3.9, 2.6];
/// T30 band agreement (test 17).
const T30_TOLERANCE: f32 = 0.05;
const WINDOW_TOLERANCE_DB: f32 = 0.5;
const MFP_TOLERANCE: f32 = 0.01;

fn options() -> GpuOptions {
    GpuOptions {
        // Long enough for the 100k-triangle throughput run on a shared GPU.
        readback_timeout_ms: 10_000,
        ..Default::default()
    }
}

fn initialized(mut backend: Box<dyn PropagationBackend>) -> Box<dyn PropagationBackend> {
    backend.initialize().expect("tier initializes");
    backend
}

fn cpu() -> Box<dyn PropagationBackend> {
    initialized(Box::new(CpuBackend::new(4).unwrap()))
}

fn gpu(kind: BackendKind) -> Box<dyn PropagationBackend> {
    match kind {
        BackendKind::GpuRayQuery => initialized(Box::new(GpuBackend::ray_query(options()))),
        BackendKind::GpuCompute => initialized(Box::new(GpuBackend::compute(options()))),
        BackendKind::Cpu => cpu(),
    }
}

fn trace(
    backend: &mut dyn PropagationBackend,
    world: &AcousticWorld,
    settings: &TraceSettings,
    jobs: &[EnergyJob],
) -> (Vec<EnergyHistogram>, TraceStats) {
    backend.sync_world(world).expect("sync");
    let mut out = Vec::new();
    let stats = backend
        .trace(world, settings, jobs, &mut out)
        .expect("trace");
    assert_eq!(out.len(), jobs.len());
    (out, stats)
}

fn segments_per_second(stats: &TraceStats) -> f64 {
    stats.segments as f64 / (f64::from(stats.elapsed_us.max(1)) * 1e-6)
}

/// Compares two histograms with the test-17 tolerances, printing the values.
fn assert_agree(
    label: &str,
    test: &EnergyHistogram,
    reference: &EnergyHistogram,
    t30_tolerance: f32,
) {
    let rt_test = fit_reverb(test, 0.08).expect("fit").rt60_s;
    let rt_reference = fit_reverb(reference, 0.08).expect("fit").rt60_s;
    let early = db_ratio(window(test, 0.0, 0.05), window(reference, 0.0, 0.05));
    let late = db_ratio(
        window(test, 0.05, LENGTH_S),
        window(reference, 0.05, LENGTH_S),
    );
    let mfp = relative(test.mean_free_path_m, reference.mean_free_path_m);
    println!(
        "{label}: T30 {rt_test:.4?} vs {rt_reference:.4?} s; early dB {early:.3?}; late dB {late:.3?}; \
         MFP {:.4} vs {:.4} m ({:.3}%); escaped {:.2e} vs {:.2e}; aborted {} / {}",
        test.mean_free_path_m,
        reference.mean_free_path_m,
        mfp * 100.0,
        test.escaped,
        reference.escaped,
        test.aborted,
        reference.aborted
    );
    for b in 0..BANDS {
        let r = relative(rt_test[b], rt_reference[b]);
        assert!(
            r <= t30_tolerance,
            "{label}: band {b} T30 differs by {:.2}%",
            r * 100.0
        );
        assert!(
            early[b].abs() <= WINDOW_TOLERANCE_DB,
            "{label}: band {b} early {} dB",
            early[b]
        );
        assert!(
            late[b].abs() <= WINDOW_TOLERANCE_DB,
            "{label}: band {b} late {} dB",
            late[b]
        );
    }
    assert!(
        mfp <= MFP_TOLERANCE,
        "{label}: mean free path differs by {:.3}%",
        mfp * 100.0
    );
    assert_eq!(test.aborted, 0, "{label}: aborted rays");
}

fn shoebox_against_cpu(kind: BackendKind) -> Vec<EnergyHistogram> {
    let world = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
    let settings = settings(LENGTH_S);
    let jobs = [job(SOURCE, LISTENER, RAYS, BOUNCES, 1)];
    let mut cpu = cpu();
    let (reference, cpu_stats) = trace(cpu.as_mut(), &world, &settings, &jobs);
    let mut backend = gpu(kind);
    println!("{kind:?}: {:?}", backend.info());
    let (result, stats) = trace(backend.as_mut(), &world, &settings, &jobs);
    println!(
        "shoebox {kind:?}: {} segments in {} us ({:.3e} seg/s); CPU {} segments in {} us",
        stats.segments,
        stats.elapsed_us,
        segments_per_second(&stats),
        cpu_stats.segments,
        cpu_stats.elapsed_us
    );
    assert_agree(
        &format!("shoebox {kind:?} vs CPU"),
        &result[0],
        &reference[0],
        T30_TOLERANCE,
    );

    // Determinism (test 16): the same seed gives a bitwise-identical histogram.
    let (again, _) = trace(backend.as_mut(), &world, &settings, &jobs);
    assert_eq!(again[0], result[0], "{kind:?} is not deterministic");
    result
}

fn l_room_against_cpu(kind: BackendKind) {
    let (mut world, panel) = l_room_world();
    let settings = settings(LENGTH_S);
    let jobs = [job([8.0, 1.5, 2.0], [2.0, 1.5, 8.0], RAYS, BOUNCES, 7)];
    let mut cpu = cpu();
    let mut backend = gpu(kind);
    for (step, x) in [5.0f32, 7.0].into_iter().enumerate() {
        if step > 0 {
            // Moving the dynamic occluder keeps the revision (TLAS update / refit).
            let revision = world.revision();
            world
                .set_instance_transform(panel, translation(x, 0.0, 0.5))
                .expect("move panel");
            assert_eq!(world.revision(), revision);
        }
        let (reference, _) = trace(cpu.as_mut(), &world, &settings, &jobs);
        let (result, _) = trace(backend.as_mut(), &world, &settings, &jobs);
        assert_agree(
            &format!("L-room {kind:?} panel x={x}"),
            &result[0],
            &reference[0],
            T30_TOLERANCE,
        );
    }
}

fn throughput(kind: BackendKind) {
    // The same shoebox as 99,372 triangles (each face 91 x 91 cells).
    let world = shoebox_world(REFERENCE_ALPHA, 1.0, 91);
    println!("dense shoebox: {:?}", world.stats());
    let settings = settings(LENGTH_S);
    let jobs = [job(SOURCE, LISTENER, 1_048_576, 64, 3)];
    let mut backend = gpu(kind);
    let (out, stats) = trace(backend.as_mut(), &world, &settings, &jobs);
    println!(
        "throughput {kind:?} 100k triangles: {} segments in {} us = {:.3e} segments/s, aborted {}",
        stats.segments,
        stats.elapsed_us,
        segments_per_second(&stats),
        out[0].aborted
    );
    assert_eq!(out[0].aborted, 0);
    let small = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
    let (_, stats) = trace(backend.as_mut(), &small, &settings, &jobs);
    println!(
        "throughput {kind:?} 12 triangles: {} segments in {} us = {:.3e} segments/s",
        stats.segments,
        stats.elapsed_us,
        segments_per_second(&stats)
    );
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn test17_ray_query_matches_cpu() {
    shoebox_against_cpu(BackendKind::GpuRayQuery);
    l_room_against_cpu(BackendKind::GpuRayQuery);
    throughput(BackendKind::GpuRayQuery);
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn test18_compute_matches_cpu_and_ray_query() {
    let compute = shoebox_against_cpu(BackendKind::GpuCompute);
    l_room_against_cpu(BackendKind::GpuCompute);
    throughput(BackendKind::GpuCompute);

    // Tier 2 against tier 1 with the same seeds: within 1% per window.
    let world = shoebox_world(REFERENCE_ALPHA, 1.0, 1);
    let mut ray_query = gpu(BackendKind::GpuRayQuery);
    let (rq, _) = trace(
        ray_query.as_mut(),
        &world,
        &settings(LENGTH_S),
        &[job(SOURCE, LISTENER, RAYS, BOUNCES, 1)],
    );
    let windows = [
        (0.0, 0.05),
        (0.05, 0.2),
        (0.2, 0.5),
        (0.5, 1.0),
        (1.0, LENGTH_S),
    ];
    for (start, end) in windows {
        let a = window(&compute[0], start, end);
        let b = window(&rq[0], start, end);
        let worst = (0..BANDS)
            .filter(|&i| b[i] > 0.0)
            .map(|i| relative(a[i], b[i]))
            .fold(0.0f32, f32::max);
        println!(
            "compute vs ray query window {start}-{end} s: worst band {:.4}%",
            worst * 100.0
        );
        assert!(
            worst <= 0.01,
            "window {start}-{end} s differs by {:.3}%",
            worst * 100.0
        );
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn readback_timeout_is_reported_and_not_fatal() {
    let world = shoebox_world(REFERENCE_ALPHA, 1.0, 91);
    let mut backend = GpuBackend::compute(GpuOptions {
        readback_timeout_ms: 1,
        ..Default::default()
    });
    backend.initialize().expect("compute tier");
    backend.sync_world(&world).expect("sync");
    let mut out = Vec::new();
    let heavy = [job(SOURCE, LISTENER, 1_048_576, 256, 5)];
    let e = backend
        .trace(&world, &settings(LENGTH_S), &heavy, &mut out)
        .unwrap_err();
    println!("{e}");
    assert_eq!(e.code, "acoustics.backend.timeout");
    assert!(!e.fatal);
    // The tier stays usable after a timeout.
    assert!(backend.info().adapter.is_some());
    let light = [job(SOURCE, LISTENER, 64, 4, 5)];
    // Once the queued heavy work drains, a light trace completes within 1 ms.
    let mut ok = false;
    let mut timeouts = 0;
    for _ in 0..100 {
        match backend.trace(&world, &settings(LENGTH_S), &light, &mut out) {
            Ok(_) => {
                ok = true;
                break;
            }
            Err(e) => {
                assert_eq!(e.code, "acoustics.backend.timeout", "{e}");
                timeouts += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    println!("recovered after {timeouts} further timeouts");
    assert!(ok, "the tier never recovered after a timeout");
}
