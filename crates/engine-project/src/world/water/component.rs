//! The authored `engine.WaterBody` and `engine.Buoyancy` components and their schemas.
use crate::{ComponentDescriptor, Error, Project, Result, Scene};
use bevy_ecs::prelude::*;
use engine_water::{
    GerstnerWave, MAX_GERSTNER_WAVES, OceanSettings, RiverSettings, ShorelineSettings, WaterOptics,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const WATER_BODY: &str = "engine.WaterBody";
pub const BUOYANCY: &str = "engine.Buoyancy";
/// Most buoyancy sample points per body.
pub const MAX_BUOYANCY_POINTS: usize = 64;

/// What kind of water a body is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaterKind {
    /// Unbounded for queries; `half_extent` sizes the drawn surface.
    #[default]
    Ocean,
    /// Bounded by `half_extent` (local XZ).
    Lake,
    /// Along `river` (local space).
    River,
}

/// How the surface moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveModel {
    /// Flat (calm) water.
    None,
    /// The analytic `gerstner` wave set.
    #[default]
    Gerstner,
    /// The Tessendorf FFT ocean of `fft` (tiled every `fft.patch_length`).
    Fft,
}

/// How a water body reaches the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaterRenderMode {
    /// The `world.water` GPU feature when its renderer marker exists, else the CPU mesh.
    #[default]
    Auto,
    Cpu,
    Gpu,
}

/// Wave settings of a water body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaveSettings {
    pub model: WaveModel,
    /// Up to 16 Gerstner waves (`model: gerstner`).
    pub gerstner: Vec<GerstnerWave>,
    /// FFT ocean (`model: fft`).
    pub fft: OceanSettings,
    /// Water depth for the Gerstner dispersion relation (m; 0 = deep water).
    pub depth: f32,
    /// Wave time per simulated second (0..=10).
    pub time_scale: f32,
}
impl Default for WaveSettings {
    fn default() -> Self {
        Self {
            model: WaveModel::Gerstner,
            gerstner: vec![
                GerstnerWave {
                    direction_degrees: 0.0,
                    wavelength: 18.0,
                    amplitude: 0.35,
                    steepness: 0.5,
                    phase: 0.0,
                },
                GerstnerWave {
                    direction_degrees: 40.0,
                    wavelength: 9.0,
                    amplitude: 0.15,
                    steepness: 0.4,
                    phase: 1.3,
                },
                GerstnerWave {
                    direction_degrees: -30.0,
                    wavelength: 4.0,
                    amplitude: 0.05,
                    steepness: 0.3,
                    phase: 2.1,
                },
            ],
            fft: OceanSettings::default(),
            depth: 0.0,
            time_scale: 1.0,
        }
    }
}

/// Shoreline attenuation, optionally over the scene's terrains (see
/// `engine_water::ShorelineSettings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaterShoreline {
    pub enabled: bool,
    /// Depth (m) at which waves reach full amplitude.
    pub fade_depth: f32,
    /// Depth (m) below which shoreline foam appears.
    pub foam_depth: f32,
    /// Amplitude factor at zero depth.
    pub min_amplitude: f32,
    /// Sample `engine.Terrain` heights as the ground when terrains exist.
    pub use_terrain: bool,
}
impl Default for WaterShoreline {
    fn default() -> Self {
        let s = ShorelineSettings::default();
        Self {
            enabled: s.enabled,
            fade_depth: s.fade_depth,
            foam_depth: s.foam_depth,
            min_amplitude: s.min_amplitude,
            use_terrain: true,
        }
    }
}
impl WaterShoreline {
    /// The model settings.
    pub fn settings(&self) -> ShorelineSettings {
        ShorelineSettings {
            enabled: self.enabled,
            fade_depth: self.fade_depth,
            foam_depth: self.foam_depth,
            min_amplitude: self.min_amplitude,
        }
    }
}

/// Authored `engine.WaterBody`. The entity's `Transform` places the local frame: local `y = 0`
/// is the mean surface (rivers: their centreline heights are local `y`). Keep it upright
/// (rotation about Y only, unit scale) so heights and buoyancy stay vertical.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaterBodyComponent {
    pub kind: WaterKind,
    /// Half size in local X and Z (m, 0.5..=100000): the lake bounds, or the drawn ocean area.
    pub half_extent: [f32; 2],
    pub waves: WaveSettings,
    /// River centreline, widths, depths and flow (`kind: river`).
    pub river: RiverSettings,
    /// Colours, absorption, underwater fog and foam.
    pub optics: WaterOptics,
    pub shoreline: WaterShoreline,
    /// Fluid density in the units of `engine.PhysicsBody3D` material densities (0.01..=100).
    pub density: f32,
    pub render_mode: WaterRenderMode,
    /// Surface grid cells per side for the drawn mesh (8..=256).
    pub grid_resolution: u32,
    /// Whether the surface draws (queries and buoyancy are unaffected).
    pub visible: bool,
}
impl Default for WaterBodyComponent {
    fn default() -> Self {
        Self {
            kind: WaterKind::Ocean,
            half_extent: [50.0, 50.0],
            waves: WaveSettings::default(),
            river: RiverSettings::default(),
            optics: WaterOptics::default(),
            shoreline: WaterShoreline::default(),
            density: 1.0,
            render_mode: WaterRenderMode::Auto,
            grid_resolution: 64,
            visible: true,
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::new("water.component.invalid", message)
}
fn mapped(error: engine_water::Error) -> Error {
    Error::new(error.code, error.message)
}
impl WaterBodyComponent {
    /// Validates every field (the same checks the runtime applies before building it).
    pub fn validate(&self) -> Result<()> {
        if !self
            .half_extent
            .iter()
            .all(|v| v.is_finite() && (0.5..=100_000.0).contains(v))
        {
            return Err(invalid("half_extent must be within 0.5..=100000 m"));
        }
        let w = &self.waves;
        if w.gerstner.len() > MAX_GERSTNER_WAVES {
            return Err(invalid("at most 16 Gerstner waves"));
        }
        for wave in &w.gerstner {
            wave.validate().map_err(mapped)?;
        }
        w.fft.validate().map_err(mapped)?;
        if !(w.depth == 0.0 || (w.depth.is_finite() && (0.1..=10_000.0).contains(&w.depth))) {
            return Err(invalid(
                "waves.depth must be 0 (deep) or within 0.1..=10000 m",
            ));
        }
        if !(w.time_scale.is_finite() && (0.0..=10.0).contains(&w.time_scale)) {
            return Err(invalid("waves.time_scale must be within 0..=10"));
        }
        if self.kind == WaterKind::River {
            self.river.validate().map_err(mapped)?;
        }
        self.optics.validate().map_err(mapped)?;
        self.shoreline.settings().validate().map_err(mapped)?;
        if !(self.density.is_finite() && (0.01..=100.0).contains(&self.density)) {
            return Err(invalid("density must be within 0.01..=100"));
        }
        if !(8..=256).contains(&self.grid_resolution) {
            return Err(invalid("grid_resolution must be within 8..=256"));
        }
        Ok(())
    }
}

/// Authored `engine.Buoyancy` on a dynamic `engine.PhysicsBody3D`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuoyancyComponent {
    /// Sample points in body-local space (at most 64). Empty: a 2 x 2 x 2 lattice over the
    /// collider bounds (box, ball, capsule and cylinder shapes).
    pub points: Vec<[f32; 3]>,
    /// Displaced volume when fully submerged (m^3, 0..=1e6). 0: the collider's volume.
    pub volume: f32,
    /// Vertical extent of each point's slab (m, 0..=1000). 0: the lattice cell height
    /// (authored points: the collider height / 2).
    pub point_height: f32,
    /// Damping toward the water velocity per second while submerged (0..=100).
    pub linear_drag: f32,
    /// Rotational damping per second while submerged (0..=100).
    pub angular_drag: f32,
    /// Extra horizontal coupling to river flow per second (0..=100).
    pub flow_drag: f32,
    /// Authored id of the water body to float in; empty: whichever body has the highest
    /// surface above each point.
    pub water: String,
}
impl Default for BuoyancyComponent {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            volume: 0.0,
            point_height: 0.0,
            linear_drag: 1.0,
            angular_drag: 1.0,
            flow_drag: 1.0,
            water: String::new(),
        }
    }
}
impl BuoyancyComponent {
    pub fn validate(&self) -> Result<()> {
        if self.points.len() > MAX_BUOYANCY_POINTS {
            return Err(invalid("at most 64 buoyancy points"));
        }
        if self
            .points
            .iter()
            .any(|p| p.iter().any(|v| !v.is_finite() || v.abs() > 10_000.0))
        {
            return Err(invalid("buoyancy points must be finite and within 10000 m"));
        }
        let ok = |v: f32, hi: f32| v.is_finite() && (0.0..=hi).contains(&v);
        if !ok(self.volume, 1.0e6) || !ok(self.point_height, 1000.0) {
            return Err(invalid("buoyancy volume 0..=1e6, point_height 0..=1000"));
        }
        if !ok(self.linear_drag, 100.0)
            || !ok(self.angular_drag, 100.0)
            || !ok(self.flow_drag, 100.0)
        {
            return Err(invalid("buoyancy drags must be within 0..=100"));
        }
        if self.water.len() > 128 {
            return Err(invalid("water ids are at most 128 bytes"));
        }
        Ok(())
    }
}

fn number(lo: f64, hi: f64, description: &str) -> Value {
    json!({"type":"number","minimum":lo,"maximum":hi,"description":description})
}
fn vector(n: usize, lo: f64, hi: f64) -> Value {
    json!({"type":"array","items":{"type":"number","minimum":lo,"maximum":hi},"minItems":n,"maxItems":n})
}
fn color() -> Value {
    vector(3, 0.0, 1.0)
}

fn gerstner_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "direction_degrees":{"type":"number","description":"travel direction, degrees from +X toward +Z"},
        "wavelength":number(0.05, 5000.0, "crest-to-crest metres"),
        "amplitude":number(0.0, 400.0, "metres, at most wavelength / 14"),
        "steepness":number(0.0, 1.0, "crest sharpness; the set's sum is scaled to at most 0.9"),
        "phase":{"type":"number","description":"radians"}
    }})
}
fn fft_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "resolution":{"type":"integer","enum":[16,32,64,128,256,512]},
        "patch_length":number(1.0, 10000.0, "tile side in metres"),
        "spectrum":{"type":"string","enum":["phillips","jonswap"]},
        "wind_speed":number(0.1, 60.0, "m/s at 10 m"),
        "wind_direction_degrees":{"type":"number"},
        "amplitude":number(0.0, 100.0, "spectrum multiplier"),
        "fetch":number(1000.0, 1.0e7, "JONSWAP fetch in metres"),
        "peak_enhancement":number(1.0, 10.0, "JONSWAP gamma"),
        "choppiness":number(0.0, 2.0, "horizontal displacement factor"),
        "small_wave_cutoff":number(0.0, 100.0, "metres; 0 = off"),
        "depth":number(0.0, 10000.0, "dispersion depth; 0 = deep"),
        "loop_period":number(0.0, 3600.0, "seconds; 0 = never repeats"),
        "seed":{"type":"integer","minimum":0,"maximum":4294967295u64}
    }})
}
fn optics_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "shallow_color":color(),"deep_color":color(),
        "absorption":vector(3, 0.0, 100.0),
        "fog_color":color(),
        "fog_density":number(0.0, 10.0, "per metre"),
        "foam_color":color(),
        "foam_threshold":number(0.0, 1.0, "Jacobian determinant where crest foam starts"),
        "foam_intensity":number(0.0, 4.0, "foam brightness"),
        "roughness":number(0.01, 1.0, "sun highlight roughness"),
        "refraction":number(0.0, 1.0, "screen-space refraction offset scale")
    }})
}

pub(super) fn water_descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: WATER_BODY.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "kind":{"type":"string","enum":["ocean","lake","river"]},
            "half_extent":vector(2, 0.5, 100000.0),
            "waves":{"type":"object","additionalProperties":false,"properties":{
                "model":{"type":"string","enum":["none","gerstner","fft"]},
                "gerstner":{"type":"array","maxItems":16,"items":gerstner_schema()},
                "fft":fft_schema(),
                "depth":number(0.0, 10000.0, "Gerstner dispersion depth; 0 = deep"),
                "time_scale":number(0.0, 10.0, "wave seconds per simulated second")
            }},
            "river":{"type":"object","additionalProperties":false,"properties":{
                "points":{"type":"array","minItems":2,"maxItems":256,"items":vector(3, -1.0e6, 1.0e6),
                    "description":"local centreline points (y = surface height), source first"},
                "widths":{"type":"array","minItems":1,"maxItems":256,"items":{"type":"number","minimum":0.1,"maximum":10000}},
                "depths":{"type":"array","minItems":1,"maxItems":256,"items":{"type":"number","minimum":0.01,"maximum":1000}},
                "flow_speed":number(0.0, 50.0, "mean speed at the source, m/s"),
                "subdivisions":{"type":"integer","minimum":1,"maximum":64}
            }},
            "optics":optics_schema(),
            "shoreline":{"type":"object","additionalProperties":false,"properties":{
                "enabled":{"type":"boolean"},
                "fade_depth":number(0.01, 1000.0, "depth of full wave amplitude"),
                "foam_depth":number(0.01, 100.0, "depth below which shore foam appears"),
                "min_amplitude":number(0.0, 1.0, "amplitude factor at zero depth"),
                "use_terrain":{"type":"boolean","description":"use engine.Terrain heights as the ground"}
            }},
            "density":number(0.01, 100.0, "fluid density in body-density units"),
            "render_mode":{"type":"string","enum":["auto","cpu","gpu"]},
            "grid_resolution":{"type":"integer","minimum":8,"maximum":256},
            "visible":{"type":"boolean"}
        }}),
        defaults: serde_json::to_value(WaterBodyComponent::default()).expect("finite defaults"),
        requires: vec!["Transform".into()],
        entity_references: vec![],
        data_only: false,
    }
}

pub(super) fn buoyancy_descriptor() -> ComponentDescriptor {
    ComponentDescriptor {
        name: BUOYANCY.into(),
        version: 1,
        schema: json!({"type":"object","additionalProperties":false,"properties":{
            "points":{"type":"array","maxItems":64,"items":vector(3, -10000.0, 10000.0),
                "description":"body-local sample points; empty = lattice over the collider"},
            "volume":number(0.0, 1.0e6, "displaced m^3 when submerged; 0 = collider volume"),
            "point_height":number(0.0, 1000.0, "slab height per point; 0 = automatic"),
            "linear_drag":number(0.0, 100.0, "per second toward the water velocity"),
            "angular_drag":number(0.0, 100.0, "per second"),
            "flow_drag":number(0.0, 100.0, "extra horizontal river coupling per second"),
            "water":{"type":"string","maxLength":128,"description":"water body id; empty = any"}
        }}),
        defaults: serde_json::to_value(BuoyancyComponent::default()).expect("finite defaults"),
        requires: vec!["Transform".into()],
        // Prefab instancing remaps the water body id; an empty string means any body.
        entity_references: vec!["/water".into()],
        data_only: false,
    }
}

/// Scene validation of every `engine.WaterBody` and `engine.Buoyancy`.
pub fn validate_scene(_project: &Project, scene: &Scene) -> Result<()> {
    for entity in &scene.entities {
        let custom = &entity.components.custom;
        let context = |e: String| invalid(format!("entity {}: {e}", entity.id));
        if let Some(value) = custom.get(WATER_BODY) {
            let water: WaterBodyComponent =
                serde_json::from_value(value.data.clone()).map_err(|e| context(e.to_string()))?;
            water.validate().map_err(|e| context(e.to_string()))?;
        }
        if let Some(value) = custom.get(BUOYANCY) {
            let buoyancy: BuoyancyComponent =
                serde_json::from_value(value.data.clone()).map_err(|e| context(e.to_string()))?;
            buoyancy.validate().map_err(|e| context(e.to_string()))?;
        }
    }
    Ok(())
}
