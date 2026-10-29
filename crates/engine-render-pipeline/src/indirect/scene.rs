//! Native per-view IBL configuration and per-draw resolved material inputs.
use super::{GpuIndirectResources, MAX_PROBES, ProbeVolume, select_probe};
use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use std::sync::Arc;

pub const MAX_INDIRECT_MATERIALS: usize = 4096;
pub const INDIRECT_UNIFORM_BYTES: u64 = std::mem::size_of::<IndirectUniform>() as u64;
pub(crate) const MATERIAL_WGSL: &str = include_str!("material.wgsl");

/// Immutable environment reference. Rotation maps environment-local directions
/// into world space. Intensity includes any desired exposure, exactly once.
#[derive(Clone, Copy, Debug)]
pub struct EnvironmentLighting {
    environment: usize,
    world_from_environment: Quat,
    intensity: f32,
}
impl EnvironmentLighting {
    pub fn new(environment: usize, world_from_environment: Quat, intensity: f32) -> Result<Self> {
        ensure!(
            world_from_environment.is_finite()
                && (world_from_environment.length_squared() - 1.0).abs() <= 1e-3,
            "environment rotation must be a finite unit quaternion"
        );
        ensure!(
            intensity.is_finite() && (0.0..=1000.0).contains(&intensity),
            "environment intensity must be finite and within 0..1000"
        );
        Ok(Self {
            environment,
            world_from_environment: world_from_environment.normalize(),
            intensity,
        })
    }
    pub fn environment(&self) -> usize {
        self.environment
    }
}

#[derive(Clone, Debug)]
pub struct IndirectProbe {
    pub volume: ProbeVolume,
    pub lighting: EnvironmentLighting,
}

/// Validated immutable native scene lighting, configured independently per ViewId.
/// Caller resources must belong to the ScenePipeline's own device.
pub struct IndirectScene {
    resources: Arc<GpuIndirectResources>,
    global: Option<EnvironmentLighting>,
    probes: Vec<IndirectProbe>,
    volumes: Vec<ProbeVolume>,
}
impl IndirectScene {
    pub fn new(
        resources: Arc<GpuIndirectResources>,
        global: Option<EnvironmentLighting>,
        probes: Vec<IndirectProbe>,
    ) -> Result<Self> {
        ensure!(
            resources.environments.len() <= super::MAX_ENVIRONMENTS
                && resources.lightmaps.len() <= super::MAX_LIGHTMAPS,
            "indirect resource counts exceed scene limits"
        );
        ensure!(
            probes.len() <= MAX_PROBES,
            "at most {MAX_PROBES} local indirect probes"
        );
        for lighting in global.iter().chain(probes.iter().map(|p| &p.lighting)) {
            ensure!(
                lighting.environment < resources.environments.len(),
                "environment index {} is missing",
                lighting.environment
            );
        }
        let volumes: Vec<_> = probes.iter().map(|p| p.volume.clone()).collect();
        // Also validates duplicate IDs, independently of which probe contains a draw.
        select_probe(&volumes, Vec3::ZERO)?;
        Ok(Self {
            resources,
            global,
            probes,
            volumes,
        })
    }
    pub fn resources(&self) -> &GpuIndirectResources {
        &self.resources
    }

    pub(crate) fn resolve(&self, world: Vec3, ambient: Vec3) -> Result<ResolvedIndirect> {
        let selection = select_probe(&self.volumes, world)?;
        let local = selection.local.map(|i| self.probes[i].lighting);
        let choices = [self.global, local];
        let mut uniform = IndirectUniform::zeroed();
        uniform.diffuse0[0] = ambient.extend(0.0).to_array();
        uniform.environment_from_world0 = Mat4::IDENTITY.to_cols_array_2d();
        uniform.environment_from_world1 = Mat4::IDENTITY.to_cols_array_2d();
        uniform.weights_lods = [selection.global_weight, selection.local_weight, 0.0, 0.0];
        uniform.scales = [1.0, 0.0, 0.0, 0.0];
        let mut environments = [None; 2];
        let mut key = [0, 0, self.resources.brdf.id()];
        for (slot, choice) in choices.into_iter().enumerate() {
            let Some(choice) = choice else {
                continue;
            };
            let environment = &self.resources.environments[choice.environment];
            let coefficients = environment
                .diffuse
                .coefficients()
                .map(|c| [c[0], c[1], c[2], 0.0]);
            let rotation =
                Mat4::from_quat(choice.world_from_environment.conjugate()).to_cols_array_2d();
            if slot == 0 {
                uniform.diffuse0 = coefficients;
                uniform.environment_from_world0 = rotation;
            } else {
                uniform.diffuse1 = coefficients;
                uniform.environment_from_world1 = rotation;
            }
            uniform.weights_lods[slot + 2] = (environment.texture.mip_level_count() - 1) as f32;
            uniform.scales[slot] = choice.intensity;
            uniform.scales[slot + 2] = 1.0;
            environments[slot] = Some(choice.environment);
            key[slot] = environment.id();
        }
        ensure!(
            bytemuck::cast_slice::<IndirectUniform, f32>(std::slice::from_ref(&uniform))
                .iter()
                .all(|v| v.is_finite()),
            "resolved indirect material contains nonfinite values"
        );
        Ok(ResolvedIndirect {
            uniform,
            environments,
            key,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct IndirectUniform {
    diffuse0: [[f32; 4]; 9],
    diffuse1: [[f32; 4]; 9],
    environment_from_world0: [[f32; 4]; 4],
    environment_from_world1: [[f32; 4]; 4],
    weights_lods: [f32; 4],
    scales: [f32; 4],
}

pub(crate) struct ResolvedIndirect {
    pub uniform: IndirectUniform,
    pub environments: [Option<usize>; 2],
    pub key: [u64; 3],
}

/// Optional groups 1/2 for native IBL; legacy material layouts remain unchanged.
pub struct IndirectMaterialLayouts {
    pub textures: wgpu::BindGroupLayout,
    pub params: wgpu::BindGroupLayout,
}

/// Existing view/material resources plus 2 cubes, a LUT, sampler and uniform.
/// Rejection leaves the ordinary material path available.
pub fn check_material_limits(limits: &wgpu::Limits) -> Result<()> {
    ensure!(
        limits.max_sampled_textures_per_shader_stage >= 11
            && limits.max_samplers_per_shader_stage >= 3,
        "indirect material unavailable: needs 11 sampled textures and 3 samplers per fragment stage"
    );
    ensure!(
        limits.max_uniform_buffers_per_shader_stage >= 4
            && limits.max_dynamic_uniform_buffers_per_pipeline_layout >= 3
            && limits.max_uniform_buffer_binding_size >= INDIRECT_UNIFORM_BYTES,
        "indirect material unavailable: needs four uniform bindings, three dynamic bindings and a 448-byte uniform"
    );
    Ok(())
}

pub(crate) fn material_layouts(device: &wgpu::Device) -> Option<IndirectMaterialLayouts> {
    check_material_limits(&device.limits()).ok()?;
    let fragment = wgpu::ShaderStages::FRAGMENT;
    let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
    let texture = |binding, dimension, visibility| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    };
    let sampler = |binding, visibility| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    };
    let d2 = wgpu::TextureViewDimension::D2;
    let cube = wgpu::TextureViewDimension::Cube;
    let textures = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("engine indirect material textures"),
        entries: &[
            texture(0, d2, both),
            sampler(1, both),
            texture(2, d2, both),
            texture(3, d2, both),
            texture(4, cube, fragment),
            texture(5, cube, fragment),
            texture(6, d2, fragment),
            sampler(7, fragment),
        ],
    });
    let uniform = |binding, size, visibility| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    };
    let params = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("engine indirect material params"),
        entries: &[
            uniform(0, 32, both),
            uniform(1, INDIRECT_UNIFORM_BYTES, fragment),
            uniform(2, 16, both),
        ],
    });
    Some(IndirectMaterialLayouts { textures, params })
}

pub(crate) fn black_cube(gpu: &crate::GpuContext) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("indirect neutral cube"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 6,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &[0; 48],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        texture.size(),
    );
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

#[derive(Clone, Copy)]
pub(crate) struct MaterialArena {
    pub maps: u64,
    pub indirect: u64,
    pub stride: u64,
}
impl MaterialArena {
    pub fn new(alignment: u32, indirect: bool) -> Self {
        let align = u64::from(alignment).max(16);
        let maps = 32u64.div_ceil(align) * align;
        let indirect_offset = (maps + 16).div_ceil(align) * align;
        let end = if indirect {
            indirect_offset + INDIRECT_UNIFORM_BYTES
        } else {
            maps + 16
        };
        Self {
            maps,
            indirect: indirect_offset,
            stride: end.div_ceil(align) * align,
        }
    }
}
