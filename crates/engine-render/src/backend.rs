//! The backend contract: capabilities and the [`RenderBackend`] trait.

use crate::command::DrawCommand;
use crate::error::Result;
use crate::frame::{FrameDesc, FrameOutcome, OffscreenDesc, ReadbackImage};
use crate::resources::{
    MeshHandle, MeshResource, ResourceId, ShaderHandle, ShaderResource, TargetHandle, Texture2D,
    TextureHandle,
};
use std::collections::BTreeMap;
use std::fmt;

/// Which implementation sits behind the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum BackendKind {
    /// Deterministic in-memory recording backend (no GPU).
    Headless,
    /// The `wgpu` portability layer (itself over Vulkan, DX12, Metal or GL).
    #[default]
    Wgpu,
}

impl BackendKind {
    pub const ALL: [BackendKind; 2] = [Self::Headless, Self::Wgpu];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Wgpu => "wgpu",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RenderFeature {
    BackendAvailable,
    /// Backend command encoding in parallel, beyond parallel CPU draw collection.
    CpuParallelRecording,
    GpuParallelEncoding,
    PortabilitySubset,
    SampledTextures,
    DepthAttachment,
    OffscreenTargets,
    SurfacePresentation,
    Readback,
}

impl RenderFeature {
    pub const ALL: [RenderFeature; 9] = [
        Self::BackendAvailable,
        Self::CpuParallelRecording,
        Self::GpuParallelEncoding,
        Self::PortabilitySubset,
        Self::SampledTextures,
        Self::DepthAttachment,
        Self::OffscreenTargets,
        Self::SurfacePresentation,
        Self::Readback,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::BackendAvailable => "backend-available",
            Self::CpuParallelRecording => "cpu-parallel-recording",
            Self::GpuParallelEncoding => "gpu-parallel-encoding",
            Self::PortabilitySubset => "portability-subset",
            Self::SampledTextures => "sampled-textures",
            Self::DepthAttachment => "depth-attachment",
            Self::OffscreenTargets => "offscreen-targets",
            Self::SurfacePresentation => "surface-presentation",
            Self::Readback => "readback",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderFeatureStatus {
    pub feature: RenderFeature,
    pub name: &'static str,
    pub supported: bool,
    /// Empty when supported.
    pub reason: String,
}

/// Device limits the adapter layer pre-validates against, so an oversized
/// authored asset is rejected with a typed error before it reaches the GPU.
///
/// Backends report the real device values; the [`Default`] matches wgpu's
/// downlevel defaults, which the in-memory recording backend reports so
/// headless tests exercise the same checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceLimits {
    /// Bind group indices must be below this.
    pub max_bind_groups: u32,
    /// `@binding` numbers must be below this, and so must the number of
    /// bindings a group uses.
    pub max_bindings_per_bind_group: u32,
    /// Largest vertex stride in bytes.
    pub max_vertex_buffer_array_stride: u32,
    /// Most attributes in one vertex layout.
    pub max_vertex_attributes: u32,
    /// Largest uniform binding in bytes.
    pub max_uniform_buffer_binding_size: u32,
}

impl Default for DeviceLimits {
    fn default() -> Self {
        Self {
            max_bind_groups: 4,
            max_bindings_per_bind_group: 1000,
            max_vertex_buffer_array_stride: 255,
            max_vertex_attributes: 16,
            max_uniform_buffer_binding_size: 16 << 10,
        }
    }
}

/// What a backend can do, reported after `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderCapabilities {
    pub kind: BackendKind,
    pub backend_name: String,
    /// Human-readable adapter/device description, if any.
    pub device_name: Option<String>,
    pub max_frames_in_flight: u32,
    pub max_texture_dimension: u32,
    /// Limits checked before resources reach the backend.
    pub limits: DeviceLimits,
    pub backend_ready: bool,
    pub parallel_recording: bool,
    pub portability_subset: bool,
    pub sampled_textures: bool,
    pub depth_attachment: bool,
    pub offscreen_targets: bool,
    pub surface_presentation: bool,
    pub readback: bool,
    /// Reasons for features that are unsupported or only partly available.
    pub unsupported_reasons: BTreeMap<RenderFeature, String>,
}

impl RenderCapabilities {
    /// Baseline for a ready backend with full feature coverage.
    pub fn full(kind: BackendKind, backend_name: impl Into<String>) -> Self {
        Self {
            kind,
            backend_name: backend_name.into(),
            device_name: None,
            max_frames_in_flight: 2,
            max_texture_dimension: 8192,
            limits: DeviceLimits::default(),
            backend_ready: true,
            parallel_recording: false,
            portability_subset: false,
            sampled_textures: true,
            depth_attachment: true,
            offscreen_targets: true,
            surface_presentation: true,
            readback: true,
            unsupported_reasons: BTreeMap::new(),
        }
    }

    fn flag(&self, feature: RenderFeature) -> bool {
        match feature {
            RenderFeature::BackendAvailable => self.backend_ready,
            RenderFeature::CpuParallelRecording | RenderFeature::GpuParallelEncoding => {
                self.parallel_recording
            }
            RenderFeature::PortabilitySubset => self.portability_subset,
            RenderFeature::SampledTextures => self.sampled_textures,
            RenderFeature::DepthAttachment => self.depth_attachment,
            RenderFeature::OffscreenTargets => self.offscreen_targets,
            RenderFeature::SurfacePresentation => self.surface_presentation,
            RenderFeature::Readback => self.readback,
        }
    }

    pub fn feature(&self, feature: RenderFeature) -> RenderFeatureStatus {
        let supported = self.flag(feature);
        let reason = if supported {
            String::new()
        } else {
            self.unsupported_reasons
                .get(&feature)
                .cloned()
                .unwrap_or_else(|| "not reported by the backend".into())
        };
        RenderFeatureStatus {
            feature,
            name: feature.name(),
            supported,
            reason,
        }
    }

    /// Multi-line, deterministic feature report for logs and error messages.
    pub fn diagnostics(&self) -> String {
        let mut text = format!(
            "backend {} ({}){}; frames in flight {}\n",
            self.backend_name,
            self.kind,
            self.device_name
                .as_deref()
                .map(|name| format!(" on {name}"))
                .unwrap_or_default(),
            self.max_frames_in_flight
        );
        for feature in RenderFeature::ALL {
            let status = self.feature(feature);
            text.push_str(&format!(
                "  {}: {}{}\n",
                status.name,
                if status.supported { "yes" } else { "no" },
                if status.reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", status.reason)
                }
            ));
        }
        text
    }
}

/// The graphics adapter a [`crate::RenderDevice`] drives.
///
/// The device owns handle allocation, validation and lifetime tracking, so a
/// backend can assume: handles passed to `create_*` are fresh and unique;
/// `destroy_resource` is only called once per handle and only after every frame
/// that used it has completed; draws reference live resources, satisfy the
/// shader's reflected interface, and follow final submission order.
///
/// Backends are single-threaded objects driven through `&mut self`.
pub trait RenderBackend {
    /// Capability snapshot; meaningful after `initialize`.
    fn capabilities(&self) -> RenderCapabilities;
    /// Acquire device resources. Must be idempotent. Returns
    /// [`RenderError::Unavailable`] with a clear message when no loader/adapter exists.
    fn initialize(&mut self) -> Result<()>;
    fn create_shader(&mut self, handle: ShaderHandle, resource: &ShaderResource) -> Result<()>;
    fn create_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()>;
    fn create_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()>;
    fn create_target(&mut self, handle: TargetHandle, desc: &OffscreenDesc) -> Result<()>;
    /// Replace mesh contents. Frames already submitted keep seeing the old data.
    fn update_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()>;
    /// Replace texture contents (same dimensions are not required).
    fn update_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()>;
    fn destroy_resource(&mut self, id: ResourceId) -> Result<()>;
    /// Resize the presentation surface (recreating the swapchain and depth).
    /// Zero sizes are filtered out by the device.
    fn resize(&mut self, width: u32, height: u32) -> Result<()>;
    /// Record and submit one frame to `frame.target`, presenting if it is the surface.
    fn submit_frame(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome>;
    /// Wait for the GPU, then copy an offscreen target to tightly packed RGBA8.
    fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage>;
    fn wait_idle(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
}

impl<B: RenderBackend + ?Sized> RenderBackend for Box<B> {
    fn capabilities(&self) -> RenderCapabilities {
        (**self).capabilities()
    }
    fn initialize(&mut self) -> Result<()> {
        (**self).initialize()
    }
    fn create_shader(&mut self, handle: ShaderHandle, resource: &ShaderResource) -> Result<()> {
        (**self).create_shader(handle, resource)
    }
    fn create_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        (**self).create_mesh(handle, resource)
    }
    fn create_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        (**self).create_texture(handle, texture)
    }
    fn create_target(&mut self, handle: TargetHandle, desc: &OffscreenDesc) -> Result<()> {
        (**self).create_target(handle, desc)
    }
    fn update_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        (**self).update_mesh(handle, resource)
    }
    fn update_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        (**self).update_texture(handle, texture)
    }
    fn destroy_resource(&mut self, id: ResourceId) -> Result<()> {
        (**self).destroy_resource(id)
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        (**self).resize(width, height)
    }
    fn submit_frame(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome> {
        (**self).submit_frame(frame, draws)
    }
    fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        (**self).read_target(target)
    }
    fn wait_idle(&mut self) -> Result<()> {
        (**self).wait_idle()
    }
    fn shutdown(&mut self) -> Result<()> {
        (**self).shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_names_are_stable_and_wgpu_is_the_default() {
        assert_eq!(BackendKind::Headless.to_string(), "headless");
        assert_eq!(BackendKind::Wgpu.to_string(), "wgpu");
        assert_eq!(BackendKind::default(), BackendKind::Wgpu);
        assert_eq!(BackendKind::ALL.len(), 2);
    }

    #[test]
    fn unsupported_feature_reports_its_reason_or_a_default() {
        let mut caps = RenderCapabilities::full(BackendKind::Headless, "test");
        caps.portability_subset = false;
        caps.readback = false;
        caps.unsupported_reasons
            .insert(RenderFeature::Readback, "swapchain only".into());
        let readback = caps.feature(RenderFeature::Readback);
        assert!(!readback.supported);
        assert_eq!(readback.reason, "swapchain only");
        assert_eq!(readback.name, "readback");
        assert!(
            caps.feature(RenderFeature::SampledTextures)
                .reason
                .is_empty()
        );
        assert_eq!(
            caps.feature(RenderFeature::GpuParallelEncoding).reason,
            "not reported by the backend"
        );
    }

    #[test]
    fn diagnostics_list_every_feature_in_a_stable_order() {
        let mut caps = RenderCapabilities::full(BackendKind::Wgpu, "wgpu/test");
        caps.device_name = Some("Test GPU".into());
        caps.depth_attachment = false;
        caps.unsupported_reasons
            .insert(RenderFeature::DepthAttachment, "no D32".into());
        let text = caps.diagnostics();
        assert!(text.starts_with("backend wgpu/test (wgpu) on Test GPU; frames in flight 2"));
        assert!(text.contains("depth-attachment: no (no D32)"));
        assert!(text.contains("sampled-textures: yes"));
        assert_eq!(text.lines().count(), 1 + RenderFeature::ALL.len());
        assert_eq!(text, caps.diagnostics());
    }
}
