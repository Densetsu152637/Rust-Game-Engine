//! The initialized backend: every Vulkan object the backend owns, and the logic
//! behind each [`engine_render::RenderBackend`] call.
//!
//! Teardown order (see [`Core::teardown`]): wait idle, deferred items, resources,
//! frame slots, swapchain, pipelines, uploader/sampler, and finally the [`Gpu`]
//! (allocator, device, surface, instance) which is the last field.

use crate::VulkanConfig;
use crate::WindowSource;
use crate::error::VkCheck;
use crate::frame::{BindTracker, DeferredQueue, DrawBinds, FrameSlot, SetKey, UniformPlanner};
use crate::instance::{Gpu, ValidationLog};
use crate::pipeline::{PipelineKey, Program};
use crate::resources::{MeshObj, TextureObj, Uploader, check_texture_size, create_sampler};
use crate::swapchain::{Acquired, Presented, Recreated, SurfaceState};
use crate::target::TargetObj;
use crate::translate::{self, COLOR_FORMAT};
use ash::vk;
use engine_render::shader::BindingKind;
use engine_render::{
    BackendKind, DrawCommand, FrameDesc, FrameOutcome, FrameSkip, FrameStats, MeshHandle,
    MeshResource, OffscreenDesc, ReadbackImage, RenderCapabilities, RenderError, RenderFeature,
    RenderTarget, ResourceId, Result, ShaderHandle, ShaderResource, TargetHandle, Texture2D,
    TextureHandle, UniformSlot,
};
use std::collections::HashMap;

/// Diagnostic counters of an initialized backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanStats {
    /// Physical device name.
    pub adapter: String,
    /// `PhysicalDeviceType` of the adapter (for example `DISCRETE_GPU`).
    pub device_type: String,
    /// Vulkan API version the device reports (major, minor, patch).
    pub api_version: (u32, u32, u32),
    /// Graphics pipelines currently cached.
    pub pipelines: usize,
    /// Retired objects still waiting for in-flight frames.
    pub pending_releases: usize,
    /// Frames submitted to the queue by this backend.
    pub frames_submitted: u64,
    /// Current swapchain extent, if a window swapchain exists.
    pub surface_extent: Option<(u32, u32)>,
}

/// An object whose destruction waits for in-flight frames.
pub(crate) enum Retired {
    Mesh(MeshObj),
    Texture(TextureObj),
    Target(TargetObj),
    Program(Program),
    Pipeline(vk::Pipeline),
}

impl Retired {
    fn destroy(self, gpu: &mut Gpu) {
        match self {
            Self::Mesh(mesh) => mesh.destroy(gpu),
            Self::Texture(texture) => texture.destroy(gpu),
            Self::Target(target) => target.destroy(gpu),
            Self::Program(mut program) => program.destroy(&gpu.device),
            // SAFETY: only called once the frames that used the pipeline completed.
            Self::Pipeline(pipeline) => unsafe { gpu.device.destroy_pipeline(pipeline, None) },
        }
    }
}

/// One draw, resolved to Vulkan handles.
struct PreparedDraw {
    binds: DrawBinds,
    draw_count: u32,
    indexed: bool,
    /// The scissor clamps to nothing, so the draw cannot touch a pixel.
    skip: bool,
}

/// Where a frame renders, known before the swapchain image is acquired.
struct Destination {
    width: u32,
    height: u32,
    format: vk::Format,
    pass: vk::RenderPass,
    /// `None` for the surface (chosen per acquired image).
    framebuffer: Option<vk::Framebuffer>,
}

pub(crate) struct Core {
    caps: RenderCapabilities,
    frames_in_flight: u32,
    /// Frames submitted to the queue by this backend.
    submitted: u64,
    /// Frames `0..completed` are known to have finished on the GPU.
    completed: u64,
    frames: Vec<FrameSlot>,
    deferred: DeferredQueue<Retired>,
    programs: HashMap<u64, Program>,
    meshes: HashMap<u64, MeshObj>,
    textures: HashMap<u64, TextureObj>,
    targets: HashMap<u64, TargetObj>,
    white: Option<TextureObj>,
    sampler: vk::Sampler,
    store: crate::pipeline::PipelineStore,
    uploader: Uploader,
    surface: Option<SurfaceState>,
    /// Must stay last: dropped after `Core::drop` has destroyed everything above.
    gpu: Gpu,
}

fn contract(message: String) -> RenderError {
    RenderError::Backend(format!("backend contract violation: {message}"))
}

fn build_capabilities(gpu: &Gpu, frames: u32, has_window: bool) -> RenderCapabilities {
    let limits = &gpu.info.limits;
    let mut caps = RenderCapabilities::full(BackendKind::Vulkan, "vulkan");
    caps.device_name = Some(gpu.info.name.clone());
    caps.max_frames_in_flight = frames;
    caps.max_texture_dimension = limits
        .max_image_dimension2_d
        .min(limits.max_framebuffer_width)
        .min(limits.max_framebuffer_height);
    caps.backend_ready = true;
    caps.parallel_recording = false;
    caps.portability_subset = gpu.info.portability_subset;
    caps.surface_presentation = has_window;
    let mut reason = |feature: RenderFeature, text: &str| {
        caps.unsupported_reasons.insert(feature, text.to_string());
    };
    reason(
        RenderFeature::CpuParallelRecording,
        "frames are recorded on one thread into one primary command buffer",
    );
    reason(
        RenderFeature::GpuParallelEncoding,
        "frames are recorded on one thread into one primary command buffer",
    );
    if !gpu.info.portability_subset {
        reason(
            RenderFeature::PortabilitySubset,
            "the device does not expose VK_KHR_portability_subset (fully conformant Vulkan)",
        );
    }
    if !has_window {
        reason(
            RenderFeature::SurfacePresentation,
            "the backend was created without a window (offscreen only)",
        );
    }
    caps
}

impl Core {
    pub(crate) fn new(
        config: &VulkanConfig,
        window: Option<&dyn WindowSource>,
        size: (u32, u32),
        library: Option<&std::ffi::OsStr>,
        log: &ValidationLog,
    ) -> Result<Self> {
        let gpu = Gpu::new(config, window, library, log)?;
        let frames_in_flight = config.frames_in_flight.clamp(1, 8);
        let caps = build_capabilities(&gpu, frames_in_flight, window.is_some());
        let mut core = Self {
            caps,
            frames_in_flight,
            submitted: 0,
            completed: 0,
            frames: Vec::new(),
            deferred: DeferredQueue::default(),
            programs: HashMap::new(),
            meshes: HashMap::new(),
            textures: HashMap::new(),
            targets: HashMap::new(),
            white: None,
            sampler: vk::Sampler::null(),
            store: crate::pipeline::PipelineStore::empty(),
            uploader: Uploader::empty(),
            surface: None,
            gpu,
        };
        // On failure `core` is dropped, which destroys whatever was created.
        core.populate(size)?;
        Ok(core)
    }

    fn populate(&mut self, size: (u32, u32)) -> Result<()> {
        let family = self.gpu.queue_family;
        for _ in 0..self.frames_in_flight {
            let slot = FrameSlot::new(&self.gpu.device, family)?;
            self.frames.push(slot);
        }
        self.store.init(&self.gpu.device)?;
        self.uploader.init(&self.gpu.device, family)?;
        self.sampler = create_sampler(&self.gpu.device)?;
        let white = TextureObj::create(&mut self.gpu, &self.uploader, &Texture2D::white())?;
        self.white = Some(white);
        if self.gpu.instance.surface.is_some() {
            let mut surface = SurfaceState::new(&self.gpu, size);
            surface.recreate(&mut self.gpu, &mut self.store)?;
            self.surface = Some(surface);
            self.refresh_acquire_semaphores()?;
        }
        Ok(())
    }

    pub(crate) fn capabilities(&self) -> &RenderCapabilities {
        &self.caps
    }

    pub(crate) fn stats(&self) -> VulkanStats {
        let version = self.gpu.info.api_version;
        VulkanStats {
            adapter: self.gpu.info.name.clone(),
            device_type: format!("{:?}", self.gpu.info.device_type),
            api_version: (
                vk::api_version_major(version),
                vk::api_version_minor(version),
                vk::api_version_patch(version),
            ),
            pipelines: self.store.len(),
            pending_releases: self.deferred.len(),
            frames_submitted: self.submitted,
            surface_extent: self
                .surface
                .as_ref()
                .and_then(|s| s.extent())
                .map(|e| (e.width, e.height)),
        }
    }

    // ------------------------------------------------------------ resources

    pub(crate) fn create_shader(
        &mut self,
        handle: ShaderHandle,
        resource: &ShaderResource,
    ) -> Result<()> {
        let program = Program::create(&self.gpu.device, &self.gpu.info.limits, resource)?;
        self.programs.insert(handle.id(), program);
        Ok(())
    }

    pub(crate) fn create_mesh(
        &mut self,
        handle: MeshHandle,
        resource: &MeshResource,
    ) -> Result<()> {
        let mesh = MeshObj::create(&mut self.gpu, resource)?;
        self.meshes.insert(handle.id(), mesh);
        Ok(())
    }

    pub(crate) fn update_mesh(
        &mut self,
        handle: MeshHandle,
        resource: &MeshResource,
    ) -> Result<()> {
        if !self.meshes.contains_key(&handle.id()) {
            return Err(RenderError::StaleHandle(handle.into()));
        }
        let fresh = MeshObj::create(&mut self.gpu, resource)?;
        if let Some(old) = self.meshes.insert(handle.id(), fresh) {
            self.deferred.push(self.submitted, Retired::Mesh(old));
        }
        self.collect();
        Ok(())
    }

    pub(crate) fn create_texture(
        &mut self,
        handle: TextureHandle,
        texture: &Texture2D,
    ) -> Result<()> {
        check_texture_size(texture, self.caps.max_texture_dimension)?;
        let created = TextureObj::create(&mut self.gpu, &self.uploader, texture)?;
        self.textures.insert(handle.id(), created);
        Ok(())
    }

    pub(crate) fn update_texture(
        &mut self,
        handle: TextureHandle,
        texture: &Texture2D,
    ) -> Result<()> {
        if !self.textures.contains_key(&handle.id()) {
            return Err(RenderError::StaleHandle(handle.into()));
        }
        check_texture_size(texture, self.caps.max_texture_dimension)?;
        let fresh = TextureObj::create(&mut self.gpu, &self.uploader, texture)?;
        if let Some(old) = self.textures.insert(handle.id(), fresh) {
            self.deferred.push(self.submitted, Retired::Texture(old));
        }
        self.collect();
        Ok(())
    }

    pub(crate) fn create_target(
        &mut self,
        handle: TargetHandle,
        desc: &OffscreenDesc,
    ) -> Result<()> {
        if desc.width > self.caps.max_texture_dimension
            || desc.height > self.caps.max_texture_dimension
        {
            return Err(RenderError::InvalidResource(format!(
                "offscreen target {}x{} exceeds the device limit {}",
                desc.width, desc.height, self.caps.max_texture_dimension
            )));
        }
        let target = TargetObj::create(
            &mut self.gpu,
            &self.uploader,
            self.store.offscreen_pass,
            desc.width,
            desc.height,
        )?;
        self.targets.insert(handle.id(), target);
        Ok(())
    }

    pub(crate) fn destroy_resource(&mut self, id: ResourceId) -> Result<()> {
        let stale = || RenderError::StaleHandle(id);
        match id {
            ResourceId::Shader(handle) => {
                let program = self.programs.remove(&handle.id()).ok_or_else(stale)?;
                for pipeline in self.store.take_shader(handle.id()) {
                    self.deferred
                        .push(self.submitted, Retired::Pipeline(pipeline));
                }
                self.deferred
                    .push(self.submitted, Retired::Program(program));
            }
            ResourceId::Mesh(handle) => {
                let mesh = self.meshes.remove(&handle.id()).ok_or_else(stale)?;
                self.deferred.push(self.submitted, Retired::Mesh(mesh));
            }
            ResourceId::Texture(handle) => {
                let texture = self.textures.remove(&handle.id()).ok_or_else(stale)?;
                self.deferred
                    .push(self.submitted, Retired::Texture(texture));
            }
            ResourceId::Target(handle) => {
                let target = self.targets.remove(&handle.id()).ok_or_else(stale)?;
                self.deferred.push(self.submitted, Retired::Target(target));
            }
        }
        self.collect();
        Ok(())
    }

    /// Destroy retired objects whose frames completed.
    fn collect(&mut self) {
        for item in self.deferred.drain_ready(self.completed) {
            item.destroy(&mut self.gpu);
        }
    }

    // ------------------------------------------------------------ idle/resize

    pub(crate) fn wait_idle(&mut self) -> Result<()> {
        self.gpu.wait_idle()?;
        self.completed = self.submitted;
        for slot in &mut self.frames {
            slot.in_flight = None;
        }
        self.collect();
        Ok(())
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let Some(surface) = self.surface.as_mut() else {
            return Err(RenderError::Unsupported {
                feature: "surface-presentation".into(),
                reason: "the backend was created without a window".into(),
            });
        };
        surface.requested = (width, height);
        surface.recreate(&mut self.gpu, &mut self.store)?;
        self.refresh_acquire_semaphores()
    }

    /// After a swapchain change an unconsumed acquire signal could linger.
    fn refresh_acquire_semaphores(&mut self) -> Result<()> {
        for slot in &mut self.frames {
            slot.recreate_semaphore(&self.gpu.device)?;
        }
        Ok(())
    }

    pub(crate) fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        if !self.targets.contains_key(&target.id()) {
            return Err(RenderError::StaleHandle(target.into()));
        }
        self.wait_idle()?;
        let Some(target) = self.targets.get(&target.id()) else {
            return Err(RenderError::StaleHandle(target.into()));
        };
        target.read(&mut self.gpu, &self.uploader)
    }

    // ------------------------------------------------------------- the frame

    pub(crate) fn submit_frame(
        &mut self,
        frame: &FrameDesc,
        draws: &[DrawCommand],
    ) -> Result<FrameOutcome> {
        if frame.target == RenderTarget::Surface {
            let Some(surface) = self.surface.as_mut() else {
                return Err(RenderError::Unsupported {
                    feature: "surface-presentation".into(),
                    reason: "the backend was created without a window".into(),
                });
            };
            if surface.swapchain.is_none()
                && surface.recreate(&mut self.gpu, &mut self.store)? == Recreated::Suspended
            {
                return Ok(FrameOutcome::Skipped(FrameSkip::Suspended));
            }
        }

        // Reuse the slot only once its previous frame finished on the GPU.
        let slot_index = (self.submitted % u64::from(self.frames_in_flight)) as usize;
        if let Some(done) = self.frames[slot_index].wait(&self.gpu.device)? {
            self.completed = self.completed.max(done + 1);
        }
        self.collect();
        self.frames[slot_index]
            .descriptors
            .reset(&self.gpu.device)?;
        self.frames[slot_index].set_cache.clear();

        let destination = self.destination(frame.target)?;
        let prepared = self.prepare_draws(slot_index, draws, &destination)?;

        // Acquire last: everything fallible except recording is done.
        let mut acquired_index = None;
        let mut suboptimal = false;
        let mut framebuffer = destination.framebuffer;
        if frame.target == RenderTarget::Surface {
            let Some(surface) = self.surface.as_ref() else {
                return Err(contract("surface vanished".into()));
            };
            match surface.acquire(self.frames[slot_index].image_available)? {
                Acquired::Image {
                    index,
                    suboptimal: sub,
                } => {
                    suboptimal = sub;
                    acquired_index = Some(index);
                    framebuffer = surface
                        .swapchain
                        .as_ref()
                        .and_then(|s| s.framebuffers.get(index as usize).copied());
                }
                Acquired::OutOfDate => {
                    return Ok(match self.recreate_surface()? {
                        Recreated::Suspended => FrameOutcome::Skipped(FrameSkip::Suspended),
                        Recreated::Ready => FrameOutcome::Skipped(FrameSkip::OutOfDate),
                    });
                }
                Acquired::Timeout => return Ok(FrameOutcome::Skipped(FrameSkip::Timeout)),
            }
        }
        let framebuffer =
            framebuffer.ok_or_else(|| contract("no framebuffer for the frame target".into()))?;

        let stats = self.record(
            slot_index,
            frame,
            &destination,
            framebuffer,
            &prepared,
            draws.len(),
        )?;
        self.submit_and_present(slot_index, acquired_index, suboptimal)?;
        Ok(FrameOutcome::Rendered(stats))
    }

    fn destination(&self, target: RenderTarget) -> Result<Destination> {
        match target {
            RenderTarget::Offscreen(handle) => {
                let target = self
                    .targets
                    .get(&handle.id())
                    .ok_or(RenderError::StaleHandle(handle.into()))?;
                Ok(Destination {
                    width: target.width,
                    height: target.height,
                    format: COLOR_FORMAT,
                    pass: self.store.offscreen_pass,
                    framebuffer: Some(target.framebuffer),
                })
            }
            RenderTarget::Surface => {
                let swapchain = self
                    .surface
                    .as_ref()
                    .and_then(|s| s.swapchain.as_ref())
                    .ok_or_else(|| contract("surface has no swapchain".into()))?;
                let (_, pass) = self
                    .store
                    .surface_pass
                    .ok_or_else(|| contract("surface has no render pass".into()))?;
                Ok(Destination {
                    width: swapchain.extent.width,
                    height: swapchain.extent.height,
                    format: swapchain.format,
                    pass,
                    framebuffer: None,
                })
            }
        }
    }

    fn recreate_surface(&mut self) -> Result<Recreated> {
        let Some(surface) = self.surface.as_mut() else {
            return Ok(Recreated::Suspended);
        };
        let outcome = surface.recreate(&mut self.gpu, &mut self.store)?;
        // `recreate` waited for the device, so every frame has completed.
        self.completed = self.submitted;
        self.refresh_acquire_semaphores()?;
        Ok(outcome)
    }

    /// Resolve draws to Vulkan handles: plan and write uniforms, find or build
    /// pipelines, and allocate descriptor sets (all before any recording).
    fn prepare_draws(
        &mut self,
        slot_index: usize,
        draws: &[DrawCommand],
        destination: &Destination,
    ) -> Result<Vec<PreparedDraw>> {
        // 1. Uniform placement, in shader declaration order per set.
        let mut planner = UniformPlanner::new(self.gpu.uniform_alignment());
        let mut placed: Vec<Vec<(UniformSlot, u32)>> = Vec::with_capacity(draws.len());
        for draw in draws {
            let program = self
                .programs
                .get(&draw.shader.id())
                .ok_or(RenderError::StaleHandle(draw.shader.into()))?;
            let mut offsets = Vec::new();
            for group in 0..program.plan.groups.len() {
                for slot in program.plan.dynamic_slots(group) {
                    let uniform =
                        draw.uniforms
                            .iter()
                            .find(|u| u.slot == slot)
                            .ok_or_else(|| {
                                contract(format!(
                                    "draw {} lacks uniform (group {}, binding {})",
                                    draw.order, slot.group, slot.binding
                                ))
                            })?;
                    offsets.push((slot, planner.place(slot, &uniform.bytes)?));
                }
            }
            placed.push(offsets);
        }

        // 2. The arena, filled before any descriptor set points at it.
        let frame = &mut self.frames[slot_index];
        frame.ensure_arena(self.gpu.memory(), planner.size())?;
        if let Some(arena) = frame.arena.as_mut() {
            for (offset, bytes) in planner.writes() {
                arena.write(*offset as usize, bytes)?;
            }
        }
        let arena_buffer = frame.arena.as_ref().map(|a| a.raw);

        // 3. Pipelines, descriptor sets and bind records.
        let white_view = self
            .white
            .as_ref()
            .map(|t| t.view)
            .ok_or_else(|| contract("white texture missing".into()))?;
        let mut prepared = Vec::with_capacity(draws.len());
        for (draw, offsets) in draws.iter().zip(&placed) {
            let program = self
                .programs
                .get(&draw.shader.id())
                .ok_or(RenderError::StaleHandle(draw.shader.into()))?;
            let mesh = self
                .meshes
                .get(&draw.mesh.id())
                .ok_or(RenderError::StaleHandle(draw.mesh.into()))?;
            let key = PipelineKey {
                shader: draw.shader.id(),
                layout: mesh.layout.clone(),
                state: draw.state.pipeline_state(),
                format: destination.format,
            };
            let pipeline = self.store.get_or_create(&self.gpu.device, program, &key)?;
            let texture_view = match draw.texture {
                Some(handle) => {
                    self.textures
                        .get(&handle.id())
                        .ok_or(RenderError::StaleHandle(handle.into()))?
                        .view
                }
                None => white_view,
            };

            let mut sets = Vec::new();
            for (set_index, group) in program.plan.groups.iter().enumerate() {
                if group.is_empty() {
                    continue;
                }
                let texture_id = if group.has_texture() {
                    draw.texture.map(|t| t.id())
                } else {
                    None
                };
                let cache_key: SetKey = (draw.shader.id(), set_index as u32, texture_id);
                let frame = &mut self.frames[slot_index];
                let set = match frame.set_cache.get(&cache_key) {
                    Some(set) => *set,
                    None => {
                        let set = frame
                            .descriptors
                            .allocate(&self.gpu.device, program.set_layouts[set_index])?;
                        write_set(
                            &self.gpu.device,
                            set,
                            group,
                            arena_buffer,
                            texture_view,
                            self.sampler,
                        )?;
                        frame.set_cache.insert(cache_key, set);
                        set
                    }
                };
                let dynamic: Vec<u32> = group
                    .dynamic_bindings()
                    .filter_map(|b| {
                        let slot = UniformSlot::new(set_index as u32, b.binding);
                        offsets.iter().find(|(s, _)| *s == slot).map(|(_, o)| *o)
                    })
                    .collect();
                sets.push((set_index as u32, set, dynamic));
            }

            let scissor = translate::effective_scissor(
                draw.state.scissor,
                destination.width,
                destination.height,
            );
            prepared.push(PreparedDraw {
                binds: DrawBinds {
                    pipeline,
                    layout: program.pipeline_layout,
                    vertex: mesh.vertex_buffer(),
                    index: mesh.index_buffer(),
                    sets,
                    scissor,
                },
                draw_count: mesh.draw_count,
                indexed: mesh.index_buffer().is_some(),
                skip: scissor.is_empty(),
            });
        }
        Ok(prepared)
    }

    /// Record the frame into the slot's command buffer.
    fn record(
        &mut self,
        slot_index: usize,
        frame: &FrameDesc,
        destination: &Destination,
        framebuffer: vk::Framebuffer,
        prepared: &[PreparedDraw],
        draw_count: usize,
    ) -> Result<FrameStats> {
        let device = &self.gpu.device;
        let slot = &self.frames[slot_index];
        let cmd = slot.cmd;
        // SAFETY: the slot's fence was waited, so its command buffer is idle.
        unsafe { device.reset_command_pool(slot.pool, vk::CommandPoolResetFlags::empty()) }
            .vk("vkResetCommandPool")?;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: `cmd` is in the initial state after the pool reset.
        unsafe { device.begin_command_buffer(cmd, &begin) }.vk("vkBeginCommandBuffer")?;

        let clears = translate::clear_values(frame.clear_color, frame.clear_depth);
        let pass_info = vk::RenderPassBeginInfo::default()
            .render_pass(destination.pass)
            .framebuffer(framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: vk::Extent2D {
                    width: destination.width,
                    height: destination.height,
                },
            })
            .clear_values(&clears);
        // SAFETY: `cmd` is recording; the pass and framebuffer are compatible.
        unsafe { device.cmd_begin_render_pass(cmd, &pass_info, vk::SubpassContents::INLINE) };

        let viewport =
            translate::effective_viewport(frame.viewport, destination.width, destination.height);
        let mut tracker = BindTracker::default();
        if !viewport.is_empty() {
            // SAFETY: `cmd` is recording inside the render pass; the pipelines
            // declare viewport and scissor as dynamic state.
            unsafe { device.cmd_set_viewport(cmd, 0, &[translate::flipped_viewport(viewport)]) };
            for draw in prepared.iter().filter(|d| !d.skip) {
                record_draw(device, cmd, &mut tracker, draw);
            }
        }
        // SAFETY: matches the begin above.
        unsafe { device.cmd_end_render_pass(cmd) };
        // SAFETY: `cmd` is recording.
        unsafe { device.end_command_buffer(cmd) }.vk("vkEndCommandBuffer")?;
        Ok(FrameStats {
            draws: draw_count,
            batches: tracker.batches(),
        })
    }

    fn submit_and_present(
        &mut self,
        slot_index: usize,
        image: Option<u32>,
        suboptimal: bool,
    ) -> Result<()> {
        let device = &self.gpu.device;
        let slot = &mut self.frames[slot_index];
        let buffers = [slot.cmd];
        let wait = [slot.image_available];
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signal: Vec<vk::Semaphore> = match (image, &self.surface) {
            (Some(index), Some(surface)) => surface
                .swapchain
                .as_ref()
                .and_then(|s| s.render_finished.get(index as usize).copied())
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let mut submit = vk::SubmitInfo::default().command_buffers(&buffers);
        if image.is_some() {
            submit = submit
                .wait_semaphores(&wait)
                .wait_dst_stage_mask(&stages)
                .signal_semaphores(&signal);
        }
        // SAFETY: the fence is signaled (the slot was waited) and belongs to this device.
        unsafe { device.reset_fences(&[slot.fence]) }.vk("vkResetFences")?;
        // SAFETY: the queue, command buffer, semaphores and fence belong to this
        // device; the command buffer is fully recorded.
        unsafe { device.queue_submit(self.gpu.queue, &[submit], slot.fence) }
            .vk("vkQueueSubmit")?;
        slot.in_flight = Some(self.submitted);
        self.submitted += 1;

        let (Some(index), Some(surface)) = (image, &self.surface) else {
            return Ok(());
        };
        let wait_present = signal
            .first()
            .copied()
            .ok_or_else(|| contract("no render-finished semaphore".into()))?;
        let presented = surface.present(self.gpu.queue, index, wait_present)?;
        if suboptimal || !matches!(presented, Presented::Ok) {
            self.recreate_surface()?;
        }
        Ok(())
    }

    // -------------------------------------------------------------- teardown

    /// Destroy everything except the [`Gpu`]. Idempotent.
    fn teardown(&mut self) {
        let _ = self.gpu.wait_idle();
        self.completed = self.submitted;
        for item in self.deferred.drain_all() {
            item.destroy(&mut self.gpu);
        }
        for (_, mesh) in self.meshes.drain() {
            mesh.destroy(&mut self.gpu);
        }
        for (_, texture) in self.textures.drain() {
            texture.destroy(&mut self.gpu);
        }
        for (_, target) in self.targets.drain() {
            target.destroy(&mut self.gpu);
        }
        for (_, mut program) in self.programs.drain() {
            program.destroy(&self.gpu.device);
        }
        if let Some(white) = self.white.take() {
            white.destroy(&mut self.gpu);
        }
        for mut slot in std::mem::take(&mut self.frames) {
            slot.destroy(&mut self.gpu);
        }
        if let Some(mut surface) = self.surface.take() {
            surface.destroy(&mut self.gpu);
        }
        self.store.destroy(&self.gpu.device);
        self.uploader.destroy(&self.gpu.device);
        // SAFETY: the device is idle and nothing uses the sampler.
        unsafe { self.gpu.device.destroy_sampler(self.sampler, None) };
        self.sampler = vk::Sampler::null();
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Record one draw, binding only what changed since the previous one.
fn record_draw(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    tracker: &mut BindTracker,
    draw: &PreparedDraw,
) {
    let binds = &draw.binds;
    let delta = tracker.next(binds);
    // SAFETY: `cmd` is recording inside a render pass; every handle was created
    // from this device and stays alive until the frame completes.
    unsafe {
        if delta.pipeline {
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, binds.pipeline);
        }
        if delta.scissor {
            device.cmd_set_scissor(cmd, 0, &[translate::rect_to_vk(binds.scissor)]);
        }
        if delta.vertex {
            device.cmd_bind_vertex_buffers(cmd, 0, &[binds.vertex], &[0]);
        }
        if delta.index
            && let Some(index) = binds.index
        {
            device.cmd_bind_index_buffer(cmd, index, 0, vk::IndexType::UINT32);
        }
        for &i in &delta.sets {
            let (set_index, set, offsets) = &binds.sets[i];
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                binds.layout,
                *set_index,
                &[*set],
                offsets,
            );
        }
        if draw.indexed {
            device.cmd_draw_indexed(cmd, draw.draw_count, 1, 0, 0, 0);
        } else {
            device.cmd_draw(cmd, draw.draw_count, 1, 0, 0);
        }
    }
}

/// Point a freshly allocated set at the arena, texture view and sampler.
fn write_set(
    device: &ash::Device,
    set: vk::DescriptorSet,
    group: &crate::pipeline::GroupPlan,
    arena: Option<vk::Buffer>,
    texture_view: vk::ImageView,
    sampler: vk::Sampler,
) -> Result<()> {
    enum Info {
        Buffer(vk::DescriptorBufferInfo),
        Image(vk::DescriptorImageInfo),
    }
    let mut infos = Vec::with_capacity(group.bindings.len());
    for binding in &group.bindings {
        infos.push(match binding.kind {
            BindingKind::UniformBuffer { size } => {
                let buffer =
                    arena.ok_or_else(|| contract("uniform arena missing for a draw".into()))?;
                Info::Buffer(
                    vk::DescriptorBufferInfo::default()
                        .buffer(buffer)
                        .offset(0)
                        .range(u64::from(size)),
                )
            }
            BindingKind::SampledTexture2d => Info::Image(
                vk::DescriptorImageInfo::default()
                    .image_view(texture_view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            ),
            BindingKind::Sampler => {
                Info::Image(vk::DescriptorImageInfo::default().sampler(sampler))
            }
        });
    }
    let writes: Vec<vk::WriteDescriptorSet<'_>> = group
        .bindings
        .iter()
        .zip(&infos)
        .map(|(binding, info)| {
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(binding.binding)
                .descriptor_type(translate::descriptor_type(binding.kind));
            match info {
                Info::Buffer(buffer) => write.buffer_info(std::slice::from_ref(buffer)),
                Info::Image(image) => write.image_info(std::slice::from_ref(image)),
            }
        })
        .collect();
    // SAFETY: the set was just allocated from this device and every referenced
    // buffer, view and sampler is alive.
    unsafe { device.update_descriptor_sets(&writes, &[]) };
    Ok(())
}
