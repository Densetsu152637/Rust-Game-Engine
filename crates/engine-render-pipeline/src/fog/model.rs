//! CPU reference of the fog model.
//!
//! The same medium as `wgsl/fog.wgsl`, in `f64`, with the same height
//! integral, noise lattice hash, step layout and source term. It is the oracle
//! of the unit tests and the GPU readback comparisons; nothing in the renderer
//! calls it per frame except [`noise_origin`], which the host evaluates in
//! `f64` so the shader only ever sees small, wrapped coordinates.
//!
//! Medium. Extinction is
//! `sigma_t(p) = density * exp(-falloff * (y - base)) * m(p)` with the noise
//! modulation `m = 1 + amount * (2 n(p) - 1)` (`n` is value noise in `0..=1`,
//! so `m` stays within `1 -+ amount` and its mean is about one). Scattering is
//! `albedo * sigma_t`. Along a ray the source term
//! `albedo * (sun_irradiance * phase + ambient)` is constant, so single
//! scattering collapses to `L = scene * T + source * (1 - T)` with
//! `T = exp(-tau)`; only the optical depth `tau` needs integrating.

use super::{FogFrame, FogSettings, MAX_NOISE_STEPS};
use crate::atmosphere::{MAX_RADIANCE, mie_phase};

/// Noise lattice period in cells per axis (a power of two).
pub const NOISE_PERIOD: u32 = 1024;
/// Largest exponent evaluated, which keeps `exp` finite in `f32`.
pub const EXPONENT_LIMIT: f64 = 80.0;

/// Below this `|k dy L|` the integral uses a series, above it the exponential
/// difference; matches the shader.
const SERIES_LIMIT: f64 = 0.01;

/// Optical depth of `density * exp(-falloff * h)` along a segment of `length`
/// metres that starts at height `h0` above the base with vertical cosine `dy`.
/// Exact for the exponential profile.
pub fn height_optical_depth(density: f64, falloff: f64, h0: f64, dy: f64, length: f64) -> f64 {
    let e0 = (-falloff * h0).min(EXPONENT_LIMIT);
    let x = falloff * dy * length;
    if x.abs() < SERIES_LIMIT {
        let f = 1.0 - x * (0.5 - x * (1.0 / 6.0 - x * (1.0 / 24.0 - x / 120.0)));
        return density * length * e0.exp() * f;
    }
    let e1 = (e0 - x).min(EXPONENT_LIMIT);
    (density * (e0.exp() - e1.exp()) / (falloff * dy)).max(0.0)
}

/// Transmittance of pure height fog (no noise) along a ray: the closed form
/// the GPU tests compare against.
pub fn height_transmittance(
    density: f64,
    falloff: f64,
    camera_height: f64,
    base_height: f64,
    dy: f64,
    distance: f64,
) -> f64 {
    (-height_optical_depth(density, falloff, camera_height - base_height, dy, distance)).exp()
}

/// Hash of one lattice cell to `[0, 1)`, wrapped to [`NOISE_PERIOD`].
pub fn cell_value(cell: [i64; 3], seed: u32) -> f64 {
    let mask = i64::from(NOISE_PERIOD - 1);
    let mut v = cell.map(|c| (c & mask) as u32);
    let add = 1_013_904_223u32
        .wrapping_add(seed.wrapping_mul(747_796_405))
        .wrapping_add(2_891_336_453);
    v = v.map(|c| c.wrapping_mul(1_664_525).wrapping_add(add));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    v = v.map(|c| c ^ (c >> 16));
    v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
    v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
    v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
    f64::from(v[0] >> 8) / 16_777_216.0
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Value noise in `0..=1` at lattice coordinates `c` (quintic interpolation).
pub fn value_noise(c: [f64; 3], seed: u32) -> f64 {
    let base = c.map(f64::floor);
    let f: [f64; 3] = std::array::from_fn(|k| c[k] - base[k]);
    let u = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let b = base.map(|v| v as i64);
    let at = |dx: i64, dy: i64, dz: i64| cell_value([b[0] + dx, b[1] + dy, b[2] + dz], seed);
    let x00 = lerp(at(0, 0, 0), at(1, 0, 0), u[0]);
    let x10 = lerp(at(0, 1, 0), at(1, 1, 0), u[0]);
    let x01 = lerp(at(0, 0, 1), at(1, 0, 1), u[0]);
    let x11 = lerp(at(0, 1, 1), at(1, 1, 1), u[0]);
    lerp(lerp(x00, x10, u[1]), lerp(x01, x11, u[1]), u[2])
}

/// The camera in noise space: absolute position minus the wind drift, in
/// lattice cells, wrapped to one noise period per axis. Evaluated in `f64`
/// from absolute coordinates, so it is invariant under floating-origin rebases
/// and never loses precision at large times or distances.
pub fn noise_origin(camera: [f64; 3], wind: [f32; 3], time: f64, scale: f32) -> [f64; 3] {
    let period = f64::from(NOISE_PERIOD);
    std::array::from_fn(|k| {
        ((camera[k] - f64::from(wind[k]) * time) / f64::from(scale)).rem_euclid(period)
    })
}

/// Noise modulation of the extinction at lattice coordinates `c`.
pub fn modulation(amount: f64, c: [f64; 3], seed: u32) -> f64 {
    1.0 + amount * (2.0 * value_noise(c, seed) - 1.0)
}

/// Optical depth along `dir` over `distance` metres: the first
/// `noise_distance` metres in `noise_steps` midpoint segments with noise
/// modulation, the remainder as the analytic height integral.
pub fn optical_depth(frame: &FogFrame, dir: [f64; 3], distance: f64) -> f64 {
    let s = &frame.settings;
    let (density, falloff) = (f64::from(s.density), f64::from(s.height_falloff));
    let h0 = frame.camera_height();
    let amount = f64::from(s.noise_amount);
    let mut tau = 0.0;
    let mut marched = 0.0;
    if amount > 0.0 {
        let origin = frame.noise_origin();
        let frequency = 1.0 / f64::from(s.noise_scale);
        let steps = s.noise_steps.min(MAX_NOISE_STEPS);
        marched = distance.min(f64::from(s.noise_distance));
        let dt = marched / f64::from(steps);
        for i in 0..steps {
            let start = f64::from(i) * dt;
            let mid = (start + 0.5 * dt) * frequency;
            let c = std::array::from_fn(|k| origin[k] + dir[k] * mid);
            tau += height_optical_depth(density, falloff, h0 + dir[1] * start, dir[1], dt)
                * modulation(amount, c, s.noise_seed);
        }
    }
    if distance > marched {
        tau += height_optical_depth(
            density,
            falloff,
            h0 + dir[1] * marched,
            dir[1],
            distance - marched,
        );
    }
    tau
}

/// Source radiance per unit extinction: `albedo * (sun * phase + ambient)`.
/// `sun` is the unit vector toward the sun, `irradiance` its irradiance in
/// shading units; `None` when the view has no directional light.
pub fn source_radiance(
    settings: &FogSettings,
    dir: [f64; 3],
    sun: Option<([f64; 3], [f64; 3])>,
    ambient: [f64; 3],
) -> [f64; 3] {
    let mut source: [f64; 3] = ambient.map(|a| a * f64::from(settings.ambient_intensity));
    if let Some((toward, irradiance)) = sun {
        let mu = dir[0] * toward[0] + dir[1] * toward[1] + dir[2] * toward[2];
        let phase =
            mie_phase(mu, f64::from(settings.anisotropy)) * f64::from(settings.sun_intensity);
        for k in 0..3 {
            source[k] += irradiance[k] * phase;
        }
    }
    std::array::from_fn(|k| (f64::from(settings.albedo[k]) * source[k]).min(MAX_RADIANCE))
}

/// A fogged pixel: `scene * T + source * (1 - T)` along `dir` over
/// `distance` metres.
pub fn fogged(
    frame: &FogFrame,
    dir: [f64; 3],
    distance: f64,
    scene: [f64; 3],
    sun: Option<([f64; 3], [f64; 3])>,
    ambient: [f64; 3],
) -> [f64; 3] {
    if distance <= 0.0 {
        return scene;
    }
    let t = (-optical_depth(frame, dir, distance)).exp();
    let source = source_radiance(&frame.settings, dir, sun, ambient);
    std::array::from_fn(|k| scene[k] * t + source[k] * (1.0 - t))
}
