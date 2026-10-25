//! Authored components of level streaming: `engine.WorldPartition`, `engine.StreamingSource`
//! and `engine.DataLayer`.
use crate::{ComponentDescriptor, Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_streaming::PartitionManifest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

pub const PARTITION_NAME: &str = "engine.WorldPartition";
pub const SOURCE_NAME: &str = "engine.StreamingSource";
pub const LAYER_NAME: &str = "engine.DataLayer";
/// Asset kind of a partition manifest.
pub const MANIFEST_KIND: &str = "partition";
/// Largest manifest document the project loader reads.
pub(super) const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RADIUS: f64 = 1.0e9;
const MAX_LAYERS: usize = 64;
const MAX_NAME: usize = 64;

fn yes() -> bool {
    true
}

/// A named data layer and whether its content is streamed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataLayerSetting {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// A streamed world: a cell grid whose content comes from a partition manifest.
///
/// Cells within `load_radius` of a streaming source load; they unload again only beyond
/// `unload_radius` (the band in between is hysteresis). Layers not listed are enabled.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorldPartition {
    /// Id of a project asset of kind `partition` (see the `streaming_partition` MCP tool).
    pub manifest: String,
    /// Metres from a source (to the nearest point of a cell) within which cells load.
    pub load_radius: f64,
    /// Metres beyond which a loaded cell unloads; at least `load_radius`.
    pub unload_radius: f64,
    /// Most cells that start loading, and most that are applied to the world, per tick.
    pub loads_per_tick: u32,
    /// Byte budget (cell file sizes) per tick; `0` is unlimited. One cell is always admitted.
    pub bytes_per_tick: u64,
    /// Most cells being read in the background at once.
    pub max_in_flight: u32,
    /// Memory budget: most cells loading, loaded or unloading at once.
    pub max_loaded_cells: u32,
    /// Use every `Camera3D` entity as a priority-0 source.
    pub camera_source: bool,
    /// Read and parse cell files on the calling thread within the tick (deterministic; for
    /// tests and tools). Off: a background thread does the I/O.
    pub synchronous: bool,
    /// Show the cell's HLOD proxy while the cell is not loaded.
    pub hlod: bool,
    /// Only show proxies of cells within this many metres of a source; `0` is unlimited.
    pub hlod_radius: f64,
    pub layers: Vec<DataLayerSetting>,
}

impl Default for WorldPartition {
    fn default() -> Self {
        Self {
            manifest: String::new(),
            load_radius: 200.0,
            unload_radius: 250.0,
            loads_per_tick: 4,
            bytes_per_tick: 0,
            max_in_flight: 4,
            max_loaded_cells: 64,
            camera_source: true,
            synchronous: false,
            hlod: true,
            hlod_radius: 0.0,
            layers: Vec::new(),
        }
    }
}

impl WorldPartition {
    /// Checks what the JSON schema cannot: radii ordering, finiteness and unique layer names.
    pub fn check(&self) -> Result<()> {
        let bad = |m: &str| Error::new("streaming.partition.invalid", m.to_owned());
        let radius = |v: f64| v.is_finite() && v > 0.0 && v <= MAX_RADIUS;
        if !radius(self.load_radius) || !radius(self.unload_radius) {
            return Err(bad("load_radius and unload_radius must be in (0, 1e9]"));
        }
        if self.unload_radius < self.load_radius {
            return Err(bad("unload_radius must be at least load_radius"));
        }
        if !(self.hlod_radius.is_finite() && (0.0..=MAX_RADIUS).contains(&self.hlod_radius)) {
            return Err(bad("hlod_radius must be in [0, 1e9]"));
        }
        if !(1..=1024).contains(&self.loads_per_tick) {
            return Err(bad("loads_per_tick must be in 1..=1024"));
        }
        if !(1..=64).contains(&self.max_in_flight) {
            return Err(bad("max_in_flight must be in 1..=64"));
        }
        if !(1..=65_536).contains(&self.max_loaded_cells) {
            return Err(bad("max_loaded_cells must be in 1..=65536"));
        }
        if self.layers.len() > MAX_LAYERS {
            return Err(bad("at most 64 layers"));
        }
        let mut names = BTreeSet::new();
        for layer in &self.layers {
            if layer.name.is_empty()
                || layer.name.len() > MAX_NAME
                || layer.name.chars().any(char::is_control)
                || !names.insert(layer.name.as_str())
            {
                return Err(bad(
                    "layer names must be unique, 1-64 characters, no control characters",
                ));
            }
        }
        Ok(())
    }
    /// Whether `layer` is enabled (unlisted layers and the default layer `""` are).
    pub fn layer_enabled(&self, layer: &str) -> bool {
        layer.is_empty()
            || self
                .layers
                .iter()
                .find(|l| l.name == layer)
                .is_none_or(|l| l.enabled)
    }
    /// Sets a layer, adding it to the list when new.
    pub fn set_layer(&mut self, layer: &str, enabled: bool) {
        match self.layers.iter_mut().find(|l| l.name == layer) {
            Some(existing) => existing.enabled = enabled,
            None => self.layers.push(DataLayerSetting {
                name: layer.to_owned(),
                enabled,
            }),
        }
    }
}

/// A point that pulls cells in, in addition to cameras.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamingSource {
    pub enabled: bool,
    /// Higher priority sources are served first and win eviction ties.
    pub priority: i32,
    /// Multiplies the partition's load and unload radii for this source.
    pub radius_scale: f64,
}

impl Default for StreamingSource {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
            radius_scale: 1.0,
        }
    }
}

impl StreamingSource {
    pub fn check(&self) -> Result<()> {
        if self.radius_scale.is_finite() && (0.01..=100.0).contains(&self.radius_scale) {
            Ok(())
        } else {
            Err(Error::new(
                "streaming.source.invalid",
                "radius_scale must be in [0.01, 100]",
            ))
        }
    }
}

/// Marks an entity as belonging to a data layer. Read by the `streaming_partition` tool, which
/// writes the entity into that layer's cell file. It has no effect on a running world.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataLayer {
    pub layer: String,
}

pub(super) fn descriptors() -> Vec<ComponentDescriptor> {
    let number = |min: f64, max: f64, description: &str| json!({"type": "number", "minimum": min, "maximum": max, "description": description});
    let integer = |min: u64, max: u64, description: &str| json!({"type": "integer", "minimum": min, "maximum": max, "description": description});
    let partition = WorldPartition::default();
    vec![
        ComponentDescriptor {
            name: PARTITION_NAME.into(),
            version: 1,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "manifest": {"type": "string", "maxLength": 128,
                        "description": "Id of a `partition` asset; empty streams nothing."},
                    "load_radius": number(0.001, MAX_RADIUS, "Metres from a source within which cells load."),
                    "unload_radius": number(0.001, MAX_RADIUS, "Metres beyond which loaded cells unload; at least load_radius."),
                    "loads_per_tick": integer(1, 1024, "Cells started and applied per tick."),
                    "bytes_per_tick": integer(0, 4_294_967_296, "Cell-file bytes per tick; 0 is unlimited."),
                    "max_in_flight": integer(1, 64, "Cells read in the background at once."),
                    "max_loaded_cells": integer(1, 65_536, "Memory budget in cells; the farthest are evicted first."),
                    "camera_source": {"type": "boolean", "description": "Treat every Camera3D as a streaming source."},
                    "synchronous": {"type": "boolean", "description": "Read cells on the calling thread (deterministic)."},
                    "hlod": {"type": "boolean", "description": "Show HLOD proxies of cells that are not loaded."},
                    "hlod_radius": number(0.0, MAX_RADIUS, "Only show proxies within this distance of a source; 0 is unlimited."),
                    "layers": {"type": "array", "maxItems": MAX_LAYERS,
                        "description": "Data layers and whether they are streamed; unlisted layers are enabled.",
                        "items": {"type": "object", "additionalProperties": false, "required": ["name"],
                            "properties": {
                                "name": {"type": "string", "minLength": 1, "maxLength": MAX_NAME},
                                "enabled": {"type": "boolean"},
                            }}},
                },
            }),
            defaults: serde_json::to_value(&partition).expect("plain data"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: SOURCE_NAME.into(),
            version: 1,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": {"type": "boolean"},
                    "priority": {"type": "integer", "minimum": -1_000_000, "maximum": 1_000_000,
                        "description": "Higher priority sources are served first."},
                    "radius_scale": number(0.01, 100.0, "Multiplier of the partition radii for this source."),
                },
            }),
            defaults: serde_json::to_value(StreamingSource::default()).expect("plain data"),
            requires: vec!["Transform".into()],
            entity_references: vec![],
            data_only: false,
        },
        ComponentDescriptor {
            name: LAYER_NAME.into(),
            version: 1,
            schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "layer": {"type": "string", "maxLength": MAX_NAME,
                        "description": "Data layer name; empty is the default layer."},
                },
            }),
            defaults: serde_json::to_value(DataLayer::default()).expect("plain data"),
            requires: vec![],
            entity_references: vec![],
            data_only: false,
        },
    ]
}

/// Loads and validates the `partition` asset `id`.
pub fn load_manifest(project: &Project, id: &str) -> Result<PartitionManifest> {
    let asset = project.require_asset(id, MANIFEST_KIND)?;
    let file = crate::confined_file(&project.root, &asset.path)?;
    let bytes = crate::schema::read_bounded(&file, MAX_MANIFEST_BYTES)?;
    PartitionManifest::from_json(&bytes)
        .map_err(|e| Error::new("streaming.manifest.invalid", format!("manifest {id}: {e}")))
}

/// Scene validation: components are well formed and the manifest asset exists.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        if let Some(value) = custom.get(PARTITION_NAME) {
            let partition: WorldPartition =
                serde_json::from_value(value.data.clone()).map_err(|e| {
                    Error::new(
                        "streaming.partition.invalid",
                        format!("entity {}: {e}", entity.id),
                    )
                })?;
            partition.check()?;
            if !partition.manifest.is_empty() {
                project.require_asset(&partition.manifest, MANIFEST_KIND)?;
            }
        }
        if let Some(value) = custom.get(SOURCE_NAME) {
            let source: StreamingSource =
                serde_json::from_value(value.data.clone()).map_err(|e| {
                    Error::new(
                        "streaming.source.invalid",
                        format!("entity {}: {e}", entity.id),
                    )
                })?;
            source.check()?;
        }
    }
    Ok(())
}
