//! Steamworks integration seam.
//!
//! [`SteamBackend`] abstracts the Steam API. [`NullBackend`] is used when Steam is
//! not compiled in, [`FakeBackend`] is a deterministic scriptable double for tests,
//! and `RealBackend` (feature `steam`) wraps `steamworks`. With the feature off the
//! crate has no Steam dependency and every call returns
//! `Unavailable(NotCompiled)`.

mod backend;
mod cloud;
mod error;
mod fake;
mod fake_lobby;
mod fake_net;
mod fake_ugc;
mod input;
mod lobby;
mod net;
mod null;
#[cfg(feature = "online")]
mod online;
mod plugin;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real_input;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real_lobby;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real_net;
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
mod real_ugc;
mod session;
mod stats;
mod transport;
mod types;
mod wire;
mod workshop;

pub use backend::SteamBackend;
pub use cloud::{CloudReport, CloudSync, SyncAction};
pub use error::{SteamError, SteamResult, UnavailableReason};
pub use fake::FakeBackend;
pub use input::{STEAM_INPUT_DEVICE_ID, SteamInputLayout, SteamInputSource};
pub use lobby::{
    ConnectIdentity, EnterError, HOST_KEY, LobbyBackend, LobbyEvent, LobbyRaw, LobbyView,
    MAX_LOBBY_MEMBERS, PORT_KEY, SteamLobbies, listed_identity,
};
pub use net::*;
pub use null::NullBackend;
#[cfg(feature = "online")]
pub use online::OnlineAdapter;
pub use plugin::{REQUIRED_EXIT_CODE, SteamPlugin, SteamRuntimeStatus, event_json, status_json};
#[cfg(all(
    feature = "steam",
    any(windows, target_os = "linux", target_os = "macos")
))]
pub use real::RealBackend;
pub use session::SteamSession;
pub use stats::{DEFAULT_STORE_INTERVAL_TICKS, MAX_PENDING_CHANGES, StatsService};
pub use transport::{
    AuthPolicy, SteamClientConfig, SteamClientEndpoint, SteamLimits, SteamListen,
    SteamServerConfig, SteamServerEndpoint,
};
pub use types::*;
pub use workshop::{
    ItemState, ItemUpdate, ItemVisibility, MAX_QUERY_IDS, PublicQuery, PublicSort, QueryPage,
    RequestId, UgcEvent, UgcQuery, UgcRaw, UgcService, UpdateProgress, UpdateStage, WorkshopItem,
};

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
