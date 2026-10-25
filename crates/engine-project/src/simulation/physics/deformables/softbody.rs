//! Authored `engine.SoftBody`: data, validation, schema and the engine-xpbd description.
use super::cloth::descriptor_for;
use super::{Coupling, bad, check, number, vector};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_xpbd::{SoftBodyDesc, SoftBodyParams};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Most vertices of an inline soft-body mesh.
pub const MAX_SOFT_VERTICES: usize = 16_384;
/// Most tetrahedra of an inline soft-body mesh.
pub const MAX_SOFT_TETS: usize = 65_536;
/// Most box segments per axis.
pub const MAX_BOX_SEGMENTS: u32 = 32;

/// Where the tetrahedral mesh comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoftBodySource {
    /// A `size` box centred on the entity, cut into `resolution` cells per axis.
    #[default]
    Box,
    /// Inline entity-local `positions` and `tets`.
    Mesh,
}

/// An XPBD tetrahedral soft body. Particles simulate in world space; the entity `Transform`
/// only places the body when it is created (it is an anchor, nothing is written back).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoftBody {
    pub source: SoftBodySource,
    /// Box extents (X, Y, Z) in meters.
    pub size: [f32; 3],
    /// Box cells per axis.
    pub resolution: [u32; 3],
    /// Inline mesh vertices in entity-local space.
    pub positions: Vec<[f32; 3]>,
    /// Inline mesh tetrahedra.
    pub tets: Vec<[u32; 4]>,
    /// Particles held fixed at their creation position.
    pub pinned: Vec<u32>,
    /// Density in kg/m^3.
    pub density: f32,
    pub edge_compliance: f32,
    /// Inverse bulk modulus in 1/Pa.
    pub volume_compliance: f32,
    pub damping: f32,
    /// Collision margin above rigid surfaces in meters.
    pub particle_radius: f32,
    pub friction: f32,
    pub gravity_scale: f32,
    pub substeps: u32,
    pub iterations: u32,
    /// Reaction impulses on dynamic rigid bodies (two-way coupling); off keeps one-way collision.
    pub two_way: bool,
    /// Multiplier on the reaction impulses, 0..=100.
    pub coupling_scale: f32,
    /// Rigid bodies whose `material.layer` shares a bit with this mask collide.
    pub collision_mask: u32,
}
impl Default for SoftBody {
    fn default() -> Self {
        let params = SoftBodyParams::default();
        Self {
            source: SoftBodySource::Box,
            size: [1.; 3],
            resolution: [4; 3],
            positions: vec![],
            tets: vec![],
            pinned: vec![],
            density: 500.,
            edge_compliance: params.edge_compliance,
            volume_compliance: params.volume_compliance,
            damping: params.damping,
            particle_radius: params.particle_radius,
            friction: params.friction,
            gravity_scale: 1.,
            substeps: 10,
            iterations: 1,
            two_way: true,
            coupling_scale: 1.,
            collision_mask: u32::MAX,
        }
    }
}
impl SoftBody {
    pub(super) fn coupling(&self) -> Coupling {
        Coupling {
            two_way: self.two_way,
            scale: self.coupling_scale,
            mask: self.collision_mask,
        }
    }
    /// Particles the mesh will have.
    pub fn particle_count(&self) -> usize {
        match self.source {
            SoftBodySource::Box => self.resolution.iter().map(|&n| n as usize + 1).product(),
            SoftBodySource::Mesh => self.positions.len(),
        }
    }
    /// Bounds of every authored value; no world access.
    pub fn validate(&self) -> Result<()> {
        const NAME: &str = "engine.SoftBody";
        match self.source {
            SoftBodySource::Box => check(
                NAME,
                &[
                    (
                        self.size.iter().all(|v| (0.001..=1000.).contains(v)),
                        "size (0.001..=1000 m)",
                    ),
                    (
                        self.resolution
                            .iter()
                            .all(|v| (1..=MAX_BOX_SEGMENTS).contains(v)),
                        "resolution (1..=32)",
                    ),
                ],
            )?,
            SoftBodySource::Mesh => {
                if !(4..=MAX_SOFT_VERTICES).contains(&self.positions.len())
                    || !(1..=MAX_SOFT_TETS).contains(&self.tets.len())
                {
                    return bad(NAME, "mesh needs 4..=16384 positions and 1..=65536 tets");
                }
                if self
                    .positions
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 1e5)
                    || self
                        .tets
                        .iter()
                        .flatten()
                        .any(|&i| i as usize >= self.positions.len())
                {
                    return bad(NAME, "mesh positions must be finite and indices in range");
                }
            }
        }
        if self.pinned.len() > super::cloth::MAX_PINNED
            || self
                .pinned
                .iter()
                .any(|&p| p as usize >= self.particle_count())
        {
            return bad(NAME, "pinned needs at most 4096 in-range particle indices");
        }
        let unit = |v: f32, max: f32| v.is_finite() && (0. ..=max).contains(&v);
        check(
            NAME,
            &[
                (
                    self.density.is_finite() && (1e-3..=1e5).contains(&self.density),
                    "density",
                ),
                (unit(self.edge_compliance, 1e6), "edge_compliance"),
                (unit(self.volume_compliance, 1e6), "volume_compliance"),
                (unit(self.damping, 1e3), "damping"),
                (unit(self.particle_radius, 100.), "particle_radius"),
                (unit(self.friction, 10.), "friction"),
                (
                    self.gravity_scale.is_finite() && self.gravity_scale.abs() <= 100.,
                    "gravity_scale",
                ),
                ((1..=64).contains(&self.substeps), "substeps (1..=64)"),
                ((1..=16).contains(&self.iterations), "iterations (1..=16)"),
            ],
        )?;
        self.coupling().validate(NAME)
    }
    /// Copy with every live-tunable field reset: a difference in the rest rebuilds the body.
    pub(super) fn structure(&self) -> Self {
        let live = Self::default();
        Self {
            edge_compliance: live.edge_compliance,
            volume_compliance: live.volume_compliance,
            damping: live.damping,
            particle_radius: live.particle_radius,
            friction: live.friction,
            gravity_scale: live.gravity_scale,
            substeps: live.substeps,
            iterations: live.iterations,
            two_way: live.two_way,
            coupling_scale: live.coupling_scale,
            collision_mask: live.collision_mask,
            ..self.clone()
        }
    }
    pub(super) fn params(&self) -> SoftBodyParams {
        SoftBodyParams {
            edge_compliance: self.edge_compliance,
            volume_compliance: self.volume_compliance,
            damping: self.damping,
            particle_radius: self.particle_radius,
            friction: self.friction,
        }
    }
    /// World-space description at the entity pose `(scale, rotation, position)`.
    pub(super) fn describe(&self, pose: (Vec3, Quat, Vec3)) -> Result<SoftBodyDesc> {
        let to_world = |p: Vec3| pose.2 + pose.1 * (p * pose.0);
        let convert =
            |e: engine_xpbd::Error| crate::simulation::invalid(format!("engine.SoftBody: {e}"));
        let mut desc = match self.source {
            SoftBodySource::Box => {
                let half = Vec3::from(self.size) / 2.;
                SoftBodyDesc::cuboid(-half, half, self.resolution, self.density).map_err(convert)?
            }
            SoftBodySource::Mesh => SoftBodyDesc {
                positions: self.positions.iter().map(|p| Vec3::from(*p)).collect(),
                tets: self.tets.clone(),
                density: self.density,
                inverse_masses: None,
                pinned: vec![],
                params: self.params(),
            },
        };
        for position in &mut desc.positions {
            *position = to_world(*position);
        }
        desc.pinned = self.pinned.clone();
        desc.pinned.sort_unstable();
        desc.pinned.dedup();
        desc.params = self.params();
        Ok(desc)
    }
}

pub(super) fn descriptor() -> crate::ComponentDescriptor {
    let mut properties = json!({
        "source":{"type":"string","enum":["box","mesh"]},
        "size":{"type":"array","items":{"type":"number","minimum":0.001,"maximum":1000},"minItems":3,"maxItems":3,
            "description":"Box extents in meters."},
        "resolution":{"type":"array","items":{"type":"integer","minimum":1,"maximum":MAX_BOX_SEGMENTS},"minItems":3,"maxItems":3,
            "description":"Box cells per axis."},
        "positions":{"type":"array","maxItems":MAX_SOFT_VERTICES,"items":vector(3),
            "description":"Inline mesh vertices in entity-local space."},
        "tets":{"type":"array","maxItems":MAX_SOFT_TETS,"items":{"type":"array","minItems":4,"maxItems":4,
            "items":{"type":"integer","minimum":0}}},
        "pinned":{"type":"array","maxItems":super::cloth::MAX_PINNED,"items":{"type":"integer","minimum":0},
            "description":"Particles held fixed at their creation position."},
        "density":number(1e-3, 1e5, "Density in kg/m^3."),
        "edge_compliance":number(0., 1e6, "Edge-length compliance."),
        "volume_compliance":number(0., 1e6, "Volume compliance (inverse bulk modulus, 1/Pa)."),
        "damping":number(0., 1e3, "Velocity damping rate in 1/s."),
        "particle_radius":number(0., 100., "Collision margin above rigid surfaces in meters."),
        "friction":number(0., 10., "Friction against rigid bodies."),
        "gravity_scale":number(-100., 100., "Multiplier on scene gravity."),
        "substeps":{"type":"integer","minimum":1,"maximum":64},
        "iterations":{"type":"integer","minimum":1,"maximum":16}
    });
    super::coupling_schema(&mut properties);
    descriptor_for("engine.SoftBody", SoftBody::default(), properties, vec![])
}
