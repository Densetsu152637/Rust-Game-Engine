//! A Gameplay Ability System in the style of Unreal's GAS.
//!
//! * [`tags`]: hierarchical gameplay tags (`State.Stunned` matches `State`), counted
//!   containers and all/any/none queries.
//! * [`defs`]: the authored [`Definitions`] document (attribute sets, effects, abilities),
//!   validated into a shared [`Library`].
//! * [`AbilitySystem`]: actors with attributes (base and aggregated current values,
//!   clamping, change events), owned tags, active effects (instant, duration, infinite,
//!   periodic, stacking, inhibition) and ability instances.
//! * [`AbilityRuntime`]: activation rules, commit (cost and cooldown), data-only steps and
//!   native [`AbilityBehaviour`]s registered by name, gameplay-event triggers.
//! * Prediction: [`AbilitySystem`] implements `engine_net_predict::PredictionHooks`.
//!   Activations and effects carrying a `PredictionKey` are journaled; the ledger's commit
//!   keeps them and its undo (reject, timeout, parent undone, clear) restores attributes
//!   and tags exactly.
//!
//! Aggregation of a current value, per attribute in evaluation order:
//! an `override` modifier (the latest applied wins) replaces everything; otherwise
//! `((base + Σadd) * (1 + Σ(mul - 1))) / (1 + Σ(div - 1))`, then the attribute's clamp
//! bounds. A stack of n counts as n applications of each modifier.
pub mod defs;
pub mod tags;

mod abilities;
mod system;

pub use abilities::{
    AbilityBehaviour, AbilityRuntime, ActivationRequest, BehaviourContext, BehaviourFactory,
    BehaviourRegistry, BehaviourStatus, InstanceInfo,
};
pub use defs::{Definitions, Library};
pub use system::{
    AbilitySystem, ActorId, ActorInit, ApplyFailure, AttributeValue, CancelReason, EffectHandle,
    EffectInfo, EffectSpec, FailReason, GameplayEvent, InstanceId, MAX_ACTORS,
    MAX_EFFECTS_PER_ACTOR, MAX_EXECUTIONS_PER_TICK, MAX_INSTANCES_PER_ACTOR, MAX_LOOSE_TAGS,
    MAX_OUTCOMES, MAX_PENDING_EVENTS, Outcome, RemoveReason, TIME_EPSILON,
};
pub use tags::{Requirements, TagContainer, TagQuery, tag_matches, valid_tag};
