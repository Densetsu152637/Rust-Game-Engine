//! The backend-neutral sprite renderer.
//!
//! [`SpriteRenderer`] turns an engine [`RenderFrame`] into neutral draw commands
//! and submits them through a [`RenderDevice`], so any [`RenderBackend`]
//! (wgpu or the recording backend) renders the same pixels.

use super::cache::{ResourceCache, address};
use super::vertex::VERTEX_STRIDE;
use super::{RenderLimits, RenderStats};
use crate::{CameraView, PixelRect as CameraRect};
use anyhow::{Context, Result, ensure};
use engine_project::lighting2d::Light2DKind;
use engine_project::rendering3d::LightingFrame2D;
use engine_project::runtime::{BlendMode, Draw, DrawSpace, RenderFrame};
use engine_render::{
    DeviceConfig, DrawCommand, DrawState, DrawUniform, FrameDesc, FrameOutcome, PixelRect,
    RenderBackend, RenderCommandBuffer, RenderDevice, RenderTarget, ShaderHandle, ShaderResource,
    ShaderSource, UniformSlot,
};
use engine_render_pipeline::Light;
use engine_render_pipeline::sprite_lighting::{SpriteLight, SpriteLighting, SpriteLightingUniform};
use glam::Vec3;
use std::collections::BTreeMap;
use std::collections::HashSet;

/// The sprite shader shipped with the engine.
pub const TEXTURED_WGSL: &str = include_str!("../textured.wgsl");

/// Where the shader reads the camera matrix: `@group(0) @binding(0)`.
const CAMERA_SLOT: UniformSlot = UniformSlot::new(0, 0);

/// Check the limits a renderer is built with.
pub(crate) fn validate_limits(limits: &RenderLimits) -> Result<()> {
    ensure!(
        (1..=8).contains(&limits.max_frames_in_flight),
        "frame limit must be 1..=8"
    );
    ensure!(limits.max_draws > 0, "draw limit must be positive");
    Ok(())
}

pub(crate) fn add_stats(stats: &mut RenderStats, overlay: RenderStats) {
    stats.draws += overlay.draws;
    stats.batches += overlay.batches;
    stats.mesh_uploads += overlay.mesh_uploads;
    stats.texture_uploads += overlay.texture_uploads;
    stats.cached_meshes += overlay.cached_meshes;
    stats.cached_textures += overlay.cached_textures;
}

/// A frame converted into neutral commands, ready to submit.
pub(crate) struct BuiltFrame {
    pub commands: RenderCommandBuffer,
    pub stats: RenderStats,
    pub viewport: PixelRect,
    pub painter: Vec<engine_render_pipeline::OverlayItem>,
}

impl BuiltFrame {
    pub fn desc(&self, target: RenderTarget) -> FrameDesc {
        let mut desc = FrameDesc::new(target);
        desc.viewport = Some(self.viewport);
        desc
    }
}

/// Renders sprite/text/UI frames on any backend.
///
/// CPU assets are immutable `Arc` snapshots: retaining the same `Arc` means
/// unchanged; publish a new `Arc` for edits. GPU resources retain the `Arc` to
/// prevent address reuse and are retired after their last frame reference.
pub struct SpriteRenderer<B: RenderBackend = Box<dyn RenderBackend>> {
    device: RenderDevice<B>,
    limits: RenderLimits,
    shader: ShaderHandle,
    cache: ResourceCache,
    materials: super::material2d::Materials2d,
    surface_size: (u32, u32),
    lighting_limits: wgpu::Limits,
}

impl<B: RenderBackend> SpriteRenderer<B> {
    /// Initialize `backend` (its `max_frames_in_flight` request comes from
    /// `limits`) and create the sprite shader.
    pub fn new(backend: B, limits: RenderLimits) -> Result<Self> {
        validate_limits(&limits)?;
        let mut device = RenderDevice::new(
            backend,
            DeviceConfig {
                frames_in_flight: limits.max_frames_in_flight as u32,
            },
        )?;
        let shader = device.create_shader(ShaderResource::new(
            "textured mesh",
            ShaderSource::wgsl(TEXTURED_WGSL),
        ))?;
        Ok(Self {
            device,
            limits,
            shader,
            cache: ResourceCache::default(),
            materials: Default::default(),
            surface_size: (0, 0),
            lighting_limits: wgpu::Limits::default(),
        })
    }

    pub fn device(&self) -> &RenderDevice<B> {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut RenderDevice<B> {
        &mut self.device
    }

    pub fn limits(&self) -> RenderLimits {
        self.limits
    }

    /// Size of the presentation surface, `(0, 0)` until known.
    pub fn surface_size(&self) -> (u32, u32) {
        self.surface_size
    }

    /// Record the surface size without touching the backend (for backends that
    /// already created the surface at this size).
    pub fn set_surface_size(&mut self, size: (u32, u32)) {
        self.surface_size = size;
    }
    pub(crate) fn set_lighting_limits(&mut self, limits: wgpu::Limits) {
        self.lighting_limits = limits;
    }

    /// Resize the presentation surface. Zero is a suspended surface, not a size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        ensure!(
            width > 0 && height > 0,
            "zero size is a suspended surface, not a render target"
        );
        self.device.resize(width, height)?;
        self.surface_size = (width, height);
        Ok(())
    }

    /// Render `frame` to `target`. Returns `None` when the backend skipped the
    /// frame (minimized, occluded, swapchain out of date, acquire timeout).
    /// Invalid CPU data fails before changing cached resources or submitting
    /// partial draws.
    pub fn render(
        &mut self,
        frame: &RenderFrame,
        target: RenderTarget,
    ) -> Result<Option<RenderStats>> {
        let size = match target {
            RenderTarget::Surface => self.surface_size,
            RenderTarget::Offscreen(handle) => self
                .device
                .target_size(handle)
                .context("render target was destroyed")?,
        };
        ensure!(
            size.0 > 0 && size.1 > 0,
            "render target must be nonzero (surface size unknown?)"
        );
        let built = self.build(frame, size)?;
        let desc = built.desc(target);
        match self.device.submit(desc, built.commands)? {
            FrameOutcome::Rendered(_) => Ok(Some(built.stats)),
            FrameOutcome::Skipped(_) => Ok(None),
        }
    }

    /// Validate a whole scene frame (3D meshes and materials included) and
    /// draw its sprites/UI with their materials. The 3D scene itself renders
    /// only through the wgpu render pipeline ([`crate::GpuRenderer`]); on
    /// other backends it is validated but not drawn.
    pub fn render_scene(
        &mut self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
        target: RenderTarget,
    ) -> Result<Option<RenderStats>> {
        let size = self.target_size(target)?;
        self.validate_scene(frame, size)?;
        let overlay = self.build_overlay(frame, size)?;
        let desc = overlay.desc(target);
        match self.device.submit(desc, overlay.commands)? {
            FrameOutcome::Rendered(_) => Ok(Some(overlay.stats)),
            FrameOutcome::Skipped(_) => Ok(None),
        }
    }

    /// The sprites/UI of `frame` with their materials for a target of `size`.
    pub(crate) fn build_overlay(
        &mut self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
        size: (u32, u32),
    ) -> Result<BuiltFrame> {
        let built = self.build_with_materials(
            &frame.sprites,
            size,
            &frame.materials2d,
            frame.lighting2d.as_ref(),
        )?;
        self.materials
            .retire(&mut self.device, &frame.materials2d)?;
        let empty = BTreeMap::new();
        self.materials.retire_lit(
            &mut self.device,
            frame
                .lighting2d
                .as_ref()
                .map(|lit| &lit.materials)
                .unwrap_or(&empty),
        )?;
        Ok(built)
    }

    /// Handle of a cached sprite texture uploaded from `texture`.
    pub(crate) fn texture_handle(
        &self,
        texture: &std::sync::Arc<engine_project::content::Texture>,
    ) -> Option<engine_render::TextureHandle> {
        self.cache.texture_handle(texture)
    }

    /// The 3D viewport of `frame` on a target of `size` (the 2D camera fit).
    pub(crate) fn scene_viewport(
        &self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
        size: (u32, u32),
    ) -> Result<CameraRect> {
        Ok(CameraView::new(&frame.sprites.camera, size.0, size.1)?.viewport)
    }

    /// Submit commands built for this device. `false` when the backend
    /// skipped the frame.
    pub(crate) fn submit(
        &mut self,
        desc: FrameDesc,
        commands: RenderCommandBuffer,
    ) -> Result<bool> {
        Ok(matches!(
            self.device.submit(desc, commands)?,
            FrameOutcome::Rendered(_)
        ))
    }

    fn target_size(&self, target: RenderTarget) -> Result<(u32, u32)> {
        let size = match target {
            RenderTarget::Surface => self.surface_size,
            RenderTarget::Offscreen(handle) => self
                .device
                .target_size(handle)
                .context("render target was destroyed")?,
        };
        ensure!(size.0 > 0 && size.1 > 0, "render target must be nonzero");
        Ok(size)
    }

    /// Check a whole scene frame (limits, mesh/texture data, 3D materials and
    /// shaders, 2D materials) before any cache or GPU state changes.
    pub(crate) fn validate_scene(
        &self,
        frame: &engine_project::rendering3d::SceneRenderFrame,
        size: (u32, u32),
    ) -> Result<CameraView> {
        ensure!(
            frame.meshes.len() + frame.sprites.draws.len() <= self.limits.max_draws,
            "frame draw limit exceeded"
        );
        // Reuse sprite validation for the mesh/texture data and its bounded budgets.
        let proxies = engine_project::runtime::RenderFrame {
            camera: frame.sprites.camera.clone(),
            draws: frame
                .sprites
                .draws
                .iter()
                .cloned()
                .chain(frame.meshes.iter().map(|d| Draw {
                    id: d.id.clone(),
                    mesh: d.mesh.clone(),
                    texture: d.texture.clone(),
                    layer: 0,
                    space: DrawSpace::World,
                    clip: None,
                    state: Default::default(),
                    motion: None,
                    offset: [0.0; 3],
                }))
                .collect(),
        };
        let camera = CameraView::new(&frame.sprites.camera, size.0, size.1)?;
        let extra: Vec<_> = frame
            .meshes
            .iter()
            .flat_map(|d| [&d.metallic_roughness_texture, &d.normal_texture])
            .flatten()
            .cloned()
            .chain(
                frame
                    .lighting2d
                    .iter()
                    .flat_map(|lit| lit.materials.values())
                    .flat_map(|m| [&m.metallic_roughness_texture, &m.normal_texture])
                    .flatten()
                    .cloned(),
            )
            .collect();
        self.validate(&proxies, &camera, &extra)?;
        super::scene3d::validate_meshes(frame)?;
        // Failed authoring edits must not create resources for earlier valid draws.
        for draw in &frame.sprites.draws {
            if let Some(material) = frame.materials2d.get(&draw.id) {
                super::material2d::validate(&draw.id, material)?;
            }
        }
        if let Some(lighting) = &frame.lighting2d {
            self.prepare_lighting(&frame.sprites, lighting)?;
            for (id, material) in &lighting.materials {
                super::material2d::Materials2d::validate_lit(id, material)?;
            }
        }
        Ok(camera)
    }

    /// Validate, sync caches and convert `frame` for a target of `size`.
    pub(crate) fn build(&mut self, frame: &RenderFrame, size: (u32, u32)) -> Result<BuiltFrame> {
        let built = self.build_with_materials(frame, size, &Default::default(), None)?;
        self.materials
            .retire_lit(&mut self.device, &BTreeMap::new())?;
        Ok(built)
    }
    fn build_with_materials(
        &mut self,
        frame: &RenderFrame,
        size: (u32, u32),
        materials: &std::collections::BTreeMap<
            String,
            engine_project::rendering3d::MaterialSnapshot,
        >,
        lighting: Option<&LightingFrame2D>,
    ) -> Result<BuiltFrame> {
        let camera = CameraView::new(&frame.camera, size.0, size.1)?;
        let prepared = lighting
            .map(|lit| self.prepare_lighting(frame, lit))
            .transpose()?
            .unwrap_or_default();
        let extra: Vec<_> = lighting
            .into_iter()
            .flat_map(|lit| lit.materials.values())
            .flat_map(|m| [&m.metallic_roughness_texture, &m.normal_texture])
            .flatten()
            .cloned()
            .collect();
        self.validate(frame, &camera, &extra)?;
        let uploads = self
            .cache
            .sync_with_textures(&mut self.device, &frame.draws, &extra)?;
        let mut stats = RenderStats {
            mesh_uploads: uploads.meshes,
            texture_uploads: uploads.textures,
            ..Default::default()
        };
        let mut draws: Vec<&Draw> = frame.draws.iter().collect();
        // Stable IDs make equal-layer draws independent of ECS iteration order.
        draws.sort_by(|a, b| {
            (a.space == DrawSpace::Ui, a.layer, &a.id).cmp(&(
                b.space == DrawSpace::Ui,
                b.layer,
                &b.id,
            ))
        });
        let cameras = [
            DrawUniform::from_pod("camera", CAMERA_SLOT, &camera.world),
            DrawUniform::from_pod("camera", CAMERA_SLOT, &camera.ui),
        ];
        let mut commands = self.device.make_frame();
        let mut painter = Vec::new();
        let mut previous = None;
        for draw in draws {
            let Some(scissor) = camera.scissor(draw.clip)? else {
                continue;
            };
            let Some(mesh) = self.cache.mesh_handle(&draw.mesh) else {
                continue; // empty mesh: nothing to draw
            };
            let texture = match &draw.texture {
                Some(texture) => Some(
                    self.cache
                        .texture_handle(texture)
                        .context("texture missing from the cache")?,
                ),
                None => None,
            };
            let camera_index = match draw.space {
                DrawSpace::World => 0,
                DrawSpace::Ui => 1,
            };
            let state = DrawState {
                blend: match draw.state.blend {
                    BlendMode::StraightAlpha => engine_render::BlendMode::StraightAlpha,
                    _ => engine_render::BlendMode::Opaque,
                },
                depth_test: draw.state.depth_test,
                depth_write: draw.state.depth_write,
                cull: engine_render::CullMode::None,
                scissor: Some(neutral_rect(scissor)),
            };
            // A presenter-supplied offset shifts the draw through its camera
            // matrix; offset-free draws share the frame's two camera uniforms.
            let uniform = if draw.offset == [0.0; 3] {
                cameras[camera_index].clone()
            } else {
                DrawUniform::from_pod(
                    "camera",
                    CAMERA_SLOT,
                    &camera.world_with_offset(draw.offset),
                )
            };
            let mut command = DrawCommand::new(self.shader, mesh)
                .with_state(state)
                .with_uniform(uniform);
            if let Some(material) = materials.get(&draw.id) {
                let matrix = if camera_index == 0 {
                    camera.world_with_offset(draw.offset)
                } else {
                    camera.ui
                };
                let (shader, uniforms) = self.materials.material(
                    &mut self.device,
                    &draw.id,
                    material,
                    glam::Mat4::from_cols_array(&matrix),
                )?;
                command.shader = shader;
                command.uniforms = uniforms;
            }
            if let Some(lit) = lighting.and_then(|lit| lit.materials.get(&draw.id)) {
                let (shader, uniforms) = self.materials.lit_material(
                    &mut self.device,
                    &draw.id,
                    lit,
                    glam::Mat4::from_cols_array(&camera.world),
                    &prepared[&draw.id],
                )?;
                command.shader = shader;
                command.uniforms = uniforms;
                command.data_textures = [
                    lit.metallic_roughness_texture
                        .as_ref()
                        .and_then(|tex| self.cache.texture_handle(tex)),
                    lit.normal_texture
                        .as_ref()
                        .and_then(|tex| self.cache.texture_handle(tex)),
                ];
            }
            command.texture = texture;
            let shader = command.shader;
            painter.push(engine_render_pipeline::OverlayItem {
                order: engine_render_pipeline::OverlayOrderKey {
                    ui: draw.space == DrawSpace::Ui,
                    layer: draw.layer,
                    id: draw.id.clone(),
                },
                token: commands.len() as u64,
            });
            commands.record(command)?;
            // A batch is a run of draws sharing pipeline state, texture, camera
            // and scissor; painter order is never changed to reduce them.
            let key = (
                shader,
                (materials.contains_key(&draw.id) || prepared.contains_key(&draw.id))
                    .then_some(draw.id.as_str()),
                state.pipeline_state(),
                draw.texture.as_ref().map(address),
                camera_index,
                scissor_key(scissor),
            );
            if previous.as_ref() != Some(&key) {
                stats.batches += 1;
                previous = Some(key);
            }
            stats.draws += 1;
        }
        stats.cached_meshes = self.cache.mesh_count();
        stats.cached_textures = self.cache.texture_count();
        Ok(BuiltFrame {
            commands,
            stats,
            viewport: neutral_rect(camera.viewport),
            painter,
        })
    }

    fn prepare_lighting(
        &self,
        frame: &RenderFrame,
        authored: &LightingFrame2D,
    ) -> Result<BTreeMap<String, SpriteLightingUniform>> {
        if authored.materials.is_empty() {
            return Ok(BTreeMap::new());
        }
        let lights = authored
            .lighting
            .lights
            .iter()
            .map(|item| {
                let color = Vec3::from_array(item.light.color);
                let intensity = item.light.intensity * authored.lighting.exposure;
                let light = match item.light.kind {
                    Light2DKind::Point => Light::point(
                        Vec3::from_array(item.position),
                        color,
                        intensity,
                        item.light.range,
                    ),
                    Light2DKind::Directional => {
                        Light::directional(Vec3::from_array(item.light.direction), color, intensity)
                    }
                };
                SpriteLight::new(item.id.clone(), light)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ambient = Vec3::from_array(authored.lighting.ambient) * authored.lighting.exposure;
        let lighting = SpriteLighting::new(ambient, lights)?;
        let mut selected: Vec<_> = frame
            .draws
            .iter()
            .filter(|draw| authored.materials.contains_key(&draw.id))
            .collect();
        selected.sort_by(|a, b| {
            (a.space == DrawSpace::Ui, a.layer, &a.id).cmp(&(
                b.space == DrawSpace::Ui,
                b.layer,
                &b.id,
            ))
        });
        ensure!(
            selected.len() == authored.materials.len(),
            "lit material references a missing draw"
        );
        ensure!(
            selected.iter().all(|draw| draw.space == DrawSpace::World),
            "SpriteLit requires world-space draws"
        );
        let offsets: Vec<_> = selected
            .iter()
            .map(|draw| Vec3::from_array(draw.offset))
            .collect();
        let values = lighting.prepare_draws(&self.lighting_limits, &offsets)?;
        Ok(selected
            .into_iter()
            .zip(values)
            .map(|(draw, uniform)| (draw.id.clone(), uniform))
            .collect())
    }

    fn validate(
        &self,
        frame: &RenderFrame,
        camera: &CameraView,
        extra: &[std::sync::Arc<engine_project::content::Texture>],
    ) -> Result<()> {
        ensure!(
            frame.draws.len() <= self.limits.max_draws,
            "frame draw limit exceeded"
        );
        let max_dimension = self.device.capabilities().max_texture_dimension;
        let mut ids = HashSet::new();
        let mut meshes = HashSet::new();
        let mut textures = HashSet::new();
        let mut mesh_bytes = 0usize;
        let mut texture_bytes = 0usize;
        for draw in &frame.draws {
            ensure!(
                !draw.id.is_empty() && ids.insert(&draw.id),
                "empty or duplicate draw ID: {}",
                draw.id
            );
            camera.scissor(draw.clip)?;
            ensure!(
                draw.offset.iter().all(|x| x.is_finite())
                    && (draw.space == DrawSpace::World || draw.offset == [0.0; 3]),
                "draw offset must be finite and only moves world-space draws: {}",
                draw.id
            );
            let mesh = &draw.mesh;
            if meshes.insert(address(mesh)) {
                let bytes = mesh
                    .vertices
                    .len()
                    .checked_mul(VERTEX_STRIDE)
                    .and_then(|n| {
                        mesh.indices
                            .len()
                            .checked_mul(4)
                            .and_then(|i| n.checked_add(i))
                    })
                    .context("mesh byte size overflow")?;
                mesh_bytes = mesh_bytes
                    .checked_add(bytes)
                    .context("mesh byte total overflow")?;
                ensure!(
                    mesh_bytes <= self.limits.max_mesh_bytes,
                    "frame mesh byte limit exceeded"
                );
                ensure!(
                    mesh.vertices.len() <= u32::MAX as usize
                        && mesh.indices.len() <= u32::MAX as usize,
                    "mesh draw count overflow"
                );
                if !self.cache.has_current_mesh(mesh) {
                    ensure!(
                        (mesh.normals.is_empty() || mesh.normals.len() == mesh.vertices.len())
                            && mesh.normals.iter().flatten().all(|n| n.is_finite()),
                        "invalid mesh normals"
                    );
                    ensure!(
                        mesh.vertices.iter().all(|v| v
                            .position
                            .iter()
                            .chain(v.uv.iter())
                            .chain(v.color.iter())
                            .all(|x| x.is_finite())),
                        "mesh contains nonfinite vertex data"
                    );
                    ensure!(
                        mesh.indices
                            .iter()
                            .all(|i| (*i as usize) < mesh.vertices.len()),
                        "mesh index exceeds vertex count"
                    );
                    let count = if mesh.indices.is_empty() {
                        mesh.vertices.len()
                    } else {
                        mesh.indices.len()
                    };
                    ensure!(count % 3 == 0, "mesh must contain complete triangles");
                }
            }
        }
        for texture in frame
            .draws
            .iter()
            .filter_map(|d| d.texture.as_ref())
            .chain(extra.iter())
        {
            if textures.insert(address(texture)) {
                let expected = (texture.width as usize)
                    .checked_mul(texture.height as usize)
                    .and_then(|n| n.checked_mul(4))
                    .context("texture size overflow")?;
                ensure!(
                    texture.width > 0 && texture.height > 0 && expected == texture.pixels.len(),
                    "invalid RGBA texture dimensions/data"
                );
                ensure!(
                    texture.width <= max_dimension && texture.height <= max_dimension,
                    "texture exceeds device limit"
                );
                texture_bytes = texture_bytes
                    .checked_add(expected)
                    .context("texture byte total overflow")?;
                ensure!(
                    texture_bytes <= self.limits.max_texture_bytes,
                    "frame texture byte limit exceeded"
                );
            }
        }
        Ok(())
    }

    /// Wait for the GPU, retire every cached resource and shut the device down.
    /// Safe to call twice; dropping the renderer does the same without reporting.
    pub fn shutdown(&mut self) -> Result<()> {
        self.cache.clear(&mut self.device).ok();
        self.device.shutdown()?;
        Ok(())
    }
}

fn neutral_rect(rect: CameraRect) -> PixelRect {
    PixelRect::new(rect.x, rect.y, rect.width, rect.height)
}

fn scissor_key(rect: CameraRect) -> (u32, u32, u32, u32) {
    (rect.x, rect.y, rect.width, rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::{
        Camera2D,
        content::{Mesh, Texture, Vertex, sprite_quad},
        runtime::DrawState as SpriteState,
    };
    use engine_render::headless::{BackendEvent, RecordingBackend, RecordingLog};
    use std::sync::Arc;

    fn camera() -> Camera2D {
        Camera2D {
            logical_size: [64, 64],
            pixels_per_unit: 1.0,
            ..Default::default()
        }
    }

    fn renderer(limits: RenderLimits) -> (SpriteRenderer<RecordingBackend>, RecordingLog) {
        let backend = RecordingBackend::new();
        let log = backend.log();
        let mut renderer = SpriteRenderer::new(backend, limits).unwrap();
        renderer.set_surface_size((64, 64));
        (renderer, log)
    }

    fn white() -> Arc<Texture> {
        Arc::new(Texture {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        })
    }

    fn quad(color: [f32; 4], z: f32, texture: &Texture) -> Arc<Mesh> {
        Arc::new(
            sprite_quad(
                [0.0, 0.0, z],
                [16.0, 16.0],
                [0.5, 0.5],
                [0, 0, 1, 1],
                texture,
                color,
            )
            .unwrap(),
        )
    }

    fn draw(id: &str, layer: i32, mesh: &Arc<Mesh>, texture: Option<&Arc<Texture>>) -> Draw {
        Draw {
            id: id.into(),
            mesh: mesh.clone(),
            texture: texture.cloned(),
            layer,
            space: DrawSpace::World,
            clip: None,
            state: SpriteState::default(),
            motion: None,
            offset: [0.0; 3],
        }
    }

    fn frame(draws: Vec<Draw>) -> RenderFrame {
        RenderFrame {
            camera: camera(),
            draws,
        }
    }

    fn frames(log: &RecordingLog) -> Vec<engine_render::headless::RecordedFrame> {
        log.frames()
    }

    fn error(renderer: &mut SpriteRenderer<RecordingBackend>, frame: &RenderFrame) -> String {
        format!(
            "{:#}",
            renderer
                .render(frame, RenderTarget::Surface)
                .expect_err("render must fail")
        )
    }

    #[test]
    fn limits_are_validated_when_the_renderer_is_built() {
        let build = |limits| SpriteRenderer::new(RecordingBackend::new(), limits).err();
        let error = build(RenderLimits {
            max_frames_in_flight: 0,
            ..Default::default()
        })
        .unwrap();
        assert!(error.to_string().contains("frame limit must be 1..=8"));
        let error = build(RenderLimits {
            max_frames_in_flight: 9,
            ..Default::default()
        })
        .unwrap();
        assert!(error.to_string().contains("frame limit must be 1..=8"));
        let error = build(RenderLimits {
            max_draws: 0,
            ..Default::default()
        })
        .unwrap();
        assert!(error.to_string().contains("draw limit must be positive"));
    }

    #[test]
    fn frames_in_flight_come_from_the_limits() {
        let (renderer, _) = renderer(RenderLimits {
            max_frames_in_flight: 1,
            ..Default::default()
        });
        assert_eq!(renderer.device().frames_in_flight(), 1);
        assert_eq!(renderer.limits().max_frames_in_flight, 1);
    }

    #[test]
    fn draws_sort_by_layer_then_id_regardless_of_input_order() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let (a, b, c) = (
            quad([1.0; 4], 0.5, &atlas),
            quad([0.5; 4], 0.5, &atlas),
            quad([0.25; 4], 0.5, &atlas),
        );
        let draws = vec![
            draw("z", 0, &a, None),
            draw("a", 1, &b, None),
            draw("b", 0, &c, None),
        ];
        renderer
            .render(&frame(draws.clone()), RenderTarget::Surface)
            .unwrap();
        let mut reversed = draws;
        reversed.reverse();
        renderer
            .render(&frame(reversed), RenderTarget::Surface)
            .unwrap();
        let frames = frames(&log);
        assert_eq!(frames.len(), 2);
        let meshes =
            |index: usize| -> Vec<_> { frames[index].draws.iter().map(|d| d.mesh).collect() };
        assert_eq!(meshes(0), meshes(1));
        // Layer 0 sorted by id ("b" then "z"), then layer 1: c, a, b.
        let handles = |mesh: &Arc<Mesh>| renderer.cache.mesh_handle(mesh).unwrap();
        assert_eq!(meshes(0), [handles(&c), handles(&a), handles(&b)]);
    }

    #[test]
    fn stats_count_uploads_batches_and_cache_sizes() {
        let (mut renderer, _) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let draws = vec![
            draw("a", 0, &mesh, Some(&atlas)),
            draw("b", 0, &mesh, Some(&atlas)),
            draw("c", 1, &mesh, None),
        ];
        let first = renderer
            .render(&frame(draws.clone()), RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!(
            first,
            RenderStats {
                draws: 3,
                batches: 2,
                mesh_uploads: 1,
                texture_uploads: 1,
                cached_meshes: 1,
                cached_textures: 1,
            }
        );
        let second = renderer
            .render(&frame(draws), RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!((second.mesh_uploads, second.texture_uploads), (0, 0));
        let empty = renderer
            .render(&frame(vec![]), RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!(empty, RenderStats::default());
    }

    #[test]
    fn camera_and_ui_spaces_use_distinct_uniforms_and_break_batches() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let mut ui = draw("ui", 1, &mesh, None);
        ui.space = DrawSpace::Ui;
        let stats = renderer
            .render(
                &frame(vec![draw("world", 0, &mesh, None), ui]),
                RenderTarget::Surface,
            )
            .unwrap()
            .unwrap();
        assert_eq!(stats.batches, 2);
        let recorded = &frames(&log)[0].draws;
        let camera = CameraView::new(&camera(), 64, 64).unwrap();
        let bytes = |m: &[f32; 16]| bytemuck::cast_slice::<f32, u8>(m).to_vec();
        assert_eq!(recorded[0].uniforms, [(CAMERA_SLOT, bytes(&camera.world))]);
        assert_eq!(recorded[1].uniforms, [(CAMERA_SLOT, bytes(&camera.ui))]);
        assert_eq!(recorded[0].uniforms[0].1.len(), 64);
    }

    #[test]
    fn a_draw_offset_shifts_that_draw_through_its_camera_matrix_only() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let mut moved = draw("moved", 0, &mesh, None);
        moved.offset = [2.0, -1.0, 0.0];
        let still = draw("still", 1, &mesh, None);
        let stats = renderer
            .render(&frame(vec![moved, still]), RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!(stats.draws, 2);
        let recorded = &frames(&log)[0].draws;
        let camera = CameraView::new(&camera(), 64, 64).unwrap();
        let bytes = |m: &[f32; 16]| bytemuck::cast_slice::<f32, u8>(m).to_vec();
        assert_eq!(
            recorded[0].uniforms,
            [(
                CAMERA_SLOT,
                bytes(&camera.world_with_offset([2.0, -1.0, 0.0]))
            )]
        );
        assert_eq!(recorded[1].uniforms, [(CAMERA_SLOT, bytes(&camera.world))]);
        assert_eq!(
            recorded[0].mesh, recorded[1].mesh,
            "the shared mesh is not re-uploaded or rebuilt"
        );
    }

    #[test]
    fn invalid_draw_offsets_are_rejected_before_anything_is_submitted() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let mut infinite = draw("a", 0, &mesh, None);
        infinite.offset = [f32::INFINITY, 0.0, 0.0];
        assert!(error(&mut renderer, &frame(vec![infinite])).contains("offset"));
        let mut ui = draw("b", 0, &mesh, None);
        ui.space = DrawSpace::Ui;
        ui.offset = [1.0, 0.0, 0.0];
        assert!(error(&mut renderer, &frame(vec![ui])).contains("offset"));
        assert!(frames(&log).is_empty());
    }

    #[test]
    fn viewport_and_scissor_follow_the_camera_fit() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        renderer.set_surface_size((128, 96));
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let mut clipped = draw("ui", 1, &mesh, None);
        clipped.space = DrawSpace::Ui;
        clipped.clip = Some([4.0, 4.0, 10.0, 8.0]);
        renderer
            .render(
                &frame(vec![draw("world", 0, &mesh, None), clipped]),
                RenderTarget::Surface,
            )
            .unwrap();
        let recorded = &frames(&log)[0];
        // 64x64 logical on 128x96: 1x fit, centred.
        assert_eq!(recorded.desc.viewport, Some(PixelRect::new(32, 16, 64, 64)));
        assert_eq!(
            recorded.draws[0].state.scissor,
            Some(PixelRect::new(32, 16, 64, 64)),
            "unclipped draws scissor to the viewport"
        );
        assert_eq!(
            recorded.draws[1].state.scissor,
            Some(PixelRect::new(36, 20, 10, 8))
        );
        assert_eq!(recorded.desc.clear_color, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(recorded.desc.clear_depth, 1.0);
    }

    #[test]
    fn empty_clips_and_empty_meshes_are_skipped() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let empty = Arc::new(Mesh::default());
        let mut hidden = draw("hidden", 0, &mesh, None);
        hidden.clip = Some([0.0, 0.0, 0.0, 5.0]);
        let stats = renderer
            .render(
                &frame(vec![
                    draw("shown", 0, &mesh, None),
                    hidden,
                    draw("nothing", 0, &empty, None),
                ]),
                RenderTarget::Surface,
            )
            .unwrap()
            .unwrap();
        assert_eq!(stats.draws, 1);
        assert_eq!(stats.cached_meshes, 2, "the empty mesh stays cached");
        assert_eq!(frames(&log)[0].draws.len(), 1);
        assert_eq!(renderer.device().counts().meshes, 1);
    }

    #[test]
    fn draw_states_map_onto_pipeline_state() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        // The engine default is straight alpha without depth testing or writes.
        let alpha = draw("alpha", 0, &mesh, None);
        assert_eq!(
            alpha.state,
            SpriteState {
                blend: BlendMode::StraightAlpha,
                depth_test: false,
                depth_write: false,
            }
        );
        let mut opaque = draw("opaque", 1, &mesh, None);
        opaque.state = SpriteState {
            blend: BlendMode::Opaque,
            depth_test: true,
            depth_write: true,
        };
        let stats = renderer
            .render(&frame(vec![alpha, opaque]), RenderTarget::Surface)
            .unwrap()
            .unwrap();
        assert_eq!(stats.batches, 2);
        let recorded = &frames(&log)[0].draws;
        assert_eq!(
            recorded[0].state.blend,
            engine_render::BlendMode::StraightAlpha
        );
        assert!(!recorded[0].state.depth_test && !recorded[0].state.depth_write);
        assert_eq!(recorded[1].state.blend, engine_render::BlendMode::Opaque);
        assert!(recorded[1].state.depth_test && recorded[1].state.depth_write);
        assert_eq!(recorded[1].state.cull, engine_render::CullMode::None);
    }

    #[test]
    fn invalid_frames_fail_before_any_cache_or_submission_change() {
        let (mut renderer, log) = renderer(RenderLimits {
            max_draws: 2,
            ..Default::default()
        });
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        let a = draw("a", 0, &mesh, None);
        let b = draw("b", 0, &mesh, None);
        let mut excess = a.clone();
        excess.id = "excess".into();
        assert!(
            error(&mut renderer, &frame(vec![a.clone(), b.clone(), excess]))
                .contains("draw limit exceeded")
        );
        let mut duplicate = b.clone();
        duplicate.id = "a".into();
        assert!(
            error(&mut renderer, &frame(vec![a.clone(), duplicate]))
                .contains("empty or duplicate draw ID")
        );
        let mut unnamed = a.clone();
        unnamed.id.clear();
        assert!(error(&mut renderer, &frame(vec![unnamed])).contains("empty or duplicate"));
        let bad_texture = Arc::new(Texture {
            width: 2,
            height: 2,
            pixels: vec![0; 3],
        });
        let mut textured = a.clone();
        textured.texture = Some(bad_texture);
        assert!(error(&mut renderer, &frame(vec![textured])).contains("invalid RGBA texture"));
        assert_eq!(renderer.cache.mesh_count(), 0, "nothing was uploaded");
        assert!(frames(&log).is_empty(), "nothing was submitted");
        assert!(
            !log.events()
                .iter()
                .any(|e| matches!(e, BackendEvent::CreateMesh { .. }))
        );
    }

    fn vertex(position: [f32; 3]) -> Vertex {
        Vertex {
            position,
            uv: [0.0; 2],
            color: [1.0; 4],
        }
    }

    #[test]
    fn malformed_meshes_are_rejected() {
        let (mut renderer, _) = renderer(RenderLimits::default());
        let mesh = |vertices: Vec<Vertex>, indices: Vec<u32>| {
            let mesh = Arc::new(Mesh {
                normals: vec![],
                vertices,
                indices,
            });
            frame(vec![draw("m", 0, &mesh, None)])
        };
        let nan = vec![vertex([f32::NAN, 0.0, 0.0]); 3];
        assert!(error(&mut renderer, &mesh(nan, vec![])).contains("nonfinite vertex data"));
        let tri = vec![vertex([0.0; 3]); 3];
        assert!(
            error(&mut renderer, &mesh(tri.clone(), vec![0, 1, 7]))
                .contains("index exceeds vertex count")
        );
        assert!(
            error(&mut renderer, &mesh(tri.clone(), vec![0, 1])).contains("complete triangles")
        );
        let pair = vec![vertex([0.0; 3]); 2];
        assert!(error(&mut renderer, &mesh(pair, vec![])).contains("complete triangles"));
        let fine = mesh(tri, vec![0, 1, 2]);
        assert!(renderer.render(&fine, RenderTarget::Surface).is_ok());
    }

    #[test]
    fn byte_limits_apply_per_frame() {
        let (mut renderer, _) = renderer(RenderLimits {
            max_mesh_bytes: 36 * 3 - 1,
            ..Default::default()
        });
        let tri = Arc::new(Mesh {
            normals: vec![],
            vertices: vec![vertex([0.0; 3]); 3],
            indices: vec![],
        });
        assert!(
            error(&mut renderer, &frame(vec![draw("m", 0, &tri, None)]))
                .contains("mesh byte limit")
        );
        let (mut renderer, _) = renderer_with_texture_limit();
        let big = Arc::new(Texture {
            width: 2,
            height: 2,
            pixels: vec![0; 16],
        });
        let tri = Arc::new(Mesh {
            normals: vec![],
            vertices: vec![vertex([0.0; 3]); 3],
            indices: vec![],
        });
        assert!(
            error(&mut renderer, &frame(vec![draw("t", 0, &tri, Some(&big))]))
                .contains("texture byte limit")
        );
    }

    fn renderer_with_texture_limit() -> (SpriteRenderer<RecordingBackend>, RecordingLog) {
        renderer(RenderLimits {
            max_texture_bytes: 15,
            ..Default::default()
        })
    }

    #[test]
    fn oversized_textures_exceed_the_device_limit() {
        let backend = RecordingBackend::with_config(engine_render::headless::RecordingConfig {
            max_texture_dimension: 4,
            ..Default::default()
        });
        let mut renderer = SpriteRenderer::new(backend, RenderLimits::default()).unwrap();
        renderer.set_surface_size((64, 64));
        let wide = Arc::new(Texture {
            width: 5,
            height: 1,
            pixels: vec![0; 20],
        });
        let tri = Arc::new(Mesh {
            normals: vec![],
            vertices: vec![vertex([0.0; 3]); 3],
            indices: vec![],
        });
        assert!(
            error(&mut renderer, &frame(vec![draw("w", 0, &tri, Some(&wide))]))
                .contains("texture exceeds device limit")
        );
    }

    #[test]
    fn unknown_surface_size_is_an_error_and_resize_rejects_zero() {
        let mut renderer =
            SpriteRenderer::new(RecordingBackend::new(), RenderLimits::default()).unwrap();
        let message = format!(
            "{:#}",
            renderer
                .render(&frame(vec![]), RenderTarget::Surface)
                .unwrap_err()
        );
        assert!(message.contains("nonzero"), "{message}");
        assert!(renderer.resize(0, 10).is_err());
        renderer.resize(64, 64).unwrap();
        assert_eq!(renderer.surface_size(), (64, 64));
        assert!(
            renderer
                .render(&frame(vec![]), RenderTarget::Surface)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn evicted_resources_are_released_after_frames_in_flight() {
        let (mut renderer, log) = renderer(RenderLimits {
            max_frames_in_flight: 2,
            ..Default::default()
        });
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        renderer
            .render(
                &frame(vec![draw("a", 0, &mesh, Some(&atlas))]),
                RenderTarget::Surface,
            )
            .unwrap();
        assert!(log.destroyed().is_empty());
        for _ in 0..3 {
            renderer
                .render(&frame(vec![]), RenderTarget::Surface)
                .unwrap();
        }
        assert_eq!(log.destroyed().len(), 2, "mesh and texture were released");
        assert_eq!(renderer.device().counts().pending_destruction, 0);
    }

    #[test]
    fn offscreen_targets_render_at_their_own_size() {
        let (mut renderer, log) = renderer(RenderLimits::default());
        let target = renderer.device_mut().create_target(128, 96).unwrap();
        let atlas = white();
        let mesh = quad([1.0; 4], 0.5, &atlas);
        renderer
            .render(
                &frame(vec![draw("a", 0, &mesh, None)]),
                RenderTarget::Offscreen(target),
            )
            .unwrap();
        assert_eq!(
            frames(&log)[0].desc.viewport,
            Some(PixelRect::new(32, 16, 64, 64))
        );
    }

    #[test]
    fn plain_frame_retires_previous_sprite_lit_shader_after_in_flight_frames() {
        use engine_project::{
            lighting2d::LightingSnapshot2D,
            rendering3d::{LightingFrame2D, LitMaterialSnapshot2D, MeshMaterial, SceneRenderFrame},
        };
        let (mut renderer, log) = renderer(RenderLimits::default());
        let atlas = white();
        let mesh = quad([1.0; 4], 0.0, &atlas);
        let plain = frame(vec![draw("lit", 0, &mesh, Some(&atlas))]);
        let mut scene: SceneRenderFrame = plain.clone().into();
        scene.lighting2d = Some(LightingFrame2D {
            lighting: LightingSnapshot2D {
                ambient: [1.0; 3],
                exposure: 1.0,
                lights: vec![],
            },
            materials: BTreeMap::from([(
                "lit".into(),
                LitMaterialSnapshot2D {
                    material: MeshMaterial::default(),
                    shader: None,
                    metallic_roughness_texture: None,
                    normal_texture: None,
                },
            )]),
        });
        renderer
            .render_scene(&scene, RenderTarget::Surface)
            .unwrap();
        assert_eq!(renderer.device().counts().shaders, 2);
        renderer.render(&plain, RenderTarget::Surface).unwrap();
        assert_eq!(renderer.device().counts().shaders, 1);
        assert_eq!(renderer.device().counts().pending_destruction, 1);
        for _ in 0..3 {
            renderer.render(&plain, RenderTarget::Surface).unwrap();
        }
        assert_eq!(renderer.device().counts().pending_destruction, 0);
        assert_eq!(
            log.live_resources().len(),
            3,
            "base shader, mesh and texture remain"
        );
    }
}
