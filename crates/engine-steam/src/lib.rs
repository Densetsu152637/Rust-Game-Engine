//! Steamworks integration seam.
//!
//! [`SteamBackend`] abstracts the Steam API. [`NullBackend`] is used when Steam is
//! not compiled in, [`FakeBackend`] is a deterministic scriptable double for tests,
//! and `RealBackend` (feature `steam`) wraps `steamworks`. With the feature off the
//! crate has no Steam dependency and every call returns
//! `Unavailable(NotCompiled)`.

mod backend;
mod error;
mod fake;
mod null;
mod plugin;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real;
mod session;
mod stats;
mod types;

pub use backend::SteamBackend;
pub use error::{SteamError, SteamResult, UnavailableReason};
pub use fake::FakeBackend;
pub use null::NullBackend;
pub use plugin::{REQUIRED_EXIT_CODE, SteamPlugin, SteamRuntimeStatus, event_json, status_json};
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
pub use real::RealBackend;
pub use session::SteamSession;
pub use stats::{DEFAULT_STORE_INTERVAL_TICKS, StatsService};
pub use types::*;

/// True when this build contains the real Steam backend.
pub const STEAM_COMPILED: bool = cfg!(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
));

/// The backend a game should use by default: the real one when compiled in,
/// otherwise [`NullBackend`].
pub fn default_backend() -> Box<dyn SteamBackend + Send> {
    #[cfg(all(
        feature = "steam",
        any(windows, target_os = "linux", target_os = "macos")
    ))]
    {
        Box::new(RealBackend::new())
    }
    #[cfg(not(all(
        feature = "steam",
        any(windows, target_os = "linux", target_os = "macos")
    )))]
    {
        Box::new(NullBackend::not_compiled())
    }
}
