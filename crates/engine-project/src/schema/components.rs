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
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            version: 1,
            position: [0.0; 3],
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
}
