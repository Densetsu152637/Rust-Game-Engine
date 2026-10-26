//! State trees: a graph kind, a compiler and a deterministic executor.
//!
//! * [`kind`](mod@kind): the `state_tree` [`GraphKind`](engine_graph::GraphKind), its node
//!   types, ports and parameter schemas.
//! * [`compile`](mod@compile): turns a validated document into an immutable [`StateTree`].
//! * [`exec`]: [`StateTreeInstance`], the per-agent tick executor with hierarchical
//!   selection, prioritised and delayed transitions, evaluators and a bounded log.
//! * [`host`]: the [`StateTreeHost`] seam (navigation, events, inputs, natives).
//!
//! Native tasks and evaluators reuse the behavior tree machinery
//! ([`NativeTask`](crate::behavior_tree::NativeTask) and its registry). See
//! `docs/ai-state-trees.md`.
pub mod compile;
pub mod exec;
pub mod host;
pub mod kind;

pub use compile::{Completion, Selection, StateTree, Trigger, compile, st_codes};
pub use exec::{
    LogEvent, LogRecord, MAX_LOG, MAX_PENDING_EVENTS, MAX_TRANSITIONS_PER_TICK, Phase, StateDebug,
    StateTreeDebug, StateTreeInstance, TickReport, TransitionRecord,
};
pub use host::StateTreeHost;
pub use kind::{KIND, kind, node, port, shared_kind};
