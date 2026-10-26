//! The authored `engine.Foliage` component, its schema and the runtime marker components.
use crate::{ComponentDescriptor, Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_foliage::{ExclusionZone, FoliageType, WindParams, validate_types};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const COMPONENT_NAME: &str = "engine.Foliage";
/// Largest extent of the placement box per axis.
const MAX_EXTENT: f32 = 1.0e5;

/// Where the instances come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoliageMode {
    /// Scatter `types` over the ground inside the box; regenerate when an input changes.
    #[default]
    Generated,
    /// Load the instances of a `foliage` asset (`instances`); the ground only re-projects
    /// heights.
    Asset,
}

/// How foliage reaches the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoliageRenderMode {
    /// The GPU feature when a foliage GPU renderer is installed ([`super::FoliageGpuRenderer`]),
    /// else the CPU merged-cluster fallback.
    #[default]
    Auto,
    /// Always the CPU fallback; the GPU feature skips the foliage.
    Cpu,
    /// Always the GPU feature; nothing draws without a renderer (reported in the status).
    Gpu,
}

/// Distance culling and level-of-detail tuning.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FoliageCulling {
    /// Cull distance of types without their own `cull_distance`.
    pub max_distance: f32,
    /// Multiplies every LOD, impostor and cull distance (a quality bias).
    pub lod_scale: f32,
    /// Fraction (0..=0.5) by which switching distances are widened when moving away and
    /// narrowed when moving closer, so an instance near a band edge does not flicker.
    pub hysteresis: f32,
}
impl Default for FoliageCulling {
    fn default() -> Self {
        Self {
            max_distance: 200.0,
            lod_scale: 1.0,
            hysteresis: 0.1,
        }
    }
}

/// Authored `engine.Foliage`: vegetation placed over terrain or a PCG surface.
///
/// Instances live in world space, like PCG output. The placement box is `center` / `size`
/// relative to the entity's world position at generation time (rotation and scale are
/// ignored); the entity does not need to stay still afterwards.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FoliageComponent {
    /// Foliage types. In `asset` mode entries replace the asset's type of the same name.
    pub types: Vec<FoliageType>,
    /// Authored id of an `engine.Terrain` entity to grow on. Wins over `surface`; honours
    /// terrain holes and follows terrain edits.
    pub terrain: String,
    /// `PcgSurfaceRegistry` name to grow on when `terrain` is empty; empty = flat ground at
    /// y = 0. Holes of terrains are not visible through a registry surface.
    pub surface: String,
    pub seed: u64,
    pub mode: FoliageMode,
    /// Id of a `foliage` asset (`asset` mode).
    pub instances: String,
    /// Placement box centre relative to the entity's world position.
    pub center: [f32; 3],
    /// Placement box size in metres; `y` sets the altitude range (`bounds.min.y` is the floor
    /// that types' `altitude` limits are relative to).
    pub size: [f32; 3],
    pub exclusion_zones: Vec<ExclusionZone>,
    /// Storage and culling cell edge in metres (1..=4096).
    pub cell_size: f32,
    /// Instance cap (1..=262144); exceeding it fails the generation, it never truncates.
    pub max_instances: u32,
    /// Bump to regenerate with otherwise unchanged values.
    pub regenerate: u32,
    pub culling: FoliageCulling,
    pub wind: WindParams,
    pub render_mode: FoliageRenderMode,
    /// CPU fallback: most vertices of all merged cluster meshes (1000..=4000000); farther
    /// clusters are dropped first.
    pub cpu_vertex_budget: u32,
    /// Whether the foliage draws (generation and queries are unaffected).
    pub visible: bool,
}

impl Default for FoliageComponent {
    fn default() -> Self {
        Self {
            types: Vec::new(),
            terrain: String::new(),
            surface: String::new(),
            seed: 0,
            mode: FoliageMode::Generated,
            instances: String::new(),
            center: [0.0, 128.0, 0.0],
            size: [64.0, 256.0, 64.0],
            exclusion_zones: Vec::new(),
            cell_size: 32.0,
            max_instances: 65_536,
            regenerate: 0,
            culling: FoliageCulling::default(),
            wind: WindParams::default(),
            render_mode: FoliageRenderMode::Auto,
            cpu_vertex_budget: 400_000,
            visible: true,
        }
    }
}

impl FoliageComponent {
    /// Validates every field (assets are checked when the foliage loads).
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("foliage.component.invalid", m.to_string()));
        validate_types(&self.types).map_err(|e| Error::new(e.code, e.message))?;
        if self.terrain.len() > 128 || self.surface.len() > 128 || self.instances.len() > 128 {
            return bad("terrain, surface and instances are at most 128 bytes");
        }
        if self.mode == FoliageMode::Asset && self.instances.is_empty() {
            return bad("mode asset needs an instances asset id");
        }
        if self
            .center
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1.0e6)
        {
            return bad("center must be finite and within 1e6");
        }
        if self.size.iter().any(|v| !(*v > 0.0 && *v <= MAX_EXTENT)) {
            return bad("size components must be within (0, 100000]");
        }
        if self.exclusion_zones.len() > 64 {
            return bad("at most 64 exclusion zones");
        }
        for zone in &self.exclusion_zones {
            zone.validate().map_err(|e| Error::new(e.code, e.message))?;
        }
        if !(self.cell_size.is_finite() && (1.0..=4096.0).contains(&self.cell_size)) {
            return bad("cell_size must be within 1..=4096");
        }
        if self.max_instances == 0 || self.max_instances as usize > engine_foliage::MAX_INSTANCES {
            return bad("max_instances must be within 1..=262144");
        }
        let c = &self.culling;
        if !(c.max_distance.is_finite() && c.max_distance > 0.0 && c.max_distance <= 1.0e6)
            || !(c.lod_scale.is_finite() && c.lod_scale > 0.0 && c.lod_scale <= 100.0)
            || !(c.hysteresis.is_finite() && (0.0..=0.5).contains(&c.hysteresis))
        {
            return bad(
                "culling needs max_distance within (0, 1e6], lod_scale within (0, 100] and hysteresis within 0..=0.5",
            );
        }
        self.wind
            .validate()
            .map_err(|e| Error::new(e.code, e.message))?;
        if !(1000..=4_000_000).contains(&self.cpu_vertex_budget) {
            return bad("cpu_vertex_budget must be within 1000..=4000000");
        }
        Ok(())
    }

    /// The fields that decide which instances exist (everything except render tuning).
    pub(super) fn generation_key(&self) -> GenerationKey {
        GenerationKey {
            types: self.types.clone(),
            terrain: self.terrain.clone(),
            surface: self.surface.clone(),
            seed: self.seed,
            mode: self.mode,
            instances: self.instances.clone(),
            center: self.center,
            size: self.size,
            zones: self.exclusion_zones.clone(),
            cell_size: self.cell_size,
            max_instances: self.max_instances,
            regenerate: self.regenerate,
        }
    }
}

/// See [`FoliageComponent::generation_key`].
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GenerationKey {
    types: Vec<FoliageType>,
    terrain: String,
    surface: String,
    seed: u64,
    mode: FoliageMode,
    instances: String,
    center: [f32; 3],
    size: [f32; 3],
    zones: Vec<ExclusionZone>,
    cell_size: f32,
    max_instances: u32,
    regenerate: u32,
}

/// Marks a CPU fallback cluster entity: the instances of one type in one cell at one LOD.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoliageCluster {
    /// The entity carrying `engine.Foliage`.
    pub owner: Entity,
    pub cell: engine_foliage::CellKey,
    pub type_index: u16,
}
/// Marks every entity the foliage service generated.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoliageGenerated {
    pub owner: Entity,
}

fn vector(n: usize, min: f64, max: f64) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":min,"maximum":max},"minItems":n,"maxItems":n})
}
fn number(min: f64, max: f64, description: &str) -> Value {
    json!({"type":"number","minimum":min,"maximum":max,"description":description})
}
fn string(max: u64) -> Value {
    json!({"type":"string","maxLength":max})
}
fn id_list(max_items: u64) -> Value {
    json!({"type":"array","maxItems":max_items,"items":{"type":"string","minLength":1,"maxLength":128}})
}

fn type_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "name": {"type":"string","minLength":1,"maxLength":64,"description":"unique, [A-Za-z0-9_.-]"},
        "mesh": {"type":"string","minLength":1,"maxLength":128,"description":"mesh asset id (LOD 0)"},
        "lod_meshes": id_list(4),
        "texture": {"type":"string","minLength":1,"maxLength":128},
        "tint": vector(4, 0.0, 8.0),
        "impostor": {"type":"object","additionalProperties":false,"properties":{
            "texture": {"type":"string","minLength":1,"maxLength":128},
            "size": vector(2, 0.0001, 1000.0)
        },"required":["texture","size"]},
        "density": number(0.0, 64.0, "instances per square metre"),
        "density_map": {"type":"object","additionalProperties":false,"properties":{
            "kind": {"type":"string","enum":["uniform","layer","grid","texture"]},
            "layer": {"type":"string","minLength":1,"maxLength":128},
            "invert": {"type":"boolean"},
            "threshold": number(0.0, 0.9999, "weights at or below count as 0"),
            "origin": vector(2, -1.0e6, 1.0e6),
            "cell_size": number(0.0001, 100000.0, "grid cell edge"),
            "columns": {"type":"integer","minimum":1,"maximum":1024},
            "rows": {"type":"integer","minimum":1,"maximum":1024},
            "values": {"type":"array","maxItems":1048576,"items":{"type":"number","minimum":0.0,"maximum":1.0}},
            "asset": {"type":"string","minLength":1,"maxLength":128},
            "size": vector(2, 0.0001, 100000.0)
        },"required":["kind"]},
        "scale": vector(2, 0.001, 1000.0),
        "yaw_degrees": vector(2, -3600.0, 3600.0),
        "tilt_degrees": number(0.0, 45.0, "largest random lean"),
        "slope_degrees": vector(2, 0.0, 90.0),
        "height": vector(2, -100000.0, 100000.0),
        "altitude": vector(2, -200000.0, 200000.0),
        "align_to_normal": number(0.0, 1.0, "0 upright, 1 aligned to the ground normal"),
        "ground_offset": number(-1000.0, 1000.0, "vertical offset from the ground"),
        "min_spacing": number(0.0, 1000.0, "minimum horizontal distance between instances of the type"),
        "excludes": {"type":"array","maxItems":32,"items":{"type":"object","additionalProperties":false,
            "properties":{"name":{"type":"string","minLength":1,"maxLength":128},
                "radius":number(0.0001, 1000.0, "metres")},"required":["name","radius"]}},
        "lod_distances": {"type":"array","maxItems":4,"items":{"type":"number","minimum":0.0001,"maximum":1.0e6}},
        "impostor_distance": number(0.0001, 1.0e6, "impostor replaces the last mesh beyond this"),
        "cull_distance": number(0.0001, 1.0e6, "defaults to culling.max_distance"),
        "wind_weight": number(0.0, 1.0, "0 rigid, 1 fully bent by wind")
    }})
}

fn zone_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "shape": {"type":"string","enum":["circle","rect"]},
        "center": vector(2, -1.0e6, 1.0e6),
        "radius": number(0.0001, 1.0e6, "circle radius"),
        "min": vector(2, -1.0e6, 1.0e6),
        "max": vector(2, -1.0e6, 1.0e6),
        "types": id_list(32)
    },"required":["shape"]})
}

pub(super) fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: COMPONENT_NAME.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "types": {"type":"array","maxItems":32,"items":type_schema()},
            "terrain": {"type":"string","maxLength":128,
                "description":"authored id of an engine.Terrain entity; wins over surface"},
            "surface": string(128),
            "seed": {"type":"integer","minimum":0,"maximum":9_007_199_254_740_991_u64},
            "mode": {"type":"string","enum":["generated","asset"]},
            "instances": {"type":"string","maxLength":128,"description":"foliage asset id (asset mode)"},
            "center": vector(3, -1.0e6, 1.0e6),
            "size": vector(3, 0.0001, MAX_EXTENT as f64),
            "exclusion_zones": {"type":"array","maxItems":64,"items":zone_schema()},
            "cell_size": number(1.0, 4096.0, "storage and culling cell edge"),
            "max_instances": {"type":"integer","minimum":1,"maximum":262_144},
            "regenerate": {"type":"integer","minimum":0,"maximum":4_294_967_295_u64},
            "culling": {"type":"object","additionalProperties":false,"properties":{
                "max_distance": number(0.0001, 1.0e6, "default cull distance"),
                "lod_scale": number(0.0001, 100.0, "multiplies all distances"),
                "hysteresis": number(0.0, 0.5, "LOD switch dead band fraction")
            }},
            "wind": {"type":"object","additionalProperties":false,"properties":{
                "direction_degrees": number(-3600.0, 3600.0, "0 = +X, 90 = +Z"),
                "strength": number(0.0, 100.0, "lean in metres at the top; 0 = no wind"),
                "sway_frequency": number(0.0, 20.0, "Hz"),
                "gust_frequency": number(0.0, 5.0, "Hz"),
                "gust_strength": number(0.0, 1.0, "gust modulation"),
                "flutter": number(0.0, 1.0, "sideways flutter"),
                "wavelength": number(1.0, 1000.0, "metres per phase wave")
            }},
            "render_mode": {"type":"string","enum":["auto","cpu","gpu"]},
            "cpu_vertex_budget": {"type":"integer","minimum":1000,"maximum":4_000_000},
            "visible": {"type":"boolean"}
        }}),
        defaults: serde_json::to_value(FoliageComponent::default()).expect("finite defaults"),
        requires: vec!["Transform".into()],
        // Prefab instancing remaps the terrain id; an empty string means none (surface mode).
        entity_references: vec!["/terrain".into()],
        data_only: false,
    }
}

/// Scene validation: components must be well formed and reference existing assets.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(COMPONENT_NAME) else {
            continue;
        };
        let foliage: FoliageComponent =
            serde_json::from_value(value.data.clone()).map_err(|e| {
                Error::new(
                    "foliage.component.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
        foliage.validate()?;
        if foliage.mode == FoliageMode::Asset {
            project.require_asset(&foliage.instances, "foliage")?;
        }
        for t in &foliage.types {
            project.require_asset(&t.mesh, "mesh")?;
            for m in &t.lod_meshes {
                project.require_asset(m, "mesh")?;
            }
            if let Some(texture) = &t.texture {
                project.require_asset(texture, "texture")?;
            }
            if let Some(i) = &t.impostor {
                project.require_asset(&i.texture, "texture")?;
            }
            if let engine_foliage::DensityMap::Texture { asset, .. } = &t.density_map {
                project.require_asset(asset, "texture")?;
            }
        }
    }
    Ok(())
}
