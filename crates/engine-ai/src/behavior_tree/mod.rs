//! Behavior trees: a graph kind, a compiler and a deterministic executor.
//!
//! * [`kind`](mod@kind): the `behavior_tree` [`GraphKind`](engine_graph::GraphKind), its node
//!   types, ports and parameter schemas.
//! * [`compile`]: turns a validated document into an immutable [`BehaviorTree`].
//! * [`exec`]: [`TreeInstance`], the per-agent tick executor with observer aborts,
//!   services and a bounded debug view.
//! * [`host`]: the [`Host`] seam (navigation, events, native tasks).
//!
//! See `docs/ai-behavior-trees.md`.
pub mod compile;
pub mod exec;
pub mod host;
pub mod kind;

pub use compile::{AbortMode, BehaviorTree, CooldownLock, Operator, Policy, bt_codes, compile};
pub use exec::{
    AbortReason, AbortRecord, LogEvent, LogRecord, MAX_LOG, MAX_PASSES, NodeDebug, TickReport,
    TreeDebug, TreeInstance,
};
pub use host::{
    Host, MoveStatus, NativeCall, NativeTask, NativeTaskInstances, NativeTaskRegistry, Status,
    TaskContext, TaskFactory,
};
pub use kind::{KIND, kind, node, port, shared_kind};
