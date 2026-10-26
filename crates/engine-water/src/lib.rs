//! Surface water model (pure Rust: no GPU, ECS or file-system access).
//!
//! * [`gerstner`]: Gerstner wave sets with bounded steepness (no loops), displacement,
//!   normals, orbital velocity and the dispersion relation.
//! * [`spectrum`] + [`ocean`]: a seeded, tileable Tessendorf FFT ocean over the Phillips or
//!   JONSWAP spectrum with choppiness, time evolution and CPU sampling.
//! * [`fft`]: the radix-2 complex FFT the ocean uses.
//! * [`displace`]: the bounded fixed-point inversion of horizontal displacement that turns a
//!   displaced surface into a height query at a world position.
//! * [`river`]: spline rivers with width, depth and a mass-conserving flow field.
//! * [`surface`]: water bodies (ocean, lake, river) as queryable surfaces, optics, underwater
//!   fog, and shoreline attenuation over a [`GroundHeight`] sampler.
//! * [`buoyancy`]: buoyancy and drag impulses for a body sampled at points.
//!
//! See docs/world-water.md for the formulas, bounds and how `engine-project` and
//! `engine-world-render` use the model.
pub mod buoyancy;
pub mod displace;
mod error;
pub mod fft;
pub mod gerstner;
pub mod ocean;
pub mod river;
mod rng;
pub mod spectrum;
pub mod surface;

pub use buoyancy::{
    BodyMotion, BuoyancyImpulse, BuoyancyPoint, DragParams, WaterAt, box_points, buoyancy_impulse,
    submerged_fraction,
};
pub use displace::{INVERSE_MAX_ITERATIONS, INVERSE_TOLERANCE, Inversion, invert_horizontal};
pub use error::{Error, Result};
pub use fft::Complex;
pub use gerstner::{
    GerstnerSet, GerstnerWave, MAX_AMPLITUDE_RATIO, MAX_GERSTNER_WAVES, MAX_STEEPNESS_SUM,
    WavePoint, angular_frequency,
};
pub use ocean::{OceanFrame, OceanPoint, OceanSpectrum, OceanTexel, WaveStats};
pub use river::{MAX_RIVER_POINTS, RiverPoint, RiverSettings, RiverSpline};
pub use spectrum::{OCEAN_RESOLUTIONS, OceanSettings, SpectrumKind};
pub use surface::{
    Attenuation, GroundHeight, NoGround, ShorelineSettings, SurfaceSample, Underwater, WaterOptics,
    WaterShape, WaterSurface, Waves,
};

/// Standard gravity (m/s^2).
pub const GRAVITY: f32 = 9.81;
