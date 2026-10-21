//! Native engine foundation. Project code shares these Rust types in one executable.
//!
//! Bevy owns entities, components, queries, resources, deferred commands, and
//! scheduling. This crate adds lifecycle and simulation-to-render ownership only.

mod app;
mod buffered;
mod snapshot;

pub use anyhow::{self, Result};
pub use bevy_ecs;
pub use glam;

pub use app::{AppState, EngineApp, RuntimeControl, Stage, Time};
pub use buffered::{Current, Next, publish_buffered};
pub use snapshot::{ExtractedFrom, ExtractionSet, RenderWorld, extract_component};

pub mod prelude {
    pub use crate::{
        AppState, Current, EngineApp, ExtractedFrom, ExtractionSet, Next, RenderWorld, Result,
        RuntimeControl, Stage, Time, extract_component, publish_buffered,
    };
    pub use bevy_ecs::prelude::*;
    pub use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
}
