//! Authored hardware ray-traced lighting: the per-camera [`RayTracing`]
//! component and the renderer-reported [`RayTracingReport`]. The desktop
//! renderer maps the component onto `engine_render_rt::RtEffectsSettings`
//! (same field names, ranges and defaults) and runs each enabled effect ray
//! traced when the device's hardware tier is available, otherwise its raster
//! fallback. See docs/rendering-raytracing.md ("Frame integration").
use crate::{Error, Result};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

fn one() -> u32 {
    1
}

fn within(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}

fn above(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && value > min && value <= max
}

macro_rules! settings {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident: $ty:ty = $default:expr),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $($(#[$fmeta])* pub $field: $ty,)*
        }
        impl Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }
    };
}

settings!(
    /// Ray-traced sun shadows (fallback: the shadow maps).
    RtShadows {
        enabled: bool = false,
        /// Sun angular radius in radians; 0 traces hard shadows. 0..=0.2.
        sun_angular_radius: f32 = 0.0,
        /// Rays per pixel for soft shadows, 1..=64.
        samples_per_pixel: u32 = 1,
        /// Ray origin offset along the normal (m), 0..=1.
        normal_bias: f32 = 0.001,
        /// Longest shadow ray (m), (0, 1e6].
        max_distance: f32 = 10_000.0,
        /// Denoise soft shadows.
        denoise: bool = true,
    }
);

settings!(
    /// Ray-traced ambient occlusion (fallback: screen-space SSAO).
    RtAmbientOcclusion {
        enabled: bool = false,
        /// Occlusion radius (m), (0, 100].
        radius: f32 = 1.0,
        /// 1..=64.
        rays_per_pixel: u32 = 1,
        normal_bias: f32 = 0.001,
        denoise: bool = true,
    }
);

settings!(
    /// Ray-traced reflections (fallback: none).
    RtReflections {
        enabled: bool = false,
        /// Surfaces rougher than this get no reflection, 0..=1.
        roughness_cutoff: f32 = 0.5,
        max_distance: f32 = 1000.0,
        normal_bias: f32 = 0.001,
        denoise: bool = true,
    }
);

settings!(
    /// Ray-traced diffuse global illumination (fallback: constant ambient).
    RtDiffuseGi {
        enabled: bool = false,
        /// Path segments, 1..=8.
        bounces: u32 = 1,
        rays_per_pixel: u32 = 1,
        max_distance: f32 = 1000.0,
        normal_bias: f32 = 0.001,
        denoise: bool = true,
    }
);

settings!(
    /// The SVGF-style denoiser shared by the effects.
    RtDenoiser {
        atrous_iterations: u32 = 5,
        max_history_length: u32 = 32,
        reprojection_depth_tolerance: f32 = 0.1,
        reprojection_normal_threshold: f32 = 0.9,
        phi_depth: f32 = 0.05,
        phi_normal: f32 = 128.0,
        phi_luminance: f32 = 4.0,
        min_history_for_temporal_variance: u32 = 4,
    }
);

/// Per-camera ray-traced lighting, on the `Camera3D` entity. Each enabled
/// effect also turns its `engine.rt.*` render feature on for the camera
/// (`Camera3D.render_features` can still force a feature off or on).
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RayTracing {
    #[serde(default = "one")]
    pub version: u32,
    pub shadows: RtShadows,
    pub ambient_occlusion: RtAmbientOcclusion,
    pub reflections: RtReflections,
    pub diffuse_gi: RtDiffuseGi,
    pub denoiser: RtDenoiser,
}

impl Default for RayTracing {
    fn default() -> Self {
        Self {
            version: 1,
            shadows: RtShadows::default(),
            ambient_occlusion: RtAmbientOcclusion::default(),
            reflections: RtReflections::default(),
            diffuse_gi: RtDiffuseGi::default(),
            denoiser: RtDenoiser::default(),
        }
    }
}

impl RayTracing {
    /// Whether any effect is enabled (the device then requests ray query).
    pub fn any_enabled(&self) -> bool {
        self.shadows.enabled
            || self.ambient_occlusion.enabled
            || self.reflections.enabled
            || self.diffuse_gi.enabled
    }

    /// The first out-of-range field, by its path.
    pub fn validate(&self) -> Result<()> {
        let s = &self.shadows;
        let a = &self.ambient_occlusion;
        let r = &self.reflections;
        let g = &self.diffuse_gi;
        let d = &self.denoiser;
        let checks: [(&str, bool); 22] = [
            ("version", self.version == 1),
            (
                "shadows.sun_angular_radius",
                within(s.sun_angular_radius, 0.0, 0.2),
            ),
            (
                "shadows.samples_per_pixel",
                (1..=64).contains(&s.samples_per_pixel),
            ),
            ("shadows.normal_bias", within(s.normal_bias, 0.0, 1.0)),
            ("shadows.max_distance", above(s.max_distance, 0.0, 1e6)),
            ("ambient_occlusion.radius", above(a.radius, 0.0, 100.0)),
            (
                "ambient_occlusion.rays_per_pixel",
                (1..=64).contains(&a.rays_per_pixel),
            ),
            (
                "ambient_occlusion.normal_bias",
                within(a.normal_bias, 0.0, 1.0),
            ),
            (
                "reflections.roughness_cutoff",
                within(r.roughness_cutoff, 0.0, 1.0),
            ),
            ("reflections.max_distance", above(r.max_distance, 0.0, 1e6)),
            ("reflections.normal_bias", within(r.normal_bias, 0.0, 1.0)),
            ("diffuse_gi.bounces", (1..=8).contains(&g.bounces)),
            (
                "diffuse_gi.rays_per_pixel",
                (1..=64).contains(&g.rays_per_pixel),
            ),
            ("diffuse_gi.max_distance", above(g.max_distance, 0.0, 1e6)),
            ("diffuse_gi.normal_bias", within(g.normal_bias, 0.0, 1.0)),
            ("denoiser.atrous_iterations", d.atrous_iterations <= 8),
            (
                "denoiser.max_history_length",
                (1..=256).contains(&d.max_history_length),
            ),
            (
                "denoiser.reprojection_depth_tolerance",
                above(d.reprojection_depth_tolerance, 0.0, 1.0),
            ),
            (
                "denoiser.reprojection_normal_threshold",
                within(d.reprojection_normal_threshold, -1.0, 1.0),
            ),
            ("denoiser.phi_depth", above(d.phi_depth, 0.0, 10.0)),
            ("denoiser.phi_normal", within(d.phi_normal, 1.0, 1024.0)),
            (
                "denoiser.phi_luminance / min_history_for_temporal_variance",
                above(d.phi_luminance, 0.0, 1000.0)
                    && (1..=32).contains(&d.min_history_for_temporal_variance),
            ),
        ];
        match checks.iter().find(|(_, ok)| !ok) {
            None => Ok(()),
            Some((field, _)) => Err(Error::new(
                "raytracing.invalid",
                format!(
                    "RayTracing.{field} is outside its documented range (see component_schema)"
                ),
            )),
        }
    }
}

/// How one effect ran in the last camera view.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct RtEffectReport {
    /// `shadows`, `ambient_occlusion`, `reflections` or `diffuse_gi`.
    pub effect: String,
    /// `ray_traced`, `fallback` or `disabled`.
    pub mode: String,
    pub denoised: bool,
    /// What runs instead (`shadow_maps`, `ssao`, `none`, `constant_ambient`)
    /// when the mode is `fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Why the effect is not ray traced, or a note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Renderer-reported ray tracing of the active camera view (small: four
/// effects, a tier and scene counts).
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct RayTracingReport {
    pub state: crate::rendering3d::RenderPipelineState,
    /// `hardware` or `unavailable`.
    pub tier: String,
    /// Why the hardware tier is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub effects: Vec<RtEffectReport>,
    /// Acceleration structures of the synced scene.
    pub blas_count: u32,
    pub instance_count: u32,
    pub triangle_count: u64,
    /// BLAS built by the last scene sync (incremental: 0 when no mesh changed).
    pub last_blas_built: u32,
}

/// Host-written ray-tracing report (sibling of `PostControl`).
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct RayTracingControl {
    pub status: Option<RayTracingReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_unknown_fields_are_rejected() {
        let rt: RayTracing = serde_json::from_value(serde_json::json!({
            "shadows": {"enabled": true}
        }))
        .unwrap();
        assert!(rt.shadows.enabled && rt.any_enabled());
        rt.validate().unwrap();
        assert!(!RayTracing::default().any_enabled());
        assert!(
            serde_json::from_value::<RayTracing>(serde_json::json!({"shadows": {"bogus": 1}}))
                .is_err()
        );
    }

    #[test]
    fn out_of_range_fields_are_named() {
        let mut rt = RayTracing::default();
        rt.ambient_occlusion.radius = 0.0;
        let error = rt.validate().unwrap_err().to_string();
        assert!(error.contains("ambient_occlusion.radius"), "{error}");
        let mut rt = RayTracing::default();
        rt.diffuse_gi.bounces = 9;
        assert!(rt.validate().unwrap_err().to_string().contains("bounces"));
    }
}
