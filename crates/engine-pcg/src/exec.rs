//! Graph validation and the deterministic, bounded executor.
use crate::data::{Data, Instance};
use crate::graph::{Diagnostic, Graph, MAX_EDGES, MAX_ID_LEN, MAX_NODES};
use crate::registry::{NodeContext, Params, Registry, RunSettings};
use crate::rng::derive_seed;
use crate::surface::Surface;
use std::collections::{BTreeMap, BTreeSet};

/// Checks a graph and returns every problem found (empty when valid).
///
/// Checks: size limits, node id validity and uniqueness, unknown node kinds, unknown ports
/// and parameters, parameter types/ranges, edge type compatibility, one edge per input,
/// missing required inputs, and cycles.
pub fn validate(graph: &Graph, registry: &Registry) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if graph.nodes.len() > MAX_NODES {
        out.push(Diagnostic::new(
            "pcg.limit.nodes",
            format!(
                "{} nodes exceed the limit of {MAX_NODES}",
                graph.nodes.len()
            ),
        ));
    }
    if graph.edges.len() > MAX_EDGES {
        out.push(Diagnostic::new(
            "pcg.limit.edges",
            format!(
                "{} edges exceed the limit of {MAX_EDGES}",
                graph.edges.len()
            ),
        ));
    }
    if graph.nodes.len() > MAX_NODES * 4 || graph.edges.len() > MAX_EDGES * 4 {
        return out; // refuse to analyse absurdly large graphs
    }
    let mut kinds = BTreeMap::new();
    for node in &graph.nodes {
        if node.id.is_empty()
            || node.id.len() > MAX_ID_LEN
            || !node
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            out.push(
                Diagnostic::new(
                    "pcg.node.invalid_id",
                    format!(
                        "node id `{}` must be 1..={MAX_ID_LEN} characters of [A-Za-z0-9_.-]",
                        node.id
                    ),
                )
                .at_node(&node.id),
            );
        }
        if kinds.insert(node.id.as_str(), node).is_some() {
            out.push(
                Diagnostic::new("pcg.node.duplicate_id", "node id is used more than once")
                    .at_node(&node.id),
            );
        }
        let Some(definition) = registry.definition(&node.kind) else {
            out.push(
                Diagnostic::new(
                    "pcg.node.unknown_kind",
                    format!("unknown node kind `{}`", node.kind),
                )
                .at_node(&node.id),
            );
            continue;
        };
        for (name, value) in &node.params {
            match definition.param(name) {
                None => out.push(
                    Diagnostic::new(
                        "pcg.param.unknown",
                        format!("`{}` has no parameter `{name}`", node.kind),
                    )
                    .at_node(&node.id),
                ),
                Some(spec) => {
                    if let Err(reason) = spec.check(value) {
                        out.push(
                            Diagnostic::new(
                                "pcg.param.invalid",
                                format!("parameter `{name}`: {reason}"),
                            )
                            .at_node(&node.id),
                        );
                    }
                }
            }
        }
        // Parameters whose default is not usable must be set explicitly.
        for spec in &definition.params {
            if matches!(spec.ty, crate::registry::ParamType::Entries)
                && !node.params.contains_key(&spec.name)
            {
                out.push(
                    Diagnostic::new(
                        "pcg.param.required",
                        format!("parameter `{}` must be set", spec.name),
                    )
                    .at_node(&node.id),
                );
            }
        }
    }

    let mut connected: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut seen_edges = BTreeSet::new();
    for edge in &graph.edges {
        if !seen_edges.insert(edge) {
            out.push(Diagnostic::new(
                "pcg.edge.duplicate",
                format!(
                    "edge {}.{} -> {}.{} is listed twice",
                    edge.from.node, edge.from.port, edge.to.node, edge.to.port
                ),
            ));
            continue;
        }
        let (from, to) = (
            kinds.get(edge.from.node.as_str()),
            kinds.get(edge.to.node.as_str()),
        );
        if from.is_none() {
            out.push(
                Diagnostic::new("pcg.edge.unknown_node", "edge source node does not exist")
                    .at_node(&edge.from.node)
                    .at_port(&edge.from.port),
            );
        }
        if to.is_none() {
            out.push(
                Diagnostic::new("pcg.edge.unknown_node", "edge target node does not exist")
                    .at_node(&edge.to.node)
                    .at_port(&edge.to.port),
            );
        }
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        let (Some(from_def), Some(to_def)) = (
            registry.definition(&from.kind),
            registry.definition(&to.kind),
        ) else {
            continue; // unknown kinds are already reported
        };
        let source = from_def.output(&edge.from.port);
        let target = to_def.input(&edge.to.port);
        if source.is_none() {
            out.push(
                Diagnostic::new(
                    "pcg.edge.unknown_port",
                    format!("`{}` has no output port `{}`", from.kind, edge.from.port),
                )
                .at_node(&edge.from.node)
                .at_port(&edge.from.port),
            );
        }
        if target.is_none() {
            out.push(
                Diagnostic::new(
                    "pcg.edge.unknown_port",
                    format!("`{}` has no input port `{}`", to.kind, edge.to.port),
                )
                .at_node(&edge.to.node)
                .at_port(&edge.to.port),
            );
        }
        let (Some(source), Some(target)) = (source, target) else {
            continue;
        };
        if source.ty != target.ty {
            out.push(
                Diagnostic::new(
                    "pcg.edge.type_mismatch",
                    format!(
                        "cannot connect {:?} output {}.{} to {:?} input",
                        source.ty, edge.from.node, edge.from.port, target.ty
                    ),
                )
                .at_node(&edge.to.node)
                .at_port(&edge.to.port),
            );
        }
        let count = connected
            .entry((edge.to.node.as_str(), edge.to.port.as_str()))
            .or_default();
        *count += 1;
        if *count == 2 {
            out.push(
                Diagnostic::new("pcg.input.multiple", "an input accepts only one edge")
                    .at_node(&edge.to.node)
                    .at_port(&edge.to.port),
            );
        }
    }
    for node in &graph.nodes {
        let Some(definition) = registry.definition(&node.kind) else {
            continue;
        };
        for input in definition.inputs.iter().filter(|i| i.required) {
            if !connected.contains_key(&(node.id.as_str(), input.name.as_str())) {
                out.push(
                    Diagnostic::new(
                        "pcg.input.missing",
                        format!("required input `{}` is not connected", input.name),
                    )
                    .at_node(&node.id)
                    .at_port(&input.name),
                );
            }
        }
    }
    if let Err(cycle) = topological_order(graph) {
        out.push(Diagnostic::new(
            "pcg.graph.cycle",
            format!("graph contains a cycle through: {}", cycle.join(", ")),
        ));
    }
    out
}

/// Deterministic topological order (ties broken by node id). `Err` lists the nodes that are
/// part of or downstream of a cycle.
fn topological_order(graph: &Graph) -> Result<Vec<&str>, Vec<String>> {
    let mut indegree: BTreeMap<&str, usize> =
        graph.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        if let (Some(_), Some(degree)) = (
            indegree.get(edge.from.node.as_str()),
            indegree.get(edge.to.node.as_str()).copied(),
        ) {
            indegree.insert(edge.to.node.as_str(), degree + 1);
            outgoing
                .entry(edge.from.node.as_str())
                .or_default()
                .push(edge.to.node.as_str());
        }
    }
    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order = Vec::with_capacity(indegree.len());
    while let Some(id) = ready.pop_first() {
        order.push(id);
        for next in outgoing.get(id).into_iter().flatten() {
            let degree = indegree.get_mut(next).expect("edge target indexed");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(next);
            }
        }
    }
    if order.len() == indegree.len() {
        Ok(order)
    } else {
        let done: BTreeSet<&str> = order.into_iter().collect();
        Err(indegree
            .keys()
            .filter(|id| !done.contains(*id))
            .map(|id| (*id).to_owned())
            .collect())
    }
}

/// Per-node summary of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeReport {
    pub id: String,
    pub kind: String,
    /// Points produced by the node (0 when it has no point output).
    pub points: usize,
    /// Instances produced by the node.
    pub instances: usize,
}

/// Result of a successful run.
#[derive(Clone, Debug)]
pub struct RunResult {
    /// Instances of all sink spawners (instance outputs nobody consumes), in execution order.
    pub instances: Vec<Instance>,
    /// One report per node in execution order.
    pub nodes: Vec<NodeReport>,
    /// Every node's outputs by node id then port name, for inspection and custom consumers.
    pub outputs: BTreeMap<String, BTreeMap<String, Data>>,
}

/// Failure of [`run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunError {
    pub diagnostics: Vec<Diagnostic>,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lines: Vec<String> = self.diagnostics.iter().map(ToString::to_string).collect();
        write!(f, "{}", lines.join("; "))
    }
}

impl std::error::Error for RunError {}

/// Validates then executes `graph` against `surface`.
///
/// Nodes run in a fixed topological order (ties by node id) and each draws randomness only
/// from [`derive_seed`]`(settings.seed, node id)`, so the result is a pure function of
/// `(graph, settings, surface)`. Output sizes are bounded by `settings.limits`.
pub fn run(
    graph: &Graph,
    registry: &Registry,
    surface: &dyn Surface,
    settings: &RunSettings,
) -> Result<RunResult, RunError> {
    let diagnostics = validate(graph, registry);
    if !diagnostics.is_empty() {
        return Err(RunError { diagnostics });
    }
    let order = topological_order(graph).expect("validated graph is acyclic");
    let nodes: BTreeMap<&str, _> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut incoming: BTreeMap<&str, Vec<&crate::graph::Edge>> = BTreeMap::new();
    let mut consumed: BTreeSet<(&str, &str)> = BTreeSet::new();
    for edge in &graph.edges {
        incoming
            .entry(edge.to.node.as_str())
            .or_default()
            .push(edge);
        consumed.insert((edge.from.node.as_str(), edge.from.port.as_str()));
    }
    let mut outputs: BTreeMap<String, BTreeMap<String, Data>> = BTreeMap::new();
    let mut reports = Vec::new();
    let mut instances = Vec::new();
    for id in order {
        let node = nodes[id];
        let definition = registry.definition(&node.kind).expect("validated kind");
        let function = registry.function(&node.kind).expect("validated kind");
        let mut inputs = BTreeMap::new();
        for edge in incoming.get(id).into_iter().flatten() {
            let data = outputs
                .get(edge.from.node.as_str())
                .and_then(|o| o.get(&edge.from.port))
                .cloned()
                .ok_or_else(|| RunError {
                    diagnostics: vec![
                        Diagnostic::new(
                            "pcg.node.missing_output",
                            format!(
                                "upstream `{}.{}` produced no value",
                                edge.from.node, edge.from.port
                            ),
                        )
                        .at_node(id),
                    ],
                })?;
            inputs.insert(edge.to.port.clone(), data);
        }
        let ctx = NodeContext {
            node_id: id,
            seed: derive_seed(settings.seed, id),
            params: Params { node, definition },
            inputs: &inputs,
            surface,
            run: settings,
        };
        let produced = function(&ctx).map_err(|e| RunError {
            diagnostics: vec![e.into_diagnostic(id)],
        })?;
        for spec in &definition.outputs {
            match produced.get(&spec.name) {
                Some(data) if data.port_type() == spec.ty => {}
                _ => {
                    return Err(RunError {
                        diagnostics: vec![
                            Diagnostic::new(
                                "pcg.node.bad_output",
                                format!("node did not produce a {:?} on `{}`", spec.ty, spec.name),
                            )
                            .at_node(id)
                            .at_port(&spec.name),
                        ],
                    });
                }
            }
        }
        let (mut point_count, mut instance_count) = (0, 0);
        for (port, data) in &produced {
            match data {
                Data::Points(p) => {
                    point_count += p.len();
                    if p.len() > settings.limits.max_points {
                        return Err(limit_error(
                            id,
                            "points",
                            p.len(),
                            settings.limits.max_points,
                        ));
                    }
                }
                Data::Instances(list) => {
                    instance_count += list.len();
                    if list.len() > settings.limits.max_instances {
                        return Err(limit_error(
                            id,
                            "instances",
                            list.len(),
                            settings.limits.max_instances,
                        ));
                    }
                    if !consumed.contains(&(id, port.as_str())) {
                        instances.extend(list.iter().cloned());
                        if instances.len() > settings.limits.max_instances {
                            return Err(limit_error(
                                id,
                                "instances",
                                instances.len(),
                                settings.limits.max_instances,
                            ));
                        }
                    }
                }
                Data::Field(_) => {}
            }
        }
        reports.push(NodeReport {
            id: id.to_owned(),
            kind: node.kind.clone(),
            points: point_count,
            instances: instance_count,
        });
        outputs.insert(id.to_owned(), produced);
    }
    Ok(RunResult {
        instances,
        nodes: reports,
        outputs,
    })
}

fn limit_error(node: &str, what: &str, count: usize, limit: usize) -> RunError {
    RunError {
        diagnostics: vec![
            Diagnostic::new(
                &format!("pcg.limit.{what}"),
                format!("{count} {what} exceed the limit of {limit}"),
            )
            .at_node(node),
        ],
    }
}
