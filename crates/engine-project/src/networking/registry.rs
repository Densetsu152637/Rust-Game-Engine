//! The fixed replication schema shared by every delta-mode endpoint, and value conversions.
use super::{
    error,
    settings::{NetworkSettings, StateKind},
};
use crate::{Error, Result};
use engine_net_replication::{
    ComponentDef, ComponentId, Condition, Quantization, ReplicationError, ReplicationRegistry,
    RpcDef, RpcDirection, RpcId, StateDelivery, Value, ValueSpec,
};
use std::sync::Arc;

/// Pose priority classes; an entity's authored priority snaps to the nearest one (log scale).
pub(crate) const TIERS: [f32; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
pub(crate) const MAX_RPC_NAME: usize = 64;
pub(crate) const MAX_RPC_PAYLOAD: usize = 768;
pub(crate) const MAX_ID_BYTES: u16 = 128;
pub(crate) const MAX_STATE_TEXT: u16 = 64;

pub(crate) fn repl(error: ReplicationError) -> Error {
    Error::new("network.replication", error.to_string())
}

pub(crate) fn tier_for(priority: f32) -> usize {
    TIERS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (priority.ln() - a.ln())
                .abs()
                .total_cmp(&(priority.ln() - b.ln()).abs())
        })
        .map_or(2, |(i, _)| i)
}

/// Component and RPC identifiers resolved from the registry.
#[derive(Clone)]
pub(crate) struct Ids {
    pub(crate) id: ComponentId,
    pub(crate) pose: Vec<ComponentId>,
    pub(crate) state: Option<ComponentId>,
    pub(crate) call: RpcId,
    pub(crate) call_owned: RpcId,
    pub(crate) call_entity: RpcId,
    pub(crate) event: RpcId,
    pub(crate) event_entity: RpcId,
}
pub(crate) const POSITION: u8 = 0;
pub(crate) const ROTATION: u8 = 1;
pub(crate) const SCALE: u8 = 2;
pub(crate) const VELOCITY: u8 = 3;
pub(crate) const POSE_PROPERTIES: [&str; 4] = ["position", "rotation", "scale", "velocity"];

fn spec(kind: StateKind) -> ValueSpec {
    match kind {
        StateKind::Bool => ValueSpec::bool(),
        StateKind::I32 => ValueSpec::i32(),
        StateKind::F32 => ValueSpec::f32(),
        StateKind::String => ValueSpec::string(MAX_STATE_TEXT),
    }
}

pub(crate) fn build(settings: &NetworkSettings) -> Result<(Arc<ReplicationRegistry>, Ids)> {
    let mut builder = ReplicationRegistry::builder().component(
        ComponentDef::new("engine.id")
            .delivery(StateDelivery::Reliable)
            .condition(Condition::InitialOnly)
            .property("id", ValueSpec::string(MAX_ID_BYTES)),
    );
    for (i, priority) in TIERS.iter().enumerate() {
        builder = builder.component(
            ComponentDef::new(format!("engine.pose.{i}"))
                .delivery(StateDelivery::Unreliable)
                .priority(*priority)
                .property("position", ValueSpec::vec3())
                .property(
                    "rotation",
                    ValueSpec::quat().quantized(Quantization::SmallestThree { bits: 12 }),
                )
                .property("scale", ValueSpec::vec3())
                .property("velocity", ValueSpec::vec3()),
        );
    }
    if !settings.state_properties.is_empty() {
        let mut state = ComponentDef::new("engine.state")
            .delivery(StateDelivery::Reliable)
            .priority(2.0);
        for property in &settings.state_properties {
            state = state.property(property.name.clone(), spec(property.kind));
        }
        builder = builder.component(state);
    }
    let rate = settings.rpc_calls_per_second;
    let call = |name: &str, direction| {
        RpcDef::new(name, direction)
            .arg("name", ValueSpec::string(MAX_RPC_NAME as u16))
            .arg("payload", ValueSpec::bytes(MAX_RPC_PAYLOAD as u16))
    };
    let registry = Arc::new(
        builder
            .rpc(call("engine.call", RpcDirection::ClientToServer).max_calls_per_sec(rate))
            .rpc(
                call("engine.call_owned", RpcDirection::ClientToServer)
                    .targets_entity()
                    .requires_ownership()
                    .max_calls_per_sec(rate),
            )
            .rpc(
                call("engine.call_entity", RpcDirection::ClientToServer)
                    .targets_entity()
                    .max_calls_per_sec(rate),
            )
            .rpc(call("engine.event", RpcDirection::ServerToClient))
            .rpc(call("engine.event_entity", RpcDirection::ServerToClient).targets_entity())
            .build()
            .map_err(repl)?,
    );
    let component = |name: &str| {
        registry
            .component_id(name)
            .ok_or_else(|| error("replication schema lacks a component"))
    };
    let rpc = |name: &str| {
        registry
            .rpc_id(name)
            .ok_or_else(|| error("replication schema lacks an rpc"))
    };
    let ids = Ids {
        id: component("engine.id")?,
        pose: (0..TIERS.len())
            .map(|i| component(&format!("engine.pose.{i}")))
            .collect::<Result<_>>()?,
        state: if settings.state_properties.is_empty() {
            None
        } else {
            Some(component("engine.state")?)
        },
        call: rpc("engine.call")?,
        call_owned: rpc("engine.call_owned")?,
        call_entity: rpc("engine.call_entity")?,
        event: rpc("engine.event")?,
        event_entity: rpc("engine.event_entity")?,
    };
    Ok((registry, ids))
}

/// Replication value as JSON (state properties only use scalar kinds and text).
pub(crate) fn value_json(value: &Value) -> serde_json::Value {
    use serde_json::json;
    match value {
        Value::Bool(v) => json!(v),
        Value::U8(v) => json!(v),
        Value::U16(v) => json!(v),
        Value::U32(v) => json!(v),
        Value::U64(v) => json!(v),
        Value::I32(v) => json!(v),
        Value::I64(v) => json!(v),
        Value::F32(v) => json!(f64::from(*v)),
        Value::Vec2(v) => json!(v),
        Value::Vec3(v) => json!(v),
        Value::Quat(v) => json!(v),
        Value::String(v) => json!(v),
        Value::Bytes(v) => json!(v),
    }
}

pub(crate) fn json_value(kind: StateKind, value: &serde_json::Value) -> Result<Value> {
    let bad = || error("State value does not match the property type");
    Ok(match kind {
        StateKind::Bool => Value::Bool(value.as_bool().ok_or_else(bad)?),
        StateKind::I32 => Value::I32(
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(bad)?,
        ),
        StateKind::F32 => {
            let v = value.as_f64().ok_or_else(bad)? as f32;
            if !v.is_finite() {
                return Err(bad());
            }
            Value::F32(v)
        }
        StateKind::String => Value::String(value.as_str().ok_or_else(bad)?.to_owned()),
    })
}

pub(crate) fn default_state(settings: &NetworkSettings) -> Vec<Value> {
    settings
        .state_properties
        .iter()
        .map(|p| spec(p.kind).default_value())
        .collect()
}

/// Validated RPC name.
pub(crate) fn rpc_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_RPC_NAME || name.chars().any(char::is_control) {
        return Err(error("RPC names are 1-64 bytes without control characters"));
    }
    Ok(())
}

pub(crate) fn rpc_args(name: &str, payload: &serde_json::Value) -> Result<Vec<Value>> {
    rpc_name(name)?;
    let bytes = serde_json::to_vec(payload)?;
    if bytes.len() > MAX_RPC_PAYLOAD {
        return Err(error("RPC payload exceeds 768 bytes of JSON"));
    }
    Ok(vec![Value::String(name.into()), Value::Bytes(bytes)])
}

/// Decodes untrusted RPC arguments into a name and a JSON payload.
pub(crate) fn decode_rpc(args: &[Value]) -> Option<(String, serde_json::Value)> {
    let [Value::String(name), Value::Bytes(bytes)] = args else {
        return None;
    };
    rpc_name(name).ok()?;
    Some((name.clone(), serde_json::from_slice(bytes).ok()?))
}
