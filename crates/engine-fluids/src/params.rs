//! Solver parameters and the box domain, with boundary validation.

use crate::{Error, MAX_ITERATIONS, MAX_SUBSTEPS, PARTICLE_RADIUS_RANGE, Result};
use glam::Vec3;

/// Largest accepted coordinate magnitude for domain bounds, in meters.
pub(crate) const MAX_COORDINATE: f32 = 1.0e6;

/// Position Based Fluids parameters. All values are validated by [`FluidParams::validate`]
/// before a volume accepts them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidParams {
    /// Collision radius of one particle in meters. The rest spacing is `2 * particle_radius`.
    pub particle_radius: f32,
    /// Smoothing (kernel) radius as a multiple of the rest spacing, `1.5..=3.0`.
    pub kernel_scale: f32,
    /// Rest density in kg/m^3 (water: 1000).
    pub rest_density: f32,
    /// Gravity acceleration in m/s^2.
    pub gravity: Vec3,
    /// Fixed substeps per step, `1..=MAX_SUBSTEPS`.
    pub substeps: u32,
    /// Density-constraint iterations per substep, `1..=MAX_ITERATIONS`.
    pub iterations: u32,
    /// Constraint relaxation (CFM) relative to the rest-lattice gradient norm, `1e-6..=10`.
    /// Larger values soften the density constraint and damp the Jacobi solve.
    pub relaxation: f32,
    /// Tensile-instability correction strength `k`, `0..=1` (0 disables it).
    pub tensile_k: f32,
    /// Tensile correction reference distance as a fraction of the kernel radius, `0.05..=0.5`.
    pub tensile_dq: f32,
    /// XSPH viscosity blend per substep, `0..=1`.
    pub viscosity: f32,
    /// Vorticity confinement strength in m/s, `0..=100` (0 disables it).
    pub vorticity: f32,
    /// Particle speed clamp in m/s, `(0, 1000]`.
    pub max_speed: f32,
    /// Colour-field gradient magnitude (scaled by the kernel radius) above which a particle is
    /// flagged as surface, `(0, 10]`.
    pub surface_threshold: f32,
}

impl Default for FluidParams {
    fn default() -> Self {
        Self {
            particle_radius: 0.025,
            kernel_scale: 2.0,
            rest_density: 1000.0,
            gravity: Vec3::new(0.0, -9.81, 0.0),
            substeps: 2,
            iterations: 4,
            relaxation: 0.1,
            tensile_k: 0.1,
            tensile_dq: 0.2,
            viscosity: 0.01,
            vorticity: 0.0,
            max_speed: 50.0,
            surface_threshold: 0.6,
        }
    }
}

fn in_range(value: f32, min: f32, max: f32, what: &'static str) -> Result<()> {
    if value.is_finite() && value >= min && value <= max {
        Ok(())
    } else {
        Err(Error::Invalid(what))
    }
}

impl FluidParams {
    /// Rest spacing between particle centres, `2 * particle_radius`.
    pub fn spacing(&self) -> f32 {
        2.0 * self.particle_radius
    }

    /// Kernel (smoothing) radius in meters.
    pub fn kernel_radius(&self) -> f32 {
        self.spacing() * self.kernel_scale
    }

    pub fn validate(&self) -> Result<()> {
        let (min_r, max_r) = PARTICLE_RADIUS_RANGE;
        in_range(
            self.particle_radius,
            min_r,
            max_r,
            "particle radius must be finite and within PARTICLE_RADIUS_RANGE",
        )?;
        in_range(
            self.kernel_scale,
            1.5,
            3.0,
            "kernel scale must be finite and within 1.5..=3.0",
        )?;
        if !(self.rest_density.is_finite() && self.rest_density > 0.0) {
            return Err(Error::Invalid("rest density must be finite and positive"));
        }
        in_range(
            self.rest_density,
            1.0e-3,
            1.0e5,
            "rest density must be within 1e-3..=1e5 kg/m^3",
        )?;
        if !self.gravity.is_finite() || self.gravity.length() > 1000.0 {
            return Err(Error::Invalid(
                "gravity must be finite and at most 1000 m/s^2",
            ));
        }
        if !(1..=MAX_SUBSTEPS).contains(&self.substeps) {
            return Err(Error::Invalid("substeps must be within 1..=MAX_SUBSTEPS"));
        }
        if !(1..=MAX_ITERATIONS).contains(&self.iterations) {
            return Err(Error::Invalid(
                "iterations must be within 1..=MAX_ITERATIONS",
            ));
        }
        in_range(
            self.relaxation,
            1.0e-6,
            10.0,
            "relaxation must be finite and within 1e-6..=10",
        )?;
        in_range(
            self.tensile_k,
            0.0,
            1.0,
            "tensile k must be finite and within 0..=1",
        )?;
        in_range(
            self.tensile_dq,
            0.05,
            0.5,
            "tensile dq must be finite and within 0.05..=0.5",
        )?;
        in_range(
            self.viscosity,
            0.0,
            1.0,
            "viscosity must be finite and within 0..=1",
        )?;
        in_range(
            self.vorticity,
            0.0,
            100.0,
            "vorticity must be finite and within 0..=100",
        )?;
        if !(self.max_speed.is_finite() && self.max_speed > 0.0 && self.max_speed <= 1000.0) {
            return Err(Error::Invalid(
                "max speed must be finite and within (0, 1000]",
            ));
        }
        if !(self.surface_threshold.is_finite()
            && self.surface_threshold > 0.0
            && self.surface_threshold <= 10.0)
        {
            return Err(Error::Invalid(
                "surface threshold must be finite and within (0, 10]",
            ));
        }
        Ok(())
    }
}

/// Closed axis-aligned box that contains every particle. Walls keep particle centres at least
/// one particle radius inside, add boundary density, and apply restitution and friction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Domain {
    pub min: Vec3,
    pub max: Vec3,
    /// Normal restitution of wall contacts, `0..=1`.
    pub restitution: f32,
    /// Coulomb friction coefficient of wall contacts, `0..=10`.
    pub friction: f32,
}

impl Domain {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self {
            min,
            max,
            restitution: 0.0,
            friction: 0.1,
        }
    }

    /// Validate against the particle spacing: every axis must hold at least two particles.
    pub fn validate(&self, params: &FluidParams) -> Result<()> {
        if !self.min.is_finite() || !self.max.is_finite() {
            return Err(Error::Invalid("domain bounds must be finite"));
        }
        if self.min.abs().max_element() > MAX_COORDINATE
            || self.max.abs().max_element() > MAX_COORDINATE
        {
            return Err(Error::Invalid(
                "domain bounds must be within 1e6 m of the origin",
            ));
        }
        if (self.max - self.min).min_element() < 2.0 * params.spacing() {
            return Err(Error::Invalid(
                "domain must be at least two particle spacings wide on every axis",
            ));
        }
        in_range(
            self.restitution,
            0.0,
            1.0,
            "domain restitution must be finite and within 0..=1",
        )?;
        in_range(
            self.friction,
            0.0,
            10.0,
            "domain friction must be finite and within 0..=10",
        )
    }

    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}
