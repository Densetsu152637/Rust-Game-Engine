use crate::{
    Error, Result,
    input::{BindingMap, validate_bindings},
};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component as PathComponent, Path, PathBuf},
};

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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneEntity {
    pub id: String,
    pub name: String,
    pub components: Components,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema: u32,
    #[serde(rename = "scene_id")]
    pub id: String,
    pub entities: Vec<SceneEntity>,
    #[serde(skip)]
    pub source: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
}
#[derive(Clone, Debug)]
pub struct Project {
    pub schema: u32,
    pub name: String,
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub startup_scene: PathBuf,
    pub rust_manifest: PathBuf,
    pub assets: BTreeMap<String, Asset>,
    pub input_actions: BTreeMap<String, String>,
    pub input_bindings: BindingMap,
    pub scene: Scene,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    name: String,
    startup_scene: PathBuf,
    assets: Vec<Asset>,
    #[serde(default)]
    rust: Option<RustConfig>,
    #[serde(default, rename = "inputActions")]
    input_actions: BTreeMap<String, String>,
    #[serde(default, rename = "inputBindings")]
    input_bindings: BindingMap,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustConfig {
    pub manifest: PathBuf,
}

/// Canonicalize an existing file and reject traversal, absolute paths, and symlink escapes.
pub fn confined_file(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|c| {
            matches!(
                c,
                PathComponent::ParentDir | PathComponent::RootDir | PathComponent::Prefix(_)
            )
        })
    {
        return Err(Error::new(
            "project.path.outside_root",
            "Expected a project-relative path without parent traversal",
        )
        .at(relative, ""));
    }
    let root = fs::canonicalize(root)?;
    let file = fs::canonicalize(root.join(relative))
        .map_err(|e| Error::new("project.asset.missing", e.to_string()).at(relative, ""))?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err(Error::new(
            "project.path.outside_root",
            "Referenced file must exist inside project root",
        )
        .at(relative, ""));
    }
    Ok(file)
}
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            "project.file.too_large",
            format!("File exceeds {limit} bytes"),
        )
        .at(path, ""));
    }
    Ok(bytes)
}
pub fn load_scene(root: &Path, relative: &Path) -> Result<Scene> {
    let file = confined_file(root, relative)?;
    let mut scene: Scene = serde_json::from_slice(&read_bounded(&file, 16 * 1024 * 1024)?)?;
    scene.source = relative.to_owned();
    scene.validate()?;
    Ok(scene)
}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn finite<const N: usize>(values: [f32; N]) -> bool {
    values.iter().all(|v| v.is_finite())
}
fn rectangle(r: [u32; 4]) -> bool {
    r[2] > 0
        && r[3] > 0
        && r[0].checked_add(r[2]).is_some_and(|n| n <= 8192)
        && r[1].checked_add(r[3]).is_some_and(|n| n <= 8192)
}
impl Scene {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::new(
                "project.schema.unsupported",
                "Only scene schema 1 is supported",
            ));
        }
        if !identifier(&self.id) || self.entities.len() > 65536 {
            return Err(Error::new(
                "scene.invalid",
                "Invalid scene ID or entity capacity exceeded",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut cameras = 0;
        for entity in &self.entities {
            let bad = |message: &str| {
                Error::new("scene.component.invalid", message).at(&self.source, &entity.id)
            };
            if !identifier(&entity.id) || !identifier(&entity.name) || !ids.insert(&entity.id) {
                return Err(Error::new(
                    "scene.entity.id.duplicate",
                    "Invalid or duplicate persistent entity ID/name",
                )
                .at(&self.source, &entity.id));
            }
            let c = &entity.components;
            if let Some(t) = &c.transform
                && (t.version != 1 || !finite(t.position))
            {
                return Err(bad("Invalid Transform"));
            }
            if let Some(s) = &c.script
                && (s.version != 1 || !identifier(&s.asset))
            {
                return Err(bad("Invalid Script"));
            }
            if let Some(m) = &c.mesh_renderer
                && (m.version != 1
                    || !identifier(&m.mesh)
                    || m.texture.as_ref().is_some_and(|s| !identifier(s)))
            {
                return Err(bad("Invalid MeshRenderer"));
            }
            if (c.sprite_renderer.is_some() || c.collider_2d.is_some()) && c.transform.is_none() {
                return Err(bad("SpriteRenderer and Collider2D require Transform"));
            }
            if let Some(s) = &c.sprite_renderer
                && (s.version != 1
                    || !identifier(&s.texture)
                    || !finite(s.size)
                    || s.size.iter().any(|n| *n <= 0.0)
                    || !finite(s.pivot)
                    || s.pivot.iter().any(|n| !(0.0..=1.0).contains(n))
                    || !finite(s.tint)
                    || s.tint.iter().any(|n| !(0.0..=1.0).contains(n))
                    || s.frames.len() > 4096
                    || s.source.is_some_and(|r| !rectangle(r))
                    || s.frames.iter().any(|r| !rectangle(*r))
                    || !s.frames_per_second.is_finite()
                    || !(0.0..=1000.0).contains(&s.frames_per_second))
            {
                return Err(bad("Invalid SpriteRenderer"));
            }
            if let Some(camera) = &c.camera_2d {
                cameras += 1;
                if camera.version != 1
                    || camera.logical_size.iter().any(|n| *n == 0 || *n > 8192)
                    || !camera.pixels_per_unit.is_finite()
                    || camera.pixels_per_unit <= 0.0
                    || !finite(camera.center)
                {
                    return Err(bad("Invalid Camera2D"));
                }
            }
            if let Some(b) = &c.collider_2d
                && (b.version != 1
                    || !finite(b.size)
                    || b.size.iter().any(|n| *n <= 0.0 || *n > 1e6)
                    || !finite(b.offset)
                    || b.offset.iter().any(|n| n.abs() > 1e6)
                    || b.layer == 0
                    || b.mask == 0)
            {
                return Err(bad("Invalid Collider2D"));
            }
        }
        if cameras > 1 {
            return Err(Error::new(
                "scene.camera.multiple",
                "At most one Camera2D is supported",
            ));
        }
        for e in &self.entities {
            if let Some(follow) = e
                .components
                .camera_2d
                .as_ref()
                .and_then(|c| c.follow.as_ref())
                && !self
                    .entities
                    .iter()
                    .any(|t| &t.id == follow && t.components.transform.is_some())
            {
                return Err(Error::new(
                    "scene.camera.follow",
                    "Camera follow requires a transformed entity",
                ));
            }
        }
        Ok(())
    }
    pub fn revision(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).expect("validated scene serialization"))
            .to_hex()
            .to_string()
    }
}
impl Project {
    pub fn load(manifest: impl AsRef<Path>) -> Result<Self> {
        let manifest = fs::canonicalize(manifest)?;
        let root = manifest
            .parent()
            .ok_or_else(|| Error::new("project.path.invalid", "Manifest has no parent"))?
            .to_owned();
        let raw: Manifest = serde_json::from_slice(&read_bounded(&manifest, 16 * 1024 * 1024)?)?;
        if raw.schema != 1 {
            return Err(Error::new(
                "project.schema.unsupported",
                "Only project schema 1 is supported",
            ));
        }
        if !identifier(&raw.name) || raw.assets.len() > 65536 {
            return Err(Error::new(
                "project.invalid",
                "Invalid project name or asset capacity exceeded",
            ));
        }
        let mut assets = BTreeMap::new();
        let mut paths = BTreeSet::new();
        for asset in raw.assets {
            if !identifier(&asset.id) || assets.contains_key(&asset.id) {
                return Err(Error::new(
                    "project.asset.id.duplicate",
                    "Invalid or duplicate asset ID",
                ));
            }
            if ![
                "script", "mesh", "texture", "font", "audio", "scene", "shader",
            ]
            .contains(&asset.kind.as_str())
            {
                return Err(Error::new("project.asset.kind", "Unsupported asset kind"));
            }
            let file = confined_file(&root, &asset.path)?;
            if !paths.insert(file.to_string_lossy().to_lowercase()) {
                return Err(Error::new(
                    "project.asset.path.duplicate",
                    "Asset paths must be unique",
                ));
            }
            if asset.kind == "script" && file.extension().and_then(|e| e.to_str()) != Some("rs") {
                return Err(Error::new(
                    "project.script.language",
                    "Native scripts must be Rust .rs sources",
                ));
            }
            assets.insert(asset.id.clone(), asset);
        }
        let scene = load_scene(&root, &raw.startup_scene)?;
        let mut bindings = raw.input_bindings;
        for (action, key) in &raw.input_actions {
            bindings.entry(action.clone()).or_insert_with(|| {
                vec![if key.contains(':') {
                    key.clone()
                } else {
                    format!("Key:{key}")
                }]
            });
        }
        let validation = validate_bindings(&bindings, false);
        if !validation.errors.is_empty() {
            return Err(Error::new(
                "project.input.invalid",
                validation.errors.join("; "),
            ));
        }
        let rust_manifest = raw
            .rust
            .as_ref()
            .map(|r| r.manifest.clone())
            .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
        if raw.rust.is_some() {
            confined_file(&root, &rust_manifest)?;
        }
        let project = Self {
            schema: 1,
            name: raw.name,
            root,
            manifest,
            startup_scene: raw.startup_scene,
            rust_manifest,
            assets,
            input_actions: raw.input_actions,
            input_bindings: bindings,
            scene,
        };
        project.validate()?;
        Ok(project)
    }
    pub fn rust_manifest(&self) -> Result<PathBuf> {
        confined_file(&self.root, &self.rust_manifest)
    }
    pub fn resolve_asset(&self, id: &str) -> Result<PathBuf> {
        let asset = self
            .assets
            .get(id)
            .ok_or_else(|| Error::new("project.asset.unknown", format!("Unknown asset {id}")))?;
        confined_file(&self.root, &asset.path)
    }
    pub fn require_asset(&self, id: &str, kind: &str) -> Result<&Asset> {
        let asset = self
            .assets
            .get(id)
            .ok_or_else(|| Error::new("project.asset.unknown", format!("Unknown asset {id}")))?;
        if asset.kind != kind {
            return Err(Error::new(
                "project.asset.kind",
                format!("Asset {id} must be {kind}"),
            ));
        }
        Ok(asset)
    }
    pub fn validate_scene(&self, scene: &Scene) -> Result<()> {
        scene.validate()?;
        for entity in &scene.entities {
            let c = &entity.components;
            if let Some(s) = &c.script {
                self.require_asset(&s.asset, "script")?;
            }
            if let Some(m) = &c.mesh_renderer {
                self.require_asset(&m.mesh, "mesh")?;
                if let Some(t) = &m.texture {
                    self.require_asset(t, "texture")?;
                }
            }
            if let Some(s) = &c.sprite_renderer {
                self.require_asset(&s.texture, "texture")?;
            }
        }
        Ok(())
    }
    /// Validate all declared scenes/content, not only the startup scene.
    pub fn validate(&self) -> Result<()> {
        self.validate_scene(&self.scene)?;
        for asset in self.assets.values() {
            let file = self.resolve_asset(&asset.id)?;
            match asset.kind.as_str() {
                "scene" => self.validate_scene(&load_scene(&self.root, &asset.path)?)?,
                "mesh" => {
                    crate::content::load_mesh(&file)?;
                }
                "texture" => {
                    crate::content::load_texture(&file)?;
                }
                "font" => {
                    let font = crate::content::load_font(&file)?;
                    self.require_asset(&font.texture, "texture")?;
                    let texture =
                        crate::content::load_texture(&self.resolve_asset(&font.texture)?)?;
                    font.validate(texture.width, texture.height)?;
                }
                "script" => {
                    read_bounded(&file, 1024 * 1024)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
