use super::{
    CompiledGraph, MAX_DELAY_FRAMES, MAX_PARAMETERS, MAX_RENDER_FRAMES, MAX_SIGNAL,
    compile::{Op, Wave},
};
use crate::{AudioError, Result};
use std::sync::Arc;
#[derive(Clone, Copy, Debug)]
pub struct ParameterUpdate<'a> {
    pub name: &'a str,
    pub value: f32,
    pub ramp_frames: u32,
}
#[derive(Clone, Copy)]
pub(super) struct Change {
    pub value: f32,
    pub frames: u32,
}
pub(super) fn updates(
    program: &CompiledGraph,
    values: &[ParameterUpdate<'_>],
) -> Result<[Option<Change>; MAX_PARAMETERS]> {
    if values.len() > MAX_PARAMETERS {
        return Err(error(
            "audio.graph.updates",
            "At most 64 updates per atomic batch",
        ));
    }
    let mut pending = [None; MAX_PARAMETERS];
    for value in values {
        if value.name.len() > 64
            || !value.value.is_finite()
            || value.value.abs() > MAX_SIGNAL
            || value.ramp_frames > 65_536
        {
            return Err(error(
                "audio.graph.parameter",
                "Invalid parameter value/name/ramp",
            ));
        }
        let index = program
            .parameters
            .iter()
            .position(|(name, _)| name == value.name)
            .ok_or_else(|| error("audio.graph.parameter", "Unknown parameter"))?;
        if pending[index].is_some() {
            return Err(error(
                "audio.graph.parameter",
                "Duplicate parameter in atomic update batch",
            ));
        }
        pending[index] = Some(Change {
            value: value.value,
            frames: value.ramp_frames,
        });
    }
    Ok(pending)
}
pub(super) fn error(code: &'static str, message: &str) -> AudioError {
    AudioError::new(code, message)
}
#[derive(Clone, Copy, Default)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}
impl Ramp {
    fn set(&mut self, change: Change) {
        self.target = change.value;
        self.left = change.frames;
        if self.left == 0 {
            self.value = self.target;
            self.step = 0.;
        } else {
            self.step = (self.target - self.value) / self.left as f32;
        }
    }
    fn next(&mut self) -> f32 {
        if self.left > 0 {
            self.value += self.step;
            self.left -= 1;
            if self.left == 0 {
                self.value = self.target;
            }
        }
        self.value
    }
}
#[derive(Clone, Copy)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}
struct Envelope {
    stage: Stage,
    gate: bool,
    level: f32,
    target: f32,
    step: f32,
    left: u32,
}
impl Default for Envelope {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            gate: false,
            level: 0.,
            target: 0.,
            step: 0.,
            left: 0,
        }
    }
}
impl Envelope {
    fn begin(&mut self, stage: Stage, target: f32, frames: u32) {
        self.stage = stage;
        self.target = target;
        self.left = frames;
        if frames == 0 {
            self.level = target;
            self.step = 0.;
        } else {
            self.step = (target - self.level) / frames as f32;
        }
    }
    fn sample(&mut self, gate: bool, attack: u32, decay: u32, sustain: f32, release: u32) -> f32 {
        if gate && !self.gate {
            self.begin(Stage::Attack, 1., attack);
        } else if !gate && self.gate {
            self.begin(Stage::Release, 0., release);
        }
        self.gate = gate;
        for _ in 0..4 {
            if self.left > 0 {
                self.level += self.step;
                self.left -= 1;
                if self.left == 0 {
                    self.level = self.target;
                }
                return self.level;
            }
            match self.stage {
                Stage::Attack => self.begin(Stage::Decay, sustain, decay),
                Stage::Decay => {
                    self.stage = Stage::Sustain;
                    self.level = sustain;
                }
                Stage::Release => {
                    self.stage = Stage::Idle;
                    self.level = 0.;
                }
                Stage::Idle | Stage::Sustain => return self.level,
            }
        }
        self.level
    }
}
enum State {
    None,
    Phase(f64),
    Noise(u32),
    Filter(f32),
    Delay { buffer: Box<[f32]>, at: usize },
    Envelope(Envelope),
}
/// Independent phase/filter/delay/envelope/parameter state for one voice.
pub struct GraphInstance {
    pub(super) program: Arc<CompiledGraph>,
    rate: u32,
    states: Vec<State>,
    values: [f32; super::MAX_NODES],
    parameters: [Ramp; MAX_PARAMETERS],
    frames: u64,
    failed: bool,
}
impl GraphInstance {
    pub fn new(program: Arc<CompiledGraph>, sample_rate: u32) -> Result<Self> {
        if !crate::valid_rate(sample_rate) {
            return Err(error("audio.graph.rate", "Expected 8-192 kHz sample rate"));
        }
        let total: usize = program
            .ops
            .iter()
            .map(|op| match op {
                Op::Delay { seconds, .. } => (f64::from(*seconds) * f64::from(sample_rate))
                    .round()
                    .max(1.) as usize,
                _ => 0,
            })
            .sum();
        if total > MAX_DELAY_FRAMES {
            return Err(error(
                "audio.graph.delay_limit",
                "Aggregate delay storage exceeds 1048576 mono frames",
            ));
        }
        let states = program
            .ops
            .iter()
            .map(|op| match op {
                Op::Oscillator { .. } => State::Phase(0.),
                Op::Noise(seed) => State::Noise(*seed),
                Op::Lowpass { .. } => State::Filter(0.),
                Op::Adsr { .. } => State::Envelope(Envelope::default()),
                Op::Delay { seconds, .. } => State::Delay {
                    buffer: vec![
                        0.;
                        (f64::from(*seconds) * f64::from(sample_rate))
                            .round()
                            .max(1.) as usize
                    ]
                    .into_boxed_slice(),
                    at: 0,
                },
                _ => State::None,
            })
            .collect();
        let mut instance = Self {
            program,
            rate: sample_rate,
            states,
            values: [0.; super::MAX_NODES],
            parameters: [Ramp::default(); MAX_PARAMETERS],
            frames: 0,
            failed: false,
        };
        instance.reset();
        Ok(instance)
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    /// Generated frames, distinct from the stream mixer's consumed source clock.
    pub fn generated_frames(&self) -> u64 {
        self.frames
    }
    pub fn set_parameters(&mut self, values: &[ParameterUpdate<'_>]) -> Result<()> {
        let pending = updates(&self.program, values)?;
        self.apply(&pending);
        Ok(())
    }
    pub(super) fn apply(&mut self, changes: &[Option<Change>; MAX_PARAMETERS]) {
        for (i, change) in changes.iter().enumerate() {
            if let Some(change) = change {
                self.parameters[i].set(*change);
            }
        }
    }
    /// Restores defaults and all DSP seeds/state without reallocating delay storage.
    pub fn reset(&mut self) {
        for (i, state) in self.states.iter_mut().enumerate() {
            match state {
                State::Phase(v) => *v = 0.,
                State::Noise(v) => {
                    if let Op::Noise(seed) = self.program.ops[i] {
                        *v = seed;
                    }
                }
                State::Filter(v) => *v = 0.,
                State::Delay { buffer, at } => {
                    buffer.fill(0.);
                    *at = 0;
                }
                State::Envelope(env) => *env = Envelope::default(),
                State::None => {}
            }
        }
        self.parameters.fill(Ramp::default());
        for (i, (_, value)) in self.program.parameters.iter().enumerate() {
            self.parameters[i].set(Change {
                value: *value,
                frames: 0,
            });
        }
        self.values.fill(0.);
        self.frames = 0;
        self.failed = false;
    }
    /// Renders at most 4096 stereo frames. Success allocates nothing; errors silence a valid-sized block and require reset.
    pub fn render(&mut self, out: &mut [[f32; 2]]) -> Result<()> {
        if out.len() > MAX_RENDER_FRAMES {
            return Err(error(
                "audio.graph.block_limit",
                "At most 4096 frames per render call",
            ));
        }
        out.fill([0.; 2]);
        if self.failed {
            return Err(error(
                "audio.graph.failed",
                "Reset a failed instance before rendering",
            ));
        }
        let result = self.render_inner(out);
        if result.is_err() {
            out.fill([0.; 2]);
            self.failed = true;
        } else {
            self.frames = self.frames.saturating_add(out.len() as u64);
        }
        result
    }
    fn render_inner(&mut self, out: &mut [[f32; 2]]) -> Result<()> {
        let fs = self.rate as f32;
        for frame in out {
            for parameter in self
                .parameters
                .iter_mut()
                .take(self.program.parameters.len())
            {
                parameter.next();
            }
            for &index in &self.program.order {
                let value = match (&self.program.ops[index], &mut self.states[index]) {
                    (Op::Constant(value), _) => *value,
                    (Op::Parameter(parameter), _) => self.parameters[*parameter].value,
                    (
                        Op::Oscillator {
                            frequency,
                            hz,
                            wave,
                        },
                        State::Phase(phase),
                    ) => {
                        let frequency = frequency.map_or(*hz, |i| self.values[i]);
                        if !(0. ..=0.45 * fs).contains(&frequency) {
                            return Err(error(
                                "audio.graph.frequency",
                                "Oscillator frequency exceeds 0..0.45 sample rate",
                            ));
                        }
                        let value = match wave {
                            Wave::Sine => (std::f64::consts::TAU * *phase).sin() as f32,
                            Wave::Triangle => (1. - 4. * (*phase - 0.5).abs()) as f32,
                            Wave::Saw => (2. * *phase - 1.) as f32,
                            Wave::Square => {
                                if *phase < 0.5 {
                                    1.
                                } else {
                                    -1.
                                }
                            }
                        };
                        *phase = (*phase + f64::from(frequency) / f64::from(self.rate)).fract();
                        value
                    }
                    (Op::Noise(_), State::Noise(seed)) => {
                        *seed ^= *seed << 13;
                        *seed ^= *seed >> 17;
                        *seed ^= *seed << 5;
                        (f64::from(*seed) / f64::from(u32::MAX) * 2. - 1.) as f32
                    }
                    (Op::Mix(a, b), _) => self.values[*a] + self.values[*b],
                    (Op::Gain { input, gain, value }, _) => {
                        let gain = gain.map_or(*value, |i| self.values[i]);
                        if gain.abs() > 16. {
                            return Err(error("audio.graph.gain", "Gain exceeds [-16,16]"));
                        }
                        self.values[*input] * gain
                    }
                    (
                        Op::Adsr {
                            gate,
                            attack,
                            decay,
                            sustain,
                            release,
                        },
                        State::Envelope(env),
                    ) => env.sample(
                        self.values[*gate] > 0.5,
                        (*attack * fs).round() as u32,
                        (*decay * fs).round() as u32,
                        *sustain,
                        (*release * fs).round() as u32,
                    ),
                    (Op::Lowpass { input, cutoff, hz }, State::Filter(history)) => {
                        let cutoff = cutoff.map_or(*hz, |i| self.values[i]);
                        if !(1. ..=0.45 * fs).contains(&cutoff) {
                            return Err(error(
                                "audio.graph.cutoff",
                                "Filter cutoff exceeds 1..0.45 sample rate",
                            ));
                        }
                        let a = 1. - (-std::f32::consts::TAU * cutoff / fs).exp();
                        *history += a * (self.values[*input] - *history);
                        *history
                    }
                    (
                        Op::Delay {
                            input, feedback, ..
                        },
                        State::Delay { buffer, at },
                    ) => {
                        let old = buffer[*at];
                        let stored = self.values[*input] + *feedback * old;
                        check_signal(stored)?;
                        buffer[*at] = stored;
                        *at = (*at + 1) % buffer.len();
                        old
                    }
                    (Op::Output(_, _), _) => 0.,
                    _ => return Err(error("audio.graph.state", "Compiled node state mismatch")),
                };
                check_signal(value)?;
                self.values[index] = value;
            }
            if let Op::Output(left, right) = self.program.ops[self.program.output] {
                *frame = [
                    self.values[left].clamp(-1., 1.),
                    self.values[right].clamp(-1., 1.),
                ];
            }
        }
        Ok(())
    }
}
fn check_signal(value: f32) -> Result<()> {
    if value.is_finite() && value.abs() <= MAX_SIGNAL {
        Ok(())
    } else {
        Err(error(
            "audio.graph.math",
            "Nonfinite or excessive intermediate signal",
        ))
    }
}
