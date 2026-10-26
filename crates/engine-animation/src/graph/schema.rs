use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphKind, GraphLimits, NodeTypeDef, NodeTypeRegistry,
    ParamDef, ParamType, PortDef, PortType,
};
use serde_json::json;
fn pose() -> PortType {
    PortType::Custom("animation.pose".into())
}
fn param(name: &str, ty: ParamType) -> ParamDef {
    ParamDef::new(name, ty).required()
}
fn source(name: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "animation", "Evaluates a local pose")
        .port(PortDef::output("pose", pose()).multiple())
}
/// Shared palette and validation contract for the `animation` graph kind.
pub fn kind() -> GraphKind {
    let registry = NodeTypeRegistry::new()
        .with(
            NodeTypeDef::new("output", "animation", "Final local pose")
                .root()
                .port(PortDef::input("pose", pose()).required()),
        )
        .with(
            source("clip")
                .param(param("clip", ParamType::String).max_len(128))
                .param(ParamDef::new("looping", ParamType::Bool).default_value(json!(true)))
                .param(
                    ParamDef::new("speed", ParamType::Float)
                        .range(0., 100.)
                        .default_value(json!(1.)),
                ),
        )
        .with(
            source("blend")
                .port(PortDef::input("a", pose()).required())
                .port(PortDef::input("b", pose()).required())
                .param(param("parameter", ParamType::Variable).variable_types(&[PortType::Float])),
        )
        .with(
            source("blend1d")
                .port(PortDef::input("samples", pose()).multiple().required())
                .param(param("parameter", ParamType::Variable).variable_types(&[PortType::Float]))
                .param(param("positions", ParamType::Json)),
        )
        .with(
            source("blend2d")
                .port(PortDef::input("samples", pose()).multiple().required())
                .param(param("x", ParamType::Variable).variable_types(&[PortType::Float]))
                .param(param("y", ParamType::Variable).variable_types(&[PortType::Float]))
                .param(param("positions", ParamType::Json))
                .param(param("triangles", ParamType::Json)),
        )
        .with(
            source("ik_two_bone")
                .port(PortDef::input("input", pose()).required())
                .param(param("root", ParamType::Int).range(0., 4095.))
                .param(param("mid", ParamType::Int).range(0., 4095.))
                .param(param("end", ParamType::Int).range(0., 4095.))
                .param(param("target", ParamType::Variable).variable_types(&[PortType::Vec3]))
                .param(param("pole", ParamType::Variable).variable_types(&[PortType::Vec3]))
                .param(param("weight", ParamType::Variable).variable_types(&[PortType::Float]))
                .param(
                    ParamDef::new("min_bend", ParamType::Float)
                        .range(0., std::f64::consts::PI)
                        .default_value(json!(0.)),
                )
                .param(
                    ParamDef::new("max_bend", ParamType::Float)
                        .range(0., std::f64::consts::PI)
                        .default_value(json!(std::f64::consts::PI)),
                ),
        )
        .with(
            source("state_machine")
                .port(PortDef::input("states", pose()).multiple().required())
                .param(param("states", ParamType::Json))
                .param(param("initial", ParamType::String))
                .param(param("transitions", ParamType::Json)),
        );
    GraphKind::new(
        "animation",
        "Validated pose graphs, blend spaces and two-bone IK",
        registry,
    )
    .limits(GraphLimits {
        max_nodes: 256,
        max_edges: 1024,
        max_variables: 64,
    })
    .variable_types(&[PortType::Bool, PortType::Float, PortType::Vec3])
    .check(check)
}
fn check(document: &GraphDocument) -> Vec<GraphDiagnostic> {
    super::compile::compile_semantics(document)
        .err()
        .into_iter()
        .collect()
}
