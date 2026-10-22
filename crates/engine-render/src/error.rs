use crate::resources::ResourceId;
use std::fmt;

/// Failure reported by the adapter layer or by a backend behind it.
///
/// Variants separate caller mistakes (invalid data, stale handles, ordering
/// misuse) from backend failures so callers can decide whether retrying or
/// recreating the device could help.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderError {
    /// The backend cannot provide a requested feature.
    Unsupported { feature: String, reason: String },
    /// Resource data failed validation before reaching the backend.
    InvalidResource(String),
    /// A draw or frame description is inconsistent with the resources it names.
    InvalidDraw(String),
    /// The handle was destroyed, never created by this device, or of the wrong kind.
    StaleHandle(ResourceId),
    /// Two draws in one command buffer carried the same stable order key.
    DuplicateDrawKey { producer: u64, draw: u64 },
    /// Serial and keyed recording were mixed inside one command buffer.
    MixedDrawOrdering,
    /// WGSL parse/validation/translation diagnostics.
    Shader(String),
    /// The backend or device is unavailable (no loader, no adapter, no device).
    Unavailable(String),
    /// A backend operation failed at runtime.
    Backend(String),
    /// The device was lost; the device and backend must be recreated.
    DeviceLost(String),
    /// A call needs `initialize` first or follows `shutdown`.
    NotReady(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { feature, reason } => {
                write!(f, "unsupported feature {feature}: {reason}")
            }
            Self::InvalidResource(message) => write!(f, "invalid resource: {message}"),
            Self::InvalidDraw(message) => write!(f, "invalid draw: {message}"),
            Self::StaleHandle(id) => write!(f, "stale or unknown resource handle {id}"),
            Self::DuplicateDrawKey { producer, draw } => {
                write!(
                    f,
                    "duplicate draw order key (producer {producer}, draw {draw})"
                )
            }
            Self::MixedDrawOrdering => {
                f.write_str("serial and keyed draw ordering cannot be mixed in one frame")
            }
            // Diagnostics are shown verbatim so tooling output stays stable.
            Self::Shader(message) => f.write_str(message),
            Self::Unavailable(message) => write!(f, "graphics backend unavailable: {message}"),
            Self::Backend(message) => write!(f, "graphics backend failure: {message}"),
            Self::DeviceLost(message) => write!(f, "graphics device lost: {message}"),
            Self::NotReady(message) => write!(f, "render device not ready: {message}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Result alias used throughout the adapter layer.
pub type Result<T, E = RenderError> = std::result::Result<T, E>;
