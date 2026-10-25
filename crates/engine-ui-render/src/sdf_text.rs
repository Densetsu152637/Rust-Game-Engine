//! `ui.sdf_text`: screen-space UI text of SDF families, drawn from engine-text
//! SDF atlas pages with `GLYPH_SHADER_WGSL` in the Overlay stage.
//!
//! The Overlay stage runs after tonemapping, in the target format, and after
//! the host's `engine.sprites` node, so this is where the UI composites: the
//! text lands on top of the sprite/UI layer (sprites and UI are drawn first;
//! see `docs/ui-render.md` for what that means for layering). Glyph quads are
//! positioned by the UI's logical-pixel camera fit, scaled in the fragment
//! shader from the distance field, so edges stay about one pixel wide at any
//! UI scale.
use crate::{
    gpu::{
        GrowBuffer, Geometry, MeshRange, TextureUploads, UI_VERTEX_LAYOUT, linear_sampler,
        texture_key,
    },
    layout::{UiFit, ui_clip_from_pixels},
};
use anyhow::{Context, Result};
use engine_project::{
    content::Texture,
    ui_tree::{FEATURE_SDF_TEXT, UiFeatureReport, UiGpuFeature, UiGpuFrame, UiGpuQueue},
};
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureSetup, PipelineFeature, Stage, ViewKind, names,
};
use engine_text::{GLYPH_SHADER_WGSL, SHADER_MODE_SDF};
use std::{collections::HashSet, sync::Arc};

/// The glyph shader's uniform: `view_proj` and `params` (mode, edge bias,
/// anti-aliasing width scale).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TextUniform {
    view_proj: [[f32; 4]; 4],
    params: [f32; 4],
}

struct Run {
    texture: Arc<Texture>,
    range: MeshRange,
    clip: Option<[f32; 4]>,
}

struct Device {
    layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    sampler: wgpu::Sampler,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}

/// Draws [`UiGpuFrame::sdf_text`].
pub struct SdfTextFeature {
    queue: UiGpuQueue,
    device: Option<Device>,
    uploads: TextureUploads,
    vertices: GrowBuffer,
    indices: GrowBuffer,
    uniform: Option<wgpu::Buffer>,
    /// The published frame the buffers hold (the queue replaces it each frame).
    uploaded: Option<(Arc<UiGpuFrame>, Vec<Run>)>,
}

impl SdfTextFeature {
    pub fn new(queue: UiGpuQueue) -> Self {
        queue.mark_registered(UiGpuFeature::SdfText);
        Self {
            queue,
            device: None,
            uploads: TextureUploads::default(),
            vertices: GrowBuffer::new("ui sdf vertices", wgpu::BufferUsages::VERTEX),
            indices: GrowBuffer::new("ui sdf indices", wgpu::BufferUsages::INDEX),
            uniform: None,
            uploaded: None,
        }
    }

    fn upload(&mut self, ctx: &FeatureContext<'_>, frame: &Arc<UiGpuFrame>) -> Result<()> {
        if self
            .uploaded
            .as_ref()
            .is_some_and(|(held, _)| Arc::ptr_eq(held, frame))
        {
            return Ok(());
        }
        let mut geometry = Geometry::default();
        let mut runs = Vec::with_capacity(frame.sdf_text.len());
        for draw in &frame.sdf_text {
            let Some(texture) = draw.texture.clone() else {
                continue;
            };
            if let Some(range) = geometry.push(&draw.mesh)? {
                runs.push(Run {
                    texture,
                    range,
                    clip: draw.clip,
                });
            }
        }
        self.vertices
            .write(ctx.gpu, bytemuck::cast_slice(&geometry.vertices));
        self.indices
            .write(ctx.gpu, bytemuck::cast_slice(&geometry.indices));
        let uniform = TextUniform {
            view_proj: ui_clip_from_pixels(frame.ui_size),
            params: [SHADER_MODE_SDF, 0.0, 1.0, 0.0],
        };
        match &self.uniform {
            Some(buffer) => ctx
                .gpu
                .queue
                .write_buffer(buffer, 0, bytemuck::bytes_of(&uniform)),
            None => {
                self.uniform = Some(crate::gpu::uniform_buffer(
                    &ctx.gpu.device,
                    "ui sdf uniform",
                    &uniform,
                ))
            }
        }
        let live: HashSet<usize> = runs.iter().map(|r| texture_key(&r.texture)).collect();
        self.uploads.retain(&live);
        self.uploaded = Some((frame.clone(), runs));
        Ok(())
    }
}

impl Drop for SdfTextFeature {
    fn drop(&mut self) {
        self.queue.mark_unregistered(UiGpuFeature::SdfText);
    }
}

fn pipeline(
    device: &wgpu::Device,
    parts: &Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ui sdf pipeline layout"),
        bind_group_layouts: &[Some(&parts.layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ui sdf pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &parts.module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(UI_VERTEX_LAYOUT)],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &parts.module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl PipelineFeature for SdfTextFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(FEATURE_SDF_TEXT, Stage::Overlay).after(names::SPRITES)
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui sdf bind layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui sdf shader"),
            source: wgpu::ShaderSource::Wgsl(GLYPH_SHADER_WGSL.into()),
        });
        self.device = Some(Device {
            layout,
            module,
            sampler: linear_sampler(device),
            pipelines: Vec::new(),
        });
        self.queue.set_ready(UiGpuFeature::SdfText, true);
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        // Other views (editor viewports, XR eyes, probes) have no screen UI.
        if ctx.view.kind != ViewKind::Camera {
            return Ok(());
        }
        let frame = self.queue.frame();
        let target = ctx.output()?.clone();
        let Some(fit) = UiFit::new(frame.ui_size, target.size) else {
            return Ok(());
        };
        if frame.sdf_text.is_empty() {
            self.queue.report(UiGpuFeature::SdfText, UiFeatureReport::default());
            return Ok(());
        }
        self.upload(ctx, &frame)?;
        let device = self.device.as_mut().context("ui.sdf_text was not prepared")?;
        if !device.pipelines.iter().any(|(f, _)| *f == target.format) {
            let built = pipeline(&ctx.gpu.device, device, target.format);
            device.pipelines.push((target.format, built));
        }
        let render_pipeline = &device
            .pipelines
            .iter()
            .find(|(f, _)| *f == target.format)
            .expect("built above")
            .1;
        let (_, runs) = self.uploaded.as_ref().expect("uploaded above");
        let uniform = self.uniform.as_ref().expect("uploaded above");
        // One bind group per distinct page.
        let mut binds: Vec<(usize, wgpu::BindGroup)> = Vec::new();
        for run in runs {
            let key = texture_key(&run.texture);
            if binds.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let texture = self.uploads.get(ctx.gpu, &run.texture)?;
            binds.push((
                key,
                ctx.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("ui sdf page"),
                    layout: &device.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&texture.linear_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&device.sampler),
                        },
                    ],
                }),
            ));
        }
        let mut pass = ctx
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui sdf text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            })
            .forget_lifetime();
        pass.set_viewport(
            fit.x as f32,
            fit.y as f32,
            fit.width as f32,
            fit.height as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(render_pipeline);
        pass.set_vertex_buffer(0, self.vertices.get().expect("uploaded").slice(..));
        pass.set_index_buffer(
            self.indices.get().expect("uploaded").slice(..),
            wgpu::IndexFormat::Uint32,
        );
        let mut drawn = 0;
        for run in runs {
            let Some((x, y, w, h)) = fit.scissor(run.clip) else {
                continue;
            };
            let key = texture_key(&run.texture);
            let bind = &binds.iter().find(|(k, _)| *k == key).expect("bound above").1;
            pass.set_scissor_rect(x, y, w, h);
            pass.set_bind_group(0, bind, &[]);
            let first = run.range.first_index;
            pass.draw_indexed(
                first..first + run.range.index_count,
                run.range.base_vertex,
                0..1,
            );
            drawn += 1;
        }
        drop(pass);
        self.queue.report(
            UiGpuFeature::SdfText,
            UiFeatureReport {
                draws: drawn,
                items: runs.len() as u32,
            },
        );
        Ok(())
    }
}
