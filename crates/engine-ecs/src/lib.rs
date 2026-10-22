//! ECS layer of the engine.
//!
//! Bevy ECS owns entities, components, queries, resources, deferred commands and
//! scheduling; this crate re-exports it so every engine crate shares one
//! dependency identity. It adds only what Bevy does not provide: published
//! [`Current`]/[`Next`] buffered values and the change-driven simulation to
//! render-world extraction ([`RenderWorld`]). It has no app lifecycle and no
//! stages: those live in `engine-core`.

mod buffered;
mod snapshot;

pub use bevy_ecs;
pub use glam;

pub use buffered::{Buffered, Current, Next, publish_buffered};
pub use snapshot::{
    ExtractedFrom, ExtractionSet, RenderWorld, cleanup_extracted_entities, extract_component,
};

pub mod prelude {
    pub use crate::{
        Buffered, Current, ExtractedFrom, ExtractionSet, Next, RenderWorld, extract_component,
        publish_buffered,
    };
    pub use bevy_ecs::prelude::*;
    pub use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
}
