use engine_graph::{GraphDiagnostic, GraphDocument, GraphIndex, GraphNode, Strictness};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Copy, Debug)]
pub(super) enum Wave {
    Sine,
    Triangle,
    Saw,
    Square,
}
#[derive(Clone, Debug)]
pub(super) enum Op {
    Constant(f32),
    Parameter(usize),
    Oscillator {
        frequency: Option<usize>,
        hz: f32,
        wave: Wave,
    },
    Noise(u32),
    Mix(usize, usize),
    Gain {
        input: usize,
        gain: Option<usize>,
        value: f32,
    },
    Adsr {
        gate: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    Lowpass {
        input: usize,
        cutoff: Option<usize>,
        hz: f32,
    },
    Delay {
        input: usize,
        seconds: f32,
        feedback: f32,
    },
    Output(usize, usize),
}
impl Op {
    fn inputs(&self) -> Vec<usize> {
        match self {
            Self::Constant(_) | Self::Parameter(_) | Self::Noise(_) => vec![],
            Self::Oscillator { frequency, .. } => frequency.iter().copied().collect(),
            Self::Mix(a, b) | Self::Output(a, b) => vec![*a, *b],
            Self::Gain { input, gain, .. } => std::iter::once(*input)
                .chain(gain.iter().copied())
                .collect(),
            Self::Adsr { gate, .. } => vec![*gate],
            Self::Lowpass { input, cutoff, .. } => std::iter::once(*input)
                .chain(cutoff.iter().copied())
                .collect(),
            Self::Delay { input, .. } => vec![*input],
        }
    }
}
/// Immutable execution plan. Per-voice DSP state belongs to `GraphInstance`.
#[derive(Clone, Debug)]
pub struct CompiledGraph {
    pub(super) ops: Vec<Op>,
    pub(super) order: Vec<usize>,
    pub(super) output: usize,
    pub(super) parameters: Vec<(String, f32)>,
}
impl CompiledGraph {
    pub fn validate_parameters(&self, values: &[super::ParameterUpdate<'_>]) -> crate::Result<()> {
        super::runtime::updates(self, values).map(|_| ())
    }
    pub fn parameter_names(&self) -> impl Iterator<Item = &str> {
        self.parameters.iter().map(|(n, _)| n.as_str())
    }
    pub fn node_count(&self) -> usize {
        self.ops.len()
    }
}
/// Full shared validation and domain compilation; all allocation occurs outside playback callbacks.
pub fn compile(document: &GraphDocument) -> Result<Arc<CompiledGraph>, Vec<GraphDiagnostic>> {
    let diagnostics = engine_graph::validate(document, &super::kind(), Strictness::Complete);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    plan(document).map(Arc::new).map_err(|e| vec![e])
}
fn diagnostic(code: &str, path: impl Into<String>, message: &str) -> GraphDiagnostic {
    GraphDiagnostic::new(code, path, message)
}
fn number(node: &GraphNode, name: &str, default: f32) -> f32 {
    node.params
        .get(name)
        .and_then(|v| v.as_f64())
        .map_or(default, |v| v as f32)
}
pub(super) fn plan(document: &GraphDocument) -> Result<CompiledGraph, GraphDiagnostic> {
    let index = GraphIndex::new(document);
    let mut parameters = vec![];
    for variable in &document.variables {
        let value = variable
            .default
            .as_ref()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| {
                diagnostic(
                    "audio.graph.parameter",
                    format!("/variables/{}", variable.name),
                    "Float variables require finite numeric defaults",
                )
            })?;
        if !value.is_finite() || value.abs() > f64::from(super::MAX_SIGNAL) {
            return Err(diagnostic(
                "audio.graph.parameter",
                format!("/variables/{}", variable.name),
                "Variable default exceeds signal bounds",
            ));
        }
        parameters.push((variable.name.clone(), value as f32));
    }
    parameters.sort_by(|a, b| a.0.cmp(&b.0));
    let lookup: BTreeMap<_, _> = parameters
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.as_str(), i))
        .collect();
    let mut ops = vec![];
    let mut output = None;
    for (at, node) in document.nodes.iter().enumerate() {
        let connection = |port: &str| {
            index
                .incoming(node.id.as_str(), port)
                .first()
                .and_then(|e| index.position(e.from.node.as_str()))
        };
        let required = |port: &str| {
            connection(port).ok_or_else(|| {
                diagnostic(
                    "audio.graph.input",
                    format!("/nodes/{}/ports/{port}", node.id),
                    "Required signal is not connected",
                )
            })
        };
        let op = match node.node_type.as_str() {
            "constant" => Op::Constant(number(node, "value", 0.)),
            "parameter" => {
                let name = node
                    .params
                    .get("variable")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Op::Parameter(*lookup.get(name).ok_or_else(|| {
                    diagnostic(
                        "audio.graph.parameter",
                        format!("/nodes/{}", node.id),
                        "Unknown Float variable",
                    )
                })?)
            }
            "oscillator" => Op::Oscillator {
                frequency: connection("frequency"),
                hz: number(node, "frequency", 440.),
                wave: match node
                    .params
                    .get("waveform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sine")
                {
                    "triangle" => Wave::Triangle,
                    "saw" => Wave::Saw,
                    "square" => Wave::Square,
                    _ => Wave::Sine,
                },
            },
            "noise" => Op::Noise(
                node.params
                    .get("seed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as u32,
            ),
            "mix" => Op::Mix(required("a")?, required("b")?),
            "gain" => Op::Gain {
                input: required("input")?,
                gain: connection("gain"),
                value: number(node, "gain", 1.),
            },
            "adsr" => Op::Adsr {
                gate: required("gate")?,
                attack: number(node, "attack", 0.01),
                decay: number(node, "decay", 0.1),
                sustain: number(node, "sustain", 0.7),
                release: number(node, "release", 0.2),
            },
            "lowpass" => Op::Lowpass {
                input: required("input")?,
                cutoff: connection("cutoff"),
                hz: number(node, "cutoff", 1000.),
            },
            "delay" => Op::Delay {
                input: required("input")?,
                seconds: number(node, "seconds", 0.1),
                feedback: number(node, "feedback", 0.),
            },
            "output" => {
                if output.replace(at).is_some() {
                    return Err(diagnostic(
                        "audio.graph.output",
                        "/nodes",
                        "Exactly one stereo output is required",
                    ));
                }
                Op::Output(required("left")?, required("right")?)
            }
            _ => {
                return Err(diagnostic(
                    "audio.graph.node",
                    format!("/nodes/{}", node.id),
                    "Unknown audio node",
                ));
            }
        };
        ops.push(op);
    }
    let output = output.ok_or_else(|| {
        diagnostic(
            "audio.graph.output",
            "/nodes",
            "Exactly one stereo output is required",
        )
    })?;
    let inputs: Vec<_> = ops.iter().map(Op::inputs).collect();
    let mut done = vec![false; ops.len()];
    let mut order = vec![];
    while order.len() < ops.len() {
        let Some(next) = (0..ops.len()).find(|&i| !done[i] && inputs[i].iter().all(|&p| done[p]))
        else {
            return Err(diagnostic(
                "audio.graph.cycle",
                "/edges",
                "Directed signal cycles are forbidden, including through delay nodes",
            ));
        };
        done[next] = true;
        order.push(next);
    }
    let mut needed = vec![false; ops.len()];
    let mut pending = vec![output];
    while let Some(node) = pending.pop() {
        if !needed[node] {
            needed[node] = true;
            pending.extend(&inputs[node]);
        }
    }
    if let Some(at) = needed.iter().position(|n| !*n) {
        return Err(diagnostic(
            "audio.graph.unused",
            format!("/nodes/{}", document.nodes[at].id),
            "Every node must contribute to the stereo output",
        ));
    }
    Ok(CompiledGraph {
        ops,
        order,
        output,
        parameters,
    })
}
