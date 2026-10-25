//! Materials on 2D sprites: the same material WGSL as 3D meshes, composed
//! against the sprite implementation of `engine::material` (one per-draw
//! uniform, unlit) and submitted through the backend-neutral sprite path.

use super::scene3d::LIT_WGSL;
use super::vertex::vertex_layout;
use anyhow::{Result, bail};
use engine_project::rendering3d::MaterialSnapshot;
use engine_render::{
    DrawUniform, RenderBackend, RenderDevice, ShaderHandle, ShaderInterface, ShaderResource,
    ShaderSource, UniformSlot,
};
use engine_render_pipeline::{MaterialTarget, compose_material};
use glam::Mat4;
use std::collections::{BTreeMap, HashMap};

/// Composed, reflected and checked sprite material shader.
fn interface(id: &str, source: &str) -> Result<(String, ShaderInterface)> {
    let composed = compose_material(source, MaterialTarget::Sprite)?;
    let interface = ShaderInterface::reflect(&ShaderResource::new(
        id,
        ShaderSource::wgsl(composed.clone()),
    ))?;
    interface.check_vertex_layout(&vertex_layout())?;
    interface.check_uniforms(&uniforms(
        &MaterialSnapshot {
            material: Default::default(),
            shader: None,
        },
        Mat4::IDENTITY,
        &interface,
    )?)?;
    Ok((composed, interface))
}

/// Check a sprite material before any GPU resource is created.
pub(super) fn validate(id: &str, material: &MaterialSnapshot) -> Result<()> {
    material.material.validate()?;
    interface(id, material.shader.as_deref().unwrap_or(LIT_WGSL)).map(drop)
}

fn uniforms(
    material: &MaterialSnapshot,
    clip_from_world: Mat4,
    interface: &ShaderInterface,
) -> Result<Vec<DrawUniform>> {
    let m = &material.material;
    let flag = |b: bool| if b { 1.0f32 } else { 0.0 };
    let values = [
        m.tint,
        [
            flag(m.unlit),
            m.metallic,
            m.roughness,
            flag(m.receive_shadows),
        ],
    ];
    interface
        .uniform_bindings()
        .map(
            |(binding, _)| match (binding.slot.group, binding.slot.binding) {
                (0, 0) => Ok(DrawUniform::from_pod(
                    "clip from world",
                    UniformSlot::new(0, 0),
                    &clip_from_world.to_cols_array(),
                )),
                (2, 0) => Ok(DrawUniform::from_pod(
                    "material",
                    UniformSlot::new(2, 0),
                    &values,
                )),
                (2, 2) => Ok(DrawUniform::from_pod(
                    "maps",
                    UniformSlot::new(2, 2),
                    &[0.0f32, m.normal_scale, 0.0, 0.0],
                )),
                slot => bail!("unsupported material uniform slot {slot:?}"),
            },
        )
        .collect()
}

/// Shader handles of the sprite materials in use, keyed by material source.
#[derive(Default)]
pub(super) struct Materials2d {
    shaders: HashMap<String, (ShaderHandle, ShaderInterface)>,
}

impl Materials2d {
    /// The shader and uniforms drawing a sprite with `material`.
    pub fn material<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        id: &str,
        material: &MaterialSnapshot,
        clip_from_world: Mat4,
    ) -> Result<(ShaderHandle, Vec<DrawUniform>)> {
        material.material.validate()?;
        let source = material.shader.as_deref().unwrap_or(LIT_WGSL);
        if !self.shaders.contains_key(source) {
            let (composed, interface) = interface(id, source)?;
            let shader =
                device.create_shader(ShaderResource::new(id, ShaderSource::wgsl(composed)))?;
            self.shaders.insert(source.into(), (shader, interface));
        }
        let (shader, interface) = &self.shaders[source];
        let values = uniforms(material, clip_from_world, interface)?;
        interface.check_uniforms(&values)?;
        Ok((*shader, values))
    }

    /// Release shaders no material of `materials` uses.
    pub fn retire<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        materials: &BTreeMap<String, MaterialSnapshot>,
    ) -> Result<()> {
        let unused: Vec<String> = self
            .shaders
            .keys()
            .filter(|source| {
                !materials
                    .values()
                    .any(|m| m.shader.as_deref().unwrap_or(LIT_WGSL) == source.as_str())
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
