//! Per-peer packet construction: relevancy, priority selection and budgeted packing.
use super::peer::{Item, ItemKind, PeerState, Phase, Record, Slot};
use super::world::{Entity, EntityRelevance, World};
use super::{EntityView, Relevance, RelevanceFn};
use crate::ids::NetId;
use crate::limits::{ReplicationLimits, ServerConfig};
use crate::packet::{
    KIND_STATE, OP_DESPAWN, OP_OWNER, OP_SPAWN, OP_UPDATE, OutPacket, STATE_FIXED_HEADER,
    put_common,
};
use crate::registry::{Condition, ReplicationRegistry, StateDelivery};
use crate::wire::{put_varint, varint_len};
use engine_network::{Delivery, Lane, PeerId};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Ctx<'a> {
    pub registry: &'a ReplicationRegistry,
    pub cfg: &'a ServerConfig,
    pub limits: &'a ReplicationLimits,
    pub world: &'a World,
    pub callback: Option<&'a RelevanceFn>,
    pub fingerprint: u32,
    /// Per component: minimum ticks between updates of one property for one peer.
    pub min_interval: &'a [u32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Reliable,
    Unreliable,
}

impl Class {
    fn of(delivery: StateDelivery) -> Self {
        match delivery {
            StateDelivery::Reliable => Self::Reliable,
            StateDelivery::Unreliable => Self::Unreliable,
        }
    }
    fn accum_index(self) -> usize {
        match self {
            Self::Reliable => 1,
            Self::Unreliable => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CandKind {
    Despawn,
    Owner(bool),
    Spawn,
    Update(Class),
}

struct Cand {
    key: NetId,
    kind: CandKind,
    rank: u8,
    accum: f32,
}

// ---------------------------------------------------------------------------------------------
// Relevancy

fn spatial_relevance(
    ctx: &Ctx<'_>,
    view: Option<([f32; 3], f32)>,
    e: &Entity,
    already_visible: bool,
) -> Option<f32> {
    let (center, radius) = view?;
    let p = e.position?;
    let d = distance(center, p);
    let limit = if already_visible {
        radius * (1.0 + ctx.cfg.relevance_hysteresis)
    } else {
        radius
    };
    if d > limit {
        return None;
    }
    let t = if radius > 0.0 {
        (d / radius).min(1.0)
    } else {
        1.0
    };
    Some(1.0 - (1.0 - ctx.cfg.min_relevance_scale) * t)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Entities relevant to `peer` with their priority scale. Nothing outside this set is ever
/// encoded for the peer.
fn compute_relevance(ctx: &Ctx<'_>, peer: PeerId, st: &PeerState) -> BTreeMap<NetId, f32> {
    let w = ctx.world;
    let mut candidates: Vec<u32> = Vec::new();
    if ctx.callback.is_some() {
        candidates.extend(w.iter().map(|e| e.id.index()));
    } else {
        candidates.extend(w.always.iter());
        candidates.extend(w.owner_only.iter());
        if let Some((center, radius)) = st.view {
            w.grid.query(
                center,
                radius * (1.0 + ctx.cfg.relevance_hysteresis),
                &mut candidates,
            );
        }
    }
    let mut out = BTreeMap::new();
    for index in candidates {
        let Some(e) = w.by_index(index) else {
            continue;
        };
        let already_visible = st
            .records
            .get(&e.id)
            .is_some_and(|r| matches!(r.phase, Phase::Unsent | Phase::Spawning | Phase::Live));
        let builtin = match e.mode {
            EntityRelevance::Always => Some(1.0),
            EntityRelevance::OwnerOnly => (e.owner == Some(peer)).then_some(1.0),
            EntityRelevance::Spatial => spatial_relevance(ctx, st.view, e, already_visible),
        };
        let decision = match ctx.callback {
            None => builtin,
            Some(callback) => {
                let view = EntityView {
                    id: e.id,
                    owner: e.owner,
                    position: e.position,
                    tag: e.tag,
                    peer_position: st.view.map(|v| v.0),
                    distance: st.view.zip(e.position).map(|(v, p)| distance(v.0, p)),
                    comps: &e.comps,
                };
                match callback(peer, &view) {
                    Relevance::Default => builtin,
                    Relevance::Hidden => None,
                    Relevance::Visible(scale) if scale.is_finite() && scale > 0.0 => Some(scale),
                    Relevance::Visible(_) => None,
                }
            }
        };
        if let Some(mut scale) = decision {
            if e.owner == Some(peer) {
                scale *= ctx.cfg.owner_priority_scale;
            }
            out.insert(e.id, scale);
        }
    }
    out
}

/// Aligns the peer's records with the relevant set: new entities get `Unsent` records, entities
/// that left relevance (or were destroyed, or changed ownership visibility) start despawning and
/// their baselines are dropped when the despawn is acknowledged.
fn sync_records(ctx: &Ctx<'_>, peer: PeerId, st: &mut PeerState, relevant: &BTreeMap<NetId, f32>) {
    let keys: Vec<NetId> = st.records.keys().copied().collect();
    for key in keys {
        let Some(rec) = st.records.get_mut(&key) else {
            continue;
        };
        let entity = ctx.world.entity(key);
        match (relevant.get(&key), entity) {
            (Some(scale), Some(e)) => {
                rec.scale = *scale;
                let owned = e.owner == Some(peer);
                if matches!(rec.phase, Phase::Spawning | Phase::Live)
                    && e.owner_conditioned
                    && owned != rec.vis_owned
                {
                    rec.phase = Phase::Despawn;
                }
            }
            _ => match rec.phase {
                Phase::Unsent => {
                    st.records.remove(&key);
                }
                Phase::Spawning | Phase::Live => rec.phase = Phase::Despawn,
                Phase::Despawn | Phase::Despawning => {}
            },
        }
    }
    for (key, scale) in relevant {
        if st.records.contains_key(key) {
            continue;
        }
        if st.records.len() >= ctx.limits.max_entities_per_peer {
            break;
        }
        let Some(e) = ctx.world.entity(*key) else {
            continue;
        };
        let rec = st.new_record(e.props.len(), *scale);
        st.records.insert(*key, rec);
    }
}

// ---------------------------------------------------------------------------------------------
// Block encoding

/// Bitmask of properties of component `ec` that must be sent to this peer now.
fn eligible_mask(
    ctx: &Ctx<'_>,
    e: &Entity,
    rec: &Record,
    ec: &super::world::EntityComp,
    tick: u32,
) -> (u32, f32, Class) {
    let def = &ctx.registry.components()[usize::from(ec.id.0)];
    let class = Class::of(def.delivery);
    if def.condition == Condition::InitialOnly || !def.condition.visible_to(rec.vis_owned) {
        return (0, 0.0, class);
    }
    let interval = ctx.min_interval[usize::from(ec.id.0)];
    let mut mask = 0u32;
    for p in 0..ec.len {
        let slot: &Slot = &rec.slots[ec.start + p];
        let ver = e.props[ec.start + p].ver;
        if ver != slot.acked
            && ver != slot.inflight
            && (interval == 0 || tick.wrapping_sub(slot.last_sent_tick) >= interval)
        {
            mask |= 1 << p;
        }
    }
    (mask, def.priority, class)
}

/// Highest component priority among pending changes of `class`, if any.
fn pending_priority(
    ctx: &Ctx<'_>,
    e: &Entity,
    rec: &Record,
    class: Class,
    tick: u32,
) -> Option<f32> {
    let mut best: Option<f32> = None;
    for ec in &e.comps {
        let (mask, priority, c) = eligible_mask(ctx, e, rec, ec, tick);
        if mask != 0 && c == class {
            best = Some(best.map_or(priority, |b: f32| b.max(priority)));
        }
    }
    best
}

fn encode_spawn(
    ctx: &Ctx<'_>,
    e: &Entity,
    owned: bool,
    block: &mut Vec<u8>,
    kinds: &mut Vec<ItemKind>,
) {
    block.push(OP_SPAWN);
    e.id.put(block);
    block.push(u8::from(owned));
    let visible: Vec<_> = e
        .comps
        .iter()
        .filter(|ec| {
            ctx.registry.components()[usize::from(ec.id.0)]
                .condition
                .visible_to(owned)
        })
        .collect();
    put_varint(block, visible.len() as u64);
    kinds.push(ItemKind::Spawn);
    kinds.push(ItemKind::Owner(owned));
    for ec in visible {
        put_varint(block, u64::from(ec.id.0));
        for p in 0..ec.len {
            let prop = &e.props[ec.start + p];
            block.extend_from_slice(&prop.bytes);
            kinds.push(ItemKind::Prop {
                slot: (ec.start + p) as u16,
                ver: prop.ver,
            });
        }
    }
}

/// Returns false when nothing of `class` is pending.
fn encode_update(
    ctx: &Ctx<'_>,
    e: &Entity,
    rec: &Record,
    class: Class,
    tick: u32,
    block: &mut Vec<u8>,
    kinds: &mut Vec<ItemKind>,
) -> bool {
    let pending: Vec<_> = e
        .comps
        .iter()
        .filter_map(|ec| {
            let (mask, _, c) = eligible_mask(ctx, e, rec, ec, tick);
            (mask != 0 && c == class).then_some((ec, mask))
        })
        .collect();
    if pending.is_empty() {
        return false;
    }
    block.push(OP_UPDATE);
    e.id.put(block);
    put_varint(block, pending.len() as u64);
    for (ec, mask) in pending {
        put_varint(block, u64::from(ec.id.0));
        put_varint(block, u64::from(mask));
        for p in 0..ec.len {
            if mask >> p & 1 == 1 {
                let prop = &e.props[ec.start + p];
                block.extend_from_slice(&prop.bytes);
                kinds.push(ItemKind::Prop {
                    slot: (ec.start + p) as u16,
                    ver: prop.ver,
                });
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Packing

struct Builder {
    seq: u32,
    body: Vec<u8>,
    count: usize,
    items: Vec<Item>,
}

impl Builder {
    fn total(count: usize, body: usize) -> usize {
        STATE_FIXED_HEADER + varint_len(count as u64) + body
    }
}

struct Finished {
    class: Class,
    builder: Builder,
}

/// Fills at most two open packets (reliable and unreliable class) within a byte budget.
struct Packer<'a> {
    st_next_seq: &'a mut u32,
    max_payload: usize,
    max_blocks: usize,
    remaining: usize,
    open: [Option<Builder>; 2],
    done: Vec<Finished>,
}

impl Packer<'_> {
    fn class_slot(class: Class) -> usize {
        match class {
            Class::Reliable => 0,
            Class::Unreliable => 1,
        }
    }

    fn alloc_seq(&mut self) -> u32 {
        let seq = *self.st_next_seq;
        *self.st_next_seq = seq.wrapping_add(1);
        seq
    }

    /// Adds a block if it fits both the packet size and the remaining budget; returns the
    /// sequence number of the packet that carries it.
    fn place(
        &mut self,
        class: Class,
        block: &[u8],
        key: NetId,
        inc: u32,
        kinds: &[ItemKind],
    ) -> Option<u32> {
        let s = Self::class_slot(class);
        if let Some(b) = &self.open[s] {
            let before = Builder::total(b.count, b.body.len());
            let after = Builder::total(b.count + 1, b.body.len() + block.len());
            if after <= self.max_payload
                && after - before <= self.remaining
                && b.count < self.max_blocks
            {
                self.remaining -= after - before;
                let b = self.open[s].as_mut()?;
                b.body.extend_from_slice(block);
                b.count += 1;
                b.items
                    .extend(kinds.iter().map(|k| Item { key, inc, kind: *k }));
                return Some(b.seq);
            }
        }
        let fresh = Builder::total(1, block.len());
        if fresh > self.max_payload || fresh > self.remaining {
            return None;
        }
        if let Some(old) = self.open[s].take() {
            self.done.push(Finished {
                class,
                builder: old,
            });
        }
        let seq = self.alloc_seq();
        self.remaining -= fresh;
        self.open[s] = Some(Builder {
            seq,
            body: block.to_vec(),
            count: 1,
            items: kinds.iter().map(|k| Item { key, inc, kind: *k }).collect(),
        });
        Some(seq)
    }

    fn finish(mut self) -> Vec<Finished> {
        let classes = [Class::Reliable, Class::Unreliable];
        for (s, class) in classes.into_iter().enumerate() {
            if let Some(b) = self.open[s].take() {
                self.done.push(Finished { class, builder: b });
            }
        }
        self.done.sort_by_key(|f| f.builder.seq);
        self.done
    }
}

fn would_fit_alone(max_payload: usize, block: &[u8]) -> bool {
    Builder::total(1, block.len()) <= max_payload
}

#[derive(Clone, Copy)]
enum Commit {
    Despawn,
    Owner(bool),
    Spawn(bool),
    Update(Class),
}

fn stamp_inflight(rec: &mut Record, seq: u32, tick: u32, kinds: &[ItemKind]) {
    for kind in kinds {
        if let ItemKind::Prop { slot, ver } = kind {
            let s = &mut rec.slots[usize::from(*slot)];
            s.inflight = *ver;
            s.inflight_seq = seq;
            s.last_sent_tick = tick;
        }
    }
}

/// Moves a record forward after its block was placed in packet `seq`.
fn apply_commit(rec: &mut Record, seq: u32, tick: u32, commit: Commit, kinds: &[ItemKind]) {
    match commit {
        Commit::Spawn(owned) => {
            rec.phase = Phase::Spawning;
            rec.vis_owned = owned;
            rec.slots.iter_mut().for_each(|s| *s = Slot::default());
            rec.owned_acked = None;
            rec.owned_inflight = Some(owned);
            rec.accum[0] = 0.0;
            stamp_inflight(rec, seq, tick, kinds);
        }
        Commit::Despawn => rec.phase = Phase::Despawning,
        Commit::Owner(v) => rec.owned_inflight = Some(v),
        Commit::Update(class) => {
            rec.accum[class.accum_index()] = 0.0;
            stamp_inflight(rec, seq, tick, kinds);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry point

pub(crate) fn build_peer(
    ctx: &Ctx<'_>,
    peer: PeerId,
    st: &mut PeerState,
    tick: u32,
    budget_bytes: usize,
    max_payload: usize,
) -> Vec<OutPacket> {
    st.detect_loss(tick, ctx.cfg, ctx.limits.max_in_flight_packets);
    let relevant = compute_relevance(ctx, peer, st);
    sync_records(ctx, peer, st, &relevant);

    let mut tel_equivalent = 0usize;
    let mut cands: Vec<Cand> = Vec::new();
    for (key, rec) in st.records.iter_mut() {
        let entity = ctx.world.entity(*key);
        match (rec.phase, entity) {
            (Phase::Despawn, _) => cands.push(Cand {
                key: *key,
                kind: CandKind::Despawn,
                rank: 0,
                accum: f32::MAX,
            }),
            (Phase::Unsent, Some(e)) => {
                rec.accum[0] += ctx.cfg.spawn_priority * rec.scale;
                cands.push(Cand {
                    key: *key,
                    kind: CandKind::Spawn,
                    rank: 1,
                    accum: rec.accum[0],
                });
                tel_equivalent += e.spawn_block_estimate(e.owner == Some(peer));
            }
            (Phase::Spawning, Some(e)) => {
                tel_equivalent += e.spawn_block_estimate(e.owner == Some(peer));
            }
            (Phase::Live, Some(e)) => {
                let desired = e.owner == Some(peer);
                tel_equivalent += e.spawn_block_estimate(desired);
                if rec.owned_acked != Some(desired) && rec.owned_inflight != Some(desired) {
                    cands.push(Cand {
                        key: *key,
                        kind: CandKind::Owner(desired),
                        rank: 0,
                        accum: f32::MAX,
                    });
                }
                for class in [Class::Reliable, Class::Unreliable] {
                    let i = class.accum_index();
                    match pending_priority(ctx, e, rec, class, tick) {
                        Some(priority) => {
                            rec.accum[i] += priority * rec.scale;
                            cands.push(Cand {
                                key: *key,
                                kind: CandKind::Update(class),
                                rank: 1,
                                accum: rec.accum[i],
                            });
                        }
                        None => rec.accum[i] = 0.0,
                    }
                }
            }
            _ => {}
        }
    }
    cands.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then(b.accum.total_cmp(&a.accum))
            .then(a.key.cmp(&b.key))
    });

    let mut packer = Packer {
        st_next_seq: &mut st.next_seq,
        max_payload,
        max_blocks: ctx.limits.max_blocks_per_packet,
        remaining: budget_bytes,
        open: [None, None],
        done: Vec::new(),
    };
    let mut sent: BTreeSet<NetId> = BTreeSet::new();
    let mut deferred: BTreeSet<NetId> = BTreeSet::new();
    let mut spawn_bytes = 0u64;
    let mut delta_bytes = 0u64;
    let mut oversize = 0u64;
    let mut block: Vec<u8> = Vec::new();
    let mut kinds: Vec<ItemKind> = Vec::new();

    for cand in &cands {
        let Some(rec) = st.records.get(&cand.key) else {
            continue;
        };
        let inc = rec.inc;
        block.clear();
        kinds.clear();
        let class = match cand.kind {
            CandKind::Update(c) => c,
            _ => Class::Reliable,
        };
        let commit = match cand.kind {
            CandKind::Despawn => {
                block.push(OP_DESPAWN);
                cand.key.put(&mut block);
                kinds.push(ItemKind::Despawn);
                Commit::Despawn
            }
            CandKind::Owner(v) => {
                block.push(OP_OWNER);
                cand.key.put(&mut block);
                block.push(u8::from(v));
                kinds.push(ItemKind::Owner(v));
                Commit::Owner(v)
            }
            CandKind::Spawn => {
                let Some(e) = ctx.world.entity(cand.key) else {
                    continue;
                };
                let owned = e.owner == Some(peer);
                encode_spawn(ctx, e, owned, &mut block, &mut kinds);
                Commit::Spawn(owned)
            }
            CandKind::Update(class) => {
                let Some(e) = ctx.world.entity(cand.key) else {
                    continue;
                };
                if !encode_update(ctx, e, rec, class, tick, &mut block, &mut kinds) {
                    continue;
                }
                Commit::Update(class)
            }
        };
        if !would_fit_alone(max_payload, &block) {
            oversize += 1;
            deferred.insert(cand.key);
            continue;
        }
        match packer.place(class, &block, cand.key, inc, &kinds) {
            Some(seq) => {
                sent.insert(cand.key);
                match commit {
                    Commit::Spawn(_) => spawn_bytes += block.len() as u64,
                    Commit::Update(_) => delta_bytes += block.len() as u64,
                    _ => {}
                }
                if let Some(rec) = st.records.get_mut(&cand.key) {
                    apply_commit(rec, seq, tick, commit, &kinds);
                }
            }
            None => {
                deferred.insert(cand.key);
            }
        }
    }
    let finished = packer.finish();
    let mut out = Vec::with_capacity(finished.len());
    let mut bytes_tick = 0usize;
    for f in finished {
        let mut bytes = Vec::with_capacity(Builder::total(f.builder.count, f.builder.body.len()));
        put_common(&mut bytes, KIND_STATE, ctx.fingerprint);
        bytes.extend_from_slice(&f.builder.seq.to_le_bytes());
        bytes.extend_from_slice(&tick.to_le_bytes());
        put_varint(&mut bytes, f.builder.count as u64);
        bytes.extend_from_slice(&f.builder.body);
        bytes_tick += bytes.len();
        let reliable = f.class == Class::Reliable;
        st.sent.push(super::peer::SentPacket {
            seq: f.builder.seq,
            tick,
            reliable,
            items: f.builder.items,
        });
        out.push(OutPacket {
            lane: Lane::REPLICATION,
            delivery: if reliable {
                Delivery::ReliableOrdered
            } else {
                Delivery::Unreliable
            },
            bytes,
            sequence: Some(f.builder.seq),
        });
    }

    let t = &mut st.tel;
    t.tick = tick;
    t.bytes_tick = bytes_tick;
    t.packets_tick = out.len();
    t.bytes_total += bytes_tick as u64;
    t.packets_total += out.len() as u64;
    t.entities_relevant = relevant.len();
    t.entities_sent = sent.len();
    t.entities_deferred = deferred.difference(&sent).count();
    t.full_state_bytes_total += spawn_bytes;
    t.delta_bytes_total += delta_bytes;
    t.snapshot_equivalent_bytes_total += tel_equivalent as u64;
    t.oversize_blocks += oversize;
    t.refresh_ratio();
    st.refresh_lag(tick);
    out
}
