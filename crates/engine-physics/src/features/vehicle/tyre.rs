//! Slip-based tyre model (docs/physics-vehicles.md#tyre-model). Pure maths, no Rapier types.
use crate::{Error, Result};

/// One Pacejka "magic formula" axis, `y(x) = sin(C atan(B x - E (B x - atan(B x))))`.
///
/// `x` is the slip ratio (longitudinal) or the slip angle in radians (lateral). The result is
/// the force as a fraction of the peak friction force `D`, so its peak is about 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacejkaAxis {
    /// Stiffness factor `B` (`0.1..=100`).
    pub b: f32,
    /// Shape factor `C` (`1.05..=2`, so the sine reaches its peak).
    pub c: f32,
    /// Curvature factor `E` (`-10..=1`).
    pub e: f32,
}
impl PacejkaAxis {
    pub fn validate(self) -> Result<Self> {
        let ok = self.b.is_finite()
            && self.c.is_finite()
            && self.e.is_finite()
            && (0.1..=100.).contains(&self.b)
            && (1.05..=2.).contains(&self.c)
            && (-10. ..=1.).contains(&self.e);
        if !ok {
            return Err(Error("invalid tyre curve"));
        }
        Ok(self)
    }
    /// Normalized force at slip `x` (odd function, peak about 1).
    pub fn curve(&self, x: f32) -> f32 {
        let bx = self.b * x;
        (self.c * (bx - self.e * (bx - bx.atan())).atan()).sin()
    }
    /// Slip of the first maximum of [`PacejkaAxis::curve`], found by golden-section search over
    /// `0..=2` (slip ratio 2 or about 115 degrees).
    pub fn peak_slip(&self) -> f32 {
        // The curve rises monotonically up to its first maximum, so start with a coarse scan
        // to bracket it.
        let mut best = 0.;
        let mut best_value = f32::MIN;
        for i in 0..=400 {
            let x = i as f32 * 0.005;
            let value = self.curve(x);
            if value > best_value {
                best_value = value;
                best = x;
            } else if value < best_value - 1e-3 {
                break;
            }
        }
        let mut lo = (best - 0.005).max(0.);
        let mut hi = (best + 0.005).min(2.);
        let phi = 0.618_034;
        for _ in 0..24 {
            let a = hi - (hi - lo) * phi;
            let b = lo + (hi - lo) * phi;
            if self.curve(a) < self.curve(b) {
                lo = a;
            } else {
                hi = b;
            }
        }
        0.5 * (lo + hi)
    }
}

/// Tyre settings, shared by every wheel of a vehicle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TyreDesc {
    pub longitudinal: PacejkaAxis,
    pub lateral: PacejkaAxis,
    /// Peak friction coefficient `mu` at the nominal load on a reference surface (`0.05..=5`).
    pub friction: f32,
    /// Lateral peak as a fraction of the longitudinal peak (`0.2..=2`).
    pub lateral_scale: f32,
    /// Normal load in newtons at which `friction` applies (`1..=1e6`).
    pub nominal_load: f32,
    /// Fractional loss of `mu` per unit of extra relative load (`0..=1`): `mu = friction *
    /// (1 - sensitivity (load / nominal - 1))`, clamped to `0.25..=2` times `friction`.
    pub load_sensitivity: f32,
    /// Collider friction treated as the reference surface (`0.01..=100`). The grip of a surface
    /// scales with `collider friction / reference_surface_friction`.
    pub reference_surface_friction: f32,
    /// Speed in m/s below which slip is computed against this speed instead of the true one
    /// (`0.05..=10`); keeps slip finite and the force smooth near standstill.
    pub low_speed: f32,
}
impl Default for TyreDesc {
    fn default() -> Self {
        Self {
            longitudinal: PacejkaAxis {
                b: 10.,
                c: 1.35,
                e: 0.,
            },
            lateral: PacejkaAxis {
                b: 8.,
                c: 1.3,
                e: -0.5,
            },
            friction: 1.1,
            lateral_scale: 1.,
            nominal_load: 4000.,
            load_sensitivity: 0.1,
            reference_surface_friction: 0.5,
            low_speed: 1.,
        }
    }
}
impl TyreDesc {
    pub fn validate(self) -> Result<Self> {
        self.longitudinal.validate()?;
        self.lateral.validate()?;
        let ok = (0.05..=5.).contains(&self.friction)
            && (0.2..=2.).contains(&self.lateral_scale)
            && (1. ..=1e6).contains(&self.nominal_load)
            && (0. ..=1.).contains(&self.load_sensitivity)
            && (0.01..=100.).contains(&self.reference_surface_friction)
            && (0.05..=10.).contains(&self.low_speed);
        if !ok {
            return Err(Error("invalid tyre settings"));
        }
        Ok(self)
    }
    /// Peak longitudinal force (newtons) at `load` on a surface with grip `surface_scale`.
    pub fn peak_force(&self, load: f32, surface_scale: f32) -> f32 {
        let relative = load / self.nominal_load;
        let sensitivity = (1. - self.load_sensitivity * (relative - 1.)).clamp(0.25, 2.);
        self.friction * sensitivity * surface_scale * load.max(0.)
    }
    /// Longitudinal and lateral force plus the slips they came from.
    ///
    /// `forward` is the contact-patch speed along the wheel heading, `side` the speed along the
    /// axle (positive toward the axle direction), `surface` the tread speed `omega * radius`.
    pub fn forces(
        &self,
        load: f32,
        surface_scale: f32,
        forward: f32,
        side: f32,
        surface: f32,
    ) -> TyreForces {
        let reference = forward.abs().max(self.low_speed);
        let ratio = (surface - forward) / reference;
        let angle = side.atan2(reference);
        let dx = self.peak_force(load, surface_scale);
        let dy = dx * self.lateral_scale;
        if dx <= 0. {
            return TyreForces {
                longitudinal: 0.,
                lateral: 0.,
                slip_ratio: ratio,
                slip_angle: angle,
                peak_longitudinal: 0.,
                peak_lateral: 0.,
            };
        }
        let fx = self.longitudinal.curve(ratio);
        let fy = -self.lateral.curve(angle);
        // Friction ellipse: (Fx / Dx)^2 + (Fy / Dy)^2 <= 1.
        let usage = fx * fx + fy * fy;
        let scale = if usage > 1. { usage.sqrt().recip() } else { 1. };
        TyreForces {
            longitudinal: fx * scale * dx,
            lateral: fy * scale * dy,
            slip_ratio: ratio,
            slip_angle: angle,
            peak_longitudinal: dx,
            peak_lateral: dy,
        }
    }
}

/// Result of [`TyreDesc::forces`]. Forces are on the chassis, in newtons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TyreForces {
    /// Along the wheel heading (positive drives the vehicle forward).
    pub longitudinal: f32,
    /// Along the axle; opposes the sideways slip.
    pub lateral: f32,
    pub slip_ratio: f32,
    pub slip_angle: f32,
    /// Friction limits `Dx`, `Dy` used for the ellipse.
    pub peak_longitudinal: f32,
    pub peak_lateral: f32,
}
