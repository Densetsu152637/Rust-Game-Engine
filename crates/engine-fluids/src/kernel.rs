//! SPH kernels and the constants derived from validated parameters.
//!
//! Density uses the poly6 kernel and constraint gradients use the spiky gradient, as in
//! Macklin & Mueller 2013. Particle mass is calibrated so that a cubic lattice at the rest
//! spacing has exactly the rest density, and the domain walls contribute a tabulated density of
//! virtual lattice particles behind each wall plane (boundary handling without wall particles).

use crate::params::FluidParams;
use glam::Vec3;
use std::f64::consts::PI;

/// Samples of the wall boundary tables over distances `0..=h` from a wall plane.
const WALL_SAMPLES: usize = 64;
/// Lateral sub-cell offsets averaged into the wall tables (per axis).
const WALL_OFFSETS: usize = 4;

#[derive(Clone, Debug)]
pub(crate) struct Kernel {
    pub h: f32,
    pub h2: f32,
    poly6: f32,
    spiky: f32,
}

impl Kernel {
    pub fn new(h: f32) -> Self {
        let hd = f64::from(h);
        Self {
            h,
            h2: h * h,
            poly6: (315.0 / (64.0 * PI * hd.powi(9))) as f32,
            spiky: (-45.0 / (PI * hd.powi(6))) as f32,
        }
    }

    /// Poly6 kernel value for squared distance `r2`.
    #[inline]
    pub fn w(&self, r2: f32) -> f32 {
        if r2 >= self.h2 {
            return 0.0;
        }
        let d = self.h2 - r2;
        self.poly6 * d * d * d
    }

    /// Spiky kernel gradient with respect to particle `i` for the offset `r = x_i - x_j` of
    /// length `len`. It points from `i` toward `j`.
    #[inline]
    pub fn grad(&self, r: Vec3, len: f32) -> Vec3 {
        if len >= self.h || len <= self.h * 1.0e-6 {
            return Vec3::ZERO;
        }
        let d = self.h - len;
        r * (self.spiky * d * d / len)
    }
}

/// Constants derived once from [`FluidParams`].
#[derive(Clone, Debug)]
pub(crate) struct Derived {
    pub kernel: Kernel,
    pub radius: f32,
    pub spacing: f32,
    pub mass: f32,
    pub inv_rest: f32,
    /// Rest volume of one particle, `mass / rest_density`.
    pub volume: f32,
    /// Self contribution `mass * W(0)` to the density.
    pub self_density: f32,
    /// Constraint relaxation added to the lambda denominator.
    pub epsilon: f32,
    /// Tensile correction scale `k / D_rest` (subtracted from pair lambdas).
    pub tensile_scale: f32,
    /// `1 / W(dq)` for the tensile correction.
    pub inv_w_dq: f32,
    wall_density: [f32; WALL_SAMPLES],
    wall_grad: [f32; WALL_SAMPLES],
}

impl Derived {
    pub fn new(params: &FluidParams) -> Self {
        let s = f64::from(params.particle_radius) * 2.0;
        let h = s * f64::from(params.kernel_scale);
        let kernel = Kernel::new(h as f32);
        let h2 = h * h;
        let poly6 = 315.0 / (64.0 * PI * h.powi(9));
        let spiky = -45.0 / (PI * h.powi(6));
        let w = |r2: f64| {
            if r2 >= h2 {
                0.0
            } else {
                let d = h2 - r2;
                poly6 * d * d * d
            }
        };
        let grad = |r: f64| {
            if r >= h || r <= 0.0 {
                0.0
            } else {
                spiky * (h - r) * (h - r)
            }
        };
        // Cubic rest lattice: density sum and squared gradient sum.
        let reach = (h / s).ceil() as i32 + 1;
        let (mut sum_w, mut sum_g2) = (0.0f64, 0.0f64);
        for i in -reach..=reach {
            for j in -reach..=reach {
                for k in -reach..=reach {
                    let r2 = f64::from(i * i + j * j + k * k) * s * s;
                    sum_w += w(r2);
                    let g = grad(r2.sqrt());
                    sum_g2 += g * g;
                }
            }
        }
        let rest = f64::from(params.rest_density);
        let mass = rest / sum_w;
        let volume = 1.0 / sum_w;
        let d_rest = volume * volume * sum_g2;
        // Wall tables: virtual lattice layers at s/2, 3s/2, ... behind the wall plane, averaged
        // over lateral sub-cell offsets so the result depends only on the wall distance.
        let mut wall_density = [0.0f32; WALL_SAMPLES];
        let mut wall_grad = [0.0f32; WALL_SAMPLES];
        for (sample, (rho_out, grad_out)) in wall_density
            .iter_mut()
            .zip(wall_grad.iter_mut())
            .enumerate()
        {
            let d = h * sample as f64 / (WALL_SAMPLES - 1) as f64;
            let (mut rho, mut dn) = (0.0f64, 0.0f64);
            for ou in 0..WALL_OFFSETS {
                for ov in 0..WALL_OFFSETS {
                    let offset_u = s * ou as f64 / WALL_OFFSETS as f64;
                    let offset_v = s * ov as f64 / WALL_OFFSETS as f64;
                    let mut layer = 0;
                    loop {
                        let normal = d + s * (0.5 + f64::from(layer));
                        if normal >= h {
                            break;
                        }
                        for b in -reach..=reach {
                            for c in -reach..=reach {
                                let du = f64::from(b) * s - offset_u;
                                let dv = f64::from(c) * s - offset_v;
                                let r2 = du * du + dv * dv + normal * normal;
                                if r2 >= h2 {
                                    continue;
                                }
                                rho += w(r2);
                                let r = r2.sqrt();
                                dn += grad(r) * normal / r;
                            }
                        }
                        layer += 1;
                    }
                }
            }
            let samples = (WALL_OFFSETS * WALL_OFFSETS) as f64;
            *rho_out = (mass * rho / samples) as f32;
            *grad_out = (mass * dn / samples) as f32;
        }
        let dq = f64::from(params.tensile_dq) * h;
        Self {
            kernel,
            radius: params.particle_radius,
            spacing: s as f32,
            mass: mass as f32,
            inv_rest: (1.0 / rest) as f32,
            volume: volume as f32,
            self_density: (mass * w(0.0)) as f32,
            epsilon: (f64::from(params.relaxation) * d_rest) as f32,
            tensile_scale: (f64::from(params.tensile_k) / d_rest) as f32,
            inv_w_dq: (1.0 / w(dq * dq)) as f32,
            wall_density,
            wall_grad,
        }
    }

    /// Density contributed by one wall at distance `d` (meters, measured from the wall plane
    /// into the domain) and its derivative with respect to `d` (non-positive).
    #[inline]
    pub fn wall(&self, d: f32) -> (f32, f32) {
        let h = self.kernel.h;
        if d >= h {
            return (0.0, 0.0);
        }
        let t = (d.max(0.0) / h) * (WALL_SAMPLES - 1) as f32;
        let index = (t as usize).min(WALL_SAMPLES - 2);
        let frac = t - index as f32;
        let lerp =
            |table: &[f32; WALL_SAMPLES]| table[index] + (table[index + 1] - table[index]) * frac;
        (lerp(&self.wall_density), lerp(&self.wall_grad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_and_wall_calibration_reproduce_rest_density() {
        let params = FluidParams::default();
        let derived = Derived::new(&params);
        let s = derived.spacing;
        // A particle at rest distance s/2 from a wall: its fluid half-lattice plus the wall
        // table must come close to the rest density.
        let reach = 3;
        let mut rho = 0.0;
        for i in -reach..=reach {
            for j in 0..=reach {
                for k in -reach..=reach {
                    let r = Vec3::new(i as f32, j as f32, k as f32) * s;
                    rho += derived.mass * derived.kernel.w(r.length_squared());
                }
            }
        }
        let (wall, slope) = derived.wall(0.5 * s);
        let total = rho + wall;
        let rest = params.rest_density;
        assert!(
            (total - rest).abs() < 0.03 * rest,
            "near-wall density {total} vs rest {rest}"
        );
        assert!(slope < 0.0);
        assert_eq!(derived.wall(derived.kernel.h), (0.0, 0.0));
    }
}
