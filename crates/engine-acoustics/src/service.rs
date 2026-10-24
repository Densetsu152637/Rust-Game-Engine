use crate::engine::{
    Clock, Engine, MAX_QUEUED_EVENTS, summarize, validate_request, validate_settings,
};
use crate::{AcousticWorld, AcousticsError, Affine3, CpuBackend, InstanceId, Result};
use crate::{
    BackendKind, BackendPreference, BandGains, DirectPath, EnergyHistogram, ListenerPose,
    PropagationBackend, ReflectionPath, TraceStats, WorldStats,
};
use engine_audio::VoiceHandle;
use engine_audio::acoustic::{AcousticFrame, AcousticPublisher, ReverbParams};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceRequest {
    /// Caller id; for a voice source it must equal `voice.get()`.
    pub id: u64,
    /// Voice that receives the published parameters. `None` traces the source
    /// without publishing (queries). Addition to section 7.2: engine-audio has no
    /// public way to rebuild a `VoiceHandle` from its `u64`.
    pub voice: Option<VoiceHandle>,
    pub position: [f32; 3],
    pub radius_m: f32,
    pub occlusion: bool,
    pub reflections: bool,
    pub reverb: bool,
    pub hero: bool,
    pub priority: f32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropagationSettings {
    pub occlusion_rays: u16,
    pub reflection_order: u8,
    pub max_reflections: u8,
    pub er_rays: u16,
    pub reverb_rays: u32,
    pub max_bounces: u16,
    pub histogram_bin_s: f32,
    pub histogram_length_s: f32,
    pub late_start_s: f32,
    pub receiver_radius_m: f32,
    pub speed_of_sound: f32,
    pub air_np_per_m: BandGains,
    pub seed: u64,
    pub time_budget_us: u32,
}
impl Default for PropagationSettings {
    /// The documented CPU defaults of section 5.4.
    fn default() -> Self {
        Self {
            occlusion_rays: 16,
            reflection_order: 2,
            max_reflections: 8,
            er_rays: 256,
            reverb_rays: 4096,
            max_bounces: 64,
            histogram_bin_s: 0.001,
            histogram_length_s: 2.0,
            late_start_s: 0.08,
            receiver_radius_m: 1.0,
            speed_of_sound: 343.0,
            air_np_per_m: [0.0; 4],
            seed: 1,
            time_budget_us: 4_000,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct PropagationRequest {
    pub sequence: u64,
    pub listener: ListenerPose,
    pub sources: Vec<SourceRequest>,
    pub settings: PropagationSettings,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SourceResult {
    pub id: u64,
    pub direct: DirectPath,
    pub reflections: Vec<ReflectionPath>,
    pub histogram: Option<EnergyHistogram>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PropagationResult {
    pub sequence: u64,
    pub world_revision: u64,
    pub backend: BackendKind,
    pub listener_field: Option<EnergyHistogram>,
    pub reverb: ReverbParams,
    pub sources: Vec<SourceResult>,
    pub stats: TraceStats,
    pub budget_exceeded: bool,
}
/// Compact per-source summary for the game thread. Addition to section 7.2, which
/// names `PropagationSummary` without defining it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceSummary {
    pub id: u64,
    pub visible_fraction: f32,
    pub reflections: u8,
}
/// What `AcousticsService::latest` returns. Addition to section 7.2.
#[derive(Clone, Debug, PartialEq)]
pub struct PropagationSummary {
    pub sequence: u64,
    pub world_revision: u64,
    pub backend: BackendKind,
    pub reverb: ReverbParams,
    pub sources: Vec<SourceSummary>,
    pub budget_exceeded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceMode {
    Threaded,
    Synchronous,
}
/// The CPU backend is appended automatically after the host-supplied `backends`.
pub struct ServiceConfig {
    pub preference: BackendPreference,
    pub mode: ServiceMode,
    pub update_hz: f32,
    pub reverb_update_hz: f32,
    pub settings: PropagationSettings,
    pub backends: Vec<Box<dyn PropagationBackend>>,
}
/// Runs on the worker after each result to adjust the frame before publishing.
pub type ParamHook = Box<dyn FnMut(&PropagationResult, &mut AcousticFrame) + Send>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub enum AcousticState {
    #[default]
    Disabled,
    Starting,
    Active,
    Fallback,
    Failed,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct SourceStatus {
    pub id: u64,
    pub visible_fraction: f32,
    pub occluded: bool,
    pub reflections: u8,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AcousticStatus {
    pub requested: BackendPreference,
    pub active: Option<BackendKind>,
    pub state: AcousticState,
    /// At most 512 bytes, truncated on a char boundary.
    pub fallback_reason: Option<String>,
    pub adapter: Option<String>,
    pub hardware_rt: bool,
    pub world: WorldStats,
    pub updates: u64,
    pub last_update_us: u32,
    pub max_update_us: u32,
    pub budget_overruns: u64,
    pub rays_last_update: u64,
    pub sources_traced: u32,
    pub sources_skipped: u32,
    pub publishes: u64,
    pub audio_try_lock_misses: u64,
    pub gpu_timeouts: u64,
    pub aborted_rays: u64,
    /// At most 32 entries.
    pub sources: Vec<SourceStatus>,
}

/// Notifications for the game thread (drained with
/// [`AcousticsService::drain_events`]). Addition to section 7.2 so the ECS layer can
/// emit `acoustics.backend_changed` and `acoustics.failed` without diffing status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceEvent {
    /// The active tier changed (demotion, re-probe, or no tier left).
    BackendChanged {
        from: Option<BackendKind>,
        to: Option<BackendKind>,
        reason: String,
    },
    /// An update failed; dry parameters were published.
    Failed { code: &'static str, message: String },
    /// A non-fatal problem (for example a rejected dynamic transform or an
    /// unavailable convolution kernel).
    Warning { code: &'static str, message: String },
}

#[derive(Default)]
struct Inbox {
    world: Option<AcousticWorld>,
    dynamic: Vec<(InstanceId, Affine3)>,
    request: Option<PropagationRequest>,
    preference: Option<BackendPreference>,
    hook: Option<Option<ParamHook>>,
    convolution_rate: Option<Option<u32>>,
    shutdown: bool,
}
impl Inbox {
    fn has_work(&self) -> bool {
        self.world.is_some()
            || !self.dynamic.is_empty()
            || self.request.is_some()
            || self.preference.is_some()
            || self.shutdown
    }
}

struct Shared {
    inbox: Mutex<Inbox>,
    wake: Condvar,
    outbox: Mutex<Option<PropagationSummary>>,
    status: Mutex<AcousticStatus>,
    events: Mutex<Vec<ServiceEvent>>,
}

/// The propagation service: owns the backends and the publisher, runs propagation
/// off the audio thread and publishes `AcousticFrame`s.
///
/// `Threaded` runs a worker thread at `update_hz` (and on every submit); calls on
/// the service only take short locks and never wait for tracing. `Synchronous` runs
/// nothing in the background: `step_blocking` performs one update in the caller,
/// tracing the late field every `round(update_hz / reverb_update_hz)` steps, which
/// makes results bitwise reproducible. The service is `Send + Sync`.
pub struct AcousticsService {
    mode: ServiceMode,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    engine: Option<Mutex<Engine>>,
    last: Option<PropagationResult>,
    dynamic_ids: Vec<InstanceId>,
}

fn poisoned() -> AcousticsError {
    AcousticsError::new("acoustics.backend.failed", "service state was poisoned")
}

impl AcousticsService {
    /// Validates the configuration, appends the CPU tier, selects the backend for
    /// `config.preference` and (threaded) starts the worker.
    pub fn new(config: ServiceConfig, publisher: AcousticPublisher) -> Result<Self> {
        if !(config.update_hz.is_finite() && config.update_hz > 0.0 && config.update_hz <= 240.0) {
            return Err(AcousticsError::new(
                "acoustics.request.invalid",
                "update_hz must be in (0, 240]",
            ));
        }
        if !(config.reverb_update_hz.is_finite()
            && config.reverb_update_hz > 0.0
            && config.reverb_update_hz <= config.update_hz)
        {
            return Err(AcousticsError::new(
                "acoustics.request.invalid",
                "reverb_update_hz must be in (0, update_hz]",
            ));
        }
        validate_settings(&config.settings)?;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as u8;
        let mut backends = config.backends;
        let cpu = CpuBackend::new(threads).map_err(|e| AcousticsError::new(e.code, e.message))?;
        backends.push(Box::new(cpu));
        let clock = match config.mode {
            ServiceMode::Synchronous => Clock::Steps {
                every: (config.update_hz / config.reverb_update_hz)
                    .round()
                    .max(1.0) as u64,
            },
            ServiceMode::Threaded => Clock::Threaded {
                hz: config.reverb_update_hz,
            },
        };
        let engine = Engine::new(backends, config.preference, publisher, clock);
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox::default()),
            wake: Condvar::new(),
            outbox: Mutex::new(None),
            status: Mutex::new(engine.status.clone()),
            events: Mutex::new(Vec::new()),
        });
        let mut service = Self {
            mode: config.mode,
            shared: shared.clone(),
            worker: None,
            engine: None,
            last: None,
            dynamic_ids: Vec::new(),
        };
        match config.mode {
            ServiceMode::Synchronous => service.engine = Some(Mutex::new(engine)),
            ServiceMode::Threaded => {
                let period = Duration::from_secs_f32(1.0 / config.update_hz);
                let worker = std::thread::Builder::new()
                    .name("acoustics-propagation".into())
                    .spawn(move || run_worker(engine, shared, period))
                    .map_err(|e| {
                        AcousticsError::new(
                            "acoustics.backend.unavailable",
                            format!("could not start the propagation worker: {e}"),
                        )
                    })?;
                service.worker = Some(worker);
            }
        }
        Ok(service)
    }

    pub fn mode(&self) -> ServiceMode {
        self.mode
    }

    fn inbox(&self) -> Result<std::sync::MutexGuard<'_, Inbox>> {
        self.shared.inbox.lock().map_err(|_| poisoned())
    }
    fn notify(&self) {
        self.shared.wake.notify_one();
    }

    /// Non-blocking hand-off; the newest world wins and replaces pending dynamic
    /// updates.
    pub fn set_world(&mut self, world: AcousticWorld) {
        self.dynamic_ids = world.dynamic_instances().collect();
        if let Ok(mut st) = self.shared.status.lock() {
            st.world = world.stats();
        }
        if let Ok(mut inbox) = self.inbox() {
            inbox.dynamic.clear();
            inbox.world = Some(world);
        }
        self.notify();
    }

    /// Queues dynamic instance transforms (checked against the last world).
    pub fn update_dynamic(&mut self, updates: &[(InstanceId, Affine3)]) -> Result<()> {
        for (id, t) in updates {
            if self.dynamic_ids.binary_search(id).is_err() {
                return Err(AcousticsError::new(
                    "acoustics.world.invalid",
                    format!("instance {} is not a dynamic instance of the world", id.0),
                ));
            }
            if t.iter().any(|v| !v.is_finite()) {
                return Err(AcousticsError::new(
                    "acoustics.world.invalid",
                    "transform has a non-finite value",
                ));
            }
        }
        self.inbox()?.dynamic.extend_from_slice(updates);
        self.notify();
        Ok(())
    }

    /// Validates and queues a request (latest wins); never waits for tracing.
    pub fn submit(&mut self, request: PropagationRequest) -> Result<()> {
        validate_request(&request)?;
        self.inbox()?.request = Some(request);
        self.notify();
        Ok(())
    }

    /// The newest summary since the last call; `None` when there is none or the
    /// worker holds the slot right now.
    pub fn latest(&mut self) -> Option<PropagationSummary> {
        self.shared.outbox.try_lock().ok()?.take()
    }

    /// Synchronous mode only: applies pending inputs and runs one update in the
    /// caller.
    pub fn step_blocking(&mut self) -> Result<&PropagationResult> {
        let Some(engine) = &self.engine else {
            return Err(AcousticsError::new(
                "acoustics.request.invalid",
                "step_blocking needs ServiceMode::Synchronous",
            ));
        };
        let mut engine = engine.lock().map_err(|_| poisoned())?;
        let inbox = std::mem::take(&mut *self.shared.inbox.lock().map_err(|_| poisoned())?);
        apply(&mut engine, inbox);
        let outcome = engine.update();
        if let Err(e) = &outcome
            && e.code != "acoustics.request.invalid"
        {
            engine.fail(e);
        }
        publish_state(&self.shared, &mut engine, outcome.as_ref().ok());
        drop(engine);
        self.last = Some(outcome?);
        Ok(self.last.as_ref().expect("just stored"))
    }

    pub fn status(&self) -> AcousticStatus {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Events since the last call, oldest first.
    pub fn drain_events(&mut self) -> Vec<ServiceEvent> {
        self.shared
            .events
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    /// Re-probes the backends from `p` downward (also the "retry GPU" control).
    pub fn set_preference(&mut self, p: BackendPreference) -> Result<()> {
        if let Some(engine) = &self.engine {
            let mut engine = engine.lock().map_err(|_| poisoned())?;
            engine.set_preference(p);
            publish_state(&self.shared, &mut engine, None);
        } else {
            if let Ok(mut st) = self.shared.status.lock() {
                st.requested = p;
            }
            self.inbox()?.preference = Some(p);
            self.notify();
        }
        Ok(())
    }

    /// Runs on the worker after each result, before publishing.
    pub fn set_param_hook(&mut self, hook: Option<ParamHook>) {
        if let Some(engine) = &self.engine {
            if let Ok(mut engine) = engine.lock() {
                engine.hook = hook;
            }
        } else if let Ok(mut inbox) = self.inbox() {
            inbox.hook = Some(hook);
        }
    }

    /// With `Some(rate)`, late-field histograms are also synthesized into impulse
    /// responses and prepared as convolution kernels at the output sample rate (the
    /// listener field and hero voices). Addition to section 7.2: the service has no
    /// other way to learn the output rate.
    pub fn set_convolution_rate(&mut self, rate: Option<u32>) -> Result<()> {
        if rate.is_some_and(|r| !(8_000..=192_000).contains(&r)) {
            return Err(AcousticsError::new(
                "acoustics.request.invalid",
                "convolution sample rate must be 8-192 kHz",
            ));
        }
        if let Some(engine) = &self.engine {
            engine.lock().map_err(|_| poisoned())?.convolution_rate = rate;
        } else {
            self.inbox()?.convolution_rate = Some(rate);
        }
        Ok(())
    }

    /// Joins the worker and publishes dry parameters. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            if let Ok(mut inbox) = self.shared.inbox.lock() {
                inbox.shutdown = true;
            }
            self.notify();
            let _ = worker.join();
        }
        if let Some(engine) = &self.engine
            && let Ok(mut engine) = engine.lock()
        {
            engine.publish_dry();
            engine.status.state = AcousticState::Disabled;
            publish_state(&self.shared, &mut engine, None);
        }
        if let Ok(mut st) = self.shared.status.lock() {
            st.state = AcousticState::Disabled;
        }
    }
}

impl Drop for AcousticsService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn apply(engine: &mut Engine, inbox: Inbox) {
    if let Some(p) = inbox.preference {
        engine.set_preference(p);
    }
    if let Some(world) = inbox.world {
        engine.set_world(world);
    }
    if !inbox.dynamic.is_empty() {
        engine.update_dynamic(&inbox.dynamic);
    }
    if let Some(hook) = inbox.hook {
        engine.hook = hook;
    }
    if let Some(rate) = inbox.convolution_rate {
        engine.convolution_rate = rate;
    }
    if let Some(request) = inbox.request {
        engine.request = Some(request);
    }
}

/// Copies status, events and the summary to the shared slots.
fn publish_state(shared: &Shared, engine: &mut Engine, result: Option<&PropagationResult>) {
    if let Ok(mut st) = shared.status.lock() {
        *st = engine.status.clone();
    }
    if !engine.events.is_empty()
        && let Ok(mut events) = shared.events.lock()
    {
        events.append(&mut engine.events);
        let excess = events.len().saturating_sub(MAX_QUEUED_EVENTS);
        events.drain(..excess);
    }
    if let Some(r) = result
        && let Ok(mut out) = shared.outbox.lock()
    {
        *out = Some(summarize(r));
    }
}

fn run_worker(mut engine: Engine, shared: Arc<Shared>, period: Duration) {
    let mut last_update: Option<Instant> = None;
    loop {
        let inbox = {
            let Ok(mut guard) = shared.inbox.lock() else {
                break;
            };
            loop {
                if guard.has_work() {
                    break;
                }
                let ready = engine.world.is_some() && engine.request.is_some();
                let wait = match last_update {
                    Some(t) if ready => period.saturating_sub(t.elapsed()),
                    None if ready => Duration::ZERO,
                    _ => period,
                };
                if ready && wait.is_zero() {
                    break;
                }
                match shared.wake.wait_timeout(guard, wait) {
                    Ok((g, _)) => guard = g,
                    Err(_) => return,
                }
            }
            std::mem::take(&mut *guard)
        };
        if inbox.shutdown {
            break;
        }
        apply(&mut engine, inbox);
        let mut result = None;
        if engine.world.is_some() && engine.request.is_some() {
            match engine.update() {
                Ok(r) => result = Some(r),
                Err(e) => engine.fail(&e),
            }
            last_update = Some(Instant::now());
        }
        publish_state(&shared, &mut engine, result.as_ref());
    }
    engine.publish_dry();
    engine.status.state = AcousticState::Disabled;
    publish_state(&shared, &mut engine, None);
}
