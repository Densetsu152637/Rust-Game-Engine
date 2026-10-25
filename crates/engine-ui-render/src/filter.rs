//! `ui.color_filter`: the colour-vision filter, a PostProcess-stage pass that
//! multiplies linear HDR scene colour by the matrix of
//! [`crate::color::filter_matrix`].
//!
//! It runs after `engine.post` (exposure, bloom, grading) and before the
//! Tonemap stage, so it filters scene-referred linear colour of the 3D view.
//! The 2D sprite/UI layer composes after tonemapping and is not filtered; 2D-only
//! frames have no post-process stage at all (the status says so).
use crate::color::{FilterUniform, filter_matrix};
use crate::gpu::uniform_buffer;
use anyhow::{Context, Result};
use engine_project::ui_tree::{FEATURE_COLOR_FILTER, UiFeatureReport, UiGpuFeature, UiGpuQueue};
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureSetup, HDR_FORMAT, PipelineFeature, Stage, ViewKind,
    names,
};

const SHADER: &str = include_str!("wgsl/filter.wgsl");

struct Device {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    /// The matrix the uniform buffer holds.
    uploaded: FilterUniform,
}

/// Applies [`engine_project::ui_tree::UiGpuFrame::color_filter`].
pub struct ColorFilterFeature {
    queue: UiGpuQueue,
    device: Option<Device>,
}

impl ColorFilterFeature {
    pub fn new(queue: UiGpuQueue) -> Self {
        queue.mark_registered(UiGpuFeature::ColorFilter);
        Self {
            queue,
            device: None,
        }
    }
}

impl Drop for ColorFilterFeature {
    fn drop(&mut self) {
        self.queue.mark_unregistered(UiGpuFeature::ColorFilter);
    }
}

impl PipelineFeature for ColorFilterFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(FEATURE_COLOR_FILTER, Stage::PostProcess).after(names::POST)
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui color filter layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let source = setup.composer.compose(SHADER)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui color filter shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui color filter pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui color filter pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uploaded = FilterUniform::default();
        self.device = Some(Device {
            layout,
            pipeline,
            uniform: uniform_buffer(device, "ui color filter matrix", &uploaded),
            uploaded,
        });
        self.queue.set_ready(UiGpuFeature::ColorFilter, true);
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        if ctx.view.kind == ViewKind::Probe {
            return Ok(());
        }
        let settings = self.queue.frame().color_filter;
        if !settings.is_active() {
            return Ok(());
        }
        let device = self
            .device
            .as_mut()
            .context("ui.color_filter was not prepared")?;
        let uniform = FilterUniform::new(&filter_matrix(settings));
        if uniform != device.uploaded {
            ctx.gpu
                .queue
                .write_buffer(&device.uniform, 0, bytemuck::bytes_of(&uniform));
            device.uploaded = uniform;
        }
        let (source, destination) = ctx.post_process_io()?;
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ui color filter"),
                layout: &device.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: device.uniform.as_entire_binding(),
                    },
                ],
            });
        let mut pass = ctx
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui color filter"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            })
            .forget_lifetime();
        pass.set_pipeline(&device.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.queue.report(
            UiGpuFeature::ColorFilter,
            UiFeatureReport { draws: 1, items: 1 },
        );
        Ok(())
    }
}
