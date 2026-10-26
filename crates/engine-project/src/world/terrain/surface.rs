//! `engine_pcg::Surface` for a placed heightfield: PCG volumes (and foliage scattering) sample
//! terrain height, normal and splat-layer weights in world coordinates.
use engine_pcg::{Aabb, Surface};
use engine_terrain::Heightfield;
use glam::{Mat3, Mat4, Vec3};
use std::sync::Arc;

/// A snapshot of one terrain placed by its world transform. Queries take world `(x, z)`; the
/// terrain is assumed upright (rotation about Y only), so local X/Z follow from world X/Z.
#[derive(Clone, Debug)]
pub struct TerrainSurface {
    field: Arc<Heightfield>,
    world_from_local: Mat4,
    local_from_world: Mat4,
    normal_from_local: Mat3,
    bounds: Aabb,
}

impl TerrainSurface {
    /// Places `field` with `world_from_local`. Returns `None` for a singular transform.
    pub fn new(field: Arc<Heightfield>, world_from_local: Mat4) -> Option<Self> {
        if world_from_local.determinant().abs() < 1e-12 || !world_from_local.is_finite() {
            return None;
        }
        let (lo, hi) = field.local_bounds();
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let p = world_from_local.transform_point3(corner);
            min = min.min(p);
            max = max.max(p);
        }
        Some(Self {
            local_from_world: world_from_local.inverse(),
            normal_from_local: Mat3::from_mat4(world_from_local).inverse().transpose(),
            world_from_local,
            field,
            bounds: Aabb { min, max },
        })
    }
    /// The heightfield snapshot.
    pub fn field(&self) -> &Arc<Heightfield> {
        &self.field
    }
    fn local(&self, x: f32, z: f32) -> Vec3 {
        self.local_from_world.transform_point3(Vec3::new(x, 0.0, z))
    }
}

impl Surface for TerrainSurface {
    fn bounds(&self) -> Aabb {
        self.bounds
    }
    fn height(&self, x: f32, z: f32) -> f32 {
        let l = self.local(x, z);
        let h = self.field.height_at(l.x, l.z);
        self.world_from_local
            .transform_point3(Vec3::new(l.x, h, l.z))
            .y
    }
    fn normal(&self, x: f32, z: f32) -> Vec3 {
        let l = self.local(x, z);
        (self.normal_from_local * self.field.smooth_normal_at(l.x, l.z)).normalize()
    }
    fn layer_weight(&self, layer: &str, x: f32, z: f32) -> Option<f32> {
        let l = self.local(x, z);
        self.field.layer_weight_named(layer, l.x, l.z)
    }
}
