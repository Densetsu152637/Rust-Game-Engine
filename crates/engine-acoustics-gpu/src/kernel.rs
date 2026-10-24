//! WGSL module composition, compute pipelines, dispatch and readback.

use crate::device::{GpuDevice, Tier};
use crate::error;
use crate::upload::GpuScene;
use bytemuck::{Pod, Zeroable};
use engine_acoustics::{BANDS, BackendError, EnergyHistogram, EnergyJob, TraceSettings};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const COMMON: &str = include_str!("shaders/common.wgsl");
const INTERSECT_RAY_QUERY: &str = include_str!("shaders/intersect_rayquery.wgsl");
const INTERSECT_BVH: &str = include_str!("shaders/intersect_bvh.wgsl");

/// Job pieces per dispatch (section 6.3).
pub(crate) const MAX_JOBS_PER_DISPATCH: usize = 64;
/// Default upper bound on traced segments (rays x (bounces + 1)) per queue
/// submission. 2^24 segments is about 43 ms of compute-tier traversal of a
/// 100k-triangle mesh on the RTX 5080 (3.9e8 segments/s measured) and about 2 ms
/// on the ray-query tier, so one request never holds the shared GPU for long
/// and stays far from the OS GPU watchdog (2 s on Windows).
pub(crate) const DEFAULT_SEGMENT_BUDGET: u64 = 1 << 24;
/// Per-job ray cap on the GPU (section 5.3).
pub(crate) const MAX_GPU_RAYS: u32 = 1_048_576;
pub(crate) const MAX_BOUNCES: u16 = 256;
const WORKGROUP: u32 = 64;
const STATS_WORDS: usize = 16;
const FLAG_SATURATED: u32 = 1;
const FLAG_NONFINITE: u32 = 2;
/// Fixed point: 2^40 counts per unit of energy, 2^16 per metre.
const ENERGY_SCALE: f64 = (1u64 << 40) as f64;
const LENGTH_SCALE: f64 = (1u64 << 16) as f64;

/// The composed WGSL source for `tier` (the `enable` directive must lead).
pub(crate) fn shader_source(tier: Tier) -> String {
    match tier {
        Tier::RayQuery => format!("{INTERSECT_RAY_QUERY}\n{COMMON}"),
        Tier::Compute => format!("{INTERSECT_BVH}\n{COMMON}"),
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct SettingsUniform {
    air: [f32; 4],
    bin_s: f32,
    bins: u32,
    speed_of_sound: f32,
    job_count: u32,
    top_base: u32,
    instance_count: u32,
    probe_count: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct JobRecord {
    emitter: [f32; 4],
    receiver: [f32; 4],
    /// All rays of the job (deposits are weighted by `1 / rays`).
    rays: u32,
    max_bounces: u32,
    seed_lo: u32,
    seed_hi: u32,
    /// The rays `first_ray .. first_ray + count` traced by this dispatch.
    first_ray: u32,
    count: u32,
    _pad: [u32; 2],
}

/// One probe ray and its nearest hit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub(crate) struct ProbeSlot {
    /// xyz origin, w = t_max.
    pub origin: [f32; 4],
    pub dir: [f32; 4],
    /// kind (0 miss, 1 triangle, 2 aborted), t bits, triangle, instance.
    pub result: [u32; 4],
}
impl ProbeSlot {
    pub fn new(origin: [f32; 3], dir: [f32; 3], t_max: f32) -> Self {
        Self {
            origin: [origin[0], origin[1], origin[2], t_max],
            dir: [dir[0], dir[1], dir[2], 0.0],
            result: [0; 4],
        }
    }
    pub fn hit(&self) -> Option<(f32, u32, u32)> {
        (self.result[0] == 1).then(|| {
            (
                f32::from_bits(self.result[1]),
                self.result[2],
                self.result[3],
            )
        })
    }
    pub fn aborted(&self) -> bool {
        self.result[0] == 2
    }
}

/// Pipelines for one tier on one device.
pub(crate) struct Kernel {
    tier: Tier,
    layout: wgpu::BindGroupLayout,
    trace: wgpu::ComputePipeline,
    probe: wgpu::ComputePipeline,
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Kernel {
    pub fn new(gpu: &GpuDevice, tier: Tier) -> Result<Self, BackendError> {
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let source = shader_source(tier);
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("acoustics energy tracer"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            storage_entry(1, true),
            storage_entry(2, true),
            storage_entry(3, true),
            storage_entry(4, true),
            storage_entry(5, false),
            storage_entry(6, false),
            storage_entry(8, false),
        ];
        entries.push(match tier {
            Tier::RayQuery => wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::AccelerationStructure {
                    vertex_return: false,
                },
                count: None,
            },
            Tier::Compute => storage_entry(7, true),
        });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("acoustics tracer bindings"),
                entries: &entries,
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("acoustics tracer layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let pipeline = |entry: &str| {
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
        };
        let trace = pipeline("trace_energy");
        let probe = pipeline("cast_rays");
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(error::unavailable(format!(
                "{} shader or pipeline creation failed: {e}",
                crate::device::tier_name(tier)
            )));
        }
        Ok(Self {
            tier,
            layout,
            trace,
            probe,
        })
    }

    fn bind_group(
        &self,
        gpu: &GpuDevice,
        scene: &GpuScene,
        buffers: &DispatchBuffers,
    ) -> Result<wgpu::BindGroup, BackendError> {
        let DispatchBuffers {
            settings,
            jobs,
            histogram,
            stats,
            probe,
        } = buffers;
        let scene_binding = match self.tier {
            Tier::RayQuery => scene
                .accel
                .as_ref()
                .ok_or_else(|| error::failed("ray-query scene has no TLAS"))?
                .tlas
                .as_binding(),
            Tier::Compute => scene
                .nodes
                .as_ref()
                .ok_or_else(|| error::failed("compute scene has no BVH buffer"))?
                .as_entire_binding(),
        };
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        Ok(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("acoustics tracer bind group"),
            layout: &self.layout,
            entries: &[
                entry(0, settings.as_entire_binding()),
                entry(1, jobs.as_entire_binding()),
                entry(2, scene.triangles.as_entire_binding()),
                entry(3, scene.instances.as_entire_binding()),
                entry(4, scene.materials.as_entire_binding()),
                entry(5, histogram.as_entire_binding()),
                entry(6, stats.as_entire_binding()),
                entry(7, scene_binding),
                entry(8, probe.as_entire_binding()),
            ],
        }))
    }
}

/// Per-dispatch buffers bound next to the scene.
struct DispatchBuffers {
    settings: wgpu::Buffer,
    jobs: wgpu::Buffer,
    histogram: wgpu::Buffer,
    stats: wgpu::Buffer,
    probe: wgpu::Buffer,
}

/// A submitted readback that has not completed yet.
pub(crate) struct PendingReadback {
    buffer: wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    state: Arc<AtomicU8>,
}
const MAP_PENDING: u8 = 0;
const MAP_DONE: u8 = 1;
const MAP_FAILED: u8 = 2;

pub(crate) enum ReadError {
    /// Still running; the readback is dropped (wgpu keeps it alive until the
    /// GPU finishes) and [`QueueGuard`] refuses new submissions until it is done.
    Timeout,
    Failed(BackendError),
}

impl PendingReadback {
    /// Waits up to `timeout` for the readback.
    pub fn wait(self, gpu: &GpuDevice, timeout: Duration) -> Result<Vec<u8>, ReadError> {
        let polled = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(self.submission.clone()),
            timeout: Some(timeout),
        });
        if let Some(message) = gpu.failure() {
            return Err(ReadError::Failed(error::failed(message)));
        }
        match polled {
            Ok(_) => {}
            Err(wgpu::PollError::Timeout) => return Err(ReadError::Timeout),
            Err(e) => {
                return Err(ReadError::Failed(error::failed(format!(
                    "GPU poll failed: {e}"
                ))));
            }
        }
        if self.state.load(Ordering::Acquire) == MAP_PENDING {
            // The map callback runs on the next maintenance pass after the
            // submission completed.
            let _ = gpu.device.poll(wgpu::PollType::Poll);
        }
        match self.state.load(Ordering::Acquire) {
            MAP_DONE => {
                let bytes = self
                    .buffer
                    .slice(..)
                    .get_mapped_range()
                    .map(|view| view.to_vec())
                    .map_err(|e| ReadError::Failed(error::failed(format!("GPU readback: {e}"))));
                self.buffer.unmap();
                bytes
            }
            MAP_FAILED => Err(ReadError::Failed(error::failed(
                "GPU readback mapping failed",
            ))),
            _ => Err(ReadError::Timeout),
        }
    }
}

fn submit_with_readback(
    gpu: &GpuDevice,
    mut encoder: wgpu::CommandEncoder,
    sources: &[(&wgpu::Buffer, u64)],
) -> Result<PendingReadback, BackendError> {
    let total: u64 = sources.iter().map(|(_, size)| size).sum();
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("acoustics readback"),
        size: total,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut offset = 0;
    for (buffer, size) in sources {
        encoder.copy_buffer_to_buffer(buffer, 0, &readback, offset, *size);
        offset += size;
    }
    let submission = gpu.queue.submit(Some(encoder.finish()));
    let state = Arc::new(AtomicU8::new(MAP_PENDING));
    let flag = state.clone();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            flag.store(
                if result.is_ok() { MAP_DONE } else { MAP_FAILED },
                Ordering::Release,
            );
        });
    Ok(PendingReadback {
        buffer: readback,
        submission,
        state,
    })
}

fn uniform(gpu: &GpuDevice, settings: &SettingsUniform) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    gpu.device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("acoustics settings"),
            contents: bytemuck::bytes_of(settings),
            usage: wgpu::BufferUsages::UNIFORM,
        })
}

fn zeroed_storage(gpu: &GpuDevice, label: &str, size: u64) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(16),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn init_storage(gpu: &GpuDevice, label: &str, bytes: &[u8]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    let padded;
    let contents = if bytes.len() < 16 {
        let mut p = [0u8; 16];
        p[..bytes.len()].copy_from_slice(bytes);
        padded = p;
        &padded[..]
    } else {
        bytes
    };
    gpu.device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        })
}

/// Rejects settings and jobs outside the documented limits (section 5.3).
pub(crate) fn validate_request(
    settings: &TraceSettings,
    jobs: &[EnergyJob],
) -> Result<(), BackendError> {
    let finite3 = |v: [f32; 3]| v.iter().all(|c| c.is_finite());
    if !(0.0005..=0.010).contains(&settings.bin_s) {
        return Err(error::invalid("histogram bin must be 0.5-10 ms"));
    }
    if settings.bins == 0 || settings.bins as f32 * settings.bin_s > 4.0 + 1e-4 {
        return Err(error::invalid(
            "histogram must have 1 or more bins and at most 4 s",
        ));
    }
    if !(settings.speed_of_sound.is_finite() && (300.0..=400.0).contains(&settings.speed_of_sound))
    {
        return Err(error::invalid("speed of sound must be 300-400 m/s"));
    }
    if settings
        .air_np_per_m
        .iter()
        .any(|a| !(a.is_finite() && *a >= 0.0))
    {
        return Err(error::invalid(
            "air absorption must be finite and non-negative",
        ));
    }
    for (i, job) in jobs.iter().enumerate() {
        if !(1..=MAX_GPU_RAYS).contains(&job.rays) {
            return Err(error::invalid(format!(
                "job {i}: rays must be 1-{MAX_GPU_RAYS}"
            )));
        }
        if job.max_bounces > MAX_BOUNCES {
            return Err(error::invalid(format!(
                "job {i}: bounces must be at most {MAX_BOUNCES}"
            )));
        }
        if !(finite3(job.emitter) && finite3(job.receiver)) {
            return Err(error::invalid(format!("job {i}: positions must be finite")));
        }
        if !(job.receiver_radius_m.is_finite() && job.receiver_radius_m > 0.0) {
            return Err(error::invalid(format!(
                "job {i}: receiver radius must be positive"
            )));
        }
    }
    Ok(())
}

/// Rays `first_ray .. first_ray + rays` of job `job`, traced in one dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub job: usize,
    pub first_ray: u32,
    pub rays: u32,
}

/// Estimated segments of one ray: the emission plus one per reflection.
fn ray_cost(job: &EnergyJob) -> u64 {
    u64::from(job.max_bounces) + 1
}

/// Splits `jobs` into dispatches of at most [`MAX_JOBS_PER_DISPATCH`] pieces and
/// at most `budget` estimated segments each (a single ray may exceed a budget
/// smaller than its cost). Pieces keep job order and ray order, so every ray is
/// traced exactly once.
pub(crate) fn plan_dispatches(jobs: &[EnergyJob], budget: u64) -> Vec<Vec<Piece>> {
    let budget = budget.max(1);
    let mut dispatches = Vec::new();
    let mut current: Vec<Piece> = Vec::new();
    let mut used = 0u64;
    for (job_index, job) in jobs.iter().enumerate() {
        let cost = ray_cost(job);
        let mut first = 0u32;
        while first < job.rays {
            if current.len() == MAX_JOBS_PER_DISPATCH
                || (!current.is_empty() && used + cost > budget)
            {
                dispatches.push(std::mem::take(&mut current));
                used = 0;
            }
            let fit = ((budget - used.min(budget)) / cost).max(1);
            let take = u64::from(job.rays - first).min(fit) as u32;
            current.push(Piece {
                job: job_index,
                first_ray: first,
                rays: take,
            });
            used += u64::from(take) * cost;
            first += take;
        }
    }
    if !current.is_empty() {
        dispatches.push(current);
    }
    dispatches
}

/// Keeps at most one submission outstanding after a readback timeout: until
/// the timed-out work completes, new traces return `acoustics.backend.timeout`
/// without submitting anything.
#[derive(Debug, Default)]
pub(crate) struct QueueGuard {
    outstanding: Option<Arc<AtomicBool>>,
    /// Trace and probe submissions made through this guard.
    pub submitted: u64,
}

impl QueueGuard {
    /// `true` while timed-out work is still running on the GPU.
    pub fn is_blocked(&mut self, gpu: &GpuDevice) -> bool {
        if let Some(done) = &self.outstanding {
            let _ = gpu.device.poll(wgpu::PollType::Poll);
            if !done.load(Ordering::Acquire) {
                return true;
            }
            self.outstanding = None;
        }
        false
    }

    fn ready(&mut self, gpu: &GpuDevice) -> Result<(), BackendError> {
        if self.is_blocked(gpu) {
            return Err(error::timeout(
                "an earlier GPU submission is still running after a readback timeout; \
                 nothing new was submitted",
            ));
        }
        Ok(())
    }

    fn timed_out(&mut self, gpu: &GpuDevice) {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        gpu.queue
            .on_submitted_work_done(move || flag.store(true, Ordering::Release));
        self.outstanding = Some(done);
    }

    /// Waits for `pending`, recording a timeout.
    fn wait(
        &mut self,
        gpu: &GpuDevice,
        pending: PendingReadback,
        timeout: Duration,
        what: &str,
    ) -> Result<Vec<u8>, BackendError> {
        match pending.wait(gpu, timeout) {
            Ok(bytes) => Ok(bytes),
            Err(ReadError::Timeout) => {
                self.timed_out(gpu);
                Err(error::timeout(format!(
                    "{what} readback exceeded {} ms",
                    timeout.as_millis()
                )))
            }
            Err(ReadError::Failed(e)) => Err(e),
        }
    }
}

/// Submits one dispatch of at most [`MAX_JOBS_PER_DISPATCH`] pieces.
fn submit_trace(
    gpu: &GpuDevice,
    kernel: &Kernel,
    scene: &GpuScene,
    settings: &TraceSettings,
    jobs: &[EnergyJob],
    pieces: &[Piece],
) -> Result<PendingReadback, BackendError> {
    debug_assert!(!pieces.is_empty() && pieces.len() <= MAX_JOBS_PER_DISPATCH);
    let records: Vec<JobRecord> = pieces
        .iter()
        .map(|piece| {
            let job = &jobs[piece.job];
            JobRecord {
                emitter: [job.emitter[0], job.emitter[1], job.emitter[2], 0.0],
                receiver: [
                    job.receiver[0],
                    job.receiver[1],
                    job.receiver[2],
                    job.receiver_radius_m,
                ],
                rays: job.rays,
                max_bounces: u32::from(job.max_bounces),
                seed_lo: job.seed as u32,
                seed_hi: (job.seed >> 32) as u32,
                first_ray: piece.first_ray,
                count: piece.rays,
                _pad: [0; 2],
            }
        })
        .collect();
    let uniform_data = SettingsUniform {
        air: settings.air_np_per_m,
        bin_s: settings.bin_s,
        bins: settings.bins,
        speed_of_sound: settings.speed_of_sound,
        job_count: pieces.len() as u32,
        top_base: scene.top_base,
        instance_count: scene.instance_count,
        probe_count: 0,
        _pad: 0,
    };
    let histogram_size = pieces.len() as u64 * u64::from(settings.bins) * BANDS as u64 * 8;
    let stats_size = (pieces.len() * STATS_WORDS * 4) as u64;
    if histogram_size > gpu.limits.max_storage_buffer_binding_size {
        return Err(error::invalid(
            "histogram exceeds the device storage binding size",
        ));
    }
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let buffers = DispatchBuffers {
        settings: uniform(gpu, &uniform_data),
        jobs: init_storage(gpu, "acoustics jobs", bytemuck::cast_slice(&records)),
        histogram: zeroed_storage(gpu, "acoustics histogram", histogram_size),
        stats: zeroed_storage(gpu, "acoustics job stats", stats_size),
        probe: zeroed_storage(gpu, "acoustics probe (unused)", 48),
    };
    let bind_group = kernel.bind_group(gpu, scene, &buffers)?;
    let max_rays = pieces.iter().map(|p| p.rays).max().unwrap_or(0);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("acoustics trace"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("acoustics trace"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&kernel.trace);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(max_rays.div_ceil(WORKGROUP), pieces.len() as u32, 1);
    }
    let pending = submit_with_readback(
        gpu,
        encoder,
        &[
            (&buffers.histogram, histogram_size),
            (&buffers.stats, stats_size),
        ],
    )?;
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(error::failed(format!("GPU validation error in trace: {e}")));
    }
    Ok(pending)
}

fn read_u64(words: &[u32], index: usize) -> u64 {
    u64::from(words[index]) | (u64::from(words[index + 1]) << 32)
}

/// Integer accumulators of one job across its pieces. Integer sums make the
/// result independent of how the job was split.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobTotals {
    bins: Vec<[u64; BANDS]>,
    escaped: [u64; BANDS],
    segments: u64,
    free_paths: u64,
    free_length: u64,
    aborted: u64,
}

impl JobTotals {
    fn new(bins: usize) -> Self {
        Self {
            bins: vec![[0; BANDS]; bins],
            ..Default::default()
        }
    }

    /// The histogram of a job none of whose rays can hit anything (empty
    /// world): every ray escapes with its full energy on its first segment, as
    /// on the CPU.
    pub fn all_escaped(job: &EnergyJob, settings: &TraceSettings) -> EnergyHistogram {
        EnergyHistogram {
            bin_s: settings.bin_s,
            bins: vec![[0.0; BANDS]; settings.bins as usize],
            rays: job.rays,
            escaped: 1.0,
            aborted: 0,
            mean_free_path_m: 0.0,
        }
    }

    fn finish(&self, job: &EnergyJob, settings: &TraceSettings) -> EnergyHistogram {
        let escaped = self
            .escaped
            .iter()
            .map(|&v| v as f64 / ENERGY_SCALE)
            .sum::<f64>()
            / BANDS as f64;
        EnergyHistogram {
            bin_s: settings.bin_s,
            bins: self
                .bins
                .iter()
                .map(|b| b.map(|v| (v as f64 / ENERGY_SCALE) as f32))
                .collect(),
            rays: job.rays,
            escaped: escaped as f32,
            aborted: self.aborted.min(u64::from(u32::MAX)) as u32,
            mean_free_path_m: if self.free_paths > 0 {
                (self.free_length as f64 / LENGTH_SCALE / self.free_paths as f64) as f32
            } else {
                0.0
            },
        }
    }
}

/// Adds a completed dispatch readback to the totals of its pieces' jobs.
fn accumulate(
    bytes: &[u8],
    settings: &TraceSettings,
    pieces: &[Piece],
    totals: &mut [JobTotals],
) -> Result<(), BackendError> {
    let words: &[u32] = bytemuck::try_cast_slice(bytes)
        .map_err(|_| error::failed("GPU readback has the wrong size"))?;
    let bins = settings.bins as usize;
    let histogram_words = pieces.len() * bins * BANDS * 2;
    if words.len() != histogram_words + pieces.len() * STATS_WORDS {
        return Err(error::failed("GPU readback has the wrong size"));
    }
    let (histogram, stats) = words.split_at(histogram_words);
    for (p, piece) in pieces.iter().enumerate() {
        let s = &stats[p * STATS_WORDS..(p + 1) * STATS_WORDS];
        if s[1] & FLAG_SATURATED != 0 {
            return Err(error::failed(format!(
                "job {}: histogram fixed-point accumulator saturated",
                piece.job
            )));
        }
        if s[1] & FLAG_NONFINITE != 0 {
            return Err(error::failed(format!(
                "job {}: non-finite energy on the GPU",
                piece.job
            )));
        }
        let total = &mut totals[piece.job];
        total.aborted += u64::from(s[0]);
        total.segments += read_u64(s, 2);
        total.free_paths += read_u64(s, 4);
        total.free_length += read_u64(s, 6);
        for (b, escaped) in total.escaped.iter_mut().enumerate() {
            *escaped += read_u64(s, 8 + 2 * b);
        }
        let base = p * bins * BANDS * 2;
        for (bin, cell) in total.bins.iter_mut().enumerate() {
            for (band, value) in cell.iter_mut().enumerate() {
                *value += read_u64(histogram, base + (bin * BANDS + band) * 2);
            }
        }
    }
    Ok(())
}

/// Histograms of a blocking trace and its cost.
pub(crate) struct RawTrace {
    pub histograms: Vec<EnergyHistogram>,
    pub segments: u64,
    pub elapsed: Duration,
}

/// Casts probe rays and returns the slots with their results (blocking up to
/// `timeout`).
pub(crate) fn cast_rays(
    gpu: &GpuDevice,
    kernel: &Kernel,
    scene: &GpuScene,
    rays: &[ProbeSlot],
    timeout: Duration,
    guard: &mut QueueGuard,
) -> Result<Vec<ProbeSlot>, BackendError> {
    guard.ready(gpu)?;
    let uniform_data = SettingsUniform {
        bin_s: 0.001,
        bins: 1,
        speed_of_sound: 343.0,
        top_base: scene.top_base,
        instance_count: scene.instance_count,
        probe_count: rays.len() as u32,
        ..Default::default()
    };
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let buffers = DispatchBuffers {
        settings: uniform(gpu, &uniform_data),
        jobs: zeroed_storage(gpu, "acoustics jobs (unused)", 48),
        histogram: zeroed_storage(gpu, "acoustics histogram (unused)", 16),
        stats: zeroed_storage(gpu, "acoustics stats (unused)", 64),
        probe: init_storage(gpu, "acoustics probe rays", bytemuck::cast_slice(rays)),
    };
    let bind_group = kernel.bind_group(gpu, scene, &buffers)?;
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("acoustics probe"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("acoustics probe"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&kernel.probe);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups((rays.len() as u32).div_ceil(WORKGROUP), 1, 1);
    }
    let size = size_of_val(rays) as u64;
    let pending = submit_with_readback(gpu, encoder, &[(&buffers.probe, size)])?;
    guard.submitted += 1;
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(error::failed(format!("GPU validation error in probe: {e}")));
    }
    let bytes = guard.wait(gpu, pending, timeout, "probe")?;
    Ok(bytemuck::pod_collect_to_vec(&bytes))
}

/// Traces `jobs` end to end (submit to decoded result), one dispatch per
/// [`plan_dispatches`] entry, waiting for each before submitting the next.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_blocking(
    gpu: &GpuDevice,
    kernel: &Kernel,
    scene: &GpuScene,
    settings: &TraceSettings,
    jobs: &[EnergyJob],
    timeout: Duration,
    segment_budget: u64,
    guard: &mut QueueGuard,
) -> Result<RawTrace, BackendError> {
    validate_request(settings, jobs)?;
    let start = Instant::now();
    let mut totals: Vec<JobTotals> = jobs
        .iter()
        .map(|_| JobTotals::new(settings.bins as usize))
        .collect();
    for pieces in plan_dispatches(jobs, segment_budget) {
        guard.ready(gpu)?;
        let pending = submit_trace(gpu, kernel, scene, settings, jobs, &pieces)?;
        guard.submitted += 1;
        let bytes = guard.wait(gpu, pending, timeout, "trace")?;
        accumulate(&bytes, settings, &pieces, &mut totals)?;
    }
    Ok(RawTrace {
        histograms: totals
            .iter()
            .zip(jobs)
            .map(|(t, job)| t.finish(job, settings))
            .collect(),
        segments: totals.iter().map(|t| t.segments).sum(),
        elapsed: start.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(rays: u32, max_bounces: u16) -> EnergyJob {
        EnergyJob {
            emitter: [0.0; 3],
            receiver: [1.0; 3],
            receiver_radius_m: 1.0,
            rays,
            max_bounces,
            seed: 1,
        }
    }

    /// Every ray of every job appears exactly once, in order, and no dispatch
    /// exceeds the budget or the piece limit.
    fn check_plan(jobs: &[EnergyJob], budget: u64) -> Vec<Vec<Piece>> {
        let plan = plan_dispatches(jobs, budget);
        let mut next = vec![0u32; jobs.len()];
        for dispatch in &plan {
            assert!(!dispatch.is_empty() && dispatch.len() <= MAX_JOBS_PER_DISPATCH);
            let cost: u64 = dispatch
                .iter()
                .map(|p| u64::from(p.rays) * ray_cost(&jobs[p.job]))
                .sum();
            let single_ray = dispatch.len() == 1 && dispatch[0].rays == 1;
            assert!(
                cost <= budget || single_ray,
                "dispatch cost {cost} > {budget}"
            );
            for piece in dispatch {
                assert!(piece.rays > 0);
                assert_eq!(piece.first_ray, next[piece.job], "rays out of order");
                next[piece.job] += piece.rays;
            }
        }
        for (j, job) in jobs.iter().enumerate() {
            assert_eq!(next[j], job.rays, "job {j} rays not all planned");
        }
        plan
    }

    #[test]
    fn worst_case_request_is_split_under_the_default_budget() {
        // 64 jobs x 1M rays x 256 bounces is about 1.7e10 segments.
        let jobs = vec![job(MAX_GPU_RAYS, MAX_BOUNCES); 64];
        let plan = check_plan(&jobs, DEFAULT_SEGMENT_BUDGET);
        let total: u64 = jobs.iter().map(|j| u64::from(j.rays) * ray_cost(j)).sum();
        assert!(plan.len() as u64 >= total / DEFAULT_SEGMENT_BUDGET);
        // Each 1M-ray job spans several submissions.
        assert!(plan.len() > 64 * 16);
    }

    #[test]
    fn small_requests_share_one_dispatch() {
        let jobs = vec![job(4096, 64); 3];
        let plan = check_plan(&jobs, DEFAULT_SEGMENT_BUDGET);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].len(), 3);
    }

    #[test]
    fn piece_limit_and_tiny_budgets_are_respected() {
        let many = vec![job(1, 0); 200];
        let plan = check_plan(&many, DEFAULT_SEGMENT_BUDGET);
        assert_eq!(plan.len(), 4); // 64 + 64 + 64 + 8
        check_plan(&[job(1000, 9), job(7, 255), job(3, 0)], 100);
        // A budget below one ray's cost still makes progress, one ray at a time.
        let plan = check_plan(&[job(5, 255)], 10);
        assert_eq!(plan.len(), 5);
        assert!(plan_dispatches(&[], DEFAULT_SEGMENT_BUDGET).is_empty());
    }
}
