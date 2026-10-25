//! Errors of ray-tracing operations.

use crate::status::RayTracingStatus;

/// A failed ray-tracing operation.
#[derive(Clone, Debug, PartialEq)]
pub enum RtError {
    /// Input rejected at the crate boundary (mesh, instance, transform, kernel
    /// parameters or a resource limit). Nothing was submitted to the GPU.
    Invalid(String),
    /// The hardware tier is not usable on this device (see the status reason).
    Unavailable(RayTracingStatus),
    /// A GPU-side failure: validation error, device loss or readback timeout.
    /// The device's status is demoted to unavailable.
    Gpu(String),
}

impl std::fmt::Display for RtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid ray-tracing input: {message}"),
            Self::Unavailable(status) => write!(f, "{status}"),
            Self::Gpu(message) => write!(f, "ray-tracing GPU failure: {message}"),
        }
    }
}

impl std::error::Error for RtError {}

pub(crate) fn invalid(message: impl Into<String>) -> RtError {
    RtError::Invalid(message.into())
}
