//! Fullscreen built-ins: the default output operator, bilinear upscale and
//! the depth export used by upscaler/generator integrations.

use crate::scoped;
use crate::shader::{DEPTH_EXPORT_WGSL, OUTPUT_WGSL, ShaderComposer, UPSCALE_WGSL};
use crate::targets::HDR_FORMAT;
use crate::view::Viewport;
use anyhow::Result;
use std::collections::HashMap;

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(16),
        },
        count: None,
    }
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn module(device: &wgpu::Device, label: &str, source: &str) -> Result<wgpu::ShaderModule> {
    let source = ShaderComposer::new().compose(source)?;
    Ok(device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    }))
}

/// `engine.output`: clamp linear HDR to 0..1 into the target viewport.
pub(crate) struct OutputPass {
    layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    origin: wgpu::Buffer,
}

impl OutputPass {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine output"),
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Float { filterable: false }),
                uniform_entry(1),
            ],
        });
        let module = scoped(device, || module(device, "engine output", OUTPUT_WGSL))?;
        let origin = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine output origin"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            layout,
            module,
            pipelines: HashMap::new(),
            origin,
        })
    }

    /// Clear `target` to opaque black, then write `hdr` into `viewport`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        hdr: &wgpu::TextureView,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        viewport: Viewport,
    ) -> Result<()> {
        let pipeline = match self.pipelines.get(&format) {
            Some(pipeline) => pipeline.clone(),
            None => {
                let pipeline = scoped(device, || {
                    Ok(fullscreen_pipeline(device, "engine output", &self.layout, &self.module, format))
                })?;
                self.pipelines.insert(format, pipeline.clone());
                pipeline
            }
        };
        queue.write_buffer(
            &self.origin,
            0,
            bytemuck::cast_slice(&[viewport.x as f32, viewport.y as f32, 0.0, 0.0]),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine output"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.origin.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("engine output"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_viewport(
            viewport.x as f32,
            viewport.y as f32,
            viewport.width as f32,
            viewport.height as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// `engine.upscale`: bilinear render-size HDR to output-size HDR.
pub(crate) struct UpscalePass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    scale: wgpu::Buffer,
}

impl UpscalePass {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine upscale"),
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(2),
            ],
        });
        let pipeline = scoped(device, || {
            let module = module(device, "engine upscale", UPSCALE_WGSL)?;
            Ok(fullscreen_pipeline(device, "engine upscale", &layout, &module, HDR_FORMAT))
        })?;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("engine upscale sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let scale = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("engine upscale scale"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            layout,
            pipeline,
            sampler,
            scale,
        })
    }

    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        output: [u32; 2],
    ) {
        queue.write_buffer(
            &self.scale,
            0,
            bytemuck::cast_slice(&[1.0 / output[0] as f32, 1.0 / output[1] as f32, 0.0, 0.0]),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine upscale"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.scale.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("engine upscale"),
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

/// Writes scene depth (`Depth32Float`) into an `R32Float` colour texture,
/// the depth format NGX and similar integrations consume.
pub struct DepthExport {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl DepthExport {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine depth export"),
            entries: &[texture_entry(0, wgpu::TextureSampleType::Depth)],
        });
        let pipeline = scoped(device, || {
            let module = module(device, "engine depth export", DEPTH_EXPORT_WGSL)?;
            Ok(fullscreen_pipeline(
                device,
                "engine depth export",
                &layout,
                &module,
                wgpu::TextureFormat::R32Float,
            ))
        })?;
        Ok(Self { layout, pipeline })
    }

    /// Encode `depth` (a `Depth32Float` view) into `destination` (`R32Float`,
    /// same size).
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        destination: &wgpu::TextureView,
    ) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine depth export"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("engine depth export"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
