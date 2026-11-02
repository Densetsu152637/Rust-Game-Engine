//! Backend-level types for Steam Networking Sockets. The transport endpoints in
//! `transport.rs` are written against these, so they run on the fake or the real backend.

use std::net::SocketAddr;

use crate::types::SteamId;

/// A listen socket owned by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetListenId(pub u32);

/// A connection owned by a backend (the backend's own handle space, not a Steam handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetConnId(pub u32);

/// Who is on the other end of a connection, as far as the networking layer knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetRemote {
    /// A SteamID authenticated by Steam (P2P connections).
    Steam(SteamId),
    /// An unauthenticated address (IP connections).
    Ip(SocketAddr),
    /// No usable identity.
    Unknown,
}

/// State of the Steam Datagram Relay network that P2P connections need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    /// `net_init_relay` has not been called (or Steam has not started trying).
    NotStarted,
    /// Initialising; P2P connections cannot be made yet.
    Pending,
    Ready,
    Failed(String),
}

/// How to send one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetSendFlags {
    pub reliable: bool,
    /// Send immediately instead of waiting for the Nagle timer.
    pub no_nagle: bool,
}

/// Nonsecret per-connection telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetConnStats {
    pub ping_ms: f32,
    /// Bytes accepted by `net_send` and not yet delivered to the remote end.
    pub queued_bytes: u64,
}

/// What a backend reports from `net_poll_events`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetEvent {
    /// A remote asked to connect to a listen socket. The connection must be accepted with
    /// `net_accept` or closed with `net_close`.
    ConnectionRequest {
        listen: NetListenId,
        conn: NetConnId,
        remote: NetRemote,
    },
    /// The connection is established (outgoing connected, or an accepted request completed).
    Connected { conn: NetConnId, remote: NetRemote },
    /// The connection ended or failed. Messages already received can still be read.
    Closed { conn: NetConnId, reason: String },
    /// Result of `net_begin_auth`, or a later revocation (`accepted` false).
    AuthResult {
        user: SteamId,
        accepted: bool,
        reason: String,
    },
}

/// The address in the text form of an IP `SteamNetworkingIdentity`
/// (`ip:1.2.3.4:27015`, `ip:[::1]:27015`, `ip:[::ffff:1.2.3.4]:27015`). Any other text,
/// including `steamid:` and `str:` identities, is `None`.
#[cfg_attr(not(feature = "steam"), allow(dead_code))]
pub(crate) fn parse_ip_identity(text: &str) -> Option<SocketAddr> {
    text.strip_prefix("ip:")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_identity_text_forms_parse() {
        assert_eq!(
            parse_ip_identity("ip:127.0.0.1:27015"),
            Some("127.0.0.1:27015".parse().unwrap())
        );
        assert_eq!(
            parse_ip_identity("ip:[::1]:27015"),
            Some("[::1]:27015".parse().unwrap())
        );
        assert_eq!(
            parse_ip_identity("ip:[::ffff:10.1.2.3]:9"),
            Some("[::ffff:10.1.2.3]:9".parse().unwrap())
        );
        for bad in [
            "",
            "ip:",
            "ip:127.0.0.1",
            "ip:not-an-address:1",
            "steamid:76561198929353896",
            "str:hello",
            "127.0.0.1:27015",
            "IP:127.0.0.1:1",
        ] {
            assert_eq!(parse_ip_identity(bad), None, "{bad:?}");
        }
    }
}
