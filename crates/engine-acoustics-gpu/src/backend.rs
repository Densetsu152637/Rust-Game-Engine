//! `GpuBackend`: the `PropagationBackend` for tiers 1 (hardware ray query) and 2
//! (compute BVH traversal).

use crate::device::{GpuDevice, Tier, tier_name};
use crate::kernel::{self, Kernel};
use crate::scene::{FlatScene, MeshRange};
use crate::selftest::{self, SelfTestReport};
use crate::upload::GpuScene;
use crate::{GpuOptions, GraphicsApi, error};
use engine_acoustics::{
    AcousticWorld, BackendError, BackendInfo, BackendKind, EnergyHistogram, EnergyJob,
    PropagationBackend, TraceSettings, TraceStats,
};
use std::time::Duration;

/// An initialized tier: device, pipelines and the self-test that admitted it.
struct Runtime {
    gpu: GpuDevice,
    kernel: Kernel,
    self_test: SelfTestReport,
}

/// The world as last uploaded.
struct WorldState {
    revision: u64,
    dynamic_generation: u64,
    scene: FlatScene,
    gpu: GpuScene,
}

/// Tier 1 ([`GpuBackend::ray_query`]) or tier 2 ([`GpuBackend::compute`]).
///
/// The tier is unusable until [`PropagationBackend::initialize`] has opened a
/// device and passed the self-test; until then every call fails with
/// `acoustics.backend.unavailable` and `info().hardware_rt` is false.
pub struct GpuBackend {
    tier: Tier,
    options: GpuOptions,
    api: GraphicsApi,
    runtime: Option<Runtime>,
    world: Option<WorldState>,
}

impl std::fmt::Debug for GpuBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBackend")
            .field("tier", &self.tier)
            .field("options", &self.options)
            .field("api", &self.api)
            .field("initialized", &self.runtime.is_some())
            .finish()
    }
}

impl GpuBackend {
    /// Tier 1: hardware ray query (`EXPERIMENTAL_RAY_QUERY`, Vulkan only).
    pub fn ray_query(options: GpuOptions) -> Self {
        Self::new(Tier::RayQuery, options)
    }

    /// Tier 2: GPU-accelerated BVH traversal in a compute shader. This is not
    /// hardware ray tracing.
    pub fn compute(options: GpuOptions) -> Self {
        Self::new(Tier::Compute, options)
    }

    fn new(tier: Tier, options: GpuOptions) -> Self {
        Self {
            tier,
            options,
            api: GraphicsApi::Primary,
            runtime: None,
            world: None,
        }
    }

    /// Restricts adapter discovery to one graphics API (an addition to section
    /// 7.2, used to report a backend's real capability, for example DX12's lack
    /// of ray query). The default, [`GraphicsApi::Primary`], uses Vulkan for
    /// tier 1 and every primary API for tier 2.
    pub fn with_api(mut self, api: GraphicsApi) -> Self {
        self.api = api;
        self
    }

    pub fn options(&self) -> &GpuOptions {
        &self.options
    }

    /// What this tier is, in words suitable for status output.
    pub fn description(&self) -> &'static str {
        match self.tier {
            Tier::RayQuery => "hardware ray query (wgpu EXPERIMENTAL_RAY_QUERY on Vulkan)",
            Tier::Compute => {
                "GPU-accelerated BVH traversal in a compute shader (not hardware ray tracing)"
            }
        }
    }

    /// The measurements of the self-test that admitted this tier, if it passed.
    pub fn self_test_report(&self) -> Option<&SelfTestReport> {
        self.runtime.as_ref().map(|r| &r.self_test)
    }

    fn kind(&self) -> BackendKind {
        match self.tier {
            Tier::RayQuery => BackendKind::GpuRayQuery,
            Tier::Compute => BackendKind::GpuCompute,
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(u64::from(self.options.readback_timeout_ms.max(1)))
    }

    fn runtime(&self) -> Result<&Runtime, BackendError> {
        self.runtime.as_ref().ok_or_else(|| {
            error::unavailable(format!("{} is not initialized", tier_name(self.tier)))
        })
    }

    /// A sticky device failure is fatal: the runtime is dropped so the tier
    /// cannot be used again without a new `initialize`.
    fn check_device(&mut self) -> Result<(), BackendError> {
        if let Some(message) = self.runtime.as_ref().and_then(|r| r.gpu.failure()) {
            self.runtime = None;
            self.world = None;
            return Err(error::failed(format!(
                "{}: {message}",
                tier_name(self.tier)
            )));
        }
        Ok(())
    }

    fn fatal(&mut self, e: BackendError) -> BackendError {
        if e.fatal {
            self.runtime = None;
            self.world = None;
        }
        e
    }

    fn upload_world(&mut self, world: &AcousticWorld) -> Result<(), BackendError> {
        let runtime = self.runtime()?;
        let scene = FlatScene::from_view(&world.gpu_view());
        let (meshes, mesh_of_instance): (Vec<MeshRange>, Vec<u32>) =
            scene.validate().map_err(|e| {
                error::failed(format!(
                    "{} cannot use this world: {}",
                    tier_name(self.tier),
                    e.message
                ))
            })?;
        let scope = runtime
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let uploaded =
            GpuScene::upload(&runtime.gpu, self.tier, &scene, &meshes, &mesh_of_instance)
                .map_err(|e| error::failed(e.message));
        let captured = pollster::block_on(scope.pop());
        let uploaded = uploaded?;
        if let Some(e) = captured {
            return Err(error::failed(format!(
                "GPU validation error uploading the world: {e}"
            )));
        }
        self.world = Some(WorldState {
            revision: world.revision(),
            dynamic_generation: world.dynamic_generation(),
            scene,
            gpu: uploaded,
        });
        Ok(())
    }

    /// Same revision: refresh dynamic instance transforms and the refitted
    /// instance BVH.
    fn update_dynamic(&mut self, world: &AcousticWorld) -> Result<(), BackendError> {
        let view = world.gpu_view();
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            error::unavailable(format!("{} is not initialized", tier_name(self.tier)))
        })?;
        let Some(state) = self.world.as_mut() else {
            return Ok(());
        };
        state.dynamic_generation = world.dynamic_generation();
        if view.instances.len() != state.scene.instances.len()
            || view.top_nodes.len() != state.scene.top_nodes.len()
        {
            return Err(error::failed("world changed shape without a new revision"));
        }
        let changed: Vec<usize> = view
            .instances
            .iter()
            .zip(&state.scene.instances)
            .enumerate()
            .filter(|(_, (new, old))| new != old)
            .map(|(i, _)| i)
            .collect();
        if changed.is_empty() && view.top_nodes == state.scene.top_nodes.as_slice() {
            return Ok(());
        }
        for &i in &changed {
            let (new, old) = (view.instances[i], state.scene.instances[i]);
            if (new.node_offset, new.triangle_offset) != (old.node_offset, old.triangle_offset) {
                return Err(error::failed(
                    "instance mesh changed without a new revision",
                ));
            }
            if new
                .world_from_object
                .iter()
                .chain(&new.object_from_world)
                .any(|v| !v.is_finite())
            {
                return Err(error::failed(format!(
                    "instance {i} transform is not finite"
                )));
            }
            state.scene.instances[i] = new;
        }
        state.scene.top_nodes.copy_from_slice(view.top_nodes);
        let scope = runtime
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let result = state
            .gpu
            .update_instances(&runtime.gpu, &state.scene, &changed);
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(error::failed(format!(
                "GPU validation error updating instances: {e}"
            )));
        }
        result
    }
}

/// Nearest hit of one [`GpuBackend::raycast_batch`] ray.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuHit {
    pub t: f32,
    pub instance: u32,
    /// Index into `GpuWorldView::triangles` (its `v1[3]` bits hold the
    /// original triangle index).
    pub triangle: u32,
}

impl GpuBackend {
    /// Nearest hits of `(origin, dir, t_max)` rays through this tier's
    /// intersection routine (all instance masks). For parity tests against the
    /// CPU BVH; not part of the propagation contract.
    #[doc(hidden)]
    pub fn raycast_batch(
        &mut self,
        world: &AcousticWorld,
        rays: &[([f32; 3], [f32; 3], f32)],
    ) -> Result<Vec<Option<GpuHit>>, BackendError> {
        self.sync_world(world)?;
        let timeout = self.timeout();
        let runtime = self.runtime()?;
        let state = self
            .world
            .as_ref()
            .ok_or_else(|| error::failed("world was not uploaded"))?;
        let mut hits = Vec::with_capacity(rays.len());
        for chunk in rays.chunks(65_536) {
            let slots: Vec<kernel::ProbeSlot> = chunk
                .iter()
                .map(|(o, d, t)| kernel::ProbeSlot::new(*o, *d, *t))
                .collect();
            let results =
                kernel::cast_rays(&runtime.gpu, &runtime.kernel, &state.gpu, &slots, timeout)?;
            for r in results {
                if r.aborted() {
                    return Err(error::failed("a probe ray aborted"));
                }
                hits.push(r.hit().map(|(t, triangle, instance)| GpuHit {
                    t,
                    instance,
                    triangle,
                }));
            }
        }
        self.check_device()?;
        Ok(hits)
    }
}

impl PropagationBackend for GpuBackend {
    fn info(&self) -> BackendInfo {
        // Hardware RT and the adapter are only claimed once the self-test passed.
        let runtime = self.runtime.as_ref();
        BackendInfo {
            kind: self.kind(),
            hardware_rt: self.tier == Tier::RayQuery && runtime.is_some(),
            adapter: runtime.map(|r| r.gpu.adapter_label()),
            driver: runtime.and_then(|r| r.gpu.driver_label()),
        }
    }

    fn initialize(&mut self) -> Result<(), BackendError> {
        self.runtime = None;
        self.world = None;
        let gpu = GpuDevice::open(self.tier, &self.options, self.api)?;
        let kernel = Kernel::new(&gpu, self.tier)?;
        let self_test = selftest::run(
            &gpu,
            &kernel,
            self.tier,
            self.options.force_self_test_failure,
        )?;
        if let Some(message) = gpu.failure() {
            return Err(error::unavailable(format!(
                "{} self-test left a device failure: {message}",
                tier_name(self.tier)
            )));
        }
        self.runtime = Some(Runtime {
            gpu,
            kernel,
            self_test,
        });
        Ok(())
    }

    fn sync_world(&mut self, world: &AcousticWorld) -> Result<(), BackendError> {
        self.runtime()?;
        let result = match &self.world {
            Some(state)
                if state.revision == world.revision()
                    && state.dynamic_generation == world.dynamic_generation() =>
            {
                Ok(())
            }
            Some(state) if state.revision == world.revision() => self.update_dynamic(world),
            _ => self.upload_world(world),
        };
        let result = result.map_err(|e| self.fatal(e));
        self.check_device()?;
        result
    }

    fn trace(
        &mut self,
        world: &AcousticWorld,
        settings: &TraceSettings,
        jobs: &[EnergyJob],
        out: &mut Vec<EnergyHistogram>,
    ) -> Result<TraceStats, BackendError> {
        out.clear();
        kernel::validate_request(settings, jobs)?;
        if jobs.is_empty() {
            return Ok(TraceStats::default());
        }
        self.sync_world(world)?;
        let timeout = self.timeout();
        let runtime = self.runtime()?;
        let state = self
            .world
            .as_ref()
            .ok_or_else(|| error::failed("world was not uploaded"))?;
        let result = kernel::trace_blocking(
            &runtime.gpu,
            &runtime.kernel,
            &state.gpu,
            settings,
            jobs,
            timeout,
        );
        let trace = match result {
            Ok(trace) => trace,
            Err(e) => {
                let e = self.fatal(e);
                self.check_device()?;
                return Err(e);
            }
        };
        self.check_device()?;
        for (i, h) in trace.histograms.iter().enumerate() {
            let finite = h.mean_free_path_m.is_finite()
                && h.escaped.is_finite()
                && h.bins.iter().flatten().all(|v| v.is_finite());
            if !finite {
                return Err(self.fatal(error::failed(format!("job {i}: non-finite histogram"))));
            }
        }
        out.extend(trace.histograms);
        Ok(TraceStats {
            rays: jobs.iter().map(|j| u64::from(j.rays)).sum(),
            segments: trace.segments,
            elapsed_us: trace.elapsed.as_micros().min(u128::from(u32::MAX)) as u32,
        })
    }
}
