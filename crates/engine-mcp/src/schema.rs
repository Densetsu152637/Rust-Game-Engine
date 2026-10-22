//! Small builders for the JSON Schemas advertised by tools, plus typed argument
//! readers that turn wrong-typed arguments into agent-friendly tool failures.

use crate::registry::ToolFailure;
use serde_json::{Map, Value, json};

pub fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

pub fn string_enum(description: &str, values: &[&str]) -> Value {
    json!({"type": "string", "enum": values, "description": description})
}

pub fn integer(description: &str, minimum: i64, maximum: i64, default: Option<i64>) -> Value {
    let mut schema = json!({"type": "integer", "minimum": minimum, "maximum": maximum,
        "description": description});
    if let Some(default) = default {
        schema["default"] = json!(default);
    }
    schema
}

pub fn boolean(description: &str, default: bool) -> Value {
    json!({"type": "boolean", "default": default, "description": description})
}

pub fn array_of(description: &str, items: Value, max_items: usize) -> Value {
    json!({"type": "array", "items": items, "maxItems": max_items, "description": description})
}

/// A closed `type: object` schema.
pub fn object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let properties: Map<String, Value> = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect();
    json!({"type": "object", "properties": properties, "required": required,
        "additionalProperties": false})
}

/// An output schema common to every tool: `ok` always, `error`/`diagnostics` on
/// failure, plus the tool's own success members. Only `ok` is required so the
/// same schema describes both outcomes.
pub fn output(properties: Vec<(&str, Value)>) -> Value {
    let mut all: Map<String, Value> = Map::new();
    all.insert(
        "ok".into(),
        json!({"type": "boolean", "description": "true on success, false on failure."}),
    );
    all.insert("error".into(), error_schema());
    all.insert("diagnostics".into(), diagnostics_schema());
    for (name, schema) in properties {
        all.insert(name.to_owned(), schema);
    }
    json!({"type": "object", "properties": all, "required": ["ok"]})
}

fn error_schema() -> Value {
    json!({"type": "object", "description": "Present when ok is false.",
        "properties": {
            "code": {"type": "string", "description": "Stable machine-readable error code."},
            "message": {"type": "string"},
            "hint": {"type": "string", "description": "What to do next."},
            "details": {"type": "object"}
        }, "required": ["code", "message"]})
}

pub fn diagnostics_schema() -> Value {
    json!({"type": "array", "description": "Engine diagnostics (validation, compiler).",
        "items": {"type": "object", "properties": {
            "code": {"type": "string"},
            "severity": {"type": "string"},
            "file": {"type": "string", "description": "Project-relative file."},
            "path": {"type": "string", "description": "JSON path, entity ID or line:column."},
            "message": {"type": "string"}
        }, "required": ["code", "severity", "message"]}})
}

pub fn invalid(name: &str, expected: &str) -> ToolFailure {
    ToolFailure::new(
        "invalid_argument",
        format!("argument `{name}` must be {expected}"),
    )
    .with_hint("Check the tool's inputSchema and retry.")
}

/// Typed readers over a tool's argument object. A JSON `null` counts as absent.
pub struct Args<'a>(pub &'a Map<String, Value>);

impl<'a> Args<'a> {
    fn get(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name).filter(|v| !v.is_null())
    }
    pub fn raw(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name)
    }
    pub fn string(&self, name: &str) -> Result<&'a str, ToolFailure> {
        self.optional_string(name)?
            .ok_or_else(|| invalid(name, "a string"))
    }
    pub fn optional_string(&self, name: &str) -> Result<Option<&'a str>, ToolFailure> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(invalid(name, "a string")),
        }
    }
    pub fn boolean(&self, name: &str, default: bool) -> Result<bool, ToolFailure> {
        match self.get(name) {
            None => Ok(default),
            Some(Value::Bool(value)) => Ok(*value),
            Some(_) => Err(invalid(name, "a boolean")),
        }
    }
    /// An integer in `minimum..=maximum`, or `default` when absent.
    pub fn integer(
        &self,
        name: &str,
        minimum: u64,
        maximum: u64,
        default: u64,
    ) -> Result<u64, ToolFailure> {
        match self.get(name) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .filter(|n| (minimum..=maximum).contains(n))
                .ok_or_else(|| invalid(name, &format!("an integer from {minimum} to {maximum}"))),
        }
    }
    pub fn optional_integer(
        &self,
        name: &str,
        minimum: u64,
        maximum: u64,
    ) -> Result<Option<u64>, ToolFailure> {
        match self.get(name) {
            None => Ok(None),
            Some(_) => self.integer(name, minimum, maximum, minimum).map(Some),
        }
    }
    pub fn optional_object(
        &self,
        name: &str,
    ) -> Result<Option<&'a Map<String, Value>>, ToolFailure> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Object(map)) => Ok(Some(map)),
            Some(_) => Err(invalid(name, "an object")),
        }
    }
    pub fn string_array(&self, name: &str) -> Result<Option<Vec<&'a str>>, ToolFailure> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| invalid(name, "an array of strings"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(invalid(name, "an array of strings")),
        }
    }
    pub fn one_of(&self, name: &str, allowed: &[&str]) -> Result<Option<&'a str>, ToolFailure> {
        match self.optional_string(name)? {
            Some(value) if !allowed.contains(&value) => {
                Err(invalid(name, &format!("one of: {}", allowed.join(", "))))
            }
            other => Ok(other),
        }
    }
}
