//! Delta mode: `engine-net-replication` over any `MessageEndpoint`.
//!
//! String entity IDs map to server-allocated `NetId`s. Every replicated entity carries an
//! `engine.id` component (sent with the spawn only) and one `engine.pose.N` component whose
//! priority class comes from its `engine.NetworkReplication`. Clients only act on IDs they
//! authored as `remote` templates; unknown IDs are ignored.
use super::{
    NetworkReplica, NetworkStatus, NetworkVelocity, ReplicaAuthority,
    components::{NetworkReplication, NetworkVoice, RelevanceRule},
    error,
    lagcomp::LagState,
    lane::LaneEndpoint,
    registry::{self, Ids, POSE_PROPERTIES, POSITION, ROTATION, SCALE, VELOCITY, repl},
    settings::{NetworkSettings, StateKind},
    transport,
    voice::{ClientVoice, ServerVoice},
};
use crate::{Result, SceneEntity, Transform, runtime::AuthoredId};
use bevy_ecs::prelude::*;
use engine_net_predict::MessageKind;
use engine_net_replication::{
    BandwidthBudget, ClientEvent, EntityInit, NetId, PropertyId, ReplicationClient,
    ReplicationError, ReplicationLimits, ReplicationRegistry, ReplicationServer, RpcTarget,
    ServerConfig, ServerEvent, Value,
};
use engine_network::{
    Delivery, Error as NetError, Lane, LinkEvent, MessageEndpoint, PeerId, SERVER_PEER,
};
use glam::Quat;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

/// Wire header of an engine input command on `Lane::INPUT`: `[WIRE_VERSION, INPUT_KIND]`.
const INPUT_KIND: u8 = 0x20;
const WIRE_VERSION: u8 = 1;
const INPUT_FIXED: usize = 2 + 8 + 4 + 12;
const MAX_INPUT_TICK_LEAD: u64 = 256;
const MAX_INBOX: usize = 256;
const MAX_INBOX_BYTES: usize = 1 << 20;
const MAX_MALFORMED_BEFORE_KICK: u32 = 64;
const MAX_PACKET: usize = 1200;

/// A payload on a lane the engine does not interpret (prediction traffic on `Lane::INPUT`,
/// `Lane::SESSION` and application lanes).
#[derive(Clone, Debug, PartialEq)]
pub struct LanePayload {
    pub peer: PeerId,
    pub lane: u8,
    pub delivery: Delivery,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeltaCounters {
    pub rpc_sent: u64,
    pub rpc_received: u64,
    /// Calls refused by the allow-list, bad JSON or the server's validation.
    pub rpc_rejected: u64,
    pub events_dropped: u64,
    pub malformed_packets: u64,
    pub send_errors: u64,
    pub backpressure_drops: u64,
    pub lane_payloads_dropped: u64,
    pub entities_spawned: u64,
    pub entities_despawned: u64,
}

struct Entry {
    net: NetId,
    tier: usize,
    relevance: RelevanceRule,
    last_pose_tick: u32,
}
struct PeerView {
    center: Option<[f32; 3]>,
    radius: f32,
}
/// Position, rotation, scale and velocity of a mirrored entity.
type PoseValues = ([f32; 3], [f32; 4], [f32; 3], [f32; 3]);

struct Live {
    pos: [f32; 3],
    rot: [f32; 4],
    scale: [f32; 3],
    vel: [f32; 3],
    rule: NetworkReplication,
}

pub(crate) struct ServerSide {
    pub(crate) rs: ReplicationServer,
    entries: BTreeMap<String, Entry>,
    by_net: BTreeMap<NetId, String>,
    pub(crate) owners: BTreeMap<String, PeerId>,
    connected: BTreeSet<PeerId>,
    views: BTreeMap<PeerId, PeerView>,
    budgets: BTreeMap<PeerId, BandwidthBudget>,
    input_ticks: BTreeMap<PeerId, u64>,
    malformed: BTreeMap<PeerId, u32>,
    pub(crate) last_tick: u64,
    pub(crate) lag: LagState,
    pub(crate) voice: Option<ServerVoice>,
}

pub(crate) struct ClientSide {
    pub(crate) rc: ReplicationClient,
    by_net: BTreeMap<NetId, String>,
    by_id: BTreeMap<String, NetId>,
    input_tick: u64,
    pub(crate) voice: Option<ClientVoice>,
}

pub(crate) enum Side {
    Server(Box<ServerSide>),
    Client(Box<ClientSide>),
}

pub(crate) struct Delta {
    pub(crate) registry: Arc<ReplicationRegistry>,
    pub(crate) ids: Ids,
    pub(crate) now_ms: f64,
    pub(crate) inbox: VecDeque<LanePayload>,
    inbox_bytes: usize,
    pub(crate) c: DeltaCounters,
    pub(crate) side: Side,
}

fn limits(settings: &NetworkSettings) -> ReplicationLimits {
    ReplicationLimits {
        max_entities: settings.max_entities,
        max_entities_per_peer: settings.max_entities,
        max_entity_bytes: 600,
        ..ReplicationLimits::default()
    }
}

impl Delta {
    pub(crate) fn new(settings: &NetworkSettings, server: bool) -> Result<Self> {
        let (registry, ids) = registry::build(settings)?;
        let side = if server {
            let config = ServerConfig {
                tick_rate_hz: settings.tick_rate_hz(),
                grid_cell_size: (settings.view_radius / 2.0).max(1.0),
                ..ServerConfig::default()
            };
            Side::Server(Box::new(ServerSide {
                rs: ReplicationServer::new(registry.clone(), limits(settings), config)
                    .map_err(repl)?,
                entries: BTreeMap::new(),
                by_net: BTreeMap::new(),
                owners: BTreeMap::new(),
                connected: BTreeSet::new(),
                views: BTreeMap::new(),
                budgets: BTreeMap::new(),
                input_ticks: BTreeMap::new(),
                malformed: BTreeMap::new(),
                last_tick: 0,
                lag: LagState::new(settings)?,
                voice: settings.voice.as_ref().map(ServerVoice::new).transpose()?,
            }))
        } else {
            Side::Client(Box::new(ClientSide {
                rc: ReplicationClient::new(registry.clone(), limits(settings)).map_err(repl)?,
                by_net: BTreeMap::new(),
                by_id: BTreeMap::new(),
                input_tick: 0,
                voice: settings.voice.as_ref().map(ClientVoice::new).transpose()?,
            }))
        };
        Ok(Self {
            registry,
            ids,
            now_ms: 0.0,
            inbox: VecDeque::new(),
            inbox_bytes: 0,
            c: DeltaCounters::default(),
            side,
        })
    }

    pub(crate) fn push_inbox(&mut self, payload: LanePayload) {
        if self.inbox.len() >= MAX_INBOX || self.inbox_bytes + payload.bytes.len() > MAX_INBOX_BYTES
        {
            self.c.lane_payloads_dropped += 1;
            return;
        }
        self.inbox_bytes += payload.bytes.len();
        self.inbox.push_back(payload);
    }
    pub(crate) fn take_inbox(&mut self) -> Vec<LanePayload> {
        self.inbox_bytes = 0;
        self.inbox.drain(..).collect()
    }
}

fn quantity(value: &Value) -> Option<[f32; 3]> {
    if let Value::Vec3(v) = value {
        Some(*v)
    } else {
        None
    }
}

/// Emits an engine event, counting (instead of failing the tick) when the queue is full.
fn emit_soft(
    world: &mut World,
    c: &mut DeltaCounters,
    name: &str,
    target: Option<Entity>,
    payload: serde_json::Value,
) {
    if super::emit(world, name, target, payload).is_err() {
        c.events_dropped += 1;
    }
}

/// Shared per-call context.
pub(crate) struct Cx<'a> {
    pub(crate) world: &'a mut World,
    pub(crate) ep: &'a mut LaneEndpoint,
    pub(crate) settings: &'a NetworkSettings,
    pub(crate) report: &'a mut NetworkStatus,
    pub(crate) templates: &'a BTreeMap<String, SceneEntity>,
}

// ---------------------------------------------------------------------------------- server

fn encode_input(command: &engine_network::InputCommand) -> Vec<u8> {
    let mut out = Vec::with_capacity(INPUT_FIXED + command.entity_id.len());
    out.extend_from_slice(&[WIRE_VERSION, INPUT_KIND]);
    out.extend_from_slice(&command.tick.to_le_bytes());
    out.extend_from_slice(&command.buttons.to_le_bytes());
    for v in command.movement {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(command.entity_id.as_bytes());
    out
}
fn decode_input(bytes: &[u8]) -> Option<engine_network::InputCommand> {
    if bytes.len() <= INPUT_FIXED || bytes.len() > INPUT_FIXED + 128 || bytes[1] != INPUT_KIND {
        return None;
    }
    let f = |i: usize| f32::from_le_bytes(bytes[i..i + 4].try_into().expect("4 bytes"));
    let entity_id = std::str::from_utf8(&bytes[INPUT_FIXED..]).ok()?;
    if entity_id.chars().any(char::is_control) {
        return None;
    }
    let movement = [f(14), f(18), f(22)];
    if movement.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
        return None;
    }
    Some(engine_network::InputCommand {
        tick: u64::from_le_bytes(bytes[2..10].try_into().expect("8 bytes")),
        entity_id: entity_id.into(),
        movement,
        buttons: u32::from_le_bytes(bytes[10..14].try_into().expect("4 bytes")),
    })
}

impl ServerSide {
    fn remove_entry(&mut self, id: &str, c: &mut DeltaCounters) {
        if let Some(entry) = self.entries.remove(id) {
            self.by_net.remove(&entry.net);
            let _ = self.rs.despawn(entry.net);
            c.entities_despawned += 1;
        }
    }

    fn forget_peer(&mut self, peer: PeerId) {
        self.rs.remove_peer(peer);
        self.connected.remove(&peer);
        self.views.remove(&peer);
        self.budgets.remove(&peer);
        self.input_ticks.remove(&peer);
        self.malformed.remove(&peer);
        self.owners.retain(|_, owner| *owner != peer);
        if let Some(voice) = self.voice.as_mut() {
            voice.remove_peer(peer);
        }
    }

    fn drain_rpc_events(&mut self, d_ids: &Ids, cx: &mut Cx<'_>, c: &mut DeltaCounters) {
        for event in self.rs.drain_events() {
            match event {
                ServerEvent::RpcReceived {
                    peer,
                    rpc,
                    entity,
                    args,
                } => {
                    c.rpc_received += 1;
                    let Some((name, payload)) = registry::decode_rpc(&args) else {
                        c.rpc_rejected += 1;
                        continue;
                    };
                    if cx
                        .settings
                        .rpc_allowlist
                        .as_ref()
                        .is_some_and(|allowed| !allowed.contains(&name))
                    {
                        c.rpc_rejected += 1;
                        continue;
                    }
                    let entity_id = entity.and_then(|e| self.by_net.get(&e).cloned());
                    let target = entity_id
                        .as_deref()
                        .and_then(|id| crate::services::find_entity(cx.world, id));
                    let owned = rpc == d_ids.call_owned;
                    emit_soft(
                        cx.world,
                        c,
                        "network.rpc",
                        target,
                        serde_json::json!({"peer_id":peer,"name":name,"entity_id":entity_id,"owned":owned,"payload":payload}),
                    );
                }
                ServerEvent::RpcRejected { peer, reason, .. } => {
                    c.rpc_rejected += 1;
                    emit_soft(
                        cx.world,
                        c,
                        "network.rpc_rejected",
                        None,
                        serde_json::json!({"peer_id":peer,"reason":reason.as_str()}),
                    );
                }
            }
        }
    }

    fn handle_input(&mut self, cx: &mut Cx<'_>, c: &mut DeltaCounters, peer: PeerId, bytes: &[u8]) {
        let Some(command) = decode_input(bytes) else {
            self.reject_input(cx, c, peer, "malformed input");
            return;
        };
        let target = crate::services::find_entity(cx.world, &command.entity_id);
        let server_replica = target.is_some_and(|e| {
            cx.world
                .get::<NetworkReplica>(e)
                .is_some_and(|r| r.authority == ReplicaAuthority::Server)
        });
        let owned = self.owners.get(&command.entity_id) == Some(&peer);
        let last = self.input_ticks.get(&peer).copied();
        let reason = if !owned {
            Some("peer does not own the entity")
        } else if !server_replica {
            Some("replica no longer exists")
        } else if last.is_some_and(|l| command.tick <= l) {
            Some("input tick replayed")
        } else if command.tick > self.last_tick.saturating_add(MAX_INPUT_TICK_LEAD) {
            Some("input tick too far ahead")
        } else {
            None
        };
        if let Some(reason) = reason {
            if reason == "replica no longer exists" {
                self.owners.remove(&command.entity_id);
            }
            self.reject_input(cx, c, peer, reason);
            return;
        }
        self.input_ticks.insert(peer, command.tick);
        cx.report.accepted_inputs = cx.report.accepted_inputs.saturating_add(1);
        emit_soft(
            cx.world,
            c,
            "network.input",
            target,
            serde_json::json!({"peer_id":peer,"tick":command.tick,"entity_id":command.entity_id,"movement":command.movement,"buttons":command.buttons}),
        );
    }

    fn reject_input(&mut self, cx: &mut Cx<'_>, c: &mut DeltaCounters, peer: PeerId, reason: &str) {
        cx.report.rejected_inputs = cx.report.rejected_inputs.saturating_add(1);
        emit_soft(
            cx.world,
            c,
            "network.input_rejected",
            None,
            serde_json::json!({"peer_id":peer,"reason":reason}),
        );
    }

    fn malformed(&mut self, cx: &mut Cx<'_>, c: &mut DeltaCounters, peer: PeerId) {
        c.malformed_packets += 1;
        let count = self.malformed.entry(peer).or_default();
        *count += 1;
        if *count > MAX_MALFORMED_BEFORE_KICK {
            cx.ep.disconnect(peer);
        }
    }

    pub(crate) fn before_step(
        &mut self,
        ids: &Ids,
        now_ms: f64,
        inbox: &mut Vec<LanePayload>,
        c: &mut DeltaCounters,
        cx: &mut Cx<'_>,
    ) -> Result<()> {
        let events = cx.ep.poll(cx.settings.poll_budget).map_err(transport)?;
        for event in events {
            match event {
                LinkEvent::Authenticated(peer) => {
                    emit_soft(
                        cx.world,
                        c,
                        "network.authenticated",
                        None,
                        serde_json::json!({"peer_id":peer}),
                    );
                }
                LinkEvent::Connected(peer) => {
                    if self.rs.add_peer(peer).is_err() {
                        cx.ep.disconnect(peer);
                        continue;
                    }
                    self.connected.insert(peer);
                    self.views.insert(
                        peer,
                        PeerView {
                            center: None,
                            radius: cx.settings.view_radius,
                        },
                    );
                    if let Some(voice) = self.voice.as_mut() {
                        voice.add_peer(peer);
                    }
                    emit_soft(
                        cx.world,
                        c,
                        "network.connected",
                        None,
                        serde_json::json!({"peer_id":peer}),
                    );
                }
                LinkEvent::Disconnected { peer, reason } => {
                    self.forget_peer(peer);
                    cx.report.last_error = Some(reason.clone());
                    emit_soft(
                        cx.world,
                        c,
                        "network.disconnected",
                        None,
                        serde_json::json!({"peer_id":peer,"reason":reason}),
                    );
                }
                LinkEvent::Message {
                    peer,
                    lane,
                    delivery,
                    payload,
                } => {
                    if !self.connected.contains(&peer) {
                        continue;
                    }
                    match lane {
                        Lane::REPLICATION | Lane::RPC => {
                            let mut result = self.rs.receive(peer, &payload);
                            if matches!(result, Err(ReplicationError::Backpressure(_))) {
                                self.drain_rpc_events(ids, cx, c);
                                result = self.rs.receive(peer, &payload);
                            }
                            match result {
                                Ok(()) => {}
                                Err(ReplicationError::Backpressure(_)) => {
                                    c.backpressure_drops += 1;
                                }
                                Err(_) => self.malformed(cx, c, peer),
                            }
                        }
                        Lane::INPUT if payload.get(1) == Some(&INPUT_KIND) => {
                            self.handle_input(cx, c, peer, &payload);
                        }
                        Lane::VOICE => {
                            if let Some(voice) = self.voice.as_mut() {
                                let copies = voice.route(peer, &payload, now_ms);
                                for (listener, bytes) in copies {
                                    if cx
                                        .ep
                                        .send(
                                            listener,
                                            engine_net_voice::VOICE_LANE,
                                            engine_net_voice::VOICE_DELIVERY,
                                            &bytes,
                                        )
                                        .is_err()
                                    {
                                        c.send_errors += 1;
                                    }
                                }
                            }
                        }
                        other => inbox.push(LanePayload {
                            peer,
                            lane: other.0,
                            delivery,
                            bytes: payload,
                        }),
                    }
                }
            }
        }
        self.drain_rpc_events(ids, cx, c);
        cx.report.peers = self.connected.iter().copied().collect();
        cx.report.pending_peers = 0;
        cx.report.connected = !self.connected.is_empty();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn after_step(
        &mut self,
        ids: &Ids,
        c: &mut DeltaCounters,
        cx: &mut Cx<'_>,
        tick: u64,
    ) -> Result<()> {
        self.last_tick = tick;
        let tick32 = tick as u32;
        let mut live: BTreeMap<String, Live> = BTreeMap::new();
        for entity in cx.world.iter_entities() {
            let Some(replica) = entity.get::<NetworkReplica>() else {
                continue;
            };
            if replica.authority != ReplicaAuthority::Server {
                continue;
            }
            super::validate_replica(cx.world, entity.id())?;
            let id = entity
                .get::<AuthoredId>()
                .ok_or_else(|| error("Replica lacks stable ID"))?
                .id
                .clone();
            if id.len() > usize::from(registry::MAX_ID_BYTES) {
                return Err(error("Replicated entity IDs are at most 128 bytes"));
            }
            let transform = entity
                .get::<Transform>()
                .ok_or_else(|| error("Replica lacks Transform"))?;
            crate::transforms::local_matrix(transform)?;
            live.insert(
                id,
                Live {
                    pos: transform.position,
                    rot: Quat::from_array(transform.rotation).normalize().to_array(),
                    scale: transform.scale,
                    vel: entity
                        .get::<NetworkVelocity>()
                        .copied()
                        .unwrap_or_default()
                        .0,
                    rule: entity
                        .get::<NetworkReplication>()
                        .copied()
                        .unwrap_or_default(),
                },
            );
        }
        let gone: Vec<String> = self
            .entries
            .keys()
            .filter(|id| !live.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.remove_entry(&id, c);
        }
        let tick_rate = cx.settings.tick_rate_hz();
        for (id, l) in &live {
            let tier = registry::tier_for(l.rule.priority);
            let owner = self
                .owners
                .get(id)
                .copied()
                .filter(|p| self.connected.contains(p));
            if self
                .entries
                .get(id)
                .is_some_and(|e| e.tier != tier || e.relevance != l.rule.relevance)
            {
                self.remove_entry(id, c);
            }
            if !self.entries.contains_key(id) {
                if l.rule.relevance == RelevanceRule::OwnerOnly && owner.is_none() {
                    continue;
                }
                let mut init = EntityInit::new()
                    .at(l.pos)
                    .relevance(l.rule.relevance.into())
                    .component(ids.id, vec![Value::String(id.clone())])
                    .component(ids.pose[tier], pose_values(l));
                if let Some(owner) = owner {
                    init = init.owner(owner);
                }
                if let Some(state) = ids.state {
                    init = init.component(state, registry::default_state(cx.settings));
                }
                let net = self.rs.spawn(init).map_err(repl)?;
                self.by_net.insert(net, id.clone());
                self.entries.insert(
                    id.clone(),
                    Entry {
                        net,
                        tier,
                        relevance: l.rule.relevance,
                        last_pose_tick: tick32,
                    },
                );
                c.entities_spawned += 1;
                continue;
            }
            let (net, last) = {
                let e = &self.entries[id];
                (e.net, e.last_pose_tick)
            };
            if self.rs.owner(net) != owner && self.rs.set_owner(net, owner).is_err() {
                self.remove_entry(id, c);
                continue;
            }
            self.rs.set_position(net, l.pos).map_err(repl)?;
            let interval = if l.rule.max_update_hz > 0.0 {
                ((tick_rate as f32 / l.rule.max_update_hz).ceil() as u32).max(1)
            } else {
                1
            };
            if tick32.wrapping_sub(last) >= interval {
                for (property, value) in pose_values(l).into_iter().enumerate() {
                    self.rs
                        .set_property(net, ids.pose[tier], PropertyId(property as u8), &value)
                        .map_err(repl)?;
                }
                if let Some(entry) = self.entries.get_mut(id) {
                    entry.last_pose_tick = tick32;
                }
            }
        }
        self.sync_voice_and_views(cx.world, &live);
        self.build_and_send(c, cx, tick32);
        self.lag.record(cx.world, tick);
        Ok(())
    }

    fn sync_voice_and_views(&mut self, world: &World, live: &BTreeMap<String, Live>) {
        if let Some(voice) = self.voice.as_mut() {
            for entity in world.iter_entities() {
                let (Some(spec), Some(id)) =
                    (entity.get::<NetworkVoice>(), entity.get::<AuthoredId>())
                else {
                    continue;
                };
                let Some(&peer) = self.owners.get(&id.id) else {
                    continue;
                };
                if !self.connected.contains(&peer) {
                    continue;
                }
                if let Ok(matrix) = crate::world_transform(world, entity.id()) {
                    let position = matrix.position();
                    if voice.router.set_position(peer, Some(position)).is_ok() {
                        voice.positions.insert(peer, position);
                    }
                }
                voice.narrow.insert(peer, spec.proximity_distance);
                if voice.channels.get(&peer) != Some(&spec.channel) {
                    if let Some(old) = voice.channels.insert(peer, spec.channel) {
                        let _ = voice.router.leave_channel(peer, old);
                    }
                    let _ = voice.router.join_channel(peer, spec.channel);
                }
            }
        }
        let peers: Vec<PeerId> = self.connected.iter().copied().collect();
        for peer in peers {
            let Some(view) = self.views.get(&peer) else {
                continue;
            };
            let center = view.center.or_else(|| {
                self.owners
                    .iter()
                    .filter(|(_, owner)| **owner == peer)
                    .find_map(|(id, _)| live.get(id).map(|l| l.pos))
            });
            if let Some(center) = center {
                let _ = self.rs.set_peer_view(peer, center, view.radius);
            }
        }
    }

    fn build_and_send(&mut self, c: &mut DeltaCounters, cx: &mut Cx<'_>, tick: u32) {
        let rate = cx.settings.tick_rate_hz();
        let bytes_per_second = cx.settings.bytes_per_second;
        for peer in self.connected.iter().copied().collect::<Vec<_>>() {
            let max_payload = [Delivery::Unreliable, Delivery::ReliableOrdered]
                .iter()
                .filter_map(|d| cx.ep.max_payload(peer, *d))
                .min()
                .unwrap_or(0)
                .min(MAX_PACKET);
            if max_payload < engine_net_replication::MIN_PAYLOAD + 600 {
                continue;
            }
            let budget = self
                .budgets
                .entry(peer)
                .or_insert_with(|| BandwidthBudget::new(bytes_per_second, rate))
                .next_tick();
            let Ok(packets) = self.rs.build(peer, tick, budget, max_payload) else {
                continue;
            };
            for packet in packets {
                match cx
                    .ep
                    .send(peer, packet.lane, packet.delivery, &packet.bytes)
                {
                    Ok(()) => {}
                    Err(NetError::QueueFull) => {
                        if let Some(sequence) = packet.sequence {
                            self.rs.mark_unsent(peer, sequence);
                        }
                        c.send_errors += 1;
                    }
                    Err(_) => c.send_errors += 1,
                }
            }
        }
    }
}

fn pose_values(l: &Live) -> Vec<Value> {
    vec![
        Value::Vec3(l.pos),
        Value::Quat(l.rot),
        Value::Vec3(l.scale),
        Value::Vec3(l.vel),
    ]
}

// ---------------------------------------------------------------------------------- client

impl ClientSide {
    fn read_pose(&self, ids: &Ids, net: NetId) -> Option<PoseValues> {
        let entity = self.rc.entity(net)?;
        let comp = ids.pose.iter().find_map(|id| entity.component(*id))?;
        let position = quantity(comp.values.get(usize::from(POSITION))?)?;
        let Value::Quat(rotation) = comp.values.get(usize::from(ROTATION))? else {
            return None;
        };
        let scale = quantity(comp.values.get(usize::from(SCALE))?)?;
        let velocity = quantity(comp.values.get(usize::from(VELOCITY))?)?;
        Some((position, *rotation, scale, velocity))
    }

    fn apply_pose(
        &self,
        cx: &mut Cx<'_>,
        ids: &Ids,
        net: NetId,
        id: &str,
        alpha: f32,
    ) -> Result<()> {
        let Some((position, rotation, scale, velocity)) = self.read_pose(ids, net) else {
            return Ok(());
        };
        let Some(entity) = crate::services::find_entity(cx.world, id) else {
            return Ok(());
        };
        if !super::is_remote_replica(cx.world, entity) {
            return Err(error("Remote template ID now belongs to a local entity"));
        }
        let current = cx.world.get::<Transform>(entity).cloned();
        let mut transform = Transform {
            position,
            rotation,
            scale,
            ..Transform::default()
        };
        if let (Some(current), true) = (current, alpha < 1.0) {
            let lerp = |a: [f32; 3], b: [f32; 3]| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * alpha);
            transform.position = lerp(current.position, position);
            transform.scale = lerp(current.scale, scale);
            transform.rotation = Quat::from_array(current.rotation)
                .slerp(Quat::from_array(rotation), alpha)
                .normalize()
                .to_array();
        }
        crate::transforms::local_matrix(&transform)?;
        cx.world
            .entity_mut(entity)
            .insert((transform, NetworkVelocity(velocity)));
        Ok(())
    }

    fn id_of(&self, ids: &Ids, net: NetId) -> Option<String> {
        let entity = self.rc.entity(net)?;
        match entity.get(ids.id, PropertyId(0))? {
            Value::String(id) => Some(id.clone()),
            _ => None,
        }
    }

    pub(crate) fn before_step(
        &mut self,
        ids: &Ids,
        now_ms: f64,
        dt: f32,
        inbox: &mut Vec<LanePayload>,
        c: &mut DeltaCounters,
        cx: &mut Cx<'_>,
    ) -> Result<()> {
        let events = cx.ep.poll(cx.settings.poll_budget).map_err(transport)?;
        for event in events {
            match event {
                LinkEvent::Authenticated(peer) => emit_soft(
                    cx.world,
                    c,
                    "network.authenticated",
                    None,
                    serde_json::json!({"peer_id":peer}),
                ),
                LinkEvent::Connected(_) => emit_soft(
                    cx.world,
                    c,
                    "network.connected",
                    None,
                    serde_json::json!({}),
                ),
                LinkEvent::Disconnected { reason, .. } => {
                    cx.report.last_error = Some(reason.clone());
                    emit_soft(
                        cx.world,
                        c,
                        "network.disconnected",
                        None,
                        serde_json::json!({"reason":reason}),
                    );
                }
                LinkEvent::Message {
                    lane,
                    delivery,
                    payload,
                    ..
                } => match lane {
                    Lane::REPLICATION | Lane::RPC => {
                        if self.rc.receive(&payload).is_err() {
                            c.malformed_packets += 1;
                        }
                    }
                    Lane::VOICE => {
                        if let Some(voice) = self.voice.as_mut() {
                            voice.receive(&payload, now_ms);
                        }
                    }
                    other => inbox.push(LanePayload {
                        peer: SERVER_PEER,
                        lane: other.0,
                        delivery,
                        bytes: payload,
                    }),
                },
            }
        }
        if let Some(ack) = self.rc.build_ack()
            && cx
                .ep
                .send(SERVER_PEER, ack.lane, ack.delivery, &ack.bytes)
                .is_err()
        {
            c.send_errors += 1;
        }
        let mut fresh = BTreeSet::new();
        for event in self.rc.drain_events() {
            self.handle_event(ids, c, cx, event, &mut fresh)?;
        }
        let alpha = if cx.settings.remote_smoothing_hz > 0.0 {
            1.0 - (-cx.settings.remote_smoothing_hz * dt).exp()
        } else {
            1.0
        };
        let mapped: Vec<(NetId, String)> =
            self.by_net.iter().map(|(n, id)| (*n, id.clone())).collect();
        for (net, id) in mapped {
            let alpha = if fresh.contains(&net) { 1.0 } else { alpha };
            self.apply_pose(cx, ids, net, &id, alpha)?;
        }
        // Authored remote templates exist only while the server replicates them.
        for id in cx.templates.keys() {
            if !self.by_id.contains_key(id) {
                super::despawn_remote(cx.world, id)?;
            }
        }
        crate::refresh_world_transforms(cx.world)?;
        cx.report.connected = !cx.ep.connected_peers().is_empty();
        cx.report.remote_entities = cx
            .templates
            .keys()
            .filter(|id| crate::services::find_entity(cx.world, id).is_some())
            .count();
        cx.report.latest_snapshot_tick =
            (self.rc.server_tick() > 0).then(|| u64::from(self.rc.server_tick()));
        Ok(())
    }

    fn handle_event(
        &mut self,
        ids: &Ids,
        c: &mut DeltaCounters,
        cx: &mut Cx<'_>,
        event: ClientEvent,
        fresh: &mut BTreeSet<NetId>,
    ) -> Result<()> {
        match event {
            ClientEvent::Spawned { entity: net, owned } => {
                let Some(id) = self.id_of(ids, net) else {
                    return Ok(());
                };
                let Some(template) = cx.templates.get(&id) else {
                    return Ok(());
                };
                if self.by_id.contains_key(&id) {
                    return Ok(());
                }
                if let Some(existing) = crate::services::find_entity(cx.world, &id) {
                    if !super::is_remote_replica(cx.world, existing) {
                        return Err(error("Remote template ID now belongs to a local entity"));
                    }
                } else {
                    super::spawn_remote(cx.world, template)?;
                }
                self.by_net.insert(net, id.clone());
                self.by_id.insert(id.clone(), net);
                fresh.insert(net);
                c.entities_spawned += 1;
                let target = crate::services::find_entity(cx.world, &id);
                emit_soft(
                    cx.world,
                    c,
                    "network.entity_spawned",
                    target,
                    serde_json::json!({"entity_id":id,"owned":owned}),
                );
            }
            ClientEvent::Despawned { entity: net } => {
                if let Some(id) = self.by_net.remove(&net) {
                    self.by_id.remove(&id);
                    super::despawn_remote(cx.world, &id)?;
                    c.entities_despawned += 1;
                    emit_soft(
                        cx.world,
                        c,
                        "network.entity_despawned",
                        None,
                        serde_json::json!({"entity_id":id}),
                    );
                }
            }
            ClientEvent::OwnerChanged { entity: net, owned } => {
                if let Some(id) = self.by_net.get(&net).cloned() {
                    emit_soft(
                        cx.world,
                        c,
                        "network.owner_changed",
                        None,
                        serde_json::json!({"entity_id":id,"owned":owned}),
                    );
                }
            }
            ClientEvent::PropertyChanged(change) => {
                let Some(id) = self.by_net.get(&change.entity).cloned() else {
                    return Ok(());
                };
                let name = if Some(change.component) == ids.state {
                    cx.settings
                        .state_properties
                        .get(usize::from(change.property.0))
                        .map(|p| p.name.clone())
                } else if cx.settings.property_events && ids.pose.contains(&change.component) {
                    POSE_PROPERTIES
                        .get(usize::from(change.property.0))
                        .map(|n| (*n).to_owned())
                } else {
                    None
                };
                if let Some(property) = name {
                    emit_soft(
                        cx.world,
                        c,
                        "network.property_changed",
                        None,
                        serde_json::json!({"entity_id":id,"property":property,"old":registry::value_json(&change.old),"new":registry::value_json(&change.new)}),
                    );
                }
            }
            ClientEvent::RpcReceived { entity, args, .. } => {
                c.rpc_received += 1;
                let Some((name, payload)) = registry::decode_rpc(&args) else {
                    c.rpc_rejected += 1;
                    return Ok(());
                };
                let entity_id = entity.and_then(|e| self.by_net.get(&e).cloned());
                let target = entity_id
                    .as_deref()
                    .and_then(|id| crate::services::find_entity(cx.world, id));
                emit_soft(
                    cx.world,
                    c,
                    "network.rpc",
                    target,
                    serde_json::json!({"name":name,"entity_id":entity_id,"payload":payload}),
                );
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------------ public API

/// Who a server-to-client RPC reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcRecipient {
    Peer(PeerId),
    /// The owner of the target entity.
    Owner,
    /// Every peer that currently has the target entity.
    Relevant,
}

/// What a client-to-server RPC addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientRpcTarget<'a> {
    /// No entity.
    Session,
    /// An entity the client currently sees.
    Entity(&'a str),
    /// An entity the client owns; the server enforces ownership.
    Owned(&'a str),
}

fn session_parts(
    world: &mut World,
) -> Result<bevy_ecs::change_detection::Mut<'_, super::NetworkSession>> {
    world
        .get_resource_mut::<super::NetworkSession>()
        .ok_or_else(|| error("No network session"))
}

/// Server: sends a named JSON RPC to clients. Returns the number of recipients. The payload is
/// at most 768 bytes of JSON.
pub fn rpc_to_clients(
    world: &mut World,
    recipient: RpcRecipient,
    name: &str,
    entity_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<usize> {
    let args = registry::rpc_args(name, payload)?;
    let mut session = session_parts(world)?;
    let super::NetworkSession {
        endpoint, delta, ..
    } = &mut *session;
    let (Some(super::Endpoint::LaneServer(lane)), Some(delta)) =
        (endpoint.as_mut(), delta.as_mut())
    else {
        return Err(error("RPC to clients requires a delta-mode server session"));
    };
    let ids = delta.ids.clone();
    let Side::Server(server) = &mut delta.side else {
        return Err(error("RPC to clients requires a delta-mode server session"));
    };
    let net = match entity_id {
        Some(id) => Some(
            server
                .entries
                .get(id)
                .ok_or_else(|| error("Entity is not replicated"))?
                .net,
        ),
        None => None,
    };
    let rpc = if net.is_some() {
        ids.event_entity
    } else {
        ids.event
    };
    let target = match recipient {
        RpcRecipient::Peer(p) => RpcTarget::Peer(p),
        RpcRecipient::Owner => RpcTarget::Owner,
        RpcRecipient::Relevant => RpcTarget::Relevant,
    };
    let packets = server.rs.call_rpc(rpc, target, net, &args).map_err(repl)?;
    let lane = lane.get_mut().unwrap_or_else(|e| e.into_inner());
    let count = packets.len();
    for (peer, packet) in packets {
        lane.send(peer, packet.lane, packet.delivery, &packet.bytes)
            .map_err(transport)?;
        delta.c.rpc_sent += 1;
    }
    Ok(count)
}

/// Client: sends a named JSON RPC to the server. It surfaces there as a `network.rpc` event
/// after rate, visibility, ownership and argument validation.
pub fn rpc_to_server(
    world: &mut World,
    target: ClientRpcTarget<'_>,
    name: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let args = registry::rpc_args(name, payload)?;
    let mut session = session_parts(world)?;
    let super::NetworkSession {
        endpoint, delta, ..
    } = &mut *session;
    let (Some(super::Endpoint::LaneClient(lane)), Some(delta)) =
        (endpoint.as_mut(), delta.as_mut())
    else {
        return Err(error(
            "RPC to the server requires a delta-mode client session",
        ));
    };
    let ids = delta.ids.clone();
    let Side::Client(client) = &mut delta.side else {
        return Err(error(
            "RPC to the server requires a delta-mode client session",
        ));
    };
    let (rpc, net) = match target {
        ClientRpcTarget::Session => (ids.call, None),
        ClientRpcTarget::Entity(id) | ClientRpcTarget::Owned(id) => {
            let net = *client
                .by_id
                .get(id)
                .ok_or_else(|| error("Entity is not replicated to this client"))?;
            (
                if matches!(target, ClientRpcTarget::Owned(_)) {
                    ids.call_owned
                } else {
                    ids.call_entity
                },
                Some(net),
            )
        }
    };
    let packet = client.rc.call_server(rpc, net, &args).map_err(repl)?;
    lane.get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .send(SERVER_PEER, packet.lane, packet.delivery, &packet.bytes)
        .map_err(transport)?;
    delta.c.rpc_sent += 1;
    Ok(())
}

/// Server: sets a custom replicated state property of an entity (see
/// `NetworkSettings::state_properties`).
pub fn set_state(
    world: &mut World,
    entity_id: &str,
    property: &str,
    value: &serde_json::Value,
) -> Result<()> {
    let mut session = session_parts(world)?;
    let super::NetworkSession {
        settings, delta, ..
    } = &mut *session;
    let Some(delta) = delta.as_mut() else {
        return Err(error("State requires a delta-mode session"));
    };
    let (Side::Server(server), Some(component)) = (&mut delta.side, delta.ids.state) else {
        return Err(error(
            "State requires a delta-mode server with state properties",
        ));
    };
    let index = settings
        .state_properties
        .iter()
        .position(|p| p.name == property)
        .ok_or_else(|| error("Unknown state property"))?;
    let kind: StateKind = settings.state_properties[index].kind;
    let net = server
        .entries
        .get(entity_id)
        .ok_or_else(|| error("Entity is not replicated"))?
        .net;
    server
        .rs
        .set_property(
            net,
            component,
            PropertyId(index as u8),
            &registry::json_value(kind, value)?,
        )
        .map_err(repl)?;
    Ok(())
}

/// Server or client: the current value of a custom state property.
pub fn state(world: &World, entity_id: &str, property: &str) -> Option<serde_json::Value> {
    let session = world.get_resource::<super::NetworkSession>()?;
    let delta = session.delta.as_ref()?;
    let component = delta.ids.state?;
    let index = session
        .settings
        .state_properties
        .iter()
        .position(|p| p.name == property)?;
    let value = match &delta.side {
        Side::Server(server) => {
            let net = server.entries.get(entity_id)?.net;
            server
                .rs
                .get_property(net, component, PropertyId(index as u8))
                .ok()?
        }
        Side::Client(client) => {
            let net = *client.by_id.get(entity_id)?;
            client
                .rc
                .entity(net)?
                .get(component, PropertyId(index as u8))?
                .clone()
        }
    };
    Some(registry::value_json(&value))
}

/// Server: fixes (or, with `None`, releases) the center of a peer's view and sets its radius.
/// Without a center the view follows the first entity granted to the peer.
pub fn set_peer_view(
    world: &mut World,
    peer: PeerId,
    center: Option<[f32; 3]>,
    radius: f32,
) -> Result<()> {
    let mut session = session_parts(world)?;
    let Some(delta) = session.delta.as_mut() else {
        return Err(error("Peer views require a delta-mode server session"));
    };
    let Side::Server(server) = &mut delta.side else {
        return Err(error("Peer views require a delta-mode server session"));
    };
    if !radius.is_finite() || radius < 0.0 {
        return Err(error("View radius must be finite and nonnegative"));
    }
    let view = server
        .views
        .get_mut(&peer)
        .ok_or_else(|| error("Unknown peer"))?;
    view.center = center;
    view.radius = radius;
    if let Some(center) = center {
        server
            .rs
            .set_peer_view(peer, center, radius)
            .map_err(repl)?;
    }
    Ok(())
}

/// Client: the server tick the client is displaying, for lag-compensated shot claims.
pub fn view_tick(world: &World) -> Option<f64> {
    let session = world.get_resource::<super::NetworkSession>()?;
    let Side::Client(client) = &session.delta.as_ref()?.side else {
        return None;
    };
    let tick = client.rc.server_tick();
    (tick > 0).then(|| (f64::from(tick) - session.settings.interpolation_delay_ticks).max(0.0))
}

/// Lane payloads the engine does not interpret (prediction traffic, session and application
/// lanes), oldest first. The inbox is bounded; overflow is counted in the status report.
pub fn take_lane_payloads(world: &mut World) -> Vec<LanePayload> {
    world
        .get_resource_mut::<super::NetworkSession>()
        .and_then(|mut s| s.delta.as_mut().map(Delta::take_inbox))
        .unwrap_or_default()
}

/// Sends a payload on `Lane::INPUT` (prediction), `Lane::SESSION`, `Lane::DEFAULT` or an
/// application lane. A client always addresses the server; `peer` is ignored there.
pub fn send_lane_payload(
    world: &mut World,
    peer: PeerId,
    lane: u8,
    delivery: Delivery,
    bytes: &[u8],
) -> Result<()> {
    let lane = Lane(lane);
    if !lane.is_valid()
        || matches!(lane, Lane::REPLICATION | Lane::RPC | Lane::VOICE)
        || (lane == Lane::INPUT && bytes.get(1) == Some(&INPUT_KIND))
    {
        return Err(error("Lane is owned by the engine"));
    }
    let mut session = session_parts(world)?;
    match session.endpoint.as_mut() {
        Some(super::Endpoint::LaneServer(l)) => l
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .send(peer, lane, delivery, bytes),
        Some(super::Endpoint::LaneClient(l)) => l
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .send(SERVER_PEER, lane, delivery, bytes),
        _ => return Err(error("Lane payloads require a delta-mode session")),
    }
    .map_err(transport)
}

/// Whether a lane-`INPUT` payload is prediction traffic (an `engine_net_predict` message).
pub fn is_prediction_payload(bytes: &[u8]) -> bool {
    MessageKind::peek(bytes).is_some()
}

/// Client: queues an input command on `Lane::INPUT`.
pub(crate) fn send_lane_input(
    world: &mut World,
    command: engine_network::InputCommand,
) -> Result<()> {
    let mut session = session_parts(world)?;
    let super::NetworkSession {
        endpoint, delta, ..
    } = &mut *session;
    let (Some(super::Endpoint::LaneClient(lane)), Some(delta)) =
        (endpoint.as_mut(), delta.as_mut())
    else {
        return Err(error("Sending input requires a client session"));
    };
    let Side::Client(client) = &mut delta.side else {
        return Err(error("Sending input requires a client session"));
    };
    if command.entity_id.is_empty()
        || command.entity_id.len() > 128
        || command.entity_id.chars().any(char::is_control)
        || command
            .movement
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1.0)
        || command.tick <= client.input_tick
    {
        return Err(error(
            "Input needs a valid ID, finite movement in [-1, 1] and an increasing tick",
        ));
    }
    lane.get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .send(
            SERVER_PEER,
            Lane::INPUT,
            Delivery::ReliableOrdered,
            &encode_input(&command),
        )
        .map_err(transport)?;
    client.input_tick = command.tick;
    Ok(())
}

/// Server: grant `peer` input and RPC ownership of an entity (delta mode).
pub(crate) fn assign_owner(server: &mut ServerSide, entity_id: &str, peer: PeerId) -> Result<()> {
    if !server.connected.contains(&peer) {
        return Err(error("Ownership requires an admitted peer"));
    }
    server.owners.insert(entity_id.to_owned(), peer);
    Ok(())
}
pub(crate) fn revoke_owner(server: &mut ServerSide, entity_id: &str) {
    server.owners.remove(entity_id);
}
pub(crate) fn disconnect_peer(server: &mut ServerSide, peer: PeerId) {
    server.forget_peer(peer);
}
