//! Measurement helpers for the acoustic acceptance suites (section 8 of
//! `docs/spatial-audio-design.md`). Analysis runs in f64 and only uses the
//! public engine API.
#![allow(dead_code)]

use engine_audio::acoustic::{
    AcousticPublisher, MixerEnvironment, ReverbRenderer, SpatialRenderer, VoiceAcoustics,
    VoicePropagation,
};
use engine_audio::{
    Attenuation, AudioConfig, AudioControl, AudioEngine, Clip, PlayOptions, SpatialOptions,
    VoiceHandle,
};
use std::f64::consts::PI;
use std::sync::Arc;

pub const RATE: u32 = 48_000;
pub const FS: f64 = RATE as f64;

/// Deterministic uniform noise in [-amp, amp].
pub fn noise(len: usize, seed: u32, amp: f32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            ((s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

pub fn clip(samples: Vec<f32>) -> Arc<Clip> {
    Clip::new(RATE, 1, samples).unwrap()
}

pub fn engine() -> AudioEngine {
    AudioEngine::offline(AudioConfig {
        sample_rate: RATE,
        max_voices: 64,
    })
    .unwrap()
}

pub fn environment(renderer: SpatialRenderer, reverb: ReverbRenderer) -> MixerEnvironment {
    MixerEnvironment {
        renderer,
        reverb,
        ..Default::default()
    }
}

/// Looped acoustic voice at `position` with no distance law, no air and no
/// propagation delay (individual tests switch those on).
pub fn acoustic_options(position: [f32; 3]) -> PlayOptions {
    PlayOptions {
        looped: true,
        spatial: SpatialOptions {
            enabled: true,
            position,
            attenuation: Attenuation::None,
            ..Default::default()
        },
        acoustics: VoiceAcoustics {
            enabled: true,
            propagation_delay: false,
            air_absorption: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Edits one published propagation entry.
pub type Edit<'a> = &'a dyn Fn(&mut VoicePropagation);

/// Publishes one propagation entry per `(voice, edit)` pair.
pub fn publish_voices(
    publisher: &mut AcousticPublisher,
    voices: &[(VoiceHandle, Edit<'_>)],
    reverb: Option<engine_audio::acoustic::ReverbParams>,
) {
    let mut sorted: Vec<_> = voices.iter().collect();
    sorted.sort_by_key(|(v, _)| *v);
    publisher
        .publish(|frame| {
            for (voice, edit) in &sorted {
                let mut p = VoicePropagation::new(*voice);
                edit(&mut p);
                frame.voices.push(p);
            }
            if let Some(r) = reverb {
                frame.reverb = r;
            }
            Ok(())
        })
        .unwrap();
}

/// Mixes `frames` frames in `block`-frame calls and returns (left, right).
pub fn render(engine: &mut AudioEngine, frames: usize, block: usize) -> (Vec<f32>, Vec<f32>) {
    let mut out = vec![0.0f32; frames * 2];
    for chunk in out.chunks_mut(block * 2) {
        engine.mix(chunk).unwrap();
    }
    split(&out)
}

pub fn split(out: &[f32]) -> (Vec<f32>, Vec<f32>) {
    out.chunks(2).map(|f| (f[0], f[1])).unzip()
}

pub fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// Windowed single-frequency DFT of `x` (Hann window).
pub fn dft(x: &[f64], freq: f64) -> (f64, f64) {
    let n = x.len();
    let w = 2.0 * PI * freq / FS;
    x.iter().enumerate().fold((0.0, 0.0), |(re, im), (i, v)| {
        let win = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        let (s, c) = (w * i as f64).sin_cos();
        (re + v * win * c, im - v * win * s)
    })
}

/// H1 transfer-function magnitude estimate from input `x` to output `y` at
/// `freq`, averaged over Hann segments of `seg` samples with 50 % overlap.
pub fn transfer(x: &[f32], y: &[f32], freq: f64, seg: usize) -> f64 {
    let mut sxy = (0.0, 0.0);
    let mut sxx = 0.0;
    let mut start = 0;
    while start + seg <= x.len().min(y.len()) {
        let xs: Vec<f64> = x[start..start + seg]
            .iter()
            .map(|v| f64::from(*v))
            .collect();
        let ys: Vec<f64> = y[start..start + seg]
            .iter()
            .map(|v| f64::from(*v))
            .collect();
        let (xr, xi) = dft(&xs, freq);
        let (yr, yi) = dft(&ys, freq);
        sxy.0 += yr * xr + yi * xi;
        sxy.1 += yi * xr - yr * xi;
        sxx += xr * xr + xi * xi;
        start += seg / 2;
    }
    sxy.0.hypot(sxy.1) / sxx
}

/// In-place radix-2 complex FFT (f64, analysis only).
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    assert!(n.is_power_of_two());
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut size = 2;
    while size <= n {
        let half = size / 2;
        let step = sign * 2.0 * PI / size as f64;
        for start in (0..n).step_by(size) {
            for k in 0..half {
                let (s, c) = (step * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + half);
                let br = re[b] * c - im[b] * s;
                let bi = re[b] * s + im[b] * c;
                re[b] = re[a] - br;
                im[b] = im[a] - bi;
                re[a] += br;
                im[a] += bi;
            }
        }
        size *= 2;
    }
    if inverse {
        for i in 0..n {
            re[i] /= n as f64;
            im[i] /= n as f64;
        }
    }
}

/// Zero-phase octave-band filter around `centre` (raised-cosine skirts of a
/// sixth of an octave beyond the nominal edges).
pub fn octave_band(x: &[f32], centre: f64) -> Vec<f64> {
    let n = x.len().next_power_of_two() * 2;
    let mut re: Vec<f64> = x.iter().map(|v| f64::from(*v)).collect();
    re.resize(n, 0.0);
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im, false);
    let lo = centre / 2f64.sqrt();
    let hi = centre * 2f64.sqrt();
    let skirt = 2f64.powf(1.0 / 6.0);
    for k in 0..n {
        let f = (k.min(n - k)) as f64 * FS / n as f64;
        let w = if f <= 0.0 {
            0.0
        } else if f >= lo && f <= hi {
            1.0
        } else if f < lo && f > lo / skirt {
            let t = (f / (lo / skirt)).log2() / skirt.log2();
            0.5 - 0.5 * (PI * t).cos()
        } else if f > hi && f < hi * skirt {
            let t = ((hi * skirt) / f).log2() / skirt.log2();
            0.5 - 0.5 * (PI * t).cos()
        } else {
            0.0
        };
        re[k] *= w;
        im[k] *= w;
    }
    fft(&mut re, &mut im, true);
    re.truncate(x.len());
    re
}

/// Schroeder backward integration (dB, normalised to 0 at t = 0) and the
/// T30 estimate from a least-squares fit between -5 and -35 dB.
pub fn t30(h: &[f64]) -> f64 {
    let mut edc = vec![0.0; h.len()];
    let mut acc = 0.0;
    for i in (0..h.len()).rev() {
        acc += h[i] * h[i];
        edc[i] = acc;
    }
    let total = edc[0];
    let curve: Vec<f64> = edc.iter().map(|e| 10.0 * (e / total).log10()).collect();
    let start = curve.iter().position(|d| *d <= -5.0).unwrap();
    let end = curve.iter().position(|d| *d <= -35.0).unwrap();
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    let count = (end - start) as f64;
    for (i, d) in curve.iter().enumerate().take(end).skip(start) {
        let t = i as f64 / FS;
        sx += t;
        sy += d;
        sxx += t * t;
        sxy += t * d;
    }
    let slope = (count * sxy - sx * sy) / (count * sxx - sx * sx);
    -60.0 / slope
}

/// Lag (samples, sub-sample by parabolic interpolation) maximising
/// `sum a[n] b[n - lag]`; positive when `a` lags `b`.
pub fn lag(a: &[f32], b: &[f32], max_lag: i64) -> f64 {
    let corr = |k: i64| -> f64 {
        let mut s = 0.0;
        for n in (max_lag as usize)..(a.len() - max_lag as usize) {
            s += f64::from(a[n]) * f64::from(b[(n as i64 - k) as usize]);
        }
        s
    };
    let values: Vec<f64> = (-max_lag..=max_lag).map(corr).collect();
    let (i, _) = values
        .iter()
        .enumerate()
        .max_by(|x, y| x.1.total_cmp(y.1))
        .unwrap();
    let i = i.clamp(1, values.len() - 2);
    let (y0, y1, y2) = (values[i - 1], values[i], values[i + 1]);
    let offset = 0.5 * (y0 - y2) / (y0 - 2.0 * y1 + y2);
    (i as f64 - max_lag as f64) + offset
}

/// Energy of `x` between `lo` and `hi` Hz (FFT of the whole slice).
pub fn band_energy(x: &[f32], lo: f64, hi: f64) -> f64 {
    let n = x.len().next_power_of_two();
    let mut re: Vec<f64> = x
        .iter()
        .enumerate()
        .map(|(i, v)| f64::from(*v) * (0.5 - 0.5 * (2.0 * PI * i as f64 / x.len() as f64).cos()))
        .collect();
    re.resize(n, 0.0);
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im, false);
    (0..n / 2)
        .filter(|k| {
            let f = *k as f64 * FS / n as f64;
            f >= lo && f <= hi
        })
        .map(|k| re[k] * re[k] + im[k] * im[k])
        .sum()
}

/// Frequency from interpolated positive-going zero crossings in `x`.
pub fn zero_crossing_frequency(x: &[f32]) -> f64 {
    let mut crossings = Vec::new();
    for i in 1..x.len() {
        let (a, b) = (f64::from(x[i - 1]), f64::from(x[i]));
        if a < 0.0 && b >= 0.0 {
            crossings.push(i as f64 - 1.0 + a / (a - b));
        }
    }
    let periods = (crossings.len() - 1) as f64;
    periods * FS / (crossings[crossings.len() - 1] - crossings[0])
}

/// Shared voice setup: engine with an environment and a publisher.
pub fn setup(env: MixerEnvironment) -> (AudioEngine, AudioControl, AcousticPublisher) {
    let engine = engine();
    let control = engine.control();
    control.set_environment(env).unwrap();
    let publisher = control.acoustic_publisher().unwrap();
    (engine, control, publisher)
}
