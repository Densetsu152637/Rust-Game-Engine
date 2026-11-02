//! Generic hooks a native host runs around a game run. A service is created by the
//! embedding binary (for example the Steam integration) and driven by the desktop
//! runner; the hook carries no platform-specific types.

use crate::Runtime;
use std::fmt;

/// Failure that ends the process before the game starts. `code` is the process
/// exit code the launcher should use; `0` means "exit quietly, nothing is wrong"
/// (for example the platform is relaunching the game).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostServiceExit {
    pub code: i32,
    pub message: String,
}

impl fmt::Display for HostServiceExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostServiceExit {}

/// A service run by the desktop runner on the tick thread, in registration order.
pub trait HostService {
    /// Short name for diagnostics.
    fn name(&self) -> &str;

    /// Runs once before any window, GPU device or audio device exists and before the
    /// runtime starts. Returning an error ends the run with that error.
    fn before_graphics(&mut self, _runtime: &mut Runtime) -> Result<(), HostServiceExit> {
        Ok(())
    }

    /// Runs immediately before every fixed `Runtime::tick`, exactly once per tick.
    fn before_tick(&mut self, _runtime: &mut Runtime, _tick: u64) {}

    /// Runs once at the end of a run, after the runtime stopped and the GPU is idle
    /// (in reverse registration order).
    fn after_gpu_idle(&mut self) {}
}
