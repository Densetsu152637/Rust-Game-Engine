//! Authored settings of the RT lighting effects and the denoiser.
//!
//! Plain serde data (snake_case fields, unknown fields rejected, every field
//! optional with the documented default) meant to become authored component
//! data. `validate` checks every range; kernels call it on every dispatch, so
//! invalid settings never reach the GPU. [`RtEffectsSettings::status`] reports
//! honestly which effects the ray-traced path would run for a device status.

use crate::error::{RtError, invalid};
use crate::status::{RayTracingStatus, RtEffect, RtEffectMode, RtEffectStatus, RtEffectsStatus};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// Largest sun angular radius (radians, about 11.5 degrees).
pub const MAX_SUN_ANGULAR_RADIUS: f32 = 0.2;
/// Largest per-pixel ray or sample count of one dispatch.
pub const MAX_RAYS_PER_PIXEL: u32 = 64;
/// Largest ray distance in world units.
pub const MAX_RAY_DISTANCE: f32 = 1.0e6;
/// Largest AO radius in world units.
pub const MAX_AO_RADIUS: f32 = 100.0;
/// Largest normal bias in world units.
pub const MAX_NORMAL_BIAS: f32 = 1.0;
/// Largest diffuse GI path length.
pub const MAX_GI_BOUNCES: u32 = 8;
/// Largest number of a-trous iterations (step widths 1 .. 128 px).
pub const MAX_ATROUS_ITERATIONS: u32 = 8;
/// Largest temporal history length in frames.
pub const MAX_HISTORY_LENGTH: u32 = 256;

fn check_f32(
    field: &str,
    value: f32,
    range: RangeInclusive<f32>,
    exclusive_min: bool,
) -> Result<(), RtError> {
    let ok =
        value.is_finite() && range.contains(&value) && !(exclusive_min && value == *range.start());
    if ok {
        return Ok(());
    }
    let open = if exclusive_min { "(" } else { "[" };
    Err(invalid(format!(
        "{field} = {value} must be within {open}{}, {}]",
        range.start(),
        range.end()
    )))
}

fn check_u32(field: &str, value: u32, range: RangeInclusive<u32>) -> Result<(), RtError> {
    if range.contains(&value) {
        return Ok(());
    }
    Err(invalid(format!(
        "{field} = {value} must be within [{}, {}]",
        range.start(),
        range.end()
    )))
}

/// RT sun shadows ([`crate::ShadowMaskKernel`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtShadowSettings {
    /// Default `false`.
    pub enabled: bool,
    /// Angular radius of the sun disc in radians, `[0, 0.2]`. `0` (default)
    /// traces one hard-shadow ray; above `0` the mask is soft (the real sun is
    /// about 0.00465).
    pub sun_angular_radius: f32,
    /// Shadow rays per pixel for soft shadows, `[1, 64]`, default 1.
    pub samples_per_pixel: u32,
    /// Ray-origin offset along the normal in world units, `[0, 1]`, default 0.001.
    pub normal_bias: f32,
    /// Shadow ray length in world units, `(0, 1e6]`, default 10000.
    pub max_distance: f32,
    /// Denoise soft shadows with the spatio-temporal denoiser (default `true`;
    /// hard shadows are exact and never denoised).
    pub denoise: bool,
}

impl Default for RtShadowSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            sun_angular_radius: 0.0,
            samples_per_pixel: 1,
            normal_bias: 1e-3,
            max_distance: 1e4,
            denoise: true,
        }
    }
}

impl RtShadowSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        check_f32(
            "shadows.sun_angular_radius",
            self.sun_angular_radius,
            0.0..=MAX_SUN_ANGULAR_RADIUS,
            false,
        )?;
        check_u32(
            "shadows.samples_per_pixel",
            self.samples_per_pixel,
            1..=MAX_RAYS_PER_PIXEL,
        )?;
        check_f32(
            "shadows.normal_bias",
            self.normal_bias,
            0.0..=MAX_NORMAL_BIAS,
            false,
        )?;
        check_f32(
            "shadows.max_distance",
            self.max_distance,
            0.0..=MAX_RAY_DISTANCE,
            true,
        )
    }

    /// `true` when the mask is soft (and so worth denoising).
    pub fn is_soft(&self) -> bool {
        self.sun_angular_radius > 0.0
    }
}

/// RT ambient occlusion ([`crate::AmbientOcclusionKernel`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtAoSettings {
    /// Default `false`.
    pub enabled: bool,
    /// Occlusion radius (ray length) in world units, `(0, 100]`, default 1.
    pub radius: f32,
    /// Cosine-weighted rays per pixel, `[1, 64]`, default 1.
    pub rays_per_pixel: u32,
    /// Ray-origin offset along the normal in world units, `[0, 1]`, default 0.001.
    pub normal_bias: f32,
    /// Denoise with the spatio-temporal denoiser (default `true`).
    pub denoise: bool,
}

impl Default for RtAoSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            radius: 1.0,
            rays_per_pixel: 1,
            normal_bias: 1e-3,
            denoise: true,
        }
    }
}

impl RtAoSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        check_f32(
            "ambient_occlusion.radius",
            self.radius,
            0.0..=MAX_AO_RADIUS,
            true,
        )?;
        check_u32(
            "ambient_occlusion.rays_per_pixel",
            self.rays_per_pixel,
            1..=MAX_RAYS_PER_PIXEL,
        )?;
        check_f32(
            "ambient_occlusion.normal_bias",
            self.normal_bias,
            0.0..=MAX_NORMAL_BIAS,
            false,
        )
    }
}

/// RT reflections ([`crate::ReflectionKernel`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtReflectionSettings {
    /// Default `false`.
    pub enabled: bool,
    /// Pixels whose perceptual roughness is above this, `[0, 1]`, are not
    /// traced and are flagged for the raster fallback. Default 0.5.
    pub roughness_cutoff: f32,
    /// Reflection and sun-visibility ray length in world units, `(0, 1e6]`,
    /// default 1000.
    pub max_distance: f32,
    /// Ray-origin offset along the normal in world units, `[0, 1]`, default 0.001.
    pub normal_bias: f32,
    /// Denoise with the spatio-temporal denoiser (default `true`).
    pub denoise: bool,
}

impl Default for RtReflectionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            roughness_cutoff: 0.5,
            max_distance: 1000.0,
            normal_bias: 1e-3,
            denoise: true,
        }
    }
}

impl RtReflectionSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        check_f32(
            "reflections.roughness_cutoff",
            self.roughness_cutoff,
            0.0..=1.0,
            false,
        )?;
        check_f32(
            "reflections.max_distance",
            self.max_distance,
            0.0..=MAX_RAY_DISTANCE,
            true,
        )?;
        check_f32(
            "reflections.normal_bias",
            self.normal_bias,
            0.0..=MAX_NORMAL_BIAS,
            false,
        )
    }
}

/// RT diffuse global illumination ([`crate::DiffuseGiKernel`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtGiSettings {
    /// Default `false`.
    pub enabled: bool,
    /// Path segments traced from the surface, `[1, 8]`, default 1
    /// (one-bounce GI: emission, sun and sky seen from the surface).
    pub bounces: u32,
    /// Cosine-weighted paths per pixel, `[1, 64]`, default 1.
    pub rays_per_pixel: u32,
    /// Ray length in world units, `(0, 1e6]`, default 1000.
    pub max_distance: f32,
    /// Ray-origin offset along the normal in world units, `[0, 1]`, default 0.001.
    pub normal_bias: f32,
    /// Denoise with the spatio-temporal denoiser (default `true`).
    pub denoise: bool,
}

impl Default for RtGiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bounces: 1,
            rays_per_pixel: 1,
            max_distance: 1000.0,
            normal_bias: 1e-3,
            denoise: true,
        }
    }
}

impl RtGiSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        check_u32("diffuse_gi.bounces", self.bounces, 1..=MAX_GI_BOUNCES)?;
        check_u32(
            "diffuse_gi.rays_per_pixel",
            self.rays_per_pixel,
            1..=MAX_RAYS_PER_PIXEL,
        )?;
        check_f32(
            "diffuse_gi.max_distance",
            self.max_distance,
            0.0..=MAX_RAY_DISTANCE,
            true,
        )?;
        check_f32(
            "diffuse_gi.normal_bias",
            self.normal_bias,
            0.0..=MAX_NORMAL_BIAS,
            false,
        )
    }
}

/// The spatio-temporal (SVGF-style) denoiser ([`crate::Denoiser`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DenoiserSettings {
    /// Edge-aware a-trous wavelet iterations, `[0, 8]`, default 5 (step widths
    /// 1, 2, 4, 8, 16 px). `0` keeps only temporal accumulation.
    pub atrous_iterations: u32,
    /// Most frames the temporal history averages, `[1, 256]`, default 32
    /// (the blend factor never drops below `1 / max_history_length`; 1 disables
    /// temporal accumulation).
    pub max_history_length: u32,
    /// History is rejected when the previous view distance differs from the
    /// current one by more than this fraction, `(0, 1]`, default 0.1.
    pub reprojection_depth_tolerance: f32,
    /// History is rejected when the cosine between the previous and current
    /// normals is below this, `[-1, 1]`, default 0.9.
    pub reprojection_normal_threshold: f32,
    /// Depth edge stop: plane distance between taps, relative to the centre's
    /// view distance, `(0, 10]`, default 0.05.
    pub phi_depth: f32,
    /// Normal edge stop exponent on `max(0, dot(n_p, n_q))`, `[1, 1024]`, default 128.
    pub phi_normal: f32,
    /// Luminance edge stop in units of the local standard deviation,
    /// `(0, 1000]`, default 4.
    pub phi_luminance: f32,
    /// Below this history length (frames) the variance is estimated spatially
    /// from a 7 x 7 neighbourhood instead of the temporal moments, `[1, 32]`,
    /// default 4.
    pub min_history_for_temporal_variance: u32,
}

impl Default for DenoiserSettings {
    fn default() -> Self {
        Self {
            atrous_iterations: 5,
            max_history_length: 32,
            reprojection_depth_tolerance: 0.1,
            reprojection_normal_threshold: 0.9,
            phi_depth: 0.05,
            phi_normal: 128.0,
            phi_luminance: 4.0,
            min_history_for_temporal_variance: 4,
        }
    }
}

impl DenoiserSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        check_u32(
            "denoiser.atrous_iterations",
            self.atrous_iterations,
            0..=MAX_ATROUS_ITERATIONS,
        )?;
        check_u32(
            "denoiser.max_history_length",
            self.max_history_length,
            1..=MAX_HISTORY_LENGTH,
        )?;
        check_f32(
            "denoiser.reprojection_depth_tolerance",
            self.reprojection_depth_tolerance,
            0.0..=1.0,
            true,
        )?;
        check_f32(
            "denoiser.reprojection_normal_threshold",
            self.reprojection_normal_threshold,
            -1.0..=1.0,
            false,
        )?;
        check_f32("denoiser.phi_depth", self.phi_depth, 0.0..=10.0, true)?;
        check_f32("denoiser.phi_normal", self.phi_normal, 1.0..=1024.0, false)?;
        check_f32(
            "denoiser.phi_luminance",
            self.phi_luminance,
            0.0..=1000.0,
            true,
        )?;
        check_u32(
            "denoiser.min_history_for_temporal_variance",
            self.min_history_for_temporal_variance,
            1..=32,
        )
    }
}

/// Every RT lighting effect plus the shared denoiser settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtEffectsSettings {
    pub shadows: RtShadowSettings,
    pub ambient_occlusion: RtAoSettings,
    pub reflections: RtReflectionSettings,
    pub diffuse_gi: RtGiSettings,
    pub denoiser: DenoiserSettings,
}

impl RtEffectsSettings {
    pub fn validate(&self) -> Result<(), RtError> {
        self.shadows.validate()?;
        self.ambient_occlusion.validate()?;
        self.reflections.validate()?;
        self.diffuse_gi.validate()?;
        self.denoiser.validate()
    }

    /// What the ray-traced path of each effect would run for these settings on
    /// a device with status `rt`: `Disabled` (settings), `Fallback` with the
    /// precise reason (tier unavailable or invalid settings) or `RayTraced`.
    /// This describes the kernels of this crate; it does not claim that a frame
    /// runs them (the renderer reports that once the effects are wired in).
    pub fn status(&self, rt: &RayTracingStatus) -> RtEffectsStatus {
        let denoiser = self.denoiser.validate();
        let shadow_note = (self.shadows.is_soft() && !self.shadows.denoise).then(|| {
            format!(
                "soft shadows with {} sample(s) per pixel are not denoised",
                self.shadows.samples_per_pixel
            )
        });
        let reflection_note = Some(format!(
            "pixels rougher than roughness_cutoff {:.2} are flagged for the raster fallback",
            self.reflections.roughness_cutoff
        ));
        RtEffectsStatus {
            shadows: effect_status(
                RtEffect::Shadows,
                self.shadows.enabled,
                self.shadows.validate(),
                self.shadows.denoise && self.shadows.is_soft(),
                &denoiser,
                rt,
                shadow_note,
            ),
            ambient_occlusion: effect_status(
                RtEffect::AmbientOcclusion,
                self.ambient_occlusion.enabled,
                self.ambient_occlusion.validate(),
                self.ambient_occlusion.denoise,
                &denoiser,
                rt,
                None,
            ),
            reflections: effect_status(
                RtEffect::Reflections,
                self.reflections.enabled,
                self.reflections.validate(),
                self.reflections.denoise,
                &denoiser,
                rt,
                reflection_note,
            ),
            diffuse_gi: effect_status(
                RtEffect::DiffuseGi,
                self.diffuse_gi.enabled,
                self.diffuse_gi.validate(),
                self.diffuse_gi.denoise,
                &denoiser,
                rt,
                None,
            ),
        }
    }
}

fn effect_status(
    effect: RtEffect,
    enabled: bool,
    valid: Result<(), RtError>,
    denoise: bool,
    denoiser: &Result<(), RtError>,
    rt: &RayTracingStatus,
    note: Option<String>,
) -> RtEffectStatus {
    if !enabled {
        return RtEffectStatus::new(
            effect,
            RtEffectMode::Disabled,
            false,
            "disabled in settings",
        );
    }
    if let Err(e) = valid {
        return RtEffectStatus::new(
            effect,
            RtEffectMode::Fallback,
            false,
            format!("{e}; using the raster fallback"),
        );
    }
    if !rt.is_hardware() {
        return RtEffectStatus::new(
            effect,
            RtEffectMode::Fallback,
            false,
            format!("{rt}; using the raster fallback"),
        );
    }
    if denoise && let Err(e) = denoiser {
        return RtEffectStatus::new(
            effect,
            RtEffectMode::RayTraced,
            false,
            format!("not denoised: {e}"),
        );
    }
    RtEffectStatus {
        effect,
        mode: RtEffectMode::RayTraced,
        denoised: denoise,
        reason: note.map(|n| crate::status::bounded_reason(&n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::RtUnavailableCause;

    #[test]
    fn defaults_are_valid_and_opt_in() {
        let settings = RtEffectsSettings::default();
        settings.validate().unwrap();
        assert!(!settings.shadows.enabled && !settings.ambient_occlusion.enabled);
        assert!(!settings.reflections.enabled && !settings.diffuse_gi.enabled);
        assert!(!settings.shadows.is_soft());
    }

    #[test]
    fn every_range_is_checked_with_the_field_name() {
        let bad = |s: RtEffectsSettings, field: &str| {
            let message = s.validate().unwrap_err().to_string();
            assert!(message.contains(field), "{message} should name {field}");
        };
        let d = RtEffectsSettings::default();
        let mut s = d;
        s.shadows.sun_angular_radius = 0.3;
        bad(s, "shadows.sun_angular_radius");
        let mut s = d;
        s.shadows.samples_per_pixel = 0;
        bad(s, "shadows.samples_per_pixel");
        let mut s = d;
        s.shadows.max_distance = 0.0;
        bad(s, "shadows.max_distance");
        let mut s = d;
        s.ambient_occlusion.radius = 0.0;
        bad(s, "ambient_occlusion.radius");
        let mut s = d;
        s.ambient_occlusion.radius = f32::NAN;
        bad(s, "ambient_occlusion.radius");
        let mut s = d;
        s.ambient_occlusion.rays_per_pixel = 65;
        bad(s, "ambient_occlusion.rays_per_pixel");
        let mut s = d;
        s.ambient_occlusion.normal_bias = -1.0;
        bad(s, "ambient_occlusion.normal_bias");
        let mut s = d;
        s.reflections.roughness_cutoff = 1.5;
        bad(s, "reflections.roughness_cutoff");
        let mut s = d;
        s.reflections.max_distance = f32::INFINITY;
        bad(s, "reflections.max_distance");
        let mut s = d;
        s.diffuse_gi.bounces = 0;
        bad(s, "diffuse_gi.bounces");
        let mut s = d;
        s.diffuse_gi.bounces = 9;
        bad(s, "diffuse_gi.bounces");
        let mut s = d;
        s.diffuse_gi.rays_per_pixel = 0;
        bad(s, "diffuse_gi.rays_per_pixel");
        let mut s = d;
        s.denoiser.atrous_iterations = 9;
        bad(s, "denoiser.atrous_iterations");
        let mut s = d;
        s.denoiser.max_history_length = 0;
        bad(s, "denoiser.max_history_length");
        let mut s = d;
        s.denoiser.reprojection_depth_tolerance = 0.0;
        bad(s, "denoiser.reprojection_depth_tolerance");
        let mut s = d;
        s.denoiser.reprojection_normal_threshold = -1.5;
        bad(s, "denoiser.reprojection_normal_threshold");
        let mut s = d;
        s.denoiser.phi_normal = 0.5;
        bad(s, "denoiser.phi_normal");
        let mut s = d;
        s.denoiser.phi_luminance = 0.0;
        bad(s, "denoiser.phi_luminance");
        let mut s = d;
        s.denoiser.min_history_for_temporal_variance = 0;
        bad(s, "denoiser.min_history_for_temporal_variance");
        // Inclusive bounds are accepted.
        let mut s = d;
        s.shadows.sun_angular_radius = MAX_SUN_ANGULAR_RADIUS;
        s.ambient_occlusion.radius = MAX_AO_RADIUS;
        s.ambient_occlusion.rays_per_pixel = MAX_RAYS_PER_PIXEL;
        s.reflections.roughness_cutoff = 0.0;
        s.diffuse_gi.bounces = MAX_GI_BOUNCES;
        s.denoiser.atrous_iterations = 0;
        s.denoiser.max_history_length = MAX_HISTORY_LENGTH;
        s.validate().unwrap();
    }

    #[test]
    fn serde_uses_defaults_and_rejects_unknown_fields() {
        let s: RtEffectsSettings = serde_json::from_str(
            r#"{ "ambient_occlusion": { "enabled": true, "radius": 2.0 }, "denoiser": { "atrous_iterations": 3 } }"#,
        )
        .unwrap();
        assert!(s.ambient_occlusion.enabled);
        assert_eq!(s.ambient_occlusion.radius, 2.0);
        assert_eq!(s.ambient_occlusion.rays_per_pixel, 1);
        assert_eq!(s.denoiser.atrous_iterations, 3);
        assert_eq!(s.denoiser.max_history_length, 32);
        let back: RtEffectsSettings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert!(
            serde_json::from_str::<RtEffectsSettings>(
                r#"{ "ambient_occlusion": { "radios": 2.0 } }"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<RtEffectsSettings>(r#"{ "caustics": {} }"#).is_err());
    }

    #[test]
    fn status_reports_disabled_fallback_and_ray_traced_with_reasons() {
        let mut settings = RtEffectsSettings::default();
        settings.ambient_occlusion.enabled = true;
        settings.reflections.enabled = true;
        settings.diffuse_gi.enabled = true;
        settings.diffuse_gi.bounces = 0;
        let hardware = RayTracingStatus::hardware("GPU (Vulkan)".into());
        let status = settings.status(&hardware);
        assert_eq!(status.shadows.mode, RtEffectMode::Disabled);
        assert_eq!(
            status.shadows.reason.as_deref(),
            Some("disabled in settings")
        );
        assert_eq!(status.ambient_occlusion.mode, RtEffectMode::RayTraced);
        assert!(status.ambient_occlusion.denoised);
        assert!(status.ambient_occlusion.reason.is_none());
        assert_eq!(status.reflections.mode, RtEffectMode::RayTraced);
        assert!(
            status
                .reflections
                .reason
                .as_deref()
                .unwrap()
                .contains("roughness_cutoff 0.50"),
            "{:?}",
            status.reflections.reason
        );
        assert_eq!(status.diffuse_gi.mode, RtEffectMode::Fallback);
        assert!(
            status
                .diffuse_gi
                .reason
                .as_deref()
                .unwrap()
                .contains("diffuse_gi.bounces")
        );

        let unavailable = RayTracingStatus::unavailable(
            RtUnavailableCause::UnsupportedBackend,
            "DX12 adapter X does not expose EXPERIMENTAL_RAY_QUERY",
            None,
        );
        let status = settings.status(&unavailable);
        assert_eq!(status.ambient_occlusion.mode, RtEffectMode::Fallback);
        let reason = status.ambient_occlusion.reason.unwrap();
        assert!(
            reason.contains("DX12 adapter X") && reason.contains("raster fallback"),
            "{reason}"
        );
        assert!(!status.ambient_occlusion.denoised);

        settings.denoiser.phi_luminance = -1.0;
        let status = settings.status(&hardware);
        assert_eq!(status.ambient_occlusion.mode, RtEffectMode::RayTraced);
        assert!(!status.ambient_occlusion.denoised);
        assert!(
            status
                .ambient_occlusion
                .reason
                .unwrap()
                .contains("not denoised")
        );

        settings.shadows.enabled = true;
        settings.shadows.sun_angular_radius = 0.01;
        settings.shadows.denoise = false;
        let status = settings.status(&hardware);
        assert_eq!(status.shadows.mode, RtEffectMode::RayTraced);
        assert!(!status.shadows.denoised);
        assert!(status.shadows.reason.unwrap().contains("not denoised"));
        let json = serde_json::to_string(&settings.status(&unavailable)).unwrap();
        assert!(json.contains("\"mode\":\"fallback\""), "{json}");
        assert!(json.contains("\"effect\":\"ambient_occlusion\""), "{json}");
    }
}
