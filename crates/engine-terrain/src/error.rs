//! Terrain errors with stable codes.
use std::fmt;

/// A terrain failure with a stable `code` (for example `terrain.asset.malformed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code such as `terrain.size`.
    pub code: &'static str,
    /// Description of the failure.
    pub message: String,
}
impl Error {
    /// Builds an error with a stable `code`.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
/// Result alias for terrain operations.
pub type Result<T> = std::result::Result<T, Error>;
