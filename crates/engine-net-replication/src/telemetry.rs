//! Nonsecret counters safe for logs, reports and MCP output. They contain no property values,
//! entity contents or credentials.
use engine_network::PeerId;
use serde::Serialize;
use std::collections::BTreeMap;

/// Why a client-to-server RPC was refused. Soft failures never disconnect the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum RpcRejectReason {
    UnknownRpc,
    WrongDirection,
    RateLimited,
    /// The target entity does not exist or is not visible to the caller.
    UnknownEntity,
    MissingTarget,
    UnexpectedTarget,
    NotOwner,
    /// Argument count, type, length or finiteness check failed.
    BadArgs,
}

impl RpcRejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownRpc => "unknown_rpc",
            Self::WrongDirection => "wrong_direction",
            Self::RateLimited => "rate_limited",
            Self::UnknownEntity => "unknown_entity",
            Self::MissingTarget => "missing_target",
            Self::UnexpectedTarget => "unexpected_target",
            Self::NotOwner => "not_owner",
            Self::BadArgs => "bad_args",
        }
    }
}

/// Per-peer replication counters. `*_tick` fields describe the most recent `build` call.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PeerTelemetry {
    pub peer: PeerId,
    pub tick: u32,
    pub bytes_tick: usize,
    pub packets_tick: usize,
    pub bytes_total: u64,
    pub packets_total: u64,
    /// Entities relevant to the peer this tick.
    pub entities_relevant: usize,
    /// Entities with at least one block placed this tick.
    pub entities_sent: usize,
    /// Entities with pending work that did not fit the budget this tick.
    pub entities_deferred: usize,
    /// Cumulative bytes of spawn (full-state) blocks.
    pub full_state_bytes_total: u64,
    /// Cumulative bytes of update (delta) blocks.
    pub delta_bytes_total: u64,
    /// Cumulative estimate of what a protocol that resent every relevant entity in full each
    /// tick would have cost.
    pub snapshot_equivalent_bytes_total: u64,
    /// Snapshot-equivalent bytes divided by bytes actually sent (0 until anything was sent).
    pub compression_ratio: f32,
    /// Packets sent and not yet acknowledged or declared lost.
    pub unacked_packets: usize,
    /// Sequence numbers between the newest sent packet and the newest acknowledged one.
    pub ack_lag_packets: u32,
    /// Ticks since the oldest unacknowledged packet was built.
    pub oldest_unacked_age_ticks: u32,
    pub packets_acked: u64,
    pub packets_lost: u64,
    /// Blocks that could never fit `max_payload` (configuration error).
    pub oversize_blocks: u64,
    pub rpc_received: u64,
    pub rpc_rejected: u64,
    pub rpc_rejected_by_reason: BTreeMap<&'static str, u64>,
    /// Rejections counted above but not queued as `RpcRejected` because the peer already had
    /// its cap of rejection events pending.
    pub rpc_rejections_coalesced: u64,
    /// RPC packets refused with `ReplicationError::Backpressure` because the peer's reserved
    /// event share could not hold their calls.
    pub rpc_packets_refused: u64,
    pub rpc_sent: u64,
}

impl PeerTelemetry {
    pub(crate) fn refresh_ratio(&mut self) {
        self.compression_ratio = if self.bytes_total == 0 {
            0.0
        } else {
            self.snapshot_equivalent_bytes_total as f32 / self.bytes_total as f32
        };
    }
}

/// Server-wide snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServerTelemetry {
    pub tick: u32,
    pub entities: usize,
    pub peers: Vec<PeerTelemetry>,
    /// Always 0 on the server since event capacity is reserved per peer; kept for format
    /// compatibility. See `PeerTelemetry::rpc_packets_refused`.
    pub events_dropped: u64,
}

/// Client counters.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClientTelemetry {
    pub packets_received: u64,
    pub packets_rejected: u64,
    pub duplicate_packets: u64,
    pub bytes_received: u64,
    pub entities: usize,
    pub last_server_tick: u32,
    /// Updates skipped because their entity or component was unknown, stale or older than the
    /// state already applied (expected under reordering).
    pub stale_blocks: u64,
    pub rpc_received: u64,
    pub rpc_dropped: u64,
    pub events_dropped: u64,
}
