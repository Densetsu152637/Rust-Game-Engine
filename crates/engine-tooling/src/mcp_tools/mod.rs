//! Execution tools for the MCP server (`engine mcp <root> --allow-exec`).
//!
//! `engine-mcp` cannot depend on the native compiler, so these tools live here
//! and are registered through [`engine_mcp::ToolProvider`]. They reuse the real
//! pipeline: [`NativeCompiler`] builds a unique generation (a failed build never
//! touches earlier generations), and runs execute the generation as a child
//! process through [`RunningGeneration`], exactly like `engine run`.
//!
//! Project code runs with the host user's privileges. Every call is bounded by a
//! timeout, output is size-capped, runs use a throw-away user-data directory and
//! only confined project files are passed to the child.

mod build;
mod exec;
mod run;
mod screenshot;
mod support;
#[cfg(test)]
mod tests;

use build::BuildTool;
use run::RunTool;
use screenshot::ScreenshotTool;

use crate::compiler::BuildArtifact;
use engine_mcp::{Tool, ToolProvider};
use std::sync::{Arc, Mutex};

pub(super) const DEFAULT_BUILD_TIMEOUT: u64 = 600;
pub(super) const MAX_BUILD_TIMEOUT: u64 = 1800;
pub(super) const DEFAULT_RUN_TIMEOUT: u64 = 120;
pub(super) const MAX_RUN_TIMEOUT: u64 = 600;
pub(super) const MAX_TICKS: u64 = 100_000;
pub(super) const MAX_SCREENSHOT_TICKS: u64 = 10_000;
pub(super) const MAX_INPUTS: usize = 400;
pub(super) const MAX_STDOUT_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const MAX_DIAGNOSTICS: usize = 30;
pub(super) const MAX_ENTITY_BYTES: usize = 300_000;
pub(super) const MAX_LOGS: usize = 200;
pub(super) const CLIP: usize = 2000;
/// Head-room for the JSON envelope around an image content block.
pub(super) const IMAGE_OVERHEAD: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct State {
    /// The newest generation built by this server, reusable by later runs.
    pub(super) last_build: Mutex<Option<BuildArtifact>>,
}

/// Provides `project_build`, `project_run_headless` and `render_screenshot`.
/// Register it only for servers started with exec access.
#[derive(Clone, Default)]
pub struct ExecTools {
    state: Arc<State>,
}

impl ExecTools {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolProvider for ExecTools {
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(BuildTool(self.state.clone())),
            Box::new(RunTool(self.state.clone())),
            Box::new(ScreenshotTool(self.state.clone())),
        ]
    }
}
