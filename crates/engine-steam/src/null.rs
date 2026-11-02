//! Backend used when Steam is not available.

use crate::backend::SteamBackend;
use crate::types::SteamStatus;

/// Reports a fixed unavailable status; every call returns `Unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NullBackend {
    status: SteamStatus,
}

impl NullBackend {
    /// A backend for a build without the `steam` feature.
    pub fn not_compiled() -> Self {
        Self {
            status: SteamStatus::NotCompiled,
        }
    }

    /// A backend with an explicit unavailable status.
    ///
    /// A `Ready` status is replaced by `NotCompiled` so the backend stays unavailable.
    pub fn with_status(status: SteamStatus) -> Self {
        if status.is_ready() {
            Self::not_compiled()
        } else {
            Self { status }
        }
    }
}

impl Default for NullBackend {
    fn default() -> Self {
        Self::not_compiled()
    }
}

impl SteamBackend for NullBackend {
    fn status(&self) -> SteamStatus {
        self.status.clone()
    }
}
