//! Translation of cargo output into build failures.

use super::BuildFailure;
use super::process::CompilerOutput;

pub(super) fn build_outcome(
    output: &CompilerOutput,
    diagnostics: &[serde_json::Value],
    code: &str,
) -> Result<(), BuildFailure> {
    if output.status.success() && !output.cancelled {
        return Ok(());
    }
    Err(BuildFailure {
        code: if output.cancelled {
            "compiler.cancelled"
        } else {
            code
        }
        .into(),
        message: if output.cancelled {
            "Compilation cancelled"
        } else {
            "Native Rust compilation failed"
        }
        .into(),
        diagnostics: diagnostics.to_vec(),
        stderr: output.stderr.clone(),
        exit_code: output.status.code(),
        cancelled: output.cancelled,
    })
}
