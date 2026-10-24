/// Failures carry a stable `acoustics.*` code and a human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcousticsError {
    pub code: &'static str,
    pub message: String,
}
impl AcousticsError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for AcousticsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AcousticsError {}
pub type Result<T> = std::result::Result<T, AcousticsError>;

/// The structured error returned by every not-yet-implemented operation.
pub fn unimplemented(what: &str) -> AcousticsError {
    AcousticsError::new(
        "acoustics.unimplemented",
        format!("{what} is not implemented yet"),
    )
}
