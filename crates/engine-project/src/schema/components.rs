use super::*;

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn unit2() -> [f32; 2] {
    [1.0; 2]
}
fn half2() -> [f32; 2] {
    [0.5; 2]
}
fn white() -> [f32; 4] {
    [1.0; 4]
}
fn logical_size() -> [u32; 2] {
    [320, 180]
}
fn ppu() -> f32 {
    16.0
}
fn all_layers() -> u32 {
    u32::MAX
}

#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    #[serde(default = "one")]
    pub version: u32,
    pub position: [f32; 3],
    #[serde(
        default = "identity_rotation",
        skip_serializing_if = "is_identity_rotation"
    )]
    pub rotation: [f32; 4],
    #[serde(default = "unit_scale", skip_serializing_if = "is_unit_scale")]
    pub scale: [f32; 3],
}
fn identity_rotation() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}
fn is_identity_rotation(value: &[f32; 4]) -> bool {
    *value == identity_rotation()
}
fn is_unit_scale(value: &[f32; 3]) -> bool {
    *value == unit_scale()
}
fn unit_scale() -> [f32; 3] {
    [1.0; 3]
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            version: 1,
            position: [0.0; 3],
            rotation: identity_rotation(),
            scale: unit_scale(),
        }
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(default = "one")]
    pub version: u32,
    pub asset: String,
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MeshRenderer {
    #[serde(default = "one")]
    pub version: u32,
    pub mesh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<String>,
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpriteRenderer {
    #[serde(default = "one")]
    pub version: u32,
    pub texture: String,
    #[serde(default = "unit2")]
    pub size: [f32; 2],
    #[serde(default = "half2")]
    pub pivot: [f32; 2],
    #[serde(default = "white")]
    pub tint: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<[u32; 4]>,
    #[serde(default)]
    pub frames: Vec<[u32; 4]>,
    #[serde(default)]
    pub frames_per_second: f32,
    #[serde(default = "yes", rename = "loop")]
    pub looping: bool,
    #[serde(default)]
    pub layer: i32,
    #[serde(default = "yes")]
    pub visible: bool,
}
impl Default for SpriteRenderer {
    fn default() -> Self {
        Self {
            version: 1,
            texture: String::new(),
            size: unit2(),
            pivot: half2(),
            tint: white(),
            source: None,
            frames: vec![],
            frames_per_second: 0.0,
            looping: true,
            layer: 0,
            visible: true,
        }
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Camera2D {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default = "logical_size")]
    pub logical_size: [u32; 2],
    #[serde(default = "ppu")]
    pub pixels_per_unit: f32,
    #[serde(default)]
    pub center: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<String>,
    #[serde(default = "yes")]
    pub pixel_snap: bool,
}
impl Default for Camera2D {
    fn default() -> Self {
        Self {
            version: 1,
            logical_size: logical_size(),
            pixels_per_unit: ppu(),
            center: [0.0; 2],
            follow: None,
            pixel_snap: true,
        }
    }
}
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Collider2D {
    #[serde(default = "one")]
    pub version: u32,
    pub size: [f32; 2],
    #[serde(default)]
    pub offset: [f32; 2],
    #[serde(default)]
    pub trigger: bool,
    #[serde(default = "one")]
    pub layer: u32,
    #[serde(default = "all_layers")]
    pub mask: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Components {
    #[serde(rename = "Camera3D", default, skip_serializing_if = "Option::is_none")]
    pub camera3d: Option<crate::rendering3d::Camera3D>,
    #[serde(
        rename = "MeshMaterial",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub material: Option<crate::rendering3d::MeshMaterial>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, crate::catalog::CustomValue>,
    #[serde(
        rename = "EventBindings",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub event_bindings: Option<crate::events::EventBindings>,
    #[serde(rename = "Transform", default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(rename = "Script", default, skip_serializing_if = "Option::is_none")]
    pub script: Option<Script>,
    #[serde(
        rename = "MeshRenderer",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mesh_renderer: Option<MeshRenderer>,
    #[serde(
        rename = "SpriteRenderer",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub sprite_renderer: Option<SpriteRenderer>,
    #[serde(rename = "Camera2D", default, skip_serializing_if = "Option::is_none")]
    pub camera_2d: Option<Camera2D>,
    #[serde(
        rename = "Collider2D",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub collider_2d: Option<Collider2D>,
    #[serde(rename = "AreaLight", default, skip_serializing_if = "Option::is_none")]
    pub area_light: Option<crate::lights::AreaLight>,
    #[serde(
        rename = "DirectionalLight",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub directional_light: Option<crate::lights::DirectionalLight>,
    #[serde(
        rename = "PointLight",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub point_light: Option<crate::lights::PointLight>,
    #[serde(rename = "SpotLight", default, skip_serializing_if = "Option::is_none")]
    pub spot_light: Option<crate::lights::SpotLight>,
    #[serde(
        rename = "PostProcess",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub post_process: Option<crate::post::PostProcess>,
    #[serde(
        rename = "PostProcessVolume",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub post_process_volume: Option<crate::post::PostProcessVolume>,
    #[serde(
        rename = "RayTracing",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ray_tracing: Option<crate::raytracing::RayTracing>,
}
