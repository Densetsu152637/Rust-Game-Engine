use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// BLAKE3 content digest, independent of mtime and absolute checkout location.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheFingerprint(pub [u8; 32]);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedDependency {
    /// Canonical path relative to the project root.
    pub path: PathBuf,
    pub fingerprint: CacheFingerprint,
    pub byte_len: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedScene {
    pub nodes: Vec<ImportedNode>,
    #[serde(default)]
    pub morphs: ImportedMorphs,
    /// Roots of each glTF scene, preserving scene index order.
    pub scenes: Vec<Vec<usize>>,
    pub default_scene: Option<usize>,
    pub meshes: Vec<ImportedMesh>,
    pub materials: Vec<ImportedMaterial>,
    pub images: Vec<ImportedImage>,
    pub textures: Vec<ImportedTexture>,
    pub skins: Vec<ImportedSkin>,
    pub clips: Vec<ImportedClip>,
    pub dependencies: Vec<ImportedDependency>,
    pub fingerprint: CacheFingerprint,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedNode {
    pub name: Option<String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub transform: ImportedTransform,
    /// Original column-major local matrix (retains matrix-form source precision).
    pub matrix: [[f32; 4]; 4],
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ImportedTransform {
    pub translation: [f32; 3],
    /// Quaternion in x/y/z/w order.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedMesh {
    pub name: Option<String>,
    pub primitives: Vec<ImportedPrimitive>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedPrimitive {
    pub vertices: Vec<ImportedVertex>,
    /// Triangle-list indices. Nonindexed primitives receive sequential indices.
    pub indices: Vec<u32>,
    /// None means glTF's implicit default material.
    pub material: Option<usize>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ImportedVertex {
    pub position: [f32; 3],
    pub normal: Option<[f32; 3]>,
    pub tangent: Option<[f32; 4]>,
    pub uv0: Option<[f32; 2]>,
    pub uv1: Option<[f32; 2]>,
    pub color: Option<[f32; 4]>,
    pub joints: Option<[u16; 4]>,
    pub weights: Option<[f32; 4]>,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TextureReference {
    pub texture: usize,
    pub tex_coord: u32,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedMaterial {
    pub name: Option<String>,
    pub base_color: [f32; 4],
    pub base_color_texture: Option<TextureReference>,
    pub metallic: f32,
    pub roughness: f32,
    pub metallic_roughness_texture: Option<TextureReference>,
    pub normal_texture: Option<TextureReference>,
    pub normal_scale: f32,
    pub occlusion_texture: Option<TextureReference>,
    pub occlusion_strength: f32,
    pub emissive: [f32; 3],
    pub emissive_texture: Option<TextureReference>,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedImage {
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    /// PNG decoded to RGBA8, in source color space; adapters choose sRGB usage.
    pub rgba8: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedTexture {
    pub image: usize,
    /// glTF/OpenGL sampler constants, None leaves the filter unspecified.
    pub mag_filter: Option<u32>,
    pub min_filter: Option<u32>,
    pub wrap_s: u32,
    pub wrap_t: u32,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedSkin {
    pub name: Option<String>,
    /// Node indices, in skin-palette order (not necessarily hierarchy order).
    pub joints: Vec<usize>,
    pub inverse_bind_matrices: Vec<[[f32; 4]; 4]>,
    pub skeleton: Option<usize>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedClip {
    pub name: Option<String>,
    pub duration: f32,
    pub channels: Vec<ImportedChannel>,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedChannel {
    pub target_node: usize,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    /// CubicSpline contains incoming tangent, value, outgoing tangent per key.
    pub values: ChannelValues,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ChannelValues {
    Translation(Vec<[f32; 3]>),
    Rotation(Vec<[f32; 4]>),
    Scale(Vec<[f32; 3]>),
    /// Flat target weights per key (incoming/value/outgoing groups for cubic).
    Weights(Vec<f32>),
}

/// Morph data is separate from shared base geometry and TRS skeleton animation.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ImportedMorphs {
    pub primitives: Vec<ImportedMorphPrimitive>,
    pub node_weights: std::collections::BTreeMap<usize, Vec<f32>>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedMorphPrimitive {
    pub mesh: usize,
    pub primitive: usize,
    pub weights: Vec<f32>,
    pub targets: Vec<ImportedMorphTarget>,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImportedMorphTarget {
    pub positions: Option<Vec<[f32; 3]>>,
    pub normals: Option<Vec<[f32; 3]>>,
    /// Preserved for future tangent-frame render layouts; current renderer derives tangents.
    pub tangents: Option<Vec<[f32; 3]>>,
}

impl ImportedScene {
    /// Validate sidecars even for authored model JSON that bypasses glTF import.
    pub fn validate_morphs(&self) -> Result<(), &'static str> {
        let mut seen = std::collections::BTreeSet::new();
        let mut mesh_counts = std::collections::BTreeMap::new();
        let mut elements = 0usize;
        let mut target_total = 0usize;
        if self.morphs.node_weights.len() > 100_000 {
            return Err("morph node budget exceeded");
        }
        for morph in &self.morphs.primitives {
            target_total = target_total
                .checked_add(morph.targets.len())
                .ok_or("morph target count overflow")?;
            if target_total > 100_000 {
                return Err("morph target budget exceeded");
            }
            let source = self
                .meshes
                .get(morph.mesh)
                .and_then(|m| m.primitives.get(morph.primitive))
                .ok_or("morph primitive outside model")?;
            if !seen.insert((morph.mesh, morph.primitive))
                || morph.targets.is_empty()
                || morph.targets.len() > 256
                || morph.weights.len() != morph.targets.len()
                || morph.weights.iter().any(|v| !v.is_finite())
            {
                return Err("invalid morph targets or defaults");
            }
            if mesh_counts
                .insert(morph.mesh, morph.targets.len())
                .is_some_and(|n| n != morph.targets.len())
            {
                return Err("inconsistent mesh morph target counts");
            }
            for target in &morph.targets {
                if target.positions.is_none()
                    && target.normals.is_none()
                    && target.tangents.is_none()
                {
                    return Err("empty morph target");
                }
                if target.normals.is_some() && source.vertices.iter().any(|v| v.normal.is_none()) {
                    return Err("morph normal requires base normals");
                }
                if target.tangents.is_some() && source.vertices.iter().any(|v| v.tangent.is_none())
                {
                    return Err("morph tangent requires base tangents");
                }
                for deltas in [&target.positions, &target.normals, &target.tangents]
                    .into_iter()
                    .flatten()
                {
                    elements = elements
                        .checked_add(deltas.len())
                        .ok_or("morph budget overflow")?;
                    if elements > 1_000_000
                        || deltas.len() != source.vertices.len()
                        || deltas.iter().flatten().any(|v| !v.is_finite())
                    {
                        return Err("invalid or over-budget morph deltas");
                    }
                }
            }
        }
        for (&node, weights) in &self.morphs.node_weights {
            let mesh = self
                .nodes
                .get(node)
                .and_then(|n| n.mesh)
                .ok_or("morph node outside model")?;
            let width = mesh_counts.get(&mesh).ok_or("morph node has no targets")?;
            if weights.len() != *width || weights.iter().any(|v| !v.is_finite()) {
                return Err("invalid node morph defaults");
            }
        }
        for (mesh_index, mesh) in self.meshes.iter().enumerate() {
            if mesh_counts.contains_key(&mesh_index)
                && (0..mesh.primitives.len()).any(|p| !seen.contains(&(mesh_index, p)))
            {
                return Err("inconsistent mesh morph target counts");
            }
        }
        Ok(())
    }
}
