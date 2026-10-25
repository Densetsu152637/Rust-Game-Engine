//! The ray-tracing device: the experimental-feature token, the self-test gate,
//! standalone device creation for tests and tools, and sticky failure tracking.
//!
//! This module holds the crate's only `unsafe` expression
//! ([`experimental_features_token`]). Everything else is safe wgpu code.

use crate::error::RtError;
use crate::layout::{ProbeHit, ProbeRay, scene_bind_group_layout};
use crate::probe::ProbeKernel;
use crate::request::RtFeatureRequest;
use crate::scene::RtScene;
use crate::selftest::{self, SelfTestReport};
use crate::status::{
    RayTracingStatus, RtUnavailableCause, adapter_label, adapter_unavailable_reason,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long blocking helpers wait for the GPU.
pub(crate) const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// The single audited opt-in to wgpu's experimental features.
///
/// Only [`RtFeatureRequest::for_adapter`] calls this, after checking that the
/// adapter is a Vulkan adapter exposing `Features::EXPERIMENTAL_RAY_QUERY`, and
/// the resulting request enables no other `EXPERIMENTAL_*` feature.
#[allow(unsafe_code)]
pub(crate) fn experimental_features_token() -> wgpu::ExperimentalFeatures {
    // SAFETY: `ExperimentalFeatures::enabled` has no memory-safety precondition
    // of its own. Its `# Safety` contract (wgpu-types 30.0.1, `tokens.rs`) is an
    // acknowledgement that wgpu's experimental APIs may contain UB-causing bugs
    // reachable from otherwise safe calls, plus an agreement to report such bugs
    // upstream. We accept that for exactly one experimental feature,
    // `EXPERIMENTAL_RAY_QUERY` (BLAS/TLAS creation and builds, acceleration
    // structure bindings and WGSL `ray_query`): the only caller,
    // `RtFeatureRequest::for_adapter`, returns the token solely for a Vulkan
    // adapter that exposes that feature (the one backend verified on hardware,
    // docs/rendering-raytracing.md), and the request never adds another
    // `EXPERIMENTAL_*` feature, so a renderer merging it gains no other
    // experimental API. The token is active from device creation, including
    // while the GPU self-test builds acceleration structures and traces; the
    // self-test then gates every later use, because no `RtDevice` (and so no
    // `RtScene`, kernel or scene bind group) exists until exact hit results
    // match a CPU reference. Mitigations: wgpu is pinned to `=30.0.1`; every
    // mesh, instance, limit and kernel input is validated at the crate boundary
    // against the device limits before it reaches wgpu; GPU validation errors
    // and device loss demote the status to unavailable and stop further use;
    // devices created without the request never see the token. Bugs found are
    // reported upstream per the token audit in docs/rendering-raytracing.md.
    unsafe { wgpu::ExperimentalFeatures::enabled() }
}

pub(crate) fn create_instance(backends: wgpu::Backends) -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

/// Sticky failure shared by a device and every scene and kernel made from it.
#[derive(Debug, Default)]
pub(crate) struct Health {
    failure: Mutex<Option<String>>,
}

impl Health {
    pub fn fail(&self, message: impl Into<String>) {
        if let Ok(mut failure) = self.failure.lock()
            && failure.is_none()
        {
            *failure = Some(message.into());
        }
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }
}

/// Device handles shared by scenes and kernels.
pub(crate) struct Core {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
    pub scene_layout: wgpu::BindGroupLayout,
    pub health: Arc<Health>,
}

impl Core {
    pub fn status(&self) -> RayTracingStatus {
        match self.health.failure() {
            None => RayTracingStatus::hardware(adapter_label(&self.info)),
            Some(message) => RayTracingStatus::unavailable(
                RtUnavailableCause::DeviceLost,
                message,
                Some(adapter_label(&self.info)),
            ),
        }
    }

    pub fn ensure_usable(&self) -> Result<(), RtError> {
        match self.health.failure() {
            None => Ok(()),
            Some(_) => Err(RtError::Unavailable(self.status())),
        }
    }

    /// Records a GPU failure and returns it as an error.
    pub fn failed(&self, message: String) -> RtError {
        self.health.fail(message.clone());
        RtError::Gpu(message)
    }

    /// Submits `encoder` inside a validation scope and waits for completion.
    pub fn submit_and_wait(
        &self,
        encoder: wgpu::CommandEncoder,
        what: &str,
    ) -> Result<(), RtError> {
        self.ensure_usable()?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let index = self.queue.submit(Some(encoder.finish()));
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(self.failed(format!("GPU validation error during {what}: {e}")));
        }
        self.wait(index, what)
    }

    pub fn wait(&self, index: wgpu::SubmissionIndex, what: &str) -> Result<(), RtError> {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(WAIT_TIMEOUT),
            })
            .map(|_| ())
            .map_err(|e| self.failed(format!("waiting for {what} failed: {e}")))
    }

    /// Copies `buffer` (a `COPY_SRC` buffer of at least `size` bytes) to the CPU.
    pub fn read_buffer(
        &self,
        buffer: &wgpu::Buffer,
        size: u64,
        what: &str,
    ) -> Result<Vec<u8>, RtError> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine-render-rt readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("engine-render-rt readback"),
            });
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
        self.submit_and_wait(encoder, what)?;
        let slice = staging.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(WAIT_TIMEOUT),
            })
            .map_err(|e| self.failed(format!("mapping {what} failed: {e}")))?;
        match receiver.recv_timeout(WAIT_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(self.failed(format!("mapping {what} failed: {e}"))),
            Err(_) => return Err(self.failed(format!("mapping {what} timed out"))),
        }
        let bytes = slice
            .get_mapped_range()
            .map_err(|e| self.failed(format!("mapping {what} failed: {e}")))?
            .to_vec();
        staging.unmap();
        Ok(bytes)
    }
}

/// Test hooks for [`RtDevice::new_with_options`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtInitOptions {
    /// Test hook: report the self-test as failed without running it.
    #[doc(hidden)]
    pub force_self_test_failure: bool,
}

/// Options of [`RtDevice::open_standalone`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StandaloneOptions {
    /// Case-insensitive substring of the adapter name.
    pub adapter_name_contains: Option<String>,
    /// Backends to search; `None` means Vulkan, the only validated backend.
    /// Other backends report why the hardware tier is unavailable.
    pub backends: Option<wgpu::Backends>,
    pub init: RtInitOptions,
}

/// A device whose hardware ray-query tier passed its GPU self-test.
///
/// Existence of an `RtDevice` is the proof that the hardware tier works on that
/// device; [`RtDevice::status`] stays `Hardware` until a GPU failure is
/// recorded. Scenes ([`RtScene`]) and kernels are created from it.
pub struct RtDevice {
    core: Arc<Core>,
    probe: ProbeKernel,
    report: SelfTestReport,
    storage_r8unorm: bool,
    storage_r16float: bool,
    // Kept alive for standalone devices.
    _standalone: Option<(wgpu::Instance, wgpu::Adapter)>,
}

impl std::fmt::Debug for RtDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtDevice")
            .field("adapter", &adapter_label(&self.core.info))
            .field("status", &self.core.status())
            .field("self_test", &self.report)
            .finish()
    }
}

impl RtDevice {
    /// Verifies `device` (created from `adapter` with a merged
    /// [`RtFeatureRequest`]) and runs the GPU self-test on it. On success the
    /// device runs the hardware tier; otherwise the status says exactly why not.
    ///
    /// Does not install wgpu error or device-lost callbacks (the renderer owns
    /// those); report a loss with [`RtDevice::report_device_lost`].
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, RayTracingStatus> {
        Self::new_with_options(adapter, device, queue, &RtInitOptions::default())
    }

    pub fn new_with_options(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: &RtInitOptions,
    ) -> Result<Self, RayTracingStatus> {
        let info = adapter.get_info();
        let label = adapter_label(&info);
        if let Some((cause, reason)) = adapter_unavailable_reason(
            info.backend,
            info.device_type,
            adapter
                .features()
                .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY),
            &info.name,
        ) {
            return Err(RayTracingStatus::unavailable(cause, reason, Some(label)));
        }
        if !device
            .features()
            .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY)
        {
            return Err(RayTracingStatus::unavailable(
                RtUnavailableCause::DeviceMissingFeature,
                "the device was created without EXPERIMENTAL_RAY_QUERY; merge \
                 RtFeatureRequest::for_adapter into the device descriptor",
                Some(label),
            ));
        }
        let limits = device.limits();
        if limits.max_tlas_instance_count == 0
            || limits.max_blas_primitive_count == 0
            || limits.max_blas_geometry_count == 0
            || limits.max_acceleration_structures_per_shader_stage == 0
        {
            return Err(RayTracingStatus::unavailable(
                RtUnavailableCause::DeviceMissingFeature,
                format!(
                    "the device has no acceleration-structure limits (TLAS instances {}, BLAS \
                     primitives {}, AS per stage {}); merge RtFeatureRequest::limits",
                    limits.max_tlas_instance_count,
                    limits.max_blas_primitive_count,
                    limits.max_acceleration_structures_per_shader_stage
                ),
                Some(label),
            ));
        }
        let core = Arc::new(Core {
            scene_layout: scene_bind_group_layout(device),
            device: device.clone(),
            queue: queue.clone(),
            info,
            limits,
            health: Arc::new(Health::default()),
        });
        let probe = ProbeKernel::new(&core);
        let self_test_failed = |reason: String| {
            RayTracingStatus::unavailable(
                RtUnavailableCause::SelfTestFailed,
                reason,
                Some(label.clone()),
            )
        };
        if options.force_self_test_failure {
            return Err(self_test_failed(
                "self-test step 'closest hit' failed: forced by \
                 RtInitOptions::force_self_test_failure (test hook)"
                    .into(),
            ));
        }
        let report = match selftest::run(&core, &probe) {
            Ok(report) => report,
            Err(RtError::Gpu(message)) => {
                return Err(RayTracingStatus::unavailable(
                    RtUnavailableCause::DeviceLost,
                    format!("GPU failure during the self-test: {message}"),
                    Some(label),
                ));
            }
            Err(RtError::Invalid(message)) => return Err(self_test_failed(message)),
            Err(e) => return Err(self_test_failed(e.to_string())),
        };
        let has_specific = device
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
        let storage_write = |format| {
            has_specific
                && adapter
                    .get_texture_format_features(format)
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::STORAGE_WRITE_ONLY)
        };
        Ok(Self {
            storage_r8unorm: storage_write(wgpu::TextureFormat::R8Unorm),
            storage_r16float: storage_write(wgpu::TextureFormat::R16Float),
            core,
            probe,
            report,
            _standalone: None,
        })
    }

    /// Opens its own instance, adapter and device with the ray-tracing request
    /// (for tests, tools and headless use), then runs the self-test. Installs
    /// error and device-lost callbacks that demote the status.
    pub fn open_standalone(options: &StandaloneOptions) -> Result<Self, RayTracingStatus> {
        let backends = options.backends.unwrap_or(wgpu::Backends::VULKAN);
        let instance = create_instance(backends);
        let adapter = select_adapter(&instance, backends, options)?;
        let request = RtFeatureRequest::for_adapter(&adapter)?;
        let info = adapter.get_info();
        let mut descriptor = wgpu::DeviceDescriptor {
            label: Some("engine-render-rt standalone device"),
            ..Default::default()
        };
        request.apply(&mut descriptor);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&descriptor)).map_err(|e| {
                RayTracingStatus::unavailable(
                    RtUnavailableCause::DeviceRequestFailed,
                    format!("device creation with EXPERIMENTAL_RAY_QUERY failed: {e}"),
                    Some(adapter_label(&info)),
                )
            })?;
        let mut rt = Self::new_with_options(&adapter, &device, &queue, &options.init)?;
        install_failure_sinks(&device, &rt.core.health);
        rt._standalone = Some((instance, adapter));
        Ok(rt)
    }

    /// `Hardware` until a GPU failure is recorded, then `Unavailable` with the
    /// failure (cause `DeviceLost`).
    pub fn status(&self) -> RayTracingStatus {
        self.core.status()
    }

    /// Measurements of the self-test that admitted this device.
    pub fn self_test_report(&self) -> &SelfTestReport {
        &self.report
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.core.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.core.queue
    }

    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.core.info
    }

    pub fn limits(&self) -> &wgpu::Limits {
        &self.core.limits
    }

    /// The layout of [`RtScene::bind_group`], for pipeline layouts of passes
    /// that use the WGSL library.
    pub fn scene_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.core.scene_layout
    }

    /// Whether `format` can be a storage-texture output of the RT kernels on
    /// this device (`R32Float` always; `R8Unorm`/`R16Float` need
    /// `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter support).
    pub fn supports_storage_format(&self, format: wgpu::TextureFormat) -> bool {
        match format {
            wgpu::TextureFormat::R32Float | wgpu::TextureFormat::Rgba32Float => true,
            wgpu::TextureFormat::R8Unorm => self.storage_r8unorm,
            wgpu::TextureFormat::R16Float => self.storage_r16float,
            _ => false,
        }
    }

    /// Records a device loss (or another fatal GPU error) observed by the host,
    /// demoting the status. Every later operation fails with `Unavailable`.
    pub fn report_device_lost(&self, message: &str) {
        self.core.health.fail(format!("device lost: {message}"));
    }

    /// Traces `rays` against `scene` and returns, per ray, the closest hit with
    /// its surface and the any-hit visibility of the same ray. Blocking; for
    /// tests and tools.
    pub fn cast_rays(&self, scene: &RtScene, rays: &[ProbeRay]) -> Result<Vec<ProbeHit>, RtError> {
        self.probe.cast(&self.core, scene, rays)
    }

    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }
}

fn device_rank(kind: wgpu::DeviceType) -> u8 {
    match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

/// Picks the best hardware adapter: name filter, then eligibility for the
/// hardware tier, then discrete over integrated. Software adapters are skipped.
fn select_adapter(
    instance: &wgpu::Instance,
    backends: wgpu::Backends,
    options: &StandaloneOptions,
) -> Result<wgpu::Adapter, RayTracingStatus> {
    let filter = options
        .adapter_name_contains
        .as_ref()
        .map(|s| s.to_lowercase());
    let mut candidates: Vec<wgpu::Adapter> =
        pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .filter(|a| {
                let info = a.get_info();
                info.device_type != wgpu::DeviceType::Cpu
                    && filter
                        .as_ref()
                        .is_none_or(|f| info.name.to_lowercase().contains(f))
            })
            .collect();
    if candidates.is_empty() {
        return Err(RayTracingStatus::unavailable(
            RtUnavailableCause::NoAdapter,
            match &filter {
                Some(f) => format!("no hardware adapter on {backends:?} matches {f:?}"),
                None => format!("no hardware adapter on {backends:?}"),
            },
            None,
        ));
    }
    candidates.sort_by_key(|a| {
        let info = a.get_info();
        let eligible = adapter_unavailable_reason(
            info.backend,
            info.device_type,
            a.features()
                .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY),
            &info.name,
        )
        .is_none();
        (!eligible, device_rank(info.device_type))
    });
    Ok(candidates.swap_remove(0))
}

fn install_failure_sinks(device: &wgpu::Device, health: &Arc<Health>) {
    let sink = health.clone();
    device.on_uncaptured_error(Arc::new(move |e| {
        sink.fail(format!("uncaptured GPU error: {e}"));
    }));
    let sink = health.clone();
    device.set_device_lost_callback(move |reason, message| {
        if reason != wgpu::DeviceLostReason::Destroyed {
            sink.fail(format!("GPU device lost ({reason:?}): {message}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_keeps_the_first_failure() {
        let health = Health::default();
        assert!(health.failure().is_none());
        health.fail("first");
        health.fail("second");
        assert_eq!(health.failure().as_deref(), Some("first"));
    }
}
