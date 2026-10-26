//! `ui.sdf_text`: screen-space UI text of SDF families, drawn from engine-text
//! SDF atlas pages with `GLYPH_SHADER_WGSL` in the Overlay stage.
//!
//! Runs join the host sprite painter stream using the UI producer's authored
//! paint IDs. The SDF-only `sNN` atlas marker is normalized back to the text
//! marker `2NN`, so same-node foreground/caret geometry remains above text.
use crate::{
    gpu::{
        Geometry, GrowBuffer, MeshRange, TextureUploads, UI_VERTEX_LAYOUT, linear_sampler,
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
    FeatureContext, FeatureDescriptor, FeatureSetup, OverlayBatch, OverlayItem, OverlayOrderKey,
    PipelineFeature, Stage, ViewData, ViewKind, names,
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
    snapshot: Option<Arc<UiGpuFrame>>,
    sequence: Option<Arc<UiGpuFrame>>,
    pending: UiFeatureReport,
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
            snapshot: None,
            sequence: None,
            pending: UiFeatureReport::default(),
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
        for draw in frame.sdf_text.iter().filter(|d| renderable(d)) {
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
        FeatureDescriptor::new(FEATURE_SDF_TEXT, Stage::Overlay)
            .after(names::SPRITES)
            .replayable()
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

    fn begin_presentation_sequence(&mut self) -> Result<()> {
        self.sequence = Some(self.queue.frame());
        Ok(())
    }
    fn end_presentation_sequence(&mut self) {
        self.sequence = None;
        self.snapshot = None;
    }

    fn overlay_items(&mut self, view: &ViewData) -> Result<Option<Vec<OverlayItem>>> {
        if view.kind != ViewKind::Camera {
            return Ok(None);
        }
        let frame = self.sequence.clone().unwrap_or_else(|| self.queue.frame());
        let items = frame
            .sdf_text
            .iter()
            .filter(|d| renderable(d))
            .enumerate()
            .map(|(index, draw)| OverlayItem {
                order: OverlayOrderKey {
                    ui: true,
                    layer: draw.layer,
                    id: paint_id(&draw.id),
                },
                token: index as u64,
            })
            .collect();
        self.snapshot = Some(frame);
        Ok(Some(items))
    }

    fn encode_overlay(
        &mut self,
        ctx: &mut FeatureContext<'_>,
        batch: OverlayBatch<'_>,
    ) -> Result<()> {
        if batch.first {
            self.pending = UiFeatureReport::default();
        }
        let frame = self
            .snapshot
            .clone()
            .context("SDF painter snapshot was not retained")?;
        let report = self.draw_frame(ctx, &frame, Some(batch.tokens))?;
        self.pending.draws += report.draws;
        self.pending.items += report.items;
        if batch.last {
            self.queue.report(UiGpuFeature::SdfText, self.pending);
            self.snapshot = None;
        }
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        if ctx.view.kind != ViewKind::Camera {
            return Ok(());
        }
        let frame = self.queue.frame();
        let report = self.draw_frame(ctx, &frame, None)?;
        self.queue.report(UiGpuFeature::SdfText, report);
        Ok(())
    }
}

impl SdfTextFeature {
    fn draw_frame(
        &mut self,
        ctx: &mut FeatureContext<'_>,
        frame: &Arc<UiGpuFrame>,
        tokens: Option<&[u64]>,
    ) -> Result<UiFeatureReport> {
        let target = ctx.output()?.clone();
        let Some(fit) = UiFit::new(frame.ui_size, target.size) else {
            return Ok(UiFeatureReport::default());
        };
        if frame.sdf_text.is_empty() {
            return Ok(UiFeatureReport::default());
        }
        self.upload(ctx, frame)?;
        let device = self
            .device
            .as_mut()
            .context("ui.sdf_text was not prepared")?;
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
                ctx.gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
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
        let all: Vec<u64> = if tokens.is_none() {
            (0..runs.len() as u64).collect()
        } else {
            vec![]
        };
        for token in tokens.unwrap_or(&all) {
            let run = runs
                .get(*token as usize)
                .context("SDF overlay token is not in the retained snapshot")?;
            let Some((x, y, w, h)) = fit.scissor(run.clip) else {
                continue;
            };
            let key = texture_key(&run.texture);
            let bind = &binds
                .iter()
                .find(|(k, _)| *k == key)
                .expect("bound above")
                .1;
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
        Ok(UiFeatureReport {
            draws: drawn,
            items: tokens.map_or(runs.len(), <[u64]>::len) as u32,
        })
    }
}

fn renderable(draw: &engine_project::runtime::Draw) -> bool {
    draw.texture.is_some()
        && !draw.mesh.indices.is_empty()
        && draw
            .mesh
            .indices
            .iter()
            .all(|&i| (i as usize) < draw.mesh.vertices.len())
}

fn paint_id(id: &str) -> String {
    if let Some((prefix, tail)) = id.rsplit_once(':')
        && tail.len() == 3
        && tail.starts_with('s')
        && tail[1..].bytes().all(|b| b.is_ascii_digit())
    {
        format!("{prefix}:2{}", &tail[1..])
    } else {
        id.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdf_marker_restores_the_authored_text_slot_below_foreground() {
        let key = paint_id("game-ui:68!00000:s00");
        assert_eq!(key, "game-ui:68!00000:200");
        assert!("game-ui:68!00000:1" < key.as_str());
        assert!(key.as_str() < "game-ui:68!00000:3");
        assert_eq!(paint_id("arbitrary:sdf"), "arbitrary:sdf");
    }
}
