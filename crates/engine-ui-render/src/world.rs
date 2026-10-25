//! `ui.world`: `engine.UiWorldPanel` and `engine.WorldText` in `Camera3D`
//! scenes, drawn in the Transparent stage.
//!
//! Items are placed by their entity matrix, optionally turned toward the camera
//! of the view being drawn (so each XR eye or editor viewport faces its own
//! camera), rasterised with the view's `engine::view` view-projection and
//! depth-tested against the scene depth without writing it. Items are sorted
//! back to front per view ([`crate::layout::order_items`]) and blended with
//! straight alpha into linear HDR colour. See `docs/ui-render.md`.
use crate::{
    gpu::{
        Geometry, GrowBuffer, MeshRange, TextureUploads, UI_VERTEX_LAYOUT, linear_sampler,
        texture_key,
    },
    layout::order_items,
};
use anyhow::{Context, Result};
use engine_project::{
    content::Texture,
    ui_tree::{
        FEATURE_WORLD, UiFeatureReport, UiGpuFeature, UiGpuFrame, UiGpuQueue, WorldBillboard,
    },
};
use engine_render_pipeline::{
    DEPTH_FORMAT, FeatureContext, FeatureDescriptor, FeatureSetup, HDR_FORMAT, PipelineFeature,
    Stage, ViewKind, names,
};
use std::{collections::HashSet, sync::Arc};

const SHADER: &str = include_str!("wgsl/world.wgsl");
/// Bytes of one [`ItemUniform`].
const ITEM_BYTES: u64 = 80;

/// Placement of one batch: the item's entity matrix, billboard mode and
/// whether the texture is an SDF page.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ItemUniform {
    world_from_local: [[f32; 4]; 4],
    params: [u32; 4],
}

fn billboard_code(mode: WorldBillboard) -> u32 {
    match mode {
        WorldBillboard::None => 0,
        WorldBillboard::Full => 1,
        WorldBillboard::Yaw => 2,
        WorldBillboard::Screen => 3,
    }
}

struct Batch {
    texture: Option<Arc<Texture>>,
    sdf: bool,
    range: MeshRange,
}

/// Batches of the published frame, grouped per item in draw order.
struct Prepared {
    frame: Arc<UiGpuFrame>,
    batches: Vec<Batch>,
    /// Per item: its batches (indices into `batches`).
    item_batches: Vec<std::ops::Range<usize>>,
}

struct Device {
    item_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    item_stride: u64,
}

/// Draws [`UiGpuFrame::world`].
pub struct WorldUiFeature {
    queue: UiGpuQueue,
    device: Option<Device>,
    uploads: TextureUploads,
    vertices: GrowBuffer,
    indices: GrowBuffer,
    items: GrowBuffer,
    prepared: Option<Prepared>,
}

impl WorldUiFeature {
    pub fn new(queue: UiGpuQueue) -> Self {
        queue.mark_registered(UiGpuFeature::World);
        Self {
            queue,
            device: None,
            uploads: TextureUploads::default(),
            vertices: GrowBuffer::new("ui world vertices", wgpu::BufferUsages::VERTEX),
            indices: GrowBuffer::new("ui world indices", wgpu::BufferUsages::INDEX),
            items: GrowBuffer::new("ui world items", wgpu::BufferUsages::UNIFORM),
            prepared: None,
        }
    }

    fn upload(&mut self, ctx: &FeatureContext<'_>, frame: &Arc<UiGpuFrame>) -> Result<()> {
        if self
            .prepared
            .as_ref()
            .is_some_and(|p| Arc::ptr_eq(&p.frame, frame))
        {
            return Ok(());
        }
        let stride = self.device.as_ref().context("ui.world was not prepared")?.item_stride;
        let mut geometry = Geometry::default();
        let mut batches = Vec::new();
        let mut item_batches = Vec::with_capacity(frame.world.len());
        let mut uniforms: Vec<u8> = Vec::new();
        for item in &frame.world {
            let first = batches.len();
            for draw in &item.draws {
                let Some(range) = geometry.push(&draw.mesh)? else {
                    continue;
                };
                let uniform = ItemUniform {
                    world_from_local: item.world_from_local,
                    params: [billboard_code(item.billboard), u32::from(draw.sdf), 0, 0],
                };
                let at = uniforms.len();
                uniforms.resize(at + stride as usize, 0);
                uniforms[at..at + ITEM_BYTES as usize]
                    .copy_from_slice(bytemuck::bytes_of(&uniform));
                batches.push(Batch {
                    texture: draw.texture.clone(),
                    sdf: draw.sdf,
                    range,
                });
            }
            item_batches.push(first..batches.len());
        }
        self.vertices
            .write(ctx.gpu, bytemuck::cast_slice(&geometry.vertices));
        self.indices
            .write(ctx.gpu, bytemuck::cast_slice(&geometry.indices));
        self.items.write(ctx.gpu, &uniforms);
        let live: HashSet<usize> = batches
            .iter()
            .filter_map(|b| b.texture.as_ref().map(texture_key))
            .collect();
        self.uploads.retain(&live);
        self.prepared = Some(Prepared {
            frame: frame.clone(),
            batches,
            item_batches,
        });
        Ok(())
    }
}

impl Drop for WorldUiFeature {
    fn drop(&mut self) {
        self.queue.mark_unregistered(UiGpuFeature::World);
    }
}

impl PipelineFeature for WorldUiFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(FEATURE_WORLD, Stage::Transparent).after(names::TRANSPARENT)
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let item_stride =
            ITEM_BYTES.next_multiple_of(u64::from(device.limits().min_uniform_buffer_offset_alignment));
        let item_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui world item layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(ITEM_BYTES),
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui world texture layout"),
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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let source = setup.composer.compose(SHADER)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui world shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui world pipeline layout"),
            bind_group_layouts: &[
                Some(&setup.layouts.view),
                Some(&item_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui world pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(UI_VERTEX_LAYOUT)],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.device = Some(Device {
            item_layout,
            texture_layout,
            sampler: linear_sampler(device),
            pipeline,
            item_stride,
        });
        self.queue.set_ready(UiGpuFeature::World, true);
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        if ctx.view.kind == ViewKind::Probe {
            return Ok(());
        }
        let frame = self.queue.frame();
        if frame.world.is_empty() {
            self.queue.report(UiGpuFeature::World, UiFeatureReport::default());
            return Ok(());
        }
        self.upload(ctx, &frame)?;
        let order = order_items(&frame.world, ctx.view);
        let device = self.device.as_ref().context("ui.world was not prepared")?;
        let prepared = self.prepared.as_ref().expect("uploaded above");
        let item_buffer = self.items.get().expect("uploaded above");
        let item_bind = ctx.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui world items"),
            layout: &device.item_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: item_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(ITEM_BYTES),
                }),
            }],
        });
        // One texture bind group per distinct (page, kind); `None` is white.
        let mut binds: Vec<((Option<usize>, bool), wgpu::BindGroup)> = Vec::new();
        for batch in &prepared.batches {
            let key = (batch.texture.as_ref().map(texture_key), batch.sdf);
            if binds.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let texture = match &batch.texture {
                Some(texture) => self.uploads.get(ctx.gpu, texture)?,
                None => self.uploads.white(ctx.gpu)?,
            };
            let view = if batch.sdf {
                &texture.linear_view
            } else {
                &texture.view
            };
            binds.push((
                key,
                ctx.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("ui world texture"),
                    layout: &device.texture_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&device.sampler),
                        },
                    ],
                }),
            ));
        }
        let mut pass = ctx.scene_pass("ui world")?;
        pass.set_pipeline(&device.pipeline);
        pass.set_bind_group(0, ctx.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.get().expect("uploaded above").slice(..));
        pass.set_index_buffer(
            self.indices.get().expect("uploaded above").slice(..),
            wgpu::IndexFormat::Uint32,
        );
        let mut draws = 0;
        for &item in &order {
            for batch_index in prepared.item_batches[item].clone() {
                let batch = &prepared.batches[batch_index];
                let key = (batch.texture.as_ref().map(texture_key), batch.sdf);
                let bind = &binds.iter().find(|(k, _)| *k == key).expect("bound above").1;
                pass.set_bind_group(1, &item_bind, &[batch_index as u32 * device.item_stride as u32]);
                pass.set_bind_group(2, bind, &[]);
                let first = batch.range.first_index;
                pass.draw_indexed(first..first + batch.range.index_count, batch.range.base_vertex, 0..1);
                draws += 1;
            }
        }
        drop(pass);
        self.queue.report(
            UiGpuFeature::World,
            UiFeatureReport {
                draws,
                items: order.len() as u32,
            },
        );
        Ok(())
    }
}
