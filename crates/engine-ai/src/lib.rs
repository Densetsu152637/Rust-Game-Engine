//! Pure AI runtimes for the engine, independent of any ECS or renderer.
//!
//! * [`blackboard`]: typed key/value store with change notifications.
//! * [`behavior_tree`]: the `behavior_tree` graph kind, compiler and executor.
//! * [`rng`]: the deterministic generator every AI decision must use.
//!
//! State trees, perception and environment queries join as sibling modules. Every
//! runtime is a pure function of authored data, its inputs and `dt`; games embed it
//! through a host trait (see `engine-project` for the ECS binding).
pub mod behavior_tree;
pub mod blackboard;
pub mod rng;

pub use blackboard::{BbValue, Blackboard, BlackboardError};
pub use rng::SplitMix64;
