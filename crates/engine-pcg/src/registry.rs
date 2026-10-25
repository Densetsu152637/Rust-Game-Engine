//! Node kinds: port/parameter definitions and the functions that execute them.
use crate::data::Data;
use crate::graph::{Diagnostic, Node};
use crate::surface::{Aabb, Surface};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Type of data carried by a port. Edges must connect equal types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Points,
    Field,
    Instances,
}

/// Declared input or output port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: PortType,
    /// Only meaningful for inputs: an unconnected required input is a validation error.
    pub required: bool,
}

impl PortSpec {
    pub fn required(name: &str, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
        }
    }
    pub fn optional(name: &str, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
        }
    }
}

/// Parameter type with its accepted range.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamType {
    Float {
        min: f64,
        max: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Bool,
    Str,
    Enum {
        values: Vec<String>,
    },
    /// Array of three numbers in `[min, max]`.
    Vec3 {
        min: f64,
        max: f64,
    },
    /// Spawner entry list: `[{asset, kind: prefab|mesh, weight}]`.
    Entries,
}

/// Declared parameter with default.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamSpec {
    pub name: String,
    #[serde(flatten)]
    pub ty: ParamType,
    pub default: Value,
}

impl ParamSpec {
    pub fn float(name: &str, default: f64, min: f64, max: f64) -> Self {
        Self::make(name, ParamType::Float { min, max }, json!(default))
    }
    pub fn int(name: &str, default: i64, min: i64, max: i64) -> Self {
        Self::make(name, ParamType::Int { min, max }, json!(default))
    }
    pub fn boolean(name: &str, default: bool) -> Self {
        Self::make(name, ParamType::Bool, json!(default))
    }
    pub fn string(name: &str, default: &str) -> Self {
        Self::make(name, ParamType::Str, json!(default))
    }
    pub fn choice(name: &str, values: &[&str], default: &str) -> Self {
        Self::make(
            name,
            ParamType::Enum {
                values: values.iter().map(|v| (*v).to_owned()).collect(),
            },
            json!(default),
        )
    }
    pub fn vec3(name: &str, default: [f64; 3], min: f64, max: f64) -> Self {
        Self::make(name, ParamType::Vec3 { min, max }, json!(default))
    }
    pub fn entries(name: &str) -> Self {
        Self::make(name, ParamType::Entries, json!([]))
    }
    fn make(name: &str, ty: ParamType, default: Value) -> Self {
        Self {
            name: name.into(),
            ty,
            default,
        }
    }

    /// Checks `value` against this parameter's type and range.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match &self.ty {
            ParamType::Float { min, max } => match value.as_f64() {
                Some(v) if v.is_finite() && v >= *min && v <= *max => Ok(()),
                Some(v) => Err(format!("{v} is outside {min}..={max}")),
                None => Err("expected a number".into()),
            },
            ParamType::Int { min, max } => match value.as_i64() {
                Some(v) if v >= *min && v <= *max => Ok(()),
                Some(v) => Err(format!("{v} is outside {min}..={max}")),
                None => Err("expected an integer".into()),
            },
            ParamType::Bool => value
                .is_boolean()
                .then_some(())
                .ok_or_else(|| "expected a boolean".into()),
            ParamType::Str => match value.as_str() {
                Some(s) if s.len() <= 256 => Ok(()),
                Some(_) => Err("string longer than 256 bytes".into()),
                None => Err("expected a string".into()),
            },
            ParamType::Enum { values } => match value.as_str() {
                Some(s) if values.iter().any(|v| v == s) => Ok(()),
                Some(s) => Err(format!("`{s}` is not one of {}", values.join(", "))),
                None => Err("expected a string".into()),
            },
            ParamType::Vec3 { min, max } => {
                let items = value.as_array().filter(|a| a.len() == 3);
                let Some(items) = items else {
                    return Err("expected an array of three numbers".into());
                };
                for item in items {
                    match item.as_f64() {
                        Some(v) if v.is_finite() && v >= *min && v <= *max => {}
                        _ => return Err(format!("components must be numbers in {min}..={max}")),
                    }
                }
                Ok(())
            }
            ParamType::Entries => check_entries(value),
        }
    }
}

/// Maximum entries in a spawner.
pub const MAX_ENTRIES: usize = 64;

fn check_entries(value: &Value) -> Result<(), String> {
    let items = value.as_array().ok_or("expected an array of entries")?;
    if items.is_empty() || items.len() > MAX_ENTRIES {
        return Err(format!("expected 1..={MAX_ENTRIES} entries"));
    }
    for (i, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("entry {i} must be an object"))?;
        if let Some(key) = object
            .keys()
            .find(|k| !["asset", "kind", "weight"].contains(&k.as_str()))
        {
            return Err(format!("entry {i} has unknown field `{key}`"));
        }
        match object.get("asset").and_then(Value::as_str) {
            Some(a) if !a.is_empty() && a.len() <= 256 => {}
            _ => return Err(format!("entry {i} needs a non-empty `asset` string")),
        }
        if let Some(kind) = object.get("kind")
            && !matches!(kind.as_str(), Some("prefab" | "mesh"))
        {
            return Err(format!("entry {i} `kind` must be `prefab` or `mesh`"));
        }
        if let Some(weight) = object.get("weight") {
            match weight.as_f64() {
                Some(w) if w.is_finite() && w > 0.0 && w <= 1.0e6 => {}
                _ => return Err(format!("entry {i} `weight` must be in (0, 1e6]")),
            }
        }
    }
    Ok(())
}

/// Declaration of a node kind.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeDefinition {
    pub kind: String,
    pub title: String,
    pub doc: String,
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
    pub params: Vec<ParamSpec>,
}

impl NodeDefinition {
    pub fn input(&self, name: &str) -> Option<&PortSpec> {
        self.inputs.iter().find(|p| p.name == name)
    }
    pub fn output(&self, name: &str) -> Option<&PortSpec> {
        self.outputs.iter().find(|p| p.name == name)
    }
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// Resolved parameter values of one node (explicit value or declared default).
pub struct Params<'a> {
    pub node: &'a Node,
    pub definition: &'a NodeDefinition,
}

impl Params<'_> {
    pub fn value(&self, name: &str) -> &Value {
        self.node
            .params
            .get(name)
            .or_else(|| self.definition.param(name).map(|p| &p.default))
            .unwrap_or(&Value::Null)
    }
    pub fn float(&self, name: &str) -> f64 {
        self.value(name).as_f64().unwrap_or(0.0)
    }
    pub fn int(&self, name: &str) -> i64 {
        self.value(name).as_i64().unwrap_or(0)
    }
    pub fn boolean(&self, name: &str) -> bool {
        self.value(name).as_bool().unwrap_or(false)
    }
    pub fn string(&self, name: &str) -> &str {
        self.value(name).as_str().unwrap_or("")
    }
    pub fn vec3(&self, name: &str) -> [f64; 3] {
        let v = self.value(name);
        std::array::from_fn(|i| v.get(i).and_then(Value::as_f64).unwrap_or(0.0))
    }
}

/// Hard limits applied while running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum points any single node may output.
    pub max_points: usize,
    /// Maximum instances any single node may output (and the total result).
    pub max_instances: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_points: 262_144,
            max_instances: 262_144,
        }
    }
}

/// Inputs of one execution of the graph.
#[derive(Clone, Debug)]
pub struct RunSettings {
    /// Effective graph seed (replaces `Graph::seed`).
    pub seed: u64,
    /// Region to generate in, in surface-local coordinates.
    pub bounds: Aabb,
    /// Absolute position of the local origin; noise is sampled at `local + offset`.
    pub domain_offset: [f64; 3],
    pub limits: Limits,
}

impl RunSettings {
    pub fn new(seed: u64, bounds: Aabb) -> Self {
        Self {
            seed,
            bounds,
            domain_offset: [0.0; 3],
            limits: Limits::default(),
        }
    }
}

/// Failure raised by a node implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeError {
    pub code: String,
    pub message: String,
}

impl NodeError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new("pcg.node.failed", message)
    }
    pub(crate) fn into_diagnostic(self, node: &str) -> Diagnostic {
        Diagnostic::new(&self.code, self.message).at_node(node)
    }
}

/// Outputs produced by a node, keyed by output port name.
pub type Outputs = BTreeMap<String, Data>;

/// Everything a node function can see.
pub struct NodeContext<'a> {
    pub node_id: &'a str,
    /// Seed derived from the graph seed and this node's id.
    pub seed: u64,
    pub params: Params<'a>,
    pub inputs: &'a BTreeMap<String, Data>,
    pub surface: &'a dyn Surface,
    pub run: &'a RunSettings,
}

impl NodeContext<'_> {
    pub fn points(&self, port: &str) -> Result<&Arc<Vec<crate::data::Point>>, NodeError> {
        match self.inputs.get(port) {
            Some(Data::Points(p)) => Ok(p),
            _ => Err(NodeError::failed(format!(
                "input `{port}` has no points connected"
            ))),
        }
    }
    pub fn field(&self, port: &str) -> Result<&Arc<crate::data::Field>, NodeError> {
        match self.inputs.get(port) {
            Some(Data::Field(f)) => Ok(f),
            _ => Err(NodeError::failed(format!(
                "input `{port}` has no field connected"
            ))),
        }
    }
    pub fn optional_field(&self, port: &str) -> Option<&Arc<crate::data::Field>> {
        match self.inputs.get(port) {
            Some(Data::Field(f)) => Some(f),
            _ => None,
        }
    }
    pub fn field_context(&self) -> crate::data::FieldContext<'_> {
        crate::data::FieldContext {
            surface: self.surface,
            domain_offset: self.run.domain_offset,
        }
    }
}

/// Function implementing a node kind.
pub type NodeFn = Arc<dyn Fn(&NodeContext<'_>) -> Result<Outputs, NodeError> + Send + Sync>;

/// All known node kinds. Start from [`Registry::builtin`] and add game-specific kinds with
/// [`Registry::register`].
#[derive(Clone)]
pub struct Registry {
    kinds: BTreeMap<String, (NodeDefinition, NodeFn)>,
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            kinds: BTreeMap::new(),
        }
    }
    /// The built-in node library.
    pub fn builtin() -> Self {
        let mut registry = Self::empty();
        crate::nodes::register_builtin(&mut registry);
        registry
    }
    /// Adds a node kind. Fails when the kind already exists or its definition is inconsistent.
    pub fn register(
        &mut self,
        definition: NodeDefinition,
        run: impl Fn(&NodeContext<'_>) -> Result<Outputs, NodeError> + Send + Sync + 'static,
    ) -> Result<(), String> {
        if definition.kind.is_empty() || definition.kind.len() > 64 {
            return Err("kind must be 1..=64 characters".into());
        }
        if self.kinds.contains_key(&definition.kind) {
            return Err(format!(
                "node kind `{}` is already registered",
                definition.kind
            ));
        }
        for ports in [&definition.inputs, &definition.outputs] {
            for (i, port) in ports.iter().enumerate() {
                if ports
                    .iter()
                    .skip(i + 1)
                    .any(|other| other.name == port.name)
                {
                    return Err(format!("duplicate port name `{}`", port.name));
                }
            }
        }
        for param in &definition.params {
            param
                .check(&param.default)
                .or_else(|e| {
                    // Entries default is the empty list and is intentionally invalid.
                    if matches!(param.ty, ParamType::Entries) {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| format!("default of `{}` is invalid: {e}", param.name))?;
        }
        self.kinds
            .insert(definition.kind.clone(), (definition, Arc::new(run)));
        Ok(())
    }
    pub fn definition(&self, kind: &str) -> Option<&NodeDefinition> {
        self.kinds.get(kind).map(|(d, _)| d)
    }
    pub fn definitions(&self) -> impl Iterator<Item = &NodeDefinition> {
        self.kinds.values().map(|(d, _)| d)
    }
    pub(crate) fn function(&self, kind: &str) -> Option<&NodeFn> {
        self.kinds.get(kind).map(|(_, f)| f)
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.kinds.keys()).finish()
    }
}
