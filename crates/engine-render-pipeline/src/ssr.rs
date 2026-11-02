//! Screen-space reflection settings (`engine.ssr`, docs/rendering-ssr.md).

use anyhow::{Result, ensure};

/// Most trace steps per pixel.
pub const MAX_SSR_STEPS: u32 = 256;
/// Largest ray length in view-space units.
pub const MAX_SSR_DISTANCE: f32 = 10_000.0;
/// Largest depth thickness in view-space units.
pub const MAX_SSR_THICKNESS: f32 = 100.0;
/// Largest reflection intensity multiplier.
pub const MAX_SSR_INTENSITY: f32 = 4.0;

/// Per-view controls of the screen-space reflection pass.
///
/// The pass traces the mirror direction of every reflective built-in lit
/// pixel through the current frame's depth, with at most `max_steps` samples
/// along the projected ray. A hit blends the opaque scene colour found there
/// over the pixel's image-based specular; a miss or a faded-out hit leaves
/// the opaque pass's result unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SsrSettings {
    /// Samples along the projected ray, `1..=256`. When the ray covers more
    /// screen pixels than steps the stride grows, so thin occluders can be
    /// missed; raising the count costs time linearly.
    pub max_steps: u32,
    /// Longest ray in view-space units, `(0, 10000]`. Hits fade out over the
    /// last quarter of this length.
    pub max_distance: f32,
    /// How far behind a depth sample the ray may be and still hit it, in
    /// view-space units, `(0, 100]`.
    pub thickness: f32,
    /// Perceptual roughness above which a pixel is not reflected, `(0, 1]`.
    /// Pixels fade out between half the cutoff and the cutoff.
    pub roughness_cutoff: f32,
    /// Width of the screen-edge fade of the hit position as a fraction of the
    /// screen, `[0, 0.5]`. Zero disables the fade (hits at the very border
    /// are then cut hard).
    pub edge_fade: f32,
    /// Multiplier of the traced radiance, `[0, 4]`. Zero skips the pass.
    pub intensity: f32,
}

impl Default for SsrSettings {
    fn default() -> Self {
        Self {
            max_steps: 64,
            max_distance: 30.0,
            thickness: 0.5,
            roughness_cutoff: 0.4,
            edge_fade: 0.1,
            intensity: 1.0,
        }
    }
}

impl SsrSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_SSR_STEPS).contains(&self.max_steps),
            "SSR max_steps must be within 1..={MAX_SSR_STEPS}"
        );
        ensure!(
            self.max_distance.is_finite()
                && self.max_distance > 0.0
                && self.max_distance <= MAX_SSR_DISTANCE,
            "SSR max_distance must be finite within (0, {MAX_SSR_DISTANCE}]"
        );
        ensure!(
            self.thickness.is_finite()
                && self.thickness > 0.0
                && self.thickness <= MAX_SSR_THICKNESS,
            "SSR thickness must be finite within (0, {MAX_SSR_THICKNESS}]"
        );
        ensure!(
            self.roughness_cutoff.is_finite()
                && self.roughness_cutoff > 0.0
                && self.roughness_cutoff <= 1.0,
            "SSR roughness_cutoff must be finite within (0, 1]"
        );
        ensure!(
            self.edge_fade.is_finite() && (0.0..=0.5).contains(&self.edge_fade),
            "SSR edge_fade must be finite within [0, 0.5]"
        );
        ensure!(
            self.intensity.is_finite() && (0.0..=MAX_SSR_INTENSITY).contains(&self.intensity),
            "SSR intensity must be finite within [0, {MAX_SSR_INTENSITY}]"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        SsrSettings::default().validate().unwrap();
    }

    #[test]
    fn out_of_range_and_non_finite_values_are_rejected() {
        let ok = SsrSettings::default();
        let bad = [
            SsrSettings { max_steps: 0, ..ok },
            SsrSettings {
                max_steps: MAX_SSR_STEPS + 1,
                ..ok
            },
            SsrSettings {
                max_distance: 0.0,
                ..ok
            },
            SsrSettings {
                max_distance: f32::NAN,
                ..ok
            },
            SsrSettings {
                max_distance: MAX_SSR_DISTANCE * 2.0,
                ..ok
            },
            SsrSettings {
                thickness: -1.0,
                ..ok
            },
            SsrSettings {
                thickness: f32::INFINITY,
                ..ok
            },
            SsrSettings {
                roughness_cutoff: 0.0,
                ..ok
            },
            SsrSettings {
                roughness_cutoff: 1.5,
                ..ok
            },
            SsrSettings {
                edge_fade: 0.6,
                ..ok
            },
            SsrSettings {
                edge_fade: f32::NAN,
                ..ok
            },
            SsrSettings {
                intensity: -0.1,
                ..ok
            },
            SsrSettings {
                intensity: 5.0,
                ..ok
            },
        ];
        for settings in bad {
            assert!(settings.validate().is_err(), "{settings:?}");
        }
        for settings in [
            SsrSettings { max_steps: 1, ..ok },
            SsrSettings {
                max_steps: MAX_SSR_STEPS,
                edge_fade: 0.0,
                intensity: 0.0,
                roughness_cutoff: 1.0,
                ..ok
            },
        ] {
            settings.validate().unwrap();
        }
    }
}
