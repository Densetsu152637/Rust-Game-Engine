//! Typed script values shared by every script backend.
use engine_graph::{PortType, value_within_limits};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Longest string a script may hold, pass to a host function or emit.
pub const MAX_STRING_BYTES: usize = 4096;
/// Name of the custom graph port type that carries a JSON value.
pub const JSON_PORT_TYPE: &str = "script.json";

/// A runtime entity handle (`Entity::to_bits`). Runtime-only: never persisted and
/// never trusted; host functions re-validate it against the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityRef(pub u64);

/// A script value. `Int` is `i64`, `Float` is `f64` (rounded to `f32` at the
/// `Vec3`, transform and component boundaries).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Vec3([f32; 3]),
    Entity(EntityRef),
    Json(Arc<serde_json::Value>),
}

/// The static type of a [`Value`] (everything but `Unit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Bool,
    Int,
    Float,
    #[serde(rename = "string")]
    Str,
    Vec3,
    Entity,
    Json,
}

impl ValueType {
    /// Every value type, in a stable order.
    pub const ALL: [ValueType; 7] = [
        Self::Bool,
        Self::Int,
        Self::Float,
        Self::Str,
        Self::Vec3,
        Self::Entity,
        Self::Json,
    ];

    /// The graph port type of this value type.
    pub fn port_type(self) -> PortType {
        match self {
            Self::Bool => PortType::Bool,
            Self::Int => PortType::Int,
            Self::Float => PortType::Float,
            Self::Str => PortType::String,
            Self::Vec3 => PortType::Vec3,
            Self::Entity => PortType::Entity,
            Self::Json => PortType::Custom(JSON_PORT_TYPE.to_owned()),
        }
    }
    /// The value type of a graph port type; `None` for flow, `any` and foreign
    /// custom types.
    pub fn from_port_type(ty: &PortType) -> Option<Self> {
        match ty {
            PortType::Bool => Some(Self::Bool),
            PortType::Int => Some(Self::Int),
            PortType::Float => Some(Self::Float),
            PortType::String => Some(Self::Str),
            PortType::Vec3 => Some(Self::Vec3),
            PortType::Entity => Some(Self::Entity),
            PortType::Custom(name) if name == JSON_PORT_TYPE => Some(Self::Json),
            _ => None,
        }
    }
    /// Stable lower-case name (`bool`, `int`, `float`, `string`, `vec3`, `entity`, `json`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Str => "string",
            Self::Vec3 => "vec3",
            Self::Entity => "entity",
            Self::Json => "json",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }
    /// The zero value of the type (`false`, `0`, `0.0`, `""`, origin, null entity, `null`).
    pub fn default_value(self) -> Value {
        match self {
            Self::Bool => Value::Bool(false),
            Self::Int => Value::Int(0),
            Self::Float => Value::Float(0.0),
            Self::Str => Value::Str(Arc::from("")),
            Self::Vec3 => Value::Vec3([0.0; 3]),
            Self::Entity => Value::Entity(EntityRef(0)),
            Self::Json => Value::Json(Arc::new(serde_json::Value::Null)),
        }
    }
}

impl Value {
    pub fn str(text: &str) -> Self {
        Self::Str(Arc::from(text))
    }
    pub fn json(value: serde_json::Value) -> Self {
        Self::Json(Arc::new(value))
    }
    /// The value's type; `None` for `Unit`.
    pub fn value_type(&self) -> Option<ValueType> {
        Some(match self {
            Self::Unit => return None,
            Self::Bool(_) => ValueType::Bool,
            Self::Int(_) => ValueType::Int,
            Self::Float(_) => ValueType::Float,
            Self::Str(_) => ValueType::Str,
            Self::Vec3(_) => ValueType::Vec3,
            Self::Entity(_) => ValueType::Entity,
            Self::Json(_) => ValueType::Json,
        })
    }
    /// Approximate heap and inline footprint, for state accounting.
    pub fn byte_size(&self) -> usize {
        16 + match self {
            Self::Str(s) => s.len(),
            Self::Json(j) => json_size(j),
            _ => 0,
        }
    }
    /// Converts an authored JSON value to a typed value. A non-finite or
    /// oversized value, or a wrong JSON type, is `None`. Entities have no
    /// authored form (a runtime handle), so `Entity` is always `None`.
    pub fn from_json(ty: ValueType, value: &serde_json::Value) -> Option<Self> {
        let checked = match ty {
            ValueType::Bool => Self::Bool(value.as_bool()?),
            ValueType::Int => Self::Int(value.as_i64()?),
            ValueType::Float => Self::Float(value.as_f64().filter(|f| f.is_finite())?),
            ValueType::Str => Self::str(value.as_str()?),
            ValueType::Vec3 => {
                let items = value.as_array()?;
                if items.len() != 3 {
                    return None;
                }
                let mut out = [0.0f32; 3];
                for (slot, item) in out.iter_mut().zip(items) {
                    let f = item.as_f64()? as f32;
                    if !f.is_finite() {
                        return None;
                    }
                    *slot = f;
                }
                Self::Vec3(out)
            }
            ValueType::Entity => return None,
            ValueType::Json => {
                if !value_within_limits(value) {
                    return None;
                }
                Self::json(value.clone())
            }
        };
        validate_value(ty, &checked).ok()?;
        Some(checked)
    }
    /// The JSON form of a value (`Unit` is `null`, an entity its numeric handle).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Unit => serde_json::Value::Null,
            Self::Bool(b) => (*b).into(),
            Self::Int(i) => (*i).into(),
            Self::Float(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::Str(s) => s.to_string().into(),
            Self::Vec3(v) => serde_json::json!([v[0], v[1], v[2]]),
            Self::Entity(e) => e.0.into(),
            Self::Json(j) => (**j).clone(),
        }
    }
}

fn json_size(value: &serde_json::Value) -> usize {
    // Bounded by `value_within_limits` (512 nodes, depth 8) at every boundary.
    match value {
        serde_json::Value::String(s) => 16 + s.len(),
        serde_json::Value::Array(a) => 16 + a.iter().map(json_size).sum::<usize>(),
        serde_json::Value::Object(o) => {
            16 + o.iter().map(|(k, v)| k.len() + json_size(v)).sum::<usize>()
        }
        _ => 16,
    }
}

/// Checks that `value` is a legal value of `ty`: exact type, finite numbers,
/// strings within [`MAX_STRING_BYTES`], JSON within the shared document limits.
///
/// # Errors
/// A human-readable reason.
pub fn validate_value(ty: ValueType, value: &Value) -> Result<(), String> {
    match (ty, value) {
        (ValueType::Bool, Value::Bool(_))
        | (ValueType::Int, Value::Int(_))
        | (ValueType::Entity, Value::Entity(_)) => Ok(()),
        (ValueType::Float, Value::Float(f)) if f.is_finite() => Ok(()),
        (ValueType::Float, Value::Float(_)) => Err("float is not finite".to_owned()),
        (ValueType::Vec3, Value::Vec3(v)) if v.iter().all(|c| c.is_finite()) => Ok(()),
        (ValueType::Vec3, Value::Vec3(_)) => Err("vec3 is not finite".to_owned()),
        (ValueType::Str, Value::Str(s)) if s.len() <= MAX_STRING_BYTES => Ok(()),
        (ValueType::Str, Value::Str(s)) => Err(format!(
            "string of {} bytes exceeds {MAX_STRING_BYTES}",
            s.len()
        )),
        (ValueType::Json, Value::Json(j)) if value_within_limits(j) => Ok(()),
        (ValueType::Json, Value::Json(_)) => Err("json exceeds the value limits".to_owned()),
        (ty, other) => Err(format!(
            "expected {}, found {}",
            ty.name(),
            other.value_type().map_or("unit", ValueType::name)
        )),
    }
}
