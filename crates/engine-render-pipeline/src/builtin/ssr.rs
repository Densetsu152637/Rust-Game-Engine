//! `engine.ssr`: the screen-space reflection pass over the opaque scene colour.

use super::passes::{fullscreen_pipeline, module, texture_entry};
use crate::scoped;
use crate::shader::SSR_WGSL;
use crate::ssr::SsrSettings;
use crate::targets::{HDR_FORMAT, SurfaceMaterial};
use crate::view::ViewData;
use anyhow::Result;

/// `SsrParams` of `wgsl/ssr.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SsrUniform {
    pub view_from_clip: [[f32; 4]; 4],
    pub clip_from_view: [[f32; 4]; 4],
    pub view_from_world: [[f32; 4]; 4],
    pub size: [f32; 4],
    pub trace: [f32; 4],
    pub shade: [f32; 4],
}

impl SsrUniform {
    /// The uniform for a view: the depth buffer was rasterized with the
    /// jittered projection, so tracing uses it (and its inverse) too.
    pub fn new(view: &ViewData, settings: &SsrSettings) -> Self {
        let projection = view.jittered_clip_from_view();
        let [width, height] = view.render_size.map(|v| v as f32);
        Self {
            view_from_clip: projection.inverse().to_cols_array_2d(),
            clip_from_view: projection.to_cols_array_2d(),
            view_from_world: view.view_from_world().to_cols_array_2d(),
            size: [width, height, 1.0 / width, 1.0 / height],
            trace: [
                settings.max_steps as f32,
                settings.max_distance,
                settings.thickness,
                settings.roughness_cutoff,
            ],
            shade: [
                settings.edge_fade,
                settings.intensity,
                // An orthographic projection has w = 1 in its last row.
                if projection.w_axis.w != 0.0 { 1.0 } else { 0.0 },
                0.0,
            ],
        }
    }
}

/// Device-level state of the SSR pass: one fullscreen pipeline and a
/// uniform buffer rewritten for every view before its scene submission.
pub(crate) struct SsrPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
}

impl SsrPass {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine ssr"),
            entries: &[
                texture_entry(0, float),
                texture_entry(1, wgpu::TextureSampleType::Depth),
                texture_entry(2, float),
                texture_entry(3, float),
                texture_entry(4, float),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<SsrUniform>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let pipeline = scoped(device, || {
            let module = module(device, "engine ssr", SSR_WGSL)?;
            Ok(fullscreen_pipeline(
                device,
                "engine ssr",
                &layout,
                &module,
                HDR_FORMAT,
            ))
        })?;
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine ssr params"),
            size: std::mem::size_of::<SsrUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            layout,
            pipeline,
            params,
        })
    }

    /// Read `source` (the opaque scene colour), the scene `depth` and the
    /// surface material, and write every pixel of the distinct `destination`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewData,
        settings: &SsrSettings,
        source: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        surface: &SurfaceMaterial,
        destination: &wgpu::TextureView,
    ) {
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&SsrUniform::new(view, settings)),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine ssr"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&surface.normal_roughness.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&surface.f0_coverage.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&surface.fallback_specular.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("engine ssr"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat4;

    #[test]
    fn uniform_layout_and_projection_kind() {
        assert_eq!(std::mem::size_of::<SsrUniform>(), 240);
        let perspective = glam::camera::rh::proj::directx::perspective(1.0, 1.5, 0.1, 50.0);
        let mut view = ViewData::new(
            crate::ViewId::MAIN,
            Mat4::IDENTITY,
            perspective,
            [120, 80],
            [120, 80],
        )
        .unwrap();
        let settings = SsrSettings::default();
        let uniform = SsrUniform::new(&view, &settings);
        assert_eq!(uniform.shade[2], 0.0);
        assert_eq!(uniform.size, [120.0, 80.0, 1.0 / 120.0, 1.0 / 80.0]);
        assert_eq!(uniform.trace[0], settings.max_steps as f32);
        view.clip_from_view =
            glam::camera::rh::proj::directx::orthographic(-2.0, 2.0, -2.0, 2.0, 0.1, 10.0);
        assert_eq!(SsrUniform::new(&view, &settings).shade[2], 1.0);
    }
}
