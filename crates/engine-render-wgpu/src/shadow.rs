//! One depth-only directional shadow pass. Resources are local to each backend.
use crate::{core::Core, translate::vertex_format};
use engine_render::{DrawCommand, RenderError, Result, VertexLayout};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

pub(crate) struct ShadowResources {
    pub view: wgpu::TextureView,
    pub resolution: u32,
    layout: wgpu::BindGroupLayout,
    shader: wgpu::ShaderModule,
    pipelines: HashMap<VertexLayout, wgpu::RenderPipeline>,
}
impl ShadowResources {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow matrix"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow depth shader"),
            source: wgpu::ShaderSource::Wgsl("@group(0) @binding(0) var<uniform> mvp: mat4x4<f32>; @vertex fn vs(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> { return mvp * vec4<f32>(position, 1.0); }".into()),
        });
        Self {
            view: depth_map(device, 64),
            resolution: 64,
            layout,
            shader,
            pipelines: HashMap::new(),
        }
    }
    fn pipeline(&mut self, device: &wgpu::Device, mesh: &VertexLayout) -> wgpu::RenderPipeline {
        self.pipelines
            .entry(mesh.clone())
            .or_insert_with(|| {
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("shadow pipeline layout"),
                    bind_group_layouts: &[Some(&self.layout)],
                    immediate_size: 0,
                });
                let attrs: Vec<_> = mesh
                    .attributes
                    .iter()
                    .map(|a| wgpu::VertexAttribute {
                        format: vertex_format(a.format),
                        offset: a.offset.into(),
                        shader_location: a.location,
                    })
                    .collect();
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("directional shadow depth"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &self.shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: mesh.stride.into(),
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &attrs,
                        })],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::LessEqual),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: Default::default(),
                    fragment: None,
                    multiview_mask: None,
                    cache: None,
                })
            })
            .clone()
    }
}
fn depth_map(device: &wgpu::Device, resolution: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("directional shadow map"),
            size: wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
impl Core {
    pub fn prepare_shadow_map(&mut self, draws: &[DrawCommand]) -> Result<()> {
        let resolution = draws
            .iter()
            .filter_map(|d| d.shadow.as_ref())
            .next()
            .map_or(64, |s| s.resolution);
        if resolution > self.gpu.device.limits().max_texture_dimension_2d
            || !(64..=4096).contains(&resolution)
            || !resolution.is_power_of_two()
            || draws.iter().filter_map(|d| d.shadow.as_ref()).any(|s| {
                s.resolution != resolution || !s.light_mvp.iter().flatten().all(|n| n.is_finite())
            })
        {
            return Err(RenderError::InvalidDraw(
                "shadow maps require one bounded, consistent resolution and finite matrices".into(),
            ));
        }
        if resolution != self.shadow.resolution {
            self.shadow.view = depth_map(&self.gpu.device, resolution);
            self.shadow.resolution = resolution;
            self.bind_groups.clear();
        }
        Ok(())
    }
    pub fn encode_shadows(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        draws: &[DrawCommand],
    ) -> Result<()> {
        let mut prepared = Vec::new();
        for draw in draws {
            let Some(caster) = &draw.shadow else { continue };
            let mesh = self
                .meshes
                .get(&draw.mesh.id())
                .ok_or_else(|| RenderError::InvalidDraw("missing shadow mesh".into()))?;
            if !mesh.layout.attributes.iter().any(|a| {
                a.location == 0
                    && matches!(
                        a.format,
                        engine_render::VertexFormat::Float32x3
                            | engine_render::VertexFormat::Float32x4
                    )
            }) {
                return Err(RenderError::InvalidDraw(
                    "shadow casters require position at location 0".into(),
                ));
            }
            let pipeline = self.shadow.pipeline(&self.gpu.device, &mesh.layout);
            let buffer = self
                .gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("shadow transform"),
                    contents: bytemuck::bytes_of(&caster.light_mvp),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let group = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shadow transform"),
                    layout: &self.shadow.layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
            prepared.push((
                pipeline,
                group,
                mesh.vertex.clone(),
                mesh.index.clone(),
                mesh.draw_count,
            ));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("directional shadow pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadow.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        for (pipeline, group, vertex, index, count) in &prepared {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.set_vertex_buffer(0, vertex.slice(..));
            if let Some(index) = index {
                pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*count, 0, 0..1);
            } else {
                pass.draw(0..*count, 0..1);
            }
        }
        Ok(())
    }
}
