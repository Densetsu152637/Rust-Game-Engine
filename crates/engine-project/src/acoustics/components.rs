//! Authored acoustic and audio components (`engine.Acoustic*`, `engine.Audio*`).
//!
//! Every type is `#[serde(default, deny_unknown_fields)]`, serialized without
//! nulls (the catalog schema subset has no nullable types, so an absent optional
//! property means "none"), and checked twice: by the declarative schema in
//! [`super::descriptors`] and by the typed `validate()` methods here.
use crate::{Error, Result};
use bevy_ecs::prelude::Component;
use engine_acoustics::{BackendPreference, MaterialPreset};
use engine_audio::acoustic::{ReverbRenderer, SpatialRenderer, VoiceAcoustics};
use serde::{Deserialize, Serialize};

pub const ENVIRONMENT: &str = "engine.AcousticEnvironment";
pub const MATERIAL: &str = "engine.AcousticMaterial";
pub const GEOMETRY: &str = "engine.AcousticGeometry";
pub const VOLUME: &str = "engine.AcousticVolume";
pub const SOURCE: &str = "engine.AcousticSource";
pub const EMITTER: &str = "engine.AudioEmitter";
pub const LISTENER: &str = "engine.AudioListener";
/// Every component name owned by this module.
pub const NAMES: [&str; 7] = [
    ENVIRONMENT,
    MATERIAL,
    GEOMETRY,
    VOLUME,
    SOURCE,
    EMITTER,
    LISTENER,
];

pub(super) fn invalid(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}
fn component_invalid(name: &str, message: impl std::fmt::Display) -> Error {
    invalid("acoustics.component.invalid", format!("{name}: {message}"))
}
fn finite_in(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}
fn bands_in_unit(values: &[f32; 4]) -> bool {
    values.iter().all(|v| finite_in(*v, 0.0, 1.0))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Auto,
    Cpu,
    GpuCompute,
    GpuRayQuery,
}
impl From<Backend> for BackendPreference {
    fn from(value: Backend) -> Self {
        match value {
            Backend::Auto => Self::Auto,
            Backend::Cpu => Self::Cpu,
            Backend::GpuCompute => Self::GpuCompute,
            Backend::GpuRayQuery => Self::GpuRayQuery,
        }
    }
}

/// Spatial output law. The project default is `binaural`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Renderer {
    #[default]
    Binaural,
    Stereo,
}
impl From<Renderer> for SpatialRenderer {
    fn from(value: Renderer) -> Self {
        match value {
            Renderer::Binaural => Self::Binaural,
            Renderer::Stereo => Self::StereoPan,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reverb {
    Off,
    #[default]
    Parametric,
    Convolution,
}
impl From<Reverb> for ReverbRenderer {
    fn from(value: Reverb) -> Self {
        match value {
            Reverb::Off => Self::Off,
            Reverb::Parametric => Self::Parametric,
            Reverb::Convolution => Self::Convolution,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverbSource {
    #[default]
    Traced,
    Volumes,
    Off,
}

/// Material preset names (the rows of `engine_acoustics::MaterialPreset`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    #[default]
    Default,
    Concrete,
    Brick,
    Plaster,
    Wood,
    Glass,
    Metal,
    Carpet,
    Curtain,
    Grass,
    Water,
    Absorber,
    Reflector,
}
impl Preset {
    pub const NAMES: [&'static str; 13] = [
        "default",
        "concrete",
        "brick",
        "plaster",
        "wood",
        "glass",
        "metal",
        "carpet",
        "curtain",
        "grass",
        "water",
        "absorber",
        "reflector",
    ];
    pub fn engine(self) -> MaterialPreset {
        match self {
            Self::Default => MaterialPreset::Default,
            Self::Concrete => MaterialPreset::Concrete,
            Self::Brick => MaterialPreset::Brick,
            Self::Plaster => MaterialPreset::Plaster,
            Self::Wood => MaterialPreset::Wood,
            Self::Glass => MaterialPreset::Glass,
            Self::Metal => MaterialPreset::Metal,
            Self::Carpet => MaterialPreset::Carpet,
            Self::Curtain => MaterialPreset::Curtain,
            Self::Grass => MaterialPreset::Grass,
            Self::Water => MaterialPreset::Water,
            Self::Absorber => MaterialPreset::Absorber,
            Self::Reflector => MaterialPreset::Reflector,
        }
    }
}

/// Air properties. Explicit `absorption_db_per_m` wins over the ISO 9613-1 estimate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Air {
    /// `false` disables air absorption entirely (the former `air: null`).
    pub enabled: bool,
    pub temperature_c: f32,
    pub humidity_percent: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absorption_db_per_m: Option<[f32; 4]>,
}
impl Default for Air {
    fn default() -> Self {
        Self {
            enabled: true,
            temperature_c: 20.0,
            humidity_percent: 50.0,
            absorption_db_per_m: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Quality {
    pub occlusion_rays: u16,
    pub reflection_order: u8,
    pub max_reflections: u8,
    pub er_rays: u16,
    pub reverb_rays: u32,
    pub max_bounces: u16,
    pub histogram_length_s: f32,
    pub histogram_bin_ms: f32,
    pub late_start_ms: f32,
    pub receiver_radius_m: f32,
    pub time_budget_ms: f32,
}
impl Default for Quality {
    fn default() -> Self {
        Self {
            occlusion_rays: 16,
            reflection_order: 2,
            max_reflections: 8,
            er_rays: 256,
            reverb_rays: 4096,
            max_bounces: 64,
            histogram_length_s: 2.0,
            histogram_bin_ms: 1.0,
            late_start_ms: 80.0,
            receiver_radius_m: 1.0,
            time_budget_ms: 4.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeometrySettings {
    /// Entities with `MeshRenderer` contribute their meshes.
    pub render_meshes: bool,
    /// Include collider-only, non-sensor 3D physics boxes as acoustic geometry.
    pub physics_bodies: bool,
    /// Baked `acoustics` asset used instead of live static collection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    pub default_material: Preset,
    /// Instance mask applied to entities without `engine.AcousticGeometry`.
    pub mask: u8,
}
impl Default for GeometrySettings {
    fn default() -> Self {
        Self {
            render_meshes: true,
            physics_bodies: false,
            asset: None,
            default_material: Preset::Default,
            mask: 255,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    #[default]
    SideView,
    TopDown,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnclosureMode {
    None,
    #[default]
    Auto,
    Bounds,
}

/// The 2D to 3D proxy world (design record section 5.2b).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Proxy2D {
    pub plane: Plane,
    /// World units per acoustic metre; the only proportion knob.
    pub units_per_meter: f32,
    pub depth_m: f32,
    pub enclose_depth: bool,
    pub wall_height_m: f32,
    pub ear_height_m: f32,
    pub floor: bool,
    pub ceiling: bool,
    pub open_sky: bool,
    pub enclosure: EnclosureMode,
    pub margin_m: f32,
    /// `[min_x, min_y, max_x, max_y]` in world units when `enclosure` is `bounds`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[f32; 4]>,
    pub colliders: bool,
    pub physics_bodies: bool,
    pub material: Preset,
    pub floor_material: Preset,
    pub ceiling_material: Preset,
    pub side_material: Preset,
}
impl Default for Proxy2D {
    fn default() -> Self {
        Self {
            plane: Plane::SideView,
            units_per_meter: 1.0,
            depth_m: 8.0,
            enclose_depth: true,
            wall_height_m: 3.0,
            ear_height_m: 1.6,
            floor: true,
            ceiling: true,
            open_sky: false,
            enclosure: EnclosureMode::Auto,
            margin_m: 0.0,
            bounds: None,
            colliders: true,
            physics_bodies: true,
            material: Preset::Default,
            floor_material: Preset::Wood,
            ceiling_material: Preset::Plaster,
            side_material: Preset::Plaster,
        }
    }
}

/// `engine.AcousticEnvironment`: at most one per scene; no Transform required.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcousticEnvironment {
    pub enabled: bool,
    pub backend: Backend,
    pub renderer: Renderer,
    pub reverb: Reverb,
    pub reverb_source: ReverbSource,
    pub update_hz: f32,
    pub reverb_update_hz: f32,
    pub speed_of_sound: f32,
    pub air: Air,
    pub quality: Quality,
    pub seed: u64,
    pub geometry: GeometrySettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy2d: Option<Proxy2D>,
}
impl Default for AcousticEnvironment {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: Backend::Auto,
            renderer: Renderer::Binaural,
            reverb: Reverb::Parametric,
            reverb_source: ReverbSource::Traced,
            update_hz: 15.0,
            reverb_update_hz: 4.0,
            speed_of_sound: 343.0,
            air: Air::default(),
            quality: Quality::default(),
            seed: 1,
            geometry: GeometrySettings::default(),
            proxy2d: None,
        }
    }
}
impl AcousticEnvironment {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(component_invalid(ENVIRONMENT, m));
        let q = &self.quality;
        if !finite_in(self.update_hz, 1.0, 120.0)
            || !finite_in(self.reverb_update_hz, 0.1, 60.0)
            || !finite_in(self.speed_of_sound, 300.0, 400.0)
        {
            return bad("update_hz 1-120, reverb_update_hz 0.1-60, speed_of_sound 300-400");
        }
        if !finite_in(self.air.temperature_c, -20.0, 50.0)
            || !finite_in(self.air.humidity_percent, 0.0, 100.0)
            || self
                .air
                .absorption_db_per_m
                .is_some_and(|a| a.iter().any(|v| !finite_in(*v, 0.0, 10.0)))
        {
            return bad("air: temperature -20..50 C, humidity 0-100 %, absorption 0-10 dB/m");
        }
        if q.occlusion_rays > 256
            || q.reflection_order > 3
            || q.max_reflections > 16
            || q.er_rays > 4096
            || !(64..=1_048_576).contains(&q.reverb_rays)
            || !(1..=256).contains(&q.max_bounces)
            || !finite_in(q.histogram_length_s, 0.1, 4.0)
            || !finite_in(q.histogram_bin_ms, 0.5, 10.0)
            || !finite_in(q.late_start_ms, 10.0, 500.0)
            || q.late_start_ms >= q.histogram_length_s * 1000.0
            || !finite_in(q.receiver_radius_m, 0.1, 10.0)
            || !finite_in(q.time_budget_ms, 0.5, 100.0)
        {
            return bad("quality value outside its documented range");
        }
        if self.backend == Backend::Cpu && q.reverb_rays > 65_536 {
            return bad("quality.reverb_rays is at most 65536 for the cpu backend");
        }
        if self.geometry.mask == 0 {
            return bad("geometry.mask must be non-zero");
        }
        if let Some(p) = &self.proxy2d {
            p.validate()?;
        }
        Ok(())
    }
}
impl Proxy2D {
    fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(component_invalid(ENVIRONMENT, format!("proxy2d: {m}")));
        if !finite_in(self.units_per_meter, 0.001, 1000.0)
            || !finite_in(self.depth_m, 0.1, 1000.0)
            || !finite_in(self.wall_height_m, 0.1, 1000.0)
            || !finite_in(self.ear_height_m, 0.0, self.wall_height_m)
            || !finite_in(self.margin_m, 0.0, 1000.0)
        {
            return bad(
                "units_per_meter 0.001-1000, depth_m and wall_height_m 0.1-1000, ear_height_m within the wall height, margin_m 0-1000",
            );
        }
        match (self.enclosure, self.bounds) {
            (EnclosureMode::Bounds, Some(b))
                if b.iter().all(|v| v.is_finite() && v.abs() <= 1e6)
                    && b[0] < b[2]
                    && b[1] < b[3] => {}
            (EnclosureMode::Bounds, _) => {
                return bad(
                    "enclosure bounds needs finite bounds [min_x, min_y, max_x, max_y] with min < max",
                );
            }
            (_, Some(_)) => return bad("bounds is only valid with enclosure \"bounds\""),
            _ => {}
        }
        Ok(())
    }
}

/// `engine.AcousticMaterial`: a preset plus optional explicit overrides.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcousticMaterialSpec {
    pub preset: Preset,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absorption: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scattering: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transmission: Option<[f32; 4]>,
}
impl AcousticMaterialSpec {
    /// Range checks, plus `transmission <= absorption` when both are explicit.
    /// A transmission override against a preset's absorption is checked when the
    /// world is built, because preset rows live in `engine-acoustics`.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(component_invalid(MATERIAL, m));
        if self.absorption.as_ref().is_some_and(|a| !bands_in_unit(a))
            || self
                .transmission
                .as_ref()
                .is_some_and(|a| !bands_in_unit(a))
            || self.scattering.is_some_and(|s| !finite_in(s, 0.0, 1.0))
        {
            return bad(
                "absorption, transmission and scattering are energy coefficients in [0, 1]",
            );
        }
        if let (Some(a), Some(t)) = (self.absorption, self.transmission)
            && a.iter().zip(&t).any(|(a, t)| t > a)
        {
            return bad("transmission must not exceed absorption in any band");
        }
        Ok(())
    }
    /// The material this component describes: the preset row with overrides applied.
    pub fn resolve(&self) -> engine_acoustics::Result<engine_acoustics::AcousticMaterial> {
        let mut material = engine_acoustics::AcousticMaterial::preset(self.preset.engine())?;
        if let Some(a) = self.absorption {
            material.absorption = a;
        }
        if let Some(s) = self.scattering {
            material.scattering = s;
        }
        if let Some(t) = self.transmission {
            material.transmission = t;
        }
        material.validate()?;
        Ok(material)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometryMode {
    #[default]
    Static,
    Dynamic,
    Exclude,
}
/// `engine.AcousticGeometry`: per-entity control of mesh contribution.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcousticGeometry {
    pub mode: GeometryMode,
    /// Simplified acoustic mesh asset replacing the render mesh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    pub mask: u8,
}
impl Default for AcousticGeometry {
    fn default() -> Self {
        Self {
            mode: GeometryMode::Static,
            mesh: None,
            mask: 1,
        }
    }
}
impl AcousticGeometry {
    pub fn validate(&self) -> Result<()> {
        if self.mask == 0 || self.mesh.as_deref().is_some_and(|m| !super::identifier(m)) {
            return Err(component_invalid(
                GEOMETRY,
                "mask must be non-zero and mesh a valid asset ID",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeShape {
    #[default]
    Box,
    Mesh,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Solid,
    #[default]
    Room,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Space {
    #[default]
    World,
    Proxy2d,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mobility {
    #[default]
    Static,
    Dynamic,
}
impl From<Mobility> for engine_acoustics::Mobility {
    fn from(value: Mobility) -> Self {
        match value {
            Mobility::Static => Self::Static,
            Mobility::Dynamic => Self::Dynamic,
        }
    }
}
/// Face names for `open_faces`, in the bit order of `VolumeKind::Room`.
pub const FACES: [&str; 6] = ["-x", "+x", "-y", "+y", "-z", "+z"];

/// `engine.AcousticVolume`: an acoustic-only solid or room shell.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcousticVolume {
    pub shape: VolumeShape,
    pub half_extents: [f32; 3],
    /// Mesh asset for `shape: "mesh"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    pub kind: VolumeKind,
    pub open_faces: Vec<String>,
    pub space: Space,
    pub mobility: Mobility,
}
impl Default for AcousticVolume {
    fn default() -> Self {
        Self {
            shape: VolumeShape::Box,
            half_extents: [1.0; 3],
            mesh: None,
            kind: VolumeKind::Room,
            open_faces: vec![],
            space: Space::World,
            mobility: Mobility::Static,
        }
    }
}
impl AcousticVolume {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(component_invalid(VOLUME, m));
        if self
            .half_extents
            .iter()
            .any(|v| !finite_in(*v, 0.001, 100_000.0))
        {
            return bad("half_extents must be 0.001-100000");
        }
        if (self.shape == VolumeShape::Mesh) != self.mesh.is_some()
            || self.mesh.as_deref().is_some_and(|m| !super::identifier(m))
        {
            return bad("mesh is required exactly when shape is \"mesh\"");
        }
        if self.open_faces.len() > 6
            || self.open_faces.iter().any(|f| !FACES.contains(&f.as_str()))
            || (1..self.open_faces.len())
                .any(|i| self.open_faces[..i].contains(&self.open_faces[i]))
        {
            return bad("open_faces lists distinct faces from -x, +x, -y, +y, -z, +z");
        }
        if !self.open_faces.is_empty() && self.kind != VolumeKind::Room {
            return bad("open_faces applies to room volumes only");
        }
        if self.space == Space::Proxy2d
            && (self.shape != VolumeShape::Box
                || self.kind != VolumeKind::Solid
                || self.mobility != Mobility::Static)
        {
            return bad("proxy2d volumes are static solid boxes (a 2D footprint)");
        }
        Ok(())
    }
    /// `open_faces` as the bit mask of `engine_acoustics::VolumeKind::Room`.
    pub fn open_mask(&self) -> u8 {
        self.open_faces
            .iter()
            .filter_map(|f| FACES.iter().position(|n| n == f))
            .fold(0, |mask, bit| mask | 1 << bit)
    }
}

/// `engine.AcousticSource`: per-source acoustics for voices the entity owns.
/// It wins over native `PlayOptions.acoustics` while an enabled
/// `engine.AcousticEnvironment` exists.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AcousticSource {
    pub enabled: bool,
    pub occlusion: bool,
    pub reflections: bool,
    pub reverb_send: f32,
    pub doppler: f32,
    pub propagation_delay: bool,
    pub air_absorption: bool,
    pub radius_m: f32,
    pub priority: f32,
    pub hero: bool,
}
impl Default for AcousticSource {
    fn default() -> Self {
        Self {
            enabled: true,
            occlusion: true,
            reflections: true,
            reverb_send: 1.0,
            doppler: 1.0,
            propagation_delay: true,
            air_absorption: true,
            radius_m: 0.25,
            priority: 1.0,
            hero: false,
        }
    }
}
impl AcousticSource {
    pub fn validate(&self) -> Result<()> {
        if !finite_in(self.reverb_send, 0.0, 1.0)
            || !finite_in(self.doppler, 0.0, 4.0)
            || !finite_in(self.radius_m, 0.0, 10.0)
            || !finite_in(self.priority, 0.0, 1000.0)
        {
            return Err(component_invalid(
                SOURCE,
                "reverb_send 0-1, doppler 0-4, radius_m 0-10, priority 0-1000",
            ));
        }
        Ok(())
    }
    /// The mixer options this component dictates. `metres_per_unit` comes from the
    /// environment (`1` for 3D worlds in metres, `1 / units_per_meter` for a proxy).
    pub fn voice_acoustics(&self, metres_per_unit: f32) -> VoiceAcoustics {
        VoiceAcoustics {
            enabled: self.enabled,
            propagation_delay: self.propagation_delay,
            doppler: self.doppler,
            air_absorption: self.air_absorption,
            metres_per_unit,
            reverb_send: self.reverb_send,
            ..VoiceAcoustics::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterBus {
    Music,
    #[default]
    Effects,
    Dialogue,
}
impl From<EmitterBus> for engine_audio::Bus {
    fn from(value: EmitterBus) -> Self {
        match value {
            EmitterBus::Music => Self::Music,
            EmitterBus::Effects => Self::Effects,
            EmitterBus::Dialogue => Self::Dialogue,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterAttenuation {
    None,
    Linear,
    #[default]
    Inverse,
}
impl From<EmitterAttenuation> for engine_audio::Attenuation {
    fn from(value: EmitterAttenuation) -> Self {
        match value {
            EmitterAttenuation::None => Self::None,
            EmitterAttenuation::Linear => Self::Linear,
            EmitterAttenuation::Inverse => Self::Inverse,
        }
    }
}
/// Distance model of a spatial emitter (the fields of `engine_audio::SpatialOptions`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmitterSpatial {
    pub min_distance: f32,
    pub max_distance: f32,
    pub rolloff: f32,
    pub attenuation: EmitterAttenuation,
}
impl Default for EmitterSpatial {
    fn default() -> Self {
        Self {
            min_distance: 1.0,
            max_distance: 100.0,
            rolloff: 1.0,
            attenuation: EmitterAttenuation::Inverse,
        }
    }
}

/// `engine.AudioEmitter`: resident/streamed catalog audio or a procedural graph.
/// Spatial voices follow the entity and use the common acoustic ownership path.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioEmitter {
    /// Asset ID of kind `audio`.
    pub clip: String,
    /// File-backed streaming instead of resident PCM.
    pub streamed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    pub parameters: std::collections::BTreeMap<String, f32>,
    pub queue_frames: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_frames: Option<u64>,
    /// Start playing when the scene starts.
    pub autoplay: bool,
    pub looped: bool,
    pub bus: EmitterBus,
    pub gain: f32,
    pub pitch: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spatial: Option<EmitterSpatial>,
    /// Event name that restarts playback, delivered through `audio::notify`
    /// (including `trigger:<entity>:<enter|stay|exit>` from 2D triggers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_on: Option<String>,
    /// Event name that stops playback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_on: Option<String>,
}
impl Default for AudioEmitter {
    fn default() -> Self {
        Self {
            clip: String::new(),
            streamed: false,
            graph: None,
            parameters: std::collections::BTreeMap::new(),
            queue_frames: 4096,
            duration_frames: None,
            autoplay: true,
            looped: false,
            bus: EmitterBus::Effects,
            gain: 1.0,
            pitch: 1.0,
            spatial: Some(EmitterSpatial::default()),
            play_on: None,
            stop_on: None,
        }
    }
}
impl AudioEmitter {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(invalid("audio.emitter.invalid", format!("{EMITTER}: {m}")));
        // Empty defaults are structurally valid catalog placeholders; runtime requires a source.
        if !self.clip.is_empty() && !super::identifier(&self.clip) {
            return bad("clip must name an audio asset");
        }
        if self.graph.as_ref().is_some_and(|id| !super::identifier(id)) {
            return bad("graph must name a graph asset");
        }
        if self.graph.is_some() && (!self.clip.is_empty() || self.streamed) {
            return bad("Select exactly one clip or graph; streamed selects a file only");
        }
        if self.graph.is_none() && (!self.parameters.is_empty() || self.duration_frames.is_some()) {
            return bad("Parameters and duration require a procedural graph");
        }
        if self.looped && (self.streamed || self.graph.is_some()) {
            return bad("Looped nonresident sources are not supported");
        }
        if !(2..=65536).contains(&self.queue_frames)
            || self.duration_frames.is_some_and(|n| n > 192000_u64 * 86400)
        {
            return bad("Invalid queue size or duration");
        }
        if self.parameters.len() > 64
            || self.parameters.iter().any(|(name, value)| {
                name.is_empty()
                    || name.len() > 64
                    || name.chars().any(char::is_control)
                    || !value.is_finite()
                    || value.abs() > 1e6
            })
        {
            return bad("At most 64 bounded finite graph parameters");
        }
        if !finite_in(self.gain, 0.0, 1.0) || !finite_in(self.pitch, 0.125, 8.0) {
            return bad("gain 0-1 and pitch 0.125-8");
        }
        if let Some(s) = &self.spatial
            && (!finite_in(s.min_distance, 0.0001, 1e6)
                || !finite_in(s.max_distance, 0.0001, 1e6)
                || s.max_distance <= s.min_distance
                || !finite_in(s.rolloff, 0.0, 100.0))
        {
            return bad("spatial needs 0 < min_distance < max_distance and rolloff 0-100");
        }
        for event in [&self.play_on, &self.stop_on].into_iter().flatten() {
            if event.is_empty() || event.len() > 128 || event.chars().any(char::is_control) {
                return bad("play_on and stop_on are event names of 1-128 characters");
            }
        }
        Ok(())
    }
    /// Mixer play options for this emitter (acoustics are merged separately).
    pub fn play_options(&self, owner: &str) -> engine_audio::PlayOptions {
        let mut options = engine_audio::PlayOptions {
            looped: self.looped,
            bus: self.bus.into(),
            gain: self.gain,
            owner: owner.into(),
            pitch: self.pitch,
            ..Default::default()
        };
        if let Some(s) = &self.spatial {
            options.spatial.enabled = true;
            options.spatial.min_distance = s.min_distance;
            options.spatial.max_distance = s.max_distance;
            options.spatial.rolloff = s.rolloff;
            options.spatial.attenuation = s.attenuation.into();
        }
        options
    }
}

/// `engine.AudioListener`: the entity's world pose is the listener. When several
/// are active the first in authored-ID order wins; a native
/// `audio::listener` call overrides it.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioListener {
    pub active: bool,
}
impl Default for AudioListener {
    fn default() -> Self {
        Self { active: true }
    }
}

/// Typed validation for the components of this module; other names pass.
pub fn validate_component(name: &str, data: &serde_json::Value) -> Result<()> {
    fn parse<T: serde::de::DeserializeOwned>(name: &str, data: &serde_json::Value) -> Result<T> {
        serde_json::from_value(data.clone()).map_err(|e| component_invalid(name, e))
    }
    match name {
        ENVIRONMENT => parse::<AcousticEnvironment>(name, data)?.validate(),
        MATERIAL => parse::<AcousticMaterialSpec>(name, data)?.validate(),
        GEOMETRY => parse::<AcousticGeometry>(name, data)?.validate(),
        VOLUME => parse::<AcousticVolume>(name, data)?.validate(),
        SOURCE => parse::<AcousticSource>(name, data)?.validate(),
        EMITTER => parse::<AudioEmitter>(name, data)?.validate(),
        LISTENER => parse::<AudioListener>(name, data).map(|_| ()),
        _ => Ok(()),
    }
}
