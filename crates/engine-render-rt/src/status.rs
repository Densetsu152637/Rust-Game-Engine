//! Honest ray-tracing status: which tier runs and, when it does not, exactly why.

use serde::{Deserialize, Serialize};

/// Status reasons are bounded so they can be stored in snapshots and telemetry.
pub const MAX_REASON_BYTES: usize = 512;

/// Truncates `text` to at most [`MAX_REASON_BYTES`] on a char boundary.
pub fn bounded_reason(text: &str) -> String {
    if text.len() <= MAX_REASON_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_REASON_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The ray-tracing tier a device runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RayTracingTier {
    /// Hardware ray query (`EXPERIMENTAL_RAY_QUERY` on Vulkan) whose GPU
    /// self-test passed on this device.
    Hardware,
    /// No ray tracing; callers use their raster fallbacks.
    Unavailable,
}

/// Machine-readable category of an [`RayTracingTier::Unavailable`] status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RtUnavailableCause {
    /// No hardware (non-software) adapter was found.
    NoAdapter,
    /// The adapter does not expose `EXPERIMENTAL_RAY_QUERY`.
    MissingAdapterFeature,
    /// The adapter is not Vulkan; ray query is validated on Vulkan only.
    UnsupportedBackend,
    /// Device creation with the ray-tracing request failed.
    DeviceRequestFailed,
    /// The device was created without `EXPERIMENTAL_RAY_QUERY` or without
    /// acceleration-structure limits (the [`crate::RtFeatureRequest`] was not merged).
    DeviceMissingFeature,
    /// The GPU self-test produced a result that differs from the CPU reference.
    SelfTestFailed,
    /// The device was lost or reported an uncaptured or validation error.
    DeviceLost,
}

/// Status of the ray-tracing core on one device.
///
/// `tier == Hardware` is reported only by an [`crate::RtDevice`] whose self-test
/// passed and that has seen no device loss or GPU error since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RayTracingStatus {
    pub tier: RayTracingTier,
    /// `None` for `Hardware`; otherwise why the hardware tier is unavailable,
    /// at most [`MAX_REASON_BYTES`].
    pub cause: Option<RtUnavailableCause>,
    pub reason: Option<String>,
    /// Adapter name and backend, for example `"NVIDIA GeForce RTX 5080 (Vulkan)"`,
    /// when an adapter was involved.
    pub adapter: Option<String>,
}

impl RayTracingStatus {
    pub(crate) fn hardware(adapter: String) -> Self {
        Self {
            tier: RayTracingTier::Hardware,
            cause: None,
            reason: None,
            adapter: Some(adapter),
        }
    }

    pub(crate) fn unavailable(
        cause: RtUnavailableCause,
        reason: impl AsRef<str>,
        adapter: Option<String>,
    ) -> Self {
        Self {
            tier: RayTracingTier::Unavailable,
            cause: Some(cause),
            reason: Some(bounded_reason(reason.as_ref())),
            adapter,
        }
    }

    /// `true` only for a self-tested hardware tier.
    pub fn is_hardware(&self) -> bool {
        self.tier == RayTracingTier::Hardware
    }
}

impl std::fmt::Display for RayTracingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.tier, &self.reason) {
            (RayTracingTier::Hardware, _) => write!(
                f,
                "hardware ray query on {}",
                self.adapter.as_deref().unwrap_or("unknown adapter")
            ),
            (RayTracingTier::Unavailable, reason) => write!(
                f,
                "ray tracing unavailable: {}",
                reason.as_deref().unwrap_or("no reason recorded")
            ),
        }
    }
}

/// One RT lighting effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RtEffect {
    Shadows,
    AmbientOcclusion,
    Reflections,
    DiffuseGi,
}

/// How an effect runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RtEffectMode {
    /// The ray-traced kernel runs.
    RayTraced,
    /// Enabled, but the raster fallback runs (`reason` says why).
    Fallback,
    /// Disabled in settings.
    Disabled,
}

/// Status of one effect, from [`crate::RtEffectsSettings::status`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtEffectStatus {
    pub effect: RtEffect,
    pub mode: RtEffectMode,
    /// The signal goes through the spatio-temporal denoiser.
    pub denoised: bool,
    /// Why the effect is disabled or falls back, or a note on a ray-traced
    /// effect (for example the roughness cutoff of reflections); at most
    /// [`MAX_REASON_BYTES`].
    pub reason: Option<String>,
}

impl RtEffectStatus {
    pub(crate) fn new(
        effect: RtEffect,
        mode: RtEffectMode,
        denoised: bool,
        reason: impl AsRef<str>,
    ) -> Self {
        Self {
            effect,
            mode,
            denoised,
            reason: Some(bounded_reason(reason.as_ref())),
        }
    }
}

/// Status of every RT lighting effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtEffectsStatus {
    pub shadows: RtEffectStatus,
    pub ambient_occlusion: RtEffectStatus,
    pub reflections: RtEffectStatus,
    pub diffuse_gi: RtEffectStatus,
}

/// `adapter_label` as used in statuses: `"<name> (<backend>)"`.
pub(crate) fn adapter_label(info: &wgpu::AdapterInfo) -> String {
    format!("{} ({:?})", info.name, info.backend)
}

/// Why an adapter cannot run the hardware tier, or `None` if it can.
/// Pure so the wording is unit-tested without a GPU.
pub(crate) fn adapter_unavailable_reason(
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    has_ray_query: bool,
    name: &str,
) -> Option<(RtUnavailableCause, String)> {
    if device_type == wgpu::DeviceType::Cpu {
        return Some((
            RtUnavailableCause::NoAdapter,
            format!("{name} is a software adapter; hardware ray tracing needs a GPU"),
        ));
    }
    match (backend, has_ray_query) {
        (wgpu::Backend::Vulkan, true) => None,
        (wgpu::Backend::Vulkan, false) => Some((
            RtUnavailableCause::MissingAdapterFeature,
            format!(
                "Vulkan adapter {name} does not expose EXPERIMENTAL_RAY_QUERY \
                 (needs VK_KHR_ray_query and VK_KHR_acceleration_structure)"
            ),
        )),
        (wgpu::Backend::Dx12, false) => Some((
            RtUnavailableCause::UnsupportedBackend,
            format!(
                "DX12 adapter {name} does not expose EXPERIMENTAL_RAY_QUERY: wgpu needs DXC \
                 (shader model 6.5) for DX12 ray query and this Cargo-only build uses FXC; \
                 ray query is validated on Vulkan only"
            ),
        )),
        (backend, true) => Some((
            RtUnavailableCause::UnsupportedBackend,
            format!(
                "ray query not validated on {backend:?} (adapter {name} exposes \
                 EXPERIMENTAL_RAY_QUERY, but only Vulkan is verified)"
            ),
        )),
        (backend, false) => Some((
            RtUnavailableCause::UnsupportedBackend,
            format!(
                "{backend:?} adapter {name} does not expose EXPERIMENTAL_RAY_QUERY; \
                 ray query is validated on Vulkan only"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_bounded_on_char_boundaries() {
        let long = "é".repeat(400);
        let bounded = bounded_reason(&long);
        assert!(bounded.len() <= MAX_REASON_BYTES);
        assert!(bounded.len() >= MAX_REASON_BYTES - 1);
        assert!(bounded.chars().all(|c| c == 'é'));
        assert_eq!(bounded_reason("short"), "short");
        let status = RayTracingStatus::unavailable(RtUnavailableCause::DeviceLost, &long, None);
        assert!(status.reason.unwrap().len() <= MAX_REASON_BYTES);
    }

    #[test]
    fn only_vulkan_with_the_feature_is_eligible() {
        use wgpu::{Backend, DeviceType};
        let gpu = DeviceType::DiscreteGpu;
        assert!(adapter_unavailable_reason(Backend::Vulkan, gpu, true, "A").is_none());
        let (cause, text) = adapter_unavailable_reason(Backend::Vulkan, gpu, false, "A").unwrap();
        assert_eq!(cause, RtUnavailableCause::MissingAdapterFeature);
        assert!(text.contains("EXPERIMENTAL_RAY_QUERY") && text.contains("Vulkan adapter A"));
        let (cause, text) = adapter_unavailable_reason(Backend::Dx12, gpu, false, "B").unwrap();
        assert_eq!(cause, RtUnavailableCause::UnsupportedBackend);
        assert!(text.contains("DXC"), "{text}");
        let (cause, text) = adapter_unavailable_reason(Backend::Metal, gpu, true, "C").unwrap();
        assert_eq!(cause, RtUnavailableCause::UnsupportedBackend);
        assert!(text.contains("not validated on Metal"), "{text}");
        let (cause, _) =
            adapter_unavailable_reason(Backend::Vulkan, DeviceType::Cpu, true, "llvmpipe").unwrap();
        assert_eq!(cause, RtUnavailableCause::NoAdapter);
    }

    #[test]
    fn status_serializes_with_snake_case_tiers() {
        let status = RayTracingStatus::unavailable(
            RtUnavailableCause::SelfTestFailed,
            "step 'closest hit' ray 3: t = 5.1 expected 5.0",
            Some("GPU (Vulkan)".into()),
        );
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"tier\":\"unavailable\""), "{json}");
        assert!(json.contains("\"cause\":\"self_test_failed\""), "{json}");
        let back: RayTracingStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
        assert!(!status.is_hardware());
        assert!(status.to_string().contains("t = 5.1"));
        let hw = RayTracingStatus::hardware("GPU (Vulkan)".into());
        assert!(hw.is_hardware() && hw.reason.is_none() && hw.cause.is_none());
        assert_eq!(hw.to_string(), "hardware ray query on GPU (Vulkan)");
    }
}
