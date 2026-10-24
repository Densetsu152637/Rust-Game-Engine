//! Deterministic random numbers and sampling shared bit-for-bit with the GPU tracer.
//!
//! Every function here uses only wrapping `u32` arithmetic and a fixed sequence of
//! `f32` operations so that `engine-acoustics-gpu` can mirror it in WGSL. The
//! integer outputs ([`pcg_hash`], [`ray_state`], [`Pcg::next_u32`]) are identical on
//! both sides; the floating-point samples agree to rounding (WGSL does not promise
//! correctly rounded `sqrt`, `cos` or `sin`).
//!
//! Stream protocol for one energy ray (section 5.4.4 of the design record):
//!
//! 1. `state = ray_state(job.seed, ray_index)`.
//! 2. Emission: `u1 = next_f32()`, `u2 = next_f32()`, `dir = uniform_sphere(u1, u2)`.
//! 3. At every reflection, exactly three draws in this order, whether or not they
//!    are used: `u_scatter`, `u1`, `u2`. The ray scatters diffusely
//!    (`cosine_hemisphere(n, u1, u2)`) when `u_scatter < scattering`, otherwise it
//!    reflects specularly.

/// PCG hash multiplier.
pub const PCG_MULTIPLIER: u32 = 747_796_405;
/// PCG hash increment.
pub const PCG_INCREMENT: u32 = 2_891_336_453;
/// PCG hash output multiplier.
pub const PCG_OUTPUT_MULTIPLIER: u32 = 277_803_737;

/// The PCG hash (O'Neill's PCG-RXS-M-XS 32/32 output on one LCG step), as used for
/// GPU noise by Jarzynski and Olano, "Hash Functions for GPU Rendering", JCGT 9(3),
/// 2020.
///
/// `s = v*747796405 + 2891336453; w = ((s >> ((s >> 28) + 4)) ^ s) * 277803737;
/// out = (w >> 22) ^ w`, all wrapping `u32`.
#[inline]
pub const fn pcg_hash(value: u32) -> u32 {
    let state = value
        .wrapping_mul(PCG_MULTIPLIER)
        .wrapping_add(PCG_INCREMENT);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(PCG_OUTPUT_MULTIPLIER);
    (word >> 22) ^ word
}

/// Initial stream state of one ray. `seed` is the job seed
/// ([`crate::EnergyJob::seed`]), split into its low and high 32-bit words:
/// `pcg_hash(pcg_hash(lo ^ pcg_hash(hi)) ^ ray)`.
#[inline]
pub const fn ray_state(seed: u64, ray: u32) -> u32 {
    let lo = seed as u32;
    let hi = (seed >> 32) as u32;
    pcg_hash(pcg_hash(lo ^ pcg_hash(hi)) ^ ray)
}

/// Derives the seed of job number `job` from a request seed, so each job has an
/// independent stream that does not depend on how jobs are batched.
#[inline]
pub const fn job_seed(seed: u64, job: u32) -> u64 {
    seed ^ ((pcg_hash(job ^ 0x5bd1_e995) as u64) << 32 | pcg_hash(job) as u64)
}

/// A PCG-hash stream: each draw replaces the state with `pcg_hash(state)` and
/// returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pcg {
    pub state: u32,
}
impl Pcg {
    pub const fn new(state: u32) -> Self {
        Self { state }
    }
    /// The stream of ray `ray` of a job with seed `seed`.
    pub const fn for_ray(seed: u64, ray: u32) -> Self {
        Self::new(ray_state(seed, ray))
    }
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        self.state = pcg_hash(self.state);
        self.state
    }
    /// Uniform in `[0, 1)`: the top 24 bits times `2^-24` (exact in `f32`).
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }
}

/// Uniform direction on the unit sphere: `z = 1 - 2 u1`, `phi = 2 pi u2`,
/// `(r cos phi, r sin phi, z)` with `r = sqrt(max(0, 1 - z^2))`.
#[inline]
pub fn uniform_sphere(u1: f32, u2: f32) -> [f32; 3] {
    let z = 1.0 - 2.0 * u1;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = std::f32::consts::TAU * u2;
    [r * phi.cos(), r * phi.sin(), z]
}

/// Orthonormal tangent and bitangent for unit `n` (Duff et al., "Building an
/// Orthonormal Basis, Revisited", JCGT 6(1), 2017).
#[inline]
pub fn orthonormal_basis(n: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let sign = if n[2] >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (sign + n[2]);
    let b = n[0] * n[1] * a;
    (
        [1.0 + sign * n[0] * n[0] * a, sign * b, -sign * n[0]],
        [b, sign + n[1] * n[1] * a, -n[1]],
    )
}

/// Cosine-weighted direction about unit `n` (Lambertian scattering):
/// local `(r cos phi, r sin phi, sqrt(max(0, 1 - u1)))` with `r = sqrt(u1)` and
/// `phi = 2 pi u2`, expressed in [`orthonormal_basis`]`(n)` as `t*x + b*y + n*z`.
#[inline]
pub fn cosine_hemisphere(n: [f32; 3], u1: f32, u2: f32) -> [f32; 3] {
    let r = u1.sqrt();
    let phi = std::f32::consts::TAU * u2;
    let x = r * phi.cos();
    let y = r * phi.sin();
    let z = (1.0 - u1).max(0.0).sqrt();
    let (t, b) = orthonormal_basis(n);
    [
        t[0] * x + b[0] * y + n[0] * z,
        t[1] * x + b[1] * y + n[1] * z,
        t[2] * x + b[2] * y + n[2] * z,
    ]
}

/// Point `i` of `count` on the Fibonacci sphere (deterministic, near-uniform):
/// `z = 1 - (2i + 1)/count`, `phi = i * golden_angle`.
#[inline]
pub fn fibonacci_sphere(i: u32, count: u32) -> [f32; 3] {
    const GOLDEN_ANGLE: f32 = 2.399_963_2; // pi * (3 - sqrt(5))
    let z = 1.0 - (2 * i + 1) as f32 / count.max(1) as f32;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = GOLDEN_ANGLE * i as f32;
    [r * phi.cos(), r * phi.sin(), z]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcg_hash_matches_reference_values() {
        // Reference values computed by hand from the documented formula.
        let s: u32 = 747_796_405u32.wrapping_mul(1).wrapping_add(2_891_336_453);
        let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
        assert_eq!(pcg_hash(1), (w >> 22) ^ w);
        assert_ne!(pcg_hash(0), pcg_hash(1));
        assert_eq!(ray_state(7, 3), ray_state(7, 3));
        assert_ne!(ray_state(7, 3), ray_state(7, 4));
        assert_ne!(ray_state(7, 3), ray_state(8 << 32 | 7, 3));
    }

    #[test]
    fn samples_are_unit_and_in_range() {
        let mut rng = Pcg::for_ray(42, 0);
        let mut sum = [0.0f64; 3];
        for _ in 0..20_000 {
            let u = rng.next_f32();
            assert!((0.0..1.0).contains(&u));
            let d = uniform_sphere(rng.next_f32(), rng.next_f32());
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5);
            for (s, v) in sum.iter_mut().zip(d) {
                *s += f64::from(v);
            }
            let n = [0.0, 0.0, -1.0];
            let c = cosine_hemisphere(n, rng.next_f32(), rng.next_f32());
            assert!(
                c[2] <= 1e-6,
                "cosine sample must stay in the hemisphere of n"
            );
        }
        for s in sum {
            assert!(
                (s / 20_000.0).abs() < 0.02,
                "uniform sphere mean is near zero"
            );
        }
    }
}
