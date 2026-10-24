//! QUIC endpoint limits and transport tuning, validated on construction.
use engine_network::{Error, Lane, Result};
use quinn_proto::{IdleTimeout, TransportConfig, VarInt, congestion};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

/// Unidirectional streams every connection keeps for itself: one control stream and one ordered
/// stream per lane. `max_concurrent_streams` adds reliable-unordered streams on top.
pub(crate) const RESERVED_STREAMS: usize = 1 + Lane::MAX as usize + 1;
/// Largest accepted `max_message_bytes`.
pub const MAX_MESSAGE_BYTES_LIMIT: usize = 16 * 1024 * 1024;

/// Congestion controller from quinn-proto. Cubic is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CongestionControl {
    #[default]
    Cubic,
    NewReno,
    Bbr,
}

/// Trusted per-endpoint resource limits. Every field is validated by the endpoint constructors.
#[derive(Clone, Debug, PartialEq)]
pub struct QuicLimits {
    /// Live connections (pending + admitted) a server keeps; further attempts are refused.
    pub max_peers: usize,
    /// Connections a server keeps in the handshake/authorization phase at once.
    pub max_pending_handshakes: usize,
    /// Absolute deadline from first packet to admission (server) or welcome (client).
    pub handshake_timeout: Duration,
    /// QUIC idle timeout negotiated with the peer (the smaller of both sides wins).
    pub idle_timeout: Duration,
    /// Keep-alive interval; must be shorter than `idle_timeout`. `None` disables keep-alives.
    pub keep_alive_interval: Option<Duration>,
    /// Largest reliable payload.
    pub max_message_bytes: usize,
    /// Per-peer bytes queued by `send` and not yet accepted by QUIC flow control; also QUIC's
    /// per-connection send window and receive window.
    pub max_queued_bytes: usize,
    /// Concurrent reliable-unordered streams per direction.
    pub max_concurrent_streams: usize,
    /// Per-peer received payload bytes per second (fixed one-second windows).
    pub max_received_bytes_per_second: usize,
    /// Per-peer received messages per second (fixed one-second windows).
    pub max_messages_per_second: usize,
    /// UDP datagrams read from the socket per poll, and packets written per connection per poll.
    pub max_datagrams_per_poll: usize,
    /// QUIC DATAGRAM send and receive buffer bytes per connection.
    pub datagram_buffer_bytes: usize,
}
impl Default for QuicLimits {
    fn default() -> Self {
        Self {
            max_peers: 16,
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(30),
            keep_alive_interval: Some(Duration::from_secs(5)),
            max_message_bytes: 256 * 1024,
            max_queued_bytes: 1024 * 1024,
            max_concurrent_streams: 32,
            max_received_bytes_per_second: 8 * 1024 * 1024,
            max_messages_per_second: 4096,
            max_datagrams_per_poll: 256,
            datagram_buffer_bytes: 64 * 1024,
        }
    }
}
impl QuicLimits {
    pub fn validate(&self) -> Result<()> {
        let keep_alive_ok = self
            .keep_alive_interval
            .is_none_or(|k| !k.is_zero() && k < self.idle_timeout);
        if self.max_peers == 0
            || self.max_pending_handshakes == 0
            || self.handshake_timeout.is_zero()
            || self.idle_timeout.is_zero()
            || IdleTimeout::try_from(self.idle_timeout).is_err()
            || !keep_alive_ok
            || self.max_message_bytes == 0
            || self.max_message_bytes > MAX_MESSAGE_BYTES_LIMIT
            || self.max_queued_bytes < self.max_message_bytes + 6
            || self.max_queued_bytes > u32::MAX as usize
            || self.max_concurrent_streams == 0
            || self.max_concurrent_streams > 1024
            || self.max_received_bytes_per_second == 0
            || self.max_messages_per_second == 0
            || self.max_datagrams_per_poll == 0
            || self.datagram_buffer_bytes < 2048
            || self.datagram_buffer_bytes > 16 * 1024 * 1024
        {
            return Err(Error::Invalid("invalid QUIC limits"));
        }
        Ok(())
    }
    pub(crate) fn transport(&self, congestion: CongestionControl) -> Arc<TransportConfig> {
        let mut transport = TransportConfig::default();
        let window = VarInt::from_u32(self.max_queued_bytes as u32);
        transport
            .max_concurrent_bidi_streams(VarInt::from_u32(0))
            .max_concurrent_uni_streams(VarInt::from_u32(
                RESERVED_STREAMS as u32 + self.max_concurrent_streams as u32,
            ))
            .stream_receive_window(window)
            .receive_window(window)
            .send_window(self.max_queued_bytes as u64)
            .max_idle_timeout(IdleTimeout::try_from(self.idle_timeout).ok())
            .keep_alive_interval(self.keep_alive_interval)
            .datagram_receive_buffer_size(Some(self.datagram_buffer_bytes))
            .datagram_send_buffer_size(self.datagram_buffer_bytes)
            .enable_segmentation_offload(false);
        match congestion {
            CongestionControl::Cubic => transport
                .congestion_controller_factory(Arc::new(congestion::CubicConfig::default())),
            CongestionControl::NewReno => transport
                .congestion_controller_factory(Arc::new(congestion::NewRenoConfig::default())),
            CongestionControl::Bbr => {
                transport.congestion_controller_factory(Arc::new(congestion::BbrConfig::default()))
            }
        };
        Arc::new(transport)
    }
}

/// Server endpoint configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct QuicServerConfig {
    pub limits: QuicLimits,
    pub congestion: CongestionControl,
    /// Answer unvalidated client addresses with a stateless Retry before allocating connection
    /// state (anti-spoofing/amplification). Enabled by default; disabling saves one round trip
    /// on connect and should be reserved for trusted networks or a validating relay.
    pub require_address_validation: bool,
}
impl Default for QuicServerConfig {
    fn default() -> Self {
        Self {
            limits: QuicLimits::default(),
            congestion: CongestionControl::default(),
            require_address_validation: true,
        }
    }
}

/// Client endpoint configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuicClientConfig {
    pub limits: QuicLimits,
    pub congestion: CongestionControl,
}
