//! Pure gameplay frameworks shared by every game.
//!
//! * [`ability`]: a Gameplay Ability System in the style of Unreal's GAS: attribute sets,
//!   hierarchical gameplay tags, gameplay effects (instant, duration, infinite, periodic,
//!   stacking), abilities with costs, cooldowns and tag rules, native and data-only ability
//!   behaviours, and client prediction through `engine-net-predict` prediction keys.
//!
//! * [`inventory`]: item definitions, slot containers with weight limits, atomic add, remove,
//!   move, split and transfer, and tag-gated equipment.
//! * [`quest`]: staged quests whose objectives count named events, flags or inventory
//!   contents, with prerequisites, rewards and failure.
//! * [`dialogue`]: the `dialogue` graph kind and a deterministic conversation runtime.
//!
//! The crate has no engine or ECS dependency: `engine-project` wraps it as the
//! `engine.AbilitySystem` component, the `gameplay` asset kind and its runtime service.
pub mod ability;
pub mod dialogue;
pub mod inventory;
pub mod quest;
