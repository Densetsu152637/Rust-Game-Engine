//! Mapping of wgpu failures onto [`RenderError`].

use crate::context::GpuContext;
use engine_render::{RenderError, Result};

/// Fail with the sticky GPU error log, if any: device loss maps to
/// [`RenderError::DeviceLost`], validation errors to [`RenderError::Backend`].
pub(crate) fn check_gpu(gpu: &GpuContext) -> Result<()> {
    match gpu.failure() {
        None => Ok(()),
        Some((true, message)) => Err(RenderError::DeviceLost(message)),
        Some((false, message)) => Err(RenderError::Backend(message)),
    }
}

/// Prefer the recorded GPU failure (it names the root cause); otherwise wrap
/// `error` as a backend failure.
pub(crate) fn wgpu_error(gpu: &GpuContext, error: &anyhow::Error) -> RenderError {
    match check_gpu(gpu) {
        Err(recorded) => recorded,
        Ok(()) => RenderError::Backend(format!("{error:#}")),
    }
}

/// Wrap a displayable failure of a wgpu call as a backend error.
pub(crate) fn backend_error(context: &str, error: impl std::fmt::Display) -> RenderError {
    RenderError::Backend(format!("{context}: {error}"))
}
