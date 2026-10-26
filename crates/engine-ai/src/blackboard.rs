//! Typed key/value store shared by the nodes of one AI agent.
//!
//! Keys are declared up front (the document variables of a graph) with a type;
//! values are set, read and cleared by node, native code or host overrides. Every
//! change that alters a value is recorded as a notification that observer
//! decorators consume once per tick.
use engine_graph::{GraphVariable, PortType, value_conforms};
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Longest string value a blackboard stores.
pub const MAX_STRING: usize = 4096;
/// Most keys a blackboard declares.
pub const MAX_KEYS: usize = 256;

/// A value stored under a blackboard key. `Entity` holds an authored entity id.
#[derive(Clone, Debug, PartialEq)]
pub enum BbValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Vec3([f32; 3]),
    Entity(String),
}

impl Serialize for BbValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(v) => serializer.serialize_bool(*v),
            Self::Int(v) => serializer.serialize_i64(*v),
            Self::Float(v) => serializer.serialize_f64(*v),
            Self::String(v) | Self::Entity(v) => serializer.serialize_str(v),
            Self::Vec3(v) => v.serialize(serializer),
        }
    }
}

impl BbValue {
    /// The blackboard type of the value.
    pub fn ty(&self) -> PortType {
        match self {
            Self::Bool(_) => PortType::Bool,
            Self::Int(_) => PortType::Int,
            Self::Float(_) => PortType::Float,
            Self::String(_) => PortType::String,
            Self::Vec3(_) => PortType::Vec3,
            Self::Entity(_) => PortType::Entity,
        }
    }

    /// Parses `value` as a `ty`. Integers are accepted for `Float`. Returns `None`
    /// for a wrong shape, non-finite number, oversized string or non-data type.
    pub fn from_json(ty: &PortType, value: &Value) -> Option<Self> {
        if !value_conforms(ty, value) {
            return None;
        }
        match ty {
            PortType::Bool => value.as_bool().map(Self::Bool),
            PortType::Int => value.as_i64().map(Self::Int),
            PortType::Float => value.as_f64().filter(|f| f.is_finite()).map(Self::Float),
            PortType::String => value
                .as_str()
                .filter(|s| s.len() <= MAX_STRING)
                .map(|s| Self::String(s.to_owned())),
            PortType::Entity => value
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .map(|s| Self::Entity(s.to_owned())),
            PortType::Vec3 => {
                let items = value.as_array()?;
                let mut out = [0.0f32; 3];
                for (slot, item) in out.iter_mut().zip(items) {
                    let v = item.as_f64()? as f32;
                    if !v.is_finite() {
                        return None;
                    }
                    *slot = v;
                }
                Some(Self::Vec3(out))
            }
            PortType::Flow | PortType::Any | PortType::Custom(_) => None,
        }
    }

    /// JSON form for events and tools. `Vec3` components widen from `f32`.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Bool(v) => Value::from(*v),
            Self::Int(v) => Value::from(*v),
            Self::Float(v) => Value::from(*v),
            Self::String(v) | Self::Entity(v) => Value::from(v.clone()),
            Self::Vec3(v) => Value::from(v.to_vec()),
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// Orders two values when both are numeric; `None` otherwise.
    pub fn partial_compare(&self, other: &Self) -> Option<Ordering> {
        self.number()?.partial_cmp(&other.number()?)
    }

    /// Equality that treats `Int` and `Float` as one numeric type.
    pub fn loosely_equals(&self, other: &Self) -> bool {
        match (self.number(), other.number()) {
            (Some(a), Some(b)) => a == b,
            _ => self == other,
        }
    }
}

/// Why a blackboard operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlackboardError {
    UnknownKey(String),
    TypeMismatch { key: String, expected: PortType },
    Invalid(String),
}

impl fmt::Display for BlackboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(key) => write!(f, "blackboard key {key:?} is not declared"),
            Self::TypeMismatch { key, expected } => {
                write!(f, "blackboard key {key:?} holds {expected:?} values")
            }
            Self::Invalid(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for BlackboardError {}

/// Typed key/value store with change notifications.
#[derive(Clone, Debug, Default)]
pub struct Blackboard {
    types: BTreeMap<String, PortType>,
    values: BTreeMap<String, BbValue>,
    changed: BTreeSet<String>,
    revision: u64,
}

impl Blackboard {
    /// Declares one key per variable and applies each variable's default.
    pub fn from_variables(variables: &[GraphVariable]) -> Result<Self, BlackboardError> {
        if variables.len() > MAX_KEYS {
            return Err(BlackboardError::Invalid(format!(
                "more than {MAX_KEYS} blackboard keys"
            )));
        }
        let mut board = Self::default();
        for variable in variables {
            board
                .types
                .insert(variable.name.clone(), variable.ty.clone());
        }
        for variable in variables {
            if let Some(default) = &variable.default {
                board.set_json(&variable.name, default)?;
            }
        }
        board.changed.clear();
        board.revision = 0;
        Ok(board)
    }

    pub fn key_type(&self, key: &str) -> Option<&PortType> {
        self.types.get(key)
    }
    pub fn get(&self, key: &str) -> Option<&BbValue> {
        self.values.get(key)
    }
    pub fn is_set(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    /// Declared keys in sorted order.
    pub fn keys(&self) -> impl Iterator<Item = (&String, &PortType)> {
        self.types.iter()
    }
    /// Count of value-changing writes since creation (wraps never in practice).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Stores `value`; `Int` converts to a `Float` key. Returns whether the
    /// stored value changed (and so whether a notification was recorded).
    pub fn set(&mut self, key: &str, value: BbValue) -> Result<bool, BlackboardError> {
        let ty = self
            .types
            .get(key)
            .ok_or_else(|| BlackboardError::UnknownKey(key.to_owned()))?;
        let value = match (ty, value) {
            (PortType::Float, BbValue::Int(v)) => BbValue::Float(v as f64),
            (ty, value) if value.ty() == *ty => value,
            _ => {
                return Err(BlackboardError::TypeMismatch {
                    key: key.to_owned(),
                    expected: ty.clone(),
                });
            }
        };
        if self.values.get(key) == Some(&value) {
            return Ok(false);
        }
        self.values.insert(key.to_owned(), value);
        self.mark(key);
        Ok(true)
    }

    /// Stores a JSON value, parsed as the key's declared type.
    pub fn set_json(&mut self, key: &str, value: &Value) -> Result<bool, BlackboardError> {
        let ty = self
            .types
            .get(key)
            .ok_or_else(|| BlackboardError::UnknownKey(key.to_owned()))?
            .clone();
        let parsed = BbValue::from_json(&ty, value).ok_or(BlackboardError::TypeMismatch {
            key: key.to_owned(),
            expected: ty,
        })?;
        self.set(key, parsed)
    }

    /// Removes the value; returns whether one was set.
    pub fn clear(&mut self, key: &str) -> Result<bool, BlackboardError> {
        if !self.types.contains_key(key) {
            return Err(BlackboardError::UnknownKey(key.to_owned()));
        }
        if self.values.remove(key).is_some() {
            self.mark(key);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn mark(&mut self, key: &str) {
        self.changed.insert(key.to_owned());
        self.revision += 1;
    }

    /// Whether notifications are waiting.
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }

    /// Takes the keys whose value changed since the last call, in sorted order.
    pub fn take_changes(&mut self) -> BTreeSet<String> {
        std::mem::take(&mut self.changed)
    }

    /// Bounded summary for telemetry: at most `max_keys` set keys in sorted order,
    /// strings cut to `max_string` characters.
    pub fn summary(&self, max_keys: usize, max_string: usize) -> BTreeMap<String, BbValue> {
        self.values
            .iter()
            .take(max_keys)
            .map(|(key, value)| {
                let value = match value {
                    BbValue::String(s) if s.chars().count() > max_string => {
                        BbValue::String(s.chars().take(max_string).collect())
                    }
                    BbValue::Entity(s) if s.chars().count() > max_string => {
                        BbValue::Entity(s.chars().take(max_string).collect())
                    }
                    other => other.clone(),
                };
                (key.clone(), value)
            })
            .collect()
    }
}
