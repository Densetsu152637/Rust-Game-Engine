//! The `engine.decals` pass (OpaqueEffects stage).
//!
//! The scene colour is copied once to the stage destination. Each decal then
//! draws a scissored fullscreen triangle into the destination, reading the
//! previous result from the source, and copies its screen rectangle back to
//! the source so the next decal sees it. Both textures hold the final image
//! afterwards.

use super::{DECAL_UNIFORM_BYTES, DecalReport, DecalTexture, plan};
use crate::context::{FeatureContext, FeatureSetup};
use crate::feature::{FeatureBody, FeatureDescriptor, FeatureNeeds, FeatureRegistry};
use crate::feature::{PipelineFeature, Stage};
use crate::pipeline::names;
use crate::resources::GpuTexture;
use crate::shader::DECALS_WGSL;
use crate::targets::HDR_FORMAT;
use crate::view::ViewId;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Reports shared with [`crate::ScenePipeline`].
#[derive(Default)]
pub(crate) struct DecalShared {
    pub reports: Mutex<HashMap<ViewId, DecalReport>>,
}

struct Gpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    white: Arc<GpuTexture>,
    stride: u64,
}

pub(crate) struct DecalFeature {
    shared: Arc<DecalShared>,
    gpu: Option<Gpu>,
    /// One uniform buffer per view (a slot per decal).
    buffers: HashMap<ViewId, wgpu::Buffer>,
}

impl DecalFeature {
    pub fn new(shared: Arc<DecalShared>) -> Self {
        Self {
            shared,
            gpu: None,
            buffers: HashMap::new(),
        }
    }
}

pub(crate) fn descriptor() -> FeatureDescriptor {
    FeatureDescriptor::new(names::DECALS, Stage::OpaqueEffects)
        // Decals decorate the lit colour before SSR reflects it.
        .before(names::SSR)
        .disabled_by_default()
        .needs(FeatureNeeds {
            normals: true,
            ..Default::default()
        })
}

fn texture_entry(binding: u32, filterable: bool, depth: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable }
            },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn region_copy(texture: &wgpu::Texture, origin: wgpu::Origin3d) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin,
        aspect: wgpu::TextureAspect::All,
    }
}

impl PipelineFeature for DecalFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        descriptor()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("engine decals"),
            entries: &[
                texture_entry(0, false, false),
                texture_entry(1, false, true),
                texture_entry(2, false, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(DECAL_UNIFORM_BYTES),
                    },
                    count: None,
                },
                texture_entry(4, true, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let source = setup.composer.compose(DECALS_WGSL)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("engine decals"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("engine decals"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("engine decals"),
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
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("engine decals"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white = GpuTexture::rgba8(
            device,
            &setup.gpu.queue,
            "engine decals white",
            [1, 1],
            &[255; 4],
        )?;
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1);
        let stride = DECAL_UNIFORM_BYTES.div_ceil(alignment) * alignment;
        self.gpu = Some(Gpu {
            layout,
            pipeline,
            sampler,
            white,
            stride,
        });
        Ok(())
    }

    fn forget_view(&mut self, view: ViewId) {
        self.buffers.remove(&view);
        if let Ok(mut reports) = self.shared.reports.lock() {
            reports.remove(&view);
        }
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let view = ctx.view;
        let render_size = view.render_size;
        let plan = plan(
            &view.decals,
            view.jittered_clip_from_world(),
            view.jittered_clip_from_world().inverse(),
            render_size,
        )?;
        if let Ok(mut reports) = self.shared.reports.lock() {
            reports.insert(view.id, plan.report);
        }
        if plan.items.is_empty() {
            return Ok(());
        }
        let gpu = self
            .gpu
            .as_ref()
            .context("engine.decals was not prepared")?;
        let device = ctx.gpu.device.clone();
        let depth = ctx.depth()?.view.clone();
        let normals = ctx
            .normals()
            .context("engine.decals needs the normals prepass")?
            .view
            .clone();
        let source_texture = ctx.scene_color()?.texture.clone();
        let (source, destination) = ctx.opaque_effect_io()?;
        let destination_texture = ctx.scene_color()?.texture.clone();
        let full = wgpu::Extent3d {
            width: render_size[0],
            height: render_size[1],
            depth_or_array_layers: 1,
        };
        ctx.encoder.copy_texture_to_texture(
            source_texture.as_image_copy(),
            destination_texture.as_image_copy(),
            full,
        );
        let buffer = self.buffers.entry(view.id).or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("engine decals"),
                size: gpu.stride * super::MAX_DECALS as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        for (slot, item) in plan.items.iter().enumerate() {
            let offset = slot as u64 * gpu.stride;
            ctx.gpu
                .queue
                .write_buffer(buffer, offset, bytemuck::bytes_of(&item.uniform));
            let texture_view = match &view.decals[item.index].texture {
                DecalTexture::Texture(texture) => &texture.view,
                DecalTexture::None | DecalTexture::Missing => &gpu.white.view,
            };
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine decals"),
                layout: &gpu.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&depth),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&normals),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer,
                            offset,
                            size: wgpu::BufferSize::new(DECAL_UNIFORM_BYTES),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                    },
                ],
            });
            let rect = item.rect;
            {
                let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("engine decal"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &destination,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_scissor_rect(rect.x0, rect.y0, rect.width(), rect.height());
                pass.set_pipeline(&gpu.pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            // Keep the source current for the next decal.
            let region = wgpu::Origin3d {
                x: rect.x0,
                y: rect.y0,
                z: 0,
            };
            ctx.encoder.copy_texture_to_texture(
                region_copy(&destination_texture, region),
                region_copy(&source_texture, region),
                wgpu::Extent3d {
                    width: rect.width(),
                    height: rect.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(())
    }
}

/// Replace the descriptor-only `engine.decals` of the built-in registry with
/// its implementation and prepare it.
pub(crate) fn install(
    registry: &mut FeatureRegistry,
    setup: &FeatureSetup<'_>,
    shared: Arc<DecalShared>,
) -> Result<()> {
    let mut feature = DecalFeature::new(shared);
    crate::scoped(&setup.gpu.device, || feature.prepare(setup))
        .context("preparing render feature engine.decals")?;
    let registered = registry
        .features
        .iter_mut()
        .find(|r| r.descriptor.name == names::DECALS)
        .context("built-in feature engine.decals is not registered")?;
    registered.body = FeatureBody::Owned(Box::new(feature));
    registered.prepared = true;
    Ok(())
}
