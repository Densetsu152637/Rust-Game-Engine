//! The GPU light list: a storage buffer with a stable layout that already
//! reserves directional, point, spot and area lights, plus shadow views.
//!
//! Only directional lights are shaded by the built-in material today; the
//! other kinds are carried (and validated) so the lights feature can add
//! their shading without changing the layout.

use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};

/// Most lights one view carries.
pub const MAX_LIGHTS: usize = 4096;
/// Most shadow views (atlas layers) one view carries.
pub const MAX_SHADOWS: usize = 16;

/// `EngineLight.kind` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LightKind {
    /// Infinitely distant; `direction` points from surfaces toward the light.
    Directional = 0,
    /// `position`, `range` (0 = unbounded inverse-square falloff).
    Point = 1,
    /// `position`, `direction` (light forward), `range`, cone cosines in
    /// `params.xy` (inner, outer).
    Spot = 2,
    /// Rectangular area light: `position` centre, `direction` forward,
    /// `params.xy` half width/height, `params.z` two-sided flag.
    Area = 3,
}

/// One light: `EngineLight` in `engine::view` (80 bytes, std430).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuLight {
    /// Linear RGB colour.
    pub color: [f32; 3],
    /// Radiance scale: directional lights shade with `color * intensity`.
    pub intensity: f32,
    pub position: [f32; 3],
    pub range: f32,
    /// Unit vector; see [`LightKind`] for its meaning per kind.
    pub direction: [f32; 3],
    /// A [`LightKind`] value.
    pub kind: u32,
    pub params: [f32; 4],
    /// Index into the shadow list, or -1 without a shadow.
    pub shadow_index: i32,
    /// Reserved bit flags (zero).
    pub flags: u32,
    pub _reserved: [u32; 2],
}

impl GpuLight {
    /// A directional light; `direction` points from surfaces toward it.
    pub fn directional(direction: Vec3, color: Vec3, intensity: f32) -> Self {
        Self {
            color: color.to_array(),
            intensity,
            position: [0.0; 3],
            range: 0.0,
            direction: direction.normalize_or_zero().to_array(),
            kind: LightKind::Directional as u32,
            params: [0.0; 4],
            shadow_index: -1,
            flags: 0,
            _reserved: [0; 2],
        }
    }

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

/// One shadow view: `EngineShadow` in `engine::view` (80 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuShadow {
    /// Light clip-from-world (depth 0..1).
    pub clip_from_world: [[f32; 4]; 4],
    /// x constant depth bias, y atlas layer, z resolution in texels, w 1.
    pub params: [f32; 4],
}

/// Shadow view settings used to render one atlas layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowView {
    pub clip_from_world: Mat4,
    /// Power of two in 64..=4096.
    pub resolution: u32,
    pub bias: f32,
}

impl ShadowView {
    /// Orthographic view of a directional light covering a box of `extent`
    /// (width and height) and `depth` around `center`. The orientation is
    /// built near the origin so small volumes stay valid at large coordinates.
    pub fn directional(
        direction: Vec3,
        center: Vec3,
        extent: f32,
        depth: f32,
        resolution: u32,
        bias: f32,
    ) -> Self {
        let direction = direction.normalize();
        let up = if direction.dot(Vec3::Y).abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let view = glam::camera::rh::view::look_at_mat4(direction * depth * 0.5, Vec3::ZERO, up)
            * Mat4::from_translation(-center);
        let half = extent * 0.5;
        let projection =
            glam::camera::rh::proj::directx::orthographic(-half, half, -half, half, 0.0, depth);
        Self {
            clip_from_world: projection * view,
            resolution,
            bias,
        }
    }
}

/// Every light and shadow view of one view's frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightList {
    pub lights: Vec<GpuLight>,
    pub shadows: Vec<ShadowView>,
}

impl LightList {
    /// Check counts, kinds, finiteness and shadow references.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.lights.len() <= MAX_LIGHTS,
            "at most {MAX_LIGHTS} lights per view"
        );
        ensure!(
            self.shadows.len() <= MAX_SHADOWS,
            "at most {MAX_SHADOWS} shadow views per view"
        );
        for light in &self.lights {
            ensure!(light.kind().is_some(), "unknown light kind {}", light.kind);
            ensure!(
                light
                    .color
                    .iter()
                    .chain(&light.position)
                    .chain(&light.direction)
                    .chain(&light.params)
                    .chain([&light.intensity, &light.range])
                    .all(|v| v.is_finite()),
                "light values must be finite"
            );
            ensure!(
                light.shadow_index < self.shadows.len() as i32 && light.shadow_index >= -1,
                "light shadow index {} has no shadow view",
                light.shadow_index
            );
        }
        let resolution = self.shadows.first().map(|s| s.resolution);
        for shadow in &self.shadows {
            ensure!(
                (64..=4096).contains(&shadow.resolution) && shadow.resolution.is_power_of_two(),
                "shadow resolution must be a power of two in 64..=4096"
            );
            ensure!(
                Some(shadow.resolution) == resolution,
                "every shadow view of a frame uses one atlas resolution"
            );
            ensure!(
                shadow.clip_from_world.is_finite() && shadow.bias.is_finite(),
                "shadow views must be finite"
            );
        }
        Ok(())
    }

    /// Shadow atlas resolution and layer count (at least one 64x64 layer).
    pub fn atlas(&self) -> (u32, u32) {
        (
            self.shadows.first().map_or(64, |s| s.resolution),
            self.shadows.len().max(1) as u32,
        )
    }

    /// `EngineLightList` bytes: a 16-byte header (`counts.x` = light count)
    /// followed by the lights. Holds at least one zeroed light so the binding
    /// is never empty.
    pub fn light_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16 + 80 * self.lights.len().max(1));
        bytes.extend_from_slice(bytemuck::cast_slice(&[self.lights.len() as u32, 0, 0, 0]));
        if self.lights.is_empty() {
            bytes.extend_from_slice(bytemuck::bytes_of(&GpuLight::zeroed()));
        } else {
            bytes.extend_from_slice(bytemuck::cast_slice(&self.lights));
        }
        bytes
    }

    /// `array<EngineShadow>` bytes (at least one zeroed entry).
    pub fn shadow_bytes(&self) -> Vec<u8> {
        let shadows: Vec<GpuShadow> = self
            .shadows
            .iter()
            .enumerate()
            .map(|(layer, s)| GpuShadow {
                clip_from_world: s.clip_from_world.to_cols_array_2d(),
                params: [s.bias, layer as f32, s.resolution as f32, 1.0],
            })
            .collect();
        if shadows.is_empty() {
            return bytemuck::bytes_of(&GpuShadow::zeroed()).to_vec();
        }
        bytemuck::cast_slice(&shadows).to_vec()
    }
}

use bytemuck::Zeroable;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn light_layout_is_the_documented_80_bytes() {
        assert_eq!(size_of::<GpuLight>(), 80);
        assert_eq!(offset_of!(GpuLight, intensity), 12);
        assert_eq!(offset_of!(GpuLight, position), 16);
        assert_eq!(offset_of!(GpuLight, range), 28);
        assert_eq!(offset_of!(GpuLight, direction), 32);
        assert_eq!(offset_of!(GpuLight, kind), 44);
        assert_eq!(offset_of!(GpuLight, params), 48);
        assert_eq!(offset_of!(GpuLight, shadow_index), 64);
        assert_eq!(size_of::<GpuShadow>(), 80);
    }

    #[test]
    fn buffers_have_a_header_and_are_never_empty() {
        let empty = LightList::default();
        assert_eq!(empty.light_bytes().len(), 16 + 80);
        assert_eq!(&empty.light_bytes()[..4], &0u32.to_ne_bytes());
        assert_eq!(empty.shadow_bytes().len(), 80);
        assert_eq!(empty.atlas(), (64, 1));
        let mut list = LightList::default();
        list.lights
            .push(GpuLight::directional(Vec3::Z, Vec3::ONE, 3.0));
        list.lights.push(GpuLight {
            kind: LightKind::Point as u32,
            ..GpuLight::directional(Vec3::Z, Vec3::ONE, 1.0)
        });
        assert_eq!(list.light_bytes().len(), 16 + 160);
        assert_eq!(&list.light_bytes()[..4], &2u32.to_ne_bytes());
        list.validate().unwrap();
    }

    #[test]
    fn invalid_lists_are_rejected() {
        let mut list = LightList::default();
        list.lights.push(GpuLight {
            shadow_index: 0,
            ..GpuLight::directional(Vec3::Z, Vec3::ONE, 1.0)
        });
        assert!(list.validate().is_err(), "dangling shadow index");
        list.shadows.push(ShadowView::directional(
            Vec3::Z,
            Vec3::ZERO,
            10.0,
            10.0,
            512,
            0.001,
        ));
        list.validate().unwrap();
        assert_eq!(list.atlas(), (512, 1));
        list.shadows.push(ShadowView {
            resolution: 256,
            ..list.shadows[0]
        });
        assert!(list.validate().is_err(), "mixed resolutions");
        list.shadows.pop();
        list.lights[0].kind = 9;
        assert!(list.validate().is_err(), "unknown kind");
    }
}
