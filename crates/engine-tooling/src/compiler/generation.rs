//! Running generations and hot-replacement between them.

use super::native::NativeCompiler;
use super::process::*;
use super::{
    BuildArtifact, BuildFailure, CompileRequest, GENERATION_SEQUENCE, cancelled, failure, io_error,
};
use process_wrap::std::ChildWrapper;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// Owns and reaps a child process. Dropping it cancels a still-running child.
pub struct RunningGeneration {
    pub artifact: BuildArtifact,
    child: Box<dyn ChildWrapper>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    ready_file: PathBuf,
    stop_file: PathBuf,
    started: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationExit {
    pub success: bool,
    pub code: Option<i32>,
    pub cancelled: bool,
    pub forced: bool,
}

impl RunningGeneration {
    pub fn spawn(artifact: BuildArtifact, arguments: &[OsString]) -> Result<Self, BuildFailure> {
        let mut running = Self::prepare(artifact, arguments, &AtomicBool::new(false))?;
        running.start()?;
        Ok(running)
    }

    /// Start the executable behind its readiness barrier. No gameplay runs until
    /// `start` is called, so preparation cannot claim save/window ownership.
    pub fn prepare(
        artifact: BuildArtifact,
        arguments: &[OsString],
        cancel: &AtomicBool,
    ) -> Result<Self, BuildFailure> {
        Self::prepare_with_output(artifact, arguments, cancel, false)
    }

    /// As [`Self::prepare`]; `capture_stdout` pipes the generation's standard
    /// output for [`Self::take_stdout`] instead of inheriting the caller's.
    pub fn prepare_with_output(
        artifact: BuildArtifact,
        arguments: &[OsString],
        cancel: &AtomicBool,
        capture_stdout: bool,
    ) -> Result<Self, BuildFailure> {
        let control = format!(
            "control-{}",
            GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let ready_file = artifact.directory.join(format!("{control}.ready"));
        let stop_file = artifact.directory.join(format!("{control}.stop"));
        let mut command = Command::new(&artifact.executable);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .env("ENGINE_GENERATION_READY_FILE", &ready_file)
            .env("ENGINE_GENERATION_STOP_FILE", &stop_file);
        if capture_stdout {
            command.stdout(Stdio::piped());
        }
        let mut child = spawn_owned(command)?;
        let stdin = child.stdin().take();
        let stdout = child.stdout().take();
        let mut result = Self {
            artifact,
            child,
            stdin,
            stdout,
            ready_file,
            stop_file,
            started: false,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if let Some(exit) = result.try_wait()? {
                return Err(failure(
                    "process.ready",
                    format!("Generation exited before readiness: {:?}", exit.code),
                ));
            }
            if result.ready_file.is_file() {
                return Ok(result);
            }
            if std::time::Instant::now() >= deadline {
                return Err(failure(
                    "process.ready_timeout",
                    "Generation did not reach its startup barrier",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn start(&mut self) -> Result<(), BuildFailure> {
        if self.started {
            return Err(failure("process.started", "Generation has already started"));
        }
        let mut stdin = self
            .stdin
            .take()
            .ok_or_else(|| failure("process.start", "Generation control pipe is closed"))?;
        stdin
            .write_all(b"start\n")
            .and_then(|()| stdin.flush())
            .map_err(io_error)?;
        self.started = true;
        Ok(())
    }
    /// The piped standard output when prepared with `capture_stdout`.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }
    /// Fault injection for lifecycle tests: retaining this handle keeps the real
    /// prepared child alive while activation exercises a missing control pipe.
    #[cfg(test)]
    pub(crate) fn take_start_pipe_for_test(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> Result<Option<GenerationExit>, BuildFailure> {
        self.child.try_wait().map_err(io_error).map(|status| {
            status.map(|s| GenerationExit {
                success: s.success(),
                code: s.code(),
                cancelled: false,
                forced: false,
            })
        })
    }
    pub fn stop(&mut self) -> Result<GenerationExit, BuildFailure> {
        self.stop_with_timeout(Duration::from_secs(2))
    }
    pub fn stop_with_timeout(&mut self, timeout: Duration) -> Result<GenerationExit, BuildFailure> {
        if let Some(exit) = self.try_wait()? {
            // End descendants that outlived the leader; its status is known.
            terminate_tree(&mut self.child)?;
            return Ok(exit);
        }
        if self.started {
            fs::write(&self.stop_file, b"stop").map_err(io_error)?;
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                if let Some(mut exit) = self.try_wait()? {
                    exit.cancelled = true;
                    return Ok(exit);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        terminate_tree(&mut self.child)?;
        let status = reap(&mut self.child)?;
        Ok(GenerationExit {
            success: false,
            code: status.code(),
            cancelled: true,
            forced: true,
        })
    }
    pub fn wait(&mut self, cancel: &AtomicBool) -> Result<GenerationExit, BuildFailure> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return self.stop();
            }
            if let Some(exit) = self.try_wait()? {
                return Ok(exit);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for RunningGeneration {
    fn drop(&mut self) {
        if self.stop().is_err() {
            let _ = terminate_tree(&mut self.child);
            let _ = reap(&mut self.child);
        }
        let _ = fs::remove_file(&self.ready_file);
        let _ = fs::remove_file(&self.stop_file);
    }
}

/// Candidate compilation/readiness happen before the previous process is stopped.
/// The candidate's gameplay starts only after successful graceful old teardown.
pub struct GenerationManager {
    pub compiler: NativeCompiler,
    pub active: Option<RunningGeneration>,
}
impl GenerationManager {
    pub fn new(compiler: NativeCompiler) -> Self {
        Self {
            compiler,
            active: None,
        }
    }
    pub fn rebuild_and_replace(
        &mut self,
        request: &CompileRequest,
        arguments: &[OsString],
        cancel: &AtomicBool,
    ) -> Result<&mut RunningGeneration, BuildFailure> {
        let previous_good = self.compiler.last_good.clone();
        let candidate = self.compiler.compile(request, cancel)?;
        if cancel.load(Ordering::Acquire) {
            self.compiler.last_good = previous_good;
            return Err(cancelled());
        }
        let mut running = match RunningGeneration::prepare(candidate, arguments, cancel) {
            Ok(running) => running,
            Err(error) => {
                self.compiler.last_good = previous_good;
                return Err(error);
            }
        };
        if cancel.load(Ordering::Acquire) {
            self.compiler.last_good = previous_good;
            return Err(cancelled());
        }
        if let Some(old) = self.active.as_mut() {
            match old.stop() {
                Ok(exit) if exit.success && !exit.forced => {}
                Ok(_) => {
                    self.compiler.last_good = previous_good;
                    return Err(failure(
                        "process.teardown",
                        "Previous generation did not shut down cleanly; candidate not activated",
                    ));
                }
                Err(error) => {
                    self.compiler.last_good = previous_good;
                    return Err(error);
                }
            }
        }
        if let Err(error) = running.start() {
            self.compiler.last_good = previous_good;
            return Err(error);
        }
        self.active = Some(running);
        Ok(self.active.as_mut().expect("installed generation"))
    }
}

#[cfg(test)]
mod lifecycle_regressions {
    use super::super::process::run_compiler;
    use super::*;
    use std::path::Path;

    #[test]
    fn cancellation_remains_live_after_leader_exits_with_inherited_pipes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("probe.rs");
        let executable = temp
            .path()
            .join(if cfg!(windows) { "probe.exe" } else { "probe" });
        fs::write(
            &source,
            r#"fn main(){
            if std::env::args().any(|a|a=="--child"){
                std::thread::sleep(std::time::Duration::from_secs(60));
            }else{
                std::process::Command::new(std::env::current_exe().unwrap()).arg("--child")
                    .stdin(std::process::Stdio::null()).spawn().unwrap();
            }
        }"#,
        )
        .unwrap();
        let cargo = Path::new(env!("CARGO"));
        let rustc = cargo
            .parent()
            .unwrap()
            .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
        assert!(
            Command::new(rustc)
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let output = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                cancel.store(true, Ordering::Release);
            });
            run_compiler(Command::new(executable), &cancel).unwrap()
        });
        assert!(output.cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
