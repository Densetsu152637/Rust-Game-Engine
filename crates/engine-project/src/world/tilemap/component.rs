//! The authored `engine.Tilemap` component and its schema.
use crate::ComponentDescriptor;
use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Collision generation settings of a [`TilemapComponent`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TilemapCollision {
    /// Generate colliders from solid tiles.
    pub enabled: bool,
    /// Collision layer bits given to every generated collider.
    pub layer: u32,
    /// Collision mask given to every generated collider.
    pub mask: u32,
    /// Generate trigger volumes instead of solid colliders.
    pub trigger: bool,
    /// Only these tile layers collide; empty means every layer.
    pub layers: Vec<String>,
}
impl Default for TilemapCollision {
    fn default() -> Self {
        Self {
            enabled: true,
            layer: 1,
            mask: u32::MAX,
            trigger: false,
            layers: Vec::new(),
        }
    }
}

/// Authored `engine.Tilemap`: which tileset and map to show, cell size and collision.
///
/// The map comes from the `map` tilemap asset, else from `inline` (a tilemap document, subject
/// to the 64 KiB component limit), else it starts empty with the default bounds.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TilemapComponent {
    /// `tileset` asset id.
    pub tileset: String,
    /// `tilemap` asset id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    /// Inline tilemap document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<Value>,
    /// Size of one cell in world units.
    pub cell_size: [f32; 2],
    /// Multiplied into every vertex colour (including layer opacity).
    pub tint: [f32; 4],
    /// Whether the tiles draw.
    pub visible: bool,
    /// Base draw layer; each tile layer draws at `render_layer + z_order`.
    pub render_layer: i32,
    /// Collision generation.
    pub collision: TilemapCollision,
}
impl Default for TilemapComponent {
    fn default() -> Self {
        Self {
            tileset: String::new(),
            map: None,
            inline: None,
            cell_size: [1.0; 2],
            tint: [1.0; 4],
            visible: true,
            render_layer: 0,
            collision: TilemapCollision::default(),
        }
    }
}
impl TilemapComponent {
    pub(super) fn validate(&self) -> crate::Result<()> {
        let bad = |m: &str| crate::Error::new("tilemap.component.invalid", m.to_string());
        if self.tileset.is_empty() {
            return Err(bad("engine.Tilemap requires a tileset asset id"));
        }
        if !self
            .cell_size
            .iter()
            .all(|v| v.is_finite() && *v > 0.0 && *v <= 10_000.0)
        {
            return Err(bad("cell_size must be finite and within (0, 10000]"));
        }
        if !self
            .tint
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        {
            return Err(bad("tint components must be within 0..=1"));
        }
        if self.map.is_some() && self.inline.is_some() {
            return Err(bad("use either map or inline, not both"));
        }
        if self.collision.layers.len() > 32 || self.collision.layers.iter().any(|l| l.len() > 64) {
            return Err(bad("at most 32 collision layer names of 64 characters"));
        }
        if self.collision.enabled && (self.collision.layer == 0 || self.collision.mask == 0) {
            return Err(bad("collision layer and mask must be non-zero"));
        }
        Ok(())
    }
}

/// Marks a generated render entity: one chunk of one tile layer.
#[derive(Component, Clone, Debug)]
pub struct TileChunkRender {
    /// The entity carrying `engine.Tilemap`.
    pub owner: Entity,
    /// Tile layer name.
    pub layer: String,
    /// Chunk coordinate.
    pub chunk: engine_tilemap::ChunkKey,
    /// Draw layer used by [`crate::runtime::Draw`] ordering.
    pub draw_layer: i32,
}
/// Marks every entity the tilemap service generated (render chunks and colliders).
#[derive(Component, Clone, Copy, Debug)]
pub struct TilemapGenerated {
    /// The entity carrying `engine.Tilemap`.
    pub owner: Entity,
}

fn vector(n: usize, min: f64, max: f64) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":min,"maximum":max},"minItems":n,"maxItems":n})
}

/// Component descriptors contributed by the tilemap feature.
pub fn component_descriptors() -> Vec<ComponentDescriptor> {
    vec![ComponentDescriptor {
        name: "engine.Tilemap".into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "tileset":{"type":"string","maxLength":128,"description":"tileset asset id"},
            "map":{"type":"string","maxLength":128,"description":"tilemap asset id"},
            "inline":{"type":"object","description":"inline tilemap document (version 1); alternative to map"},
            "cell_size":vector(2,0.0001,10000.0),
            "tint":vector(4,0.0,1.0),
            "visible":{"type":"boolean"},
            "render_layer":{"type":"integer","minimum":-1000000,"maximum":1000000},
            "collision":{"type":"object","additionalProperties":false,"properties":{
                "enabled":{"type":"boolean"},
                "layer":{"type":"integer","minimum":0,"maximum":4294967295u64},
                "mask":{"type":"integer","minimum":0,"maximum":4294967295u64},
                "trigger":{"type":"boolean"},
                "layers":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":64}}
            }}
        }}),
        defaults: serde_json::to_value(TilemapComponent::default())
            .expect("finite builtin defaults"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }]
}
