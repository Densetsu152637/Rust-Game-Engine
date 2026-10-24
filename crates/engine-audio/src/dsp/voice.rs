//! Per-voice acoustic rendering (section 7.3).
//!
//! Signal flow per voice (mono source, already resampled by the mixer):
//!
//! ```text
//! mono -+-> raw line --------(delay D)--> reverb send (shared bus)
//!       +-> BandFilter(direct x distance x air) -> direct line -(D + ear ITD)-> spatializer
//!       +-> BandFilter(early-reflection shape) -> early line --(D + tap + ITD)-> per-tap spatializer
//!       +-> hero send (emission time, when the voice owns a convolution slot)
//! ```
//!
//! `D` is the propagation delay. It is solved at emission time from the
//! source velocity, `|L(t) - S(t - D)| = c D`, and ramped linearly to each new
//! target over the position update interval, so constant velocity gives a
//! constant Doppler ratio (`c / (c - v)` for a moving source, `(c + v) / c` for a
//! moving listener). Larger jumps than a 2x pitch change duck the voice for
//! 2 x 128 frames and move the delay while silent.

use super::{
    BLOCK,
    band::{BandDesign, BandFilter},
    delay::DelayLine,
    dot,
    hrtf::{self, EAR_GAIN, HeadModel, PINNA_ECHOES, Pinna, Shadow},
    normalize,
    ramp::{DirRamp, Ramp, Ramp4},
    sanitize,
};
use crate::acoustic::{
    BANDS, MAX_REFLECTION_TAPS, MixerEnvironment, PARAM_RAMP_FRAMES, SpatialRenderer,
    VoicePropagation,
};
use crate::{Attenuation, SpatialOptions};

const TAP_SLOTS: usize = 2 * MAX_REFLECTION_TAPS;
/// Minimum read delay (cubic Hermite needs one newer sample).
const BASE: f64 = 1.0;
/// Largest delay change per frame before the voice ducks instead of gliding.
const MAX_SLOPE: f64 = 0.5;
/// Tap delays glide at most this many samples per sample (5 % pitch).
const TAP_SLEW: f64 = 0.05;
/// Taps whose delays differ by less than this are treated as the same path.
const TAP_MATCH_S: f64 = 0.000_5;
const SNAP_FRAMES: usize = 128;
const FRONT: [f64; 3] = [0.0, 0.0, -1.0];

/// Listener pose for one mix call, in world units.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListenerFrame {
    pub(crate) position: [f64; 3],
    pub(crate) right: [f64; 3],
    pub(crate) up: [f64; 3],
    pub(crate) forward: [f64; 3],
    /// The listener moved since the previous mix call, `interval` frames apart.
    pub(crate) moved: Option<u64>,
}

impl ListenerFrame {
    /// Listener-frame unit direction (+X right, +Y up, -Z forward) toward `world`.
    fn direction(&self, delta: [f64; 3]) -> [f64; 3] {
        normalize([
            dot(delta, self.right),
            dot(delta, self.up),
            -dot(delta, self.forward),
        ])
        .unwrap_or(FRONT)
    }
}

/// Shared read-only state for preparing voices in one mix call.
pub(crate) struct Context<'a> {
    pub(crate) design: &'a BandDesign,
    pub(crate) head: &'a HeadModel,
    pub(crate) env: &'a MixerEnvironment,
    pub(crate) listener: &'a ListenerFrame,
    pub(crate) rate: u32,
}

/// Where a voice's reverb send goes.
pub(crate) enum Send<'a> {
    /// Shared late field (FDN or listener kernel); taken after the propagation delay.
    Shared(&'a mut [f32]),
    /// The voice's own convolution slot; taken at emission time because the
    /// pair impulse response already contains the propagation time.
    Hero(&'a mut [f32]),
}

#[derive(Clone, Copy, Debug)]
struct Ear {
    itd: Ramp,
    shadow: Shadow,
}

#[derive(Clone, Copy, Debug)]
struct Tap {
    live: bool,
    fresh: bool,
    matched: bool,
    delay: f64,
    target_delay: f64,
    amp: f32,
    gain: Ramp,
    dir: DirRamp,
    pan: [Ramp; 2],
    ears: [Ear; 2],
}

impl Tap {
    fn idle() -> Self {
        Self {
            live: false,
            fresh: false,
            matched: false,
            delay: 0.0,
            target_delay: 0.0,
            amp: 0.0,
            gain: Ramp::new(0.0),
            dir: DirRamp::new(FRONT),
            pan: [Ramp::new(0.0); 2],
            ears: [Ear {
                itd: Ramp::new(0.0),
                shadow: Shadow::new((1.0, 0.0)),
            }; 2],
        }
    }
}

/// Acoustic state of one voice. Allocated on the control thread.
pub(crate) struct VoiceDsp {
    raw: DelayLine,
    direct: DelayLine,
    early: DelayLine,
    max_delay: f64,
    direct_filter: BandFilter,
    early_filter: BandFilter,
    direct_lanes: Ramp4,
    early_lanes: Ramp4,
    prop_direct: [f32; BANDS],
    prop_send: f32,
    er_shape: [f32; BANDS],
    override_dir: DirRamp,
    override_blend: Ramp,
    taps: [Tap; TAP_SLOTS],
    send: Ramp,
    pan: [Ramp; 2],
    ears: [Ear; 2],
    pinna: [Pinna; 2],
    pinna_delays: [Ramp; PINNA_ECHOES],
    renderer: SpatialRenderer,
    started: bool,
    last_pos: [f64; 3],
    velocity: [f64; 3],
    since_move: u64,
    interval: u64,
    delay0: f64,
    delay: f64,
    delay_step: f64,
    delay_left: u64,
    snap_to: Option<f64>,
    duck: Ramp,
    tail: Option<u64>,
    /// Convolution slot owned by this voice, if any.
    pub(crate) hero: Option<usize>,
}

/// Mixer distance law (identical to the legacy equal-power path).
pub(crate) fn distance_volume(source: &SpatialOptions, distance: f64) -> f64 {
    if source.attenuation == Attenuation::None {
        return 1.0;
    }
    if distance >= f64::from(source.max_distance) {
        return 0.0;
    }
    let min = f64::from(source.min_distance);
    let d = distance.max(min);
    match source.attenuation {
        Attenuation::Linear => (1.0
            - f64::from(source.rolloff) * (d - min) / (f64::from(source.max_distance) - min))
            .clamp(0.0, 1.0),
        _ => min / (min + f64::from(source.rolloff) * (d - min)),
    }
}

/// Emission-time delay in seconds for source-to-listener vector `delta_m`
/// (metres) and source velocity `v` (m/s): the positive root of
/// `|delta + v D| = c D`.
pub(crate) fn retarded_delay(delta_m: [f64; 3], v: [f64; 3], c: f64) -> f64 {
    let speed = dot(v, v).sqrt();
    let v = if speed > 0.5 * c {
        v.map(|x| x * 0.5 * c / speed)
    } else {
        v
    };
    let a = c * c - dot(v, v);
    let b = dot(delta_m, v);
    (b + (b * b + a * dot(delta_m, delta_m)).sqrt()) / a
}

impl VoiceDsp {
    /// Allocates delay lines for `max_delay_s` at `rate`.
    pub(crate) fn new(rate: u32, max_delay_s: f32, head: &HeadModel) -> Box<Self> {
        let max_delay = (f64::from(max_delay_s) * f64::from(rate)).ceil();
        let capacity = max_delay as usize + head.max_ear_delay().ceil() as usize + BLOCK + 8;
        Box::new(Self {
            raw: DelayLine::new(capacity),
            direct: DelayLine::new(capacity),
            early: DelayLine::new(capacity),
            max_delay,
            direct_filter: BandFilter::default(),
            early_filter: BandFilter::default(),
            direct_lanes: Ramp4::new([0.0; BANDS]),
            early_lanes: Ramp4::new([0.0; BANDS]),
            prop_direct: [1.0; BANDS],
            prop_send: 1.0,
            er_shape: [1.0; BANDS],
            override_dir: DirRamp::new(FRONT),
            override_blend: Ramp::new(0.0),
            taps: [Tap::idle(); TAP_SLOTS],
            send: Ramp::new(0.0),
            pan: [Ramp::new(0.0); 2],
            ears: [Ear {
                itd: Ramp::new(0.0),
                shadow: Shadow::new((1.0, 0.0)),
            }; 2],
            pinna: [Pinna::default(), Pinna::default()],
            pinna_delays: [Ramp::new(0.0); PINNA_ECHOES],
            renderer: SpatialRenderer::default(),
            started: false,
            last_pos: [0.0; 3],
            velocity: [0.0; 3],
            since_move: 0,
            interval: 0,
            delay0: 0.0,
            delay: 0.0,
            delay_step: 0.0,
            delay_left: 0,
            snap_to: None,
            duck: Ramp::new(1.0),
            tail: None,
            hero: None,
        })
    }

    /// Whether the first mix call has initialised this state.
    pub(crate) fn started(&self) -> bool {
        self.started
    }
    /// Delay-line capacity this state was built for.
    pub(crate) fn built_for(&self, rate: u32, max_delay_s: f32) -> bool {
        self.max_delay == (f64::from(max_delay_s) * f64::from(rate)).ceil()
    }

    /// Adopts published propagation (`None` = neutral: unity direct, no taps).
    pub(crate) fn apply(&mut self, p: Option<&VoicePropagation>, rate: u32) {
        let ramp = PARAM_RAMP_FRAMES;
        let Some(p) = p else {
            self.prop_direct = [1.0; BANDS];
            self.prop_send = 1.0;
            self.override_blend.set(0.0, ramp);
            self.taps
                .iter_mut()
                .filter(|t| t.live)
                .for_each(|t| t.amp = 0.0);
            return;
        };
        self.prop_direct = p.direct.map(|g| sanitize(g, 0.0, 16.0));
        self.prop_send = sanitize(p.reverb_send, 0.0, 1.0);
        match p.direction.and_then(|d| normalize(d.map(f64::from))) {
            Some(dir) => {
                if self.override_blend.target() == 0.0 && self.override_blend.settled() {
                    self.override_dir = DirRamp::new(dir);
                } else {
                    self.override_dir.set(dir, ramp);
                }
                self.override_blend.set(1.0, ramp);
            }
            None => self.override_blend.set(0.0, ramp),
        }
        for tap in &mut self.taps {
            tap.matched = false;
        }
        let fs = f64::from(rate);
        let window = TAP_MATCH_S * fs;
        let mut energy = [0.0f64; BANDS];
        let mut total = 0.0f64;
        for t in &p.taps[..usize::from(p.tap_count).min(MAX_REFLECTION_TAPS)] {
            let gains = t.gains.map(|g| sanitize(g, 0.0, 16.0));
            let amp = (gains.iter().map(|g| g * g).sum::<f32>() / BANDS as f32).sqrt();
            let delay = f64::from(sanitize(t.delay_s, 0.0, 10.0)) * fs;
            let dir = normalize(t.direction.map(f64::from)).unwrap_or(FRONT);
            for b in 0..BANDS {
                energy[b] += f64::from(gains[b] * gains[b]);
            }
            total += f64::from(amp * amp);
            let matched = self
                .taps
                .iter_mut()
                .filter(|s| s.live && !s.matched && (s.target_delay - delay).abs() <= window)
                .min_by(|a, b| {
                    (a.target_delay - delay)
                        .abs()
                        .total_cmp(&(b.target_delay - delay).abs())
                });
            if let Some(slot) = matched {
                slot.matched = true;
                slot.target_delay = delay;
                slot.amp = amp;
                slot.dir.set(dir, ramp);
                continue;
            }
            let slot = match self.taps.iter().position(|s| !s.live) {
                Some(i) => i,
                None => self
                    .taps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.matched)
                    .min_by(|a, b| a.1.gain.value().total_cmp(&b.1.gain.value()))
                    .map_or(0, |(i, _)| i),
            };
            self.taps[slot] = Tap {
                live: true,
                fresh: true,
                matched: true,
                delay,
                target_delay: delay,
                amp,
                dir: DirRamp::new(dir),
                ..Tap::idle()
            };
        }
        for tap in self.taps.iter_mut().filter(|t| t.live && !t.matched) {
            tap.amp = 0.0;
        }
        self.er_shape = if total > 0.0 {
            energy.map(|e| (e / total).sqrt() as f32)
        } else {
            [1.0; BANDS]
        };
    }

    /// Per-mix-call parameter update. `gain` is voice gain times bus gain.
    pub(crate) fn prepare(
        &mut self,
        ctx: &Context<'_>,
        spatial: &SpatialOptions,
        acoustics: &crate::acoustic::VoiceAcoustics,
        gain: f32,
        frames: usize,
    ) {
        let fs = f64::from(ctx.rate);
        let fresh = !self.started || self.renderer != ctx.env.renderer;
        self.renderer = ctx.env.renderer;
        // Source motion.
        let pos = spatial.position.map(f64::from);
        let mut moved = ctx.listener.moved;
        if !self.started {
            self.last_pos = pos;
        } else if pos != self.last_pos {
            let dt = self.since_move.max(1);
            self.velocity = std::array::from_fn(|i| (pos[i] - self.last_pos[i]) / dt as f64);
            self.last_pos = pos;
            self.interval = dt;
            self.since_move = 0;
            moved = Some(moved.map_or(dt, |m| m.max(dt)));
        } else if self.since_move > 2 * self.interval + 4096 {
            self.velocity = [0.0; 3];
        }
        self.since_move += frames as u64;

        // Geometry.
        let delta: [f64; 3] = std::array::from_fn(|i| pos[i] - ctx.listener.position[i]);
        let distance = dot(delta, delta).sqrt();
        let mpu = f64::from(acoustics.metres_per_unit);
        let distance_m = distance * mpu;
        let volume = distance_volume(spatial, distance) as f32;
        let audible = if volume > 0.0 || spatial.attenuation == Attenuation::None {
            1.0
        } else {
            0.0
        };
        let geometric = ctx.listener.direction(delta);
        let blend = f64::from(self.override_blend.advance(frames));
        let dir = if blend <= 0.0 {
            geometric
        } else {
            let o = self.override_dir.at(frames);
            normalize(std::array::from_fn(|i| {
                geometric[i] * (1.0 - blend) + o[i] * blend
            }))
            .unwrap_or(o)
        };
        self.override_dir.advance(frames);

        // Propagation delay and Doppler.
        let c = f64::from(ctx.env.speed_of_sound);
        let target = if acoustics.propagation_delay {
            let delta_m = delta.map(|d| -d * mpu);
            let v = self.velocity.map(|x| x * fs * mpu);
            let physical = retarded_delay(delta_m, v, c) * fs;
            if !self.started {
                self.delay0 = physical;
            }
            self.delay0 + f64::from(acoustics.doppler) * (physical - self.delay0)
        } else {
            0.0
        };
        self.set_delay(
            target.clamp(0.0, self.max_delay),
            moved.unwrap_or(frames as u64).max(1),
        );

        // Gains.
        let air: [f32; BANDS] = if acoustics.air_absorption {
            ctx.env
                .air_np_per_m
                .map(|m| (-f64::from(m) * distance_m).exp() as f32)
        } else {
            [1.0; BANDS]
        };
        let ramp = if fresh { 0 } else { PARAM_RAMP_FRAMES };
        let direct: [f32; BANDS] =
            std::array::from_fn(|b| volume * gain * self.prop_direct[b] * air[b]);
        self.direct_lanes.set(ctx.design.lanes(direct), ramp);
        self.early_lanes.set(ctx.design.lanes(self.er_shape), ramp);
        self.send.set(
            gain * acoustics.reverb_send * self.prop_send * audible,
            ramp,
        );

        // Spatializer targets, ramped across this call.
        let spread = if fresh { 0 } else { frames };
        match self.renderer {
            SpatialRenderer::StereoPan => set_pan(&mut self.pan, dir, spread),
            SpatialRenderer::Binaural => {
                set_ears(&mut self.ears, ctx.head, dir, spread);
                let (az, el) = hrtf::azimuth_elevation(dir);
                let pd = ctx.head.pinna_delays(az, el);
                for k in 0..PINNA_ECHOES {
                    self.pinna_delays[k].set(pd[k], spread);
                }
            }
        }
        for tap in self.taps.iter_mut().filter(|t| t.live) {
            let tdir = tap.dir.at(frames);
            tap.dir.advance(frames);
            let spread = if tap.fresh || fresh { 0 } else { frames };
            let gain_ramp = if tap.fresh {
                PARAM_RAMP_FRAMES
            } else {
                ramp.max(1)
            };
            match self.renderer {
                SpatialRenderer::StereoPan => set_pan(&mut tap.pan, tdir, spread),
                SpatialRenderer::Binaural => set_ears(&mut tap.ears, ctx.head, tdir, spread),
            }
            tap.gain.set(tap.amp * gain * audible, gain_ramp);
            tap.fresh = false;
        }
        self.started = true;
    }

    fn set_delay(&mut self, target: f64, ramp_len: u64) {
        if !self.started {
            self.delay = target;
            self.delay_left = 0;
            return;
        }
        if let Some(pending) = &mut self.snap_to {
            *pending = target;
            return;
        }
        let change = target - self.delay;
        if change == 0.0 {
            self.delay_left = 0;
        } else if change.abs() / ramp_len as f64 > MAX_SLOPE {
            self.snap_to = Some(target);
            self.duck.set(0.0, SNAP_FRAMES);
        } else {
            self.delay_step = change / ramp_len as f64;
            self.delay_left = ramp_len;
        }
    }

    fn advance_delay(&mut self, dly: &mut [f64], duck: &mut [f32]) {
        for (d, g) in dly.iter_mut().zip(duck.iter_mut()) {
            if let Some(t) = self.snap_to
                && self.duck.settled()
                && self.duck.value() == 0.0
            {
                self.delay = t;
                self.delay_left = 0;
                self.snap_to = None;
                self.duck.set(1.0, SNAP_FRAMES);
            }
            if self.delay_left > 0 {
                self.delay += self.delay_step;
                self.delay_left -= 1;
            }
            *d = self.delay;
            *g = self.duck.next();
        }
    }

    /// Starts draining after the clip ended (once).
    pub(crate) fn end_input(&mut self) {
        if self.tail.is_none() {
            let taps = self
                .taps
                .iter()
                .filter(|t| t.live)
                .map(|t| t.delay)
                .fold(0.0, f64::max);
            self.tail = Some((self.delay + taps) as u64 + 64 + 2 * BLOCK as u64);
        }
    }
    /// True once the propagation tail has fully played after the clip ended.
    pub(crate) fn drained(&self) -> bool {
        self.tail == Some(0)
    }

    /// Renders one block (`mono.len() <= BLOCK`) into `out` and the reverb send.
    ///
    /// Whenever a path's delay and gains are constant across the block (the
    /// common steady state), it reads the delay line with block reads and runs
    /// its filters with fixed coefficients; otherwise it evaluates every ramp
    /// per sample.
    pub(crate) fn render(
        &mut self,
        design: &BandDesign,
        head: &HeadModel,
        pinna_on: bool,
        mono: &[f32],
        out: &mut [[f32; 2]],
        send: Send<'_>,
    ) {
        let n = mono.len();
        let mut fd = [0.0f32; BLOCK];
        fd[..n].copy_from_slice(mono);
        self.direct_filter
            .process(design, &mut fd[..n], &mut self.direct_lanes);
        let any_tap = self.taps.iter().any(|t| t.live);
        let mut fe = [0.0f32; BLOCK];
        if any_tap {
            fe[..n].copy_from_slice(mono);
            self.early_filter
                .process(design, &mut fe[..n], &mut self.early_lanes);
        }
        for i in 0..n {
            self.raw.push(mono[i]);
            self.direct.push(fd[i]);
            self.early.push(fe[i]);
        }
        let steady = self.delay_left == 0 && self.snap_to.is_none() && self.duck.settled();
        // The delay moves linearly across the whole block (constant or ramping).
        let linear = self.snap_to.is_none()
            && self.duck.settled()
            && (self.delay_left == 0 || self.delay_left >= n as u64);
        let d0 = self.delay;
        let mut dly = [0.0f64; BLOCK];
        let mut duck = [1.0f32; BLOCK];
        self.advance_delay(&mut dly[..n], &mut duck[..n]);
        let (first, last) = (dly[0], dly[n - 1]);
        let age = |i: usize| BASE + (n - 1 - i) as f64;
        let mut vout = [[0.0f32; 2]; BLOCK];
        let mut tmp = [0.0f32; BLOCK];

        match send {
            Send::Shared(bus) => {
                if steady && self.send.settled() {
                    let g = self.send.value() * duck[0];
                    if g != 0.0 {
                        self.raw.read_hermite_block(d0 + BASE, &mut tmp[..n]);
                        for (b, s) in bus.iter_mut().zip(&tmp[..n]) {
                            *b += s * g;
                        }
                    }
                } else if linear {
                    self.raw
                        .read_sweep(first + BASE, last + BASE, true, &mut tmp[..n]);
                    for (b, s) in bus.iter_mut().zip(&tmp[..n]) {
                        *b += s * self.send.next();
                    }
                } else {
                    for i in 0..n {
                        let g = self.send.next() * duck[i];
                        if g != 0.0 {
                            bus[i] += self.raw.read_hermite(dly[i] + age(i)) * g;
                        }
                    }
                }
            }
            Send::Hero(bus) => {
                for i in 0..n {
                    bus[i] += mono[i] * self.send.next();
                }
            }
        }

        let a1 = head.a1();
        match self.renderer {
            SpatialRenderer::StereoPan => {
                if steady && self.pan[0].settled() && self.pan[1].settled() {
                    let (l, r) = (self.pan[0].value(), self.pan[1].value());
                    self.direct.read_hermite_block(d0 + BASE, &mut tmp[..n]);
                    for (o, s) in vout[..n].iter_mut().zip(&tmp[..n]) {
                        o[0] += s * l;
                        o[1] += s * r;
                    }
                } else {
                    if linear {
                        self.direct
                            .read_sweep(first + BASE, last + BASE, true, &mut tmp[..n]);
                    } else {
                        for (i, t) in tmp[..n].iter_mut().enumerate() {
                            *t = self.direct.read_hermite(dly[i] + age(i));
                        }
                    }
                    for (o, s) in vout[..n].iter_mut().zip(&tmp[..n]) {
                        o[0] += s * self.pan[0].next();
                        o[1] += s * self.pan[1].next();
                    }
                }
            }
            SpatialRenderer::Binaural => {
                // Pinna echo delays move by a small fraction of a sample per
                // block, so each block uses the value reached at its middle.
                let half = n / 2;
                let delays: [f32; PINNA_ECHOES] = std::array::from_fn(|k| {
                    let mid = self.pinna_delays[k].advance(half);
                    self.pinna_delays[k].advance(n - half);
                    mid
                });
                for e in 0..2 {
                    let ear = &mut self.ears[e];
                    let pinna = &mut self.pinna[e];
                    if steady && ear.itd.settled() && ear.shadow.settled() {
                        let d = d0 + f64::from(ear.itd.value()) + BASE;
                        self.direct.read_hermite_block(d, &mut tmp[..n]);
                        ear.shadow.run(a1, &mut tmp[..n], 1.0);
                    } else {
                        let itd_linear = ear.itd.linear_over(n);
                        let mut itd = [0.0f32; BLOCK];
                        ear.itd.fill(&mut itd[..n]);
                        if linear && itd_linear {
                            let a = first + f64::from(itd[0]) + BASE;
                            let b = last + f64::from(itd[n - 1]) + BASE;
                            self.direct.read_sweep(a, b, true, &mut tmp[..n]);
                        } else {
                            for (i, t) in tmp[..n].iter_mut().enumerate() {
                                *t = self
                                    .direct
                                    .read_hermite(dly[i] + f64::from(itd[i]) + age(i));
                            }
                        }
                        ear.shadow.run_ramped(a1, &mut tmp[..n]);
                    }
                    if pinna_on {
                        pinna.run(&mut tmp[..n], &delays);
                    }
                    for (o, s) in vout[..n].iter_mut().zip(&tmp[..n]) {
                        o[e] += s * EAR_GAIN;
                    }
                    ear.shadow.flush();
                    pinna.flush();
                }
            }
        }

        let renderer = self.renderer;
        // Per-sample read position shared by every tap (delay plus block age).
        let mut base = [0.0f64; BLOCK];
        for (i, b) in base[..n].iter_mut().enumerate() {
            *b = dly[i] + age(i) - BASE;
        }
        let mut gains = [0.0f32; BLOCK];
        let mut pos = [0.0f64; BLOCK];
        let mut itd = [0.0f32; BLOCK];
        for tap in self.taps.iter_mut().filter(|t| t.live) {
            let gliding = tap.delay != tap.target_delay;
            let fixed = steady && !gliding && tap.gain.settled();
            if !fixed {
                for (i, p) in pos[..n].iter_mut().enumerate() {
                    if gliding {
                        tap.delay += (tap.target_delay - tap.delay).clamp(-TAP_SLEW, TAP_SLEW);
                    }
                    *p = base[i] + tap.delay;
                }
            }
            let gain_settled = tap.gain.settled();
            let g = tap.gain.value();
            if !gain_settled {
                tap.gain.fill(&mut gains[..n]);
            }
            match renderer {
                SpatialRenderer::StereoPan => {
                    let pan_settled = tap.pan[0].settled() && tap.pan[1].settled();
                    if fixed {
                        self.early.read_linear_block(d0 + tap.delay, &mut tmp[..n]);
                    } else if linear && !gliding {
                        let (a, b) = (first + tap.delay, last + tap.delay);
                        self.early.read_sweep(a, b, false, &mut tmp[..n]);
                    } else {
                        for i in 0..n {
                            tmp[i] = self.early.read_linear(pos[i]);
                        }
                    }
                    if gain_settled && pan_settled {
                        let (l, r) = (tap.pan[0].value() * g, tap.pan[1].value() * g);
                        for (o, s) in vout[..n].iter_mut().zip(&tmp[..n]) {
                            o[0] += s * l;
                            o[1] += s * r;
                        }
                    } else {
                        if gain_settled {
                            gains[..n].fill(g);
                        }
                        for i in 0..n {
                            let s = tmp[i] * gains[i];
                            vout[i][0] += s * tap.pan[0].next();
                            vout[i][1] += s * tap.pan[1].next();
                        }
                    }
                }
                SpatialRenderer::Binaural => {
                    for (e, ear) in tap.ears.iter_mut().enumerate() {
                        let ear_fixed = ear.itd.settled();
                        if fixed && ear_fixed {
                            let d = d0 + tap.delay + f64::from(ear.itd.value());
                            self.early.read_linear_block(d, &mut tmp[..n]);
                        } else if ear_fixed {
                            let offset = f64::from(ear.itd.value());
                            if linear && !gliding {
                                let (a, b) =
                                    (first + tap.delay + offset, last + tap.delay + offset);
                                self.early.read_sweep(a, b, false, &mut tmp[..n]);
                            } else {
                                for i in 0..n {
                                    tmp[i] = self.early.read_linear(pos[i] + offset);
                                }
                            }
                        } else {
                            if fixed {
                                for (i, p) in pos[..n].iter_mut().enumerate() {
                                    *p = base[i] + tap.delay;
                                }
                            }
                            ear.itd.fill(&mut itd[..n]);
                            for i in 0..n {
                                tmp[i] = self.early.read_linear(pos[i] + f64::from(itd[i]));
                            }
                        }
                        if ear.shadow.settled() {
                            let scale = if gain_settled { g * EAR_GAIN } else { EAR_GAIN };
                            ear.shadow.run(a1, &mut tmp[..n], scale);
                        } else {
                            ear.shadow.run_ramped(a1, &mut tmp[..n]);
                            if gain_settled {
                                tmp[..n].iter_mut().for_each(|t| *t *= g * EAR_GAIN);
                            } else {
                                tmp[..n].iter_mut().for_each(|t| *t *= EAR_GAIN);
                            }
                        }
                        if gain_settled {
                            for (o, s) in vout[..n].iter_mut().zip(&tmp[..n]) {
                                o[e] += s;
                            }
                        } else {
                            for i in 0..n {
                                vout[i][e] += tmp[i] * gains[i];
                            }
                        }
                    }
                }
            }
            for ear in &mut tap.ears {
                ear.shadow.flush();
            }
            if tap.gain.settled() && tap.gain.value() == 0.0 && tap.amp == 0.0 {
                tap.live = false;
            }
        }

        for i in 0..n {
            out[i][0] += vout[i][0] * duck[i];
            out[i][1] += vout[i][1] * duck[i];
        }
        if let Some(t) = &mut self.tail {
            *t = t.saturating_sub(n as u64);
        }
    }
}

fn set_pan(pan: &mut [Ramp; 2], dir: [f64; 3], frames: usize) {
    let p = dir[0].clamp(-1.0, 1.0);
    pan[0].set(((1.0 - p) / 2.0).sqrt() as f32, frames);
    pan[1].set(((1.0 + p) / 2.0).sqrt() as f32, frames);
}

fn set_ears(ears: &mut [Ear; 2], head: &HeadModel, dir: [f64; 3], frames: usize) {
    for (e, ear) in ears.iter_mut().enumerate() {
        let theta = hrtf::incidence(dir, e);
        ear.itd.set(head.ear_delay(theta), frames);
        ear.shadow.set(head.shadow(theta), frames);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retarded_delay_gives_source_doppler() {
        let c = 343.0;
        // Source 30 m away approaching at 20 m/s along the line of sight.
        let d0 = retarded_delay([30.0, 0.0, 0.0], [20.0, 0.0, 0.0], c);
        let d1 = retarded_delay([30.0 - 20.0 * 0.01, 0.0, 0.0], [20.0, 0.0, 0.0], c);
        let ratio = 1.0 - (d1 - d0) / 0.01;
        assert!((ratio - c / (c - 20.0)).abs() < 1e-9, "{ratio}");
        assert!((retarded_delay([0.0, 3.43, 0.0], [0.0; 3], c) - 0.01).abs() < 1e-12);
    }

    #[test]
    fn distance_law_matches_legacy_rules() {
        let s = SpatialOptions {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(distance_volume(&s, 0.5), 1.0);
        assert!((distance_volume(&s, 4.0) - 0.25).abs() < 1e-12);
        assert_eq!(distance_volume(&s, 100.0), 0.0);
    }
}
