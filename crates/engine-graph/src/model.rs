//! The serde JSON document model shared by every graph domain.
//!
//! A [`GraphDocument`] is plain data: nodes with typed ports, edges between ports,
//! typed variables and free-form metadata. What the nodes *mean* is decided by the
//! document's [`GraphKind`](crate::GraphKind); this module knows nothing about it.
use crate::diagnostic::{GraphDiagnostic, codes};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

/// Document format version written and accepted by this crate.
pub const GRAPH_VERSION: u32 = 1;
/// Largest accepted serialized document.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
/// Hard node ceiling; a [`GraphKind`](crate::GraphKind) may only lower it.
pub const MAX_NODES: usize = 4096;
/// Hard edge ceiling; a kind may only lower it.
pub const MAX_EDGES: usize = 16384;
/// Hard variable ceiling; a kind may only lower it.
pub const MAX_VARIABLES: usize = 256;
/// Most ports one node may carry.
pub const MAX_PORTS_PER_NODE: usize = 64;
/// Most params one node may carry.
pub const MAX_PARAMS_PER_NODE: usize = 64;
/// Most metadata entries a document may carry.
pub const MAX_METADATA: usize = 64;
/// Longest node id, port name, variable name or param name.
pub const MAX_NAME_LEN: usize = 64;

/// Stable node identifier, unique within one document.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl From<&str> for NodeId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a port receives or produces connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    In,
    Out,
}

/// Value category carried by a port or stored in a variable.
///
/// `Flow` ports carry control flow (tree parent/child links, execution order) and
/// connect only to other `Flow` ports. `Any` connects to every data type.
/// `Custom` names a domain-private type that only matches itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Flow,
    Bool,
    Int,
    Float,
    String,
    Vec3,
    Entity,
    Any,
    Custom(String),
}

impl PortType {
    /// Whether a value of type `from` may feed a port of this type.
    ///
    /// Equal types connect; `Any` connects to any non-`Flow` type; `Int` widens to
    /// `Float`; `Flow` connects only to `Flow`.
    pub fn accepts(&self, from: &PortType) -> bool {
        match (from, self) {
            (PortType::Flow, PortType::Flow) => true,
            (PortType::Flow, _) | (_, PortType::Flow) => false,
            (PortType::Any, _) | (_, PortType::Any) => true,
            (PortType::Int, PortType::Float) => true,
            (a, b) => a == b,
        }
    }
    /// Whether the type can hold a stored value (variables, blackboard keys).
    pub fn is_data(&self) -> bool {
        !matches!(self, PortType::Flow)
    }
}

/// One connection point of a node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub name: String,
    pub direction: PortDirection,
    #[serde(rename = "type")]
    pub ty: PortType,
    /// Whether more than one edge may attach to this port.
    #[serde(default)]
    pub multiple: bool,
}

/// A node instance. `ports` may be omitted: the registry's declaration of
/// `node_type` is then the node's port list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub id: NodeId,
    pub node_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
    /// Editor canvas position; never interpreted by runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 2]>,
}

/// Address of one port on one node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRef {
    pub node: NodeId,
    pub port: String,
}

impl PortRef {
    pub fn new(node: &str, port: &str) -> Self {
        Self {
            node: node.into(),
            port: port.to_owned(),
        }
    }
}

/// A directed connection from an `Out` port to an `In` port.
///
/// The order of edges leaving one port is the order in `GraphDocument::edges`;
/// domains use it as child or execution order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    pub from: PortRef,
    pub to: PortRef,
}

/// A named, typed document variable (a blackboard key for behavior trees).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: PortType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A complete graph asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDocument {
    pub version: u32,
    /// Selects the [`GraphKind`](crate::GraphKind), for example `behavior_tree`.
    pub kind: String,
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub variables: Vec<GraphVariable>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl GraphDocument {
    /// An empty document of `kind` at the current format version.
    pub fn new(kind: &str) -> Self {
        Self {
            version: GRAPH_VERSION,
            kind: kind.to_owned(),
            nodes: Vec::new(),
            edges: Vec::new(),
            variables: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Parses a document, refusing oversized input and unknown fields.
    pub fn from_json(bytes: &[u8]) -> Result<Self, GraphDiagnostic> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(GraphDiagnostic::new(
                codes::DOCUMENT_TOO_LARGE,
                "",
                format!("graph document exceeds {MAX_DOCUMENT_BYTES} bytes"),
            ));
        }
        serde_json::from_slice(bytes)
            .map_err(|e| GraphDiagnostic::new(codes::DOCUMENT_MALFORMED, "", e.to_string()))
    }

    /// Parses a document already held as a JSON value.
    pub fn from_value(value: Value) -> Result<Self, GraphDiagnostic> {
        serde_json::from_value(value)
            .map_err(|e| GraphDiagnostic::new(codes::DOCUMENT_MALFORMED, "", e.to_string()))
    }

    /// Stable pretty JSON (struct field order, sorted maps) ending in a newline.
    pub fn to_json_pretty(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("graph documents are plain data");
        text.push('\n');
        text
    }

    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id.as_str() == id)
    }
    pub fn variable(&self, name: &str) -> Option<&GraphVariable> {
        self.variables.iter().find(|v| v.name == name)
    }
}

/// Read-only lookup tables over a document: node positions and the edges at each
/// port in document order. Built once by validators and compilers.
pub struct GraphIndex<'a> {
    document: &'a GraphDocument,
    nodes: BTreeMap<&'a str, usize>,
    outgoing: PortEdges<'a>,
    incoming: PortEdges<'a>,
}

type PortEdges<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<&'a GraphEdge>>>;

impl<'a> GraphIndex<'a> {
    pub fn new(document: &'a GraphDocument) -> Self {
        let mut nodes = BTreeMap::new();
        for (index, node) in document.nodes.iter().enumerate() {
            nodes.entry(node.id.as_str()).or_insert(index);
        }
        let mut outgoing: PortEdges<'a> = BTreeMap::new();
        let mut incoming: PortEdges<'a> = BTreeMap::new();
        for edge in &document.edges {
            outgoing
                .entry(edge.from.node.as_str())
                .or_default()
                .entry(edge.from.port.as_str())
                .or_default()
                .push(edge);
            incoming
                .entry(edge.to.node.as_str())
                .or_default()
                .entry(edge.to.port.as_str())
                .or_default()
                .push(edge);
        }
        Self {
            document,
            nodes,
            outgoing,
            incoming,
        }
    }
    pub fn node(&self, id: &str) -> Option<&'a GraphNode> {
        self.nodes.get(id).map(|&i| &self.document.nodes[i])
    }
    /// Position of the first node with this id in `GraphDocument::nodes`.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.nodes.get(id).copied()
    }
    /// Edges leaving `node.port`, in document order.
    pub fn outgoing(&self, node: &str, port: &str) -> &[&'a GraphEdge] {
        self.outgoing
            .get(node)
            .and_then(|ports| ports.get(port))
            .map_or(&[], Vec::as_slice)
    }
    /// Edges entering `node.port`, in document order.
    pub fn incoming(&self, node: &str, port: &str) -> &[&'a GraphEdge] {
        self.incoming
            .get(node)
            .and_then(|ports| ports.get(port))
            .map_or(&[], Vec::as_slice)
    }
    /// Target node ids of the edges leaving `node.port`, in document order.
    pub fn targets(&self, node: &str, port: &str) -> Vec<&'a str> {
        self.outgoing(node, port)
            .iter()
            .map(|e| e.to.node.as_str())
            .collect()
    }
}
