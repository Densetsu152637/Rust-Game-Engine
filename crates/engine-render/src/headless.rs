//! A deterministic, GPU-free backend for tests.
//!
//! [`RecordingBackend`] implements [`RenderBackend`] by logging every call into
//! a shared [`RecordingLog`] and keeping just enough state to enforce the
//! backend contract: a double destroy, a draw of a destroyed resource or an
//! unknown target is reported as a backend failure, so a [`crate::RenderDevice`]
//! bug shows up as a test failure instead of silently passing.
//!
//! Offscreen targets "render" by filling with the sRGB-encoded clear color, which
//! gives readback a deterministic, checkable value without rasterizing.

use crate::backend::{BackendKind, DeviceLimits, RenderBackend, RenderCapabilities};
use crate::command::{DrawCommand, UniformSlot};
use crate::error::{RenderError, Result};
use crate::frame::{
    FrameDesc, FrameOutcome, FrameSkip, FrameStats, OffscreenDesc, ReadbackImage, RenderTarget,
};
use crate::resources::{
    MeshHandle, MeshResource, ResourceId, ShaderHandle, ShaderResource, TargetHandle, Texture2D,
    TextureHandle,
};
use crate::state::DrawState;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

/// A draw as the backend received it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedDraw {
    pub shader: ShaderHandle,
    pub mesh: MeshHandle,
    pub texture: Option<TextureHandle>,
    pub uniforms: Vec<(UniformSlot, Vec<u8>)>,
    pub state: DrawState,
    pub order: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordedFrame {
    pub desc: FrameDesc,
    pub draws: Vec<RecordedDraw>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    Initialize,
    CreateShader {
        handle: ShaderHandle,
        name: String,
    },
    CreateMesh {
        handle: MeshHandle,
        vertex_count: u32,
        index_count: usize,
    },
    CreateTexture {
        handle: TextureHandle,
        width: u32,
        height: u32,
    },
    CreateTarget {
        handle: TargetHandle,
        width: u32,
        height: u32,
    },
    UpdateMesh {
        handle: MeshHandle,
    },
    UpdateTexture {
        handle: TextureHandle,
    },
    Destroy(ResourceId),
    Resize {
        width: u32,
        height: u32,
    },
    Frame(RecordedFrame),
    Readback(TargetHandle),
    WaitIdle,
    Shutdown,
}

/// Operation a test can make fail once via [`RecordingLog::fail_next`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailPoint {
    Initialize,
    CreateShader,
    CreateMesh,
    CreateTexture,
    SubmitFrame,
    Readback,
    Destroy,
}

/// Static behavior of a [`RecordingBackend`].
#[derive(Clone, Debug)]
pub struct RecordingConfig {
    pub max_frames_in_flight: u32,
    pub max_texture_dimension: u32,
    /// Limits reported in the capabilities (wgpu downlevel defaults by default).
    pub limits: DeviceLimits,
    /// Initial surface size; `None` means the backend has no presentation surface.
    pub surface: Option<(u32, u32)>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            max_frames_in_flight: 2,
            max_texture_dimension: 4096,
            limits: DeviceLimits::default(),
            surface: Some((64, 64)),
        }
    }
}

#[derive(Default)]
struct Inner {
    events: Vec<BackendEvent>,
    live: BTreeSet<ResourceId>,
    destroyed: Vec<ResourceId>,
    failures: BTreeMap<FailPoint, Vec<RenderError>>,
    targets: BTreeMap<TargetHandle, ReadbackImage>,
    surface_size: Option<(u32, u32)>,
    initialized: bool,
    shut_down: bool,
}

/// Shared view of everything a [`RecordingBackend`] did. Clone it before moving
/// the backend into a device.
#[derive(Clone, Default)]
pub struct RecordingLog {
    inner: Arc<Mutex<Inner>>,
}

impl RecordingLog {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn events(&self) -> Vec<BackendEvent> {
        self.lock().events.clone()
    }

    pub fn frames(&self) -> Vec<RecordedFrame> {
        self.lock()
            .events
            .iter()
            .filter_map(|event| match event {
                BackendEvent::Frame(frame) => Some(frame.clone()),
                _ => None,
            })
            .collect()
    }

    /// Resources created and not yet destroyed, in handle order.
    pub fn live_resources(&self) -> Vec<ResourceId> {
        self.lock().live.iter().copied().collect()
    }

    /// Resources in the order the backend destroyed them.
    pub fn destroyed(&self) -> Vec<ResourceId> {
        self.lock().destroyed.clone()
    }

    /// Index into [`Self::events`] where `event` first occurs, for ordering assertions.
    pub fn position(&self, wanted: impl Fn(&BackendEvent) -> bool) -> Option<usize> {
        self.lock().events.iter().position(wanted)
    }

    pub fn clear_events(&self) {
        self.lock().events.clear();
    }

    /// Make the next call at `point` fail with `error` (queued; each is used once).
    pub fn fail_next(&self, point: FailPoint, error: RenderError) {
        self.lock().failures.entry(point).or_default().push(error);
    }

    pub fn is_shut_down(&self) -> bool {
        self.lock().shut_down
    }
}

/// Deterministic in-memory backend.
pub struct RecordingBackend {
    config: RecordingConfig,
    log: RecordingLog,
}

impl Default for RecordingBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingBackend {
    pub fn new() -> Self {
        Self::with_config(RecordingConfig::default())
    }

    pub fn with_config(config: RecordingConfig) -> Self {
        let log = RecordingLog::default();
        log.lock().surface_size = config.surface;
        Self { config, log }
    }

    /// Shared log handle; clone freely.
    pub fn log(&self) -> RecordingLog {
        self.log.clone()
    }

    fn check_fail(&self, point: FailPoint) -> Result<()> {
        let mut inner = self.log.lock();
        match inner.failures.get_mut(&point) {
            Some(queue) if !queue.is_empty() => Err(queue.remove(0)),
            _ => Ok(()),
        }
    }

    fn ready(&self) -> Result<MutexGuard<'_, Inner>> {
        let inner = self.log.lock();
        if inner.shut_down {
            return Err(RenderError::NotReady("backend was shut down".into()));
        }
        if !inner.initialized {
            return Err(RenderError::NotReady("backend is not initialized".into()));
        }
        Ok(inner)
    }
}

fn contract(message: String) -> RenderError {
    RenderError::Backend(format!("backend contract violation: {message}"))
}

fn srgb_encode(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5) as u8
}

impl RenderBackend for RecordingBackend {
    fn capabilities(&self) -> RenderCapabilities {
        let mut caps = RenderCapabilities::full(BackendKind::Headless, "recording");
        caps.device_name = Some("in-memory recorder".into());
        caps.max_frames_in_flight = self.config.max_frames_in_flight;
        caps.max_texture_dimension = self.config.max_texture_dimension;
        caps.limits = self.config.limits;
        caps.backend_ready = self.log.lock().initialized;
        if self.config.surface.is_none() {
            caps.surface_presentation = false;
            caps.unsupported_reasons.insert(
                crate::backend::RenderFeature::SurfacePresentation,
                "recording backend configured without a surface".into(),
            );
        }
        caps
    }

    fn initialize(&mut self) -> Result<()> {
        self.check_fail(FailPoint::Initialize)?;
        let mut inner = self.log.lock();
        if inner.shut_down {
            return Err(RenderError::NotReady("backend was shut down".into()));
        }
        if !inner.initialized {
            inner.initialized = true;
            inner.events.push(BackendEvent::Initialize);
        }
        Ok(())
    }

    fn create_shader(&mut self, handle: ShaderHandle, resource: &ShaderResource) -> Result<()> {
        self.check_fail(FailPoint::CreateShader)?;
        let mut inner = self.ready()?;
        if !inner.live.insert(handle.into()) {
            return Err(contract(format!("{handle:?} created twice")));
        }
        inner.events.push(BackendEvent::CreateShader {
            handle,
            name: resource.name.clone(),
        });
        Ok(())
    }

    fn create_mesh(&mut self, handle: MeshHandle, resource: &MeshResource) -> Result<()> {
        self.check_fail(FailPoint::CreateMesh)?;
        let mut inner = self.ready()?;
        if !inner.live.insert(handle.into()) {
            return Err(contract(format!("{handle:?} created twice")));
        }
        inner.events.push(BackendEvent::CreateMesh {
            handle,
            vertex_count: resource.vertex_count,
            index_count: resource.indices.len(),
        });
        Ok(())
    }

    fn create_texture(&mut self, handle: TextureHandle, texture: &Texture2D) -> Result<()> {
        self.check_fail(FailPoint::CreateTexture)?;
        let mut inner = self.ready()?;
        if !inner.live.insert(handle.into()) {
            return Err(contract(format!("{handle:?} created twice")));
        }
        inner.events.push(BackendEvent::CreateTexture {
            handle,
            width: texture.width,
            height: texture.height,
        });
        Ok(())
    }

    fn create_target(&mut self, handle: TargetHandle, desc: &OffscreenDesc) -> Result<()> {
        let mut inner = self.ready()?;
        if !inner.live.insert(handle.into()) {
            return Err(contract(format!("{handle:?} created twice")));
        }
        let pixels = desc.width as usize * desc.height as usize * 4;
        inner.targets.insert(
            handle,
            ReadbackImage {
                width: desc.width,
                height: desc.height,
                rgba8: vec![0; pixels],
            },
        );
        inner.events.push(BackendEvent::CreateTarget {
            handle,
            width: desc.width,
            height: desc.height,
        });
        Ok(())
    }

    fn update_mesh(&mut self, handle: MeshHandle, _resource: &MeshResource) -> Result<()> {
        let mut inner = self.ready()?;
        if !inner.live.contains(&handle.into()) {
            return Err(contract(format!("update of dead {handle:?}")));
        }
        inner.events.push(BackendEvent::UpdateMesh { handle });
        Ok(())
    }

    fn update_texture(&mut self, handle: TextureHandle, _texture: &Texture2D) -> Result<()> {
        let mut inner = self.ready()?;
        if !inner.live.contains(&handle.into()) {
            return Err(contract(format!("update of dead {handle:?}")));
        }
        inner.events.push(BackendEvent::UpdateTexture { handle });
        Ok(())
    }

    fn destroy_resource(&mut self, id: ResourceId) -> Result<()> {
        self.check_fail(FailPoint::Destroy)?;
        let mut inner = self.ready()?;
        if !inner.live.remove(&id) {
            return Err(contract(format!("{id} destroyed while not live")));
        }
        if let ResourceId::Target(target) = id {
            inner.targets.remove(&target);
        }
        inner.destroyed.push(id);
        inner.events.push(BackendEvent::Destroy(id));
        Ok(())
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let mut inner = self.ready()?;
        if inner.surface_size.is_none() {
            return Err(RenderError::Unsupported {
                feature: "surface-presentation".into(),
                reason: "recording backend configured without a surface".into(),
            });
        }
        inner.surface_size = Some((width, height));
        inner.events.push(BackendEvent::Resize { width, height });
        Ok(())
    }

    fn submit_frame(&mut self, frame: &FrameDesc, draws: &[DrawCommand]) -> Result<FrameOutcome> {
        self.check_fail(FailPoint::SubmitFrame)?;
        let mut inner = self.ready()?;
        match frame.target {
            RenderTarget::Surface => match inner.surface_size {
                None => {
                    return Err(RenderError::Unsupported {
                        feature: "surface-presentation".into(),
                        reason: "recording backend configured without a surface".into(),
                    });
                }
                Some((0, _) | (_, 0)) => return Ok(FrameOutcome::Skipped(FrameSkip::Suspended)),
                Some(_) => {}
            },
            RenderTarget::Offscreen(target) => {
                if !inner.live.contains(&target.into()) {
                    return Err(contract(format!("frame targets dead {target:?}")));
                }
            }
        }
        for draw in draws {
            let mut referenced = vec![draw.shader.into(), draw.mesh.into()];
            referenced.extend(draw.texture.map(ResourceId::from));
            if let Some(dead) = referenced.iter().find(|id| !inner.live.contains(id)) {
                return Err(contract(format!(
                    "draw {} references dead resource {dead}",
                    draw.order
                )));
            }
        }
        if let RenderTarget::Offscreen(target) = frame.target
            && let Some(image) = inner.targets.get_mut(&target)
        {
            let color = frame.clear_color.map(srgb_encode);
            let alpha = (frame.clear_color[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            for pixel in image.rgba8.as_chunks_mut::<4>().0 {
                *pixel = [color[0], color[1], color[2], alpha];
            }
        }
        let recorded = RecordedFrame {
            desc: *frame,
            draws: draws
                .iter()
                .map(|draw| RecordedDraw {
                    shader: draw.shader,
                    mesh: draw.mesh,
                    texture: draw.texture,
                    uniforms: draw
                        .uniforms
                        .iter()
                        .map(|u| (u.slot, u.bytes.clone()))
                        .collect(),
                    state: draw.state,
                    order: draw.order,
                })
                .collect(),
        };
        inner.events.push(BackendEvent::Frame(recorded));
        Ok(FrameOutcome::Rendered(FrameStats {
            draws: draws.len(),
            batches: draws.len().min(1),
        }))
    }

    fn read_target(&mut self, target: TargetHandle) -> Result<ReadbackImage> {
        self.check_fail(FailPoint::Readback)?;
        let mut inner = self.ready()?;
        let image = inner
            .targets
            .get(&target)
            .cloned()
            .ok_or_else(|| contract(format!("readback of dead {target:?}")))?;
        inner.events.push(BackendEvent::Readback(target));
        Ok(image)
    }

    fn wait_idle(&mut self) -> Result<()> {
        let mut inner = self.ready()?;
        inner.events.push(BackendEvent::WaitIdle);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        let mut inner = self.log.lock();
        if inner.shut_down {
            return Ok(());
        }
        if let Some(leaked) = inner.live.iter().next().copied() {
            let count = inner.live.len();
            inner.shut_down = true;
            inner.events.push(BackendEvent::Shutdown);
            return Err(contract(format!(
                "shutdown with {count} live resources (first: {leaked})"
            )));
        }
        inner.shut_down = true;
        inner.events.push(BackendEvent::Shutdown);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_encoding_matches_reference_values() {
        assert_eq!(srgb_encode(0.0), 0);
        assert_eq!(srgb_encode(1.0), 255);
        assert_eq!(srgb_encode(0.5), 188);
        assert_eq!(srgb_encode(-3.0), 0);
        assert_eq!(srgb_encode(9.0), 255);
    }

    #[test]
    fn calls_before_initialize_are_rejected() {
        let mut backend = RecordingBackend::new();
        assert!(!backend.capabilities().backend_ready);
        let error = backend.wait_idle().unwrap_err();
        assert!(matches!(error, RenderError::NotReady(_)), "{error}");
        backend.initialize().unwrap();
        backend.initialize().unwrap();
        assert!(backend.capabilities().backend_ready);
        assert_eq!(backend.log().events(), [BackendEvent::Initialize]);
    }
}
