//! Optional native lit sprites, independent of project/desktop extraction.
//!
//! Geometry is already world-space, as on the ordinary sprite path. Pass an
//! unshifted clip-from-world matrix; the uniform applies presenter offset once.
use crate::{GpuLight, Light, LightShape};
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::collections::BTreeSet;

pub const MAX_SPRITE_LIGHTS: usize = 64;
pub const MAX_LIT_SPRITE_DRAWS: usize = 4096;
pub const SPRITE_LIGHTING_UNIFORM_BYTES: u64 = std::mem::size_of::<SpriteLightingUniform>() as u64;
pub const SPRITE_LIT_WGSL: &str = include_str!("lit.wgsl");
pub(crate) const MATERIAL_WGSL: &str = include_str!("material.wgsl");
const MAX_COORDINATE: f32 = 1_000_000.0;
const MAX_INTENSITY: f32 = 1_000_000.0;

/// Stable name and validated point/directional light in world-space shading units.
/// Directional `Light::direction` is the direction rays travel, as in 3D.
#[derive(Clone, Debug)]
pub struct SpriteLight {
    id: String,
    light: Light,
}
impl SpriteLight {
    pub fn new(id: String, mut light: Light) -> Result<Self> {
        ensure!(
            !id.is_empty() && id.len() <= 64,
            "sprite light ID must contain 1..=64 bytes"
        );
        ensure!(
            matches!(light.shape, LightShape::Directional | LightShape::Point),
            "SpriteLit supports point and directional lights only"
        );
        ensure!(
            light.shadow.is_none(),
            "SpriteLit does not support shadow requests"
        );
        light
            .validate()
            .with_context(|| format!("sprite light {id}"))?;
        ensure!(
            light.color.max_element() <= 1.0 && light.intensity <= MAX_INTENSITY,
            "sprite light colour must be in 0..1 and intensity in 0..1000000"
        );
        ensure!(
            light.position.abs().max_element() <= MAX_COORDINATE && light.range.is_finite(),
            "sprite light position must be within +/-1000000 and range finite"
        );
        light.direction = light.direction.normalize();
        Ok(Self { id, light })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn light(&self) -> &Light {
        &self.light
    }
}

/// Immutable light snapshot. Only light IDs are sorted; draw order stays caller-owned.
#[derive(Clone, Debug)]
pub struct SpriteLighting {
    ambient: Vec3,
    lights: Vec<SpriteLight>,
}
impl SpriteLighting {
    pub fn new(ambient: Vec3, mut lights: Vec<SpriteLight>) -> Result<Self> {
        ensure!(
            ambient.is_finite()
                && ambient.min_element() >= 0.0
                && ambient.max_element() <= MAX_INTENSITY,
            "sprite ambient must be finite and within 0..1000000"
        );
        ensure!(
            lights.len() <= MAX_SPRITE_LIGHTS,
            "at most {MAX_SPRITE_LIGHTS} sprite lights"
        );
        let mut ids = BTreeSet::new();
        for light in &lights {
            ensure!(
                ids.insert(light.id()),
                "duplicate sprite light ID {}",
                light.id()
            );
        }
        drop(ids);
        lights.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self { ambient, lights })
    }
    pub fn lights(&self) -> &[SpriteLight] {
        &self.lights
    }
    pub fn ambient(&self) -> Vec3 {
        self.ambient
    }

    /// Validate the whole draw set before allocating its uniforms. Each result
    /// corresponds to the offset at the SAME index. No upload/submission occurs.
    /// Supply these POD values at group 2/binding 1 using the existing DrawUniform.
    pub fn prepare_draws(
        &self,
        limits: &wgpu::Limits,
        offsets: &[Vec3],
    ) -> Result<Vec<SpriteLightingUniform>> {
        check_limits(limits)?;
        ensure!(
            offsets.len() <= MAX_LIT_SPRITE_DRAWS,
            "at most {MAX_LIT_SPRITE_DRAWS} lit sprite draws per view"
        );
        ensure!(
            offsets
                .iter()
                .all(|v| v.is_finite() && v.abs().max_element() <= MAX_COORDINATE),
            "sprite presenter offsets must be finite and within +/-1000000"
        );
        let alignment = u64::from(limits.min_uniform_buffer_offset_alignment).max(16);
        let stride = SPRITE_LIGHTING_UNIFORM_BYTES.div_ceil(alignment) * alignment;
        let bytes = stride * offsets.len() as u64;
        ensure!(
            bytes <= limits.max_buffer_size && bytes <= u64::from(u32::MAX),
            "sprite lighting uniforms exceed the device buffer/dynamic-offset budget"
        );
        let mut uniform = SpriteLightingUniform::zeroed();
        uniform.ambient = self.ambient.extend(0.0).to_array();
        uniform.count[0] = self.lights.len() as u32;
        for (dst, src) in uniform.lights.iter_mut().zip(&self.lights) {
            *dst = src.light.gpu();
        }
        Ok(offsets
            .iter()
            .map(|offset| SpriteLightingUniform {
                offset: offset.extend(0.0).to_array(),
                ..uniform
            })
            .collect())
    }
}

/// Exact native SpriteLit group 2/binding 1 layout. Other material variants
/// have separate layouts; this is never bound to the legacy Sprite material.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SpriteLightingUniform {
    ambient: [f32; 4],
    count: [u32; 4],
    offset: [f32; 4],
    lights: [GpuLight; MAX_SPRITE_LIGHTS],
}
impl SpriteLightingUniform {
    pub fn light_count(&self) -> u32 {
        self.count[0]
    }
    pub fn offset(&self) -> Vec3 {
        Vec3::new(self.offset[0], self.offset[1], self.offset[2])
    }
}

/// Check the actual three-group SpriteLit material before creating resources.
/// It uses four dynamic bindings overall, three uniform bindings in fragments,
/// three sampled D2 textures and one nearest/clamp sampler. No optional feature.
pub fn check_limits(limits: &wgpu::Limits) -> Result<()> {
    ensure!(
        limits.max_bind_groups >= 3 && limits.max_bindings_per_bind_group >= 4,
        "SpriteLit needs three bind groups and four bindings per group"
    );
    ensure!(
        u64::from(limits.max_uniform_buffer_binding_size) >= SPRITE_LIGHTING_UNIFORM_BYTES,
        "SpriteLit needs a {SPRITE_LIGHTING_UNIFORM_BYTES}-byte uniform binding"
    );
    ensure!(
        limits.max_dynamic_uniform_buffers_per_pipeline_layout >= 4
            && limits.max_uniform_buffers_per_shader_stage >= 3,
        "SpriteLit needs four dynamic uniform bindings and three uniform bindings per fragment stage"
    );
    ensure!(
        limits.max_sampled_textures_per_shader_stage >= 3
            && limits.max_samplers_per_shader_stage >= 1,
        "SpriteLit needs three sampled textures and one sampler per fragment stage"
    );
    Ok(())
}
/// Existing material/map uniforms at 2/0 and 2/2. Texture handles themselves go
/// through DrawCommand.data_textures in MR/normal order; these flags enable them.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpriteTextureMaps {
    pub normal: bool,
    pub metallic_roughness: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct SpriteMaterialUniforms {
    material: [[f32; 4]; 2],
    maps: [f32; 4],
}
impl SpriteMaterialUniforms {
    pub fn new(params: crate::MaterialParams, textures: SpriteTextureMaps) -> Result<Self> {
        crate::validate_params(&params)?;
        let flag = |value: bool| if value { 1.0 } else { 0.0 };
        Ok(Self {
            material: [
                params.tint,
                [flag(params.unlit), params.metallic, params.roughness, 0.0],
            ],
            maps: [
                flag(textures.normal),
                params.normal_scale,
                flag(textures.metallic_roughness),
                0.0,
            ],
        })
    }
    pub fn material(&self) -> &[[f32; 4]; 2] {
        &self.material
    }
    pub fn maps(&self) -> &[f32; 4] {
        &self.maps
    }
}
