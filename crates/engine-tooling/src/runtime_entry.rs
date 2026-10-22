//! Entry point linked into a native gameplay generation. The callback is a real
//! Rust function operating on the same public ECS/world types as the engine.
use crate::arguments::{parse_run, validate_actions};
use engine_core::{EngineApp, Result};
use engine_desktop::RunOptions;
use engine_project::{Project, Runtime, Scene};
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

fn execute(configure: fn(&mut EngineApp) -> Result<()>) -> Result<engine_desktop::RunReport> {
    let arguments = parse_run(&std::env::args_os().skip(1).collect::<Vec<_>>())?;
    let mut project = Project::load(&arguments.manifest)?;
    validate_actions(&project, &arguments)?;
    if let Some(file) = &arguments.scene_override {
        apply_scene_override(&mut project, file)?;
    }
    let mut runtime = Runtime::new(project)?;
    configure(runtime.app_mut())?;
    let monitor = StopMonitor::new();
    engine_desktop::run(
        runtime,
        RunOptions {
            headless: arguments.headless,
            ticks: arguments.ticks,
            inputs: arguments.inputs,
            user_data: arguments.user_data,
            shutdown: Some(monitor.shutdown.clone()),
        },
    )
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
