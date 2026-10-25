//! DLSS device preparation. NGX needs Vulkan extensions requested at device
//! creation, so the choice is made before the host [`GpuContext`] exists. The
//! types here compile in every build; only the `dlss` feature (forwarding to
//! `engine-dlss/ngx-vulkan`) can make a device ready.

use crate::{GpuContext, WgpuOptions};
use anyhow::Result;
use engine_core::bevy_ecs::prelude::Resource;
use std::path::PathBuf;

/// The engine's default NGX application identity (nonsecret). Shipping games
/// should set their own [`DlssConfig::project_id`].
pub const DEFAULT_NGX_PROJECT_ID: &str = "6f3c8d9a-0b4e-4f5a-9e1d-2c7b8a4e5f60";
/// Optional override of the directory holding the official DLSS runtime library.
pub const NGX_RUNTIME_DIR_ENV_VAR: &str = "NGX_RUNTIME_DIR";
/// Optional override of the NGX log directory.
pub const NGX_LOG_DIR_ENV_VAR: &str = "NGX_LOG_DIR";

/// Host DLSS configuration. Insert into the world before launching the runner
/// (like [`crate::GpuRequirements`]); machine paths never belong in authored
/// scenes. Unset directories resolve from [`NGX_RUNTIME_DIR_ENV_VAR`] /
/// [`NGX_LOG_DIR_ENV_VAR`], else the executable's directory and
/// `<temp>/engine-ngx-logs`.
#[derive(Resource, Clone, Debug)]
pub struct DlssConfig {
    pub project_id: String,
    pub runtime_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
    /// Prepare a device for DLSS Super Resolution and Frame Generation even
    /// when the first frame requests neither, so callbacks can enable them
    /// later at runtime.
    pub prepare_device: bool,
}
impl Default for DlssConfig {
    fn default() -> Self {
        Self {
            project_id: DEFAULT_NGX_PROJECT_ID.into(),
            runtime_dir: None,
            log_dir: None,
            prepare_device: false,
        }
    }
}
impl DlssConfig {
    /// Explicit directory, else the environment variable, else `fallback`.
    fn resolve(
        explicit: &Option<PathBuf>,
        variable: &str,
        fallback: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        explicit
            .clone()
            .or_else(|| {
                std::env::var_os(variable)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(fallback)
    }
    pub fn resolved_runtime_dir(&self) -> Option<PathBuf> {
        Self::resolve(&self.runtime_dir, NGX_RUNTIME_DIR_ENV_VAR, || {
            std::env::current_exe().ok()?.parent().map(PathBuf::from)
        })
    }
    /// Created when missing, since NGX requires an existing log directory.
    pub fn resolved_log_dir(&self) -> Option<PathBuf> {
        let dir = Self::resolve(&self.log_dir, NGX_LOG_DIR_ENV_VAR, || {
            Some(std::env::temp_dir().join("engine-ngx-logs"))
        })?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }
}

/// Whether the renderer's device can run DLSS. Produced by
/// [`request_upscaler_gpu`]; a renderer built without one reports why.
pub struct UpscalerDevice(pub(crate) DeviceSupport);

pub(crate) enum DeviceSupport {
    /// No DLSS-capable device was requested.
    NotPrepared,
    Unavailable(String),
    #[cfg(feature = "dlss")]
    Ready(DlssPaths),
}

#[cfg(feature = "dlss")]
#[derive(Clone, Debug)]
pub(crate) struct DlssPaths {
    pub project_id: String,
    pub runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    /// `None` when the device carries NGX's Frame Generation extensions;
    /// otherwise why frame generation cannot run on it.
    pub frame_generation: Option<String>,
}

/// Which NGX features the host device should be prepared for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DlssRequest {
    pub super_resolution: bool,
    pub frame_generation: bool,
}
impl DlssRequest {
    pub const NONE: Self = Self {
        super_resolution: false,
        frame_generation: false,
    };
    pub const ALL: Self = Self {
        super_resolution: true,
        frame_generation: true,
    };
    pub fn any(self) -> bool {
        self.super_resolution || self.frame_generation
    }
    /// The request implied by a first frame and the host configuration.
    pub fn for_frame(
        frame: &engine_project::rendering3d::SceneRenderFrame,
        config: &DlssConfig,
    ) -> Self {
        use engine_project::rendering3d::{FrameGenerationMode, UpscalerMode};
        if config.prepare_device {
            return Self::ALL;
        }
        let camera = frame.camera3d.as_ref().map(|c| &c.camera);
        Self {
            super_resolution: camera.is_some_and(|c| c.upscaler.mode == UpscalerMode::DlssSr),
            frame_generation: camera
                .is_some_and(|c| c.frame_generation.mode == FrameGenerationMode::DlssFg),
        }
    }
}

impl Default for UpscalerDevice {
    fn default() -> Self {
        Self(DeviceSupport::NotPrepared)
    }
}

impl UpscalerDevice {
    /// `None` when DLSS can run on this device; otherwise the precise reason.
    pub fn unavailable_reason(&self) -> Option<String> {
        match &self.0 {
            DeviceSupport::NotPrepared => Some(build_unavailable_reason().unwrap_or_else(|| {
                "the GPU device was created without DLSS extensions; request DLSS before device \
                 creation (Camera3D.upscaler in the first frame or DlssConfig.prepare_device)"
                    .into()
            })),
            DeviceSupport::Unavailable(reason) => Some(reason.clone()),
            #[cfg(feature = "dlss")]
            DeviceSupport::Ready(_) => None,
        }
    }

    /// `None` when DLSS Frame Generation can run on this device.
    pub fn frame_generation_unavailable_reason(&self) -> Option<String> {
        match &self.0 {
            DeviceSupport::NotPrepared => {
                Some(fg_build_unavailable_reason().unwrap_or_else(|| {
                    "the GPU device was created without DLSS extensions; request frame generation \
                 before device creation (Camera3D.frame_generation in the first frame or \
                 DlssConfig.prepare_device)"
                        .into()
                }))
            }
            DeviceSupport::Unavailable(reason) => Some(reason.clone()),
            #[cfg(feature = "dlss")]
            DeviceSupport::Ready(paths) => paths.frame_generation.clone(),
        }
    }
}

/// Why this build cannot run DLSS Frame Generation at all, if it cannot.
pub fn fg_build_unavailable_reason() -> Option<String> {
    #[cfg(not(feature = "dlss"))]
    {
        Some("engine-desktop was built without the `dlss` feature".into())
    }
    #[cfg(feature = "dlss")]
    {
        engine_dlss::build_support(engine_dlss::Feature::FrameGeneration)
            .err()
            .map(|e| e.to_string())
    }
}

/// Why this build cannot run DLSS at all, if it cannot.
pub fn build_unavailable_reason() -> Option<String> {
    #[cfg(not(feature = "dlss"))]
    {
        Some("engine-desktop was built without the `dlss` feature".into())
    }
    #[cfg(feature = "dlss")]
    {
        engine_dlss::build_support(engine_dlss::Feature::SuperResolution)
            .err()
            .map(|e| e.to_string())
    }
}

/// Create the host GPU. When `request` asks for any DLSS feature, a
/// DLSS-capable Vulkan device is attempted first (with Frame Generation
/// extensions when requested, else Super Resolution only); any failure (no
/// feature/SDK, non-Vulkan selection, non-NVIDIA adapter, missing runtime,
/// unsupported extensions) creates the ordinary device instead and records the
/// reason. Frame generation shares the SR context, so it also needs the SR
/// runtime. `surface`, when given, must be usable by the chosen adapter.
pub async fn request_upscaler_gpu(
    instance: wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    options: &WgpuOptions,
    config: &DlssConfig,
    request: DlssRequest,
) -> Result<(GpuContext, UpscalerDevice)> {
    if !request.any() {
        let gpu = GpuContext::request_with_options(instance, surface, options).await?;
        return Ok((gpu, UpscalerDevice::default()));
    }
    let reason = match try_dlss_device(&instance, surface, options, config, request).await {
        Ok(ready) => return ready_gpu(instance, ready, options),
        Err(reason) => reason,
    };
    let gpu = GpuContext::request_with_options(instance, surface, options).await?;
    Ok((gpu, UpscalerDevice(DeviceSupport::Unavailable(reason))))
}

#[cfg(not(feature = "dlss"))]
type Ready = std::convert::Infallible;
#[cfg(feature = "dlss")]
type Ready = (wgpu::Adapter, wgpu::Device, wgpu::Queue, DlssPaths);

#[cfg(not(feature = "dlss"))]
fn ready_gpu(
    _: wgpu::Instance,
    ready: Ready,
    _: &WgpuOptions,
) -> Result<(GpuContext, UpscalerDevice)> {
    match ready {}
}
#[cfg(feature = "dlss")]
fn ready_gpu(
    instance: wgpu::Instance,
    ready: Ready,
    options: &WgpuOptions,
) -> Result<(GpuContext, UpscalerDevice)> {
    let (adapter, device, queue, paths) = ready;
    // The NGX device carries the composed engine request plus NGX's Vulkan
    // extensions; adopting it reports which requested features it enabled.
    let gpu = GpuContext::adopt(instance, adapter, device, queue, &options.requirements)?;
    Ok((gpu, UpscalerDevice(DeviceSupport::Ready(paths))))
}

#[cfg(not(feature = "dlss"))]
async fn try_dlss_device(
    _: &wgpu::Instance,
    _: Option<&wgpu::Surface<'_>>,
    _: &WgpuOptions,
    _: &DlssConfig,
    _: DlssRequest,
) -> std::result::Result<Ready, String> {
    Err(build_unavailable_reason().unwrap_or_default())
}

#[cfg(feature = "dlss")]
async fn try_dlss_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    options: &WgpuOptions,
    config: &DlssConfig,
    request: DlssRequest,
) -> std::result::Result<Ready, String> {
    if let Some(reason) = build_unavailable_reason() {
        return Err(reason);
    }
    if !options.backends.to_wgpu().contains(wgpu::Backends::VULKAN) {
        return Err(format!(
            "DLSS requires the Vulkan backend; the {} graphics API was selected",
            options.backends.name()
        ));
    }
    let mut paths = DlssPaths {
        project_id: config.project_id.clone(),
        runtime_dir: config
            .resolved_runtime_dir()
            .ok_or("no DLSS runtime directory (set DlssConfig.runtime_dir or NGX_RUNTIME_DIR)")?,
        log_dir: config
            .resolved_log_dir()
            .ok_or("cannot create a DLSS log directory (set DlssConfig.log_dir or NGX_LOG_DIR)")?,
        frame_generation: Some(
            "the GPU device was prepared without DLSS Frame Generation extensions; request \
             frame generation before device creation (Camera3D.frame_generation in the first \
             frame or DlssConfig.prepare_device)"
                .into(),
        ),
    };
    let adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN).await;
    let adapter = adapters
        .into_iter()
        .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
        .max_by_key(|a| {
            let info = a.get_info();
            (
                info.vendor == 0x10de,
                info.device_type == wgpu::DeviceType::DiscreteGpu,
            )
        })
        .ok_or("no Vulkan adapter is available for DLSS")?;
    let composed = GpuContext::compose_device(options, &adapter).map_err(|e| format!("{e:#}"))?;
    let descriptor = composed.descriptor("engine desktop device");
    let session = engine_dlss::SessionOptions {
        project_id: paths.project_id.clone(),
        runtime_dir: paths.runtime_dir.clone(),
        log_dir: paths.log_dir.clone(),
        // Device preparation validates paths and support only; sessions use
        // their real output size later.
        output_size: [1, 1],
        quality: engine_dlss::Quality::Quality,
    };
    let name = adapter.get_info().name;
    if request.frame_generation {
        use engine_dlss::Feature;
        match engine_dlss::create_device_for_features(
            &adapter,
            &descriptor,
            &session,
            &[Feature::SuperResolution, Feature::FrameGeneration],
        ) {
            Ok((device, queue)) => {
                paths.frame_generation = None;
                return Ok((adapter, device, queue, paths));
            }
            Err(e) => {
                paths.frame_generation = Some(format!(
                    "DLSS Frame Generation device preparation failed on {name}: {e}"
                ));
            }
        }
    }
    let (device, queue) = engine_dlss::create_device(&adapter, &descriptor, &session)
        .map_err(|e| format!("DLSS device preparation failed on {name}: {e}"))?;
    Ok((adapter, device, queue, paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_renderer_without_a_prepared_device_reports_a_precise_reason() {
        let reason = UpscalerDevice::default().unavailable_reason().unwrap();
        if cfg!(feature = "dlss") {
            assert!(
                reason.contains("NGX") || reason.contains("without DLSS extensions"),
                "{reason}"
            );
        } else {
            assert_eq!(
                reason,
                "engine-desktop was built without the `dlss` feature"
            );
        }
        let explicit = UpscalerDevice(DeviceSupport::Unavailable(
            "DLSS requires an NVIDIA Vulkan adapter".into(),
        ));
        assert_eq!(
            explicit.unavailable_reason().as_deref(),
            Some("DLSS requires an NVIDIA Vulkan adapter")
        );
    }

    #[test]
    fn explicit_directories_win_over_the_environment() {
        let dir = std::env::temp_dir().join(format!("engine-dlss-config-{}", std::process::id()));
        let config = DlssConfig {
            runtime_dir: Some(dir.join("runtime")),
            log_dir: Some(dir.join("logs")),
            ..Default::default()
        };
        assert_eq!(config.resolved_runtime_dir(), Some(dir.join("runtime")));
        assert_eq!(config.resolved_log_dir(), Some(dir.join("logs")));
        assert!(dir.join("logs").is_dir(), "the log directory is created");
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(DlssConfig::default().project_id, DEFAULT_NGX_PROJECT_ID);
    }
}
