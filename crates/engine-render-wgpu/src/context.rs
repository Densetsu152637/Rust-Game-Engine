//! The shared native GPU context and the options that create it.

use crate::backends::WgpuBackends;
use crate::requirements::{ComposedDevice, DeviceFeatureReport, GpuRequirements};
use anyhow::{Context, Result, anyhow, bail};
use std::sync::{Arc, Mutex};

/// How a backend finds its adapter and device.
#[derive(Clone, Debug)]
pub struct WgpuOptions {
    /// Which wgpu graphics APIs may be used.
    pub backends: WgpuBackends,
    pub power_preference: wgpu::PowerPreference,
    /// Feature/limit requests of every contributing crate, merged at device
    /// creation ([`GpuRequirements::compose`]).
    pub requirements: GpuRequirements,
    /// Frames the CPU may run ahead of the GPU (`1..=8`); also the surface's
    /// desired maximum frame latency.
    pub frames_in_flight: u32,
}

impl Default for WgpuOptions {
    fn default() -> Self {
        Self {
            backends: WgpuBackends::default(),
            power_preference: wgpu::PowerPreference::HighPerformance,
            requirements: GpuRequirements::default(),
            frames_in_flight: 2,
        }
    }
}

/// One recorded GPU failure.
#[derive(Clone, Debug)]
pub(crate) struct GpuFailure {
    pub lost: bool,
    pub message: String,
}

const MAX_RECORDED_FAILURES: usize = 16;

/// Public native device access. Resources created here belong to this device;
/// they are never transferred across independent dynamic-library Rust ABIs.
///
/// The context is a bundle of cheap handles: cloning it shares the same
/// instance, adapter, device, queue and sticky error log, so a renderer and a
/// [`crate::WgpuBackend`] can drive one device.
#[derive(Clone)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    errors: Arc<Mutex<Vec<GpuFailure>>>,
    features: Arc<DeviceFeatureReport>,
}

impl GpuContext {
    /// Instance over the primary graphics APIs (Vulkan, DX12, Metal).
    pub fn instance() -> wgpu::Instance {
        Self::instance_with(WgpuBackends::default())
    }

    /// Instance restricted to `backends`. A [`WgpuBackends::Primary`] request
    /// (the default) still honours wgpu's own `WGPU_BACKEND` override.
    pub fn instance_with(backends: WgpuBackends) -> wgpu::Instance {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends.to_wgpu();
        if backends == WgpuBackends::Primary {
            descriptor.backends = descriptor.backends.with_env();
        }
        wgpu::Instance::new(descriptor)
    }

    pub async fn request(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        Self::request_with_requirements(instance, surface, GpuRequirements::default()).await
    }

    pub async fn request_with_requirements(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        requirements: GpuRequirements,
    ) -> Result<Self> {
        let options = WgpuOptions {
            requirements,
            ..Default::default()
        };
        Self::request_with_options(instance, surface, &options).await
    }

    /// Request an adapter and device honouring the power preference and the
    /// device requirements of `options`.
    pub async fn request_with_options(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        options: &WgpuOptions,
    ) -> Result<Self> {
        let adapter = Self::request_adapter(&instance, surface, options).await?;
        let composed = Self::compose_device(options, &adapter)?;
        let (device, queue) = adapter
            .request_device(&composed.descriptor("engine desktop device"))
            .await
            .context("GPU device creation failed")?;
        let mut gpu = Self::from_device(instance, adapter, device, queue);
        gpu.features = Arc::new(composed.report);
        Ok(gpu)
    }

    /// The adapter [`Self::request_with_options`] would use.
    pub async fn request_adapter(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        options: &WgpuOptions,
    ) -> Result<wgpu::Adapter> {
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                power_preference: options.power_preference,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no compatible GPU adapter")
    }

    /// Merge `options.requirements` against `adapter`. Use
    /// [`ComposedDevice::descriptor`] when creating the device yourself (for
    /// example with vendor extensions), then [`Self::adopt`] it.
    pub fn compose_device(options: &WgpuOptions, adapter: &wgpu::Adapter) -> Result<ComposedDevice> {
        options
            .requirements
            .compose(adapter.features(), &adapter.limits())
            .map_err(|reason| anyhow!("GPU device requirements: {reason}"))
    }

    /// Wrap a device the caller created outside the engine (for example an
    /// OpenXR runtime's Vulkan device brought into wgpu through wgpu-hal's
    /// `from_raw` constructors in the caller's crate) after checking it
    /// against `requirements`: every required feature and limit must be
    /// enabled on `device`, and the report lists which optional features it
    /// has. This entry point is safe; any `unsafe` raw-handle wrapping stays in
    /// the caller. `device` and `queue` must come from `adapter`, which must
    /// come from `instance`.
    pub fn adopt(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        requirements: &GpuRequirements,
    ) -> Result<Self> {
        let report = requirements
            .verify_device(device.features(), &device.limits())
            .map_err(|reason| anyhow!("adopted GPU device: {reason}"))?;
        let mut gpu = Self::from_device(instance, adapter, device, queue);
        gpu.features = Arc::new(report);
        Ok(gpu)
    }

    /// Which requested features the device enabled. Devices wrapped by
    /// [`Self::from_device`] report their features without requests.
    pub fn feature_report(&self) -> &DeviceFeatureReport {
        &self.features
    }

    /// Wrap a device created by the host from `adapter` (for example one with
    /// extra native extensions) and install the engine's error and device-loss
    /// recording. `device` and `queue` must come from `adapter`, which must come
    /// from `instance`.
    pub fn from_device(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        // Validation errors of engine calls are caught per call by
        // `GpuContext::validated`, so they never reach this handler. What arrives
        // here is out-of-memory, internal errors, and validation errors of the
        // host's own direct use of the public `device` outside any scope (custom
        // native passes); those stay sticky, as `check_errors` documents.
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut errors) = sink.lock()
                && errors.len() < MAX_RECORDED_FAILURES
            {
                let kind = match error {
                    wgpu::Error::Validation { .. } => "validation",
                    wgpu::Error::OutOfMemory { .. } => "out of memory",
                    _ => "fault",
                };
                errors.push(GpuFailure {
                    lost: false,
                    message: format!("GPU {kind}: {error}"),
                });
            }
        }));
        let sink = errors.clone();
        device.set_device_lost_callback(move |reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed
                && let Ok(mut errors) = sink.lock()
                && errors.len() < MAX_RECORDED_FAILURES
            {
                errors.push(GpuFailure {
                    lost: true,
                    message: format!("GPU device lost ({reason:?}): {message}"),
                });
            }
        });
        let features = Arc::new(DeviceFeatureReport {
            requests: Vec::new(),
            device_features: device.features(),
            experimental_enabled: device
                .features()
                .intersects(wgpu::Features::all_experimental_mask()),
        });
        Self {
            instance,
            adapter,
            device,
            queue,
            errors,
            features,
        }
    }

    pub async fn headless() -> Result<Self> {
        Self::request(Self::instance(), None).await
    }

    /// Blocking [`Self::request_with_options`] without a surface.
    pub fn headless_with_options(options: &WgpuOptions) -> Result<Self> {
        pollster::block_on(Self::request_with_options(
            Self::instance_with(options.backends),
            None,
            options,
        ))
    }

    /// Run `body` inside a validation error scope. A validation error raised
    /// by the GPU calls inside is returned from this call as
    /// [`RenderError::Backend`] and never written to the sticky log, so one
    /// rejected resource cannot poison later calls. A failure returned by `body`
    /// itself wins over a captured one. The scope is a guard, so it is popped on
    /// every return path, including `?`.
    ///
    /// Out-of-memory and device loss are not captured here; they stay sticky and
    /// are reported by [`crate::error::check_gpu`].
    pub(crate) fn validated<T>(
        &self,
        body: impl FnOnce() -> engine_render::Result<T>,
    ) -> engine_render::Result<T> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = body();
        let captured = pollster::block_on(scope.pop());
        let value = result?;
        match captured {
            Some(error) => Err(engine_render::RenderError::Backend(format!(
                "GPU validation: {error}"
            ))),
            None => Ok(value),
        }
    }

    /// The first recorded failure and whether it was a device loss.
    pub(crate) fn failure(&self) -> Option<(bool, String)> {
        let errors = self.errors.lock().ok()?;
        if errors.is_empty() {
            return None;
        }
        let lost = errors.iter().any(|failure| failure.lost);
        let message = errors
            .iter()
            .map(|failure| failure.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Some((lost, message))
    }

    /// Sticky device errors: device loss, out-of-memory, and validation errors of
    /// calls made directly on the public `device` outside a scope. Validation
    /// errors of the backend's own calls are scoped (see [`Self::validated`]) and
    /// never appear here. The caller can tear down and recreate the host.
    pub fn check_errors(&self) -> Result<()> {
        let errors = self
            .errors
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU error mutex poisoned"))?;
        if !errors.is_empty() {
            bail!(
                "{}",
                errors
                    .iter()
                    .map(|failure| failure.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        Ok(())
    }

    pub fn wait_idle(&self) -> Result<()> {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .context("waiting for GPU")?;
        self.check_errors()
    }
}
