use std::fmt;

/// Stable error code plus a human-readable message. Codes are `ml.<area>.<reason>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlError {
    pub code: &'static str,
    pub message: String,
}
impl MlError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl fmt::Display for MlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for MlError {}

pub type Result<T> = std::result::Result<T, MlError>;
