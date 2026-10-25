//! Mutually authenticated QUIC endpoints for the engine's [`MessageEndpoint`] contract.
//!
//! Built on the sans-IO `quinn-proto` state machine with rustls (ring provider). Endpoints are
//! driven synchronously inside `poll` over a pluggable nonblocking [`DatagramSocket`]: no async
//! runtime, threads or sleeping. Delivery mapping: reliable-ordered payloads use one ordered
//! stream per lane, reliable-unordered payloads one short stream each, and unreliable(-sequenced)
//! payloads QUIC DATAGRAM frames. See `docs/networking-transport.md`.
//!
//! [`MessageEndpoint`]: engine_network::MessageEndpoint
mod client;
mod conditioned;
mod config;
mod link;
mod security;
mod server;
mod socket;

pub use client::QuicClient;
pub use conditioned::{
    ConditionStats, ConditionedSocket, DEFAULT_MAX_DELAYED_BYTES, DEFAULT_MAX_DELAYED_DATAGRAMS,
};
pub use config::{
    CongestionControl, MAX_MESSAGE_BYTES_LIMIT, QuicClientConfig, QuicLimits, QuicServerConfig,
};
pub use link::QUIC_PROTOCOL_VERSION;
pub use security::{QUIC_ALPN, QuicClientSecurity, QuicServerSecurity};
pub use server::QuicServer;
pub use socket::DatagramSocket;

#[cfg(test)]
mod tests;
