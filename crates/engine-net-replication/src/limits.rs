use crate::error::{ReplicationError, Result};
use serde::Serialize;

/// Resource caps checked before allocation. Server and client must use the same
/// `max_entities`; application configuration is trusted, packet contents are not.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplicationLimits {
    /// Live entities on the server and in a client mirror (also bounds `NetId` indices).
    pub max_entities: usize,
    /// Entities tracked per peer (relevant plus pending despawns).
    pub max_entities_per_peer: usize,
    pub max_peers: usize,
    /// Largest encoded spawn block of one entity. Choose `max_payload` above this plus ~32.
    pub max_entity_bytes: usize,
    /// Properties across all components of one entity.
    pub max_props_per_entity: usize,
    /// Blocks accepted in one state packet.
    pub max_blocks_per_packet: usize,
    /// Largest packet any `receive` accepts.
    pub max_packet_bytes: usize,
    pub max_rpcs_per_packet: usize,
    /// Encoded argument bytes of one RPC call.
    pub max_rpc_payload_bytes: usize,
    /// Unacknowledged packets tracked per peer; older ones are treated as lost.
    pub max_in_flight_packets: usize,
    /// Buffered events/callback-less outputs before newer ones are dropped and counted.
    pub max_pending_events: usize,
    /// Largest accepted peer view radius.
    pub max_view_radius: f32,
    /// Largest accepted absolute coordinate for entity and view positions.
    pub max_abs_position: f32,
}

impl Default for ReplicationLimits {
    fn default() -> Self {
        Self {
            max_entities: 16_384,
            max_entities_per_peer: 16_384,
            max_peers: 64,
            max_entity_bytes: 900,
            max_props_per_entity: 256,
            max_blocks_per_packet: 2_048,
            max_packet_bytes: 64 * 1024,
            max_rpcs_per_packet: 16,
            max_rpc_payload_bytes: 1_024,
            max_in_flight_packets: 256,
            max_pending_events: 4_096,
            max_view_radius: 1.0e6,
            max_abs_position: 1.0e7,
        }
    }
}

impl ReplicationLimits {
    pub fn validate(&self) -> Result<()> {
        let ok = self.max_entities > 0
            && self.max_entities <= (1 << 24)
            && self.max_entities_per_peer > 0
            && self.max_peers > 0
            && (64..=65_536).contains(&self.max_entity_bytes)
            && (1..=1024).contains(&self.max_props_per_entity)
            && self.max_blocks_per_packet > 0
            && self.max_packet_bytes >= 64
            && self.max_rpcs_per_packet > 0
            && self.max_rpc_payload_bytes > 0
            && self.max_in_flight_packets >= 8
            && self.max_pending_events > 0
            && self.max_view_radius.is_finite()
            && self.max_view_radius > 0.0
            && self.max_abs_position.is_finite()
            && self.max_abs_position > 0.0;
        if ok {
            Ok(())
        } else {
            Err(ReplicationError::Invalid("invalid replication limits"))
        }
    }
}

/// Server behaviour tuning (all values trusted native configuration).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServerConfig {
    /// Simulation ticks per second; converts `max_update_hz` and RPC rates to ticks.
    pub tick_rate_hz: u32,
    /// Spatial grid cell edge; about the typical view radius divided by 2..4 works well.
    pub grid_cell_size: f32,
    /// Fraction of the view radius an already-visible entity may exceed before it despawns.
    pub relevance_hysteresis: f32,
    /// Ticks after which an unacknowledged unreliable-class packet is declared lost.
    pub ack_timeout_ticks: u32,
    /// Ticks after which an unacknowledged reliable-class packet is resent.
    pub reliable_timeout_ticks: u32,
    /// An unacknowledged unreliable packet is lost once a packet this many sequence numbers
    /// newer was acknowledged.
    pub reorder_grace: u32,
    /// Priority weight added per tick while an entity spawn is pending.
    pub spawn_priority: f32,
    /// Multiplier applied to the relevance scale of entities the peer owns.
    pub owner_priority_scale: f32,
    /// Smallest spatial relevance scale (at the view radius edge).
    pub min_relevance_scale: f32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            tick_rate_hz: 60,
            grid_cell_size: 32.0,
            relevance_hysteresis: 0.1,
            ack_timeout_ticks: 30,
            reliable_timeout_ticks: 600,
            reorder_grace: 3,
            spawn_priority: 4.0,
            owner_priority_scale: 2.0,
            min_relevance_scale: 0.1,
        }
    }
}

impl ServerConfig {
    pub fn validate(&self) -> Result<()> {
        let ok = (1..=1000).contains(&self.tick_rate_hz)
            && self.grid_cell_size.is_finite()
            && self.grid_cell_size >= 0.01
            && self.relevance_hysteresis.is_finite()
            && (0.0..=10.0).contains(&self.relevance_hysteresis)
            && self.ack_timeout_ticks > 0
            && self.reliable_timeout_ticks > 0
            && self.reorder_grace > 0
            && self.spawn_priority.is_finite()
            && self.spawn_priority > 0.0
            && self.owner_priority_scale.is_finite()
            && self.owner_priority_scale > 0.0
            && self.min_relevance_scale.is_finite()
            && self.min_relevance_scale > 0.0
            && self.min_relevance_scale <= 1.0;
        if ok {
            Ok(())
        } else {
            Err(ReplicationError::Invalid(
                "invalid replication server config",
            ))
        }
    }
}

/// Converts a bytes/second budget into whole per-tick budgets without losing fractions.
#[derive(Clone, Debug)]
pub struct BandwidthBudget {
    bytes_per_sec: u64,
    tick_rate_hz: u64,
    carry: u64,
}

impl BandwidthBudget {
    pub fn new(bytes_per_sec: u64, tick_rate_hz: u32) -> Self {
        Self {
            bytes_per_sec,
            tick_rate_hz: u64::from(tick_rate_hz.max(1)),
            carry: 0,
        }
    }
    /// Budget in bytes for the next tick; the sum over one second equals `bytes_per_sec`.
    pub fn next_tick(&mut self) -> usize {
        let total = self.bytes_per_sec + self.carry;
        self.carry = total % self.tick_rate_hz;
        (total / self.tick_rate_hz) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        ReplicationLimits::default().validate().unwrap();
        ServerConfig::default().validate().unwrap();
        let bad = ReplicationLimits {
            max_entities: 0,
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn budget_conserves_bytes_per_second() {
        let mut b = BandwidthBudget::new(1000, 60);
        let total: usize = (0..60).map(|_| b.next_tick()).sum();
        assert_eq!(total, 1000);
    }
}
