//! Plain data types shared by every backend.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::UnavailableReason;

/// Steam application id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AppId(pub u32);

/// 64-bit SteamID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SteamId(pub u64);

/// The signed-in Steam user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamUser {
    pub id: SteamId,
    pub name: String,
}

/// Availability of Steam to the running game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteamStatus {
    /// Built without the `steam` feature.
    NotCompiled,
    /// Compiled in, but no Steam client is running.
    NotRunning,
    /// The client is running but initialisation failed.
    InitFailed(String),
    /// Steam is initialised.
    Ready { app_id: AppId, user: SteamUser },
}

impl SteamStatus {
    /// True when Steam calls can succeed.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// The unavailability reason, or `None` when ready.
    pub fn unavailable_reason(&self) -> Option<UnavailableReason> {
        match self {
            Self::NotCompiled => Some(UnavailableReason::NotCompiled),
            Self::NotRunning => Some(UnavailableReason::NotRunning),
            Self::InitFailed(why) => Some(UnavailableReason::InitFailed(why.clone())),
            Self::Ready { .. } => None,
        }
    }
}

/// An auth session ticket for identity proof to a peer or server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthTicket {
    pub handle: u32,
    pub data: Vec<u8>,
}

/// A stat value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatValue {
    Int(i32),
    Float(f32),
}

/// Lobby identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LobbyId(pub u64);

/// Lobby visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyVisibility {
    Private,
    FriendsOnly,
    Public,
    Invisible,
}

/// Filters for a lobby search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LobbyFilter {
    /// Metadata keys that must equal the given values.
    pub string_equals: Vec<(String, String)>,
    /// Maximum results; `0` means the backend default.
    pub max_results: u32,
    /// Only lobbies with at least this many free slots (`0` ignores the slot count).
    pub open_slots: u32,
}

/// Snapshot of a lobby.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyInfo {
    pub id: LobbyId,
    pub owner: SteamId,
    pub members: Vec<SteamId>,
    pub max_members: u32,
    pub data: BTreeMap<String, String>,
}

/// Workshop item identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkshopItemId(pub u64);

/// A Workshop item installed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledItem {
    pub id: WorkshopItemId,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Install time in Unix seconds, as Steam reports it.
    pub timestamp: u32,
    /// Steam has a newer version waiting to be downloaded.
    pub needs_update: bool,
}

/// Steam Input controller handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControllerId(pub u64);

/// A physical control Steam Input maps an action to (a Steam `EInputActionOrigin` value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionOrigin(pub u32);

/// Whether an action is a button-like (digital) or stick/trigger-like (analog) action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputActionKind {
    Digital,
    Analog,
}

/// State of a digital action on one controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DigitalActionState {
    pub down: bool,
    /// False when the action is not part of the controller's active action set.
    pub active: bool,
}

/// State of an analog action on one controller. Steam reports `y` positive up/forward.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AnalogActionState {
    pub x: f32,
    pub y: f32,
    pub active: bool,
}

/// Display data for an action origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionGlyph {
    pub origin: ActionOrigin,
    /// Localised origin name such as "Button A".
    pub name: String,
    /// Path to a glyph image supplied by Steam (empty when Steam has none).
    pub glyph_path: String,
}

/// Events drained from Steam once per tick.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SteamEvent {
    StatusChanged(SteamStatus),
    ShutdownRequested,
    OverlayToggled(bool),
    /// A user's persona (name, avatar, state) changed; the signed-in user only.
    PersonaChanged(SteamId),
    /// Connection to the Steam servers was gained (`true`) or lost (`false`).
    ConnectionChanged {
        connected: bool,
    },
    AchievementUnlocked(String),
    /// The signed-in user stats and achievements arrived from Steam.
    StatsReceived,
    StatsStored,
    CloudConflict {
        name: String,
    },
    /// A Steam Cloud sync of a save slot failed (the local save is unaffected).
    CloudSyncFailed {
        slot: String,
        message: String,
    },
    LobbyCreated(LobbyId),
    LobbyJoined(LobbyId),
    LobbyMemberJoined {
        lobby: LobbyId,
        user: SteamId,
    },
    LobbyMemberLeft {
        lobby: LobbyId,
        user: SteamId,
    },
    WorkshopItemInstalled(WorkshopItemId),
    ControllerConnected(ControllerId),
    ControllerDisconnected(ControllerId),
}
