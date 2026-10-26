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
    mode: PlayMode,
    target_dir: Option<PathBuf>,
    reloading: bool,
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
            mode: PlayMode::Visible,
            target_dir: std::env::var_os("ENGINE_NATIVE_TARGET_DIR").map(PathBuf::from),
            reloading: false,
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
    fn prepare(&mut self, project: Project) {
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
            RunningGeneration::prepare(artifact, &arguments, &cancel).map_err(Into::into)
        }));
    }

    /// Join and discard the pending candidate without ever starting gameplay.
    fn discard_candidate(&mut self) -> Result<()> {
        self.cancel.store(true, Ordering::Release);
        self.reloading = false;
        if let Some(worker) = self.worker.take() {
            // Dropping either a prepared generation or an error leaves the old
            // active generation alone. Even a panic cannot leak a prepared child.
            drop(
                worker
                    .join()
                    .map_err(|_| anyhow!("Editor compiler worker panicked"))?,
            );
        }
        Ok(())
    }

    fn candidate_failed(&mut self, error: engine_core::anyhow::Error) -> PlayStatus {
        self.reloading = false;
        let message = describe(&error);
        if self.active.is_some() {
            PlayStatus::ReloadFailed {
                message: format!("Reload rejected; previous game continues: {message}"),
            }
        } else {
            PlayStatus::Stopped {
                success: is_cancellation(&error),
                message,
            }
        }
    }

    fn activate(&mut self, mut candidate: RunningGeneration) -> PlayStatus {
        self.reloading = false;
        if let Some(mut old) = self.active.take() {
            let teardown = old.stop();
            match teardown {
                Ok(exit) if exit.success && !exit.forced => {}
                result => {
                    // Once teardown begins there is no rollback/state transfer.
                    // Drop owns final cleanup, and candidate never receives start.
                    let message = match result {
                        Ok(exit) => format!(
                            "Previous game teardown failed (exit {:?}, forced {}); candidate not started",
                            exit.code, exit.forced
                        ),
                        Err(error) => {
                            format!("Previous game teardown failed; candidate not started: {error}")
                        }
                    };
                    return PlayStatus::Stopped {
                        success: false,
                        message,
                    };
                }
            }
        }
        if let Err(error) = candidate.start() {
            return PlayStatus::Stopped {
                success: false,
                message: format!("Candidate activation failed; no game remains active: {error}"),
            };
        }
        self.active = Some(candidate);
        PlayStatus::Running
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
        self.reloading = false;
        self.prepare(project);
        Ok(())
    }
    fn reload(&mut self, project: Project) -> Result<()> {
        ensure!(self.active.is_some(), "No running game to reload");
        ensure!(
            self.worker.is_none(),
            "A replacement is already being prepared"
        );
        self.reloading = true;
        self.prepare(project);
        Ok(())
    }
    fn cancel_reload(&mut self) -> Result<()> {
        ensure!(self.reloading, "No replacement is being prepared");
        self.discard_candidate()
    }
    fn stop(&mut self) -> Result<()> {
        // Always release active even if joining the candidate reports a panic.
        let candidate_result = self.discard_candidate();
        let active_result = if let Some(mut running) = self.active.take() {
            running.stop().map_err(Into::into).and_then(|exit| {
                ensure!(
                    !exit.forced,
                    "Editor play exceeded its graceful shutdown deadline"
                );
                ensure!(
                    exit.success,
                    "Editor play shutdown failed with {:?}",
                    exit.code
                );
                Ok(())
            })
        } else {
            Ok(())
        };
        candidate_result.and(active_result)
    }
    fn poll(&mut self) -> Result<PlayStatus> {
        // Observe the old game even while compiling. If it exits, the explicitly
        // requested candidate may still complete, but failures cannot claim the
        // previous game is still running.
        let mut previous_exit = None;
        if let Some(running) = self.active.as_mut() {
            if let Some(exit) = running.try_wait()? {
                self.active.take();
                previous_exit = Some(PlayStatus::Stopped {
                    success: exit.success,
                    message: format!("Native game exited with {:?}", exit.code),
                });
            }
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(if self.reloading && self.active.is_some() {
                PlayStatus::Reloading
            } else {
                PlayStatus::Compiling
            });
        }
        if let Some(worker) = self.worker.take() {
            let candidate = worker
                .join()
                .unwrap_or_else(|_| Err(anyhow!("Editor compiler worker panicked")));
            return Ok(match candidate {
                Ok(candidate) if self.cancel.load(Ordering::Acquire) => {
                    drop(candidate);
                    self.candidate_failed(anyhow!(CANCELLED))
                }
                Ok(candidate) => self.activate(candidate),
                Err(error) => self.candidate_failed(error),
            });
        }
        if self.active.is_some() {
            return Ok(PlayStatus::Running);
        }
        Ok(previous_exit.unwrap_or(PlayStatus::Stopped {
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

#[cfg(test)]
#[path = "play_host_tests.rs"]
mod tests;
