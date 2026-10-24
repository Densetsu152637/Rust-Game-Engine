//! The acoustics device: instance, adapter selection, the experimental-feature
//! token and sticky GPU failure recording.
//!
//! This module holds the crate's only `unsafe` expression
//! ([`experimental_features_token`]). Everything else here is safe wgpu code.

use crate::{GpuOptions, GraphicsApi, error};
use engine_acoustics::BackendError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Tier a device is opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    RayQuery,
    Compute,
}

static INSTANCES_CREATED: AtomicU64 = AtomicU64::new(0);

/// How many `wgpu::Instance`s this crate has created in this process. Lets a test
/// prove that a CPU-only selection never touches wgpu.
pub fn wgpu_instances_created() -> u64 {
    INSTANCES_CREATED.load(Ordering::Relaxed)
}

/// The single audited opt-in to wgpu's experimental features.
///
/// Only [`Tier::RayQuery`] devices request it, and only together with
/// `Features::EXPERIMENTAL_RAY_QUERY`; the compute tier never calls this.
#[allow(unsafe_code)]
fn experimental_features_token() -> wgpu::ExperimentalFeatures {
    // SAFETY: `ExperimentalFeatures::enabled` has no memory-safety precondition of
    // its own. Its `# Safety` contract (wgpu-types 30.0.1, `tokens.rs`) is an
    // acknowledgement that wgpu's experimental APIs may contain UB-causing bugs
    // reachable from safe calls. We accept that for exactly one feature,
    // `EXPERIMENTAL_RAY_QUERY` (BLAS/TLAS builds and WGSL `ray_query`), which we
    // use only on Vulkan adapters (the one backend verified on hardware,
    // docs/spatial-audio-design.md section 6.1) and only after the tier's
    // self-test has checked exact hit results. Mitigations against such bugs:
    // wgpu is pinned to `=30.0.1`, all inputs are validated `gpu_layout` data,
    // validation errors and device loss are fatal and demote to a lower tier,
    // and the compute and CPU tiers never use the token.
    unsafe { wgpu::ExperimentalFeatures::enabled() }
}

/// Failures recorded by wgpu callbacks outside an error scope.
#[derive(Debug, Default)]
struct FailureLog {
    lost: bool,
    messages: Vec<String>,
}
const MAX_FAILURES: usize = 8;

/// An open acoustics device. It is independent of any renderer device.
pub(crate) struct GpuDevice {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
    failures: Arc<Mutex<FailureLog>>,
    // Kept alive for the lifetime of the device.
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
}

impl GpuDevice {
    /// Opens a device for `tier`, or explains precisely why the tier is unavailable.
    pub fn open(tier: Tier, options: &GpuOptions, api: GraphicsApi) -> Result<Self, BackendError> {
        // The test hook fails before any device is created, as a real adapter
        // without the feature would.
        if tier == Tier::RayQuery && options.deny_ray_query {
            return Err(error::unavailable(
                "EXPERIMENTAL_RAY_QUERY denied by GpuOptions::deny_ray_query (test hook)",
            ));
        }
        let backends = match tier {
            // Hardware ray query is validated on Vulkan only; an explicit API
            // filter may still pick another backend so its real capability is
            // reported.
            Tier::RayQuery if api == GraphicsApi::Primary => wgpu::Backends::VULKAN,
            _ => api.backends(),
        };
        let instance = create_instance(backends);
        let adapter = select_adapter(&instance, backends, options, tier)?;
        let info = adapter.get_info();
        let mut features = wgpu::Features::empty();
        let mut limits = wgpu::Limits::default();
        let mut experimental = wgpu::ExperimentalFeatures::disabled();
        if tier == Tier::RayQuery {
            if let Some(reason) = ray_query_unavailable_reason(&adapter) {
                return Err(error::unavailable(reason));
            }
            features |= wgpu::Features::EXPERIMENTAL_RAY_QUERY;
            limits = limits.using_minimum_supported_acceleration_structure_values();
            experimental = experimental_features_token();
        }
        let adapter_limits = adapter.limits();
        // Large worlds need more than the 128 MiB default binding size; never
        // ask for more than the adapter offers.
        limits.max_storage_buffer_binding_size = adapter_limits.max_storage_buffer_binding_size;
        limits.max_buffer_size = adapter_limits.max_buffer_size;
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("engine-acoustics-gpu device"),
            required_features: features,
            required_limits: limits.clone(),
            experimental_features: experimental,
            ..Default::default()
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&descriptor)).map_err(|e| {
                error::unavailable(format!(
                    "{} device creation failed on {} ({:?}): {e}",
                    tier_name(tier),
                    info.name,
                    info.backend
                ))
            })?;
        let failures = Arc::new(Mutex::new(FailureLog::default()));
        install_failure_sinks(&device, &failures);
        Ok(Self {
            device,
            queue,
            info,
            limits,
            failures,
            _instance: instance,
            _adapter: adapter,
        })
    }

    /// `Some(message)` once a sticky failure (device loss, out of memory, or an
    /// uncaptured error) was recorded. Every such failure is fatal for the tier.
    pub fn failure(&self) -> Option<String> {
        let log = self.failures.lock().ok()?;
        if log.messages.is_empty() && !log.lost {
            return None;
        }
        Some(log.messages.join("; "))
    }

    pub fn adapter_label(&self) -> String {
        format!("{} ({:?})", self.info.name, self.info.backend)
    }

    pub fn driver_label(&self) -> Option<String> {
        let driver = [self.info.driver.as_str(), self.info.driver_info.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!driver.is_empty()).then_some(driver)
    }
}

pub(crate) fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::RayQuery => "GpuRayQuery",
        Tier::Compute => "GpuCompute",
    }
}

pub(crate) fn create_instance(backends: wgpu::Backends) -> wgpu::Instance {
    INSTANCES_CREATED.fetch_add(1, Ordering::Relaxed);
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    wgpu::Instance::new(descriptor)
}

/// Why `adapter` cannot run the hardware ray-query tier, or `None` if it can.
pub(crate) fn ray_query_unavailable_reason(adapter: &wgpu::Adapter) -> Option<String> {
    let info = adapter.get_info();
    let has_feature = adapter
        .features()
        .contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY);
    match (info.backend, has_feature) {
        (wgpu::Backend::Vulkan, true) => None,
        (wgpu::Backend::Vulkan, false) => Some(format!(
            "Vulkan adapter {} does not expose EXPERIMENTAL_RAY_QUERY \
             (needs VK_KHR_ray_query and acceleration structures)",
            info.name
        )),
        (wgpu::Backend::Dx12, false) => Some(format!(
            "DX12 adapter {} does not expose EXPERIMENTAL_RAY_QUERY: wgpu needs DXC \
             (shader model 6.5) for DX12 ray query and this Cargo-only build uses FXC; \
             ray query is validated on Vulkan only",
            info.name
        )),
        (backend, true) => Some(format!(
            "ray query not validated on {backend:?} (adapter {} exposes \
             EXPERIMENTAL_RAY_QUERY, but only Vulkan is verified)",
            info.name
        )),
        (backend, false) => Some(format!(
            "{backend:?} adapter {} does not expose EXPERIMENTAL_RAY_QUERY; ray query \
             is validated on Vulkan only",
            info.name
        )),
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

fn backend_rank(backend: wgpu::Backend) -> u8 {
    match backend {
        wgpu::Backend::Vulkan => 0,
        wgpu::Backend::Metal => 1,
        wgpu::Backend::Dx12 => 2,
        _ => 3,
    }
}

/// Picks the best hardware adapter: name filter, then (for ray query) feature
/// support, then discrete over integrated, then Vulkan over other backends.
/// Software (CPU) adapters are never used: they are not GPU acceleration.
fn select_adapter(
    instance: &wgpu::Instance,
    backends: wgpu::Backends,
    options: &GpuOptions,
    tier: Tier,
) -> Result<wgpu::Adapter, BackendError> {
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    if adapters.is_empty() {
        return Err(error::unavailable(format!(
            "{}: no wgpu adapter on backends {backends:?}",
            tier_name(tier)
        )));
    }
    let filter = options
        .adapter_name_contains
        .as_ref()
        .map(|s| s.to_lowercase());
    let mut candidates: Vec<wgpu::Adapter> = adapters
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
        return Err(error::unavailable(match &filter {
            Some(f) => format!(
                "{}: no hardware adapter on {backends:?} matches adapter_name_contains={f:?}",
                tier_name(tier)
            ),
            None => format!(
                "{}: only software adapters on {backends:?}",
                tier_name(tier)
            ),
        }));
    }
    candidates.sort_by_key(|a| {
        let info = a.get_info();
        let ray_query = tier == Tier::RayQuery && ray_query_unavailable_reason(a).is_none();
        (
            !ray_query,
            device_rank(info.device_type),
            backend_rank(info.backend),
        )
    });
    Ok(candidates.swap_remove(0))
}

fn install_failure_sinks(device: &wgpu::Device, failures: &Arc<Mutex<FailureLog>>) {
    let sink = failures.clone();
    device.on_uncaptured_error(Arc::new(move |e| {
        if let Ok(mut log) = sink.lock()
            && log.messages.len() < MAX_FAILURES
        {
            log.messages.push(format!("uncaptured GPU error: {e}"));
        }
    }));
    let sink = failures.clone();
    device.set_device_lost_callback(move |reason, message| {
        if reason != wgpu::DeviceLostReason::Destroyed
            && let Ok(mut log) = sink.lock()
        {
            log.lost = true;
            if log.messages.len() < MAX_FAILURES {
                log.messages
                    .push(format!("GPU device lost ({reason:?}): {message}"));
            }
        }
    });
}
