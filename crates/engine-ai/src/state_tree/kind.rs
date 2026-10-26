//! The `state_tree` graph kind: node types, ports and parameter schemas.
use crate::behavior_tree::kind::blackboard_types;
use engine_graph::{
    GraphKind, NodeTypeDef, NodeTypeRegistry, ParamDef, ParamType, PortDef, PortType,
};
use serde_json::json;
use std::sync::OnceLock;

/// Name of the graph kind in `GraphDocument::kind`.
pub const KIND: &str = "state_tree";
/// Most nodes (states, tasks, transitions, conditions, evaluators) in one tree.
pub const MAX_TREE_NODES: usize = 1024;
/// Deepest state nesting; top-level states have depth 0, so at most this many levels.
pub const MAX_DEPTH: usize = 32;
/// Most child states one state (or the root) may have.
pub const MAX_CHILDREN: usize = 64;
/// Most tasks one state may have.
pub const MAX_TASKS: usize = 16;
/// Most transitions one state may own.
pub const MAX_TRANSITIONS: usize = 32;
/// Most conditions one state or transition may carry.
pub const MAX_CONDITIONS: usize = 16;
/// Most evaluators one tree may have.
pub const MAX_EVALUATORS: usize = 32;

/// Node type names.
pub mod node {
    pub const ROOT: &str = "root";
    pub const STATE: &str = "state";
    pub const TRANSITION: &str = "transition";
    pub const BLACKBOARD_CONDITION: &str = "blackboard_condition";
    pub const WAIT: &str = "wait";
    pub const SET_BLACKBOARD: &str = "set_blackboard";
    pub const CLEAR_BLACKBOARD: &str = "clear_blackboard";
    pub const EMIT_EVENT: &str = "emit_event";
    pub const MOVE_TO: &str = "move_to";
    pub const NATIVE_TASK: &str = "native_task";
    pub const COPY_EVALUATOR: &str = "copy_evaluator";
    pub const COMPUTE_EVALUATOR: &str = "compute_evaluator";
    pub const NATIVE_EVALUATOR: &str = "native_evaluator";
}

/// Port names used by the node types.
pub mod port {
    /// Flow in of every state.
    pub const PARENT: &str = "parent";
    /// Ordered flow out of the root and of states: the child states.
    pub const CHILDREN: &str = "children";
    /// Tasks attached to a state.
    pub const TASKS: &str = "tasks";
    /// Conditions attached to a state (enter conditions) or a transition.
    pub const CONDITIONS: &str = "conditions";
    /// Transitions attached to a state.
    pub const TRANSITIONS: &str = "transitions";
    /// Evaluators attached to the root.
    pub const EVALUATORS: &str = "evaluators";
    /// In port of tasks, conditions, transitions and evaluators.
    pub const OWNER: &str = "owner";
}

/// Custom port type linking tasks to their state.
pub fn task_type() -> PortType {
    PortType::Custom("st.task".to_owned())
}
/// Custom port type linking conditions to their state or transition.
pub fn condition_type() -> PortType {
    PortType::Custom("st.condition".to_owned())
}
/// Custom port type linking transitions to their state.
pub fn transition_type() -> PortType {
    PortType::Custom("st.transition".to_owned())
}
/// Custom port type linking evaluators to the root.
pub fn evaluator_type() -> PortType {
    PortType::Custom("st.evaluator".to_owned())
}

fn key_param(description: &str) -> ParamDef {
    ParamDef::new("key", ParamType::Variable)
        .required()
        .describe(description)
}

fn task(name: &str, description: &str, considered: bool) -> NodeTypeDef {
    NodeTypeDef::new(name, "task", description)
        .port(PortDef::input(port::OWNER, task_type()).required())
        .param(
            ParamDef::new("considered", ParamType::Bool)
                .default_value(json!(considered))
                .describe("Whether the task's completion counts toward completing its state. Instant tasks default to false so they do not complete the state on entry."),
        )
}

fn evaluator(name: &str, description: &str) -> NodeTypeDef {
    NodeTypeDef::new(name, "evaluator", description)
        .port(PortDef::input(port::OWNER, evaluator_type()).required())
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
            NodeTypeDef::new(node::ROOT, "root", "Entry of the tree; owns the top-level states (tried in order at start) and the evaluators.")
                .root()
                .port(
                    PortDef::output(port::CHILDREN, PortType::Flow)
                        .multiple()
                        .describe("Top-level states; edge order is selection order."),
                )
                .port(
                    PortDef::output(port::EVALUATORS, evaluator_type())
                        .multiple()
                        .describe("Evaluators; they run every tick before transitions, in edge order."),
                ),
        )
        .with(
            NodeTypeDef::new(node::STATE, "state", "A state: enter conditions, ordered tasks, transitions and child states.")
                .port(PortDef::input(port::PARENT, PortType::Flow).required())
                .port(
                    PortDef::output(port::CHILDREN, PortType::Flow)
                        .multiple()
                        .describe("Child states; edge order is selection order."),
                )
                .port(
                    PortDef::output(port::TASKS, task_type())
                        .multiple()
                        .describe("Tasks started in edge order when the state is entered."),
                )
                .port(
                    PortDef::output(port::CONDITIONS, condition_type())
                        .multiple()
                        .describe("Enter conditions; all must hold for the state to be selected."),
                )
                .port(
                    PortDef::output(port::TRANSITIONS, transition_type())
                        .multiple()
                        .describe("Transitions owned by the state."),
                )
                .param(
                    ParamDef::new("selection", ParamType::Enum)
                        .choices(&["children_in_order", "children_or_self", "self"])
                        .default_value(json!("children_in_order"))
                        .describe("children_in_order selects the first selectable child (a state with children is selected only through one of them); children_or_self falls back to the state itself as the leaf; self ignores the children."),
                )
                .param(
                    ParamDef::new("completion", ParamType::Enum)
                        .choices(&["any", "all"])
                        .default_value(json!("any"))
                        .describe("any completes the state when the first considered task completes; all when every considered task succeeded, or at the first failure."),
                ),
        )
        .with(
            NodeTypeDef::new(node::TRANSITION, "transition", "Moves the tree to another state or completes it when its trigger fires and its conditions hold.")
                .port(PortDef::input(port::OWNER, transition_type()).required())
                .port(
                    PortDef::output(port::CONDITIONS, condition_type())
                        .multiple()
                        .describe("All must hold for the transition to fire."),
                )
                .param(
                    ParamDef::new("trigger", ParamType::Enum)
                        .choices(&["on_completed", "on_succeeded", "on_failed", "on_tick", "on_event"])
                        .default_value(json!("on_completed")),
                )
                .param(
                    ParamDef::new("event", ParamType::String)
                        .max_len(128)
                        .describe("Event name for on_event; not allowed with other triggers."),
                )
                .param(
                    ParamDef::new("target", ParamType::Enum)
                        .required()
                        .choices(&["state", "parent", "next_sibling", "succeeded", "failed"])
                        .describe("state goes to the state named by state; parent re-enters the owner's parent; next_sibling enters the first selectable later sibling; succeeded and failed complete the tree."),
                )
                .param(
                    ParamDef::new("state", ParamType::String)
                        .max_len(64)
                        .describe("Node id of the target state when target is state."),
                )
                .param(
                    ParamDef::new("priority", ParamType::Int)
                        .range(0.0, 1000.0)
                        .default_value(json!(0))
                        .describe("Higher priorities are evaluated first; ties go to the deeper state, then to authoring order."),
                )
                .param(
                    ParamDef::new("delay", ParamType::Float)
                        .range(0.0, 3600.0)
                        .default_value(json!(0.0))
                        .describe("Seconds the trigger and conditions must keep holding before the transition fires."),
                )
                .param(
                    ParamDef::new("delay_variance", ParamType::Float)
                        .range(0.0, 3600.0)
                        .default_value(json!(0.0))
                        .describe("Each arming draws the delay uniformly from delay +/- variance (seeded, floored at 0)."),
                ),
        )
        .with(
            NodeTypeDef::new(node::BLACKBOARD_CONDITION, "condition", "A blackboard comparison; usable as an enter condition or a transition condition.")
                .port(PortDef::input(port::OWNER, condition_type()).required())
                .param(key_param("Blackboard key to compare."))
                .param(
                    ParamDef::new("operator", ParamType::Enum)
                        .choices(&["is_set", "is_not_set", "eq", "ne", "lt", "le", "gt", "ge"])
                        .default_value(json!("is_set")),
                )
                .param(ParamDef::new("value", ParamType::Json).describe("Comparison operand of the key's type; unused by is_set and is_not_set.")),
        )
        .with(
            task(node::WAIT, "Running for seconds, then success.", true)
                .param(ParamDef::new("seconds", ParamType::Float).range(0.0, 3600.0).default_value(json!(1.0))),
        )
        .with(
            task(node::SET_BLACKBOARD, "Writes a constant to a blackboard key on entry; succeeds immediately.", false)
                .param(key_param("Blackboard key to write."))
                .param(ParamDef::new("value", ParamType::Json).required().describe("Value of the key's type.")),
        )
        .with(
            task(node::CLEAR_BLACKBOARD, "Removes the value of a blackboard key on entry; succeeds immediately.", false)
                .param(key_param("Blackboard key to clear.")),
        )
        .with(event_params(task(node::EMIT_EVENT, "Emits an event to the agent entity on entry; succeeds immediately.", false)))
        .with(
            task(node::MOVE_TO, "Requests navigation to a point and runs until arrival (success), failure or timeout; leaving the state cancels it.", true)
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
            task(node::NATIVE_TASK, "Runs a game-registered native task (start, tick, abort on state exit).", true)
                .param(ParamDef::new("name", ParamType::String).required().max_len(128).describe("Registered native task name."))
                .param(ParamDef::new("params", ParamType::Json).describe("Opaque JSON handed to the task.")),
        )
        .with(
            evaluator(node::COPY_EVALUATOR, "Copies a host input or another key into a key every tick; an absent source clears the key.")
                .param(ParamDef::new("input", ParamType::String).max_len(128).describe("Host input name; exactly one of input and from."))
                .param(ParamDef::new("from", ParamType::Variable).describe("Source key; exactly one of input and from."))
                .param(key_param("Destination key.")),
        )
        .with(
            evaluator(node::COMPUTE_EVALUATOR, "Computes a key from one or two keys every tick; an unset operand clears the key.")
                .param(
                    ParamDef::new("op", ParamType::Enum)
                        .required()
                        .choices(&["add", "sub", "mul", "min", "max", "distance", "not", "and", "or"])
                        .describe("Numeric ops take int or float keys; distance takes two vec3 keys and yields a float; not, and, or take bools."),
                )
                .param(ParamDef::new("a", ParamType::Variable).required().describe("First operand key."))
                .param(ParamDef::new("b", ParamType::Variable).describe("Second operand key; unused by not."))
                .param(key_param("Destination key.")),
        )
        .with(
            evaluator(node::NATIVE_EVALUATOR, "Runs a game-registered native evaluator every tick (start on the first tick, stop when the tree ends).")
                .param(ParamDef::new("name", ParamType::String).required().max_len(128).describe("Registered native evaluator name."))
                .param(ParamDef::new("params", ParamType::Json).describe("Opaque JSON handed to the evaluator.")),
        )
}

fn build_kind() -> GraphKind {
    GraphKind::new(
        KIND,
        "State tree: hierarchical states with enter conditions, ordered tasks, prioritised transitions and tree-level evaluators over a typed blackboard.",
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

/// The state tree graph kind (cheap to clone).
pub fn kind() -> GraphKind {
    shared_kind().clone()
}

/// Process-wide instance of the kind, built once.
pub fn shared_kind() -> &'static GraphKind {
    static KIND_CELL: OnceLock<GraphKind> = OnceLock::new();
    KIND_CELL.get_or_init(build_kind)
}
