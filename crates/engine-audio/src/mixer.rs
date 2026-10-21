use crate::{
    Attenuation, AudioConfig, AudioError, Bus, Clip, Listener, PlayOptions, Result, SpatialOptions,
    allocate_handle, gain,
};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoiceHandle(pub(crate) u64);
impl VoiceHandle {
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceState {
    Stopped,
    Playing,
    Paused,
}
/// Cursor is measured in source PCM frames (possibly fractional after resampling).
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSnapshot {
    pub state: VoiceState,
    pub cursor: f64,
    pub options: PlayOptions,
}
pub(crate) struct Voice {
    handle: VoiceHandle,
    clip: Arc<Clip>,
    options: PlayOptions,
    cursor: f64,
    paused: bool,
}
pub(crate) struct Mixer {
    pub(crate) config: AudioConfig,
    pub(crate) voices: Vec<Voice>,
    gains: [f32; 4],
    pub(crate) listener: Listener,
    pub(crate) running: bool,
    pub(crate) failure: Option<AudioError>,
}
impl Mixer {
    pub(crate) fn new(config: AudioConfig) -> Self {
        Self {
            config,
            voices: Vec::with_capacity(config.max_voices),
            gains: [1.0; 4],
            listener: Listener::default(),
            running: true,
            failure: None,
        }
    }
    pub(crate) fn ready(&self) -> Result<()> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if !self.running {
            return Err(AudioError::new(
                "audio.not_initialized",
                "Audio engine has shut down",
            ));
        }
        Ok(())
    }
    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        self.failure
            .get_or_insert_with(|| AudioError::new("audio.device.failed", message));
        self.voices.clear();
        self.listener = Listener::default();
    }
    pub(crate) fn mix(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        if !self.running || self.failure.is_some() {
            return;
        }
        for voice in &mut self.voices {
            if voice.paused {
                continue;
            }
            let clip = &voice.clip;
            let frames = clip.frames();
            let volume = voice.options.gain * self.gains[0] * self.gains[voice.options.bus.index()];
            let step = f64::from(clip.sample_rate()) / f64::from(self.config.sample_rate)
                * f64::from(voice.options.pitch);
            let spatial = spatial_gains(voice.options.spatial, self.listener);
            for frame in output.as_chunks_mut::<2>().0 {
                if voice.cursor >= frames as f64 {
                    if !voice.options.looped {
                        break;
                    }
                    voice.cursor %= frames as f64;
                }
                let first = voice.cursor as usize;
                let second = if first + 1 < frames {
                    first + 1
                } else if voice.options.looped {
                    0
                } else {
                    first
                };
                let alpha = (voice.cursor - first as f64) as f32;
                for channel in 0..2 {
                    let sample = |index| {
                        if voice.options.spatial.enabled && clip.channels() == 2 {
                            (clip.samples()[index * 2] + clip.samples()[index * 2 + 1]) * 0.5
                        } else {
                            clip.samples()[index * clip.channels() as usize
                                + if clip.channels() == 1 { 0 } else { channel }]
                        }
                    };
                    let a = sample(first);
                    let b = sample(second);
                    frame[channel] += (a + (b - a) * alpha) * volume * spatial[channel];
                }
                voice.cursor += step;
            }
        }
        self.voices
            .retain(|v| v.options.looped || v.cursor < v.clip.frames() as f64);
        for sample in output {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

/// Thread-safe controls. All full configurations validate before taking effect;
/// source cursor and paused state survive reconfiguration.
#[derive(Clone)]
pub struct AudioControl {
    pub(crate) inner: Arc<Mutex<Mixer>>,
}
impl AudioControl {
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Mixer>> {
        self.inner
            .lock()
            .map_err(|_| AudioError::new("audio.state.poisoned", "Audio state lock was poisoned"))
    }
    pub fn play(&self, clip: Arc<Clip>, mut options: PlayOptions) -> Result<VoiceHandle> {
        options.validate()?;
        options.gain = gain(options.gain);
        let mut mixer = self.lock()?;
        mixer.ready()?;
        if mixer.voices.len() >= mixer.config.max_voices {
            return Err(AudioError::new(
                "audio.voice.limit",
                "Concurrent voice limit reached",
            ));
        }
        let handle = VoiceHandle(allocate_handle()?);
        mixer.voices.push(Voice {
            handle,
            clip,
            options,
            cursor: 0.0,
            paused: false,
        });
        Ok(handle)
    }
    pub fn stop(&self, handle: VoiceHandle) -> bool {
        self.lock().is_ok_and(|mut m| {
            let len = m.voices.len();
            m.voices.retain(|v| v.handle != handle);
            len != m.voices.len()
        })
    }
    pub fn pause(&self, handle: VoiceHandle) -> bool {
        self.set_paused(handle, true)
    }
    pub fn resume(&self, handle: VoiceHandle) -> bool {
        self.set_paused(handle, false)
    }
    fn set_paused(&self, handle: VoiceHandle, paused: bool) -> bool {
        self.lock().is_ok_and(|mut m| {
            m.voices
                .iter_mut()
                .find(|v| v.handle == handle)
                .is_some_and(|v| {
                    v.paused = paused;
                    true
                })
        })
    }
    pub fn set_gain(&self, handle: VoiceHandle, value: f32) -> bool {
        self.lock().is_ok_and(|mut m| {
            m.voices
                .iter_mut()
                .find(|v| v.handle == handle)
                .is_some_and(|v| {
                    v.options.gain = gain(value);
                    true
                })
        })
    }
    pub fn configure(&self, handle: VoiceHandle, mut options: PlayOptions) -> Result<bool> {
        options.validate()?;
        options.gain = gain(options.gain);
        let mut m = self.lock()?;
        m.ready()?;
        if let Some(voice) = m.voices.iter_mut().find(|v| v.handle == handle) {
            voice.options = options;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn set_listener(&self, listener: Listener) -> Result<()> {
        listener.validate()?;
        let mut m = self.lock()?;
        m.ready()?;
        m.listener = listener;
        Ok(())
    }
    pub fn reset_listener(&self) -> Result<()> {
        self.set_listener(Listener::default())
    }
    pub fn listener(&self) -> Result<Listener> {
        Ok(self.lock()?.listener)
    }
    pub fn voice_state(&self, handle: VoiceHandle) -> VoiceState {
        self.voice(handle).map_or(VoiceState::Stopped, |v| v.state)
    }
    pub fn voice(&self, handle: VoiceHandle) -> Option<VoiceSnapshot> {
        self.lock()
            .ok()?
            .voices
            .iter()
            .find(|v| v.handle == handle)
            .map(|v| VoiceSnapshot {
                state: if v.paused {
                    VoiceState::Paused
                } else {
                    VoiceState::Playing
                },
                cursor: v.cursor,
                options: v.options.clone(),
            })
    }
    pub fn voice_count(&self) -> usize {
        self.lock().map_or(0, |m| m.voices.len())
    }
    pub fn stop_owner(&self, owner: &str) {
        if let Ok(mut mixer) = self.lock() {
            mixer.voices.retain(|v| v.options.owner != owner);
        }
    }
    pub fn stop_all(&self) {
        if let Ok(mut mixer) = self.lock() {
            mixer.voices.clear();
        }
    }
    pub fn set_bus_gain(&self, bus: Bus, value: f32) -> Result<()> {
        let mut m = self.lock()?;
        m.ready()?;
        m.gains[bus.index()] = gain(value);
        Ok(())
    }
    pub fn bus_gain(&self, bus: Bus) -> Result<f32> {
        Ok(self.lock()?.gains[bus.index()])
    }
}

fn spatial_gains(source: SpatialOptions, listener: Listener) -> [f32; 2] {
    if !source.enabled {
        return [1.0; 2];
    }
    let delta: [f64; 3] =
        std::array::from_fn(|i| f64::from(source.position[i]) - f64::from(listener.position[i]));
    let distance = delta[0].hypot(delta[1]).hypot(delta[2]);
    let mut volume = 1.0;
    if source.attenuation != Attenuation::None {
        if distance >= f64::from(source.max_distance) {
            return [0.0; 2];
        }
        let min = f64::from(source.min_distance);
        let d = distance.max(min);
        volume = match source.attenuation {
            Attenuation::Linear => (1.0
                - f64::from(source.rolloff) * (d - min) / (f64::from(source.max_distance) - min))
                .clamp(0.0, 1.0),
            _ => min / (min + f64::from(source.rolloff) * (d - min)),
        };
    }
    let f = listener.forward.map(f64::from);
    let u = listener.up.map(f64::from);
    let r = [
        f[1] * u[2] - f[2] * u[1],
        f[2] * u[0] - f[0] * u[2],
        f[0] * u[1] - f[1] * u[0],
    ];
    let length = r[0].hypot(r[1]).hypot(r[2]);
    let pan = if distance > 0.0 {
        ((delta[0] * r[0] + delta[1] * r[1] + delta[2] * r[2]) / (distance * length))
            .clamp(-1.0, 1.0)
    } else {
        0.0
    };
    [
        (volume * ((1.0 - pan) / 2.0).sqrt()) as f32,
        (volume * ((1.0 + pan) / 2.0).sqrt()) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioEngine, AudioSources, SourceAction};

    #[test]
    fn backend_failure_is_sticky_rejects_play_and_cleans_owned_voices() {
        let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
        let control = engine.control();
        let mut sources = AudioSources::new(control.clone(), "scene").unwrap();
        let source = sources
            .create(
                Clip::new(48_000, 1, vec![0.5]).unwrap(),
                PlayOptions {
                    looped: true,
                    ..Default::default()
                },
            )
            .unwrap();
        sources.control(source, SourceAction::Play).unwrap();
        control
            .set_listener(Listener {
                position: [3.0, 0.0, 0.0],
                ..Default::default()
            })
            .unwrap();
        // Exercise the exact failure transition used by CPAL's error callback.
        control.lock().unwrap().fail("device disconnected");
        control.lock().unwrap().fail("later diagnostic");
        assert_eq!(
            engine.device_error().unwrap().message,
            "device disconnected"
        );
        assert_eq!(control.voice_count(), 0);
        assert_eq!(control.listener().unwrap(), Listener::default());
        assert_eq!(sources.state(source), VoiceState::Stopped);
        assert_eq!(
            sources
                .control(source, SourceAction::Play)
                .unwrap_err()
                .code,
            "audio.device.failed"
        );
        assert!(sources.voice(source).is_none());
        assert_eq!(
            engine.mix(&mut [1.0; 2]).unwrap_err().code,
            "audio.device.failed"
        );
        engine.shutdown();
        assert_eq!(
            engine.device_error().unwrap().message,
            "device disconnected"
        );
    }
}
