//! Error and availability types. No Steam call panics; failures are values.

use std::fmt;

/// Why Steam cannot be used right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The `steam` Cargo feature is off (or the platform is unsupported).
    NotCompiled,
    /// Steam support is compiled in but the Steam client is not running.
    NotRunning,
    /// Initialisation failed with the given message.
    InitFailed(String),
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCompiled => f.write_str("Steam support is not compiled in"),
            Self::NotRunning => f.write_str("the Steam client is not running"),
            Self::InitFailed(why) => write!(f, "Steam initialisation failed: {why}"),
        }
    }
}

/// Failure of a Steam operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteamError {
    /// Steam is not usable; the reason says why.
    Unavailable(UnavailableReason),
    /// The named stat, achievement, file, lobby or item does not exist.
    NotFound(String),
    /// A file, key or stat name was rejected as unsafe or malformed.
    InvalidName(String),
    /// Remote storage quota would be exceeded.
    QuotaExceeded,
    /// Steam Cloud is disabled for this app or account.
    CloudDisabled,
    /// A lobby is at its member limit.
    LobbyFull,
    /// Steam reported a failure.
    Rejected(String),
}

impl fmt::Display for SteamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "Steam unavailable: {reason}"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::InvalidName(name) => write!(f, "invalid name: {name:?}"),
            Self::QuotaExceeded => f.write_str("Steam Cloud quota exceeded"),
            Self::CloudDisabled => f.write_str("Steam Cloud is disabled"),
            Self::LobbyFull => f.write_str("lobby is full"),
            Self::Rejected(why) => write!(f, "Steam rejected the request: {why}"),
        }
    }
}

impl std::error::Error for SteamError {}

/// Result alias for Steam operations.
pub type SteamResult<T> = Result<T, SteamError>;
