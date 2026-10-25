//! Per-frame render data of the deformables and the gated debug segments.
//!
//! [`PhysicsRenderData`] is a world resource refreshed after every simulation step that ran
//! (at most once per frame). Positions are world space. A render feature reads it from the
//! extracted world and uploads the arrays it needs; triangle lists are shared (`Arc`) because
//! the topology is fixed until the object is rebuilt.
use super::super::{DebugSegment, DebugSegments, service};
use crate::Result;
use bevy_ecs::prelude::*;
use glam::Vec3;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Cloth surface for drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothRender {
    pub positions: Vec<Vec3>,
    /// Area-weighted per-vertex normals.
    pub normals: Vec<Vec3>,
    /// Triangles indexing `positions` (counter-clockwise seen from the normal side).
    pub triangles: Arc<[[u32; 3]]>,
}

/// Soft-body surface for drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct SoftBodyRender {
    /// All particle positions; only surface vertices are referenced by `triangles`.
    pub positions: Vec<Vec3>,
    /// Area-weighted normals (zero for interior particles).
    pub normals: Vec<Vec3>,
    /// Outward-facing boundary triangles indexing `positions`.
    pub triangles: Arc<[[u32; 3]]>,
    pub volume: f32,
    pub rest_volume: f32,
}

/// Fluid particles for drawing as spheres, points or a screen-space/marching-cubes surface.
#[derive(Clone, Debug, PartialEq)]
pub struct FluidRender {
    pub positions: Vec<Vec3>,
    pub velocities: Vec<Vec3>,
    pub densities: Vec<f32>,
    /// Particles on the fluid surface (colour-field normal above the threshold).
    pub surface: Vec<bool>,
    pub particle_radius: f32,
    pub rest_density: f32,
    pub capacity: usize,
    /// World-space domain box.
    pub domain_min: Vec3,
    pub domain_max: Vec3,
}

/// Everything a renderer needs to draw the simulated deformables, keyed by the authoring
/// entity (`engine.Cloth`, `engine.SoftBody`, `engine.FluidVolume`).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct PhysicsRenderData {
    /// Incremented every time the data was refreshed.
    pub frame: u64,
    pub cloths: BTreeMap<Entity, ClothRender>,
    pub soft_bodies: BTreeMap<Entity, SoftBodyRender>,
    pub fluids: BTreeMap<Entity, FluidRender>,
}
impl PhysicsRenderData {
    /// Particles across all objects.
    pub fn particle_count(&self) -> usize {
        self.cloths
            .values()
            .map(|c| c.positions.len())
            .sum::<usize>()
            + self
                .soft_bodies
                .values()
                .map(|c| c.positions.len())
                .sum::<usize>()
            + self
                .fluids
                .values()
                .map(|c| c.positions.len())
                .sum::<usize>()
    }
}

/// Area-weighted vertex normals of `triangles` over `positions`.
pub(super) fn vertex_normals(positions: &[Vec3], triangles: &[[u32; 3]]) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for [a, b, c] in triangles {
        let (pa, pb, pc) = (
            positions[*a as usize],
            positions[*b as usize],
            positions[*c as usize],
        );
        let area_normal = (pb - pa).cross(pc - pa);
        for index in [a, b, c] {
            normals[*index as usize] += area_normal;
        }
    }
    for normal in &mut normals {
        *normal = normal.normalize_or_zero();
    }
    normals
}

const CLOTH_COLOR: [f32; 4] = [0.3, 0.9, 1., 1.];
const SOFT_COLOR: [f32; 4] = [1., 0.7, 0.2, 1.];
const FLUID_COLOR: [f32; 4] = [0.2, 0.5, 1., 1.];
const FLUID_SURFACE_COLOR: [f32; 4] = [0.8, 0.9, 1., 1.];

/// Cloth and soft-body edges and fluid particles as world-space segments, at most
/// `max_segments`; enabled by `engine.PhysicsSettings.debug` (fails otherwise).
///
/// Fluid particles are short segments along their velocity (a tiny vertical tick at rest),
/// brighter on the surface. The engine-physics debug contributor hook takes a stateless
/// function, so the deformables expose their segments here instead of through `debug_view`.
pub fn deformable_debug_segments(world: &World, max_segments: usize) -> Result<DebugSegments> {
    if !service(world)?.settings.debug {
        return Err(crate::simulation::invalid(
            "Physics debug view is disabled; set engine.PhysicsSettings.debug",
        ));
    }
    let Some(data) = world.get_resource::<PhysicsRenderData>() else {
        return Ok(DebugSegments {
            segments: vec![],
            truncated: false,
        });
    };
    let mut segments = Vec::new();
    let mut truncated = false;
    let mut push = |from: Vec3, to: Vec3, color: [f32; 4]| {
        if segments.len() < max_segments {
            segments.push(DebugSegment { from, to, color });
        } else {
            truncated = true;
        }
    };
    for cloth in data.cloths.values() {
        for [a, b, c] in cloth.triangles.iter() {
            let p = |i: &u32| cloth.positions[*i as usize];
            push(p(a), p(b), CLOTH_COLOR);
            push(p(b), p(c), CLOTH_COLOR);
            push(p(c), p(a), CLOTH_COLOR);
        }
    }
    for body in data.soft_bodies.values() {
        for [a, b, c] in body.triangles.iter() {
            let p = |i: &u32| body.positions[*i as usize];
            push(p(a), p(b), SOFT_COLOR);
            push(p(b), p(c), SOFT_COLOR);
            push(p(c), p(a), SOFT_COLOR);
        }
    }
    for fluid in data.fluids.values() {
        for (i, position) in fluid.positions.iter().enumerate() {
            let velocity = fluid.velocities[i];
            let tick = if velocity.length_squared() > 1e-6 {
                velocity * 0.02
            } else {
                Vec3::new(0., fluid.particle_radius * 0.5, 0.)
            };
            let color = if fluid.surface[i] {
                FLUID_SURFACE_COLOR
            } else {
                FLUID_COLOR
            };
            push(*position, *position + tick, color);
        }
    }
    Ok(DebugSegments {
        segments,
        truncated,
    })
}
