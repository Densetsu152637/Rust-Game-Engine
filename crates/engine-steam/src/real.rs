//! Real Steamworks backend. Stage S1 provides only the stub; S2 fills it in.

use crate::backend::SteamBackend;
use crate::types::SteamStatus;

/// Backend over `steamworks::Client` (not yet implemented).
pub struct RealBackend {
    // Filled in by stage S2; binds the steamworks type to this crate now.
    #[allow(dead_code)]
    client: Option<steamworks::Client>,
}

impl RealBackend {
    /// Creates the stub backend, which reports `InitFailed` until S2 lands.
    pub fn new() -> Self {
        Self { client: None }
    }
}

impl Default for RealBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SteamBackend for RealBackend {
    fn status(&self) -> SteamStatus {
        SteamStatus::InitFailed("the real Steam backend is not implemented yet".into())
    }
}
