use crate::{
    AcousticWorld, Affine3, BackendKind, BackendPreference, BandGains, DirectPath, EnergyHistogram,
    InstanceId, ListenerPose, PropagationBackend, ReflectionPath, Result, TraceStats, WorldStats,
    unimplemented,
};
use engine_audio::acoustic::{AcousticFrame, AcousticPublisher, ReverbParams};
use std::convert::Infallible;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceRequest {
    /// `VoiceHandle::get`.
    pub id: u64,
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

/// The propagation service. `AcousticsService::new` is not implemented yet, so no
/// value exists and the methods below are unreachable.
pub struct AcousticsService {
    never: Infallible,
}
impl AcousticsService {
    pub fn new(_config: ServiceConfig, _publisher: AcousticPublisher) -> Result<Self> {
        Err(unimplemented("AcousticsService::new"))
    }
    /// Non-blocking hand-off.
    pub fn set_world(&mut self, _world: AcousticWorld) {
        match self.never {}
    }
    pub fn update_dynamic(&mut self, _updates: &[(InstanceId, Affine3)]) -> Result<()> {
        match self.never {}
    }
    /// Latest wins; never waits for tracing.
    pub fn submit(&mut self, _request: PropagationRequest) -> Result<()> {
        match self.never {}
    }
    /// `try_lock`; `None` if busy or no news.
    pub fn latest(&mut self) -> Option<PropagationSummary> {
        match self.never {}
    }
    /// Synchronous mode only.
    pub fn step_blocking(&mut self) -> Result<&PropagationResult> {
        match self.never {}
    }
    pub fn status(&self) -> AcousticStatus {
        match self.never {}
    }
    /// Re-probes the backends.
    pub fn set_preference(&mut self, _p: BackendPreference) -> Result<()> {
        match self.never {}
    }
    pub fn set_param_hook(&mut self, _hook: Option<ParamHook>) {
        match self.never {}
    }
    /// Joins the worker and publishes dry.
    pub fn shutdown(&mut self) {
        match self.never {}
    }
}
