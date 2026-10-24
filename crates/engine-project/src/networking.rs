//! Explicit runtime attachment of trusted-network sessions; authored data never opens sockets.
use crate::{
    ComponentDescriptor, EngineEvent, Error, Result, Runtime, Scene, SceneEntity, Transform,
    runtime::{AuthoredId, EntityLookup, OwnedBy, ProjectResource},
};
use bevy_ecs::prelude::*;
use engine_network::{
    Client, ClientEvent, EntitySnapshot, InputCommand, PeerId, PollBudget, Server, ServerEvent,
    Snapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const REPLICA_COMPONENT: &str = "engine.NetworkReplica";
const POSE_WRITERS: &[&str] = &[
    "engine.PhysicsBody2D",
    "engine.PhysicsBody3D",
    "engine.AnimationPlayer",
    "engine.NavigationAgent",
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
pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<NetworkReplica>(app, REPLICA_COMPONENT, 1)
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
#[derive(Clone, Debug)]
pub struct NetworkSettings {
    pub poll_budget: PollBudget,
    /// Used only to advance rendering between snapshot arrivals, in simulation ticks.
    pub tick_seconds: f64,
    pub interpolation_delay_ticks: f64,
}
impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            poll_budget: PollBudget::default(),
            tick_seconds: 1.0 / 60.0,
            interpolation_delay_ticks: 1.0,
        }
    }
}
impl NetworkSettings {
    fn validate(&self) -> Result<()> {
        if !self.tick_seconds.is_finite()
            || self.tick_seconds <= 0.0
            || !self.interpolation_delay_ticks.is_finite()
            || self.interpolation_delay_ticks < 0.0
        {
            return Err(error(
                "Require positive tick_seconds and nonnegative interpolation delay",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct NetworkStatus {
    pub role: String,
    pub connected: bool,
    pub peer_id: Option<PeerId>,
    pub peers: Vec<PeerId>,
    pub latest_snapshot_tick: Option<u64>,
    pub accepted_inputs: u64,
    pub rejected_inputs: u64,
    pub remote_entities: usize,
    pub last_error: Option<String>,
}
enum Endpoint {
    Server(Server),
    Client(Box<Client>),
}
#[derive(Resource)]
pub struct NetworkSession {
    endpoint: Option<Endpoint>,
    settings: NetworkSettings,
    templates: BTreeMap<String, SceneEntity>,
    snapshot_age_ticks: f64,
    report: NetworkStatus,
}
impl NetworkSession {
    pub fn status(&self) -> &NetworkStatus {
        &self.report
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

/// Attach a caller-created server; neither loading a scene nor reading metadata binds a port.
pub fn attach_server(
    runtime: &mut Runtime,
    server: Server,
    settings: NetworkSettings,
) -> Result<()> {
    attach(runtime, Endpoint::Server(server), settings)
}
/// Attach a caller-created client. Only locally authored Remote templates are eligible to spawn.
pub fn attach_client(
    runtime: &mut Runtime,
    client: Client,
    settings: NetworkSettings,
) -> Result<()> {
    attach(runtime, Endpoint::Client(Box::new(client)), settings)
}
fn attach(runtime: &mut Runtime, endpoint: Endpoint, settings: NetworkSettings) -> Result<()> {
    settings.validate()?;
    if runtime.running() || runtime.app().world().contains_resource::<NetworkSession>() {
        return Err(error("Attach one network session before runtime startup"));
    }
    let mut templates = BTreeMap::new();
    if matches!(endpoint, Endpoint::Client(_)) {
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
    }
    let role = if matches!(endpoint, Endpoint::Server(_)) {
        "server"
    } else {
        "client"
    }
    .into();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(NetworkSession {
            endpoint: Some(endpoint),
            settings,
            templates,
            snapshot_age_ticks: 0.0,
            report: NetworkStatus {
                role,
                connected: false,
                peer_id: None,
                peers: vec![],
                latest_snapshot_tick: None,
                accepted_inputs: 0,
                rejected_inputs: 0,
                remote_entities: 0,
                last_error: None,
            },
        });
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
    match session.endpoint.as_mut() {
        Some(Endpoint::Server(server)) => {
            server.revoke_owner(entity_id);
            Ok(())
        }
        _ => Err(error("Input ownership requires a server session")),
    }
}
pub fn send_input(world: &mut World, command: InputCommand) -> Result<()> {
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
/// Runtime calls this before event dispatch and scripts, after component hydration.
pub fn before_step(world: &mut World, dt: f32) -> Result<()> {
    with_session(world, |world, session| {
        validate(world)?;
        match session.endpoint.as_mut() {
            Some(Endpoint::Server(server)) => {
                let events = server
                    .poll(session.settings.poll_budget)
                    .map_err(transport)?;
                for event in events {
                    match event {
                        ServerEvent::Connected(peer_id) => emit(
                            world,
                            "network.connected",
                            None,
                            serde_json::json!({"peer_id":peer_id}),
                        )?,
                        ServerEvent::Disconnected { peer_id, reason } => emit(
                            world,
                            "network.disconnected",
                            None,
                            serde_json::json!({"peer_id":peer_id,"reason":reason}),
                        )?,
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
                session.report.connected = !session.report.peers.is_empty();
            }
            Some(Endpoint::Client(client)) => {
                session.snapshot_age_ticks += f64::from(dt) / session.settings.tick_seconds;
                for event in client
                    .poll(session.settings.poll_budget)
                    .map_err(transport)?
                {
                    match event {
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
            None => {}
        }
        Ok(())
    })
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
        session.report.peer_id = None;
    }
}
