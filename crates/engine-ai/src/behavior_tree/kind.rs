//! The `behavior_tree` graph kind: node types, ports and parameter schemas.
use engine_graph::{
    GraphKind, NodeTypeDef, NodeTypeRegistry, ParamDef, ParamType, PortDef, PortType,
};
use serde_json::json;
use std::sync::OnceLock;

/// Name of the graph kind in `GraphDocument::kind`.
pub const KIND: &str = "behavior_tree";
/// Most nodes (including services) in one tree; trees are small by design.
pub const MAX_TREE_NODES: usize = 1024;
/// Deepest nesting the executor recurses into.
pub const MAX_DEPTH: usize = 64;
/// Most children one composite may have.
pub const MAX_CHILDREN: usize = 64;

/// Node type names.
pub mod node {
    pub const ROOT: &str = "root";
    pub const SEQUENCE: &str = "sequence";
    pub const SELECTOR: &str = "selector";
    pub const PARALLEL: &str = "parallel";
    pub const RANDOM_SELECTOR: &str = "random_selector";
    pub const INVERTER: &str = "inverter";
    pub const REPEAT: &str = "repeat";
    pub const RETRY: &str = "retry";
    pub const COOLDOWN: &str = "cooldown";
    pub const TIME_LIMIT: &str = "time_limit";
    pub const FORCE_SUCCESS: &str = "force_success";
    pub const FORCE_FAILURE: &str = "force_failure";
    pub const BLACKBOARD_CONDITION: &str = "blackboard_condition";
    pub const WAIT: &str = "wait";
    pub const SET_BLACKBOARD: &str = "set_blackboard";
    pub const CLEAR_BLACKBOARD: &str = "clear_blackboard";
    pub const EMIT_EVENT: &str = "emit_event";
    pub const MOVE_TO: &str = "move_to";
    pub const NATIVE_TASK: &str = "native_task";
    pub const NATIVE_SERVICE: &str = "native_service";
    pub const SET_BLACKBOARD_SERVICE: &str = "set_blackboard_service";
    pub const EMIT_EVENT_SERVICE: &str = "emit_event_service";
}

/// Port names used by the node types.
pub mod port {
    /// Flow in of every non-root node.
    pub const PARENT: &str = "parent";
    /// Ordered flow out of composites.
    pub const CHILDREN: &str = "children";
    /// Single flow out of the root and of decorators.
    pub const CHILD: &str = "child";
    /// Services attached to a composite.
    pub const SERVICES: &str = "services";
    /// Service in.
    pub const OWNER: &str = "owner";
}

/// Custom port type that links services to their composite.
pub fn service_type() -> PortType {
    PortType::Custom("bt.service".to_owned())
}

/// Variable types a blackboard key may have.
pub fn blackboard_types() -> [PortType; 6] {
    [
        PortType::Bool,
        PortType::Int,
        PortType::Float,
        PortType::String,
        PortType::Vec3,
        PortType::Entity,
    ]
}

fn parent_in() -> PortDef {
    PortDef::input(port::PARENT, PortType::Flow).required()
}

fn composite(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "composite", description)
        .port(parent_in())
        .port(
            PortDef::output(port::CHILDREN, PortType::Flow)
                .multiple()
                .describe("Ordered children; edge order is priority order."),
        )
        .port(
            PortDef::output(port::SERVICES, service_type())
                .multiple()
                .describe("Services that tick while this branch is active."),
        )
}

fn decorator(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "decorator", description)
        .port(parent_in())
        .port(PortDef::output(port::CHILD, PortType::Flow).required())
}

fn task(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "task", description).port(parent_in())
}

fn service(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "service", description)
        .port(PortDef::input(port::OWNER, service_type()).required())
        .param(
            ParamDef::new("interval", ParamType::Float)
                .range(0.0, 3600.0)
                .default_value(json!(0.5))
                .describe("Seconds between runs; the service also runs once when its branch activates. 0 runs every tick."),
        )
}

fn key_param(description: &str) -> ParamDef {
    ParamDef::new("key", ParamType::Variable)
        .required()
        .describe(description)
}

fn event_params(def: NodeTypeDef) -> NodeTypeDef {
    def.param(
        ParamDef::new("event", ParamType::String)
            .required()
            .max_len(128)
            .describe("Event name, lower-case dotted, delivered to the agent entity on the next runtime tick."),
    )
    .param(ParamDef::new("payload", ParamType::Json).describe("Optional JSON object payload."))
}

fn registry() -> NodeTypeRegistry {
    NodeTypeRegistry::new()
        .with(
            NodeTypeDef::new(node::ROOT, "root", "Entry of the tree; runs its single child every tick.")
                .root()
                .port(PortDef::output(port::CHILD, PortType::Flow).required()),
        )
        .with(composite(node::SEQUENCE, "Runs children in order; fails at the first failure, succeeds when all succeed."))
        .with(composite(node::SELECTOR, "Runs children in priority order; succeeds at the first success, fails when all fail."))
        .with(
            composite(node::PARALLEL, "Runs all children concurrently and decides by policy.")
                .param(
                    ParamDef::new("success_policy", ParamType::Enum)
                        .choices(&["require_all", "require_one"])
                        .default_value(json!("require_all"))
                        .describe("Succeed when every child succeeds, or when one does."),
                )
                .param(
                    ParamDef::new("failure_policy", ParamType::Enum)
                        .choices(&["require_one", "require_all"])
                        .default_value(json!("require_one"))
                        .describe("Fail when one child fails (checked first), or only when all do."),
                ),
        )
        .with(
            composite(node::RANDOM_SELECTOR, "Selector over a seeded random order drawn at every activation.")
                .param(
                    ParamDef::new("seed", ParamType::Int)
                        .range(0.0, 9.0e18)
                        .default_value(json!(0))
                        .describe("Authored seed; combined with the agent seed and the node."),
                ),
        )
        .with(decorator(node::INVERTER, "Swaps success and failure of its child."))
        .with(
            decorator(node::REPEAT, "Runs its child count times, restarting on the next tick after each completion.")
                .param(ParamDef::new("count", ParamType::Int).range(1.0, 1000.0).default_value(json!(1)))
                .param(
                    ParamDef::new("stop_on_failure", ParamType::Bool)
                        .default_value(json!(true))
                        .describe("Fail at the first child failure instead of counting it."),
                ),
        )
        .with(
            decorator(node::RETRY, "Re-runs its child until it succeeds, up to attempts runs.")
                .param(ParamDef::new("attempts", ParamType::Int).range(1.0, 1000.0).default_value(json!(3))),
        )
        .with(
            decorator(node::COOLDOWN, "After its child completes, fails immediately for duration seconds.")
                .param(ParamDef::new("duration", ParamType::Float).range(0.0, 3600.0).default_value(json!(1.0)))
                .param(
                    ParamDef::new("lock_on", ParamType::Enum)
                        .choices(&["any", "success", "failure"])
                        .default_value(json!("any"))
                        .describe("Which child outcome starts the cooldown."),
                ),
        )
        .with(
            decorator(node::TIME_LIMIT, "Aborts its child and fails when it runs longer than seconds.")
                .param(ParamDef::new("seconds", ParamType::Float).range(0.0, 3600.0).default_value(json!(5.0))),
        )
        .with(decorator(node::FORCE_SUCCESS, "Reports success whenever its child completes."))
        .with(decorator(node::FORCE_FAILURE, "Reports failure whenever its child completes."))
        .with(
            decorator(node::BLACKBOARD_CONDITION, "Runs its child only while a blackboard comparison holds; observer aborts re-check it when the key changes.")
                .param(key_param("Blackboard key to compare."))
                .param(
                    ParamDef::new("operator", ParamType::Enum)
                        .choices(&["is_set", "is_not_set", "eq", "ne", "lt", "le", "gt", "ge"])
                        .default_value(json!("is_set")),
                )
                .param(ParamDef::new("value", ParamType::Json).describe("Comparison operand of the key's type; unused by is_set and is_not_set."))
                .param(
                    ParamDef::new("abort", ParamType::Enum)
                        .choices(&["none", "self", "lower_priority", "both"])
                        .default_value(json!("none"))
                        .describe("self aborts the child when the condition turns false; lower_priority preempts a lower-priority running sibling when it turns true."),
                ),
        )
        .with(
            task(node::WAIT, "Running for seconds, then success.")
                .param(ParamDef::new("seconds", ParamType::Float).range(0.0, 3600.0).default_value(json!(1.0))),
        )
        .with(
            task(node::SET_BLACKBOARD, "Writes a constant to a blackboard key; always succeeds.")
                .param(key_param("Blackboard key to write."))
                .param(ParamDef::new("value", ParamType::Json).required().describe("Value of the key's type.")),
        )
        .with(task(node::CLEAR_BLACKBOARD, "Removes the value of a blackboard key; always succeeds.").param(key_param("Blackboard key to clear.")))
        .with(event_params(task(node::EMIT_EVENT, "Emits an event to the agent entity; always succeeds.")))
        .with(
            task(node::MOVE_TO, "Requests navigation to a point and runs until arrival (success), failure or timeout.")
                .param(ParamDef::new("target", ParamType::Vec3).describe("Literal world-space goal; exactly one of target and key."))
                .param(
                    ParamDef::new("key", ParamType::Variable)
                        .variable_types(&[PortType::Vec3])
                        .describe("Vec3 blackboard key holding the goal; exactly one of target and key."),
                )
                .param(
                    ParamDef::new("timeout", ParamType::Float)
                        .range(0.0, 3600.0)
                        .default_value(json!(0.0))
                        .describe("Seconds before the move is cancelled and the task fails; 0 waits forever."),
                ),
        )
        .with(
            task(node::NATIVE_TASK, "Runs a game-registered native task (start, tick, abort).")
                .param(ParamDef::new("name", ParamType::String).required().max_len(128).describe("Registered native task name."))
                .param(ParamDef::new("params", ParamType::Json).describe("Opaque JSON handed to the task.")),
        )
        .with(
            service(node::NATIVE_SERVICE, "Runs a native task periodically while the composite is active; its status is ignored.")
                .param(ParamDef::new("name", ParamType::String).required().max_len(128))
                .param(ParamDef::new("params", ParamType::Json)),
        )
        .with(
            service(node::SET_BLACKBOARD_SERVICE, "Writes a constant to a blackboard key periodically.")
                .param(key_param("Blackboard key to write."))
                .param(ParamDef::new("value", ParamType::Json).required()),
        )
        .with(event_params(service(node::EMIT_EVENT_SERVICE, "Emits an event periodically.")))
}

fn build_kind() -> GraphKind {
    GraphKind::new(
        KIND,
        "Behavior tree: composites, decorators with observer aborts, services and tasks over a typed blackboard.",
        registry(),
    )
    .single_root()
    .variable_types(&blackboard_types())
    .limits(engine_graph::GraphLimits {
        max_nodes: MAX_TREE_NODES,
        max_edges: 4 * MAX_TREE_NODES,
        max_variables: 128,
    })
    .check(super::compile::check_document)
}

/// The behavior tree graph kind (cheap to clone; registries of other crates take
/// a value).
pub fn kind() -> GraphKind {
    shared_kind().clone()
}

/// Process-wide instance of the kind, built once.
pub fn shared_kind() -> &'static GraphKind {
    static KIND_CELL: OnceLock<GraphKind> = OnceLock::new();
    KIND_CELL.get_or_init(build_kind)
}
