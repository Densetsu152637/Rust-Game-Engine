//! Explicit runtime attachment of network sessions.
//!
//! Two modes share one adapter. `Snapshot` is the original whole-snapshot replication over the
//! TCP `Server`/`Client` event API. `Delta` drives any [`MessageEndpoint`] (TCP, QUIC, optionally
//! under a [`NetworkConditions`] simulator) with `engine-net-replication` deltas, RPCs, input,
//! lag compensation and voice on the transport lanes. See `docs/networking.md`.
use crate::{
    ComponentDescriptor, EngineEvent, Error, Result, Runtime, Scene, SceneEntity, Transform,
    runtime::{AuthoredId, EntityLookup, OwnedBy, ProjectResource},
};
use bevy_ecs::prelude::*;
use engine_network::{
    Client, ClientEvent, EntitySnapshot, InputCommand, LinkStats, MessageEndpoint, PeerId, Server,
    ServerEvent, Snapshot, TransportKind, TransportSecurity,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, PoisonError},
};

mod components;
mod delta;
mod inspect;
mod lagcomp;
mod lane;
mod registry;
mod settings;
mod voice;

pub use components::{
    HITBOXES_COMPONENT, HitboxShape, HitboxSpec, NAMES as COMPONENT_NAMES, NetworkHitboxes,
    NetworkReplication, NetworkSessionSettings, NetworkVoice, REPLICATION_COMPONENT, RelevanceRule,
    SESSION_COMPONENT, VOICE_COMPONENT, validate_component, validate_scene,
};
pub use delta::{
    ClientRpcTarget, DeltaCounters, LanePayload, RpcRecipient, is_prediction_payload,
    rpc_to_clients, rpc_to_server, send_lane_payload, set_peer_view, set_state, state,
    take_lane_payloads, view_tick,
};
/// Prediction toolkit (client prediction, rollback, prediction keys, lag compensation types).
/// See the recipe in `docs/networking.md`.
pub use engine_net_predict as predict;
/// Replication schema types for native extensions.
pub use engine_net_replication as replication;
/// Voice codec and routing types.
pub use engine_net_voice as voice_types;
pub use engine_network::NetworkConditions;
pub use inspect::{inspect_report, inspect_scene};
pub use lagcomp::{
    LagCompStatus, LagHit, LagRaycast, LagRaycastResult, hitbox_pose_at,
    lag_compensated_overlap_sphere, lag_compensated_raycast,
};
pub use settings::{
    ConditionsConfig, NetworkMode, NetworkSessionConfig, NetworkSettings, StateKind, StateProperty,
    VoiceCodecFactory, VoiceConfig, VoiceRoutingKind, VoiceSettings,
};
pub use voice::{
    VoiceStatus, pull_voice, send_voice_frame, set_voice_channel, set_voice_muted,
    set_voice_speaking_allowed,
};

pub const REPLICA_COMPONENT: &str = "engine.NetworkReplica";
const POSE_WRITERS: &[&str] = &[
    "engine.PhysicsBody2D",
    "engine.PhysicsBody3D",
    "engine.AnimationPlayer",
    "engine.NavigationAgent",
    "engine.CrowdAgent",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaAuthority {
    Server,
    Remote,
}

/// Explicit pose authority. Remote entities must be root-level, script-free templates.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkReplica {
    pub authority: ReplicaAuthority,
}
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct NetworkVelocity(pub [f32; 3]);

pub fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: REPLICA_COMPONENT.into(),
        version: 1,
        schema: serde_json::json!({"type":"object", "properties":{"authority":{"type":"string","enum":["server","remote"]}},"required":["authority"],"additionalProperties":false}),
        defaults: serde_json::json!({"authority":"server"}),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}
/// Descriptors of every network component, chained into the component catalog.
pub fn descriptors() -> Vec<ComponentDescriptor> {
    let mut all = vec![descriptor()];
    all.extend(components::descriptors());
    all
}
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<NetworkReplica>(app, REPLICA_COMPONENT, 1)?;
    components::register(app)
}
pub fn is_remote_replica(world: &World, entity: Entity) -> bool {
    world
        .get::<NetworkReplica>(entity)
        .is_some_and(|r| r.authority == ReplicaAuthority::Remote)
}
fn error(message: impl Into<String>) -> Error {
    Error::new("network.runtime.invalid", message)
}
fn transport(error: engine_network::Error) -> Error {
    Error::new("network.transport", error.to_string())
}
fn voice_error(error: engine_net_voice::VoiceError) -> Error {
    Error::new("network.voice", error.to_string())
}

/// Conservative single-writer contract, also callable by other simulation adapters.
pub fn validate_replica(world: &World, entity: Entity) -> Result<()> {
    let Some(replica) = world.get::<NetworkReplica>(entity) else {
        return Ok(());
    };
    if world.get::<crate::Parent>(entity).is_some()
        || world.get::<Transform>(entity).is_none()
        || world.get::<AuthoredId>(entity).is_none()
    {
        return Err(error(
            "Network replicas require stable authored IDs and root-level Transform",
        ));
    }
    if replica.authority == ReplicaAuthority::Remote {
        if world.get::<crate::Script>(entity).is_some() || world.get::<OwnedBy>(entity).is_some() {
            return Err(error(
                "Remote replicas cannot run scripts or be script-owned",
            ));
        }
        let components = crate::catalog::snapshot(world, entity)?;
        if POSE_WRITERS
            .iter()
            .any(|name| components.contains_key(*name))
        {
            return Err(error(
                "Remote replica conflicts with physics, animation or navigation pose authority",
            ));
        }
        let id = &world
            .get::<AuthoredId>(entity)
            .ok_or_else(|| error("Missing replica ID"))?
            .id;
        if world
            .iter_entities()
            .any(|e| e.get::<crate::Parent>().is_some_and(|p| p.0 == *id))
        {
            return Err(error("Remote replicas cannot parent local entities"));
        }
    }
    Ok(())
}
/// One peer's transport counters.
#[derive(Clone, Debug, Serialize)]
pub struct PeerLinkStatus {
    pub peer_id: PeerId,
    #[serde(flatten)]
    pub link: LinkStats,
}

/// Replication counters of a delta-mode session (bounded to `max_reported_peers` peers).
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReplicationStatus {
    /// Schema hash as 16 hex digits; both ends must report the same value.
    pub schema_hash: String,
    pub tick: u32,
    pub entities: usize,
    pub peers: Vec<replication::PeerTelemetry>,
    pub peers_truncated: bool,
    pub client: Option<replication::ClientTelemetry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NetworkStatus {
    pub role: String,
    pub security: TransportSecurity,
    pub pending_peers: usize,
    pub connected: bool,
    pub peer_id: Option<PeerId>,
    pub peers: Vec<PeerId>,
    pub latest_snapshot_tick: Option<u64>,
    pub accepted_inputs: u64,
    pub rejected_inputs: u64,
    pub remote_entities: usize,
    pub last_error: Option<String>,
    pub mode: NetworkMode,
    /// `None` in snapshot mode over the legacy TCP event API.
    pub transport: Option<TransportKind>,
    /// Active link simulation (delta mode).
    pub conditions: Option<NetworkConditions>,
    pub peer_count: usize,
    pub links: Vec<PeerLinkStatus>,
    pub replication: Option<ReplicationStatus>,
    pub counters: Option<DeltaCounters>,
    pub voice: Option<VoiceStatus>,
    pub lag_comp: Option<LagCompStatus>,
}

pub(crate) enum Endpoint {
    Server(Box<Server>),
    Client(Box<Client>),
    LaneServer(Mutex<lane::LaneEndpoint>),
    LaneClient(Mutex<lane::LaneEndpoint>),
}
#[derive(Resource)]
pub struct NetworkSession {
    pub(crate) endpoint: Option<Endpoint>,
    pub(crate) settings: NetworkSettings,
    templates: BTreeMap<String, SceneEntity>,
    snapshot_age_ticks: f64,
    pub(crate) report: NetworkStatus,
    pub(crate) delta: Option<delta::Delta>,
}
impl NetworkSession {
    pub fn status(&self) -> &NetworkStatus {
        &self.report
    }
}
fn lane_mut(endpoint: &mut Option<Endpoint>) -> Option<&mut lane::LaneEndpoint> {
    match endpoint {
        Some(Endpoint::LaneServer(l) | Endpoint::LaneClient(l)) => {
            Some(l.get_mut().unwrap_or_else(PoisonError::into_inner))
        }
        _ => None,
    }
}
/// Session protocols currently identify entities within one scene only.
pub fn is_attached(world: &World) -> bool {
    world
        .get_resource::<NetworkSession>()
        .is_some_and(|session| session.endpoint.is_some())
}
/// Close sockets before changing scenes; the final status report remains inspectable.
pub fn detach(world: &mut World) {
    shutdown(world);
}
pub fn status(world: &World) -> Option<NetworkStatus> {
    world
        .get_resource::<NetworkSession>()
        .map(|session| session.report.clone())
}

/// Attach a caller-created TCP server; neither loading a scene nor reading metadata binds a
/// port. With `settings.mode == NetworkMode::Delta` the server is driven through its
/// [`MessageEndpoint`] lanes instead of the legacy snapshot event API.
pub fn attach_server(
    runtime: &mut Runtime,
    server: Server,
    settings: NetworkSettings,
) -> Result<()> {
    if settings.mode == NetworkMode::Delta {
        return attach_lane(runtime, Box::new(server), settings, true);
    }
    attach(runtime, Endpoint::Server(Box::new(server)), settings)
}
/// Attach a caller-created TCP client. Only locally authored Remote templates are eligible to
/// spawn.
pub fn attach_client(
    runtime: &mut Runtime,
    client: Client,
    settings: NetworkSettings,
) -> Result<()> {
    if settings.mode == NetworkMode::Delta {
        return attach_lane(runtime, Box::new(client), settings, false);
    }
    attach(runtime, Endpoint::Client(Box::new(client)), settings)
}
/// Attach any server [`MessageEndpoint`] (TCP, `QuicServer`, ...), optionally simulating
/// `settings.conditions`. Endpoint attachment always uses [`NetworkMode::Delta`].
pub fn attach_endpoint_server(
    runtime: &mut Runtime,
    endpoint: Box<dyn MessageEndpoint + Send>,
    settings: NetworkSettings,
) -> Result<()> {
    attach_lane(runtime, endpoint, settings, true)
}
/// Attach any client [`MessageEndpoint`]; see [`attach_endpoint_server`].
pub fn attach_endpoint_client(
    runtime: &mut Runtime,
    endpoint: Box<dyn MessageEndpoint + Send>,
    settings: NetworkSettings,
) -> Result<()> {
    attach_lane(runtime, endpoint, settings, false)
}

fn collect_templates(runtime: &Runtime) -> Result<BTreeMap<String, SceneEntity>> {
    let mut templates = BTreeMap::new();
    for authored in &runtime.active_scene().entities {
        let replica = authored
            .components
            .custom
            .get(REPLICA_COMPONENT)
            .map(|v| serde_json::from_value::<NetworkReplica>(v.data.clone()))
            .transpose()?;
        if replica.is_some_and(|r| r.authority == ReplicaAuthority::Remote) {
            if authored.parent.is_some()
                || authored.components.transform.is_none()
                || authored.components.script.is_some()
                || POSE_WRITERS
                    .iter()
                    .any(|name| authored.components.custom.contains_key(*name))
                || runtime
                    .active_scene()
                    .entities
                    .iter()
                    .any(|child| child.parent.as_ref() == Some(&authored.id))
            {
                return Err(error(
                    "Remote templates require root Transform and no scripts, children or pose writers",
                ));
            }
            templates.insert(authored.id.clone(), authored.clone());
        }
    }
    Ok(templates)
}
fn check_attachable(runtime: &Runtime, settings: &NetworkSettings) -> Result<()> {
    settings.validate()?;
    if runtime.running() || runtime.app().world().contains_resource::<NetworkSession>() {
        return Err(error("Attach one network session before runtime startup"));
    }
    Ok(())
}
fn initial_report(role: &str, security: TransportSecurity, mode: NetworkMode) -> NetworkStatus {
    NetworkStatus {
        role: role.into(),
        security,
        pending_peers: 0,
        connected: false,
        peer_id: None,
        peers: vec![],
        latest_snapshot_tick: None,
        accepted_inputs: 0,
        rejected_inputs: 0,
        remote_entities: 0,
        last_error: None,
        mode,
        transport: None,
        conditions: None,
        peer_count: 0,
        links: vec![],
        replication: None,
        counters: None,
        voice: None,
        lag_comp: None,
    }
}

fn attach(runtime: &mut Runtime, endpoint: Endpoint, settings: NetworkSettings) -> Result<()> {
    check_attachable(runtime, &settings)?;
    let templates = if matches!(endpoint, Endpoint::Client(_)) {
        collect_templates(runtime)?
    } else {
        BTreeMap::new()
    };
    let (role, security) = match &endpoint {
        Endpoint::Server(server) => ("server", server.security()),
        Endpoint::Client(client) => ("client", client.security()),
        _ => unreachable!("legacy attach receives TCP endpoints only"),
    };
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(NetworkSession {
            endpoint: Some(endpoint),
            settings,
            templates,
            snapshot_age_ticks: 0.0,
            report: initial_report(role, security, NetworkMode::Snapshot),
            delta: None,
        });
    Ok(())
}

fn attach_lane(
    runtime: &mut Runtime,
    endpoint: Box<dyn MessageEndpoint + Send>,
    settings: NetworkSettings,
    server: bool,
) -> Result<()> {
    check_attachable(runtime, &settings)?;
    let templates = if server {
        BTreeMap::new()
    } else {
        collect_templates(runtime)?
    };
    let lane = lane::LaneEndpoint::new(endpoint, settings.conditions.clone()).map_err(transport)?;
    let mut report = initial_report(
        if server { "server" } else { "client" },
        lane.security(),
        NetworkMode::Delta,
    );
    report.transport = Some(lane.transport());
    report.conditions.clone_from(&settings.conditions);
    let delta = delta::Delta::new(&settings, server)?;
    let lane = Mutex::new(lane);
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(NetworkSession {
            endpoint: Some(if server {
                Endpoint::LaneServer(lane)
            } else {
                Endpoint::LaneClient(lane)
            }),
            settings,
            templates,
            snapshot_age_ticks: 0.0,
            report,
            delta: Some(delta),
        });
    Ok(())
}

/// Replace the link simulation of a delta-mode session (`None` removes it). Events still held
/// by the previous simulator are discarded. Authored defaults live in the project's
/// `engine.NetworkSession` component (settable through MCP before a headless run).
pub fn set_conditions(world: &mut World, conditions: Option<NetworkConditions>) -> Result<()> {
    if let Some(conditions) = &conditions {
        conditions.validate().map_err(transport)?;
    }
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    let (is_server, lane) = match session.endpoint.take() {
        Some(Endpoint::LaneServer(l)) => (true, l),
        Some(Endpoint::LaneClient(l)) => (false, l),
        other => {
            session.endpoint = other;
            return Err(error("Conditions require a delta-mode session"));
        }
    };
    let lane = lane.into_inner().unwrap_or_else(PoisonError::into_inner);
    let lane = lane
        .with_conditions(conditions.clone())
        .map_err(transport)?;
    let lane = Mutex::new(lane);
    session.endpoint = Some(if is_server {
        Endpoint::LaneServer(lane)
    } else {
        Endpoint::LaneClient(lane)
    });
    session.settings.conditions.clone_from(&conditions);
    session.report.conditions = conditions;
    Ok(())
}

pub fn assign_owner(world: &mut World, entity_id: &str, peer_id: PeerId) -> Result<()> {
    let entity = crate::services::find_entity(world, entity_id)
        .ok_or_else(|| error("Unknown replicated entity"))?;
    if !world
        .get::<NetworkReplica>(entity)
        .is_some_and(|r| r.authority == ReplicaAuthority::Server)
    {
        return Err(error(
            "Input ownership requires a server-authoritative replica",
        ));
    }
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    if let Some(delta::Delta {
        side: delta::Side::Server(server),
        ..
    }) = session.delta.as_mut()
    {
        return delta::assign_owner(server, entity_id, peer_id);
    }
    match session.endpoint.as_mut() {
        Some(Endpoint::Server(server)) => {
            server.assign_owner(entity_id, peer_id).map_err(transport)
        }
        _ => Err(error("Input ownership requires a server session")),
    }
}
/// Revoke a grant before deleting or reassigning a replicated gameplay object.
pub fn revoke_owner(world: &mut World, entity_id: &str) -> Result<()> {
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    if let Some(delta::Delta {
        side: delta::Side::Server(server),
        ..
    }) = session.delta.as_mut()
    {
        delta::revoke_owner(server, entity_id);
        return Ok(());
    }
    match session.endpoint.as_mut() {
        Some(Endpoint::Server(server)) => {
            server.revoke_owner(entity_id);
            Ok(())
        }
        _ => Err(error("Input ownership requires a server session")),
    }
}
/// Host policy can revoke an admitted identity immediately, removing all of its ownership grants.
pub fn disconnect_peer(world: &mut World, peer_id: PeerId) -> Result<bool> {
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    let NetworkSession {
        endpoint,
        delta,
        report,
        ..
    } = &mut *session;
    if let (
        Some(Endpoint::LaneServer(lane)),
        Some(delta::Delta {
            side: delta::Side::Server(server),
            ..
        }),
    ) = (endpoint.as_mut(), delta.as_mut())
    {
        let removed = lane
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .disconnect(peer_id);
        delta::disconnect_peer(server, peer_id);
        report.peers.retain(|id| *id != peer_id);
        report.connected = !report.peers.is_empty();
        return Ok(removed);
    }
    match endpoint.as_mut() {
        Some(Endpoint::Server(server)) => {
            let removed = server.disconnect(peer_id);
            let pending = server.pending_peers();
            report.peers.retain(|id| *id != peer_id);
            report.pending_peers = pending;
            report.connected = !report.peers.is_empty();
            Ok(removed)
        }
        _ => Err(error("Disconnecting a peer requires a server session")),
    }
}
pub fn send_input(world: &mut World, command: InputCommand) -> Result<()> {
    let delta_client = world
        .get_resource::<NetworkSession>()
        .is_some_and(|s| matches!(s.endpoint, Some(Endpoint::LaneClient(_))));
    if delta_client {
        return delta::send_lane_input(world, command);
    }
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    match session.endpoint.as_mut() {
        Some(Endpoint::Client(client)) => client.send_input(command).map_err(transport),
        _ => Err(error("Sending input requires a client session")),
    }
}
fn emit(
    world: &mut World,
    name: &str,
    target: Option<Entity>,
    payload: serde_json::Value,
) -> Result<()> {
    crate::emit_event(
        world,
        EngineEvent {
            name: name.into(),
            source: None,
            target,
            payload,
        },
    )
}
fn with_session(
    world: &mut World,
    operation: impl FnOnce(&mut World, &mut NetworkSession) -> Result<()>,
) -> Result<()> {
    let Some(mut session) = world.remove_resource::<NetworkSession>() else {
        return Ok(());
    };
    let result = operation(world, &mut session);
    if let Err(error) = &result {
        session.report.last_error = Some(error.to_string());
    }
    world.insert_resource(session);
    result
}

/// Validate before startup callbacks, as well as each tick after native changes.
pub fn validate(world: &World) -> Result<()> {
    for entity in world
        .iter_entities()
        .filter(|e| e.contains::<NetworkReplica>())
    {
        validate_replica(world, entity.id())?;
    }
    Ok(())
}

/// Delta mode: runs `step` against the lane endpoint. Returns `false` for legacy sessions.
fn lane_step(
    world: &mut World,
    session: &mut NetworkSession,
    step: impl FnOnce(&mut delta::Delta, &mut delta::Cx<'_>) -> Result<()>,
) -> Result<bool> {
    let NetworkSession {
        endpoint,
        settings,
        templates,
        report,
        delta,
        ..
    } = session;
    let (Some(ep), Some(d)) = (lane_mut(endpoint), delta.as_mut()) else {
        return Ok(false);
    };
    let mut cx = delta::Cx {
        world,
        ep,
        settings,
        report,
        templates,
    };
    step(d, &mut cx)?;
    Ok(true)
}

/// Fills the report from the endpoint and replication state (bounded by `max_reported_peers`).
fn refresh_report(session: &mut NetworkSession) {
    let NetworkSession {
        endpoint,
        settings,
        report,
        delta,
        ..
    } = session;
    let (Some(ep), Some(d)) = (lane_mut(endpoint), delta.as_mut()) else {
        return;
    };
    let cap = settings.max_reported_peers;
    let connected = ep.connected_peers();
    report.peer_count = connected.len();
    report.peers = connected.iter().copied().take(cap).collect();
    report.links = connected
        .iter()
        .take(cap)
        .filter_map(|p| {
            ep.stats(*p)
                .map(|link| PeerLinkStatus { peer_id: *p, link })
        })
        .collect();
    report.counters = Some(d.c.clone());
    let schema_hash = format!("{:016x}", d.registry.schema_hash());
    match &mut d.side {
        delta::Side::Server(server) => {
            let telemetry = server.rs.telemetry();
            report.replication = Some(ReplicationStatus {
                schema_hash,
                tick: telemetry.tick,
                entities: telemetry.entities,
                peers_truncated: telemetry.peers.len() > cap,
                peers: telemetry.peers.into_iter().take(cap).collect(),
                client: None,
            });
            report.voice = server.voice.as_ref().map(voice::ServerVoice::status);
            report.lag_comp = Some(server.lag.status.clone());
        }
        delta::Side::Client(client) => {
            let telemetry = client.rc.telemetry();
            report.replication = Some(ReplicationStatus {
                schema_hash,
                tick: telemetry.last_server_tick,
                entities: telemetry.entities,
                peers: vec![],
                peers_truncated: false,
                client: Some(telemetry),
            });
            report.voice = client.voice.as_mut().map(voice::ClientVoice::status);
        }
    }
}

/// Runtime calls this before event dispatch and scripts, after component hydration.
pub fn before_step(world: &mut World, dt: f32) -> Result<()> {
    with_session(world, |world, session| {
        validate(world)?;
        let handled = lane_step(world, session, |d, cx| {
            d.now_ms += f64::from(dt) * 1000.0;
            let now_ms = d.now_ms;
            let delta::Delta { ids, side, c, .. } = &mut *d;
            let mut pending = Vec::new();
            match side {
                delta::Side::Server(server) => {
                    server.before_step(ids, now_ms, &mut pending, c, cx)?;
                }
                delta::Side::Client(client) => {
                    client.before_step(ids, now_ms, dt, &mut pending, c, cx)?;
                }
            }
            for payload in pending {
                d.push_inbox(payload);
            }
            Ok(())
        })?;
        if handled {
            refresh_report(session);
            return Ok(());
        }
        match session.endpoint.as_mut() {
            Some(Endpoint::Server(server)) => {
                let events = server
                    .poll(session.settings.poll_budget)
                    .map_err(transport)?;
                for event in events {
                    match event {
                        ServerEvent::Authenticated(peer_id) => emit(
                            world,
                            "network.authenticated",
                            None,
                            serde_json::json!({"peer_id":peer_id}),
                        )?,
                        ServerEvent::Connected(peer_id) => emit(
                            world,
                            "network.connected",
                            None,
                            serde_json::json!({"peer_id":peer_id}),
                        )?,
                        ServerEvent::Disconnected { peer_id, reason } => {
                            session.report.last_error = Some(reason.clone());
                            emit(
                                world,
                                "network.disconnected",
                                None,
                                serde_json::json!({"peer_id":peer_id,"reason":reason}),
                            )?;
                        }
                        ServerEvent::Input { peer_id, command } => {
                            let target = crate::services::find_entity(world, &command.entity_id);
                            // A callback may have despawned/replaced a previously authorized entity.
                            if target.is_some_and(|entity| {
                                world
                                    .get::<NetworkReplica>(entity)
                                    .is_some_and(|r| r.authority == ReplicaAuthority::Server)
                            }) {
                                session.report.accepted_inputs =
                                    session.report.accepted_inputs.saturating_add(1);
                                emit(
                                    world,
                                    "network.input",
                                    target,
                                    serde_json::json!({"peer_id":peer_id,"tick":command.tick,"entity_id":command.entity_id,"movement":command.movement,"buttons":command.buttons}),
                                )?;
                            } else {
                                session.report.rejected_inputs =
                                    session.report.rejected_inputs.saturating_add(1);
                                server.revoke_owner(&command.entity_id);
                                emit(
                                    world,
                                    "network.input_rejected",
                                    None,
                                    serde_json::json!({"peer_id":peer_id,"reason":"replica no longer exists"}),
                                )?;
                            }
                        }
                        ServerEvent::InputRejected { peer_id, reason } => {
                            session.report.rejected_inputs =
                                session.report.rejected_inputs.saturating_add(1);
                            emit(
                                world,
                                "network.input_rejected",
                                None,
                                serde_json::json!({"peer_id":peer_id,"reason":reason}),
                            )?;
                        }
                    }
                }
                session.report.peers = server.peer_ids().collect();
                session.report.pending_peers = server.pending_peers();
                session.report.connected = !session.report.peers.is_empty();
            }
            Some(Endpoint::Client(client)) => {
                session.snapshot_age_ticks += f64::from(dt) / session.settings.tick_seconds;
                for event in client
                    .poll(session.settings.poll_budget)
                    .map_err(transport)?
                {
                    match event {
                        ClientEvent::Authenticated(peer_id) => emit(
                            world,
                            "network.authenticated",
                            None,
                            serde_json::json!({"peer_id":peer_id}),
                        )?,
                        ClientEvent::Connected(peer_id) => emit(
                            world,
                            "network.connected",
                            None,
                            serde_json::json!({"peer_id":peer_id}),
                        )?,
                        ClientEvent::Disconnected(reason) => {
                            session.report.last_error = Some(reason.clone());
                            emit(
                                world,
                                "network.disconnected",
                                None,
                                serde_json::json!({"reason":reason}),
                            )?;
                        }
                        ClientEvent::Snapshot(tick) => {
                            session.report.latest_snapshot_tick = Some(tick);
                            session.snapshot_age_ticks = 0.0;
                            emit(
                                world,
                                "network.snapshot",
                                None,
                                serde_json::json!({"tick":tick}),
                            )?;
                        }
                    }
                }
                session.report.connected = client.is_connected();
                session.report.peer_id = client.peer_id();
                if client.is_connected() {
                    client.send_heartbeat().map_err(transport)?;
                }
                if let Some(latest) = client.state().latest() {
                    let render_tick = latest.tick as f64
                        - session.settings.interpolation_delay_ticks
                        + session.snapshot_age_ticks;
                    if let Some(snapshot) = client.state().sample(render_tick) {
                        apply_remote(world, &session.templates, &snapshot)?;
                    }
                }
                session.report.remote_entities = session
                    .templates
                    .keys()
                    .filter(|id| crate::services::find_entity(world, id).is_some())
                    .count();
            }
            _ => {}
        }
        Ok(())
    })
}

/// Spawns a remote replica from its authored template.
pub(crate) fn spawn_remote(world: &mut World, template: &SceneEntity) -> Result<Entity> {
    let entity = crate::runtime::spawn(world, template);
    let project = world.resource::<ProjectResource>().0.clone();
    let mut scene: Scene = project.scene.clone();
    scene.entities = vec![template.clone()];
    crate::catalog::hydrate(world, &scene, &project.component_catalog)?;
    validate_replica(world, entity)?;
    Ok(entity)
}
/// Despawns a remote replica and forgets its lookup entry.
pub(crate) fn despawn_remote(world: &mut World, id: &str) -> Result<()> {
    if let Some(entity) = crate::services::find_entity(world, id) {
        if !is_remote_replica(world, entity) {
            return Err(error("Remote template ID now belongs to a local entity"));
        }
        validate_replica(world, entity)?;
        world.despawn(entity);
        world.resource_mut::<EntityLookup>().0.remove(id);
    }
    Ok(())
}

fn apply_remote(
    world: &mut World,
    templates: &BTreeMap<String, SceneEntity>,
    snapshot: &Snapshot,
) -> Result<()> {
    // Validate every affected pose/owner before removing or moving any entity.
    for id in templates.keys() {
        if let Some(entity) = crate::services::find_entity(world, id) {
            if !is_remote_replica(world, entity) {
                return Err(error("Remote template ID now belongs to a local entity"));
            }
            validate_replica(world, entity)?;
        }
    }
    for remote in &snapshot.entities {
        if templates.contains_key(&remote.id) {
            crate::transforms::local_matrix(&Transform {
                position: remote.translation,
                rotation: remote.rotation,
                scale: remote.scale,
                ..Transform::default()
            })?;
        }
    }
    let present: BTreeSet<_> = snapshot.entities.iter().map(|e| e.id.as_str()).collect();
    for id in templates.keys() {
        if !present.contains(id.as_str())
            && let Some(entity) = crate::services::find_entity(world, id)
        {
            if !is_remote_replica(world, entity) {
                return Err(error("Remote template ID now belongs to a local entity"));
            }
            validate_replica(world, entity)?;
            world.despawn(entity);
            world.resource_mut::<EntityLookup>().0.remove(id);
        }
    }
    for remote in &snapshot.entities {
        let Some(template) = templates.get(&remote.id) else {
            continue;
        };
        let entity = if let Some(entity) = crate::services::find_entity(world, &remote.id) {
            if !is_remote_replica(world, entity) {
                return Err(error("Remote template ID now belongs to a local entity"));
            }
            validate_replica(world, entity)?;
            entity
        } else {
            let entity = crate::runtime::spawn(world, template);
            let project = world.resource::<ProjectResource>().0.clone();
            let mut scene: Scene = project.scene.clone();
            scene.entities = vec![template.clone()];
            crate::catalog::hydrate(world, &scene, &project.component_catalog)?;
            validate_replica(world, entity)?;
            entity
        };
        let transform = Transform {
            position: remote.translation,
            rotation: remote.rotation,
            scale: remote.scale,
            ..Transform::default()
        };
        crate::transforms::local_matrix(&transform)?;
        world
            .entity_mut(entity)
            .insert((transform, NetworkVelocity(remote.velocity)));
    }
    crate::refresh_world_transforms(world)
}

/// Runtime calls after simulation so snapshots contain the authoritative final poses.
pub fn after_step(world: &mut World, tick: u64) -> Result<()> {
    with_session(world, |world, session| {
        let handled = lane_step(world, session, |d, cx| {
            let delta::Delta { ids, side, c, .. } = d;
            if let delta::Side::Server(server) = side {
                server.after_step(ids, c, cx, tick)?;
            }
            Ok(())
        })?;
        if handled {
            refresh_report(session);
            return Ok(());
        }
        let Some(Endpoint::Server(server)) = session.endpoint.as_mut() else {
            return Ok(());
        };
        let mut entities = Vec::new();
        for entity in world.iter_entities() {
            if !entity
                .get::<NetworkReplica>()
                .is_some_and(|r| r.authority == ReplicaAuthority::Server)
            {
                continue;
            }
            validate_replica(world, entity.id())?;
            let id = entity
                .get::<AuthoredId>()
                .ok_or_else(|| error("Replica lacks stable ID"))?
                .id
                .clone();
            let transform = entity
                .get::<Transform>()
                .ok_or_else(|| error("Replica lacks Transform"))?;
            crate::transforms::local_matrix(transform)?;
            let rotation = glam::Quat::from_array(transform.rotation)
                .normalize()
                .to_array();
            entities.push(EntitySnapshot {
                id,
                translation: transform.position,
                rotation,
                scale: transform.scale,
                velocity: entity
                    .get::<NetworkVelocity>()
                    .copied()
                    .unwrap_or_default()
                    .0,
            });
        }
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        server
            .publish_snapshot(Snapshot { tick, entities })
            .map_err(transport)?;
        session.report.latest_snapshot_tick = Some(tick);
        Ok(())
    })
}

/// Close sockets while retaining a report for MCP/headless diagnostics.
pub fn shutdown(world: &mut World) {
    if let Some(mut session) = world.get_resource_mut::<NetworkSession>() {
        session.endpoint = None;
        session.report.connected = false;
        session.report.peers.clear();
        session.report.links.clear();
        session.report.peer_count = 0;
        session.report.pending_peers = 0;
        session.report.peer_id = None;
    }
}
