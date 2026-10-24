//! The propagation engine run by `AcousticsService`: backend selection and
//! demotion, one propagation update (direct paths, image sources, late field),
//! fitting, publishing and status. It runs on the worker thread (threaded mode) or
//! in the caller (synchronous mode); the same code serves both.

use crate::direct::{ListenerFrame, direct_path};
use crate::fit::validate_histogram;
use crate::image_source::{ImageSourceSettings, MAX_ORDER, image_sources};
use crate::rng::job_seed;
use crate::trace_cpu::{BIN_RANGE_S, CPU_MAX_RAYS, MAX_BOUNCES, MAX_HISTOGRAM_S};
use crate::{
    AcousticState, AcousticStatus, AcousticWorld, AcousticsError, BackendError, BackendKind,
    BackendPreference, EnergyHistogram, EnergyJob, ParamHook, PropagationBackend,
    PropagationRequest, PropagationResult, PropagationSettings, PropagationSummary, Result,
    ServiceEvent, SourceResult, SourceStatus, SourceSummary, TraceSettings, TraceStats, fit_reverb,
    math, synthesize_ir,
};
use engine_audio::acoustic::{
    AcousticPublisher, ConvolutionKernel, MAX_ACOUSTIC_VOICES, MAX_CONVOLUTION_VOICES,
    MAX_REFLECTION_TAPS, ReflectionTap, ReverbParams, VoicePropagation,
};
use std::time::Instant;

/// Sources submitted per request (section 5.3).
pub const MAX_SUBMITTED_SOURCES: usize = 256;
/// Sources traced per update, ranked by `priority / distance` (section 5.3).
pub const MAX_TRACED_SOURCES: usize = 64;
/// Largest ray count per GPU job (section 5.3).
pub const GPU_MAX_RAYS: u32 = 1_048_576;
/// Consecutive non-fatal failures (for example readback timeouts) that demote.
pub const DEMOTE_AFTER_FAILURES: u32 = 3;
/// `AcousticStatus::fallback_reason` byte limit.
pub const MAX_REASON_BYTES: usize = 512;
/// `AcousticStatus::sources` entry limit.
pub const MAX_STATUS_SOURCES: usize = 32;
/// Queued `ServiceEvent`s kept for `drain_events`.
pub const MAX_QUEUED_EVENTS: usize = 64;

fn request_invalid(message: impl Into<String>) -> AcousticsError {
    AcousticsError::new("acoustics.request.invalid", message)
}

/// Checks propagation settings against section 5.3.
pub fn validate_settings(s: &PropagationSettings) -> Result<()> {
    let finite_pos = |v: f32| v.is_finite() && v > 0.0;
    if s.occlusion_rays > 4096 {
        return Err(request_invalid("occlusion_rays must be at most 4096"));
    }
    if s.reflection_order > MAX_ORDER {
        return Err(request_invalid("reflection_order must be at most 3"));
    }
    if usize::from(s.max_reflections) > MAX_REFLECTION_TAPS {
        return Err(request_invalid("max_reflections must be at most 16"));
    }
    if s.reflection_order > 0 && s.er_rays == 0 {
        return Err(request_invalid("er_rays must be positive with reflections"));
    }
    if s.reverb_rays > GPU_MAX_RAYS {
        return Err(request_invalid("reverb_rays must be at most 1,048,576"));
    }
    if s.max_bounces > MAX_BOUNCES {
        return Err(request_invalid("max_bounces must be at most 256"));
    }
    if !(s.histogram_bin_s.is_finite()
        && s.histogram_bin_s >= BIN_RANGE_S.0
        && s.histogram_bin_s <= BIN_RANGE_S.1)
    {
        return Err(request_invalid("histogram_bin_s must be 0.0005-0.01"));
    }
    if !(s.histogram_length_s.is_finite()
        && s.histogram_length_s >= s.histogram_bin_s
        && s.histogram_length_s <= MAX_HISTOGRAM_S)
    {
        return Err(request_invalid("histogram_length_s must be one bin to 4 s"));
    }
    if !(s.late_start_s.is_finite()
        && s.late_start_s >= 0.0
        && s.late_start_s < s.histogram_length_s)
    {
        return Err(request_invalid(
            "late_start_s must be >= 0 and shorter than the histogram",
        ));
    }
    if !finite_pos(s.receiver_radius_m) || s.receiver_radius_m > 100.0 {
        return Err(request_invalid("receiver_radius_m must be in (0, 100]"));
    }
    if !(s.speed_of_sound.is_finite() && (300.0..=400.0).contains(&s.speed_of_sound)) {
        return Err(request_invalid("speed_of_sound must be 300-400 m/s"));
    }
    if !s.air_np_per_m.iter().all(|m| m.is_finite() && *m >= 0.0) {
        return Err(request_invalid("air_np_per_m must be finite and >= 0"));
    }
    if s.time_budget_us == 0 {
        return Err(request_invalid("time_budget_us must be positive"));
    }
    Ok(())
}

/// Checks a request: settings, listener pose and at most 256 unique sources with
/// finite positions, radius `0..=100` m and priority `>= 0`.
pub fn validate_request(r: &PropagationRequest) -> Result<()> {
    validate_settings(&r.settings)?;
    ListenerFrame::new(&r.listener)?;
    if r.sources.len() > MAX_SUBMITTED_SOURCES {
        return Err(request_invalid("at most 256 sources per request"));
    }
    let mut ids: Vec<u64> = r.sources.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(request_invalid("source ids must be unique"));
    }
    for s in &r.sources {
        if !math::finite3(s.position) {
            return Err(request_invalid("source positions must be finite"));
        }
        if !(s.radius_m.is_finite() && (0.0..=100.0).contains(&s.radius_m)) {
            return Err(request_invalid("source radius_m must be 0-100"));
        }
        if !(s.priority.is_finite() && s.priority >= 0.0) {
            return Err(request_invalid("source priority must be finite and >= 0"));
        }
        if s.voice.is_some_and(|v| v.get() != s.id) {
            return Err(request_invalid(
                "a voice source's id must equal voice.get()",
            ));
        }
    }
    Ok(())
}

/// Truncates on a char boundary to at most `max` bytes.
pub(crate) fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

struct Slot {
    backend: Box<dyn PropagationBackend>,
    kind: BackendKind,
    initialized: bool,
    failed: bool,
    synced: Option<(u64, u64)>,
}

fn tiers(p: BackendPreference) -> &'static [BackendKind] {
    use BackendKind::*;
    match p {
        BackendPreference::Auto | BackendPreference::GpuRayQuery => &[GpuRayQuery, GpuCompute, Cpu],
        BackendPreference::GpuCompute => &[GpuCompute, Cpu],
        BackendPreference::Cpu => &[Cpu],
    }
}

/// How the engine schedules late-field traces and adapts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Clock {
    /// Every `every`-th step (synchronous mode, deterministic).
    Steps { every: u64 },
    /// At most `hz` per second, adapting ray counts to the time budget.
    Threaded { hz: f32 },
}

pub(crate) struct Engine {
    slots: Vec<Slot>,
    active: Option<usize>,
    preference: BackendPreference,
    reasons: Vec<String>,
    /// Runtime failure that caused the next `select`.
    pending_reason: Option<String>,
    pub world: Option<AcousticWorld>,
    pub request: Option<PropagationRequest>,
    publisher: AcousticPublisher,
    pub hook: Option<ParamHook>,
    pub convolution_rate: Option<u32>,
    kernel_warned: bool,
    clock: Clock,
    steps: u64,
    last_reverb_at: Option<Instant>,
    reverb_dirty: bool,
    reverb: ReverbParams,
    listener_ir: Option<engine_audio::acoustic::StereoImpulseResponse>,
    hero_irs: Vec<(u64, engine_audio::acoustic::StereoImpulseResponse)>,
    consecutive_failures: u32,
    ray_scale: f32,
    pub status: AcousticStatus,
    pub events: Vec<ServiceEvent>,
    produced: bool,
}

impl Engine {
    pub fn new(
        backends: Vec<Box<dyn PropagationBackend>>,
        preference: BackendPreference,
        publisher: AcousticPublisher,
        clock: Clock,
    ) -> Self {
        let slots = backends
            .into_iter()
            .map(|backend| Slot {
                kind: backend.info().kind,
                backend,
                initialized: false,
                failed: false,
                synced: None,
            })
            .collect();
        let mut engine = Self {
            slots,
            active: None,
            preference,
            reasons: Vec::new(),
            pending_reason: None,
            world: None,
            request: None,
            publisher,
            hook: None,
            convolution_rate: None,
            kernel_warned: false,
            clock,
            steps: 0,
            last_reverb_at: None,
            reverb_dirty: true,
            reverb: ReverbParams::default(),
            listener_ir: None,
            hero_irs: Vec::new(),
            consecutive_failures: 0,
            ray_scale: 1.0,
            status: AcousticStatus {
                requested: preference,
                state: AcousticState::Starting,
                ..AcousticStatus::default()
            },
            events: Vec::new(),
            produced: false,
        };
        engine.select();
        engine
    }

    fn event(&mut self, e: ServiceEvent) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.remove(0);
        }
        self.events.push(e);
    }

    /// Picks the first tier at or below the preference whose backend initializes.
    /// A `Cpu` preference only ever touches the CPU slot.
    fn select(&mut self) {
        let previous = self.active.map(|i| self.slots[i].kind);
        self.active = None;
        self.reasons.clear();
        self.reasons.extend(self.pending_reason.take());
        'tiers: for &tier in tiers(self.preference) {
            let mut any = false;
            for i in 0..self.slots.len() {
                if self.slots[i].kind != tier {
                    continue;
                }
                any = true;
                let slot = &mut self.slots[i];
                if slot.failed {
                    continue;
                }
                if !slot.initialized {
                    match slot.backend.initialize() {
                        Ok(()) => slot.initialized = true,
                        Err(e) => {
                            slot.failed = true;
                            self.reasons.push(format!("{tier:?}: {}", e.message));
                            continue;
                        }
                    }
                }
                self.active = Some(i);
                break 'tiers;
            }
            if !any {
                self.reasons
                    .push(format!("{tier:?}: no backend configured"));
            }
        }
        let now = self.active.map(|i| self.slots[i].kind);
        if previous != now && (previous.is_some() || self.produced) {
            let reason = self.reason().unwrap_or_default();
            self.event(ServiceEvent::BackendChanged {
                from: previous,
                to: now,
                reason,
            });
        }
        self.refresh_backend_status();
    }

    fn reason(&self) -> Option<String> {
        (!self.reasons.is_empty()).then(|| truncate(self.reasons.join("; "), MAX_REASON_BYTES))
    }

    fn requested_tier(&self) -> BackendKind {
        tiers(self.preference)[0]
    }

    fn refresh_backend_status(&mut self) {
        self.status.requested = self.preference;
        match self.active {
            Some(i) => {
                let info = self.slots[i].backend.info();
                self.status.active = Some(info.kind);
                self.status.adapter = info.adapter;
                self.status.hardware_rt = info.hardware_rt;
                let fallback = info.kind != self.requested_tier();
                self.status.fallback_reason = if fallback { self.reason() } else { None };
                self.status.state = if !self.produced {
                    AcousticState::Starting
                } else if fallback {
                    AcousticState::Fallback
                } else {
                    AcousticState::Active
                };
            }
            None => {
                self.status.active = None;
                self.status.adapter = None;
                self.status.hardware_rt = false;
                self.status.state = AcousticState::Failed;
                self.status.fallback_reason = self.reason();
            }
        }
    }

    /// Re-probes every backend (no automatic promotion happens otherwise).
    pub fn set_preference(&mut self, p: BackendPreference) {
        self.preference = p;
        for slot in &mut self.slots {
            slot.failed = false;
        }
        self.select();
    }

    pub fn set_world(&mut self, world: AcousticWorld) {
        self.status.world = world.stats();
        self.world = Some(world);
        self.reverb_dirty = true;
    }

    pub fn update_dynamic(&mut self, updates: &[(crate::InstanceId, crate::Affine3)]) {
        let Some(world) = self.world.as_mut() else {
            return;
        };
        for &(id, t) in updates {
            if let Err(e) = world.set_instance_transform(id, t) {
                self.events.push(ServiceEvent::Warning {
                    code: e.code,
                    message: truncate(e.message, MAX_REASON_BYTES),
                });
            }
        }
        self.status.world = world.stats();
    }

    /// Marks the active backend failed and selects the next tier.
    fn demote(&mut self, error: &BackendError) {
        if let Some(i) = self.active {
            let kind = self.slots[i].kind;
            // A re-probe must initialize (and self-test) it again.
            self.slots[i].failed = true;
            self.slots[i].initialized = false;
            self.slots[i].synced = None;
            self.pending_reason = Some(format!(
                "{kind:?} failed at runtime ({}): {}",
                error.code, error.message
            ));
            self.consecutive_failures = 0;
            self.select();
        }
    }

    /// Makes sure the active backend holds the current world.
    fn sync_active(&mut self) -> std::result::Result<(), BackendError> {
        let (Some(i), Some(world)) = (self.active, self.world.as_ref()) else {
            return Ok(());
        };
        let key = (world.revision(), world.dynamic_generation());
        if self.slots[i].synced != Some(key) {
            self.slots[i].backend.sync_world(world)?;
            self.slots[i].synced = Some(key);
        }
        Ok(())
    }

    /// Traces late-field jobs on the active tier, demoting through the tiers on
    /// failure. `Ok(None)` keeps the previous late field (a non-fatal failure).
    fn trace_jobs(
        &mut self,
        settings: &TraceSettings,
        jobs: &mut [EnergyJob],
    ) -> Result<Option<(Vec<EnergyHistogram>, TraceStats)>> {
        loop {
            let Some(i) = self.active else {
                return Err(AcousticsError::new(
                    "acoustics.backend.unavailable",
                    self.reason().unwrap_or_else(|| "no backend".into()),
                ));
            };
            let max = if self.slots[i].kind == BackendKind::Cpu {
                CPU_MAX_RAYS
            } else {
                GPU_MAX_RAYS
            };
            for job in jobs.iter_mut() {
                job.rays = job.rays.min(max);
            }
            let outcome = self.sync_active().and_then(|()| {
                let world = self.world.as_ref().expect("checked by the caller");
                let mut out = Vec::with_capacity(jobs.len());
                let stats = self.slots[i]
                    .backend
                    .trace(world, settings, jobs, &mut out)?;
                if out.len() != jobs.len() {
                    return Err(BackendError {
                        code: "acoustics.backend.failed",
                        message: "backend returned the wrong number of histograms".into(),
                        fatal: true,
                    });
                }
                for h in &out {
                    if h.bins.len() != settings.bins as usize {
                        return Err(BackendError {
                            code: "acoustics.backend.failed",
                            message: "histogram has the wrong number of bins".into(),
                            fatal: true,
                        });
                    }
                    validate_histogram(h).map_err(|e| BackendError {
                        code: e.code,
                        message: e.message,
                        fatal: true,
                    })?;
                }
                Ok((out, stats))
            });
            match outcome {
                Ok(v) => {
                    self.consecutive_failures = 0;
                    return Ok(Some(v));
                }
                Err(e) => {
                    if e.code == "acoustics.backend.timeout" {
                        self.status.gpu_timeouts += 1;
                    }
                    if e.code == "acoustics.request.invalid" && !e.fatal {
                        return Err(AcousticsError::new(e.code, e.message));
                    }
                    if !e.fatal {
                        self.consecutive_failures += 1;
                        if self.consecutive_failures < DEMOTE_AFTER_FAILURES {
                            return Ok(None);
                        }
                    }
                    self.demote(&e);
                }
            }
        }
    }

    fn reverb_due(&self) -> bool {
        if self.reverb_dirty {
            return true;
        }
        match self.clock {
            Clock::Steps { every } => self.steps.is_multiple_of(every.max(1)),
            Clock::Threaded { hz } => self
                .last_reverb_at
                .is_none_or(|t| t.elapsed().as_secs_f32() >= 1.0 / hz),
        }
    }

    /// One propagation update and publish. Requires a world and a request.
    pub fn update(&mut self) -> Result<PropagationResult> {
        let start = Instant::now();
        let (Some(world), Some(request)) = (self.world.as_ref(), self.request.as_ref()) else {
            return Err(request_invalid(
                "a world and a request are needed before propagation",
            ));
        };
        let request = request.clone();
        let s = request.settings;
        let listener = ListenerFrame::new(&request.listener)?;
        // Rank by priority / distance; ties keep request order.
        let mut ranked: Vec<usize> = (0..request.sources.len()).collect();
        let score = |i: usize| {
            let src = &request.sources[i];
            let d = math::length(math::sub(src.position, listener.position)).max(0.1);
            src.priority / d
        };
        ranked.sort_by(|&a, &b| score(b).total_cmp(&score(a)).then(a.cmp(&b)));
        ranked.truncate(MAX_TRACED_SOURCES);
        ranked.sort_unstable();
        let er = ImageSourceSettings {
            order: s.reflection_order,
            er_rays: s.er_rays,
            max_paths: usize::from(s.max_reflections),
            speed_of_sound: s.speed_of_sound,
            air_np_per_m: s.air_np_per_m,
        };
        let mut sources: Vec<SourceResult> = ranked
            .iter()
            .map(|&i| {
                let src = &request.sources[i];
                let direct = direct_path(
                    world,
                    &listener,
                    src.position,
                    src.radius_m,
                    s.occlusion_rays,
                    src.occlusion,
                );
                let reflections = if src.reflections {
                    image_sources(world, src.position, &listener, &er)
                        .into_iter()
                        .map(|p| p.path)
                        .collect()
                } else {
                    Vec::new()
                };
                SourceResult {
                    id: src.id,
                    direct,
                    reflections,
                    histogram: None,
                }
            })
            .collect();
        let world_revision = world.revision();

        let mut stats = TraceStats::default();
        let mut listener_field = None;
        if self.reverb_due() && s.reverb_rays > 0 {
            let rays = match self.clock {
                Clock::Threaded { .. } => ((s.reverb_rays as f32 * self.ray_scale) as u32).max(64),
                Clock::Steps { .. } => s.reverb_rays,
            };
            let bins = (s.histogram_length_s / s.histogram_bin_s).round().max(1.0) as u32;
            let trace_settings = TraceSettings {
                bin_s: s.histogram_bin_s,
                bins,
                speed_of_sound: s.speed_of_sound,
                air_np_per_m: s.air_np_per_m,
            };
            let mut jobs = vec![EnergyJob {
                emitter: listener.position,
                receiver: listener.position,
                receiver_radius_m: s.receiver_radius_m,
                rays,
                max_bounces: s.max_bounces,
                seed: job_seed(s.seed, 0),
            }];
            let heroes: Vec<usize> = ranked
                .iter()
                .enumerate()
                .filter(|(_, i)| request.sources[**i].hero && request.sources[**i].reverb)
                .map(|(k, _)| k)
                .take(MAX_CONVOLUTION_VOICES)
                .collect();
            for &k in &heroes {
                let src = &request.sources[ranked[k]];
                jobs.push(EnergyJob {
                    emitter: src.position,
                    seed: job_seed(s.seed, 1 + (src.id as u32 ^ (src.id >> 32) as u32)),
                    ..jobs[0]
                });
            }
            if let Some((hists, trace_stats)) = self.trace_jobs(&trace_settings, &mut jobs)? {
                stats = trace_stats;
                let mut hists = hists.into_iter();
                let field = hists.next().expect("one histogram per job");
                self.reverb = fit_reverb(&field, s.late_start_s)?;
                for (&k, h) in heroes.iter().zip(hists) {
                    sources[k].histogram = Some(h);
                }
                self.status.aborted_rays += u64::from(field.aborted)
                    + sources
                        .iter()
                        .filter_map(|r| r.histogram.as_ref())
                        .map(|h| u64::from(h.aborted))
                        .sum::<u64>();
                self.prepare_irs(&field, &sources, s.seed);
                listener_field = Some(field);
                self.reverb_dirty = false;
                self.last_reverb_at = Some(Instant::now());
            }
        }

        let backend = self.active.map_or(BackendKind::Cpu, |i| self.slots[i].kind);
        let elapsed_us = start.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        let budget_exceeded = elapsed_us > s.time_budget_us;
        let result = PropagationResult {
            sequence: request.sequence,
            world_revision,
            backend,
            listener_field,
            reverb: self.reverb,
            sources,
            stats,
            budget_exceeded,
        };
        self.publish(&request, &result)?;
        self.steps += 1;
        self.produced = true;
        self.finish_status(&request, &result, elapsed_us);
        if let Clock::Threaded { .. } = self.clock {
            if budget_exceeded {
                self.ray_scale = (self.ray_scale * 0.5).max(1.0 / 64.0);
            } else if elapsed_us < s.time_budget_us / 2 {
                self.ray_scale = (self.ray_scale * 1.25).min(1.0);
            }
        }
        Ok(result)
    }

    fn prepare_irs(&mut self, field: &EnergyHistogram, sources: &[SourceResult], seed: u64) {
        self.listener_ir = None;
        self.hero_irs.clear();
        let Some(rate) = self.convolution_rate else {
            return;
        };
        if let Ok(ir) = synthesize_ir(field, rate, job_seed(seed, 0x4952)) {
            self.listener_ir = Some(ir);
        }
        for r in sources {
            if let Some(h) = &r.histogram
                && let Ok(ir) = synthesize_ir(h, rate, job_seed(seed, r.id as u32))
            {
                self.hero_irs.push((r.id, ir));
            }
        }
    }

    fn publish(&mut self, request: &PropagationRequest, result: &PropagationResult) -> Result<()> {
        let proxy = self.world.as_ref().is_some_and(|w| w.proxy().is_some());
        let rate = self.convolution_rate;
        let mut kernel_error: Option<String> = None;
        let mut prepare =
            |ir: &engine_audio::acoustic::StereoImpulseResponse| -> Option<Box<ConvolutionKernel>> {
                let rate = rate?;
                match ConvolutionKernel::prepare(ir, rate) {
                    Ok(k) => Some(Box::new(k)),
                    Err(e) => {
                        kernel_error.get_or_insert(e.to_string());
                        None
                    }
                }
            };
        let mut voices: Vec<VoicePropagation> = Vec::with_capacity(result.sources.len());
        for r in &result.sources {
            let Some(src) = request.sources.iter().find(|s| s.id == r.id) else {
                continue;
            };
            let Some(voice) = src.voice else {
                continue;
            };
            let mut v = VoicePropagation::new(voice);
            v.direct = r.direct.gains;
            v.direction = proxy.then_some(r.direct.direction);
            for (tap, path) in v.taps.iter_mut().zip(&r.reflections) {
                *tap = ReflectionTap {
                    delay_s: path.delay_s,
                    gains: path.gains,
                    direction: path.direction,
                };
            }
            v.tap_count = r.reflections.len().min(MAX_REFLECTION_TAPS) as u8;
            v.reverb_send = if src.reverb { 1.0 } else { 0.0 };
            if let Some((_, ir)) = self.hero_irs.iter().find(|(id, _)| *id == r.id) {
                v.kernel = prepare(ir);
            }
            voices.push(v);
        }
        voices.sort_by_key(|v| v.voice);
        voices.truncate(MAX_ACOUSTIC_VOICES);
        let listener_kernel = self.listener_ir.as_ref().and_then(&mut prepare);
        let reverb = result.reverb;
        let hook = &mut self.hook;
        let published = self.publisher.publish(|frame| {
            frame.voices.extend(voices);
            frame.reverb = reverb;
            frame.listener_kernel = listener_kernel;
            if let Some(hook) = hook {
                hook(result, frame);
            }
            Ok(())
        });
        if let Some(message) = kernel_error
            && !self.kernel_warned
        {
            self.kernel_warned = true;
            self.event(ServiceEvent::Warning {
                code: "acoustics.backend.failed",
                message: truncate(
                    format!("convolution kernel unavailable: {message}"),
                    MAX_REASON_BYTES,
                ),
            });
        }
        published.map(|_| ()).map_err(|e| {
            AcousticsError::new(
                "acoustics.backend.failed",
                format!("publishing parameters failed: {e}"),
            )
        })
    }

    fn finish_status(
        &mut self,
        request: &PropagationRequest,
        result: &PropagationResult,
        elapsed_us: u32,
    ) {
        let st = &mut self.status;
        st.updates += 1;
        st.last_update_us = elapsed_us;
        st.max_update_us = st.max_update_us.max(elapsed_us);
        if result.budget_exceeded {
            st.budget_overruns += 1;
        }
        st.rays_last_update = result.stats.rays;
        st.sources_traced = result.sources.len() as u32;
        st.sources_skipped = (request.sources.len() - result.sources.len()) as u32;
        let p = self.publisher.stats();
        st.publishes = p.published;
        st.audio_try_lock_misses = p.audio_try_lock_misses;
        st.sources = result
            .sources
            .iter()
            .take(MAX_STATUS_SOURCES)
            .map(|r| SourceStatus {
                id: r.id,
                visible_fraction: r.direct.visible_fraction,
                occluded: r.direct.visible_fraction < 0.5,
                reflections: r.reflections.len() as u8,
            })
            .collect();
        self.refresh_backend_status();
    }

    /// Publishes dry parameters (on shutdown and on failure).
    pub fn publish_dry(&mut self) {
        let _ = self.publisher.reset_to_dry();
        self.status.publishes = self.publisher.stats().published;
    }

    /// Marks a failed update: publish dry and report.
    pub fn fail(&mut self, e: &AcousticsError) {
        self.publish_dry();
        self.status.state = AcousticState::Failed;
        self.status.fallback_reason = Some(truncate(e.to_string(), MAX_REASON_BYTES));
        self.event(ServiceEvent::Failed {
            code: e.code,
            message: truncate(e.message.clone(), MAX_REASON_BYTES),
        });
    }
}

/// Compact summary of a result for the game thread.
pub(crate) fn summarize(r: &PropagationResult) -> PropagationSummary {
    PropagationSummary {
        sequence: r.sequence,
        world_revision: r.world_revision,
        backend: r.backend,
        reverb: r.reverb,
        sources: r
            .sources
            .iter()
            .map(|s| SourceSummary {
                id: s.id,
                visible_fraction: s.direct.visible_fraction,
                reflections: s.reflections.len() as u8,
            })
            .collect(),
        budget_exceeded: r.budget_exceeded,
    }
}
