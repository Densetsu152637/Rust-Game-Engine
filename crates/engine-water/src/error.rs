//! Error type with stable codes.
use std::fmt;

/// A water model error with a stable code such as `water.waves.invalid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code.
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

/// Result with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
