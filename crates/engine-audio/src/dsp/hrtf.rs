//! Dataset-free binaural rendering: the Brown & Duda (1998) structural model.
//!
//! C. P. Brown and R. O. Duda, "A structural model for binaural sound
//! synthesis", IEEE Trans. Speech and Audio Processing 6(5), 1998:
//!
//! * Per-ear delay (their eq. 5, offset by a/c so it is never negative), with
//!   `theta` the angle between the source direction and the ear axis:
//!   `(a/c)(1 - cos theta)` for `theta < 90 deg`, else `(a/c)(1 + theta - 90 deg)`.
//!   The interaural difference is Woodworth's `(a/c)(psi + sin psi)`.
//! * Head shadow (eq. 2-4): `H(s) = (1 + alpha s / 2w0) / (1 + s / 2w0)`, `w0 = c/a`,
//!   `alpha(theta) = (1 + amin/2) + (1 - amin/2) cos(theta / theta_min * 180 deg)`,
//!   `amin = 0.1`, `theta_min = 150 deg`, discretised by the bilinear transform.
//! * Pinna echoes (eq. 6 and Table I): `tau_k = A_k cos(az/2) sin(D_k (90 deg - el)) + B_k`
//!   samples at 44.1 kHz with reflection coefficients `rho_k`.
//!
//! Each ear's low-frequency gain is `1/sqrt(2)`, so a source has the same total
//! power at low frequencies as with the equal-power stereo law.

use super::ramp::Ramp;
use std::f64::consts::{FRAC_PI_2, PI};

pub(crate) const EAR_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
const ALPHA_MIN: f64 = 0.1;
const THETA_MIN: f64 = 150.0 * PI / 180.0;
pub(crate) const PINNA_ECHOES: usize = 5;
const PINNA_RHO: [f32; PINNA_ECHOES] = [0.5, -1.0, 0.5, -0.25, 0.25];
const PINNA_A: [f64; PINNA_ECHOES] = [1.0, 5.0, 5.0, 5.0, 5.0];
const PINNA_B: [f64; PINNA_ECHOES] = [2.0, 4.0, 7.0, 11.0, 13.0];
const PINNA_D: [f64; PINNA_ECHOES] = [1.0, 0.5, 0.5, 0.5, 0.5];
const PINNA_RATE: f64 = 44_100.0;
const PINNA_HISTORY: usize = 128;

/// Listener-frame ear axes: +X is the right ear.
pub(crate) const EAR_AXES: [[f64; 3]; 2] = [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]];

/// Sample-rate and head-size dependent constants.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeadModel {
    radius_over_c_samples: f64,
    k: f64,
    beta: f64,
    a1: f32,
    pinna_scale: f64,
}

impl HeadModel {
    pub(crate) fn new(sample_rate: u32, head_radius_m: f32, speed_of_sound: f32) -> Self {
        let fs = f64::from(sample_rate);
        let a = f64::from(head_radius_m);
        let c = f64::from(speed_of_sound);
        let k = 2.0 * fs;
        let beta = 2.0 * c / a;
        Self {
            radius_over_c_samples: a / c * fs,
            k,
            beta,
            a1: ((beta - k) / (k + beta)) as f32,
            pinna_scale: fs / PINNA_RATE,
        }
    }
    /// Longest per-ear delay in samples.
    pub(crate) fn max_ear_delay(&self) -> f64 {
        self.radius_over_c_samples * (1.0 + FRAC_PI_2)
    }
    /// Per-ear delay in samples for incidence angle `theta` (radians from the ear axis).
    pub(crate) fn ear_delay(&self, theta: f64) -> f32 {
        let t = if theta < FRAC_PI_2 {
            1.0 - theta.cos()
        } else {
            1.0 + theta - FRAC_PI_2
        };
        (self.radius_over_c_samples * t) as f32
    }
    /// Head-shadow numerator `(b0, b1)`; the pole is fixed (`a1`).
    pub(crate) fn shadow(&self, theta: f64) -> (f32, f32) {
        let alpha =
            (1.0 + ALPHA_MIN / 2.0) + (1.0 - ALPHA_MIN / 2.0) * (theta / THETA_MIN * PI).cos();
        let den = self.k + self.beta;
        (
            ((alpha * self.k + self.beta) / den) as f32,
            ((self.beta - alpha * self.k) / den) as f32,
        )
    }
    pub(crate) fn a1(&self) -> f32 {
        self.a1
    }
    /// Pinna echo delays in samples for listener-frame `azimuth` (0 = front,
    /// positive right) and `elevation`, both in radians.
    pub(crate) fn pinna_delays(&self, azimuth: f64, elevation: f64) -> [f32; PINNA_ECHOES] {
        let az = azimuth.clamp(-PI, PI);
        let el = elevation.clamp(-FRAC_PI_2, FRAC_PI_2);
        std::array::from_fn(|k| {
            let tau =
                PINNA_A[k] * (az / 2.0).cos() * (PINNA_D[k] * (FRAC_PI_2 - el)).sin() + PINNA_B[k];
            (tau * self.pinna_scale) as f32
        })
    }
}

/// Angle between a unit direction and an ear axis.
pub(crate) fn incidence(dir: [f64; 3], ear: usize) -> f64 {
    super::dot(dir, EAR_AXES[ear]).clamp(-1.0, 1.0).acos()
}

/// Listener-frame azimuth (0 front, positive right) and elevation.
pub(crate) fn azimuth_elevation(dir: [f64; 3]) -> (f64, f64) {
    (dir[0].atan2(-dir[2]), dir[1].clamp(-1.0, 1.0).asin())
}

/// One ear's head-shadow filter with ramped numerator coefficients.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shadow {
    pub(crate) b0: Ramp,
    pub(crate) b1: Ramp,
    state: f32,
}

impl Shadow {
    pub(crate) fn new((b0, b1): (f32, f32)) -> Self {
        Self {
            b0: Ramp::new(b0),
            b1: Ramp::new(b1),
            state: 0.0,
        }
    }
    pub(crate) fn set(&mut self, (b0, b1): (f32, f32), frames: usize) {
        self.b0.set(b0, frames);
        self.b1.set(b1, frames);
    }
    pub(crate) fn settled(&self) -> bool {
        self.b0.settled() && self.b1.settled()
    }
    /// Filters a block whose coefficients may be ramping; `io` holds the input
    /// and receives the output.
    pub(crate) fn run_ramped(&mut self, a1: f32, io: &mut [f32]) {
        let n = io.len();
        let mut b0 = [0.0f32; super::BLOCK];
        let mut b1 = [0.0f32; super::BLOCK];
        self.b0.fill(&mut b0[..n]);
        self.b1.fill(&mut b1[..n]);
        let mut s = self.state;
        for i in 0..n {
            let x = io[i];
            let y = b0[i] * x + s;
            s = b1[i] * x - a1 * y;
            io[i] = y;
        }
        self.state = s;
    }
    /// Filters a block with the (settled) current coefficients, scaling by `gain`.
    pub(crate) fn run(&mut self, a1: f32, io: &mut [f32], gain: f32) {
        let (b0, b1) = (self.b0.value(), self.b1.value());
        let mut s = self.state;
        for x in io.iter_mut() {
            let y = b0 * *x + s;
            s = b1 * *x - a1 * y;
            *x = y * gain;
        }
        self.state = s;
    }
    pub(crate) fn flush(&mut self) {
        super::flush(&mut self.state);
    }
}

/// One ear's pinna echo stage.
#[derive(Clone, Debug)]
pub(crate) struct Pinna {
    history: [f32; PINNA_HISTORY],
    pos: usize,
}

impl Default for Pinna {
    fn default() -> Self {
        Self {
            history: [0.0; PINNA_HISTORY],
            pos: 0,
        }
    }
}

impl Pinna {
    /// Block form with fixed echo delays.
    pub(crate) fn run(&mut self, io: &mut [f32], delays: &[f32; PINNA_ECHOES]) {
        let taps: [(usize, f32); PINNA_ECHOES] = std::array::from_fn(|k| {
            let d = delays[k].clamp(0.0, (PINNA_HISTORY - 2) as f32);
            (d as usize, d - d.trunc())
        });
        let mask = PINNA_HISTORY - 1;
        for x in io.iter_mut() {
            self.pos = (self.pos + 1) & mask;
            self.history[self.pos] = *x;
            let mut y = *x;
            for (k, (i, t)) in taps.iter().enumerate() {
                let a = self.history[self.pos.wrapping_sub(*i) & mask];
                let b = self.history[self.pos.wrapping_sub(*i + 1) & mask];
                y += PINNA_RHO[k] * (a + (b - a) * t);
            }
            *x = y;
        }
    }
    pub(crate) fn flush(&mut self) {
        self.history.iter_mut().for_each(super::flush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn woodworth_itd_at_ninety_degrees() {
        let h = HeadModel::new(48_000, 0.0875, 343.0);
        let right = [1.0, 0.0, 0.0];
        let itd = h.ear_delay(incidence(right, 0)) - h.ear_delay(incidence(right, 1));
        let ms = f64::from(itd) / 48.0;
        assert!(
            (ms - 0.0875 / 343.0 * (FRAC_PI_2 + 1.0) * 1000.0).abs() < 1e-6,
            "{ms}"
        );
        let front = [0.0, 0.0, -1.0];
        assert_eq!(
            h.ear_delay(incidence(front, 0)),
            h.ear_delay(incidence(front, 1))
        );
    }

    #[test]
    fn shadow_has_unit_dc_gain_and_boosts_the_near_ear() {
        let h = HeadModel::new(48_000, 0.0875, 343.0);
        for theta in [0.0, 1.0, 2.6, PI] {
            let (b0, b1) = h.shadow(theta);
            assert!(((b0 + b1) / (1.0 + h.a1()) - 1.0).abs() < 1e-5);
        }
        let near = h.shadow(0.0);
        let far = h.shadow(PI);
        // Nyquist gain (b0 - b1) / (1 - a1).
        assert!((near.0 - near.1) > 1.5 * (1.0 - h.a1()));
        assert!((far.0 - far.1) < 0.4 * (1.0 - h.a1()));
    }

    #[test]
    fn pinna_echoes_cancel_at_dc() {
        let mut p = Pinna::default();
        let h = HeadModel::new(48_000, 0.0875, 343.0);
        let d = h.pinna_delays(0.3, 0.2);
        let mut block = [1.0f32; 200];
        p.run(&mut block, &d);
        assert!((block[199] - 1.0).abs() < 1e-6);
    }
}
