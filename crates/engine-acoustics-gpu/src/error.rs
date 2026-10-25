//! `acoustics.backend.*` errors (docs/spatial-audio-design.md section 5.3).

use engine_acoustics::BackendError;

/// The tier cannot be used (capability, adapter or self-test). Not fatal: the
/// caller falls back to a lower tier.
pub(crate) fn unavailable(message: impl Into<String>) -> BackendError {
    BackendError {
        code: "acoustics.backend.unavailable",
        message: message.into(),
        fatal: false,
    }
}

/// A runtime failure (validation error, device loss, non-finite or saturated
/// result). Fatal: the tier must be demoted immediately.
pub(crate) fn failed(message: impl Into<String>) -> BackendError {
    BackendError {
        code: "acoustics.backend.failed",
        message: message.into(),
        fatal: true,
    }
}

/// The readback did not complete within `readback_timeout_ms`. Not fatal on its
/// own; the service demotes after three consecutive timeouts.
pub(crate) fn timeout(message: impl Into<String>) -> BackendError {
    BackendError {
        code: "acoustics.backend.timeout",
        message: message.into(),
        fatal: false,
    }
}

/// The caller passed an invalid job, setting or world.
pub(crate) fn invalid(message: impl Into<String>) -> BackendError {
    BackendError {
        code: "acoustics.request.invalid",
        message: message.into(),
        fatal: false,
    }
}
