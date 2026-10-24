//! Acoustics lifecycle: service creation, per-tick world refresh, request
//! submission, result handling, events and status (design record section 7.6).
//!
//! Failures never abort the runtime: a world or service that cannot be built
//! leaves the mixer dry, sets the status to `failed` with the coded reason and
//! emits one `acoustics.failed` event per distinct failure.
use super::{
    air::air_np_per_m,
    collect::{self, Collected, MeshLookup},
    components::*,
    volumes::RoomSet,
};
use crate::{
    EngineEvent, Error, Result,
    audio::{self, RuntimeAudio},
    rendering3d::bounded_upscaler_reason,
    runtime::{AuthoredId, RuntimeLog},
};
use bevy_ecs::prelude::*;
use engine_acoustics::{
    AcousticState, AcousticStatus, AcousticsService, BackendKind, BackendPreference, InstanceId,
    ListenerPose, ParamHook, PropagationBackend, PropagationRequest, PropagationSettings,
    PropagationSummary, ProxyMapping, ServiceConfig, ServiceMode, SourceRequest,
};
use engine_audio::acoustic::MixerEnvironment;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};

pub const EVENT_BACKEND_CHANGED: &str = "acoustics.backend_changed";
pub const EVENT_OCCLUSION_CHANGED: &str = "acoustics.occlusion_changed";
pub const EVENT_WORLD_REBUILT: &str = "acoustics.world_rebuilt";
pub const EVENT_FAILED: &str = "acoustics.failed";
/// Events delivered per tick; further ones are dropped and logged.
pub const MAX_EVENTS_PER_TICK: usize = 32;
/// Minimum time between full world rebuilds.
pub const REBUILD_DEBOUNCE_S: f32 = 0.5;
/// Sources submitted per request (the service traces at most 64 of them).
pub const MAX_SUBMITTED_SOURCES: usize = 256;
/// Hero (per-pair convolution) sources.
pub const MAX_HERO_SOURCES: usize = 4;
/// Warnings kept for `acoustics_inspect` and `warnings()`.
const MAX_WARNINGS: usize = 64;
/// A source becomes occluded below this visible fraction ...
pub const OCCLUDED_BELOW: f32 = 0.3;
/// ... and is audible again above this one.
pub const VISIBLE_ABOVE: f32 = 0.7;

/// Callbacks write the request fields; the runtime writes `status` (the
/// `UpscalerControl` pattern). `retry_gpu` and `rebuild_world` are consumed.
#[derive(Resource, Clone, Debug, Default)]
pub struct AcousticControl {
    /// Replaces the authored `backend` while set.
    pub override_backend: Option<BackendPreference>,
    /// Re-probe the requested backend tiers on the next tick.
    pub retry_gpu: bool,
    /// Stop submitting requests (the mixer keeps its last parameters).
    pub paused: bool,
    /// Rebuild the world on the next tick, ignoring the debounce.
    pub rebuild_world: bool,
    pub status: AcousticStatus,
}
/// Metres per world unit while an enabled environment exists; read by
/// `audio::source` to complete `engine.AcousticSource` voice options.
#[derive(Resource, Clone, Copy, Debug)]
pub struct AcousticUnits(pub f32);
/// What the host configured with `Runtime::set_acoustics`.
#[derive(Resource)]
pub struct AcousticHost {
    pub(crate) backends: Mutex<Vec<Box<dyn PropagationBackend>>>,
    pub(crate) mode: ServiceMode,
}
/// The mixer output rate in Hz, supplied by the host so the service can prepare
/// convolution kernels (used only while `reverb` is `convolution`).
#[derive(Resource, Clone, Copy, Debug)]
pub struct AcousticOutputRate(pub u32);
/// The native parameter hook, shared with the worker.
#[derive(Resource, Clone, Default)]
pub struct AcousticHooks(pub(crate) Arc<Mutex<Option<ParamHook>>>);

/// Hysteretic occlusion state per voice.
#[derive(Debug, Default)]
pub struct Occlusion {
    states: BTreeMap<u64, bool>,
}
impl Occlusion {
    /// Feed a visible fraction. `Some(occluded)` is returned exactly when the
    /// state crosses a threshold (enter below 0.3, exit above 0.7).
    pub fn update(&mut self, id: u64, visible: f32) -> Option<bool> {
        let state = self.states.entry(id).or_insert(false);
        let next = if *state {
            visible <= VISIBLE_ABOVE
        } else {
            visible < OCCLUDED_BELOW
        };
        (std::mem::replace(state, next) != next).then_some(next)
    }
    pub fn retain(&mut self, live: impl Fn(u64) -> bool) {
        self.states.retain(|id, _| live(*id));
    }
    pub fn is_occluded(&self, id: u64) -> bool {
        self.states.get(&id).copied().unwrap_or(false)
    }
}

#[derive(Resource)]
pub(crate) struct RuntimeAcoustics {
    service: Mutex<Option<AcousticsService>>,
    mode: ServiceMode,
    environment: AcousticEnvironment,
    preference: BackendPreference,
    signature: Option<blake3::Hash>,
    failed_signature: Option<blake3::Hash>,
    since_rebuild: f32,
    accumulator: f32,
    sequence: u64,
    proxy: Option<ProxyMapping>,
    dynamic: Vec<(String, InstanceId)>,
    last_dynamic: BTreeMap<String, [f32; 12]>,
    occlusion: Occlusion,
    owners: BTreeMap<u64, Entity>,
    failure: Option<(String, String)>,
    reported: Option<(String, String)>,
    rooms: Arc<Mutex<RoomSet>>,
    warnings: Vec<String>,
}

fn audio_error(error: engine_audio::AudioError) -> (String, String) {
    (error.code.to_owned(), error.message)
}
fn backend_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Cpu => "cpu",
        BackendKind::GpuCompute => "gpu_compute",
        BackendKind::GpuRayQuery => "gpu_ray_query",
    }
}
fn entity_name(world: &World, entity: Entity) -> String {
    world
        .get::<AuthoredId>(entity)
        .map_or_else(|| format!("entity:{}", entity.to_bits()), |a| a.id.clone())
}

/// Tracing settings of an environment.
pub fn propagation_settings(env: &AcousticEnvironment) -> PropagationSettings {
    let q = &env.quality;
    PropagationSettings {
        occlusion_rays: q.occlusion_rays,
        reflection_order: q.reflection_order,
        max_reflections: q.max_reflections,
        er_rays: q.er_rays,
        reverb_rays: q.reverb_rays,
        max_bounces: q.max_bounces,
        histogram_bin_s: q.histogram_bin_ms / 1000.0,
        histogram_length_s: q.histogram_length_s,
        late_start_s: q.late_start_ms / 1000.0,
        receiver_radius_m: q.receiver_radius_m,
        speed_of_sound: env.speed_of_sound,
        air_np_per_m: air_np_per_m(&env.air),
        seed: env.seed,
        time_budget_us: (q.time_budget_ms * 1000.0) as u32,
    }
}
/// Mixer-wide settings of an environment.
pub fn mixer_environment(env: &AcousticEnvironment) -> MixerEnvironment {
    MixerEnvironment {
        renderer: env.renderer.into(),
        reverb: if env.reverb_source == ReverbSource::Off {
            engine_audio::acoustic::ReverbRenderer::Off
        } else {
            env.reverb.into()
        },
        speed_of_sound: env.speed_of_sound,
        air_np_per_m: air_np_per_m(&env.air),
        ..MixerEnvironment::default()
    }
}
fn failed_status(requested: BackendPreference, code: &str, message: &str) -> AcousticStatus {
    AcousticStatus {
        requested,
        state: AcousticState::Failed,
        fallback_reason: Some(bounded_upscaler_reason(&format!("{code}: {message}"))),
        ..Default::default()
    }
}

impl RuntimeAcoustics {
    fn new(environment: AcousticEnvironment) -> Self {
        Self {
            service: Mutex::new(None),
            mode: ServiceMode::Synchronous,
            preference: environment.backend.into(),
            environment,
            signature: None,
            failed_signature: None,
            since_rebuild: 0.0,
            accumulator: 0.0,
            sequence: 0,
            proxy: None,
            dynamic: vec![],
            last_dynamic: BTreeMap::new(),
            occlusion: Occlusion::default(),
            owners: BTreeMap::new(),
            failure: None,
            reported: None,
            rooms: Arc::new(Mutex::new(RoomSet::default())),
            warnings: vec![],
        }
    }
    fn service(&mut self) -> Option<&mut AcousticsService> {
        self.service
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
    }
    fn shutdown(&mut self) {
        if let Some(mut service) = self
            .service
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            service.shutdown();
        }
    }
    fn fail(&mut self, code: &str, message: &str, events: &mut Vec<EngineEvent>) {
        let key = (code.to_owned(), message.to_owned());
        if self.reported.as_ref() != Some(&key) {
            events.push(EngineEvent {
                name: EVENT_FAILED.into(),
                source: None,
                target: None,
                payload: serde_json::json!({"code": code, "message": message}),
            });
            self.reported = Some(key.clone());
        }
        self.failure = Some(key);
    }
    /// The status to publish: the service's own, overridden by a recorded failure.
    fn status(&mut self) -> AcousticStatus {
        let requested = self.preference;
        let failure = self.failure.clone();
        let mut status = match self.service() {
            Some(service) => service.status(),
            None => AcousticStatus {
                requested,
                ..Default::default()
            },
        };
        if let Some((code, message)) = failure {
            status = AcousticStatus {
                requested,
                ..failed_status(requested, &code, &message)
            }
            .with_world_of(&status);
        }
        status
    }
}
trait KeepWorld {
    fn with_world_of(self, other: &AcousticStatus) -> Self;
}
impl KeepWorld for AcousticStatus {
    /// A failure keeps the counters and world statistics of the live service.
    fn with_world_of(mut self, other: &AcousticStatus) -> Self {
        self.world = other.world;
        self.updates = other.updates;
        self.publishes = other.publishes;
        self.audio_try_lock_misses = other.audio_try_lock_misses;
        self
    }
}

struct Flags {
    override_backend: Option<BackendPreference>,
    retry_gpu: bool,
    paused: bool,
    rebuild: bool,
}

/// Create the service for `state`. The error is `(code, message)`.
fn create_service(
    world: &World,
    state: &mut RuntimeAcoustics,
) -> std::result::Result<AcousticsService, (String, String)> {
    let environment = &state.environment;
    let publisher = match world.get_resource::<RuntimeAudio>() {
        Some(audio) => {
            audio
                .control
                .set_environment(mixer_environment(environment))
                .map_err(audio_error)?;
            audio.control.acoustic_publisher().map_err(audio_error)?
        }
        None => {
            // Headless run without audio: a private offline mixer hosts the
            // publisher so the service, status and events still work. Nothing is
            // audible. The engine is dropped; the publisher is independent of it.
            let engine =
                engine_audio::AudioEngine::offline(Default::default()).map_err(audio_error)?;
            engine.control().acoustic_publisher().map_err(audio_error)?
        }
    };
    let (backends, mode) = match world.get_resource::<AcousticHost>() {
        Some(host) => (
            std::mem::take(&mut *host.backends.lock().unwrap_or_else(PoisonError::into_inner)),
            host.mode,
        ),
        None => (vec![], ServiceMode::Synchronous),
    };
    state.mode = mode;
    let config = ServiceConfig {
        preference: state.preference,
        mode,
        update_hz: environment.update_hz,
        reverb_update_hz: environment.reverb_update_hz,
        settings: propagation_settings(environment),
        backends,
    };
    let mut service =
        AcousticsService::new(config, publisher).map_err(|e| (e.code.to_owned(), e.message))?;
    if environment.reverb == Reverb::Convolution
        && let Some(rate) = world.get_resource::<AcousticOutputRate>()
    {
        service
            .set_convolution_rate(Some(rate.0))
            .map_err(|e| (e.code.to_owned(), e.message))?;
    }
    let user = world
        .get_resource::<AcousticHooks>()
        .cloned()
        .unwrap_or_default()
        .0;
    let rooms = state.rooms.clone();
    service.set_param_hook(Some(Box::new(move |result, frame| {
        if let Ok(rooms) = rooms.lock() {
            rooms.apply(frame);
        }
        if let Ok(mut hook) = user.lock()
            && let Some(hook) = hook.as_mut()
        {
            hook(result, frame);
        }
    })));
    Ok(service)
}

/// Create or refresh the acoustics state for the scene's environment. Called at
/// runtime start and after each scene transition. Never fails on service or
/// world problems; those become status `failed`.
pub(crate) fn start(world: &mut World) -> Result<()> {
    let found = collect::find_environment(world)?;
    let previous = world.remove_resource::<RuntimeAcoustics>();
    let Some((_, environment)) = found else {
        if let Some(mut previous) = previous {
            previous.shutdown();
        }
        world.remove_resource::<AcousticUnits>();
        set_status(world, AcousticStatus::default());
        return Ok(());
    };
    let mut state = match previous {
        Some(mut state) => {
            // The same service continues across scenes; the world is rebuilt.
            state.preference = environment.backend.into();
            state.environment = environment.clone();
            state.signature = None;
            state.failed_signature = None;
            state.since_rebuild = f32::INFINITY;
            state.occlusion = Occlusion::default();
            state
        }
        None => RuntimeAcoustics::new(environment.clone()),
    };
    state.accumulator = 1.0 / environment.update_hz;
    state.failure = None;
    let mut events = vec![];
    if environment.enabled {
        world.insert_resource(AcousticUnits(
            environment
                .proxy2d
                .as_ref()
                .map_or(1.0, |p| 1.0 / p.units_per_meter),
        ));
        if let Ok(mut rooms) = state.rooms.lock() {
            rooms.active = environment.reverb_source == ReverbSource::Volumes;
        }
        if state.service().is_some() {
            if let Some(audio) = world.get_resource::<RuntimeAudio>()
                && let Err((code, message)) = audio
                    .control
                    .set_environment(mixer_environment(&environment))
                    .map_err(audio_error)
            {
                state.fail(&code, &message, &mut events);
            }
        } else {
            match create_service(world, &mut state) {
                Ok(service) => {
                    *state
                        .service
                        .get_mut()
                        .unwrap_or_else(PoisonError::into_inner) = Some(service);
                }
                Err((code, message)) => state.fail(&code, &message, &mut events),
            }
        }
    } else {
        state.shutdown();
        world.remove_resource::<AcousticUnits>();
    }
    let status = if environment.enabled {
        state.status()
    } else {
        AcousticStatus {
            requested: state.preference,
            ..Default::default()
        }
    };
    set_status(world, status);
    world.insert_resource(state);
    emit(world, events);
    Ok(())
}

fn set_status(world: &mut World, status: AcousticStatus) {
    if let Some(mut control) = world.get_resource_mut::<AcousticControl>() {
        control.status = status;
    }
}
fn emit(world: &mut World, mut events: Vec<EngineEvent>) {
    if events.len() > MAX_EVENTS_PER_TICK {
        let dropped = events.len() - MAX_EVENTS_PER_TICK;
        events.truncate(MAX_EVENTS_PER_TICK);
        if let Some(mut log) = world.get_resource_mut::<RuntimeLog>() {
            log.0
                .push(format!("acoustics: {dropped} events dropped (32 per tick)"));
        }
    }
    if events.is_empty() {
        return;
    }
    let count = events.len();
    if crate::emit_events(world, events).is_err()
        && let Some(mut log) = world.get_resource_mut::<RuntimeLog>()
    {
        log.0
            .push(format!("acoustics: {count} events dropped (queue full)"));
    }
}

/// Stop the service (it publishes a dry frame) and release the state.
pub(crate) fn shutdown(world: &mut World) {
    if let Some(mut state) = world.remove_resource::<RuntimeAcoustics>() {
        state.shutdown();
    }
    world.remove_resource::<AcousticUnits>();
}

/// Run one acoustics update after audio synchronization.
pub(crate) fn tick(world: &mut World, dt: f32, lookup: &MeshLookup) -> Result<()> {
    if !world.contains_resource::<RuntimeAcoustics>() {
        return Ok(());
    }
    let flags = {
        let mut control = world.resource_mut::<AcousticControl>();
        Flags {
            override_backend: control.override_backend,
            retry_gpu: std::mem::take(&mut control.retry_gpu),
            paused: control.paused,
            rebuild: std::mem::take(&mut control.rebuild_world),
        }
    };
    let mut events = vec![];
    let status = world.resource_scope(|world, mut state: Mut<RuntimeAcoustics>| {
        state.step(world, dt, lookup, &flags, &mut events);
        state.status()
    });
    set_status(world, status);
    emit(world, events);
    Ok(())
}

impl RuntimeAcoustics {
    fn step(
        &mut self,
        world: &World,
        dt: f32,
        lookup: &MeshLookup,
        flags: &Flags,
        events: &mut Vec<EngineEvent>,
    ) {
        if self.service().is_none() || !self.environment.enabled {
            return;
        }
        self.since_rebuild += dt;
        let wanted = flags
            .override_backend
            .unwrap_or_else(|| self.environment.backend.into());
        if wanted != self.preference || flags.retry_gpu {
            self.preference = wanted;
            if let Some(Err(e)) = self.service().map(|s| s.set_preference(wanted)) {
                self.fail(e.code, &e.message, events);
            }
        }
        let collected = match collect::collect(world, lookup) {
            Ok(Some(collected)) => collected,
            Ok(None) => return,
            Err(e) => {
                let d = &e.diagnostics[0];
                self.fail(&d.code.clone(), &d.message.clone(), events);
                return;
            }
        };
        self.refresh_world(&collected, flags.rebuild, events);
        if self.signature.is_none() || flags.paused {
            return;
        }
        self.update_dynamic(&collected, events);
        self.submit(world, events);
        if self.mode == ServiceMode::Synchronous {
            self.accumulator += dt;
            let period = 1.0 / self.environment.update_hz;
            if self.accumulator >= period {
                self.accumulator = (self.accumulator - period).min(period);
                if let Some(Err(e)) = self.service().map(|s| s.step_blocking().map(|_| ())) {
                    self.fail(e.code, &e.message, events);
                }
            }
        }
        if let Some(summary) = self.service().and_then(AcousticsService::latest) {
            self.handle(world, &summary, events);
        }
        self.drain_service_events(events);
    }

    fn refresh_world(&mut self, collected: &Collected, force: bool, events: &mut Vec<EngineEvent>) {
        let signature = collected.signature();
        let changed = self.signature != Some(signature);
        let blocked = !force && self.failed_signature == Some(signature);
        let due = self.signature.is_none() || force || self.since_rebuild >= REBUILD_DEBOUNCE_S;
        if !(changed || force) || blocked || !due {
            return;
        }
        match collect::build_world(collected) {
            Ok(build) => {
                let stats = build.world.stats();
                let revision = build.world.revision();
                if let Some(service) = self.service() {
                    service.set_world(build.world);
                }
                self.proxy = build.proxy;
                self.dynamic = build.dynamic;
                self.last_dynamic = collected.dynamic_transforms();
                self.signature = Some(signature);
                self.failed_signature = None;
                self.since_rebuild = 0.0;
                self.failure = None;
                self.reported = None;
                self.warnings = collected.warnings.clone();
                if let Ok(mut rooms) = self.rooms.lock() {
                    rooms.rooms =
                        super::volumes::rooms(collected, air_np_per_m(&collected.environment.air));
                }
                events.push(EngineEvent {
                    name: EVENT_WORLD_REBUILT.into(),
                    source: None,
                    target: None,
                    payload: serde_json::json!({
                        "revision": revision,
                        "triangles": stats.triangles,
                        "dropped_degenerate": stats.dropped_degenerate,
                    }),
                });
            }
            Err(e) => {
                let d = &e.diagnostics[0];
                self.failed_signature = Some(signature);
                self.fail(&d.code.clone(), &d.message.clone(), events);
            }
        }
    }

    fn update_dynamic(&mut self, collected: &Collected, events: &mut Vec<EngineEvent>) {
        let poses = collected.dynamic_transforms();
        let updates: Vec<_> = self
            .dynamic
            .iter()
            .filter_map(|(key, id)| {
                let pose = poses.get(key)?;
                (self.last_dynamic.get(key) != Some(pose)).then_some((*id, *pose))
            })
            .collect();
        if updates.is_empty() {
            return;
        }
        if let Some(Err(e)) = self.service().map(|s| s.update_dynamic(&updates)) {
            self.fail(e.code, &e.message, events);
            return;
        }
        self.last_dynamic = poses;
    }

    fn submit(&mut self, world: &World, events: &mut Vec<EngineEvent>) {
        let listener = audio::current_listener(world).unwrap_or_default();
        let mut pose = ListenerPose {
            position: listener.position,
            forward: listener.forward,
            up: listener.up,
        };
        let map = |proxy: &Option<ProxyMapping>, p: [f32; 3]| {
            proxy.as_ref().map_or(p, |proxy| proxy.to_acoustic(p))
        };
        if let Some(proxy) = &self.proxy {
            pose = proxy.pose_to_acoustic(pose);
        }
        let voices = audio::acoustic_voices(world);
        let distance = |p: [f32; 3]| {
            (0..3)
                .map(|a| (p[a] - listener.position[a]).powi(2))
                .sum::<f32>()
                .sqrt()
        };
        let mut ranked: Vec<_> = voices
            .iter()
            .map(|v| {
                let c = v.component.clone().unwrap_or_default();
                (c.priority / distance(v.position).max(0.1), v, c)
            })
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.voice.cmp(&b.1.voice)));
        ranked.truncate(MAX_SUBMITTED_SOURCES);
        let mut heroes = 0;
        let sources: Vec<_> = ranked
            .iter()
            .map(|(_, v, c)| {
                let hero = c.hero && heroes < MAX_HERO_SOURCES;
                heroes += usize::from(hero);
                SourceRequest {
                    id: v.voice.get(),
                    voice: Some(v.voice),
                    position: map(&self.proxy, v.position),
                    radius_m: c.radius_m,
                    occlusion: c.occlusion,
                    reflections: c.reflections,
                    reverb: c.reverb_send > 0.0,
                    hero,
                    priority: c.priority,
                }
            })
            .collect();
        self.owners = voices.iter().map(|v| (v.voice.get(), v.owner)).collect();
        if let Ok(mut rooms) = self.rooms.lock() {
            rooms.listener = listener.position;
        }
        self.sequence += 1;
        let request = PropagationRequest {
            sequence: self.sequence,
            listener: pose,
            sources,
            settings: propagation_settings(&self.environment),
        };
        if let Some(Err(e)) = self.service().map(|s| s.submit(request)) {
            self.fail(e.code, &e.message, events);
        }
    }

    fn handle(
        &mut self,
        world: &World,
        summary: &PropagationSummary,
        events: &mut Vec<EngineEvent>,
    ) {
        let owners = self.owners.clone();
        self.occlusion.retain(|id| owners.contains_key(&id));
        for source in &summary.sources {
            let Some(owner) = owners.get(&source.id) else {
                continue;
            };
            if let Some(occluded) = self.occlusion.update(source.id, source.visible_fraction) {
                events.push(EngineEvent {
                    name: EVENT_OCCLUSION_CHANGED.into(),
                    source: Some(*owner),
                    target: None,
                    payload: serde_json::json!({
                        "source": entity_name(world, *owner),
                        "occluded": occluded,
                        "visible_fraction": source.visible_fraction,
                    }),
                });
            }
        }
    }

    /// Map the service's own notifications to `acoustics.*` events and the log.
    fn drain_service_events(&mut self, events: &mut Vec<EngineEvent>) {
        let Some(drained) = self.service().map(AcousticsService::drain_events) else {
            return;
        };
        for event in drained {
            match event {
                ServiceEvent::BackendChanged { from, to, reason } => {
                    events.push(EngineEvent {
                        name: EVENT_BACKEND_CHANGED.into(),
                        source: None,
                        target: None,
                        payload: serde_json::json!({
                            "from": from.map_or("none", backend_name),
                            "to": to.map_or("none", backend_name),
                            "reason": reason,
                        }),
                    });
                }
                ServiceEvent::Failed { code, message } => self.fail(code, &message, events),
                ServiceEvent::Warning { code, message } => self.warnings.push(format!("{code}: {message}")),
            }
        }
        self.warnings.truncate(MAX_WARNINGS);
    }
}

/// Findings of the last world build (ignored components and similar).
pub fn warnings(world: &World) -> Vec<String> {
    world
        .get_resource::<RuntimeAcoustics>()
        .map_or_else(Vec::new, |s| s.warnings.clone())
}

/// The status to report in a snapshot: absent without an environment.
pub fn status(world: &World) -> Option<AcousticStatus> {
    world.get_resource::<RuntimeAcoustics>()?;
    Some(world.get_resource::<AcousticControl>()?.status.clone())
}

/// Install (or clear) the native parameter hook. It runs on the propagation
/// worker after each result and may adjust the frame before it is published.
pub fn set_param_hook(world: &mut World, hook: Option<ParamHook>) {
    if !world.contains_resource::<AcousticHooks>() {
        world.insert_resource(AcousticHooks::default());
    }
    let hooks = world.resource::<AcousticHooks>();
    *hooks.0.lock().unwrap_or_else(PoisonError::into_inner) = hook;
}
/// Install (or remove) a native DSP effect on a mixer bus; the previous effect is
/// returned so the caller, not the audio thread, drops it.
pub fn set_bus_effect(
    world: &World,
    bus: engine_audio::Bus,
    effect: Option<Box<dyn engine_audio::acoustic::DspEffect>>,
) -> Result<Option<Box<dyn engine_audio::acoustic::DspEffect>>> {
    world
        .get_resource::<RuntimeAudio>()
        .ok_or_else(|| Error::new("audio.unavailable", "Host did not configure audio"))?
        .control
        .set_bus_effect(bus, effect)
        .map_err(|e| Error::new(e.code, e.message))
}
