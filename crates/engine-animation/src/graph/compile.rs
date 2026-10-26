use super::{BlendSpace1D, BlendSpace2D};
use engine_graph::{GraphDiagnostic, GraphDocument, PortType, Strictness};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(super) enum Node {
    Output(usize),
    Clip {
        id: String,
        speed: f32,
        looping: bool,
    },
    Blend {
        a: usize,
        b: usize,
        parameter: String,
    },
    One {
        inputs: Vec<usize>,
        parameter: String,
        space: BlendSpace1D,
    },
    Two {
        inputs: Vec<usize>,
        x: String,
        y: String,
        space: BlendSpace2D,
    },
    Ik {
        input: usize,
        chain: [usize; 3],
        limits: crate::ik::BendLimits,
        target: String,
        pole: String,
        weight: String,
    },
    Machine {
        inputs: Vec<usize>,
        initial: usize,
        transitions: Vec<Transition>,
    },
}
impl Node {
    pub fn inputs(&self) -> Vec<usize> {
        match self {
            Self::Output(i) => vec![*i],
            Self::Ik { input, .. } => vec![*input],
            Self::Clip { .. } => vec![],
            Self::Blend { a, b, .. } => vec![*a, *b],
            Self::One { inputs, .. } | Self::Two { inputs, .. } | Self::Machine { inputs, .. } => {
                inputs.clone()
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operator {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionDocument {
    from: String,
    to: String,
    parameter: String,
    op: Operator,
    value: Value,
    duration: f32,
}
#[derive(Clone, Debug)]
pub(super) struct Transition {
    pub from: usize,
    pub to: usize,
    pub parameter: String,
    pub op: Operator,
    pub value: Value,
    pub duration: f32,
}
impl Transition {
    pub fn matches(&self, parameters: &BTreeMap<String, Value>) -> bool {
        let a = &parameters[&self.parameter];
        match self.op {
            Operator::Eq => {
                a == &self.value
                    || a.as_f64()
                        .zip(self.value.as_f64())
                        .is_some_and(|(a, b)| a == b)
            }
            Operator::Ne => {
                !(a == &self.value
                    || a.as_f64()
                        .zip(self.value.as_f64())
                        .is_some_and(|(a, b)| a == b))
            }
            Operator::Gt => a.as_f64() > self.value.as_f64(),
            Operator::Ge => a.as_f64() >= self.value.as_f64(),
            Operator::Lt => a.as_f64() < self.value.as_f64(),
            Operator::Le => a.as_f64() <= self.value.as_f64(),
        }
    }
}
/// Immutable, fully validated graph. Clip assets are resolved by the host.
#[derive(Clone, Debug)]
pub struct Graph {
    pub(super) nodes: Vec<Node>,
    pub(super) order: Vec<usize>,
    pub(super) output: usize,
    pub(super) parameters: BTreeMap<String, Value>,
    clips: BTreeSet<String>,
}
impl Graph {
    pub fn compile(document: &GraphDocument) -> Result<Self, Vec<GraphDiagnostic>> {
        let mut kind = super::kind();
        kind.check = None;
        let errors = engine_graph::validate(document, &kind, Strictness::Complete);
        if !errors.is_empty() {
            return Err(errors);
        }
        compile_semantics(document).map_err(|error| vec![error])
    }
    pub fn requires_skeleton(&self) -> bool {
        self.nodes
            .iter()
            .any(|node| matches!(node, Node::Ik { .. }))
    }
    pub fn clip_ids(&self) -> impl Iterator<Item = &str> {
        self.clips.iter().map(String::as_str)
    }
    pub fn parameter_defaults(&self) -> &BTreeMap<String, Value> {
        &self.parameters
    }
}
fn problem(path: &str, message: impl Into<String>) -> GraphDiagnostic {
    GraphDiagnostic::new("animation.graph.invalid", path, message)
}
fn parsed<T: serde::de::DeserializeOwned>(
    params: &BTreeMap<String, Value>,
    name: &str,
    path: &str,
) -> Result<T, GraphDiagnostic> {
    serde_json::from_value(params.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|e| problem(&format!("{path}/params/{name}"), e.to_string()))
}
pub(super) fn valid_parameter(default: &Value, value: &Value) -> bool {
    let number = |value: &Value| {
        value
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() <= f64::from(f32::MAX))
    };
    if default.is_boolean() {
        value.is_boolean()
    } else if default.is_array() {
        value
            .as_array()
            .is_some_and(|v| v.len() == 3 && v.iter().all(number))
    } else {
        number(value)
    }
}
pub(super) fn compile_semantics(document: &GraphDocument) -> Result<Graph, GraphDiagnostic> {
    let kind = super::kind();
    let mut parameters = BTreeMap::new();
    for variable in &document.variables {
        let default = variable
            .default
            .clone()
            .unwrap_or_else(|| match variable.ty {
                PortType::Bool => Value::Bool(false),
                PortType::Vec3 => serde_json::json!([0., 0., 0.]),
                _ => Value::from(0.),
            });
        if !valid_parameter(&default, &default) {
            return Err(problem(
                &format!("/variables/{}", variable.name),
                "parameter is outside finite f32 range",
            ));
        }
        parameters.insert(variable.name.clone(), default);
    }
    let ids: BTreeMap<_, _> = document
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let outputs: Vec<_> = document
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.node_type == "output")
        .map(|(i, _)| i)
        .collect();
    if outputs.len() != 1 {
        return Err(problem("/nodes", "exactly one output node is required"));
    }
    let mut nodes = Vec::new();
    let mut clips = BTreeSet::new();
    for node in &document.nodes {
        let path = format!("/nodes/{}", node.id);
        let definition = kind
            .registry
            .get(&node.node_type)
            .ok_or_else(|| problem(&path, "unknown node type"))?;
        let p = definition.effective_params(node);
        let incoming = |port: &str| -> Vec<usize> {
            document
                .edges
                .iter()
                .filter(|e| e.to.node == node.id && e.to.port == port)
                .map(|e| ids[e.from.node.as_str()])
                .collect()
        };
        let input = |port: &str| -> Result<usize, GraphDiagnostic> {
            let values = incoming(port);
            if values.len() != 1 {
                Err(problem(&path, format!("exactly one {port} input required")))
            } else {
                Ok(values[0])
            }
        };
        let parameter = |name: &str| -> Result<String, GraphDiagnostic> {
            let name: String = parsed(&p, name, &path)?;
            if !parameters.get(&name).is_some_and(Value::is_number) {
                return Err(problem(&path, "float parameter required"));
            }
            Ok(name)
        };
        nodes.push(match node.node_type.as_str() {
            "output" => Node::Output(input("pose")?),
            "clip" => {
                let id: String = parsed(&p, "clip", &path)?;
                if id.is_empty() || id.chars().any(char::is_control) {
                    return Err(problem(&path, "clip asset id is empty or invalid"));
                }
                clips.insert(id.clone());
                Node::Clip {
                    id,
                    speed: parsed(&p, "speed", &path)?,
                    looping: parsed(&p, "looping", &path)?,
                }
            }
            "blend" => Node::Blend {
                a: input("a")?,
                b: input("b")?,
                parameter: parameter("parameter")?,
            },
            "blend1d" => {
                let positions: Vec<f32> = parsed(&p, "positions", &path)?;
                let inputs = incoming("samples");
                if inputs.len() != positions.len() {
                    return Err(problem(
                        &path,
                        "blend1d positions and ordered sample inputs differ",
                    ));
                }
                Node::One {
                    inputs,
                    parameter: parameter("parameter")?,
                    space: BlendSpace1D::new(positions)
                        .map_err(|e| problem(&path, e.to_string()))?,
                }
            }
            "blend2d" => {
                let positions: Vec<[f32; 2]> = parsed(&p, "positions", &path)?;
                let triangles: Vec<[usize; 3]> = parsed(&p, "triangles", &path)?;
                let inputs = incoming("samples");
                if inputs.len() != positions.len() {
                    return Err(problem(
                        &path,
                        "blend2d positions and ordered sample inputs differ",
                    ));
                }
                Node::Two {
                    inputs,
                    x: parameter("x")?,
                    y: parameter("y")?,
                    space: BlendSpace2D::new(positions, triangles)
                        .map_err(|e| problem(&path, e.to_string()))?,
                }
            }
            "ik_two_bone" => {
                let vector = |name: &str| -> Result<String, GraphDiagnostic> {
                    let variable: String = parsed(&p, name, &path)?;
                    if !parameters.get(&variable).is_some_and(Value::is_array) {
                        return Err(problem(&path, "IK requires a vec3 parameter"));
                    }
                    Ok(variable)
                };
                let chain = [
                    parsed(&p, "root", &path)?,
                    parsed(&p, "mid", &path)?,
                    parsed(&p, "end", &path)?,
                ];
                if chain.iter().collect::<BTreeSet<_>>().len() != 3 {
                    return Err(problem(&path, "IK joint indices must be distinct"));
                }
                let weight = parameter("weight")?;
                if !parameters[&weight]
                    .as_f64()
                    .is_some_and(|v| (0.0..=1.).contains(&v))
                {
                    return Err(problem(&path, "IK weight default must be in 0..1"));
                }
                let limits = crate::ik::BendLimits::new(
                    parsed(&p, "min_bend", &path)?,
                    parsed(&p, "max_bend", &path)?,
                )
                .map_err(|e| problem(&path, e.to_string()))?;
                Node::Ik {
                    input: input("input")?,
                    chain,
                    limits,
                    target: vector("target")?,
                    pole: vector("pole")?,
                    weight,
                }
            }
            "state_machine" => {
                let states: Vec<String> = parsed(&p, "states", &path)?;
                let inputs = incoming("states");
                if states.len() != inputs.len()
                    || states.is_empty()
                    || states.len() > 64
                    || states.iter().any(|s| !engine_graph::valid_name(s))
                    || states.iter().collect::<BTreeSet<_>>().len() != states.len()
                {
                    return Err(problem(
                        &path,
                        "state names must be unique and match 1..64 ordered inputs",
                    ));
                }
                let state_index = |name: &str| {
                    states
                        .iter()
                        .position(|s| s == name)
                        .ok_or_else(|| problem(&path, format!("unknown state {name}")))
                };
                let initial = state_index(&parsed::<String>(&p, "initial", &path)?)?;
                let authored: Vec<TransitionDocument> = parsed(&p, "transitions", &path)?;
                if authored.len() > 128 {
                    return Err(problem(&path, "transition limit exceeded"));
                }
                let transitions = authored
                    .into_iter()
                    .map(|t| {
                        let from = state_index(&t.from)?;
                        let to = state_index(&t.to)?;
                        let default = parameters
                            .get(&t.parameter)
                            .ok_or_else(|| problem(&path, "unknown transition parameter"))?;
                        if from == to
                            || default.is_array()
                            || !valid_parameter(default, &t.value)
                            || (default.is_boolean()
                                && !matches!(t.op, Operator::Eq | Operator::Ne))
                            || !t.duration.is_finite()
                            || !(0.0..=60.).contains(&t.duration)
                        {
                            return Err(problem(
                                &path,
                                "invalid transition condition, state or duration",
                            ));
                        }
                        Ok(Transition {
                            from,
                            to,
                            parameter: t.parameter,
                            op: t.op,
                            value: t.value,
                            duration: t.duration,
                        })
                    })
                    .collect::<Result<_, _>>()?;
                Node::Machine {
                    inputs,
                    initial,
                    transitions,
                }
            }
            _ => return Err(problem(&path, "unknown animation node")),
        });
    }
    fn visit(
        i: usize,
        nodes: &[Node],
        marks: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<(), GraphDiagnostic> {
        if marks[i] == 2 {
            return Ok(());
        }
        if marks[i] == 1 {
            return Err(problem("/edges", "pose dependency cycle"));
        }
        marks[i] = 1;
        for input in nodes[i].inputs() {
            visit(input, nodes, marks, order)?;
        }
        marks[i] = 2;
        order.push(i);
        Ok(())
    }
    let mut marks = vec![0; nodes.len()];
    let mut order = Vec::new();
    visit(outputs[0], &nodes, &mut marks, &mut order)?;
    if order.len() != nodes.len() {
        return Err(problem("/nodes", "every node must contribute to output"));
    }
    Ok(Graph {
        nodes,
        order,
        output: outputs[0],
        parameters,
        clips,
    })
}
