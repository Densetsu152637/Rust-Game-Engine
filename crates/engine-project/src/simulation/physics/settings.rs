//! Scene-level `engine.PhysicsSettings`: gravity overrides, solver iterations, named layers
//! with a symmetric collision matrix, and named materials.
use super::super::{SimulationConfig, invalid, mapped};
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
}
impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            gravity_2d: None,
            gravity_3d: None,
            solver_iterations: 4,
            layers: vec![],
            disabled_pairs: vec![],
            materials: vec![],
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
}
impl Resolved {
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
        Ok(Self {
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
