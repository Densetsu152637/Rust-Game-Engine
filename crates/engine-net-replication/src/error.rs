use engine_network::PeerId;
use std::fmt;

use crate::NetId;

pub type Result<T> = std::result::Result<T, ReplicationError>;

/// Error returned by replication APIs.
///
/// `Malformed`, `Limit` and `SchemaMismatch` describe untrusted peer bytes: the caller may count
/// them and disconnect a repeat offender. `Invalid`, `Registry` and the `Unknown*` variants are
/// caller-side misuse of the native API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplicationError {
    /// Packet bytes violate the wire format (truncated, bad count, bad UTF-8, non-finite float...).
    Malformed(&'static str),
    /// A decoded count or length exceeds the configured `ReplicationLimits`.
    Limit(&'static str),
    /// The peer's schema fingerprint differs from the local registry.
    SchemaMismatch,
    /// Native API misuse (type mismatch, out-of-range value, wrong direction...).
    Invalid(&'static str),
    /// Invalid registry definition.
    Registry(String),
    UnknownPeer(PeerId),
    UnknownEntity(NetId),
}

impl fmt::Display for ReplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "malformed replication data: {m}"),
            Self::Limit(m) => write!(f, "replication limit exceeded: {m}"),
            Self::SchemaMismatch => f.write_str("replication schema mismatch"),
            Self::Invalid(m) => write!(f, "invalid replication request: {m}"),
            Self::Registry(m) => write!(f, "invalid replication registry: {m}"),
            Self::UnknownPeer(p) => write!(f, "unknown replication peer {p}"),
            Self::UnknownEntity(e) => write!(f, "unknown replication entity {e}"),
        }
    }
}
impl std::error::Error for ReplicationError {}
