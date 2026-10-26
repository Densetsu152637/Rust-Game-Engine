//! The `script_graph` graph kind: node types, ports and parameter schemas.
use super::compile;
use crate::value::JSON_PORT_TYPE;
use engine_graph::{
    GraphKind, GraphLimits, NodeTypeDef, NodeTypeRegistry, ParamDef, ParamType, PortDef, PortType,
};
use serde_json::json;
use std::sync::OnceLock;

/// Name of the graph kind in `GraphDocument::kind`.
pub const KIND: &str = "script_graph";
/// Most nodes in one script graph.
pub const MAX_NODES: usize = 2048;
/// Most edges in one script graph.
pub const MAX_EDGES: usize = 8192;
/// Most variables in one script graph.
pub const MAX_VARIABLES: usize = 128;

/// Node type names.
pub mod node {
    pub const ON_START: &str = "on_start";
    pub const ON_UPDATE: &str = "on_update";
    pub const ON_EVENT: &str = "on_event";
    pub const ON_STOP: &str = "on_stop";
    pub const FUNCTION_ENTRY: &str = "function_entry";
    pub const BRANCH: &str = "branch";
    pub const SEQUENCE: &str = "sequence";
    pub const FOR_RANGE: &str = "for_range";
    pub const WHILE: &str = "while";
    pub const DO_ONCE: &str = "do_once";
    pub const GATE: &str = "gate";
    pub const WAIT: &str = "wait";
    pub const RETURN: &str = "return";
    pub const SET_VAR: &str = "set_var";
    pub const CALL_FUNCTION: &str = "call_function";
    pub const CALL: &str = "call";
    pub const LOG: &str = "log";
    pub const EMIT_EVENT: &str = "emit_event";
    pub const GET_VAR: &str = "get_var";
    pub const SELF: &str = "self";
    pub const CONST: &str = "const";
    pub const ADD: &str = "add";
    pub const SUB: &str = "sub";
    pub const MUL: &str = "mul";
    pub const DIV: &str = "div";
    pub const MIN: &str = "min";
    pub const MAX: &str = "max";
    pub const ABS: &str = "abs";
    pub const NEG: &str = "neg";
    pub const TO_FLOAT: &str = "to_float";
    pub const COMPARE: &str = "compare";
    pub const AND: &str = "and";
    pub const OR: &str = "or";
    pub const NOT: &str = "not";
    pub const VEC3_MAKE: &str = "vec3_make";
    pub const VEC3_BREAK: &str = "vec3_break";
    pub const VEC3_ADD: &str = "vec3_add";
    pub const VEC3_SUB: &str = "vec3_sub";
    pub const VEC3_SCALE: &str = "vec3_scale";
    pub const VEC3_DOT: &str = "vec3_dot";
    pub const VEC3_LENGTH: &str = "vec3_length";
    pub const CONCAT: &str = "concat";
    pub const TO_JSON: &str = "to_json";
    pub const JSON_TO: &str = "json_to";
}

/// Port names used by the node types.
pub mod port {
    /// Flow in of every flow node.
    pub const EXEC: &str = "exec";
    /// Default flow out.
    pub const THEN: &str = "then";
    pub const TRUE: &str = "true";
    pub const FALSE: &str = "false";
    pub const BODY: &str = "body";
    pub const COMPLETED: &str = "completed";
    pub const CLOSED: &str = "closed";
    pub const INDEX: &str = "index";
    pub const RESULT: &str = "result";
    pub const VALUE: &str = "value";
}

/// Variable types a script graph variable may have.
pub fn variable_types() -> [PortType; 6] {
    [
        PortType::Bool,
        PortType::Int,
        PortType::Float,
        PortType::String,
        PortType::Vec3,
        PortType::Entity,
    ]
}

fn json_type() -> PortType {
    PortType::Custom(JSON_PORT_TYPE.to_owned())
}

fn flow_in() -> PortDef {
    PortDef::input(port::EXEC, PortType::Flow)
        .multiple()
        .describe(
            "Execution comes in here. At most one edge may arrive (script.graph.flow_fanout).",
        )
}

fn then_out() -> PortDef {
    flow_out(port::THEN).describe("Continues here when this node is done.")
}

/// A flow out port. All of them are `multiple` at the graph level so that the kind's
/// own `script.graph.flow_fanout` rule (one edge, except `sequence`) reports fan-out.
fn flow_out(name: &str) -> PortDef {
    PortDef::output(name, PortType::Flow).multiple()
}

fn flow_node(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "flow", description)
        .port(flow_in())
        .port(then_out())
}

fn entry(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "entry", description).port(then_out())
}

fn pure(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "value", description)
}

fn data_in(name: &str, ty: PortType) -> PortDef {
    PortDef::input(name, ty)
}
/// Value outputs may feed any number of readers.
fn data_out(name: &str, ty: PortType) -> PortDef {
    PortDef::output(name, ty).multiple()
}

fn type_param(choices: &[&str], default: &str) -> ParamDef {
    ParamDef::new("type", ParamType::Enum)
        .choices(choices)
        .default_value(json!(default))
        .describe("Operand type; the operand ports are checked against it.")
}

fn binary(name: &str, description: &str) -> NodeTypeDef {
    pure(name, description)
        .param(type_param(&["int", "float"], "float"))
        .port(data_in("a", PortType::Any).required())
        .port(data_in("b", PortType::Any).required())
        .port(data_out(port::RESULT, PortType::Any))
}

fn unary(name: &str, description: &str) -> NodeTypeDef {
    pure(name, description)
        .param(type_param(&["int", "float"], "float"))
        .port(data_in("a", PortType::Any).required())
        .port(data_out(port::RESULT, PortType::Any))
}

fn bool2(name: &str, description: &str) -> NodeTypeDef {
    pure(name, description)
        .port(data_in("a", PortType::Bool).required())
        .port(data_in("b", PortType::Bool).required())
        .port(data_out(port::RESULT, PortType::Bool))
}

fn vec2(name: &str, description: &str, out: PortType) -> NodeTypeDef {
    pure(name, description)
        .port(data_in("a", PortType::Vec3).required())
        .port(data_in("b", PortType::Vec3).required())
        .port(data_out(port::RESULT, out))
}

fn registry() -> NodeTypeRegistry {
    use node as n;
    let payload = json_type();
    NodeTypeRegistry::new()
        .with(entry(n::ON_START, "Runs once when the script instance starts."))
        .with(
            entry(n::ON_UPDATE, "Runs every tick with the elapsed time.")
                .port(data_out("dt", PortType::Float).describe("Seconds since the previous tick.")),
        )
        .with(
            entry(n::ON_EVENT, "Runs when the named game event is delivered to the entity.")
                .param(
                    ParamDef::new("event", ParamType::String)
                        .required()
                        .max_len(128)
                        .describe("Event name: lower-case dotted segments such as collision.enter."),
                )
                .port(data_out("payload", payload.clone()).describe("The event payload."))
                .port(data_out("source", PortType::Entity).describe("The emitting entity, or entity 0 when none.")),
        )
        .with(entry(n::ON_STOP, "Runs once when the instance stops (component removed or runtime stop)."))
        .with(
            entry(n::FUNCTION_ENTRY, "Entry of a reusable function. Parameters are extra data out ports (set_ports).")
                .param(
                    ParamDef::new("name", ParamType::String)
                        .required()
                        .max_len(64)
                        .describe("Function name, unique in the document."),
                )
                .extensible(),
        )
        .with(
            flow_node(n::BRANCH, "Continues down true or false by a condition.")
                .port(data_in("condition", PortType::Bool).required())
                .port(flow_out(port::TRUE))
                .port(flow_out(port::FALSE)),
        )
        .with(
            NodeTypeDef::new(n::SEQUENCE, "flow", "Runs each then edge in edge order, one after another.")
                .port(flow_in())
                .port(flow_out(port::THEN).describe("Edge order is run order.")),
        )
        .with(
            NodeTypeDef::new(n::FOR_RANGE, "flow", "Runs body for each index in start..end, then completed.")
                .port(flow_in())
                .port(data_in("start", PortType::Int).describe("Overrides the start param."))
                .port(data_in("end", PortType::Int).describe("Overrides the end param (exclusive)."))
                .port(flow_out(port::BODY))
                .port(flow_out(port::COMPLETED))
                .port(data_out(port::INDEX, PortType::Int).describe("Readable only inside the body."))
                .param(ParamDef::new("start", ParamType::Int).default_value(json!(0)))
                .param(ParamDef::new("end", ParamType::Int).default_value(json!(0))),
        )
        .with(
            NodeTypeDef::new(n::WHILE, "flow", "Repeats body while the condition holds, at most max_iterations times.")
                .port(flow_in())
                .port(data_in("condition", PortType::Bool).required())
                .port(flow_out(port::BODY))
                .port(flow_out(port::COMPLETED))
                .param(
                    ParamDef::new("max_iterations", ParamType::Int)
                        .range(1.0, 10_000.0)
                        .default_value(json!(1000))
                        .describe("Exceeding it traps the instance (script.fuel.exhausted)."),
                ),
        )
        .with(flow_node(n::DO_ONCE, "Lets execution through only the first time this instance reaches it."))
        .with(
            NodeTypeDef::new(n::GATE, "flow", "Lets execution through while open; otherwise continues down closed.")
                .port(flow_in())
                .port(data_in("open", PortType::Bool).describe("Overrides the open param."))
                .port(then_out())
                .port(flow_out(port::CLOSED))
                .param(ParamDef::new("open", ParamType::Bool).default_value(json!(true)))
                .param(
                    ParamDef::new("latch", ParamType::Bool)
                        .default_value(json!(false))
                        .describe("Once open it stays open for the rest of the instance."),
                ),
        )
        .with(
            flow_node(n::WAIT, "Latent: resumes after the given seconds of accumulated tick time. Not allowed in functions.")
                .port(data_in("seconds", PortType::Float).describe("Overrides the seconds param."))
                .param(
                    ParamDef::new("seconds", ParamType::Float)
                        .range(0.0, 3600.0)
                        .default_value(json!(1.0)),
                ),
        )
        .with(
            NodeTypeDef::new(n::RETURN, "flow", "Ends the function, returning its extra data in ports.")
                .port(flow_in())
                .extensible(),
        )
        .with(
            flow_node(n::SET_VAR, "Stores a value in an instance variable.")
                .param(
                    ParamDef::new("variable", ParamType::Variable)
                        .required()
                        .variable_types(&variable_types()),
                )
                .port(data_in(port::VALUE, PortType::Any).required()),
        )
        .with(
            flow_node(n::CALL_FUNCTION, "Calls a function of this document. Extra in ports are its arguments, extra out ports its results.")
                .param(ParamDef::new("function", ParamType::String).required().max_len(64))
                .extensible(),
        )
        .with(
            flow_node(n::CALL, "Calls a host function. Extra in ports are its parameters, the extra out port `result` its return value. A pure function takes no flow edges and is a value node.")
                .param(ParamDef::new("function", ParamType::String).required().max_len(64))
                .extensible(),
        )
        .with(
            flow_node(n::LOG, "Appends a line to the runtime log (never stdout).")
                .port(data_in("message", PortType::String).required()),
        )
        .with(
            flow_node(n::EMIT_EVENT, "Emits a game event, delivered on the next tick.")
                .param(ParamDef::new("event", ParamType::String).required().max_len(128))
                .param(ParamDef::new("payload", ParamType::Json).describe("Optional JSON payload."))
                .port(data_in("target", PortType::Entity).describe("Optional receiver; omitted broadcasts.")),
        )
        .with(
            pure(n::GET_VAR, "Reads an instance variable.")
                .param(
                    ParamDef::new("variable", ParamType::Variable)
                        .required()
                        .variable_types(&variable_types()),
                )
                .port(data_out(port::VALUE, PortType::Any)),
        )
        .with(pure(n::SELF, "The entity this script runs on.").port(data_out("entity", PortType::Entity)))
        .with(
            pure(n::CONST, "A constant value.")
                .param(type_param(&["bool", "int", "float", "string", "vec3", "json"], "float"))
                .param(ParamDef::new("value", ParamType::Json).required())
                .port(data_out(port::VALUE, PortType::Any)),
        )
        .with(binary(n::ADD, "a + b (checked; non-finite or overflow traps)."))
        .with(binary(n::SUB, "a - b."))
        .with(binary(n::MUL, "a * b."))
        .with(binary(n::DIV, "a / b (integer division by zero traps)."))
        .with(binary(n::MIN, "The smaller of a and b."))
        .with(binary(n::MAX, "The larger of a and b."))
        .with(unary(n::ABS, "Absolute value."))
        .with(unary(n::NEG, "Negation."))
        .with(
            pure(n::TO_FLOAT, "Converts an int to a float.")
                .port(data_in("a", PortType::Int).required())
                .port(data_out(port::RESULT, PortType::Float)),
        )
        .with(
            pure(n::COMPARE, "Compares a and b; ordering ops need int, float or string.")
                .param(type_param(&["int", "float", "string", "bool", "entity"], "float"))
                .param(
                    ParamDef::new("op", ParamType::Enum)
                        .choices(&["eq", "ne", "lt", "le", "gt", "ge"])
                        .default_value(json!("eq")),
                )
                .port(data_in("a", PortType::Any).required())
                .port(data_in("b", PortType::Any).required())
                .port(data_out(port::RESULT, PortType::Bool)),
        )
        .with(bool2(n::AND, "Logical and."))
        .with(bool2(n::OR, "Logical or."))
        .with(
            pure(n::NOT, "Logical not.")
                .port(data_in("a", PortType::Bool).required())
                .port(data_out(port::RESULT, PortType::Bool)),
        )
        .with(
            pure(n::VEC3_MAKE, "Builds a vec3 from three floats (rounded to f32).")
                .port(data_in("x", PortType::Float))
                .port(data_in("y", PortType::Float))
                .port(data_in("z", PortType::Float))
                .port(data_out(port::RESULT, PortType::Vec3)),
        )
        .with(
            pure(n::VEC3_BREAK, "Splits a vec3 into three floats.")
                .port(data_in("v", PortType::Vec3).required())
                .port(data_out("x", PortType::Float))
                .port(data_out("y", PortType::Float))
                .port(data_out("z", PortType::Float)),
        )
        .with(vec2(n::VEC3_ADD, "a + b.", PortType::Vec3))
        .with(vec2(n::VEC3_SUB, "a - b.", PortType::Vec3))
        .with(
            pure(n::VEC3_SCALE, "v * s.")
                .port(data_in("v", PortType::Vec3).required())
                .port(data_in("s", PortType::Float).required())
                .port(data_out(port::RESULT, PortType::Vec3)),
        )
        .with(vec2(n::VEC3_DOT, "Dot product.", PortType::Float))
        .with(
            pure(n::VEC3_LENGTH, "Euclidean length.")
                .port(data_in("v", PortType::Vec3).required())
                .port(data_out(port::RESULT, PortType::Float)),
        )
        .with(
            pure(n::CONCAT, "Joins two strings; a result over 4096 bytes traps.")
                .port(data_in("a", PortType::String).required())
                .port(data_in("b", PortType::String).required())
                .port(data_out(port::RESULT, PortType::String)),
        )
        .with(
            pure(n::TO_JSON, "Wraps a value as JSON (an entity becomes its numeric handle).")
                .param(type_param(&["bool", "int", "float", "string", "vec3", "entity"], "float"))
                .port(data_in("value", PortType::Any).required())
                .port(data_out(port::RESULT, json_type())),
        )
        .with(
            pure(n::JSON_TO, "Extracts a typed value from JSON; a mismatch traps (script.host.argument).")
                .param(type_param(&["bool", "int", "float", "string", "vec3"], "float"))
                .port(data_in("value", json_type()).required())
                .port(data_out(port::RESULT, PortType::Any)),
        )
}

/// The `script_graph` graph kind.
pub fn kind() -> GraphKind {
    shared_kind().clone()
}

/// Process-wide instance of the kind, built once.
pub fn shared_kind() -> &'static GraphKind {
    static KIND_CELL: OnceLock<GraphKind> = OnceLock::new();
    KIND_CELL.get_or_init(|| {
        GraphKind::new(
            KIND,
            "Visual scripting: entry nodes (on_start, on_update, on_event, on_stop), structured flow \
             (branch, sequence, loops, wait), typed values and calls to declared host functions. \
             Compiled to a bounded, deterministic VM. Flow edges form a tree (no merges); loops are \
             structural nodes.",
            registry(),
        )
        .limits(GraphLimits {
            max_nodes: MAX_NODES,
            max_edges: MAX_EDGES,
            max_variables: MAX_VARIABLES,
        })
        .variable_types(&variable_types())
        .check(compile::check_document)
    })
}

/// Whether `name` is a valid event name: 1..=128 bytes of lower-case dotted
/// segments of `[a-z0-9_]`.
pub fn valid_event_name(name: &str) -> bool {
    name.len() <= 128
        && !name.is_empty()
        && name.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}
