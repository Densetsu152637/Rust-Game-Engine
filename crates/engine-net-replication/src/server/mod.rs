//! Authoritative replication server: entity state, per-peer delta streams, interest management,
//! budgeted packet building and validated client RPCs. Sans-IO and deterministic.
mod build;
mod peer;
mod world;

use crate::NetId;
use crate::error::{ReplicationError, Result};
use crate::limits::{ReplicationLimits, ServerConfig};
use crate::packet::{
    KIND_ACK, KIND_RPC, MIN_PAYLOAD, OutPacket, RawCall, decode_args, encode_args,
    encode_rpc_packet, read_calls, read_common, rpc_packet,
};
use crate::registry::{ComponentId, PropertyId, ReplicationRegistry, RpcDirection, RpcId};
use crate::telemetry::{PeerTelemetry, RpcRejectReason, ServerTelemetry};
use crate::value::Value;
use crate::wire::{Reader, varint_len};
use engine_network::PeerId;
use peer::{PeerState, Phase};
use std::collections::VecDeque;
use std::sync::Arc;
use world::{Entity, EntityComp, PropState, World};

pub use world::EntityRelevance;

/// Result of a custom relevancy callback for one `(peer, entity)` pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Relevance {
    /// Use the entity's built-in rule (always / spatial / owner-only).
    Default,
    /// The entity is not relevant to this peer.
    Hidden,
    /// The entity is relevant with this priority scale (> 0), regardless of position.
    Visible(f32),
}

/// Read-only description of an entity handed to the relevancy callback.
pub struct EntityView<'a> {
    pub id: NetId,
    pub owner: Option<PeerId>,
    pub position: Option<[f32; 3]>,
    /// Host-defined tag (team, layer, ...) set with `EntityInit::tag`/`set_tag`.
    pub tag: u32,
    /// Center of the peer's view, if one was set.
    pub peer_position: Option<[f32; 3]>,
    pub distance: Option<f32>,
    comps: &'a [EntityComp],
}

impl EntityView<'_> {
    pub fn has_component(&self, id: ComponentId) -> bool {
        self.comps.iter().any(|c| c.id == id)
    }
}

/// Native relevancy hook. Called for every live entity and peer while installed, so keep it
/// cheap and deterministic.
pub type RelevanceFn = Box<dyn Fn(PeerId, &EntityView<'_>) -> Relevance + Send + Sync>;

/// Recipients of a server-to-client RPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcTarget {
    Peer(PeerId),
    /// The owner of the target entity.
    Owner,
    /// Every peer that currently has the target entity (spawn acknowledged).
    Relevant,
}

/// Outcome of a client-to-server call that reached the server.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerEvent {
    /// A call passed direction, rate, target, ownership and argument validation.
    RpcReceived {
        peer: PeerId,
        rpc: RpcId,
        entity: Option<NetId>,
        args: Vec<Value>,
    },
    /// A call failed validation. The peer stays connected; repeated rejections are the host's
    /// signal to throttle or kick.
    RpcRejected {
        peer: PeerId,
        rpc: Option<RpcId>,
        entity: Option<NetId>,
        reason: RpcRejectReason,
    },
}

/// Initial description of a replicated entity.
#[derive(Clone, Debug, Default)]
pub struct EntityInit {
    pub owner: Option<PeerId>,
    pub position: Option<[f32; 3]>,
    /// `None` infers `Spatial` when a position is given and `Always` otherwise.
    pub relevance: Option<EntityRelevance>,
    pub tag: u32,
    /// Components with one value per property, in registry order. The set is fixed for the
    /// entity's lifetime.
    pub components: Vec<(ComponentId, Vec<Value>)>,
}

impl EntityInit {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn owner(mut self, peer: PeerId) -> Self {
        self.owner = Some(peer);
        self
    }
    pub fn at(mut self, position: [f32; 3]) -> Self {
        self.position = Some(position);
        self
    }
    pub fn relevance(mut self, relevance: EntityRelevance) -> Self {
        self.relevance = Some(relevance);
        self
    }
    pub fn tag(mut self, tag: u32) -> Self {
        self.tag = tag;
        self
    }
    pub fn component(mut self, id: ComponentId, values: Vec<Value>) -> Self {
        self.components.push((id, values));
        self
    }
}

pub struct ReplicationServer {
    registry: Arc<ReplicationRegistry>,
    limits: ReplicationLimits,
    cfg: ServerConfig,
    fingerprint: u32,
    min_interval: Vec<u32>,
    world: World,
    peers: std::collections::BTreeMap<PeerId, PeerState>,
    callback: Option<RelevanceFn>,
    events: VecDeque<ServerEvent>,
    events_dropped: u64,
    tick: u32,
    scratch: Vec<u8>,
}

impl ReplicationServer {
    pub fn new(
        registry: Arc<ReplicationRegistry>,
        limits: ReplicationLimits,
        cfg: ServerConfig,
    ) -> Result<Self> {
        limits.validate()?;
        cfg.validate()?;
        let min_interval = registry
            .components()
            .iter()
            .map(|c| {
                c.max_update_hz.map_or(0, |hz| {
                    ((cfg.tick_rate_hz as f32 / hz).ceil() as u32).max(1)
                })
            })
            .collect();
        Ok(Self {
            fingerprint: registry.fingerprint(),
            world: World::new(limits.max_entities, cfg.grid_cell_size),
            registry,
            limits,
            cfg,
            min_interval,
            peers: Default::default(),
            callback: None,
            events: VecDeque::new(),
            events_dropped: 0,
            tick: 0,
            scratch: Vec::new(),
        })
    }

    pub fn registry(&self) -> &Arc<ReplicationRegistry> {
        &self.registry
    }
    pub fn schema_hash(&self) -> u64 {
        self.registry.schema_hash()
    }
    pub fn limits(&self) -> &ReplicationLimits {
        &self.limits
    }

    // ------------------------------------------------------------------ authoritative state

    /// Creates an entity and returns its `NetId`. Nothing is sent until a peer finds it
    /// relevant during `build`.
    pub fn spawn(&mut self, init: EntityInit) -> Result<NetId> {
        let EntityInit {
            owner,
            position,
            relevance,
            tag,
            mut components,
        } = init;
        if let Some(p) = position {
            self.check_position(p)?;
        }
        if let Some(o) = owner
            && !self.peers.contains_key(&o)
        {
            return Err(ReplicationError::UnknownPeer(o));
        }
        let mode = relevance.unwrap_or(if position.is_some() {
            EntityRelevance::Spatial
        } else {
            EntityRelevance::Always
        });
        match mode {
            EntityRelevance::Spatial if position.is_none() => {
                return Err(ReplicationError::Invalid("spatial entity needs a position"));
            }
            EntityRelevance::OwnerOnly if owner.is_none() => {
                return Err(ReplicationError::Invalid(
                    "owner-only entity needs an owner",
                ));
            }
            _ => {}
        }
        components.sort_by_key(|(id, _)| *id);
        if components.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(ReplicationError::Invalid("duplicate component on entity"));
        }
        let mut comps = Vec::with_capacity(components.len());
        let mut props: Vec<PropState> = Vec::new();
        let (mut size_owner, mut size_other) = (0usize, 0usize);
        let mut owner_conditioned = false;
        for (id, values) in &components {
            let def = self
                .registry
                .component(*id)
                .ok_or(ReplicationError::Invalid("unknown component id"))?;
            if values.len() != def.properties.len() {
                return Err(ReplicationError::Invalid(
                    "component needs one value per property",
                ));
            }
            let start = props.len();
            let mut bytes_total = varint_len(u64::from(id.0));
            for (pdef, value) in def.properties.iter().zip(values) {
                let mut bytes = Vec::new();
                pdef.spec.encode(value, &mut bytes)?;
                bytes_total += bytes.len();
                props.push(PropState { bytes, ver: 1 });
            }
            if props.len() > self.limits.max_props_per_entity {
                return Err(ReplicationError::Limit("too many properties on one entity"));
            }
            if def.condition.visible_to(true) {
                size_owner += bytes_total;
            }
            if def.condition.visible_to(false) {
                size_other += bytes_total;
            }
            owner_conditioned |= def.condition.owner_dependent();
            comps.push(EntityComp {
                id: *id,
                start,
                len: def.properties.len(),
            });
        }
        if 16 + size_owner.max(size_other) > self.limits.max_entity_bytes {
            return Err(ReplicationError::Limit(
                "entity state above max_entity_bytes",
            ));
        }
        let id = self.world.allocate()?;
        self.world.insert(Entity {
            id,
            owner,
            mode,
            position,
            tag,
            comps,
            props,
            cell: None,
            owner_conditioned,
            size_owner,
            size_other,
        });
        Ok(id)
    }

    /// Removes an entity. Peers that know it receive a despawn on their next `build`; the
    /// `NetId` index is reused only later and with a new generation.
    pub fn despawn(&mut self, id: NetId) -> Result<()> {
        self.world
            .remove(id)
            .map(|_| ())
            .ok_or(ReplicationError::UnknownEntity(id))
    }

    pub fn contains(&self, id: NetId) -> bool {
        self.world.entity(id).is_some()
    }

    pub fn entity_count(&self) -> usize {
        self.world.len()
    }

    /// Updates one property. Returns whether the quantized value changed; unchanged values
    /// cost nothing on the wire.
    pub fn set_property(
        &mut self,
        id: NetId,
        component: ComponentId,
        property: PropertyId,
        value: &Value,
    ) -> Result<bool> {
        let def = self
            .registry
            .component(component)
            .ok_or(ReplicationError::Invalid("unknown component id"))?;
        let pdef = def
            .properties
            .get(usize::from(property.0))
            .ok_or(ReplicationError::Invalid("unknown property id"))?;
        self.scratch.clear();
        pdef.spec.encode(value, &mut self.scratch)?;
        let owner_sees = def.condition.visible_to(true);
        let other_sees = def.condition.visible_to(false);
        let max = self.limits.max_entity_bytes;
        let e = self
            .world
            .entity_mut(id)
            .ok_or(ReplicationError::UnknownEntity(id))?;
        let ec = e
            .comps
            .iter()
            .find(|c| c.id == component)
            .ok_or(ReplicationError::Invalid("entity lacks this component"))?;
        let slot = ec.start + usize::from(property.0);
        let prop = &mut e.props[slot];
        if prop.bytes == self.scratch {
            return Ok(false);
        }
        let (old, new) = (prop.bytes.len(), self.scratch.len());
        let grow = |size: usize| size + new - old;
        if (owner_sees && 16 + grow(e.size_owner) > max)
            || (other_sees && 16 + grow(e.size_other) > max)
        {
            return Err(ReplicationError::Limit(
                "entity state above max_entity_bytes",
            ));
        }
        if owner_sees {
            e.size_owner = grow(e.size_owner);
        }
        if other_sees {
            e.size_other = grow(e.size_other);
        }
        let prop = &mut e.props[slot];
        prop.bytes.clear();
        prop.bytes.extend_from_slice(&self.scratch);
        prop.ver = prop.ver.wrapping_add(1).max(1);
        Ok(true)
    }

    /// Reads back the authoritative (quantized) value, which is what clients converge to.
    pub fn get_property(
        &self,
        id: NetId,
        component: ComponentId,
        property: PropertyId,
    ) -> Result<Value> {
        let def = self
            .registry
            .component(component)
            .ok_or(ReplicationError::Invalid("unknown component id"))?;
        let pdef = def
            .properties
            .get(usize::from(property.0))
            .ok_or(ReplicationError::Invalid("unknown property id"))?;
        let e = self
            .world
            .entity(id)
            .ok_or(ReplicationError::UnknownEntity(id))?;
        let ec = e
            .comps
            .iter()
            .find(|c| c.id == component)
            .ok_or(ReplicationError::Invalid("entity lacks this component"))?;
        let bytes = &e.props[ec.start + usize::from(property.0)].bytes;
        let mut r = Reader::new(bytes);
        pdef.spec.decode(&mut r)
    }

    fn check_position(&self, p: [f32; 3]) -> Result<()> {
        if p.iter()
            .all(|v| v.is_finite() && v.abs() <= self.limits.max_abs_position)
        {
            Ok(())
        } else {
            Err(ReplicationError::Invalid(
                "position must be finite and within max_abs_position",
            ))
        }
    }

    /// Moves an entity (updates the spatial index). Positions are server-side relevancy data;
    /// replicate a position component separately if clients need it.
    pub fn set_position(&mut self, id: NetId, position: [f32; 3]) -> Result<()> {
        self.check_position(position)?;
        self.world.move_to(id, position)
    }

    pub fn position(&self, id: NetId) -> Option<[f32; 3]> {
        self.world.entity(id)?.position
    }

    pub fn set_tag(&mut self, id: NetId, tag: u32) -> Result<()> {
        self.world
            .entity_mut(id)
            .ok_or(ReplicationError::UnknownEntity(id))?
            .tag = tag;
        Ok(())
    }

    /// Changes the owner. Peers whose component visibility depends on ownership receive a
    /// despawn followed by a fresh spawn; others receive a small owner update.
    pub fn set_owner(&mut self, id: NetId, owner: Option<PeerId>) -> Result<()> {
        if let Some(o) = owner
            && !self.peers.contains_key(&o)
        {
            return Err(ReplicationError::UnknownPeer(o));
        }
        let e = self
            .world
            .entity_mut(id)
            .ok_or(ReplicationError::UnknownEntity(id))?;
        if e.mode == EntityRelevance::OwnerOnly && owner.is_none() {
            return Err(ReplicationError::Invalid(
                "owner-only entity needs an owner",
            ));
        }
        e.owner = owner;
        Ok(())
    }

    pub fn owner(&self, id: NetId) -> Option<PeerId> {
        self.world.entity(id)?.owner
    }

    // ------------------------------------------------------------------------------- peers

    pub fn add_peer(&mut self, peer: PeerId) -> Result<()> {
        if self.peers.contains_key(&peer) {
            return Err(ReplicationError::Invalid("peer already added"));
        }
        if self.peers.len() >= self.limits.max_peers {
            return Err(ReplicationError::Limit("max_peers reached"));
        }
        let state = PeerState::new(peer, self.registry.rpcs().len(), |i| {
            self.registry.rpcs()[i].max_calls_per_sec.max(1) as f32
        });
        self.peers.insert(peer, state);
        Ok(())
    }

    /// Drops all replication state of a peer and clears ownership of its entities.
    pub fn remove_peer(&mut self, peer: PeerId) -> bool {
        if self.peers.remove(&peer).is_none() {
            return false;
        }
        let owned: Vec<NetId> = self
            .world
            .iter()
            .filter(|e| e.owner == Some(peer))
            .map(|e| e.id)
            .collect();
        for id in owned {
            if let Some(e) = self.world.entity_mut(id) {
                e.owner = None;
            }
        }
        true
    }

    pub fn peers(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }

    /// Sets the center and radius of the peer's interest sphere. Spatial entities outside it
    /// (plus hysteresis) are never encoded for the peer.
    pub fn set_peer_view(&mut self, peer: PeerId, position: [f32; 3], radius: f32) -> Result<()> {
        self.check_position(position)?;
        if !radius.is_finite() || radius < 0.0 || radius > self.limits.max_view_radius {
            return Err(ReplicationError::Invalid("view radius out of range"));
        }
        self.peers
            .get_mut(&peer)
            .ok_or(ReplicationError::UnknownPeer(peer))?
            .view = Some((position, radius));
        Ok(())
    }

    /// Installs (or clears) the custom relevancy callback.
    pub fn set_relevance_callback(&mut self, callback: Option<RelevanceFn>) {
        self.callback = callback;
    }

    /// True once `peer` acknowledged the spawn of `id` and has not been told to despawn it.
    pub fn is_live_for(&self, peer: PeerId, id: NetId) -> bool {
        self.peers
            .get(&peer)
            .and_then(|p| p.records.get(&id))
            .is_some_and(|r| r.phase == Phase::Live)
    }

    /// True while the server tracks any state (including pending despawns) for the pair.
    pub fn is_tracked_for(&self, peer: PeerId, id: NetId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|p| p.records.contains_key(&id))
    }

    // ------------------------------------------------------------------------ packet flow

    /// Builds this tick's packets for `peer`.
    ///
    /// `budget_bytes` caps the total payload bytes across the returned packets and `max_payload`
    /// caps each packet (use the transport's smallest payload limit, about 1100 bytes for QUIC
    /// datagrams). Both are hard limits. Higher-priority and longer-starved work goes first, so
    /// a tight budget delays low-priority updates but never starves them.
    pub fn build(
        &mut self,
        peer: PeerId,
        tick: u32,
        budget_bytes: usize,
        max_payload: usize,
    ) -> Result<Vec<OutPacket>> {
        if max_payload < MIN_PAYLOAD {
            return Err(ReplicationError::Invalid(
                "max_payload below the 64 byte minimum",
            ));
        }
        self.tick = self.tick.max(tick);
        let st = self
            .peers
            .get_mut(&peer)
            .ok_or(ReplicationError::UnknownPeer(peer))?;
        let ctx = build::Ctx {
            registry: &self.registry,
            cfg: &self.cfg,
            limits: &self.limits,
            world: &self.world,
            callback: self.callback.as_ref(),
            fingerprint: self.fingerprint,
            min_interval: &self.min_interval,
        };
        Ok(build::build_peer(
            &ctx,
            peer,
            st,
            tick,
            budget_bytes,
            max_payload,
        ))
    }

    /// Reports that the transport refused packet `sequence` for `peer` (for example
    /// `Error::QueueFull`), so its content is resent immediately instead of after a timeout.
    pub fn mark_unsent(&mut self, peer: PeerId, sequence: u32) -> bool {
        self.peers
            .get_mut(&peer)
            .is_some_and(|p| p.mark_unsent(sequence))
    }

    /// Consumes bytes received from `peer` on `Lane::REPLICATION` (acknowledgements) or
    /// `Lane::RPC` (client calls). `Err` means the bytes were not a valid packet; individual RPC
    /// calls that fail validation are reported as `ServerEvent::RpcRejected` and return `Ok`.
    pub fn receive(&mut self, peer: PeerId, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.limits.max_packet_bytes {
            return Err(ReplicationError::Limit("packet above max_packet_bytes"));
        }
        if !self.peers.contains_key(&peer) {
            return Err(ReplicationError::UnknownPeer(peer));
        }
        let mut r = Reader::new(bytes);
        match read_common(&mut r, self.fingerprint)? {
            KIND_ACK => {
                let largest = r.u32()?;
                let bits = r.u64()?;
                r.end()?;
                let (tick, cfg) = (self.tick, &self.cfg);
                let st = self
                    .peers
                    .get_mut(&peer)
                    .ok_or(ReplicationError::UnknownPeer(peer))?;
                st.on_ack(largest, bits, tick, cfg)
            }
            KIND_RPC => {
                let calls = read_calls(&mut r, &self.limits)?;
                for call in calls {
                    self.handle_call(peer, call);
                }
                Ok(())
            }
            _ => Err(ReplicationError::Malformed(
                "clients may only send acks and rpcs",
            )),
        }
    }

    fn push_event(&mut self, event: ServerEvent) {
        if self.events.len() >= self.limits.max_pending_events {
            self.events_dropped += 1;
        } else {
            self.events.push_back(event);
        }
    }

    /// Takes the accumulated RPC events.
    pub fn drain_events(&mut self) -> Vec<ServerEvent> {
        self.events.drain(..).collect()
    }

    fn handle_call(&mut self, peer: PeerId, call: RawCall<'_>) {
        let rpc_id = u16::try_from(call.rpc).ok().map(RpcId);
        match self.validate_call(peer, rpc_id, &call) {
            Ok(args) => {
                if let Some(st) = self.peers.get_mut(&peer) {
                    st.tel.rpc_received += 1;
                }
                if let Some(rpc) = rpc_id {
                    self.push_event(ServerEvent::RpcReceived {
                        peer,
                        rpc,
                        entity: call.entity,
                        args,
                    });
                }
            }
            Err(reason) => {
                if let Some(st) = self.peers.get_mut(&peer) {
                    st.tel.rpc_received += 1;
                    st.tel.rpc_rejected += 1;
                    *st.tel
                        .rpc_rejected_by_reason
                        .entry(reason.as_str())
                        .or_default() += 1;
                }
                let known = rpc_id.filter(|id| self.registry.rpc(*id).is_some());
                self.push_event(ServerEvent::RpcRejected {
                    peer,
                    rpc: known,
                    entity: call.entity,
                    reason,
                });
            }
        }
    }

    fn validate_call(
        &mut self,
        peer: PeerId,
        rpc_id: Option<RpcId>,
        call: &RawCall<'_>,
    ) -> std::result::Result<Vec<Value>, RpcRejectReason> {
        use RpcRejectReason as Why;
        let def = rpc_id
            .and_then(|id| self.registry.rpc(id))
            .ok_or(Why::UnknownRpc)?;
        if def.direction != RpcDirection::ClientToServer {
            return Err(Why::WrongDirection);
        }
        let tick = self.tick;
        let tick_rate = self.cfg.tick_rate_hz as f32;
        let st = self.peers.get_mut(&peer).ok_or(Why::UnknownRpc)?;
        // Every attempt, valid or not, consumes a token so malformed spam is throttled too.
        let bucket = &mut st.buckets[usize::from(rpc_id.map_or(0, |r| r.0))];
        let per_sec = def.max_calls_per_sec.max(1) as f32;
        let elapsed = tick.saturating_sub(bucket.last_tick) as f32;
        bucket.tokens = (bucket.tokens + elapsed * per_sec / tick_rate).min(per_sec);
        bucket.last_tick = tick;
        if bucket.tokens < 1.0 {
            return Err(Why::RateLimited);
        }
        bucket.tokens -= 1.0;
        match (def.targets_entity, call.entity) {
            (true, None) => return Err(Why::MissingTarget),
            (false, Some(_)) => return Err(Why::UnexpectedTarget),
            _ => {}
        }
        if let Some(id) = call.entity {
            let visible = st.records.get(&id).is_some_and(|r| r.phase == Phase::Live);
            let entity = self.world.entity(id).filter(|_| visible);
            let entity = entity.ok_or(Why::UnknownEntity)?;
            if def.requires_ownership && entity.owner != Some(peer) {
                return Err(Why::NotOwner);
            }
        }
        decode_args(def, call.args).map_err(|_| Why::BadArgs)
    }

    /// Sends a server-to-client RPC. Returns one `(peer, packet)` per recipient; entity-targeted
    /// calls reach only peers that have the entity, so entity IDs never leak to others.
    pub fn call_rpc(
        &mut self,
        rpc: RpcId,
        target: RpcTarget,
        entity: Option<NetId>,
        args: &[Value],
    ) -> Result<Vec<(PeerId, OutPacket)>> {
        let def = self
            .registry
            .rpc(rpc)
            .ok_or(ReplicationError::Invalid("unknown rpc id"))?
            .clone();
        if def.direction != RpcDirection::ServerToClient {
            return Err(ReplicationError::Invalid("rpc is not server-to-client"));
        }
        if def.targets_entity != entity.is_some() {
            return Err(ReplicationError::Invalid("rpc entity target mismatch"));
        }
        if let Some(id) = entity
            && self.world.entity(id).is_none()
        {
            return Err(ReplicationError::UnknownEntity(id));
        }
        let payload = encode_args(&def, args)?;
        if payload.len() > self.limits.max_rpc_payload_bytes {
            return Err(ReplicationError::Limit("rpc payload above limit"));
        }
        let recipients: Vec<PeerId> = match target {
            RpcTarget::Peer(p) => {
                if !self.peers.contains_key(&p) {
                    return Err(ReplicationError::UnknownPeer(p));
                }
                if let Some(id) = entity
                    && !self.is_live_for(p, id)
                {
                    return Err(ReplicationError::Invalid("entity is not live for the peer"));
                }
                vec![p]
            }
            RpcTarget::Owner => {
                let id = entity.ok_or(ReplicationError::Invalid("owner target needs an entity"))?;
                let owner = self
                    .owner(id)
                    .ok_or(ReplicationError::Invalid("entity has no owner"))?;
                if !self.is_live_for(owner, id) {
                    return Err(ReplicationError::Invalid(
                        "entity is not live for its owner",
                    ));
                }
                vec![owner]
            }
            RpcTarget::Relevant => {
                let id =
                    entity.ok_or(ReplicationError::Invalid("relevant target needs an entity"))?;
                self.peers
                    .keys()
                    .copied()
                    .filter(|p| self.is_live_for(*p, id))
                    .collect()
            }
        };
        let bytes = encode_rpc_packet(self.fingerprint, rpc.0, entity, &payload);
        let mut out = Vec::with_capacity(recipients.len());
        for p in recipients {
            if let Some(st) = self.peers.get_mut(&p) {
                st.tel.rpc_sent += 1;
            }
            out.push((p, rpc_packet(&def, bytes.clone())));
        }
        Ok(out)
    }

    // ---------------------------------------------------------------------------- telemetry

    pub fn peer_telemetry(&self, peer: PeerId) -> Option<&PeerTelemetry> {
        self.peers.get(&peer).map(|p| &p.tel)
    }

    pub fn telemetry(&self) -> ServerTelemetry {
        ServerTelemetry {
            tick: self.tick,
            entities: self.world.len(),
            peers: self.peers.values().map(|p| p.tel.clone()).collect(),
            events_dropped: self.events_dropped,
        }
    }
}
