//! The ground foliage grows on: a terrain entity (live heightfield, holes) or a PCG surface.
use super::component::FoliageComponent;
use crate::world::pcg::PcgSurfaceRegistry;
use crate::world::terrain::{self, TerrainSurface};
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_foliage::HoleMask;
use engine_pcg::{Aabb, Surface};
use engine_terrain::Heightfield;
use glam::{Mat3, Mat4, Vec3};
use std::sync::Arc;

/// A placed terrain with hole awareness (world coordinates in, world coordinates out).
pub struct TerrainGround {
    surface: TerrainSurface,
    field: Arc<Heightfield>,
    local_from_world: Mat4,
    normal_from_local: Mat3,
}

impl TerrainGround {
    /// Places `field` with `world_from_local`; `None` for a singular transform. Surface
    /// queries and hole tests take world `(x, z)`; the normal is the exact triangle normal.
    pub fn new(field: Arc<Heightfield>, world_from_local: Mat4) -> Option<Self> {
        let surface = TerrainSurface::new(field.clone(), world_from_local)?;
        Some(Self {
            surface,
            local_from_world: world_from_local.inverse(),
            normal_from_local: Mat3::from_mat4(world_from_local).inverse().transpose(),
            field,
        })
    }
}

impl Surface for TerrainGround {
    fn bounds(&self) -> Aabb {
        self.surface.bounds()
    }
    fn height(&self, x: f32, z: f32) -> f32 {
        self.surface.height(x, z)
    }
    /// The exact normal of the terrain triangle under the point (not the smoothed sample
    /// normal PCG uses), so slope limits are exact against the drawn and simulated surface.
    fn normal(&self, x: f32, z: f32) -> Vec3 {
        let local = self.local_from_world.transform_point3(Vec3::new(x, 0.0, z));
        (self.normal_from_local * self.field.normal_at(local.x, local.z)).normalize_or(Vec3::Y)
    }
    fn layer_weight(&self, layer: &str, x: f32, z: f32) -> Option<f32> {
        self.surface.layer_weight(layer, x, z)
    }
}

impl HoleMask for TerrainGround {
    fn is_hole(&self, x: f32, z: f32) -> bool {
        let local = self.local_from_world.transform_point3(Vec3::new(x, 0.0, z));
        !self.field.contains(local.x, local.z) || self.field.is_hole_at(local.x, local.z)
    }
}

/// Identity of a ground: changes whenever the ground's content may have changed.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum GroundKey {
    Terrain {
        entity: Entity,
        revision: u64,
        matrix: [[u32; 4]; 4],
    },
    Surface {
        name: String,
        pointer: usize,
    },
}

pub(super) struct Ground {
    pub surface: Arc<dyn Surface>,
    /// Present for terrain targets.
    pub terrain: Option<Arc<TerrainGround>>,
    pub key: GroundKey,
}

impl Ground {
    pub fn holes(&self) -> Option<&dyn HoleMask> {
        self.terrain.as_deref().map(|t| t as &dyn HoleMask)
    }
}

/// Resolves the component's ground against the world.
pub(super) fn resolve(world: &mut World, component: &FoliageComponent) -> Result<Ground> {
    if !component.terrain.is_empty() {
        let entity = crate::services::find_entity(world, &component.terrain).ok_or_else(|| {
            Error::new(
                "foliage.ground.terrain",
                format!("no entity `{}` to grow on", component.terrain),
            )
        })?;
        let field = terrain::heightfield(world, entity).map_err(|e| {
            Error::new(
                "foliage.ground.terrain",
                format!(
                    "entity `{}` is not a usable terrain: {e}",
                    component.terrain
                ),
            )
        })?;
        let matrix = crate::world_transform(world, entity)?.matrix;
        let world_from_local = Mat4::from_cols_array_2d(&matrix);
        let ground = Arc::new(
            TerrainGround::new(field.clone(), world_from_local).ok_or_else(|| {
                Error::new("foliage.ground.terrain", "singular terrain transform")
            })?,
        );
        return Ok(Ground {
            surface: ground.clone(),
            terrain: Some(ground),
            key: GroundKey::Terrain {
                entity,
                revision: field.revision(),
                matrix: matrix.map(|c| c.map(f32::to_bits)),
            },
        });
    }
    let registry = world.get_resource::<PcgSurfaceRegistry>();
    let surface = registry
        .and_then(|r| r.get(&component.surface))
        .ok_or_else(|| {
            Error::new(
                "foliage.ground.surface",
                format!("no surface `{}` is registered", component.surface),
            )
        })?;
    Ok(Ground {
        key: GroundKey::Surface {
            name: component.surface.clone(),
            pointer: Arc::as_ptr(&surface).cast::<()>() as usize,
        },
        surface,
        terrain: None,
    })
}
