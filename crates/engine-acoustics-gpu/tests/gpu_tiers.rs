//! Tier initialization, self-tests and forced fallbacks on real hardware
//! (docs/spatial-audio-design.md tests 17-19). Ignored by default; run with
//! `cargo test -p engine-acoustics-gpu --test gpu_tiers -- --ignored --nocapture`.

use engine_acoustics::{AcousticState, BackendKind, BackendPreference, PropagationBackend};
use engine_acoustics_gpu::{
    GpuBackend, GpuOptions, GraphicsApi, probe_capabilities, select_backend, wgpu_instances_created,
};

#[test]
#[ignore = "requires a native GPU adapter"]
fn capabilities_report_ray_query_per_adapter() {
    let adapters = probe_capabilities().expect("probe");
    for a in &adapters {
        println!(
            "adapter {:?} backend={} ray_query={} reason={:?}",
            a.name, a.backend, a.ray_query, a.reason
        );
        if !a.ray_query {
            assert!(a.reason.is_some());
        }
    }
    assert!(!adapters.is_empty());
}

#[test]
#[ignore = "requires a Vulkan adapter with ray query (verified on an RTX 5080)"]
fn ray_query_tier_passes_its_self_test() {
    let mut backend = GpuBackend::ray_query(GpuOptions::default());
    backend.initialize().expect("ray-query tier");
    let info = backend.info();
    println!("info: {info:?}");
    println!("self-test: {:?}", backend.self_test_report());
    assert_eq!(info.kind, BackendKind::GpuRayQuery);
    assert!(info.hardware_rt);
    assert!(info.adapter.as_deref().unwrap().contains("Vulkan"));
    assert!(info.driver.is_some());
    let report = backend.self_test_report().unwrap();
    assert_eq!(report.quad_hits, report.quad_expected_hits);
    assert_eq!(report.quad_expected_hits, 16);
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn compute_tier_passes_its_self_test_without_claiming_hardware_rt() {
    let mut backend = GpuBackend::compute(GpuOptions::default());
    backend.initialize().expect("compute tier");
    let info = backend.info();
    println!("info: {info:?}");
    println!("self-test: {:?}", backend.self_test_report());
    assert_eq!(info.kind, BackendKind::GpuCompute);
    assert!(!info.hardware_rt);
    assert!(info.adapter.is_some());
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn denied_ray_query_falls_back_to_compute() {
    let options = GpuOptions {
        deny_ray_query: true,
        ..Default::default()
    };
    let cpu = Box::new(engine_acoustics::CpuBackend::new(1).unwrap());
    let selection = select_backend(
        BackendPreference::GpuRayQuery,
        &options,
        GraphicsApi::Primary,
        cpu,
    )
    .expect("a tier");
    println!("{selection:?}");
    assert_eq!(selection.active, BackendKind::GpuCompute);
    assert_eq!(selection.state, AcousticState::Fallback);
    assert!(!selection.hardware_rt);
    let reason = selection.fallback_reason.unwrap();
    assert!(reason.contains("EXPERIMENTAL_RAY_QUERY"), "{reason}");
}

#[test]
#[ignore = "requires a native DX12 adapter (Windows)"]
fn dx12_reports_why_ray_query_is_unavailable() {
    let mut backend = GpuBackend::ray_query(GpuOptions::default()).with_api(GraphicsApi::Dx12);
    let e = backend.initialize().unwrap_err();
    println!("{e}");
    assert_eq!(e.code, "acoustics.backend.unavailable");
    assert!(e.message.contains("DX12"), "{}", e.message);
    // The compute tier still works on DX12.
    let mut compute = GpuBackend::compute(GpuOptions::default()).with_api(GraphicsApi::Dx12);
    compute.initialize().expect("compute tier on DX12");
    println!("DX12 compute: {:?}", compute.info());
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn forced_self_test_failure_names_the_step_and_demotes() {
    let options = GpuOptions {
        force_self_test_failure: true,
        ..Default::default()
    };
    let mut backend = GpuBackend::compute(options.clone());
    let e = backend.initialize().unwrap_err();
    println!("{e}");
    assert!(
        e.message.contains("self-test step 'quad hit count'"),
        "{}",
        e.message
    );
    assert!(!backend.info().hardware_rt && backend.info().adapter.is_none());
    // With every GPU tier failing, the selection falls through to the CPU tier;
    // the CPU tier's own result decides the outcome.
    let cpu = Box::new(engine_acoustics::CpuBackend::new(1).unwrap());
    match select_backend(BackendPreference::Auto, &options, GraphicsApi::Primary, cpu) {
        Ok(selection) => {
            println!("{selection:?}");
            assert_eq!(selection.active, BackendKind::Cpu);
            let reason = selection.fallback_reason.unwrap();
            assert!(reason.contains("GpuRayQuery: ") && reason.contains("GpuCompute: "));
            assert!(reason.contains("quad hit count"), "{reason}");
        }
        Err(e) => {
            println!("no tier: {e}");
            assert!(e.message.contains("quad hit count"), "{}", e.message);
            assert!(e.message.contains("Cpu: "), "{}", e.message);
        }
    }
}

#[test]
fn cpu_preference_never_creates_a_wgpu_instance() {
    let before = wgpu_instances_created();
    let cpu = Box::new(engine_acoustics::CpuBackend::new(1).unwrap());
    let result = select_backend(
        BackendPreference::Cpu,
        &GpuOptions::default(),
        GraphicsApi::Primary,
        cpu,
    );
    assert_eq!(wgpu_instances_created(), before);
    if let Ok(selection) = result {
        assert_eq!(selection.active, BackendKind::Cpu);
        assert_eq!(selection.state, AcousticState::Active);
        assert!(selection.fallback_reason.is_none());
    }
}
