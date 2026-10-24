//! Deterministic stereo mixing and native output with explicit scene ownership.
//!
//! [`AudioEngine`] owns the output device; [`AudioControl`] can be cloned into
//! application resources. Dropping a source controller stops its voices, and
//! dropping the engine stops the device and invalidates surviving controls.

pub mod acoustic;
mod clip;
mod device;
mod mixer;
mod sources;

pub use clip::{Clip, decode_wav, load_wav};
pub use mixer::{AudioControl, VoiceHandle, VoiceSnapshot, VoiceState};
pub use sources::{AudioSources, ObserverHandle, SourceAction, SourceHandle};

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Audio failures retain a stable machine-readable code and a human message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioError {
    pub code: &'static str,
    pub message: String,
}

impl AudioError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AudioError {}
pub type Result<T> = std::result::Result<T, AudioError>;

pub(crate) const MAX_WAV_BYTES: usize = 64 * 1024 * 1024;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_handle() -> Result<u64> {
    NEXT_HANDLE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| AudioError::new("audio.handle.exhausted", "Audio handle space exhausted"))
}
pub(crate) fn valid_rate(rate: u32) -> bool {
    (8_000..=192_000).contains(&rate)
}
pub(crate) fn gain(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Output rate in Hz and the maximum number of simultaneous playing/paused voices.
#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub max_voices: usize,
}
impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            max_voices: 64,
        }
    }
}
impl AudioConfig {
    fn validate(self) -> Result<Self> {
        if !valid_rate(self.sample_rate) || !(1..=1024).contains(&self.max_voices) {
            return Err(AudioError::new(
                "audio.options.invalid",
                "Output rate must be 8-192 kHz and voice limit 1-1024",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bus {
    Master,
    Music,
    #[default]
    Effects,
    Dialogue,
}
impl Bus {
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

pub type Vec3 = [f32; 3];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Attenuation {
    None,
    Linear,
    #[default]
    Inverse,
}

/// Distances use world units. Stereo clips become mono emitters when enabled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialOptions {
    pub enabled: bool,
    pub position: Vec3,
    pub min_distance: f32,
    pub max_distance: f32,
    pub rolloff: f32,
    pub attenuation: Attenuation,
}
impl Default for SpatialOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            position: [0.0; 3],
            min_distance: 1.0,
            max_distance: 100.0,
            rolloff: 1.0,
            attenuation: Attenuation::Inverse,
        }
    }
}

/// Right-handed world: +X right, +Y up, default forward -Z.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}
impl Default for Listener {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        }
    }
}
impl Listener {
    pub fn validate(&self) -> Result<()> {
        let f = self.forward.map(f64::from);
        let u = self.up.map(f64::from);
        let length = |v: [f64; 3]| v[0].hypot(v[1]).hypot(v[2]);
        if !self
            .position
            .iter()
            .chain(&self.forward)
            .chain(&self.up)
            .all(|v| v.is_finite())
            || length(f) < 1e-6
            || length(u) < 1e-6
            || ((f[0] * u[0] + f[1] * u[1] + f[2] * u[2]) / (length(f) * length(u))).abs()
                > 0.999999
        {
            return Err(AudioError::new(
                "audio.listener.invalid",
                "Listener vectors must be finite, nonzero and nonparallel",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayOptions {
    pub looped: bool,
    pub bus: Bus,
    pub gain: f32,
    pub owner: String,
    pub pitch: f32,
    pub spatial: SpatialOptions,
    /// Acoustic propagation options. The default is disabled, which keeps the
    /// legacy mixing path and bit-identical output.
    pub acoustics: acoustic::VoiceAcoustics,
}
impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            looped: false,
            bus: Bus::Effects,
            gain: 1.0,
            owner: String::new(),
            pitch: 1.0,
            spatial: SpatialOptions::default(),
            acoustics: acoustic::VoiceAcoustics::default(),
        }
    }
}
impl PlayOptions {
    /// Validates configuration before mutation. Gain is separately clamped to
    /// [0,1], with nonfinite gain becoming silence; pitch is restricted to 1/8-8.
    pub fn validate(&self) -> Result<()> {
        let s = &self.spatial;
        if self.bus == Bus::Master
            || self.owner.len() > 256
            || !self.pitch.is_finite()
            || !(0.125..=8.0).contains(&self.pitch)
            || !s.position.iter().all(|v| v.is_finite())
            || !s.min_distance.is_finite()
            || s.min_distance <= 0.0
            || !s.max_distance.is_finite()
            || s.max_distance <= s.min_distance
            || !s.rolloff.is_finite()
            || s.rolloff < 0.0
        {
            return Err(AudioError::new(
                "audio.play.invalid",
                "Invalid bus, owner, pitch or spatial options",
            ));
        }
        self.acoustics.validate()?;
        Ok(())
    }
}

/// Owns a live CPAL stream or a deterministic offline mixer. Construct/drop on
/// the host lifecycle thread; cloned controls are safe to use across threads.
pub struct AudioEngine {
    control: AudioControl,
    stream: Option<cpal::Stream>,
    device_frames: Arc<AtomicU64>,
}
impl AudioEngine {
    pub fn offline(config: AudioConfig) -> Result<Self> {
        let config = config.validate()?;
        Ok(Self {
            control: AudioControl {
                inner: Arc::new(Mutex::new(mixer::Mixer::new(config))),
            },
            stream: None,
            device_frames: Arc::new(AtomicU64::new(0)),
        })
    }
    /// Opens and starts the default native output device. Failure never falls
    /// back to offline success. The device must support the requested stereo rate.
    pub fn device(config: AudioConfig) -> Result<Self> {
        let mut engine = Self::offline(config)?;
        engine.stream = Some(device::open(&engine.control, engine.device_frames.clone())?);
        engine.control.lock()?.ready()?;
        Ok(engine)
    }
    pub fn control(&self) -> AudioControl {
        self.control.clone()
    }
    /// Fills interleaved stereo; odd output lengths are rejected atomically.
    /// Live output owns its cursor, so offline mixing on a device engine is rejected.
    pub fn mix(&mut self, output: &mut [f32]) -> Result<()> {
        if self.stream.is_some() {
            return Err(AudioError::new(
                "audio.mix.device_active",
                "Device output owns the playback cursor",
            ));
        }
        if !output.len().is_multiple_of(2) {
            return Err(AudioError::new(
                "audio.mix.invalid",
                "Expected interleaved stereo output",
            ));
        }
        let mut mixer = self.control.lock()?;
        mixer.ready()?;
        mixer.mix(output);
        Ok(())
    }
    pub fn playback_enabled(&self) -> bool {
        self.stream.is_some() && self.control.lock().is_ok_and(|m| m.ready().is_ok())
    }
    pub fn device_error(&self) -> Option<AudioError> {
        self.control.lock().map_or_else(Some, |m| m.failure.clone())
    }
    /// Frames actually submitted from the device callback (not proof of audibility).
    pub fn device_frames(&self) -> u64 {
        self.device_frames.load(Ordering::Relaxed)
    }
    pub fn shutdown(&mut self) {
        // Stop callbacks before invalidating state. Surviving controls remain
        // valid handles but reject playback after shutdown.
        self.stream.take();
        if let Ok(mut mixer) = self.control.inner.lock() {
            mixer.running = false;
            mixer.voices.clear();
            mixer.listener = Listener::default();
        }
    }
}
impl std::ops::Deref for AudioEngine {
    type Target = AudioControl;
    fn deref(&self) -> &Self::Target {
        &self.control
    }
}
impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
