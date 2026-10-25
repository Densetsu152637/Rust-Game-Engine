//! Mixer-wide acoustic state: shared designs, listener tracking, frame
//! adoption, the late field (FDN and convolution) and hero voices.

use super::{
    BLOCK, band::BandDesign, convolver::Convolver, fdn::LateReverb, hrtf::HeadModel, normalize,
    voice::ListenerFrame,
};
use crate::acoustic::{
    AcousticFrame, AcousticMixStats, ConvolutionKernel, MAX_CONVOLUTION_VOICES, MixerEnvironment,
    ReverbRenderer, VoicePropagation,
};
use crate::mixer::Voice;
use crate::{Listener, VoiceHandle};

/// Default cap on voices rendered through the acoustic path.
pub(crate) const DEFAULT_MAX_ACOUSTIC_VOICES: usize = 32;

struct Hero {
    conv: Convolver,
    voice: Option<VoiceHandle>,
    now: Option<usize>,
    prev: Option<usize>,
    fingerprint: Option<u64>,
    xfade: bool,
}

/// Convolution state, allocated when the convolution renderer is selected.
pub(crate) struct ConvBank {
    listener: Convolver,
    listener_fp: Option<u64>,
    listener_xfade: bool,
    heroes: [Hero; MAX_CONVOLUTION_VOICES],
}

impl ConvBank {
    fn new(rate: u32) -> Box<Self> {
        Box::new(Self {
            listener: Convolver::new(rate),
            listener_fp: None,
            listener_xfade: false,
            heroes: std::array::from_fn(|_| Hero {
                conv: Convolver::new(rate),
                voice: None,
                now: None,
                prev: None,
                fingerprint: None,
                xfade: false,
            }),
        })
    }
}

pub(crate) struct AcousticEngine {
    pub(crate) rate: u32,
    pub(crate) design: BandDesign,
    pub(crate) head: HeadModel,
    pub(crate) env: MixerEnvironment,
    pub(crate) reverb: Option<Box<LateReverb>>,
    conv: Option<Box<ConvBank>>,
    listener_last: Option<[f64; 3]>,
    listener_since: u64,
    pub(crate) max_voices: usize,
    pub(crate) stats: AcousticMixStats,
}

fn lookup(frame: &AcousticFrame, voice: VoiceHandle) -> Option<usize> {
    frame.voices.binary_search_by(|p| p.voice.cmp(&voice)).ok()
}

impl AcousticEngine {
    pub(crate) fn new(rate: u32) -> Self {
        let env = MixerEnvironment::default();
        Self {
            rate,
            design: BandDesign::new(rate),
            head: HeadModel::new(rate, env.binaural.head_radius_m, env.speed_of_sound),
            env,
            reverb: None,
            conv: None,
            listener_last: None,
            listener_since: 0,
            max_voices: DEFAULT_MAX_ACOUSTIC_VOICES,
            stats: AcousticMixStats::default(),
        }
    }

    /// Control thread: applies a validated environment and (de)allocates the
    /// convolution bank. The returned bank (if any) is dropped by the caller.
    pub(crate) fn set_environment(&mut self, env: MixerEnvironment) -> Option<Box<ConvBank>> {
        self.env = env;
        self.head = HeadModel::new(self.rate, env.binaural.head_radius_m, env.speed_of_sound);
        if env.reverb == ReverbRenderer::Convolution {
            if self.conv.is_none() {
                self.conv = Some(ConvBank::new(self.rate));
            }
            None
        } else {
            self.conv.take()
        }
    }

    /// Control thread: allocates the late-field network once a publisher exists.
    pub(crate) fn enable_reverb(&mut self) {
        if self.reverb.is_none() {
            self.reverb = Some(Box::new(LateReverb::new(self.rate)));
        }
    }

    /// A kernel cross-fade is waiting for its block boundary; new frames must
    /// not be adopted until it has run (the old kernel lives in the fading frame).
    pub(crate) fn busy(&self) -> bool {
        self.conv
            .as_ref()
            .is_some_and(|c| c.listener_xfade || c.heroes.iter().any(|h| h.xfade))
    }

    /// True when the late field is silent and nothing can feed it.
    pub(crate) fn late_idle(&self) -> bool {
        self.reverb.as_ref().is_none_or(|r| r.idle())
            && self.conv.as_ref().is_none_or(|c| {
                c.listener_fp.is_none()
                    && !c.listener_xfade
                    && c.heroes.iter().all(|h| h.voice.is_none() && !h.xfade)
            })
    }

    /// Listener pose and motion for this mix call.
    pub(crate) fn listener_frame(&mut self, listener: &Listener, frames: usize) -> ListenerFrame {
        let position = listener.position.map(f64::from);
        let moved = match self.listener_last {
            Some(last) if last != position => Some(self.listener_since.max(1)),
            _ => None,
        };
        if moved.is_some() || self.listener_last.is_none() {
            self.listener_last = Some(position);
            self.listener_since = 0;
        }
        self.listener_since += frames as u64;
        let forward = normalize(listener.forward.map(f64::from)).unwrap_or([0.0, 0.0, -1.0]);
        let up0 = listener.up.map(f64::from);
        let right = normalize([
            forward[1] * up0[2] - forward[2] * up0[1],
            forward[2] * up0[0] - forward[0] * up0[2],
            forward[0] * up0[1] - forward[1] * up0[0],
        ])
        .unwrap_or([1.0, 0.0, 0.0]);
        let up = [
            right[1] * forward[2] - right[2] * forward[1],
            right[2] * forward[0] - right[0] * forward[2],
            right[0] * forward[1] - right[1] * forward[0],
        ];
        ListenerFrame {
            position,
            right,
            up,
            forward,
            moved,
        }
    }

    fn accepted<'a>(
        &self,
        conv: &Convolver,
        kernel: Option<&'a ConvolutionKernel>,
    ) -> Option<&'a ConvolutionKernel> {
        kernel.filter(|k| conv.accepts(k, self.rate))
    }

    /// Adopts `active` (audio thread after a poll, or control thread after a
    /// configuration change with `adopted == false`).
    pub(crate) fn apply_frame(
        &mut self,
        voices: &mut [Voice],
        active: &AcousticFrame,
        adopted: bool,
    ) {
        if adopted {
            self.stats.frames_adopted += 1;
        }
        for v in voices.iter_mut() {
            let handle = v.handle;
            if let Some(dsp) = &mut v.dsp {
                let p: Option<&VoicePropagation> =
                    lookup(active, handle).map(|i| &active.voices[i]);
                dsp.apply(p, self.rate);
            }
        }
        let convolution = self.env.reverb == ReverbRenderer::Convolution;
        let mut listener_kernel = false;
        if let Some(mut bank) = self.conv.take() {
            let now = self.accepted(&bank.listener, active.listener_kernel.as_deref());
            if active.listener_kernel.is_some() && now.is_none() {
                self.stats.rejected_kernels += 1;
            }
            let fp = now.map(|k| k.fingerprint);
            if fp != bank.listener_fp {
                bank.listener_fp = fp;
                bank.listener_xfade = true;
            }
            listener_kernel = fp.is_some();
            self.assign_heroes(&mut bank, voices, active);
            self.conv = Some(bank);
        } else {
            for dsp in voices.iter_mut().filter_map(|v| v.dsp.as_deref_mut()) {
                dsp.hero = None;
            }
        }
        let parametric =
            self.env.reverb == ReverbRenderer::Parametric || (convolution && !listener_kernel);
        if let Some(reverb) = &mut self.reverb {
            reverb.set_params(
                &self.design,
                &active.reverb,
                parametric,
                self.env.speed_of_sound,
            );
        }
    }

    fn assign_heroes(&mut self, bank: &mut ConvBank, voices: &mut [Voice], active: &AcousticFrame) {
        let present = |h: VoiceHandle| voices.iter().any(|v| v.handle == h && v.dsp.is_some());
        for hero in &mut bank.heroes {
            let Some(voice) = hero.voice else {
                hero.prev = None;
                continue;
            };
            let index = lookup(active, voice).filter(|_| present(voice));
            let kernel = index.and_then(|i| active.voices[i].kernel.as_deref());
            let kernel = kernel.filter(|k| hero.conv.accepts(k, self.rate));
            hero.prev = hero.now;
            match (index, kernel) {
                (Some(i), Some(k)) => {
                    hero.now = Some(i);
                    if hero.fingerprint != Some(k.fingerprint) {
                        hero.fingerprint = Some(k.fingerprint);
                        hero.xfade = true;
                    }
                }
                _ => {
                    hero.voice = None;
                    hero.now = None;
                    hero.fingerprint = None;
                    hero.xfade = true;
                }
            }
        }
        for (i, p) in active.voices.iter().enumerate() {
            let Some(kernel) = p.kernel.as_deref() else {
                continue;
            };
            if bank.heroes.iter().any(|h| h.voice == Some(p.voice)) || !present(p.voice) {
                continue;
            }
            let Some(hero) = bank
                .heroes
                .iter_mut()
                .find(|h| h.voice.is_none() && !h.xfade)
            else {
                break;
            };
            if !hero.conv.accepts(kernel, self.rate) {
                self.stats.rejected_kernels += 1;
                continue;
            }
            hero.conv.reset();
            hero.voice = Some(p.voice);
            hero.now = Some(i);
            hero.prev = None;
            hero.fingerprint = Some(kernel.fingerprint);
            hero.xfade = true;
        }
        for v in voices.iter_mut() {
            let handle = v.handle;
            if let Some(dsp) = &mut v.dsp {
                dsp.hero = bank.heroes.iter().position(|h| h.voice == Some(handle));
            }
        }
        self.stats.hero_voices = bank.heroes.iter().filter(|h| h.voice.is_some()).count() as u32;
    }

    /// Renders the late field for one block and adds it to `out`.
    pub(crate) fn late(
        &mut self,
        active: &AcousticFrame,
        fading: &AcousticFrame,
        shared: &[f32],
        heroes: &[[f32; BLOCK]; MAX_CONVOLUTION_VOICES],
        out: &mut [[f32; 2]],
    ) {
        let n = shared.len();
        let mut listener_kernel = false;
        if let Some(bank) = &mut self.conv {
            let now = active
                .listener_kernel
                .as_deref()
                .filter(|k| bank.listener.accepts(k, self.rate));
            let prev = fading
                .listener_kernel
                .as_deref()
                .filter(|k| bank.listener.accepts(k, self.rate));
            listener_kernel = now.is_some();
            if now.is_some() || bank.listener_xfade {
                bank.listener
                    .process(shared, out, now, prev, &mut bank.listener_xfade);
            }
            for (slot, hero) in bank.heroes.iter_mut().enumerate() {
                if hero.voice.is_none() && !hero.xfade {
                    continue;
                }
                let now = pick(active, hero.now);
                let prev = pick(fading, hero.prev);
                hero.conv
                    .process(&heroes[slot][..n], out, now, prev, &mut hero.xfade);
            }
        }
        if let Some(reverb) = &mut self.reverb {
            let feed: &[f32] = if listener_kernel {
                &[0.0; BLOCK][..n]
            } else {
                shared
            };
            reverb.process(&self.design, self.env.speed_of_sound, feed, out);
            self.stats.reverb_active = !reverb.idle();
        }
    }
}

fn pick(frame: &AcousticFrame, index: Option<usize>) -> Option<&ConvolutionKernel> {
    index
        .and_then(|i| frame.voices.get(i))
        .and_then(|p| p.kernel.as_deref())
}
