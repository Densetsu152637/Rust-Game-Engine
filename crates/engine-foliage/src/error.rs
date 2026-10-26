//! Foliage errors with stable codes.
use std::fmt;

/// A foliage failure with a stable `code` (for example `foliage.type.invalid`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code such as `foliage.limit.instances`.
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

/// Result alias for foliage operations.
pub type Result<T> = std::result::Result<T, Error>;
