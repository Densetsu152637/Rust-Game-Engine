//! World-building feature `foliage`: the `engine.Foliage` component over `engine-foliage`
//! (types, scatter, painting, LOD bands, wind), `foliage` instance assets, CPU merged-cluster
//! fallback meshes and the render set the GPU feature (`engine-world-render`, `world.foliage`)
//! draws. See docs/world-foliage.md.
//!
//! Render path rule: foliage draws through the GPU feature when its `render_mode` is `gpu`,
//! or `auto` while the [`FoliageGpuRenderer`] marker resource exists (inserted by
//! `engine_world_render::install`); otherwise it spawns CPU cluster entities
//! (`MeshRenderer` + `GeometryOverride`). It is never on both paths: GPU-path foliage has no
//! cluster entities and CPU-path foliage is not in [`FoliageRenderSet`].
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

mod assets;
mod component;
mod cpu;
mod ground;
mod service;

pub use assets::{Loaded, LoadedType, load_foliage, mapped};
pub use component::{
    COMPONENT_NAME, FoliageCluster, FoliageComponent, FoliageCulling, FoliageGenerated,
    FoliageMode, FoliageRenderMode, validate_scene,
};
pub use cpu::{IMPOSTOR_VERTICES, impostor_mesh, merge_instances};
pub use service::{
    FoliageGpuRenderer, FoliageRenderItem, FoliageRenderSet, FoliageRenderType, FoliageStatus,
    Foliages, MAX_FOLIAGES, RenderImpostor, export_asset, field, paint, status, sync_foliage,
    types, update_foliage_render,
};

pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    vec![component::descriptor()]
}
/// Registers the typed ECS adapter for `engine.Foliage`.
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    crate::register_component::<FoliageComponent>(app, COMPONENT_NAME, 1)
}
/// Installs the foliage service and an empty render set.
pub fn install(world: &mut World) -> Result<()> {
    service::install(world);
    Ok(())
}
/// Generates, loads and re-projects foliage before physics (after terrain and PCG).
pub fn pre_simulation(world: &mut World, _dt: f32) -> Result<()> {
    sync_foliage(world)
}
/// Chooses render paths and builds CPU clusters for the active camera after transforms.
pub fn post_transforms(world: &mut World, _dt: f32) -> Result<()> {
    update_foliage_render(world)
}
