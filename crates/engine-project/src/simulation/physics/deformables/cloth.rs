//! Authored `engine.Cloth`: data, validation, schema and the engine-xpbd description.
use super::{Coupling, bad, check, number, vector};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_xpbd::{ClothDesc, ClothParams};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Most vertices of an inline cloth mesh.
pub const MAX_CLOTH_VERTICES: usize = 16_384;
/// Most triangles of an inline cloth mesh.
pub const MAX_CLOTH_TRIANGLES: usize = 32_768;
/// Most grid segments per axis.
pub const MAX_GRID_SEGMENTS: u32 = 128;
/// Most entity attachments of one cloth.
pub const MAX_ATTACHMENTS: usize = 64;
/// Most fixed pinned particles of one cloth.
pub const MAX_PINNED: usize = 4096;

/// Where the cloth mesh comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClothSource {
    /// A `size` rectangle in the entity's local XZ plane, `resolution` segments per side,
    /// centred on the entity, facing +Y.
    #[default]
    Grid,
    /// Inline entity-local `positions` and `triangles`.
    Mesh,
}

/// Grid edge whose particles are fixed in place (grid source only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClothEdge {
    #[default]
    None,
    /// The edge at local `x = -size[0]/2`.
    MinX,
    /// The edge at local `x = +size[0]/2`.
    MaxX,
    /// The edge at local `z = -size[1]/2`.
    MinZ,
    /// The edge at local `z = +size[1]/2`.
    MaxZ,
    /// The four corner particles.
    Corners,
}

/// A particle that follows an entity (a skinned bone or joint) every step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClothAttachment {
    /// Particle index (`j * (resolution[0] + 1) + i` for grids).
    pub particle: u32,
    /// Authored id of the entity (needs `Transform`).
    pub entity: String,
    /// Point in the entity's local space the particle is held at. When omitted the particle
    /// keeps the place it has at creation relative to the entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f32; 3]>,
}
/// An XPBD cloth. Particles simulate in world space; the entity `Transform` only places the
/// cloth when it is created (it is an anchor, nothing is written back).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cloth {
    pub source: ClothSource,
    /// Grid width (X) and depth (Z) in meters.
    pub size: [f32; 2],
    /// Grid segments along X and Z.
    pub resolution: [u32; 2],
    /// Inline mesh vertices in entity-local space (mesh source).
    pub positions: Vec<[f32; 3]>,
    /// Inline mesh triangles (mesh source).
    pub triangles: Vec<[u32; 3]>,
    /// Grid edge held fixed in world space.
    pub fixed: ClothEdge,
    /// Further particles held fixed at their creation position.
    pub pinned: Vec<u32>,
    /// Particles that follow entities every step.
    pub attachments: Vec<ClothAttachment>,
    /// Mass per area in kg/m^2.
    pub density: f32,
    pub stretch_compliance: f32,
    pub bend_compliance: f32,
    /// Long-range tether slack factor (>= 1); omitted disables tethers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tether_slack: Option<f32>,
    pub damping: f32,
    /// Collision margin above rigid surfaces in meters.
    pub thickness: f32,
    pub friction: f32,
    pub drag: f32,
    pub lift: f32,
    /// Air velocity in m/s; with `drag` and `lift` it drives the cloth.
    pub wind: [f32; 3],
    pub air_density: f32,
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
impl Default for Cloth {
    fn default() -> Self {
        let params = ClothParams::default();
        Self {
            source: ClothSource::Grid,
            size: [1.; 2],
            resolution: [16; 2],
            positions: vec![],
            triangles: vec![],
            fixed: ClothEdge::None,
            pinned: vec![],
            attachments: vec![],
            density: 0.2,
            stretch_compliance: params.stretch_compliance,
            bend_compliance: params.bend_compliance,
            tether_slack: None,
            damping: params.damping,
            thickness: params.thickness,
            friction: params.friction,
            drag: params.drag,
            lift: params.lift,
            wind: [0.; 3],
            air_density: 1.2,
            gravity_scale: 1.,
            substeps: 10,
            iterations: 1,
            two_way: true,
            coupling_scale: 1.,
            collision_mask: u32::MAX,
        }
    }
}
impl Cloth {
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
            ClothSource::Grid => {
                (self.resolution[0] as usize + 1) * (self.resolution[1] as usize + 1)
            }
            ClothSource::Mesh => self.positions.len(),
        }
    }
    /// Bounds of every authored value; no world access.
    pub fn validate(&self) -> Result<()> {
        const NAME: &str = "engine.Cloth";
        match self.source {
            ClothSource::Grid => {
                check(
                    NAME,
                    &[
                        (
                            self.size.iter().all(|v| (0.001..=1000.).contains(v)),
                            "size (0.001..=1000 m)",
                        ),
                        (
                            self.resolution
                                .iter()
                                .all(|v| (1..=MAX_GRID_SEGMENTS).contains(v)),
                            "resolution (1..=128)",
                        ),
                    ],
                )?;
            }
            ClothSource::Mesh => {
                if !(3..=MAX_CLOTH_VERTICES).contains(&self.positions.len())
                    || !(1..=MAX_CLOTH_TRIANGLES).contains(&self.triangles.len())
                {
                    return bad(
                        NAME,
                        "mesh needs 3..=16384 positions and 1..=32768 triangles",
                    );
                }
                if self
                    .positions
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 1e5)
                    || self
                        .triangles
                        .iter()
                        .flatten()
                        .any(|&i| i as usize >= self.positions.len())
                {
                    return bad(NAME, "mesh positions must be finite and indices in range");
                }
                if self.fixed != ClothEdge::None {
                    return bad(NAME, "fixed edges apply to the grid source only");
                }
            }
        }
        let count = self.particle_count();
        if self.pinned.len() > MAX_PINNED || self.pinned.iter().any(|&p| p as usize >= count) {
            return bad(NAME, "pinned needs at most 4096 in-range particle indices");
        }
        if self.attachments.len() > MAX_ATTACHMENTS {
            return bad(NAME, "at most 64 attachments");
        }
        for attachment in &self.attachments {
            if attachment.entity.is_empty() || attachment.entity.len() > 256 {
                return bad(NAME, "attachment entity must be 1..=256 characters");
            }
            if attachment.particle as usize >= count {
                return bad(NAME, "attachment particle out of range");
            }
            if attachment
                .offset
                .is_some_and(|o| o.iter().any(|v| !v.is_finite() || v.abs() > 1e5))
            {
                return bad(NAME, "attachment offset must be finite");
            }
        }
        let unit = |v: f32, max: f32| v.is_finite() && (0. ..=max).contains(&v);
        check(
            NAME,
            &[
                (
                    self.density.is_finite() && (1e-4..=1e4).contains(&self.density),
                    "density",
                ),
                (unit(self.stretch_compliance, 1e6), "stretch_compliance"),
                (unit(self.bend_compliance, 1e6), "bend_compliance"),
                (
                    self.tether_slack
                        .is_none_or(|s| s.is_finite() && (1. ..=10.).contains(&s)),
                    "tether_slack (1..=10)",
                ),
                (unit(self.damping, 1e3), "damping"),
                (unit(self.thickness, 100.), "thickness"),
                (unit(self.friction, 10.), "friction"),
                (unit(self.drag, 100.), "drag"),
                (unit(self.lift, 100.), "lift"),
                (
                    self.wind.iter().all(|v| v.is_finite() && v.abs() <= 1e3),
                    "wind (finite, at most 1000 m/s)",
                ),
                (unit(self.air_density, 100.), "air_density"),
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

    /// Copy with every live-tunable field reset: a difference in the rest rebuilds the cloth.
    pub(super) fn structure(&self) -> Self {
        let live = Self::default();
        Self {
            stretch_compliance: live.stretch_compliance,
            bend_compliance: live.bend_compliance,
            tether_slack: None,
            damping: live.damping,
            thickness: live.thickness,
            friction: live.friction,
            drag: live.drag,
            lift: live.lift,
            wind: live.wind,
            air_density: live.air_density,
            gravity_scale: live.gravity_scale,
            substeps: live.substeps,
            iterations: live.iterations,
            two_way: live.two_way,
            coupling_scale: live.coupling_scale,
            collision_mask: live.collision_mask,
            ..self.clone()
        }
    }
    pub(super) fn params(&self) -> ClothParams {
        ClothParams {
            stretch_compliance: self.stretch_compliance,
            bend_compliance: self.bend_compliance,
            tether_slack: self.tether_slack,
            damping: self.damping,
            thickness: self.thickness,
            friction: self.friction,
            drag: self.drag,
            lift: self.lift,
        }
    }

    /// World-space description at the entity pose `(scale, rotation, position)`.
    pub(super) fn describe(&self, pose: (Vec3, Quat, Vec3)) -> Result<ClothDesc> {
        let to_world = |p: Vec3| pose.2 + pose.1 * (p * pose.0);
        let convert =
            |e: engine_xpbd::Error| crate::simulation::invalid(format!("engine.Cloth: {e}"));
        let mut desc = match self.source {
            ClothSource::Grid => {
                let [w, d] = self.size;
                let mut desc = ClothDesc::grid(
                    Vec3::new(-w / 2., 0., d / 2.),
                    Vec3::new(w, 0., 0.),
                    Vec3::new(0., 0., -d),
                    self.resolution[0],
                    self.resolution[1],
                )
                .map_err(convert)?;
                desc.pinned = self.fixed_grid_particles();
                desc
            }
            ClothSource::Mesh => ClothDesc {
                positions: self.positions.iter().map(|p| Vec3::from(*p)).collect(),
                triangles: self.triangles.clone(),
                areal_density: self.density,
                inverse_masses: None,
                pinned: vec![],
                params: self.params(),
            },
        };
        for position in &mut desc.positions {
            *position = to_world(*position);
        }
        desc.pinned.extend(&self.pinned);
        desc.pinned.sort_unstable();
        desc.pinned.dedup();
        desc.areal_density = self.density;
        desc.params = self.params();
        Ok(desc)
    }
    fn fixed_grid_particles(&self) -> Vec<u32> {
        let [nu, nv] = self.resolution;
        let index = |i: u32, j: u32| j * (nu + 1) + i;
        match self.fixed {
            ClothEdge::None => vec![],
            // Vertex (i, j) sits at x = -w/2 + i/nu * w and z = +d/2 - j/nv * d.
            ClothEdge::MinX => (0..=nv).map(|j| index(0, j)).collect(),
            ClothEdge::MaxX => (0..=nv).map(|j| index(nu, j)).collect(),
            ClothEdge::MaxZ => (0..=nu).map(|i| index(i, 0)).collect(),
            ClothEdge::MinZ => (0..=nu).map(|i| index(i, nv)).collect(),
            ClothEdge::Corners => vec![index(0, 0), index(nu, 0), index(0, nv), index(nu, nv)],
        }
    }
}

pub(super) fn descriptor() -> crate::ComponentDescriptor {
    let mut properties = json!({
        "source":{"type":"string","enum":["grid","mesh"]},
        "size":{"type":"array","items":{"type":"number","minimum":0.001,"maximum":1000},"minItems":2,"maxItems":2,
            "description":"Grid width (X) and depth (Z) in meters."},
        "resolution":{"type":"array","items":{"type":"integer","minimum":1,"maximum":MAX_GRID_SEGMENTS},"minItems":2,"maxItems":2,
            "description":"Grid segments along X and Z."},
        "positions":{"type":"array","maxItems":MAX_CLOTH_VERTICES,"items":vector(3),
            "description":"Inline mesh vertices in entity-local space."},
        "triangles":{"type":"array","maxItems":MAX_CLOTH_TRIANGLES,"items":{"type":"array","minItems":3,"maxItems":3,
            "items":{"type":"integer","minimum":0}}},
        "fixed":{"type":"string","enum":["none","min_x","max_x","min_z","max_z","corners"],
            "description":"Grid edge held fixed in world space."},
        "pinned":{"type":"array","maxItems":MAX_PINNED,"items":{"type":"integer","minimum":0},
            "description":"Particles held fixed at their creation position."},
        "attachments":{"type":"array","maxItems":MAX_ATTACHMENTS,"items":{"type":"object","additionalProperties":false,
            "required":["particle","entity"],"properties":{
            "particle":{"type":"integer","minimum":0},
            "entity":{"type":"string","minLength":1,"maxLength":256,
                "description":"Authored id of the entity (a skinned bone or joint) the particle follows."},
            "offset":vector(3)}}},
        "density":number(1e-4, 1e4, "Mass per area in kg/m^2."),
        "stretch_compliance":number(0., 1e6, "Edge stretch compliance (inverse stiffness)."),
        "bend_compliance":number(0., 1e6, "Dihedral bending compliance."),
        "tether_slack":number(1., 10., "Long-range tether slack factor; omit to disable tethers."),
        "damping":number(0., 1e3, "Velocity damping rate in 1/s."),
        "thickness":number(0., 100., "Collision margin above rigid surfaces in meters."),
        "friction":number(0., 10., "Friction against rigid bodies."),
        "drag":number(0., 100., "Aerodynamic drag coefficient."),
        "lift":number(0., 100., "Aerodynamic lift coefficient."),
        "wind":vector(3),
        "air_density":number(0., 100., "Air density in kg/m^3 for wind forces."),
        "gravity_scale":number(-100., 100., "Multiplier on scene gravity."),
        "substeps":{"type":"integer","minimum":1,"maximum":64},
        "iterations":{"type":"integer","minimum":1,"maximum":16}
    });
    super::coupling_schema(&mut properties);
    descriptor_for(
        "engine.Cloth",
        Cloth::default(),
        properties,
        (0..MAX_ATTACHMENTS)
            .map(|i| format!("/attachments/{i}/entity"))
            .collect(),
    )
}

pub(super) fn descriptor_for<T: Serialize>(
    name: &str,
    defaults: T,
    properties: Value,
    entity_references: Vec<String>,
) -> crate::ComponentDescriptor {
    crate::ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":properties}),
        defaults: serde_json::to_value(defaults).expect("finite defaults"),
        requires: vec!["Transform".into()],
        entity_references,
        data_only: false,
    }
}
