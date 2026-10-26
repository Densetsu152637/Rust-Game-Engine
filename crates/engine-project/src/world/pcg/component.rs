//! The authored `engine.PcgVolume` component and its validation.
use crate::{ComponentDescriptor, Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_pcg::Graph;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const COMPONENT_NAME: &str = "engine.PcgVolume";
/// Largest graph document the project loader reads.
const MAX_GRAPH_BYTES: u64 = 4 * 1024 * 1024;
/// Hard ceiling of `max_instances`.
pub const MAX_VOLUME_INSTANCES: u32 = 65_536;
const MAX_EXTENT: f32 = 1.0e6;

/// When the runtime generates a volume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PcgMode {
    /// Generate on the first tick and again whenever the volume changes.
    #[default]
    Runtime,
    /// Never generate at runtime; the output was written into the scene by `pcg_run`.
    Baked,
}

/// A box that is filled with the output of a PCG graph.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PcgVolume {
    /// Id of a project asset of kind `pcg`.
    pub graph: String,
    /// Seed of the run; each node derives its own stream from it and its node id.
    pub seed: u64,
    /// Box centre relative to the volume entity's world position.
    pub center: [f32; 3],
    /// Box size in metres.
    pub size: [f32; 3],
    pub mode: PcgMode,
    /// Bump to force regeneration with otherwise unchanged values.
    pub regenerate: u32,
    /// Name in the `PcgSurfaceRegistry`; empty selects the default (flat ground at y = 0).
    pub surface: String,
    /// Upper bound of instances; exceeding it fails the run without changing the output.
    pub max_instances: u32,
}

impl Default for PcgVolume {
    fn default() -> Self {
        Self {
            graph: String::new(),
            seed: 0,
            center: [0.0; 3],
            size: [32.0, 64.0, 32.0],
            mode: PcgMode::Runtime,
            regenerate: 0,
            surface: String::new(),
            max_instances: 4096,
        }
    }
}

impl PcgVolume {
    /// Checks the numeric fields the JSON schema cannot (positive finite extents).
    pub fn check(&self) -> Result<()> {
        let bad = |message: &str| Error::new("pcg.volume.invalid", message.to_owned());
        if self
            .center
            .iter()
            .any(|v| !v.is_finite() || v.abs() > MAX_EXTENT)
        {
            return Err(bad("center must be finite and within 1e6"));
        }
        if self.size.iter().any(|v| !(*v > 0.0 && *v <= MAX_EXTENT)) {
            return Err(bad("size components must be in (0, 1e6]"));
        }
        if self.max_instances == 0 || self.max_instances > MAX_VOLUME_INSTANCES {
            return Err(bad("max_instances must be in 1..=65536"));
        }
        Ok(())
    }
}

pub(super) fn descriptor() -> ComponentDescriptor {
    let vector = |min: f64, max: f64| json!({"type":"array","items":{"type":"number","minimum":min,"maximum":max},"minItems":3,"maxItems":3});
    ComponentDescriptor {
        name: COMPONENT_NAME.into(),
        version: 1,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "graph": {"type": "string", "maxLength": 128,
                    "description": "Id of a `pcg` graph asset; empty generates nothing."},
                "seed": {"type": "integer", "minimum": 0, "maximum": 9_007_199_254_740_991_u64},
                "center": vector(-1.0e6, 1.0e6),
                "size": vector(0.001, 1.0e6),
                "mode": {"type": "string", "enum": ["runtime", "baked"]},
                "regenerate": {"type": "integer", "minimum": 0, "maximum": 4_294_967_295_u64,
                    "description": "Bump to regenerate with unchanged values."},
                "surface": {"type": "string", "maxLength": 128,
                    "description": "PcgSurfaceRegistry name; empty is flat ground at y = 0."},
                "max_instances": {"type": "integer", "minimum": 1, "maximum": MAX_VOLUME_INSTANCES},
            },
        }),
        defaults: serde_json::to_value(PcgVolume::default()).expect("plain data"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}

/// Loads and parses the `pcg` asset `id`. Node kinds are not checked here: games may register
/// their own after loading, so full validation happens when a graph runs.
pub fn load_graph(project: &Project, id: &str) -> Result<Graph> {
    let asset = project.require_asset(id, "pcg")?;
    let file = crate::confined_file(&project.root, &asset.path)?;
    let bytes = crate::schema::read_bounded(&file, MAX_GRAPH_BYTES)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("pcg.graph.parse", format!("graph {id} is not UTF-8")))?;
    Graph::from_json(text).map_err(|d| Error::new(&d.code, format!("graph {id}: {}", d.message)))
}

/// Scene validation: volumes must be well formed and reference an existing `pcg` asset.
pub fn validate_scene(project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let Some(value) = entity.components.custom.get(COMPONENT_NAME) else {
            continue;
        };
        let volume: PcgVolume = serde_json::from_value(value.data.clone())
            .map_err(|e| Error::new("pcg.volume.invalid", format!("entity {}: {e}", entity.id)))?;
        volume.check()?;
        if !volume.graph.is_empty() {
            project.require_asset(&volume.graph, "pcg")?;
        }
    }
    Ok(())
}
