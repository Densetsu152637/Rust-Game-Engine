//! Scene-level `engine.PhysicsSettings`: gravity overrides, solver iterations, named layers
//! with a symmetric collision matrix, and named materials.
use super::super::{SimulationConfig, invalid, mapped};
use super::stepping::Interpolation;
use super::types::{BodyMaterial, CombineRule};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::CollisionLayers;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Most named materials per scene.
pub const MAX_MATERIALS: usize = 64;
/// Most disabled layer pairs (every unordered pair of 32 layers, including self pairs).
pub const MAX_DISABLED_PAIRS: usize = 528;

/// A reusable material referenced by `material.preset` on physics bodies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NamedMaterial {
    pub name: String,
    pub density: f32,
    pub friction: f32,
    pub restitution: f32,
    pub friction_combine: CombineRule,
    pub restitution_combine: CombineRule,
}
impl Default for NamedMaterial {
    fn default() -> Self {
        let inline = BodyMaterial::default();
        Self {
            name: String::new(),
            density: inline.density,
            friction: inline.friction,
            restitution: inline.restitution,
            friction_combine: inline.friction_combine,
            restitution_combine: inline.restitution_combine,
        }
    }
}
/// One per scene, on any entity (no Transform needed). Every layer pair collides unless
/// listed in `disabled_pairs`; `[a, a]` disables a layer's self-collision.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhysicsSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_2d: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_3d: Option<[f32; 3]>,
    pub solver_iterations: u32,
    pub layers: Vec<String>,
    pub disabled_pairs: Vec<[String; 2]>,
    pub materials: Vec<NamedMaterial>,
    /// Fixed simulation step in seconds. When set, frame time accumulates and whole steps
    /// of exactly this length run (see [`StepStatus`](super::StepStatus)); unset keeps the
    /// variable substepping of the frame delta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_timestep: Option<f32>,
    /// Most fixed steps per frame; further accumulated time is dropped and reported.
    pub max_substeps: u32,
    /// How dynamic body `Transform`s are written between fixed steps.
    pub interpolation: Interpolation,
    /// Enables [`debug_view`](super::debug_view); the view is data only.
    pub debug: bool,
    /// Include collider AABBs in the debug view.
    pub debug_aabbs: bool,
    /// Cap on colliders, contacts and anchors listed by the debug view.
    pub debug_max_items: u32,
}
/// Most fixed steps per frame accepted by `max_substeps`.
pub const MAX_FIXED_SUBSTEPS: u32 = 64;
/// Fixed step bounds in seconds.
pub const FIXED_TIMESTEP_RANGE: std::ops::RangeInclusive<f32> = 0.001..=0.1;
impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            gravity_2d: None,
            gravity_3d: None,
            solver_iterations: 4,
            layers: vec![],
            disabled_pairs: vec![],
            materials: vec![],
            fixed_timestep: None,
            max_substeps: 8,
            interpolation: Interpolation::None,
            debug: false,
            debug_aabbs: false,
            debug_max_items: 4096,
        }
    }
}

/// Validated settings in engine-physics terms. Feature hooks read it through `Context`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resolved {
    pub(crate) gravity_2d: [f32; 2],
    pub(crate) gravity_3d: [f32; 3],
    pub(crate) solver_iterations: usize,
    pub(crate) layers: CollisionLayers,
    pub(crate) materials: BTreeMap<String, engine_physics::Material>,
    pub(crate) fixed_timestep: Option<f32>,
    pub(crate) max_substeps: usize,
    pub(crate) interpolation: Interpolation,
    pub(crate) debug: bool,
    pub(crate) debug_aabbs: bool,
    pub(crate) debug_max_items: usize,
}
impl Resolved {
    /// True when `other` changes how the simulation runs (everything but the debug toggles
    /// and stepping policy), which wakes bodies and may rebuild them.
    pub(crate) fn simulation_differs(&self, other: &Self) -> bool {
        self.gravity_2d != other.gravity_2d
            || self.gravity_3d != other.gravity_3d
            || self.solver_iterations != other.solver_iterations
            || self.layers != other.layers
            || self.materials != other.materials
    }
    pub(crate) fn new(
        config: &SimulationConfig,
        settings: Option<&PhysicsSettings>,
    ) -> Result<Self> {
        let default = PhysicsSettings::default();
        let settings = settings.unwrap_or(&default);
        let finite = |v: &[f32]| v.iter().all(|n| n.is_finite() && n.abs() <= 1e8);
        if !settings.gravity_2d.is_none_or(|g| finite(&g))
            || !settings.gravity_3d.is_none_or(|g| finite(&g))
        {
            return Err(invalid("PhysicsSettings gravity must be finite"));
        }
        if !(1..=engine_physics::MAX_SOLVER_ITERATIONS as u32).contains(&settings.solver_iterations)
        {
            return Err(invalid("PhysicsSettings solver_iterations must be 1..=64"));
        }
        let mut layers = CollisionLayers::new(settings.layers.iter().cloned()).map_err(mapped)?;
        if settings.disabled_pairs.len() > MAX_DISABLED_PAIRS {
            return Err(invalid("PhysicsSettings lists too many disabled pairs"));
        }
        for [a, b] in &settings.disabled_pairs {
            layers.set_collides(a, b, false).map_err(|_| {
                invalid(format!(
                    "PhysicsSettings disables unknown layer pair {a}/{b}"
                ))
            })?;
        }
        if settings.materials.len() > MAX_MATERIALS {
            return Err(invalid("PhysicsSettings allows at most 64 materials"));
        }
        let mut materials = BTreeMap::new();
        for named in &settings.materials {
            if CollisionLayers::new([named.name.as_str()]).is_err() {
                return Err(invalid(
                    "Physics material names use 1..=64 of [A-Za-z0-9_.-]",
                ));
            }
            let material = engine_physics::Material {
                density: named.density,
                friction: named.friction,
                restitution: named.restitution,
                friction_combine: named.friction_combine.into(),
                restitution_combine: named.restitution_combine.into(),
                ..Default::default()
            };
            material.validate().map_err(mapped)?;
            if materials.insert(named.name.clone(), material).is_some() {
                return Err(invalid(format!(
                    "Duplicate physics material {}",
                    named.name
                )));
            }
        }
        if let Some(step) = settings.fixed_timestep
            && !(step.is_finite() && FIXED_TIMESTEP_RANGE.contains(&step))
        {
            return Err(invalid(
                "PhysicsSettings fixed_timestep must be 0.001..=0.1 seconds",
            ));
        }
        if !(1..=MAX_FIXED_SUBSTEPS).contains(&settings.max_substeps) {
            return Err(invalid("PhysicsSettings max_substeps must be 1..=64"));
        }
        if settings.interpolation != Interpolation::None && settings.fixed_timestep.is_none() {
            return Err(invalid(
                "PhysicsSettings interpolation requires fixed_timestep",
            ));
        }
        if !(1..=engine_physics::dim3::MAX_DEBUG_ITEMS as u32).contains(&settings.debug_max_items) {
            return Err(invalid("PhysicsSettings debug_max_items must be 1..=65536"));
        }
        Ok(Self {
            fixed_timestep: settings.fixed_timestep,
            max_substeps: settings.max_substeps as usize,
            interpolation: settings.interpolation,
            debug: settings.debug,
            debug_aabbs: settings.debug_aabbs,
            debug_max_items: settings.debug_max_items as usize,
            gravity_2d: settings.gravity_2d.unwrap_or(config.gravity_2d),
            gravity_3d: settings.gravity_3d.unwrap_or(config.gravity_3d),
            solver_iterations: settings.solver_iterations as usize,
            layers,
            materials,
        })
    }
    /// Engine material for an authored body, applying `preset` and `layer_name`.
    pub(crate) fn material(&self, authored: &BodyMaterial) -> Result<engine_physics::Material> {
        let mut material = engine_physics::Material::from(authored);
        if let Some(name) = &authored.preset {
            let preset = self
                .materials
                .get(name)
                .ok_or_else(|| invalid(format!("Unknown physics material preset {name}")))?;
            material = engine_physics::Material {
                sensor: material.sensor,
                layer: material.layer,
                mask: material.mask,
                ..*preset
            };
        }
        if let Some(name) = &authored.layer_name {
            material = self
                .layers
                .apply(material, name)
                .map_err(|_| invalid(format!("Unknown physics layer {name}")))?;
        }
        Ok(material)
    }
}
/// The single settings component in `world`, rejecting duplicates.
pub(crate) fn find(world: &World) -> Result<Option<&PhysicsSettings>> {
    let mut found = None;
    for entity in world.iter_entities() {
        if let Some(settings) = entity.get::<PhysicsSettings>() {
            if found.is_some() {
                return Err(invalid("At most one engine.PhysicsSettings per scene"));
            }
            found = Some(settings);
        }
    }
    Ok(found)
}
