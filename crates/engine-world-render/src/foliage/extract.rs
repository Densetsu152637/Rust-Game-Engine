//! Extraction of the foliage render set into the render world.
use bevy_ecs::prelude::*;
use engine_core::{EngineApp, ExtractionSet, RenderWorld, Stage};
use engine_project::world::foliage::{FoliageGpuRenderer, FoliageRenderSet};

/// The `Extract`-stage system: copies the simulation world's [`FoliageRenderSet`] (shared
/// instance fields and meshes, so no instance data is copied) into the render world, where
/// the foliage feature reads it.
pub fn extract_foliage(world: &mut World) {
    let set = world
        .get_resource::<FoliageRenderSet>()
        .cloned()
        .unwrap_or_default();
    if let Some(mut render) = world.get_resource_mut::<RenderWorld>() {
        render.world_mut().insert_resource(set);
    }
}

pub(super) fn install(app: &mut EngineApp) {
    app.world_mut().insert_resource(FoliageGpuRenderer);
    app.schedule_mut(Stage::Extract).add_systems(
        extract_foliage
            .after(ExtractionSet::Cleanup)
            .ambiguous_with_all(),
    );
}
