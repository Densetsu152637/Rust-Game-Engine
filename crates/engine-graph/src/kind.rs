//! Graph kinds: the node-type registry and rules one domain layers on the model.
use crate::{
    diagnostic::GraphDiagnostic,
    model::{
        GraphDocument, GraphNode, MAX_EDGES, MAX_NODES, MAX_VARIABLES, Port, PortDirection,
        PortType,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Declares a port of a node type.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PortDef {
    #[serde(flatten)]
    pub port: Port,
    /// Completeness (not structure): an unconnected required port is reported by
    /// full validation as `graph.port.unconnected`.
    pub required: bool,
    pub description: String,
}

impl PortDef {
    fn new(name: &str, direction: PortDirection, ty: PortType) -> Self {
        Self {
            port: Port {
                name: name.to_owned(),
                direction,
                ty,
                multiple: false,
            },
            required: false,
            description: String::new(),
        }
    }
    pub fn input(name: &str, ty: PortType) -> Self {
        Self::new(name, PortDirection::In, ty)
    }
    pub fn output(name: &str, ty: PortType) -> Self {
        Self::new(name, PortDirection::Out, ty)
    }
    /// Allows more than one edge on this port.
    pub fn multiple(mut self) -> Self {
        self.port.multiple = true;
        self
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn describe(mut self, text: &str) -> Self {
        self.description = text.to_owned();
        self
    }
}

/// Value category of a node parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    Bool,
    Int,
    Float,
    String,
    Vec3,
    /// One of `ParamDef::choices`.
    Enum,
    /// The name of a document variable, optionally restricted by type.
    Variable,
    /// Any JSON value within the size limits; the domain interprets it.
    Json,
}

/// Declares a parameter of a node type.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    /// Completeness: a missing required parameter is `graph.param.missing`.
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Longest accepted string (default 256).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_len: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// For [`ParamType::Variable`]: accepted variable types (empty = any data type).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variable_types: Vec<PortType>,
    pub description: String,
}

impl ParamDef {
    pub fn new(name: &str, ty: ParamType) -> Self {
        Self {
            name: name.to_owned(),
            ty,
            required: false,
            default: None,
            min: None,
            max: None,
            max_len: None,
            choices: Vec::new(),
            variable_types: Vec::new(),
            description: String::new(),
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn default_value(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }
    pub fn max_len(mut self, len: usize) -> Self {
        self.max_len = Some(len);
        self
    }
    pub fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|c| (*c).to_owned()).collect();
        self
    }
    pub fn variable_types(mut self, types: &[PortType]) -> Self {
        self.variable_types = types.to_vec();
        self
    }
    pub fn describe(mut self, text: &str) -> Self {
        self.description = text.to_owned();
        self
    }
}

/// Declares one node type: its ports, parameters and palette metadata.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeTypeDef {
    pub name: String,
    /// Palette group for editors, for example `composite` or `task`.
    pub category: String,
    pub description: String,
    pub ports: Vec<PortDef>,
    pub params: Vec<ParamDef>,
    /// Marks the entry node of a `single_root` kind.
    pub root: bool,
    /// Whether a node of this type may carry ports beyond the declared ones.
    pub extensible: bool,
}

impl NodeTypeDef {
    pub fn new(name: &str, category: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            category: category.to_owned(),
            description: description.to_owned(),
            ports: Vec::new(),
            params: Vec::new(),
            root: false,
            extensible: false,
        }
    }
    pub fn port(mut self, port: PortDef) -> Self {
        self.ports.push(port);
        self
    }
    pub fn param(mut self, param: ParamDef) -> Self {
        self.params.push(param);
        self
    }
    pub fn root(mut self) -> Self {
        self.root = true;
        self
    }
    pub fn extensible(mut self) -> Self {
        self.extensible = true;
        self
    }
    pub fn param_def(&self, name: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.name == name)
    }
    pub fn port_def(&self, name: &str) -> Option<&PortDef> {
        self.ports.iter().find(|p| p.port.name == name)
    }
    /// The declared ports as document ports.
    pub fn declared_ports(&self) -> Vec<Port> {
        self.ports.iter().map(|p| p.port.clone()).collect()
    }
    /// The node's parameter values with declared defaults filled in; keys sorted.
    pub fn effective_params(&self, node: &GraphNode) -> BTreeMap<String, Value> {
        let mut values: BTreeMap<String, Value> = self
            .params
            .iter()
            .filter_map(|p| p.default.clone().map(|d| (p.name.clone(), d)))
            .collect();
        values.extend(node.params.iter().map(|(k, v)| (k.clone(), v.clone())));
        values
    }
}

/// All node types of one graph kind, ordered by name.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NodeTypeRegistry {
    types: BTreeMap<String, NodeTypeDef>,
}

impl NodeTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    /// Adds a type; a duplicate name or duplicate port/param names are rejected.
    pub fn register(&mut self, def: NodeTypeDef) -> Result<(), String> {
        let unique = |names: Vec<&str>| {
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted.len() == names.len()
        };
        if !unique(def.ports.iter().map(|p| p.port.name.as_str()).collect())
            || !unique(def.params.iter().map(|p| p.name.as_str()).collect())
        {
            return Err(format!(
                "node type {} repeats a port or param name",
                def.name
            ));
        }
        if self.types.contains_key(&def.name) {
            return Err(format!("node type {} is already registered", def.name));
        }
        self.types.insert(def.name.clone(), def);
        Ok(())
    }
    /// Builder form of [`register`](Self::register); panics on a programming error
    /// (duplicate declaration), which tests of the owning kind catch.
    pub fn with(mut self, def: NodeTypeDef) -> Self {
        let name = def.name.clone();
        self.register(def)
            .unwrap_or_else(|e| panic!("invalid node registry {name}: {e}"));
        self
    }
    pub fn get(&self, name: &str) -> Option<&NodeTypeDef> {
        self.types.get(name)
    }
    pub fn iter(&self) -> impl Iterator<Item = &NodeTypeDef> {
        self.types.values()
    }
    pub fn len(&self) -> usize {
        self.types.len()
    }
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Size bounds of a kind; each is clamped to the crate's hard ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GraphLimits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_variables: usize,
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_nodes: MAX_NODES,
            max_edges: MAX_EDGES,
            max_variables: MAX_VARIABLES,
        }
    }
}

impl GraphLimits {
    pub(crate) fn effective(self) -> Self {
        Self {
            max_nodes: self.max_nodes.min(MAX_NODES),
            max_edges: self.max_edges.min(MAX_EDGES),
            max_variables: self.max_variables.min(MAX_VARIABLES),
        }
    }
}

/// Kind-specific semantic checks run by full validation after the generic ones
/// pass, typically by compiling the document. Must be pure and deterministic.
pub type KindCheck = fn(&GraphDocument) -> Vec<GraphDiagnostic>;

/// A graph domain: its name, structural rules and node types.
#[derive(Clone, Debug)]
pub struct GraphKind {
    pub name: String,
    pub description: String,
    /// When false, any directed cycle is `graph.cycle`.
    pub allow_cycles: bool,
    /// When true, exactly one node of a `root` type must exist and every node
    /// must be reachable from it along edges.
    pub single_root: bool,
    pub limits: GraphLimits,
    /// Types variables of this kind may have; empty means any data type.
    pub variable_types: Vec<PortType>,
    pub registry: NodeTypeRegistry,
    pub check: Option<KindCheck>,
}

impl GraphKind {
    pub fn new(name: &str, description: &str, registry: NodeTypeRegistry) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            allow_cycles: false,
            single_root: false,
            limits: GraphLimits::default(),
            variable_types: Vec::new(),
            registry,
            check: None,
        }
    }
    pub fn allow_cycles(mut self) -> Self {
        self.allow_cycles = true;
        self
    }
    pub fn single_root(mut self) -> Self {
        self.single_root = true;
        self
    }
    pub fn limits(mut self, limits: GraphLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn variable_types(mut self, types: &[PortType]) -> Self {
        self.variable_types = types.to_vec();
        self
    }
    pub fn check(mut self, check: KindCheck) -> Self {
        self.check = Some(check);
        self
    }
    /// Serializable description for schema tools and editor palettes.
    pub fn info(&self) -> GraphKindInfo<'_> {
        GraphKindInfo {
            name: &self.name,
            description: &self.description,
            allow_cycles: self.allow_cycles,
            single_root: self.single_root,
            limits: self.limits.effective(),
            variable_types: &self.variable_types,
            node_types: self.registry.iter().collect(),
        }
    }
}

/// Borrowed, serializable view of a [`GraphKind`] (node types ordered by name).
#[derive(Debug, Serialize)]
pub struct GraphKindInfo<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub allow_cycles: bool,
    pub single_root: bool,
    pub limits: GraphLimits,
    pub variable_types: &'a [PortType],
    pub node_types: Vec<&'a NodeTypeDef>,
}
