//! CPU scene rendering contracts shared by the game, editor and screenshot host.
use crate::{
    Error, Result,
    content::{Mesh, Texture},
    runtime::RenderFrame,
};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Projection3D {
    Perspective {
        fov_y_radians: f32,
        near: f32,
        far: f32,
    },
    Orthographic {
        height: f32,
        near: f32,
        far: f32,
    },
}
impl Default for Projection3D {
    fn default() -> Self {
        Self::Perspective {
            fov_y_radians: std::f32::consts::FRAC_PI_3,
            near: 0.1,
            far: 1000.0,
        }
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Camera3D {
    pub version: u32,
    pub projection: Projection3D,
    pub active: bool,
    pub lighting: SceneLighting,
    /// Temporal upscaling request for this camera's 3D scene. A request is not a
    /// claim: [`UpscalerStatus`] reports what the renderer actually did.
    pub upscaler: UpscalerSettings,
    /// Frame generation request; [`FrameGenerationStatus`] reports what ran.
    pub frame_generation: FrameGenerationSettings,
    /// Per-camera render feature overrides by stable feature name
    /// (`engine.opaque`, `world.terrain`, ...): `false` disables a feature in
    /// this camera's view, `true` enables one that is off by default. Names of
    /// features the renderer does not have are reported, not rejected
    /// (features register natively at runtime). [`RenderPipelineStatus`]
    /// reports the resolved state.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub render_features: BTreeMap<String, bool>,
}
impl Default for Camera3D {
    fn default() -> Self {
        Self {
            version: 1,
            projection: Projection3D::default(),
            active: true,
            lighting: SceneLighting::default(),
            upscaler: UpscalerSettings::default(),
            frame_generation: FrameGenerationSettings::default(),
            render_features: BTreeMap::new(),
        }
    }
}

/// Most render feature overrides one camera may author.
pub const MAX_RENDER_FEATURE_OVERRIDES: usize = 64;
/// Longest render feature name, in bytes.
pub const MAX_RENDER_FEATURE_NAME_BYTES: usize = 64;

/// Whether `name` is a valid render feature name (1..=64 bytes of `[a-z0-9._-]`).
pub fn valid_render_feature_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_RENDER_FEATURE_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Whether the renderer has drawn a frame with the reported feature state.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderPipelineState {
    /// No 3D frame rendered (for example a headless run); `features` lists
    /// what the renderer would resolve for the active camera.
    #[default]
    NotRendered,
    Rendered,
}

/// One render feature in the active camera's view.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct RenderFeatureView {
    pub name: String,
    /// Stage name (`opaque`, `post_process`, `overlay`, ...).
    pub stage: String,
    pub enabled: bool,
    /// `default`, `view_override`, `required` or `required_override_ignored`.
    pub source: String,
    /// `ran`, `idle`, `disabled`, `superseded`, `replaced_by_host`, `pending`.
    pub activity: String,
}

/// Most features one status lists.
pub const MAX_REPORTED_RENDER_FEATURES: usize = 256;

/// Renderer-reported render feature list and per-view state.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct RenderPipelineStatus {
    pub state: RenderPipelineState,
    /// The authored overrides of the active camera.
    pub overrides: BTreeMap<String, bool>,
    /// Registered features in execution order with their state in the view.
    pub features: Vec<RenderFeatureView>,
    /// Override names that match no registered feature.
    pub unknown_overrides: Vec<String>,
    /// Built-in mesh draws, instanced draw calls and instances of the frame.
    pub draws: u64,
    pub draw_calls: u64,
    pub instances: u64,
}

/// Host-written render pipeline status (sibling of [`UpscalerControl`]).
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct RenderPipelineControl {
    pub status: RenderPipelineStatus,
}

/// Texture IDs with this prefix name a [`TextureStreams`] stream instead of a
/// catalog texture (`MeshMaterial.texture = "stream:intro_video"`).
pub const STREAM_TEXTURE_PREFIX: &str = "stream:";
/// Most texture streams a runtime keeps.
pub const MAX_TEXTURE_STREAMS: usize = 64;
/// Largest stream edge, in pixels.
pub const MAX_STREAM_DIMENSION: u32 = 8192;

/// Whether `name` is a valid stream name (1..=64 bytes of `[A-Za-z0-9_.-]`).
pub fn valid_stream_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// How the texels of a stream reach the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureStreamMode {
    /// [`TextureStreams::upload_rgba`] frames; the renderer uploads each new
    /// revision into one persistent texture.
    CpuUpload,
    /// The host renderer owns the GPU texture and the caller writes into it
    /// (`GpuRenderer::stream_texture` on desktop).
    External,
}

/// The latest frame of one stream, as extracted for rendering.
#[derive(Clone, Debug)]
pub struct StreamFrame {
    pub name: String,
    pub mode: TextureStreamMode,
    pub size: [u32; 2],
    pub revision: u64,
    /// CPU pixels of the current revision (CPU mode), or a stable
    /// transparent placeholder of the stream size (external mode). Draws
    /// sampling the stream reference this exact `Arc`, which identifies it.
    pub texture: Arc<Texture>,
}

/// Dynamic textures (video frames, camera feeds, render-to-texture) that
/// materials and sprites sample by `stream:<name>`.
#[derive(bevy_ecs::prelude::Resource, Default, Debug)]
pub struct TextureStreams {
    streams: BTreeMap<String, StreamFrame>,
}

impl TextureStreams {
    fn create(&mut self, name: &str, mode: TextureStreamMode, size: [u32; 2]) -> Result<()> {
        if !valid_stream_name(name) {
            return Err(Error::new(
                "texture_stream.name",
                "Stream names are 1..=64 bytes of [A-Za-z0-9_.-]",
            ));
        }
        if size.iter().any(|&n| n == 0 || n > MAX_STREAM_DIMENSION) {
            return Err(Error::new(
                "texture_stream.size",
                "Stream sizes are 1..=8192 pixels per edge",
            ));
        }
        if !self.streams.contains_key(name) && self.streams.len() >= MAX_TEXTURE_STREAMS {
            return Err(Error::new(
                "texture_stream.limit",
                "At most 64 texture streams",
            ));
        }
        let pixels = vec![0; size[0] as usize * size[1] as usize * 4];
        self.streams.insert(
            name.to_owned(),
            StreamFrame {
                name: name.to_owned(),
                mode,
                size,
                revision: 0,
                texture: Arc::new(Texture {
                    width: size[0],
                    height: size[1],
                    pixels,
                }),
            },
        );
        Ok(())
    }

    /// A CPU-upload stream starting as transparent black.
    pub fn create_cpu(&mut self, name: &str, width: u32, height: u32) -> Result<()> {
        self.create(name, TextureStreamMode::CpuUpload, [width, height])
    }

    /// A stream whose GPU texture the caller writes into.
    pub fn create_external(&mut self, name: &str, width: u32, height: u32) -> Result<()> {
        self.create(name, TextureStreamMode::External, [width, height])
    }

    /// Replace the frame of a CPU stream (tightly packed sRGB RGBA8 rows, top
    /// row first, of the stream size). Returns the new revision.
    pub fn upload_rgba(&mut self, name: &str, pixels: Vec<u8>) -> Result<u64> {
        let stream = self
            .streams
            .get_mut(name)
            .ok_or_else(|| Error::new("texture_stream.unknown", name))?;
        if stream.mode != TextureStreamMode::CpuUpload {
            return Err(Error::new(
                "texture_stream.mode",
                "Only CPU-upload streams accept uploads",
            ));
        }
        if pixels.len() != stream.size[0] as usize * stream.size[1] as usize * 4 {
            return Err(Error::new(
                "texture_stream.size",
                "Uploads are tightly packed RGBA8 of the stream size",
            ));
        }
        stream.revision += 1;
        stream.texture = Arc::new(Texture {
            width: stream.size[0],
            height: stream.size[1],
            pixels,
        });
        Ok(stream.revision)
    }

    /// Note that the caller wrote new content into an external stream.
    pub fn mark_written(&mut self, name: &str) -> Result<u64> {
        let stream = self
            .streams
            .get_mut(name)
            .ok_or_else(|| Error::new("texture_stream.unknown", name))?;
        stream.revision += 1;
        Ok(stream.revision)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.streams.remove(name).is_some()
    }

    pub fn get(&self, name: &str) -> Option<&StreamFrame> {
        self.streams.get(name)
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }
}

/// Which temporal upscaler renders the 3D scene before 2D sprites/UI.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UpscalerMode {
    /// Render the 3D scene at output resolution (the default).
    #[default]
    None,
    /// NVIDIA DLSS Super Resolution through NGX on Vulkan. Requires an engine
    /// build with the desktop `dlss` feature, the official SDK and runtime, and
    /// a supported NVIDIA GPU; otherwise the scene renders natively and the
    /// status reports why.
    DlssSr,
}

/// DLSS quality preset. `dlaa` keeps the render size equal to the output size
/// (anti-aliasing only); the others let NGX choose a smaller render size.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UpscalerQuality {
    UltraPerformance,
    Performance,
    Balanced,
    #[default]
    Quality,
    Dlaa,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct UpscalerSettings {
    pub mode: UpscalerMode,
    pub quality: UpscalerQuality,
}

/// What the renderer did with the upscaler request of the last presented 3D frame.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpscalerState {
    /// No renderer has presented a 3D frame (for example a headless run).
    #[default]
    NotRendered,
    /// No upscaler was requested; the scene rendered at output resolution.
    Native,
    /// DLSS evaluated this frame's scene; `render_size` is the NGX input size.
    Active,
    /// An upscaler was requested but the scene rendered natively; see `fallback_reason`.
    Fallback,
}

/// Longest fallback reason kept in [`UpscalerStatus`], in bytes.
pub const MAX_UPSCALER_REASON_BYTES: usize = 512;

/// Renderer-reported upscaler state. `Active` is reported only for a frame whose
/// DLSS evaluation succeeded; `evaluated_frames` counts successful evaluations.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct UpscalerStatus {
    pub requested: UpscalerSettings,
    pub state: UpscalerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_size: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_size: Option<[u32; 2]>,
    pub evaluated_frames: u64,
    /// Temporal history discards (first frame, cuts, resizes, quality changes).
    pub history_resets: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
}

/// `reason` truncated to [`MAX_UPSCALER_REASON_BYTES`] on a character boundary.
pub fn bounded_upscaler_reason(reason: &str) -> String {
    let mut end = reason.len().min(MAX_UPSCALER_REASON_BYTES);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

/// Whether presentation inserts generated frames between real frames.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FrameGenerationMode {
    #[default]
    Off,
    /// NVIDIA DLSS Frame Generation through NGX on Vulkan (desktop `dlss`
    /// feature, official SR and FG runtimes, supported NVIDIA GPU).
    DlssFg,
}

/// Inclusive multiplier range: presented frames per real frame.
pub const FRAME_GENERATION_MULTIPLIERS: std::ops::RangeInclusive<u32> = 2..=4;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct FrameGenerationSettings {
    pub mode: FrameGenerationMode,
    /// Presented frames per rendered frame: 2 inserts one generated frame,
    /// 4 inserts three. The renderer also checks NGX's reported maximum.
    pub multiplier: u32,
}
impl Default for FrameGenerationSettings {
    fn default() -> Self {
        Self {
            mode: FrameGenerationMode::Off,
            multiplier: 2,
        }
    }
}
impl FrameGenerationSettings {
    pub fn validate(&self) -> Result<()> {
        if !FRAME_GENERATION_MULTIPLIERS.contains(&self.multiplier) {
            return Err(Error::new(
                "camera3d.frame_generation.invalid",
                "Frame generation multiplier must be 2, 3 or 4",
            ));
        }
        Ok(())
    }
}

/// What the renderer did with the frame generation request of the last frame.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrameGenerationState {
    /// No renderer has produced a 3D frame (for example a headless run).
    #[default]
    NotRendered,
    /// Not requested; only real frames are produced.
    Off,
    /// The last real frame went through NGX frame generation.
    Active,
    /// Requested but not running; only real frames are produced.
    Fallback,
}

/// Renderer-reported frame generation state. Counters only include frames
/// that an actual NGX evaluation produced.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct FrameGenerationStatus {
    pub requested: FrameGenerationSettings,
    pub state: FrameGenerationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Effective multiplier of an active frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<u32>,
    /// NGX's maximum generated frames per real frame, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_frame_max: Option<u32>,
    /// Presentable generated frames produced by NGX.
    pub generated_frames: u64,
    /// Generated frames a host presented to a window surface.
    pub presented_frames: u64,
    /// Real frames evaluated with reset (no interpolation from history).
    pub history_resets: u64,
}

/// Runtime frame generation control, the sibling of [`UpscalerControl`].
/// Callbacks may set `override_settings` and `reset_history` (both upscaler
/// and frame generation history are reset by either flag); the host writes
/// `status` after each frame.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct FrameGenerationControl {
    pub override_settings: Option<FrameGenerationSettings>,
    pub reset_history: bool,
    pub status: FrameGenerationStatus,
}

/// Runtime upscaler control shared by native callbacks and the host renderer.
/// Callbacks may set `override_settings` (replacing the active camera's authored
/// `upscaler` while set) and `reset_history` (discard temporal history on the
/// next presented frame, for example after a camera cut). The host writes
/// `status` after each presented frame; callbacks should treat it as read-only.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct UpscalerControl {
    pub override_settings: Option<UpscalerSettings>,
    pub reset_history: bool,
    pub status: UpscalerStatus,
}
impl Camera3D {
    pub fn validate(&self) -> Result<()> {
        self.lighting.validate()?;
        self.frame_generation.validate()?;
        if self.render_features.len() > MAX_RENDER_FEATURE_OVERRIDES
            || !self
                .render_features
                .keys()
                .all(|name| valid_render_feature_name(name))
        {
            return Err(Error::new(
                "camera3d.render_features.invalid",
                "render_features maps at most 64 feature names (1..=64 bytes of [a-z0-9._-]) to booleans",
            ));
        }
        let (size, near, far, perspective) = match self.projection {
            Projection3D::Perspective {
                fov_y_radians,
                near,
                far,
            } => (fov_y_radians, near, far, true),
            Projection3D::Orthographic { height, near, far } => (height, near, far, false),
        };
        if self.version != 1
            || ![size, near, far].iter().all(|v| v.is_finite())
            || size <= 0.0
            || near <= 0.0
            || far <= near
            || (perspective && size >= std::f32::consts::PI)
        {
            return Err(Error::new(
                "camera3d.invalid",
                "Camera requires version 1, finite positive projection and 0 < near < far",
            ));
        }
        Ok(())
    }
}
/// View-wide lighting of a camera. Lights are entities with
/// `DirectionalLight`, `PointLight`, `SpotLight` or `AreaLight` components
/// (see [`crate::lights`]).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SceneLighting {
    /// Ambient radiance added to lit materials, 0..1 (already exposed).
    pub ambient: f32,
    /// Linear factor from photometric light units to output: a 1 lux
    /// directional light on a white diffuse surface gives `exposure / pi`.
    /// Physically based scenes use `1 / (1.2 * 2^EV100)`, for example about
    /// 2.5e-5 for sunlight at EV100 15. Range 1e-8..=1000.
    pub exposure: f32,
}
impl Default for SceneLighting {
    fn default() -> Self {
        Self {
            ambient: 0.08,
            exposure: 1.0,
        }
    }
}
impl SceneLighting {
    pub fn validate(&self) -> Result<()> {
        if !self.ambient.is_finite()
            || !(0.0..=1.0).contains(&self.ambient)
            || !self.exposure.is_finite()
            || !(1e-8..=1000.0).contains(&self.exposure)
        {
            return Err(Error::new(
                "lighting.invalid",
                "Camera lighting requires ambient in 0..1 and exposure in 1e-8..1000",
            ));
        }
        Ok(())
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MeshMaterial {
    pub version: u32,
    pub shader: Option<String>,
    pub texture: Option<String>,
    pub metallic_roughness_texture: Option<String>,
    pub normal_texture: Option<String>,
    pub normal_scale: f32,
    pub tint: [f32; 4],
    pub unlit: bool,
    pub transparent: bool,
    pub metallic: f32,
    pub roughness: f32,
    pub cast_shadows: bool,
    pub receive_shadows: bool,
}
impl Default for MeshMaterial {
    fn default() -> Self {
        Self {
            version: 1,
            shader: None,
            texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            tint: [1.0; 4],
            unlit: false,
            transparent: false,
            metallic: 0.0,
            roughness: 0.5,
            cast_shadows: true,
            receive_shadows: true,
        }
    }
}
impl MeshMaterial {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !self.metallic.is_finite()
            || !(0.0..=1.0).contains(&self.metallic)
            || !self.roughness.is_finite()
            || !(0.0..=1.0).contains(&self.roughness)
            || !self
                .tint
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
            || self.shader.as_ref().is_some_and(String::is_empty)
            || self.texture.as_ref().is_some_and(String::is_empty)
            || self
                .metallic_roughness_texture
                .as_ref()
                .is_some_and(String::is_empty)
            || self.normal_texture.as_ref().is_some_and(String::is_empty)
            || !self.normal_scale.is_finite()
            || !(0.0..=10.0).contains(&self.normal_scale)
        {
            return Err(Error::new(
                "material.invalid",
                "Material requires version 1, nonempty asset references, finite RGBA/metallic/roughness in 0..1 and normal_scale in 0..10",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct CameraInstance3D {
    pub camera: Camera3D,
    pub world: [[f32; 4]; 4],
}
#[derive(Clone, Debug)]
pub struct MeshDraw3D {
    pub id: String,
    pub mesh: Arc<Mesh>,
    pub texture: Option<Arc<Texture>>,
    pub metallic_roughness_texture: Option<Arc<Texture>>,
    pub normal_texture: Option<Arc<Texture>>,
    pub model: [[f32; 4]; 4],
    pub material: MeshMaterial,
    /// An asset source snapshot, validated by the renderer before submission.
    /// None selects the built-in lit shader.
    pub shader: Option<Arc<str>>,
}
#[derive(Clone, Debug)]
pub struct SceneRenderFrame {
    pub sprites: RenderFrame,
    pub camera3d: Option<CameraInstance3D>,
    pub meshes: Vec<MeshDraw3D>,
    /// Enabled light components in stable entity ID order.
    pub lights: Vec<crate::lights::LightInstance3D>,
    pub materials2d: BTreeMap<String, MaterialSnapshot>,
    /// Texture streams sampled by the draws of this frame.
    pub streams: Vec<StreamFrame>,
}
#[derive(Clone, Debug)]
pub struct MaterialSnapshot {
    pub material: MeshMaterial,
    pub shader: Option<Arc<str>>,
}
impl From<RenderFrame> for SceneRenderFrame {
    fn from(sprites: RenderFrame) -> Self {
        Self {
            sprites,
            camera3d: None,
            meshes: vec![],
            lights: vec![],
            materials2d: BTreeMap::new(),
            streams: Vec::new(),
        }
    }
}
