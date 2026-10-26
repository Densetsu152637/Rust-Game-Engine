use crate::acoustic::{
    AcousticConsumer, AcousticFrame, AcousticMixStats, AcousticPublisher, DspEffect, EffectContext,
    MAX_ACOUSTIC_VOICES, MAX_CONVOLUTION_VOICES, MixerEnvironment, ReverbParams, channel,
};
use crate::dsp::{
    BLOCK,
    render::AcousticEngine,
    voice::{Context, Send, VoiceDsp},
};
use crate::stream::{StreamConfig, StreamGeneration, StreamPlayback, StreamSnapshot, StreamWriter};
use crate::{
    Attenuation, AudioConfig, AudioError, Bus, Clip, Listener, PlayOptions, Result, SpatialOptions,
    allocate_handle, gain,
};
use std::sync::{Arc, Mutex, MutexGuard};

enum VoiceInput {
    Clip(Arc<Clip>),
    Stream(StreamPlayback),
}
impl VoiceInput {
    fn finished(&self, cursor: f64, looped: bool) -> bool {
        match self {
            Self::Clip(c) => !looped && cursor >= c.frames() as f64,
            Self::Stream(s) => s.ended(),
        }
    }
}

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
    pub(crate) handle: VoiceHandle,
    input: VoiceInput,
    options: PlayOptions,
    cursor: f64,
    paused: bool,
    /// Acoustic DSP state; `None` renders through the legacy path.
    pub(crate) dsp: Option<Box<VoiceDsp>>,
    /// Acoustics were requested but the voice cap was reached (counted once).
    capped: bool,
}
pub(crate) struct Mixer {
    pub(crate) config: AudioConfig,
    pub(crate) voices: Vec<Voice>,
    gains: [f32; 4],
    pub(crate) listener: Listener,
    pub(crate) running: bool,
    pub(crate) failure: Option<AudioError>,
    effects: [Option<Box<dyn DspEffect>>; 4],
    acoustic: Option<AcousticConsumer>,
    engine: AcousticEngine,
    /// DSP states retired by the audio thread, freed by the next control call.
    retired: Vec<Box<VoiceDsp>>,
    frame_cursor: u64,
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
            effects: [None, None, None, None],
            acoustic: None,
            engine: AcousticEngine::new(config.sample_rate),
            retired: Vec::with_capacity(config.max_voices),
            frame_cursor: 0,
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
        // May run in the callback: park DSP boxes instead of freeing them here.
        for voice in &mut self.voices {
            if let Some(dsp) = voice.dsp.take()
                && self.retired.len() < self.retired.capacity()
            {
                self.retired.push(dsp);
            }
        }
        self.voices.clear();
        self.listener = Listener::default();
    }

    /// Control thread: (re)builds or drops acoustic state for `options`.
    /// Returns the state and whether the request is held back by the cap.
    fn attach_dsp(
        &mut self,
        options: &PlayOptions,
        existing: Option<Box<VoiceDsp>>,
        was_capped: bool,
    ) -> (Option<Box<VoiceDsp>>, bool) {
        if !(options.acoustics.enabled && options.spatial.enabled) {
            return (None, false);
        }
        let rate = self.config.sample_rate;
        if let Some(dsp) = existing
            && dsp.built_for(rate, options.acoustics.max_delay_s)
        {
            return (Some(dsp), false);
        }
        let in_use = self.voices.iter().filter(|v| v.dsp.is_some()).count();
        if in_use >= self.engine.max_voices {
            if !was_capped {
                self.engine.stats.over_cap_fallbacks += 1;
            }
            return (None, true);
        }
        let dsp = VoiceDsp::new(rate, options.acoustics.max_delay_s, &self.engine.head);
        (Some(dsp), false)
    }

    /// Re-applies the active frame after a control-side change.
    fn reapply_frame(&mut self) {
        let empty = empty_frame();
        let active = self.acoustic.as_ref().map_or(&empty, |c| c.active());
        self.engine.apply_frame(&mut self.voices, active, false);
    }

    pub(crate) fn mix(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        let mut adopted = false;
        if !self.engine.busy()
            && let Some(acoustic) = &mut self.acoustic
        {
            // Adopt the newest published parameters; never waits for the writer.
            adopted = acoustic.poll();
        }
        if !self.running || self.failure.is_some() {
            return;
        }
        if adopted && let Some(acoustic) = &self.acoustic {
            self.engine
                .apply_frame(&mut self.voices, acoustic.active(), true);
        }
        let general = self.effects.iter().any(Option::is_some)
            || self.voices.iter().any(|v| v.dsp.is_some())
            || !self.engine.late_idle();
        let rate = self.config.sample_rate;
        if general {
            self.mix_general(output);
        } else {
            // Legacy summation path, unchanged: bit-identical output.
            for voice in &mut self.voices {
                if voice.paused {
                    continue;
                }
                let volume =
                    voice.options.gain * self.gains[0] * self.gains[voice.options.bus.index()];
                let spatial = spatial_gains(voice.options.spatial, self.listener);
                render_legacy(voice, rate, output.as_chunks_mut::<2>().0, volume, spatial);
            }
        }
        let retired = &mut self.retired;
        self.voices.retain_mut(|v| {
            let finished = v.input.finished(v.cursor, v.options.looped);
            let alive = !finished || v.dsp.as_ref().is_some_and(|d| !d.drained());
            if !alive
                && let Some(dsp) = v.dsp.take()
                && retired.len() < retired.capacity()
            {
                retired.push(dsp);
            }
            alive
        });
        for sample in output.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        self.frame_cursor += (output.len() / 2) as u64;
    }

    /// Bus-structured path: acoustic voices, bus effects and the late field.
    fn mix_general(&mut self, output: &mut [f32]) {
        let frames = output.len() / 2;
        let rate = self.config.sample_rate;
        let listener = self.engine.listener_frame(&self.listener, frames);
        {
            let engine = &self.engine;
            let ctx = Context {
                design: &engine.design,
                head: &engine.head,
                env: &engine.env,
                listener: &listener,
                rate,
            };
            for voice in &mut self.voices {
                if voice.paused {
                    continue;
                }
                if let Some(dsp) = &mut voice.dsp {
                    let g = voice.options.gain * self.gains[voice.options.bus.index()];
                    dsp.prepare(
                        &ctx,
                        &voice.options.spatial,
                        &voice.options.acoustics,
                        g,
                        frames,
                    );
                }
            }
        }
        let empty = empty_frame();
        let (active, fading) = self
            .acoustic
            .as_ref()
            .map_or((&empty, &empty), |c| (c.active(), c.fading()));
        let pinna = self.engine.env.binaural.pinna_echoes;
        let out = output.as_chunks_mut::<2>().0;
        let mut start = 0;
        while start < frames {
            let n = BLOCK.min(frames - start);
            let mut buses = [[[0.0f32; 2]; BLOCK]; 4];
            let mut shared = [0.0f32; BLOCK];
            let mut heroes = [[0.0f32; BLOCK]; MAX_CONVOLUTION_VOICES];
            for voice in &mut self.voices {
                if voice.paused {
                    continue;
                }
                let bus = voice.options.bus.index();
                if voice.dsp.is_some() {
                    let mut mono = [0.0f32; BLOCK];
                    let produced = read_mono(voice, rate, &mut mono[..n]);
                    let Some(dsp) = &mut voice.dsp else {
                        continue;
                    };
                    if produced < n && voice.input.finished(voice.cursor, voice.options.looped) {
                        dsp.end_input();
                    }
                    let send = match dsp.hero {
                        Some(slot) => Send::Hero(&mut heroes[slot][..n]),
                        None => Send::Shared(&mut shared[..n]),
                    };
                    dsp.render(
                        &self.engine.design,
                        &self.engine.head,
                        pinna,
                        &mono[..n],
                        &mut buses[bus][..n],
                        send,
                    );
                } else {
                    let volume = voice.options.gain * self.gains[bus];
                    let spatial = spatial_gains(voice.options.spatial, self.listener);
                    render_legacy(voice, rate, &mut buses[bus][..n], volume, spatial);
                }
            }
            let cursor = self.frame_cursor + start as u64;
            for bus in [Bus::Music, Bus::Effects, Bus::Dialogue] {
                if let Some(effect) = &mut self.effects[bus.index()] {
                    effect.process(
                        &mut buses[bus.index()][..n],
                        &EffectContext {
                            sample_rate: rate,
                            bus,
                            frame_cursor: cursor,
                        },
                    );
                }
            }
            let (master, rest) = buses.split_at_mut(1);
            let master = &mut master[0][..n];
            for bus in rest.iter() {
                for (m, b) in master.iter_mut().zip(bus) {
                    m[0] += b[0];
                    m[1] += b[1];
                }
            }
            self.engine
                .late(active, fading, &shared[..n], &heroes, master);
            for m in master.iter_mut() {
                m[0] *= self.gains[0];
                m[1] *= self.gains[0];
            }
            if let Some(effect) = &mut self.effects[Bus::Master.index()] {
                effect.process(
                    master,
                    &EffectContext {
                        sample_rate: rate,
                        bus: Bus::Master,
                        frame_cursor: cursor,
                    },
                );
            }
            for (o, m) in out[start..start + n].iter_mut().zip(master.iter()) {
                o[0] += m[0];
                o[1] += m[1];
            }
            start += n;
        }
    }
}

/// A dry frame that owns no heap memory (safe to build and drop on the audio thread).
fn empty_frame() -> AcousticFrame {
    AcousticFrame {
        sequence: 0,
        voices: Vec::new(),
        reverb: ReverbParams::default(),
        listener_kernel: None,
    }
}

/// The legacy per-voice synthesis loop (linear interpolation, equal-power pan).
fn render_legacy(
    voice: &mut Voice,
    rate: u32,
    out: &mut [[f32; 2]],
    volume: f32,
    spatial: [f32; 2],
) {
    let clip = match &mut voice.input {
        VoiceInput::Clip(clip) => clip,
        VoiceInput::Stream(stream) => {
            stream.render(rate, voice.options.pitch, out.len(), |i, sample| {
                let sample = if voice.options.spatial.enabled {
                    [(sample[0] + sample[1]) * 0.5; 2]
                } else {
                    sample
                };
                for c in 0..2 {
                    out[i][c] += sample[c] * volume * spatial[c];
                }
            });
            return;
        }
    };
    let frames = clip.frames();
    let step = f64::from(clip.sample_rate()) / f64::from(rate) * f64::from(voice.options.pitch);
    for frame in out {
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

/// Mono source signal for an acoustic voice (stereo clips are downmixed, as
/// on the legacy spatial path); returns the frames produced before a one-shot
/// clip ended. The rest of `out` stays zero.
fn read_mono(voice: &mut Voice, rate: u32, out: &mut [f32]) -> usize {
    let clip = match &mut voice.input {
        VoiceInput::Clip(clip) => clip,
        VoiceInput::Stream(stream) => {
            return stream.render(rate, voice.options.pitch, out.len(), |i, sample| {
                out[i] = (sample[0] + sample[1]) * 0.5;
            });
        }
    };
    let frames = clip.frames();
    let step = f64::from(clip.sample_rate()) / f64::from(rate) * f64::from(voice.options.pitch);
    let samples = clip.samples();
    let stereo = clip.channels() == 2;
    let sample = |index: usize| {
        if stereo {
            (samples[index * 2] + samples[index * 2 + 1]) * 0.5
        } else {
            samples[index]
        }
    };
    for (i, o) in out.iter_mut().enumerate() {
        if voice.cursor >= frames as f64 {
            if !voice.options.looped {
                return i;
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
        let a = sample(first);
        let b = sample(second);
        *o = a + (b - a) * alpha;
        voice.cursor += step;
    }
    out.len()
}

/// Thread-safe controls. All full configurations validate before taking effect;
/// source cursor and paused state survive reconfiguration.
#[derive(Clone)]
pub struct AudioControl {
    pub(crate) inner: Arc<Mutex<Mixer>>,
}
impl AudioControl {
    /// Locks the mixer from the audio path (never frees retired DSP state).
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Mixer>> {
        self.inner
            .lock()
            .map_err(|_| AudioError::new("audio.state.poisoned", "Audio state lock was poisoned"))
    }
    /// Locks the mixer from a control call and frees DSP state retired by the
    /// audio thread, so it is never freed inside the callback.
    fn control(&self) -> Result<MutexGuard<'_, Mixer>> {
        let mut mixer = self.lock()?;
        mixer.retired.clear();
        Ok(mixer)
    }
    pub fn play(&self, clip: Arc<Clip>, mut options: PlayOptions) -> Result<VoiceHandle> {
        options.validate()?;
        options.gain = gain(options.gain);
        let mut mixer = self.control()?;
        mixer.ready()?;
        if mixer.voices.len() >= mixer.config.max_voices {
            return Err(AudioError::new(
                "audio.voice.limit",
                "Concurrent voice limit reached",
            ));
        }
        let handle = VoiceHandle(allocate_handle()?);
        let (dsp, capped) = mixer.attach_dsp(&options, None, false);
        let acoustic = dsp.is_some();
        mixer.voices.push(Voice {
            handle,
            input: VoiceInput::Clip(clip),
            options,
            cursor: 0.0,
            paused: false,
            dsp,
            capped,
        });
        if acoustic {
            // Parameters already published for this handle apply immediately.
            mixer.reapply_frame();
        }
        Ok(handle)
    }
    /// Starts a bounded PCM queue. All producer work runs outside the audio callback.
    pub fn play_stream(
        &self,
        config: StreamConfig,
        mut options: PlayOptions,
    ) -> Result<(VoiceHandle, StreamWriter)> {
        options.validate()?;
        if options.looped {
            return Err(AudioError::new(
                "audio.stream.loop",
                "Stream loops require producer seek/replay",
            ));
        }
        options.gain = gain(options.gain);
        let mut mixer = self.control()?;
        mixer.ready()?;
        if mixer.voices.len() >= mixer.config.max_voices {
            return Err(AudioError::new(
                "audio.voice.limit",
                "Concurrent voice limit reached",
            ));
        }
        let (stream, writer) = StreamPlayback::new(config)?;
        let handle = VoiceHandle(allocate_handle()?);
        let (dsp, capped) = mixer.attach_dsp(&options, None, false);
        let acoustic = dsp.is_some();
        mixer.voices.push(Voice {
            handle,
            input: VoiceInput::Stream(stream),
            options,
            cursor: 0.,
            paused: false,
            dsp,
            capped,
        });
        if acoustic {
            mixer.reapply_frame();
        }
        Ok((handle, writer))
    }
    pub fn stream_snapshot(&self, handle: VoiceHandle) -> Result<Option<StreamSnapshot>> {
        let m = self.lock()?;
        match m
            .voices
            .iter()
            .find(|v| v.handle == handle)
            .map(|v| &v.input)
        {
            Some(VoiceInput::Stream(s)) => s.snapshot().map(Some),
            _ => Ok(None),
        }
    }
    /// Invalidates buffered PCM and old producer work; decoding must resume at `source_frame`.
    pub fn seek_stream(&self, handle: VoiceHandle, source_frame: u64) -> Result<StreamGeneration> {
        let mut m = self.control()?;
        m.ready()?;
        let index = m
            .voices
            .iter()
            .position(|v| v.handle == handle)
            .ok_or_else(|| AudioError::new("audio.stream.unknown", "Unknown stream voice"))?;
        let VoiceInput::Stream(s) = &mut m.voices[index].input else {
            return Err(AudioError::new(
                "audio.stream.unknown",
                "Voice is not streamed",
            ));
        };
        let generation = s.seek(source_frame)?;
        // Recreate delay/filter state on the control thread so pre-seek audio cannot leak.
        let options = m.voices[index].options.clone();
        let old = m.voices[index].dsp.take();
        drop(old);
        let (dsp, capped) = m.attach_dsp(&options, None, false);
        m.voices[index].dsp = dsp;
        m.voices[index].capped = capped;
        m.reapply_frame();
        Ok(generation)
    }
    pub fn stop(&self, handle: VoiceHandle) -> bool {
        self.control().is_ok_and(|mut m| {
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
        self.control().is_ok_and(|mut m| {
            m.voices
                .iter_mut()
                .find(|v| v.handle == handle)
                .is_some_and(|v| {
                    v.paused = paused;
                    if let VoiceInput::Stream(s) = &v.input {
                        s.pause(paused);
                    }
                    true
                })
        })
    }
    pub fn set_gain(&self, handle: VoiceHandle, value: f32) -> bool {
        self.control().is_ok_and(|mut m| {
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
        let mut m = self.control()?;
        m.ready()?;
        let Some(index) = m.voices.iter().position(|v| v.handle == handle) else {
            return Ok(false);
        };
        if options.looped && matches!(m.voices[index].input, VoiceInput::Stream(_)) {
            return Err(AudioError::new(
                "audio.stream.loop",
                "Stream loops require producer seek/replay",
            ));
        }
        let existing = m.voices[index].dsp.take();
        let had = existing.is_some();
        let was_capped = m.voices[index].capped;
        let (dsp, capped) = m.attach_dsp(&options, existing, was_capped);
        let fresh = dsp.as_ref().is_some_and(|d| !had || !d.started());
        let voice = &mut m.voices[index];
        voice.dsp = dsp;
        voice.capped = capped;
        voice.options = options;
        if fresh {
            m.reapply_frame();
        }
        Ok(true)
    }
    pub fn set_listener(&self, listener: Listener) -> Result<()> {
        listener.validate()?;
        let mut m = self.control()?;
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
                cursor: match &v.input {
                    VoiceInput::Clip(_) => v.cursor,
                    VoiceInput::Stream(s) => s.snapshot().map_or(v.cursor, |s| s.source_frame),
                },
                options: v.options.clone(),
            })
    }
    pub fn voice_count(&self) -> usize {
        self.lock().map_or(0, |m| m.voices.len())
    }
    pub fn stop_owner(&self, owner: &str) {
        if let Ok(mut mixer) = self.control() {
            mixer.voices.retain(|v| v.options.owner != owner);
        }
    }
    pub fn stop_all(&self) {
        if let Ok(mut mixer) = self.control() {
            mixer.voices.clear();
        }
    }
    pub fn set_bus_gain(&self, bus: Bus, value: f32) -> Result<()> {
        let mut m = self.control()?;
        m.ready()?;
        m.gains[bus.index()] = gain(value);
        Ok(())
    }
    /// Sets the mixer-wide acoustic environment after validating it. Selecting
    /// `ReverbRenderer::Convolution` allocates the convolution state here.
    pub fn set_environment(&self, environment: MixerEnvironment) -> Result<()> {
        environment.validate()?;
        let mut m = self.control()?;
        m.ready()?;
        let released = m.engine.set_environment(environment);
        m.reapply_frame();
        drop(m);
        drop(released);
        Ok(())
    }
    pub fn environment(&self) -> Result<MixerEnvironment> {
        Ok(self.lock()?.engine.env)
    }
    /// Installs or removes a bus effect and returns the previous one, which the
    /// caller drops so the audio thread never frees it. A new effect is `reset`
    /// before it is installed. `process` receives blocks of at most 256 frames;
    /// bus effects see that bus after voice and bus gains, and the `Master`
    /// effect sees the sum of all buses plus the late reverb after master gain.
    pub fn set_bus_effect(
        &self,
        bus: Bus,
        mut effect: Option<Box<dyn DspEffect>>,
    ) -> Result<Option<Box<dyn DspEffect>>> {
        if let Some(effect) = &mut effect {
            effect.reset();
        }
        let mut m = self.control()?;
        m.ready()?;
        Ok(std::mem::replace(&mut m.effects[bus.index()], effect))
    }
    /// Returns the parameter writer. Only one can exist at a time: a second call
    /// fails while the first is alive and succeeds again once it is dropped.
    pub fn acoustic_publisher(&self) -> Result<AcousticPublisher> {
        let mut m = self.control()?;
        m.ready()?;
        if m.acoustic
            .as_ref()
            .is_some_and(AcousticConsumer::writer_alive)
        {
            return Err(AudioError::new(
                "audio.acoustic.publisher_taken",
                "The acoustic publisher was already obtained",
            ));
        }
        let (publisher, consumer) = channel();
        let previous = m.acoustic.replace(consumer);
        m.engine.enable_reverb();
        m.reapply_frame();
        drop(m);
        drop(previous);
        Ok(publisher)
    }
    pub fn bus_gain(&self, bus: Bus) -> Result<f32> {
        Ok(self.lock()?.gains[bus.index()])
    }
    /// Caps voices rendered through the acoustic path (0..=256, default 32).
    /// Applies to later `play`/`configure` calls; voices over the cap use the
    /// legacy path and are counted in `AcousticMixStats::over_cap_fallbacks`.
    pub fn set_max_acoustic_voices(&self, limit: usize) -> Result<()> {
        if limit > MAX_ACOUSTIC_VOICES {
            return Err(AudioError::new(
                "audio.options.invalid",
                "Acoustic voice limit must be 0-256",
            ));
        }
        self.control()?.engine.max_voices = limit;
        Ok(())
    }
    /// Mixer-side acoustic counters.
    pub fn acoustic_stats(&self) -> Result<AcousticMixStats> {
        let m = self.lock()?;
        Ok(AcousticMixStats {
            acoustic_voices: m.voices.iter().filter(|v| v.dsp.is_some()).count() as u32,
            ..m.engine.stats
        })
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

    #[test]
    fn retired_dsp_is_parked_until_a_control_call() {
        let mut engine = AudioEngine::offline(AudioConfig::default()).unwrap();
        let control = engine.control();
        let mut options = PlayOptions::default();
        options.spatial.enabled = true;
        options.acoustics.enabled = true;
        options.acoustics.propagation_delay = false;
        control
            .play(Clip::new(48_000, 1, vec![0.5; 64]).unwrap(), options)
            .unwrap();
        let mut out = vec![0.0; 4096];
        for _ in 0..4 {
            engine.mix(&mut out).unwrap();
        }
        assert_eq!(control.voice_count(), 0, "drained one-shot retires");
        assert_eq!(control.lock().unwrap().retired.len(), 1);
        control.set_bus_gain(Bus::Music, 1.0).unwrap();
        assert!(control.lock().unwrap().retired.is_empty());
    }
}
