//! Lights of one view: the CPU description ([`Light`], [`LightList`]), its
//! shadow requests ([`ShadowSettings`]) and the GPU layouts the lighting
//! shaders read ([`GpuLight`], [`GpuShadow`]).
//!
//! Intensities are in *shading units*: the photometric quantity multiplied
//! by the view's exposure (see [`photometric`]). The built-in material turns
//! them into pixel values with the BRDF, so `intensity = 1` on a directional
//! light gives a white Lambertian surface facing it `1 / pi`.

use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};

/// Most lights one view uploads after culling (directional + local).
pub const MAX_LIGHTS: usize = 4096;
/// Most lights a frame may submit; culling and the [`MAX_LIGHTS`] budget
/// reduce them (the overflow is reported, not an error).
pub const MAX_SUBMITTED_LIGHTS: usize = 65_536;
/// Most shadow views (cascades, cube faces, spot views) one view renders.
pub const MAX_SHADOWS: usize = 256;
/// Most cascades of a directional light.
pub const MAX_CASCADES: u32 = 4;
/// Allowed shadow resolutions: powers of two in this range.
pub const SHADOW_RESOLUTIONS: std::ops::RangeInclusive<u32> = 64..=4096;
/// Largest PCF kernel radius, in shadow texels.
pub const MAX_PCF_RADIUS: u32 = 4;
/// Largest PCSS filter radius, in shadow texels.
pub const MAX_PCSS_RADIUS: u32 = 32;

/// `EngineLight.kind` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LightKind {
    Directional = 0,
    Point = 1,
    Spot = 2,
    /// Rectangular area light.
    Area = 3,
}

/// Kind-specific light data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightShape {
    /// Infinitely distant; `intensity` is illuminance (lux x exposure).
    Directional,
    /// `intensity` is luminous intensity (candela x exposure).
    Point,
    /// A point light limited to a cone around `direction`: full intensity
    /// inside `inner_angle`, smoothstep falloff to zero at `outer_angle`
    /// (half angles in radians, `0 <= inner < outer <= pi/2`).
    Spot { inner_angle: f32, outer_angle: f32 },
    /// A rectangle of uniform luminance (`intensity`: cd/m^2 x exposure)
    /// centred on `position`, emitting along `direction`; `right` is the
    /// rectangle's width axis (orthogonal to `direction`).
    Area {
        width: f32,
        height: f32,
        right: Vec3,
        two_sided: bool,
    },
}

impl LightShape {
    pub fn kind(&self) -> LightKind {
        match self {
            Self::Directional => LightKind::Directional,
            Self::Point => LightKind::Point,
            Self::Spot { .. } => LightKind::Spot,
            Self::Area { .. } => LightKind::Area,
        }
    }
}

/// How shadow map samples are filtered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShadowFilter {
    /// One hardware-compared (2x2 bilinear) tap.
    Hard,
    /// A `(2 radius + 1)^2` grid of bilinear compares one texel apart
    /// (`radius` in `0..=MAX_PCF_RADIUS`).
    Pcf { radius: u32 },
    /// Percentage-closer soft shadows: a blocker search, then a filter
    /// whose radius grows with the blocker-receiver distance, up to
    /// `max_radius` texels. `source_size` is the emitter radius in metres
    /// for local lights and the angular diameter in radians for directional
    /// lights.
    Pcss { source_size: f32, max_radius: u32 },
}

impl ShadowFilter {
    fn mode(&self) -> f32 {
        match self {
            Self::Hard => 0.0,
            Self::Pcf { .. } => 1.0,
            Self::Pcss { .. } => 2.0,
        }
    }
}

/// Cascaded shadow map settings of a directional light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CascadeSettings {
    /// Number of cascades, `1..=MAX_CASCADES`.
    pub count: u32,
    /// Practical split scheme weight: 0 uniform, 1 logarithmic.
    pub split_lambda: f32,
    /// View depth (metres from the camera) covered by the last cascade;
    /// clamped to the camera's far plane. Beyond it surfaces are unshadowed.
    pub max_distance: f32,
    /// Fraction (`0..=0.5`) at the far end of each cascade blended into the
    /// next one (the last cascade fades out).
    pub blend: f32,
    /// How far (metres) beyond a cascade's bounding sphere, toward the light,
    /// casters are still rendered.
    pub caster_extension: f32,
}

impl Default for CascadeSettings {
    fn default() -> Self {
        Self {
            count: 4,
            split_lambda: 0.75,
            max_distance: 100.0,
            blend: 0.1,
            caster_extension: 100.0,
        }
    }
}

/// Shadow request of one light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSettings {
    /// Edge of each shadow view (cascade, cube face or spot view) in atlas
    /// texels: a power of two in [`SHADOW_RESOLUTIONS`].
    pub resolution: u32,
    /// Receiver depth offset toward the light, in shadow texels (the world
    /// size of a texel at the receiver), `0..=16`.
    pub depth_bias: f32,
    /// Receiver offset along its normal, in shadow texels, `0..=16`.
    pub normal_bias: f32,
    pub filter: ShadowFilter,
    /// Near plane (metres) of perspective shadow views (spot, point, area).
    pub near: f32,
    /// Directional lights only.
    pub cascades: CascadeSettings,
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            resolution: 1024,
            depth_bias: 1.0,
            normal_bias: 1.5,
            filter: ShadowFilter::Pcf { radius: 1 },
            near: 0.05,
            cascades: CascadeSettings::default(),
        }
    }
}

impl ShadowSettings {
    pub fn validate(&self, kind: LightKind) -> Result<()> {
        ensure!(
            SHADOW_RESOLUTIONS.contains(&self.resolution) && self.resolution.is_power_of_two(),
            "shadow resolution must be a power of two in 64..=4096"
        );
        ensure!(
            (0.0..=16.0).contains(&self.depth_bias) && (0.0..=16.0).contains(&self.normal_bias),
            "shadow depth and normal bias must be finite, in 0..=16 texels"
        );
        match self.filter {
            ShadowFilter::Hard => {}
            ShadowFilter::Pcf { radius } => ensure!(
                radius <= MAX_PCF_RADIUS,
                "PCF radius must be at most {MAX_PCF_RADIUS} texels"
            ),
            ShadowFilter::Pcss {
                source_size,
                max_radius,
            } => {
                let limit = if kind == LightKind::Directional {
                    0.2
                } else {
                    10.0
                };
                ensure!(
                    source_size.is_finite() && source_size > 0.0 && source_size <= limit,
                    "PCSS source size must be in (0, {limit}] (radians for directional, metres otherwise)"
                );
                ensure!(
                    (1..=MAX_PCSS_RADIUS).contains(&max_radius),
                    "PCSS max radius must be 1..={MAX_PCSS_RADIUS} texels"
                );
            }
        }
        ensure!(
            self.near.is_finite() && (0.001..=100.0).contains(&self.near),
            "shadow near plane must be in 0.001..=100 metres"
        );
        let c = &self.cascades;
        ensure!(
            (1..=MAX_CASCADES).contains(&c.count)
                && (0.0..=1.0).contains(&c.split_lambda)
                && c.max_distance.is_finite()
                && c.max_distance > 0.0
                && (0.0..=0.5).contains(&c.blend)
                && c.caster_extension.is_finite()
                && (0.0..=100_000.0).contains(&c.caster_extension),
            "cascades need count 1..=4, split lambda and blend in range, positive max distance \
             and a caster extension in 0..=100000"
        );
        Ok(())
    }

    /// Filter radius margin (texels) shadow views keep around their area.
    pub(crate) fn kernel_texels(&self) -> u32 {
        match self.filter {
            ShadowFilter::Hard => 1,
            ShadowFilter::Pcf { radius } => radius + 1,
            ShadowFilter::Pcss { max_radius, .. } => max_radius + 1,
        }
    }
}

/// One light of a view, in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub shape: LightShape,
    /// Linear RGB.
    pub color: Vec3,
    /// Shading units (see the module docs and [`LightShape`]).
    pub intensity: f32,
    /// Ignored by directional lights.
    pub position: Vec3,
    /// Unit vector the light travels along (spot axis, area normal,
    /// directional light direction).
    pub direction: Vec3,
    /// Distance (metres) at which the smooth window ends the inverse-square
    /// falloff; local lights only, `> 0`.
    pub range: f32,
    pub shadow: Option<ShadowSettings>,
}

impl Light {
    /// A directional light travelling along `direction`.
    pub fn directional(direction: Vec3, color: Vec3, illuminance: f32) -> Self {
        Self {
            shape: LightShape::Directional,
            color,
            intensity: illuminance,
            position: Vec3::ZERO,
            direction: direction.normalize_or_zero(),
            range: 0.0,
            shadow: None,
        }
    }

    pub fn point(position: Vec3, color: Vec3, candela: f32, range: f32) -> Self {
        Self {
            shape: LightShape::Point,
            color,
            intensity: candela,
            position,
            direction: Vec3::NEG_Y,
            range,
            shadow: None,
        }
    }

    pub fn spot(
        position: Vec3,
        direction: Vec3,
        color: Vec3,
        candela: f32,
        range: f32,
        inner_angle: f32,
        outer_angle: f32,
    ) -> Self {
        Self {
            shape: LightShape::Spot {
                inner_angle,
                outer_angle,
            },
            direction: direction.normalize_or_zero(),
            ..Self::point(position, color, candela, range)
        }
    }

    pub fn kind(&self) -> LightKind {
        self.shape.kind()
    }

    pub fn with_shadow(mut self, shadow: ShadowSettings) -> Self {
        self.shadow = Some(shadow);
        self
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.color.is_finite()
                && self.color.min_element() >= 0.0
                && self.intensity.is_finite()
                && self.intensity >= 0.0
                && self.position.is_finite()
                && self.direction.is_finite()
                && (self.direction.length() - 1.0).abs() < 1e-3,
            "lights need finite nonnegative colour and intensity, finite position and a unit direction"
        );
        if self.kind() != LightKind::Directional {
            ensure!(
                self.range.is_finite() && self.range > 0.0 && self.range <= 1.0e6,
                "local lights need a range in (0, 1e6] metres"
            );
        }
        match self.shape {
            LightShape::Spot {
                inner_angle,
                outer_angle,
            } => ensure!(
                inner_angle.is_finite()
                    && outer_angle.is_finite()
                    && inner_angle >= 0.0
                    && inner_angle < outer_angle
                    && outer_angle <= std::f32::consts::FRAC_PI_2,
                "spot lights need 0 <= inner < outer <= pi/2"
            ),
            LightShape::Area {
                width,
                height,
                right,
                ..
            } => {
                ensure!(
                    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0,
                    "area lights need a positive finite size"
                );
                ensure!(
                    right.is_finite()
                        && (right.length() - 1.0).abs() < 1e-3
                        && right.dot(self.direction).abs() < 1e-3,
                    "area light `right` must be a unit vector orthogonal to its direction"
                );
                ensure!(
                    self.range > 0.5 * width.hypot(height),
                    "area light range must exceed half the rectangle's diagonal"
                );
            }
            LightShape::Directional | LightShape::Point => {}
        }
        if let Some(shadow) = &self.shadow {
            shadow.validate(self.kind())?;
        }
        Ok(())
    }

    /// The GPU record (without its shadow index).
    pub(crate) fn gpu(&self) -> GpuLight {
        let (direction, tangent, params) = match self.shape {
            LightShape::Directional => {
                let cascades = self
                    .shadow
                    .map_or(CascadeSettings::default(), |s| s.cascades);
                (
                    -self.direction,
                    Vec3::ZERO,
                    [cascades.count as f32, cascades.blend, 0.0, 0.0],
                )
            }
            LightShape::Point => (self.direction, Vec3::ZERO, [0.0; 4]),
            LightShape::Spot {
                inner_angle,
                outer_angle,
            } => (
                self.direction,
                Vec3::ZERO,
                [inner_angle.cos(), outer_angle.cos(), 0.0, 0.0],
            ),
            LightShape::Area {
                width,
                height,
                right,
                two_sided,
            } => (
                self.direction,
                right,
                [
                    width * 0.5,
                    height * 0.5,
                    if two_sided { 1.0 } else { 0.0 },
                    0.0,
                ],
            ),
        };
        GpuLight {
            color: self.color.to_array(),
            intensity: self.intensity,
            position: self.position.to_array(),
            range: self.range,
            direction: direction.to_array(),
            kind: self.kind() as u32,
            tangent: tangent.to_array(),
            shadow_index: -1,
            params,
        }
    }
}

/// One light: `EngineLight` in `engine::view` (80 bytes, std430).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuLight {
    /// Linear RGB colour.
    pub color: [f32; 3],
    /// Shading units (see [`Light::intensity`]).
    pub intensity: f32,
    pub position: [f32; 3],
    pub range: f32,
    /// Directional: unit vector from surfaces toward the light. Spot and
    /// area: the direction the light travels (spot axis, area normal).
    pub direction: [f32; 3],
    /// A [`LightKind`] value.
    pub kind: u32,
    /// Area: the rectangle's unit width axis.
    pub tangent: [f32; 3],
    /// First `EngineShadow` of the light (cascade 0, cube face +X), or -1.
    pub shadow_index: i32,
    /// Directional: cascade count, cascade blend. Spot: cos inner, cos
    /// outer. Area: half width, half height, two-sided (0/1).
    pub params: [f32; 4],
}

impl GpuLight {
    pub fn kind(&self) -> Option<LightKind> {
        Some(match self.kind {
            0 => LightKind::Directional,
            1 => LightKind::Point,
            2 => LightKind::Spot,
            3 => LightKind::Area,
            _ => return None,
        })
    }
}

/// One shadow view: `EngineShadow` in `engine::view` (128 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuShadow {
    /// Light clip-from-world (depth 0..1).
    pub clip_from_world: [[f32; 4]; 4],
    /// Atlas rectangle: uv offset x, y, uv size, atlas layer.
    pub rect: [f32; 4],
    /// Depth bias (texels), normal bias (texels), world size of one texel
    /// (orthographic: absolute; perspective: at unit distance), 1 when
    /// perspective.
    pub bias: [f32; 4],
    /// Filter mode (0 hard, 1 PCF, 2 PCSS), PCF radius (texels), PCSS
    /// source size, PCSS max radius (texels).
    pub sampling: [f32; 4],
    /// Projection near and far (metres along the view axis), cascade split
    /// near and far (camera view depth; zero for non-cascades).
    pub range: [f32; 4],
}

/// Shadow view settings used to render one atlas rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowView {
    pub clip_from_world: Mat4,
    /// Edge in atlas texels.
    pub resolution: u32,
    /// Projection near/far along the view axis (metres).
    pub near: f32,
    pub far: f32,
    pub perspective: bool,
    /// World size of one texel (orthographic) or of one texel at unit
    /// distance (perspective).
    pub texel_world: f32,
    /// Camera view-depth range of a cascade (`[0, 0]` otherwise).
    pub split: [f32; 2],
}

impl ShadowView {
    pub(crate) fn gpu(&self, settings: &ShadowSettings, rect: [f32; 4]) -> GpuShadow {
        let (radius, size, max_radius) = match settings.filter {
            ShadowFilter::Hard => (0.0, 0.0, 0.0),
            ShadowFilter::Pcf { radius } => (radius as f32, 0.0, 0.0),
            ShadowFilter::Pcss {
                source_size,
                max_radius,
            } => (0.0, source_size, max_radius as f32),
        };
        GpuShadow {
            clip_from_world: self.clip_from_world.to_cols_array_2d(),
            rect,
            bias: [
                settings.depth_bias,
                settings.normal_bias,
                self.texel_world,
                if self.perspective { 1.0 } else { 0.0 },
            ],
            sampling: [settings.filter.mode(), radius, size, max_radius],
            range: [self.near, self.far, self.split[0], self.split[1]],
        }
    }
}

/// Every light of one view's frame. Shadow views are derived per view by
/// the pipeline (cascades follow the camera).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightList {
    pub lights: Vec<Light>,
}

impl LightList {
    /// Check the count bound and every light.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.lights.len() <= MAX_SUBMITTED_LIGHTS,
            "at most {MAX_SUBMITTED_LIGHTS} lights per frame"
        );
        for (index, light) in self.lights.iter().enumerate() {
            light
                .validate()
                .map_err(|error| anyhow::anyhow!("light {index}: {error}"))?;
        }
        Ok(())
    }
}

/// Conversions from photometric units to shading units.
pub mod photometric {
    use std::f32::consts::PI;

    /// Exposure that maps scene luminance to output for an EV100 camera
    /// setting (`1 / (1.2 * 2^ev100)`, the saturation-based convention).
    pub fn exposure_from_ev100(ev100: f32) -> f32 {
        1.0 / (1.2 * 2f32.powf(ev100))
    }

    /// Luminous intensity (cd) of an isotropic point light of `lumens`.
    /// Spot lights use the same conversion, so narrowing the cone does not
    /// change the intensity on its axis.
    pub fn candela_from_lumens(lumens: f32) -> f32 {
        lumens / (4.0 * PI)
    }

    /// Luminance (cd/m^2) of a Lambertian rectangle of `area` m^2 emitting
    /// `lumens` from one side (or split over both sides).
    pub fn area_luminance_from_lumens(lumens: f32, area: f32, two_sided: bool) -> f32 {
        let sides = if two_sided { 2.0 } else { 1.0 };
        lumens / (sides * PI * area.max(1e-6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn gpu_layouts_are_the_documented_sizes() {
        assert_eq!(size_of::<GpuLight>(), 80);
        assert_eq!(offset_of!(GpuLight, intensity), 12);
        assert_eq!(offset_of!(GpuLight, position), 16);
        assert_eq!(offset_of!(GpuLight, range), 28);
        assert_eq!(offset_of!(GpuLight, direction), 32);
        assert_eq!(offset_of!(GpuLight, kind), 44);
        assert_eq!(offset_of!(GpuLight, tangent), 48);
        assert_eq!(offset_of!(GpuLight, shadow_index), 60);
        assert_eq!(offset_of!(GpuLight, params), 64);
        assert_eq!(size_of::<GpuShadow>(), 128);
        assert_eq!(offset_of!(GpuShadow, rect), 64);
        assert_eq!(offset_of!(GpuShadow, range), 112);
    }

    #[test]
    fn gpu_records_encode_each_kind() {
        let sun = Light::directional(Vec3::NEG_Y, Vec3::ONE, 3.0).gpu();
        assert_eq!(sun.direction, [0.0, 1.0, 0.0], "toward the light");
        assert_eq!(sun.kind, 0);
        let spot = Light::spot(Vec3::ZERO, Vec3::NEG_Z, Vec3::ONE, 10.0, 5.0, 0.2, 0.4).gpu();
        assert!((spot.params[0] - 0.2f32.cos()).abs() < 1e-6);
        assert!((spot.params[1] - 0.4f32.cos()).abs() < 1e-6);
        assert_eq!(spot.direction, [0.0, 0.0, -1.0]);
        let area = Light {
            shape: LightShape::Area {
                width: 2.0,
                height: 1.0,
                right: Vec3::X,
                two_sided: true,
            },
            ..Light::point(Vec3::ZERO, Vec3::ONE, 1.0, 4.0)
        };
        area.validate().unwrap();
        assert_eq!(area.gpu().params, [1.0, 0.5, 1.0, 0.0]);
        assert_eq!(area.gpu().tangent, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn invalid_lights_are_rejected() {
        let ok = Light::point(Vec3::ZERO, Vec3::ONE, 10.0, 5.0);
        ok.validate().unwrap();
        let bad = |light: Light| assert!(light.validate().is_err(), "{light:?}");
        bad(Light { range: 0.0, ..ok });
        bad(Light {
            intensity: f32::NAN,
            ..ok
        });
        bad(Light {
            color: Vec3::new(-1.0, 0.0, 0.0),
            ..ok
        });
        bad(Light::spot(
            Vec3::ZERO,
            Vec3::Z,
            Vec3::ONE,
            1.0,
            1.0,
            0.5,
            0.4,
        ));
        bad(Light {
            shape: LightShape::Area {
                width: 1.0,
                height: 1.0,
                right: Vec3::Y,
                two_sided: false,
            },
            ..ok
        });
        bad(ok.with_shadow(ShadowSettings {
            resolution: 1000,
            ..Default::default()
        }));
        bad(ok.with_shadow(ShadowSettings {
            filter: ShadowFilter::Pcf { radius: 9 },
            ..Default::default()
        }));
        let sun = Light::directional(Vec3::NEG_Y, Vec3::ONE, 1.0);
        bad(sun.with_shadow(ShadowSettings {
            filter: ShadowFilter::Pcss {
                source_size: 1.0,
                max_radius: 8,
            },
            ..Default::default()
        }));
        bad(sun.with_shadow(ShadowSettings {
            cascades: CascadeSettings {
                count: 5,
                ..Default::default()
            },
            ..Default::default()
        }));
        let list = LightList {
            lights: vec![ok, sun],
        };
        list.validate().unwrap();
    }

    #[test]
    fn photometric_conversions() {
        use photometric::*;
        assert!((candela_from_lumens(4.0 * std::f32::consts::PI) - 1.0).abs() < 1e-6);
        assert!((area_luminance_from_lumens(std::f32::consts::PI, 1.0, false) - 1.0).abs() < 1e-6);
        assert!((area_luminance_from_lumens(std::f32::consts::PI, 1.0, true) - 0.5).abs() < 1e-6);
        assert!((exposure_from_ev100(0.0) - 1.0 / 1.2).abs() < 1e-6);
        assert!((exposure_from_ev100(15.0) * 100_000.0 - 2.543).abs() < 0.01);
    }
}
