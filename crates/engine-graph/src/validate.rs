//! Document validation against a [`GraphKind`]; deterministic and bounded.
//!
//! Two strictness levels exist because edits pass through incomplete states:
//! [`Strictness::Structure`] checks what must hold at every moment (identity,
//! types, bounds, port rules, cycles); [`Strictness::Complete`] adds what must
//! hold for a finished graph (required params and ports, the root rule,
//! reachability, and the kind's own check).
use crate::{
    diagnostic::{GraphDiagnostic, Sink, codes},
    kind::{GraphKind, NodeTypeDef, ParamDef, ParamType},
    model::{
        GRAPH_VERSION, GraphDocument, GraphEdge, GraphVariable, MAX_METADATA, MAX_NAME_LEN,
        MAX_PARAMS_PER_NODE, MAX_PORTS_PER_NODE, Port, PortDirection, PortType,
    },
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// How much of the rule set a validation applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strictness {
    Structure,
    Complete,
}

const MAX_VALUE_NODES: usize = 512;
const MAX_VALUE_DEPTH: usize = 8;
const MAX_STRING_BYTES: usize = 4096;
const DEFAULT_PARAM_STRING: usize = 256;
const MAX_DESCRIPTION: usize = 256;
const MAX_POSITION: f32 = 1.0e6;

/// Whether `name` is a valid node id, port, variable, param or metadata key.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Bounds nesting, entry count and string length of an embedded JSON value.
pub fn value_within_limits(value: &Value) -> bool {
    let mut stack = vec![(value, 1usize)];
    let mut count = 0usize;
    while let Some((value, depth)) = stack.pop() {
        count += 1;
        if count > MAX_VALUE_NODES || depth > MAX_VALUE_DEPTH {
            return false;
        }
        match value {
            Value::String(s) if s.len() > MAX_STRING_BYTES => return false,
            Value::Array(items) => stack.extend(items.iter().map(|v| (v, depth + 1))),
            Value::Object(map) => {
                if map.keys().any(|k| k.len() > MAX_STRING_BYTES) {
                    return false;
                }
                stack.extend(map.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    true
}

/// Whether a JSON value is a legal value of a data type: `Bool` boolean, `Int`
/// integer, `Float` number, `String` string, `Vec3` array of three numbers,
/// `Entity` string (an authored entity id); `Any` and `Custom` accept anything.
pub fn value_conforms(ty: &PortType, value: &Value) -> bool {
    match ty {
        PortType::Flow => false,
        PortType::Bool => value.is_boolean(),
        PortType::Int => value.as_i64().is_some() || value.as_u64().is_some(),
        PortType::Float => value.is_number(),
        PortType::String | PortType::Entity => value.is_string(),
        PortType::Vec3 => value
            .as_array()
            .is_some_and(|a| a.len() == 3 && a.iter().all(Value::is_number)),
        PortType::Any | PortType::Custom(_) => true,
    }
}

/// Validates `doc` against `kind`. Output order is deterministic: document
/// header, variables, metadata, nodes in array order, edges in array order, then
/// whole-graph rules.
pub fn validate(doc: &GraphDocument, kind: &GraphKind, level: Strictness) -> Vec<GraphDiagnostic> {
    let mut sink = Sink::default();
    if doc.version != GRAPH_VERSION {
        sink.push(
            codes::VERSION_UNSUPPORTED,
            "",
            format!(
                "graph version must be {GRAPH_VERSION}, found {}",
                doc.version
            ),
        );
    }
    if doc.kind != kind.name {
        sink.push(
            codes::KIND_MISMATCH,
            "",
            format!(
                "document kind {:?} is validated as {:?}",
                doc.kind, kind.name
            ),
        );
    }
    if !within_limits(doc, kind, &mut sink) {
        return sink.finish();
    }
    let variables = check_variables(doc, kind, &mut sink);
    check_metadata(doc, &mut sink);
    let ports = check_nodes(doc, kind, &variables, level, &mut sink);
    let linked = check_edges(doc, &ports, &mut sink);
    if level == Strictness::Complete {
        check_required_ports(doc, kind, &linked, &mut sink);
    }
    if !kind.allow_cycles {
        check_cycles(&ports, &linked, &mut sink);
    }
    if level == Strictness::Complete && kind.single_root {
        check_root(doc, kind, &ports, &linked, &mut sink);
    }
    let mut diagnostics = sink.finish();
    if level == Strictness::Complete
        && diagnostics.is_empty()
        && let Some(check) = kind.check
    {
        diagnostics = check(doc);
        diagnostics.truncate(crate::diagnostic::MAX_DIAGNOSTICS);
    }
    diagnostics
}

fn within_limits(doc: &GraphDocument, kind: &GraphKind, sink: &mut Sink) -> bool {
    let limits = kind.limits.effective();
    let mut ok = true;
    if doc.nodes.len() > limits.max_nodes {
        ok = false;
        sink.push(
            codes::LIMIT_NODES,
            "",
            format!(
                "{} nodes exceed the limit of {}",
                doc.nodes.len(),
                limits.max_nodes
            ),
        );
    }
    if doc.edges.len() > limits.max_edges {
        ok = false;
        sink.push(
            codes::LIMIT_EDGES,
            "",
            format!(
                "{} edges exceed the limit of {}",
                doc.edges.len(),
                limits.max_edges
            ),
        );
    }
    if doc.variables.len() > limits.max_variables {
        ok = false;
        sink.push(
            codes::LIMIT_VARIABLES,
            "",
            format!(
                "{} variables exceed the limit of {}",
                doc.variables.len(),
                limits.max_variables
            ),
        );
    }
    ok
}

fn check_variables<'a>(
    doc: &'a GraphDocument,
    kind: &GraphKind,
    sink: &mut Sink,
) -> BTreeMap<&'a str, &'a GraphVariable> {
    let mut seen = BTreeMap::new();
    for variable in &doc.variables {
        let path = format!("/variables/{}", variable.name);
        if !valid_name(&variable.name) {
            sink.push(
                codes::VARIABLE_NAME_INVALID,
                path,
                "variable names are 1-64 characters of letters, digits, '_', '-' or '.'",
            );
            continue;
        }
        if seen.contains_key(variable.name.as_str()) {
            sink.push(codes::VARIABLE_DUPLICATE, path, "variable name repeats");
            continue;
        }
        if !variable.ty.is_data()
            || (!kind.variable_types.is_empty() && !kind.variable_types.contains(&variable.ty))
        {
            sink.push(
                codes::VARIABLE_TYPE,
                &path,
                format!(
                    "{:?} is not a variable type of kind {}",
                    variable.ty, kind.name
                ),
            );
        }
        if variable.description.len() > MAX_DESCRIPTION {
            sink.push(codes::LIMIT_VALUE, &path, "description is too long");
        }
        if let Some(default) = &variable.default {
            if !value_within_limits(default) {
                sink.push(codes::LIMIT_VALUE, &path, "default value is too large");
            } else if !value_conforms(&variable.ty, default) {
                sink.push(
                    codes::VARIABLE_DEFAULT,
                    &path,
                    format!("default does not match type {:?}", variable.ty),
                );
            }
        }
        seen.insert(variable.name.as_str(), variable);
    }
    seen
}

fn check_metadata(doc: &GraphDocument, sink: &mut Sink) {
    if doc.metadata.len() > MAX_METADATA {
        sink.push(
            codes::LIMIT_METADATA,
            "/metadata",
            format!("more than {MAX_METADATA} metadata entries"),
        );
    }
    for (key, value) in &doc.metadata {
        let path = format!("/metadata/{key}");
        if !valid_name(key) {
            sink.push(codes::METADATA_KEY_INVALID, &path, "invalid metadata key");
        }
        if !value_within_limits(value) {
            sink.push(codes::LIMIT_VALUE, &path, "metadata value is too large");
        }
    }
}

/// Effective ports per first-occurrence node id; `None` when the type is unknown.
type NodePorts<'a> = BTreeMap<&'a str, Option<Vec<Port>>>;

fn check_nodes<'a>(
    doc: &'a GraphDocument,
    kind: &GraphKind,
    variables: &BTreeMap<&str, &GraphVariable>,
    level: Strictness,
    sink: &mut Sink,
) -> NodePorts<'a> {
    let mut ports: NodePorts<'a> = BTreeMap::new();
    for node in &doc.nodes {
        let id = node.id.as_str();
        let path = format!("/nodes/{id}");
        if !valid_name(id) {
            sink.push(
                codes::NODE_ID_INVALID,
                &path,
                "node ids are 1-64 characters of letters, digits, '_', '-' or '.'",
            );
            continue;
        }
        if ports.contains_key(id) {
            sink.push(codes::NODE_DUPLICATE, &path, "node id repeats");
            continue;
        }
        if let Some(position) = node.position
            && position
                .iter()
                .any(|c| !c.is_finite() || c.abs() > MAX_POSITION)
        {
            sink.push(
                codes::NODE_POSITION_INVALID,
                &path,
                "position must be finite and within +/-1e6",
            );
        }
        let Some(def) = kind.registry.get(&node.node_type) else {
            sink.push(
                codes::NODE_UNKNOWN_TYPE,
                &path,
                format!("kind {} has no node type {:?}", kind.name, node.node_type),
            );
            ports.insert(id, None);
            continue;
        };
        ports.insert(
            id,
            Some(node_ports(node.ports.as_slice(), def, &path, sink)),
        );
        check_params(node.params.iter(), def, variables, &path, sink);
        if level == Strictness::Complete {
            for param in &def.params {
                if param.required
                    && param.default.is_none()
                    && !node.params.contains_key(&param.name)
                {
                    sink.push(
                        codes::PARAM_MISSING,
                        format!("{path}/params/{}", param.name),
                        "required parameter is missing",
                    );
                }
            }
        }
    }
    ports
}

/// The ports a node effectively has; reports explicit ports that disagree with
/// the registry declaration.
fn node_ports(explicit: &[Port], def: &NodeTypeDef, path: &str, sink: &mut Sink) -> Vec<Port> {
    let declared = def.declared_ports();
    if explicit.is_empty() {
        return declared;
    }
    if explicit.len() > MAX_PORTS_PER_NODE {
        sink.push(
            codes::LIMIT_PORTS,
            path,
            format!("more than {MAX_PORTS_PER_NODE} ports"),
        );
        return declared;
    }
    let mut names = BTreeSet::new();
    for port in explicit {
        if !valid_name(&port.name) || !names.insert(port.name.as_str()) {
            sink.push(
                codes::PORT_DUPLICATE,
                format!("{path}/ports/{}", port.name),
                "port names must be valid and unique within a node",
            );
        }
    }
    for want in &declared {
        if !explicit.iter().any(|p| p == want) {
            sink.push(
                codes::NODE_PORTS_MISMATCH,
                format!("{path}/ports/{}", want.name),
                format!("node type {} declares this port as {want:?}", def.name),
            );
        }
    }
    if !def.extensible {
        for port in explicit {
            if !declared.iter().any(|d| d.name == port.name) {
                sink.push(
                    codes::NODE_PORTS_MISMATCH,
                    format!("{path}/ports/{}", port.name),
                    format!("node type {} declares no such port", def.name),
                );
            }
        }
    }
    explicit.to_vec()
}

fn check_params<'a>(
    params: impl ExactSizeIterator<Item = (&'a String, &'a Value)>,
    def: &NodeTypeDef,
    variables: &BTreeMap<&str, &GraphVariable>,
    path: &str,
    sink: &mut Sink,
) {
    if params.len() > MAX_PARAMS_PER_NODE {
        sink.push(
            codes::LIMIT_PARAMS,
            path,
            format!("more than {MAX_PARAMS_PER_NODE} params"),
        );
        return;
    }
    for (name, value) in params {
        let here = format!("{path}/params/{name}");
        let Some(param) = def.param_def(name) else {
            sink.push(
                codes::PARAM_UNKNOWN,
                here,
                format!("node type {} has no parameter {name:?}", def.name),
            );
            continue;
        };
        if !value_within_limits(value) {
            sink.push(codes::LIMIT_VALUE, here, "parameter value is too large");
            continue;
        }
        check_param(param, value, variables, &here, sink);
    }
}

fn check_param(
    param: &ParamDef,
    value: &Value,
    variables: &BTreeMap<&str, &GraphVariable>,
    path: &str,
    sink: &mut Sink,
) {
    let type_error = |sink: &mut Sink, expected: &str| {
        sink.push(codes::PARAM_TYPE, path, format!("expected {expected}"));
    };
    let in_range = |number: f64| {
        param.min.is_none_or(|min| number >= min) && param.max.is_none_or(|max| number <= max)
    };
    let range_error = |sink: &mut Sink| {
        sink.push(
            codes::PARAM_RANGE,
            path,
            format!("outside [{:?}, {:?}]", param.min, param.max),
        );
    };
    match param.ty {
        ParamType::Bool => {
            if !value.is_boolean() {
                type_error(sink, "a boolean");
            }
        }
        ParamType::Int => {
            if value.as_i64().is_none() && value.as_u64().is_none() {
                type_error(sink, "an integer");
            } else if !in_range(value.as_f64().unwrap_or(f64::NAN)) {
                range_error(sink);
            }
        }
        ParamType::Float => match value.as_f64() {
            Some(number) if number.is_finite() => {
                if !in_range(number) {
                    range_error(sink);
                }
            }
            _ => type_error(sink, "a finite number"),
        },
        ParamType::String => match value.as_str() {
            Some(text) => {
                if text.chars().count() > param.max_len.unwrap_or(DEFAULT_PARAM_STRING) {
                    sink.push(codes::PARAM_RANGE, path, "string is too long");
                }
            }
            None => type_error(sink, "a string"),
        },
        ParamType::Vec3 => match value.as_array() {
            Some(items) if items.len() == 3 && items.iter().all(|v| v.as_f64().is_some()) => {
                if !items
                    .iter()
                    .all(|v| in_range(v.as_f64().unwrap_or(f64::NAN)))
                {
                    range_error(sink);
                }
            }
            _ => type_error(sink, "an array of three numbers"),
        },
        ParamType::Enum => match value.as_str() {
            Some(text) if param.choices.iter().any(|c| c == text) => {}
            Some(_) => sink.push(
                codes::PARAM_RANGE,
                path,
                format!("expected one of {:?}", param.choices),
            ),
            None => type_error(sink, "a string"),
        },
        ParamType::Variable => match value.as_str() {
            Some(name) => match variables.get(name) {
                None => sink.push(
                    codes::PARAM_VARIABLE,
                    path,
                    format!("no variable named {name:?}"),
                ),
                Some(variable)
                    if !param.variable_types.is_empty()
                        && !param.variable_types.contains(&variable.ty) =>
                {
                    sink.push(
                        codes::PARAM_VARIABLE,
                        path,
                        format!("variable {name:?} has type {:?}", variable.ty),
                    );
                }
                Some(_) => {}
            },
            None => type_error(sink, "a variable name"),
        },
        ParamType::Json => {}
    }
}

/// An edge whose endpoints, ports and directions resolved; type mismatches are
/// reported but the edge still takes part in graph traversal.
type LinkedEdges<'a> = Vec<&'a GraphEdge>;

pub(crate) fn edge_path(edge: &GraphEdge) -> String {
    format!(
        "/edges/{}.{}->{}.{}",
        edge.from.node, edge.from.port, edge.to.node, edge.to.port
    )
}

fn find_port<'a>(ports: &'a NodePorts<'_>, node: &str, port: &str) -> Option<Option<&'a Port>> {
    // Outer None: unknown node. Inner None: node of unknown type or unknown port.
    ports
        .get(node)
        .map(|p| p.as_ref().and_then(|p| p.iter().find(|p| p.name == port)))
}

fn check_edges<'a>(
    doc: &'a GraphDocument,
    ports: &NodePorts<'_>,
    sink: &mut Sink,
) -> LinkedEdges<'a> {
    let mut linked = Vec::new();
    let mut seen = BTreeSet::new();
    for edge in &doc.edges {
        let path = edge_path(edge);
        if !seen.insert((&edge.from, &edge.to)) {
            sink.push(codes::EDGE_DUPLICATE, &path, "edge repeats");
            continue;
        }
        let mut resolved: [Option<&Port>; 2] = [None, None];
        for (slot, end) in [&edge.from, &edge.to].into_iter().enumerate() {
            match ports.get(end.node.as_str()) {
                None => sink.push(
                    codes::EDGE_UNKNOWN_NODE,
                    &path,
                    format!("no node {:?}", end.node.as_str()),
                ),
                // Unknown node type: already reported once on the node.
                Some(None) => {}
                Some(Some(list)) => match list.iter().find(|p| p.name == end.port) {
                    Some(port) => resolved[slot] = Some(port),
                    None => sink.push(
                        codes::EDGE_UNKNOWN_PORT,
                        &path,
                        format!("node {:?} has no port {:?}", end.node.as_str(), end.port),
                    ),
                },
            }
        }
        let [Some(from), Some(to)] = resolved else {
            continue;
        };
        if from.direction != PortDirection::Out || to.direction != PortDirection::In {
            sink.push(
                codes::EDGE_DIRECTION,
                &path,
                "edges run from an out port to an in port",
            );
            continue;
        }
        if !to.ty.accepts(&from.ty) {
            sink.push(
                codes::EDGE_TYPE_MISMATCH,
                &path,
                format!("{:?} cannot feed {:?}", from.ty, to.ty),
            );
        }
        linked.push(edge);
    }
    let mut degree: BTreeMap<(&str, &str, PortDirection), usize> = BTreeMap::new();
    for edge in &linked {
        *degree
            .entry((edge.from.node.as_str(), &edge.from.port, PortDirection::Out))
            .or_default() += 1;
        *degree
            .entry((edge.to.node.as_str(), &edge.to.port, PortDirection::In))
            .or_default() += 1;
    }
    for ((node, port, _), count) in degree {
        if count > 1
            && find_port(ports, node, port)
                .flatten()
                .is_some_and(|p| !p.multiple)
        {
            sink.push(
                codes::EDGE_MULTIPLE,
                format!("/nodes/{node}/ports/{port}"),
                format!("{count} edges attach to a single-connection port"),
            );
        }
    }
    linked
}

fn check_required_ports(
    doc: &GraphDocument,
    kind: &GraphKind,
    linked: &LinkedEdges<'_>,
    sink: &mut Sink,
) {
    let mut connected: BTreeSet<(&str, &str)> = BTreeSet::new();
    for edge in linked {
        connected.insert((edge.from.node.as_str(), edge.from.port.as_str()));
        connected.insert((edge.to.node.as_str(), edge.to.port.as_str()));
    }
    let mut seen = BTreeSet::new();
    for node in &doc.nodes {
        let Some(def) = kind.registry.get(&node.node_type) else {
            continue;
        };
        if !seen.insert(node.id.as_str()) {
            continue;
        }
        for port in def.ports.iter().filter(|p| p.required) {
            if !connected.contains(&(node.id.as_str(), port.port.name.as_str())) {
                sink.push(
                    codes::PORT_UNCONNECTED,
                    format!("/nodes/{}/ports/{}", node.id, port.port.name),
                    "required port has no edge",
                );
            }
        }
    }
}

fn check_cycles(ports: &NodePorts<'_>, linked: &LinkedEdges<'_>, sink: &mut Sink) {
    let mut forward: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut backward: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut outdegree: BTreeMap<&str, usize> = BTreeMap::new();
    for node in ports.keys() {
        indegree.insert(node, 0);
        outdegree.insert(node, 0);
    }
    for edge in linked {
        let (a, b) = (edge.from.node.as_str(), edge.to.node.as_str());
        forward.entry(a).or_default().push(b);
        backward.entry(b).or_default().push(a);
        *indegree.entry(b).or_default() += 1;
        *outdegree.entry(a).or_default() += 1;
    }
    let mut live: BTreeSet<&str> = ports.keys().copied().collect();
    for (adjacent, mut degree) in [(&forward, indegree), (&backward, outdegree)] {
        let mut queue: VecDeque<&str> = degree
            .iter()
            .filter(|(n, d)| **d == 0 && live.contains(*n))
            .map(|(n, _)| *n)
            .collect();
        let mut done = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            done.insert(node);
            for next in adjacent.get(node).into_iter().flatten() {
                let d = degree.get_mut(next).expect("every node has a degree");
                *d -= 1;
                if *d == 0 {
                    queue.push_back(next);
                }
            }
        }
        live.retain(|n| !done.contains(n));
    }
    if let Some(first) = live.iter().next() {
        sink.push(
            codes::CYCLE,
            format!("/nodes/{first}"),
            format!("{} node(s) lie on or between directed cycles", live.len()),
        );
    }
}

fn check_root(
    doc: &GraphDocument,
    kind: &GraphKind,
    ports: &NodePorts<'_>,
    linked: &LinkedEdges<'_>,
    sink: &mut Sink,
) {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    for node in &doc.nodes {
        if seen.insert(node.id.as_str())
            && kind.registry.get(&node.node_type).is_some_and(|d| d.root)
        {
            roots.push(node.id.as_str());
        }
    }
    match roots.len() {
        0 => sink.push(codes::ROOT_MISSING, "", "the graph has no root node"),
        1 => {}
        _ => {
            for extra in &roots[1..] {
                sink.push(
                    codes::ROOT_MULTIPLE,
                    format!("/nodes/{extra}"),
                    "a graph has exactly one root node",
                );
            }
        }
    }
    let mut adjacent: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in linked {
        adjacent
            .entry(edge.from.node.as_str())
            .or_default()
            .push(edge.to.node.as_str());
    }
    let mut reached: BTreeSet<&str> = roots.iter().copied().collect();
    let mut queue: VecDeque<&str> = roots.iter().copied().collect();
    while let Some(node) = queue.pop_front() {
        for next in adjacent.get(node).into_iter().flatten() {
            if reached.insert(next) {
                queue.push_back(next);
            }
        }
    }
    if roots.is_empty() {
        return;
    }
    for (node, known) in ports {
        if known.is_some() && !reached.contains(node) {
            sink.push(
                codes::NODE_UNREACHABLE,
                format!("/nodes/{node}"),
                "node is not reachable from the root",
            );
        }
    }
}
