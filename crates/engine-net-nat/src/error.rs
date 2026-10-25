use std::{fmt, io};

/// Errors from constructors and configuration. Malformed or unauthenticated datagrams are
/// never errors: they are dropped and counted in the relevant statistics.
#[derive(Debug)]
pub enum Error {
    /// Invalid configuration or argument.
    Invalid(&'static str),
    Io(io::Error),
    /// The system random number generator failed.
    Random,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "invalid NAT traversal input: {e}"),
            Self::Io(e) => write!(f, "NAT traversal I/O: {e}"),
            Self::Random => f.write_str("system random number generator failed"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
