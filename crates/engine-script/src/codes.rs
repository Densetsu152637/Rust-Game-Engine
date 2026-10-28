//! Stable diagnostic and trap codes of the script core.
//!
//! Codes are API: tools, tests and documentation match on them, never on messages.

/// Trap: the per-tick fuel budget ran out (or a `while` hit its iteration bound).
pub const FUEL_EXHAUSTED: &str = "script.fuel.exhausted";
/// Trap: instance state, a string or the pending-wait list outgrew its limit.
pub const MEMORY_LIMIT: &str = "script.memory.limit";
/// Trap: guest call depth exceeded the budget.
pub const CALL_DEPTH: &str = "script.call.depth";
/// Trap: a host function argument failed type, finiteness or size validation.
pub const HOST_ARGUMENT: &str = "script.host.argument";
/// Trap: a host function reported failure.
pub const HOST_FAILED: &str = "script.host.failed";
/// Trap: the script used a capability its component did not grant.
pub const CAPABILITY_DENIED: &str = "script.capability.denied";
/// Trap: a non-finite number reached a boundary.
pub const VALUE_NONFINITE: &str = "script.value.nonfinite";
/// Trap: the per-tick emit budget was exceeded.
pub const EVENT_LIMIT: &str = "script.event.limit";
/// Trap: the spawn budget was exceeded (reserved; spawn is not shipped).
pub const SPAWN_LIMIT: &str = "script.spawn.limit";

/// Start-time: a script function signature has no registered implementation.
pub const FUNCTION_UNREGISTERED: &str = "script.function.unregistered";
/// Start-time: a registered implementation disagrees with the declared signature.
pub const FUNCTION_SIGNATURE: &str = "script.function.signature";
/// A function name was registered twice.
pub const FUNCTION_DUPLICATE: &str = "script.function.duplicate";
/// A function declaration or name is malformed (including reserved `engine.*`).
pub const FUNCTION_INVALID: &str = "script.function.invalid";

/// Project-aware: a `call` node names a function that is not declared.
pub const CALL_UNKNOWN: &str = "script.call.unknown";
/// Project-aware: a `call` node's ports or flow use disagree with the signature.
pub const CALL_PORTS: &str = "script.call.ports";
/// Project-aware: a constant component name is not in the catalog.
pub const CAPABILITY_UNKNOWN_COMPONENT: &str = "script.capability.unknown_component";

/// An instance variable override names no variable or has the wrong type.
pub const VARIABLE_INVALID: &str = "script.variable.invalid";

/// `script_graph` kind rules.
pub mod graph {
    pub const ENTRY_MISSING: &str = "script.graph.entry_missing";
    pub const UNREACHABLE: &str = "script.graph.unreachable";
    pub const VALUE_UNAVAILABLE: &str = "script.graph.value_unavailable";
    pub const FLOW_FANOUT: &str = "script.graph.flow_fanout";
    pub const LOOP_BOUND: &str = "script.graph.loop_bound";
    pub const LATENT_IN_FUNCTION: &str = "script.graph.latent_in_function";
    pub const FUNCTION_UNKNOWN: &str = "script.graph.function_unknown";
    pub const FUNCTION_DUPLICATE: &str = "script.graph.function_duplicate";
    pub const FUNCTION_RECURSIVE: &str = "script.graph.function_recursive";
    pub const FUNCTION_PORTS: &str = "script.graph.function_ports";
    pub const EVENT_NAME: &str = "script.graph.event_name";
    pub const CONST_TYPE: &str = "script.graph.const_type";
    pub const TYPE_MISMATCH: &str = "script.graph.type_mismatch";
    pub const CODE_SIZE: &str = "script.graph.code_size";
}

/// Every code above, for coverage checks and documentation.
pub const ALL: &[&str] = &[
    FUEL_EXHAUSTED,
    MEMORY_LIMIT,
    CALL_DEPTH,
    HOST_ARGUMENT,
    HOST_FAILED,
    CAPABILITY_DENIED,
    VALUE_NONFINITE,
    EVENT_LIMIT,
    SPAWN_LIMIT,
    FUNCTION_UNREGISTERED,
    FUNCTION_SIGNATURE,
    FUNCTION_DUPLICATE,
    FUNCTION_INVALID,
    CALL_UNKNOWN,
    CALL_PORTS,
    CAPABILITY_UNKNOWN_COMPONENT,
    VARIABLE_INVALID,
    graph::ENTRY_MISSING,
    graph::UNREACHABLE,
    graph::VALUE_UNAVAILABLE,
    graph::FLOW_FANOUT,
    graph::LOOP_BOUND,
    graph::LATENT_IN_FUNCTION,
    graph::FUNCTION_UNKNOWN,
    graph::FUNCTION_DUPLICATE,
    graph::FUNCTION_RECURSIVE,
    graph::FUNCTION_PORTS,
    graph::EVENT_NAME,
    graph::CONST_TYPE,
    graph::TYPE_MISMATCH,
    graph::CODE_SIZE,
];
