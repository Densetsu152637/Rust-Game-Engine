//! Replication client: validates and applies server packets to a local mirror, emits events and
//! property callbacks, and produces acknowledgement and RPC packets. Sans-IO.
//!
//! Every state packet is decoded and fully validated before anything is applied, so a malformed
//! packet changes nothing and is not acknowledged. Application is order-independent: each
//! property remembers the sequence number of the packet that last wrote it and each entity slot
//! remembers its latest spawn/despawn sequence, so reordered, duplicated or late packets can
//! neither resurrect a despawned entity nor overwrite newer values.
use crate::NetId;
use crate::error::{ReplicationError, Result};
use crate::ids::{gen_newer, seq_newer};
use crate::limits::ReplicationLimits;
use crate::packet::{
    KIND_ACK, KIND_RPC, KIND_STATE, OP_DESPAWN, OP_OWNER, OP_SPAWN, OP_UPDATE, OutPacket,
    decode_args, encode_ack, encode_args, encode_rpc_packet, read_calls, read_common, rpc_packet,
};
use crate::registry::{ComponentId, PropertyId, ReplicationRegistry, RpcDirection, RpcId};
use crate::telemetry::ClientTelemetry;
use crate::value::Value;
use crate::wire::Reader;
use engine_network::{Delivery, Lane};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Replicated component values on a mirrored entity.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientComponent {
    pub id: ComponentId,
    pub values: Vec<Value>,
    seqs: Vec<u32>,
}

/// Local copy of one replicated entity.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientEntity {
    pub id: NetId,
    /// True when this client owns the entity (the server never reveals other owners).
    pub owned: bool,
    pub components: Vec<ClientComponent>,
    owner_seq: u32,
}

impl ClientEntity {
    pub fn component(&self, id: ComponentId) -> Option<&ClientComponent> {
        self.components.iter().find(|c| c.id == id)
    }
    pub fn get(&self, component: ComponentId, property: PropertyId) -> Option<&Value> {
        self.component(component)?
            .values
            .get(usize::from(property.0))
    }
}

/// One applied property change.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyChange {
    pub entity: NetId,
    pub component: ComponentId,
    pub property: PropertyId,
    pub old: Value,
    pub new: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    /// The entity entered the mirror; read its initial state with `ReplicationClient::entity`.
    Spawned {
        entity: NetId,
        owned: bool,
    },
    Despawned {
        entity: NetId,
    },
    /// A property of a live entity changed value (not emitted for the initial spawn state).
    PropertyChanged(PropertyChange),
    OwnerChanged {
        entity: NetId,
        owned: bool,
    },
    RpcReceived {
        rpc: RpcId,
        entity: Option<NetId>,
        args: Vec<Value>,
    },
}

type ChangeCallback = Box<dyn FnMut(&PropertyChange) + Send + Sync>;

enum Block {
    Spawn {
        id: NetId,
        owned: bool,
        comps: Vec<(ComponentId, Vec<Value>)>,
    },
    Update {
        id: NetId,
        comps: Vec<(ComponentId, u32, Vec<Value>)>,
    },
    Despawn {
        id: NetId,
    },
    Owner {
        id: NetId,
        owned: bool,
    },
}

struct Slot {
    generation: u16,
    /// Sequence of the newest spawn/despawn applied; also the tombstone for a dead slot.
    struct_seq: u32,
    entity: Option<ClientEntity>,
}

pub struct ReplicationClient {
    registry: Arc<ReplicationRegistry>,
    limits: ReplicationLimits,
    fingerprint: u32,
    slots: BTreeMap<u32, Slot>,
    alive: usize,
    largest: Option<u32>,
    bits: u64,
    ack_dirty: bool,
    server_tick: u32,
    events: VecDeque<ClientEvent>,
    callbacks: BTreeMap<(ComponentId, PropertyId), Vec<ChangeCallback>>,
    tel: ClientTelemetry,
}

impl ReplicationClient {
    pub fn new(registry: Arc<ReplicationRegistry>, limits: ReplicationLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            fingerprint: registry.fingerprint(),
            registry,
            limits,
            slots: BTreeMap::new(),
            alive: 0,
            largest: None,
            bits: 0,
            ack_dirty: false,
            server_tick: 0,
            events: VecDeque::new(),
            callbacks: BTreeMap::new(),
            tel: ClientTelemetry::default(),
        })
    }

    pub fn registry(&self) -> &Arc<ReplicationRegistry> {
        &self.registry
    }
    pub fn schema_hash(&self) -> u64 {
        self.registry.schema_hash()
    }

    // ------------------------------------------------------------------------------ mirror

    pub fn entity(&self, id: NetId) -> Option<&ClientEntity> {
        let slot = self.slots.get(&id.index())?;
        if slot.generation == id.generation() {
            slot.entity.as_ref()
        } else {
            None
        }
    }

    /// Live entities in ascending `NetId` index order.
    pub fn entities(&self) -> impl Iterator<Item = &ClientEntity> {
        self.slots.values().filter_map(|s| s.entity.as_ref())
    }

    pub fn entity_count(&self) -> usize {
        self.alive
    }

    /// Highest server tick seen in any accepted state packet.
    pub fn server_tick(&self) -> u32 {
        self.server_tick
    }

    pub fn telemetry(&self) -> ClientTelemetry {
        ClientTelemetry {
            entities: self.alive,
            last_server_tick: self.server_tick,
            ..self.tel.clone()
        }
    }

    // ------------------------------------------------------------------------ events/callbacks

    pub fn drain_events(&mut self) -> Vec<ClientEvent> {
        self.events.drain(..).collect()
    }

    /// Registers a native callback run for every applied change of one property, in addition to
    /// `ClientEvent::PropertyChanged`. Callbacks must not block.
    pub fn on_property_changed(
        &mut self,
        component: ComponentId,
        property: PropertyId,
        callback: impl FnMut(&PropertyChange) + Send + Sync + 'static,
    ) -> Result<()> {
        let def = self
            .registry
            .component(component)
            .ok_or(ReplicationError::Invalid("unknown component id"))?;
        if usize::from(property.0) >= def.properties.len() {
            return Err(ReplicationError::Invalid("unknown property id"));
        }
        self.callbacks
            .entry((component, property))
            .or_default()
            .push(Box::new(callback));
        Ok(())
    }

    fn push_event(&mut self, event: ClientEvent) {
        if self.events.len() >= self.limits.max_pending_events {
            self.tel.events_dropped += 1;
        } else {
            self.events.push_back(event);
        }
    }

    // ------------------------------------------------------------------------------ packets

    /// Consumes a packet from the server (either replication or RPC lane). `Err` means the
    /// bytes were invalid and nothing was applied.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<()> {
        self.tel.bytes_received += bytes.len() as u64;
        let result = self.receive_inner(bytes);
        if result.is_err() {
            self.tel.packets_rejected += 1;
        }
        result
    }

    fn receive_inner(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.limits.max_packet_bytes {
            return Err(ReplicationError::Limit("packet above max_packet_bytes"));
        }
        let mut r = Reader::new(bytes);
        match read_common(&mut r, self.fingerprint)? {
            KIND_STATE => {
                let seq = r.u32()?;
                let tick = r.u32()?;
                let blocks = self.decode_blocks(&mut r)?;
                self.tel.packets_received += 1;
                self.ack_dirty = true;
                if self.already_received(seq) {
                    self.tel.duplicate_packets += 1;
                    return Ok(());
                }
                self.mark_received(seq);
                if seq_newer(tick, self.server_tick) {
                    self.server_tick = tick;
                }
                for block in blocks {
                    self.apply(block, seq);
                }
                Ok(())
            }
            KIND_RPC => self.receive_rpc(&mut r),
            KIND_ACK => Err(ReplicationError::Malformed("servers do not send acks")),
            _ => Err(ReplicationError::Malformed("unknown packet kind")),
        }
    }

    fn already_received(&self, seq: u32) -> bool {
        let Some(largest) = self.largest else {
            return false;
        };
        if seq == largest {
            return true;
        }
        let d = largest.wrapping_sub(seq);
        (1..=64).contains(&d) && (self.bits >> (d - 1)) & 1 == 1
    }

    fn mark_received(&mut self, seq: u32) {
        let Some(largest) = self.largest else {
            self.largest = Some(seq);
            self.bits = 0;
            return;
        };
        if seq_newer(seq, largest) {
            let shift = seq.wrapping_sub(largest);
            self.bits = match shift {
                1..=63 => (self.bits << shift) | (1 << (shift - 1)),
                64 => 1 << 63,
                _ => 0,
            };
            self.largest = Some(seq);
        } else {
            let d = largest.wrapping_sub(seq);
            if (1..=64).contains(&d) {
                self.bits |= 1 << (d - 1);
            }
        }
    }

    /// Acknowledgement for everything received since the last call, or `None` when nothing new
    /// arrived. Send it on `Lane::REPLICATION`; it is idempotent and loss-tolerant.
    pub fn build_ack(&mut self) -> Option<OutPacket> {
        if !self.ack_dirty {
            return None;
        }
        self.ack_dirty = false;
        let largest = self.largest?;
        Some(OutPacket {
            lane: Lane::REPLICATION,
            delivery: Delivery::Unreliable,
            bytes: encode_ack(self.fingerprint, largest, self.bits),
            sequence: None,
        })
    }

    // ---------------------------------------------------------------------------------- decode

    fn decode_blocks(&self, r: &mut Reader<'_>) -> Result<Vec<Block>> {
        let count = r.varint()?;
        // A block is at least an op byte plus a three-byte NetId.
        if count > self.limits.max_blocks_per_packet as u64 {
            return Err(ReplicationError::Limit("too many blocks in one packet"));
        }
        if count > (r.remaining() / 4) as u64 {
            return Err(ReplicationError::Malformed(
                "block count exceeds packet size",
            ));
        }
        let mut blocks = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let op = r.u8()?;
            let id = NetId::read(r, self.limits.max_entities)?;
            blocks.push(match op {
                OP_SPAWN => {
                    let start = r.remaining();
                    let owned = decode_flag(r)?;
                    let comps = self.decode_spawn_components(r)?;
                    if start - r.remaining() > self.limits.max_entity_bytes {
                        return Err(ReplicationError::Limit(
                            "spawn state above max_entity_bytes",
                        ));
                    }
                    Block::Spawn { id, owned, comps }
                }
                OP_UPDATE => Block::Update {
                    id,
                    comps: self.decode_update_components(r)?,
                },
                OP_DESPAWN => Block::Despawn { id },
                OP_OWNER => Block::Owner {
                    id,
                    owned: decode_flag(r)?,
                },
                _ => return Err(ReplicationError::Malformed("unknown block op")),
            });
        }
        r.end()?;
        Ok(blocks)
    }

    fn component_count(&self, r: &mut Reader<'_>) -> Result<usize> {
        let n = r.varint()?;
        if n > self.registry.components().len() as u64 {
            return Err(ReplicationError::Limit(
                "more components than the registry has",
            ));
        }
        Ok(n as usize)
    }

    fn component_def(
        &self,
        r: &mut Reader<'_>,
        previous: &mut Option<ComponentId>,
    ) -> Result<(ComponentId, &crate::registry::ComponentDef)> {
        let raw = r.varint()?;
        let id = u16::try_from(raw)
            .map(ComponentId)
            .map_err(|_| ReplicationError::Malformed("component id out of range"))?;
        let def = self
            .registry
            .component(id)
            .ok_or(ReplicationError::Malformed("unknown component id"))?;
        if previous.is_some_and(|p| id <= p) {
            return Err(ReplicationError::Malformed(
                "components not strictly ascending",
            ));
        }
        *previous = Some(id);
        Ok((id, def))
    }

    fn decode_spawn_components(
        &self,
        r: &mut Reader<'_>,
    ) -> Result<Vec<(ComponentId, Vec<Value>)>> {
        let n = self.component_count(r)?;
        let mut out = Vec::with_capacity(n);
        let mut previous = None;
        let mut props = 0usize;
        for _ in 0..n {
            let (id, def) = self.component_def(r, &mut previous)?;
            props += def.properties.len();
            if props > self.limits.max_props_per_entity {
                return Err(ReplicationError::Limit("too many properties on one entity"));
            }
            let mut values = Vec::with_capacity(def.properties.len());
            for p in &def.properties {
                values.push(p.spec.decode(r)?);
            }
            out.push((id, values));
        }
        Ok(out)
    }

    fn decode_update_components(
        &self,
        r: &mut Reader<'_>,
    ) -> Result<Vec<(ComponentId, u32, Vec<Value>)>> {
        let n = self.component_count(r)?;
        if n == 0 {
            return Err(ReplicationError::Malformed("empty update block"));
        }
        let mut out = Vec::with_capacity(n);
        let mut previous = None;
        for _ in 0..n {
            let (id, def) = self.component_def(r, &mut previous)?;
            let mask = r.varint_u32()?;
            let nprops = def.properties.len();
            if mask == 0 || (nprops < 32 && mask >> nprops != 0) {
                return Err(ReplicationError::Malformed("invalid property mask"));
            }
            let mut values = Vec::with_capacity(mask.count_ones() as usize);
            for (bit, p) in def.properties.iter().enumerate() {
                if mask >> bit & 1 == 1 {
                    values.push(p.spec.decode(r)?);
                }
            }
            out.push((id, mask, values));
        }
        Ok(out)
    }

    // ----------------------------------------------------------------------------------- apply

    fn apply(&mut self, block: Block, seq: u32) {
        match block {
            Block::Spawn { id, owned, comps } => self.apply_spawn(id, owned, comps, seq),
            Block::Update { id, comps } => self.apply_update(id, comps, seq),
            Block::Despawn { id } => self.apply_despawn(id, seq),
            Block::Owner { id, owned } => self.apply_owner(id, owned, seq),
        }
    }

    fn apply_spawn(
        &mut self,
        id: NetId,
        owned: bool,
        comps: Vec<(ComponentId, Vec<Value>)>,
        seq: u32,
    ) {
        let index = id.index();
        let mut refresh = false;
        match self.slots.get_mut(&index) {
            None => {
                self.slots.insert(
                    index,
                    Slot {
                        generation: id.generation(),
                        struct_seq: seq,
                        entity: None,
                    },
                );
            }
            Some(slot) if gen_newer(id.generation(), slot.generation) => {
                let replaced = slot.entity.take().map(|e| e.id);
                slot.generation = id.generation();
                slot.struct_seq = seq;
                if let Some(old) = replaced {
                    self.alive -= 1;
                    self.push_event(ClientEvent::Despawned { entity: old });
                }
            }
            Some(slot) if slot.generation == id.generation() => {
                if !seq_newer(seq, slot.struct_seq) {
                    self.tel.stale_blocks += 1;
                    return;
                }
                slot.struct_seq = seq;
                refresh = slot.entity.is_some();
            }
            Some(_) => {
                self.tel.stale_blocks += 1;
                return;
            }
        }
        if refresh {
            self.refresh_entity(id, owned, comps, seq);
            return;
        }
        if self.alive >= self.limits.max_entities {
            self.tel.stale_blocks += 1;
            return;
        }
        let components = comps
            .into_iter()
            .map(|(cid, values)| ClientComponent {
                id: cid,
                seqs: vec![seq; values.len()],
                values,
            })
            .collect();
        if let Some(slot) = self.slots.get_mut(&index) {
            slot.entity = Some(ClientEntity {
                id,
                owned,
                components,
                owner_seq: seq,
            });
            self.alive += 1;
            self.push_event(ClientEvent::Spawned { entity: id, owned });
        }
    }

    /// A newer spawn for an entity that is already mirrored: reapply its full state.
    fn refresh_entity(
        &mut self,
        id: NetId,
        owned: bool,
        comps: Vec<(ComponentId, Vec<Value>)>,
        seq: u32,
    ) {
        for (cid, values) in comps {
            for (bit, value) in values.into_iter().enumerate() {
                self.write_property(id, cid, bit, value, seq);
            }
        }
        self.apply_owner(id, owned, seq);
    }

    fn apply_update(&mut self, id: NetId, comps: Vec<(ComponentId, u32, Vec<Value>)>, seq: u32) {
        if self.entity(id).is_none() {
            self.tel.stale_blocks += 1;
            return;
        }
        for (cid, mask, values) in comps {
            let mut values = values.into_iter();
            for bit in 0..32usize {
                if mask >> bit & 1 == 1
                    && let Some(value) = values.next()
                {
                    self.write_property(id, cid, bit, value, seq);
                }
            }
        }
    }

    /// Writes one property if `seq` is newer than the packet that last wrote it.
    fn write_property(&mut self, id: NetId, cid: ComponentId, bit: usize, value: Value, seq: u32) {
        let Some(slot) = self.slots.get_mut(&id.index()) else {
            return;
        };
        let Some(entity) = slot
            .entity
            .as_mut()
            .filter(|e| e.id.generation() == id.generation())
        else {
            return;
        };
        let Some(comp) = entity.components.iter_mut().find(|c| c.id == cid) else {
            self.tel.stale_blocks += 1;
            return;
        };
        if bit >= comp.values.len() || !seq_newer(seq, comp.seqs[bit]) {
            self.tel.stale_blocks += 1;
            return;
        }
        comp.seqs[bit] = seq;
        if comp.values[bit] == value {
            return;
        }
        let old = std::mem::replace(&mut comp.values[bit], value.clone());
        let change = PropertyChange {
            entity: id,
            component: cid,
            property: PropertyId(bit as u8),
            old,
            new: value,
        };
        if let Some(list) = self.callbacks.get_mut(&(cid, change.property)) {
            for callback in list {
                callback(&change);
            }
        }
        self.push_event(ClientEvent::PropertyChanged(change));
    }

    fn apply_despawn(&mut self, id: NetId, seq: u32) {
        let index = id.index();
        match self.slots.get_mut(&index) {
            None => {
                self.slots.insert(
                    index,
                    Slot {
                        generation: id.generation(),
                        struct_seq: seq,
                        entity: None,
                    },
                );
            }
            Some(slot) => {
                let newer_gen = gen_newer(id.generation(), slot.generation);
                if !newer_gen
                    && (slot.generation != id.generation() || !seq_newer(seq, slot.struct_seq))
                {
                    self.tel.stale_blocks += 1;
                    return;
                }
                slot.generation = id.generation();
                slot.struct_seq = seq;
                if let Some(old) = slot.entity.take() {
                    self.alive -= 1;
                    self.push_event(ClientEvent::Despawned { entity: old.id });
                }
            }
        }
    }

    fn apply_owner(&mut self, id: NetId, owned: bool, seq: u32) {
        let Some(slot) = self.slots.get_mut(&id.index()) else {
            self.tel.stale_blocks += 1;
            return;
        };
        let Some(entity) = slot
            .entity
            .as_mut()
            .filter(|e| e.id.generation() == id.generation())
        else {
            self.tel.stale_blocks += 1;
            return;
        };
        if !seq_newer(seq, entity.owner_seq) && seq != entity.owner_seq {
            self.tel.stale_blocks += 1;
            return;
        }
        entity.owner_seq = seq;
        if entity.owned != owned {
            entity.owned = owned;
            self.push_event(ClientEvent::OwnerChanged { entity: id, owned });
        }
    }

    // ------------------------------------------------------------------------------------ rpc

    fn receive_rpc(&mut self, r: &mut Reader<'_>) -> Result<()> {
        let calls = read_calls(r, &self.limits)?;
        for call in calls {
            self.tel.rpc_received += 1;
            let accepted = u16::try_from(call.rpc)
                .ok()
                .map(RpcId)
                .and_then(|id| self.registry.rpc(id).map(|def| (id, def)))
                .filter(|(_, def)| def.direction == RpcDirection::ServerToClient)
                .filter(|(_, def)| def.targets_entity == call.entity.is_some())
                .filter(|_| call.entity.is_none_or(|e| self.entity(e).is_some()))
                .and_then(|(id, def)| decode_args(def, call.args).ok().map(|a| (id, a)));
            match accepted {
                Some((rpc, args)) => self.push_event(ClientEvent::RpcReceived {
                    rpc,
                    entity: call.entity,
                    args,
                }),
                None => self.tel.rpc_dropped += 1,
            }
        }
        Ok(())
    }

    /// Builds a client-to-server RPC packet. The server validates it again (direction, rate,
    /// visibility, ownership, arguments); a rejected call is reported only on the server.
    pub fn call_server(
        &self,
        rpc: RpcId,
        entity: Option<NetId>,
        args: &[Value],
    ) -> Result<OutPacket> {
        let def = self
            .registry
            .rpc(rpc)
            .ok_or(ReplicationError::Invalid("unknown rpc id"))?;
        if def.direction != RpcDirection::ClientToServer {
            return Err(ReplicationError::Invalid("rpc is not client-to-server"));
        }
        if def.targets_entity != entity.is_some() {
            return Err(ReplicationError::Invalid("rpc entity target mismatch"));
        }
        if let Some(id) = entity
            && self.entity(id).is_none()
        {
            return Err(ReplicationError::UnknownEntity(id));
        }
        let payload = encode_args(def, args)?;
        if payload.len() > self.limits.max_rpc_payload_bytes {
            return Err(ReplicationError::Limit("rpc payload above limit"));
        }
        let bytes = encode_rpc_packet(self.fingerprint, rpc.0, entity, &payload);
        Ok(rpc_packet(def, bytes))
    }
}

fn decode_flag(r: &mut Reader<'_>) -> Result<bool> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ReplicationError::Malformed("flag out of range")),
    }
}
