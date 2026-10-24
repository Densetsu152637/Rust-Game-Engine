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
}
impl Default for Camera3D {
    fn default() -> Self {
        Self {
            version: 1,
            projection: Projection3D::default(),
            active: true,
        }
    }
}
impl Camera3D {
    pub fn validate(&self) -> Result<()> {
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
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MeshMaterial {
    pub version: u32,
    pub shader: Option<String>,
    pub texture: Option<String>,
    pub tint: [f32; 4],
    pub unlit: bool,
    pub transparent: bool,
}
impl Default for MeshMaterial {
    fn default() -> Self {
        Self {
            version: 1,
            shader: None,
            texture: None,
            tint: [1.0; 4],
            unlit: false,
            transparent: false,
        }
    }
}
impl MeshMaterial {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !self
                .tint
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
            || self.shader.as_ref().is_some_and(String::is_empty)
            || self.texture.as_ref().is_some_and(String::is_empty)
        {
            return Err(Error::new(
                "material.invalid",
                "Material requires version 1, nonempty asset references and RGBA in 0..1",
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
