//! Headless runs of a project with an `engine.AcousticEnvironment`: the status the
//! run report carries, the forced `--acoustics-backend` override and the host
//! configuration. Tests that need a GPU are `#[ignore]`d; run them with
//! `cargo test -p engine-desktop --test acoustics_headless -- --ignored`.

use engine_acoustics::{AcousticState, BackendKind, BackendPreference, ServiceMode};
use engine_desktop::{RunOptions, RunReport, parse_acoustics_backend, run};
use engine_project::{Project, Runtime};
use std::path::Path;

fn showcase() -> Runtime {
    Runtime::new(
        Project::load(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/acoustics-showcase/project.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn options(user_data: &tempfile::TempDir, backend: Option<BackendPreference>) -> RunOptions {
    RunOptions {
        headless: true,
        ticks: 60,
        user_data: Some(user_data.path().to_owned()),
        acoustics_backend: backend,
        ..Default::default()
    }
}

fn run_with(runtime: Runtime, backend: Option<BackendPreference>) -> RunReport {
    let user_data = tempfile::tempdir().unwrap();
    run(runtime, options(&user_data, backend)).unwrap()
}

#[test]
fn the_backend_option_parses_the_documented_values() {
    for (text, expected) in [
        ("auto", BackendPreference::Auto),
        ("ray_query", BackendPreference::GpuRayQuery),
        ("compute", BackendPreference::GpuCompute),
        ("cpu", BackendPreference::Cpu),
        ("gpu_ray_query", BackendPreference::GpuRayQuery),
        ("gpu_compute", BackendPreference::GpuCompute),
    ] {
        assert_eq!(parse_acoustics_backend(text).unwrap(), expected, "{text}");
    }
    let error = parse_acoustics_backend("vulkan").unwrap_err().to_string();
    assert!(error.contains("auto, ray_query, compute or cpu"), "{error}");
}

#[test]
fn a_forced_cpu_run_reports_active_cpu_without_opening_a_gpu() {
    let before = engine_acoustics_gpu::wgpu_instances_created();
    let report = run_with(showcase(), Some(BackendPreference::Cpu));
    let status = report
        .scene
        .acoustics
        .expect("an environment reports status");
    assert_eq!(status.requested, BackendPreference::Cpu);
    assert_eq!(status.state, AcousticState::Active, "{status:?}");
    assert_eq!(status.active, Some(BackendKind::Cpu));
    assert!(!status.hardware_rt && status.adapter.is_none());
    assert_eq!(status.fallback_reason, None);
    assert_eq!(status.world.triangles, 14);
    assert!(status.updates > 0 && status.publishes > 0, "{status:?}");
    assert_eq!(
        engine_acoustics_gpu::wgpu_instances_created(),
        before,
        "a cpu preference never creates a wgpu instance"
    );
}

#[test]
fn headless_runs_are_synchronous_and_repeat_exactly() {
    let first = run_with(showcase(), Some(BackendPreference::Cpu));
    let second = run_with(showcase(), Some(BackendPreference::Cpu));
    let (a, b) = (
        first.scene.acoustics.unwrap(),
        second.scene.acoustics.unwrap(),
    );
    assert_eq!((a.updates, a.publishes), (b.updates, b.publishes));
    assert_eq!(a.rays_last_update, b.rays_last_update);
    assert_eq!(a.sources.len(), b.sources.len());
    for (x, y) in a.sources.iter().zip(&b.sources) {
        assert_eq!(x.visible_fraction.to_bits(), y.visible_fraction.to_bits());
    }
    // The divider hides the speaker from the listener.
    assert!(a.sources[0].visible_fraction < 0.1, "{:?}", a.sources);
}

#[test]
fn an_unavailable_gpu_tier_falls_back_with_a_reason() {
    // The host configured no GPU backends, so the authored `auto` falls to the CPU.
    let mut runtime = showcase();
    runtime
        .set_acoustics(vec![], ServiceMode::Synchronous)
        .unwrap();
    let report = run_with(runtime, None);
    let status = report.scene.acoustics.unwrap();
    assert_eq!(status.requested, BackendPreference::Auto);
    assert_eq!(status.state, AcousticState::Fallback, "{status:?}");
    assert_eq!(status.active, Some(BackendKind::Cpu));
    let reason = status.fallback_reason.expect("a fallback explains itself");
    assert!(reason.contains("no backend configured"), "{reason}");
    assert!(reason.len() <= 512);
}

#[test]
fn a_scene_without_an_environment_reports_no_acoustics() {
    let report = run(
        Runtime::new(
            Project::load(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/project.json"),
            )
            .unwrap(),
        )
        .unwrap(),
        options(&tempfile::tempdir().unwrap(), Some(BackendPreference::Auto)),
    )
    .unwrap();
    assert!(report.scene.acoustics.is_none());
}

// ---- GPU (RTX 5080 on the development machine) -------------------------------------------

#[test]
#[ignore = "needs a Vulkan GPU with ray query; run on the RTX 5080 with --ignored"]
fn a_gpu_run_reports_the_active_tier_and_adapter() {
    let report = run_with(showcase(), None);
    let status = report.scene.acoustics.unwrap();
    eprintln!("auto: {status:#?}");
    assert_eq!(status.requested, BackendPreference::Auto);
    assert_eq!(status.state, AcousticState::Active, "{status:?}");
    assert_eq!(status.active, Some(BackendKind::GpuRayQuery));
    assert!(status.hardware_rt);
    assert!(status.adapter.as_deref().is_some_and(|a| !a.is_empty()));
    assert_eq!(status.fallback_reason, None);
    assert!(status.updates > 0 && status.aborted_rays == 0, "{status:?}");

    let compute = run_with(showcase(), Some(BackendPreference::GpuCompute));
    let status = compute.scene.acoustics.unwrap();
    assert_eq!(status.state, AcousticState::Active, "{status:?}");
    assert_eq!(status.active, Some(BackendKind::GpuCompute));
    assert!(!status.hardware_rt);
}

#[test]
#[ignore = "needs a Vulkan GPU with ray query; run on the RTX 5080 with --ignored"]
fn the_acoustics_device_coexists_with_a_live_renderer_device() {
    use engine_desktop::{GpuContext, GpuRenderer, OffscreenTarget, RenderLimits};
    use engine_project::{Camera2D, runtime::RenderFrame};
    let gpu = pollster::block_on(GpuContext::headless()).unwrap();
    let target = OffscreenTarget::new(&gpu, 64, 64).unwrap();
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default()).unwrap();
    let frame = RenderFrame {
        camera: Camera2D {
            logical_size: [64, 64],
            pixels_per_unit: 1.0,
            ..Default::default()
        },
        draws: vec![],
    };
    renderer.render(&frame, &target.view, None).unwrap();
    // The renderer's device is alive while the acoustics service opens its own.
    let report = run_with(showcase(), None);
    let status = report.scene.acoustics.unwrap();
    assert_eq!(status.state, AcousticState::Active, "{status:?}");
    assert_eq!(status.active, Some(BackendKind::GpuRayQuery));
    renderer.render(&frame, &target.view, None).unwrap();
    let pixels = target.read_rgba(&renderer.gpu).unwrap();
    assert_eq!(pixels.len(), 64 * 64 * 4);
}
