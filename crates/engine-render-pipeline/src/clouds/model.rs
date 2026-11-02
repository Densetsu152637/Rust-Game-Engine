//! CPU reference of the cloud model.
//!
//! The same medium as `wgsl/clouds.wgsl`, in `f64`, with the same noise
//! lattice, height profile, coverage remap, step layout, light march and
//! source term. It is the oracle of the unit tests and the GPU readback
//! comparisons; nothing in the renderer calls it per frame.
//!
//! Medium. Heights `y` are measured from the base altitude and
//! `h = y / thickness` is the height fraction. The extinction at a point is
//!
//! ```text
//! sigma_t = density * remap(mix(1, fbm(c), amount), coverage) * profile(h)
//! remap(n, k) = clamp((n - (1 - k)) / k, 0, 1)
//! profile(h)  = smoothstep(0, 0.1, h) * (1 - smoothstep(0.6, 1, h))
//! ```
//!
//! where `c` is the point in wrapped noise space and `fbm` is the normalised
//! sum of up to [`MAX_OCTAVES`] octaves of fog's value noise. The profile
//! integrates to [`PROFILE_INTEGRAL`] `= 0.75` over the layer, so a constant
//! noise field (`amount = 0`) gives the closed form [`slab_transmittance`].
//!
//! Lighting. Each primary sample sees the sun through `light_steps` midpoint
//! segments of `light_distance` toward it (optical depth `tau`), and scatters
//! `albedo * (E * visibility * sum_k a^k P(mu, c^k) exp(-b^k tau) + ambient)`
//! per unit extinction with `P` a dual-lobe Cornette-Shanks phase. The sum over
//! `k < 3` with `a = b = c = 1/2` is the Wrenninge multiple-scattering
//! approximation (only `k = 0` is single scattering). The source is constant
//! over a step, so the step contributes `T * source * (1 - exp(-sigma dt))`.

use super::{CloudFrame, CloudSettings};
use crate::atmosphere::{MAX_RADIANCE, mie_phase};
pub use crate::fog::{NOISE_PERIOD, noise_origin, value_noise};

/// Most noise octaves.
pub const MAX_OCTAVES: u32 = 4;
/// Integral of the height profile over `h` in `0..=1`.
pub const PROFILE_INTEGRAL: f64 = 0.75;
/// The march stops when the transmittance falls below this.
pub const CUTOFF_TRANSMITTANCE: f64 = 0.01;
/// Asymmetry of the backward phase lobe.
pub const BACK_ANISOTROPY: f64 = -0.3;
/// Weight of the backward lobe in the dual-lobe phase.
pub const BACK_WEIGHT: f64 = 0.3;
/// Multiple-scattering octaves when enabled (one is single scattering).
pub const MS_OCTAVES: u32 = 3;
/// Multiple scattering: per-octave factor of the amplitude, the light-march
/// optical depth and the phase asymmetry.
pub const MS_FACTOR: f64 = 0.5;
/// Ambient light at the layer base relative to the top.
pub const AMBIENT_BASE: f64 = 0.5;
/// Directions flatter than this never leave the layer's horizontal plane.
pub const FLAT_DIRECTION: f64 = 1.0e-5;
/// Height fraction over which the profile rises from the base.
pub const PROFILE_RISE: f64 = 0.1;
/// Height fraction where the profile starts to fall toward the top.
pub const PROFILE_FALL: f64 = 0.6;
/// Sun elevation (direction `y`) over which the sun fades in above the
/// horizon plane.
pub const SUN_HORIZON: f64 = 0.05;

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Vertical density profile at height fraction `h`: zero outside `0..1`.
pub fn profile(h: f64) -> f64 {
    smoothstep(0.0, PROFILE_RISE, h) * (1.0 - smoothstep(PROFILE_FALL, 1.0, h))
}

/// Transmittance through a layer of constant noise field (`amount = 0`,
/// coverage above zero) along a ray with vertical cosine `dy`, for a ray that
/// crosses the whole layer: `exp(-density * thickness * 0.75 / |dy|)`.
pub fn slab_transmittance(density: f64, thickness: f64, dy: f64) -> f64 {
    (-density * thickness * PROFILE_INTEGRAL / dy.abs()).exp()
}

/// Normalised fractal noise in `0..=1` at lattice coordinates `c`.
pub fn fbm(c: [f64; 3], octaves: u32, seed: u32) -> f64 {
    let (mut sum, mut norm, mut amplitude, mut frequency) = (0.0, 0.0, 1.0, 1.0);
    for i in 0..octaves.clamp(1, MAX_OCTAVES) {
        sum += amplitude * value_noise(c.map(|v| v * frequency), seed.wrapping_add(i));
        norm += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    sum / norm
}

/// Coverage remap of a noise value: `0` below `1 - coverage`, rising to `1`.
pub fn remap(n: f64, coverage: f64) -> f64 {
    ((n - (1.0 - coverage)) / coverage.max(1.0e-3)).clamp(0.0, 1.0)
}

/// Extinction (1/m) at noise-space point `c` and height `y` metres above the
/// base.
pub fn extinction(settings: &CloudSettings, c: [f64; 3], y: f64) -> f64 {
    let h = y / settings.thickness();
    if h <= 0.0 || h >= 1.0 {
        return 0.0;
    }
    let amount = f64::from(settings.noise_amount);
    let n = if amount > 0.0 {
        1.0 + amount * (fbm(c, settings.noise_octaves, settings.noise_seed) - 1.0)
    } else {
        1.0
    };
    f64::from(settings.density) * remap(n, f64::from(settings.coverage)) * profile(h)
}

/// Dual-lobe Cornette-Shanks phase: the forward lobe `g * scale` and the
/// backward lobe `BACK_ANISOTROPY * scale`, weighted `1 - w` and `w`.
pub fn phase(mu: f64, g: f64, scale: f64) -> f64 {
    (1.0 - BACK_WEIGHT) * mie_phase(mu, g * scale)
        + BACK_WEIGHT * mie_phase(mu, BACK_ANISOTROPY * scale)
}

/// Entry and exit distance of a ray from `h0` metres above the base with
/// vertical cosine `dy` through a layer `thickness` metres thick, bounded by
/// `limit`. `None` when the ray misses the layer or the limit comes first.
pub fn layer_interval(h0: f64, dy: f64, thickness: f64, limit: f64) -> Option<(f64, f64)> {
    let (t0, t1) = if dy.abs() < FLAT_DIRECTION {
        if !(0.0..=thickness).contains(&h0) {
            return None;
        }
        (0.0, limit)
    } else {
        let (a, b) = (-h0 / dy, (thickness - h0) / dy);
        (a.min(b).max(0.0), a.max(b).min(limit))
    };
    (t1 > t0).then_some((t0, t1))
}

/// Result of one view-ray march.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudMarch {
    /// Fraction of the background light that survives the clouds.
    pub transmittance: f64,
    /// Radiance scattered toward the camera.
    pub inscatter: [f64; 3],
    /// Distance at which the ray enters the layer (zero when none).
    pub entry: f64,
}

impl CloudMarch {
    const NONE: Self = Self {
        transmittance: 1.0,
        inscatter: [0.0; 3],
        entry: 0.0,
    };
}

/// Optical depth from noise-space point `c` at height `y` toward the sun.
pub fn light_depth(settings: &CloudSettings, c: [f64; 3], y: f64, toward: [f64; 3]) -> f64 {
    let frequency = 1.0 / f64::from(settings.noise_scale);
    let steps = settings.light_steps;
    let dl = f64::from(settings.light_distance) / f64::from(steps);
    (0..steps)
        .map(|j| {
            let d = (f64::from(j) + 0.5) * dl;
            let p = std::array::from_fn(|k| c[k] + toward[k] * d * frequency);
            extinction(settings, p, y + toward[1] * d) * dl
        })
        .sum()
}

/// Light scattered at a sample per unit extinction: the sun (phase, light
/// march, multiple scattering) plus the ambient term, times the albedo.
pub fn source(
    settings: &CloudSettings,
    c: [f64; 3],
    y: f64,
    dir: [f64; 3],
    sun: Option<([f64; 3], [f64; 3])>,
    ambient: [f64; 3],
) -> [f64; 3] {
    let h = y / settings.thickness();
    let mut radiance: [f64; 3] = ambient.map(|a| {
        a * f64::from(settings.ambient_intensity) * (AMBIENT_BASE + (1.0 - AMBIENT_BASE) * h)
    });
    if let Some((toward, irradiance)) = sun {
        let visibility = smoothstep(0.0, SUN_HORIZON, toward[1]);
        if visibility > 0.0 {
            let tau = light_depth(settings, c, y, toward);
            let mu = dir[0] * toward[0] + dir[1] * toward[1] + dir[2] * toward[2];
            let octaves = if settings.multiple_scattering {
                MS_OCTAVES
            } else {
                1
            };
            let g = f64::from(settings.anisotropy);
            let (mut amplitude, mut attenuation, mut scale) = (1.0, 1.0, 1.0);
            let mut sun_term = 0.0;
            for _ in 0..octaves {
                sun_term += amplitude * phase(mu, g, scale) * (-tau * attenuation).exp();
                amplitude *= MS_FACTOR;
                attenuation *= MS_FACTOR;
                scale *= MS_FACTOR;
            }
            let k = visibility * sun_term * f64::from(settings.sun_intensity);
            for i in 0..3 {
                radiance[i] += irradiance[i] * k;
            }
        }
    }
    std::array::from_fn(|k| (f64::from(settings.albedo[k]) * radiance[k]).min(MAX_RADIANCE))
}

/// March the view ray `dir` through the layer, limited by the geometry at
/// `scene_distance` (`None`: sky) and the maximum distance.
pub fn march(
    frame: &CloudFrame,
    dir: [f64; 3],
    scene_distance: Option<f64>,
    sun: Option<([f64; 3], [f64; 3])>,
    ambient: [f64; 3],
) -> CloudMarch {
    let s = &frame.settings;
    let maximum = f64::from(s.max_distance);
    let limit = scene_distance.map_or(maximum, |d| d.min(maximum));
    let h0 = frame.camera_height();
    let Some((t0, t1)) = layer_interval(h0, dir[1], s.thickness(), limit) else {
        return CloudMarch::NONE;
    };
    let origin = frame.noise_origin();
    let frequency = 1.0 / f64::from(s.noise_scale);
    let steps = s.primary_steps;
    let dt = (t1 - t0) / f64::from(steps);
    let mut transmittance = 1.0;
    let mut inscatter = [0.0; 3];
    for i in 0..steps {
        let t = t0 + (f64::from(i) + 0.5) * dt;
        let c: [f64; 3] = std::array::from_fn(|k| origin[k] + dir[k] * t * frequency);
        let y = h0 + dir[1] * t;
        let sigma = extinction(s, c, y);
        if sigma <= 0.0 {
            continue;
        }
        let light = source(s, c, y, dir, sun, ambient);
        let step = (-sigma * dt).exp();
        for k in 0..3 {
            inscatter[k] += transmittance * light[k] * (1.0 - step);
        }
        transmittance *= step;
        if transmittance < CUTOFF_TRANSMITTANCE {
            break;
        }
    }
    CloudMarch {
        transmittance,
        inscatter,
        entry: t0,
    }
}

/// Opacity factor of the horizon fade at entry distance `entry`.
pub fn fade(settings: &CloudSettings, entry: f64) -> f64 {
    if settings.fade_distance > 0.0 {
        (-entry / f64::from(settings.fade_distance)).exp()
    } else {
        1.0
    }
}

/// A pixel with the clouds composited over `scene` (the background colour
/// already holding sky and aerial perspective): `scene * T' + inscatter * f`
/// with `f = exp(-entry / fade_distance)` (one without a fade distance) and
/// `T' = 1 - f (1 - T)`.
pub fn composite(
    frame: &CloudFrame,
    dir: [f64; 3],
    scene_distance: Option<f64>,
    scene: [f64; 3],
    sun: Option<([f64; 3], [f64; 3])>,
    ambient: [f64; 3],
) -> [f64; 3] {
    let m = march(frame, dir, scene_distance, sun, ambient);
    let f = fade(&frame.settings, m.entry);
    let t = 1.0 - f * (1.0 - m.transmittance);
    std::array::from_fn(|k| scene[k] * t + m.inscatter[k] * f)
}
