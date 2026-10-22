//! The native editor supplies an isolated Project clone. Compilation runs off the
//! UI thread; the source project is never saved as a side effect of Play. Unsaved
//! scene edits reach the game through a generated scene snapshot, and editor play
//! uses a private user-data directory inside the generation, never player saves.
use crate::{
    cli::{compile_request_with, compiler_diagnostic},
    compiler::{BuildFailure, RunningGeneration},
};
use engine_core::{
    Result,
    anyhow::{anyhow, ensure},
};
use engine_editor::{PlayHost, PlayStatus};
use engine_project::Project;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

pub struct NativePlayHost {
    cancel: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<RunningGeneration>>>,
    active: Option<RunningGeneration>,
    /// Outcome of a session that ended before the editor polled it.
    finished: Option<PlayStatus>,
    mode: PlayMode,
    target_dir: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayMode {
    Visible,
    Headless,
}
impl Default for NativePlayHost {
    fn default() -> Self {
        Self::new()
    }
}
impl NativePlayHost {
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            active: None,
            finished: None,
            mode: PlayMode::Visible,
            target_dir: std::env::var_os("ENGINE_NATIVE_TARGET_DIR").map(PathBuf::from),
        }
    }
    pub fn with_mode(mode: PlayMode) -> Self {
        let mut result = Self::new();
        result.mode = mode;
        result
    }
    /// Shares Cargo dependency output between projects (see `CompileRequest`).
    pub fn with_target_dir(mut self, target_dir: impl Into<PathBuf>) -> Self {
        self.target_dir = Some(target_dir.into());
        self
    }
}

/// Human-readable build failure for the editor log, led by compiler errors.
fn describe(error: &engine_core::anyhow::Error) -> String {
    let Some(build) = error.downcast_ref::<BuildFailure>() else {
        return format!("{error:#}");
    };
    let mut message = build.to_string();
    for diagnostic in build.diagnostics.iter().filter(|d| d["level"] == "error") {
        let normalized = compiler_diagnostic(diagnostic);
        message.push_str(&format!(
            "\n{} {}:{} {}",
            normalized["code"].as_str().unwrap_or_default(),
            normalized["file"].as_str().unwrap_or_default(),
            normalized["path"].as_str().unwrap_or_default(),
            normalized["message"].as_str().unwrap_or_default()
        ));
    }
    message
}

fn is_cancellation(error: &engine_core::anyhow::Error) -> bool {
    error
        .downcast_ref::<BuildFailure>()
        .is_some_and(|e| e.cancelled)
        || error.to_string() == CANCELLED
}
const CANCELLED: &str = "Editor play cancelled";

impl PlayHost for NativePlayHost {
    fn start(&mut self, project: Project) -> Result<()> {
        ensure!(
            self.worker.is_none() && self.active.is_none(),
            "Play is already active"
        );
        self.finished = None;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let mode = self.mode;
        let target_dir = self.target_dir.clone();
        self.worker = Some(thread::spawn(move || {
            let (mut compiler, request) = compile_request_with(&project, target_dir)?;
            let artifact = compiler.compile(&request, &cancel)?;
            if cancel.load(Ordering::Acquire) {
                return Err(anyhow!(CANCELLED));
            }
            let snapshot = artifact.directory.join("editor-scene.json");
            std::fs::write(&snapshot, serde_json::to_vec(&project.scene)?)?;
            let mut arguments = vec![
                project.manifest.into_os_string(),
                "--ticks".into(),
                "1000000".into(),
                "--scene-override".into(),
                snapshot.into_os_string(),
                "--user-data".into(),
                artifact.directory.join("editor-user-data").into_os_string(),
            ];
            if mode == PlayMode::Headless {
                arguments.push("--headless".into());
            }
            let mut running = RunningGeneration::prepare(artifact, &arguments, &cancel)?;
            if cancel.load(Ordering::Acquire) {
                return Err(anyhow!(CANCELLED));
            }
            running.start()?;
            Ok(running)
        }));
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        self.cancel.store(true, Ordering::Release);
        self.finished = None;
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(running)) => self.active = Some(running),
                // A cancelled or failed build never started a game.
                Ok(Err(_)) => {}
                Err(_) => return Err(anyhow!("Editor compiler worker panicked")),
            }
        }
        if let Some(mut running) = self.active.take() {
            let exit = running.stop()?;
            ensure!(
                !exit.forced,
                "Editor play exceeded its graceful shutdown deadline"
            );
        }
        Ok(())
    }
    fn poll(&mut self) -> Result<PlayStatus> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(PlayStatus::Compiling);
        }
        if let Some(worker) = self.worker.take() {
            match worker
                .join()
                .map_err(|_| anyhow!("Editor compiler worker panicked"))?
            {
                Ok(running) => self.active = Some(running),
                Err(error) => {
                    // Compilation or startup failure ends the session; the
                    // authored project and any earlier generation are untouched.
                    self.finished = Some(PlayStatus::Stopped {
                        success: is_cancellation(&error),
                        message: describe(&error),
                    });
                }
            }
        }
        if let Some(running) = self.active.as_mut() {
            if let Some(exit) = running.try_wait()? {
                self.active.take();
                return Ok(PlayStatus::Stopped {
                    success: exit.success,
                    message: format!("Native game exited with {:?}", exit.code),
                });
            }
            return Ok(PlayStatus::Running);
        }
        Ok(self.finished.take().unwrap_or(PlayStatus::Stopped {
            success: true,
            message: "Stopped".into(),
        }))
    }
}
impl Drop for NativePlayHost {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
