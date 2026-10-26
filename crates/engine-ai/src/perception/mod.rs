//! AI perception: sight cones with occlusion, hearing, damage stimuli, team
//! filtering and per-perceiver memory with decay.
//!
//! * [`config`]: senses ([`SightConfig`], [`HearingConfig`], [`DamageConfig`]), teams
//!   and affiliation filters.
//! * [`system`]: [`Perception`], the deterministic update, and the
//!   [`PerceptionHost`] seam (line-of-sight rays and hearing attenuation).
//!
//! See `docs/ai-perception.md`.
pub mod config;
pub mod system;

pub use config::{
    ANONYMOUS, Affiliation, AffiliationFilter, DamageConfig, HearingConfig, Id, MAX_MEMORY_SECONDS,
    MAX_RANGE, Sense, SensorConfig, SightConfig, TeamTable,
};
pub use system::{
    Damage, EventKind, Known, MAX_EVENTS, MAX_RECORDS, Noise, PerceiverInput, Perception,
    PerceptionEvent, PerceptionHost, TargetInput, UpdateInput, UpdateReport,
};
