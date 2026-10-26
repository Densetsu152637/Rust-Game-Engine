//! Extraction of the terrain render set into the render world.
use bevy_ecs::prelude::*;
use engine_core::{EngineApp, ExtractionSet, RenderWorld, Stage};
use engine_project::world::terrain::{TerrainGpuRenderer, TerrainRenderSet};

/// The `Extract`-stage system: copies the simulation world's [`TerrainRenderSet`] (shared
/// heightfields, so no sample data is copied) into the render world, where the terrain
/// features read it.
pub fn extract_terrain(world: &mut World) {
    let set = world
        .get_resource::<TerrainRenderSet>()
        .cloned()
        .unwrap_or_default();
    if let Some(mut render) = world.get_resource_mut::<RenderWorld>() {
        render.world_mut().insert_resource(set);
    }
}

pub(super) fn install(app: &mut EngineApp) {
    app.world_mut().insert_resource(TerrainGpuRenderer);
    app.schedule_mut(Stage::Extract).add_systems(
        extract_terrain
            .after(ExtractionSet::Cleanup)
            .ambiguous_with_all(),
    );
}
