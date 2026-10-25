//! Resolved per-view HDR post-processing settings (plain data, no serde).
//!
//! The host fills [`PostSettings`] on [`crate::ViewData::post`]; a view with
//! `Some(settings)` runs `engine.post` (PostProcess stage) and
//! `engine.tonemap` (Tonemap stage) instead of the default clamp output.

use super::lut::Lut3d;
use anyhow::{Result, ensure};
use std::sync::Arc;

/// Largest bloom mip chain length.
pub const MAX_BLOOM_MIPS: u32 = 8;
/// Auto-exposure histogram bins.
pub const HISTOGRAM_BINS: u32 = 64;

/// The display transform of the Tonemap stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TonemapOperator {
    /// Linear clamp to 0..1: the engine's default appearance.
    #[default]
    None,
    /// Narkowicz's ACES filmic fit, per channel.
    Aces,
    /// AgX-style sigmoid on Rec.2020 primaries, base look.
    Agx,
    /// AgX with a punchier look (power 1.35, saturation 1.4).
    AgxPunchy,
    /// Khronos PBR Neutral.
    Neutral,
}

impl TonemapOperator {
    pub(crate) fn code(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Aces => 1.0,
            Self::Agx => 2.0,
            Self::AgxPunchy => 3.0,
            Self::Neutral => 4.0,
        }
    }
}

/// How the scene is scaled before grading and tonemapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExposureMode {
    /// No post exposure: only `Camera3D.lighting.exposure` applies.
    #[default]
    Off,
    /// `2^(compensation - ev100)`.
    Manual,
    /// Histogram-driven, adapted over time.
    Auto,
}

/// Exposure controls. See docs/rendering-post.md for the precedence rule.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposureSettings {
    pub mode: ExposureMode,
    /// Manual exposure value: the scene is scaled by `2^-ev100`.
    pub ev100: f32,
    /// Stops added in manual and auto modes.
    pub compensation: f32,
    /// Auto: log2 luminance range of the histogram.
    pub min_ev: f32,
    pub max_ev: f32,
    /// Auto: the histogram mass between these fractions is averaged.
    pub low_percentile: f32,
    pub high_percentile: f32,
    /// Auto: adaptation rate (1/s) when the exposure scale rises (the scene
    /// got darker) and when it falls (the scene got brighter).
    pub speed_up: f32,
    pub speed_down: f32,
}

impl Default for ExposureSettings {
    fn default() -> Self {
        Self {
            mode: ExposureMode::Off,
            ev100: 0.0,
            compensation: 0.0,
            min_ev: -8.0,
            max_ev: 6.0,
            low_percentile: 0.1,
            high_percentile: 0.9,
            speed_up: 3.0,
            speed_down: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BloomSettings {
    pub enabled: bool,
    /// Luminance where bloom starts (after exposure).
    pub threshold: f32,
    /// Soft knee width 0..1 relative to the threshold.
    pub knee: f32,
    /// Weight of the blurred result added to the scene.
    pub intensity: f32,
    /// 0..1: how much each coarser mip contributes (larger is wider).
    pub scatter: f32,
    pub tint: [f32; 3],
    /// Mip chain length, 1..=MAX_BLOOM_MIPS.
    pub mips: u32,
}

impl Default for BloomSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 1.0,
            knee: 0.5,
            intensity: 0.3,
            scatter: 0.7,
            tint: [1.0; 3],
            mips: 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradingSettings {
    pub enabled: bool,
    /// -1 (cool) .. 1 (warm).
    pub temperature: f32,
    /// -1 (green) .. 1 (magenta).
    pub tint: f32,
    /// Stops added before the other grading steps.
    pub exposure_offset: f32,
    /// Power about the mid-grey pivot 0.18; 1 is neutral.
    pub contrast: f32,
    /// 0 grey, 1 neutral.
    pub saturation: f32,
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
    pub hue_shift_degrees: f32,
    /// Optional 3D LUT, blended in by `lut_intensity`.
    pub lut: Option<Arc<Lut3d>>,
    pub lut_intensity: f32,
}

impl Default for GradingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            temperature: 0.0,
            tint: 0.0,
            exposure_offset: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
            hue_shift_degrees: 0.0,
            lut: None,
            lut_intensity: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VignetteSettings {
    pub enabled: bool,
    /// 0..1 darkening at the corners.
    pub intensity: f32,
    /// 0.01..1: width of the falloff.
    pub smoothness: f32,
    /// Centre in 0..1 view coordinates.
    pub center: [f32; 2],
    pub color: [f32; 3],
}

impl Default for VignetteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.4,
            smoothness: 0.5,
            center: [0.5, 0.5],
            color: [0.0; 3],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrainSettings {
    pub enabled: bool,
    /// Peak amplitude of the zero-mean noise, 0..1.
    pub intensity: f32,
    /// 0..1: how much darker pixels get more grain.
    pub response: f32,
}

impl Default for GrainSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.1,
            response: 0.8,
        }
    }
}

/// Everything the post stack needs for one view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PostSettings {
    pub tonemap: TonemapOperator,
    pub exposure: ExposureSettings,
    pub bloom: BloomSettings,
    pub grading: GradingSettings,
    pub vignette: VignetteSettings,
    pub grain: GrainSettings,
}

fn finite_in(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}

impl PostSettings {
    /// Reject non-finite or out-of-range values before anything is encoded.
    pub fn validate(&self) -> Result<()> {
        let e = &self.exposure;
        ensure!(
            finite_in(e.ev100, -30.0, 30.0) && finite_in(e.compensation, -16.0, 16.0),
            "exposure ev100 and compensation must be finite and within +-30 / +-16"
        );
        ensure!(
            finite_in(e.min_ev, -16.0, 16.0)
                && finite_in(e.max_ev, -16.0, 16.0)
                && e.min_ev + 0.5 <= e.max_ev,
            "auto exposure needs finite min_ev + 0.5 <= max_ev within +-16"
        );
        ensure!(
            finite_in(e.low_percentile, 0.0, 1.0)
                && finite_in(e.high_percentile, 0.0, 1.0)
                && e.low_percentile < e.high_percentile,
            "auto exposure needs 0 <= low_percentile < high_percentile <= 1"
        );
        ensure!(
            finite_in(e.speed_up, 0.0, 1000.0) && finite_in(e.speed_down, 0.0, 1000.0),
            "auto exposure speeds must be finite and within 0..1000"
        );
        let b = &self.bloom;
        ensure!(
            finite_in(b.threshold, 0.0, 1000.0)
                && finite_in(b.knee, 0.0, 1.0)
                && finite_in(b.intensity, 0.0, 100.0)
                && finite_in(b.scatter, 0.0, 1.0)
                && b.tint.iter().all(|t| finite_in(*t, 0.0, 100.0))
                && (1..=MAX_BLOOM_MIPS).contains(&b.mips),
            "bloom values out of range"
        );
        let g = &self.grading;
        ensure!(
            finite_in(g.temperature, -1.0, 1.0)
                && finite_in(g.tint, -1.0, 1.0)
                && finite_in(g.exposure_offset, -16.0, 16.0)
                && finite_in(g.contrast, 0.0, 4.0)
                && finite_in(g.saturation, 0.0, 4.0)
                && g.lift.iter().all(|v| finite_in(*v, -1.0, 1.0))
                && g.gamma.iter().all(|v| finite_in(*v, 0.1, 4.0))
                && g.gain.iter().all(|v| finite_in(*v, 0.0, 4.0))
                && finite_in(g.hue_shift_degrees, -360.0, 360.0)
                && finite_in(g.lut_intensity, 0.0, 1.0),
            "grading values out of range"
        );
        let v = &self.vignette;
        ensure!(
            finite_in(v.intensity, 0.0, 1.0)
                && finite_in(v.smoothness, 0.01, 1.0)
                && v.center.iter().all(|c| finite_in(*c, 0.0, 1.0))
                && v.color.iter().all(|c| finite_in(*c, 0.0, 1.0)),
            "vignette values out of range"
        );
        ensure!(
            finite_in(self.grain.intensity, 0.0, 1.0) && finite_in(self.grain.response, 0.0, 1.0),
            "grain values out of range"
        );
        Ok(())
    }

    /// The exposure scale of the non-auto modes.
    pub fn manual_scale(&self) -> f32 {
        match self.exposure.mode {
            ExposureMode::Off => 1.0,
            _ => (self.exposure.compensation - self.exposure.ev100).exp2(),
        }
    }

    /// Whether the PostProcess-stage work changes the image at all.
    pub fn needs_post_pass(&self) -> bool {
        self.exposure.mode != ExposureMode::Off || self.bloom.enabled || self.grading.enabled
    }
}

/// White-balance multipliers for a temperature/tint pair, normalised to keep
/// the luminance of white. A simple linear model, not a chromatic adaptation
/// transform.
pub fn white_balance(temperature: f32, tint: f32) -> [f32; 3] {
    let raw = [
        1.0 + 0.4 * temperature,
        1.0 - 0.4 * tint,
        1.0 - 0.4 * temperature,
    ];
    let luma = 0.2126 * raw[0] + 0.7152 * raw[1] + 0.0722 * raw[2];
    [raw[0] / luma, raw[1] / luma, raw[2] / luma]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid_and_neutral() {
        let s = PostSettings::default();
        s.validate().unwrap();
        assert!(!s.needs_post_pass());
        assert_eq!(s.manual_scale(), 1.0);
    }

    #[test]
    fn manual_scale_follows_ev_and_compensation() {
        let mut s = PostSettings::default();
        s.exposure.mode = ExposureMode::Manual;
        s.exposure.ev100 = 2.0;
        assert!((s.manual_scale() - 0.25).abs() < 1e-6);
        s.exposure.compensation = 1.0;
        assert!((s.manual_scale() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_values() {
        let mut s = PostSettings::default();
        s.bloom.scatter = 2.0;
        assert!(s.validate().is_err());
        let mut s = PostSettings::default();
        s.exposure.low_percentile = 0.9;
        assert!(s.validate().is_err());
        let mut s = PostSettings::default();
        s.vignette.intensity = f32::NAN;
        assert!(s.validate().is_err());
    }

    #[test]
    fn neutral_white_balance_is_identity() {
        let w = white_balance(0.0, 0.0);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 1e-6));
        let warm = white_balance(0.5, 0.0);
        assert!(warm[0] > warm[2]);
    }
}
