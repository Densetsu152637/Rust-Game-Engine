//! Authored scene lights: `DirectionalLight`, `PointLight`, `SpotLight` and
//! `AreaLight` components, their shadow settings, the extracted per-frame
//! light list and the renderer-reported [`LightingStatus`].
//!
//! Units are photometric: directional lights in lux, local lights in lumens
//! (luminous power). The renderer multiplies them by the active camera's
//! `lighting.exposure` (see docs/rendering-lights.md).
use crate::{Error, Result};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

/// Largest illuminance (lux) of a directional light.
pub const MAX_LUX: f32 = 200_000.0;
/// Largest luminous power (lumens) of a local light.
pub const MAX_LUMENS: f32 = 100_000_000.0;
/// Largest local light range (metres).
pub const MAX_LIGHT_RANGE: f32 = 100_000.0;

/// How shadow map samples are filtered.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShadowFilterMode {
    /// One bilinear hardware comparison.
    Hard,
    /// Percentage-closer filtering over a `(2 pcf_radius + 1)^2` texel grid.
    #[default]
    Pcf,
    /// Percentage-closer soft shadows: penumbrae widen with the
    /// blocker-receiver distance and the light's size.
    Pcss,
}

fn invalid(code: &str, message: &str) -> Error {
    Error::new(code, message)
}

fn colour_ok(color: &[f32; 3]) -> bool {
    color
        .iter()
        .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
}

fn shared_shadow_ok(
    resolution: u32,
    depth_bias: f32,
    normal_bias: f32,
    pcf_radius: u32,
    pcss_max_radius: u32,
) -> bool {
    (64..=4096).contains(&resolution)
        && resolution.is_power_of_two()
        && (0.0..=16.0).contains(&depth_bias)
        && (0.0..=16.0).contains(&normal_bias)
        && pcf_radius <= 4
        && (1..=32).contains(&pcss_max_radius)
}

/// Shadow settings of point, spot and area lights. Point and area lights
/// render six cube faces of `resolution`^2, spot lights one view.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LightShadows {
    pub enabled: bool,
    /// Texels per shadow view edge: a power of two in 64..=4096.
    pub resolution: u32,
    /// Receiver offset toward the light, in shadow texels (0..=16).
    pub depth_bias: f32,
    /// Receiver offset along its normal, in shadow texels (0..=16).
    pub normal_bias: f32,
    pub filter: ShadowFilterMode,
    /// PCF kernel radius in texels (0..=4).
    pub pcf_radius: u32,
    /// Largest PCSS filter radius in texels (1..=32).
    pub pcss_max_radius: u32,
    /// Emitter radius in metres for PCSS penumbrae (0.001..=10).
    pub source_radius: f32,
    /// Shadow projection near plane in metres (0.001..=100).
    pub near: f32,
}
impl Default for LightShadows {
    fn default() -> Self {
        Self {
            enabled: false,
            resolution: 512,
            depth_bias: 1.0,
            normal_bias: 1.5,
            filter: ShadowFilterMode::Pcf,
            pcf_radius: 1,
            pcss_max_radius: 16,
            source_radius: 0.05,
            near: 0.05,
        }
    }
}
impl LightShadows {
    pub fn validate(&self) -> Result<()> {
        if !shared_shadow_ok(
            self.resolution,
            self.depth_bias,
            self.normal_bias,
            self.pcf_radius,
            self.pcss_max_radius,
        ) || !(0.001..=10.0).contains(&self.source_radius)
            || !(0.001..=100.0).contains(&self.near)
        {
            return Err(invalid(
                "light.shadows.invalid",
                "Light shadows need a power-of-two resolution 64..4096, biases 0..16 texels, \
                 pcf_radius 0..4, pcss_max_radius 1..32, source_radius 0.001..10 m and near 0.001..100 m",
            ));
        }
        Ok(())
    }
}

/// Cascaded shadow settings of a directional light.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DirectionalShadows {
    pub enabled: bool,
    /// Texels per cascade edge: a power of two in 64..=4096.
    pub resolution: u32,
    pub depth_bias: f32,
    pub normal_bias: f32,
    pub filter: ShadowFilterMode,
    pub pcf_radius: u32,
    pub pcss_max_radius: u32,
    /// Angular diameter of the light source in radians, for PCSS
    /// (0.0001..=0.2; the sun is about 0.0093).
    pub angular_diameter_radians: f32,
    /// Number of cascades (1..=4).
    pub cascades: u32,
    /// Split scheme weight: 0 uniform, 1 logarithmic.
    pub split_lambda: f32,
    /// Camera view depth (metres) the cascades cover; beyond it surfaces are
    /// unshadowed. Clamped to the camera's far plane.
    pub max_distance: f32,
    /// Fraction (0..=0.5) at the end of each cascade blended into the next.
    pub cascade_blend: f32,
    /// Metres beyond each cascade, toward the light, whose casters are kept.
    pub caster_extension: f32,
}
impl Default for DirectionalShadows {
    fn default() -> Self {
        Self {
            enabled: false,
            resolution: 2048,
            depth_bias: 1.0,
            normal_bias: 1.5,
            filter: ShadowFilterMode::Pcf,
            pcf_radius: 1,
            pcss_max_radius: 16,
            angular_diameter_radians: 0.0093,
            cascades: 4,
            split_lambda: 0.75,
            max_distance: 100.0,
            cascade_blend: 0.1,
            caster_extension: 100.0,
        }
    }
}
impl DirectionalShadows {
    pub fn validate(&self) -> Result<()> {
        if !shared_shadow_ok(
            self.resolution,
            self.depth_bias,
            self.normal_bias,
            self.pcf_radius,
            self.pcss_max_radius,
        ) || !(0.0001..=0.2).contains(&self.angular_diameter_radians)
            || !(1..=4).contains(&self.cascades)
            || !(0.0..=1.0).contains(&self.split_lambda)
            || !(0.01..=100_000.0).contains(&self.max_distance)
            || !(0.0..=0.5).contains(&self.cascade_blend)
            || !(0.0..=100_000.0).contains(&self.caster_extension)
        {
            return Err(invalid(
                "light.shadows.invalid",
                "Directional shadows need a power-of-two resolution 64..4096, biases 0..16 texels, \
                 pcf_radius 0..4, pcss_max_radius 1..32, angular_diameter_radians 0.0001..0.2, \
                 1..4 cascades, split_lambda 0..1, max_distance 0.01..100000, cascade_blend 0..0.5 \
                 and caster_extension 0..100000",
            ));
        }
        Ok(())
    }
}

fn one() -> u32 {
    1
}

/// An infinitely distant light travelling along its entity's local -Z axis
/// (world transform rotation; identity without a `Transform`).
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DirectionalLight {
    #[serde(default = "one")]
    pub version: u32,
    pub enabled: bool,
    /// Linear RGB in 0..1.
    pub color: [f32; 3],
    /// Illuminance in lux on a surface facing the light (0..=200000).
    pub lux: f32,
    pub shadows: DirectionalShadows,
}
impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            color: [1.0; 3],
            lux: 3.0,
            shadows: DirectionalShadows::default(),
        }
    }
}
impl DirectionalLight {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || !colour_ok(&self.color) || !(0.0..=MAX_LUX).contains(&self.lux) {
            return Err(invalid(
                "light.directional.invalid",
                "DirectionalLight requires version 1, color in 0..1 and lux in 0..200000",
            ));
        }
        self.shadows.validate()
    }
}

fn local_ok(version: u32, color: &[f32; 3], lumens: f32, range: f32) -> bool {
    version == 1
        && colour_ok(color)
        && (0.0..=MAX_LUMENS).contains(&lumens)
        && range.is_finite()
        && range > 0.0
        && range <= MAX_LIGHT_RANGE
}

/// An isotropic point light at its entity's world position.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PointLight {
    #[serde(default = "one")]
    pub version: u32,
    pub enabled: bool,
    pub color: [f32; 3],
    /// Luminous power in lumens (0..=1e8); intensity is lumens / 4 pi cd.
    pub lumens: f32,
    /// Metres where the windowed inverse-square falloff reaches zero.
    pub range: f32,
    pub shadows: LightShadows,
}
impl Default for PointLight {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            color: [1.0; 3],
            lumens: 800.0,
            range: 10.0,
            shadows: LightShadows::default(),
        }
    }
}
impl PointLight {
    pub fn validate(&self) -> Result<()> {
        if !local_ok(self.version, &self.color, self.lumens, self.range) {
            return Err(invalid(
                "light.point.invalid",
                "PointLight requires version 1, color in 0..1, lumens in 0..1e8 and range in (0, 100000]",
            ));
        }
        self.shadows.validate()
    }
}

/// A cone light at its entity's world position along its local -Z axis.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SpotLight {
    #[serde(default = "one")]
    pub version: u32,
    pub enabled: bool,
    pub color: [f32; 3],
    /// Luminous power of the equivalent point light: the axis intensity is
    /// lumens / 4 pi cd whatever the cone angles.
    pub lumens: f32,
    pub range: f32,
    /// Half angle of the fully lit cone, radians (0 <= inner < outer).
    pub inner_angle_radians: f32,
    /// Half angle where the light reaches zero, radians (<= pi/2).
    pub outer_angle_radians: f32,
    pub shadows: LightShadows,
}
impl Default for SpotLight {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            color: [1.0; 3],
            lumens: 800.0,
            range: 10.0,
            inner_angle_radians: 0.4,
            outer_angle_radians: 0.55,
            shadows: LightShadows::default(),
        }
    }
}
impl SpotLight {
    pub fn validate(&self) -> Result<()> {
        if !local_ok(self.version, &self.color, self.lumens, self.range)
            || !self.inner_angle_radians.is_finite()
            || !self.outer_angle_radians.is_finite()
            || self.inner_angle_radians < 0.0
            || self.inner_angle_radians >= self.outer_angle_radians
            || self.outer_angle_radians > std::f32::consts::FRAC_PI_2
        {
            return Err(invalid(
                "light.spot.invalid",
                "SpotLight requires version 1, color in 0..1, lumens in 0..1e8, range in (0, 100000] \
                 and 0 <= inner_angle_radians < outer_angle_radians <= pi/2",
            ));
        }
        self.shadows.validate()
    }
}

/// A rectangle of uniform luminance centred on its entity, in the local XY
/// plane (width along X, height along Y), emitting along local -Z (and +Z
/// when two-sided).
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AreaLight {
    #[serde(default = "one")]
    pub version: u32,
    pub enabled: bool,
    pub color: [f32; 3],
    /// Luminous power; luminance is lumens / (pi * width * height) per side.
    pub lumens: f32,
    /// Metres from the centre where the falloff window reaches zero; must
    /// exceed half the rectangle's diagonal.
    pub range: f32,
    pub width: f32,
    pub height: f32,
    pub two_sided: bool,
    /// Six cube faces from the centre (an approximation for large lights).
    pub shadows: LightShadows,
}
impl Default for AreaLight {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            color: [1.0; 3],
            lumens: 800.0,
            range: 10.0,
            width: 1.0,
            height: 1.0,
            two_sided: false,
            shadows: LightShadows::default(),
        }
    }
}
impl AreaLight {
    pub fn validate(&self) -> Result<()> {
        let size_ok = |v: f32| v.is_finite() && v > 0.0 && v <= 1000.0;
        if !local_ok(self.version, &self.color, self.lumens, self.range)
            || !size_ok(self.width)
            || !size_ok(self.height)
            || self.range <= 0.5 * self.width.hypot(self.height)
        {
            return Err(invalid(
                "light.area.invalid",
                "AreaLight requires version 1, color in 0..1, lumens in 0..1e8, width and height in \
                 (0, 1000] and a range in (0, 100000] above half the diagonal",
            ));
        }
        self.shadows.validate()
    }
}

/// An extracted light component.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneLight {
    Directional(DirectionalLight),
    Point(PointLight),
    Spot(SpotLight),
    Area(AreaLight),
}

/// One enabled light of a frame with its entity's world transform.
#[derive(Clone, Debug, PartialEq)]
pub struct LightInstance3D {
    /// Authored entity ID (or `entity:<bits>`).
    pub id: String,
    pub light: SceneLight,
    pub world: [[f32; 4]; 4],
}

impl LightInstance3D {
    /// A directional light shining from `toward` (a vector from surfaces
    /// toward the light, normalised here; nonzero and finite).
    pub fn directional_toward(
        id: impl Into<String>,
        toward: [f32; 3],
        light: DirectionalLight,
    ) -> Result<Self> {
        let length = toward.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !length.is_finite() || length < 1e-6 {
            return Err(invalid(
                "light.direction.invalid",
                "A light direction must be finite and nonzero",
            ));
        }
        let z = toward.map(|v| v / length);
        let up = if z[1].abs() > 0.99 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let x = cross(up, z);
        let x_length = x.iter().map(|v| v * v).sum::<f32>().sqrt();
        let x = x.map(|v| v / x_length);
        let y = cross(z, x);
        Ok(Self {
            id: id.into(),
            light: SceneLight::Directional(light),
            world: [
                [x[0], x[1], x[2], 0.0],
                [y[0], y[1], y[2], 0.0],
                [z[0], z[1], z[2], 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        })
    }
}

/// Renderer-reported light culling, clustering and shadow allocation of
/// the active camera's view (kept small for run reports).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct LightingStatus {
    pub state: crate::rendering3d::RenderPipelineState,
    /// Enabled authored lights extracted for the frame.
    pub authored: u64,
    /// Lights the renderer received, culled (outside the view or black) and
    /// dropped over the per-view budget.
    pub submitted: u64,
    pub culled: u64,
    pub over_budget: u64,
    pub directional: u64,
    pub local: u64,
    /// `clustered` or `all_lights` (engine.lights.clusters disabled).
    pub cluster_mode: String,
    /// Local lights the cluster pass assigned.
    pub clustered: u64,
    pub cluster_dims: [u32; 3],
    /// From the latest completed GPU readback of the cluster pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_overflowed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_longest_list: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_gpu_ms: Option<f64>,
    pub shadows_enabled: bool,
    pub shadowed_lights: u64,
    pub shadow_views: u64,
    pub shadow_dropped: u64,
    pub atlas_page_size: u32,
    pub atlas_pages: u32,
    pub atlas_texels_used: u64,
    pub atlas_texels_capacity: u64,
    /// Why lights or shadows were dropped (at most 4 entries).
    pub overflow: Vec<String>,
}

/// Host-written lighting status (sibling of `RenderPipelineControl`).
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct LightingControl {
    pub status: LightingStatus,
}
