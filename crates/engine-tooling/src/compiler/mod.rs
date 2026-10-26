//! Compile external Rust gameplay together with the engine into unique native
//! executable generations. No Rust ABI crosses a dynamically loaded boundary.

mod cargo;
mod diagnostics;
mod fingerprint;
mod generation;
mod native;
mod process;
mod workspace;

pub use generation::{GenerationExit, GenerationManager, RunningGeneration};
pub use native::NativeCompiler;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

static GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Every field except `target_dir` affects generation identity. `main_source` is
/// the host's real native entry point and may invoke unrestricted public engine/game
/// APIs. `target_dir` only shares Cargo's dependency output between caches; it
/// defaults to `<cache_root>/target`.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    pub engine_root: PathBuf,
    pub project_manifest: PathBuf,
    pub cache_root: PathBuf,
    pub main_source: String,
    pub features: Vec<String>,
    pub target: Option<String>,
    pub release: bool,
    pub offline: bool,
    pub target_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub fingerprint: String,
    pub generation: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub diagnostics: Vec<serde_json::Value>,
}

/// Result of compile-only validation. No executable or generation is retained.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub fingerprint: String,
    pub diagnostics: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildFailure {
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<serde_json::Value>,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
}

impl std::fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for BuildFailure {}

fn failure(code: &str, message: impl ToString) -> BuildFailure {
    BuildFailure {
        code: code.to_owned(),
        message: message.to_string(),
        diagnostics: Vec::new(),
        stderr: String::new(),
        exit_code: None,
        cancelled: false,
    }
}

fn io_error(error: std::io::Error) -> BuildFailure {
    failure("compiler.io", error)
}

fn cancelled() -> BuildFailure {
    let mut result = failure("compiler.cancelled", "Compilation cancelled");
    result.cancelled = true;
    result
}

#[cfg(test)]
pub(crate) mod test_support;
