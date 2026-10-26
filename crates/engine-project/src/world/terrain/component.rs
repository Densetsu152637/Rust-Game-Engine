//! The authored `engine.Terrain` component, its schema and the runtime marker components.
use crate::{ComponentDescriptor, Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_terrain::{LodSettings, NodeKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const COMPONENT_NAME: &str = "engine.Terrain";

/// How a terrain reaches the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerrainRenderMode {
    /// GPU feature when a terrain GPU renderer is installed ([`super::TerrainGpuRenderer`]),
    /// else the CPU chunk-mesh fallback.
    #[default]
    Auto,
    /// Always the CPU chunk-mesh fallback; the GPU feature skips the terrain.
    Cpu,
    /// Always the GPU feature; nothing draws without a GPU renderer (reported in the status).
    Gpu,
}

/// Per-layer overrides of the asset's layer definitions (index-aligned with the asset layers).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainMaterial {
    /// Texture asset id replacing the layer's texture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture: Option<String>,
    /// Linear RGBA tint replacing the layer's tint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 4]>,
    /// World units per texture repeat replacing the layer's tiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_tiling: Option<f32>,
}

/// Generated physics collision of a terrain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainCollision {
    /// Spawn fixed collider tiles.
    pub enabled: bool,
    /// Collision membership bits.
    pub layer: u32,
    /// Collision filter bits.
    pub mask: u32,
    /// Tile edge in cells (8..=255): one fixed heightfield collider per tile (hole cells are
    /// removed from the heightfield, not meshed).
    pub tile_cells: u32,
    pub friction: f32,
    pub restitution: f32,
}
impl Default for TerrainCollision {
    fn default() -> Self {
        Self {
            enabled: true,
            layer: 1,
            mask: u32::MAX,
            tile_cells: 128,
            friction: 0.8,
            restitution: 0.0,
        }
    }
}

/// Authored `engine.Terrain`: a heightmap terrain from a `terrain` asset.
///
/// The terrain's local origin is sample (0, 0) at height 0; columns run along local +X and rows
/// along +Z (see `engine_terrain::heightfield`). The entity's `Transform` places it; keep it
/// upright (rotation about Y only) for PCG surface queries.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerrainComponent {
    /// Id of a project asset of kind `terrain`.
    pub asset: String,
    /// Quadtree level of detail.
    pub lod: LodSettings,
    /// CPU fallback only: viewer movement (terrain-local units) before the chunk selection and
    /// morphs are recomputed; 0 recomputes whenever the viewer moved by a sample spacing.
    pub update_distance: f32,
    /// Per-layer overrides, at most 8.
    pub materials: Vec<TerrainMaterial>,
    pub collision: TerrainCollision,
    pub render_mode: TerrainRenderMode,
    /// Whether the terrain draws (collision and queries are unaffected).
    pub visible: bool,
    /// Name under which the terrain is registered in `PcgSurfaceRegistry`; empty disables it.
    pub surface: String,
}
impl Default for TerrainComponent {
    fn default() -> Self {
        Self {
            asset: String::new(),
            lod: LodSettings::default(),
            update_distance: 0.0,
            materials: Vec::new(),
            collision: TerrainCollision::default(),
            render_mode: TerrainRenderMode::Auto,
            visible: true,
            surface: "terrain".into(),
        }
    }
}
impl TerrainComponent {
    /// Validates every field (asset existence is checked when the terrain loads).
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::new("terrain.component.invalid", m.to_string()));
        if self.asset.is_empty() || self.asset.len() > 128 {
            return bad("engine.Terrain requires a terrain asset id of 1..=128 bytes");
        }
        self.lod
            .validate()
            .map_err(|e| Error::new(e.code, e.message))?;
        if !(self.update_distance.is_finite() && (0.0..=10_000.0).contains(&self.update_distance)) {
            return bad("update_distance must be within 0..=10000");
        }
        if self.materials.len() > engine_terrain::MAX_LAYERS {
            return bad("at most 8 layer materials");
        }
        for m in &self.materials {
            if m.texture
                .as_ref()
                .is_some_and(|t| t.is_empty() || t.len() > 128)
            {
                return bad("material texture ids need 1..=128 bytes");
            }
            if m.tint
                .is_some_and(|t| !t.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
            {
                return bad("material tint components must be within 0..=1");
            }
            if m.uv_tiling
                .is_some_and(|v| !(v.is_finite() && v > 0.0 && v <= 100_000.0))
            {
                return bad("material uv_tiling must be within (0, 100000]");
            }
        }
        let c = &self.collision;
        if !(engine_terrain::collision::MIN_TILE_CELLS..=engine_terrain::collision::MAX_TILE_CELLS)
            .contains(&c.tile_cells)
        {
            return bad("collision.tile_cells must be within 8..=255");
        }
        if c.enabled && (c.layer == 0 || c.mask == 0) {
            return bad("collision layer and mask must be non-zero");
        }
        if !(c.friction.is_finite() && (0.0..=100.0).contains(&c.friction))
            || !(c.restitution.is_finite() && (0.0..=1.0).contains(&c.restitution))
        {
            return bad("collision friction must be within 0..=100 and restitution 0..=1");
        }
        if self.surface.len() > 128 {
            return bad("surface names are at most 128 bytes");
        }
        Ok(())
    }
}

/// Marks a CPU fallback chunk entity: one selected LOD node.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainChunk {
    /// The entity carrying `engine.Terrain`.
    pub owner: Entity,
    pub node: NodeKey,
}
/// Marks a generated collider tile entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainColliderTile {
    pub owner: Entity,
    /// Tile coordinate.
    pub tile: [u32; 2],
}
/// Marks every entity the terrain service generated.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainGenerated {
    pub owner: Entity,
}

fn vector(n: usize, min: f64, max: f64) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":min,"maximum":max},"minItems":n,"maxItems":n})
}

pub(super) fn descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: COMPONENT_NAME.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "asset":{"type":"string","maxLength":128,"description":"terrain asset id (required)"},
            "lod":{"type":"object","additionalProperties":false,"properties":{
                "node_resolution":{"type":"integer","enum":[4,8,16,32,64,128],
                    "description":"cells per LOD node side"},
                "levels":{"type":"integer","minimum":0,"maximum":12,
                    "description":"quadtree levels; 0 = enough for one root"},
                "lod_distance":{"type":"number","minimum":3,"maximum":1000,
                    "description":"level-0 range in leaf node sizes"},
                "morph_fraction":{"type":"number","minimum":0.0001,"maximum":1},
                "max_nodes":{"type":"integer","minimum":1,"maximum":4096}
            }},
            "update_distance":{"type":"number","minimum":0,"maximum":10000,
                "description":"CPU fallback: viewer movement before reselection; 0 = one spacing"},
            "materials":{"type":"array","maxItems":8,"items":{"type":"object","additionalProperties":false,
                "properties":{
                    "texture":{"type":"string","minLength":1,"maxLength":128},
                    "tint":vector(4,0.0,1.0),
                    "uv_tiling":{"type":"number","minimum":0.0001,"maximum":100000}
                }}},
            "collision":{"type":"object","additionalProperties":false,"properties":{
                "enabled":{"type":"boolean"},
                "layer":{"type":"integer","minimum":0,"maximum":4294967295u64},
                "mask":{"type":"integer","minimum":0,"maximum":4294967295u64},
                "tile_cells":{"type":"integer","minimum":8,"maximum":255},
                "friction":{"type":"number","minimum":0,"maximum":100},
                "restitution":{"type":"number","minimum":0,"maximum":1}
            }},
            "render_mode":{"type":"string","enum":["auto","cpu","gpu"]},
            "visible":{"type":"boolean"},
            "surface":{"type":"string","maxLength":128,
                "description":"PcgSurfaceRegistry name; empty disables registration"}
        }}),
        defaults: serde_json::to_value(TerrainComponent::default())
            .expect("finite builtin defaults"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}

/// Scene validation: terrains must be well formed and reference an existing `terrain` asset.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(COMPONENT_NAME) else {
            continue;
        };
        let terrain: TerrainComponent =
            serde_json::from_value(value.data.clone()).map_err(|e| {
                Error::new(
                    "terrain.component.invalid",
                    format!("entity {}: {e}", entity.id),
                )
            })?;
        terrain.validate()?;
        project.require_asset(&terrain.asset, "terrain")?;
    }
    Ok(())
}
