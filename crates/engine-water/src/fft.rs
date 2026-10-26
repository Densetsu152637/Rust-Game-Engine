//! Radix-2 complex FFT (iterative Cooley-Tukey, in place) in one and two dimensions.
//!
//! Convention: the forward transform is `X[k] = sum_n x[n] e^(-2 pi i k n / N)` and the
//! inverse is `x[n] = sum_k X[k] e^(+2 pi i k n / N)`; neither is normalized, so
//! `inverse(forward(x)) = N x` (`N^2 x` in 2D). Twiddles are computed in `f64` per call, so
//! the round-trip error stays near `f32` rounding (about 1e-6 relative for N = 512).
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Mul, Sub};

/// Largest supported transform length (and 2D side).
pub const MAX_FFT_SIZE: usize = 4096;

/// A complex number in `f32`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}
impl Complex {
    pub const ZERO: Complex = Complex { re: 0.0, im: 0.0 };
    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }
    /// `e^(i theta)`.
    pub fn from_angle(theta: f32) -> Self {
        let (s, c) = theta.sin_cos();
        Self { re: c, im: s }
    }
    pub fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }
    pub fn scale(self, s: f32) -> Self {
        Self::new(self.re * s, self.im * s)
    }
    /// Multiplication by `i`.
    pub fn times_i(self) -> Self {
        Self::new(-self.im, self.re)
    }
    pub fn norm_sqr(self) -> f32 {
        self.re * self.re + self.im * self.im
    }
}
impl Add for Complex {
    type Output = Complex;
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
}
impl AddAssign for Complex {
    fn add_assign(&mut self, o: Complex) {
        self.re += o.re;
        self.im += o.im;
    }
}
impl Sub for Complex {
    type Output = Complex;
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
}
impl Mul for Complex {
    type Output = Complex;
    fn mul(self, o: Complex) -> Complex {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

fn check_size(n: usize) -> Result<()> {
    if n == 0 || !n.is_power_of_two() || n > MAX_FFT_SIZE {
        return Err(Error::new(
            "water.fft.size",
            format!("FFT length {n} must be a power of two in 1..={MAX_FFT_SIZE}"),
        ));
    }
    Ok(())
}

/// Twiddle table `e^(sign 2 pi i j / n)` for `j < n / 2`.
fn twiddles(n: usize, inverse: bool) -> Vec<Complex> {
    let sign = if inverse { 1.0 } else { -1.0 };
    (0..n / 2)
        .map(|j| {
            let angle = sign * std::f64::consts::TAU * j as f64 / n as f64;
            Complex::new(angle.cos() as f32, angle.sin() as f32)
        })
        .collect()
}

fn transform(data: &mut [Complex], table: &[Complex]) {
    let n = data.len();
    let bits = n.trailing_zeros();
    if bits == 0 {
        return;
    }
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            data.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let stride = n / len;
        for start in (0..n).step_by(len) {
            for j in 0..half {
                let w = table[j * stride];
                let a = data[start + j];
                let b = data[start + j + half] * w;
                data[start + j] = a + b;
                data[start + j + half] = a - b;
            }
        }
        len *= 2;
    }
}

/// In-place 1D transform of a power-of-two length sequence (unnormalized).
pub fn fft(data: &mut [Complex], inverse: bool) -> Result<()> {
    check_size(data.len())?;
    let table = twiddles(data.len(), inverse);
    transform(data, &table);
    Ok(())
}

/// In-place 2D transform of an `n` x `n` row-major grid (rows, then columns; unnormalized).
pub fn fft2(data: &mut [Complex], n: usize, inverse: bool) -> Result<()> {
    check_size(n)?;
    if data.len() != n * n {
        return Err(Error::new(
            "water.fft.size",
            format!("2D FFT needs {} values, got {}", n * n, data.len()),
        ));
    }
    let table = twiddles(n, inverse);
    for row in data.chunks_exact_mut(n) {
        transform(row, &table);
    }
    let mut column = vec![Complex::ZERO; n];
    for c in 0..n {
        for r in 0..n {
            column[r] = data[r * n + c];
        }
        transform(&mut column, &table);
        for r in 0..n {
            data[r * n + c] = column[r];
        }
    }
    Ok(())
}

/// Reference O(N^2) discrete Fourier transform with the same convention (for tests and
/// verification).
pub fn dft(data: &[Complex], inverse: bool) -> Vec<Complex> {
    let n = data.len();
    let sign = if inverse { 1.0 } else { -1.0 };
    (0..n)
        .map(|k| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (j, x) in data.iter().enumerate() {
                let angle = sign * std::f64::consts::TAU * ((k * j) % n) as f64 / n as f64;
                let (s, c) = angle.sin_cos();
                re += x.re as f64 * c - x.im as f64 * s;
                im += x.re as f64 * s + x.im as f64 * c;
            }
            Complex::new(re as f32, im as f32)
        })
        .collect()
}
