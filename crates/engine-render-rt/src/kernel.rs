//! Shared plumbing of the compute kernels: bind group layout entries, pipeline
//! creation, per-dispatch bind groups and boundary checks.

use crate::device::Core;
use crate::error::{RtError, invalid};
use crate::scene::RtScene;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Output format of the single-channel kernels ([`crate::ShadowMaskKernel`],
/// [`crate::AmbientOcclusionKernel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RtScalarFormat {
    /// Needs `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter support
    /// (see [`crate::RtDevice::supports_storage_format`]).
    R8Unorm,
    /// Same requirement as `R8Unorm`.
    R16Float,
    /// Always available.
    R32Float,
}

impl RtScalarFormat {
    pub fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::R16Float => wgpu::TextureFormat::R16Float,
            Self::R32Float => wgpu::TextureFormat::R32Float,
        }
    }

    pub(crate) fn wgsl(self) -> &'static str {
        match self {
            Self::R8Unorm => "r8unorm",
            Self::R16Float => "r16float",
            Self::R32Float => "r32float",
        }
    }
}

/// G-buffer views of one view and frame, shared by the RT kernels.
///
/// - `world_position`: `Rgba32Float`, xyz = world position, w >= 0.5 where
///   there is geometry (w < 0.5 marks background pixels).
/// - `world_normal`: a float texture (`Rgba16Float` or `Rgba32Float`) whose
///   xyz is the unit world-space shading normal facing the camera.
///
/// Both are `width x height`, bound as unfilterable-float `texture_2d<f32>`
/// and read with `textureLoad`.
#[derive(Clone, Copy, Debug)]
pub struct RtGBuffer<'a> {
    pub world_position: &'a wgpu::TextureView,
    pub world_normal: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

pub(crate) const WORKGROUP: u32 = 8;

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

/// `texture_2d<f32>`, unfilterable float (any float or unorm format).
pub(crate) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
    )
}

/// `texture_storage_2d<format, write>`.
pub(crate) fn storage_texture_entry(
    binding: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
    )
}

pub(crate) fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    )
}

pub(crate) fn storage_buffer_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    )
}

pub(crate) fn bind_group_layout(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    })
}

/// A compute pipeline over `layouts` (group 0, 1, ...) running `entry_point`
/// of `source`.
pub(crate) fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    layouts: &[&wgpu::BindGroupLayout],
    source: String,
    entry_point: &str,
) -> wgpu::ComputePipeline {
    let layouts: Vec<Option<&wgpu::BindGroupLayout>> = layouts.iter().map(|l| Some(*l)).collect();
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &layouts,
        immediate_size: 0,
    });
    let module = crate::wgsl::module(device, label, source);
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: Default::default(),
        cache: None,
    })
}

pub(crate) fn uniform_buffer(device: &wgpu::Device, label: &str, contents: &[u8]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

/// A bind group whose entry `i` is `resources[i]` at binding `i`.
pub(crate) fn bind_group(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    resources: &[wgpu::BindingResource<'_>],
) -> wgpu::BindGroup {
    let entries: Vec<wgpu::BindGroupEntry<'_>> = resources
        .iter()
        .enumerate()
        .map(|(i, resource)| wgpu::BindGroupEntry {
            binding: i as u32,
            resource: resource.clone(),
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &entries,
    })
}

/// One compute pass with one 8 x 8 workgroup per tile of `width x height`.
pub(crate) fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::ComputePipeline,
    groups: &[&wgpu::BindGroup],
    width: u32,
    height: u32,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    for (index, group) in groups.iter().enumerate() {
        pass.set_bind_group(index as u32, *group, &[]);
    }
    pass.dispatch_workgroups(width.div_ceil(WORKGROUP), height.div_ceil(WORKGROUP), 1);
}

/// Rejects sizes of zero or beyond the device's 2D texture limit.
pub(crate) fn check_size(core: &Core, what: &str, width: u32, height: u32) -> Result<(), RtError> {
    let max = core.limits.max_texture_dimension_2d;
    if width == 0 || height == 0 || width > max || height > max {
        return Err(invalid(format!(
            "{what} size {width}x{height} must be 1..={max} per side"
        )));
    }
    Ok(())
}

/// The committed scene bind group, after checking the device is usable and
/// the scene belongs to it.
pub(crate) fn scene_group<'s>(
    core: &Arc<Core>,
    scene: &'s RtScene,
) -> Result<&'s wgpu::BindGroup, RtError> {
    core.ensure_usable()?;
    if !scene.is_from(core) {
        return Err(invalid("the scene belongs to another RtDevice"));
    }
    scene.committed_bind_group()
}

/// `v` normalized, or an error naming `what` when it is not finite and non-zero.
pub(crate) fn unit_direction(what: &str, v: [f32; 3]) -> Result<[f32; 3], RtError> {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if !(length.is_finite() && length > 1e-6) {
        return Err(invalid(format!("{what} {v:?} must be finite and non-zero")));
    }
    Ok([v[0] / length, v[1] / length, v[2] / length])
}

pub(crate) fn finite_point(what: &str, v: [f32; 3]) -> Result<(), RtError> {
    if v.iter().all(|c| c.is_finite()) {
        Ok(())
    } else {
        Err(invalid(format!("{what} {v:?} must be finite")))
    }
}

/// Rejects formats the kernels cannot write on this device.
pub(crate) fn check_storage_format(supported: bool, format: RtScalarFormat) -> Result<(), RtError> {
    if supported {
        Ok(())
    } else {
        Err(invalid(format!(
            "{format:?} is not a storage-write format on this device (needs \
             TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES); use R32Float"
        )))
    }
}

/// Sun and sky of one frame, for [`crate::ReflectionKernel`] and
/// [`crate::DiffuseGiKernel`] (the shadow mask takes its light direction in
/// [`crate::ShadowMaskParams`]).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RtLighting {
    /// Direction towards the sun; normalized by the kernels.
    pub sun_direction: [f32; 3],
    /// Sun irradiance on a surface facing it (linear RGB, >= 0); zero turns
    /// the sun off. A Lambertian hit of albedo `a` reflects
    /// `a / pi * sun_illuminance * max(n . l, 0) * visibility`.
    pub sun_illuminance: [f32; 3],
    /// Sky radiance is `horizon + (zenith - horizon) * max(dir.y, 0)`.
    pub sky_zenith: [f32; 3],
    pub sky_horizon: [f32; 3],
}

impl Default for RtLighting {
    fn default() -> Self {
        Self {
            sun_direction: [0.0, 1.0, 0.0],
            sun_illuminance: [0.0; 3],
            sky_zenith: [1.0; 3],
            sky_horizon: [1.0; 3],
        }
    }
}

/// `RtLighting` in WGSL: four `vec4<f32>` (sun direction, sun illuminance,
/// sky zenith, sky horizon; w unused).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GpuLighting {
    sun_direction: [f32; 4],
    sun_illuminance: [f32; 4],
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
}

impl RtLighting {
    pub(crate) fn to_gpu(self) -> Result<GpuLighting, RtError> {
        let l = unit_direction("sun_direction", self.sun_direction)?;
        let radiance = |what: &str, v: [f32; 3]| {
            if v.iter().all(|c| c.is_finite() && *c >= 0.0) {
                Ok([v[0], v[1], v[2], 0.0])
            } else {
                Err(invalid(format!("{what} {v:?} must be finite and >= 0")))
            }
        };
        Ok(GpuLighting {
            sun_direction: [l[0], l[1], l[2], 0.0],
            sun_illuminance: radiance("sun_illuminance", self.sun_illuminance)?,
            sky_zenith: radiance("sky_zenith", self.sky_zenith)?,
            sky_horizon: radiance("sky_horizon", self.sky_horizon)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lighting_is_validated() {
        let ok = RtLighting::default().to_gpu().unwrap();
        assert_eq!(ok.sun_direction, [0.0, 1.0, 0.0, 0.0]);
        let bad = |l: RtLighting| l.to_gpu().unwrap_err().to_string();
        assert!(
            bad(RtLighting {
                sun_direction: [0.0; 3],
                ..Default::default()
            })
            .contains("sun_direction")
        );
        assert!(
            bad(RtLighting {
                sun_illuminance: [-1.0, 0.0, 0.0],
                ..Default::default()
            })
            .contains("sun_illuminance")
        );
        assert!(
            bad(RtLighting {
                sky_zenith: [f32::NAN; 3],
                ..Default::default()
            })
            .contains("sky_zenith")
        );
        let unit = RtLighting {
            sun_direction: [3.0, 4.0, 0.0],
            ..Default::default()
        }
        .to_gpu()
        .unwrap();
        assert_eq!(unit.sun_direction, [0.6, 0.8, 0.0, 0.0]);
        assert_eq!(size_of::<GpuLighting>(), 64);
    }
}
