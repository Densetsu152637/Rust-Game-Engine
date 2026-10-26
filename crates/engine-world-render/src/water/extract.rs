//! Extraction of the water render set into the render world.
use bevy_ecs::prelude::*;
use engine_core::{EngineApp, ExtractionSet, RenderWorld, Stage};
use engine_project::world::water::{WaterGpuRenderer, WaterRenderSet};

/// The `Extract`-stage system: copies the simulation world's [`WaterRenderSet`] (shared
/// surfaces and ocean frames, so no grid data is copied) into the render world, where the
/// water features read it.
pub fn extract_water(world: &mut World) {
    let set = world
        .get_resource::<WaterRenderSet>()
        .cloned()
        .unwrap_or_default();
    if let Some(mut render) = world.get_resource_mut::<RenderWorld>() {
        render.world_mut().insert_resource(set);
    }
}

pub(super) fn install(app: &mut EngineApp) {
    app.world_mut().insert_resource(WaterGpuRenderer);
    app.schedule_mut(Stage::Extract).add_systems(
        extract_water
            .after(ExtractionSet::Cleanup)
            .ambiguous_with_all(),
    );
}
