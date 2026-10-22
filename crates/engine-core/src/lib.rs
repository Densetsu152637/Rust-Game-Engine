//! Native engine foundation. Project code shares these Rust types in one executable.
//!
//! The ECS layer lives in `engine-ecs` and is re-exported here, so
//! `engine_core::bevy_ecs`, `engine_core::Current` and friends keep resolving.
//! This crate owns app lifecycle, stages, simulation time and run control, and
//! emits `tracing` events for lifecycle transitions (it never installs a
//! subscriber; see `engine-log`).

mod app;

pub use anyhow::{self, Result};
pub use engine_ecs::{
    self, Buffered, Current, ExtractedFrom, ExtractionSet, Next, RenderWorld, bevy_ecs,
    cleanup_extracted_entities, extract_component, glam, publish_buffered,
};

pub use app::{AppState, EngineApp, RuntimeControl, Stage, Time};

pub mod prelude {
    pub use crate::{AppState, EngineApp, Result, RuntimeControl, Stage, Time};
    pub use engine_ecs::prelude::*;
}
