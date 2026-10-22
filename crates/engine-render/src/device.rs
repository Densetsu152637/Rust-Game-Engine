//! [`RenderDevice`]: handle ownership, validation and deferred destruction on
//! top of a [`RenderBackend`].

use crate::backend::{RenderBackend, RenderCapabilities};
use crate::command::{DrawCommand, RenderCommandBuffer};
use crate::error::{RenderError, Result};
use crate::frame::{
    FrameDesc, FrameOutcome, OffscreenDesc, ReadbackImage, RenderTarget, TargetFormat,
};
use crate::resources::{
    MeshHandle, MeshResource, ResourceId, ResourceKind, ShaderHandle, ShaderResource, TargetHandle,
    Texture2D, TextureHandle, VertexLayout,
};
use crate::shader::ShaderInterface;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Device-level settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    /// Frames the CPU may run ahead of the GPU, `1..=8`. Clamped to the
    /// backend's `max_frames_in_flight`. Also the number of frames a destroyed
    /// resource stays alive in the backend.
    pub frames_in_flight: u32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            frames_in_flight: 2,
        }
    }
}

/// Counts of tracked resources.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceCounts {
    pub shaders: usize,
    pub meshes: usize,
    pub textures: usize,
    pub targets: usize,
    /// Destroyed by the caller but still owned by the backend until in-flight
    /// frames complete.
    pub pending_destruction: usize,
}

enum Live {
    Shader(Arc<ShaderInterface>),
    Mesh { layout: VertexLayout },
    Texture,
    Target { width: u32, height: u32 },
}

struct Retiring {
    id: ResourceId,
    /// Backend destroy happens once this many frames have been submitted.
    release_at: u64,
}

/// Owns a backend, mints typed handles for the resources created through it, and
/// validates and orders the frames submitted to it.
///
/// * Handle ids are never reused: a destroyed handle is rejected with
///   [`RenderError::StaleHandle`] forever.
/// * `destroy` is deferred: the backend releases a resource only after
///   `frames_in_flight` further frames were submitted, so frames that may still
///   be executing on the GPU never see a freed object. `wait_idle` and
///   `shutdown` flush the queue immediately.
/// * A [`RenderError::DeviceLost`] from the backend poisons the device. Every
///   other error (including [`RenderError::Backend`] from a rejected resource
///   and [`RenderError::LimitExceeded`] from pre-validation against
///   [`crate::DeviceLimits`]) fails only its own call.
pub struct RenderDevice<B: RenderBackend = Box<dyn RenderBackend>> {
    backend: B,
    caps: RenderCapabilities,
    frames_in_flight: u32,
    next_id: u64,
    frames_submitted: u64,
    live: BTreeMap<ResourceId, Live>,
    retiring: VecDeque<Retiring>,
    lost: Option<RenderError>,
    shut_down: bool,
}

impl<B: RenderBackend> RenderDevice<B> {
    /// Initialize the backend and wrap it. Fails with the backend's own
    /// diagnostic if it cannot become ready.
    pub fn new(mut backend: B, config: DeviceConfig) -> Result<Self> {
        if !(1..=8).contains(&config.frames_in_flight) {
            return Err(RenderError::InvalidDraw(
                "frames in flight must be 1..=8".into(),
            ));
        }
        backend.initialize()?;
        let caps = backend.capabilities();
        if !caps.backend_ready {
            return Err(RenderError::Unavailable(format!(
                "backend reported not ready after initialize\n{}",
                caps.diagnostics()
            )));
        }
        let frames_in_flight = config
            .frames_in_flight
            .min(caps.max_frames_in_flight.max(1));
        Ok(Self {
            backend,
            caps,
            frames_in_flight,
            next_id: 1,
            frames_submitted: 0,
            live: BTreeMap::new(),
            retiring: VecDeque::new(),
            lost: None,
            shut_down: false,
        })
    }

    pub fn capabilities(&self) -> &RenderCapabilities {
        &self.caps
    }

    pub fn frames_in_flight(&self) -> u32 {
        self.frames_in_flight
    }

    /// Direct backend access for backend-specific inherent APIs.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Frames successfully submitted so far (skipped frames do not count).
    pub fn frames_submitted(&self) -> u64 {
        self.frames_submitted
    }

    /// Whether `id` can still be used in draws.
    pub fn is_alive(&self, id: impl Into<ResourceId>) -> bool {
        self.live.contains_key(&id.into())
    }

    pub fn counts(&self) -> ResourceCounts {
        let mut counts = ResourceCounts {
            pending_destruction: self.retiring.len(),
            ..Default::default()
        };
        for id in self.live.keys() {
            match id.kind() {
                ResourceKind::Shader => counts.shaders += 1,
                ResourceKind::Mesh => counts.meshes += 1,
                ResourceKind::Texture => counts.textures += 1,
                ResourceKind::Target => counts.targets += 1,
            }
        }
        counts
    }

    fn guard(&self) -> Result<()> {
        if let Some(error) = &self.lost {
            return Err(error.clone());
        }
        if self.shut_down {
            return Err(RenderError::NotReady("device was shut down".into()));
        }
        Ok(())
    }

    fn note<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error @ RenderError::DeviceLost(_)) = &result {
            self.lost = Some(error.clone());
        }
        result
    }

    fn mint<H>(&mut self, make: impl FnOnce(u64) -> H) -> H {
        let id = self.next_id;
        self.next_id += 1;
        make(id)
    }

    // ------------------------------------------------------------ creation

    pub fn create_shader(&mut self, resource: ShaderResource) -> Result<ShaderHandle> {
        self.guard()?;
        let interface = ShaderInterface::reflect(&resource)?;
        interface.check_limits(&self.caps.limits)?;
        let handle = self.mint(ShaderHandle::from_raw);
        let result = self.backend.create_shader(handle, &resource);
        self.note(result)?;
        self.live
            .insert(handle.into(), Live::Shader(Arc::new(interface)));
        Ok(handle)
    }

    pub fn create_mesh(&mut self, resource: &MeshResource) -> Result<MeshHandle> {
        self.guard()?;
        resource.validate()?;
        resource.layout.check_limits(&self.caps.limits)?;
        let handle = self.mint(MeshHandle::from_raw);
        let result = self.backend.create_mesh(handle, resource);
        self.note(result)?;
        self.live.insert(
            handle.into(),
            Live::Mesh {
                layout: resource.layout.clone(),
            },
        );
        Ok(handle)
    }

    pub fn create_texture(&mut self, texture: &Texture2D) -> Result<TextureHandle> {
        self.guard()?;
        texture.validate()?;
        self.check_texture_support(texture.width, texture.height, "texture")?;
        let handle = self.mint(TextureHandle::from_raw);
        let result = self.backend.create_texture(handle, texture);
        self.note(result)?;
        self.live.insert(handle.into(), Live::Texture);
        Ok(handle)
    }

    /// Create an offscreen RGBA8 sRGB target with an owned depth attachment.
    pub fn create_target(&mut self, width: u32, height: u32) -> Result<TargetHandle> {
        self.guard()?;
        if !self.caps.offscreen_targets {
            return Err(self.unsupported(crate::backend::RenderFeature::OffscreenTargets));
        }
        self.check_texture_support(width, height, "offscreen target")?;
        let handle = self.mint(TargetHandle::from_raw);
        let desc = OffscreenDesc {
            width,
            height,
            format: TargetFormat::Rgba8Srgb,
        };
        let result = self.backend.create_target(handle, &desc);
        self.note(result)?;
        self.live
            .insert(handle.into(), Live::Target { width, height });
        Ok(handle)
    }

    fn check_texture_support(&self, width: u32, height: u32, what: &str) -> Result<()> {
        if !self.caps.sampled_textures {
            return Err(self.unsupported(crate::backend::RenderFeature::SampledTextures));
        }
        let limit = self.caps.max_texture_dimension;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(RenderError::InvalidResource(format!(
                "{what} {width}x{height} must be within 1..={limit} per side"
            )));
        }
        Ok(())
    }

    fn unsupported(&self, feature: crate::backend::RenderFeature) -> RenderError {
        let status = self.caps.feature(feature);
        RenderError::Unsupported {
            feature: status.name.into(),
            reason: status.reason,
        }
    }

    // ------------------------------------------------------------- updates

    pub fn update_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        self.guard()?;
        self.require(handle.into())?;
        resource.validate()?;
        resource.layout.check_limits(&self.caps.limits)?;
        let result = self.backend.update_mesh(handle, resource);
        self.note(result)?;
        self.live.insert(
            handle.into(),
            Live::Mesh {
                layout: resource.layout.clone(),
            },
        );
        Ok(())
    }

    pub fn update_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        self.guard()?;
        self.require(handle.into())?;
        texture.validate()?;
        self.check_texture_support(texture.width, texture.height, "texture")?;
        let result = self.backend.update_texture(handle, texture);
        self.note(result)
    }

    fn require(&self, id: ResourceId) -> Result<&Live> {
        self.live.get(&id).ok_or(RenderError::StaleHandle(id))
    }

    // --------------------------------------------------------- destruction

    /// Retire a resource. It is unusable immediately; the backend releases it
    /// once every frame submitted before this call has finished.
    pub fn destroy(&mut self, id: impl Into<ResourceId>) -> Result<()> {
        self.guard()?;
        let id = id.into();
        if self.live.remove(&id).is_none() {
            return Err(RenderError::StaleHandle(id));
        }
        self.retiring.push_back(Retiring {
            id,
            release_at: self.frames_submitted + u64::from(self.frames_in_flight),
        });
        Ok(())
    }

    /// Release every retired resource whose frames have completed. Called after
    /// each submitted frame; exposed for hosts that skip frames.
    pub fn collect_garbage(&mut self) -> Result<usize> {
        self.guard()?;
        let mut released = 0;
        while self
            .retiring
            .front()
            .is_some_and(|r| r.release_at <= self.frames_submitted)
        {
            let retiring = self.retiring.pop_front().expect("front checked");
            let result = self.backend.destroy_resource(retiring.id);
            self.note(result)?;
            released += 1;
        }
        Ok(released)
    }

    // ---------------------------------------------------------- submission

    pub fn make_frame(&self) -> RenderCommandBuffer {
        RenderCommandBuffer::new()
    }

    /// Resize the presentation surface. Zero is a suspended surface, not a size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.guard()?;
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidResource(
                "zero size is a suspended surface, not a render target".into(),
            ));
        }
        if width > self.caps.max_texture_dimension || height > self.caps.max_texture_dimension {
            return Err(RenderError::InvalidResource(format!(
                "surface {width}x{height} exceeds the device limit {}",
                self.caps.max_texture_dimension
            )));
        }
        if !self.caps.surface_presentation {
            return Err(self.unsupported(crate::backend::RenderFeature::SurfacePresentation));
        }
        let result = self.backend.resize(width, height);
        self.note(result)?;
        Ok(())
    }

    /// Validate every draw, then hand the ordered frame to the backend. On
    /// validation failure nothing is submitted and no state changes. After a
    /// rendered frame, retired resources whose frames completed are released.
    pub fn submit(
        &mut self,
        mut desc: FrameDesc,
        commands: RenderCommandBuffer,
    ) -> Result<FrameOutcome> {
        self.guard()?;
        match desc.target {
            RenderTarget::Surface => {
                if !self.caps.surface_presentation {
                    return Err(
                        self.unsupported(crate::backend::RenderFeature::SurfacePresentation)
                    );
                }
            }
            RenderTarget::Offscreen(target) => {
                self.require(target.into())?;
            }
        }
        let draws = commands.into_ordered();
        for draw in &draws {
            self.validate_draw(draw)?;
        }
        desc.frame = self.frames_submitted;
        let result = self.backend.submit_frame(&desc, &draws);
        let outcome = self.note(result)?;
        if matches!(outcome, FrameOutcome::Rendered(_)) {
            self.frames_submitted += 1;
            self.collect_garbage()?;
        }
        Ok(outcome)
    }

    fn validate_draw(&self, draw: &DrawCommand) -> Result<()> {
        let label = |what: &str| format!("draw {}: {what}", draw.order);
        let Live::Shader(interface) = self.require(draw.shader.into())? else {
            return Err(RenderError::InvalidDraw(label(
                "shader handle is not a shader",
            )));
        };
        let Live::Mesh { layout } = self.require(draw.mesh.into())? else {
            return Err(RenderError::InvalidDraw(label("mesh handle is not a mesh")));
        };
        if let Some(texture) = draw.texture {
            self.require(texture.into())?;
            if !interface.uses_texture() {
                return Err(RenderError::InvalidDraw(label(
                    "a texture is bound but the shader samples none",
                )));
            }
        }
        interface
            .check_vertex_layout(layout)
            .and_then(|()| interface.check_uniforms(&draw.uniforms))
            .map_err(|error| match error {
                RenderError::InvalidDraw(message) => RenderError::InvalidDraw(label(&message)),
                other => other,
            })
    }

    /// Wait for the GPU, then copy an offscreen target to tightly packed RGBA8.
    pub fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        self.guard()?;
        self.require(target.into())?;
        let result = self.backend.read_target(target);
        self.note(result)
    }

    /// Size an offscreen target was created with.
    pub fn target_size(&self, target: TargetHandle) -> Option<(u32, u32)> {
        match self.live.get(&target.into())? {
            Live::Target { width, height } => Some((*width, *height)),
            _ => None,
        }
    }

    /// Wait for all submitted work and release every retired resource.
    pub fn wait_idle(&mut self) -> Result<()> {
        self.guard()?;
        let result = self.backend.wait_idle();
        self.note(result)?;
        while let Some(retiring) = self.retiring.pop_front() {
            let result = self.backend.destroy_resource(retiring.id);
            self.note(result)?;
        }
        Ok(())
    }

    /// Wait for the GPU, destroy every remaining resource (retired first, then
    /// live in handle order) and shut the backend down. Safe to call twice.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.shut_down {
            return Ok(());
        }
        let mut first_error = None;
        let mut keep = |result: Result<()>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };
        keep(self.backend.wait_idle());
        while let Some(retiring) = self.retiring.pop_front() {
            keep(self.backend.destroy_resource(retiring.id));
        }
        for id in std::mem::take(&mut self.live).into_keys() {
            keep(self.backend.destroy_resource(id));
        }
        keep(self.backend.shutdown());
        self.shut_down = true;
        first_error.map_or(Ok(()), Err)
    }
}

impl<B: RenderBackend> Drop for RenderDevice<B> {
    fn drop(&mut self) {
        // Errors cannot be reported from drop; callers that care call `shutdown`.
        let _ = self.shutdown();
    }
}
