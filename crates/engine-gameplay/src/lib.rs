//! Pure gameplay frameworks shared by every game.
//!
//! * [`ability`]: a Gameplay Ability System in the style of Unreal's GAS: attribute sets,
//!   hierarchical gameplay tags, gameplay effects (instant, duration, infinite, periodic,
//!   stacking), abilities with costs, cooldowns and tag rules, native and data-only ability
//!   behaviours, and client prediction through `engine-net-predict` prediction keys.
//!
//! The crate has no engine or ECS dependency: `engine-project` wraps it as the
//! `engine.AbilitySystem` component, the `gameplay` asset kind and its runtime service.
pub mod ability;
