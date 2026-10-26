use engine_graph::{
    GraphDocument, GraphKind, GraphLimits, NodeTypeDef, NodeTypeRegistry, ParamDef, ParamType,
    PortDef, PortType,
};
fn signal() -> PortType {
    PortType::Custom("audio.signal".into())
}
fn input(name: &str) -> PortDef {
    PortDef::input(name, signal()).required()
}
fn source(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "audio", description)
        .port(PortDef::output("signal", signal()).multiple())
}
fn number(name: &str, default: f64, min: f64, max: f64) -> ParamDef {
    ParamDef::new(name, ParamType::Float)
        .default_value(default.into())
        .range(min, max)
}
/// Shared graph registry/palette. Signal connections carry one mono sample per frame.
pub fn kind() -> GraphKind {
    let registry = NodeTypeRegistry::new()
        .with(
            source("constant", "Constant sample/control signal")
                .param(number("value", 0., -1e6, 1e6)),
        )
        .with(
            source(
                "parameter",
                "Smoothed native-controlled Float graph variable",
            )
            .param(
                ParamDef::new("variable", ParamType::Variable)
                    .required()
                    .variable_types(&[PortType::Float]),
            ),
        )
        .with(
            source(
                "oscillator",
                "Phase-continuous oscillator; discontinuous waveforms alias",
            )
            .port(PortDef::input("frequency", signal()))
            .param(number("frequency", 440., 0., 20_000.))
            .param(
                ParamDef::new("waveform", ParamType::Enum)
                    .choices(&["sine", "triangle", "saw", "square"])
                    .default_value("sine".into()),
            ),
        )
        .with(
            source("noise", "Deterministic xorshift32 white-noise source").param(
                ParamDef::new("seed", ParamType::Int)
                    .default_value(1.into())
                    .range(1., u32::MAX as f64),
            ),
        )
        .with(
            source("mix", "Sum two mono signals")
                .port(input("a"))
                .port(input("b")),
        )
        .with(
            source("gain", "Multiply a signal by a bounded gain")
                .port(input("input"))
                .port(PortDef::input("gain", signal()))
                .param(number("gain", 1., -16., 16.)),
        )
        .with(
            source(
                "adsr",
                "Linear attack/decay/sustain/release envelope; gate is above 0.5",
            )
            .port(input("gate"))
            .param(number("attack", 0.01, 0., 10.))
            .param(number("decay", 0.1, 0., 10.))
            .param(number("sustain", 0.7, 0., 1.))
            .param(number("release", 0.2, 0., 10.)),
        )
        .with(
            source("lowpass", "One-pole low-pass filter")
                .port(input("input"))
                .port(PortDef::input("cutoff", signal()))
                .param(number("cutoff", 1000., 1., 20_000.)),
        )
        .with(
            source(
                "delay",
                "Integer sample delay with internal bounded feedback; no graph cycles",
            )
            .port(input("input"))
            .param(number("seconds", 0.1, 1. / 192_000., 2.))
            .param(number("feedback", 0., -0.99, 0.99)),
        )
        .with(
            NodeTypeDef::new("output", "audio", "Clipped stereo PCM output")
                .root()
                .port(input("left"))
                .port(input("right")),
        );
    GraphKind::new("audio", "Bounded procedural PCM synthesis", registry)
        .limits(GraphLimits {
            max_nodes: super::MAX_NODES,
            max_edges: 1024,
            max_variables: super::MAX_PARAMETERS,
        })
        .variable_types(&[PortType::Float])
        .check(check)
}
fn check(document: &GraphDocument) -> Vec<engine_graph::GraphDiagnostic> {
    super::compile::plan(document).err().into_iter().collect()
}
