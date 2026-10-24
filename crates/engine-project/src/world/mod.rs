//! World building: terrain, foliage, water, level streaming, procedural content generation,
//! large-world coordinates and 2D tilemaps.
//!
//! Each submodule owns its authored `engine.*` component descriptors, ECS registration,
//! installed services and per-tick hooks. This module only aggregates them in a fixed order
//! so the catalog, runtime and MCP see one stable list.
use crate::{ComponentDescriptor, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;

pub mod coordinates;
pub mod foliage;
pub mod pcg;
pub mod streaming;
pub mod terrain;
pub mod tilemap;
pub mod water;

/// Authored component descriptors for every world-building feature.
pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    let mut result = Vec::new();
    result.extend(coordinates::component_descriptors());
    result.extend(terrain::component_descriptors());
    result.extend(foliage::component_descriptors());
    result.extend(water::component_descriptors());
    result.extend(streaming::component_descriptors());
    result.extend(pcg::component_descriptors());
    result.extend(tilemap::component_descriptors());
    result
}

/// Registers typed native ECS adapters for every world-building component.
pub fn register_components(app: &mut EngineApp) -> Result<()> {
    coordinates::register_components(app)?;
    terrain::register_components(app)?;
    foliage::register_components(app)?;
    water::register_components(app)?;
    streaming::register_components(app)?;
    pcg::register_components(app)?;
    tilemap::register_components(app)
}

/// Installs world services into a fresh runtime world.
pub fn install(world: &mut World) -> Result<()> {
    coordinates::install(world)?;
    terrain::install(world)?;
    foliage::install(world)?;
    water::install(world)?;
    streaming::install(world)?;
    pcg::install(world)?;
    tilemap::install(world)
}

/// Runs before the simulation step (forces, streaming requests, generation).
pub fn pre_simulation(world: &mut World, dt: f32) -> Result<()> {
    coordinates::pre_simulation(world, dt)?;
    streaming::pre_simulation(world, dt)?;
    pcg::pre_simulation(world, dt)?;
    terrain::pre_simulation(world, dt)?;
    water::pre_simulation(world, dt)?;
    foliage::pre_simulation(world, dt)?;
    tilemap::pre_simulation(world, dt)
}

/// Runs after world transforms are refreshed (view-dependent LOD and render geometry).
pub fn post_transforms(world: &mut World, dt: f32) -> Result<()> {
    coordinates::post_transforms(world, dt)?;
    terrain::post_transforms(world, dt)?;
    water::post_transforms(world, dt)?;
    foliage::post_transforms(world, dt)?;
    streaming::post_transforms(world, dt)?;
    pcg::post_transforms(world, dt)?;
    tilemap::post_transforms(world, dt)
}
