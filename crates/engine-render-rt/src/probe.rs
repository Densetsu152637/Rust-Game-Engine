//! The probe kernel: closest hit, surface and any-hit visibility per ray.

use crate::device::Core;
use crate::error::{RtError, invalid};
use crate::layout::{ProbeHit, ProbeRay};
use crate::scene::RtScene;
use crate::wgsl;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Rays per [`crate::RtDevice::cast_rays`] call.
pub const MAX_PROBE_RAYS: usize = 1 << 20;

pub(crate) struct ProbeKernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl ProbeKernel {
    pub fn new(core: &Core) -> Self {
        let device = &core.device;
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine-render-rt probe"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer(1, wgpu::BufferBindingType::Storage { read_only: false }),
                buffer(2, wgpu::BufferBindingType::Uniform),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine-render-rt probe"),
            bind_group_layouts: &[Some(&core.scene_layout), Some(&layout)],
            immediate_size: 0,
        });
        let module = wgsl::module(device, "engine-render-rt probe", wgsl::probe_source());
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("engine-render-rt probe"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("probe"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, layout }
    }

    pub fn cast(
        &self,
        core: &Arc<Core>,
        scene: &RtScene,
        rays: &[ProbeRay],
    ) -> Result<Vec<ProbeHit>, RtError> {
        core.ensure_usable()?;
        if !scene.is_from(core) {
            return Err(invalid("the scene belongs to another RtDevice"));
        }
        if rays.is_empty() || rays.len() > MAX_PROBE_RAYS {
            return Err(invalid(format!(
                "{} probe rays; between 1 and {MAX_PROBE_RAYS} are allowed",
                rays.len()
            )));
        }
        if let Some(i) = rays.iter().position(|r| {
            !(r.origin.iter().chain(&r.direction).all(|v| v.is_finite())
                && r.t_min.is_finite()
                && r.t_max.is_finite()
                && r.t_min >= 0.0
                && r.t_max >= r.t_min)
        }) {
            return Err(invalid(format!(
                "probe ray {i} has non-finite values or an invalid [t_min, t_max]"
            )));
        }
        let scene_group = scene.committed_bind_group()?;
        let device = &core.device;
        let ray_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("engine-render-rt probe rays"),
            contents: bytemuck::cast_slice(rays),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let hit_bytes = (rays.len() * size_of::<ProbeHit>()) as u64;
        let hit_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine-render-rt probe hits"),
            size: hit_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("engine-render-rt probe params"),
            contents: bytemuck::cast_slice(&[rays.len() as u32, 0, 0, 0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine-render-rt probe"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ray_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: hit_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("engine-render-rt probe"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("engine-render-rt probe"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, scene_group, &[]);
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
        }
        core.submit_and_wait(encoder, "probe rays")?;
        let bytes = core.read_buffer(&hit_buffer, hit_bytes, "probe hits")?;
        Ok(bytemuck::cast_slice(&bytes).to_vec())
    }
}
