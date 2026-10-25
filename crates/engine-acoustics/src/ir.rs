//! Impulse-response synthesis from an energy histogram (section 5.4.6).
//!
//! Per band and ear: deterministic white noise (PCG, independent per ear and band),
//! band-limited with Linkwitz-Riley 4th-order crossovers at `BAND_EDGES_HZ`
//! (cascaded Butterworth biquads, RBJ Audio EQ Cookbook coefficients), normalised to
//! unit RMS and shaped by the square root of the histogram energy per sample,
//! linearly interpolated between bin centres. The IR therefore carries the
//! histogram's energy per band and bin (relative to the direct sound at 1 m) and
//! excludes the direct sound.

use crate::rng::{Pcg, job_seed};
use crate::{AcousticsError, BANDS, EnergyHistogram, Result, fit::validate_histogram};
use engine_audio::acoustic::{BAND_EDGES_HZ, MAX_IMPULSE_SECONDS, StereoImpulseResponse};

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}
impl Biquad {
    /// Butterworth (Q = 1/sqrt 2) low- or high-pass.
    fn new(sample_rate: f64, freq: f64, high: bool) -> Self {
        let f = freq.min(0.45 * sample_rate);
        let w = 2.0 * std::f64::consts::PI * f / sample_rate;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let b = if high {
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
        } else {
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
        };
        Self {
            b: b.map(|v| v / a0),
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// LR4 filter chain for band `band`.
fn band_chain(sample_rate: f64, band: usize) -> Vec<Biquad> {
    let mut chain = Vec::new();
    if band > 0 {
        let f = f64::from(BAND_EDGES_HZ[band - 1]);
        chain.extend([Biquad::new(sample_rate, f, true); 2]);
    }
    if band + 1 < BANDS {
        let f = f64::from(BAND_EDGES_HZ[band]);
        chain.extend([Biquad::new(sample_rate, f, false); 2]);
    }
    chain
}

/// Synthesizes a stereo IR from `h` at `sample_rate` (8-192 kHz), at most
/// `MAX_IMPULSE_SECONDS` long. Deterministic for a given `seed`.
pub fn synthesize_ir(
    h: &EnergyHistogram,
    sample_rate: u32,
    seed: u64,
) -> Result<StereoImpulseResponse> {
    validate_histogram(h)?;
    if !(8_000..=192_000).contains(&sample_rate) {
        return Err(AcousticsError::new(
            "acoustics.request.invalid",
            "IR sample rate must be 8-192 kHz",
        ));
    }
    let fs = f64::from(sample_rate);
    let seconds = (h.bins.len() as f64 * f64::from(h.bin_s)).min(f64::from(MAX_IMPULSE_SECONDS));
    let samples = (seconds * fs).floor() as usize;
    let per_bin = f64::from(h.bin_s) * fs;
    let mut left = vec![0.0f32; samples];
    let mut right = vec![0.0f32; samples];
    let mut noise = vec![0.0f64; samples];
    for band in 0..BANDS {
        // sqrt energy per bin, interpolated at bin centres.
        let root: Vec<f64> = h.bins.iter().map(|b| f64::from(b[band]).sqrt()).collect();
        if root.iter().all(|r| *r == 0.0) {
            continue;
        }
        for (ear, out) in [&mut left, &mut right].into_iter().enumerate() {
            let mut rng = Pcg::for_ray(job_seed(seed, (band * 2 + ear) as u32), 0);
            let mut chain = band_chain(fs, band);
            let mut power = 0.0f64;
            for v in noise.iter_mut() {
                let mut x = f64::from(rng.next_f32()) * 2.0 - 1.0;
                for f in &mut chain {
                    x = f.process(x);
                }
                *v = x;
                power += x * x;
            }
            let rms = (power / samples.max(1) as f64).sqrt();
            if rms <= 0.0 {
                continue;
            }
            let norm = 1.0 / (rms * per_bin.sqrt());
            for (n, (o, x)) in out.iter_mut().zip(&noise).enumerate() {
                let pos = (n as f64 + 0.5) / per_bin - 0.5;
                let i = pos.floor().max(0.0) as usize;
                let frac = (pos - i as f64).clamp(0.0, 1.0);
                let r0 = root.get(i).copied().unwrap_or(0.0);
                let r1 = root.get(i + 1).copied().unwrap_or(r0);
                let env = if pos < 0.0 { r0 } else { r0 + (r1 - r0) * frac };
                *o += (x * env * norm) as f32;
            }
        }
    }
    Ok(StereoImpulseResponse {
        sample_rate,
        left,
        right,
    })
}
