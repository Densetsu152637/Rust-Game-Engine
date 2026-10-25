//! Uniform partitioned overlap-save convolution (block `CONVOLUTION_BLOCK`,
//! FFT size twice that). Kernels are prepared off the audio thread; the
//! convolver owns the frequency-domain input history and swaps kernels with a
//! one-block linear cross-fade.

use super::fft::{FftScratch, RealFft};
use crate::acoustic::{
    CONVOLUTION_BLOCK, ConvolutionKernel, MAX_IMPULSE_SECONDS, StereoImpulseResponse,
};
use crate::{AudioError, Result, valid_rate};

const B: usize = CONVOLUTION_BLOCK;
pub(crate) const BINS: usize = B + 1;

/// Partitions needed for the longest accepted impulse response at `rate`.
pub(crate) fn max_partitions(rate: u32) -> usize {
    (MAX_IMPULSE_SECONDS as usize * rate as usize).div_ceil(B)
}

fn invalid(message: &str) -> AudioError {
    AudioError::new("audio.kernel.invalid", message)
}

/// Linear-interpolation resampling (adequate for diffuse reverb tails).
fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return x.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let len = ((x.len() as f64) / ratio).ceil() as usize;
    (0..len)
        .map(|i| {
            let p = i as f64 * ratio;
            let k = p as usize;
            let t = (p - k as f64) as f32;
            let a = x.get(k).copied().unwrap_or(0.0);
            let b = x.get(k + 1).copied().unwrap_or(0.0);
            a + (b - a) * t
        })
        .collect()
}

pub(crate) fn prepare(ir: &StereoImpulseResponse, output_rate: u32) -> Result<ConvolutionKernel> {
    if !valid_rate(ir.sample_rate) || !valid_rate(output_rate) {
        return Err(invalid("Impulse and output rates must be 8-192 kHz"));
    }
    let longest = ir.left.len().max(ir.right.len());
    if longest == 0 || longest > MAX_IMPULSE_SECONDS as usize * ir.sample_rate as usize {
        return Err(invalid(
            "Impulse response must have 1 sample to 4 seconds per channel",
        ));
    }
    if !ir.left.iter().chain(&ir.right).all(|v| v.is_finite()) {
        return Err(invalid("Impulse response samples must be finite"));
    }
    let mut left = resample(&ir.left, ir.sample_rate, output_rate);
    let mut right = resample(&ir.right, ir.sample_rate, output_rate);
    let frames = left
        .len()
        .max(right.len())
        .min(MAX_IMPULSE_SECONDS as usize * output_rate as usize);
    left.resize(frames, 0.0);
    right.resize(frames, 0.0);
    let partitions = frames.div_ceil(B);
    let fft = RealFft::new(2 * B);
    let mut scratch = fft.scratch();
    let mut spectra: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; partitions * BINS]);
    let mut time = vec![0.0f32; 2 * B];
    for (channel, samples) in [&left, &right].into_iter().enumerate() {
        for p in 0..partitions {
            time.fill(0.0);
            let chunk = &samples[p * B..((p + 1) * B).min(frames)];
            time[..chunk.len()].copy_from_slice(chunk);
            let (re, im) = spectra.split_at_mut(2 * channel + 1);
            fft.forward(
                &time,
                &mut re[2 * channel][p * BINS..(p + 1) * BINS],
                &mut im[0][p * BINS..(p + 1) * BINS],
                &mut scratch,
            );
        }
    }
    let fingerprint = left
        .iter()
        .chain(&right)
        .flat_map(|s| s.to_bits().to_le_bytes())
        .chain(output_rate.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    Ok(ConvolutionKernel {
        sample_rate: output_rate,
        partitions,
        spectra,
        fingerprint,
    })
}

/// Runtime state for one stereo-output convolution.
pub(crate) struct Convolver {
    fft: RealFft,
    scratch: FftScratch,
    capacity: usize,
    fdl_re: Vec<f32>,
    fdl_im: Vec<f32>,
    fdl_pos: usize,
    input: Vec<f32>,
    in_pos: usize,
    out: Vec<[f32; 2]>,
    acc: [Vec<f32>; 4],
    time: [Vec<f32>; 2],
    old: Vec<[f32; 2]>,
    /// Input history is all zero (nothing to flush).
    silent: bool,
}

impl Convolver {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let fft = RealFft::new(2 * B);
        let capacity = max_partitions(sample_rate);
        Self {
            scratch: fft.scratch(),
            fft,
            capacity,
            fdl_re: vec![0.0; capacity * BINS],
            fdl_im: vec![0.0; capacity * BINS],
            fdl_pos: 0,
            input: vec![0.0; 2 * B],
            in_pos: 0,
            out: vec![[0.0; 2]; B],
            acc: std::array::from_fn(|_| vec![0.0; BINS]),
            time: std::array::from_fn(|_| vec![0.0; 2 * B]),
            old: vec![[0.0; 2]; B],
            silent: true,
        }
    }

    /// Whether `kernel` can run here (rate and length).
    pub(crate) fn accepts(&self, kernel: &ConvolutionKernel, rate: u32) -> bool {
        kernel.sample_rate == rate && kernel.partitions <= self.capacity
    }

    /// Clears all history (used when a slot changes owner).
    pub(crate) fn reset(&mut self) {
        if !self.silent {
            self.fdl_re.fill(0.0);
            self.fdl_im.fill(0.0);
            self.input.fill(0.0);
            self.out.fill([0.0; 2]);
            self.silent = true;
        }
        self.in_pos = 0;
    }

    /// Adds the convolved signal to `out`. `xfade` requests a cross-fade from
    /// `prev` to `now` at the next block boundary and is cleared once done.
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        out: &mut [[f32; 2]],
        now: Option<&ConvolutionKernel>,
        prev: Option<&ConvolutionKernel>,
        xfade: &mut bool,
    ) {
        for (x, y) in input.iter().zip(out.iter_mut()) {
            let o = self.out[self.in_pos];
            y[0] += o[0];
            y[1] += o[1];
            self.input[B + self.in_pos] = *x;
            if *x != 0.0 {
                self.silent = false;
            }
            self.in_pos += 1;
            if self.in_pos == B {
                self.block(now, prev, *xfade);
                *xfade = false;
                self.in_pos = 0;
            }
        }
    }

    fn block(
        &mut self,
        now: Option<&ConvolutionKernel>,
        prev: Option<&ConvolutionKernel>,
        xfade: bool,
    ) {
        self.fdl_pos = (self.fdl_pos + 1) % self.capacity;
        let slot = self.fdl_pos * BINS;
        self.fft.forward(
            &self.input,
            &mut self.fdl_re[slot..slot + BINS],
            &mut self.fdl_im[slot..slot + BINS],
            &mut self.scratch,
        );
        self.input.copy_within(B.., 0);
        if xfade {
            self.render(prev);
            self.old.copy_from_slice(&self.out);
        }
        self.render(now);
        if xfade {
            for (i, (fresh, stale)) in self.out.iter_mut().zip(&self.old).enumerate() {
                let t = (i + 1) as f32 / B as f32;
                for c in 0..2 {
                    fresh[c] = stale[c] * (1.0 - t) + fresh[c] * t;
                }
            }
        }
    }

    /// Computes one output block for `kernel` into `self.out`.
    fn render(&mut self, kernel: Option<&ConvolutionKernel>) {
        let Some(kernel) = kernel else {
            self.out.fill([0.0; 2]);
            return;
        };
        self.acc.iter_mut().for_each(|a| a.fill(0.0));
        let [lr, li, rr, ri] = &mut self.acc;
        let [hlr, hli, hrr, hri] = &kernel.spectra;
        for p in 0..kernel.partitions.min(self.capacity) {
            let slot = ((self.fdl_pos + self.capacity - p) % self.capacity) * BINS;
            let xr = &self.fdl_re[slot..slot + BINS];
            let xi = &self.fdl_im[slot..slot + BINS];
            let k = p * BINS;
            let (alr, ali) = (&hlr[k..k + BINS], &hli[k..k + BINS]);
            let (arr, ari) = (&hrr[k..k + BINS], &hri[k..k + BINS]);
            for b in 0..BINS {
                lr[b] += xr[b] * alr[b] - xi[b] * ali[b];
                li[b] += xr[b] * ali[b] + xi[b] * alr[b];
                rr[b] += xr[b] * arr[b] - xi[b] * ari[b];
                ri[b] += xr[b] * ari[b] + xi[b] * arr[b];
            }
        }
        let [tl, tr] = &mut self.time;
        self.fft.inverse(lr, li, tl, &mut self.scratch);
        self.fft.inverse(rr, ri, tr, &mut self.scratch);
        for (i, o) in self.out.iter_mut().enumerate() {
            *o = [tl[B + i], tr[B + i]];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    }

    #[test]
    fn partitioned_matches_direct_convolution() {
        let h = noise(3000, 1);
        let hr: Vec<f32> = h.iter().map(|v| -v * 0.5).collect();
        let x = noise(9000, 2);
        let ir = StereoImpulseResponse {
            sample_rate: 48_000,
            left: h.clone(),
            right: hr.clone(),
        };
        let kernel = prepare(&ir, 48_000).unwrap();
        let mut conv = Convolver::new(48_000);
        let mut out = vec![[0.0f32; 2]; x.len()];
        for (xi, oi) in x.chunks(100).zip(out.chunks_mut(100)) {
            conv.process(xi, oi, Some(&kernel), None, &mut false);
        }
        let mut peak = 0.0f64;
        let mut worst = 0.0f64;
        for n in B..x.len() {
            let m = n - B;
            let direct = |k: &[f32]| -> f64 {
                (0..k.len().min(m + 1))
                    .map(|j| f64::from(k[j]) * f64::from(x[m - j]))
                    .sum()
            };
            let (l, r) = (direct(&h), direct(&hr));
            peak = peak.max(l.abs()).max(r.abs());
            worst = worst
                .max((f64::from(out[n][0]) - l).abs())
                .max((f64::from(out[n][1]) - r).abs());
        }
        assert!(worst / peak < 1e-5, "relative error {}", worst / peak);
    }

    #[test]
    fn rejects_invalid_impulses_and_resamples() {
        let bad = StereoImpulseResponse {
            sample_rate: 48_000,
            left: vec![],
            right: vec![],
        };
        assert_eq!(
            prepare(&bad, 48_000).unwrap_err().code,
            "audio.kernel.invalid"
        );
        let ok = StereoImpulseResponse {
            sample_rate: 24_000,
            left: vec![1.0; 100],
            right: vec![0.5; 100],
        };
        let k = prepare(&ok, 48_000).unwrap();
        assert_eq!(k.sample_rate, 48_000);
        assert_eq!(k.partitions, 1);
    }
}
