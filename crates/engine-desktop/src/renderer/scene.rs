//! 3D draw preparation on the same device and command buffer as sprites.
use super::{RenderStats, cache::ResourceCache, vertex::vertex_layout};
use anyhow::{Context, Result, ensure};
use engine_project::{
    rendering3d::*,
    runtime::{Draw, DrawSpace, DrawState},
};
use engine_render::{
    DrawCommand, DrawUniform, RenderBackend, RenderCommandBuffer, RenderDevice, ShaderHandle,
    ShaderInterface, ShaderResource, ShaderSource, UniformSlot,
};
use glam::{Mat4, Vec3};
use std::collections::HashMap;

pub const LIT_WGSL: &str = include_str!("lit.wgsl");

pub(super) fn overlay_material(draw: &Draw, material: &MaterialSnapshot) -> MeshDraw3D {
    MeshDraw3D {
        id: draw.id.clone(),
        mesh: draw.mesh.clone(),
        texture: draw.texture.clone(),
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::IDENTITY.to_cols_array_2d(),
        material: material.material.clone(),
        shader: material.shader.clone(),
    }
}

pub(super) fn validate_material(draw: &MeshDraw3D, vp: Mat4) -> Result<ShaderInterface> {
    draw.material.validate()?;
    let source = draw.shader.as_deref().unwrap_or(LIT_WGSL);
    let interface =
        ShaderInterface::reflect(&ShaderResource::new(&draw.id, ShaderSource::wgsl(source)))?;
    interface.check_vertex_layout(&vertex_layout())?;
    interface.check_uniforms(&uniforms(
        draw,
        vp,
        &interface,
        &LightingUniform::default(),
    )?)?;
    Ok(interface)
}

pub fn camera_matrix(camera: &CameraInstance3D, aspect: f32) -> Result<Mat4> {
    camera.camera.validate()?;
    ensure!(aspect.is_finite() && aspect > 0.0, "invalid camera aspect");
    let world = Mat4::from_cols_array_2d(&camera.world);
    ensure!(
        world.is_finite() && world.determinant().abs() > 1e-8,
        "camera transform is singular"
    );
    let projection = match camera.camera.projection {
        Projection3D::Perspective {
            fov_y_radians,
            near,
            far,
        } => glam::camera::rh::proj::directx::perspective(fov_y_radians, aspect, near, far),
        Projection3D::Orthographic { height, near, far } => {
            glam::camera::rh::proj::directx::orthographic(
                -height * aspect * 0.5,
                height * aspect * 0.5,
                -height * 0.5,
                height * 0.5,
                near,
                far,
            )
        }
    };
    let matrix = projection * world.inverse();
    ensure!(matrix.is_finite(), "camera matrix overflow");
    Ok(matrix)
}

#[derive(Default)]
pub(super) struct SceneResources {
    cache: ResourceCache,
    shaders: HashMap<String, (ShaderHandle, ShaderInterface)>,
    lighting: LightingUniform,
    /// Engine depth/motion shader for temporal upscaling, created on first use.
    #[cfg_attr(not(feature = "dlss"), allow(dead_code))]
    motion: Option<ShaderHandle>,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightingUniform {
    light_vp: [[f32; 4]; 4],
    eye: [f32; 4],
    direction: [f32; 4],
    radiance: [f32; 4],
    shadow: [f32; 4],
}
impl Default for LightingUniform {
    fn default() -> Self {
        Self::new(&SceneLighting::default(), Vec3::Z)
    }
}
impl LightingUniform {
    fn new(light: &SceneLighting, eye: Vec3) -> Self {
        let direction = Vec3::from_array(light.direction).normalize();
        let s = &light.shadows;
        let center = Vec3::from_array(s.center);
        let up = if direction.dot(Vec3::Y).abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        // Build orientation near the origin so small depth volumes remain valid
        // at large world coordinates (adding the eye offset there can round to zero).
        let view = glam::camera::rh::view::look_at_mat4(direction * s.depth * 0.5, Vec3::ZERO, up)
            * Mat4::from_translation(-center);
        let half = s.extent * 0.5;
        let projection =
            glam::camera::rh::proj::directx::orthographic(-half, half, -half, half, 0.0, s.depth);
        Self {
            light_vp: (projection * view).to_cols_array_2d(),
            eye: eye.extend(1.0).to_array(),
            direction: direction.extend(0.0).to_array(),
            radiance: [
                light.color[0] * light.intensity,
                light.color[1] * light.intensity,
                light.color[2] * light.intensity,
                light.ambient,
            ],
            shadow: [if s.enabled { 1.0 } else { 0.0 }, s.bias, 0.0, 0.0],
        }
    }
}
impl SceneResources {
    /// 3D color commands. `size` gives the projection aspect; `jitter` is a
    /// clip-space matrix premultiplied onto the camera (identity when native).
    pub fn build<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        frame: &SceneRenderFrame,
        size: (u32, u32),
        jitter: Mat4,
    ) -> Result<(RenderCommandBuffer, RenderStats)> {
        let mut commands = device.make_frame();
        if frame.meshes.is_empty() {
            self.cache.clear(device)?;
            return Ok((commands, RenderStats::default()));
        }
        let camera = frame
            .camera3d
            .as_ref()
            .context("3D meshes require an active Camera3D")?;
        let vp = jitter * camera_matrix(camera, size.0 as f32 / size.1 as f32)?;
        self.lighting = LightingUniform::new(
            &camera.camera.lighting,
            Mat4::from_cols_array_2d(&camera.world).w_axis.truncate(),
        );
        // Validate the entire candidate before mutating the cache.
        for draw in &frame.meshes {
            ensure!(
                Mat4::from_cols_array_2d(&draw.model).is_finite()
                    && Mat4::from_cols_array_2d(&draw.model).determinant().abs() > 1e-8,
                "nonfinite model transform"
            );
            validate_material(draw, vp)?;
        }
        let proxies: Vec<_> = frame
            .meshes
            .iter()
            .map(|d| Draw {
                id: d.id.clone(),
                mesh: d.mesh.clone(),
                texture: d.texture.clone(),
                layer: 0,
                space: DrawSpace::World,
                clip: None,
                state: DrawState::default(),
                motion: None,
                offset: [0.0; 3],
            })
            .collect();
        let extra: Vec<_> = frame
            .meshes
            .iter()
            .flat_map(|d| [&d.metallic_roughness_texture, &d.normal_texture])
            .flatten()
            .cloned()
            .collect();
        let uploads = self.cache.sync_with_textures(device, &proxies, &extra)?;
        let mut stats = RenderStats {
            mesh_uploads: uploads.meshes,
            texture_uploads: uploads.textures,
            ..Default::default()
        };
        let mut draws: Vec<_> = frame.meshes.iter().collect();
        let eye = Mat4::from_cols_array_2d(&camera.world).w_axis.truncate();
        draws.sort_by(|a, b| {
            a.material
                .transparent
                .cmp(&b.material.transparent)
                .then_with(|| {
                    if a.material.transparent {
                        let distance = |d: &MeshDraw3D| {
                            Mat4::from_cols_array_2d(&d.model)
                                .w_axis
                                .truncate()
                                .distance_squared(eye)
                        };
                        distance(b).total_cmp(&distance(a))
                    } else {
                        a.id.cmp(&b.id)
                    }
                })
                .then_with(|| a.id.cmp(&b.id))
        });
        for draw in draws {
            let (shader, uniforms) = self.material(device, draw, vp)?;
            let Some(mesh) = self.cache.mesh_handle(&draw.mesh) else {
                continue;
            };
            let mut command = DrawCommand::new(shader, mesh);
            command.uniforms = uniforms;
            command.texture = draw
                .texture
                .as_ref()
                .and_then(|t| self.cache.texture_handle(t));
            let interface = &self.shaders[draw.shader.as_deref().unwrap_or(LIT_WGSL)].1;
            for (index, texture) in [&draw.metallic_roughness_texture, &draw.normal_texture]
                .into_iter()
                .enumerate()
            {
                if interface
                    .bindings
                    .iter()
                    .any(|b| b.slot == UniformSlot::new(1, index as u32 + 2))
                {
                    command.data_textures[index] =
                        texture.as_ref().and_then(|t| self.cache.texture_handle(t));
                }
            }
            command.state.depth_test = true;
            if camera.camera.lighting.shadows.enabled
                && draw.material.cast_shadows
                && !draw.material.transparent
            {
                command.shadow = Some(engine_render::ShadowCaster {
                    light_mvp: (Mat4::from_cols_array_2d(&self.lighting.light_vp)
                        * Mat4::from_cols_array_2d(&draw.model))
                    .to_cols_array_2d(),
                    resolution: camera.camera.lighting.shadows.resolution,
                });
            }
            command.state.depth_write = !draw.material.transparent;
            command.state.blend = if draw.material.transparent {
                engine_render::BlendMode::StraightAlpha
            } else {
                engine_render::BlendMode::Opaque
            };
            commands.record(command)?;
            stats.draws += 1;
            stats.batches += 1;
        }
        stats.cached_meshes = self.cache.mesh_count();
        stats.cached_textures = self.cache.texture_count();
        Ok((commands, stats))
    }
    /// Depth/motion commands for the opaque meshes of a frame already passed
    /// to [`Self::build`] (whose cache sync and validation they rely on).
    /// Transparent meshes write neither depth nor motion, matching the color
    /// pass's depth writes. Custom vertex displacement is not replayed.
    #[cfg_attr(not(feature = "dlss"), allow(dead_code))]
    pub fn build_motion<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        frame: &SceneRenderFrame,
        temporal: &super::temporal::TemporalFrame<'_>,
    ) -> Result<RenderCommandBuffer> {
        let shader = match self.motion {
            Some(shader) => shader,
            None => {
                let resource = ShaderResource::new(
                    "temporal depth and motion",
                    ShaderSource::wgsl(super::temporal::MOTION_WGSL),
                );
                ShaderInterface::reflect(&resource)?.check_vertex_layout(&vertex_layout())?;
                let shader = device.create_shader(resource)?;
                self.motion = Some(shader);
                shader
            }
        };
        let mut commands = device.make_frame();
        let mut draws: Vec<_> = frame
            .meshes
            .iter()
            .filter(|d| !d.material.transparent)
            .collect();
        draws.sort_by(|a, b| a.id.cmp(&b.id));
        for draw in draws {
            let Some(mesh) = self.cache.mesh_handle(&draw.mesh) else {
                continue;
            };
            let mut command = DrawCommand::new(shader, mesh);
            command.uniforms = vec![DrawUniform::from_pod(
                "motion",
                UniformSlot::new(0, 0),
                &temporal.uniform(draw),
            )];
            command.state.depth_test = true;
            command.state.depth_write = true;
            command.state.blend = engine_render::BlendMode::Opaque;
            commands.record(command)?;
        }
        Ok(commands)
    }
    pub fn material<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        draw: &MeshDraw3D,
        vp: Mat4,
    ) -> Result<(ShaderHandle, Vec<DrawUniform>)> {
        draw.material.validate()?;
        let source = draw.shader.as_deref().unwrap_or(LIT_WGSL);
        if !self.shaders.contains_key(source) {
            let resource = ShaderResource::new(&draw.id, ShaderSource::wgsl(source));
            let interface = validate_material(draw, vp)?;
            self.shaders
                .insert(source.into(), (device.create_shader(resource)?, interface));
        }
        let (shader, interface) = &self.shaders[source];
        let values = uniforms(draw, vp, interface, &self.lighting)?;
        interface.check_uniforms(&values)?;
        Ok((*shader, values))
    }
    pub fn retire_shaders<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        frame: &SceneRenderFrame,
    ) -> Result<()> {
        let unused: Vec<_> = self
            .shaders
            .keys()
            .filter(|s| {
                !frame
                    .meshes
                    .iter()
                    .any(|d| d.shader.as_deref().unwrap_or(LIT_WGSL) == s.as_str())
                    && !frame
                        .materials2d
                        .values()
                        .any(|d| d.shader.as_deref().unwrap_or(LIT_WGSL) == s.as_str())
            })
            .cloned()
            .collect();
        for source in unused {
            if let Some((handle, _)) = self.shaders.remove(&source) {
                device.destroy(handle)?;
            }
        }
        Ok(())
    }
}
fn uniforms(
    draw: &MeshDraw3D,
    vp: Mat4,
    interface: &ShaderInterface,
    lighting: &LightingUniform,
) -> Result<Vec<DrawUniform>> {
    let model = Mat4::from_cols_array_2d(&draw.model);
    let mvp = (vp * model).to_cols_array();
    let material = [
        draw.material.tint,
        [
            if draw.material.unlit { 1.0 } else { 0.0 },
            draw.material.metallic,
            draw.material.roughness,
            if draw.material.receive_shadows {
                1.0
            } else {
                0.0
            },
        ],
    ];
    interface
        .uniform_bindings()
        .map(
            |(binding, _)| match (binding.slot.group, binding.slot.binding) {
                (0, 0) => Ok(DrawUniform::from_pod("mvp", UniformSlot::new(0, 0), &mvp)),
                (0, 1) => Ok(DrawUniform::from_pod(
                    "model",
                    UniformSlot::new(0, 1),
                    &draw.model,
                )),
                (2, 0) => Ok(DrawUniform::from_pod(
                    "material",
                    UniformSlot::new(2, 0),
                    &material,
                )),
                (0, 2) => Ok(DrawUniform::from_pod(
                    "normal matrix",
                    UniformSlot::new(0, 2),
                    &model.inverse().transpose().to_cols_array_2d(),
                )),
                (2, 2) => Ok(DrawUniform::from_pod(
                    "texture controls",
                    UniformSlot::new(2, 2),
                    &[
                        if draw.normal_texture.is_some() {
                            1.0f32
                        } else {
                            0.0
                        },
                        draw.material.normal_scale,
                        if draw.metallic_roughness_texture.is_some() {
                            1.0
                        } else {
                            0.0
                        },
                        0.0,
                    ],
                )),
                (2, 1) => Ok(DrawUniform::from_pod(
                    "lighting",
                    UniformSlot::new(2, 1),
                    lighting,
                )),
                slot => anyhow::bail!("unsupported material uniform slot {slot:?}"),
            },
        )
        .collect()
}
