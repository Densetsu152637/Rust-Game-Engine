//! CPU scene rendering contracts shared by the game, editor and screenshot host.
use crate::{
    Error, Result,
    content::{Mesh, Texture},
    runtime::RenderFrame,
};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Projection3D {
    Perspective {
        fov_y_radians: f32,
        near: f32,
        far: f32,
    },
    Orthographic {
        height: f32,
        near: f32,
        far: f32,
    },
}
impl Default for Projection3D {
    fn default() -> Self {
        Self::Perspective {
            fov_y_radians: std::f32::consts::FRAC_PI_3,
            near: 0.1,
            far: 1000.0,
        }
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Camera3D {
    pub version: u32,
    pub projection: Projection3D,
    pub active: bool,
    pub lighting: SceneLighting,
}
impl Default for Camera3D {
    fn default() -> Self {
        Self {
            version: 1,
            projection: Projection3D::default(),
            active: true,
            lighting: SceneLighting::default(),
        }
    }
}
impl Camera3D {
    pub fn validate(&self) -> Result<()> {
        self.lighting.validate()?;
        let (size, near, far, perspective) = match self.projection {
            Projection3D::Perspective {
                fov_y_radians,
                near,
                far,
            } => (fov_y_radians, near, far, true),
            Projection3D::Orthographic { height, near, far } => (height, near, far, false),
        };
        if self.version != 1
            || ![size, near, far].iter().all(|v| v.is_finite())
            || size <= 0.0
            || near <= 0.0
            || far <= near
            || (perspective && size >= std::f32::consts::PI)
        {
            return Err(Error::new(
                "camera3d.invalid",
                "Camera requires version 1, finite positive projection and 0 < near < far",
            ));
        }
        Ok(())
    }
}
/// One directional light, plus a bounded orthographic shadow volume in world space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SceneLighting {
    /// World-space direction from a surface toward the light; normalized by the renderer.
    pub direction: [f32; 3],
    pub color: [f32; 3],
    pub intensity: f32,
    pub ambient: f32,
    pub shadows: ShadowSettings,
}
impl Default for SceneLighting {
    fn default() -> Self {
        Self {
            direction: [0.4, 0.7, 1.0],
            color: [1.0; 3],
            intensity: 3.0,
            ambient: 0.08,
            shadows: ShadowSettings::default(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ShadowSettings {
    pub enabled: bool,
    pub resolution: u32,
    pub center: [f32; 3],
    pub extent: f32,
    pub depth: f32,
    /// Constant normalized-depth bias, applied during receiver comparison.
    pub bias: f32,
}
impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            resolution: 1024,
            center: [0.0; 3],
            extent: 20.0,
            depth: 100.0,
            bias: 0.001,
        }
    }
}
impl SceneLighting {
    pub fn validate(&self) -> Result<()> {
        let s = &self.shadows;
        if !self
            .direction
            .iter()
            .chain(&self.color)
            .chain(&s.center)
            .all(|n| n.is_finite())
            || self.direction.iter().map(|n| n * n).sum::<f32>() < 1e-12
            || !self
                .direction
                .iter()
                .map(|n| n * n)
                .sum::<f32>()
                .is_finite()
            || s.center.iter().any(|n| n.abs() > 1_000_000.0)
            || self.color.iter().any(|n| !(0.0..=1.0).contains(n))
            || !self.intensity.is_finite()
            || !(0.0..=100.0).contains(&self.intensity)
            || !self.ambient.is_finite()
            || !(0.0..=1.0).contains(&self.ambient)
            || !(64..=4096).contains(&s.resolution)
            || !s.resolution.is_power_of_two()
            || !s.extent.is_finite()
            || !(0.01..=100_000.0).contains(&s.extent)
            || !s.depth.is_finite()
            || !(0.1..=100_000.0).contains(&s.depth)
            || !s.bias.is_finite()
            || !(0.0..=0.1).contains(&s.bias)
        {
            return Err(Error::new(
                "lighting.invalid",
                "Lighting requires finite bounded values, nonzero direction and power-of-two shadow resolution 64..4096",
            ));
        }
        Ok(())
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MeshMaterial {
    pub version: u32,
    pub shader: Option<String>,
    pub texture: Option<String>,
    pub metallic_roughness_texture: Option<String>,
    pub normal_texture: Option<String>,
    pub normal_scale: f32,
    pub tint: [f32; 4],
    pub unlit: bool,
    pub transparent: bool,
    pub metallic: f32,
    pub roughness: f32,
    pub cast_shadows: bool,
    pub receive_shadows: bool,
}
impl Default for MeshMaterial {
    fn default() -> Self {
        Self {
            version: 1,
            shader: None,
            texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            tint: [1.0; 4],
            unlit: false,
            transparent: false,
            metallic: 0.0,
            roughness: 0.5,
            cast_shadows: true,
            receive_shadows: true,
        }
    }
}
impl MeshMaterial {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !self.metallic.is_finite()
            || !(0.0..=1.0).contains(&self.metallic)
            || !self.roughness.is_finite()
            || !(0.0..=1.0).contains(&self.roughness)
            || !self
                .tint
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
            || self.shader.as_ref().is_some_and(String::is_empty)
            || self.texture.as_ref().is_some_and(String::is_empty)
            || self
                .metallic_roughness_texture
                .as_ref()
                .is_some_and(String::is_empty)
            || self.normal_texture.as_ref().is_some_and(String::is_empty)
            || !self.normal_scale.is_finite()
            || !(0.0..=10.0).contains(&self.normal_scale)
        {
            return Err(Error::new(
                "material.invalid",
                "Material requires version 1, nonempty asset references, finite RGBA/metallic/roughness in 0..1 and normal_scale in 0..10",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct CameraInstance3D {
    pub camera: Camera3D,
    pub world: [[f32; 4]; 4],
}
#[derive(Clone, Debug)]
pub struct MeshDraw3D {
    pub id: String,
    pub mesh: Arc<Mesh>,
    pub texture: Option<Arc<Texture>>,
    pub metallic_roughness_texture: Option<Arc<Texture>>,
    pub normal_texture: Option<Arc<Texture>>,
    pub model: [[f32; 4]; 4],
    pub material: MeshMaterial,
    /// An asset source snapshot, validated by the renderer before submission.
    /// None selects the built-in lit shader.
    pub shader: Option<Arc<str>>,
}
#[derive(Clone, Debug)]
pub struct SceneRenderFrame {
    pub sprites: RenderFrame,
    pub camera3d: Option<CameraInstance3D>,
    pub meshes: Vec<MeshDraw3D>,
    pub materials2d: BTreeMap<String, MaterialSnapshot>,
}
#[derive(Clone, Debug)]
pub struct MaterialSnapshot {
    pub material: MeshMaterial,
    pub shader: Option<Arc<str>>,
}
impl From<RenderFrame> for SceneRenderFrame {
    fn from(sprites: RenderFrame) -> Self {
        Self {
            sprites,
            camera3d: None,
            meshes: vec![],
            materials2d: BTreeMap::new(),
        }
    }
}
