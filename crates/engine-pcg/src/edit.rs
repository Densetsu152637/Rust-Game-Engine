//! Incremental graph editing (used by MCP tools and the native builder).
use crate::graph::{Diagnostic, Edge, Graph, MAX_EDGES, MAX_NODES, Node, PortRef};
use crate::registry::Registry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// One edit. Edits keep references consistent but allow incomplete graphs (a missing required
/// input is only reported by [`crate::validate`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    AddNode {
        id: String,
        kind: String,
        #[serde(default)]
        params: BTreeMap<String, Value>,
    },
    RemoveNode {
        id: String,
    },
    Connect {
        from: PortRef,
        to: PortRef,
    },
    Disconnect {
        from: PortRef,
        to: PortRef,
    },
    SetParameter {
        node: String,
        name: String,
        value: Value,
    },
    /// Removes an explicit value so the default applies again.
    ClearParameter {
        node: String,
        name: String,
    },
    SetSeed {
        seed: u64,
    },
}

fn missing_node(id: &str) -> Diagnostic {
    Diagnostic::new("pcg.edit.unknown_node", format!("no node `{id}`")).at_node(id)
}

/// Applies `op`. On error the graph is unchanged.
pub fn apply(graph: &mut Graph, registry: &Registry, op: EditOp) -> Result<(), Diagnostic> {
    match op {
        EditOp::AddNode { id, kind, params } => {
            if graph.nodes.len() >= MAX_NODES {
                return Err(Diagnostic::new(
                    "pcg.limit.nodes",
                    format!("graphs are limited to {MAX_NODES} nodes"),
                ));
            }
            if graph.node(&id).is_some() {
                return Err(
                    Diagnostic::new("pcg.node.duplicate_id", "node id is already used")
                        .at_node(&id),
                );
            }
            let node = Node { id, kind, params };
            let probe = Graph {
                nodes: vec![node.clone()],
                ..Graph::default()
            };
            // Only structural problems of this node matter; a required spawner list may be
            // supplied by a later set_parameter.
            if let Some(d) = crate::validate(&probe, registry)
                .into_iter()
                .find(|d| !matches!(d.code.as_str(), "pcg.input.missing" | "pcg.param.required"))
            {
                return Err(d);
            }
            graph.nodes.push(node);
        }
        EditOp::RemoveNode { id } => {
            let before = graph.nodes.len();
            graph.nodes.retain(|n| n.id != id);
            if graph.nodes.len() == before {
                return Err(missing_node(&id));
            }
            graph.edges.retain(|e| e.from.node != id && e.to.node != id);
        }
        EditOp::Connect { from, to } => {
            if graph.edges.len() >= MAX_EDGES {
                return Err(Diagnostic::new(
                    "pcg.limit.edges",
                    format!("graphs are limited to {MAX_EDGES} edges"),
                ));
            }
            let mut candidate = graph.clone();
            // Replace an existing edge into the same input rather than creating a conflict.
            candidate.edges.retain(|e| e.to != to);
            candidate.edges.push(Edge { from, to });
            if let Some(d) = crate::validate(&candidate, registry).into_iter().find(|d| {
                matches!(
                    d.code.as_str(),
                    "pcg.edge.unknown_node"
                        | "pcg.edge.unknown_port"
                        | "pcg.edge.type_mismatch"
                        | "pcg.edge.duplicate"
                        | "pcg.graph.cycle"
                )
            }) {
                return Err(d);
            }
            *graph = candidate;
        }
        EditOp::Disconnect { from, to } => {
            let before = graph.edges.len();
            graph.edges.retain(|e| !(e.from == from && e.to == to));
            if graph.edges.len() == before {
                return Err(Diagnostic::new("pcg.edit.unknown_edge", "no such edge")
                    .at_node(&to.node)
                    .at_port(&to.port));
            }
        }
        EditOp::SetParameter { node, name, value } => {
            let kind = graph
                .node(&node)
                .ok_or_else(|| missing_node(&node))?
                .kind
                .clone();
            if let Some(definition) = registry.definition(&kind) {
                let spec = definition.param(&name).ok_or_else(|| {
                    Diagnostic::new(
                        "pcg.param.unknown",
                        format!("`{kind}` has no parameter `{name}`"),
                    )
                    .at_node(&node)
                })?;
                spec.check(&value).map_err(|reason| {
                    Diagnostic::new("pcg.param.invalid", format!("parameter `{name}`: {reason}"))
                        .at_node(&node)
                })?;
            }
            graph
                .nodes
                .iter_mut()
                .find(|n| n.id == node)
                .expect("node exists")
                .params
                .insert(name, value);
        }
        EditOp::ClearParameter { node, name } => {
            let target = graph
                .nodes
                .iter_mut()
                .find(|n| n.id == node)
                .ok_or_else(|| missing_node(&node))?;
            target.params.remove(&name);
        }
        EditOp::SetSeed { seed } => graph.seed = seed,
    }
    Ok(())
}

/// Fluent construction of graphs from native code.
///
/// ```
/// use engine_pcg::{GraphBuilder, Registry};
/// let registry = Registry::builtin();
/// let graph = GraphBuilder::new(&registry, 7)
///     .node("scatter", "random_sampler", [("count", 50.into())])
///     .node("trees", "spawner", [("entries", serde_json::json!([{"asset": "tree", "kind": "mesh"}]))])
///     .connect("scatter.points", "trees.points")
///     .build()
///     .unwrap();
/// assert_eq!(graph.nodes.len(), 2);
/// ```
pub struct GraphBuilder<'a> {
    registry: &'a Registry,
    graph: Graph,
    errors: Vec<Diagnostic>,
}

impl<'a> GraphBuilder<'a> {
    pub fn new(registry: &'a Registry, seed: u64) -> Self {
        Self {
            registry,
            graph: Graph::new(seed),
            errors: Vec::new(),
        }
    }
    fn apply(mut self, op: EditOp) -> Self {
        if let Err(d) = apply(&mut self.graph, self.registry, op) {
            self.errors.push(d);
        }
        self
    }
    /// Adds a node with explicit parameters (unspecified ones use defaults).
    pub fn node<const N: usize>(self, id: &str, kind: &str, params: [(&str, Value); N]) -> Self {
        self.apply(EditOp::AddNode {
            id: id.into(),
            kind: kind.into(),
            params: params.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        })
    }
    /// Connects `"node.port"` to `"node.port"`.
    pub fn connect(self, from: &str, to: &str) -> Self {
        match (split_port(from), split_port(to)) {
            (Some(from), Some(to)) => self.apply(EditOp::Connect { from, to }),
            _ => {
                let mut this = self;
                this.errors.push(Diagnostic::new(
                    "pcg.edit.port_syntax",
                    format!("expected `node.port`, got `{from}` -> `{to}`"),
                ));
                this
            }
        }
    }
    /// Returns the graph when every edit applied and the whole graph validates.
    pub fn build(self) -> Result<Graph, Vec<Diagnostic>> {
        let mut problems = self.errors;
        problems.extend(crate::validate(&self.graph, self.registry));
        if problems.is_empty() {
            Ok(self.graph)
        } else {
            Err(problems)
        }
    }
}

fn split_port(text: &str) -> Option<PortRef> {
    let (node, port) = text.rsplit_once('.')?;
    (!node.is_empty() && !port.is_empty()).then(|| PortRef::new(node, port))
}
