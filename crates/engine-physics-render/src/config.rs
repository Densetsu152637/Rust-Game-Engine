//! Configuration of the physics render features: per-feature enable flags, caps and styling.

use anyhow::{Result, ensure};
use bevy_ecs::prelude::Resource;

/// Hard upper bounds; a config above them is rejected.
pub const MAX_DEBUG_SEGMENTS: usize = 1 << 20;
pub const MAX_SURFACE_VERTICES: usize = 1 << 21;
pub const MAX_FLUID_PARTICLES: usize = 1 << 21;
pub const MAX_FRAGMENTS: usize = 4096;

/// Which features are enabled in views without a per-camera override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicsRenderToggles {
    pub debug_lines: bool,
    pub cloth: bool,
    pub soft_bodies: bool,
    pub fluid: bool,
    pub fragments: bool,
}
impl Default for PhysicsRenderToggles {
    fn default() -> Self {
        Self {
            debug_lines: true,
            cloth: true,
            soft_bodies: true,
            fluid: true,
            fragments: true,
        }
    }
}

/// How cloth, soft-body and fragment surfaces are shaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceShading {
    /// The engine light list, shadows and ambient (`engine::lighting`) through the view bind
    /// group; the same shading the built-in lit material uses.
    #[default]
    Lit,
    /// No lights: vertex colour modulated by a fixed hemisphere term of the surface normal.
    NormalShaded,
}

/// What colours a fluid particle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FluidColorMode {
    /// `density / rest_density` from 0.5 (low colour) to 1.5 (high colour).
    #[default]
    Density,
    /// Speed from 0 to `FluidStyle::max_speed`.
    Speed,
    /// Every particle uses the low colour.
    Flat,
}

/// Particle styling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidStyle {
    pub mode: FluidColorMode,
    /// Colour at the low end of the ramp (linear RGBA).
    pub low: [f32; 4],
    /// Colour at the high end of the ramp.
    pub high: [f32; 4],
    /// Speed (m/s) mapped to the high colour in [`FluidColorMode::Speed`].
    pub max_speed: f32,
    /// 0..=1: how far surface particles move toward `surface_color`.
    pub surface_emphasis: f32,
    pub surface_color: [f32; 4],
    /// Impostor radius as a multiple of `FluidRender::particle_radius`.
    pub radius_scale: f32,
}
impl Default for FluidStyle {
    fn default() -> Self {
        Self {
            mode: FluidColorMode::Density,
            low: [0.05, 0.25, 0.8, 1.0],
            high: [0.1, 0.8, 0.9, 1.0],
            max_speed: 5.0,
            surface_emphasis: 0.6,
            surface_color: [0.9, 0.97, 1.0, 1.0],
            radius_scale: 1.0,
        }
    }
}

/// Everything the extraction and the features read. Insert it as a world resource before
/// [`crate::install_physics_extraction`] and pass the same value to
/// [`crate::register_physics_features_with`].
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct PhysicsRenderConfig {
    pub enabled: PhysicsRenderToggles,
    /// Test debug lines against scene depth (otherwise they draw over everything).
    pub debug_depth_test: bool,
    /// Debug line width in render pixels.
    pub debug_line_width: f32,
    /// Cap on debug segments extracted and drawn.
    pub max_debug_segments: usize,
    /// Cap on vertices uploaded per surface feature (cloth, soft bodies, fragments).
    pub max_surface_vertices: usize,
    /// Cap on fluid particles drawn.
    pub max_fluid_particles: usize,
    /// Cap on destruction fragments drawn.
    pub max_fragments: usize,
    pub shading: SurfaceShading,
    /// Perceptual roughness of lit surfaces (0..=1).
    pub roughness: f32,
    pub cloth_color: [f32; 4],
    pub soft_body_color: [f32; 4],
    /// Outer fragment faces.
    pub fragment_color: [f32; 4],
    /// Fracture (interior) faces.
    pub fragment_interior_color: [f32; 4],
    pub fluid: FluidStyle,
}
impl Default for PhysicsRenderConfig {
    fn default() -> Self {
        Self {
            enabled: PhysicsRenderToggles::default(),
            debug_depth_test: true,
            debug_line_width: 2.0,
            max_debug_segments: 65_536,
            max_surface_vertices: 262_144,
            max_fluid_particles: 131_072,
            max_fragments: 512,
            shading: SurfaceShading::Lit,
            roughness: 0.6,
            cloth_color: [0.3, 0.7, 0.95, 1.0],
            soft_body_color: [0.95, 0.6, 0.2, 1.0],
            fragment_color: [0.75, 0.75, 0.7, 1.0],
            fragment_interior_color: [0.9, 0.35, 0.25, 1.0],
            fluid: FluidStyle::default(),
        }
    }
}
fn color_ok(color: &[f32; 4]) -> bool {
    color
        .iter()
        .all(|c| c.is_finite() && (0.0..=64.0).contains(c))
}
impl PhysicsRenderConfig {
    /// Rejects non-finite or out-of-range values and zero or oversized caps.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_DEBUG_SEGMENTS).contains(&self.max_debug_segments),
            "max_debug_segments must be 1..={MAX_DEBUG_SEGMENTS}"
        );
        ensure!(
            (3..=MAX_SURFACE_VERTICES).contains(&self.max_surface_vertices),
            "max_surface_vertices must be 3..={MAX_SURFACE_VERTICES}"
        );
        ensure!(
            (1..=MAX_FLUID_PARTICLES).contains(&self.max_fluid_particles),
            "max_fluid_particles must be 1..={MAX_FLUID_PARTICLES}"
        );
        ensure!(
            (1..=MAX_FRAGMENTS).contains(&self.max_fragments),
            "max_fragments must be 1..={MAX_FRAGMENTS}"
        );
        ensure!(
            self.debug_line_width.is_finite() && (0.25..=32.0).contains(&self.debug_line_width),
            "debug_line_width must be 0.25..=32 pixels"
        );
        ensure!(
            self.roughness.is_finite() && (0.0..=1.0).contains(&self.roughness),
            "roughness must be 0..=1"
        );
        for (name, color) in [
            ("cloth_color", &self.cloth_color),
            ("soft_body_color", &self.soft_body_color),
            ("fragment_color", &self.fragment_color),
            ("fragment_interior_color", &self.fragment_interior_color),
            ("fluid.low", &self.fluid.low),
            ("fluid.high", &self.fluid.high),
            ("fluid.surface_color", &self.fluid.surface_color),
        ] {
            ensure!(color_ok(color), "{name} must be finite RGBA in 0..=64");
        }
        ensure!(
            self.fluid.max_speed.is_finite() && self.fluid.max_speed > 0.0,
            "fluid.max_speed must be positive"
        );
        ensure!(
            self.fluid.surface_emphasis.is_finite()
                && (0.0..=1.0).contains(&self.fluid.surface_emphasis),
            "fluid.surface_emphasis must be 0..=1"
        );
        ensure!(
            self.fluid.radius_scale.is_finite() && (0.05..=8.0).contains(&self.fluid.radius_scale),
            "fluid.radius_scale must be 0.05..=8"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_is_valid_and_bad_values_are_named() {
        PhysicsRenderConfig::default().validate().unwrap();
        let config = PhysicsRenderConfig {
            max_debug_segments: 0,
            ..Default::default()
        };
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("max_debug_segments")
        );
        let mut config = PhysicsRenderConfig::default();
        config.fluid.low[1] = f32::NAN;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("fluid.low")
        );
        let config = PhysicsRenderConfig {
            debug_line_width: 100.0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
