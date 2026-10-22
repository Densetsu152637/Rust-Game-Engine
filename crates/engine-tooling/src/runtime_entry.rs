//! Entry point linked into a native gameplay generation. The callback is a real
//! Rust function operating on the same public ECS/world types as the engine.
use crate::arguments::{RunArguments, parse_run, validate_actions};
use engine_core::{EngineApp, Result};
use engine_desktop::RunOptions;
use engine_project::{Project, Runtime, Scene};
use image::ImageEncoder;
use serde_json::json;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

pub fn run(configure: fn(&mut EngineApp) -> Result<()>) -> i32 {
    // A generation is a host binary, so it installs the logging subscriber that
    // makes engine-core lifecycle `tracing` events visible. Output goes to stderr
    // only: stdout carries the run result that the launcher parses. A logging
    // failure must not stop the game.
    let _log = engine_log::init(engine_log::LogConfig::new())
        .inspect_err(|error| eprintln!("warning: logging disabled: {error}"))
        .ok();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(configure)));
    let (value, code) = match result {
        Ok(Ok(report)) => (
            json!({"schema":1,"ok":true,"command":"run","result":report}),
            0,
        ),
        Ok(Err(error)) => {
            eprintln!("{error:#}");
            let project = error.downcast_ref::<engine_project::Error>();
            let code = if project
                .is_some_and(|e| e.diagnostics.iter().any(|d| d.code.starts_with("command.")))
            {
                2
            } else {
                3
            };
            let diagnostics = match project {
                Some(project) => serde_json::to_value(&project.diagnostics).unwrap_or_default(),
                None => {
                    json!([{"code":"runtime.failed","severity":"error","file":"","path":"","message":format!("{error:#}")}])
                }
            };
            (
                json!({"schema":1,"ok":false,"command":"run","result":{"diagnostics":diagnostics}}),
                code,
            )
        }
        Err(_) => (
            json!({"schema":1,"ok":false,"command":"run","result":{"diagnostics":[{"code":"runtime.panic","severity":"error","file":"","path":"","message":"Native gameplay panicked; restart from known state"}]}}),
            3,
        ),
    };
    match serde_json::to_writer(std::io::stdout().lock(), &value).and_then(|()| {
        std::io::stdout()
            .write_all(b"\n")
            .map_err(serde_json::Error::io)
    }) {
        Ok(()) => code,
        Err(error) => {
            eprintln!("runtime output: {error}");
            3
        }
    }
}

fn execute(configure: fn(&mut EngineApp) -> Result<()>) -> Result<serde_json::Value> {
    let arguments = parse_run(&std::env::args_os().skip(1).collect::<Vec<_>>())?;
    let mut project = Project::load(&arguments.manifest)?;
    validate_actions(&project, &arguments)?;
    if let Some(file) = &arguments.scene_override {
        apply_scene_override(&mut project, file)?;
    }
    let mut runtime = Runtime::new(project)?;
    configure(runtime.app_mut())?;
    let monitor = StopMonitor::new();
    if let Some(path) = arguments.screenshot.clone() {
        return screenshot(runtime, &arguments, &path, &monitor.shutdown);
    }
    let report = engine_desktop::run_with_wgpu_backends(
        runtime,
        RunOptions {
            headless: arguments.headless,
            ticks: arguments.ticks,
            inputs: arguments.inputs,
            user_data: arguments.user_data,
            shutdown: Some(monitor.shutdown.clone()),
        },
        arguments.render_backend,
    )?;
    Ok(serde_json::to_value(report)?)
}

/// Runs the fixed-tick headless simulation (including scripted input, offline
/// audio and the optional user-data root), then renders the final frame to a PNG
/// through an offscreen GPU target. The report is the normal run report plus a
/// `screenshot` object. No GPU adapter yields the diagnostic
/// `render.gpu.unavailable`.
///
/// The wgpu graphics API is chosen like a visible run's: `--render-backend`,
/// else `ENGINE_RENDER_BACKEND`, else the platform default; an invalid
/// environment value fails before anything is simulated or written.
fn screenshot(
    runtime: Runtime,
    arguments: &RunArguments,
    path: &Path,
    shutdown: &AtomicBool,
) -> Result<serde_json::Value> {
    let backends = engine_desktop::resolve_backends_from_environment(arguments.render_backend);
    screenshot_on(runtime, arguments, path, shutdown, backends)
}

fn screenshot_on(
    mut runtime: Runtime,
    arguments: &RunArguments,
    path: &Path,
    shutdown: &AtomicBool,
    backends: Result<engine_desktop::WgpuBackends>,
) -> Result<serde_json::Value> {
    use engine_desktop::{
        FIXED_DELTA, GpuContext, GpuRenderer, GpuRequirements, NativeRenderPasses, OffscreenTarget,
        RenderLimits,
    };
    use engine_project::input::merge_input;
    let backends = backends?;
    let [width, height] = arguments.screenshot_size;
    if let Some(root) = &arguments.user_data {
        runtime.set_user_data_root(root.clone())?;
    }
    let mut audio = engine_audio::AudioEngine::offline(engine_audio::AudioConfig::default())?;
    runtime.set_audio(audio.control())?;
    runtime.start()?;
    let result = (|| -> Result<serde_json::Value> {
        let mut previous = std::collections::BTreeSet::new();
        let mut ticks = 0_u64;
        let mut mixed = [0.0_f32; 1600];
        for tick in 0..arguments.ticks {
            if shutdown.load(Ordering::Acquire) || runtime.app().stop_requested() {
                break;
            }
            let actions = arguments.inputs.get(&tick).cloned().unwrap_or_default();
            let input = merge_input(&Default::default(), &actions, &mut previous);
            runtime.tick(FIXED_DELTA as f32, input)?;
            audio.mix(&mut mixed)?;
            ticks += 1;
        }
        let frame = runtime.extract_frame()?;
        let requirements = runtime
            .app_mut()
            .world_mut()
            .remove_resource::<GpuRequirements>()
            .unwrap_or_default();
        let passes = runtime
            .app_mut()
            .world_mut()
            .remove_resource::<NativeRenderPasses>()
            .unwrap_or_default();
        let gpu = pollster::block_on(GpuContext::request_with_requirements(
            GpuContext::instance_with(backends),
            None,
            requirements,
        ))
        .map_err(|error| {
            engine_project::Error::new(
                "render.gpu.unavailable",
                format!("No usable GPU adapter for an offscreen render: {error:#}"),
            )
        })?;
        let info = gpu.adapter.get_info();
        let adapter = json!({
            "name": info.name,
            "backend": format!("{:?}", info.backend),
            "device_type": format!("{:?}", info.device_type),
        });
        let target = OffscreenTarget::new(&gpu, width, height)?;
        let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
        renderer.set_passes(passes)?;
        renderer.render(&frame, &target.view, Some(runtime.app().render_world()))?;
        let pixels = target.read_rgba(&renderer.gpu)?;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(
            &pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
        std::fs::write(path, &png)?;
        Ok(json!({"width": width, "height": height, "bytes": png.len(),
            "path": path, "adapter": adapter, "ticks": ticks}))
    })();
    let cleanup = runtime.stop();
    audio.shutdown();
    let shot = result?;
    cleanup?;
    Ok(json!({
        "ticks": shot["ticks"],
        "fixed_delta": FIXED_DELTA,
        "scene": runtime.snapshot(),
        "presented_frames": 0,
        "screenshot": shot,
    }))
}

/// Editor play supplies a validated scene snapshot while retaining the original
/// project's asset root. No authoring file is overwritten to run unsaved edits.
pub fn apply_scene_override(project: &mut Project, file: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    std::fs::File::open(file)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    engine_core::anyhow::ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Scene snapshot exceeds 16 MiB"
    );
    let scene: Scene = serde_json::from_slice(&bytes)?;
    project.validate_scene(&scene)?;
    project.scene = scene;
    Ok(())
}

struct StopMonitor {
    shutdown: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl StopMonitor {
    fn new() -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let worker = std::env::var_os("ENGINE_GENERATION_STOP_FILE").map(|file| {
            let shutdown = shutdown.clone();
            let finished = finished.clone();
            thread::spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    if Path::new(&file).is_file() {
                        shutdown.store(true, Ordering::Release);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })
        });
        Self {
            shutdown,
            finished,
            worker,
        }
    }
}
impl Drop for StopMonitor {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_desktop::resolve_wgpu_backends;

    fn runtime(directory: &Path) -> Runtime {
        std::fs::write(
            directory.join("project.json"),
            r#"{"schema":1,"name":"shot","startup_scene":"main.scene.json","assets":[],"inputActions":{}}"#,
        )
        .unwrap();
        std::fs::write(
            directory.join("main.scene.json"),
            r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
        )
        .unwrap();
        Runtime::new(Project::load(directory.join("project.json")).unwrap()).unwrap()
    }

    #[test]
    fn an_invalid_graphics_api_fails_the_screenshot_path_before_anything_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let png = directory.path().join("shot.png");
        let arguments = parse_run(&[
            directory.path().join("project.json").into_os_string(),
            "--screenshot".into(),
            png.clone().into_os_string(),
        ])
        .unwrap();
        let error = screenshot_on(
            runtime(directory.path()),
            &arguments,
            &png,
            &AtomicBool::new(false),
            resolve_wgpu_backends(arguments.render_backend, Some("glide")),
        )
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("ENGINE_RENDER_BACKEND"), "{text}");
        assert!(
            text.contains("primary, vulkan, dx12, metal, gl or all"),
            "{text}"
        );
        assert!(!png.exists());
        assert!(
            error
                .downcast_ref::<engine_project::Error>()
                .is_none_or(|e| e
                    .diagnostics
                    .iter()
                    .all(|d| d.code != "render.gpu.unavailable")),
            "a bad selection is not a missing GPU"
        );
    }

    #[test]
    fn the_render_backend_flag_overrides_a_bad_environment_value() {
        let backends =
            resolve_wgpu_backends(Some(engine_desktop::WgpuBackends::Dx12), Some("glide"));
        assert_eq!(backends.unwrap(), engine_desktop::WgpuBackends::Dx12);
    }
}
