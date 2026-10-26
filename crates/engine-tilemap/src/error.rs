//! Error type shared by every tilemap operation.
use std::fmt;

/// A stable machine-readable `code` plus a human-readable `message`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code such as `tilemap.bounds`.
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
/// Result alias for tilemap operations.
pub type Result<T> = std::result::Result<T, Error>;
