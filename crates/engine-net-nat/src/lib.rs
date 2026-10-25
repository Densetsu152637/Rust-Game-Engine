//! NAT traversal and relaying for the engine's QUIC transport (`engine-net-quic`).
//!
//! - Reflexive discovery: [`ReflexiveClient`] learns the NAT mapping from a discovery
//!   server ([`RelayServer`] answers binding requests).
//! - Candidates: [`CandidateSet`] is a bounded, validated blob exchanged out of band.
//! - Hole punching: [`PunchSession`] probes the peer's candidates with HMAC-SHA256
//!   authenticated probes keyed by a [`PunchSecret`] and selects the best working pair.
//! - [`DemuxSocket`]/[`NatChannel`] split one UDP socket between QUIC and this layer so a
//!   host keeps punching for new peers on its live QUIC socket.
//! - Relay: [`RelayServer`]/[`RelayService`] (also the `engine-relay-server` binary) relays
//!   between exactly two authorized parties per channel; [`RelaySocket`] lets
//!   `QuicClient`/`QuicServer` run over an allocation unchanged.
//!
//! Every core is sans-IO (explicit `now`, no threads) and every queue is bounded. See
//! `docs/online-nat-relay.md` for formats, NAT types, fallback and the threat model.
mod agent;
mod candidate;
mod credential;
mod demux;
mod error;
mod limiter;
mod punch;
mod reflexive;
mod relay_client;
mod relay_server;
mod server_config;
mod wire;

pub use agent::{ControlAgent, Transmit, drive_agents};
pub use candidate::{Candidate, CandidateKind, CandidateSet};
pub use credential::{HmacCredentials, StaticCredentials};
pub use demux::{DemuxConfig, DemuxSocket, DemuxStats, NatChannel};
pub use error::{Error, Result};
pub use punch::{
    PunchConfig, PunchOutcome, PunchPath, PunchRole, PunchSecret, PunchSession, PunchStats,
};
pub use reflexive::{ReflexiveClient, ReflexiveConfig, ReflexiveOutcome};
pub use relay_client::{
    AllocationId, AllocationStatus, RelayFailure, RelaySocket, RelaySocketConfig, RelaySocketStats,
};
pub use relay_server::{
    Authorizer, ChannelId, Grant, Party, RelayLimits, RelayRefusal, RelayServer, RelayService,
    RelayStats,
};
pub use server_config::{RelayAuthConfig, RelayServerConfig, StaticCredentialConfig};
pub use wire::{DATA_HEADER_BYTES, MAGIC, MAX_CREDENTIAL_BYTES, is_control_datagram};

#[cfg(test)]
mod tests;
