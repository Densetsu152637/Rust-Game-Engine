//! World-building feature `water`: oceans, lakes and rivers from `engine.WaterBody` over the
//! `engine-water` model, buoyancy for `engine.Buoyancy` bodies, a per-tick water clock, CPU
//! fallback surface meshes and the render set the GPU feature (`engine-world-render`,
//! `world.water`) draws. See docs/world-water.md.
//!
//! Render path rule (as for terrain): a water body draws through the GPU feature when its
//! `render_mode` is `gpu`, or `auto` while the [`WaterGpuRenderer`] marker resource exists
//! (inserted by `engine_world_render::install`); otherwise it spawns one CPU surface mesh entity
//! (`MeshRenderer` + `GeometryOverride` + a transparent `MeshMaterial`). A body is never on
//! both paths: GPU-path bodies have no mesh entity and CPU-path bodies are not in
//! [`WaterRenderSet`].
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

mod component;
mod service;

pub use component::{
    BUOYANCY, BuoyancyComponent, MAX_BUOYANCY_POINTS, WATER_BODY, WaterBodyComponent, WaterKind,
    WaterRenderMode, WaterShoreline, WaveModel, WaveSettings, validate_scene,
};
pub use service::{
    BuoyancyStatus, MAX_BUOYANT_BODIES, MAX_WATER_BODIES, UnderwaterView, WaterClock,
    WaterGpuRenderer, WaterMesh, WaterQuery, WaterRenderItem, WaterRenderSet, WaterStatus, Waters,
    buoyancy_status, grid_point, status, step_water, surface, underwater_at, update_water_render,
    water_at, water_at_filtered,
};

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    vec![
        component::water_descriptor(),
        component::buoyancy_descriptor(),
    ]
}
/// Registers the typed ECS adapters for `engine.WaterBody` and `engine.Buoyancy`.
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<WaterBodyComponent>(app, WATER_BODY, 1)?;
    crate::register_component::<BuoyancyComponent>(app, BUOYANCY, 1)
}
/// Installs the water service, clock and an empty render set.
pub fn install(world: &mut World) -> Result<()> {
    service::install(world);
    Ok(())
}
/// Advances the water clock, rebuilds changed bodies and applies buoyancy before physics.
pub fn pre_simulation(world: &mut World, dt: f32) -> Result<()> {
    step_water(world, dt)
}
/// Rebuilds CPU surface meshes, publishes GPU-path bodies and the camera's underwater state.
pub fn post_transforms(world: &mut World, _dt: f32) -> Result<()> {
    update_water_render(world)
}
