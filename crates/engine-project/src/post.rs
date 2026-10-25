//! Authored HDR post-processing: the per-camera [`PostProcess`] component,
//! [`PostProcessVolume`] entities that override it by region, the blend
//! ([`resolve_post`]) evaluated at a view's camera position, and the
//! renderer-reported [`PostStatus`]. See docs/rendering-post.md.
//!
//! Volume overrides are per parameter: every field of a volume is optional
//! and `None` means "not overridden". Continuous values interpolate toward
//! the override by the volume's influence; booleans, enums and asset
//! references switch once the influence reaches one half.
use crate::{Error, Result};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Largest volume extent and blend distance (metres).
pub const MAX_VOLUME_EXTENT: f32 = 1_000_000.0;
pub const MAX_BLEND_DISTANCE: f32 = 100_000.0;
/// Largest `.cube` file the runtime reads for a grading LUT.
pub const MAX_LUT_BYTES: u64 = 16 * 1024 * 1024;
/// Most distinct LUT assets one frame carries.
pub const MAX_FRAME_LUTS: usize = 8;

fn one() -> u32 {
    1
}

fn invalid(code: &str, message: &str) -> Error {
    Error::new(code, message)
}

/// Interpolation of one authored parameter.
pub trait Blend: Clone {
    /// `self` at `t == 0`, `to` at `t == 1`.
    fn blend(&self, to: &Self, t: f32) -> Self;
}

impl Blend for f32 {
    fn blend(&self, to: &Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

impl<const N: usize> Blend for [f32; N] {
    fn blend(&self, to: &Self, t: f32) -> Self {
        let mut out = *self;
        for (o, (a, b)) in out.iter_mut().zip(self.iter().zip(to)) {
            *o = a + (b - a) * t;
        }
        out
    }
}

macro_rules! switch_blend {
    ($($t:ty),*) => {$(
        impl Blend for $t {
            fn blend(&self, to: &Self, t: f32) -> Self {
                if t >= 0.5 { to.clone() } else { self.clone() }
            }
        }
    )*};
}
switch_blend!(bool, Option<String>);

/// The display transform.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TonemapMode {
    /// Linear clamp: the default appearance.
    #[default]
    None,
    Aces,
    Agx,
    AgxPunchy,
    Neutral,
}

/// How the scene is scaled before grading and tonemapping.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExposureMode {
    /// Only `Camera3D.lighting.exposure` applies.
    #[default]
    Off,
    Manual,
    Auto,
}
switch_blend!(TonemapMode, ExposureMode);

/// Defines a settings group and its all-optional override twin.
macro_rules! post_group {
    ($(#[$meta:meta])* $name:ident / $over:ident {
        $( $(#[$fmeta:meta])* $field:ident : $ty:ty = $default:expr ),* $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name { $( $(#[$fmeta])* pub $field: $ty, )* }

        impl Default for $name {
            fn default() -> Self { Self { $( $field: $default, )* } }
        }

        #[doc = concat!("Per-parameter overrides of [`", stringify!($name), "`]: `None` keeps the value below.")]
        #[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
        #[serde(default, deny_unknown_fields)]
        pub struct $over {
            $(
                #[serde(skip_serializing_if = "Option::is_none")]
                pub $field: Option<$ty>,
            )*
        }

        impl $over {
            /// Blend the set overrides into `base` with influence `t` (0..=1).
            pub fn apply(&self, base: &mut $name, t: f32) {
                $( if let Some(value) = &self.$field {
                    base.$field = Blend::blend(&base.$field, value, t);
                } )*
            }

            /// Whether any parameter is overridden.
            pub fn is_empty(&self) -> bool { true $( && self.$field.is_none() )* }
        }
    };
}

post_group! {
    /// Exposure controls (see docs/rendering-post.md for the precedence
    /// with `Camera3D.lighting.exposure`).
    Exposure / ExposureOverride {
        mode: ExposureMode = ExposureMode::Off,
        /// Manual: the scene is scaled by `2^-ev100`.
        ev100: f32 = 0.0,
        /// Stops added in manual and auto modes.
        compensation: f32 = 0.0,
        /// Auto: log2 luminance range of the histogram.
        min_ev: f32 = -8.0,
        max_ev: f32 = 6.0,
        /// Auto: the histogram mass between these fractions is averaged.
        low_percentile: f32 = 0.1,
        high_percentile: f32 = 0.9,
        /// Auto: adaptation rates (1/s) when the scene gets darker / brighter.
        speed_up: f32 = 3.0,
        speed_down: f32 = 1.0,
    }
}

post_group! {
    Bloom / BloomOverride {
        enabled: bool = false,
        threshold: f32 = 1.0,
        knee: f32 = 0.5,
        intensity: f32 = 0.3,
        scatter: f32 = 0.7,
        tint: [f32; 3] = [1.0; 3],
    }
}

post_group! {
    Grading / GradingOverride {
        enabled: bool = false,
        temperature: f32 = 0.0,
        tint: f32 = 0.0,
        exposure_offset: f32 = 0.0,
        contrast: f32 = 1.0,
        saturation: f32 = 1.0,
        lift: [f32; 3] = [0.0; 3],
        gamma: [f32; 3] = [1.0; 3],
        gain: [f32; 3] = [1.0; 3],
        hue_shift_degrees: f32 = 0.0,
        /// Asset id of a `.cube` 3D LUT (kind `lut`).
        lut: Option<String> = None,
        lut_intensity: f32 = 1.0,
    }
}

post_group! {
    Vignette / VignetteOverride {
        enabled: bool = false,
        intensity: f32 = 0.4,
        smoothness: f32 = 0.5,
        center: [f32; 2] = [0.5, 0.5],
        color: [f32; 3] = [0.0; 3],
    }
}

post_group! {
    Grain / GrainOverride {
        enabled: bool = false,
        intensity: f32 = 0.1,
        response: f32 = 0.8,
    }
}

fn within(value: f32, min: f32, max: f32) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}

/// Per-camera post-processing, on the camera entity next to `Camera3D`.
/// With no `PostProcess` (and no active volume) the camera renders with the
/// default linear-clamp output, exactly as before.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PostProcess {
    #[serde(default = "one")]
    pub version: u32,
    pub tonemap: TonemapMode,
    pub exposure: Exposure,
    pub bloom: Bloom,
    pub grading: Grading,
    pub vignette: Vignette,
    pub grain: Grain,
}

impl Default for PostProcess {
    fn default() -> Self {
        Self {
            version: 1,
            tonemap: TonemapMode::None,
            exposure: Exposure::default(),
            bloom: Bloom::default(),
            grading: Grading::default(),
            vignette: Vignette::default(),
            grain: Grain::default(),
        }
    }
}

impl PostProcess {
    pub fn validate(&self) -> Result<()> {
        let e = &self.exposure;
        let b = &self.bloom;
        let g = &self.grading;
        let v = &self.vignette;
        let n = &self.grain;
        let ok = self.version == 1
            && within(e.ev100, -30.0, 30.0)
            && within(e.compensation, -16.0, 16.0)
            && within(e.min_ev, -16.0, 16.0)
            && within(e.max_ev, -16.0, 16.0)
            && e.min_ev + 0.5 <= e.max_ev
            && within(e.low_percentile, 0.0, 1.0)
            && within(e.high_percentile, 0.0, 1.0)
            && e.low_percentile < e.high_percentile
            && within(e.speed_up, 0.0, 1000.0)
            && within(e.speed_down, 0.0, 1000.0)
            && within(b.threshold, 0.0, 1000.0)
            && within(b.knee, 0.0, 1.0)
            && within(b.intensity, 0.0, 100.0)
            && within(b.scatter, 0.0, 1.0)
            && b.tint.iter().all(|c| within(*c, 0.0, 100.0))
            && within(g.temperature, -1.0, 1.0)
            && within(g.tint, -1.0, 1.0)
            && within(g.exposure_offset, -16.0, 16.0)
            && within(g.contrast, 0.0, 4.0)
            && within(g.saturation, 0.0, 4.0)
            && g.lift.iter().all(|c| within(*c, -1.0, 1.0))
            && g.gamma.iter().all(|c| within(*c, 0.1, 4.0))
            && g.gain.iter().all(|c| within(*c, 0.0, 4.0))
            && within(g.hue_shift_degrees, -360.0, 360.0)
            && within(g.lut_intensity, 0.0, 1.0)
            && g.lut.as_deref().is_none_or(|id| !id.is_empty())
            && within(v.intensity, 0.0, 1.0)
            && within(v.smoothness, 0.01, 1.0)
            && v.center.iter().all(|c| within(*c, 0.0, 1.0))
            && v.color.iter().all(|c| within(*c, 0.0, 1.0))
            && within(n.intensity, 0.0, 1.0)
            && within(n.response, 0.0, 1.0);
        if ok {
            Ok(())
        } else {
            Err(invalid(
                "post.invalid",
                "PostProcess requires version 1 and every value finite and within its documented range",
            ))
        }
    }

    /// The asset ids this setting references.
    pub fn asset_refs(&self) -> impl Iterator<Item = &str> {
        self.grading.lut.as_deref().into_iter()
    }
}

/// The region a volume covers, in the entity's local space (the Transform
/// scales and places it).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeShape {
    /// Everywhere.
    #[default]
    Global,
    /// An axis-aligned box of full extents `size`, centred on the origin.
    Box {
        size: [f32; 3],
    },
    Sphere {
        radius: f32,
    },
}

/// Overrides of [`PostProcess`] inside a region, blended by distance,
/// priority and weight. Global volumes need no Transform; local ones do.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PostProcessVolume {
    #[serde(default = "one")]
    pub version: u32,
    pub enabled: bool,
    pub shape: VolumeShape,
    /// Higher priorities are applied later and so win over lower ones;
    /// ties break by entity ID.
    pub priority: i32,
    /// Metres outside the shape over which the influence fades to zero
    /// (0: a hard edge).
    pub blend_distance: f32,
    /// Overall strength 0..=1.
    pub weight: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tonemap: Option<TonemapMode>,
    #[serde(skip_serializing_if = "ExposureOverride::is_empty")]
    pub exposure: ExposureOverride,
    #[serde(skip_serializing_if = "BloomOverride::is_empty")]
    pub bloom: BloomOverride,
    #[serde(skip_serializing_if = "GradingOverride::is_empty")]
    pub grading: GradingOverride,
    #[serde(skip_serializing_if = "VignetteOverride::is_empty")]
    pub vignette: VignetteOverride,
    #[serde(skip_serializing_if = "GrainOverride::is_empty")]
    pub grain: GrainOverride,
}

impl Default for PostProcessVolume {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            shape: VolumeShape::Global,
            priority: 0,
            blend_distance: 0.0,
            weight: 1.0,
            tonemap: None,
            exposure: ExposureOverride::default(),
            bloom: BloomOverride::default(),
            grading: GradingOverride::default(),
            vignette: VignetteOverride::default(),
            grain: GrainOverride::default(),
        }
    }
}

impl PostProcessVolume {
    /// Apply every set override to `settings` with influence `t`.
    pub fn apply(&self, settings: &mut PostProcess, t: f32) {
        if let Some(mode) = &self.tonemap {
            settings.tonemap = Blend::blend(&settings.tonemap, mode, t);
        }
        self.exposure.apply(&mut settings.exposure, t);
        self.bloom.apply(&mut settings.bloom, t);
        self.grading.apply(&mut settings.grading, t);
        self.vignette.apply(&mut settings.vignette, t);
        self.grain.apply(&mut settings.grain, t);
    }

    pub fn validate(&self) -> Result<()> {
        let shape_ok = match self.shape {
            VolumeShape::Global => true,
            VolumeShape::Box { size } => size
                .iter()
                .all(|s| s.is_finite() && *s > 0.0 && *s <= MAX_VOLUME_EXTENT),
            VolumeShape::Sphere { radius } => {
                radius.is_finite() && radius > 0.0 && radius <= MAX_VOLUME_EXTENT
            }
        };
        if self.version != 1
            || !shape_ok
            || !(-1000..=1000).contains(&self.priority)
            || !within(self.blend_distance, 0.0, MAX_BLEND_DISTANCE)
            || !within(self.weight, 0.0, 1.0)
        {
            return Err(invalid(
                "post.volume.invalid",
                "PostProcessVolume requires version 1, a positive shape, priority in -1000..1000, blend_distance in 0..100000 and weight in 0..1",
            ));
        }
        // Every override applied at full strength must be a valid setting.
        let mut probe = PostProcess::default();
        self.apply(&mut probe, 1.0);
        probe.validate()
    }

    /// Whether the volume needs a Transform (any non-global shape).
    pub fn is_local(&self) -> bool {
        !matches!(self.shape, VolumeShape::Global)
    }

    /// The asset ids this volume references.
    pub fn asset_refs(&self) -> impl Iterator<Item = &str> {
        self.grading.lut.iter().flat_map(|lut| lut.as_deref())
    }

    /// Influence 0..=1 at `position` for a volume placed by the column-major
    /// `world` matrix: the shape, the blend distance and the weight.
    pub fn influence(&self, world: &[[f32; 4]; 4], position: [f32; 3]) -> f32 {
        let (outside, scale) = match self.shape {
            VolumeShape::Global => return self.weight.clamp(0.0, 1.0),
            shape => {
                let m = glam::Mat4::from_cols_array_2d(world);
                let det = m.determinant();
                if !m.is_finite() || det.abs() < 1e-12 {
                    return 0.0;
                }
                let local = m.inverse().transform_point3(glam::Vec3::from(position));
                let scale = (m.x_axis.truncate().length()
                    + m.y_axis.truncate().length()
                    + m.z_axis.truncate().length())
                    / 3.0;
                let outside = match shape {
                    VolumeShape::Box { size } => {
                        let half = glam::Vec3::from(size) * 0.5;
                        (local.abs() - half).max(glam::Vec3::ZERO).length()
                    }
                    VolumeShape::Sphere { radius } => (local.length() - radius).max(0.0),
                    VolumeShape::Global => 0.0,
                };
                (outside, scale)
            }
        };
        let distance = outside * scale;
        let factor = if distance <= 0.0 {
            1.0
        } else if self.blend_distance > 0.0 {
            (1.0 - distance / self.blend_distance).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (factor * self.weight).clamp(0.0, 1.0)
    }
}

/// An enabled volume extracted with its world transform.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeInstance3D {
    pub id: String,
    pub volume: PostProcessVolume,
    pub world: [[f32; 4]; 4],
}

/// A volume that influenced a view.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ActiveVolume {
    pub id: String,
    pub priority: i32,
    /// Influence 0..=1 at the camera.
    pub influence: f32,
}

/// The effective settings of one view and the volumes behind them.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPost {
    pub settings: PostProcess,
    /// In application order (lowest priority first).
    pub active: Vec<ActiveVolume>,
    /// Whether the camera itself carries a `PostProcess`.
    pub from_camera: bool,
}

/// Evaluate the post-processing of a view at `camera_position`: the camera's
/// own `PostProcess` (or the defaults) is the global base; the enabled
/// `volumes` are applied in ascending priority (ties by ID), each blended by
/// its [`PostProcessVolume::influence`]. `None` when there is neither a
/// camera `PostProcess` nor an influencing volume: the view then keeps the
/// default output.
pub fn resolve_post(
    camera: Option<&PostProcess>,
    volumes: &[VolumeInstance3D],
    camera_position: [f32; 3],
) -> Option<ResolvedPost> {
    let mut settings = camera.cloned().unwrap_or_default();
    let mut ordered: Vec<&VolumeInstance3D> = volumes.iter().filter(|v| v.volume.enabled).collect();
    ordered.sort_by(|a, b| {
        a.volume
            .priority
            .cmp(&b.volume.priority)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut active = Vec::new();
    for instance in ordered {
        let influence = instance.volume.influence(&instance.world, camera_position);
        if influence <= 0.0 {
            continue;
        }
        instance.volume.apply(&mut settings, influence);
        active.push(ActiveVolume {
            id: instance.id.clone(),
            priority: instance.volume.priority,
            influence,
        });
    }
    (camera.is_some() || !active.is_empty()).then_some(ResolvedPost {
        settings,
        active,
        from_camera: camera.is_some(),
    })
}

/// The post-processing data of a 3D frame (kept in `SceneRenderFrame`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PostFrame {
    /// The active camera's own `PostProcess`.
    pub camera: Option<PostProcess>,
    /// Enabled volumes, in stable entity ID order.
    pub volumes: Vec<VolumeInstance3D>,
    /// `.cube` text of the LUT assets referenced by the camera and volumes,
    /// by asset id. The renderer parses and validates them.
    pub luts: BTreeMap<String, Arc<str>>,
}

/// Renderer-reported post-processing state of the active camera view.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PostStatus {
    pub state: crate::rendering3d::RenderPipelineState,
    /// Whether the view runs the post stack (a camera `PostProcess` or an
    /// influencing volume).
    pub active: bool,
    /// Enabled volumes in the scene.
    pub volumes: u64,
    /// Volumes influencing the view, lowest priority first.
    pub active_volumes: Vec<ActiveVolume>,
    /// Effective settings summary.
    pub tonemap: Option<TonemapMode>,
    pub exposure_mode: Option<ExposureMode>,
    pub bloom: bool,
    pub grading: bool,
    pub vignette: bool,
    pub grain: bool,
    /// The LUT asset in effect, and why it is not applied when it fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lut: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lut_error: Option<String>,
}

impl PostStatus {
    /// The summary of `resolved` (what a renderer would apply).
    pub fn of(resolved: Option<&ResolvedPost>, volumes: usize) -> Self {
        let mut status = Self {
            volumes: volumes as u64,
            ..Self::default()
        };
        if let Some(resolved) = resolved {
            let s = &resolved.settings;
            status.active = true;
            status.active_volumes = resolved.active.clone();
            status.tonemap = Some(s.tonemap);
            status.exposure_mode = Some(s.exposure.mode);
            status.bloom = s.bloom.enabled;
            status.grading = s.grading.enabled;
            status.vignette = s.vignette.enabled;
            status.grain = s.grain.enabled;
            status.lut = s.grading.lut.clone().filter(|_| s.grading.enabled);
        }
        status
    }
}

/// Host-written post status (sibling of `LightingControl`).
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct PostControl {
    pub status: PostStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, z: f32) -> [[f32; 4]; 4] {
        glam::Mat4::from_translation(glam::Vec3::new(x, y, z)).to_cols_array_2d()
    }

    fn volume(priority: i32, f: impl FnOnce(&mut PostProcessVolume)) -> PostProcessVolume {
        let mut v = PostProcessVolume {
            priority,
            ..Default::default()
        };
        f(&mut v);
        v
    }

    fn instance(id: &str, volume: PostProcessVolume, world: [[f32; 4]; 4]) -> VolumeInstance3D {
        VolumeInstance3D {
            id: id.to_owned(),
            volume,
            world,
        }
    }

    #[test]
    fn no_post_data_keeps_the_default_output() {
        assert!(resolve_post(None, &[], [0.0; 3]).is_none());
        // A local volume that does not reach the camera does not engage it.
        let far = instance(
            "far",
            volume(0, |v| {
                v.shape = VolumeShape::Sphere { radius: 1.0 };
                v.tonemap = Some(TonemapMode::Aces);
            }),
            at(50.0, 0.0, 0.0),
        );
        assert!(resolve_post(None, &[far], [0.0; 3]).is_none());
    }

    #[test]
    fn a_camera_post_process_is_the_global_base() {
        let camera = PostProcess {
            tonemap: TonemapMode::Neutral,
            ..Default::default()
        };
        let r = resolve_post(Some(&camera), &[], [0.0; 3]).unwrap();
        assert_eq!(r.settings.tonemap, TonemapMode::Neutral);
        assert!(r.active.is_empty() && r.from_camera);
    }

    #[test]
    fn local_volume_overrides_only_inside_and_fades_over_the_blend_distance() {
        let local = instance(
            "box",
            volume(0, |v| {
                v.shape = VolumeShape::Box {
                    size: [4.0, 4.0, 4.0],
                };
                v.blend_distance = 2.0;
                v.exposure.mode = Some(ExposureMode::Manual);
                v.exposure.ev100 = Some(4.0);
                v.bloom.intensity = Some(1.3);
            }),
            at(10.0, 0.0, 0.0),
        );
        let camera = PostProcess::default();
        let inside = resolve_post(
            Some(&camera),
            std::slice::from_ref(&local),
            [11.0, 0.0, 0.0],
        )
        .unwrap();
        assert_eq!(inside.settings.exposure.ev100, 4.0);
        assert_eq!(inside.settings.exposure.mode, ExposureMode::Manual);
        assert_eq!(inside.settings.bloom.intensity, 1.3);
        assert_eq!(inside.settings.bloom.threshold, 1.0, "not overridden");
        assert_eq!(inside.active.len(), 1);
        // 1 m outside the 2 m half extent: half way through the 2 m fade.
        let edge = resolve_post(
            Some(&camera),
            std::slice::from_ref(&local),
            [13.0, 0.0, 0.0],
        )
        .unwrap();
        assert!((edge.settings.exposure.ev100 - 2.0).abs() < 1e-5);
        assert!((edge.settings.bloom.intensity - (0.3 + 0.5 * 1.0)).abs() < 1e-5);
        assert_eq!(
            edge.settings.exposure.mode,
            ExposureMode::Manual,
            "discrete values switch at one half"
        );
        // Beyond the fade nothing is overridden.
        let outside = resolve_post(
            Some(&camera),
            std::slice::from_ref(&local),
            [20.0, 0.0, 0.0],
        )
        .unwrap();
        assert_eq!(outside.settings, camera);
        assert!(outside.active.is_empty());
    }

    #[test]
    fn hard_edge_and_sphere_and_weight() {
        let sphere = instance(
            "s",
            volume(0, |v| {
                v.shape = VolumeShape::Sphere { radius: 2.0 };
                v.weight = 0.5;
                v.grain.intensity = Some(0.5);
                v.grain.enabled = Some(true);
            }),
            at(0.0, 0.0, 0.0),
        );
        let camera = PostProcess::default();
        let inside = resolve_post(
            Some(&camera),
            std::slice::from_ref(&sphere),
            [1.0, 0.0, 0.0],
        )
        .unwrap();
        assert!(
            (inside.settings.grain.intensity - 0.3).abs() < 1e-6,
            "0.1 + 0.5 * 0.4"
        );
        assert!(inside.settings.grain.enabled, "t = 0.5 switches booleans");
        let outside = resolve_post(
            Some(&camera),
            std::slice::from_ref(&sphere),
            [2.5, 0.0, 0.0],
        )
        .unwrap();
        assert!(outside.active.is_empty(), "hard edge");
    }

    #[test]
    fn priority_orders_overrides_and_ties_break_by_id() {
        let global = |id: &str, priority: i32, ev: f32| {
            instance(
                id,
                volume(priority, |v| {
                    v.exposure.mode = Some(ExposureMode::Manual);
                    v.exposure.ev100 = Some(ev);
                }),
                at(0.0, 0.0, 0.0),
            )
        };
        let volumes = [
            global("a", 5, 1.0),
            global("b", 1, 7.0),
            global("c", 5, 3.0),
        ];
        let r = resolve_post(None, &volumes, [0.0; 3]).unwrap();
        // b (priority 1) first, then a, then c (ties by ID): c wins.
        assert_eq!(r.settings.exposure.ev100, 3.0);
        let ids: Vec<_> = r.active.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
        assert!(!r.from_camera);
        // Disabled volumes are ignored.
        let mut off = global("d", 99, 9.0);
        off.volume.enabled = false;
        let r = resolve_post(None, &[off], [0.0; 3]);
        assert!(r.is_none());
    }

    #[test]
    fn a_global_volume_blends_by_weight() {
        let camera = PostProcess::default();
        let mut v = volume(0, |v| {
            v.weight = 0.25;
            v.vignette.intensity = Some(0.8);
        });
        v.tonemap = Some(TonemapMode::Aces);
        let r = resolve_post(
            Some(&camera),
            &[instance("g", v, at(0.0, 0.0, 0.0))],
            [3.0, 3.0, 3.0],
        )
        .unwrap();
        assert!((r.settings.vignette.intensity - (0.4 + 0.25 * 0.4)).abs() < 1e-6);
        assert_eq!(r.settings.tonemap, TonemapMode::None, "0.25 < 0.5");
    }

    #[test]
    fn scaled_volumes_scale_their_blend_distance() {
        let world = glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::splat(2.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ZERO,
        )
        .to_cols_array_2d();
        let v = volume(0, |v| {
            v.shape = VolumeShape::Sphere { radius: 1.0 };
            v.blend_distance = 2.0;
        });
        // The sphere is 2 m in radius in the world; 3 m is 1 m outside.
        assert!((v.influence(&world, [3.0, 0.0, 0.0]) - 0.5).abs() < 1e-5);
        assert_eq!(v.influence(&world, [1.0, 0.0, 0.0]), 1.0);
    }

    #[test]
    fn validation_rejects_bad_values() {
        assert!(PostProcess::default().validate().is_ok());
        let mut p = PostProcess::default();
        p.bloom.scatter = 1.5;
        assert!(p.validate().is_err());
        let mut p = PostProcess::default();
        p.exposure.low_percentile = 0.95;
        assert!(p.validate().is_err());
        assert!(PostProcessVolume::default().validate().is_ok());
        let bad = volume(0, |v| v.grading.saturation = Some(9.0));
        assert!(bad.validate().is_err(), "overrides are range checked");
        let bad = volume(0, |v| {
            v.shape = VolumeShape::Box {
                size: [1.0, 0.0, 1.0],
            }
        });
        assert!(bad.validate().is_err());
        let bad = volume(5000, |_| {});
        assert!(bad.validate().is_err());
    }

    #[test]
    fn json_round_trips_and_rejects_unknown_fields() {
        let v: PostProcessVolume = serde_json::from_value(serde_json::json!({
            "shape": {"type": "box", "size": [4, 4, 4]},
            "priority": 2, "blend_distance": 1.5,
            "tonemap": "agx_punchy",
            "bloom": {"intensity": 0.8}
        }))
        .unwrap();
        assert_eq!(v.tonemap, Some(TonemapMode::AgxPunchy));
        assert_eq!(v.bloom.intensity, Some(0.8));
        assert!(v.bloom.enabled.is_none());
        let back: PostProcessVolume =
            serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
        assert_eq!(back, v);
        assert!(
            serde_json::from_value::<PostProcessVolume>(
                serde_json::json!({"bloom": {"nonsense": 1}})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<PostProcess>(serde_json::json!({"tonemap": "reinhard"}))
                .is_err()
        );
    }

    #[test]
    fn status_summarises_the_effective_settings() {
        let mut camera = PostProcess::default();
        camera.tonemap = TonemapMode::Aces;
        camera.bloom.enabled = true;
        let r = resolve_post(Some(&camera), &[], [0.0; 3]).unwrap();
        let s = PostStatus::of(Some(&r), 3);
        assert!(s.active && s.bloom && !s.grain);
        assert_eq!(s.tonemap, Some(TonemapMode::Aces));
        assert_eq!(s.volumes, 3);
        assert!(!PostStatus::of(None, 0).active);
    }
}
