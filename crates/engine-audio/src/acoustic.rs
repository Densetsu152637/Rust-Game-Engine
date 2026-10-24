//! Acoustic parameter contracts shared by the mixer and the propagation service.
//!
//! This module is the frozen interface from `docs/spatial-audio-design.md` section 7.2.
//! It owns the plain-data parameter types and the std-only publication path that
//! carries propagation results to the audio callback:
//!
//! * [`AcousticPublisher`] is the single writer (propagation worker).
//! * `AcousticConsumer` is the audio-thread reader, held by the mixer.
//! * `ParamSlot` is the shared `Mutex` + generation counter between them.
//!
//! The audio thread never blocks on, allocates for, or frees anything owned by the
//! writer: it only ever uses `Mutex::try_lock` and `mem::swap` on boxed frames.
//! The DSP that applies these parameters lives in the crate's `dsp` module and is
//! driven by the mixer; see the crate README for the signal flow.
//!
//! Conventions shared with the propagation service:
//!
//! * Listener-frame directions are unit vectors pointing from the listener toward
//!   the apparent source, in listener axes +X right, +Y up and -Z forward.
//! * Air absorption (`air_np_per_m`) is amplitude attenuation in nepers per metre:
//!   amplitude falls as `exp(-m d)` and energy as `exp(-2 m d)`.
//! * Gains in [`VoicePropagation`] and [`ReflectionTap`] are amplitudes. Tap
//!   gains are absolute (spreading included); the mixer multiplies them only by
//!   the voice and bus gains. `direct` multiplies the mixer's distance law.

use crate::{AudioError, Bus, Result, VoiceHandle};
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Number of frequency bands used by every acoustic quantity.
pub const BANDS: usize = 4;
/// Band centre frequencies in hertz.
pub const BAND_CENTERS_HZ: [f32; BANDS] = [125.0, 500.0, 2000.0, 8000.0];
/// Crossover frequencies between adjacent bands (geometric means of the centres).
pub const BAND_EDGES_HZ: [f32; BANDS - 1] = [250.0, 1000.0, 4000.0];
/// One value per band.
pub type BandGains = [f32; BANDS];
/// Maximum early-reflection taps carried per voice.
pub const MAX_REFLECTION_TAPS: usize = 16;
/// Maximum voices in one published frame.
pub const MAX_ACOUSTIC_VOICES: usize = 256;
/// Maximum voices rendered through convolution.
pub const MAX_CONVOLUTION_VOICES: usize = 4;
/// Frames over which published gains and directions ramp linearly.
pub const PARAM_RAMP_FRAMES: usize = 1024;
/// Internal block size of the partitioned convolver, in frames.
pub const CONVOLUTION_BLOCK: usize = 256;
/// Longest accepted impulse response, in seconds.
pub const MAX_IMPULSE_SECONDS: u32 = 4;

fn finite_in(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && (low..=high).contains(&value)
}

/// Per-voice acoustic options (`PlayOptions::acoustics`).
///
/// The default has `enabled: false`, which selects the legacy mixer path and keeps
/// output bit-identical to the pre-acoustics mixer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceAcoustics {
    pub enabled: bool,
    /// Delay line providing latency and Doppler (default true).
    pub propagation_delay: bool,
    /// Doppler scale, `0..=4`, default 1. Requires `propagation_delay`.
    pub doppler: f32,
    /// Delay-line capacity in seconds, `0.01..=2.0`, default 0.5.
    pub max_delay_s: f32,
    /// Apply per-band air absorption (default true).
    pub air_absorption: bool,
    /// World units to metres, `0.001..=1000`, default 1.
    pub metres_per_unit: f32,
    /// Scalar reverb send, `0..=1`, default 1.
    pub reverb_send: f32,
}
impl Default for VoiceAcoustics {
    fn default() -> Self {
        Self {
            enabled: false,
            propagation_delay: true,
            doppler: 1.0,
            max_delay_s: 0.5,
            air_absorption: true,
            metres_per_unit: 1.0,
            reverb_send: 1.0,
        }
    }
}
impl VoiceAcoustics {
    /// Range checks. Disabled options are validated too so a later enable cannot fail.
    pub fn validate(&self) -> Result<()> {
        if finite_in(self.doppler, 0.0, 4.0)
            && finite_in(self.max_delay_s, 0.01, 2.0)
            && finite_in(self.metres_per_unit, 0.001, 1000.0)
            && finite_in(self.reverb_send, 0.0, 1.0)
        {
            Ok(())
        } else {
            Err(AudioError::new(
                "audio.play.invalid",
                "Acoustic options out of range (doppler 0-4, max delay 0.01-2 s, metres per unit 0.001-1000, reverb send 0-1)",
            ))
        }
    }
}

/// Output spatialisation law for acoustic voices. `Binaural` (the default, by
/// project decision) is the dataset-free Brown-Duda structural model for
/// headphones; `StereoPan` is the equal-power speaker law. Voices with acoustics
/// disabled always use the legacy equal-power path, whatever the renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpatialRenderer {
    StereoPan,
    #[default]
    Binaural,
}
/// Late-reverb renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReverbRenderer {
    Off,
    #[default]
    Parametric,
    Convolution,
}
/// Binaural model settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BinauralSettings {
    pub head_radius_m: f32,
    pub pinna_echoes: bool,
}
impl Default for BinauralSettings {
    fn default() -> Self {
        Self {
            head_radius_m: 0.0875,
            pinna_echoes: true,
        }
    }
}
/// Mixer-wide acoustic environment (`AudioControl::set_environment`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerEnvironment {
    pub renderer: SpatialRenderer,
    pub reverb: ReverbRenderer,
    pub binaural: BinauralSettings,
    /// Metres per second, `300..=400`, default 343.
    pub speed_of_sound: f32,
    /// Air absorption in nepers per metre per band, each `>= 0`.
    pub air_np_per_m: BandGains,
}
impl Default for MixerEnvironment {
    fn default() -> Self {
        Self {
            renderer: SpatialRenderer::default(),
            reverb: ReverbRenderer::default(),
            binaural: BinauralSettings::default(),
            speed_of_sound: 343.0,
            air_np_per_m: [0.0; BANDS],
        }
    }
}
impl MixerEnvironment {
    pub fn validate(&self) -> Result<()> {
        if finite_in(self.speed_of_sound, 300.0, 400.0)
            && finite_in(self.binaural.head_radius_m, 0.05, 0.12)
            && self.air_np_per_m.iter().all(|v| v.is_finite() && *v >= 0.0)
        {
            Ok(())
        } else {
            Err(AudioError::new(
                "audio.environment.invalid",
                "Environment needs speed of sound 300-400 m/s, head radius 0.05-0.12 m and finite nonnegative air absorption",
            ))
        }
    }
}

/// One early reflection. `direction` is in the listener frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReflectionTap {
    pub delay_s: f32,
    pub gains: BandGains,
    pub direction: [f32; 3],
}
/// Propagation parameters for one voice. Not `Clone`: it may own a kernel box.
pub struct VoicePropagation {
    pub voice: VoiceHandle,
    /// Occlusion times transmission, as amplitude per band.
    pub direct: BandGains,
    /// Listener-frame direction override (proxy space); `None` uses mixer geometry.
    pub direction: Option<[f32; 3]>,
    pub taps: [ReflectionTap; MAX_REFLECTION_TAPS],
    pub tap_count: u8,
    pub reverb_send: f32,
    /// Hero voices only.
    pub kernel: Option<Box<ConvolutionKernel>>,
}
impl VoicePropagation {
    /// Neutral parameters: unity direct gain, no taps, full reverb send.
    pub fn new(voice: VoiceHandle) -> Self {
        Self {
            voice,
            direct: [1.0; BANDS],
            direction: None,
            taps: [ReflectionTap::default(); MAX_REFLECTION_TAPS],
            tap_count: 0,
            reverb_send: 1.0,
            kernel: None,
        }
    }
}
/// Fitted late-field parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReverbParams {
    pub enabled: bool,
    pub rt60_s: BandGains,
    pub level: BandGains,
    pub predelay_s: f32,
    pub mean_free_path_m: f32,
}

/// One published parameter set. Capacities are reserved at construction; the audio
/// thread never allocates or frees one.
pub struct AcousticFrame {
    pub sequence: u64,
    /// Sorted by voice, at most [`MAX_ACOUSTIC_VOICES`].
    pub voices: Vec<VoicePropagation>,
    pub reverb: ReverbParams,
    pub listener_kernel: Option<Box<ConvolutionKernel>>,
}
impl AcousticFrame {
    /// Reserves full voice capacity so later fills do not reallocate.
    pub fn new() -> Self {
        Self {
            sequence: 0,
            voices: Vec::with_capacity(MAX_ACOUSTIC_VOICES),
            reverb: ReverbParams::default(),
            listener_kernel: None,
        }
    }
    /// Resets to the dry (neutral) state, keeping capacity.
    pub fn clear(&mut self) {
        self.sequence = 0;
        self.voices.clear();
        self.reverb = ReverbParams::default();
        self.listener_kernel = None;
    }
}
impl Default for AcousticFrame {
    fn default() -> Self {
        Self::new()
    }
}

/// Stereo impulse response, at most [`MAX_IMPULSE_SECONDS`] long.
#[derive(Clone, Debug, PartialEq)]
pub struct StereoImpulseResponse {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}
/// Frequency-domain partitions of an impulse response for [`CONVOLUTION_BLOCK`].
#[derive(Clone)]
pub struct ConvolutionKernel {
    pub(crate) sample_rate: u32,
    pub(crate) partitions: usize,
    /// Left re, left im, right re, right im; `partitions * (CONVOLUTION_BLOCK + 1)` each.
    pub(crate) spectra: [Vec<f32>; 4],
    pub(crate) fingerprint: u64,
}
impl std::fmt::Debug for ConvolutionKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConvolutionKernel")
            .field("sample_rate", &self.sample_rate)
            .field("partitions", &self.partitions)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}
impl ConvolutionKernel {
    /// Prepares partitions off the audio thread: validates the response (1 sample
    /// to [`MAX_IMPULSE_SECONDS`], finite, 8-192 kHz), resamples it linearly to
    /// `output_rate` and transforms each `CONVOLUTION_BLOCK` partition.
    pub fn prepare(ir: &StereoImpulseResponse, output_rate: u32) -> Result<Self> {
        crate::dsp::convolver::prepare(ir, output_rate)
    }
    /// Output rate the kernel was prepared for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    /// Content hash; the mixer only cross-fades when it changes.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

/// Mixer-side acoustic counters (`AudioControl::acoustic_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcousticMixStats {
    /// Voices currently rendered through the acoustic path.
    pub acoustic_voices: u32,
    /// Voices that requested acoustics but fell back to the legacy path at the
    /// cap (counted once per voice while it stays capped).
    pub over_cap_fallbacks: u64,
    /// Published frames adopted by the mixer.
    pub frames_adopted: u64,
    /// Kernels ignored for a rate mismatch or excess length.
    pub rejected_kernels: u64,
    /// Voices currently rendered with their own convolution kernel.
    pub hero_voices: u32,
    /// The late field is ringing.
    pub reverb_active: bool,
}

pub use crate::dsp::air::{db_per_m_to_np_per_m, iso9613_air_np_per_m};

/// Counters visible to the writer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublisherStats {
    pub published: u64,
    pub consumed: u64,
    pub audio_try_lock_misses: u64,
}

/// Shared slot between one writer and the audio thread.
struct ParamSlot {
    frame: Mutex<Box<AcousticFrame>>,
    /// Cleared when the publisher is dropped, so the mixer can issue a new one.
    writer_alive: AtomicBool,
    generation: AtomicU64,
    consumed: AtomicU64,
    misses: AtomicU64,
}

/// Single writer, obtained through `AudioControl::acoustic_publisher`. Dropping
/// it publishes the dry state and frees the slot for a later publisher.
pub struct AcousticPublisher {
    slot: Arc<ParamSlot>,
    staging: Box<AcousticFrame>,
    published: u64,
}
impl std::fmt::Debug for AcousticPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcousticPublisher")
            .field("published", &self.published)
            .finish_non_exhaustive()
    }
}
impl AcousticPublisher {
    /// Clears and fills the staging frame, then swaps it into the slot under a
    /// short lock. A failed `fill` publishes nothing. Returns the new sequence.
    pub fn publish(&mut self, fill: impl FnOnce(&mut AcousticFrame) -> Result<()>) -> Result<u64> {
        self.staging.clear();
        fill(&mut self.staging)?;
        if self.staging.voices.len() > MAX_ACOUSTIC_VOICES
            || self
                .staging
                .voices
                .iter()
                .any(|v| usize::from(v.tap_count) > MAX_REFLECTION_TAPS)
            || self
                .staging
                .voices
                .windows(2)
                .any(|w| w[0].voice >= w[1].voice)
        {
            return Err(AudioError::new(
                "audio.acoustic.invalid",
                "Frame voices must be sorted, unique, within capacity and tap limits",
            ));
        }
        let sequence = self.published + 1;
        self.staging.sequence = sequence;
        {
            let mut slot = self.slot.frame.lock().map_err(|_| poisoned())?;
            std::mem::swap(&mut *slot, &mut self.staging);
            self.slot.generation.fetch_add(1, Ordering::Release);
        }
        self.published = sequence;
        Ok(sequence)
    }
    pub fn stats(&self) -> PublisherStats {
        PublisherStats {
            published: self.published,
            consumed: self.slot.consumed.load(Ordering::Relaxed),
            audio_try_lock_misses: self.slot.misses.load(Ordering::Relaxed),
        }
    }
    /// Publishes the neutral state: no voices, reverb disabled, no kernels.
    pub fn reset_to_dry(&mut self) -> Result<u64> {
        self.publish(|_| Ok(()))
    }
    /// Holds the slot lock while `stall` runs, simulating a stalled writer.
    #[doc(hidden)]
    pub fn stall_for_test<R>(&self, stall: impl FnOnce() -> R) -> Result<R> {
        let _guard = self.slot.frame.lock().map_err(|_| poisoned())?;
        Ok(stall())
    }
}

impl Drop for AcousticPublisher {
    fn drop(&mut self) {
        // Best effort: the mixer goes dry, and old kernels are freed here.
        let _ = self.reset_to_dry();
        self.slot.writer_alive.store(false, Ordering::Release);
    }
}

fn poisoned() -> AudioError {
    AudioError::new(
        "audio.state.poisoned",
        "Acoustic parameter slot was poisoned",
    )
}

/// Audio-thread reader. Never blocks, allocates or frees.
pub(crate) struct AcousticConsumer {
    slot: Arc<ParamSlot>,
    seen: u64,
    active: Box<AcousticFrame>,
    fading: Box<AcousticFrame>,
}
impl AcousticConsumer {
    /// Adopts the newest frame if one was published and the slot is free.
    /// Returns true when the active frame changed.
    pub(crate) fn poll(&mut self) -> bool {
        let generation = self.slot.generation.load(Ordering::Acquire);
        if generation == self.seen {
            return false;
        }
        match self.slot.frame.try_lock() {
            Ok(mut slot) => {
                // slot <-> fading, then active <-> fading: the new frame becomes
                // active, the previous active stays for cross-fades, and the
                // oldest goes back to the writer to be freed there.
                std::mem::swap(&mut *slot, &mut self.fading);
                std::mem::swap(&mut self.active, &mut self.fading);
                // Re-read under the lock: the writer bumps the generation while
                // holding it, so this value matches the frame just taken. Using
                // the pre-lock value could adopt the returned (older) frame on
                // the next poll if a publish raced in between.
                self.seen = self.slot.generation.load(Ordering::Acquire);
                self.slot.consumed.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
                self.slot.misses.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
    /// Frame currently in force (dry until the first publish is adopted).
    pub(crate) fn active(&self) -> &AcousticFrame {
        &self.active
    }
    /// Previous frame, kept for cross-fades.
    pub(crate) fn fading(&self) -> &AcousticFrame {
        &self.fading
    }
    /// Whether the matching publisher still exists.
    pub(crate) fn writer_alive(&self) -> bool {
        self.slot.writer_alive.load(Ordering::Acquire)
    }
}

/// Creates a connected writer and reader pair. All allocation happens here.
pub(crate) fn channel() -> (AcousticPublisher, AcousticConsumer) {
    let slot = Arc::new(ParamSlot {
        frame: Mutex::new(Box::default()),
        writer_alive: AtomicBool::new(true),
        generation: AtomicU64::new(0),
        consumed: AtomicU64::new(0),
        misses: AtomicU64::new(0),
    });
    (
        AcousticPublisher {
            slot: slot.clone(),
            staging: Box::default(),
            published: 0,
        },
        AcousticConsumer {
            slot,
            seen: 0,
            active: Box::default(),
            fading: Box::default(),
        },
    )
}

/// Context passed to a native effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectContext {
    pub sample_rate: u32,
    pub bus: Bus,
    pub frame_cursor: u64,
}
/// Native per-bus effect. `process` runs on the audio thread: it must not block or allocate.
pub trait DspEffect: Send + 'static {
    fn process(&mut self, frames: &mut [[f32; 2]], context: &EffectContext);
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn voice(n: u64) -> VoiceHandle {
        VoiceHandle(n)
    }

    #[test]
    fn publish_then_poll_adopts_the_frame() {
        let (mut publisher, mut consumer) = channel();
        assert!(!consumer.poll());
        let sequence = publisher
            .publish(|frame| {
                frame.voices.push(VoicePropagation::new(voice(7)));
                frame.reverb.enabled = true;
                Ok(())
            })
            .unwrap();
        assert_eq!(sequence, 1);
        assert!(consumer.poll());
        assert_eq!(consumer.active().sequence, 1);
        assert_eq!(consumer.active().voices[0].voice, voice(7));
        assert!(consumer.active().reverb.enabled);
        assert!(!consumer.poll(), "no news means no change");
        let stats = publisher.stats();
        assert_eq!((stats.published, stats.consumed), (1, 1));
    }

    #[test]
    fn latest_wins_and_previous_frame_is_kept_for_fading() {
        let (mut publisher, mut consumer) = channel();
        for n in 1..=3u64 {
            publisher
                .publish(|frame| {
                    frame.voices.push(VoicePropagation::new(voice(n)));
                    Ok(())
                })
                .unwrap();
        }
        assert!(consumer.poll());
        assert_eq!(consumer.active().sequence, 3);
        assert_eq!(consumer.active().voices.len(), 1);
        assert_eq!(consumer.active().voices[0].voice, voice(3));
        publisher.publish(|_| Ok(())).unwrap();
        assert!(consumer.poll());
        assert_eq!(consumer.active().sequence, 4);
        assert_eq!(consumer.fading().sequence, 3);
        assert_eq!(publisher.stats().consumed, 2);
    }

    #[test]
    fn staging_is_cleared_between_publishes() {
        let (mut publisher, mut consumer) = channel();
        publisher
            .publish(|frame| {
                frame.voices.push(VoicePropagation::new(voice(1)));
                frame.reverb.enabled = true;
                Ok(())
            })
            .unwrap();
        publisher
            .publish(|frame| {
                assert!(frame.voices.is_empty() && !frame.reverb.enabled);
                Ok(())
            })
            .unwrap();
        consumer.poll();
        assert!(consumer.active().voices.is_empty());
    }

    #[test]
    fn failed_fill_and_invalid_frames_publish_nothing() {
        let (mut publisher, mut consumer) = channel();
        let err = publisher
            .publish(|_| Err(AudioError::new("test.fill", "boom")))
            .unwrap_err();
        assert_eq!(err.code, "test.fill");
        let unsorted = publisher.publish(|frame| {
            frame.voices.push(VoicePropagation::new(voice(2)));
            frame.voices.push(VoicePropagation::new(voice(1)));
            Ok(())
        });
        assert_eq!(unsorted.unwrap_err().code, "audio.acoustic.invalid");
        let too_many_taps = publisher.publish(|frame| {
            let mut v = VoicePropagation::new(voice(1));
            v.tap_count = (MAX_REFLECTION_TAPS + 1) as u8;
            frame.voices.push(v);
            Ok(())
        });
        assert_eq!(too_many_taps.unwrap_err().code, "audio.acoustic.invalid");
        assert!(!consumer.poll());
        assert_eq!(publisher.stats().published, 0);
    }

    #[test]
    fn reset_to_dry_clears_voices_and_reverb() {
        let (mut publisher, mut consumer) = channel();
        publisher
            .publish(|frame| {
                frame.voices.push(VoicePropagation::new(voice(1)));
                frame.reverb.enabled = true;
                Ok(())
            })
            .unwrap();
        consumer.poll();
        assert_eq!(publisher.reset_to_dry().unwrap(), 2);
        consumer.poll();
        assert!(consumer.active().voices.is_empty());
        assert_eq!(consumer.active().reverb, ReverbParams::default());
    }

    #[test]
    fn stalled_writer_never_blocks_the_consumer() {
        let (mut publisher, mut consumer) = channel();
        publisher.publish(|_| Ok(())).unwrap();
        let started = Instant::now();
        let (adopted, polls) = publisher
            .stall_for_test(|| {
                let mut adopted = false;
                for _ in 0..200 {
                    adopted |= consumer.poll();
                }
                (adopted, 200u64)
            })
            .unwrap();
        assert!(!adopted);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(publisher.stats().audio_try_lock_misses, polls);
        assert_eq!(
            consumer.active().sequence,
            0,
            "stale parameters stay in force"
        );
        assert!(consumer.poll(), "adopts once the lock is free");
        assert_eq!(consumer.active().sequence, 1);
    }

    #[test]
    fn publisher_and_consumer_work_across_threads() {
        let (mut publisher, mut consumer) = channel();
        let writer = std::thread::spawn(move || {
            for _ in 0..500 {
                publisher.publish(|_| Ok(())).unwrap();
            }
            publisher
        });
        let mut last = 0;
        while last < 500 {
            if consumer.poll() {
                assert!(consumer.active().sequence > last, "sequence is monotonic");
                last = consumer.active().sequence;
            }
            std::thread::yield_now();
        }
        let publisher = writer.join().unwrap();
        assert_eq!(publisher.stats().published, 500);
    }

    #[test]
    fn defaults_validate_and_bad_values_are_rejected() {
        VoiceAcoustics::default().validate().unwrap();
        MixerEnvironment::default().validate().unwrap();
        for bad in [
            VoiceAcoustics {
                doppler: 5.0,
                ..Default::default()
            },
            VoiceAcoustics {
                max_delay_s: 0.0,
                ..Default::default()
            },
            VoiceAcoustics {
                metres_per_unit: f32::NAN,
                ..Default::default()
            },
            VoiceAcoustics {
                reverb_send: 1.5,
                ..Default::default()
            },
        ] {
            assert_eq!(bad.validate().unwrap_err().code, "audio.play.invalid");
        }
        let env = MixerEnvironment {
            speed_of_sound: 100.0,
            ..Default::default()
        };
        assert_eq!(
            env.validate().unwrap_err().code,
            "audio.environment.invalid"
        );
    }

    #[test]
    fn kernel_preparation_validates_and_fingerprints() {
        let ir = StereoImpulseResponse {
            sample_rate: 48_000,
            left: vec![1.0],
            right: vec![1.0],
        };
        let a = ConvolutionKernel::prepare(&ir, 48_000).unwrap();
        assert_eq!(a.sample_rate(), 48_000);
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
        let other = StereoImpulseResponse {
            left: vec![0.5],
            ..ir.clone()
        };
        assert_ne!(
            ConvolutionKernel::prepare(&other, 48_000)
                .unwrap()
                .fingerprint(),
            a.fingerprint()
        );
        let bad = StereoImpulseResponse {
            left: vec![f32::NAN],
            ..ir
        };
        assert_eq!(
            ConvolutionKernel::prepare(&bad, 48_000).unwrap_err().code,
            "audio.kernel.invalid"
        );
    }

    #[test]
    fn dropping_the_publisher_goes_dry_and_frees_the_slot() {
        let (mut publisher, mut consumer) = channel();
        publisher
            .publish(|frame| {
                frame.reverb.enabled = true;
                Ok(())
            })
            .unwrap();
        consumer.poll();
        assert!(consumer.writer_alive());
        drop(publisher);
        assert!(!consumer.writer_alive());
        assert!(consumer.poll());
        assert!(!consumer.active().reverb.enabled);
    }
}
