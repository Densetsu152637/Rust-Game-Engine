//! Opt-in, bounded authoritative TCP replication for trusted networks.
//! No socket is opened until `Server::bind` or `Client::connect` is called.
mod endpoint;
mod framing;
mod state;

pub use endpoint::{Client, ClientConfig, ClientEvent, Server, ServerConfig, ServerEvent};
use serde::{Deserialize, Serialize};
pub use state::{EntitySnapshot, ReplicationState, Snapshot};
use std::{fmt, io, time::Duration};

pub type PeerId = u64;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Protocol(&'static str),
    Invalid(&'static str),
    QueueFull,
    Disconnected,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "network I/O: {e}"),
            Self::Protocol(e) => write!(f, "network protocol: {e}"),
            Self::Invalid(e) => write!(f, "invalid network data: {e}"),
            Self::QueueFull => f.write_str("network outbound queue is full"),
            Self::Disconnected => f.write_str("network peer disconnected"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Hard limits, checked before allocation/queue insertion. Application configuration is trusted.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_queued_bytes: usize,
    pub max_queued_frames: usize,
    pub max_peers: usize,
    pub max_entities: usize,
    pub max_id_bytes: usize,
    pub max_messages_per_poll: usize,
    pub max_bytes_per_poll: usize,
    pub max_input_tick_lead: u64,
    pub idle_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 256 * 1024,
            max_queued_bytes: 1024 * 1024,
            max_queued_frames: 32,
            max_peers: 16,
            max_entities: 1024,
            max_id_bytes: 128,
            max_messages_per_poll: 64,
            max_bytes_per_poll: 256 * 1024,
            max_input_tick_lead: 256,
            idle_timeout: Duration::from_secs(30),
        }
    }
}
impl Limits {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_frame_bytes < 64
            || self.max_frame_bytes > u32::MAX as usize
            || self.max_queued_bytes < self.max_frame_bytes.saturating_add(4)
            || self.max_queued_frames == 0
            || self.max_peers == 0
            || self.max_entities == 0
            || self.max_id_bytes == 0
            || self.max_messages_per_poll == 0
            || self.max_bytes_per_poll == 0
            || self.idle_timeout.is_zero()
        {
            return Err(Error::Invalid("invalid resource limits"));
        }
        Ok(())
    }
    pub(crate) fn valid_id(&self, id: &str) -> bool {
        !id.is_empty() && id.len() <= self.max_id_bytes && !id.chars().any(char::is_control)
    }
}

/// Per-connection read/write and message work; clamped to the configured hard limits.
/// Server acceptance is separately bounded; all zero fields perform no socket work.
#[derive(Clone, Copy, Debug)]
pub struct PollBudget {
    pub max_messages: usize,
    pub max_bytes: usize,
    pub max_accepts: usize,
}
impl Default for PollBudget {
    fn default() -> Self {
        Self {
            max_messages: 64,
            max_bytes: 256 * 1024,
            max_accepts: 16,
        }
    }
}
impl PollBudget {
    pub(crate) fn capped(self, limits: &Limits) -> Self {
        Self {
            max_messages: self.max_messages.min(limits.max_messages_per_poll),
            max_bytes: self.max_bytes.min(limits.max_bytes_per_poll),
            max_accepts: self.max_accepts.min(limits.max_peers),
        }
    }
}

/// Intent only: the authoritative simulation decides how this affects an entity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputCommand {
    pub tick: u64,
    pub entity_id: String,
    pub movement: [f32; 3],
    pub buttons: u32,
}
impl InputCommand {
    fn validate(&self, limits: &Limits) -> Result<()> {
        if !limits.valid_id(&self.entity_id)
            || self
                .movement
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1.0)
        {
            return Err(Error::Invalid(
                "input requires a valid ID and finite movement in [-1, 1]",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Message {
    Welcome { version: u32, peer_id: PeerId },
    Heartbeat,
    Input(InputCommand),
    Snapshot(Snapshot),
}
