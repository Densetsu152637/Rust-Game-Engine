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
use glam::Mat4;
use std::collections::HashMap;

pub const LIT_WGSL: &str = include_str!("lit.wgsl");

pub(super) fn overlay_material(draw: &Draw, material: &MaterialSnapshot) -> MeshDraw3D {
    MeshDraw3D {
        id: draw.id.clone(),
        mesh: draw.mesh.clone(),
        texture: draw.texture.clone(),
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
    interface.check_uniforms(&uniforms(draw, vp, &interface)?)?;
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
    Ok(projection * world.inverse())
}

#[derive(Default)]
pub(super) struct SceneResources {
    cache: ResourceCache,
    shaders: HashMap<String, (ShaderHandle, ShaderInterface)>,
}
impl SceneResources {
    pub fn build<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        frame: &SceneRenderFrame,
        size: (u32, u32),
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
        let vp = camera_matrix(camera, size.0 as f32 / size.1 as f32)?;
        // Validate the entire candidate before mutating the cache.
        for draw in &frame.meshes {
            ensure!(
                Mat4::from_cols_array_2d(&draw.model).is_finite(),
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
        let uploads = self.cache.sync(device, &proxies)?;
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
            command.state.depth_test = true;
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
        let values = uniforms(draw, vp, interface)?;
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
fn uniforms(draw: &MeshDraw3D, vp: Mat4, interface: &ShaderInterface) -> Result<Vec<DrawUniform>> {
    let model = Mat4::from_cols_array_2d(&draw.model);
    let mvp = (vp * model).to_cols_array();
    let material = [
        draw.material.tint,
        [if draw.material.unlit { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
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
                slot => anyhow::bail!("unsupported material uniform slot {slot:?}"),
            },
        )
        .collect()
}
