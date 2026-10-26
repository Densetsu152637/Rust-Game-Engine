//! The seam between a state tree and the game that runs it.
//!
//! A state tree shares the behavior tree [`Host`] (navigation, events, native
//! task calls) and adds what evaluators need: named host inputs and native
//! evaluator callbacks. `engine-project` implements it over the ECS; tests use
//! plain recorders.
use crate::{
    behavior_tree::{Host, NativeCall},
    blackboard::{BbValue, Blackboard},
};

/// What a running state tree needs from its game.
///
/// Native tasks reuse [`Host::native_start`], [`Host::native_tick`] and
/// [`Host::native_abort`] (abort runs when the owning state exits while the task
/// still runs). Native evaluators use the three `evaluator_*` methods.
pub trait StateTreeHost: Host {
    /// Current value of the named host input, or `None` when the host has no such
    /// input or it has no value. Read by `copy_evaluator`.
    fn input(&mut self, name: &str) -> Option<BbValue>;
    /// First run of a native evaluator (the first tick of the tree, in place of a
    /// tick).
    fn evaluator_start(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard);
    /// Every later tick, before transitions are evaluated.
    fn evaluator_tick(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard, dt: f32);
    /// The tree finished or was stopped.
    fn evaluator_stop(&mut self, call: &NativeCall<'_>, blackboard: &mut Blackboard);
}
