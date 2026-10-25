//! Authored `engine.FluidVolume`: data, validation, schema and the engine-fluids description.
use super::cloth::descriptor_for;
use super::{Coupling, bad, check, number, vector};
use crate::Result;
use crate::simulation::invalid;
use bevy_ecs::prelude::*;
use engine_fluids::{Domain, EmitterDesc, EmitterKind, FluidParams, Region};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Most particles one authored volume may hold.
pub const MAX_FLUID_CAPACITY: u32 = 131_072;
/// Most initial fill regions, emitters and kill volumes.
pub const MAX_FILLS: usize = 8;
pub const MAX_EMITTERS: usize = 16;
pub const MAX_KILL_VOLUMES: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionShape {
    #[default]
    Box,
    Sphere,
}

/// A box or sphere in entity-local, unrotated space (translated and scaled by the entity).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FluidRegion {
    pub shape: RegionShape,
    pub center: [f32; 3],
    /// Box half extents.
    pub half_extents: [f32; 3],
    /// Sphere radius.
    pub radius: f32,
    /// Initial particle velocity (fills only).
    pub velocity: [f32; 3],
}
impl Default for FluidRegion {
    fn default() -> Self {
        Self {
            shape: RegionShape::Box,
            center: [0.; 3],
            half_extents: [0.25; 3],
            radius: 0.25,
            velocity: [0.; 3],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterShape {
    /// `per_second` particles at pseudo-random places in the box `center`/`half_extents`.
    #[default]
    Rate,
    /// Disc layers leaving `position` along `direction` at `speed`.
    Nozzle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FluidEmitter {
    pub kind: EmitterShape,
    /// Rate: emission box.
    pub center: [f32; 3],
    pub half_extents: [f32; 3],
    pub per_second: f32,
    /// Rate: particle velocity.
    pub velocity: [f32; 3],
    pub seed: u64,
    /// Nozzle: disc centre, axis, radius and exit speed.
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub nozzle_radius: f32,
    pub speed: f32,
    /// Seconds each particle lives; omitted is unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<f32>,
    pub enabled: bool,
}
impl Default for FluidEmitter {
    fn default() -> Self {
        Self {
            kind: EmitterShape::Rate,
            center: [0.; 3],
            half_extents: [0.1; 3],
            per_second: 100.,
            velocity: [0.; 3],
            seed: 1,
            position: [0.; 3],
            direction: [0., -1., 0.],
            nozzle_radius: 0.1,
            speed: 2.,
            lifetime: None,
            enabled: true,
        }
    }
}

/// A particle-based fluid (PBF) in an axis-aligned box. The domain, fills, emitters and kill
/// volumes are entity-local; the entity's translation and scale place them and its rotation is
/// ignored (the domain is axis aligned in world space).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FluidVolume {
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// Wall bounce, 0..=1.
    pub restitution: f32,
    /// Wall friction, 0..=10.
    pub wall_friction: f32,
    pub particle_radius: f32,
    pub kernel_scale: f32,
    pub rest_density: f32,
    pub viscosity: f32,
    pub vorticity: f32,
    pub substeps: u32,
    pub iterations: u32,
    pub relaxation: f32,
    pub tensile_k: f32,
    pub tensile_dq: f32,
    pub max_speed: f32,
    pub surface_threshold: f32,
    pub gravity_scale: f32,
    /// Most particles the volume holds; emitters stop (and report) when it is full.
    pub capacity: u32,
    /// Regions filled with a lattice of particles when the volume is created.
    pub fills: Vec<FluidRegion>,
    pub emitters: Vec<FluidEmitter>,
    /// Particles inside these regions are removed.
    pub kill_volumes: Vec<FluidRegion>,
    /// Reaction impulses on dynamic rigid bodies (buoyancy, drag); off keeps one-way collision.
    pub two_way: bool,
    /// Multiplier on the reaction impulses, 0..=100.
    pub coupling_scale: f32,
    /// Rigid bodies whose `material.layer` shares a bit with this mask collide.
    pub collision_mask: u32,
}
impl Default for FluidVolume {
    fn default() -> Self {
        let params = FluidParams::default();
        let domain = Domain::new(Vec3::ZERO, Vec3::ZERO);
        Self {
            domain_min: [-0.5, 0., -0.5],
            domain_max: [0.5, 1., 0.5],
            restitution: domain.restitution,
            wall_friction: domain.friction,
            particle_radius: params.particle_radius,
            kernel_scale: params.kernel_scale,
            rest_density: params.rest_density,
            viscosity: params.viscosity,
            vorticity: params.vorticity,
            substeps: params.substeps,
            iterations: params.iterations,
            relaxation: params.relaxation,
            tensile_k: params.tensile_k,
            tensile_dq: params.tensile_dq,
            max_speed: params.max_speed,
            surface_threshold: params.surface_threshold,
            gravity_scale: 1.,
            capacity: 4096,
            fills: vec![],
            emitters: vec![],
            kill_volumes: vec![],
            two_way: true,
            coupling_scale: 1.,
            collision_mask: u32::MAX,
        }
    }
}

fn finite3(v: [f32; 3], max: f32) -> bool {
    v.iter().all(|c| c.is_finite() && c.abs() <= max)
}
impl FluidRegion {
    fn validate(&self, name: &str) -> Result<()> {
        let ok = finite3(self.center, 1e6)
            && finite3(self.velocity, 1000.)
            && match self.shape {
                RegionShape::Box => self
                    .half_extents
                    .iter()
                    .all(|v| v.is_finite() && *v > 0. && *v <= 1e6),
                RegionShape::Sphere => {
                    self.radius.is_finite() && self.radius > 0. && self.radius <= 1e6
                }
            };
        if ok {
            Ok(())
        } else {
            bad(
                name,
                "region needs finite center/velocity and positive extents or radius",
            )
        }
    }
    /// The world region for an entity at `position` with `scale`.
    pub(super) fn world(&self, position: Vec3, scale: Vec3) -> Region {
        let center = position + Vec3::from(self.center) * scale;
        match self.shape {
            RegionShape::Box => Region::Box {
                center,
                half_extents: Vec3::from(self.half_extents) * scale,
            },
            RegionShape::Sphere => Region::Sphere {
                center,
                radius: self.radius * scale.x,
            },
        }
    }
}
impl FluidEmitter {
    pub(super) fn world(&self, position: Vec3, scale: Vec3) -> EmitterDesc {
        let kind = match self.kind {
            EmitterShape::Rate => EmitterKind::Rate {
                region: Region::Box {
                    center: position + Vec3::from(self.center) * scale,
                    half_extents: Vec3::from(self.half_extents) * scale,
                },
                per_second: self.per_second,
                velocity: Vec3::from(self.velocity),
                seed: self.seed,
            },
            EmitterShape::Nozzle => EmitterKind::Nozzle {
                position: position + Vec3::from(self.position) * scale,
                direction: Vec3::from(self.direction),
                radius: self.nozzle_radius * scale.x,
                speed: self.speed,
            },
        };
        EmitterDesc {
            kind,
            lifetime: self.lifetime,
            enabled: self.enabled,
        }
    }
}
impl FluidVolume {
    pub(super) fn coupling(&self) -> Coupling {
        Coupling {
            two_way: self.two_way,
            scale: self.coupling_scale,
            mask: self.collision_mask,
        }
    }
    pub(super) fn params(&self, gravity: Vec3) -> FluidParams {
        FluidParams {
            particle_radius: self.particle_radius,
            kernel_scale: self.kernel_scale,
            rest_density: self.rest_density,
            gravity: gravity * self.gravity_scale,
            substeps: self.substeps,
            iterations: self.iterations,
            relaxation: self.relaxation,
            tensile_k: self.tensile_k,
            tensile_dq: self.tensile_dq,
            viscosity: self.viscosity,
            vorticity: self.vorticity,
            max_speed: self.max_speed,
            surface_threshold: self.surface_threshold,
        }
    }
    pub(super) fn domain(&self, position: Vec3, scale: Vec3) -> Domain {
        Domain {
            min: position + Vec3::from(self.domain_min) * scale,
            max: position + Vec3::from(self.domain_max) * scale,
            restitution: self.restitution,
            friction: self.wall_friction,
        }
    }
    /// The part that needs a re-fill when it changes.
    pub(super) fn fills_key(&self) -> &[FluidRegion] {
        &self.fills
    }
    /// Bounds of every authored value; no world access.
    pub fn validate(&self) -> Result<()> {
        const NAME: &str = "engine.FluidVolume";
        let convert = |e: engine_fluids::Error| invalid(format!("{NAME}: {e}"));
        if !finite3(self.domain_min, 1e6) || !finite3(self.domain_max, 1e6) {
            return bad(NAME, "domain bounds must be finite");
        }
        self.params(Vec3::ZERO).validate().map_err(convert)?;
        self.domain(Vec3::ZERO, Vec3::ONE)
            .validate(&self.params(Vec3::ZERO))
            .map_err(convert)?;
        check(
            NAME,
            &[
                (
                    (1..=MAX_FLUID_CAPACITY).contains(&self.capacity),
                    "capacity (1..=131072)",
                ),
                (
                    self.gravity_scale.is_finite() && self.gravity_scale.abs() <= 100.,
                    "gravity_scale",
                ),
                (self.fills.len() <= MAX_FILLS, "at most 8 fills"),
                (self.emitters.len() <= MAX_EMITTERS, "at most 16 emitters"),
                (
                    self.kill_volumes.len() <= MAX_KILL_VOLUMES,
                    "at most 16 kill_volumes",
                ),
            ],
        )?;
        for region in self.fills.iter().chain(&self.kill_volumes) {
            region.validate(NAME)?;
        }
        for emitter in &self.emitters {
            let ok = finite3(emitter.center, 1e6)
                && finite3(emitter.position, 1e6)
                && finite3(emitter.velocity, 1000.)
                && finite3(emitter.direction, 1e6)
                && finite3(emitter.half_extents, 1e6)
                && emitter.per_second.is_finite()
                && emitter.nozzle_radius.is_finite()
                && emitter.speed.is_finite();
            if !ok {
                return bad(NAME, "emitter values must be finite");
            }
            self.emitter_check(emitter).map_err(convert)?;
        }
        self.coupling().validate(NAME)
    }
    /// Emitter bounds that depend on the particle spacing, checked through engine-fluids.
    fn emitter_check(&self, emitter: &FluidEmitter) -> engine_fluids::Result<()> {
        let mut probe = engine_fluids::FluidVolume::new(engine_fluids::VolumeDesc {
            params: self.params(Vec3::ZERO),
            domain: self.domain(Vec3::ZERO, Vec3::ONE),
            capacity: 1,
        })?;
        probe.set_emitter(0, emitter.world(Vec3::ZERO, Vec3::ONE))
    }
}

pub(super) fn descriptor() -> crate::ComponentDescriptor {
    let region = json!({"type":"object","additionalProperties":false,"properties":{
        "shape":{"type":"string","enum":["box","sphere"]},
        "center":vector(3),"half_extents":vector(3),
        "radius":{"type":"number","minimum":0.000001},
        "velocity":vector(3)}});
    let emitter = json!({"type":"object","additionalProperties":false,"properties":{
        "kind":{"type":"string","enum":["rate","nozzle"]},
        "center":vector(3),"half_extents":vector(3),
        "per_second":number(0., 1e7, "Particles per second (rate)."),
        "velocity":vector(3),"seed":{"type":"integer","minimum":0},
        "position":vector(3),"direction":vector(3),
        "nozzle_radius":{"type":"number","minimum":0.000001},
        "speed":number(0., 1000., "Nozzle exit speed in m/s."),
        "lifetime":{"type":"number","minimum":0.000001},
        "enabled":{"type":"boolean"}}});
    let mut properties = json!({
        "domain_min":vector(3),"domain_max":vector(3),
        "restitution":number(0., 1., "Wall bounce."),
        "wall_friction":number(0., 10., "Wall friction."),
        "particle_radius":number(0.001, 10., "Particle radius in meters; rest spacing is twice this."),
        "kernel_scale":number(1.5, 3., "Smoothing radius in rest spacings."),
        "rest_density":number(0.001, 1e5, "Rest density in kg/m^3."),
        "viscosity":number(0., 1., "XSPH viscosity."),
        "vorticity":number(0., 100., "Vorticity confinement."),
        "substeps":{"type":"integer","minimum":1,"maximum":engine_fluids::MAX_SUBSTEPS},
        "iterations":{"type":"integer","minimum":1,"maximum":engine_fluids::MAX_ITERATIONS},
        "relaxation":number(1e-6, 10., "Constraint relaxation."),
        "tensile_k":number(0., 1., "Tensile instability correction."),
        "tensile_dq":number(0.05, 0.5, "Tensile correction reference distance."),
        "max_speed":number(0.001, 1000., "Particle speed clamp in m/s."),
        "surface_threshold":number(0.001, 10., "Colour-field normal length marking surface particles."),
        "gravity_scale":number(-100., 100., "Multiplier on scene gravity."),
        "capacity":{"type":"integer","minimum":1,"maximum":MAX_FLUID_CAPACITY},
        "fills":{"type":"array","maxItems":MAX_FILLS,"items":region.clone()},
        "emitters":{"type":"array","maxItems":MAX_EMITTERS,"items":emitter},
        "kill_volumes":{"type":"array","maxItems":MAX_KILL_VOLUMES,"items":region}
    });
    super::coupling_schema(&mut properties);
    descriptor_for(
        "engine.FluidVolume",
        FluidVolume::default(),
        properties,
        vec![],
    )
}
