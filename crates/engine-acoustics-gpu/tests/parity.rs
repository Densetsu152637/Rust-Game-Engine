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
/// Agreement required here. The design's test-17 tolerances (5% T30, 0.5 dB
/// windows, 1% mean free path) are far looser than the measured agreement
/// (about 0.1% and 0.001 dB), because CPU and GPU follow the same random paths
/// until rounding differences make them diverge. These keep a margin for that.
const T30_TOLERANCE: f32 = 0.005;
const WINDOW_TOLERANCE_DB: f32 = 0.05;
const MFP_TOLERANCE: f32 = 0.002;
/// ISO 9613-1-like air absorption (amplitude nepers per metre) at 20 C, 50%.
const AIR_NP_PER_M: [f32; BANDS] = [0.0002, 0.0007, 0.0025, 0.012];

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

/// Nonzero air absorption and a 0.25 m receiver: exercises the GPU's air terms
/// on segments and deposits, and the chord weighting of a small sphere.
fn air_and_small_receiver_against_cpu(kind: BackendKind) {
    let world = shoebox_world(REFERENCE_ALPHA, 0.7, 1);
    let mut settings = settings(LENGTH_S);
    settings.air_np_per_m = AIR_NP_PER_M;
    let mut small = job(SOURCE, LISTENER, RAYS, BOUNCES, 11);
    small.receiver_radius_m = 0.25;
    let jobs = [small];
    let mut cpu = cpu();
    let (reference, _) = trace(cpu.as_mut(), &world, &settings, &jobs);
    let mut backend = gpu(kind);
    let (result, _) = trace(backend.as_mut(), &world, &settings, &jobs);
    assert_agree(
        &format!("shoebox air + 0.25 m receiver {kind:?} vs CPU"),
        &result[0],
        &reference[0],
        T30_TOLERANCE,
    );
    // Air must matter: without it the 8 kHz late energy is clearly higher.
    let mut no_air = settings;
    no_air.air_np_per_m = [0.0; BANDS];
    let (dry, _) = trace(backend.as_mut(), &world, &no_air, &jobs);
    let loss = db_ratio(
        window(&result[0], 0.05, LENGTH_S),
        window(&dry[0], 0.05, LENGTH_S),
    );
    println!("{kind:?} air loss in the late window: {loss:.3?} dB");
    assert!(
        loss[3] < -1.0,
        "8 kHz air absorption has no effect: {loss:?}"
    );
}

fn l_room_against_cpu(kind: BackendKind) {
    let (mut world, panel) = l_room_world();
    let settings = settings(LENGTH_S);
    let jobs = [job([8.0, 1.5, 2.0], [2.0, 1.5, 8.0], RAYS, BOUNCES, 7)];
    let mut cpu = cpu();
    let mut backend = gpu(kind);
    let mut previous: Option<EnergyHistogram> = None;
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
        // The GPU result must follow the moved panel, not a stale TLAS/BVH.
        if let Some(before) = &previous {
            assert_ne!(
                before.bins, result[0].bins,
                "{kind:?}: moving the panel changed nothing"
            );
        }
        previous = Some(result[0].clone());
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
    air_and_small_receiver_against_cpu(BackendKind::GpuRayQuery);
    l_room_against_cpu(BackendKind::GpuRayQuery);
    throughput(BackendKind::GpuRayQuery);
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn test18_compute_matches_cpu_and_ray_query() {
    let compute = shoebox_against_cpu(BackendKind::GpuCompute);
    air_and_small_receiver_against_cpu(BackendKind::GpuCompute);
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
fn readback_timeout_is_not_fatal_and_keeps_one_submission_outstanding() {
    let world = shoebox_world(REFERENCE_ALPHA, 1.0, 91);
    let mut backend = GpuBackend::compute(GpuOptions {
        readback_timeout_ms: 1,
        ..Default::default()
    });
    backend.initialize().expect("compute tier");
    backend.sync_world(&world).expect("sync");
    let mut out = Vec::new();
    // About 43 ms per 2^24-segment piece on this mesh: the first piece times out.
    let heavy = [job(SOURCE, LISTENER, 1_048_576, 256, 5)];
    let before = backend.submissions();
    let e = backend
        .trace(&world, &settings(LENGTH_S), &heavy, &mut out)
        .unwrap_err();
    println!("{e}");
    assert_eq!(e.code, "acoustics.backend.timeout");
    assert!(!e.fatal);
    // The split request stopped at its first timed-out piece.
    assert_eq!(backend.submissions(), before + 1);
    assert!(
        backend.info().adapter.is_some(),
        "a timeout must not drop the tier"
    );

    let light = [job(SOURCE, LISTENER, 64, 4, 5)];
    let (mut refused, mut timed_out, mut ok) = (0, 0, false);
    for _ in 0..200 {
        let before = backend.submissions();
        let result = backend.trace(&world, &settings(LENGTH_S), &light, &mut out);
        let submitted = backend.submissions() - before;
        assert!(submitted <= 1, "{submitted} submissions in one light trace");
        match result {
            Ok(_) => {
                ok = true;
                break;
            }
            Err(e) => {
                assert_eq!(e.code, "acoustics.backend.timeout", "{e}");
                if e.message.contains("nothing new was submitted") {
                    // Refused while the earlier work runs: no new submission.
                    assert_eq!(submitted, 0);
                    refused += 1;
                } else {
                    // Its own readback timed out: exactly one new submission.
                    assert_eq!(submitted, 1);
                    timed_out += 1;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    println!("recovered after {refused} refused and {timed_out} timed-out traces");
    assert!(
        refused > 0,
        "no trace was refused while the heavy piece ran"
    );
    assert!(ok, "the tier never recovered after a timeout");
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn split_dispatches_give_bitwise_identical_histograms() {
    let world = shoebox_world(REFERENCE_ALPHA, 0.7, 1);
    let jobs = [
        job(SOURCE, LISTENER, 20_000, 128, 21),
        job(LISTENER, LISTENER, 30_000, 200, 22),
    ];
    let settings = settings(2.0);
    for make in [GpuBackend::ray_query, GpuBackend::compute] {
        let mut whole = make(options());
        let mut split = make(options()).with_segment_budget(1 << 18);
        whole.initialize().expect("tier");
        split.initialize().expect("tier");
        let (a, _) = trace(&mut whole, &world, &settings, &jobs);
        let before = split.submissions();
        let (b, _) = trace(&mut split, &world, &settings, &jobs);
        let pieces = split.submissions() - before;
        println!("{:?}: one submission vs {pieces}", whole.info().kind);
        assert!(pieces > 10);
        assert_eq!(a, b, "splitting changed the result");
    }
}
