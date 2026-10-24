//! RT shadow mask: per-pixel visibility of one directional light.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::scene::RtScene;
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Output format of [`ShadowMaskKernel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShadowMaskFormat {
    /// Needs `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter support
    /// (see [`RtDevice::supports_storage_format`]).
    R8Unorm,
    /// Same requirement as `R8Unorm`.
    R16Float,
    /// Always available.
    R32Float,
}

impl ShadowMaskFormat {
    pub fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::R16Float => wgpu::TextureFormat::R16Float,
            Self::R32Float => wgpu::TextureFormat::R32Float,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::R8Unorm => "r8unorm",
            Self::R16Float => "r16float",
            Self::R32Float => "r32float",
        }
    }
}

/// Per-dispatch parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowMaskParams {
    /// Direction towards the light (normalized by the kernel).
    pub direction_to_light: [f32; 3],
    /// Offset of the ray origin along the surface normal, in world units.
    pub normal_bias: f32,
    pub t_min: f32,
    pub t_max: f32,
    /// Instance mask of occluders.
    pub mask: u8,
}

impl Default for ShadowMaskParams {
    fn default() -> Self {
        Self {
            direction_to_light: [0.0, 1.0, 0.0],
            normal_bias: 1e-3,
            t_min: 0.0,
            t_max: 1e4,
            mask: 0xff,
        }
    }
}

/// Textures of one dispatch. `world_position` is `Rgba32Float` (xyz world
/// position, w >= 0.5 where there is geometry); `world_normal` is a float
/// texture whose xyz is the world normal (`Rgba16Float` or `Rgba32Float`);
/// `output` is a storage view of the kernel's format. All are `width x height`.
pub struct ShadowMaskTargets<'a> {
    pub world_position: &'a wgpu::TextureView,
    pub world_normal: &'a wgpu::TextureView,
    pub output: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuShadowParams {
    direction_to_light: [f32; 4],
    ray: [f32; 4],
    size_mask: [u32; 4],
}

/// Compute kernel `shadow_mask` (`src/shaders/shadow_mask.wgsl`), workgroup
/// 8 x 8, one invocation per pixel.
///
/// Bindings: group 0 = the scene ([`RtScene::bind_group`]); group 1 =
/// 0 `texture_2d<f32>` world positions, 1 `texture_2d<f32>` world normals
/// (both unfilterable float), 2 `texture_storage_2d<format, write>` visibility,
/// 3 uniform parameters. Writes 1 (lit) or 0 (shadowed); background pixels are 1
/// and surfaces facing away from the light are 0.
pub struct ShadowMaskKernel {
    core: Arc<Core>,
    format: ShadowMaskFormat,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl ShadowMaskKernel {
    pub fn new(rt: &RtDevice, format: ShadowMaskFormat) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        if !rt.supports_storage_format(format.texture_format()) {
            return Err(invalid(format!(
                "{format:?} is not a storage-write format on this device (needs \
                 TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES); use R32Float"
            )));
        }
        let device = &core.device;
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine-render-rt shadow mask"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: format.texture_format(),
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine-render-rt shadow mask"),
            bind_group_layouts: &[Some(&core.scene_layout), Some(&layout)],
            immediate_size: 0,
        });
        let module = wgsl::module(
            device,
            "engine-render-rt shadow mask",
            wgsl::shadow_mask_source(format.wgsl()),
        );
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("engine-render-rt shadow mask"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("shadow_mask"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self {
            core,
            format,
            pipeline,
            layout,
        })
    }

    pub fn format(&self) -> ShadowMaskFormat {
        self.format
    }

    /// Records one dispatch into `encoder`. The scene must be committed.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &RtScene,
        targets: &ShadowMaskTargets<'_>,
        params: &ShadowMaskParams,
    ) -> Result<(), RtError> {
        self.core.ensure_usable()?;
        if !scene.is_from(&self.core) {
            return Err(invalid("the scene belongs to another RtDevice"));
        }
        let max = self.core.limits.max_texture_dimension_2d;
        if targets.width == 0 || targets.height == 0 || targets.width > max || targets.height > max
        {
            return Err(invalid(format!(
                "shadow mask size {}x{} must be 1..={max} per side",
                targets.width, targets.height
            )));
        }
        let l = params.direction_to_light;
        let length = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        if !(length.is_finite() && length > 1e-6) {
            return Err(invalid(format!(
                "direction_to_light {l:?} must be finite and non-zero"
            )));
        }
        if !(params.normal_bias.is_finite()
            && params.normal_bias >= 0.0
            && params.t_min.is_finite()
            && params.t_max.is_finite()
            && params.t_min >= 0.0
            && params.t_max > params.t_min)
        {
            return Err(invalid(
                "normal_bias must be finite and >= 0, and 0 <= t_min < t_max finite",
            ));
        }
        let scene_group = scene.committed_bind_group()?;
        let uniform = GpuShadowParams {
            direction_to_light: [l[0] / length, l[1] / length, l[2] / length, 0.0],
            ray: [params.normal_bias, params.t_min, params.t_max, 0.0],
            size_mask: [targets.width, targets.height, u32::from(params.mask), 0],
        };
        let device = &self.core.device;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("engine-render-rt shadow params"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine-render-rt shadow mask"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(targets.world_position),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(targets.world_normal),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(targets.output),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("engine-render-rt shadow mask"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, scene_group, &[]);
        pass.set_bind_group(1, &group, &[]);
        pass.dispatch_workgroups(targets.width.div_ceil(8), targets.height.div_ceil(8), 1);
        Ok(())
    }
}
