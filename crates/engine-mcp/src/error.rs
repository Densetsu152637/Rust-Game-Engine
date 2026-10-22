//! Transport-level errors. Tool failures are not errors of the server: they are
//! reported to the client as structured results (see [`crate::ToolFailure`]).

use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidRoot,
    InvalidFraming,
    /// The server configuration is inconsistent (for example two tools with the
    /// same name).
    InvalidConfig(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::InvalidRoot => f.write_str("MCP project root is not an accessible directory"),
            Self::InvalidFraming => f.write_str("oversized or unterminated JSON-RPC message"),
            Self::InvalidConfig(message) => {
                write!(f, "invalid MCP server configuration: {message}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
