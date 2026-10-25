//! Authored network components: `engine.NetworkReplication`, `engine.NetworkHitboxes`,
//! `engine.NetworkVoice` and the singleton `engine.NetworkSession`.
//!
//! Every type is `#[serde(default, deny_unknown_fields)]` and is checked twice: by the
//! declarative catalog schema in [`descriptors`] and by the typed `validate()` methods.
use super::settings::NetworkSessionConfig;
use crate::{ComponentDescriptor, Error, Result, Scene};
use bevy_ecs::prelude::Component;
use engine_net_predict::{HitShape, Hitbox};
use engine_net_replication::EntityRelevance;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const REPLICATION_COMPONENT: &str = "engine.NetworkReplication";
pub const HITBOXES_COMPONENT: &str = "engine.NetworkHitboxes";
pub const VOICE_COMPONENT: &str = "engine.NetworkVoice";
pub const SESSION_COMPONENT: &str = "engine.NetworkSession";
/// Every component name added by the delta stack (`engine.NetworkReplica` is separate).
pub const NAMES: [&str; 4] = [
    HITBOXES_COMPONENT,
    REPLICATION_COMPONENT,
    SESSION_COMPONENT,
    VOICE_COMPONENT,
];

fn invalid(name: &str, message: impl std::fmt::Display) -> Error {
    Error::new("network.component.invalid", format!("{name}: {message}"))
}
fn finite_in(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}

/// Which peers an entity is relevant to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceRule {
    /// Every peer, regardless of position.
    #[default]
    Always,
    /// Peers whose view sphere contains the entity.
    Spatial,
    /// The owning peer only (the entity is not replicated until it has an owner).
    OwnerOnly,
}
impl From<RelevanceRule> for EntityRelevance {
    fn from(rule: RelevanceRule) -> Self {
        match rule {
            RelevanceRule::Always => Self::Always,
            RelevanceRule::Spatial => Self::Spatial,
            RelevanceRule::OwnerOnly => Self::OwnerOnly,
        }
    }
}

/// Per-entity delta replication rules of a server replica. Changing them at runtime respawns
/// the entity for its peers.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkReplication {
    /// Bandwidth weight, snapped to the nearest of 0.25, 0.5, 1, 2, 4 or 8.
    pub priority: f32,
    pub relevance: RelevanceRule,
    /// Cap on pose updates per second per peer; 0 means every tick.
    pub max_update_hz: f32,
}
impl Default for NetworkReplication {
    fn default() -> Self {
        Self {
            priority: 1.0,
            relevance: RelevanceRule::Always,
            max_update_hz: 0.0,
        }
    }
}
impl NetworkReplication {
    pub fn validate(&self) -> Result<()> {
        if !finite_in(self.priority, 0.1, 16.0) {
            return Err(invalid(
                REPLICATION_COMPONENT,
                "priority must be in 0.1..=16",
            ));
        }
        if self.max_update_hz != 0.0 && !finite_in(self.max_update_hz, 0.5, 240.0) {
            return Err(invalid(
                REPLICATION_COMPONENT,
                "max_update_hz must be 0 or in 0.5..=240",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitboxShape {
    #[default]
    Sphere,
    Capsule,
    Box,
}

/// One hit volume in entity space. Used fields depend on `shape`: sphere (`center`, `radius`),
/// capsule (`a`, `b`, `radius`), box (`center`, `half_extents`, `rotation`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HitboxSpec {
    pub shape: HitboxShape,
    /// Part tag reported with hits (head, torso, ...).
    pub part: u16,
    pub center: [f32; 3],
    pub radius: f32,
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub half_extents: [f32; 3],
    /// Box rotation quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
}
impl Default for HitboxSpec {
    fn default() -> Self {
        Self {
            shape: HitboxShape::Sphere,
            part: 0,
            center: [0.0; 3],
            radius: 0.5,
            a: [0.0, -0.5, 0.0],
            b: [0.0, 0.5, 0.0],
            half_extents: [0.5; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }
}
impl HitboxSpec {
    fn validate(&self) -> Result<()> {
        let point = |p: &[f32; 3]| p.iter().all(|v| finite_in(*v, -1.0e6, 1.0e6));
        let ok = match self.shape {
            HitboxShape::Sphere => point(&self.center) && finite_in(self.radius, 1.0e-4, 1.0e6),
            HitboxShape::Capsule => {
                point(&self.a) && point(&self.b) && finite_in(self.radius, 1.0e-4, 1.0e6)
            }
            HitboxShape::Box => {
                point(&self.center)
                    && self
                        .half_extents
                        .iter()
                        .all(|v| finite_in(*v, 1.0e-4, 1.0e6))
                    && self.rotation.iter().all(|v| v.is_finite())
                    && Quat::from_array(self.rotation).length() > 1.0e-3
            }
        };
        if ok {
            Ok(())
        } else {
            Err(invalid(
                HITBOXES_COMPONENT,
                "hitbox dimensions are not valid",
            ))
        }
    }
    pub fn hitbox(&self) -> Hitbox {
        let shape = match self.shape {
            HitboxShape::Sphere => HitShape::Sphere {
                center: Vec3::from_array(self.center),
                radius: self.radius,
            },
            HitboxShape::Capsule => HitShape::Capsule {
                a: Vec3::from_array(self.a),
                b: Vec3::from_array(self.b),
                radius: self.radius,
            },
            HitboxShape::Box => HitShape::OrientedBox {
                center: Vec3::from_array(self.center),
                half_extents: Vec3::from_array(self.half_extents),
                rotation: Quat::from_array(self.rotation).normalize(),
            },
        };
        Hitbox {
            shape,
            part: self.part,
        }
    }
}

/// Lag-compensation hit volumes. The server records the entity pose every tick and
/// `lag_compensated_raycast` rewinds against them.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkHitboxes {
    /// Layer bits matched by query masks; must be non-zero.
    pub layers: u32,
    pub hitboxes: Vec<HitboxSpec>,
}
impl Default for NetworkHitboxes {
    fn default() -> Self {
        Self {
            layers: 1,
            hitboxes: vec![HitboxSpec::default()],
        }
    }
}
impl NetworkHitboxes {
    pub fn validate(&self) -> Result<()> {
        if self.layers == 0 || self.hitboxes.is_empty() || self.hitboxes.len() > 32 {
            return Err(invalid(
                HITBOXES_COMPONENT,
                "layers must be non-zero with 1 to 32 hitboxes",
            ));
        }
        self.hitboxes.iter().try_for_each(HitboxSpec::validate)
    }
    pub fn to_hitboxes(&self) -> Vec<Hitbox> {
        self.hitboxes.iter().map(HitboxSpec::hitbox).collect()
    }
}

/// Voice participant: the owning peer's server-side voice position follows this entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkVoice {
    /// Channel the owning peer joins.
    pub channel: u8,
    /// Narrows the session hearing distance for this speaker; 0 uses the session distance.
    pub proximity_distance: f32,
}
impl Default for NetworkVoice {
    fn default() -> Self {
        Self {
            channel: 0,
            proximity_distance: 0.0,
        }
    }
}
impl NetworkVoice {
    pub fn validate(&self) -> Result<()> {
        if self.proximity_distance != 0.0 && !finite_in(self.proximity_distance, 0.01, 1.0e6) {
            return Err(invalid(
                VOICE_COMPONENT,
                "proximity_distance must be 0 or in 0.01..=1e6",
            ));
        }
        Ok(())
    }
}

/// Singleton session defaults.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkSessionSettings(pub NetworkSessionConfig);

fn number(min: f64, max: f64, description: &str) -> Value {
    json!({"type":"number","minimum":min,"maximum":max,"description":description})
}
fn integer(min: u64, max: u64, description: &str) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max,"description":description})
}
fn boolean(description: &str) -> Value {
    json!({"type":"boolean","description":description})
}
fn choice(values: &[&str], description: &str) -> Value {
    json!({"type":"string","enum":values,"description":description})
}
fn vector(len: u64, bound: f64, description: &str) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":-bound,"maximum":bound},"minItems":len,"maxItems":len,"description":description})
}
fn object(properties: Value, description: &str) -> Value {
    json!({"type":"object","properties":properties,"additionalProperties":false,"description":description})
}
fn descriptor(
    name: &str,
    defaults: impl Serialize,
    schema: Value,
    requires: &[&str],
) -> ComponentDescriptor {
    ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema,
        defaults: serde_json::to_value(defaults).expect("finite builtin defaults"),
        requires: requires.iter().map(|s| (*s).into()).collect(),
        entity_references: vec![],
        data_only: false,
    }
}

/// Catalog descriptors chained into [`crate::ComponentCatalog::new`].
pub fn descriptors() -> Vec<ComponentDescriptor> {
    let hitbox = object(
        json!({
            "shape": choice(&["sphere","capsule","box"], "sphere uses center and radius; capsule a, b and radius; box center, half_extents and rotation."),
            "part": integer(0, 65535, "Part tag reported with hits."),
            "center": vector(3, 1.0e6, "Sphere or box center in entity space."),
            "radius": number(0.0001, 1.0e6, "Sphere or capsule radius."),
            "a": vector(3, 1.0e6, "Capsule segment start."),
            "b": vector(3, 1.0e6, "Capsule segment end."),
            "half_extents": json!({"type":"array","items":{"type":"number","minimum":0.0001,"maximum":1.0e6},"minItems":3,"maxItems":3,"description":"Box half extents."}),
            "rotation": vector(4, 1.0e6, "Box rotation quaternion [x, y, z, w]."),
        }),
        "One hit volume in entity space.",
    );
    let conditions = object(
        json!({
            "loss": number(0.0, 1.0, "Probability an unreliable item is dropped."),
            "latency_ms": integer(0, 10_000, "Fixed one-way delay."),
            "jitter_ms": integer(0, 10_000, "Extra uniform delay in [0, jitter_ms]."),
            "duplicate": number(0.0, 1.0, "Probability an unreliable item is delivered twice."),
            "reorder": number(0.0, 1.0, "Probability an item is held back so later ones overtake it."),
            "bandwidth_bytes_per_second": integer(1, 1u64 << 40, "Simulated link rate; absent is unlimited."),
            "seed": integer(0, u64::MAX >> 11, "Deterministic simulation seed."),
        }),
        "Simulated link impairment applied to received traffic in delta mode; all zero disables it.",
    );
    vec![
        descriptor(
            HITBOXES_COMPONENT,
            NetworkHitboxes::default(),
            object(
                json!({
                    "layers": integer(1, u32::MAX as u64, "Layer bits matched by lag-compensated query masks."),
                    "hitboxes": json!({"type":"array","items":hitbox,"minItems":1,"maxItems":32,"description":"Hit volumes in entity space, recorded with the entity pose every server tick."}),
                }),
                "Lag-compensation hit volumes. Requires Transform.",
            ),
            &["Transform"],
        ),
        descriptor(
            REPLICATION_COMPONENT,
            NetworkReplication::default(),
            object(
                json!({
                    "priority": number(0.1, 16.0, "Bandwidth weight, snapped to 0.25, 0.5, 1, 2, 4 or 8."),
                    "relevance": choice(&["always","spatial","owner_only"], "always: every peer; spatial: peers whose view sphere contains the entity; owner_only: the owner."),
                    "max_update_hz": number(0.0, 240.0, "Pose update cap per second; 0 sends every tick."),
                }),
                "Delta-mode replication rules of a server replica. Requires engine.NetworkReplica.",
            ),
            &[super::REPLICA_COMPONENT],
        ),
        descriptor(
            SESSION_COMPONENT,
            NetworkSessionConfig::default(),
            object(
                json!({
                    "mode": choice(&["snapshot","delta"], "snapshot: legacy whole snapshots over TCP; delta: engine-net-replication over any transport."),
                    "conditions": conditions,
                    "bytes_per_second": integer(1, 1u64 << 32, "Per-peer replication bandwidth budget."),
                    "view_radius": number(0.0, 1.0e6, "Default interest radius of a peer."),
                    "max_entities": integer(1, 65_536, "Most replicated entities; use the same value on every endpoint."),
                    "rpc_calls_per_second": integer(1, 10_000, "Per-peer client-to-server RPC rate cap."),
                    "property_events": boolean("Also emit network.property_changed for pose properties."),
                    "remote_smoothing_hz": number(0.0, 1000.0, "Client pose smoothing rate; 0 snaps."),
                    "lag_rewind_ticks": integer(1, 4096, "Oldest tick a shooter may claim to see."),
                    "history_ticks": integer(1, 4096, "Ticks of hitbox history retained."),
                    "voice": object(json!({
                        "enabled": boolean("Create the voice router and receiver."),
                        "sample_rate": integer(8000, 48000, "Raw PCM sample rate."),
                        "frame_ms": integer(5, 60, "Raw PCM frame duration."),
                        "routing": choice(&["global","channel","proximity","channel_proximity"], "Who hears a speaker."),
                        "max_distance": number(0.01, 1.0e6, "Hearing distance of the proximity routings."),
                    }), "Voice chat defaults; capture and codecs stay native."),
                }),
                "Singleton session defaults read by NetworkSettings::from_scene and set by the MCP network_settings_set tool. At most one per scene; no Transform required. Keys and certificates never belong here.",
            ),
            &[],
        ),
        descriptor(
            VOICE_COMPONENT,
            NetworkVoice::default(),
            object(
                json!({
                    "channel": integer(0, 255, "Voice channel the owning peer joins."),
                    "proximity_distance": number(0.0, 1.0e6, "Narrows the session hearing distance for this speaker; 0 uses the session distance."),
                }),
                "Voice participant: the owner's server voice position follows this entity. Requires Transform.",
            ),
            &["Transform"],
        ),
    ]
}

pub fn register(app: &mut engine_core::EngineApp) -> Result<()> {
    crate::register_component::<NetworkHitboxes>(app, HITBOXES_COMPONENT, 1)?;
    crate::register_component::<NetworkReplication>(app, REPLICATION_COMPONENT, 1)?;
    crate::register_component::<NetworkSessionSettings>(app, SESSION_COMPONENT, 1)?;
    crate::register_component::<NetworkVoice>(app, VOICE_COMPONENT, 1)
}

/// Typed validation of one authored component (called by the catalog).
pub fn validate_component(name: &str, data: &Value) -> Result<()> {
    fn parse<T: serde::de::DeserializeOwned>(name: &str, data: &Value) -> Result<T> {
        serde_json::from_value(data.clone()).map_err(|e| invalid(name, e))
    }
    match name {
        HITBOXES_COMPONENT => parse::<NetworkHitboxes>(name, data)?.validate(),
        REPLICATION_COMPONENT => parse::<NetworkReplication>(name, data)?.validate(),
        SESSION_COMPONENT => parse::<NetworkSessionConfig>(name, data)?.validate(),
        VOICE_COMPONENT => parse::<NetworkVoice>(name, data)?.validate(),
        _ => Ok(()),
    }
}

/// Scene-level checks: at most one session component; replication rules need a replica.
pub fn validate_scene(scene: &Scene) -> Result<()> {
    let mut sessions = 0;
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if custom.contains_key(SESSION_COMPONENT) {
            sessions += 1;
        }
        if custom.contains_key(REPLICATION_COMPONENT)
            && !custom.contains_key(super::REPLICA_COMPONENT)
        {
            return Err(invalid(
                REPLICATION_COMPONENT,
                format!("entity {} also needs engine.NetworkReplica", entity.id),
            ));
        }
    }
    if sessions > 1 {
        return Err(Error::new(
            "network.session.multiple",
            "At most one engine.NetworkSession per scene",
        ));
    }
    Ok(())
}
