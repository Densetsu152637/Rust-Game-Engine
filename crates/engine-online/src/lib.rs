//! Self-hostable online services for the engine: certificate-bound authentication tokens,
//! a lobby / session browser, rule-based matchmaking and host migration, carried on
//! `Lane::SESSION` of any [`engine_network::MessageEndpoint`].
//!
//! Layering (see `docs/online-services.md`):
//! - sans-IO cores: [`token`], [`gate`], [`lobby`], [`matchmaking`], [`migration`] and the
//!   combined [`service::OnlineService`] take explicit time and return data;
//! - thin drivers: [`server::OnlineServer`] and [`client::OnlineClient`] pump an endpoint;
//! - the reference server `engine-online-server` and its JSON [`config`].
//!
//! Nothing here opens a socket except the reference server binary, and no third-party hosted
//! service is involved.
pub mod binding;
pub mod client;
pub mod config;
pub mod gate;
mod limiter;
pub mod limits;
pub mod lobby;
pub mod matchmaking;
pub mod migration;
pub mod pem;
pub mod protocol;
pub mod server;
pub mod service;
mod text;
pub mod token;

pub use binding::CertificateBinding;
pub use client::{ClientEvent, OnlineClient};
pub use gate::{GateConfig, GateError, Identity, Now, SessionGate};
pub use limits::OnlineLimits;
pub use lobby::{Lobby, LobbyError};
pub use matchmaking::{Matchmaker, ModeRules};
pub use migration::{InMemoryMigrationState, MigrationState, elect_host};
pub use protocol::{ClientBody, ClientEnvelope, ServerBody, ServerEnvelope};
pub use server::OnlineServer;
pub use service::{
    Action, InMemoryDirectory, OnlineService, PlayerDirectory, ServiceConfig, ServiceStats,
};
pub use token::{
    CertFingerprint, Claims, Scope, TokenError, TokenIssuer, TokenVerifier, TrustedKey,
    VerifierConfig, fingerprint,
};

use std::fmt;

/// Crate error. Messages never contain keys, tokens or file contents.
#[derive(Debug)]
pub enum OnlineError {
    Invalid(&'static str),
    Config(String),
    Token(TokenError),
    Network(engine_network::Error),
    Protocol(protocol::ProtocolError),
    Io(std::io::Error),
}
impl fmt::Display for OnlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "invalid: {why}"),
            Self::Config(why) => write!(f, "configuration: {why}"),
            Self::Token(e) => write!(f, "token: {e}"),
            Self::Network(e) => write!(f, "{e}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Io(e) => write!(f, "I/O: {e}"),
        }
    }
}
impl std::error::Error for OnlineError {}
impl From<TokenError> for OnlineError {
    fn from(value: TokenError) -> Self {
        Self::Token(value)
    }
}
impl From<engine_network::Error> for OnlineError {
    fn from(value: engine_network::Error) -> Self {
        Self::Network(value)
    }
}
impl From<protocol::ProtocolError> for OnlineError {
    fn from(value: protocol::ProtocolError) -> Self {
        Self::Protocol(value)
    }
}
impl From<std::io::Error> for OnlineError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
