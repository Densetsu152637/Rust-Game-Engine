//! The serializable graph model: nodes with typed ports, edges and parameters.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Current on-disk graph format version.
pub const GRAPH_VERSION: u32 = 1;
/// Maximum nodes in one graph.
pub const MAX_NODES: usize = 512;
/// Maximum edges in one graph.
pub const MAX_EDGES: usize = 2048;
/// Maximum length of a node id.
pub const MAX_ID_LEN: usize = 64;

fn current_version() -> u32 {
    GRAPH_VERSION
}

/// One end of an edge: a named port on a node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}

impl PortRef {
    pub fn new(node: &str, port: &str) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
        }
    }
}

/// A directed connection from an output port to an input port.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}

/// A graph node: a stable id, a registered kind and typed parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// A procedural generation graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    #[serde(default = "current_version")]
    pub version: u32,
    /// Graph seed; each node derives its own stream from this and its id.
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            version: GRAPH_VERSION,
            seed: 0,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

/// A precise problem found in a graph or while running it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable machine-readable code, e.g. `pcg.edge.type_mismatch`.
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            node: None,
            port: None,
            message: message.into(),
        }
    }
    pub fn at_node(mut self, node: &str) -> Self {
        self.node = Some(node.into());
        self
    }
    pub fn at_port(mut self, port: &str) -> Self {
        self.port = Some(port.into());
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code)?;
        if let Some(node) = &self.node {
            write!(f, " [{node}")?;
            if let Some(port) = &self.port {
                write!(f, ".{port}")?;
            }
            write!(f, "]")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl Graph {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            ..Self::default()
        }
    }
    /// Parses a graph, rejecting newer format versions.
    pub fn from_json(text: &str) -> Result<Self, Diagnostic> {
        let graph: Graph = serde_json::from_str(text)
            .map_err(|e| Diagnostic::new("pcg.graph.parse", e.to_string()))?;
        if graph.version == 0 || graph.version > GRAPH_VERSION {
            return Err(Diagnostic::new(
                "pcg.graph.version",
                format!(
                    "graph version {} is unsupported (this build reads 1..={GRAPH_VERSION})",
                    graph.version
                ),
            ));
        }
        Ok(graph)
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("graph is plain data")
    }
    /// The seed a run uses for `instance_seed` (a volume's or caller's seed): the graph seed
    /// and the instance seed are mixed so neither can cancel the other.
    pub fn seed_for(&self, instance_seed: u64) -> u64 {
        crate::rng::derive_seed(self.seed, &instance_seed.to_string())
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}
