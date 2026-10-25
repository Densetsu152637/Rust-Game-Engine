//! Per-peer replication state: what the peer knows (acknowledged baselines), what is in flight
//! and the ack/loss bookkeeping that keeps the two consistent.
use crate::NetId;
use crate::error::{ReplicationError, Result};
use crate::ids::seq_newer;
use crate::limits::ServerConfig;
use crate::telemetry::PeerTelemetry;
use engine_network::PeerId;
use std::collections::BTreeMap;

/// Lifecycle of one entity from one peer's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Relevant but no spawn sent (or the last attempt was lost).
    Unsent,
    /// Spawn in flight; no deltas are sent until it is acknowledged.
    Spawning,
    /// Spawn acknowledged; property deltas flow against acknowledged baselines.
    Live,
    /// The peer must be told to despawn (irrelevant, destroyed or resync).
    Despawn,
    /// Despawn in flight; the record disappears when it is acknowledged.
    Despawning,
}

/// Baseline tracking for one property of one entity incarnation. Version 0 means "none".
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Slot {
    pub acked: u32,
    pub inflight: u32,
    pub inflight_seq: u32,
    pub last_sent_tick: u32,
}

pub(crate) struct Record {
    /// Distinguishes reuse of the same `NetId` by a later spawn so stale acks never match.
    pub inc: u32,
    pub phase: Phase,
    pub slots: Vec<Slot>,
    /// Ownership the component visibility of the current spawn was computed for.
    pub vis_owned: bool,
    pub owned_acked: Option<bool>,
    pub owned_inflight: Option<bool>,
    /// Relevance priority scale from the latest relevancy pass.
    pub scale: f32,
    /// Staleness accumulators: structural, reliable updates, unreliable updates.
    pub accum: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ItemKind {
    Spawn,
    Despawn,
    Owner(bool),
    Prop { slot: u16, ver: u32 },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Item {
    pub key: NetId,
    pub inc: u32,
    pub kind: ItemKind,
}

pub(crate) struct SentPacket {
    pub seq: u32,
    pub tick: u32,
    pub reliable: bool,
    pub items: Vec<Item>,
}

/// Token bucket for client-to-server RPC rate limiting.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bucket {
    pub tokens: f32,
    pub last_tick: u32,
}

impl Bucket {
    /// Refills at `per_sec` (capped at one second's worth) and takes one token if available.
    pub(crate) fn take(&mut self, tick: u32, tick_rate: f32, per_sec: f32) -> bool {
        let elapsed = tick.saturating_sub(self.last_tick) as f32;
        self.tokens = (self.tokens + elapsed * per_sec / tick_rate).min(per_sec);
        self.last_tick = tick;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

pub(crate) struct PeerState {
    pub view: Option<([f32; 3], f32)>,
    pub next_seq: u32,
    pub next_inc: u32,
    pub records: BTreeMap<NetId, Record>,
    pub sent: Vec<SentPacket>,
    pub largest_acked: Option<u32>,
    pub buckets: Vec<Bucket>,
    /// Shared bucket for unknown-id and wrong-direction calls.
    pub invalid_bucket: Bucket,
    /// Events of this peer queued since the last drain (reserved share accounting).
    pub pending_events: usize,
    /// The `RpcRejected` subset of `pending_events`.
    pub pending_rejections: usize,
    pub tel: PeerTelemetry,
}

impl PeerState {
    pub(crate) fn new(
        peer: PeerId,
        rpc_count: usize,
        invalid_burst: f32,
        rpc_burst: impl Fn(usize) -> f32,
    ) -> Self {
        Self {
            view: None,
            next_seq: 1,
            next_inc: 1,
            records: BTreeMap::new(),
            sent: Vec::new(),
            largest_acked: None,
            buckets: (0..rpc_count)
                .map(|i| Bucket {
                    tokens: rpc_burst(i),
                    last_tick: 0,
                })
                .collect(),
            invalid_bucket: Bucket {
                tokens: invalid_burst,
                last_tick: 0,
            },
            pending_events: 0,
            pending_rejections: 0,
            tel: PeerTelemetry {
                peer,
                ..PeerTelemetry::default()
            },
        }
    }

    pub(crate) fn new_record(&mut self, slot_count: usize, scale: f32) -> Record {
        let inc = self.next_inc;
        self.next_inc = self.next_inc.wrapping_add(1);
        Record {
            inc,
            phase: Phase::Unsent,
            slots: vec![Slot::default(); slot_count],
            vis_owned: false,
            owned_acked: None,
            owned_inflight: None,
            scale,
            accum: [0.0; 3],
        }
    }

    /// Applies a client acknowledgement: `largest` plus a bitfield of the 64 packets before it.
    pub(crate) fn on_ack(
        &mut self,
        largest: u32,
        bits: u64,
        tick: u32,
        cfg: &ServerConfig,
    ) -> Result<()> {
        if !seq_newer(self.next_seq, largest) {
            return Err(ReplicationError::Malformed("ack for a packet never sent"));
        }
        if self.largest_acked.is_none_or(|l| seq_newer(largest, l)) {
            self.largest_acked = Some(largest);
        }
        let (acked, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.sent).into_iter().partition(|p| {
                if p.seq == largest {
                    return true;
                }
                let d = largest.wrapping_sub(p.seq);
                (1..=64).contains(&d) && (bits >> (d - 1)) & 1 == 1
            });
        self.sent = rest;
        for packet in acked {
            self.tel.packets_acked += 1;
            self.apply_acked(&packet);
        }
        self.detect_loss(tick, cfg, 0);
        self.refresh_lag(tick);
        Ok(())
    }

    fn apply_acked(&mut self, packet: &SentPacket) {
        for item in &packet.items {
            let Some(rec) = self.records.get_mut(&item.key) else {
                continue;
            };
            if rec.inc != item.inc {
                continue;
            }
            match item.kind {
                ItemKind::Spawn => {
                    if matches!(rec.phase, Phase::Unsent | Phase::Spawning) {
                        rec.phase = Phase::Live;
                    }
                }
                ItemKind::Despawn => {
                    if matches!(rec.phase, Phase::Despawning | Phase::Despawn) {
                        self.records.remove(&item.key);
                    }
                }
                ItemKind::Owner(v) => {
                    if rec.phase == Phase::Live {
                        rec.owned_acked = Some(v);
                        if rec.owned_inflight == Some(v) {
                            rec.owned_inflight = None;
                        }
                    }
                }
                ItemKind::Prop { slot, ver } => {
                    if rec.phase == Phase::Live
                        && let Some(s) = rec.slots.get_mut(usize::from(slot))
                    {
                        if s.acked == 0 || seq_newer(ver, s.acked) {
                            s.acked = ver;
                        }
                        if s.inflight == ver {
                            s.inflight = 0;
                        }
                    }
                }
            }
        }
    }

    fn revert_lost(&mut self, packet: &SentPacket) {
        for item in &packet.items {
            let Some(rec) = self.records.get_mut(&item.key) else {
                continue;
            };
            if rec.inc != item.inc {
                continue;
            }
            match item.kind {
                ItemKind::Spawn => {
                    if rec.phase == Phase::Spawning {
                        rec.phase = Phase::Unsent;
                    }
                }
                ItemKind::Despawn => {
                    if rec.phase == Phase::Despawning {
                        rec.phase = Phase::Despawn;
                    }
                }
                ItemKind::Owner(v) => {
                    if rec.owned_inflight == Some(v) {
                        rec.owned_inflight = None;
                    }
                }
                ItemKind::Prop { slot, ver } => {
                    if let Some(s) = rec.slots.get_mut(usize::from(slot))
                        && s.inflight == ver
                        && s.inflight_seq == packet.seq
                    {
                        s.inflight = 0;
                    }
                }
            }
        }
    }

    /// Declares in-flight packets lost when a newer packet was acknowledged past the reorder
    /// grace, when they exceed their timeout or when too many are outstanding. Lost packets
    /// release their items so the next `build` resends current state against the baseline.
    pub(crate) fn detect_loss(&mut self, tick: u32, cfg: &ServerConfig, max_in_flight: usize) {
        let largest = self.largest_acked;
        let (lost, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.sent).into_iter().partition(|p| {
                let age = tick.saturating_sub(p.tick);
                let timeout = if p.reliable {
                    cfg.reliable_timeout_ticks
                } else {
                    cfg.ack_timeout_ticks
                };
                let skipped = !p.reliable
                    && largest.is_some_and(|l| {
                        let d = l.wrapping_sub(p.seq) as i32;
                        d >= cfg.reorder_grace as i32
                    });
                age > timeout || skipped
            });
        self.sent = rest;
        for packet in lost {
            self.tel.packets_lost += 1;
            self.revert_lost(&packet);
        }
        if max_in_flight > 0 && self.sent.len() > max_in_flight {
            self.sent.sort_by_key(|p| p.tick);
            let excess = self.sent.len() - max_in_flight;
            let dropped: Vec<_> = self.sent.drain(..excess).collect();
            for packet in dropped {
                self.tel.packets_lost += 1;
                self.revert_lost(&packet);
            }
        }
    }

    /// The host could not hand packet `seq` to the transport: release its items immediately.
    pub(crate) fn mark_unsent(&mut self, seq: u32) -> bool {
        let Some(pos) = self.sent.iter().position(|p| p.seq == seq) else {
            return false;
        };
        let packet = self.sent.swap_remove(pos);
        self.tel.packets_lost += 1;
        self.revert_lost(&packet);
        true
    }

    pub(crate) fn refresh_lag(&mut self, tick: u32) {
        self.tel.unacked_packets = self.sent.len();
        self.tel.ack_lag_packets = match self.largest_acked {
            Some(l) => self.next_seq.wrapping_sub(1).wrapping_sub(l),
            None => self.next_seq.wrapping_sub(1),
        };
        self.tel.oldest_unacked_age_ticks = self
            .sent
            .iter()
            .map(|p| tick.saturating_sub(p.tick))
            .max()
            .unwrap_or(0);
    }
}
