use crate::{AcousticWorld, BandGains};

/// What a backend traces: the stochastic energy model of section 5.4.4.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnergyJob {
    pub emitter: [f32; 3],
    pub receiver: [f32; 3],
    pub receiver_radius_m: f32,
    pub rays: u32,
    pub max_bounces: u16,
    pub seed: u64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceSettings {
    pub bin_s: f32,
    pub bins: u32,
    pub speed_of_sound: f32,
    pub air_np_per_m: BandGains,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnergyHistogram {
    pub bin_s: f32,
    pub bins: Vec<BandGains>,
    pub rays: u32,
    pub escaped: f32,
    pub aborted: u32,
    pub mean_free_path_m: f32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceStats {
    pub rays: u64,
    pub segments: u64,
    pub elapsed_us: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum BackendKind {
    Cpu,
    GpuCompute,
    GpuRayQuery,
}
/// `Auto` (the default) tries hardware ray query when its self-test passes, then
/// GPU compute, then CPU. An explicit preference tries that tier and then the
/// lower tiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackendPreference {
    #[default]
    Auto,
    Cpu,
    GpuCompute,
    GpuRayQuery,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendInfo {
    pub kind: BackendKind,
    pub hardware_rt: bool,
    pub adapter: Option<String>,
    pub driver: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub code: &'static str,
    pub message: String,
    pub fatal: bool,
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for BackendError {}

pub trait PropagationBackend: Send {
    fn info(&self) -> BackendInfo;
    /// Capability probe plus self-test. `Err` means the tier is unavailable; the
    /// message becomes the fallback reason.
    fn initialize(&mut self) -> Result<(), BackendError>;
    /// A changed revision means a full rebuild; dynamic transforms are an update.
    fn sync_world(&mut self, world: &AcousticWorld) -> Result<(), BackendError>;
    /// Runs on the propagation worker only. Fills `out[i]` for `jobs[i]`.
    fn trace(
        &mut self,
        world: &AcousticWorld,
        settings: &TraceSettings,
        jobs: &[EnergyJob],
        out: &mut Vec<EnergyHistogram>,
    ) -> Result<TraceStats, BackendError>;
}

/// CPU tier. `threads` is `1..=8`.
#[derive(Clone, Copy, Debug)]
pub struct CpuBackend {
    threads: u8,
}
impl CpuBackend {
    pub fn new(threads: u8) -> Result<Self, BackendError> {
        if (1..=8).contains(&threads) {
            Ok(Self { threads })
        } else {
            Err(BackendError {
                code: "acoustics.request.invalid",
                message: "CPU backend threads must be 1-8".into(),
                fatal: false,
            })
        }
    }
    pub fn threads(&self) -> u8 {
        self.threads
    }
}
fn unimplemented_backend(what: &str) -> BackendError {
    BackendError {
        code: "acoustics.unimplemented",
        message: format!("{what} is not implemented yet"),
        fatal: false,
    }
}
impl PropagationBackend for CpuBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            kind: BackendKind::Cpu,
            hardware_rt: false,
            adapter: None,
            driver: None,
        }
    }
    fn initialize(&mut self) -> Result<(), BackendError> {
        Err(unimplemented_backend("CpuBackend::initialize"))
    }
    fn sync_world(&mut self, _world: &AcousticWorld) -> Result<(), BackendError> {
        Err(unimplemented_backend("CpuBackend::sync_world"))
    }
    fn trace(
        &mut self,
        _world: &AcousticWorld,
        _settings: &TraceSettings,
        _jobs: &[EnergyJob],
        _out: &mut Vec<EnergyHistogram>,
    ) -> Result<TraceStats, BackendError> {
        Err(unimplemented_backend("CpuBackend::trace"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectPath {
    pub visible_fraction: f32,
    pub gains: BandGains,
    pub distance_m: f32,
    pub direction: [f32; 3],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflectionPath {
    pub order: u8,
    pub delay_s: f32,
    pub length_m: f32,
    pub gains: BandGains,
    pub direction: [f32; 3],
}
