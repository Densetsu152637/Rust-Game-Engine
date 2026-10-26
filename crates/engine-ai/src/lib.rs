//! Pure AI runtimes for the engine, independent of any ECS or renderer.
//!
//! * [`blackboard`]: typed key/value store with change notifications.
//! * [`behavior_tree`]: the `behavior_tree` graph kind, compiler and executor.
//! * [`perception`]: sight, hearing and damage stimuli with per-perceiver memory.
//! * [`state_tree`]: the `state_tree` graph kind, compiler and executor.
//! * [`rng`]: the deterministic generator every AI decision must use.
//!
//! Environment queries join as a sibling module. Every
//! runtime is a pure function of authored data, its inputs and `dt`; games embed it
//! through a host trait (see `engine-project` for the ECS binding).
pub mod behavior_tree;
pub mod blackboard;
pub mod perception;
pub mod rng;
pub mod state_tree;

pub use blackboard::{BbValue, Blackboard, BlackboardError};
pub use rng::SplitMix64;
