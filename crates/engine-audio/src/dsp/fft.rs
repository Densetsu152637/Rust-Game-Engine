//! In-house radix-2 FFT (no external crates). A real transform of size `N` is
//! computed through a complex transform of size `N / 2` plus the standard
//! even/odd split. Tables are built once (control or worker thread); the
//! transforms themselves do not allocate.

use std::f64::consts::PI;

#[derive(Clone, Debug)]
pub(crate) struct RealFft {
    m: usize,
    /// `e^{-2 pi i k / M}` for `k < M/2`.
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    /// `e^{-2 pi i k / N}` for `k <= M`.
    split_re: Vec<f32>,
    split_im: Vec<f32>,
    bitrev: Vec<u32>,
}

/// Scratch for one transform: two `N/2` complex arrays.
#[derive(Clone, Debug)]
pub(crate) struct FftScratch {
    re: Vec<f32>,
    im: Vec<f32>,
}

impl RealFft {
    /// `n` must be a power of two of at least 4.
    pub(crate) fn new(n: usize) -> Self {
        assert!(
            n.is_power_of_two() && n >= 4,
            "FFT size must be a power of two >= 4"
        );
        let m = n / 2;
        let bits = m.trailing_zeros();
        let twiddle = |k: usize, size: usize| {
            let a = -2.0 * PI * k as f64 / size as f64;
            (a.cos() as f32, a.sin() as f32)
        };
        let (tw_re, tw_im) = (0..m / 2).map(|k| twiddle(k, m)).unzip();
        let (split_re, split_im) = (0..=m).map(|k| twiddle(k, n)).unzip();
        let bitrev = (0..m as u32)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (32 - bits)
                }
            })
            .collect();
        Self {
            m,
            tw_re,
            tw_im,
            split_re,
            split_im,
            bitrev,
        }
    }
    /// Number of spectrum bins (`N/2 + 1`).
    #[cfg(test)]
    pub(crate) fn bins(&self) -> usize {
        self.m + 1
    }
    pub(crate) fn scratch(&self) -> FftScratch {
        FftScratch {
            re: vec![0.0; self.m],
            im: vec![0.0; self.m],
        }
    }

    /// In-place unnormalised complex transform; `inverse` uses `e^{+i...}`.
    fn complex(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let m = self.m;
        for i in 0..m {
            let j = self.bitrev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { -1.0 } else { 1.0 };
        let mut size = 2;
        while size <= m {
            let half = size / 2;
            let step = m / size;
            for start in (0..m).step_by(size) {
                for j in 0..half {
                    let wr = self.tw_re[j * step];
                    let wi = sign * self.tw_im[j * step];
                    let a = start + j;
                    let b = a + half;
                    let br = re[b] * wr - im[b] * wi;
                    let bi = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - br;
                    im[b] = im[a] - bi;
                    re[a] += br;
                    im[a] += bi;
                }
            }
            size *= 2;
        }
    }

    /// Spectrum of `N` real samples into `N/2 + 1` bins (unnormalised).
    pub(crate) fn forward(
        &self,
        x: &[f32],
        out_re: &mut [f32],
        out_im: &mut [f32],
        s: &mut FftScratch,
    ) {
        let m = self.m;
        for i in 0..m {
            s.re[i] = x[2 * i];
            s.im[i] = x[2 * i + 1];
        }
        self.complex(&mut s.re, &mut s.im, false);
        for k in 0..=m {
            let (zr, zi) = (s.re[k % m], s.im[k % m]);
            let (cr, ci) = (s.re[(m - k) % m], -s.im[(m - k) % m]);
            let (er, ei) = (0.5 * (zr + cr), 0.5 * (zi + ci));
            // (Z - conj Z') / 2i
            let (or, oi) = (0.5 * (zi - ci), -0.5 * (zr - cr));
            let (wr, wi) = (self.split_re[k], self.split_im[k]);
            out_re[k] = er + wr * or - wi * oi;
            out_im[k] = ei + wr * oi + wi * or;
        }
    }

    /// Inverse of [`forward`](Self::forward), including the `1/N` scale.
    pub(crate) fn inverse(
        &self,
        in_re: &[f32],
        in_im: &[f32],
        out: &mut [f32],
        s: &mut FftScratch,
    ) {
        let m = self.m;
        for k in 0..m {
            let (xr, xi) = (in_re[k], in_im[k]);
            let (cr, ci) = (in_re[m - k], -in_im[m - k]);
            let (er, ei) = (0.5 * (xr + cr), 0.5 * (xi + ci));
            let (dr, di) = (0.5 * (xr - cr), 0.5 * (xi - ci));
            // O = D * conj(W^k); Z = E + i O
            let (wr, wi) = (self.split_re[k], -self.split_im[k]);
            let (or, oi) = (dr * wr - di * wi, dr * wi + di * wr);
            s.re[k] = er - oi;
            s.im[k] = ei + or;
        }
        self.complex(&mut s.re, &mut s.im, true);
        let scale = 1.0 / m as f32;
        for i in 0..m {
            out[2 * i] = s.re[i] * scale;
            out[2 * i + 1] = s.im[i] * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[f32]) -> Vec<(f64, f64)> {
        let n = x.len();
        (0..=n / 2)
            .map(|k| {
                x.iter().enumerate().fold((0.0, 0.0), |(r, i), (t, v)| {
                    let a = -2.0 * PI * (k * t) as f64 / n as f64;
                    (r + f64::from(*v) * a.cos(), i + f64::from(*v) * a.sin())
                })
            })
            .collect()
    }

    #[test]
    fn forward_matches_dft_and_inverse_round_trips() {
        for n in [4, 8, 64, 512] {
            let fft = RealFft::new(n);
            let mut s = fft.scratch();
            let x: Vec<f32> = (0..n)
                .map(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0)
                .collect();
            let mut re = vec![0.0; fft.bins()];
            let mut im = vec![0.0; fft.bins()];
            fft.forward(&x, &mut re, &mut im, &mut s);
            for (k, (r, i)) in dft(&x).into_iter().enumerate() {
                assert!(
                    (f64::from(re[k]) - r).abs() < 1e-3 && (f64::from(im[k]) - i).abs() < 1e-3,
                    "{n} {k}"
                );
            }
            let mut back = vec![0.0; n];
            fft.inverse(&re, &im, &mut back, &mut s);
            for (a, b) in x.iter().zip(&back) {
                assert!((a - b).abs() < 1e-5, "{n}");
            }
        }
    }
}
