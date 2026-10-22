//! Frame recording: uniform upload, pipeline and bind group lookup, one render
//! pass per frame.

use crate::core::{Arena, Core, create_depth};
use crate::error::{backend_error, check_gpu};
use crate::shader::{BindKey, PipelineKey};
use crate::surface::Acquired;
use crate::translate::{COLOR_FORMAT, effective_rect};
use crate::uniforms::{UniformPacker, arena_capacity};
use engine_render::{
    DrawCommand, FrameDesc, FrameOutcome, FrameStats, PixelRect, RenderError, RenderFeature,
    RenderTarget, Result, UniformSlot,
};

/// Where a frame draws.
pub(crate) struct Destination {
    pub view: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

struct Prepared {
    pipeline: wgpu::RenderPipeline,
    sets: Vec<(u32, wgpu::BindGroup, Vec<u32>)>,
    vertex: wgpu::Buffer,
    index: Option<wgpu::Buffer>,
    count: u32,
    scissor: PixelRect,
}

impl Prepared {
    fn same_batch(&self, other: &Prepared) -> bool {
        self.pipeline == other.pipeline
            && self.sets == other.sets
            && self.scissor == other.scissor
            && self.vertex == other.vertex
            && self.index == other.index
    }
}

fn stale_resource(what: &str, id: u64) -> RenderError {
    RenderError::InvalidDraw(format!("{what} #{id} is not alive in the wgpu backend"))
}

impl Core {
    pub fn submit_frame(
        &mut self,
        frame: &FrameDesc,
        draws: &[DrawCommand],
    ) -> Result<FrameOutcome> {
        match frame.target {
            RenderTarget::Offscreen(handle) => {
                let target = self
                    .targets
                    .get(&handle.id())
                    .ok_or(RenderError::StaleHandle(handle.into()))?;
                let destination = Destination {
                    view: target.view.clone(),
                    depth: target.depth.clone(),
                    width: target.width,
                    height: target.height,
                    format: COLOR_FORMAT,
                };
                self.draw_frame(&destination, frame, draws)
                    .map(FrameOutcome::Rendered)
            }
            RenderTarget::Surface => self.submit_surface(frame, draws),
        }
    }

    fn submit_surface(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome> {
        let Some(surface) = self.surface.as_mut() else {
            let status = self.caps.feature(RenderFeature::SurfacePresentation);
            return Err(RenderError::Unsupported {
                feature: status.name.into(),
                reason: status.reason,
            });
        };
        if surface.suspended {
            return Ok(FrameOutcome::Skipped(engine_render::FrameSkip::Suspended));
        }
        let (output, suboptimal) = match surface.acquire(&self.gpu)? {
            Acquired::Frame { output, suboptimal } => (output, suboptimal),
            Acquired::Skip(skip) => return Ok(FrameOutcome::Skipped(skip)),
        };
        let (width, height, format) = (
            surface.config.width,
            surface.config.height,
            surface.config.format,
        );
        let view = output.texture.create_view(&Default::default());
        let depth = self.depth_for(width, height);
        let destination = Destination {
            view,
            depth,
            width,
            height,
            format,
        };
        let stats = self.draw_frame(&destination, frame, draws)?;
        self.gpu.queue.present(output);
        if suboptimal && let Some(surface) = &self.surface {
            surface.reconfigure(&self.gpu)?;
        }
        check_gpu(&self.gpu)?;
        Ok(FrameOutcome::Rendered(stats))
    }

    /// Depth attachment for caller-supplied or swapchain views, cached per size.
    pub fn depth_for(&mut self, width: u32, height: u32) -> wgpu::TextureView {
        if self.depth_cache.len() >= 8 && !self.depth_cache.contains_key(&(width, height)) {
            self.depth_cache.clear();
        }
        self.depth_cache
            .entry((width, height))
            .or_insert_with(|| create_depth(&self.gpu.device, width, height))
            .clone()
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) -> Result<()> {
        let Some(surface) = self.surface.as_mut() else {
            let status = self.caps.feature(RenderFeature::SurfacePresentation);
            return Err(RenderError::Unsupported {
                feature: status.name.into(),
                reason: status.reason,
            });
        };
        surface.resize(&self.gpu, width, height)
    }

    /// Record and submit one frame into `destination`.
    pub fn draw_frame(
        &mut self,
        destination: &Destination,
        frame: &FrameDesc,
        draws: &[DrawCommand],
    ) -> Result<FrameStats> {
        check_gpu(&self.gpu)?;
        self.wait_for_frame_slot()?;
        let prepared = self.prepare(destination, draws)?;
        let clear = frame.clear_color.map(f64::from);
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("engine frame"),
            });
        let mut batches = 0;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("engine draws"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &destination.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0],
                            g: clear[1],
                            b: clear[2],
                            a: clear[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &destination.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(frame.clear_depth.clamp(0.0, 1.0)),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            let viewport = effective_rect(frame.viewport, destination.width, destination.height);
            if !viewport.is_empty() {
                pass.set_viewport(
                    viewport.x as f32,
                    viewport.y as f32,
                    viewport.width as f32,
                    viewport.height as f32,
                    0.0,
                    1.0,
                );
                let mut previous: Option<&Prepared> = None;
                for draw in prepared.iter().flatten() {
                    let changed = previous.is_none_or(|previous| !previous.same_batch(draw));
                    if changed {
                        let pipeline_changed =
                            previous.is_none_or(|previous| previous.pipeline != draw.pipeline);
                        if pipeline_changed {
                            pass.set_pipeline(&draw.pipeline);
                        }
                        for (index, group, offsets) in &draw.sets {
                            pass.set_bind_group(*index, group, offsets);
                        }
                        pass.set_scissor_rect(
                            draw.scissor.x,
                            draw.scissor.y,
                            draw.scissor.width,
                            draw.scissor.height,
                        );
                        pass.set_vertex_buffer(0, draw.vertex.slice(..));
                        if let Some(index) = &draw.index {
                            pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                        }
                        batches += 1;
                    }
                    if draw.index.is_some() {
                        pass.draw_indexed(0..draw.count, 0, 0..1);
                    } else {
                        pass.draw(0..draw.count, 0..1);
                    }
                    previous = Some(draw);
                }
            }
        }
        check_gpu(&self.gpu)?;
        let index = self.gpu.queue.submit([encoder.finish()]);
        self.submissions.push_back(index);
        self.gpu
            .device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| backend_error("polling GPU frame", error))?;
        check_gpu(&self.gpu)?;
        Ok(FrameStats {
            draws: draws.len(),
            batches,
        })
    }

    /// Backpressure: with `frames_in_flight` submissions pending, wait for the
    /// oldest before recording another frame.
    fn wait_for_frame_slot(&mut self) -> Result<()> {
        while self.submissions.len() >= self.frames_in_flight as usize {
            let Some(index) = self.submissions.pop_front() else {
                break;
            };
            self.gpu
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(index),
                    timeout: Some(std::time::Duration::from_secs(30)),
                })
                .map_err(|error| backend_error("GPU frame backpressure", error))?;
        }
        Ok(())
    }

    /// Resolve every draw's pipeline, bind groups and geometry. Entries for
    /// draws whose scissor is empty are `None`.
    fn prepare(
        &mut self,
        destination: &Destination,
        draws: &[DrawCommand],
    ) -> Result<Vec<Option<Prepared>>> {
        // Uniform data first: growing the arena invalidates cached bind groups.
        let alignment = self.gpu.device.limits().min_uniform_buffer_offset_alignment;
        let mut packer = UniformPacker::new(alignment);
        let mut offsets: Vec<Vec<(UniformSlot, u32)>> = Vec::with_capacity(draws.len());
        for draw in draws {
            offsets.push(
                draw.uniforms
                    .iter()
                    .map(|uniform| (uniform.slot, packer.push(&uniform.bytes)))
                    .collect(),
            );
        }
        self.upload_uniforms(packer)?;

        let mut prepared = Vec::with_capacity(draws.len());
        for (draw, draw_offsets) in draws.iter().zip(&offsets) {
            let scissor = effective_rect(draw.state.scissor, destination.width, destination.height);
            if scissor.is_empty() {
                prepared.push(None);
                continue;
            }
            prepared.push(Some(self.prepare_draw(
                destination,
                draw,
                draw_offsets,
                scissor,
            )?));
        }
        Ok(prepared)
    }

    fn upload_uniforms(&mut self, packer: UniformPacker) -> Result<()> {
        if packer.is_empty() {
            return Ok(());
        }
        let bytes = packer.finish();
        let needed = bytes.len() as u64;
        let fits = self
            .arena
            .as_ref()
            .is_some_and(|arena| arena.capacity >= needed);
        if !fits {
            let capacity = arena_capacity(needed);
            if capacity > self.gpu.device.limits().max_buffer_size {
                return Err(RenderError::Backend(format!(
                    "frame uniform data of {needed} bytes exceeds the device buffer limit"
                )));
            }
            let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("uniform arena"),
                size: capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.arena = Some(Arena { buffer, capacity });
            // Bind groups referenced the previous buffer.
            self.bind_groups.clear();
        }
        let arena = self.arena.as_ref().expect("arena allocated above");
        self.gpu.queue.write_buffer(&arena.buffer, 0, &bytes);
        Ok(())
    }

    fn prepare_draw(
        &mut self,
        destination: &Destination,
        draw: &DrawCommand,
        offsets: &[(UniformSlot, u32)],
        scissor: PixelRect,
    ) -> Result<Prepared> {
        let shader_id = draw.shader.id();
        let mesh = self
            .meshes
            .get(&draw.mesh.id())
            .ok_or_else(|| stale_resource("mesh", draw.mesh.id()))?;
        let (vertex, index, count, layout) = (
            mesh.vertex.clone(),
            mesh.index.clone(),
            mesh.draw_count,
            mesh.layout.clone(),
        );
        let shader = self
            .shaders
            .get(&shader_id)
            .ok_or_else(|| stale_resource("shader", shader_id))?;
        let texture_id = draw.texture.map(|texture| texture.id());
        if let Some(id) = texture_id
            && !self.textures.contains_key(&id)
        {
            return Err(stale_resource("texture", id));
        }

        let key = PipelineKey {
            shader: shader_id,
            layout,
            state: draw.state.pipeline_state(),
            format: destination.format,
        };
        let pipeline = match self.pipelines.get(&key) {
            Some(pipeline) => pipeline.clone(),
            None => {
                let pipeline = shader.create_pipeline(
                    &self.gpu.device,
                    &key.layout,
                    &key.state,
                    key.format,
                )?;
                check_gpu(&self.gpu)?;
                self.pipelines.insert(key, pipeline.clone());
                pipeline
            }
        };

        let mut sets = Vec::with_capacity(shader.groups.len());
        for group in &shader.groups {
            let bind_key = BindKey {
                shader: shader_id,
                group: group.group,
                texture: group.has_texture.then_some(texture_id).flatten(),
            };
            let bind_group = match self.bind_groups.get(&bind_key) {
                Some(bind_group) => bind_group.clone(),
                None => {
                    let texture = texture_id
                        .and_then(|id| self.textures.get(&id))
                        .unwrap_or(&self.white);
                    let arena = self.arena.as_ref();
                    let mut entries = Vec::with_capacity(group.bindings.len());
                    for binding in &group.bindings {
                        let resource = match binding.kind {
                            engine_render::shader::BindingKind::UniformBuffer { size } => {
                                let arena = arena.ok_or_else(|| {
                                    RenderError::InvalidDraw(
                                        "the shader declares a uniform but the draw supplied none"
                                            .into(),
                                    )
                                })?;
                                wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &arena.buffer,
                                    offset: 0,
                                    size: wgpu::BufferSize::new(u64::from(size)),
                                })
                            }
                            engine_render::shader::BindingKind::SampledTexture2d => {
                                wgpu::BindingResource::TextureView(&texture.view)
                            }
                            engine_render::shader::BindingKind::Sampler => {
                                wgpu::BindingResource::Sampler(&self.sampler)
                            }
                        };
                        entries.push(wgpu::BindGroupEntry {
                            binding: binding.slot.binding,
                            resource,
                        });
                    }
                    let created = self
                        .gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("engine bindings"),
                            layout: &group.layout,
                            entries: &entries,
                        });
                    self.bind_groups.insert(bind_key, created.clone());
                    created
                }
            };
            let mut dynamic = Vec::new();
            for (binding, _) in group.uniforms() {
                let offset = offsets
                    .iter()
                    .find(|(slot, _)| *slot == binding.slot)
                    .map(|(_, offset)| *offset)
                    .ok_or_else(|| {
                        RenderError::InvalidDraw(format!(
                            "missing uniform at (group {}, binding {})",
                            binding.slot.group, binding.slot.binding
                        ))
                    })?;
                dynamic.push(offset);
            }
            sets.push((group.group, bind_group, dynamic));
        }
        Ok(Prepared {
            pipeline,
            sets,
            vertex,
            index,
            count,
            scissor,
        })
    }
}
